//! Messages exchanged between the app (Windows) and `tcanvas-agent` (WSL).
//! Transport: one JSON object per line on the agent's stdin/stdout.

use serde::{Deserialize, Serialize};

/// Dedicated tmux socket (`tmux -L tcanvas`), isolated from the user's regular tmux.
pub const TMUX_SOCKET: &str = "tcanvas";
/// Prefix of the tmux sessions managed by the canvas.
pub const SESSION_PREFIX: &str = "tc_";
pub const DEFAULT_STALL_SECS: u64 = 60;

/// Effective tmux socket: `TCANVAS_TMUX_SOCKET` lets tests use a throwaway server.
pub fn tmux_socket() -> String {
    std::env::var("TCANVAS_TMUX_SOCKET").unwrap_or_else(|_| TMUX_SOCKET.to_string())
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(tag = "kind", content = "code")]
pub enum TermStatus {
    /// The shell is waiting for a command.
    Idle,
    /// A command is running and producing output (or a full-screen app is open).
    Running,
    /// A command is running but no longer prints anything and uses no CPU.
    Stalled,
    /// The last line looks like a question (`[y/N]`, `password:`…).
    WaitingInput,
    /// The last command failed with this code.
    Exited(i32),
    /// The tmux pane is dead.
    Dead,
}

/// Problem reported by the agent. The protocol carries the meaning, not the text:
/// the app translates it into the Windows language.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind")]
pub enum Problem {
    /// The path exists but is not a folder.
    NotADir {
        path: String,
    },
    DirNotFound {
        path: String,
    },
    /// The folder cannot be read (OS detail, not translated).
    Unreadable {
        detail: String,
    },
    /// The requested sha does not look like a sha.
    CommitRefused,
    CommitNotFound,
    /// File path is absolute or escapes the repository.
    PathRefused,
    NotAGitRepo,
    /// The file does not differ from HEAD.
    NoDiff,
    TmuxMissing {
        detail: String,
    },
    /// Installing the shell hooks failed (OS detail, not translated).
    HooksFailed {
        detail: String,
    },
}

/// Shell hooks (exit code and duration of each command) for the user's login shell.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hooks {
    Installed,
    /// Supported shell (bash, zsh) without the hooks: `AgentCmd::InstallHooks` adds them.
    Missing,
    /// Another shell (fish, ...): nothing to offer.
    Unsupported,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct SessionInfo {
    pub session: String,
    pub status: TermStatus,
    /// Foreground command (full command line), if a command is running.
    pub command: Option<String>,
    pub cwd: Option<String>,
    /// Exit code and duration of the last finished command (requires the shell hooks).
    pub last_exit: Option<i32>,
    pub last_duration_secs: Option<u64>,
    /// Last non-empty lines of the screen, for the zoomed-out preview.
    pub tail: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind", content = "from")]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    /// Renamed from this path.
    Renamed(String),
    /// New file not yet tracked by git.
    Untracked,
}

impl ChangeKind {
    pub fn letter(&self) -> char {
        match self {
            ChangeKind::Modified => 'M',
            ChangeKind::Added => 'A',
            ChangeKind::Deleted => 'D',
            ChangeKind::Renamed(_) => 'R',
            ChangeKind::Untracked => '?',
        }
    }
}

/// A modified file in a watched folder (path relative to the root).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
    /// Lines added / removed relative to HEAD (unknown outside git or for a binary).
    pub added: Option<u32>,
    pub removed: Option<u32>,
    /// Last modification (Unix seconds; 0 if the file no longer exists).
    pub mtime: u64,
    pub binary: bool,
}

/// State of a watched folder, sent on every change.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ChangeSet {
    /// Key given by the app (project id).
    pub key: String,
    /// Resolved absolute path (`~` expanded), if the folder exists.
    pub root: Option<String>,
    pub git: bool,
    pub branch: Option<String>,
    /// Detached HEAD (`branch` is then `None`).
    #[serde(default)]
    pub detached: bool,
    pub error: Option<Problem>,
    pub files: Vec<FileChange>,
    /// The list was truncated (too many files).
    pub truncated: bool,
    /// Latest commits of the branch, newest first (empty outside git).
    #[serde(default)]
    pub commits: Vec<CommitInfo>,
}

/// A recent commit of the watched repository.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct CommitInfo {
    pub sha: String,
    pub summary: String,
    pub author: String,
    /// Commit date (Unix seconds).
    pub time: u64,
}

/// A subfolder, for the folder picker.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    pub name: String,
    /// Contains a git repository (`.git`).
    pub git: bool,
}

/// Contents of a WSL folder, in response to `AgentCmd::ListDir`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct DirListing {
    /// Path as requested (used to match the response).
    pub request: String,
    /// Resolved absolute path, if the folder exists.
    pub path: Option<String>,
    pub parent: Option<String>,
    pub git: bool,
    /// Visible subfolders (hidden folders are omitted), sorted by name.
    pub dirs: Vec<DirEntry>,
    pub error: Option<Problem>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type")]
pub enum AgentEvent {
    Hello {
        version: String,
        hooks: Hooks,
    },
    /// Hooks state after `AgentCmd::InstallHooks`.
    HooksChanged {
        hooks: Hooks,
    },
    Session(SessionInfo),
    Gone {
        session: String,
    },
    Error {
        problem: Problem,
    },
    Changes(ChangeSet),
    /// `text` is empty when `problem` explains why there is no diff.
    Diff {
        key: String,
        file: String,
        text: String,
        truncated: bool,
        #[serde(default)]
        problem: Option<Problem>,
    },
    DirListing(DirListing),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type")]
pub enum AgentCmd {
    SetStall {
        session: String,
        secs: u64,
    },
    Kill {
        session: String,
    },
    /// Watch the modified files of `path` (restarted if already watched with another path).
    Watch {
        key: String,
        path: String,
    },
    Unwatch {
        key: String,
    },
    /// Diff of a file (relative path) in a watched folder.
    Diff {
        key: String,
        file: String,
    },
    /// Contents of a commit (`git show`) in a watched folder; response: `AgentEvent::Diff`
    /// with `file` = `sha`.
    ShowCommit {
        key: String,
        sha: String,
    },
    /// Subfolders of `path` (`~` accepted), for the folder picker.
    ListDir {
        path: String,
    },
    /// Install the hooks for the user's shell; response: `AgentEvent::HooksChanged`, or
    /// `AgentEvent::Error` with `Problem::HooksFailed`.
    InstallHooks,
}

pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut line = serde_json::to_string(msg).expect("protocol message serializes");
    line.push('\n');
    line
}

pub fn decode<'a, T: Deserialize<'a>>(line: &'a str) -> serde_json::Result<T> {
    serde_json::from_str(line.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_event() {
        let ev = AgentEvent::Session(SessionInfo {
            session: "tc_ab12".into(),
            status: TermStatus::Exited(2),
            command: None,
            cwd: Some("/home".into()),
            last_exit: Some(2),
            last_duration_secs: Some(31),
            tail: vec!["$ false".into()],
        });
        let line = encode(&ev);
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        assert_eq!(decode::<AgentEvent>(&line).unwrap(), ev);
    }

    #[test]
    fn roundtrip_changes() {
        let ev = AgentEvent::Changes(ChangeSet {
            key: "p1".into(),
            root: Some("/home/me/api".into()),
            git: true,
            branch: Some("main".into()),
            detached: false,
            error: None,
            files: vec![
                FileChange { path: "src/a.rs".into(), kind: ChangeKind::Modified, added: Some(3), removed: Some(1), mtime: 42, binary: false },
                FileChange { path: "b.rs".into(), kind: ChangeKind::Renamed("old.rs".into()), added: None, removed: None, mtime: 0, binary: true },
            ],
            truncated: false,
            commits: vec![],
        });
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let ev = AgentEvent::DirListing(DirListing {
            request: "~".into(),
            path: Some("/home/me".into()),
            parent: Some("/home".into()),
            git: false,
            dirs: vec![DirEntry { name: "api".into(), git: true }],
            error: Some(Problem::Unreadable { detail: "denied".into() }),
        });
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let ev = AgentEvent::Diff { key: "p1".into(), file: "a.rs".into(), text: String::new(), truncated: false, problem: Some(Problem::NotAGitRepo) };
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let ev = AgentEvent::Error { problem: Problem::TmuxMissing { detail: "no such file".into() } };
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let cmd = AgentCmd::Diff { key: "p1".into(), file: "src/a.rs".into() };
        assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
    }

    #[test]
    fn roundtrip_cmd() {
        let cmd = AgentCmd::SetStall { session: "tc_x".into(), secs: 5 };
        assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
        assert_eq!(decode::<AgentCmd>(&encode(&AgentCmd::InstallHooks)).unwrap(), AgentCmd::InstallHooks);
    }

    #[test]
    fn roundtrip_hooks() {
        for hooks in [Hooks::Installed, Hooks::Missing, Hooks::Unsupported] {
            let ev = AgentEvent::Hello { version: "1".into(), hooks };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
            let ev = AgentEvent::HooksChanged { hooks };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        }
        let ev = AgentEvent::Error { problem: Problem::HooksFailed { detail: "read-only".into() } };
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
    }
}
