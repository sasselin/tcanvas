//! Reads the state of the canvas tmux sessions (`tmux -L tcanvas`).

use std::io;
use std::process::{Command, Output};

use protocol::SESSION_PREFIX;

const FORMAT: &str =
    "#{session_name}\t#{pane_active}\t#{window_active}\t#{pane_pid}\t#{pane_dead}\t#{alternate_on}\t#{window_activity}\t#{@tc_started}\t#{@tc_ended}\t#{@tc_exit}\t#{pane_current_path}";

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

pub fn parse_rows(out: &str) -> Vec<PaneRow> {
    out.lines().filter_map(parse_row).collect()
}

fn parse_row(line: &str) -> Option<PaneRow> {
    let f: Vec<&str> = line.splitn(11, '\t').collect();
    if f.len() != 11 || !f[0].starts_with(SESSION_PREFIX) || f[1] != "1" || f[2] != "1" {
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
        path: f[10].to_string(),
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
        let out = "tc_a\t1\t1\t42\t0\t0\t1700\t1690\t1695\t3\t/home/me/my\tdir\n\
                   tc_a\t0\t1\t43\t0\t0\t1700\t\t\t\t/tmp\n\
                   personal\t1\t1\t44\t0\t0\t1700\t\t\t\t/tmp\n\
                   tc_b\t1\t1\t45\t1\t1\t1600\t\t\t\t/\n";
        let rows = parse_rows(out);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session, "tc_a");
        assert_eq!(rows[0].exit, Some(3));
        assert_eq!(rows[0].path, "/home/me/my\tdir");
        assert_eq!(rows[1].started, None);
        assert!(rows[1].dead && rows[1].alternate);
    }
}
