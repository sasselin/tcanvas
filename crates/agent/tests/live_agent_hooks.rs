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

#[test]
fn a_prompt_sent_to_the_hook_is_reported_with_the_session() {
    if !has("tmux") || !has("bash") {
        eprintln!("tmux or bash missing: test skipped");
        return;
    }
    let server = Server { socket: format!("tcanvas-agentprompt-{}", std::process::id()) };
    let session = "tc_prompt";
    server.tmux(&["new-session", "-d", "-s", session, "-x", "100", "-y", "20", "bash --norc --noprofile -i"]);
    thread::sleep(Duration::from_millis(300));

    // A prompt holding what could break the pane listing or be expanded by tmux.
    let dir = TempDir(std::env::temp_dir().join(format!("tcanvas-agentprompt-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let payload_file = dir.0.join("payload.json");
    let payload = serde_json::json!({
        "session_id": "4f1c-session",
        "transcript_path": "/home/me/.claude/projects/-home-me-api/4f1c-session.jsonl",
        "cwd": "/home/me/api",
        "hook_event_name": "UserPromptSubmit",
        "prompt": "add the total\twith taxes\nto the PDF #{session_name} \u{e9}t\u{e9}",
    });
    std::fs::write(&payload_file, payload.to_string()).unwrap();

    let mut child = Command::new(AGENT).env("TCANVAS_TMUX_SOCKET", &server.socket).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let _stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, events) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(protocol::decode(&line).expect("valid JSON line"));
        }
    });
    let agent = Agent { child, events };
    agent.wait_status(session, "Idle", |i| i.status == TermStatus::Idle);

    let expected = "add the total with taxes to the PDF #{session_name} \u{e9}t\u{e9}";
    let fake = format!("sh -c '{AGENT} hook UserPromptSubmit <{} >/dev/null; sleep 30'", payload_file.display());
    server.type_line(session, &fake);
    agent.wait_status(session, "the prompt from the hook payload", |i| {
        i.last_prompt.as_deref() == Some(expected)
            && i.agent_session_id.as_deref() == Some("4f1c-session")
            && i.agent_transcript.as_deref() == Some("/home/me/.claude/projects/-home-me-api/4f1c-session.jsonl")
    });

    // The agent quits (without SessionEnd): its prompt is no longer reported.
    server.tmux(&["send-keys", "-t", session, "C-c"]);
    agent.wait_status(session, "no prompt once the agent is gone", |i| i.status == TermStatus::Idle && i.last_prompt.is_none() && i.agent_session_id.is_none());

    // Another agent starts in the same pane; its hook brings no prompt nor transcript. It owns
    // the mark now, but must not be credited with the first agent's prompt and transcript.
    let other_file = dir.0.join("other.json");
    std::fs::write(&other_file, r#"{"sessionId":"other"}"#).unwrap();
    server.type_line(session, &format!("sh -c '{AGENT} hook agentStop <{} >/dev/null; sleep 30'", other_file.display()));
    agent.wait_status(session, "the second agent, without the first one's prompt", |i| {
        i.status == TermStatus::WaitingInput && i.agent_session_id.as_deref() == Some("other") && i.last_prompt.is_none() && i.agent_transcript.is_none()
    });
    server.tmux(&["send-keys", "-t", session, "C-c"]);
    agent.wait_status(session, "Idle again", |i| i.status == TermStatus::Idle);

    // SessionEnd clears what the hooks kept.
    let kept = |s: &Server| s.tmux(&["display", "-p", "-t", session, "#{@tc_agent_session}#{@tc_agent_transcript}#{@tc_agent_prompt}"]);
    assert!(!kept(&server).is_empty());
    server.type_line(session, &format!("{AGENT} hook SessionEnd </dev/null"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !kept(&server).is_empty() {
        assert!(Instant::now() < deadline, "SessionEnd did not clear the payload");
        thread::sleep(Duration::from_millis(100));
    }
}

/// Removed even when an assertion fails.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `git` in `dir`, seeing neither the user's nor the system's configuration.
fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn prompt_and_turn_end_marks_carry_the_commit_of_the_agents_folder() {
    if !has("tmux") || !has("bash") || !has("git") {
        eprintln!("tmux, bash or git missing: test skipped");
        return;
    }
    let server = Server { socket: format!("tcanvas-agenthead-{}", std::process::id()) };
    let session = "tc_head";
    server.tmux(&["new-session", "-d", "-s", session, "-x", "100", "-y", "20", "bash --norc --noprofile -i"]);
    thread::sleep(Duration::from_millis(300));
    let dir = TempDir(std::env::temp_dir().join(format!("tcanvas-agenthead-{}", std::process::id())));
    let repo = dir.0.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let commit = |n: u32| {
        git(&repo, &["-c", "user.name=T", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", &format!("c{n}")]);
        git(&repo, &["rev-parse", "HEAD"])
    };
    let first = commit(1);
    // The payload names the agent's folder; the hook itself runs elsewhere.
    let payload = dir.0.join("payload.json");
    std::fs::write(&payload, serde_json::json!({ "session_id": "s", "cwd": repo.display().to_string() }).to_string()).unwrap();
    let outside = dir.0.join("outside.json");
    std::fs::write(&outside, serde_json::json!({ "session_id": "s", "cwd": dir.0.display().to_string() }).to_string()).unwrap();

    let mut child = Command::new(AGENT).env("TCANVAS_TMUX_SOCKET", &server.socket).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let _stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, events) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(protocol::decode(&line).expect("valid JSON line"));
        }
    });
    let agent = Agent { child, events };
    agent.wait_status(session, "Idle", |i| i.status == TermStatus::Idle);

    // A prompt, a commit, a turn end: each mark with the head of its moment.
    server.type_line(session, &format!("cd /; {AGENT} hook UserPromptSubmit <{} >/dev/null", payload.display()));
    agent.wait_status(session, "the prompt's head", |i| i.agent_prompt_mark.is_some() && i.agent_prompt_head.as_deref() == Some(first.as_str()));
    let second = commit(2);
    server.type_line(session, &format!("{AGENT} hook Stop <{} >/dev/null", payload.display()));
    agent.wait_status(session, "the turn end's head", |i| {
        i.agent_turn_mark.is_some() && i.agent_turn_head.as_deref() == Some(second.as_str()) && i.agent_prompt_head.as_deref() == Some(first.as_str())
    });
    // One option per mark, the head in its value (set by the hook's single tmux call).
    let raw = server.tmux(&["display", "-p", "-t", session, "#{@tc_agent_turn_mark}"]);
    assert!(raw.ends_with(&format!(" {second}")), "{raw}");
    // Outside a repository: the mark moves, without a head.
    server.type_line(session, &format!("{AGENT} hook UserPromptSubmit <{} >/dev/null", outside.display()));
    agent.wait_status(session, "a prompt outside any repository", |i| i.agent_prompt_mark.is_some() && i.agent_prompt_head.is_none());
}
