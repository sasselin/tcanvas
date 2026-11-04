//! Computes a terminal's status from tmux and /proc.

use std::fs;

use protocol::TermStatus;

/// Quiet time before assuming a question is waiting for an answer.
const PROMPT_QUIET_SECS: u64 = 2;

#[derive(Clone, Debug, Default)]
pub struct Observation<'a> {
    pub now: u64,
    pub dead: bool,
    /// Full-screen app (vim, htop…): never "stalled".
    pub alternate: bool,
    /// A process other than the shell is in the foreground.
    pub fg_running: bool,
    pub activity: u64,
    /// CPU ticks used by the foreground since the last observation.
    pub cpu_delta: Option<u64>,
    pub last_line: Option<&'a str>,
    pub hook_started: Option<u64>,
    pub hook_ended: Option<u64>,
    pub hook_exit: Option<i32>,
    pub stall_secs: u64,
}

pub fn classify(o: &Observation) -> TermStatus {
    if o.dead {
        return TermStatus::Dead;
    }
    if o.fg_running {
        if o.alternate {
            return TermStatus::Running;
        }
        let quiet = o.now.saturating_sub(o.activity);
        if quiet >= PROMPT_QUIET_SECS && o.last_line.is_some_and(looks_like_question) {
            return TermStatus::WaitingInput;
        }
        if quiet >= o.stall_secs && o.cpu_delta == Some(0) {
            return TermStatus::Stalled;
        }
        return TermStatus::Running;
    }
    match last_command(o) {
        (Some(code), _) if code != 0 => TermStatus::Exited(code),
        _ => TermStatus::Idle,
    }
}

/// Exit code and duration of the last finished command, from the hooks.
pub fn last_command(o: &Observation) -> (Option<i32>, Option<u64>) {
    match (o.hook_started, o.hook_ended) {
        (Some(s), Some(e)) if e >= s => (o.hook_exit, Some(e - s)),
        _ => (None, None),
    }
}

pub fn looks_like_question(line: &str) -> bool {
    let l = line.trim_end().to_lowercase();
    const ENDINGS: [&str; 8] = ["[y/n]", "(y/n)", "[yes/no]", "(yes/no)", "[o/n]", "(o/n)", "?", "(yes/no/[fingerprint])?"];
    const KEYWORDS: [&str; 4] = ["password", "passphrase", "mot de passe", "username"];
    ENDINGS.iter().any(|e| l.ends_with(e))
        || (l.ends_with(':') && KEYWORDS.iter().any(|k| l.contains(k)))
}

/// Foreground process of the terminal whose shell is `shell_pid`.
pub struct Foreground {
    pub pid: u32,
    pub cpu_ticks: u64,
    pub cmdline: String,
}

/// `None` if the shell itself is in the foreground (or /proc is unreadable).
pub fn foreground(shell_pid: u32) -> Option<Foreground> {
    let tpgid = stat_fields(shell_pid)?.get(5)?.parse::<i64>().ok()?;
    if tpgid <= 0 || tpgid as u32 == shell_pid {
        return None;
    }
    let pid = tpgid as u32;
    let f = stat_fields(pid)?;
    let cpu_ticks = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let cmdline = raw
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    Some(Foreground { pid, cpu_ticks, cmdline })
}

/// Fields of /proc/<pid>/stat from `state` on (the parenthesized name is stripped).
fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    Some(rest.split_whitespace().map(str::to_string).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(now: u64, activity: u64) -> Observation<'static> {
        Observation { now, activity, fg_running: true, cpu_delta: Some(0), stall_secs: 60, ..Default::default() }
    }

    #[test]
    fn idle_at_prompt() {
        let o = Observation { stall_secs: 60, ..Default::default() };
        assert_eq!(classify(&o), TermStatus::Idle);
    }

    #[test]
    fn running_then_stalled() {
        assert_eq!(classify(&running(1059, 1000)), TermStatus::Running);
        assert_eq!(classify(&running(1060, 1000)), TermStatus::Stalled);
    }

    #[test]
    fn busy_cpu_is_not_stalled() {
        let o = Observation { cpu_delta: Some(40), ..running(2000, 1000) };
        assert_eq!(classify(&o), TermStatus::Running);
    }

    #[test]
    fn fullscreen_apps_never_stall() {
        let o = Observation { alternate: true, ..running(5000, 1000) };
        assert_eq!(classify(&o), TermStatus::Running);
    }

    #[test]
    fn question_detected_after_quiet_period() {
        let o = Observation { last_line: Some("Continue? [Y/n]"), ..running(1001, 1000) };
        assert_eq!(classify(&o), TermStatus::Running);
        let o = Observation { last_line: Some("Continue? [Y/n]"), ..running(1002, 1000) };
        assert_eq!(classify(&o), TermStatus::WaitingInput);
        let o = Observation { last_line: Some("[sudo] password for me:"), ..running(1003, 1000) };
        assert_eq!(classify(&o), TermStatus::WaitingInput);
        let o = Observation { last_line: Some("compiling foo: 3/9"), ..running(1003, 1000) };
        assert_eq!(classify(&o), TermStatus::Running);
    }

    #[test]
    fn failed_command_until_next_one() {
        let o = Observation { hook_started: Some(10), hook_ended: Some(45), hook_exit: Some(1), stall_secs: 60, ..Default::default() };
        assert_eq!(classify(&o), TermStatus::Exited(1));
        assert_eq!(last_command(&o), (Some(1), Some(35)));
        // New command started: started > ended.
        let o = Observation { hook_started: Some(50), ..o };
        assert_eq!(classify(&o), TermStatus::Idle);
        assert_eq!(last_command(&o), (None, None));
    }

    #[test]
    fn dead_wins() {
        let o = Observation { dead: true, ..running(0, 0) };
        assert_eq!(classify(&o), TermStatus::Dead);
    }
}
