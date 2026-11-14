//! A terminal = a tmux client in a PTY (ConPTY on Windows) parsed by `alacritty_terminal`.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use alacritty_terminal::event::{Event as TermEvent, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::Processor;

use crate::shell::launch;
use crate::shell::world::CardId;

/// Program and arguments to launch (in the PTY or in the background).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
}

/// Settings forced on the canvas's tmux server on every attach.
const TMUX_SETUP: &[&[&str]] = &[
    &["set", "-g", "status", "off"],
    &["set", "-g", "mouse", "on"],
    &["set", "-g", "history-limit", "50000"],
    &["set", "-g", "default-terminal", "tmux-256color"],
    &["set", "-s", "set-clipboard", "on"],
    &["set", "-s", "escape-time", "0"],
];

/// Program launched in WSL (via `wsl.exe` on Windows, directly elsewhere).
fn in_wsl(program: &str, args: Vec<String>) -> Launch {
    wsl_launch(cfg!(windows), program, args)
}

/// `in_wsl` for a given platform (`windows`: through `wsl.exe`), so both forms can be tested.
fn wsl_launch(windows: bool, program: &str, args: Vec<String>) -> Launch {
    if windows {
        let mut wsl = vec!["--cd".to_string(), "~".to_string(), "-e".to_string(), program.to_string()];
        wsl.extend(args);
        Launch { program: "wsl.exe".into(), args: wsl }
    } else {
        Launch { program: program.into(), args }
    }
}

fn tmux_args(socket: &str, args: &[&str]) -> Vec<String> {
    let mut full = vec!["-L".to_string(), socket.to_string()];
    full.extend(args.iter().map(|s| s.to_string()));
    full
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `cd` to the folder; `~` stays outside the quotes so the shell expands it.
fn cd_script(cwd: Option<&str>) -> String {
    match cwd.map(str::trim).filter(|c| !c.is_empty()) {
        None | Some("~") => "cd ~".into(),
        Some(c) => match c.strip_prefix("~/") {
            Some(rest) => format!("cd ~/{} 2>/dev/null || cd ~", sh_quote(rest)),
            None => format!("cd {} 2>/dev/null || cd ~", sh_quote(c)),
        },
    }
}

/// Folder (Linux side) where a card's command waits until its shell reads it: one private file
/// per session, named after it. Typed as is (`~` expanded by the user's shell): every character
/// of it, and of the session names the app generates, needs no quoting in any shell.
const STAGE_DIR: &str = "~/.local/share/tcanvas/commands";

/// Writes stdin to the staged file `$1`, readable by the user only (written aside, then renamed:
/// the shell never reads half a file). First sweeps staged files left over for more than 10
/// minutes (an app that quit before using them): regular files named like the app's sessions
/// only, in that folder only, never through a symbolic link.
const STAGE_SCRIPT: &str = r#"umask 077; d="$HOME/.local/share/tcanvas/commands"; mkdir -p "$d" || exit 1; find "$d" ! -path "$d" -prune -type f -name 'tc_*' -mmin +10 -exec rm -f {} + 2>/dev/null; cat > "$d/$1.tmp" && mv -f "$d/$1.tmp" "$d/$1""#;

/// Waits (at most `$3` tenths of a second) until the shell has taken the staged file `$1`, then
/// claims it if it is still there: `mv` (here) and `rm` (the file's first line, in the shell) race
/// on one name and exactly one wins. Exit 0: the shell took it (or it was never there); 3: it
/// never will (claimed and deleted). Stops waiting after 5 s when the session `$1` (socket `$2`)
/// does not exist. With `$3` = 0: claims it right away (discarding a command).
const CONFIRM_SCRIPT: &str = r#"f="$HOME/.local/share/tcanvas/commands/$1"; i=0; while [ -e "$f" ] && [ $i -lt "$3" ]; do if [ $i -ge 50 ] && ! tmux -L "$2" has-session -t "=$1" 2>/dev/null; then break; fi; sleep 0.1; i=$((i+1)); done; if mv -f "$f" "$f.dead" 2>/dev/null; then rm -f "$f.dead"; exit 3; fi"#;

/// How long (tenths of a second) a new card's shell gets to take its command.
const CONFIRM_TENTHS: u32 = 300;

/// How long (tenths of a second) the attach script waits for the new shell to be ready for its
/// first line; past that, nothing is typed (the command is then reported as not run).
const READY_TENTHS: u32 = 250;

/// Test-only: the `HOME` of everything a card runs on the Linux side (scratch rc files and
/// staging folder), forwarded explicitly since environment variables do not cross `wsl.exe`.
fn test_home() -> Option<String> {
    std::env::var("TCANVAS_TEST_HOME").ok().filter(|h| !h.is_empty())
}

/// `sh -c script args` on the Linux side, with `HOME` replaced when `home` is given.
fn sh_launch(windows: bool, home: Option<&str>, script: String, args: Vec<String>) -> Launch {
    let mut full = Vec::new();
    let program = match home {
        Some(h) => {
            full.extend([format!("HOME={h}"), "sh".into()]);
            "env"
        }
        None => "sh",
    };
    full.extend(["-c".into(), script]);
    full.extend(args);
    wsl_launch(windows, program, full)
}

/// Session names that can go into a staged file's path unquoted (the app's own: `tc_` + hex).
fn session_is_plain(session: &str) -> bool {
    !session.is_empty() && session.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A card's command as staged: Windows line endings (CRLF, or a lone CR) become LF, NUL bytes
/// (which no shell reads) go, trailing line breaks go. `None` for a blank command.
pub fn staged_text(command: &str) -> Option<String> {
    let text: String = command.replace("\r\n", "\n").replace('\r', "\n").chars().filter(|c| *c != '\0').collect();
    let text = text.trim_end_matches('\n');
    (!text.trim().is_empty()).then(|| text.to_string())
}

/// The staged file the shell sources: one fixed first line, then the command exactly as written.
///
/// The first line shows the command, dimmed (`sed` prints the file from its second line, with the
/// escape codes around it), so the card says what was launched; then it deletes the file. If the
/// file is already gone (claimed by `CONFIRM_SCRIPT`: the command is reported as not run), it
/// returns without running anything. Tools are called by absolute path (`/bin` holds `sed` and
/// `rm` with or without merged `/usr`), so no alias or function of the user's (`rm`, `sed`,
/// `command`...) can step in. Valid in bash, zsh and fish (`||`, `return` from a sourced file).
pub fn staged_file(session: &str, text: &str) -> String {
    let path = format!("{STAGE_DIR}/{session}");
    format!("/bin/sed -n -e '2s/^/\x1b[2m/' -e '$s/$/\x1b[0m/' -e '2,$p' {path}; /bin/rm {path} 2>/dev/null || return\n{text}\n")
}

/// The staged file for a setup followed by a command: the command runs only if the setup
/// succeeded (its last command), and the card shows both.
///
/// The setup waits in comment lines (`#` + the line: no shell runs them). The first line claims
/// the file (`mv`, racing `CONFIRM_SCRIPT` on one name like the plain file's `rm`), shows both
/// texts dimmed, writes the setup lines (from the line deleting their own file) into a file next
/// to it, `-setup` (by `sed` itself: no shell redirection, which `noclobber` could refuse), and
/// deletes the claimed copy. Then the shell sources the setup file through `builtin source` (an
/// alias or a function named `source` in the user's rc files cannot step in), and if it failed,
/// deletes the setup file (in case it never ran) and returns before the command; the next line
/// deletes it in any case. Every line of ours is valid in bash, zsh and fish 3 (`builtin
/// source`, `||`, `!`, `return` from a sourced file); the two texts reach the shell byte for
/// byte.
pub fn staged_file_with_setup(session: &str, setup: &str, text: &str) -> String {
    let path = format!("{STAGE_DIR}/{session}");
    let claimed = format!("{path}-run");
    let setup_file = format!("{path}-setup");
    let setup_lines = setup.split('\n').count();
    // Line 2 deletes the setup file, the setup is on lines 3 to `last`, then the `source` line and
    // the line deleting the setup file, then the command.
    let last = 2 + setup_lines;
    let command = last + 3;
    let commented: String = setup.split('\n').map(|line| format!("#{line}\n")).collect();
    let show = format!("/bin/sed -n -e '3,{last}s/^#//' -e '3s/^/\x1b[2m/' -e '$s/$/\x1b[0m/' -e '3,{last}p' -e '{command},$p' {claimed}");
    let write = format!("/bin/sed -n -e '2,{last}s/^#//' -e \"2,{last}w $HOME/.local/share/tcanvas/commands/{session}-setup\" {claimed}");
    format!(
        "/bin/mv {path} {claimed} 2>/dev/null || return; {show}; {write}; /bin/rm -f {claimed}\n#/bin/rm -f {setup_file}\n{commented}builtin source {setup_file} || ! /bin/rm -f {setup_file} || return\n/bin/rm -f {setup_file}\n{text}\n"
    )
}

/// What a new card runs once: a setup (a worktree recipe's), then its command only if the setup
/// succeeded; either may be missing, not both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Start {
    pub setup: Option<String>,
    pub command: Option<String>,
}

impl Start {
    /// Each text as staged (`staged_text`); `None` when both are blank.
    pub fn new(setup: Option<&str>, command: Option<&str>) -> Option<Start> {
        let (setup, command) = (setup.and_then(staged_text), command.and_then(staged_text));
        (setup.is_some() || command.is_some()).then_some(Start { setup, command })
    }

    /// A command alone.
    pub fn command(text: &str) -> Start {
        Start { setup: None, command: Some(text.to_string()) }
    }

    /// Both texts, the setup first, as offered for copying when they did not run.
    pub fn text(&self) -> String {
        [self.setup.as_deref(), self.command.as_deref()].into_iter().flatten().collect::<Vec<_>>().join("\n")
    }

    /// The staged file for this start.
    pub fn file(&self, session: &str) -> String {
        match (self.setup.as_deref(), self.command.as_deref()) {
            (Some(setup), Some(command)) => staged_file_with_setup(session, setup, command),
            (Some(text), None) | (None, Some(text)) => staged_file(session, text),
            (None, None) => staged_file(session, ""),
        }
    }
}

/// Stores a card's command on the Linux side before its first attach (`sh` reading stdin,
/// through `wsl.exe` on Windows). The text travels on a pipe, never on a command line: no
/// quoting, no length limit of Windows or tmux arguments.
pub fn stage_command(session: &str, start: &Start) -> io::Result<()> {
    if !session_is_plain(session) {
        return Err(io::Error::other(format!("session name {session:?}")));
    }
    let out = run_with_stdin(&stage_launch(cfg!(windows), test_home().as_deref(), session), start.file(session).as_bytes())?;
    if out.status.success() { Ok(()) } else { Err(io::Error::other(format!("{} {}", out.status, String::from_utf8_lossy(&out.stderr).trim()))) }
}

/// Waits until the shell of a new session has taken its staged command (`Some(true)`), or the
/// command will never run (`Some(false)`: it is then deleted). When that cannot be told (the
/// call itself failed), the file is claimed right away: a command reported as not run never runs.
pub fn confirm_command(session: &str) -> Option<bool> {
    match settle(session, CONFIRM_TENTHS) {
        None => discard_command(session).map(|claimed| !claimed),
        known => known,
    }
}

/// Deletes a staged command its shell has not taken yet (card closed, attach failed...).
/// `Some(true)`: it was there and is now gone for good; `Some(false)`: no such file (already
/// taken, or never staged); `None`: the call failed.
pub fn discard_command(session: &str) -> Option<bool> {
    settle(session, 0).map(|taken| !taken)
}

fn settle(session: &str, tenths: u32) -> Option<bool> {
    let out = run_with_stdin(&confirm_launch(cfg!(windows), test_home().as_deref(), &protocol::tmux_socket(), session, tenths), b"").ok()?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(3) => Some(false),
        _ => None,
    }
}

fn run_with_stdin(l: &Launch, input: &[u8]) -> io::Result<std::process::Output> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(&l.program);
    cmd.args(&l.args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    launch::no_window(&mut cmd);
    let mut child = cmd.spawn()?;
    child.stdin.take().expect("piped stdin").write_all(input)?;
    child.wait_with_output()
}

fn stage_launch(windows: bool, home: Option<&str>, session: &str) -> Launch {
    sh_launch(windows, home, STAGE_SCRIPT.into(), vec!["tcanvas-stage".into(), session.into()])
}

fn confirm_launch(windows: bool, home: Option<&str>, socket: &str, session: &str, tenths: u32) -> Launch {
    sh_launch(windows, home, CONFIRM_SCRIPT.into(), vec!["tcanvas-confirm".into(), session.into(), socket.into(), tenths.to_string()])
}

/// Attaches the session, creating it if needed. tmux doesn't expand `~` in `-c`: we first
/// cd into the folder with `sh`, and tmux picks up the client's current folder.
///
/// `staged`: a command was staged for this session (`stage_command`), and the new session is
/// this size. See `run_staged`.
pub fn attach_command(session: &str, cwd: Option<&str>, staged: Option<GridSize>) -> Launch {
    attach_launch(cfg!(windows), test_home().as_deref(), &protocol::tmux_socket(), session, cwd, staged)
}

/// `attach_command` for a given platform and tmux socket (pure, for tests).
fn attach_launch(windows: bool, home: Option<&str>, socket: &str, session: &str, cwd: Option<&str>, staged: Option<GridSize>) -> Launch {
    let mut args = tmux_args(socket, &["new-session", "-A", "-s", session]);
    push_setup(&mut args);
    let tmux = quote_all(&args);
    let run = staged.filter(|_| session_is_plain(session)).map(|size| format!("{} ", run_staged(socket, session, size))).unwrap_or_default();
    let script = format!("{}; {run}exec tmux {tmux}", cd_script(cwd));
    sh_launch(windows, home, script, Vec::new())
}

/// Appends the canvas settings to a tmux command line, each after a `;`.
fn push_setup(args: &mut Vec<String>) {
    for cmd in TMUX_SETUP {
        args.push(";".into());
        args.extend(cmd.iter().map(|s| s.to_string()));
    }
}

fn quote_all(args: &[String]) -> String {
    args.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ")
}

/// The line typed into a new session's shell to run its staged command: a leading space (kept
/// out of the history by shells set to ignore such lines), then `.` (`source` for fish), then the
/// fixed path. Nothing the user wrote is ever typed, so no line editor can alter it.
fn typed_line(source: &str, session: &str) -> String {
    format!(" {source} {STAGE_DIR}/{session}")
}

/// Runs a staged command once, in the session's own shell.
///
/// The session is created detached, after the canvas settings (the terminal type and history
/// size apply to panes created after them; `new-session -d` fails if the session already exists:
/// an existing session never gets the command). A background job waits until the new shell is
/// ready for its first line: its terminal has left canonical mode (readline, zle...) and its
/// foreground process group is the shell itself (not a program its rc files started). Then it
/// types `typed_line` and Enter; if that never happens in time, it types nothing. The shell
/// sources the staged file: the command runs in the user's interactive login shell, with its
/// profile, rc files and hooks, byte for byte as written (sourced text goes through no line
/// editor: no history expansion, completion or key binding), its commands as jobs of that shell,
/// and the shell stays when it ends.
fn run_staged(socket: &str, session: &str, size: GridSize) -> String {
    let tmux = |args: &[&str]| format!("tmux {}", quote_all(&tmux_args(socket, args)));
    let (cols, rows) = (size.cols.to_string(), size.rows.to_string());
    let target = format!("={session}:");
    let mut create = tmux_args(socket, &["start-server"]);
    push_setup(&mut create);
    create.extend([";", "new-session", "-d", "-s", session, "-x", &cols, "-y", &rows].map(String::from));
    let create = format!("tmux {}", quote_all(&create));
    let tty = tmux(&["display-message", "-p", "-t", &target, "#{pane_tty}"]);
    let pid = tmux(&["display-message", "-p", "-t", &target, "#{pane_pid}"]);
    let shell = tmux(&["show-options", "-gv", "default-shell"]);
    let dot = sh_quote(&typed_line(".", session));
    let source = sh_quote(&typed_line("source", session));
    let type_line = tmux(&["send-keys", "-t", &target, "-l"]);
    let enter = tmux(&["send-keys", "-t", &target, "Enter"]);
    format!(
        r#"if {create} 2>/dev/null; then ( (p=$({tty}); s=$({pid}); i=0; until stty -a <"$p" 2>/dev/null | grep -q -- -icanon && [ "$(sed 's/.*) //' "/proc/$s/stat" 2>/dev/null | cut -d' ' -f6)" = "$s" ]; do [ $i -lt {READY_TENTHS} ] || exit; sleep 0.1; i=$((i+1)); done; case $({shell}) in */fish) l={source};; *) l={dot};; esac; {type_line} "$l" && {enter}) </dev/null >/dev/null 2>&1 & ); fi;"#
    )
}

pub fn kill_command(session: &str) -> Launch {
    in_wsl("tmux", tmux_args(&protocol::tmux_socket(), &["kill-session", "-t", session]))
}

/// Stops the session and **all** its processes, in the background.
/// `tcanvas-agent kill` also hunts down detached processes and ends with SIGKILL; if it is
/// missing or fails, at least close the session with tmux.
pub fn kill_session_in_background(session: String) {
    std::thread::spawn(move || {
        if !launch::succeeds(launch::agent(&["kill", &session])) {
            let l = kill_command(&session);
            let mut cmd = std::process::Command::new(&l.program);
            cmd.args(&l.args);
            launch::no_window(&mut cmd);
            launch::succeeds(cmd);
        }
    });
}

/// Relays terminal events to the UI.
#[derive(Clone)]
pub struct Proxy {
    card: CardId,
    tx: Sender<(CardId, TermEvent)>,
    ctx: egui::Context,
}

impl EventListener for Proxy {
    fn send_event(&self, event: TermEvent) {
        if !matches!(event, TermEvent::Wakeup | TermEvent::MouseCursorDirty | TermEvent::CursorBlinkingChange) {
            let _ = self.tx.send((self.card.clone(), event));
        }
        self.ctx.request_repaint();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

pub struct TermRuntime {
    pub term: Arc<FairMutex<Term<Proxy>>>,
    /// `None` for a canned terminal (fixed output, no PTY).
    notifier: Option<Notifier>,
    size: GridSize,
    cell_px: (u16, u16),
    /// Fraction of a wheel line not yet sent.
    pub wheel_acc: f32,
    /// Mouse button held and last reported cell (for drags).
    pub mouse_down: Option<(super::input::MouseButton, usize, usize)>,
}

impl TermRuntime {
    pub fn spawn(card: &CardId, launch: &Launch, size: GridSize, cell_px: (u16, u16), tx: Sender<(CardId, TermEvent)>, ctx: egui::Context) -> io::Result<Self> {
        let options = tty::Options {
            shell: Some(tty::Shell::new(launch.program.clone(), launch.args.clone())),
            working_directory: None,
            drain_on_exit: true,
            env: HashMap::from([("TERM".into(), "xterm-256color".into()), ("COLORTERM".into(), "truecolor".into())]),
            #[cfg(windows)]
            escape_args: true,
        };
        let window = window_size(size, cell_px);
        let window_id = card.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
        let pty = tty::new(&options, window, window_id)?;
        let proxy = Proxy { card: card.clone(), tx, ctx };
        let config = Config { scrolling_history: 2000, ..Config::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &term_size(size), proxy.clone())));
        let event_loop = EventLoop::new(term.clone(), proxy, pty, true, false)?;
        let notifier = Notifier(event_loop.channel());
        event_loop.spawn();
        Ok(Self { term, notifier: Some(notifier), size, cell_px, wheel_acc: 0.0, mouse_down: None })
    }

    /// A terminal showing fixed output, without a PTY (screenshots, UI tests). Input is ignored.
    pub fn canned(card: &CardId, size: GridSize, cell_px: (u16, u16), bytes: &[u8], tx: Sender<(CardId, TermEvent)>, ctx: egui::Context) -> Self {
        let proxy = Proxy { card: card.clone(), tx, ctx };
        let config = Config { scrolling_history: 2000, ..Config::default() };
        let mut term = Term::new(config, &term_size(size), proxy);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        Self { term: Arc::new(FairMutex::new(term)), notifier: None, size, cell_px, wheel_acc: 0.0, mouse_down: None }
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let bytes: Vec<u8> = bytes.into();
        if let Some(notifier) = self.notifier.as_ref().filter(|_| !bytes.is_empty()) {
            notifier.notify(bytes);
        }
    }

    pub fn size(&self) -> GridSize {
        self.size
    }

    pub fn window_size(&self) -> WindowSize {
        window_size(self.size, self.cell_px)
    }

    pub fn resize(&mut self, size: GridSize, cell_px: (u16, u16)) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.cell_px = cell_px;
        self.term.lock().resize(term_size(size));
        if let Some(notifier) = &mut self.notifier {
            notifier.on_resize(window_size(size, cell_px));
        }
    }

    pub fn scroll_display(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&self) {
        let mut term = self.term.lock();
        if term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
        }
    }

    /// Visible text, line by line (tests and previews).
    pub fn screen_text(&self) -> String {
        let term = self.term.lock();
        let content = term.renderable_content();
        let mut lines = vec![String::new(); self.size.rows as usize];
        let offset = content.display_offset as i32;
        for cell in content.display_iter {
            let line = (cell.point.line.0 + offset) as usize;
            if let Some(l) = lines.get_mut(line) {
                l.push(cell.c);
            }
        }
        lines.iter().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n")
    }
}

impl Drop for TermRuntime {
    fn drop(&mut self) {
        if let Some(notifier) = &self.notifier {
            let _ = notifier.0.send(Msg::Shutdown);
        }
    }
}

fn term_size(s: GridSize) -> TermSize {
    TermSize::new(s.cols as usize, s.rows as usize)
}

fn window_size(s: GridSize, cell_px: (u16, u16)) -> WindowSize {
    WindowSize { num_cols: s.cols, num_lines: s.rows, cell_width: cell_px.0, cell_height: cell_px.1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canned_terminal_shows_its_output_and_ignores_input() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let size = GridSize { cols: 20, rows: 3 };
        let mut rt = TermRuntime::canned(&"c1".to_string(), size, (8, 16), b"\x1b[32mok\x1b[0m line\r\nsecond", tx, egui::Context::default());
        assert_eq!(rt.screen_text(), "ok line\nsecond\n");
        rt.write(b"ignored".to_vec());
        rt.resize(GridSize { cols: 30, rows: 4 }, (8, 16));
        assert_eq!(rt.size(), GridSize { cols: 30, rows: 4 });
    }

    fn script(l: &Launch) -> &str {
        if cfg!(windows) {
            assert_eq!(l.program, "wsl.exe");
            assert_eq!(&l.args[..5], ["--cd", "~", "-e", "sh", "-c"]);
        } else {
            assert_eq!(l.program, "sh");
        }
        l.args.last().unwrap()
    }

    #[test]
    fn attach_creates_or_attaches_with_canvas_settings() {
        let l = attach_command("tc_ab12", Some("/home/me/my proj"), None);
        let s = script(&l);
        assert!(s.starts_with("cd '/home/me/my proj' 2>/dev/null || cd ~; exec tmux '-L' '"), "{s}");
        assert!(s.contains("'new-session' '-A' '-s' 'tc_ab12' ';'"), "{s}");
        assert!(s.contains("';' 'set' '-g' 'status' 'off'"));
        assert!(s.contains("';' 'set' '-g' 'mouse' 'on'"));
        assert!(s.contains("';' 'set' '-s' 'set-clipboard' 'on'"));
    }

    /// The tmux settings, as the attach script quotes them.
    const SETUP: &str = "';' 'set' '-g' 'status' 'off' ';' 'set' '-g' 'mouse' 'on' ';' 'set' '-g' 'history-limit' '50000' ';' 'set' '-g' 'default-terminal' 'tmux-256color' ';' 'set' '-s' 'set-clipboard' 'on' ';' 'set' '-s' 'escape-time' '0'";

    #[test]
    fn without_a_command_the_attach_line_is_unchanged() {
        let plain = format!("cd '/home/me/my proj' 2>/dev/null || cd ~; exec tmux '-L' 'tcanvas' 'new-session' '-A' '-s' 'tc_ab12' {SETUP}");
        let linux = attach_launch(false, None, "tcanvas", "tc_ab12", Some("/home/me/my proj"), None);
        assert_eq!(linux, Launch { program: "sh".into(), args: vec!["-c".into(), plain.clone()] });
        let windows = attach_launch(true, None, "tcanvas", "tc_ab12", Some("/home/me/my proj"), None);
        assert_eq!(windows, Launch { program: "wsl.exe".into(), args: ["--cd", "~", "-e", "sh", "-c", &plain].map(String::from).to_vec() });
    }

    #[test]
    fn a_test_home_is_set_for_everything_run_on_the_linux_side() {
        let l = attach_launch(true, Some("/tmp/scratch home"), "s", "tc_x", None, None);
        assert_eq!(l.args[..6], ["--cd", "~", "-e", "env", "HOME=/tmp/scratch home", "sh"]);
        assert_eq!(stage_launch(false, Some("/tmp/h"), "tc_x").args[..2], ["HOME=/tmp/h", "sh"]);
        assert_eq!(confirm_launch(false, Some("/tmp/h"), "s", "tc_x", 3).program, "env");
    }

    #[test]
    fn a_staged_command_is_sourced_by_the_new_session_only() {
        let expected = format!(
            "{}{}{}{}{}",
            format_args!(r#"cd ~/'it'\''s' 2>/dev/null || cd ~; if tmux '-L' 's' 'start-server' {SETUP} ';' 'new-session' '-d' '-s' 'tc_x' '-x' '100' '-y' '30' 2>/dev/null; then "#),
            r#"( (p=$(tmux '-L' 's' 'display-message' '-p' '-t' '=tc_x:' '#{pane_tty}'); s=$(tmux '-L' 's' 'display-message' '-p' '-t' '=tc_x:' '#{pane_pid}'); i=0; "#,
            r#"until stty -a <"$p" 2>/dev/null | grep -q -- -icanon && [ "$(sed 's/.*) //' "/proc/$s/stat" 2>/dev/null | cut -d' ' -f6)" = "$s" ]; do [ $i -lt 250 ] || exit; sleep 0.1; i=$((i+1)); done; "#,
            r#"case $(tmux '-L' 's' 'show-options' '-gv' 'default-shell') in */fish) l=' source ~/.local/share/tcanvas/commands/tc_x';; *) l=' . ~/.local/share/tcanvas/commands/tc_x';; esac; tmux '-L' 's' 'send-keys' '-t' '=tc_x:' '-l' "$l" && tmux '-L' 's' 'send-keys' '-t' '=tc_x:' 'Enter') </dev/null >/dev/null 2>&1 & ); fi; "#,
            format_args!("exec tmux '-L' 's' 'new-session' '-A' '-s' 'tc_x' {SETUP}"),
        );
        let size = Some(GridSize { cols: 100, rows: 30 });
        assert_eq!(attach_launch(false, None, "s", "tc_x", Some("~/it's"), size), Launch { program: "sh".into(), args: vec!["-c".into(), expected.clone()] });
        let windows = attach_launch(true, None, "s", "tc_x", Some("~/it's"), size);
        assert_eq!(windows, Launch { program: "wsl.exe".into(), args: ["--cd", "~", "-e", "sh", "-c", &expected].map(String::from).to_vec() });
        // A session name that would need quoting is never put in a typed path.
        assert_eq!(attach_launch(false, None, "s", "tc_x;y", None, size), attach_launch(false, None, "s", "tc_x;y", None, None));
        assert!(stage_command("tc_x y", &Start::command("echo")).is_err());
    }

    #[test]
    fn the_typed_line_is_fixed_and_plain() {
        assert_eq!(typed_line(".", "tc_ab12cd34"), " . ~/.local/share/tcanvas/commands/tc_ab12cd34");
        assert_eq!(typed_line("source", "tc_ab12cd34"), " source ~/.local/share/tcanvas/commands/tc_ab12cd34");
        assert!(session_is_plain("tc_ab12cd34"));
        for bad in ["", "tc x", "tc_'", "tc;x", "tc$x", "tc/x", "tc~x", "tc\nx"] {
            assert!(!session_is_plain(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_staged_file_shows_then_claims_itself_then_holds_the_command_unchanged() {
        let file = staged_file("tc_x", "echo a !b\n\tcat <<'EOF'\nx\nEOF");
        let path = "~/.local/share/tcanvas/commands/tc_x";
        let first = format!("/bin/sed -n -e '2s/^/\x1b[2m/' -e '$s/$/\x1b[0m/' -e '2,$p' {path}; /bin/rm {path} 2>/dev/null || return");
        assert_eq!(file, format!("{first}\necho a !b\n\tcat <<'EOF'\nx\nEOF\n"));
        assert!(!first.contains("command ") && !first.contains("tail"), "only absolute paths: no alias or function applies");
    }

    #[test]
    fn the_command_text_never_travels_on_a_command_line() {
        let stage = |windows| stage_launch(windows, None, "tc_x");
        assert_eq!(stage(false), Launch { program: "sh".into(), args: ["-c", STAGE_SCRIPT, "tcanvas-stage", "tc_x"].map(String::from).to_vec() });
        assert_eq!(stage(true), Launch { program: "wsl.exe".into(), args: ["--cd", "~", "-e", "sh", "-c", STAGE_SCRIPT, "tcanvas-stage", "tc_x"].map(String::from).to_vec() });
        let confirm = confirm_launch(true, None, "my socket", "tc_x", 300);
        assert_eq!(confirm.args[..6], ["--cd", "~", "-e", "sh", "-c", CONFIRM_SCRIPT]);
        assert_eq!(confirm.args[6..], ["tcanvas-confirm", "tc_x", "my socket", "300"]);
    }

    #[test]
    fn the_staged_text_is_the_command_as_written() {
        for cmd in NASTY {
            assert_eq!(staged_text(cmd).as_deref(), Some(cmd.trim_end_matches('\n')), "{cmd:?}");
        }
        assert_eq!(staged_text("echo a\r\nnpm test\r\n\r\n").as_deref(), Some("echo a\nnpm test"), "Windows line endings become LF");
        assert_eq!(staged_text("a\rb").as_deref(), Some("a\nb"), "a lone CR too");
        assert_eq!(staged_text("  indented").as_deref(), Some("  indented"));
        assert_eq!(staged_text("a\0b").as_deref(), Some("ab"), "NUL is dropped");
        for blank in ["", "  ", "\n\t\r\n"] {
            assert_eq!(staged_text(blank), None);
        }
    }

    /// Commands with every kind of character a line editor or a quoting layer could misread.
    const NASTY: &[&str] = &[
        "echo hello && ls",
        r#"echo "double" 'single' it's"#,
        "echo $HOME ${USER:-x} $(id -u) `whoami`",
        "a; b | c > /dev/null 2>&1 & wait",
        "printf 'line1\\nline2\\n'\nsecond line\n",
        "echo café 日本語 🎉",
        r"back\slash \\ \n %s %b",
        "{braces} [x] *glob? ~tilde #hash #{pane_id}",
        "echo !important !! !$ ^a^b",
        "tab\there\n# a comment line\necho trailing \\",
        "trailing ;",
    ];

    /// A scratch `HOME` and a throwaway tmux server, removed even when an assertion fails.
    #[cfg(unix)]
    struct Live {
        home: std::path::PathBuf,
        socket: String,
    }

    #[cfg(unix)]
    impl Live {
        fn new(name: &str) -> Self {
            let home = std::env::temp_dir().join(format!("tcanvas-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&home);
            std::fs::create_dir_all(&home).unwrap();
            Self { home, socket: format!("tcanvas-test-{name}-{}", std::process::id()) }
        }

        fn staged(&self) -> std::path::PathBuf {
            self.home.join(".local/share/tcanvas/commands")
        }

        fn sh(&self, l: &Launch, input: &[u8]) -> Option<i32> {
            let mut cmd = std::process::Command::new(&l.program);
            cmd.args(&l.args).stdin(std::process::Stdio::piped());
            let mut child = cmd.spawn().unwrap();
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input).unwrap();
            child.wait().unwrap().code()
        }

        fn stage(&self, session: &str, command: &str) {
            self.stage_start(session, &Start::new(None, Some(command)).unwrap());
        }

        fn stage_start(&self, session: &str, start: &Start) {
            assert_eq!(self.sh(&stage_launch(false, Some(&self.home.display().to_string()), session), start.file(session).as_bytes()), Some(0));
        }

        /// The attach script without its final `exec tmux new-session -A` (it needs a terminal).
        fn create(&self, session: &str, shell: &str) {
            let l = attach_launch(false, Some(&self.home.display().to_string()), &self.socket, session, Some(&self.home.display().to_string()), Some(GridSize { cols: 100, rows: 30 }));
            let mut args = l.args.clone();
            let last = args.len() - 1;
            args[last] = args[last].replacen("exec tmux", "true", 1);
            assert!(std::process::Command::new(&l.program).args(&args).env("SHELL", shell).status().unwrap().success());
        }

        fn confirm(&self, session: &str, tenths: u32) -> Option<i32> {
            self.sh(&confirm_launch(false, Some(&self.home.display().to_string()), &self.socket, session, tenths), b"")
        }

        fn tmux(&self, args: &[&str]) -> String {
            let out = std::process::Command::new("tmux").args(["-L", &self.socket]).args(args).output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        /// Waits up to 20 s for a file to hold `want`.
        fn wait_file(&self, name: &str, want: &str) -> String {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                let got = std::fs::read_to_string(self.home.join(name)).unwrap_or_default();
                if got == want || std::time::Instant::now() > deadline {
                    return got;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    #[cfg(unix)]
    impl Drop for Live {
        fn drop(&mut self) {
            let _ = std::process::Command::new("tmux").args(["-L", &self.socket, "kill-server"]).output();
            let _ = std::process::Command::new("sh").args(["-c", &format!("rm -f \"${{TMUX_TMPDIR:-/tmp}}/tmux-$(id -u)/{}\"", self.socket)]).status();
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    #[cfg(unix)]
    fn tmux_available() -> bool {
        std::process::Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
    }

    #[cfg(unix)]
    fn shell_path(name: &str) -> Option<String> {
        ["/usr/bin/", "/bin/"].iter().map(|d| format!("{d}{name}")).find(|p| std::path::Path::new(p).exists())
    }

    /// Definitions an rc file could hold that must not change what the staged file does.
    #[cfg(unix)]
    const HOSTILE_RC: &str = "command() { return 0; }\nrm() { echo fn-rm; }\nalias sed='echo alias-sed'\ntail() { echo fn-tail; }\nalias mv='echo alias-mv'\nsource() { echo fn-source; }\n";

    /// The whole Linux side on a fresh throwaway tmux server with a scratch `HOME`: bash with
    /// bracketed paste turned off, then zsh, as the user's shell, both with hostile rc files.
    /// Staging, creating the session, the shell sourcing the command, the confirmation.
    #[cfg(unix)]
    #[test]
    fn a_staged_command_runs_once_byte_for_byte_in_the_users_shell() {
        if !tmux_available() {
            return;
        }
        for shell in ["bash", "zsh"] {
            let Some(path) = shell_path(shell) else {
                eprintln!("{shell} missing: skipped");
                continue;
            };
            let live = Live::new(&format!("staged-{shell}"));
            // zsh without any rc file starts its setup menu; bash without bracketed paste, where
            // anything typed would go through history expansion, completion and key bindings.
            std::fs::write(live.home.join(".zshrc"), HOSTILE_RC).unwrap();
            std::fs::write(live.home.join(".bashrc"), format!("bind 'set enable-bracketed-paste off'\n{HOSTILE_RC}")).unwrap();
            std::fs::write(live.home.join(".bash_profile"), ". ~/.bashrc\n").unwrap();
            // `!word`, `!!`, a tab, a here-document, a comment line, a trailing backslash,
            // non-ASCII and several lines, from a Windows text field (CRLF).
            let command = "printf '%s|' 'a!b' \"!!\" '\t<tab' 'café 日本' \"it's $((6*7))\" > out.txt\r\ncat >> out.txt <<'EOF'\r\n!important $HOME\r\nEOF\r\n# a comment line\r\necho trailing \\\r\nend >> out.txt\r\nprintf '%s' \"$TERM\" > term.txt\r\necho once >> runs.txt\r\n";
            live.stage("tc_x", command);
            live.create("tc_x", &path);
            assert_eq!(live.confirm("tc_x", 300), Some(0), "{shell}: taken by the shell");
            assert!(!live.staged().join("tc_x").exists());
            let want = "a!b|!!|\t<tab|café 日本|it's 42|!important $HOME\ntrailing end\n";
            assert_eq!(live.wait_file("out.txt", want), want, "{shell}");
            assert_eq!(live.wait_file("runs.txt", "once\n"), "once\n");
            // A fresh server: the pane was still created with the canvas settings.
            assert_eq!(live.wait_file("term.txt", "tmux-256color"), "tmux-256color", "{shell}");
            assert_eq!(live.tmux(&["display-message", "-p", "-t", "=tc_x:", "#{history_limit}"]), "50000");
            // The command was shown (dimmed), and no rc definition stepped in.
            let screen = live.tmux(&["capture-pane", "-p", "-t", "=tc_x:"]);
            assert!(screen.contains("echo once >> runs.txt"), "{shell}: {screen}");
            assert!(!["fn-rm", "alias-sed", "fn-tail", "alias-mv", "fn-source"].iter().any(|m| screen.contains(m)), "{shell}: {screen}");
            assert!(live.tmux(&["send-keys", "-t", "=tc_x:", "echo typed > typed.txt", "Enter"]).is_empty());
            assert_eq!(live.wait_file("typed.txt", "typed\n"), "typed\n", "{shell}: the shell stays usable");
            // The session exists: attaching again never runs a command, even a newly staged one,
            // and the confirmation claims and deletes it.
            live.stage("tc_x", "echo twice >> runs.txt");
            live.create("tc_x", &path);
            assert_eq!(live.confirm("tc_x", 20), Some(3), "{shell}: reported as not run");
            assert!(!live.staged().join("tc_x").exists());
            assert_eq!(std::fs::read_to_string(live.home.join("runs.txt")).unwrap(), "once\n", "{shell}");
        }
    }

    /// A setup and a command (a worktree recipe's, then a template's), in bash then zsh with hostile
    /// rc files: the setup runs first, byte for byte, the command only if it succeeded; the card
    /// shows both; nothing is left in the staging folder.
    #[cfg(unix)]
    #[test]
    fn a_setup_runs_before_the_command_and_a_failed_one_stops_it() {
        if !tmux_available() {
            return;
        }
        for shell in ["bash", "zsh"] {
            let Some(path) = shell_path(shell) else { continue };
            let live = Live::new(&format!("setup-{shell}"));
            std::fs::write(live.home.join(".zshrc"), format!("setopt noclobber append_create\n{HOSTILE_RC}")).unwrap();
            std::fs::write(live.home.join(".bashrc"), format!("bind 'set enable-bracketed-paste off'\nset -o noclobber\n{HOSTILE_RC}")).unwrap();
            std::fs::write(live.home.join(".bash_profile"), ". ~/.bashrc\n").unwrap();
            // A setup with a comment, a here-document and `!`, its last command succeeding.
            let setup = "# seed\ncat >> order.txt <<'EOF'\nsetup !x $HOME\nEOF\ntest -f order.txt";
            live.stage_start("tc_ok", &Start::new(Some(setup), Some("echo command >> order.txt")).unwrap());
            live.create("tc_ok", &path);
            assert_eq!(live.confirm("tc_ok", 300), Some(0), "{shell}");
            let want = "setup !x $HOME\ncommand\n";
            assert_eq!(live.wait_file("order.txt", want), want, "{shell}");
            let screen = live.tmux(&["capture-pane", "-p", "-t", "=tc_ok:"]);
            assert!(screen.contains("setup !x $HOME") && screen.contains("echo command >> order.txt"), "{shell}: both shown\n{screen}");
            assert!(!["fn-rm", "alias-sed", "fn-tail", "alias-mv", "fn-source"].iter().any(|m| screen.contains(m)), "{shell}: {screen}");
            // A failing setup: the command never runs, the shell stays.
            live.stage_start("tc_ko", &Start::new(Some("echo failing >> fail.txt\nfalse"), Some("echo command >> fail.txt")).unwrap());
            live.create("tc_ko", &path);
            assert_eq!(live.confirm("tc_ko", 300), Some(0), "{shell}");
            assert_eq!(live.wait_file("fail.txt", "failing\n"), "failing\n", "{shell}");
            assert!(live.tmux(&["send-keys", "-t", "=tc_ko:", "echo typed > typed.txt", "Enter"]).is_empty());
            assert_eq!(live.wait_file("typed.txt", "typed\n"), "typed\n", "{shell}");
            assert_eq!(std::fs::read_to_string(live.home.join("fail.txt")).unwrap(), "failing\n", "{shell}: the command did not run");
            let left: Vec<_> = std::fs::read_dir(live.staged()).unwrap().map(|e| e.unwrap().file_name()).collect();
            assert!(left.is_empty(), "{shell}: {left:?}");
        }
    }

    #[test]
    fn a_start_is_a_setup_a_command_or_both() {
        assert_eq!(Start::new(None, Some(" ")), None);
        assert_eq!(Start::new(Some("\r\n"), None), None);
        assert_eq!(Start::new(Some("npm ci\r\n"), Some("claude")), Some(Start { setup: Some("npm ci".into()), command: Some("claude".into()) }));
        let both = Start::new(Some("npm ci\nnpm run seed"), Some("claude")).unwrap();
        assert_eq!(both.text(), "npm ci\nnpm run seed\nclaude");
        // Alone, either is the plain staged file.
        assert_eq!(Start::new(Some("npm ci"), None).unwrap().file("tc_x"), staged_file("tc_x", "npm ci"));
        assert_eq!(Start::command("claude").file("tc_x"), staged_file("tc_x", "claude"));
        // Both: the setup commented out, then sourced, the command after it as written.
        let file = both.file("tc_x");
        let lines: Vec<&str> = file.lines().collect();
        let dir = "~/.local/share/tcanvas/commands";
        assert!(lines[0].starts_with(&format!("/bin/mv {dir}/tc_x {dir}/tc_x-run 2>/dev/null || return; /bin/sed ")), "{}", lines[0]);
        assert_eq!(
            lines[1..],
            [
                format!("#/bin/rm -f {dir}/tc_x-setup").as_str(),
                "#npm ci",
                "#npm run seed",
                &format!("builtin source {dir}/tc_x-setup || ! /bin/rm -f {dir}/tc_x-setup || return"),
                &format!("/bin/rm -f {dir}/tc_x-setup"),
                "claude"
            ]
        );
        assert!(!lines[0].replace("2>/dev/null", "").contains('>'), "no shell redirection");
    }

    /// An rc file that waits for an answer (a passphrase...) never receives the typed line: it
    /// is not ready, nothing is typed, and the command is reported as not run.
    #[cfg(unix)]
    #[test]
    fn nothing_is_typed_into_a_shell_that_is_not_ready() {
        let Some(bash) = shell_path("bash").filter(|_| tmux_available()) else { return };
        let live = Live::new("not-ready");
        std::fs::write(live.home.join(".bashrc"), "read -r answer; printf '%s' \"$answer\" > answer.txt\n").unwrap();
        std::fs::write(live.home.join(".bash_profile"), ". ~/.bashrc\n").unwrap();
        live.stage("tc_x", "echo ran > ran.txt");
        live.create("tc_x", &bash);
        assert_eq!(live.confirm("tc_x", 40), Some(3), "not taken: claimed");
        assert!(live.tmux(&["send-keys", "-t", "=tc_x:", "-l", "typed-by-me"]).is_empty());
        assert!(live.tmux(&["send-keys", "-t", "=tc_x:", "Enter"]).is_empty());
        assert_eq!(live.wait_file("answer.txt", "typed-by-me"), "typed-by-me", "the rc file got only what the user typed");
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!live.home.join("ran.txt").exists());
    }

    /// Staging sweeps staged files older than 10 minutes, and nothing else.
    #[cfg(unix)]
    #[test]
    fn staging_sweeps_files_left_over_by_an_app_that_quit() {
        let live = Live::new("sweep");
        let dir = live.staged();
        std::fs::create_dir_all(&dir).unwrap();
        let outside = live.home.join("outside.txt");
        for f in ["tc_old", "tc_old.tmp", "tc_new", "other_old"] {
            std::fs::write(dir.join(f), "secret").unwrap();
        }
        std::fs::write(&outside, "keep").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("tc_link")).unwrap();
        let old = |p: &std::path::Path| assert!(std::process::Command::new("touch").args(["-h", "-d", "20 minutes ago"]).arg(p).status().unwrap().success());
        for f in ["tc_old", "tc_old.tmp", "other_old", "tc_link"] {
            old(&dir.join(f));
        }
        old(&outside);
        live.stage("tc_x", "echo x");
        let left: std::collections::BTreeSet<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, ["other_old", "tc_link", "tc_new", "tc_x"].map(String::from).into());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep", "never through a link");
    }

    #[test]
    fn cwd_variants() {
        assert_eq!(cd_script(None), "cd ~");
        assert_eq!(cd_script(Some("  ")), "cd ~");
        assert_eq!(cd_script(Some("~")), "cd ~");
        assert_eq!(cd_script(Some("~/code/api")), "cd ~/'code/api' 2>/dev/null || cd ~");
        assert_eq!(cd_script(Some("/tmp/it's café")), r"cd '/tmp/it'\''s café' 2>/dev/null || cd ~");
    }

    #[test]
    fn attach_script_runs_in_a_real_shell() {
        // The script must be valid sh: check it with `sh -n` (parse without executing).
        if cfg!(windows) {
            return;
        }
        for staged in [None, Some(GridSize { cols: 80, rows: 24 })] {
            let l = attach_command("tc_x", Some("~/it's"), staged);
            let ok = std::process::Command::new("sh").args(["-n", "-c", script(&l)]).status().unwrap().success();
            assert!(ok);
        }
    }

    #[test]
    fn kill_targets_the_session() {
        let l = kill_command("tc_x");
        assert!(l.args.ends_with(&["kill-session".into(), "-t".into(), "tc_x".into()]));
    }
}
