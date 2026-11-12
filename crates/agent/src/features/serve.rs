//! Main loop: observes the sessions every second and publishes changes on stdout.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use protocol::{AgentCmd, AgentEvent, DEFAULT_STALL_SECS, Problem, SessionInfo, TermStatus};

use super::{agent_hooks, changes, hooks, kill, send as send_text, status, tasks, tmux};

const POLL: Duration = Duration::from_secs(1);
const TAIL_LINES: usize = 3;

#[derive(Default)]
struct Tracked {
    stall_secs: u64,
    /// `window_activity` at the last screen capture.
    captured_activity: Option<u64>,
    tail: Vec<String>,
    /// (foreground pid, CPU ticks) at the previous observation.
    prev_cpu: Option<(u32, u64)>,
    last_sent: Option<SessionInfo>,
    /// Start of the current run of screen output (no pause over `BUSY_GAP_SECS`).
    busy_since: Option<u64>,
}

pub fn run() -> io::Result<()> {
    // The changes thread also writes to stdout: one lock per message keeps lines whole.
    let watcher = changes::spawn(|ev| {
        if send(ev).is_err() {
            std::process::exit(0);
        }
    });
    let sender = send_text::spawn(|ev| {
        if send(ev).is_err() {
            std::process::exit(0);
        }
    });
    let lists = tasks::spawn(|ev| {
        if send(ev).is_err() {
            std::process::exit(0);
        }
    });
    let cmds = spawn_stdin_reader(watcher, sender, lists);
    send(&AgentEvent::Hello { version: env!("CARGO_PKG_VERSION").into(), hooks: hooks::state(), send_text: true, tasks: true })?;

    let mut tracked: HashMap<String, Tracked> = HashMap::new();
    loop {
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                AgentCmd::SetStall { session, secs } => tracked.entry(session).or_default().stall_secs = secs,
                AgentCmd::Kill { session } => {
                    thread::spawn(move || kill::kill_session(&session));
                }
                // Forwarded straight to the changes thread by the stdin reader.
                AgentCmd::InstallHooks => match hooks::install() {
                    Ok(_) => send(&AgentEvent::HooksChanged { hooks: hooks::state() })?,
                    Err(e) => send(&AgentEvent::Error { problem: Problem::HooksFailed { detail: e.to_string() } })?,
                },
                AgentCmd::Watch { .. }
                | AgentCmd::Unwatch { .. }
                | AgentCmd::Diff { .. }
                | AgentCmd::ShowCommit { .. }
                | AgentCmd::ListDir { .. }
                | AgentCmd::Cleanup { .. }
                | AgentCmd::WorktreeSizes { .. }
                | AgentCmd::PlanWorktree { .. }
                | AgentCmd::CreateWorktree { .. }
                | AgentCmd::SendText { .. }
                | AgentCmd::WatchTasks { .. }
                | AgentCmd::UnwatchTasks { .. }
                | AgentCmd::EditTasks { .. }
                | AgentCmd::AdoptTasks { .. } => {}
            }
        }

        let rows = match tmux::list_panes() {
            Ok(rows) => rows,
            Err(e) => {
                send(&AgentEvent::Error { problem: Problem::TmuxMissing { detail: e.to_string() } })?;
                Vec::new()
            }
        };
        let now = unix_now();

        for row in &rows {
            let t = tracked.entry(row.session.clone()).or_default();
            let info = observe(row, t, now);
            if t.last_sent.as_ref() != Some(&info) {
                send(&AgentEvent::Session(info.clone()))?;
                t.last_sent = Some(info);
            }
        }

        let gone: Vec<String> = tracked.iter().filter(|(s, t)| t.last_sent.is_some() && !rows.iter().any(|r| &r.session == *s)).map(|(s, _)| s.clone()).collect();
        for session in gone {
            tracked.remove(&session);
            send(&AgentEvent::Gone { session })?;
        }

        thread::sleep(POLL);
    }
}

fn observe(row: &tmux::PaneRow, t: &mut Tracked, now: u64) -> SessionInfo {
    if t.captured_activity != Some(row.activity) {
        t.tail = tmux::capture_tail(&row.session, TAIL_LINES);
        t.captured_activity = Some(row.activity);
    }

    let fg = if row.dead { None } else { status::foreground(row.pane_pid) };
    let cpu_delta = fg.as_ref().and_then(|f| match t.prev_cpu {
        Some((pid, ticks)) if pid == f.pid => Some(f.cpu_ticks.saturating_sub(ticks)),
        _ => None,
    });
    t.prev_cpu = fg.as_ref().map(|f| (f.pid, f.cpu_ticks));

    if now.saturating_sub(row.activity) > status::BUSY_GAP_SECS {
        t.busy_since = None;
    } else {
        t.busy_since.get_or_insert(now);
    }

    // A mark left by an agent that has since quit names other processes: ignored. The payload
    // values need a trusted mark too, and must have been written by the same foreground agent.
    let mark = row.agent.as_ref().filter(|m| fg.as_ref().is_some_and(|f| m.pids.contains(&f.pid)));
    let agent_pid = mark.and(fg.as_ref().map(|f| f.pid));
    let obs = status::Observation {
        now,
        dead: row.dead,
        alternate: row.alternate,
        fg_running: fg.is_some(),
        activity: row.activity,
        cpu_delta,
        last_line: t.tail.last().map(String::as_str),
        hook_started: row.started,
        hook_ended: row.ended,
        hook_exit: row.exit,
        stall_secs: if t.stall_secs == 0 { DEFAULT_STALL_SECS } else { t.stall_secs },
        agent: mark.map(|m| m.state),
        coding_agent: fg.as_ref().is_some_and(|f| status::is_coding_agent(&f.cmdline)),
        busy_secs: t.busy_since.map_or(0, |since| now - since),
        was_waiting: t.last_sent.as_ref().is_some_and(|i| i.status == TermStatus::WaitingInput),
    };
    let status = status::classify(&obs);
    let (last_exit, last_duration_secs) = status::last_command(&obs);

    SessionInfo {
        session: row.session.clone(),
        status,
        command: fg.map(|f| f.cmdline).filter(|_| status != TermStatus::Dead),
        cwd: Some(row.path.clone()).filter(|p| !p.is_empty()),
        last_exit,
        last_duration_secs,
        tail: t.tail.clone(),
        agent_session_id: agent_hooks::trusted(row.agent_session.as_ref(), agent_pid),
        agent_transcript: agent_hooks::trusted(row.transcript.as_ref(), agent_pid),
        last_prompt: agent_hooks::trusted(row.prompt.as_ref(), agent_pid),
    }
}

fn send(ev: &AgentEvent) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(protocol::encode(ev).as_bytes())?;
    out.flush()
}

/// Reads the app's commands. End of stdin (app closed) stops the agent.
/// Change-tracking commands go straight to their thread (no waiting for diffs), and so do text
/// to send (no waiting for the next poll) and task list commands.
fn spawn_stdin_reader(watcher: Sender<changes::Cmd>, sender: Sender<send_text::Request>, lists: Sender<tasks::Cmd>) -> Receiver<AgentCmd> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match protocol::decode::<AgentCmd>(&line) {
                Ok(AgentCmd::Watch { key, path }) => {
                    let _ = watcher.send(changes::Cmd::Watch { key, path });
                }
                Ok(AgentCmd::Unwatch { key }) => {
                    let _ = watcher.send(changes::Cmd::Unwatch { key });
                }
                Ok(AgentCmd::Diff { key, file }) => {
                    let _ = watcher.send(changes::Cmd::Diff { key, file });
                }
                Ok(AgentCmd::ShowCommit { key, sha }) => {
                    let _ = watcher.send(changes::Cmd::ShowCommit { key, sha });
                }
                Ok(AgentCmd::ListDir { path }) => {
                    let _ = watcher.send(changes::Cmd::List { path });
                }
                Ok(AgentCmd::Cleanup { key, action }) => {
                    let _ = watcher.send(changes::Cmd::Cleanup { key, action });
                }
                Ok(AgentCmd::WorktreeSizes { key, paths }) => {
                    let _ = watcher.send(changes::Cmd::Sizes { key, paths });
                }
                Ok(AgentCmd::PlanWorktree { id, key, folder, location }) => {
                    let _ = watcher.send(changes::Cmd::PlanWorktree { id, key, folder, location });
                }
                Ok(AgentCmd::CreateWorktree { id, key, request }) => {
                    let _ = watcher.send(changes::Cmd::CreateWorktree { id, key, request });
                }
                Ok(AgentCmd::SendText { id, session, text, enter, limit_ms }) => {
                    // Measured from now, on this machine's monotonic clock.
                    let deadline = limit_ms.map(|ms| std::time::Instant::now() + Duration::from_millis(ms));
                    let _ = sender.send(send_text::Request { id, session, text, enter, deadline });
                }
                Ok(AgentCmd::WatchTasks { key, path }) => {
                    let _ = lists.send(tasks::Cmd::Watch { key, path });
                }
                Ok(AgentCmd::UnwatchTasks { key }) => {
                    let _ = lists.send(tasks::Cmd::Unwatch { key });
                }
                Ok(AgentCmd::EditTasks { id, key, op, expect, list }) => {
                    if lists.send(tasks::Cmd::Edit { id, key: key.clone(), op, expect, list }).is_err() {
                        tasks_gone(id, key);
                    }
                }
                Ok(AgentCmd::AdoptTasks { id, key, from, merge, list }) => {
                    if lists.send(tasks::Cmd::Adopt { id, key: key.clone(), from, merge, list }).is_err() {
                        tasks_gone(id, key);
                    }
                }
                Ok(cmd) => {
                    if tx.send(cmd).is_err() {
                        break;
                    }
                }
                // The type only: the rest may hold task text.
                Err(e) => eprintln!("ignored command ({e}): type {}", command_type(&line)),
            }
        }
        std::process::exit(0);
    });
    rx
}

/// The `type` of a command line that did not decode, for the log.
fn command_type(line: &str) -> String {
    serde_json::from_str::<serde_json::Value>(line).ok().and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from)).unwrap_or_else(|| "unknown".into())
}

/// The task lists' thread stopped: changes are answered with a failure rather than never.
fn tasks_gone(id: u64, key: String) {
    let problem = Problem::TasksFailed { detail: "the task list thread stopped".into() };
    if send(&AgentEvent::TasksEdited { id, key, task: None, problem: Some(problem), content_rev: None }).is_err() {
        std::process::exit(0);
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
