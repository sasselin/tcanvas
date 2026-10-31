//! Un terminal = un client tmux dans un PTY (ConPTY sous Windows) analysé par `alacritty_terminal`.

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

use crate::shell::launch;
use crate::shell::world::CardId;

/// Programme et arguments à lancer (dans le PTY ou en tâche de fond).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
}

/// Réglages imposés au serveur tmux du canevas à chaque attache.
const TMUX_SETUP: &[&[&str]] = &[
    &["set", "-g", "status", "off"],
    &["set", "-g", "mouse", "on"],
    &["set", "-g", "history-limit", "50000"],
    &["set", "-g", "default-terminal", "tmux-256color"],
    &["set", "-s", "set-clipboard", "on"],
    &["set", "-s", "escape-time", "0"],
];

/// Programme lancé dans WSL (via `wsl.exe` sous Windows, directement ailleurs).
fn in_wsl(program: &str, args: Vec<String>) -> Launch {
    if cfg!(windows) {
        let mut wsl = vec!["--cd".to_string(), "~".to_string(), "-e".to_string(), program.to_string()];
        wsl.extend(args);
        Launch { program: "wsl.exe".into(), args: wsl }
    } else {
        Launch { program: program.into(), args }
    }
}

fn tmux_args(args: &[&str]) -> Vec<String> {
    let mut full = vec!["-L".to_string(), protocol::tmux_socket()];
    full.extend(args.iter().map(|s| s.to_string()));
    full
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `cd` vers le dossier ; `~` reste hors des guillemets pour que le shell le développe.
fn cd_script(cwd: Option<&str>) -> String {
    match cwd.map(str::trim).filter(|c| !c.is_empty()) {
        None | Some("~") => "cd ~".into(),
        Some(c) => match c.strip_prefix("~/") {
            Some(rest) => format!("cd ~/{} 2>/dev/null || cd ~", sh_quote(rest)),
            None => format!("cd {} 2>/dev/null || cd ~", sh_quote(c)),
        },
    }
}

/// Attache la session, en la créant si besoin. tmux ne développe pas `~` dans `-c` : on se
/// place d'abord dans le dossier avec `sh`, et tmux reprend le dossier courant du client.
pub fn attach_command(session: &str, cwd: Option<&str>) -> Launch {
    let mut args = tmux_args(&["new-session", "-A", "-s", session]);
    for cmd in TMUX_SETUP {
        args.push(";".into());
        args.extend(cmd.iter().map(|s| s.to_string()));
    }
    let tmux = args.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ");
    let script = format!("{}; exec tmux {tmux}", cd_script(cwd));
    in_wsl("sh", vec!["-c".into(), script])
}

pub fn kill_command(session: &str) -> Launch {
    in_wsl("tmux", tmux_args(&["kill-session", "-t", session]))
}

/// Arrête la session et **tous** ses processus, en arrière-plan.
/// `tcanvas-agent kill` traque aussi les processus détachés et finit par SIGKILL ; s'il est
/// introuvable ou échoue, on ferme au moins la session avec tmux.
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

/// Relaie les événements du terminal vers l'interface.
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
    notifier: Notifier,
    size: GridSize,
    cell_px: (u16, u16),
    /// Fraction de ligne de molette pas encore envoyée.
    pub wheel_acc: f32,
    /// Bouton souris enfoncé et dernière cellule rapportée (pour les glissés).
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
        Ok(Self { term, notifier, size, cell_px, wheel_acc: 0.0, mouse_down: None })
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let bytes: Vec<u8> = bytes.into();
        if !bytes.is_empty() {
            self.notifier.notify(bytes);
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
        self.notifier.on_resize(window_size(size, cell_px));
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

    /// Texte visible, ligne par ligne (tests et aperçus).
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
        let _ = self.notifier.0.send(Msg::Shutdown);
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
        let l = attach_command("tc_ab12", Some("/home/me/my proj"));
        let s = script(&l);
        assert!(s.starts_with("cd '/home/me/my proj' 2>/dev/null || cd ~; exec tmux '-L' 'tcanvas' 'new-session' '-A' '-s' 'tc_ab12' ';'"), "{s}");
        assert!(s.contains("';' 'set' '-g' 'status' 'off'"));
        assert!(s.contains("';' 'set' '-g' 'mouse' 'on'"));
        assert!(s.contains("';' 'set' '-s' 'set-clipboard' 'on'"));
    }

    #[test]
    fn cwd_variants() {
        assert_eq!(cd_script(None), "cd ~");
        assert_eq!(cd_script(Some("  ")), "cd ~");
        assert_eq!(cd_script(Some("~")), "cd ~");
        assert_eq!(cd_script(Some("~/code/api")), "cd ~/'code/api' 2>/dev/null || cd ~");
        assert_eq!(cd_script(Some("/tmp/l'été")), r"cd '/tmp/l'\''été' 2>/dev/null || cd ~");
    }

    #[test]
    fn attach_script_runs_in_a_real_shell() {
        // Le script doit être du sh valide : on le vérifie avec `sh -n` (analyse sans exécution).
        if cfg!(windows) {
            return;
        }
        let l = attach_command("tc_x", Some("~/l'appli"));
        let ok = std::process::Command::new("sh").args(["-n", "-c", script(&l)]).status().unwrap().success();
        assert!(ok);
    }

    #[test]
    fn kill_targets_the_session() {
        let l = kill_command("tc_x");
        assert!(l.args.ends_with(&["kill-session".into(), "-t".into(), "tc_x".into()]));
    }
}
