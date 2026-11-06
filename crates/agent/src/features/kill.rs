//! Full shutdown of a session: every process of its terminals, even those that ignore
//! SIGHUP (`nohup`, `setsid`, `trap '' HUP TERM`…), then the tmux session itself.
//!
//! Targets, three complementary nets:
//! - the descendants of the panes' shells;
//! - any process in the same Unix session (`nohup … &` reparented to init);
//! - any process that inherited `TMUX_PANE` from one of these panes (`setsid … &`,
//!   double-fork daemons: they leave the tree and the session but keep their environment).
//!
//! Two-step shutdown: SIGHUP + SIGTERM to let programs exit cleanly,
//! then SIGKILL for the stubborn ones.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use super::tmux;

const GRACE: Duration = Duration::from_millis(1500);
const AFTER_KILL: Duration = Duration::from_millis(1000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    pub sid: u32,
}

/// Processes to stop: descendants of `roots`, members of their Unix sessions, and `extra`
/// (found through their environment).
pub fn targets(procs: &[ProcInfo], roots: &[u32], extra: &[u32], exclude: u32) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let sids: HashSet<u32> = procs.iter().filter(|p| roots.contains(&p.pid)).map(|p| p.sid).collect();
    let mut found: HashSet<u32> = HashSet::new();
    let mut stack: Vec<u32> = roots.to_vec();
    while let Some(pid) = stack.pop() {
        if found.insert(pid) {
            stack.extend(children.get(&pid).into_iter().flatten());
        }
    }
    found.extend(procs.iter().filter(|p| sids.contains(&p.sid)).map(|p| p.pid));
    // Processes found through the environment also take their own descendants with them.
    let mut stack: Vec<u32> = extra.to_vec();
    while let Some(pid) = stack.pop() {
        if found.insert(pid) || extra.contains(&pid) {
            stack.extend(children.get(&pid).into_iter().flatten().filter(|c| !found.contains(c)));
        }
    }
    // The tmux server itself is never a target: it serves the other sessions.
    found.remove(&exclude);
    found.remove(&0);
    found.remove(&1);
    let mut list: Vec<u32> = found.into_iter().collect();
    list.sort_unstable();
    list
}

/// `pid ppid sid` from /proc/<pid>/stat (the parenthesized name may contain spaces).
pub fn parse_stat(pid: u32, stat: &str) -> Option<ProcInfo> {
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some(ProcInfo { pid, ppid: f.get(1)?.parse().ok()?, sid: f.get(3)?.parse().ok()? })
}

/// Did the process inherit from one of these panes, on this tmux server?
/// `environ`: raw contents of /proc/<pid>/environ (NUL-separated variables).
pub fn env_belongs_to(environ: &[u8], socket_path: &str, panes: &[String]) -> bool {
    let mut on_server = false;
    let mut in_pane = false;
    for var in environ.split(|b| *b == 0) {
        let var = String::from_utf8_lossy(var);
        if let Some(v) = var.strip_prefix("TMUX=") {
            on_server = v.split(',').next() == Some(socket_path);
        } else if let Some(v) = var.strip_prefix("TMUX_PANE=") {
            in_pane = panes.iter().any(|p| p == v);
        }
    }
    on_server && in_pane
}

fn env_matches(procs: &[ProcInfo], socket_path: &str, panes: &[String]) -> Vec<u32> {
    if socket_path.is_empty() || panes.is_empty() {
        return Vec::new();
    }
    procs.iter().filter(|p| fs::read(format!("/proc/{}/environ", p.pid)).is_ok_and(|env| env_belongs_to(&env, socket_path, panes))).map(|p| p.pid).collect()
}

fn all_procs() -> Vec<ProcInfo> {
    let Ok(dir) = fs::read_dir("/proc") else { return Vec::new() };
    dir.filter_map(Result::ok).filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok()).filter_map(|pid| parse_stat(pid, &fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)).collect()
}

/// Alive = exists and is not a zombie (a zombie is dead, it is just waiting for its parent).
fn alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).ok().and_then(|s| s.get(s.rfind(')')? + 1..).and_then(|r| r.split_whitespace().next()).map(|st| st != "Z")).unwrap_or(false)
}

fn signal(pids: &[u32], sig: i32) {
    for &pid in pids {
        // SAFETY: kill(2) has no memory invariants; an error (process already gone) is ignored.
        unsafe {
            libc::kill(pid as i32, sig);
        }
    }
}

fn wait_gone(pids: &[u32], limit: Duration) -> Vec<u32> {
    let deadline = Instant::now() + limit;
    loop {
        let left: Vec<u32> = pids.iter().copied().filter(|&p| alive(p)).collect();
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn tmux_lines(args: &[&str]) -> Vec<String> {
    let out = Command::new("tmux").arg("-L").arg(protocol::tmux_socket()).args(args).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_string()).collect()).unwrap_or_default()
}

/// (shell pid, `%N` id) of each pane of the session.
fn panes(session: &str) -> Vec<(u32, String)> {
    tmux_lines(&["list-panes", "-s", "-t", session, "-F", "#{pane_pid} #{pane_id}"])
        .iter()
        .filter_map(|l| {
            let (pid, id) = l.split_once(' ')?;
            Some((pid.parse().ok()?, id.to_string()))
        })
        .collect()
}

/// (server pid, socket path).
fn server() -> (u32, String) {
    let line = tmux_lines(&["display-message", "-p", "#{pid} #{socket_path}"]).into_iter().next().unwrap_or_default();
    let (pid, path) = line.split_once(' ').unwrap_or(("0", ""));
    (pid.parse().unwrap_or(0), path.to_string())
}

#[derive(Debug, PartialEq, Eq)]
pub struct KillReport {
    pub targeted: usize,
    pub needed_sigkill: usize,
    pub survivors: Vec<u32>,
}

/// Stops every process of the session, then the session. Blocks for at most ~2.5 s.
pub fn kill_session(session: &str) -> KillReport {
    let panes = panes(session);
    let roots: Vec<u32> = panes.iter().map(|p| p.0).collect();
    let ids: Vec<String> = panes.into_iter().map(|p| p.1).collect();
    let (server_pid, socket_path) = server();
    let procs = all_procs();
    let by_env = env_matches(&procs, &socket_path, &ids);
    let list = targets(&procs, &roots, &by_env, server_pid);
    signal(&list, libc::SIGHUP);
    signal(&list, libc::SIGTERM);
    let stubborn = wait_gone(&list, GRACE);
    signal(&stubborn, libc::SIGKILL);
    let survivors = wait_gone(&stubborn, AFTER_KILL);
    tmux::kill(session);
    KillReport { targeted: list.len(), needed_sigkill: stubborn.len(), survivors }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, sid: u32) -> ProcInfo {
        ProcInfo { pid, ppid, sid }
    }

    #[test]
    fn descendants_and_session_members_but_not_the_server() {
        let procs = [
            p(1, 0, 1),
            p(100, 1, 100),   // tmux server
            p(200, 100, 200), // pane shell (session 200)
            p(201, 200, 200), // command
            p(202, 201, 202), // setsid: other session, but a descendant
            p(203, 1, 200),   // nohup reparented to init, same session
            p(300, 100, 300), // shell of another tmux session: off limits
            p(301, 300, 300),
        ];
        assert_eq!(targets(&procs, &[200], &[], 100), vec![200, 201, 202, 203]);
    }

    #[test]
    fn env_matched_daemons_bring_their_children() {
        let procs = [p(1, 0, 1), p(100, 1, 100), p(200, 100, 200), p(400, 1, 400), p(401, 400, 400), p(500, 1, 500)];
        assert_eq!(targets(&procs, &[200], &[400], 100), vec![200, 400, 401]);
    }

    #[test]
    fn no_roots_no_targets() {
        assert!(targets(&[p(5, 1, 5)], &[], &[], 0).is_empty());
    }

    #[test]
    fn environment_matching() {
        let panes = vec!["%3".to_string()];
        let env = |s: &str| s.replace(';', "\0").into_bytes();
        assert!(env_belongs_to(&env("A=1;TMUX=/tmp/tmux-1000/tcanvas,99,2;TMUX_PANE=%3"), "/tmp/tmux-1000/tcanvas", &panes));
        assert!(!env_belongs_to(&env("TMUX=/tmp/tmux-1000/tcanvas,99,2;TMUX_PANE=%4"), "/tmp/tmux-1000/tcanvas", &panes), "other pane");
        assert!(!env_belongs_to(&env("TMUX=/tmp/tmux-1000/default,7,0;TMUX_PANE=%3"), "/tmp/tmux-1000/tcanvas", &panes), "other server");
        assert!(!env_belongs_to(&env("PATH=/bin"), "/tmp/tmux-1000/tcanvas", &panes));
    }

    #[test]
    fn stat_parsing_handles_spaces_in_names() {
        let stat = "4242 (my weird (name)) S 17 4242 4242 34816 4242 4194560 0";
        assert_eq!(parse_stat(4242, stat), Some(ProcInfo { pid: 4242, ppid: 17, sid: 4242 }));
    }
}
