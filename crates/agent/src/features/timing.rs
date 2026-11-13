//! The last command of a shell, from the marks its tcanvas hooks publish (`@tc_started`,
//! `@tc_ended`, `@tc_exit`), whatever the wall clock does.
//!
//! How the hooks write the marks (see `hooks.rs`):
//! - bash: nothing when a command starts; when it ends, `PROMPT_COMMAND` sets `@tc_started`,
//!   `@tc_exit` and `@tc_ended` in one tmux call. The three marks always describe one finished
//!   command, so any change of them is a command that ended.
//! - zsh: `preexec` sets `@tc_started` alone when a command starts, leaving the previous
//!   command's `@tc_exit` and `@tc_ended` in place; `precmd` then sets `@tc_exit` and
//!   `@tc_ended` in one tmux call. A command's end is always written after its start.
//!
//! The marks are wall clock seconds (`EPOCHSECONDS`), and WSL steps the wall clock back to
//! follow Windows (about 1.8 s every 30 s on some machines): an end can be stamped before its
//! start. So which end goes with which start is decided from the order in which the agent sees
//! the marks change, not from their values. Other shells than bash follow the zsh rules:
//! - `@tc_ended` or `@tc_exit` changed: the end of the latest start (zsh writes an end after its
//!   start). Except when `@tc_started` changed too, the end is stamped before the start and a
//!   process runs in the foreground: a command ended and the next one started between two polls
//!   (type-ahead), and that one is running;
//! - `@tc_started` changed alone: a new command that has not ended yet;
//! - nothing changed: same answer as before.
//!
//! The first look at a pane (agent started, or another pane became the session's active one)
//! has no history: an end stamped before its start is taken for a clock step only without a
//! foreground process and by at most `MAX_CLOCK_STEP_SECS`, else for a new command running.
//!
//! Left to the unchanged zsh hooks: a zsh command whose end mark and exit code both equal the
//! previous command's (a step back into the same second, same code) is not seen ending until
//! the next command.
//!
//! Durations are never meant to exceed the truth by more than a second; when a correction
//! cannot be placed for sure, it is left out (the duration may then come out short, or not at
//! all). A zsh command whose start the agent saw at an earlier poll than its end is timed on
//! the agent's monotonic clock, poll to poll (one poll of precision). Any other one (bash, whose
//! start is only published at the end; a zsh command shorter than a poll) takes the marks'
//! difference, corrected only by the wall clock steps certain to fall inside it (see
//! `ClockLog::inside`). On a first look the plain difference is used, without correction, and
//! dropped if negative. Forward jumps of the wall clock during a command are only taken off
//! when certain to fall inside, like steps back.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::hooks::Shell;

/// Largest gap by which a clock step can put an end mark before its start (see the module
/// docs): measured steps are under 2 s, plus a second lost to whole-second marks.
pub const MAX_CLOCK_STEP_SECS: u64 = 5;
/// Offset changes between two polls below this are noise, not a step.
const STEP_MIN_SECS: f64 = 0.1;
/// Steps kept: many hours of steps every 30 s.
const MAX_STEPS: usize = 4096;
/// Seconds added to a command's longest possible span to cover the polls around it.
const SPAN_SLACK_SECS: f64 = 2.0 + MAX_CLOCK_STEP_SECS as f64;

/// The marks of a pane, as read from tmux.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pub started: Option<u64>,
    pub ended: Option<u64>,
    pub exit: Option<i32>,
}

/// The last finished command: exit code and duration (s), `None` when unknown or not
/// trustworthy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Finished {
    pub exit: Option<i32>,
    pub secs: Option<u64>,
}

/// What the agent knows of a pane's last command, across polls.
#[derive(Clone, Debug, Default)]
pub struct LastCommand {
    /// Marks at the previous poll; `None` before the first one.
    seen: Option<Marks>,
    /// Poll that saw the start mark of the command now running change alone (zsh).
    start_seen: Option<Instant>,
    /// First of the polls, without a gap, that saw a foreground process since the last end:
    /// the command now running was already running then.
    busy_seen: Option<Instant>,
    /// Computed once, when its end is seen, so later polls report the same values.
    finished: Option<Finished>,
}

impl LastCommand {
    /// Takes the marks read at the poll of `now` (monotonic clock), the pane's shell, and
    /// whether a process other than the shell is in the foreground, and returns the last
    /// finished command: `None` while a new one runs (as far as the marks tell) or before any.
    pub fn observe(&mut self, marks: Marks, shell: Option<Shell>, fg_running: bool, now: Instant, clock: &ClockLog) -> Option<Finished> {
        let bash = shell == Some(Shell::Bash);
        let Some(prev) = self.seen.replace(marks) else {
            self.finished = match (marks.started, marks.ended) {
                (_, None) => None,
                (Some(s), Some(e)) if !bash && e < s && (fg_running || e.saturating_add(MAX_CLOCK_STEP_SECS) < s) => None,
                _ => Some(Finished { exit: marks.exit, secs: plain_secs(marks) }),
            };
            self.busy_seen = fg_running.then_some(now);
            return self.finished;
        };
        let start_changed = prev.started != marks.started;
        let end_changed = (prev.ended, prev.exit) != (marks.ended, marks.exit);
        let stamped_before = matches!((marks.started, marks.ended), (Some(s), Some(e)) if e < s);
        if end_changed && marks.ended.is_none() {
            self.finished = None;
            self.start_seen = None;
        } else if end_changed && !bash && start_changed && fg_running && stamped_before {
            // Type-ahead: the end is the previous command's, the start a new one's, running.
            self.finished = None;
            self.start_seen = Some(now);
            self.busy_seen = Some(now);
        } else if end_changed || (bash && start_changed) {
            let secs = match self.start_seen {
                Some(at) if !start_changed => Some(round_secs(now.saturating_duration_since(at).as_secs_f64())),
                // Started after the last poll: the foreground process seen before was the
                // previous command's (zsh type-ahead, both marks seen at once).
                Some(_) => hook_secs(marks, None, now, clock),
                None => hook_secs(marks, self.busy_seen, now, clock),
            };
            self.finished = Some(Finished { exit: marks.exit, secs });
            self.start_seen = None;
            // A foreground process at the poll of an end is the next command's.
            self.busy_seen = fg_running.then_some(now);
        } else if start_changed {
            self.start_seen = marks.started.map(|_| now);
            self.finished = None;
        }
        // A poll without a foreground process breaks the sighting: what runs next may be
        // another command. (At the poll of an end, the sighting was used just above.)
        if fg_running {
            self.busy_seen.get_or_insert(now);
        } else {
            self.busy_seen = None;
        }
        self.finished
    }
}

/// The marks' plain difference, `None` if negative.
fn plain_secs(marks: Marks) -> Option<u64> {
    marks.ended?.checked_sub(marks.started?)
}

/// Duration from the marks of a command whose end was seen at the poll `end_at`: their
/// difference minus the wall clock steps certain to fall inside it. `busy_at`: a poll that saw
/// the command running. `None` when negative even so.
fn hook_secs(marks: Marks, busy_at: Option<Instant>, end_at: Instant, clock: &ClockLog) -> Option<u64> {
    let raw = marks.ended? as f64 - marks.started? as f64;
    let secs = raw - clock.inside(raw, busy_at, end_at);
    // Half a second of slack: the marks are whole seconds.
    (secs > -0.5).then(|| round_secs(secs))
}

fn round_secs(secs: f64) -> u64 {
    secs.round().max(0.0) as u64
}

/// A change of the wall clock against the monotonic one, recorded at the poll `seen`: it
/// happened after the poll `after` (the previous one), by `secs` (negative: back).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Step {
    after: Instant,
    seen: Instant,
    secs: f64,
}

/// The wall clock measured against the monotonic one at every poll: its steps (back, or
/// ahead). A slow drift under `STEP_MIN_SECS` per poll is not recorded.
#[derive(Clone, Debug, Default)]
pub struct ClockLog {
    origin: Option<Instant>,
    /// Previous poll and its offset (wall minus monotonic time since `origin`, in s).
    last: Option<(Instant, f64)>,
    steps: VecDeque<Step>,
}

impl ClockLog {
    /// Records the wall clock reading `wall` (s since the epoch) taken at the poll `mono`.
    pub fn record(&mut self, mono: Instant, wall: f64) {
        let origin = *self.origin.get_or_insert(mono);
        let offset = wall - mono.saturating_duration_since(origin).as_secs_f64();
        if let Some((after, last)) = self.last
            && (offset - last).abs() >= STEP_MIN_SECS
        {
            if self.steps.len() == MAX_STEPS {
                self.steps.pop_front();
            }
            self.steps.push_back(Step { after, seen: mono, secs: offset - last });
        }
        self.last = Some((mono, offset));
    }

    /// Sum of the steps certain to fall inside a command whose marks differ by `raw` seconds
    /// and whose end was seen at the poll `end_at`, `busy_at` being a poll that saw it running.
    ///
    /// A step counts when the poll before the one that recorded it is not before the latest
    /// moment the command can have started, and its recording poll is before `end_at` (one
    /// recorded at `end_at` may have come after the end). That moment is `end_at` minus the
    /// shortest duration the marks allow (`raw` - 1, plus any jump ahead that may lie inside),
    /// or `busy_at` if earlier. `busy_at` is ignored when older than the longest the command can
    /// have lasted (`raw` + 1, plus every step back that may lie inside): it cannot be its.
    pub fn inside(&self, raw: f64, busy_at: Option<Instant>, end_at: Instant) -> f64 {
        let may_be_inside = |longest: f64| {
            let from = end_at.checked_sub(Duration::from_secs_f64(longest.max(0.0) + SPAN_SLACK_SECS));
            self.steps.iter().filter(move |s| s.after < end_at && from.is_none_or(|f| s.seen > f))
        };
        let mut longest = raw.max(0.0) + 1.0;
        for _ in 0..8 {
            let back: f64 = may_be_inside(longest).filter(|s| s.secs < 0.0).map(|s| -s.secs).sum();
            let next = raw.max(0.0) + 1.0 + back;
            if next <= longest {
                break;
            }
            longest = next;
        }
        let ahead: f64 = may_be_inside(longest).filter(|s| s.secs > 0.0).map(|s| s.secs).sum();
        let shortest = raw - 1.0 - ahead;
        if shortest < 0.0 {
            // The command may have lasted no time at all: no step is certain inside.
            return 0.0;
        }
        let Some(mut latest_start) = end_at.checked_sub(Duration::from_secs_f64(shortest)) else { return 0.0 };
        let earliest = end_at.checked_sub(Duration::from_secs_f64(longest + SPAN_SLACK_SECS));
        if let Some(busy) = busy_at.filter(|b| earliest.is_none_or(|e| *b >= e)) {
            latest_start = latest_start.min(busy);
        }
        self.steps.iter().filter(|s| s.after >= latest_start && s.seen < end_at).map(|s| s.secs).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASH: Option<Shell> = Some(Shell::Bash);
    const ZSH: Option<Shell> = Some(Shell::Zsh);

    fn marks(started: u64, ended: Option<u64>, exit: Option<i32>) -> Marks {
        Marks { started: Some(started), ended, exit }
    }

    fn secs(n: f64) -> Duration {
        Duration::from_secs_f64(n)
    }

    fn done(exit: i32, secs: Option<u64>) -> Option<Finished> {
        Some(Finished { exit: Some(exit), secs })
    }

    /// Polls every second from `t0` to `t0 + 60 s`, the wall clock reading `wall0` at `t0`,
    /// stepped by `step` s from the poll at `t0 + step_at` on.
    fn clock(t0: Instant, wall0: f64, step_at: u64, step: f64) -> ClockLog {
        let mut log = ClockLog::default();
        for i in 0..=60 {
            let jump = if i >= step_at { step } else { 0.0 };
            log.record(t0 + secs(i as f64), wall0 + i as f64 + jump);
        }
        log
    }

    #[test]
    fn failed_command_until_next_one() {
        let t0 = Instant::now();
        let log = ClockLog::default();
        let mut last = LastCommand::default();
        assert_eq!(last.observe(marks(10, Some(45), Some(1)), ZSH, false, t0, &log), done(1, Some(35)));
        assert_eq!(last.observe(marks(10, Some(45), Some(1)), ZSH, false, t0 + secs(1.0), &log), done(1, Some(35)));
        // A new command started: no last exit code while it runs.
        assert_eq!(last.observe(marks(50, Some(45), Some(1)), ZSH, true, t0 + secs(2.0), &log), None);
        assert_eq!(last.observe(marks(50, Some(52), Some(0)), ZSH, false, t0 + secs(4.0), &log), done(0, Some(2)));
    }

    #[test]
    fn zsh_end_before_start_still_reports_the_exit_code() {
        let t0 = Instant::now();
        let log = ClockLog::default();
        let mut last = LastCommand::default();
        assert_eq!(last.observe(marks(100, Some(100), Some(0)), ZSH, false, t0, &log), done(0, Some(0)));
        // `preexec` at 105: running, with or without a foreground process (a builtin).
        assert_eq!(last.observe(marks(105, Some(100), Some(0)), ZSH, false, t0 + secs(1.0), &log), None);
        assert_eq!(last.observe(marks(105, Some(100), Some(0)), ZSH, true, t0 + secs(2.0), &log), None, "still running");
        // `precmd` after the clock stepped back 2 s: end stamped 104, before the start.
        let ended = last.observe(marks(105, Some(104), Some(1)), ZSH, false, t0 + secs(3.0), &log);
        assert_eq!(ended, done(1, Some(2)), "timed poll to poll");
        assert_eq!(last.observe(marks(105, Some(104), Some(1)), ZSH, false, t0 + secs(9.0), &log), ended, "stable");
    }

    #[test]
    fn bash_end_before_start_reports_the_exit_code() {
        // bash writes the three marks at once: they always go together, even with a
        // foreground process (the next command's).
        let t0 = Instant::now();
        let log = ClockLog::default();
        let mut last = LastCommand::default();
        assert_eq!(last.observe(Marks::default(), BASH, false, t0, &log), None);
        let ended = last.observe(marks(105, Some(104), Some(2)), BASH, true, t0 + secs(1.0), &log);
        assert_eq!(ended, done(2, None), "no step seen: no duration rather than a wrong one");
    }

    #[test]
    fn bash_start_only_change_is_a_finished_command() {
        // Previous failure ended at 100; the next one, also failing, is stamped 100 to 100
        // after a step back: only `@tc_started` changes.
        let t0 = Instant::now();
        let log = ClockLog::default();
        let mut last = LastCommand::default();
        last.observe(marks(97, Some(100), Some(1)), BASH, false, t0, &log);
        assert_eq!(last.observe(marks(100, Some(100), Some(1)), BASH, false, t0 + secs(1.0), &log), done(1, Some(0)));
    }

    #[test]
    fn a_new_start_after_an_old_end_is_a_running_command() {
        let t0 = Instant::now();
        let log = ClockLog::default();
        let mut last = LastCommand::default();
        last.observe(marks(90, Some(100), Some(3)), ZSH, false, t0, &log);
        assert_eq!(last.observe(marks(90, Some(100), Some(3)), ZSH, false, t0 + secs(1.0), &log), done(3, Some(10)));
        // Even stamped before the old end (clock stepped back), a lone new start runs.
        assert_eq!(last.observe(marks(99, Some(100), Some(3)), ZSH, false, t0 + secs(2.0), &log), None);
    }

    #[test]
    fn zsh_type_ahead_shows_the_next_command_running() {
        // A failed at 100 and B started at 101 between two polls; B runs in the foreground.
        let t0 = Instant::now();
        let log = ClockLog::default();
        let mut last = LastCommand::default();
        last.observe(marks(90, Some(95), Some(0)), ZSH, false, t0, &log);
        last.observe(marks(98, Some(95), Some(0)), ZSH, true, t0 + secs(1.0), &log);
        assert_eq!(last.observe(marks(101, Some(100), Some(1)), ZSH, true, t0 + secs(3.0), &log), None);
        assert_eq!(last.observe(marks(101, Some(106), Some(0)), ZSH, false, t0 + secs(8.0), &log), done(0, Some(5)));
    }

    #[test]
    fn a_step_back_during_a_command_does_not_shorten_it() {
        // Wall clock 2 s back between the polls at t0 + 4 and t0 + 5. A command from t0 + 1 to
        // t0 + 7 is stamped 1001 to 1005.
        let t0 = Instant::now();
        let log = clock(t0, 1000.0, 5, -2.0);

        // zsh: start and end seen at different polls.
        let mut last = LastCommand::default();
        last.observe(marks(990, Some(995), Some(0)), ZSH, false, t0, &log);
        last.observe(marks(1001, Some(995), Some(0)), ZSH, false, t0 + secs(1.0), &log);
        assert_eq!(last.observe(marks(1001, Some(1005), Some(0)), ZSH, false, t0 + secs(7.0), &log), done(0, Some(6)));

        // bash: only the end is seen; a step certain to be inside is added back.
        let mut last = LastCommand::default();
        last.observe(marks(990, Some(995), Some(0)), BASH, false, t0, &log);
        assert_eq!(last.observe(marks(1001, Some(1005), Some(0)), BASH, false, t0 + secs(7.0), &log), done(0, Some(6)));
    }

    #[test]
    fn a_step_right_after_the_start_counts_once_the_command_was_seen_running() {
        // Wall clock 2 s back between t0 + 2 and t0 + 3; a command from t0 + 1 to t0 + 7 is
        // stamped 1001 to 1005: from the marks alone it may have started after the step.
        let t0 = Instant::now();
        let log = clock(t0, 1000.0, 3, -2.0);
        let mut last = LastCommand::default();
        last.observe(marks(990, Some(995), Some(0)), BASH, false, t0, &log);
        assert_eq!(last.observe(marks(1001, Some(1005), Some(0)), BASH, false, t0 + secs(7.0), &log), done(0, Some(4)), "short rather than long");

        // A foreground process seen from t0 + 2 on proves it ran before the step.
        let mut last = LastCommand::default();
        last.observe(marks(990, Some(995), Some(0)), BASH, false, t0, &log);
        for i in 2..7 {
            last.observe(marks(990, Some(995), Some(0)), BASH, true, t0 + secs(i as f64), &log);
        }
        assert_eq!(last.observe(marks(1001, Some(1005), Some(0)), BASH, false, t0 + secs(7.0), &log), done(0, Some(6)));
    }

    #[test]
    fn a_step_just_before_a_bash_command_is_not_counted() {
        // Polls at i + 0.01 s; the clock steps 2 s back at 5.5 (recorded at the poll of 6.01);
        // a bash command runs from 5.9 to 8.0, stamped 1003 to 1006, seen at the poll of 8.01.
        let t0 = Instant::now();
        let mut log = ClockLog::default();
        for i in 0..=10 {
            let at = i as f64 + 0.01;
            log.record(t0 + secs(at), 1000.0 + at - if at > 5.5 { 2.0 } else { 0.0 });
        }
        let mut last = LastCommand::default();
        last.observe(marks(990, Some(995), Some(0)), BASH, false, t0 + secs(5.01), &log);
        let ended = last.observe(marks(1003, Some(1006), Some(0)), BASH, false, t0 + secs(8.01), &log);
        assert_eq!(ended, done(0, Some(3)), "2.1 s, within a second, not 5");
    }

    #[test]
    fn an_old_foreground_sighting_does_not_count_steps() {
        // A foreground process seen once at t0 + 10, then steps every 30 s for an hour, then
        // a 3 s bash command.
        let t0 = Instant::now();
        let mut log = ClockLog::default();
        let mut wall = 1000.0;
        for i in 0..=3600 {
            if i % 30 == 15 {
                wall -= 1.8;
            }
            log.record(t0 + secs(i as f64), wall + i as f64);
        }
        let mut last = LastCommand::default();
        last.observe(Marks::default(), BASH, false, t0, &log);
        last.observe(Marks::default(), BASH, true, t0 + secs(10.0), &log);
        for i in 11..3600 {
            last.observe(Marks::default(), BASH, false, t0 + secs(i as f64), &log);
        }
        let end = (wall + 3600.0) as u64;
        assert_eq!(last.observe(marks(end - 3, Some(end), Some(0)), BASH, false, t0 + secs(3600.0), &log), done(0, Some(3)));

        // Even a sighting never cleared (a foreground process at every poll, the last one
        // belonging to the command) cannot reach back further than the command can be long.
        let mut last = LastCommand::default();
        last.observe(Marks::default(), BASH, true, t0 + secs(10.0), &log);
        assert_eq!(last.observe(marks(end - 3, Some(end), Some(0)), BASH, false, t0 + secs(3600.0), &log), done(0, Some(3)));
    }

    #[test]
    fn a_short_command_across_a_step_keeps_its_exit_code() {
        // Started at 1002 just before the step, ended at 1001 just after, both seen at t0 + 3.
        let t0 = Instant::now();
        let log = clock(t0, 1000.0, 3, -2.0);
        let mut last = LastCommand::default();
        last.observe(marks(990, Some(995), Some(0)), ZSH, false, t0, &log);
        assert_eq!(last.observe(marks(1002, Some(1001), Some(1)), ZSH, false, t0 + secs(3.0), &log), done(1, None));
    }

    #[test]
    fn a_step_outside_the_command_is_not_counted() {
        let t0 = Instant::now();
        let log = clock(t0, 1000.0, 3, -2.0);
        let mut last = LastCommand::default();
        last.observe(marks(990, Some(995), Some(0)), BASH, false, t0, &log);
        // 10 s from t0 + 20, long after the step, a foreground process seen from t0 + 21 on.
        for i in 21..30 {
            last.observe(marks(990, Some(995), Some(0)), BASH, true, t0 + secs(i as f64), &log);
        }
        assert_eq!(last.observe(marks(1018, Some(1028), Some(0)), BASH, false, t0 + secs(30.0), &log), done(0, Some(10)));
    }

    #[test]
    fn start_and_end_in_one_poll_go_together() {
        // zsh: a command shorter than a poll, here after a paste of two lines (the first one
        // seen starting): the end belongs to the latest start.
        let t0 = Instant::now();
        let log = ClockLog::default();
        let mut last = LastCommand::default();
        last.observe(marks(100, Some(100), Some(0)), ZSH, false, t0, &log);
        last.observe(marks(110, Some(100), Some(0)), ZSH, true, t0 + secs(1.0), &log);
        let ended = last.observe(marks(130, Some(130), Some(1)), ZSH, false, t0 + secs(21.0), &log);
        assert_eq!(ended, done(1, Some(0)), "the hooks' duration, not the 20 s since the older start");
    }

    #[test]
    fn a_foreground_process_at_an_end_belongs_to_the_next_command() {
        let t0 = Instant::now();
        let log = clock(t0, 1000.0, 3, -2.0);
        let mut last = LastCommand::default();
        last.observe(marks(990, Some(995), Some(0)), BASH, false, t0, &log);
        // A command ends at t0 + 1 while the next one already runs in the foreground.
        last.observe(marks(1000, Some(1001), Some(0)), BASH, true, t0 + secs(1.0), &log);
        last.observe(marks(1000, Some(1001), Some(0)), BASH, true, t0 + secs(2.0), &log);
        // That one ran from t0 + 1 to t0 + 7, right across the step, and is timed in full...
        assert_eq!(last.observe(marks(1001, Some(1005), Some(0)), BASH, false, t0 + secs(7.0), &log), done(0, Some(6)));
        // ...but the one after, from t0 + 8 to t0 + 10, does not inherit the sighting.
        assert_eq!(last.observe(marks(1006, Some(1008), Some(0)), BASH, false, t0 + secs(10.0), &log), done(0, Some(2)));
    }

    #[test]
    fn agent_restart_mid_command() {
        let t0 = Instant::now();
        let mut log = ClockLog::default();
        log.record(t0, 1000.0);
        let mut last = LastCommand::default();
        // zsh, a 60 s command started at 970, 30 s after the previous one: running.
        assert_eq!(last.observe(marks(970, Some(940), Some(0)), ZSH, true, t0, &log), None);
        log.record(t0 + secs(30.0), 1030.0);
        let ended = last.observe(marks(970, Some(1030), Some(0)), ZSH, false, t0 + secs(30.0), &log);
        assert_eq!(ended, done(0, Some(60)), "from the marks: the start was not seen");
    }

    #[test]
    fn first_look_uses_the_plain_marks() {
        // An hour of steps back, then a first look at a command stamped 5000 to 5600.
        let t0 = Instant::now();
        let mut log = ClockLog::default();
        for i in 0..=3600 {
            log.record(t0 + secs(i as f64), 1000.0 + i as f64 - 1.8 * (i / 30) as f64);
        }
        let mut last = LastCommand::default();
        assert_eq!(last.observe(marks(5000, Some(5600), Some(0)), BASH, false, t0 + secs(3600.0), &log), done(0, Some(600)));

        let mut last = LastCommand::default();
        assert_eq!(last.observe(marks(105, Some(103), Some(1)), ZSH, false, t0, &log), done(1, None), "end 2 s before start: a step");
        let mut last = LastCommand::default();
        assert_eq!(last.observe(marks(105, Some(103), Some(1)), BASH, true, t0, &log), done(1, None), "bash marks always go together");
        let mut last = LastCommand::default();
        assert_eq!(last.observe(marks(160, Some(100), Some(1)), ZSH, false, t0, &log), None, "a minute later: a new command");
        let mut last = LastCommand::default();
        assert_eq!(last.observe(marks(103, Some(100), Some(1)), ZSH, true, t0, &log), None, "a command started 3 s after a failure, running");
        let mut last = LastCommand::default();
        assert_eq!(last.observe(Marks::default(), ZSH, false, t0, &log), None, "no command yet");
    }

    #[test]
    fn clock_log_records_steps_between_polls() {
        let t0 = Instant::now();
        let log = clock(t0, 1000.0, 10, -1.8);
        assert_eq!(log.steps.len(), 1);
        let step = log.steps[0];
        assert_eq!((step.after, step.seen), (t0 + secs(9.0), t0 + secs(10.0)));
        assert!((step.secs + 1.8).abs() < 1e-6);

        // Milliseconds of jitter between readings are not steps.
        let mut log = ClockLog::default();
        log.record(t0, 1000.0);
        log.record(t0 + secs(1.0), 1001.03);
        assert!(log.steps.is_empty());
    }

    #[test]
    fn only_steps_certain_to_be_inside_count() {
        let t0 = Instant::now();
        let log = clock(t0, 1000.0, 10, -2.0);
        // Ends seen at t0 + 20: a 10 s command (marks 8 s apart) may have started at t0 + 11.
        assert_eq!(log.inside(8.0, None, t0 + secs(20.0)), 0.0);
        // Marks 15 s apart: started by t0 + 6, the step (after t0 + 9) is inside.
        assert!((log.inside(15.0, None, t0 + secs(20.0)) + 2.0).abs() < 1e-6);
        // Recorded at the poll that saw the end: may have come after it.
        assert_eq!(log.inside(5.0, None, t0 + secs(10.0)), 0.0);
        // Seen running at t0 + 9: the step after it is inside.
        assert!((log.inside(8.0, Some(t0 + secs(9.0)), t0 + secs(20.0)) + 2.0).abs() < 1e-6);
    }
}
