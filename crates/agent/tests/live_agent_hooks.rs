//! Coding agent hooks against a real throwaway tmux server: `tcanvas-agent hook <event>`
//! run from inside a pane marks it, and the status loop trusts the mark only while the
//! process that set it is in the foreground.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{AgentCmd, AgentEvent, SessionInfo, TermStatus};

const AGENT: &str = env!("CARGO_BIN_EXE_tcanvas-agent");

struct Server {
    socket: String,
}

impl Server {
    fn tmux(&self, args: &[&str]) -> String {
        let out = Command::new("tmux").arg("-L").arg(&self.socket).args(args).output().unwrap();
        assert!(out.status.success(), "tmux {args:?} failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
    /// The pane's `@tc_agent` mark, read through a format like the status loop does
    /// (`show -v` of an unset user option fails on recent tmux).
    fn mark(&self, session: &str) -> String {
        self.tmux(&["display", "-p", "-t", session, "#{@tc_agent}"])
    }
    fn type_line(&self, session: &str, line: &str) {
        self.tmux(&["send-keys", "-t", session, "-l", line]);
        self.tmux(&["send-keys", "-t", session, "Enter"]);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("tmux").arg("-L").arg(&self.socket).arg("kill-server").status();
        use std::os::unix::fs::MetadataExt;
        let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
        let tmp = std::env::var("TMUX_TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let _ = std::fs::remove_file(format!("{tmp}/tmux-{uid}/{}", self.socket));
    }
}

struct Agent {
    child: Child,
    events: Receiver<AgentEvent>,
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

impl Agent {
    fn wait_status(&self, session: &str, what: &str, pred: impl Fn(&SessionInfo) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut seen = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(AgentEvent::Session(i)) if i.session == session && pred(&i) => return,
                Ok(ev) => seen.push(ev),
                Err(_) => break,
            }
        }
        panic!("expected: {what}\nreceived: {seen:#?}");
    }
}

fn has(bin: &str) -> bool {
    Command::new(bin).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

#[test]
fn hooks_mark_the_pane_while_their_agent_runs() {
    if !has("tmux") || !has("bash") {
        eprintln!("tmux or bash missing: test skipped");
        return;
    }
    // The name contains "tcanvas": the hook only acts inside a tcanvas server.
    let server = Server { socket: format!("tcanvas-agenthooks-{}", std::process::id()) };
    let session = "tc_agent";
    server.tmux(&["new-session", "-d", "-s", session, "-x", "100", "-y", "20", "bash --norc --noprofile -i"]);
    thread::sleep(Duration::from_millis(300));

    let mut child = Command::new(AGENT).env("TCANVAS_TMUX_SOCKET", &server.socket).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, events) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(protocol::decode(&line).expect("valid JSON line"));
        }
    });
    let agent = Agent { child, events };
    use std::io::Write;
    stdin.write_all(protocol::encode(&AgentCmd::SetStall { session: session.into(), secs: 2 }).as_bytes()).unwrap();
    agent.wait_status(session, "Idle", |i| i.status == TermStatus::Idle);

    // A fake agent: turn done (waiting), then the user answers (working) and it stays
    // silent longer than the stall delay.
    let fake = format!("sh -c '{AGENT} hook Stop </dev/null >/dev/null; sleep 3; echo x | {AGENT} hook UserPromptSubmit >/dev/null; sleep 30'");
    server.type_line(session, &fake);
    agent.wait_status(session, "WaitingInput from the Stop hook", |i| i.status == TermStatus::WaitingInput);
    agent.wait_status(session, "Running from UserPromptSubmit", |i| i.status == TermStatus::Running);
    thread::sleep(Duration::from_secs(3));
    assert!(server.mark(session).starts_with("working "));

    // The fake agent quits; another silent command must not inherit its mark.
    server.tmux(&["send-keys", "-t", session, "C-c"]);
    agent.wait_status(session, "Idle at the prompt", |i| i.status == TermStatus::Idle);
    server.type_line(session, "sleep 30");
    agent.wait_status(session, "Stalled, not Running", |i| i.status == TermStatus::Stalled);
    server.tmux(&["send-keys", "-t", session, "C-c"]);

    // SessionEnd removes the mark; outside a tcanvas server the hook does nothing.
    server.type_line(session, &format!("{AGENT} hook SessionEnd </dev/null"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !server.mark(session).is_empty() {
        assert!(Instant::now() < deadline, "SessionEnd did not clear the mark");
        thread::sleep(Duration::from_millis(100));
    }
    let out = Command::new(AGENT).args(["hook", "Stop"]).env("TMUX", "/tmp/tmux-1000/default,1,0").env("TMUX_PANE", "%0").stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{}\n", "a valid 'no decision' for every agent");
}
