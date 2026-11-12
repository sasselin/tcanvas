//! End-to-end test: real throwaway tmux server + real `tcanvas-agent` binary.
//! Skipped cleanly if tmux (or the shell under test) is not installed.

use std::io::{BufRead, BufReader, Write};
use std::ops::RangeInclusive;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::{AgentCmd, AgentEvent, Hooks, SessionInfo, TermStatus};

const AGENT: &str = env!("CARGO_BIN_EXE_tcanvas-agent");

struct Server {
    socket: String,
}

impl Server {
    fn tmux(&self, args: &[&str]) {
        let ok = Command::new("tmux").arg("-L").arg(&self.socket).args(args).status().unwrap().success();
        assert!(ok, "tmux {args:?} failed");
    }
    fn type_line(&self, session: &str, line: &str) {
        self.tmux(&["send-keys", "-t", session, "-l", line]);
        self.tmux(&["send-keys", "-t", session, "Enter"]);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("tmux").arg("-L").arg(&self.socket).arg("kill-server").status();
        remove_socket_file(&self.socket);
    }
}

/// Removes the socket file tmux leaves behind after `kill-server`.
fn remove_socket_file(socket: &str) {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
    let tmp = std::env::var("TMUX_TMPDIR").unwrap_or_else(|_| "/tmp".into());
    let _ = std::fs::remove_file(format!("{tmp}/tmux-{uid}/{socket}"));
}

struct Agent {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<AgentEvent>,
}

impl Agent {
    fn spawn(socket: &str) -> Self {
        Self::spawn_with(socket, &[])
    }

    fn spawn_with(socket: &str, env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(AGENT).env("TCANVAS_TMUX_SOCKET", socket).envs(env.iter().copied()).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
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

    fn send(&mut self, cmd: &AgentCmd) {
        self.stdin.write_all(protocol::encode(cmd).as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn wait_for(&self, what: &str, pred: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut seen = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(ev) if pred(&ev) => return ev,
                Ok(ev) => seen.push(ev),
                Err(_) => break,
            }
        }
        panic!("expected: {what}\nreceived: {seen:#?}");
    }

    fn wait_status(&self, session: &str, what: &str, pred: impl Fn(&SessionInfo) -> bool) -> SessionInfo {
        match self.wait_for(what, |ev| matches!(ev, AgentEvent::Session(i) if i.session == session && pred(i))) {
            AgentEvent::Session(info) => info,
            _ => unreachable!(),
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Records the steps of the wall clock against the monotonic one while a command runs (a
/// sample every 10 ms). The shell hooks stamp commands with the wall clock, which WSL steps
/// back to follow Windows (about 1.8 s every 30 s on some machines); the agent must report
/// durations that such a step does not shorten.
struct ClockWatch {
    start: Instant,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Vec<(Duration, f64)>>>,
}

impl ClockWatch {
    fn start() -> Self {
        let start = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let offset = move || SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64() - start.elapsed().as_secs_f64();
        let thread = thread::spawn(move || {
            let mut steps = Vec::new();
            let mut last = offset();
            while !stopped.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(10));
                let now = offset();
                if (now - last).abs() >= 0.3 {
                    steps.push((start.elapsed(), now - last));
                }
                last = now;
            }
            steps
        });
        ClockWatch { start, stop, thread: Some(thread) }
    }

    /// Stops watching. Returns the time since `start`, and the steps seen: (when, by how many
    /// seconds, negative when back).
    fn stop(mut self) -> (Duration, Vec<(Duration, f64)>) {
        let elapsed = self.start.elapsed();
        self.stop.store(true, Ordering::Relaxed);
        (elapsed, self.thread.take().unwrap().join().unwrap())
    }
}

impl Drop for ClockWatch {
    /// A failed assertion before `stop` still ends the sampling thread.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Whole seconds the agent may report for a `secs`-long bash command started when the watch
/// started and seen ended `ended` later, the wall clock having stepped by `steps`. bash
/// publishes a command's start only at its end, so the agent takes the hooks' marks (whole
/// seconds: +-1) and corrects them only by the steps certain to fall inside the command. Steps
/// before it are never counted, nor are those in its first poll (before the agent sees it
/// running) or its last one (recorded at the poll that sees the end): a step back there may
/// shorten the result, a jump ahead lengthen it, so those widen the range. A step in the middle
/// must be corrected and does not widen it.
fn bash_duration(secs: u64, ended: Duration, steps: &[(Duration, f64)]) -> RangeInclusive<u64> {
    const DOUBT: Duration = Duration::from_millis(1500);
    let doubtful = steps.iter().filter(|(at, _)| *at < DOUBT || *at + DOUBT > ended).map(|(_, step)| step);
    let back: f64 = doubtful.clone().filter(|s| **s < 0.0).map(|s| -s).sum();
    let ahead: f64 = doubtful.filter(|s| **s > 0.0).sum();
    secs.saturating_sub(1 + back.ceil() as u64)..=secs + 1 + ahead.ceil() as u64
}

fn has(bin: &str) -> bool {
    Command::new(bin).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

#[test]
fn reports_running_stalled_failed_question_and_gone() {
    if !has("tmux") || !has("zsh") {
        eprintln!("tmux or zsh missing: test skipped");
        return;
    }
    let server = Server { socket: format!("tcanvas-test-{}", std::process::id()) };
    let session = "tc_live";

    // Hooks in a temporary file. The socket name contains "tcanvas", which enables them.
    let hooks = std::env::temp_dir().join(format!("tcanvas-hooks-{}.zsh", std::process::id()));
    let out = Command::new(AGENT).args(["hooks", "zsh"]).output().unwrap();
    std::fs::write(&hooks, &out.stdout).unwrap();

    server.tmux(&["new-session", "-d", "-s", session, "-x", "100", "-y", "20", "zsh -f -i"]);
    server.tmux(&["new-session", "-d", "-s", "personal", "zsh -f -i"]);
    thread::sleep(Duration::from_millis(300));
    server.type_line(session, &format!("source {}", hooks.display()));

    let mut agent = Agent::spawn(&server.socket);
    agent.wait_for("Hello", |ev| matches!(ev, AgentEvent::Hello { .. }));
    agent.send(&AgentCmd::SetStall { session: session.into(), secs: 2 });
    agent.wait_status(session, "Idle", |i| i.status == TermStatus::Idle);

    // Stalled needs a poll (about every second) seeing 2 s of silence on the wall clock,
    // which a step back (up to 2 s, see `ClockWatch`) can cut from the sleep: 6 s leaves room.
    let clock = ClockWatch::start();
    server.type_line(session, "sleep 6");
    let running = agent.wait_status(session, "Running", |i| i.status == TermStatus::Running);
    assert_eq!(running.command.as_deref(), Some("sleep 6"));
    agent.wait_status(session, "Stalled", |i| i.status == TermStatus::Stalled);
    let done = agent.wait_status(session, "Idle after sleep", |i| i.status == TermStatus::Idle && i.last_exit == Some(0));
    // zsh publishes the start: the agent times the command poll to poll on its monotonic
    // clock, within a poll whatever the wall clock did.
    let (_, steps) = clock.stop();
    assert!(matches!(done.last_duration_secs, Some(5..=7)), "duration {:?}, wall clock steps {steps:?}", done.last_duration_secs);

    server.type_line(session, "false");
    let failed = agent.wait_status(session, "Exited(1)", |i| i.status == TermStatus::Exited(1));
    assert_eq!(failed.last_exit, Some(1));

    server.type_line(session, r#"sh -c 'printf "Continue? [y/N] "; read x'"#);
    let asking = agent.wait_status(session, "WaitingInput", |i| i.status == TermStatus::WaitingInput);
    assert!(asking.tail.last().unwrap().ends_with("[y/N]"), "tail {:?}", asking.tail);
    server.type_line(session, "y");
    agent.wait_status(session, "Idle after answer", |i| i.status == TermStatus::Idle);

    // The "personal" session (no tc_ prefix) is never reported.
    agent.send(&AgentCmd::Kill { session: session.into() });
    agent.wait_for("Gone", |ev| matches!(ev, AgentEvent::Gone { session: s } if s == session));
    assert!(agent.events.try_iter().all(|ev| !matches!(ev, AgentEvent::Session(i) if i.session == "personal")));

    let _ = std::fs::remove_file(hooks);
}

fn proc_alive(cmd: &str) -> bool {
    Command::new("pgrep").args(["-fx", cmd]).output().is_ok_and(|o| o.status.success())
}

#[test]
fn kill_stops_every_process_even_stubborn_ones() {
    if !has("tmux") || !has("zsh") {
        eprintln!("tmux or zsh missing: test skipped");
        return;
    }
    let server = Server { socket: format!("tcanvas-kill-{}", std::process::id()) };
    let session = "tc_stubborn";
    // Unique durations so we only target our own processes.
    // `pgrep` looks system-wide: the pid makes them unique to this test process.
    let base = u64::from(std::process::id()) * 100;
    let cmds: Vec<String> = (0..4).map(|k| format!("sleep {}", base + k)).collect();

    server.tmux(&["new-session", "-d", "-s", session, "-x", "120", "-y", "30", "zsh -f -i"]);
    server.tmux(&["new-session", "-d", "-s", "tc_neighbor", "zsh -f -i"]);
    thread::sleep(Duration::from_millis(300));
    // Shell and children ignoring SIGHUP and SIGTERM, nohup, setsid, and a foreground command.
    server.type_line(session, &format!("trap '' HUP TERM; {} &", cmds[0]));
    server.type_line(session, &format!("nohup {} >/dev/null 2>&1 &", cmds[1]));
    server.type_line(session, &format!("setsid {} &", cmds[2]));
    server.type_line(session, &cmds[3]);
    server.type_line("tc_neighbor", &format!("sleep {}", base + 9));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cmds.iter().all(|c| proc_alive(c)) {
        assert!(Instant::now() < deadline, "test processes did not start");
        thread::sleep(Duration::from_millis(100));
    }

    let out = Command::new(AGENT).args(["kill", session]).env("TCANVAS_TMUX_SOCKET", &server.socket).output().unwrap();
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "kill: {report} {}", String::from_utf8_lossy(&out.stderr));
    assert!(report.contains("SIGKILL"), "{report}");

    for c in &cmds {
        assert!(!proc_alive(c), "{c} survived");
    }
    let has_session = |s: &str| Command::new("tmux").args(["-L", &server.socket, "has-session", "-t", s]).status().unwrap().success();
    assert!(!has_session(session), "session removed");
    assert!(has_session("tc_neighbor"), "other sessions are untouched");
    assert!(proc_alive(&format!("sleep {}", base + 9)), "nor their processes");
}

#[test]
fn bash_hooks_report_exit_code_and_duration() {
    if !has("tmux") || !has("bash") {
        eprintln!("tmux or bash missing: test skipped");
        return;
    }
    let server = Server { socket: format!("tcanvas-bash-{}", std::process::id()) };
    let session = "tc_bash";
    let hooks = std::env::temp_dir().join(format!("tcanvas-hooks-{}.bash", std::process::id()));
    let out = Command::new(AGENT).args(["hooks", "bash"]).output().unwrap();
    std::fs::write(&hooks, &out.stdout).unwrap();

    server.tmux(&["new-session", "-d", "-s", session, "-x", "100", "-y", "20", "bash --norc -i"]);
    thread::sleep(Duration::from_millis(300));
    // An existing PROMPT_COMMAND keeps running after the hooks.
    server.type_line(session, &format!("PROMPT_COMMAND='touch {}.pc'; source {}", hooks.display(), hooks.display()));

    let agent = Agent::spawn(&server.socket);
    agent.wait_for("Hello", |ev| matches!(ev, AgentEvent::Hello { .. }));
    agent.wait_status(session, "Idle", |i| i.status == TermStatus::Idle);

    // Long enough for a step in the middle, which the agent must add back (see `bash_duration`).
    let clock = ClockWatch::start();
    server.type_line(session, "sleep 5; false");
    let failed = agent.wait_status(session, "Exited(1)", |i| i.status == TermStatus::Exited(1));
    let (ended, steps) = clock.stop();
    assert_eq!(failed.last_exit, Some(1));
    let expected = bash_duration(5, ended, &steps);
    assert!(failed.last_duration_secs.is_some_and(|d| expected.contains(&d)), "duration {:?}, expected {expected:?}, wall clock steps {steps:?}", failed.last_duration_secs);

    // The user's $? is untouched by the hooks.
    server.type_line(session, "(exit 7); echo \"code=$?\"");
    let shown = agent.wait_status(session, "code=7 shown", |i| i.tail.iter().any(|l| l == "code=7"));
    assert_eq!(shown.last_exit, Some(0), "echo succeeded");
    assert!(std::path::Path::new(&format!("{}.pc", hooks.display())).exists(), "previous PROMPT_COMMAND still runs");

    let _ = std::fs::remove_file(format!("{}.pc", hooks.display()));
    let _ = std::fs::remove_file(hooks);
}

#[test]
fn an_end_stamped_before_its_start_keeps_the_exit_code() {
    if !has("tmux") {
        eprintln!("tmux missing: test skipped");
        return;
    }
    // A shell without hooks (neither bash nor zsh: the agent applies the general rules): the
    // test writes the marks itself, as the hooks would after a wall clock step back, in the
    // order each shell writes them (see `hooks.rs`).
    let server = Server { socket: format!("tcanvas-marks-{}", std::process::id()) };
    let session = "tc_marks";
    server.tmux(&["new-session", "-d", "-s", session, "sh"]);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let mark = |name: &str, value: u64| ["set", "-p", "-t", session, name, &value.to_string()].map(String::from);
    let set = |cmds: &[[String; 6]]| {
        let args: Vec<&str> = cmds.iter().enumerate().flat_map(|(i, c)| (i > 0).then_some(";").into_iter().chain(c.iter().map(String::as_str))).collect();
        server.tmux(&args);
    };

    let agent = Agent::spawn(&server.socket);
    agent.wait_for("Hello", |ev| matches!(ev, AgentEvent::Hello { .. }));
    set(&[mark("@tc_started", now - 20), mark("@tc_exit", 0), mark("@tc_ended", now - 19)]);
    agent.wait_status(session, "first command", |i| i.status == TermStatus::Idle && i.last_exit == Some(0));

    // zsh: the start alone (a command running), then the exit code and an end 2 s before it.
    set(&[mark("@tc_started", now + 2)]);
    let started = Instant::now();
    agent.wait_status(session, "running", |i| i.status == TermStatus::Idle && i.last_exit.is_none());
    thread::sleep(Duration::from_secs(3).saturating_sub(started.elapsed()));
    set(&[mark("@tc_exit", 3), mark("@tc_ended", now)]);
    let lasted = started.elapsed().as_secs_f64();
    let failed = agent.wait_status(session, "Exited(3)", |i| i.status == TermStatus::Exited(3));
    // Timed by the agent from the poll that saw the start to the one that saw the end: each
    // comes up to a poll after its mark (1 s of sleep plus the poll's own work, under 0.5 s
    // even under load), plus 0.5 s of rounding.
    const POLL_MAX_SECS: f64 = 1.5;
    let d = failed.last_duration_secs.expect("timed on the agent's clock") as f64;
    assert!((d - lasted).abs() <= POLL_MAX_SECS + 0.5, "duration {d}, lasted {lasted:.1} s");

    // bash: the three marks at once. Nothing tells how long it ran: no duration.
    set(&[mark("@tc_started", now + 10), mark("@tc_exit", 4), mark("@tc_ended", now + 8)]);
    let failed = agent.wait_status(session, "Exited(4)", |i| i.status == TermStatus::Exited(4));
    assert_eq!(failed.last_duration_secs, None);

    // zsh again: a new start after that end is a command running, not a failure.
    set(&[mark("@tc_started", now + 9)]);
    agent.wait_status(session, "running again", |i| i.status == TermStatus::Idle && i.last_exit.is_none());
}

#[test]
fn install_hooks_command_follows_the_users_shell() {
    let home = std::env::temp_dir().join(format!("tcanvas-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let socket = format!("tcanvas-nohooks-{}", std::process::id());
    let home_str = home.to_str().unwrap();

    let mut agent = Agent::spawn_with(&socket, &[("HOME", home_str), ("SHELL", "/bin/bash")]);
    agent.wait_for("Hello without hooks", |ev| matches!(ev, AgentEvent::Hello { hooks: Hooks::Missing, .. }));
    agent.send(&AgentCmd::InstallHooks);
    agent.wait_for("hooks installed", |ev| matches!(ev, AgentEvent::HooksChanged { hooks: Hooks::Installed }));
    assert!(std::fs::read_to_string(home.join(".bashrc")).unwrap().contains(".config/tcanvas/hooks.bash"));
    drop(agent);

    let agent = Agent::spawn_with(&socket, &[("HOME", home_str), ("SHELL", "/usr/bin/fish")]);
    agent.wait_for("fish: nothing to offer", |ev| matches!(ev, AgentEvent::Hello { hooks: Hooks::Unsupported, .. }));
    drop(agent);
    let _ = std::fs::remove_dir_all(home);
}
