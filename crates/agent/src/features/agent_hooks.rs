//! Coding agents (Claude Code, Codex, Gemini CLI, Copilot CLI, Qwen Code) tell tcanvas when
//! they wait for the user, through their own lifecycle hooks.
//!
//! - Sink: `tcanvas-agent hook <event>` runs from any agent's hook. It sets the tmux pane
//!   option `@tc_agent` to `<working|waiting> <pid,pid,...>` (the hook's ancestors, so the
//!   status loop can check that the mark belongs to the process in the foreground), or unsets
//!   it when the session ends. From the JSON payload on stdin it also keeps the agent's
//!   session id, transcript path and last prompt in pane options next to the mark (hex
//!   encoded, tagged with the same pids), set in the same `tmux` call. It only acts inside the tcanvas tmux server, prints `{}` (a
//!   valid "no decision" for every agent) and always exits 0: it can never block an agent.
//! - Adapters: `install` writes one hook entry per useful event into each detected agent's
//!   config, merged with what is already there; `uninstall` removes only those entries.
//! - Task integration, opt-in and separate from the status hooks (`Part::Tasks`): for the agents
//!   whose session start hook output reaches the model, an entry running
//!   `tcanvas-agent task prime --hook <form>` (the open tasks, see `task_cli`) in the output form
//!   that agent reads, and for the agents that read skills from a folder the `tcanvas-tasks`
//!   skill (`task_skill`). An entry the user changed (a matcher, a timeout) is kept and counts;
//!   an edited skill is never overwritten nor removed.
//! - Each agent is handled on its own: a settings file that does not read stops that agent only.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use protocol::TaskHooks;
use serde_json::{Map, Value, json};

use super::task_cli::HookForm;
use super::task_skill::{self, SkillChange, SkillState};

/// What an agent is doing, as published by its hooks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentState {
    Working,
    Waiting,
}

/// A parsed `@tc_agent` value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AgentMark {
    pub state: AgentState,
    /// Ancestors of the hook process; one of them is the agent itself.
    pub pids: Vec<u32>,
}

pub const PANE_OPTION: &str = "@tc_agent";

/// Hook event name (any agent's casing) -> new state. `Some(None)` clears the mark.
pub fn state_of(event: &str) -> Option<Option<AgentState>> {
    match event.to_ascii_lowercase().as_str() {
        "userpromptsubmit" | "userpromptsubmitted" | "beforeagent" | "posttooluse" | "aftertool" => Some(Some(AgentState::Working)),
        "stop" | "agentstop" | "afteragent" | "notification" | "permissionrequest" => Some(Some(AgentState::Waiting)),
        "sessionend" => Some(None),
        _ => None,
    }
}

pub fn format_mark(state: AgentState, pids: &[u32]) -> String {
    let word = match state {
        AgentState::Working => "working",
        AgentState::Waiting => "waiting",
    };
    let pids: Vec<String> = pids.iter().map(u32::to_string).collect();
    format!("{word} {}", pids.join(","))
}

pub fn parse_mark(value: &str) -> Option<AgentMark> {
    let (word, pids) = value.trim().split_once(' ').unwrap_or((value.trim(), ""));
    let state = match word {
        "working" => AgentState::Working,
        "waiting" => AgentState::Waiting,
        _ => return None,
    };
    Some(AgentMark { state, pids: pids.split(',').filter_map(|p| p.parse().ok()).collect() })
}

/// Parent pids of the current process, closest first (stops at init).
fn ancestors() -> Vec<u32> {
    let mut pids = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    while pid > 1 && pids.len() < MAX_PIDS {
        pids.push(pid);
        pid = super::status::parent_of(pid).unwrap_or(0);
    }
    pids
}

// ---------------------------------------------------------------------------------------
// Payload: what the agent tells about itself on stdin

/// Pane options next to `@tc_agent`, as `<owner pids> <hex>` (see `encode_owned`).
pub const SESSION_OPTION: &str = "@tc_agent_session";
pub const TRANSCRIPT_OPTION: &str = "@tc_agent_transcript";
pub const PROMPT_OPTION: &str = "@tc_agent_prompt";
/// Longest prompt kept, in characters (the card shows one elided line of it).
pub const PROMPT_MAX_CHARS: usize = 500;
/// Longer session ids and transcript paths are dropped: hex doubles them, and the whole
/// `tmux` call (mark included) must stay far below tmux's command size limit.
const SESSION_ID_MAX_BYTES: usize = 512;
const TRANSCRIPT_MAX_BYTES: usize = 1024;
/// At most this many owner pids are recorded (in the mark and with each value).
const MAX_PIDS: usize = 16;

/// The useful part of a hook payload. Every field is optional: agents differ, and a
/// malformed payload simply gives nothing.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Payload {
    pub session_id: Option<String>,
    pub transcript: Option<String>,
    /// Only on prompt-submit events; already one line and truncated.
    pub prompt: Option<String>,
}

/// Events that carry the prompt the user just submitted.
fn is_prompt_event(event: &str) -> bool {
    matches!(event.to_ascii_lowercase().as_str(), "userpromptsubmit" | "userpromptsubmitted" | "beforeagent")
}

/// Pane options marking the last prompt submitted and the last turn ended by a coding agent in the
/// pane: each event writes a new token (its time and the hook's pid), in the hook's single tmux
/// call (no read first: concurrent hooks never lose one). Never unset (not even at `SessionEnd`):
/// a reader tells a new prompt or a new turn end from a token that changed.
pub const PROMPTS_OPTION: &str = "@tc_agent_prompt_mark";
pub const TURNS_OPTION: &str = "@tc_agent_turn_mark";

/// The counter an event moves: a submitted prompt, or the end of a turn (the agent's reply is
/// over: `Stop`, `agentStop`, `AfterAgent`). Attention requests (`Notification`,
/// `PermissionRequest`) and tool events move none: the turn goes on.
pub fn counter_of(event: &str) -> Option<&'static str> {
    if is_prompt_event(event) {
        return Some(PROMPTS_OPTION);
    }
    matches!(event.to_ascii_lowercase().as_str(), "stop" | "agentstop" | "afteragent").then_some(TURNS_OPTION)
}

/// `; set -p -t <pane> <option> <token>`: the mark's new value, chained to the hook's call.
pub fn counter_args(pane: &str, option: &str, token: &str) -> Vec<String> {
    [";", "set", "-p", "-t", pane, option, token].map(String::from).to_vec()
}

/// A token no other hook writes: the time in nanoseconds and this process's id.
pub fn mark_token(nanos: u128, pid: u32) -> String {
    format!("{nanos}-{pid}")
}

/// First JSON value of `reader`, without waiting for the end of the stream (some callers
/// leave stdin open). `None` for empty or invalid input.
pub fn read_payload(reader: impl Read) -> Option<Value> {
    serde_json::Deserializer::from_reader(io::BufReader::new(reader)).into_iter::<Value>().next()?.ok()
}

/// Fields of a payload, under the names each agent uses (snake_case for Claude Code, Codex,
/// Gemini CLI and Qwen Code; camelCase for Copilot CLI).
pub fn payload_of(event: &str, value: Option<&Value>) -> Payload {
    let Some(obj) = value.and_then(Value::as_object) else { return Payload::default() };
    let field = |names: &[&str]| names.iter().find_map(|n| obj.get(*n).and_then(Value::as_str)).map(str::trim).filter(|s| !s.is_empty());
    let bounded = |max: usize| move |s: &str| Some(s.to_string()).filter(|s| s.len() <= max && !s.chars().any(is_hidden_char));
    Payload {
        session_id: field(&["session_id", "sessionId"]).and_then(bounded(SESSION_ID_MAX_BYTES)),
        transcript: field(&["transcript_path", "transcriptPath"]).and_then(bounded(TRANSCRIPT_MAX_BYTES)),
        prompt: if is_prompt_event(event) { field(&["prompt"]).and_then(one_line) } else { None },
    }
}

/// Control characters and Unicode bidi controls (marks, embeddings, overrides, isolates):
/// invisible, and able to reorder the text drawn around them.
pub fn is_hidden_char(c: char) -> bool {
    c.is_control() || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// The prompt on one line (any run of whitespace becomes a space, control and bidi
/// characters are dropped), at most `PROMPT_MAX_CHARS` characters with an ellipsis when cut.
pub fn one_line(text: &str) -> Option<String> {
    let visible: String = text.chars().filter(|&c| c.is_whitespace() || !is_hidden_char(c)).collect();
    let line = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    if line.chars().count() <= PROMPT_MAX_CHARS {
        return Some(line);
    }
    let mut cut: String = line.chars().take(PROMPT_MAX_CHARS - 1).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    Some(cut)
}

/// Hex of the UTF-8 bytes: nothing a prompt contains (tabs, newlines, `#{...}`) can then
/// break the `list-panes` rows or be expanded by tmux.
pub fn encode_value(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

/// Inverse of `encode_value`; `None` for an empty, malformed or non UTF-8 value.
pub fn decode_value(hex: &str) -> Option<String> {
    let hex = hex.trim();
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..hex.len()).step_by(2).map(|i| hex.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok())).collect();
    String::from_utf8(bytes?).ok()
}

/// A stored payload value and its owner: the ancestors of the hook that wrote it, as in
/// the mark. Another agent started later in the same pane is not among them, so it never
/// inherits a prompt or a transcript it did not send (with or without a session id).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Owned {
    pub pids: Vec<u32>,
    pub text: String,
}

impl Owned {
    pub fn owned_by(&self, pid: u32) -> bool {
        self.pids.contains(&pid)
    }
}

/// The text of a stored value if `agent_pid` (the foreground process, when it holds a
/// trusted mark) wrote it; `None` otherwise.
pub fn trusted(value: Option<&Owned>, agent_pid: Option<u32>) -> Option<String> {
    let pid = agent_pid?;
    value.filter(|v| v.owned_by(pid)).map(|v| v.text.clone())
}

/// `<pid,pid,...> <hex>`.
pub fn encode_owned(pids: &[u32], text: &str) -> String {
    let pids: Vec<String> = pids.iter().take(MAX_PIDS).map(u32::to_string).collect();
    format!("{} {}", pids.join(","), encode_value(text))
}

/// Inverse of `encode_owned`; `None` without an owner or with a malformed value.
pub fn decode_owned(value: &str) -> Option<Owned> {
    let (pids, hex) = value.trim().split_once(' ')?;
    let pids: Vec<u32> = pids.split(',').filter_map(|p| p.parse().ok()).collect();
    if pids.is_empty() {
        return None;
    }
    Some(Owned { pids, text: decode_value(hex)? })
}

/// Arguments of the single `tmux` call a hook makes (commands chained with `;`):
/// set the mark and whatever the payload brings, or unset all of it on `SessionEnd`.
pub fn tmux_args(pane: &str, change: Option<AgentState>, pids: &[u32], payload: &Payload) -> Vec<String> {
    let mut sets: Vec<(&str, Option<String>)> = Vec::new();
    match change {
        Some(state) => {
            sets.push((PANE_OPTION, Some(format_mark(state, pids))));
            // These stay set until `SessionEnd` or the pane closes (an agent killed without its
            // hooks cannot clear them), so readers trust them only under a trusted mark and
            // when their owner pids include the process in the foreground.
            let fields = [(SESSION_OPTION, &payload.session_id), (TRANSCRIPT_OPTION, &payload.transcript), (PROMPT_OPTION, &payload.prompt)];
            sets.extend(fields.into_iter().filter_map(|(option, v)| v.as_deref().map(|v| (option, Some(encode_owned(pids, v))))));
        }
        None => sets.extend([PANE_OPTION, SESSION_OPTION, TRANSCRIPT_OPTION, PROMPT_OPTION].map(|o| (o, None))),
    }
    let mut args = Vec::new();
    for (k, (option, value)) in sets.into_iter().enumerate() {
        if k > 0 {
            args.push(";".to_string());
        }
        match value {
            Some(v) => args.extend(["set", "-p", "-t", pane, option, &v].map(String::from)),
            None => args.extend(["set", "-p", "-u", "-t", pane, option].map(String::from)),
        }
    }
    args
}

fn run_tmux(args: &[String]) -> bool {
    // Plain `tmux` talks to the server named by $TMUX, the one this pane belongs to.
    Command::new("tmux").args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
}

/// `tcanvas-agent hook <event>`.
pub fn hook_cli(event: Option<&str>) -> io::Result<()> {
    // Agents pipe a JSON payload on stdin: read it so they never hit a closed pipe, but
    // never wait long for it (Codex-style callers may leave stdin open).
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock().take(1 << 20);
        let _ = tx.send(read_payload(&mut stdin));
        let _ = io::copy(&mut stdin, &mut io::sink());
    });

    let in_tcanvas = std::env::var("TMUX").is_ok_and(|t| t.contains("tcanvas"));
    if let (true, Some(event), Ok(pane)) = (in_tcanvas, event, std::env::var("TMUX_PANE"))
        && let Some(change) = state_of(event)
    {
        let value = rx.recv_timeout(Duration::from_millis(300)).ok().flatten();
        let payload = payload_of(event, value.as_ref());
        let pids = if change.is_some() { ancestors() } else { Vec::new() };
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let count = counter_of(event).map(|option| counter_args(&pane, option, &mark_token(nanos, std::process::id()))).unwrap_or_default();
        let with_count = |mut args: Vec<String>| {
            args.extend(count.iter().cloned());
            args
        };
        // The payload must never cost the mark: if the combined call fails, set the mark alone.
        if !run_tmux(&with_count(tmux_args(&pane, change, &pids, &payload))) && payload != Payload::default() {
            run_tmux(&with_count(tmux_args(&pane, change, &pids, &Payload::default())));
        }
    } else {
        let _ = rx.recv_timeout(Duration::from_millis(300));
    }
    let mut out = io::stdout().lock();
    let _ = out.write_all(b"{}\n");
    let _ = out.flush();
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Adapters

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    /// `{"hooks": {"Event": [{"matcher": "...", "hooks": [{"type": "command", "command": ...}]}]}}`
    /// (Claude Code, Codex, Gemini CLI, Qwen Code), merged into a shared file.
    Grouped { timeout: u64 },
    /// `{"version": 1, "hooks": {"event": [{"type": "command", "bash": ...}]}}` (Copilot CLI),
    /// in a file of its own.
    Flat,
}

#[derive(Clone, Copy, Debug)]
pub struct Adapter {
    pub name: &'static str,
    /// Config dir, relative to the home directory: its presence means the agent is installed.
    dir: &'static str,
    /// Hooks file, relative to the home directory.
    file: &'static str,
    format: Format,
    /// (event, matcher)
    events: &'static [(&'static str, Option<&'static str>)],
    /// The session start event whose output the model reads, and the form it reads it in: runs
    /// `task prime`. `None`: no such hook known for sure.
    session_start: Option<(&'static str, HookForm)>,
    /// The agent's skills folder, relative to the home directory (the `tcanvas-tasks` skill goes
    /// in a folder of that name in it). `None`: none known for sure.
    skills: Option<&'static str>,
}

/// Claude Code notifications that mean "the user must act"; others (auth, quota...) do not.
const CLAUDE_ATTENTION: &str = "permission_prompt|idle_prompt|elicitation_dialog|elicitation_url_dialog|agent_needs_input";

pub const ADAPTERS: &[Adapter] = &[
    Adapter {
        name: "Claude Code",
        dir: ".claude",
        file: ".claude/settings.json",
        format: Format::Grouped { timeout: 10 },
        events: &[("UserPromptSubmit", None), ("PostToolUse", None), ("PermissionRequest", None), ("Notification", Some(CLAUDE_ATTENTION)), ("Stop", None), ("SessionEnd", None)],
        // Plain stdout of a `SessionStart` hook is added to the context.
        session_start: Some(("SessionStart", HookForm::Plain)),
        skills: Some(".claude/skills"),
    },
    Adapter {
        name: "Codex",
        dir: ".codex",
        file: ".codex/hooks.json",
        format: Format::Grouped { timeout: 10 },
        events: &[("UserPromptSubmit", None), ("PostToolUse", None), ("PermissionRequest", None), ("Stop", None), ("SessionEnd", None)],
        // Plain stdout of a `SessionStart` hook is added as developer context; user skills are
        // read from `~/.agents/skills`.
        session_start: Some(("SessionStart", HookForm::Plain)),
        skills: Some(".agents/skills"),
    },
    Adapter {
        name: "Gemini CLI",
        dir: ".gemini",
        file: ".gemini/settings.json",
        // Milliseconds for Gemini.
        format: Format::Grouped { timeout: 10_000 },
        events: &[("BeforeAgent", None), ("AfterTool", None), ("Notification", None), ("AfterAgent", None), ("SessionEnd", None)],
        // Hook stdout must be JSON: `hookSpecificOutput.additionalContext` reaches the model.
        session_start: Some(("SessionStart", HookForm::Json)),
        skills: None,
    },
    Adapter {
        name: "Qwen Code",
        dir: ".qwen",
        file: ".qwen/settings.json",
        format: Format::Grouped { timeout: 10 },
        events: &[("UserPromptSubmit", None), ("PostToolUse", None), ("PermissionRequest", None), ("Notification", None), ("Stop", None), ("SessionEnd", None)],
        // Plain stdout of a `SessionStart` hook is added to the context.
        session_start: Some(("SessionStart", HookForm::Plain)),
        skills: None,
    },
    Adapter {
        name: "Copilot CLI",
        dir: ".copilot",
        file: ".copilot/hooks/tcanvas.json",
        format: Format::Flat,
        events: &[("userPromptSubmitted", None), ("postToolUse", None), ("permissionRequest", None), ("notification", None), ("agentStop", None), ("sessionEnd", None)],
        // Only a top-level `additionalContext` of the JSON output is used.
        session_start: Some(("sessionStart", HookForm::JsonFlat)),
        skills: None,
    },
];

/// Shell command an agent runs for `event`. The guard keeps agents quiet if tcanvas is
/// removed; `; true` keeps the exit code at 0 whatever happens.
pub fn hook_command(exe: &str, event: &str) -> String {
    let q = shell_quote(exe);
    format!("[ -x {q} ] && {q} hook {event}; true")
}

/// Shell command a session start hook runs: the open tasks, in the form the agent reads.
pub fn prime_command(exe: &str, form: HookForm) -> String {
    let q = shell_quote(exe);
    format!("[ -x {q} ] && {q} task prime --hook {}; true", form.arg())
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Which of our entries: the status hooks (`hook <event>`), or the task integration (the
/// session start entry running `task prime`, and the skill), installed separately.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Status,
    Tasks,
}

impl Part {
    /// Our entries are recognized by their command, whatever path the binary had then and
    /// whatever the user added around them.
    fn owns(self, command: &str) -> bool {
        command.contains("tcanvas-agent")
            && match self {
                Part::Status => command.contains(" hook "),
                Part::Tasks => command.contains(" task prime"),
            }
    }
}

fn entry_command(entry: &Value) -> Option<&str> {
    entry.get("command").or_else(|| entry.get("bash")).and_then(Value::as_str)
}

impl Adapter {
    pub fn detected(&self, home: &Path) -> bool {
        home.join(self.dir).is_dir()
    }

    fn our_entry(&self, command: String, matcher: Option<&str>) -> Value {
        match self.format {
            Format::Grouped { timeout } => {
                let mut group = Map::new();
                if let Some(m) = matcher {
                    group.insert("matcher".into(), json!(m));
                }
                group.insert("hooks".into(), json!([{ "type": "command", "command": command, "timeout": timeout }]));
                Value::Object(group)
            }
            Format::Flat => json!({ "type": "command", "bash": command, "timeoutSec": 10 }),
        }
    }

    /// The hook entries of `config` (inside their groups for the grouped format).
    fn entries<'a>(&self, config: &'a Value) -> Vec<&'a Value> {
        let lists = config.get("hooks").and_then(Value::as_object).into_iter().flat_map(|h| h.values()).filter_map(Value::as_array).flatten();
        match self.format {
            Format::Grouped { .. } => lists.filter_map(|g| g.get("hooks").and_then(Value::as_array)).flatten().collect(),
            Format::Flat => lists.collect(),
        }
    }

    /// Removes the entries of `part` (or of both) from `config`; returns whether anything changed.
    fn strip(&self, config: &mut Value, part: Option<Part>) -> bool {
        let ours = |h: &Value| entry_command(h).is_some_and(|c| part.map_or(Part::Status.owns(c) || Part::Tasks.owns(c), |p| p.owns(c)));
        let Some(hooks) = config.get_mut("hooks").and_then(Value::as_object_mut) else { return false };
        let mut changed = false;
        for list in hooks.values_mut().filter_map(Value::as_array_mut) {
            let before = list.len();
            match self.format {
                Format::Grouped { .. } => {
                    for group in list.iter_mut() {
                        if let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                            let n = inner.len();
                            inner.retain(|h| !ours(h));
                            changed |= inner.len() != n;
                        }
                    }
                    list.retain(|g| !g.get("hooks").and_then(Value::as_array).is_some_and(Vec::is_empty));
                }
                Format::Flat => list.retain(|h| !ours(h)),
            }
            changed |= list.len() != before;
        }
        hooks.retain(|_, list| !list.as_array().is_some_and(Vec::is_empty));
        if hooks.is_empty() {
            config.as_object_mut().expect("has hooks, so an object").remove("hooks");
        }
        changed
    }

    /// `config` with exactly our status entries for `exe` (the task entries left as they are).
    fn merged(&self, mut config: Value, exe: &str) -> Value {
        self.strip(&mut config, Some(Part::Status));
        if self.format == Format::Flat {
            config.as_object_mut().expect("object").entry("version").or_insert(json!(1));
        }
        let hooks = config.as_object_mut().expect("object").entry("hooks").or_insert_with(|| json!({}));
        for (event, matcher) in self.events {
            let list = hooks.as_object_mut().expect("hooks is an object").entry(*event).or_insert_with(|| json!([]));
            if let Some(list) = list.as_array_mut() {
                list.push(self.our_entry(hook_command(exe, event), *matcher));
            }
        }
        config
    }

    /// `config` with a session start entry: kept as it is when there is one in any form (a
    /// matcher or fields the user added stay), added otherwise.
    fn with_tasks(&self, mut config: Value, exe: &str) -> Value {
        let Some((event, form)) = self.session_start else { return config };
        if self.has_task_entry(&config) {
            return config;
        }
        if self.format == Format::Flat {
            config.as_object_mut().expect("object").entry("version").or_insert(json!(1));
        }
        let hooks = config.as_object_mut().expect("object").entry("hooks").or_insert_with(|| json!({}));
        if let Some(list) = hooks.as_object_mut().and_then(|h| h.entry(event).or_insert_with(|| json!([])).as_array_mut()) {
            list.push(self.our_entry(prime_command(exe, form), None));
        }
        config
    }

    /// None of our entries: the user's own settings, worth a backup before our first write.
    fn pristine(&self, config: &Value) -> bool {
        !self.entries(config).into_iter().any(|e| entry_command(e).is_some_and(|c| Part::Status.owns(c) || Part::Tasks.owns(c)))
    }

    fn has_task_entry(&self, config: &Value) -> bool {
        self.entries(config).into_iter().any(|e| entry_command(e).is_some_and(|c| Part::Tasks.owns(c)))
    }

    /// The `tcanvas-tasks` skill's folder, if this agent reads skills.
    fn skill_dir(&self, home: &Path) -> Option<PathBuf> {
        self.skills.map(|s| home.join(s).join(task_skill::SKILL_NAME))
    }

    /// Our status hooks are all there for `exe`.
    pub fn installed_in(&self, home: &Path, exe: &str) -> bool {
        match read_config(&home.join(self.file)) {
            Ok(Some(config)) => self.merged(config.clone(), exe) == config,
            _ => false,
        }
    }

    /// The task integration is there: a session start entry in any form, and the skill
    /// (edited or not) for the agents that read skills.
    pub fn tasks_installed_in(&self, home: &Path) -> bool {
        let entry = self.session_start.is_none() || matches!(read_config(&home.join(self.file)), Ok(Some(c)) if self.has_task_entry(&c));
        entry && self.skill_dir(home).is_none_or(|d| task_skill::state_in(&d, "") != SkillState::Missing)
    }

    /// Writes our status entries.
    pub fn install_in(&self, home: &Path, exe: &str) -> io::Result<Change> {
        let path = home.join(self.file);
        let config = read_config(&path)?.unwrap_or_else(|| json!({}));
        let merged = self.merged(config.clone(), exe);
        let settings = merged != config;
        if settings {
            write_config(&path, &merged, self.pristine(&config))?;
        }
        Ok(Change { agent: self.name, settings, skill: None })
    }

    /// Writes the task integration: the session start entry (if none is there) and the skill.
    pub fn install_tasks_in(&self, home: &Path, exe: &str) -> io::Result<Change> {
        let path = home.join(self.file);
        let config = read_config(&path)?.unwrap_or_else(|| json!({}));
        let merged = self.with_tasks(config.clone(), exe);
        let settings = merged != config;
        if settings {
            write_config(&path, &merged, self.pristine(&config))?;
        }
        let skill = self.skill_dir(home).map(|d| task_skill::install_in(&d, exe)).transpose()?;
        Ok(Change { agent: self.name, settings, skill })
    }

    /// Removes our entries of `part` (or all of ours), and with the task integration the skill
    /// unless it was edited. The settings are read first: a file that does not read stops here,
    /// with nothing removed. A settings file that only we wrote (no backup of an original) and
    /// that is left empty is removed, and so are the skills folders left empty.
    pub fn uninstall_in(&self, home: &Path, part: Option<Part>) -> io::Result<Change> {
        let path = home.join(self.file);
        let mut settings = false;
        if let Some(mut config) = read_config(&path)?
            && self.strip(&mut config, part)
        {
            let ours_alone = config == json!({}) && !backup_path(&fs::canonicalize(&path).unwrap_or_else(|_| path.clone())).exists();
            let empty_flat = self.format == Format::Flat && config.get("hooks").is_none();
            if ours_alone || empty_flat {
                fs::remove_file(&path)?;
            } else {
                write_config(&path, &config, false)?;
            }
            settings = true;
        }
        let skill = match (part, self.skill_dir(home)) {
            (Some(Part::Status), _) | (_, None) => None,
            (_, Some(dir)) => {
                let change = task_skill::uninstall_in(&dir)?;
                // The skills folders we may have created, if nothing else is in them.
                let mut p = dir.parent().map(Path::to_path_buf);
                while let Some(d) = p.filter(|d| d.starts_with(home) && *d != home && *d != home.join(self.dir)) {
                    if fs::remove_dir(&d).is_err() {
                        break;
                    }
                    p = d.parent().map(Path::to_path_buf);
                }
                Some(change)
            }
        };
        Ok(Change { agent: self.name, settings, skill })
    }
}

/// What an install or an uninstall did for one agent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Change {
    pub agent: &'static str,
    /// Its hooks settings were written.
    pub settings: bool,
    /// What happened to its skill (`None`: not concerned, or no skills folder).
    pub skill: Option<SkillChange>,
}

impl Change {
    pub fn any(&self) -> bool {
        self.settings || self.skill == Some(SkillChange::Changed)
    }
}

/// One agent's outcome: each agent is handled on its own, a failure stops only that one.
pub type Outcome = (&'static str, io::Result<Change>);

/// `None` if the file does not exist; an error if it is not a JSON object (never overwritten).
fn read_config(path: &Path) -> io::Result<Option<Value>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if text.trim().is_empty() {
        return Ok(Some(json!({})));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.is_object() => Ok(Some(v)),
        Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidData, format!("{}: not a JSON object", path.display()))),
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))),
    }
}

/// Atomic write through the symlink (dotfile managers), keeping the file's permissions (it may
/// hold secrets), with a one-time backup of the original when installing (`backup`).
fn write_config(path: &Path, config: &Value, backup: bool) -> io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir)?;
    }
    let mode = fs::metadata(&target).ok().map(|m| m.permissions());
    let saved = backup_path(&target);
    if backup && target.exists() && !saved.exists() {
        fs::copy(&target, &saved)?;
    }
    let tmp = target.with_extension("tcanvas-tmp");
    let mut text = serde_json::to_string_pretty(config).map_err(io::Error::other)?;
    text.push('\n');
    let written = (|| {
        fs::write(&tmp, text)?;
        if let Some(mode) = mode {
            fs::set_permissions(&tmp, mode)?;
        }
        fs::rename(&tmp, &target)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tcanvas.bak");
    path.with_file_name(name)
}

/// Absolute path of this binary, as the agents' hooks will call it.
pub fn current_exe() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "tcanvas-agent".into())
}

pub fn detected(home: &Path) -> impl Iterator<Item = &'static Adapter> + '_ {
    ADAPTERS.iter().filter(move |a| a.detected(home))
}

/// Every detected agent has our status hooks for `exe`. `None` if no agent is detected.
pub fn all_installed(home: &Path, exe: &str) -> Option<bool> {
    let mut any = false;
    for a in detected(home) {
        any = true;
        if !a.installed_in(home, exe) {
            return Some(false);
        }
    }
    any.then_some(true)
}

/// The task integration's state: installed when every detected agent has it.
pub fn tasks_state(home: &Path) -> TaskHooks {
    let mut found = detected(home).peekable();
    if found.peek().is_none() {
        return TaskHooks::Unsupported;
    }
    if found.all(|a| a.tasks_installed_in(home)) { TaskHooks::Installed } else { TaskHooks::Missing }
}

/// Installs every detected agent's status hooks.
pub fn install_all(home: &Path, exe: &str) -> Vec<Outcome> {
    detected(home).map(|a| (a.name, a.install_in(home, exe))).collect()
}

/// Installs every detected agent's task integration.
pub fn install_tasks_all(home: &Path, exe: &str) -> Vec<Outcome> {
    detected(home).map(|a| (a.name, a.install_tasks_in(home, exe))).collect()
}

/// Removes ours (`part`, or everything) from every agent, detected or not.
pub fn uninstall_all(home: &Path, part: Option<Part>) -> Vec<Outcome> {
    ADAPTERS.iter().map(|a| (a.name, a.uninstall_in(home, part))).collect()
}

/// Names of the agents where something changed.
pub fn changed(outcomes: &[Outcome]) -> Vec<&'static str> {
    outcomes.iter().filter(|(_, r)| r.as_ref().is_ok_and(Change::any)).map(|(name, _)| *name).collect()
}

/// The failures, one per agent (`Claude Code: <error>; Codex: <error>`); `None` when none.
pub fn failures(outcomes: &[Outcome]) -> Option<String> {
    let failed: Vec<String> = outcomes.iter().filter_map(|(name, r)| r.as_ref().err().map(|e| format!("{name}: {e}"))).collect();
    (!failed.is_empty()).then(|| failed.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/home/me/.local/bin/tcanvas-agent";

    fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tcanvas-agent-hooks-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn events_of_every_agent_map_to_a_state() {
        for e in ["UserPromptSubmit", "userPromptSubmitted", "BeforeAgent", "PostToolUse", "AfterTool", "postToolUse"] {
            assert_eq!(state_of(e), Some(Some(AgentState::Working)), "{e}");
        }
        for e in ["Stop", "agentStop", "AfterAgent", "Notification", "notification", "PermissionRequest", "permissionRequest"] {
            assert_eq!(state_of(e), Some(Some(AgentState::Waiting)), "{e}");
        }
        assert_eq!(state_of("SessionEnd"), Some(None));
        assert_eq!(state_of("sessionEnd"), Some(None));
        assert_eq!(state_of("PreCompact"), None);
        // Every installed event is understood by the sink.
        for a in ADAPTERS {
            for (e, _) in a.events {
                assert!(state_of(e).is_some(), "{} {e}", a.name);
            }
        }
    }

    #[test]
    fn marks_round_trip() {
        let v = format_mark(AgentState::Waiting, &[42, 7]);
        assert_eq!(v, "waiting 42,7");
        assert_eq!(parse_mark(&v), Some(AgentMark { state: AgentState::Waiting, pids: vec![42, 7] }));
        assert_eq!(parse_mark("working"), Some(AgentMark { state: AgentState::Working, pids: vec![] }));
        assert_eq!(parse_mark(""), None);
        assert_eq!(parse_mark("bogus 1"), None);
    }

    fn payload(event: &str, stdin: &str) -> Payload {
        payload_of(event, read_payload(stdin.as_bytes()).as_ref())
    }

    #[test]
    fn payload_fields_of_each_agent() {
        // Claude Code, Codex and Qwen Code: snake_case, `prompt` on UserPromptSubmit.
        let claude = r#"{"session_id":"abc-123","transcript_path":"/home/me/.claude/projects/-home-me-api/abc-123.jsonl","cwd":"/home/me/api","hook_event_name":"UserPromptSubmit","prompt":"add the total with taxes to the PDF"}"#;
        assert_eq!(
            payload("UserPromptSubmit", claude),
            Payload { session_id: Some("abc-123".into()), transcript: Some("/home/me/.claude/projects/-home-me-api/abc-123.jsonl".into()), prompt: Some("add the total with taxes to the PDF".into()) }
        );
        // Gemini CLI: same names, the prompt comes with BeforeAgent.
        let gemini = r#"{"session_id":"g1","transcript_path":"/tmp/g1.json","cwd":"/","hook_event_name":"BeforeAgent","timestamp":"2026-01-01T00:00:00Z","prompt":"fix the build"}"#;
        assert_eq!(payload("BeforeAgent", gemini).prompt.as_deref(), Some("fix the build"));
        // Copilot CLI: camelCase, maybe no session id at all.
        let copilot = r#"{"timestamp":1704614600000,"cwd":"/home/me","prompt":"explain this repo"}"#;
        assert_eq!(payload("userPromptSubmitted", copilot), Payload { session_id: None, transcript: None, prompt: Some("explain this repo".into()) });
        assert_eq!(payload("postToolUse", r#"{"sessionId":"c9","transcriptPath":"/t.jsonl"}"#), Payload { session_id: Some("c9".into()), transcript: Some("/t.jsonl".into()), prompt: None });
    }

    #[test]
    fn prompt_is_only_taken_from_prompt_events() {
        let p = payload("PostToolUse", r#"{"session_id":"s","prompt":"not a user prompt","tool_input":{"prompt":"nested"}}"#);
        assert_eq!(p, Payload { session_id: Some("s".into()), transcript: None, prompt: None });
    }

    #[test]
    fn missing_or_odd_fields_give_nothing() {
        assert_eq!(payload("Stop", r#"{"hook_event_name":"Stop"}"#), Payload::default());
        assert_eq!(payload("UserPromptSubmit", r#"{"session_id":null,"transcript_path":42,"prompt":"   \n\t "}"#), Payload::default());
        assert_eq!(payload("UserPromptSubmit", r#"["not", "an", "object"]"#), Payload::default());
        let long_id = format!(r#"{{"session_id":"{}"}}"#, "x".repeat(SESSION_ID_MAX_BYTES + 1));
        assert_eq!(payload("Stop", &long_id), Payload::default());
        let long_path = format!(r#"{{"transcript_path":"/{}"}}"#, "x".repeat(TRANSCRIPT_MAX_BYTES));
        assert_eq!(payload("Stop", &long_path), Payload::default());
        let at_bounds = format!(r#"{{"session_id":"{}","transcript_path":"{}"}}"#, "x".repeat(SESSION_ID_MAX_BYTES), "y".repeat(TRANSCRIPT_MAX_BYTES));
        assert!(payload("Stop", &at_bounds).session_id.is_some() && payload("Stop", &at_bounds).transcript.is_some());
        assert_eq!(payload("Stop", "{\"session_id\":\"a\\u202eb\"}"), Payload::default(), "bidi controls in an id");
    }

    #[test]
    fn the_worst_case_tmux_call_stays_far_below_tmux_limits() {
        // tmux packs the whole command line in one message of at most 16 KiB.
        let worst = Payload { session_id: Some("x".repeat(SESSION_ID_MAX_BYTES)), transcript: Some("y".repeat(TRANSCRIPT_MAX_BYTES)), prompt: one_line(&"\u{1f600}".repeat(PROMPT_MAX_CHARS * 2)) };
        assert_eq!(payload_of("UserPromptSubmit", Some(&json!({"session_id": worst.session_id, "transcript_path": worst.transcript}))).session_id, worst.session_id, "the worst case is accepted");
        let pids = vec![u32::MAX; MAX_PIDS];
        let args = tmux_args("%4294967295", Some(AgentState::Working), &pids, &worst);
        let size: usize = args.iter().map(|a| a.len() + 1).sum();
        assert!(size <= 10 * 1024, "{size} bytes");
    }

    #[test]
    fn invalid_or_empty_stdin_gives_nothing() {
        assert_eq!(read_payload(&b""[..]), None);
        assert_eq!(read_payload(&b"x\n"[..]), None);
        assert_eq!(read_payload(&b"{ \"session_id\": "[..]), None);
        assert_eq!(payload("UserPromptSubmit", "{ oops"), Payload::default());
        assert_eq!(payload_of("UserPromptSubmit", None), Payload::default());
        // A complete value is enough: the rest of the stream is not waited for.
        assert_eq!(read_payload(&b"{\"prompt\":\"hi\"}\n{ garbage"[..]), Some(json!({"prompt": "hi"})));
    }

    #[test]
    fn prompts_become_one_bounded_line() {
        let p = payload("UserPromptSubmit", r#"{"prompt":"first line\n\n  second\tline\r\nthird  "}"#);
        assert_eq!(p.prompt.as_deref(), Some("first line second line third"));

        let long = "word ".repeat(300);
        let cut = one_line(&long).unwrap();
        assert_eq!(cut.chars().count(), PROMPT_MAX_CHARS);
        assert!(cut.ends_with("word…"));
        let at_space = format!("{} {}", "a".repeat(PROMPT_MAX_CHARS - 2), "b".repeat(10));
        assert_eq!(one_line(&at_space), Some(format!("{}…", "a".repeat(PROMPT_MAX_CHARS - 2))), "no space before the ellipsis");

        // Non-ASCII text is cut on a character boundary.
        let accents = "\u{e9}".repeat(PROMPT_MAX_CHARS + 50);
        let cut = one_line(&accents).unwrap();
        assert_eq!(cut.chars().count(), PROMPT_MAX_CHARS);
        assert!(cut.starts_with('\u{e9}') && cut.ends_with('…'));
        let emoji = "\u{1f600} r\u{e9}sum\u{e9} \u{65e5}\u{672c}";
        assert_eq!(one_line(emoji).as_deref(), Some(emoji));
        let exact = "a".repeat(PROMPT_MAX_CHARS);
        assert_eq!(one_line(&exact), Some(exact));

        // Control and bidi characters are dropped; whitespace controls still separate words.
        let hidden = "fix\u{7}\u{1b}[31m the\u{202e}gnp.exe\u{2066} bug\u{200f}\u{0}\nnow\u{85}ok";
        assert_eq!(one_line(hidden).as_deref(), Some("fix[31m thegnp.exe bug now ok"));
        assert_eq!(one_line("\u{202a}\u{2069}\u{7}"), None);
    }

    #[test]
    fn stored_values_round_trip_through_hex() {
        for text in ["", "plain", "tab\tnew\nline", "#{session_name} #(rm -rf /) ;", "\u{e9}t\u{e9} \u{1f600}"] {
            let hex = encode_value(text);
            assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()), "{hex}");
            assert_eq!(decode_value(&hex), Some(text.to_string()).filter(|t| !t.is_empty()));
        }
        assert_eq!(encode_value("Hi"), "4869");
        assert_eq!(decode_value("486"), None, "odd length");
        assert_eq!(decode_value("zz"), None, "not hex");
        assert_eq!(decode_value("ff"), None, "not UTF-8");
        assert_eq!(decode_value("\u{e9}9"), None, "not ASCII");
    }

    #[test]
    fn stored_values_carry_their_owner() {
        let v = encode_owned(&[42, 7], "Hi");
        assert_eq!(v, "42,7 4869");
        let owned = decode_owned(&v).unwrap();
        assert_eq!(owned, Owned { pids: vec![42, 7], text: "Hi".into() });
        assert!(owned.owned_by(7) && !owned.owned_by(8), "another agent in the pane does not own it");
        assert_eq!(decode_owned("4869"), None, "no owner: never trusted");
        assert_eq!(decode_owned(" 4869"), None);
        assert_eq!(decode_owned("x,y 4869"), None);
        assert_eq!(decode_owned("42 zz"), None);
        assert_eq!(decode_owned(""), None);
    }

    #[test]
    fn only_the_agent_that_wrote_a_value_gets_it() {
        // Agent A (pids 42, 7) submitted a prompt, then quit without SessionEnd.
        let prompt = decode_owned(&encode_owned(&[42, 7], "old prompt"));
        assert_eq!(trusted(prompt.as_ref(), Some(42)).as_deref(), Some("old prompt"));
        // Agent B (pid 99) now holds the mark: A's prompt is not reported as B's.
        assert_eq!(trusted(prompt.as_ref(), Some(99)), None);
        // No trusted mark at all: nothing.
        assert_eq!(trusted(prompt.as_ref(), None), None);
        assert_eq!(trusted(None, Some(42)), None);
    }

    #[test]
    fn one_tmux_call_sets_the_mark_and_the_payload() {
        let p = Payload { session_id: Some("s1".into()), transcript: None, prompt: Some("Hi".into()) };
        let args = tmux_args("%3", Some(AgentState::Working), &[9, 8], &p);
        assert_eq!(args.join(" "), "set -p -t %3 @tc_agent working 9,8 ; set -p -t %3 @tc_agent_session 9,8 7331 ; set -p -t %3 @tc_agent_prompt 9,8 4869");
        // Nothing new in the payload: only the mark, as before.
        assert_eq!(tmux_args("%3", Some(AgentState::Waiting), &[9], &Payload::default()).join(" "), "set -p -t %3 @tc_agent waiting 9");
        // SessionEnd clears everything.
        assert_eq!(tmux_args("%3", None, &[], &p).join(" "), "set -p -u -t %3 @tc_agent ; set -p -u -t %3 @tc_agent_session ; set -p -u -t %3 @tc_agent_transcript ; set -p -u -t %3 @tc_agent_prompt");
    }

    #[test]
    fn hook_command_is_guarded_and_quoted() {
        assert_eq!(hook_command(EXE, "Stop"), "[ -x '/home/me/.local/bin/tcanvas-agent' ] && '/home/me/.local/bin/tcanvas-agent' hook Stop; true");
        assert_eq!(shell_quote("/a b/it's"), r"'/a b/it'\''s'");
        assert!(Part::Status.owns(&hook_command(EXE, "Stop")) && !Part::Tasks.owns(&hook_command(EXE, "Stop")));
        assert!(Part::Tasks.owns(&prime_command(EXE, HookForm::Plain)) && !Part::Status.owns(&prime_command(EXE, HookForm::Plain)));
        assert!(!Part::Status.owns("notify-send done"));
        assert!(!Part::Tasks.owns("my-script task prime"), "not ours without our binary");
    }

    fn ok(outcomes: Vec<Outcome>) -> Vec<Change> {
        outcomes.into_iter().map(|(_, r)| r.unwrap()).collect()
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn claude_install_keeps_the_user_settings_and_is_idempotent() {
        let home = temp_home("claude");
        fs::create_dir_all(home.join(".claude")).unwrap();
        let user = r#"{"model": "opus", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "notify-send done"}]}]}, "theme": "dark"}"#;
        fs::write(home.join(".claude/settings.json"), user).unwrap();
        assert_eq!(all_installed(&home, EXE), Some(false));

        assert_eq!(changed(&install_all(&home, EXE)), vec!["Claude Code"]);
        assert_eq!(all_installed(&home, EXE), Some(true));
        let v = read_json(&home.join(".claude/settings.json"));
        assert_eq!(v["model"], "opus");
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "hooks", "theme"], "key order kept");
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "notify-send done");
        assert_eq!(stop[1]["hooks"][0]["command"], hook_command(EXE, "Stop"));
        assert_eq!(v["hooks"]["Notification"][0]["matcher"], CLAUDE_ATTENTION);
        assert!(v["hooks"].get("SessionStart").is_none(), "the task integration is separate");
        assert!(!home.join(".claude/skills").exists());
        assert_eq!(fs::read_to_string(home.join(".claude/settings.json.tcanvas.bak")).unwrap(), user, "backup of the original");

        assert!(changed(&install_all(&home, EXE)).is_empty(), "second install: no change");
        // A new binary path replaces our entries instead of adding more.
        assert_eq!(all_installed(&home, "/opt/tcanvas/tcanvas-agent"), Some(false));
        ok(install_all(&home, "/opt/tcanvas/tcanvas-agent"));
        assert_eq!(read_json(&home.join(".claude/settings.json"))["hooks"]["Stop"].as_array().unwrap().len(), 2);

        assert_eq!(changed(&uninstall_all(&home, None)), vec!["Claude Code"]);
        assert_eq!(read_json(&home.join(".claude/settings.json")), serde_json::from_str::<Value>(user).unwrap(), "uninstall gives the user's settings back");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn one_bad_settings_file_stops_only_its_agent() {
        let home = temp_home("invalid");
        for dir in [".claude", ".codex", ".copilot"] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        fs::write(home.join(".claude/settings.json"), "{ oops").unwrap();
        let done = install_all(&home, EXE);
        assert!(done[0].1.is_err() && done[1].1.is_ok() && done[2].1.is_ok(), "{done:?}");
        assert_eq!(changed(&done), vec!["Codex", "Copilot CLI"]);
        assert!(failures(&done).unwrap().starts_with("Claude Code: "), "{:?}", failures(&done));
        assert_eq!(fs::read_to_string(home.join(".claude/settings.json")).unwrap(), "{ oops", "never overwritten");
        let done = install_tasks_all(&home, EXE);
        assert!(done[0].1.is_err());
        assert!(!home.join(".claude/skills").exists(), "nothing done for an agent whose settings do not read");
        assert_eq!(changed(&done), vec!["Codex", "Copilot CLI"]);

        let done = uninstall_all(&home, None);
        assert_eq!(changed(&done), vec!["Codex", "Copilot CLI"]);
        assert!(failures(&done).unwrap().starts_with("Claude Code: "));
        assert!(!home.join(".codex/hooks.json").exists() && !home.join(".agents").exists(), "the others fully uninstalled");
        // The skill is never removed while the settings do not read: no half uninstall.
        fs::write(home.join(".claude/settings.json"), "{}").unwrap();
        ok(install_tasks_all(&home, EXE));
        fs::write(home.join(".claude/settings.json"), "{ oops").unwrap();
        assert!(uninstall_all(&home, None)[0].1.is_err());
        assert!(home.join(".claude/skills/tcanvas-tasks/SKILL.md").exists());
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn only_detected_agents_get_hooks() {
        let home = temp_home("detect");
        assert_eq!(all_installed(&home, EXE), None, "no agent at all");
        assert_eq!(tasks_state(&home), TaskHooks::Unsupported);
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(home.join(".copilot")).unwrap();
        assert_eq!(changed(&install_all(&home, EXE)), vec!["Codex", "Copilot CLI"]);
        assert!(!home.join(".claude/settings.json").exists());
        assert!(!home.join(".gemini").exists());

        let codex = read_json(&home.join(".codex/hooks.json"));
        assert_eq!(codex["hooks"]["PermissionRequest"][0]["hooks"][0]["command"], hook_command(EXE, "PermissionRequest"));
        let copilot = read_json(&home.join(".copilot/hooks/tcanvas.json"));
        assert_eq!(copilot["version"], 1);
        assert_eq!(copilot["hooks"]["agentStop"][0]["bash"], hook_command(EXE, "agentStop"));
        assert_eq!(all_installed(&home, EXE), Some(true));

        ok(uninstall_all(&home, None));
        assert!(!home.join(".copilot/hooks/tcanvas.json").exists(), "our own file is removed");
        assert!(!home.join(".codex/hooks.json").exists(), "a file only we wrote is removed");
        assert!(!home.join(".codex/hooks.json.tcanvas.bak").exists());
        let _ = fs::remove_dir_all(home);
    }

    /// The session start entries (their commands) in an agent's config.
    fn prime_commands(a: &Adapter, config: &Value) -> Vec<String> {
        a.entries(config).into_iter().filter_map(entry_command).filter(|c| c.contains("task prime")).map(String::from).collect()
    }

    #[test]
    fn the_session_start_entry_runs_prime_in_each_agents_form() {
        assert_eq!(prime_command(EXE, HookForm::Plain), "[ -x '/home/me/.local/bin/tcanvas-agent' ] && '/home/me/.local/bin/tcanvas-agent' task prime --hook plain; true");
        let forms: Vec<_> = ADAPTERS.iter().map(|a| (a.name, a.session_start, a.skills)).collect();
        assert_eq!(
            forms,
            [
                ("Claude Code", Some(("SessionStart", HookForm::Plain)), Some(".claude/skills")),
                ("Codex", Some(("SessionStart", HookForm::Plain)), Some(".agents/skills")),
                ("Gemini CLI", Some(("SessionStart", HookForm::Json)), None),
                ("Qwen Code", Some(("SessionStart", HookForm::Plain)), None),
                ("Copilot CLI", Some(("sessionStart", HookForm::JsonFlat)), None),
            ]
        );
    }

    /// Every agent: a settings file with the user's own hooks (a session start one included),
    /// status hooks, then the task integration, then removing each: the user's settings come
    /// back, the skill goes.
    #[test]
    fn install_and_uninstall_round_trip_every_agent() {
        for a in ADAPTERS {
            let home = temp_home(&format!("round-{}", a.dir.trim_start_matches('.')));
            fs::create_dir_all(home.join(a.dir)).unwrap();
            let path = home.join(a.file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let (event, form) = a.session_start.unwrap();
            let user = match a.format {
                Format::Grouped { .. } => {
                    json!({"theme": "dark", "hooks": {event: [{"hooks": [{"type": "command", "command": "echo hello"}]}], "PreCompact": [{"hooks": [{"type": "command", "command": "true"}]}]}, "zeta": 1})
                }
                Format::Flat => json!({"version": 1, "hooks": {event: [{"type": "command", "bash": "echo hello"}]}}),
            };
            fs::write(&path, serde_json::to_string_pretty(&user).unwrap()).unwrap();
            assert_eq!((all_installed(&home, EXE), tasks_state(&home)), (Some(false), TaskHooks::Missing), "{}", a.name);

            assert_eq!(changed(&install_all(&home, EXE)), vec![a.name]);
            assert!(prime_commands(a, &read_json(&path)).is_empty(), "{}: status hooks only", a.name);
            assert_eq!((all_installed(&home, EXE), tasks_state(&home)), (Some(true), TaskHooks::Missing), "{}", a.name);

            assert_eq!(changed(&install_tasks_all(&home, EXE)), vec![a.name]);
            let v = read_json(&path);
            assert_eq!(prime_commands(a, &v), vec![prime_command(EXE, form)], "{}", a.name);
            assert_eq!(v["hooks"][event].as_array().unwrap().len(), 2, "{}: the user's entry stays", a.name);
            assert_eq!((all_installed(&home, EXE), tasks_state(&home)), (Some(true), TaskHooks::Installed), "{}", a.name);
            if let Some(dir) = a.skill_dir(&home) {
                assert_eq!(fs::read_to_string(dir.join(task_skill::SKILL_FILE)).unwrap(), task_skill::render(EXE));
            }
            assert!(changed(&install_tasks_all(&home, EXE)).is_empty(), "{}: idempotent", a.name);
            assert!(changed(&install_all(&home, EXE)).is_empty(), "{}: the status install keeps the task entry", a.name);
            assert_eq!(prime_commands(a, &read_json(&path)).len(), 1);

            // Removing the task integration only.
            assert_eq!(changed(&uninstall_all(&home, Some(Part::Tasks))), vec![a.name]);
            assert!(prime_commands(a, &read_json(&path)).is_empty());
            assert_eq!((all_installed(&home, EXE), tasks_state(&home)), (Some(true), TaskHooks::Missing), "{}", a.name);
            if let Some(dir) = a.skill_dir(&home) {
                assert!(!dir.exists() && !dir.parent().unwrap().exists(), "{}: the skill and the skills folder we made go", a.name);
            }

            ok(install_tasks_all(&home, EXE));
            assert_eq!(changed(&uninstall_all(&home, None)), vec![a.name]);
            assert_eq!(read_json(&path), user, "{}: the user's settings back", a.name);
            let _ = fs::remove_dir_all(home);
        }
    }

    #[test]
    fn an_older_install_still_reads_as_installed() {
        let home = temp_home("older");
        fs::create_dir_all(home.join(".claude")).unwrap();
        let claude = &ADAPTERS[0];
        let before = claude.merged(json!({"model": "opus"}), EXE);
        fs::write(home.join(".claude/settings.json"), serde_json::to_string_pretty(&before).unwrap()).unwrap();
        assert_eq!(all_installed(&home, EXE), Some(true), "status hooks unchanged: still installed");
        assert_eq!(tasks_state(&home), TaskHooks::Missing, "the task integration is opt-in");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_session_start_entry_the_user_changed_is_kept_and_counts() {
        let home = temp_home("user-edited-entry");
        fs::create_dir_all(home.join(".claude")).unwrap();
        ok(install_all(&home, EXE));
        ok(install_tasks_all(&home, EXE));
        let path = home.join(".claude/settings.json");
        let mut v = read_json(&path);
        v["hooks"]["SessionStart"][0]["matcher"] = json!("startup");
        v["hooks"]["SessionStart"][0]["hooks"][0]["timeout"] = json!(3);
        fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        assert_eq!(tasks_state(&home), TaskHooks::Installed, "any form counts");
        assert_eq!(all_installed(&home, EXE), Some(true));
        ok(install_tasks_all(&home, "/opt/tcanvas-agent"));
        let after = read_json(&path);
        assert_eq!(after["hooks"]["SessionStart"], v["hooks"]["SessionStart"], "the user's edit is kept");
        ok(install_all(&home, EXE));
        assert_eq!(read_json(&path)["hooks"]["SessionStart"], v["hooks"]["SessionStart"], "kept by the status install too");
        // An edited skill counts too, and stays.
        let skill = home.join(".claude/skills/tcanvas-tasks/SKILL.md");
        let edited = task_skill::render(EXE).replace("Append, never rewrite", "Append, and say why");
        fs::write(&skill, &edited).unwrap();
        assert_eq!(tasks_state(&home), TaskHooks::Installed);
        let done = ok(install_tasks_all(&home, EXE));
        assert_eq!(done[0].skill, Some(SkillChange::KeptEdited));
        let done = ok(uninstall_all(&home, Some(Part::Tasks)));
        assert_eq!((done[0].settings, done[0].skill), (true, Some(SkillChange::KeptEdited)));
        assert_eq!(fs::read_to_string(&skill).unwrap(), edited, "kept");
        assert_eq!(tasks_state(&home), TaskHooks::Missing, "no entry any more");
        // The skill alone missing: not installed.
        fs::remove_file(&skill).unwrap();
        ok(install_tasks_all(&home, EXE));
        fs::remove_file(&skill).unwrap();
        assert_eq!(tasks_state(&home), TaskHooks::Missing);
        let _ = fs::remove_dir_all(home);
    }

    #[cfg(unix)]
    #[test]
    fn settings_keep_their_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_home("modes");
        fs::create_dir_all(home.join(".claude")).unwrap();
        let path = home.join(".claude/settings.json");
        for mode in [0o600, 0o640, 0o444] {
            fs::write(&path, "{\"apiKeyHelper\": \"secret\"}").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            ok(install_all(&home, EXE));
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, mode, "install, {mode:o}");
            ok(uninstall_all(&home, None));
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, mode, "uninstall, {mode:o}");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let _ = fs::remove_file(home.join(".claude/settings.json.tcanvas.bak"));
        }
        let _ = fs::remove_dir_all(home);
    }

    #[cfg(unix)]
    #[test]
    fn writes_go_through_symlinks() {
        let home = temp_home("symlink");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join("dotfiles")).unwrap();
        fs::write(home.join("dotfiles/claude.json"), "{}").unwrap();
        std::os::unix::fs::symlink(home.join("dotfiles/claude.json"), home.join(".claude/settings.json")).unwrap();
        ok(install_all(&home, EXE));
        ok(install_tasks_all(&home, EXE));
        assert!(fs::symlink_metadata(home.join(".claude/settings.json")).unwrap().file_type().is_symlink());
        assert!(fs::read_to_string(home.join("dotfiles/claude.json")).unwrap().contains("task prime"));
        ok(uninstall_all(&home, None));
        assert!(fs::symlink_metadata(home.join(".claude/settings.json")).unwrap().file_type().is_symlink(), "still a link");
        assert_eq!(read_json(&home.join("dotfiles/claude.json")), json!({}));
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn prompts_and_turn_ends_are_counted_attention_requests_are_not() {
        for e in ["UserPromptSubmit", "userPromptSubmitted", "BeforeAgent"] {
            assert_eq!(counter_of(e), Some(PROMPTS_OPTION), "{e}");
        }
        for e in ["Stop", "agentStop", "AfterAgent"] {
            assert_eq!(counter_of(e), Some(TURNS_OPTION), "{e}");
        }
        for e in ["Notification", "PermissionRequest", "permissionRequest", "PostToolUse", "AfterTool", "SessionEnd", "SessionStart"] {
            assert_eq!(counter_of(e), None, "{e}");
        }
        // Every agent has both: its prompts and its turn ends are counted.
        for a in ADAPTERS {
            assert!(a.events.iter().any(|(e, _)| counter_of(e) == Some(PROMPTS_OPTION)), "{}", a.name);
            assert!(a.events.iter().any(|(e, _)| counter_of(e) == Some(TURNS_OPTION)), "{}", a.name);
        }
        assert_eq!(counter_args("%3", TURNS_OPTION, "17-42").join(" "), "; set -p -t %3 @tc_agent_turn_mark 17-42");
        // One tmux call per event: the mark goes with the state, in the same argument list.
        let mut one = tmux_args("%3", Some(AgentState::Waiting), &[9], &Payload::default());
        one.extend(counter_args("%3", TURNS_OPTION, &mark_token(17, 42)));
        assert_eq!(one.join(" "), "set -p -t %3 @tc_agent waiting 9 ; set -p -t %3 @tc_agent_turn_mark 17-42");
        // Concurrent hooks (same instant, other processes) and a restarted agent (time goes on)
        // write other tokens: none is lost, none repeats.
        assert_ne!(mark_token(17, 42), mark_token(17, 43));
        assert_ne!(mark_token(17, 42), mark_token(18, 42));
        // `SessionEnd` unsets the mark and the payload, never the counters.
        assert!(!tmux_args("%3", None, &[], &Payload::default()).iter().any(|a| a == PROMPTS_OPTION || a == TURNS_OPTION));
    }
}
