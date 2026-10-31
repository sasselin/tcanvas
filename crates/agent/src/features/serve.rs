//! Boucle principale : observe les sessions chaque seconde et publie les changements sur stdout.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use protocol::{AgentCmd, AgentEvent, DEFAULT_STALL_SECS, SessionInfo, TermStatus};

use super::{hooks, kill, status, tmux};

const POLL: Duration = Duration::from_secs(1);
const TAIL_LINES: usize = 3;

#[derive(Default)]
struct Tracked {
    stall_secs: u64,
    /// `window_activity` lors de la dernière capture d'écran.
    captured_activity: Option<u64>,
    tail: Vec<String>,
    /// (pid du premier plan, ticks CPU) à l'observation précédente.
    prev_cpu: Option<(u32, u64)>,
    last_sent: Option<SessionInfo>,
}

pub fn run() -> io::Result<()> {
    let cmds = spawn_stdin_reader();
    let mut out = io::stdout().lock();
    send(&mut out, &AgentEvent::Hello { version: env!("CARGO_PKG_VERSION").into(), hooks_installed: hooks::installed() })?;

    let mut tracked: HashMap<String, Tracked> = HashMap::new();
    loop {
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                AgentCmd::SetStall { session, secs } => tracked.entry(session).or_default().stall_secs = secs,
                AgentCmd::Kill { session } => {
                    thread::spawn(move || kill::kill_session(&session));
                }
            }
        }

        let rows = match tmux::list_panes() {
            Ok(rows) => rows,
            Err(e) => {
                send(&mut out, &AgentEvent::Error { message: format!("tmux introuvable : {e}") })?;
                Vec::new()
            }
        };
        let now = unix_now();

        for row in &rows {
            let t = tracked.entry(row.session.clone()).or_default();
            let info = observe(row, t, now);
            if t.last_sent.as_ref() != Some(&info) {
                send(&mut out, &AgentEvent::Session(info.clone()))?;
                t.last_sent = Some(info);
            }
        }

        let gone: Vec<String> = tracked
            .iter()
            .filter(|(s, t)| t.last_sent.is_some() && !rows.iter().any(|r| &r.session == *s))
            .map(|(s, _)| s.clone())
            .collect();
        for session in gone {
            tracked.remove(&session);
            send(&mut out, &AgentEvent::Gone { session })?;
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
    }
}

fn send(out: &mut impl Write, ev: &AgentEvent) -> io::Result<()> {
    out.write_all(protocol::encode(ev).as_bytes())?;
    out.flush()
}

/// Lit les commandes de l'application. La fin de stdin (application fermée) arrête l'agent.
fn spawn_stdin_reader() -> Receiver<AgentCmd> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match protocol::decode::<AgentCmd>(&line) {
                Ok(cmd) => {
                    if tx.send(cmd).is_err() {
                        break;
                    }
                }
                Err(e) => eprintln!("commande ignorée ({e}) : {line}"),
            }
        }
        std::process::exit(0);
    });
    rx
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
