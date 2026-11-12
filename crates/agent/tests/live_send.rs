//! Live test: text sent through the real `tcanvas-agent` binary to sessions of a throwaway tmux
//! server, with bash (then zsh) in a scratch `HOME`. Skipped cleanly without tmux.
//!
//! Checks: a two-line text and Enter at a prompt with bracketed paste is one input; a pane in
//! copy mode gets the paste and stays in copy mode without Enter, and with Enter is left and gets
//! the text exactly once; `cat` gets the lines as typed lines; a dead pane is refused and the
//! server (every other session) survives; a pane showing a tmux screen is refused, and a paste
//! whose Enter would land in one is reported as pasted, not submitted; a missing session and a
//! session that is not the canvas's are refused, without leaving a buffer behind; an expired
//! request is never delivered; sends are ordered per session, and a busy session does not delay
//! another; a text of 300 KB arrives whole.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{AgentCmd, AgentEvent, Problem};

const AGENT: &str = env!("CARGO_BIN_EXE_tcanvas-agent");

/// A throwaway tmux server whose shells live in a scratch `HOME`, removed even on failure. It
/// reads no tmux configuration (`-f /dev/null`).
struct Server {
    socket: String,
    home: PathBuf,
    shell: String,
}

impl Server {
    fn new(name: &str, shell: &str) -> Self {
        let home = std::env::temp_dir().join(format!("tcanvas-live-send-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // One line per prompt, one file per pane (tc_a is %0): tells one input from several.
        std::fs::write(home.join(".bashrc"), "PS1='$ '\nPROMPT_COMMAND='echo p >> ~/prompts$TMUX_PANE.txt'\n").unwrap();
        std::fs::write(home.join(".bash_profile"), ". ~/.bashrc\n").unwrap();
        std::fs::write(home.join(".zshrc"), "PS1='%% '\nprecmd() { echo p >> ~/prompts$TMUX_PANE.txt; }\n").unwrap();
        let server = Self { socket: format!("tcanvas-live-send-{name}-{}", std::process::id()), home, shell: shell.into() };
        for session in ["tc_a", "tc_b", "tc_c", "other"] {
            server.new_session(session, None);
        }
        server
    }

    fn new_session(&self, session: &str, command: Option<&str>) {
        let mut cmd = Command::new("tmux");
        cmd.args(["-f", "/dev/null", "-L", &self.socket, "new-session", "-d", "-s", session, "-x", "100", "-y", "30", "-c"]).arg(&self.home).env("HOME", &self.home).env("SHELL", &self.shell);
        cmd.args(command);
        assert!(cmd.status().unwrap().success());
    }

    fn tmux(&self, args: &[&str]) -> String {
        let out = Command::new("tmux").arg("-L").arg(&self.socket).args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn alive(&self, session: &str) -> bool {
        Command::new("tmux").args(["-L", &self.socket, "has-session", "-t", &format!("={session}")]).output().unwrap().status.success()
    }

    fn mode(&self, session: &str) -> String {
        self.tmux(&["display-message", "-p", "-t", &format!("={session}:"), "#{pane_in_mode} #{pane_mode}"])
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.home.join(name)).unwrap_or_default()
    }

    fn prompts(&self) -> usize {
        self.file("prompts%0.txt").lines().count()
    }

    /// Waits up to 15 s for `cond`.
    fn wait(&self, what: &str, cond: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !cond(self) {
            assert!(Instant::now() < deadline, "timed out: {what}\nscreen:\n{}", self.tmux(&["capture-pane", "-p", "-t", "=tc_a:"]));
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_file(&self, name: &str, want: &str) {
        self.wait(&format!("{name} = {want:?} (has {:?})", self.file(name)), |s| s.file(name) == want);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["-L", &self.socket, "kill-server"]).output();
        let _ = Command::new("sh").args(["-c", &format!("rm -f \"${{TMUX_TMPDIR:-/tmp}}/tmux-$(id -u)/{}\"", self.socket)]).status();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// An answer: (request, problem, pasted).
type Answer = (u64, Option<Problem>, bool);

struct Agent {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<AgentEvent>,
}

impl Agent {
    fn spawn(socket: &str) -> Self {
        let mut child = Command::new(AGENT).env("TCANVAS_TMUX_SOCKET", socket).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, events) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(protocol::decode(&line).expect("valid JSON line"));
            }
        });
        Agent { child, stdin, events }
    }

    fn send_limited(&mut self, id: u64, session: &str, text: &str, enter: bool, limit_ms: Option<u64>) {
        let cmd = AgentCmd::SendText { id, session: session.into(), text: text.into(), enter, limit_ms };
        self.stdin.write_all(protocol::encode(&cmd).as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn send(&mut self, id: u64, session: &str, text: &str, enter: bool) {
        self.send_limited(id, session, text, enter, None);
    }

    /// The answers to the given ids, in the order they came.
    fn answers(&self, ids: &[u64]) -> Vec<Answer> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut got = Vec::new();
        while got.len() < ids.len() {
            let left = deadline.checked_duration_since(Instant::now()).expect("answers in time");
            if let Ok(AgentEvent::TextSent { id, problem, pasted, .. }) = self.events.recv_timeout(left)
                && ids.contains(&id)
            {
                got.push((id, problem, pasted));
            }
        }
        got
    }

    fn answer(&mut self, id: u64, session: &str, text: &str, enter: bool) -> (Option<Problem>, bool) {
        self.send(id, session, text, enter);
        let (_, problem, pasted) = self.answers(&[id]).remove(0);
        (problem, pasted)
    }

    fn sent(&mut self, id: u64, session: &str, text: &str, enter: bool) -> Option<Problem> {
        let (problem, pasted) = self.answer(id, session, text, enter);
        assert!(!pasted || problem.is_none(), "{problem:?}");
        problem
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn shell_path(name: &str) -> Option<String> {
    ["/usr/bin/", "/bin/"].iter().map(|d| format!("{d}{name}")).find(|p| Path::new(p).exists())
}

#[test]
fn text_is_pasted_through_tmux() {
    if !Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success()) {
        eprintln!("tmux missing: test skipped");
        return;
    }
    for name in ["bash", "zsh"] {
        let Some(shell) = shell_path(name) else {
            eprintln!("{name} missing: skipped");
            continue;
        };
        let server = Server::new(name, &shell);
        server.wait("first prompt", |s| s.prompts() >= 1);
        let mut agent = Agent::spawn(&server.socket);
        let hello = agent.events.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(hello, AgentEvent::Hello { send_text: true, .. }), "{hello:?}");
        let session = || "tc_a";

        // 1. Two lines and Enter at a prompt: one input (one new prompt), both lines run.
        let before = server.prompts();
        assert_eq!(agent.sent(1, session(), "echo one >> m.txt\r\necho two >> m.txt", true), None);
        server.wait_file("m.txt", "one\ntwo\n");
        server.wait("the next prompt", |s| s.prompts() > before);
        thread::sleep(Duration::from_millis(500));
        assert_eq!(server.prompts(), before + 1, "{name}: one input, not one per line");

        // 2. Scrolled up (copy mode). Without Enter: pasted, the user's scroll position kept.
        server.tmux(&["copy-mode", "-t", "=tc_a:"]);
        assert_eq!(server.mode("tc_a"), "1 copy-mode");
        assert_eq!(agent.sent(2, session(), "echo kept >> cm.txt", false), None);
        assert_eq!(server.mode("tc_a"), "1 copy-mode", "still scrolled up");
        // With Enter: copy mode left, the line submitted once.
        assert_eq!(agent.sent(3, session(), "; echo scrolled >> cm.txt", true), None);
        server.wait_file("cm.txt", "kept\nscrolled\n");
        assert_eq!(server.mode("tc_a"), "0");
        // vi keys in copy mode: still left, nothing taken as a copy-mode command.
        server.tmux(&["set", "-g", "mode-keys", "vi"]);
        server.tmux(&["copy-mode", "-t", "=tc_a:"]);
        assert_eq!(agent.sent(4, session(), "echo vi >> cm.txt", true), None);
        server.wait_file("cm.txt", "kept\nscrolled\nvi\n");

        // 3. `cat` (no bracketed paste): each line once, as typed lines.
        let before = server.prompts();
        assert_eq!(agent.sent(5, session(), "cat > cat.txt", true), None);
        thread::sleep(Duration::from_millis(500));
        assert_eq!(agent.sent(6, session(), "line one\nline two", true), None);
        server.wait_file("cat.txt", "line one\nline two\n");
        server.tmux(&["send-keys", "-t", "=tc_a:", "C-c"]);
        server.wait("back at the prompt", |s| s.prompts() > before);

        // 4. A dead pane (its program exited, kept by `remain-on-exit`): refused before any
        //    paste, which would crash the whole server. Every other session lives on.
        server.tmux(&["set", "-g", "remain-on-exit", "on"]);
        server.new_session("tc_dead", Some("true"));
        server.wait("dead pane", |s| s.tmux(&["display-message", "-p", "-t", "=tc_dead:", "#{pane_dead}"]) == "1");
        assert_eq!(agent.sent(7, "tc_dead", "x", true), Some(Problem::PaneDead { session: "tc_dead".into() }));
        assert_eq!(agent.sent(8, "tc_dead", "", true), Some(Problem::PaneDead { session: "tc_dead".into() }), "nor an Enter");
        assert!(server.alive("tc_a") && server.alive("tc_dead"), "the server survives");
        assert_eq!(server.tmux(&["list-buffers"]), "", "no buffer left behind");

        // 5. A tmux screen other than copy mode: refused, nothing pasted, no key sent to it.
        server.tmux(&["clock-mode", "-t", "=tc_b:"]);
        assert_eq!(agent.sent(9, "tc_b", "echo clock >> clock.txt", true), Some(Problem::PaneBusy { session: "tc_b".into(), mode: "clock-mode".into() }));
        assert_eq!(server.mode("tc_b"), "1 clock-mode");
        // Copy mode over the clock: pasted (the shell has it), but leaving copy mode uncovers the
        // clock, which would take the Enter: pasted, not submitted.
        server.tmux(&["clock-mode", "-t", "=tc_c:"]);
        server.tmux(&["copy-mode", "-t", "=tc_c:"]);
        assert_eq!(agent.answer(10, "tc_c", "echo stacked >> stacked.txt", true), (Some(Problem::PaneBusy { session: "tc_c".into(), mode: "clock-mode".into() }), true));
        thread::sleep(Duration::from_millis(300));
        assert_eq!(server.file("stacked.txt"), "", "not submitted");
        assert_eq!(server.file("clock.txt"), "");

        // 6. Refused: a session that does not exist, one that is not the canvas's. No buffer left.
        assert_eq!(agent.sent(11, "tc_gone", "x", true), Some(Problem::SessionNotFound { session: "tc_gone".into() }));
        assert_eq!(agent.sent(12, "other", "x", true), Some(Problem::SessionNotFound { session: "other".into() }));
        assert_eq!(server.tmux(&["list-buffers"]), "", "no buffer left behind");

        // 7. Expired: never delivered, even when it waited behind other sends to its session.
        assert_eq!(agent.sent(13, session(), "echo late >> late.txt", true), None, "in time");
        agent.send_limited(14, session(), "echo now >> late.txt", true, Some(0));
        for k in 0..4u64 {
            agent.send(15 + k, session(), &format!("echo w{k} >> wait.txt"), true);
        }
        agent.send_limited(19, session(), "echo never >> late.txt", true, Some(300));
        let answers = agent.answers(&[14, 15, 16, 17, 18, 19]);
        assert_eq!(answers[0], (14, Some(Problem::Expired), false));
        assert_eq!(answers[5], (19, Some(Problem::Expired), false), "it waited past its limit: {answers:?}");
        server.wait_file("wait.txt", "w0\nw1\nw2\nw3\n");
        thread::sleep(Duration::from_millis(500));
        assert_eq!(server.file("late.txt"), "late\n", "never pasted");

        // 8. Ordered per session; a session busy with its sends (250 ms before each Enter) does
        //    not delay another one (here missing: answered at once).
        for k in 0..4u64 {
            agent.send(20 + k, "tc_a", &format!("echo a{k} >> row.txt"), true);
        }
        agent.send(30, "tc_b2", "x", false);
        let answers = agent.answers(&[20, 21, 22, 23, 30]);
        let order: Vec<u64> = answers.iter().map(|(id, ..)| *id).collect();
        let pos = |id| order.iter().position(|o| *o == id).unwrap();
        assert!(pos(30) < pos(21), "the other session was not kept waiting: {order:?}");
        assert!(pos(20) < pos(21) && pos(21) < pos(22) && pos(22) < pos(23), "{order:?}");
        server.wait_file("row.txt", "a0\na1\na2\na3\n");
        assert_eq!(server.tmux(&["list-buffers"]), "");

        // 9. 300 KB in one command line, through bracketed paste: whole.
        let long = "x".repeat(300_000);
        assert_eq!(agent.sent(31, session(), &format!("X={long}; echo ${{#X}} > len.txt"), true), None);
        server.wait_file("len.txt", "300000\n");
        assert!(server.alive("tc_dead"), "still the same server");
    }
}
