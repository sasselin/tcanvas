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

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Map, Value, json};

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
fn is_hidden_char(c: char) -> bool {
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
        // The payload must never cost the mark: if the combined call fails, set the mark alone.
        if !run_tmux(&tmux_args(&pane, change, &pids, &payload)) && payload != Payload::default() {
            run_tmux(&tmux_args(&pane, change, &pids, &Payload::default()));
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
    },
    Adapter {
        name: "Codex",
        dir: ".codex",
        file: ".codex/hooks.json",
        format: Format::Grouped { timeout: 10 },
        events: &[("UserPromptSubmit", None), ("PostToolUse", None), ("PermissionRequest", None), ("Stop", None), ("SessionEnd", None)],
    },
    Adapter {
        name: "Gemini CLI",
        dir: ".gemini",
        file: ".gemini/settings.json",
        // Milliseconds for Gemini.
        format: Format::Grouped { timeout: 10_000 },
        events: &[("BeforeAgent", None), ("AfterTool", None), ("Notification", None), ("AfterAgent", None), ("SessionEnd", None)],
    },
    Adapter {
        name: "Qwen Code",
        dir: ".qwen",
        file: ".qwen/settings.json",
        format: Format::Grouped { timeout: 10 },
        events: &[("UserPromptSubmit", None), ("PostToolUse", None), ("PermissionRequest", None), ("Notification", None), ("Stop", None), ("SessionEnd", None)],
    },
    Adapter {
        name: "Copilot CLI",
        dir: ".copilot",
        file: ".copilot/hooks/tcanvas.json",
        format: Format::Flat,
        events: &[("userPromptSubmitted", None), ("postToolUse", None), ("permissionRequest", None), ("notification", None), ("agentStop", None), ("sessionEnd", None)],
    },
];

/// Shell command an agent runs for `event`. The guard keeps agents quiet if tcanvas is
/// removed; `; true` keeps the exit code at 0 whatever happens.
pub fn hook_command(exe: &str, event: &str) -> String {
    let q = shell_quote(exe);
    format!("[ -x {q} ] && {q} hook {event}; true")
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Our entries are recognized by their command, whatever path the binary had then.
fn is_ours(command: &str) -> bool {
    command.contains("tcanvas-agent") && command.contains(" hook ")
}

fn entry_command(entry: &Value) -> Option<&str> {
    entry.get("command").or_else(|| entry.get("bash")).and_then(Value::as_str)
}

impl Adapter {
    pub fn detected(&self, home: &Path) -> bool {
        home.join(self.dir).is_dir()
    }

    fn our_entry(&self, exe: &str, event: &str, matcher: Option<&str>) -> Value {
        let command = hook_command(exe, event);
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

    /// Removes our entries from `config`; returns whether anything changed.
    fn strip(&self, config: &mut Value) -> bool {
        let Some(hooks) = config.get_mut("hooks").and_then(Value::as_object_mut) else { return false };
        let mut changed = false;
        for list in hooks.values_mut().filter_map(Value::as_array_mut) {
            let before = list.len();
            match self.format {
                Format::Grouped { .. } => {
                    for group in list.iter_mut() {
                        if let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                            let n = inner.len();
                            inner.retain(|h| !entry_command(h).is_some_and(is_ours));
                            changed |= inner.len() != n;
                        }
                    }
                    list.retain(|g| !g.get("hooks").and_then(Value::as_array).is_some_and(Vec::is_empty));
                }
                Format::Flat => list.retain(|h| !entry_command(h).is_some_and(is_ours)),
            }
            changed |= list.len() != before;
        }
        hooks.retain(|_, list| !list.as_array().is_some_and(Vec::is_empty));
        if hooks.is_empty() {
            config.as_object_mut().expect("has hooks, so an object").remove("hooks");
        }
        changed
    }

    /// `config` with exactly our entries for `exe`.
    fn merged(&self, mut config: Value, exe: &str) -> Value {
        self.strip(&mut config);
        if self.format == Format::Flat {
            config.as_object_mut().expect("object").entry("version").or_insert(json!(1));
        }
        let hooks = config.as_object_mut().expect("object").entry("hooks").or_insert_with(|| json!({}));
        for (event, matcher) in self.events {
            let list = hooks.as_object_mut().expect("hooks is an object").entry(*event).or_insert_with(|| json!([]));
            if let Some(list) = list.as_array_mut() {
                list.push(self.our_entry(exe, event, *matcher));
            }
        }
        config
    }

    pub fn installed_in(&self, home: &Path, exe: &str) -> bool {
        match read_config(&home.join(self.file)) {
            Ok(Some(config)) => self.merged(config.clone(), exe) == config,
            _ => false,
        }
    }

    pub fn install_in(&self, home: &Path, exe: &str) -> io::Result<bool> {
        let path = home.join(self.file);
        let config = read_config(&path)?.unwrap_or_else(|| json!({}));
        let merged = self.merged(config.clone(), exe);
        if merged == config {
            return Ok(false);
        }
        write_config(&path, &merged)?;
        Ok(true)
    }

    pub fn uninstall_in(&self, home: &Path) -> io::Result<bool> {
        let path = home.join(self.file);
        let Some(mut config) = read_config(&path)? else { return Ok(false) };
        if !self.strip(&mut config) {
            return Ok(false);
        }
        if self.format == Format::Flat && config.get("hooks").is_none() {
            fs::remove_file(&path)?;
        } else {
            write_config(&path, &config)?;
        }
        Ok(true)
    }
}

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

/// Atomic write through the symlink (dotfile managers), with a one-time backup.
fn write_config(path: &Path, config: &Value) -> io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir)?;
    }
    let backup = backup_path(&target);
    if target.exists() && !backup.exists() {
        fs::copy(&target, &backup)?;
    }
    let tmp = target.with_extension("tcanvas-tmp");
    let mut text = serde_json::to_string_pretty(config).map_err(io::Error::other)?;
    text.push('\n');
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &target)
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

/// Every detected agent has our hooks for `exe`. `None` if no agent is detected.
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

/// Installs every detected agent's hooks; names of the agents whose config changed.
pub fn install_all(home: &Path, exe: &str) -> io::Result<Vec<&'static str>> {
    let mut changed = Vec::new();
    for a in detected(home) {
        if a.install_in(home, exe)? {
            changed.push(a.name);
        }
    }
    Ok(changed)
}

pub fn uninstall_all(home: &Path) -> io::Result<Vec<&'static str>> {
    let mut changed = Vec::new();
    for a in ADAPTERS {
        if a.uninstall_in(home)? {
            changed.push(a.name);
        }
    }
    Ok(changed)
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
        assert!(is_ours(&hook_command(EXE, "Stop")));
        assert!(!is_ours("notify-send done"));
    }

    #[test]
    fn claude_install_keeps_the_user_settings_and_is_idempotent() {
        let home = temp_home("claude");
        fs::create_dir_all(home.join(".claude")).unwrap();
        let user = r#"{"model": "opus", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "notify-send done"}]}]}, "theme": "dark"}"#;
        fs::write(home.join(".claude/settings.json"), user).unwrap();
        assert_eq!(all_installed(&home, EXE), Some(false));

        assert_eq!(install_all(&home, EXE).unwrap(), vec!["Claude Code"]);
        assert_eq!(all_installed(&home, EXE), Some(true));
        let v: Value = serde_json::from_str(&fs::read_to_string(home.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(v["model"], "opus");
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "hooks", "theme"], "key order kept");
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "notify-send done");
        assert_eq!(stop[1]["hooks"][0]["command"], hook_command(EXE, "Stop"));
        assert_eq!(v["hooks"]["Notification"][0]["matcher"], CLAUDE_ATTENTION);
        assert_eq!(fs::read_to_string(home.join(".claude/settings.json.tcanvas.bak")).unwrap(), user, "backup of the original");

        assert!(install_all(&home, EXE).unwrap().is_empty(), "second install: no change");
        // A new binary path replaces our entries instead of adding more.
        assert_eq!(all_installed(&home, "/opt/tcanvas/tcanvas-agent"), Some(false));
        install_all(&home, "/opt/tcanvas/tcanvas-agent").unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(home.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);

        assert_eq!(uninstall_all(&home).unwrap(), vec!["Claude Code"]);
        let v: Value = serde_json::from_str(&fs::read_to_string(home.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(v, serde_json::from_str::<Value>(user).unwrap(), "uninstall gives the user's settings back");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn invalid_json_is_never_overwritten() {
        let home = temp_home("invalid");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(home.join(".claude/settings.json"), "{ oops").unwrap();
        assert!(install_all(&home, EXE).is_err());
        assert_eq!(fs::read_to_string(home.join(".claude/settings.json")).unwrap(), "{ oops");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn only_detected_agents_get_hooks() {
        let home = temp_home("detect");
        assert_eq!(all_installed(&home, EXE), None, "no agent at all");
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(home.join(".copilot")).unwrap();
        assert_eq!(install_all(&home, EXE).unwrap(), vec!["Codex", "Copilot CLI"]);
        assert!(!home.join(".claude/settings.json").exists());
        assert!(!home.join(".gemini").exists());

        let codex: Value = serde_json::from_str(&fs::read_to_string(home.join(".codex/hooks.json")).unwrap()).unwrap();
        assert_eq!(codex["hooks"]["PermissionRequest"][0]["hooks"][0]["command"], hook_command(EXE, "PermissionRequest"));
        let copilot: Value = serde_json::from_str(&fs::read_to_string(home.join(".copilot/hooks/tcanvas.json")).unwrap()).unwrap();
        assert_eq!(copilot["version"], 1);
        assert_eq!(copilot["hooks"]["agentStop"][0]["bash"], hook_command(EXE, "agentStop"));
        assert_eq!(all_installed(&home, EXE), Some(true));

        uninstall_all(&home).unwrap();
        assert!(!home.join(".copilot/hooks/tcanvas.json").exists(), "our own file is removed");
        let codex: Value = serde_json::from_str(&fs::read_to_string(home.join(".codex/hooks.json")).unwrap()).unwrap();
        assert_eq!(codex, json!({}));
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
        install_all(&home, EXE).unwrap();
        assert!(fs::symlink_metadata(home.join(".claude/settings.json")).unwrap().file_type().is_symlink());
        assert!(fs::read_to_string(home.join("dotfiles/claude.json")).unwrap().contains("tcanvas-agent"));
        let _ = fs::remove_dir_all(home);
    }
}
