//! Reads the state of the canvas tmux sessions (`tmux -L tcanvas`).

use std::io;
use std::process::{Command, Output};

use protocol::SESSION_PREFIX;

use super::agent_hooks::{self, AgentMark, Owned};

const FORMAT: &str = "#{session_name}\t#{pane_active}\t#{window_active}\t#{pane_pid}\t#{pane_dead}\t#{alternate_on}\t#{window_activity}\t#{@tc_started}\t#{@tc_ended}\t#{@tc_exit}\t#{@tc_agent}\t#{@tc_agent_session}\t#{@tc_agent_transcript}\t#{@tc_agent_prompt}\t#{@tc_agent_prompt_mark}\t#{@tc_agent_turn_mark}\t#{pane_current_path}";
/// Fields in `FORMAT`; the path comes last since it may contain tabs.
const FIELDS: usize = 17;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneRow {
    pub session: String,
    pub pane_pid: u32,
    pub dead: bool,
    pub alternate: bool,
    /// Timestamp (s) of the last displayed output.
    pub activity: u64,
    /// Values set by the shell hooks.
    pub started: Option<u64>,
    pub ended: Option<u64>,
    pub exit: Option<i32>,
    /// Value set by a coding agent's hooks (`@tc_agent`).
    pub agent: Option<AgentMark>,
    /// Payload kept by the same hooks (decoded from hex), with the pids that wrote it.
    pub agent_session: Option<Owned>,
    pub transcript: Option<Owned>,
    pub prompt: Option<Owned>,
    /// The marks of the last prompt submitted and turn ended in the pane
    /// (`agent_hooks::PROMPTS_OPTION`), if any.
    pub prompts: Option<String>,
    pub turns: Option<String>,
    pub path: String,
}

fn tmux(args: &[&str]) -> io::Result<Output> {
    Command::new("tmux").arg("-L").arg(protocol::tmux_socket()).args(args).output()
}

/// Active pane of each `tc_*` session. No sessions if the server is not running.
pub fn list_panes() -> io::Result<Vec<PaneRow>> {
    let out = tmux(&["list-panes", "-a", "-F", FORMAT])?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(parse_rows(&String::from_utf8_lossy(&out.stdout)))
}

/// Current folder of every pane on the canvas socket (every session, every window, not only the
/// active `tc_*` panes). Empty when there can be no pane: tmux is not installed, its server socket
/// does not exist, or nothing listens on it. `None` when tmux fails while its server may be up.
pub fn pane_paths() -> Option<Vec<String>> {
    let socket = socket_path();
    match tmux(&["list-panes", "-a", "-F", "#{pane_current_path}"]) {
        Ok(out) if out.status.success() => Some(String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.is_empty()).map(String::from).collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(Vec::new()),
        Err(_) => (!socket.exists()).then(Vec::new),
        Ok(out) => no_server(socket.exists(), &String::from_utf8_lossy(&out.stderr)).then(Vec::new),
    }
}

/// A failed tmux call means "no server" when its socket is absent, or when tmux says nothing
/// listens on it (a socket left behind by a server that died).
pub fn no_server(socket_exists: bool, stderr: &str) -> bool {
    !socket_exists || stderr.contains("no server running") || stderr.contains("Connection refused")
}

/// tmux's socket file for the canvas socket name: `$TMUX_TMPDIR` (default `/tmp`), then
/// `tmux-<uid>/<name>`.
fn socket_path() -> std::path::PathBuf {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
    let tmp = std::env::var("TMUX_TMPDIR").ok().filter(|t| !t.is_empty()).unwrap_or_else(|| "/tmp".into());
    std::path::Path::new(&tmp).join(format!("tmux-{uid}")).join(protocol::tmux_socket())
}

pub fn parse_rows(out: &str) -> Vec<PaneRow> {
    out.lines().filter_map(parse_row).collect()
}

fn parse_row(line: &str) -> Option<PaneRow> {
    let f: Vec<&str> = line.splitn(FIELDS, '\t').collect();
    if f.len() != FIELDS || !f[0].starts_with(SESSION_PREFIX) || f[1] != "1" || f[2] != "1" {
        return None;
    }
    Some(PaneRow {
        session: f[0].to_string(),
        pane_pid: f[3].parse().ok()?,
        dead: f[4] == "1",
        alternate: f[5] == "1",
        activity: f[6].parse().unwrap_or(0),
        started: f[7].parse().ok(),
        ended: f[8].parse().ok(),
        exit: f[9].parse().ok(),
        agent: agent_hooks::parse_mark(f[10]),
        agent_session: agent_hooks::decode_owned(f[11]),
        transcript: agent_hooks::decode_owned(f[12]),
        prompt: agent_hooks::decode_owned(f[13]),
        prompts: Some(f[14].trim().to_string()).filter(|m| !m.is_empty()),
        turns: Some(f[15].trim().to_string()).filter(|m| !m.is_empty()),
        path: f[16].to_string(),
    })
}

/// The last `n` non-empty lines of the visible screen.
pub fn capture_tail(session: &str, n: usize) -> Vec<String> {
    let Ok(out) = tmux(&["capture-pane", "-p", "-t", session]) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).filter(|l| !l.is_empty()).collect();
    let skip = lines.len().saturating_sub(n);
    lines.drain(..skip);
    lines
}

pub fn kill(session: &str) {
    let _ = tmux(&["kill-session", "-t", session]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_active_canvas_panes_only() {
        let out = "tc_a\t1\t1\t42\t0\t0\t1700\t1690\t1695\t3\twaiting 50,42\t50,42 7331\t50 2f742e6a736f6e6c\t42,50 48690a7468657265\t3\t2\t/home/me/my\tdir\n\
                   tc_a\t0\t1\t43\t0\t0\t1700\t\t\t\t\t\t\t\t\t\t/tmp\n\
                   personal\t1\t1\t44\t0\t0\t1700\t\t\t\t\t\t\t\t\t\t/tmp\n\
                   tc_b\t1\t1\t45\t1\t1\t1600\t\t\t\t\t7331\t45 zz\t\tx\t\t/\n";
        let rows = parse_rows(out);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session, "tc_a");
        assert_eq!(rows[0].exit, Some(3));
        assert_eq!(rows[0].path, "/home/me/my\tdir");
        assert_eq!(rows[0].agent, Some(AgentMark { state: agent_hooks::AgentState::Waiting, pids: vec![50, 42] }));
        assert_eq!(rows[1].started, None);
        assert_eq!(rows[0].agent_session, Some(Owned { pids: vec![50, 42], text: "s1".into() }));
        assert_eq!(rows[0].transcript, Some(Owned { pids: vec![50], text: "/t.jsonl".into() }));
        assert_eq!(rows[0].prompt, Some(Owned { pids: vec![42, 50], text: "Hi\nthere".into() }));
        assert_eq!(rows[1].agent, None);
        assert_eq!((&rows[1].agent_session, &rows[1].prompt), (&None, &None), "no owner, malformed or empty: nothing");
        assert!(rows[1].dead && rows[1].alternate);
        assert_eq!((rows[0].prompts.as_deref(), rows[0].turns.as_deref(), rows[1].prompts.as_deref(), rows[1].turns.as_deref()), (Some("3"), Some("2"), Some("x"), None), "marks as written");
    }

    #[test]
    fn a_failing_tmux_means_no_server_only_when_it_says_so() {
        assert!(no_server(false, "anything"), "no socket file");
        assert!(no_server(true, "no server running on /tmp/tmux-1000/tcanvas"));
        assert!(no_server(true, "error connecting to /tmp/tmux-1000/tcanvas (Connection refused)"));
        assert!(!no_server(true, "protocol version mismatch"), "a server may be up: unknown");
        assert!(!no_server(true, ""));
    }
}
