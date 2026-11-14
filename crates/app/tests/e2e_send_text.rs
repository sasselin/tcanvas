//! End to end: text sent to a card by the app (`Event::SendText`), delivered by the real
//! `tcanvas-agent` through tmux. Real PTYs (ConPTY + wsl.exe on Windows) + real tmux on throwaway
//! sockets, with a scratch `HOME`. On Linux, with bash, then zsh, as the user's shell; on
//! Windows, with the WSL user's own shell.
//!
//! Checks: a card whose shell is not up yet refuses text; a line sent with Enter runs; at a prompt
//! with bracketed paste on, a two-line text with Enter is submitted as one input (exactly one new
//! prompt, both lines run); a card scrolled up (tmux copy mode) gets the text exactly once and
//! leaves copy mode; a two-line text sent to `cat` arrives as two lines, each exactly once; Ctrl+C
//! still interrupts; a card whose session ended refuses the text and keeps it.
//!
//! On Windows, the Linux agent must be provided: `TCANVAS_E2E_AGENT=/wsl/path/tcanvas-agent`
//! (see `scripts/test-windows.sh`). Without it, the test is skipped.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tcanvas::features::agent_link::AgentMode;
use tcanvas::features::terminal_card::Launcher;
use tcanvas::shell::event::{Event, SendOutcome, SendRefusal};
use tcanvas::{App, Config};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Command run in WSL (directly on Linux).
fn in_wsl(program: &str) -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("wsl.exe");
        cmd.args(["-e", program]);
        cmd
    } else {
        Command::new(program)
    }
}

/// Path (on the WSL side) of the agent binary.
fn agent_path() -> Option<String> {
    if cfg!(windows) {
        return std::env::var("TCANVAS_E2E_AGENT").ok();
    }
    let ok = Command::new(env!("CARGO")).args(["build", "-q", "-p", "tcanvas-agent"]).current_dir(workspace()).status().unwrap().success();
    assert!(ok, "building the agent");
    let target = std::env::var("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|_| workspace().join("target"));
    Some(target.join("debug/tcanvas-agent").display().to_string())
}

fn harness(dir: &Path) -> Harness<'static, App> {
    let dir = dir.to_path_buf();
    Harness::builder()
        .with_size(Vec2::new(1400.0, 900.0))
        .with_step_dt(1.0 / 60.0)
        .allow_missing_glyphs()
        .build_eframe(move |cc| App::new(&cc.egui_ctx, Config { launcher: Launcher::Tmux, agent: AgentMode::Spawn, config_dir: dir, os_toasts: false, update_check: false }))
}

/// Runs the app in real time until `cond` holds (`false` on timeout).
fn wait_for(h: &mut Harness<'_, App>, secs: u64, cond: impl Fn(&App) -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        h.step();
        if cond(h.state()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    false
}

fn wait(h: &mut Harness<'_, App>, what: &str, secs: u64, cond: impl Fn(&App) -> bool) {
    let start = Instant::now();
    if wait_for(h, secs, cond) {
        eprintln!("✓ {what} in {} ms", start.elapsed().as_millis());
        return;
    }
    let app = h.state();
    let screens: Vec<String> = app.world.cards.iter().map(|c| app.cards.screen_text(&c.id).unwrap_or_default()).collect();
    panic!("timed out: {what}\nagent: {:?}\nscreens:\n{}", app.agent_state(), screens.join("\n-----\n"));
}

/// Sends text to a card the way any slice does, and returns what the sender is told once the
/// agent answered.
fn send(h: &mut Harness<'_, App>, id: &str, text: &str, enter: bool) -> SendOutcome {
    h.state_mut().dispatch(vec![Event::SendText { card: id.into(), text: text.into(), enter }]);
    wait(h, &format!("outcome of {text:?}"), 15, |app| app.cards.last_outcome(id).is_some());
    h.state().cards.last_outcome(id).cloned().unwrap()
}

/// A file on the Linux side ("" when missing).
fn read_wsl(path: &str) -> String {
    in_wsl("cat").arg(path).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Steps the app until a file on the Linux side holds `want`.
fn wait_file(h: &mut Harness<'_, App>, path: &str, want: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if read_wsl(path) == want {
            eprintln!("✓ {path} holds {want:?}");
            return;
        }
        h.step();
        std::thread::sleep(Duration::from_millis(50));
    }
    let screen = h.state().world.cards.iter().map(|c| h.state().cards.screen_text(&c.id).unwrap_or_default()).collect::<Vec<_>>().join("\n-----\n");
    panic!("{path}: {:?} instead of {want:?}\nscreens:\n{screen}", read_wsl(path));
}

/// Steps the app until `count()` reaches `want`, then a moment more: it must stay there.
fn wait_count(h: &mut Harness<'_, App>, what: &str, want: usize, count: impl Fn() -> usize) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while count() < want {
        assert!(Instant::now() < deadline, "{what}: {} instead of {want}", count());
        h.step();
        std::thread::sleep(Duration::from_millis(50));
    }
    let settle = Instant::now() + Duration::from_millis(600);
    while Instant::now() < settle {
        h.step();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(count(), want, "{what}");
    eprintln!("✓ {what}");
}

/// Kills the throwaway tmux server, removes its socket, the config dir and the WSL folders, even
/// when the test fails.
struct Cleanup {
    socket: String,
    dir: PathBuf,
    folders: Vec<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = in_wsl("tmux").args(["-L", &self.socket, "kill-server"]).output();
        let rm = format!("rm -f \"${{TMUX_TMPDIR:-/tmp}}/tmux-$(id -u)/{}\"", self.socket);
        let _ = in_wsl("sh").args(["-c", &rm]).output();
        for path in self.folders.iter().filter(|p| !p.is_empty()) {
            let _ = in_wsl("rm").args(["-rf", path]).output();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A new folder on the Linux side.
fn mktemp_wsl(template: &str) -> String {
    let out = in_wsl("mktemp").args(["-d", template]).output().unwrap();
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(out.status.success() && !path.is_empty(), "mktemp in WSL");
    path
}

/// The shells to try: on Linux bash and zsh when installed (path), on Windows the user's own.
fn shells() -> Vec<Option<String>> {
    if cfg!(windows) {
        return vec![None];
    }
    ["bash", "zsh"].iter().filter_map(|s| ["/usr/bin/", "/bin/"].iter().map(|d| format!("{d}{s}")).find(|p| Path::new(p).exists())).map(Some).collect()
}

#[test]
fn text_sent_to_a_card() {
    if !in_wsl("tmux").arg("-V").output().is_ok_and(|o| o.status.success()) {
        eprintln!("tmux missing: test skipped");
        return;
    }
    let Some(agent) = agent_path() else {
        eprintln!("TCANVAS_E2E_AGENT missing: test skipped");
        return;
    };
    for shell in shells() {
        let name = shell.as_deref().map(|s| s.rsplit('/').next().unwrap_or(s)).unwrap_or("default").to_string();
        eprintln!("--- shell: {name}");
        let socket = format!("tcanvas-e2e-send-{name}-{}", std::process::id());
        let dir = std::env::temp_dir().join(format!("tcanvas-e2e-send-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut cleanup = Cleanup { socket: socket.clone(), dir: dir.clone(), folders: Vec::new() };
        let folder = mktemp_wsl("/tmp/tcanvas-e2e-send-XXXXXX");
        cleanup.folders.push(folder.clone());
        // Never the user's real home: scratch rc files that count prompts (one line per prompt).
        let home = mktemp_wsl("/tmp/tcanvas-e2e-send-home-XXXXXX");
        cleanup.folders.push(home.clone());
        let rc = r#"cd "$1" && printf '%s\n' "PS1='\$ '" "PROMPT_COMMAND='echo p >> ~/prompts.txt'" > .bashrc && echo '. ~/.bashrc' > .bash_profile && printf '%s\n' "PS1='%% '" 'precmd() { echo p >> ~/prompts.txt; }' > .zshrc"#;
        assert!(in_wsl("sh").args(["-c", rc, "rc", &home]).status().unwrap().success());
        // SAFETY: no app thread is running between two shells (each harness is dropped).
        unsafe {
            std::env::set_var("TCANVAS_TMUX_SOCKET", &socket);
            std::env::set_var("TCANVAS_TEST_HOME", &home);
            std::env::set_var("TCANVAS_AGENT", &agent);
            if let Some(path) = &shell {
                std::env::set_var("SHELL", path);
            }
        }
        scenario(&dir, &folder, &home, &socket, shell.is_some());
        drop(cleanup);
    }
}

/// `strict`: bash or zsh with the scratch rc files (Linux): prompts are counted and bracketed
/// paste is on, so the one-input check must run.
fn scenario(dir: &Path, folder: &str, home: &str, socket: &str, strict: bool) {
    let mut h = harness(dir);
    h.step();
    let group = tcanvas::shell::world::Project {
        id: "send".into(),
        name: "send".into(),
        color: [94, 129, 244],
        rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(2000.0, 1500.0)),
        cwd: Some(folder.into()),
        templates: Vec::new(),
        messages: Vec::new(),
        worktree_location: None,
    };
    h.state_mut().world.projects.push(group);
    wait(&mut h, "agent connected", 30, |app| matches!(app.agent_state(), tcanvas::features::agent_link::LinkState::Connected { .. }));
    h.state_mut().dispatch(vec![Event::NewCard { at: None, project: Some("send".into()), command: None, cwd: None }]);
    let id = h.state().world.cards[0].id.clone();
    // 0. Not up yet (no shell shown, session not seen): refused, kept.
    assert_eq!(send(&mut h, &id, "echo too-early > early.txt", true), SendOutcome::Refused(SendRefusal::Starting));
    assert!(h.state().cards.unsent_text(&id).is_some());
    wait(&mut h, "terminal up", 30, |app| app.cards.is_running(&id));
    wait(&mut h, "focus", 5, |app| app.world.focused.as_deref() == Some(id.as_str()));
    let prompts_file = format!("{home}/prompts.txt");
    let prompts = || read_wsl(&prompts_file).lines().count();
    // Both rc files count prompts; another shell (Windows: the user's own) may not.
    let counted = wait_for(&mut h, 20, |_| prompts() >= 1);
    assert!(counted || !strict, "the scratch rc files count prompts");
    wait(&mut h, "session seen by the agent", 20, |app| app.world.card(&id).is_some_and(|c| c.info.is_some()));
    let at = |file: &str| format!("{folder}/{file}");

    // 1. A line with Enter runs (and the earlier refusal is cleared).
    let before = prompts();
    assert_eq!(send(&mut h, &id, "echo single-$((3*3)) > single.txt", true), SendOutcome::Sent);
    wait_file(&mut h, &at("single.txt"), "single-9\n");
    assert_eq!(h.state().cards.unsent_text(&id), None);
    if counted {
        wait_count(&mut h, "one prompt after one line", before + 1, prompts);
    }

    // 2. At a prompt with bracketed paste on, two lines and Enter: one input, both lines run.
    let bracketed = wait_for(&mut h, 5, |app| app.cards.bracketed_paste(&id));
    assert!(bracketed || !strict, "bash and zsh turn bracketed paste on at their prompt");
    let before = prompts();
    assert_eq!(send(&mut h, &id, "echo one-$((1+1)) >> multi.txt\r\necho two-$((2+2)) >> multi.txt\n", true), SendOutcome::Sent);
    wait_file(&mut h, &at("multi.txt"), "one-2\ntwo-4\n");
    if bracketed && counted {
        wait_count(&mut h, "submitted as one input: a single new prompt", before + 1, prompts);
    } else {
        eprintln!("one-input check skipped (bracketed paste {bracketed}, prompts counted {counted})");
    }

    // 3. Scrolled up: the pane is in tmux copy mode, where the terminal's keys would be copy-mode
    //    commands. The text still reaches the shell, exactly once, and copy mode is left.
    let target = format!("=tc_{id}:");
    let in_mode = || {
        let out = in_wsl("tmux").args(["-L", socket, "display-message", "-p", "-t", &target, "#{pane_in_mode}"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert!(in_wsl("tmux").args(["-L", socket, "copy-mode", "-t", &target]).status().unwrap().success());
    assert_eq!(in_mode(), "1");
    let before = prompts();
    assert_eq!(send(&mut h, &id, "echo scrolled >> scrolled.txt", true), SendOutcome::Sent);
    wait_file(&mut h, &at("scrolled.txt"), "scrolled\n");
    assert_eq!(in_mode(), "0", "copy mode left");
    if counted {
        wait_count(&mut h, "one prompt after the text sent in copy mode", before + 1, prompts);
    }
    assert_eq!(read_wsl(&at("scrolled.txt")), "scrolled\n", "exactly once");

    // 4. Two lines with Enter to `cat` (no bracketed paste), on a screen cleared just before (a
    //    blank screen does not make the card "starting" again): each line once, as written.
    assert_eq!(send(&mut h, &id, "clear; cat > cat.txt", true), SendOutcome::Sent);
    wait(&mut h, "blank screen", 10, |app| app.cards.screen_text(&id).is_some_and(|s| s.trim().is_empty()));
    // `cat` is running once the shell has created its output file. The card's own view of
    // bracketed paste is not a usable signal here: it did not turn off on the CI runner's tmux.
    wait(&mut h, "cat started", 10, |_| in_wsl("test").args(["-e", &at("cat.txt")]).status().is_ok_and(|s| s.success()));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(send(&mut h, &id, "line one\nline two\n", true), SendOutcome::Sent);
    wait_file(&mut h, &at("cat.txt"), "line one\nline two\n");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(read_wsl(&at("cat.txt")), "line one\nline two\n", "exactly once, no extra empty line");
    // Ctrl+C still interrupts it.
    let before = prompts();
    h.event(egui::Event::Copy);
    h.step();
    if counted {
        wait(&mut h, "back at the prompt after Ctrl+C", 10, |_| prompts() > before);
    } else {
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(send(&mut h, &id, "echo after-cat > after.txt", true), SendOutcome::Sent);
    wait_file(&mut h, &at("after.txt"), "after-cat\n");
    assert_eq!(read_wsl(&at("early.txt")), "", "the text refused at the start never ran");

    // 5. The session ends: the card refuses text, and keeps it for the user.
    assert_eq!(send(&mut h, &id, "exit", true), SendOutcome::Sent);
    wait(&mut h, "session ended", 20, |app| app.world.card(&id).is_some_and(|c| c.pty_dead));
    assert_eq!(send(&mut h, &id, "too late\nreally", true), SendOutcome::Refused(SendRefusal::Ended));
    assert_eq!(h.state().cards.unsent_text(&id), Some(("too late\nreally", &SendRefusal::Ended)));
    let copy = (tcanvas::shell::i18n::t()).copy_text;
    let mut shown = false;
    for _ in 0..60 {
        h.step();
        if h.query_by_label(copy).is_some() {
            shown = true;
            break;
        }
    }
    assert!(shown, "{copy:?} never shown");
    // A card that is gone: the sender is told, nothing is kept.
    assert_eq!(send(&mut h, "no-such-card", "hello", true), SendOutcome::Refused(SendRefusal::NoCard));
    assert_eq!(h.state().cards.unsent_text("no-such-card"), None);
}
