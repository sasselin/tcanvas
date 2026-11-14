//! Text the app sends to a session (`AgentCmd::SendText`), pasted through tmux.
//!
//! The text never goes through the app's tmux client (whose keys a pane in copy mode would take
//! as copy-mode commands): the agent loads it into a private tmux buffer by stdin, and has tmux
//! paste it into the pane. `paste-buffer -p` brackets the paste only when the pane's application
//! asked for bracketed paste, decided by tmux at the moment it pastes; tmux turns each LF into
//! CR, as the Enter key. The Enter follows as its own key, a moment later.
//!
//! Each step is one tmux command run by the server, which checks the pane and acts on it with no
//! window in between: `paste-buffer` into a dead pane crashes the whole tmux server (tmux 3.2a),
//! and keys sent to a pane showing a tmux screen act on that screen. A pane in copy mode (or view
//! mode) still gets the paste, as is; copy mode is left only before the Enter, by the same
//! command that presses it.

use std::collections::HashMap;
use std::io::{self, Write};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{AgentEvent, Problem, SESSION_PREFIX};

/// Between the paste and its Enter. Shells take the Enter right away, but a TUI application (a
/// coding agent's input box) may handle a paste asynchronously: an Enter in the same read can
/// land inside the paste as a line break, or before it is shown, instead of submitting it.
pub const ENTER_DELAY: Duration = Duration::from_millis(250);

/// End of a bracketed paste.
const PASTE_END: &str = "\x1b[201~";

/// One text to send.
pub struct Request {
    pub id: u64,
    pub session: String,
    pub text: String,
    pub enter: bool,
    /// Past this, the text is never pasted.
    pub deadline: Option<Instant>,
}

/// Why a send stopped; `pasted`: the text is in the pane already (only the Enter failed).
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub problem: Problem,
    pub pasted: bool,
}

/// The text as pasted: CRLF becomes LF (tmux then turns every LF into CR), and the end marker of
/// a bracketed paste is removed, again and again until none is left (removing one can join the
/// halves of another), so the text can never close the paste early.
pub fn paste_text(text: &str) -> String {
    let mut text = text.replace("\r\n", "\n");
    while text.contains(PASTE_END) {
        text = text.replace(PASTE_END, "");
    }
    text
}

type Reply = Arc<dyn Fn(&AgentEvent) + Send + Sync>;

/// The sending threads: one per session, so requests to a session are done one after the
/// other, in the order received (a second text never lands between the first one and its
/// Enter), while a slow session never delays another. Each is answered with
/// `AgentEvent::TextSent`.
pub fn spawn(reply: impl Fn(&AgentEvent) + Send + Sync + 'static) -> Sender<Request> {
    let delay = ENTER_DELAY;
    let reply: Reply = Arc::new(reply);
    let (tx, rx) = mpsc::channel::<Request>();
    thread::spawn(move || {
        let mut workers: HashMap<String, Sender<Request>> = HashMap::new();
        for r in rx {
            let worker = workers.entry(r.session.clone()).or_insert_with(|| worker(reply.clone(), delay));
            let _ = worker.send(r);
        }
    });
    tx
}

fn worker(reply: Reply, delay: Duration) -> Sender<Request> {
    let (tx, rx) = mpsc::channel::<Request>();
    thread::spawn(move || {
        for r in rx {
            let (problem, pasted) = match send(&r.session, &r.text, r.enter, delay, r.deadline) {
                Ok(()) => (None, false),
                Err(f) => (Some(f.problem), f.pasted),
            };
            reply(&AgentEvent::TextSent { id: r.id, session: r.session, problem, pasted });
        }
    });
    tx
}

fn tmux(args: &[&str]) -> io::Result<Output> {
    Command::new("tmux").arg("-L").arg(protocol::tmux_socket()).args(args).stdin(Stdio::null()).output()
}

/// The app's own session names (`tc_` + letters, digits, `_`): they go into tmux commands as is.
fn session_is_plain(session: &str) -> bool {
    session.starts_with(SESSION_PREFIX) && session.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A private buffer name, never used twice by this agent (nor by another agent: its pid).
fn buffer_name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("tcanvas-send-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst))
}

/// What a guarded command printed: the last `tcanvas-*` line.
#[derive(Debug, PartialEq, Eq)]
enum Mark {
    /// The target is not this session (it does not exist).
    Gone,
    Dead,
    /// A tmux screen other than copy or view mode.
    Busy(String),
    Done,
}

fn mark(stdout: &str) -> Option<Mark> {
    let line = stdout.lines().rev().find(|l| l.starts_with("tcanvas-"))?;
    Some(match line.split_once(' ').map_or((line, ""), |(a, b)| (a, b)) {
        ("tcanvas-gone", _) => Mark::Gone,
        ("tcanvas-dead", _) => Mark::Dead,
        ("tcanvas-busy", mode) => Mark::Busy(mode.to_string()),
        ("tcanvas-done", _) => Mark::Done,
        _ => return None,
    })
}

/// A pane in copy mode or view mode (scrolled up, selecting, or showing a command's output).
const COPYISH: &str = "#{||:#{==:#{pane_mode},copy-mode},#{==:#{pane_mode},view-mode}}";

/// One tmux command, run by the server as a whole: against the active pane of `session`, prints
/// `tcanvas-gone` (no such session), `tcanvas-dead` (dead pane), `tcanvas-busy <mode>` (a tmux
/// screen other than copy or view mode), or runs `act`. `cleanup` runs first on refusal. The
/// commands are tmux syntax: blocks in braces, formats in single quotes (none holds a quote), the
/// session plain (`session_is_plain`).
fn guarded(session: &str, cleanup: &str, act: &str) -> Vec<String> {
    let t = format!("={session}:");
    let refuse = |what: &str| format!("{cleanup}display-message -p -t '{t}' '{what}'");
    let busy = format!("#{{&&:#{{pane_in_mode}},#{{?{COPYISH},0,1}}}}");
    let inner = format!("if-shell -F -t '{t}' '#{{pane_dead}}' {{ {} }} {{ if-shell -F -t '{t}' '{busy}' {{ {} }} {{ {act} }} }}", refuse("tcanvas-dead"), refuse("tcanvas-busy #{pane_mode}"));
    // Outside the target (no such session), the format is read elsewhere: the name tells.
    ["if-shell", "-F", "-t", &t, &format!("#{{!=:#{{session_name}},{session}}}"), &format!("{cleanup}display-message -p tcanvas-gone"), &inner].map(String::from).to_vec()
}

/// The paste: `paste-buffer -p -d` (the buffer goes once pasted, or is deleted on refusal).
pub fn paste_command(session: &str, buffer: &str) -> Vec<String> {
    let t = format!("={session}:");
    guarded(session, &format!("delete-buffer -b {buffer} ; "), &format!("paste-buffer -p -d -b {buffer} -t '{t}' ; display-message -p tcanvas-done"))
}

/// The Enter: copy mode (or view mode) left first, then Enter, unless that uncovers another tmux
/// screen (modes stack: copy mode over the clock), which would take it.
pub fn enter_command(session: &str) -> Vec<String> {
    let t = format!("={session}:");
    let act = format!(
        "if-shell -F -t '{t}' '#{{pane_in_mode}}' {{ send-keys -t '{t}' -X cancel }} ; if-shell -F -t '{t}' '#{{pane_in_mode}}' {{ display-message -p -t '{t}' 'tcanvas-busy #{{pane_mode}}' }} {{ send-keys -t '{t}' Enter ; display-message -p tcanvas-done }}"
    );
    guarded(session, "", &act)
}

/// Runs a guarded command: `Ok(())` when it acted.
fn run_guarded(session: &str, args: &[String]) -> Result<(), Problem> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = tmux(&args).map_err(|e| Problem::SendFailed { detail: e.to_string() })?;
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    match mark(&String::from_utf8_lossy(&out.stdout)) {
        Some(Mark::Done) => Ok(()),
        Some(Mark::Gone) => Err(Problem::SessionNotFound { session: session.to_string() }),
        Some(Mark::Dead) => Err(Problem::PaneDead { session: session.to_string() }),
        Some(Mark::Busy(mode)) => Err(Problem::PaneBusy { session: session.to_string(), mode }),
        None if stderr.contains("no server running") || stderr.contains("error connecting") || stderr.contains("can't find") => Err(Problem::SessionNotFound { session: session.to_string() }),
        None => Err(Problem::SendFailed { detail: stderr }),
    }
}

/// Pastes `text` into the active pane of `session`, then presses Enter after `delay` when asked.
/// Nothing is pasted once `deadline` has passed.
pub fn send(session: &str, text: &str, enter: bool, delay: Duration, deadline: Option<Instant>) -> Result<(), Failure> {
    let not_sent = |problem| Failure { problem, pasted: false };
    if !session_is_plain(session) {
        return Err(not_sent(Problem::SessionNotFound { session: session.to_string() }));
    }
    let expired = || deadline.is_some_and(|d| Instant::now() >= d);
    if expired() {
        return Err(not_sent(Problem::Expired));
    }
    let text = paste_text(text);
    let pasted = !text.is_empty();
    if pasted {
        let buffer = buffer_name();
        load_buffer(&buffer, text.as_bytes()).map_err(not_sent)?;
        if expired() {
            let _ = tmux(&["delete-buffer", "-b", &buffer]);
            return Err(not_sent(Problem::Expired));
        }
        if let Err(problem) = run_guarded(session, &paste_command(session, &buffer)) {
            // Refused by the command (buffer deleted there), or tmux failed: never left behind.
            let _ = tmux(&["delete-buffer", "-b", &buffer]);
            return Err(not_sent(problem));
        }
    }
    if enter {
        thread::sleep(delay);
        run_guarded(session, &enter_command(session)).map_err(|problem| Failure { problem, pasted })?;
    }
    Ok(())
}

/// Loads a tmux buffer from stdin: the text never travels on a command line.
fn load_buffer(name: &str, bytes: &[u8]) -> Result<(), Problem> {
    let failed = |e: io::Error| Problem::SendFailed { detail: e.to_string() };
    let mut child = Command::new("tmux")
        .arg("-L")
        .arg(protocol::tmux_socket())
        .args(["load-buffer", "-b", name, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(failed)?;
    let written = child.stdin.take().expect("piped stdin").write_all(bytes);
    let out = child.wait_with_output().map_err(failed)?;
    written.map_err(failed)?;
    if out.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(Problem::SendFailed { detail })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pasted_text_cannot_close_the_paste() {
        assert_eq!(paste_text("echo one\r\necho two\n"), "echo one\necho two\n");
        assert_eq!(paste_text("a\x1b[201~b"), "ab");
        assert_eq!(paste_text("a\x1b[20\x1b[201~1~b"), "ab", "no marker left once one is removed");
        assert_eq!(paste_text("\x1b[2\x1b[20\x1b[201~1~01~"), "");
        assert_eq!(paste_text("café 日本 \t🎉"), "café 日本 \t🎉");
        assert_eq!(paste_text("lone\rcr"), "lone\rcr", "a lone CR stays (what Enter sends)");
    }

    #[test]
    fn only_the_apps_own_session_names() {
        for bad in ["main", "tc_x:1", "tc_x.0", "tc_x y", "tc_'x", "tc_{x}", "tc_x;y", ""] {
            assert_eq!(send(bad, "x", false, Duration::ZERO, None), Err(Failure { problem: Problem::SessionNotFound { session: bad.into() }, pasted: false }), "{bad:?}");
        }
        assert!(session_is_plain("tc_ab12_cd"));
    }

    #[test]
    fn an_expired_request_is_never_pasted() {
        let past = Instant::now() - Duration::from_millis(1);
        assert_eq!(send("tc_x", "x", true, Duration::ZERO, Some(past)), Err(Failure { problem: Problem::Expired, pasted: false }));
    }

    #[test]
    fn buffer_names_are_never_reused() {
        let (a, b) = (buffer_name(), buffer_name());
        assert_ne!(a, b);
        assert!(a.starts_with(&format!("tcanvas-send-{}-", std::process::id())));
    }

    #[test]
    fn the_checks_and_the_action_are_one_tmux_command() {
        let paste = paste_command("tc_x", "b1");
        assert_eq!(paste[..6], ["if-shell", "-F", "-t", "=tc_x:", "#{!=:#{session_name},tc_x}", "delete-buffer -b b1 ; display-message -p tcanvas-gone"]);
        assert_eq!(paste.len(), 7, "{paste:?}");
        let inner = &paste[6];
        assert!(inner.starts_with("if-shell -F -t '=tc_x:' '#{pane_dead}' { delete-buffer -b b1 ; display-message"), "{inner}");
        assert!(inner.ends_with("{ paste-buffer -p -d -b b1 -t '=tc_x:' ; display-message -p tcanvas-done } }"), "{inner}");
        assert!(!inner.contains("cancel"), "copy mode is kept for the paste");
        let enter = enter_command("tc_x");
        assert!(enter[6].contains("send-keys -t '=tc_x:' -X cancel } ; if-shell -F -t '=tc_x:' '#{pane_in_mode}'"), "{}", enter[6]);
        assert!(enter[6].contains("send-keys -t '=tc_x:' Enter ; display-message -p tcanvas-done"));
    }

    #[test]
    fn marks() {
        assert_eq!(mark("tcanvas-done\n"), Some(Mark::Done));
        assert_eq!(mark("tcanvas-busy clock-mode\n"), Some(Mark::Busy("clock-mode".into())));
        assert_eq!(mark("noise\ntcanvas-dead\n"), Some(Mark::Dead));
        assert_eq!(mark("tcanvas-gone"), Some(Mark::Gone));
        assert_eq!(mark(""), None);
    }
}
