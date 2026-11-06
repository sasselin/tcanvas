//! Coding agents (Claude Code, Codex, Gemini CLI, Copilot CLI, Qwen Code) tell tcanvas when
//! they wait for the user, through their own lifecycle hooks.
//!
//! - Sink: `tcanvas-agent hook <event>` runs from any agent's hook. It sets the tmux pane
//!   option `@tc_agent` to `<working|waiting> <pid,pid,...>` (the hook's ancestors, so the
//!   status loop can check that the mark belongs to the process in the foreground), or unsets
//!   it when the session ends. It only acts inside the tcanvas tmux server, prints `{}` (a
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
    while pid > 1 && pids.len() < 16 {
        pids.push(pid);
        pid = super::status::parent_of(pid).unwrap_or(0);
    }
    pids
}

/// `tcanvas-agent hook <event>`.
pub fn hook_cli(event: Option<&str>) -> io::Result<()> {
    // Agents pipe a JSON payload on stdin: read it so they never hit a closed pipe, but
    // never wait long for it (Codex-style callers may leave stdin open).
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = io::stdin().lock().take(1 << 20).read_to_end(&mut sink);
        let _ = tx.send(());
    });

    let in_tcanvas = std::env::var("TMUX").is_ok_and(|t| t.contains("tcanvas"));
    if let (true, Some(change), Ok(pane)) = (in_tcanvas, event.and_then(state_of), std::env::var("TMUX_PANE")) {
        // Plain `tmux` talks to the server named by $TMUX, the one this pane belongs to.
        let mut cmd = Command::new("tmux");
        match change {
            Some(state) => cmd.args(["set", "-p", "-t", &pane, PANE_OPTION, &format_mark(state, &ancestors())]),
            None => cmd.args(["set", "-p", "-u", "-t", &pane, PANE_OPTION]),
        };
        let _ = cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    }
    let _ = rx.recv_timeout(Duration::from_millis(300));
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
