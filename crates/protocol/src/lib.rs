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
    /// Removal refused: the worktree is not safe to remove, for this reason.
    WorktreeKept {
        path: String,
        reason: KeepReason,
    },
    /// The path is not one of the worktrees git lists for this repository.
    NotAWorktree {
        path: String,
    },
    /// Prune asked, but git reports no entry whose folder is gone.
    NothingToPrune,
    /// Branch deletion refused.
    BranchKept {
        branch: String,
        reason: BranchKeepReason,
    },
    /// The worktree was removed, but `git branch -d` refused its branch (git's message, not
    /// translated).
    BranchNotDeleted {
        branch: String,
        detail: String,
    },
    /// A git command that writes refused or failed (git's message, not translated).
    GitFailed {
        detail: String,
    },
    /// Prune refused: this entry would be pruned now but was not in the confirmed list.
    PruneNotConfirmed {
        path: String,
    },
    /// Removal refused: the worktree's branch is not the one the user confirmed deleting.
    BranchChanged {
        path: String,
        expected: String,
        found: Option<String>,
    },
    /// Removal refused: the files git ignores in the worktree are not the ones the user saw.
    IgnoredChanged {
        path: String,
    },
    /// Text not sent: no such canvas session.
    SessionNotFound {
        session: String,
    },
    /// Text not sent: tmux refused it (tmux's message, not translated).
    SendFailed {
        detail: String,
    },
    /// Text not sent: the session's pane is dead (its program exited, the pane kept).
    PaneDead {
        session: String,
    },
    /// Text not sent: the pane shows a tmux screen (`mode`: tmux's name, e.g. `clock-mode`,
    /// `tree-mode`) that would take the keys.
    PaneBusy {
        session: String,
        mode: String,
    },
    /// Text not sent: its time limit (`AgentCmd::SendText::limit_ms`) passed before it could be.
    Expired,
}

/// Why a worktree is not offered for removal (and why the agent refuses to remove it).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeepReason {
    MainCheckout,
    /// The repository is bare: the entry has no working tree.
    Bare,
    /// Its folder is missing right now: prune it instead.
    Missing,
    Locked,
    /// Not fully merged into the default branch (or unknown: detached head, a branch that never
    /// moved since it was created, no default branch).
    NotMerged,
    /// Its branch's reflog, or its own HEAD reflog, names commits the default branch does not
    /// have: removing it (or deleting the branch) would drop the last reference to them.
    ReflogCommits,
    /// Uncommitted changes or untracked files.
    Dirty,
    /// Tracked files flagged `assume-unchanged` or `skip-worktree`: `git status` (and git's own
    /// check before removing) does not look at them, so their local changes would go unseen.
    HiddenChanges,
    /// Its changes, or what uses it, could not be checked.
    NotChecked,
    /// Another worktree of the repository lives inside its folder.
    HoldsWorktree,
    /// Its ignored files hold a git repository (a `.git` below its root).
    HoldsRepository,
    /// A terminal (a card, or any pane of the canvas tmux server) has its folder in it.
    HasTerminal,
    /// A running program has its current folder in it.
    ProcessInside,
    /// A group's folder is in it.
    GroupFolder,
}

/// Why a merged branch is not deleted.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum BranchKeepReason {
    NotFound,
    /// The default branch itself.
    Default,
    /// A worktree has it checked out.
    CheckedOut,
    /// Not fully merged into the default branch, or never moved since it was created.
    NotMerged,
    /// Merged into the default branch, but not into what `git branch -d` checks (its upstream,
    /// else the main checkout's HEAD): git would refuse it.
    NotMergedLocally,
    /// Its reflog names commits the default branch does not have: deleting it drops them.
    ReflogCommits,
}

/// What uses a worktree's folder, besides its own state. The app fills what it knows (cards,
/// groups, the other worktrees); the agent fills everything from fresh data.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Usage {
    /// Another listed worktree is inside it.
    pub holds_worktree: bool,
    /// Tracked files flagged `assume-unchanged` or `skip-worktree`.
    pub hidden_changes: bool,
    /// A git repository sits in its ignored files.
    pub holds_repository: bool,
    pub terminal: bool,
    pub process: bool,
    pub group_folder: bool,
    /// Something could not be checked (tmux did not answer, the walk was cut short...).
    pub unknown: bool,
}

/// `root` is `path` or one of its parents, compared by whole path components (`/a/b` holds
/// `/a/b/c` and `/a/b/`, not `/a/bc`).
pub fn path_inside(root: &str, path: &str) -> bool {
    let (root, path) = (root.trim_end_matches('/'), path.trim_end_matches('/'));
    path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
}

/// Another worktree of `all` lives inside `wt`'s folder (removing `wt` would delete it).
pub fn holds_worktree(wt: &Worktree, all: &[Worktree]) -> bool {
    all.iter().any(|w| w.path.trim_end_matches('/') != wt.path.trim_end_matches('/') && path_inside(&wt.path, &w.path))
}

/// The safe-to-remove rule: `None` when the linked worktree can be removed without losing
/// anything, otherwise the first reason to keep it. The app shows the result; the agent checks
/// again on fresh data (its own `git status`, the panes, the processes, the watched folders, a
/// walk of the ignored files) right before acting.
pub fn keep_reason(wt: &Worktree, usage: &Usage) -> Option<KeepReason> {
    let reason = if wt.main {
        KeepReason::MainCheckout
    } else if wt.bare {
        KeepReason::Bare
    } else if wt.prunable {
        KeepReason::Missing
    } else if wt.locked {
        KeepReason::Locked
    } else if wt.merged != Some(true) {
        KeepReason::NotMerged
    } else if wt.reflog_commits {
        KeepReason::ReflogCommits
    } else if wt.dirty == Some(true) {
        KeepReason::Dirty
    } else if usage.hidden_changes {
        KeepReason::HiddenChanges
    } else if wt.dirty.is_none() || usage.unknown {
        KeepReason::NotChecked
    } else if usage.holds_worktree {
        KeepReason::HoldsWorktree
    } else if usage.holds_repository {
        KeepReason::HoldsRepository
    } else if usage.terminal {
        KeepReason::HasTerminal
    } else if usage.process {
        KeepReason::ProcessInside
    } else if usage.group_folder {
        KeepReason::GroupFolder
    } else {
        return None;
    };
    Some(reason)
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
    /// From the hooks of the coding agent in the foreground (empty without one, or when its
    /// hooks did not send them): its own session id, its transcript file, and the last
    /// prompt the user submitted (one line, truncated).
    #[serde(default)]
    pub agent_session_id: Option<String>,
    #[serde(default)]
    pub agent_transcript: Option<String>,
    #[serde(default)]
    pub last_prompt: Option<String>,
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
    /// The repository is a linked worktree (its `.git` is a file, not a folder).
    #[serde(default)]
    pub worktree: bool,
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
    /// The folder is a linked worktree (`.git` is a file).
    #[serde(default)]
    pub worktree: bool,
    /// Visible subfolders (hidden folders are omitted), sorted by name.
    pub dirs: Vec<DirEntry>,
    pub error: Option<Problem>,
}

/// One worktree of a repository (`git worktree list --porcelain`), with its state.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Worktree {
    /// Absolute path of its folder.
    pub path: String,
    /// Commit checked out (`None`: bare entry, or a branch without commits yet).
    pub head: Option<String>,
    /// Branch checked out, without `refs/heads/` (`None` when detached or bare).
    pub branch: Option<String>,
    pub detached: bool,
    /// The main checkout (always the first entry of the list).
    pub main: bool,
    /// The repository itself is bare: this entry has no working tree.
    pub bare: bool,
    pub locked: bool,
    /// Reason given to `git worktree lock --reason`.
    pub lock_reason: Option<String>,
    /// Its folder is gone: `git worktree prune` would remove the entry.
    pub prunable: bool,
    /// Uncommitted or untracked changes (`None`: unknown, e.g. bare or missing folder).
    pub dirty: Option<bool>,
    /// Commits of `head` missing from the default branch, and the reverse (`None` without a
    /// default branch).
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    /// The branch is fully merged into the default branch (`None` for the default branch itself,
    /// a detached head, or without a default branch).
    pub merged: Option<bool>,
    /// Time of the commit at `head` (Unix seconds).
    pub last_commit: Option<u64>,
    /// `git branch -d` would accept its branch now: merged into what git checks there (the
    /// branch's upstream, else the main checkout's HEAD). Only computed for a merged branch.
    #[serde(default)]
    pub branch_deletable: bool,
    /// A merged worktree whose branch reflog or own HEAD reflog names commits the default branch
    /// does not have (made then reset away, or made on a detached HEAD): not removable. Also set
    /// when that could not be checked.
    #[serde(default)]
    pub reflog_commits: bool,
}

/// The worktrees of the repository holding a watched folder, sent when they change.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct WorktreeList {
    /// Key given by the app with `AgentCmd::Watch` (project id).
    pub key: String,
    /// Path of the repository's main worktree (the repository itself when bare): the same for
    /// every folder of one repository. `None` outside git (the list is then empty).
    pub repo: Option<String>,
    /// Branch that ahead/behind/merged compare with: what `origin/HEAD` points to (e.g.
    /// `origin/main`), else `main`, else `master`.
    pub default_branch: Option<String>,
    /// Main checkout first, then the linked worktrees sorted by path.
    pub worktrees: Vec<Worktree>,
    /// Local branches fully merged into the default branch that no worktree has checked out
    /// (never the default branch itself, nor a branch that did not move since it was created,
    /// nor one `git branch -d` would refuse), sorted by name.
    #[serde(default)]
    pub merged_branches: Vec<MergedBranch>,
}

/// A local branch that can be deleted with `git branch -d`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct MergedBranch {
    pub name: String,
    /// Time of its last commit (Unix seconds).
    pub last_commit: Option<u64>,
}

/// A cleanup action on a watched folder's repository. Nothing is ever forced.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind")]
pub enum Cleanup {
    /// `git worktree remove <path>`, then `git branch -d <branch>` when `delete_branch` names
    /// one: the branch the user confirmed, which must still be the worktree's.
    /// `ignored`: the ignored files the user saw in the confirmation; the agent lists them again
    /// right before removing and refuses if they differ.
    RemoveWorktree { path: String, delete_branch: Option<String>, ignored: Ignored },
    /// `git worktree prune`, for entries whose folder is missing; `paths`: the entries the user
    /// confirmed (refused if git would prune any other).
    Prune { paths: Vec<String> },
    /// `git branch -d <branch>`, for a merged branch without a worktree.
    DeleteBranch { branch: String },
}

/// Disk space used by a worktree's folder, and what of it git ignores (what `git worktree
/// remove` deletes without asking).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct WorktreeSize {
    pub path: String,
    /// `None`: unknown (not a worktree of the repository, unreadable, or too big to measure).
    pub bytes: Option<u64>,
    /// `None`: unknown.
    #[serde(default)]
    pub ignored: Option<Ignored>,
}

/// The files git ignores in a worktree, as `git status --ignored` reports them (a folder wholly
/// ignored ends with `/` and is not detailed; nested paths are kept whole: `web/.env.local`).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct Ignored {
    /// Sorted, at most `MAX_IGNORED_SHOWN`.
    pub shown: Vec<String>,
    /// How many there are in all.
    pub total: usize,
}

/// Ignored paths listed per worktree.
pub const MAX_IGNORED_SHOWN: usize = 40;

impl Ignored {
    /// From every ignored path: sorted, the first `MAX_IGNORED_SHOWN` kept.
    pub fn from_paths(mut paths: Vec<String>) -> Self {
        paths.sort();
        paths.dedup();
        let total = paths.len();
        paths.truncate(MAX_IGNORED_SHOWN);
        Ignored { shown: paths, total }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "type")]
pub enum AgentEvent {
    Hello {
        version: String,
        hooks: Hooks,
        /// The agent takes `AgentCmd::SendText` (missing from older agents: they cannot).
        #[serde(default)]
        send_text: bool,
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
    /// Worktrees of a watched folder's repository (follows `AgentCmd::Watch`). An app that
    /// predates this event fails to decode the line and skips it.
    Worktrees(WorktreeList),
    /// Answer to `AgentCmd::Cleanup`: `problem` is `None` when everything asked was done.
    /// `deleted_branch`: the branch deleted along with a worktree.
    Cleaned {
        key: String,
        action: Cleanup,
        #[serde(default)]
        deleted_branch: Option<String>,
        #[serde(default)]
        problem: Option<Problem>,
    },
    /// Answer to `AgentCmd::WorktreeSizes`.
    WorktreeSizes {
        key: String,
        sizes: Vec<WorktreeSize>,
    },
    /// Answer to `AgentCmd::SendText` with its `id`, once tmux took the paste (and the Enter,
    /// if asked): `problem` is `None` when it did. `pasted` with a problem: the text was pasted,
    /// but the Enter could not be pressed (it is in the input line, not submitted).
    TextSent {
        id: u64,
        session: String,
        #[serde(default)]
        problem: Option<Problem>,
        #[serde(default)]
        pasted: bool,
    },
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
    /// Cleans up the repository of a watched folder; response: `AgentEvent::Cleaned`. The agent
    /// checks the safe-to-remove rule again on its own data before acting.
    Cleanup {
        key: String,
        action: Cleanup,
    },
    /// Disk space of these worktrees of a watched folder's repository (walked off the scan
    /// thread); response: `AgentEvent::WorktreeSizes`.
    WorktreeSizes {
        key: String,
        paths: Vec<String>,
    },
    /// Pastes `text` into the active pane of `session` (a canvas session only) through tmux: the
    /// text goes in a private tmux buffer by stdin (never on a command line), and tmux pastes it,
    /// bracketed when the application asked for it, unless the pane is dead or shows a tmux
    /// screen other than copy mode (checked by the same server-side command that pastes). Then,
    /// with `enter`, Enter as its own key a moment later (copy mode left first). Sends to one
    /// session are done in the order asked, one after the other; other sessions do not wait.
    /// `limit_ms`: past this long after the agent got it, the text is never pasted (answered
    /// `Problem::Expired`). Response: `AgentEvent::TextSent` with the same `id`.
    SendText {
        id: u64,
        session: String,
        text: String,
        enter: bool,
        #[serde(default)]
        limit_ms: Option<u64>,
    },
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
            agent_session_id: None,
            agent_transcript: None,
            last_prompt: None,
        });
        let line = encode(&ev);
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        assert_eq!(decode::<AgentEvent>(&line).unwrap(), ev);
    }

    #[test]
    fn roundtrip_agent_session_fields() {
        let ev = AgentEvent::Session(SessionInfo {
            session: "tc_ab12".into(),
            status: TermStatus::Running,
            command: Some("claude".into()),
            cwd: None,
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
            agent_session_id: Some("0b1c-42".into()),
            agent_transcript: Some("/home/me/.claude/projects/x/0b1c-42.jsonl".into()),
            last_prompt: Some("add the total with taxes to the PDF \"now\" \u{e9}t\u{e9}".into()),
        });
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
    }

    #[test]
    fn session_info_decodes_across_versions() {
        // What an older agent sends: no agent session fields.
        let old = r#"{"type":"Session","session":"tc_a","status":{"kind":"Idle"},"command":null,"cwd":"/home","last_exit":null,"last_duration_secs":null,"tail":[]}"#;
        let AgentEvent::Session(info) = decode::<AgentEvent>(old).unwrap() else { panic!("a session") };
        assert_eq!((info.agent_session_id, info.agent_transcript, info.last_prompt), (None, None, None));

        // What an older app reads: the new fields are ignored.
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldSessionInfo {
            session: String,
            status: TermStatus,
            command: Option<String>,
            cwd: Option<String>,
            last_exit: Option<i32>,
            last_duration_secs: Option<u64>,
            tail: Vec<String>,
        }
        let new = SessionInfo {
            session: "tc_a".into(),
            status: TermStatus::Idle,
            command: None,
            cwd: None,
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
            agent_session_id: Some("s".into()),
            agent_transcript: None,
            last_prompt: Some("hi".into()),
        };
        let old: OldSessionInfo = decode(&encode(&new)).unwrap();
        assert_eq!(old.session, "tc_a");
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
            worktree: false,
            dirs: vec![DirEntry { name: "api".into(), git: true, worktree: false }, DirEntry { name: "api-fix".into(), git: true, worktree: true }],
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

    fn worktree(path: &str) -> Worktree {
        Worktree {
            path: path.into(),
            head: Some("a".repeat(40)),
            branch: Some("main".into()),
            detached: false,
            main: true,
            bare: false,
            locked: false,
            lock_reason: None,
            prunable: false,
            dirty: Some(false),
            ahead: Some(0),
            behind: Some(0),
            merged: None,
            last_commit: Some(1_700_000_000),
            reflog_commits: false,
            branch_deletable: false,
        }
    }

    #[test]
    fn roundtrip_worktrees() {
        let linked = Worktree {
            path: "/home/me/shop/.claude/worktrees/my invoices".into(),
            branch: None,
            detached: true,
            main: false,
            locked: true,
            lock_reason: Some("on a USB drive".into()),
            dirty: Some(true),
            ahead: Some(6),
            behind: Some(12),
            merged: Some(false),
            branch_deletable: true,
            ..worktree("x")
        };
        let ev = AgentEvent::Worktrees(WorktreeList {
            key: "p1".into(),
            repo: Some("/home/me/shop".into()),
            default_branch: Some("origin/main".into()),
            worktrees: vec![worktree("/home/me/shop"), linked],
            merged_branches: vec![MergedBranch { name: "fix/old typo".into(), last_commit: Some(1_700_000_000) }, MergedBranch { name: "b".into(), last_commit: None }],
        });
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let empty = AgentEvent::Worktrees(WorktreeList { key: "p2".into(), repo: None, default_branch: None, worktrees: vec![], merged_branches: vec![] });
        assert_eq!(decode::<AgentEvent>(&encode(&empty)).unwrap(), empty);
        // An agent from before cleanup sends no merged branches.
        let old = r#"{"type":"Worktrees","key":"p","repo":null,"default_branch":null,"worktrees":[]}"#;
        let AgentEvent::Worktrees(l) = decode::<AgentEvent>(old).unwrap() else { panic!("a list") };
        assert!(l.merged_branches.is_empty());
        // ... and no `branch_deletable`.
        let old = r#"{"path":"/x","head":null,"branch":null,"detached":false,"main":true,"bare":false,"locked":false,"lock_reason":null,"prunable":false,"dirty":null,"ahead":null,"behind":null,"merged":null,"last_commit":null}"#;
        assert!(!decode::<Worktree>(old).unwrap().branch_deletable);
    }

    #[test]
    fn roundtrip_cleanup() {
        let actions = [
            Cleanup::RemoveWorktree { path: "/home/me/shop-taxes".into(), delete_branch: Some("taxes".into()), ignored: Ignored { shown: vec![".env".into(), "web/.env.local".into()], total: 3 } },
            Cleanup::RemoveWorktree { path: "/home/me/shop-ship".into(), delete_branch: None, ignored: Ignored::default() },
            Cleanup::Prune { paths: vec!["/home/me/gone".into()] },
            Cleanup::DeleteBranch { branch: "fix/typo".into() },
        ];
        for action in actions {
            let cmd = AgentCmd::Cleanup { key: "p1".into(), action: action.clone() };
            assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
            let ev = AgentEvent::Cleaned { key: "p1".into(), action, deleted_branch: Some("taxes".into()), problem: None };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        }
        let cmd = AgentCmd::WorktreeSizes { key: "p1".into(), paths: vec!["/a".into(), "/b c".into()] };
        assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
        let ev = AgentEvent::WorktreeSizes {
            key: "p1".into(),
            sizes: vec![
                WorktreeSize { path: "/a".into(), bytes: Some(4096), ignored: Some(Ignored { shown: vec![".env".into(), "target/".into()], total: 2 }) },
                WorktreeSize { path: "/b c".into(), bytes: None, ignored: None },
            ],
        };
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        // From an agent that does not list ignored files.
        let old = r#"{"type":"WorktreeSizes","key":"k","sizes":[{"path":"/a","bytes":1}]}"#;
        let AgentEvent::WorktreeSizes { sizes, .. } = decode::<AgentEvent>(old).unwrap() else { panic!("sizes") };
        assert_eq!(sizes[0].ignored, None);
    }

    #[test]
    fn roundtrip_cleanup_problems() {
        let mut problems: Vec<Problem> = [
            KeepReason::MainCheckout,
            KeepReason::Bare,
            KeepReason::Missing,
            KeepReason::Locked,
            KeepReason::NotMerged,
            KeepReason::Dirty,
            KeepReason::NotChecked,
            KeepReason::ReflogCommits,
            KeepReason::HiddenChanges,
            KeepReason::HoldsWorktree,
            KeepReason::HoldsRepository,
            KeepReason::HasTerminal,
            KeepReason::ProcessInside,
            KeepReason::GroupFolder,
        ]
        .into_iter()
        .map(|reason| Problem::WorktreeKept { path: "/w".into(), reason })
        .collect();
        problems.extend(
            [BranchKeepReason::NotFound, BranchKeepReason::Default, BranchKeepReason::CheckedOut, BranchKeepReason::NotMerged, BranchKeepReason::NotMergedLocally, BranchKeepReason::ReflogCommits]
                .map(|reason| Problem::BranchKept { branch: "b".into(), reason }),
        );
        problems.extend([
            Problem::NotAWorktree { path: "/etc".into() },
            Problem::NothingToPrune,
            Problem::BranchNotDeleted { branch: "b".into(), detail: "not fully merged".into() },
            Problem::GitFailed { detail: "fatal: x".into() },
            Problem::PruneNotConfirmed { path: "/m".into() },
            Problem::BranchChanged { path: "/w".into(), expected: "a".into(), found: None },
            Problem::IgnoredChanged { path: "/w".into() },
        ]);
        for problem in problems {
            let ev = AgentEvent::Cleaned { key: "k".into(), action: Cleanup::Prune { paths: vec![] }, deleted_branch: None, problem: Some(problem.clone()) };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
            let ev = AgentEvent::Error { problem };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        }
    }

    #[test]
    fn safe_to_remove_rule() {
        let safe = Worktree { main: false, merged: Some(true), dirty: Some(false), ..worktree("/home/me/shop-taxes") };
        let unused = Usage::default();
        assert_eq!(keep_reason(&safe, &unused), None, "merged, clean, unlocked, linked, unused");
        let cases = [
            (Worktree { main: true, ..safe.clone() }, KeepReason::MainCheckout),
            (Worktree { bare: true, ..safe.clone() }, KeepReason::Bare),
            (Worktree { prunable: true, ..safe.clone() }, KeepReason::Missing),
            (Worktree { locked: true, ..safe.clone() }, KeepReason::Locked),
            (Worktree { merged: Some(false), ..safe.clone() }, KeepReason::NotMerged),
            (Worktree { merged: None, ..safe.clone() }, KeepReason::NotMerged),
            (Worktree { dirty: Some(true), ..safe.clone() }, KeepReason::Dirty),
            (Worktree { reflog_commits: true, ..safe.clone() }, KeepReason::ReflogCommits),
            (Worktree { dirty: None, ..safe.clone() }, KeepReason::NotChecked),
        ];
        for (wt, reason) in cases {
            assert_eq!(keep_reason(&wt, &unused), Some(reason), "{wt:?}");
        }
        let used = [
            (Usage { unknown: true, ..unused }, KeepReason::NotChecked),
            (Usage { holds_worktree: true, ..unused }, KeepReason::HoldsWorktree),
            (Usage { hidden_changes: true, ..unused }, KeepReason::HiddenChanges),
            (Usage { holds_repository: true, ..unused }, KeepReason::HoldsRepository),
            (Usage { terminal: true, ..unused }, KeepReason::HasTerminal),
            (Usage { process: true, ..unused }, KeepReason::ProcessInside),
            (Usage { group_folder: true, ..unused }, KeepReason::GroupFolder),
        ];
        for (usage, reason) in used {
            assert_eq!(keep_reason(&safe, &usage), Some(reason), "{usage:?}");
        }
        let all = Usage { holds_worktree: true, hidden_changes: true, holds_repository: true, terminal: true, process: true, group_folder: true, unknown: true };
        assert_eq!(keep_reason(&Worktree { main: true, locked: true, dirty: Some(true), ..safe.clone() }, &all), Some(KeepReason::MainCheckout), "the first reason");
    }

    #[test]
    fn ignored_paths_are_kept_whole_sorted_and_capped() {
        let i = Ignored::from_paths(vec!["web/.env.local".into(), ".env".into(), "target/".into(), ".env".into()]);
        assert_eq!(i, Ignored { shown: vec![".env".into(), "target/".into(), "web/.env.local".into()], total: 3 });
        let many = Ignored::from_paths((0..MAX_IGNORED_SHOWN + 5).map(|n| format!("f{n:03}")).collect());
        assert_eq!((many.shown.len(), many.total), (MAX_IGNORED_SHOWN, MAX_IGNORED_SHOWN + 5));
    }

    #[test]
    fn nested_worktrees_are_found_by_whole_components() {
        let at = |p: &str| Worktree { main: false, ..worktree(p) };
        let all = [worktree("/r"), at("/r-a"), at("/r-a/.claude/worktrees/b"), at("/r-ab")];
        assert!(holds_worktree(&all[1], &all), "b is inside a");
        assert!(!holds_worktree(&all[0], &all), "siblings next to it are not inside");
        assert!(!holds_worktree(&all[2], &all), "not itself");
        assert!(!holds_worktree(&all[3], &all), "/r-ab is not inside /r-a, and holds nothing");
        assert!(!holds_worktree(&at("/r-a/"), &[at("/r-a")]), "itself with a trailing slash");
        assert!(path_inside("/a/b", "/a/b/c") && path_inside("/a/b/", "/a/b") && !path_inside("/a/b", "/a/bc") && !path_inside("/a/b/c", "/a/b"));
    }

    #[test]
    fn unknown_events_fail_to_decode_and_old_listings_still_decode() {
        // An app built before `Worktrees` sees an unknown variant: `decode` returns an error for
        // that line (the app logs it and reads the next one); it never mistakes it for another event.
        let line = encode(&AgentEvent::Worktrees(WorktreeList { key: "p".into(), repo: None, default_branch: None, worktrees: vec![], merged_branches: vec![] }));
        #[derive(Deserialize, Debug)]
        #[serde(tag = "type")]
        #[allow(dead_code)]
        enum OldEvent {
            Gone { session: String },
        }
        assert!(decode::<OldEvent>(&line).is_err());
        // An older agent's listing has no `worktree` flags.
        let old = r#"{"type":"DirListing","request":"~","path":"/home/me","parent":"/home","git":false,"dirs":[{"name":"api","git":true}],"error":null}"#;
        let AgentEvent::DirListing(l) = decode::<AgentEvent>(old).unwrap() else { panic!("a listing") };
        assert!(!l.worktree && !l.dirs[0].worktree && l.dirs[0].git);
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
            let ev = AgentEvent::Hello { version: "1".into(), hooks, send_text: true };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
            let ev = AgentEvent::HooksChanged { hooks };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        }
        let ev = AgentEvent::Error { problem: Problem::HooksFailed { detail: "read-only".into() } };
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
    }

    #[test]
    fn roundtrip_send_text() {
        let cmd = AgentCmd::SendText { id: 7, session: "tc_x".into(), text: "a\nb \x1b[201~ café".into(), enter: true, limit_ms: Some(8000) };
        let line = encode(&cmd);
        assert!(!line[..line.len() - 1].contains('\n'), "one line, whatever the text");
        assert_eq!(decode::<AgentCmd>(&line).unwrap(), cmd);
        let problems = [
            None,
            Some(Problem::SessionNotFound { session: "tc_x".into() }),
            Some(Problem::SendFailed { detail: "no space".into() }),
            Some(Problem::PaneDead { session: "tc_x".into() }),
            Some(Problem::PaneBusy { session: "tc_x".into(), mode: "clock-mode".into() }),
            Some(Problem::Expired),
        ];
        for problem in problems {
            for pasted in [false, true] {
                let ev = AgentEvent::TextSent { id: 7, session: "tc_x".into(), problem: problem.clone(), pasted };
                assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
            }
        }
        // Without a time limit (an older app), and an older answer without `pasted`.
        let old = r#"{"type":"SendText","id":1,"session":"tc_x","text":"x","enter":false}"#;
        assert!(matches!(decode::<AgentCmd>(old).unwrap(), AgentCmd::SendText { limit_ms: None, .. }));
        let old = r#"{"type":"TextSent","id":1,"session":"tc_x","problem":null}"#;
        assert!(matches!(decode::<AgentEvent>(old).unwrap(), AgentEvent::TextSent { pasted: false, .. }));
        // An older agent's hello: it cannot send text.
        let old = r#"{"type":"Hello","version":"0.1.7","hooks":"Installed"}"#;
        assert!(matches!(decode::<AgentEvent>(old).unwrap(), AgentEvent::Hello { send_text: false, .. }));
    }
}
