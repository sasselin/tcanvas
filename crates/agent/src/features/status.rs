//! Computes a terminal's status from tmux and /proc.

use std::fs;
use std::time::Instant;

use protocol::TermStatus;

use super::agent_hooks::AgentState;

/// Quiet time before assuming a question is waiting for an answer.
const PROMPT_QUIET_SECS: u64 = 2;
/// Quiet time before assuming a coding agent without hooks waits for the user. Their
/// spinners and timers redraw the screen every second while they work.
const AGENT_QUIET_SECS: u64 = 5;
/// Output needed, without a pause, before such an agent counts as working again: a redraw
/// (focus, resize) is a short burst, a spinner never stops.
const AGENT_BUSY_SECS: u64 = 3;
/// Longest pause between two outputs of one continuous run.
pub const BUSY_GAP_SECS: u64 = 2;

/// Coding agent CLIs, by program name.
const CODING_AGENTS: [&str; 10] = ["claude", "codex", "gemini", "agy", "opencode", "aider", "copilot", "cursor-agent", "qwen", "droid"];

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
    /// Exit code of the last finished command, from the shell hooks (see `timing`); `None`
    /// while a new one runs or before any.
    pub last_exit: Option<i32>,
    pub stall_secs: u64,
    /// State published by the hooks of the coding agent in the foreground (already checked
    /// to belong to that process).
    pub agent: Option<AgentState>,
    /// The foreground program is a known coding agent.
    pub coding_agent: bool,
    /// For how long the screen has kept changing (see `BUSY_GAP_SECS`).
    pub busy_secs: u64,
    /// The previous status was `WaitingInput`.
    pub was_waiting: bool,
}

pub fn classify(o: &Observation) -> TermStatus {
    if o.dead {
        return TermStatus::Dead;
    }
    if o.fg_running {
        match o.agent {
            Some(AgentState::Waiting) => return TermStatus::WaitingInput,
            Some(AgentState::Working) => return TermStatus::Running,
            None => {}
        }
        let quiet = o.now.saturating_sub(o.activity);
        if o.coding_agent {
            let waiting = quiet >= AGENT_QUIET_SECS || (o.was_waiting && o.busy_secs < AGENT_BUSY_SECS);
            return if waiting { TermStatus::WaitingInput } else { TermStatus::Running };
        }
        if o.alternate {
            return TermStatus::Running;
        }
        if quiet >= PROMPT_QUIET_SECS && o.last_line.is_some_and(looks_like_question) {
            return TermStatus::WaitingInput;
        }
        if quiet >= o.stall_secs && o.cpu_delta == Some(0) {
            return TermStatus::Stalled;
        }
        return TermStatus::Running;
    }
    match o.last_exit {
        Some(code) if code != 0 => TermStatus::Exited(code),
        _ => TermStatus::Idle,
    }
}

/// Tracks a run of screen output: from its start `since` (none yet) and the time since the
/// last output, `quiet_secs`, returns the run's new start and its length in seconds. The
/// length is the agent's own duration, so it is measured on the monotonic clock: the wall
/// clock can step back (WSL syncs it with Windows) and land before the start.
pub fn busy_run(since: Option<Instant>, now: Instant, quiet_secs: u64) -> (Option<Instant>, u64) {
    if quiet_secs > BUSY_GAP_SECS {
        return (None, 0);
    }
    let since = since.unwrap_or(now);
    (Some(since), now.saturating_duration_since(since).as_secs())
}

pub fn looks_like_question(line: &str) -> bool {
    let l = line.trim_end().to_lowercase();
    const ENDINGS: [&str; 8] = ["[y/n]", "(y/n)", "[yes/no]", "(yes/no)", "[o/n]", "(o/n)", "?", "(yes/no/[fingerprint])?"];
    const KEYWORDS: [&str; 4] = ["password", "passphrase", "mot de passe", "username"];
    ENDINGS.iter().any(|e| l.ends_with(e)) || (l.ends_with(':') && KEYWORDS.iter().any(|k| l.contains(k)))
}

/// Whether `cmdline` runs a coding agent: the program, or the script a runtime runs
/// (`node .../codex.js`, `python -m aider`...).
pub fn is_coding_agent(cmdline: &str) -> bool {
    cmdline.split_whitespace().take(3).any(|arg| {
        let name = arg.rsplit('/').next().unwrap_or(arg);
        let name = name.split_once('.').map_or(name, |(stem, _)| stem);
        CODING_AGENTS.contains(&name)
    })
}

/// Parent pid, from /proc.
pub fn parent_of(pid: u32) -> Option<u32> {
    stat_fields(pid)?.get(1)?.parse().ok()
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
    let cmdline = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect::<Vec<_>>().join(" ");
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
    fn at_the_prompt_a_failed_last_command_shows() {
        // Which command is the last one, across polls, is `timing`'s job.
        let o = Observation { last_exit: Some(1), stall_secs: 60, ..Default::default() };
        assert_eq!(classify(&o), TermStatus::Exited(1));
        let o = Observation { last_exit: None, ..o };
        assert_eq!(classify(&o), TermStatus::Idle);
        let o = Observation { last_exit: Some(0), ..o };
        assert_eq!(classify(&o), TermStatus::Idle);
    }

    #[test]
    fn agent_hooks_decide_while_the_agent_runs() {
        let waiting = Observation { agent: Some(AgentState::Waiting), alternate: true, ..running(1000, 1000) };
        assert_eq!(classify(&waiting), TermStatus::WaitingInput, "right away, even on the alternate screen");
        // A long silent tool run is not stalled while the hooks say "working".
        let working = Observation { agent: Some(AgentState::Working), ..running(5000, 1000) };
        assert_eq!(classify(&working), TermStatus::Running);
        // Back at the shell prompt, a leftover mark means nothing.
        let shell = Observation { agent: Some(AgentState::Waiting), stall_secs: 60, ..Default::default() };
        assert_eq!(classify(&shell), TermStatus::Idle);
    }

    #[test]
    fn coding_agents_without_hooks_wait_once_the_screen_is_quiet() {
        let agent = |now| Observation { coding_agent: true, alternate: true, cpu_delta: Some(3), ..running(now, 1000) };
        assert_eq!(classify(&agent(1004)), TermStatus::Running);
        assert_eq!(classify(&agent(1005)), TermStatus::WaitingInput);
        // Other full-screen apps stay "running".
        // Waiting survives a short redraw (focus, resize), not a run of output.
        let redraw = Observation { was_waiting: true, busy_secs: 2, ..agent(1000) };
        assert_eq!(classify(&redraw), TermStatus::WaitingInput);
        let working = Observation { was_waiting: true, busy_secs: 3, ..agent(1000) };
        assert_eq!(classify(&working), TermStatus::Running);
        let vim = Observation { alternate: true, ..running(5000, 1000) };
        assert_eq!(classify(&vim), TermStatus::Running);
    }

    #[test]
    fn coding_agents_are_recognized() {
        for c in ["claude", "/home/me/.local/bin/claude --resume", "node /usr/lib/node_modules/@openai/codex/bin/codex.js", "gemini", "cursor-agent", "python3 -m aider"] {
            assert!(is_coding_agent(c), "{c}");
        }
        for c in ["vim notes.md", "sleep 4", "node server.js", "claude-squad"] {
            assert!(!is_coding_agent(c), "{c}");
        }
    }

    #[test]
    fn busy_run_lasts_while_output_keeps_coming() {
        use std::time::Duration;
        let t0 = Instant::now();
        let (since, secs) = busy_run(None, t0, 0);
        assert_eq!((since, secs), (Some(t0), 0), "output starts a run");
        assert_eq!(busy_run(since, t0 + Duration::from_secs(3), BUSY_GAP_SECS), (Some(t0), 3), "a short pause keeps it");
        assert_eq!(busy_run(since, t0 + Duration::from_secs(4), BUSY_GAP_SECS + 1), (None, 0), "a longer one ends it");
        assert_eq!(busy_run(None, t0, BUSY_GAP_SECS + 1), (None, 0));
    }

    #[test]
    fn busy_run_start_in_the_future_counts_as_zero() {
        use std::time::Duration;
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(5);
        assert_eq!(busy_run(Some(later), t0, 0), (Some(later), 0));
    }

    #[test]
    fn dead_wins() {
        let o = Observation { dead: true, ..running(0, 0) };
        assert_eq!(classify(&o), TermStatus::Dead);
    }
}
