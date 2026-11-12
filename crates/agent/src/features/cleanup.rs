//! Cleanup of a watched folder's repository: remove a linked worktree (and its merged branch),
//! prune the entries whose folder is missing, delete a merged branch that has no worktree, and
//! measure worktree folders (with what git ignores in them).
//!
//! The app shows what is safe, but the agent never trusts it and refuses whenever it cannot tell.
//! Right before each write it checks again on fresh data (see `remove_checks`, `prune_checks` and
//! `branch_reason` for the order). The only git commands that write are `git worktree remove
//! <path>` (never `--force`), `git branch -d` (never `-D`) and `git worktree prune`. They run with
//! git's normal locking (not `GIT_OPTIONAL_LOCKS=0`, which only suits the read-only scans), with
//! no terminal prompt, in the C locale, from the main checkout.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use protocol::{BranchKeepReason, Cleanup, Ignored, KeepReason, Problem, Usage, Worktree, WorktreeSize, keep_reason, path_inside};

use super::changes::git_ok;
use super::worktrees::{self, Repo};

/// A walk of a folder stops after this many entries or this long: the result is then unknown.
pub const WALK_MAX_ENTRIES: usize = 2_000_000;
pub const WALK_MAX_TIME: Duration = Duration::from_secs(60);

/// What uses folders right now, besides git's own view. Read as late as possible: `run` asks
/// for it right before the write command.
#[derive(Default)]
pub struct InUse {
    /// Current folder of every pane on the canvas tmux server (`None`: tmux could not be asked).
    pub panes: Option<Vec<String>>,
    /// Current folder of every process of the user (`None`: `/proc` could not be read).
    pub processes: Option<Vec<String>>,
    /// Folders the app watches (the groups' folders), resolved.
    pub watched: Vec<String>,
}

/// Runs one action on the repository holding `dir`. `in_use` is called right before a removal.
/// `Ok`: the branch deleted along with a worktree, if any.
pub fn run(dir: &Path, action: &Cleanup, in_use: &dyn Fn() -> InUse) -> Result<Option<String>, Problem> {
    let repo = worktrees::scan_fresh(dir).ok_or(Problem::NotAGitRepo)?;
    let at = main_dir(&repo, dir);
    match action {
        Cleanup::RemoveWorktree { path, delete_branch, ignored } => {
            let wt = remove_checks(&repo, path, delete_branch.as_deref(), ignored, in_use)?;
            // Nothing between the last checks and the removal.
            git_write(&at, &["worktree", "remove", "--", &wt.path])?;
            match delete_branch {
                Some(branch) => match git_write(&at, &["branch", "-d", "--", branch]) {
                    Ok(()) => Ok(Some(branch.clone())),
                    Err(Problem::GitFailed { detail }) => Err(Problem::BranchNotDeleted { branch: branch.clone(), detail }),
                    Err(other) => Err(other),
                },
                None => Ok(None),
            }
        }
        Cleanup::Prune { paths } => {
            // The list git gives right now, not the one from the start of this action.
            let listed = git_ok(&at, &["worktree", "list", "--porcelain"]).map(|out| worktrees::parse_porcelain(&out)).ok_or(Problem::NotAGitRepo)?;
            prune_check(&listed, paths)?;
            git_write(&at, &["worktree", "prune"]).map(|()| None)
        }
        Cleanup::DeleteBranch { branch } => {
            if let Some(reason) = branch_reason(&repo, &at, branch) {
                return Err(Problem::BranchKept { branch: branch.clone(), reason });
            }
            git_write(&at, &["branch", "-d", "--", branch]).map(|()| None)
        }
    }
}

/// Prune only what git reports prunable now (a missing folder, not locked), and only if the
/// user confirmed every one of those entries.
pub fn prune_check(worktrees: &[Worktree], confirmed: &[String]) -> Result<(), Problem> {
    let prunable: Vec<&Worktree> = worktrees.iter().filter(|w| w.prunable).collect();
    if prunable.is_empty() {
        return Err(Problem::NothingToPrune);
    }
    match prunable.iter().find(|w| !confirmed.contains(&w.path)) {
        Some(w) => Err(Problem::PruneNotConfirmed { path: w.path.clone() }),
        None => Ok(()),
    }
}

/// The checks before `git worktree remove`, in this order; the first that fails refuses:
/// 1. the path is one git lists for this repository, compared as is;
/// 2. its state from the fresh scan (main, bare, missing, locked, merged, reflog commits) and a
///    full `git status` (untracked files included);
/// 3. no tracked file flagged `assume-unchanged` or `skip-worktree`;
/// 4. its folder resolves (symlinks, `..`): every later comparison uses the resolved path;
/// 5. no other worktree inside it (by git's paths and by resolved paths);
/// 6. the branch to delete is still its branch, and `git branch -d` will accept it;
/// 7. (slow) no git repository among its ignored files, on this file system only;
/// 8. its ignored files are exactly the ones the user saw;
/// 9. (last, right before the removal) no pane, no process, no watched folder inside it, and a
///    full `git status` again: git's own check before removing does not list untracked files
///    when `status.showUntrackedFiles=no`.
fn remove_checks(repo: &Repo, path: &str, delete_branch: Option<&str>, seen: &Ignored, in_use: &dyn Fn() -> InUse) -> Result<Worktree, Problem> {
    let mut wt = repo.worktrees.iter().find(|w| w.path == path).cloned().ok_or_else(|| Problem::NotAWorktree { path: path.to_string() })?;
    let kept = |wt: &Worktree, reason| Err(Problem::WorktreeKept { path: wt.path.clone(), reason });
    let root = PathBuf::from(&wt.path);
    let checkable = !wt.main && !wt.bare && !wt.prunable;
    if checkable {
        wt.dirty = dirty_now(&root);
    }
    let mut usage = Usage::default();
    if checkable {
        match hidden_changes(&root) {
            Some(hidden) => usage.hidden_changes = hidden,
            None => usage.unknown = true,
        }
    }
    let real = if checkable { real_path(&wt.path) } else { None };
    if checkable && real.is_none() {
        usage.unknown = true;
    }
    usage.holds_worktree = holds_worktree_resolved(&wt, real.as_deref(), &repo.worktrees);
    if let Some(reason) = keep_reason(&wt, &usage) {
        return kept(&wt, reason);
    }
    let real = real.expect("checked above");
    if let Some(expected) = delete_branch {
        if wt.branch.as_deref() != Some(expected) {
            return Err(Problem::BranchChanged { path: wt.path, expected: expected.to_string(), found: wt.branch });
        }
        // What `git branch -d` checks, before anything is removed: no half done removal.
        if !wt.branch_deletable {
            return Err(Problem::BranchKept { branch: expected.to_string(), reason: BranchKeepReason::NotMergedLocally });
        }
    }
    match holds_repository(Path::new(&real)) {
        Some(false) => {}
        Some(true) => return kept(&wt, KeepReason::HoldsRepository),
        None => return kept(&wt, KeepReason::NotChecked),
    }
    match ignored_listing(Path::new(&real)) {
        Some(now) if &now == seen => {}
        Some(_) => return Err(Problem::IgnoredChanged { path: wt.path }),
        None => return kept(&wt, KeepReason::NotChecked),
    }
    // Last: what uses it now, and its changes now.
    wt.dirty = dirty_now(&root);
    match hidden_changes(&root) {
        Some(hidden) => usage.hidden_changes = hidden,
        None => usage.unknown = true,
    }
    let now = in_use();
    let found = usage_at(&real, &now);
    usage = Usage { terminal: found.terminal, process: found.process, group_folder: found.group_folder, unknown: usage.unknown || found.unknown, ..usage };
    match keep_reason(&wt, &usage) {
        Some(reason) => kept(&wt, reason),
        None => Ok(wt),
    }
}

/// The resolved folder (symlinks and `..` followed), as the kernel reports pane and process
/// folders. `None` when it cannot be resolved.
pub fn real_path(path: &str) -> Option<String> {
    fs::canonicalize(path).ok().map(|p| p.to_string_lossy().into_owned())
}

/// Another listed worktree is inside this one, by git's paths or by resolved ones.
fn holds_worktree_resolved(wt: &Worktree, real: Option<&str>, all: &[Worktree]) -> bool {
    protocol::holds_worktree(wt, all)
        || real.is_some_and(|real| {
            all.iter().filter(|w| w.path != wt.path).filter_map(|w| real_path(&w.path)).any(|other| other.trim_end_matches('/') != real.trim_end_matches('/') && path_inside(real, &other))
        })
}

/// Panes, processes and watched folders inside the resolved folder `real`.
pub fn usage_at(real: &str, in_use: &InUse) -> Usage {
    let any_inside = |paths: &[String]| paths.iter().any(|p| path_inside(real, p));
    Usage {
        terminal: in_use.panes.as_deref().is_some_and(any_inside),
        process: in_use.processes.as_deref().is_some_and(any_inside),
        group_folder: any_inside(&in_use.watched),
        unknown: in_use.panes.is_none() || in_use.processes.is_none(),
        ..Usage::default()
    }
}

/// Why a branch without a worktree is not deleted (`None`: it is in the merged list, which
/// already excludes the default branch, checked out branches, branches that never moved, branches
/// `git branch -d` would refuse and branches whose reflog holds commits the base does not have).
fn branch_reason(repo: &Repo, at: &Path, branch: &str) -> Option<BranchKeepReason> {
    let refs = worktrees::read_refs(at);
    let found = refs.branches.iter().find(|(name, ..)| name == branch);
    let blocked_locally = || {
        let (Some((_, head, upstream)), Some(base)) = (found, &repo.base) else { return false };
        let main_head = worktrees::main_head(at, &repo.worktrees);
        let target = worktrees::deletion_target(upstream.as_deref(), &refs, main_head.as_deref());
        is_ancestor(at, head, &base.sha) && !target.is_some_and(|t| is_ancestor(at, head, t))
    };
    branch_keep_reason(repo, branch, found.is_some(), blocked_locally)
}

pub fn branch_keep_reason(repo: &Repo, branch: &str, exists: bool, blocked_locally: impl FnOnce() -> bool) -> Option<BranchKeepReason> {
    if repo.merged_branches.iter().any(|b| b.name == branch) {
        return None;
    }
    let reason = if repo.base.as_ref().is_some_and(|b| b.local() == branch) {
        BranchKeepReason::Default
    } else if repo.worktrees.iter().any(|w| w.branch.as_deref() == Some(branch)) {
        BranchKeepReason::CheckedOut
    } else if !exists {
        BranchKeepReason::NotFound
    } else if repo.reflog_held.iter().any(|b| b == branch) {
        BranchKeepReason::ReflogCommits
    } else if blocked_locally() {
        BranchKeepReason::NotMergedLocally
    } else {
        BranchKeepReason::NotMerged
    };
    Some(reason)
}

fn is_ancestor(at: &Path, commit: &str, of: &str) -> bool {
    git_ok(at, &["merge-base", "--is-ancestor", commit, of]).is_some()
}

/// Where the writing commands run: the main checkout (its HEAD is what `git branch -d` checks a
/// branch without upstream against), or the watched folder if that one is gone.
fn main_dir(repo: &Repo, dir: &Path) -> PathBuf {
    repo.worktrees.first().map(|w| PathBuf::from(&w.path)).filter(|p| p.is_dir()).unwrap_or_else(|| dir.to_path_buf())
}

/// A full status: modified, staged and untracked files (each untracked file, not just its
/// folder, whatever `status.showUntrackedFiles` says). Ignored files do not count. `None` when
/// git fails.
fn dirty_now(path: &Path) -> Option<bool> {
    git_ok(path, &["status", "--porcelain", "-z", "--untracked-files=all", "--ignore-submodules=none"]).map(|out| !out.is_empty())
}

/// Tracked files flagged `assume-unchanged` or `skip-worktree`: `git status` does not look at
/// them, so their local edits are invisible. `None` when git fails.
fn hidden_changes(path: &Path) -> Option<bool> {
    git_ok(path, &["ls-files", "-v", "-z"]).map(|out| has_hidden_flags(&out))
}

/// `git ls-files -v -z`: `<tag> <path>\0`; a lowercase tag is `assume-unchanged`, `S` is
/// `skip-worktree`.
pub fn has_hidden_flags(out: &str) -> bool {
    out.split('\0').filter_map(|e| e.chars().next()).any(|tag| tag.is_ascii_lowercase() || tag == 'S')
}

/// What git ignores in a worktree, as listed for the user. `None` when git fails.
pub fn ignored_listing(root: &Path) -> Option<Ignored> {
    let out = git_ok(root, &["status", "--porcelain=v1", "-z", "--ignored=traditional", "--untracked-files=normal"])?;
    Some(Ignored::from_paths(parse_ignored(&out)))
}

/// `git status --porcelain=v1 -z --ignored`: the `!! path` entries.
pub fn parse_ignored(out: &str) -> Vec<String> {
    out.split('\0').filter_map(|e| e.strip_prefix("!! ")).filter(|p| !p.is_empty()).map(String::from).collect()
}

/// Whether a git repository (a `.git` folder or file) sits among the worktree's ignored files:
/// `git worktree remove` would delete it, and its work, with the ignored files. Untracked ones
/// already make the worktree dirty. `None`: unknown (git failed, the walk was cut short, or it
/// met another file system, which git's removal would delete too but this walk does not read).
pub fn holds_repository(root: &Path) -> Option<bool> {
    let mut walk = Walk::new(root, Devices::Unknown)?;
    let out = git_ok(root, &["status", "--porcelain=v1", "-z", "--ignored=traditional", "--untracked-files=normal"])?;
    for rel in parse_ignored(&out) {
        let path = root.join(rel.trim_end_matches('/'));
        if path.file_name().is_some_and(|n| n == ".git") {
            return Some(true);
        }
        // An ignored symbolic link (one a worktree recipe made, to a folder of the main
        // checkout): git's removal deletes the link only, so what it points to is not walked.
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            continue;
        }
        let mut found = false;
        walk.each(&path, |entry, _| {
            if entry.file_name() == ".git" {
                found = true;
            }
            !found
        })?;
        if found {
            return Some(true);
        }
    }
    Some(false)
}

/// What a walk does with an entry on another file system than its root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Devices {
    /// Leave it out (sizes, like `du -x`).
    Skip,
    /// The walk's result becomes unknown (safety checks: git's removal would cross it).
    Unknown,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Visit,
    Skip,
    GiveUp,
}

/// The walk's decision for an entry, from whether it is on the root's file system.
pub fn device_step(same_device: bool, devices: Devices) -> Step {
    match (same_device, devices) {
        (true, _) => Step::Visit,
        (false, Devices::Skip) => Step::Skip,
        (false, Devices::Unknown) => Step::GiveUp,
    }
}

/// A bounded walk: symlinks not followed.
struct Walk {
    dev: u64,
    devices: Devices,
    seen: usize,
    started: Instant,
}

impl Walk {
    fn new(root: &Path, devices: Devices) -> Option<Self> {
        let meta = fs::symlink_metadata(root).ok().filter(|m| m.is_dir())?;
        Some(Walk { dev: meta.dev(), devices, seen: 0, started: Instant::now() })
    }

    /// Calls `visit` for every entry below `dir` (`dir` itself excluded) until it returns
    /// `false`. `None` when the walk went over its bounds, or met another file system while
    /// `Devices::Unknown`.
    fn each(&mut self, dir: &Path, mut visit: impl FnMut(&fs::DirEntry, &fs::Metadata) -> bool) -> Option<()> {
        let mut stack = vec![dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                self.seen += 1;
                if self.seen > WALK_MAX_ENTRIES || (self.seen.is_multiple_of(4096) && self.started.elapsed() > WALK_MAX_TIME) {
                    return None;
                }
                // `DirEntry::metadata` does not follow symlinks.
                let Ok(m) = entry.metadata() else { continue };
                match device_step(m.dev() == self.dev, self.devices) {
                    Step::Visit => {}
                    Step::Skip => continue,
                    Step::GiveUp => return None,
                }
                if !visit(&entry, &m) {
                    return Some(());
                }
                if m.is_dir() {
                    stack.push(entry.path());
                }
            }
        }
        Some(())
    }
}

/// Disk space used by a folder, like `du -sx`: allocated blocks, symlinks not followed, other
/// mounted file systems not entered, hard links counted once. `None` if `root` is not a folder or
/// the walk went over its bounds.
pub fn disk_usage(root: &Path) -> Option<u64> {
    let mut walk = Walk::new(root, Devices::Skip)?;
    let mut total = fs::symlink_metadata(root).ok()?.blocks() * 512;
    let mut linked: HashSet<u64> = HashSet::new();
    walk.each(root, |_, m| {
        if m.is_dir() || m.nlink() <= 1 || linked.insert(m.ino()) {
            total += m.blocks() * 512;
        }
        true
    })?;
    Some(total)
}

/// Size and ignored files of one worktree of the repository holding `dir`. A path git does not
/// list (or a bare or missing entry) gets nothing: nothing else is walked.
pub fn measure(dir: &Path, path: &str) -> WorktreeSize {
    let listed = git_ok(dir, &["worktree", "list", "--porcelain"]).map(|out| worktrees::parse_porcelain(&out)).unwrap_or_default();
    let mut size = WorktreeSize { path: path.to_string(), bytes: None, ignored: None };
    if listed.iter().any(|w| w.path == path && !w.bare && !w.prunable) {
        size.bytes = disk_usage(Path::new(path));
        size.ignored = ignored_listing(Path::new(path));
    }
    size
}

/// How long a git command that writes is waited for (`TCANVAS_WRITE_LIMIT_SECS` overrides it,
/// for tests). Below the app's own wait for an answer.
pub fn write_limit() -> Duration {
    let secs = std::env::var("TCANVAS_WRITE_LIMIT_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
    Duration::from_secs(secs)
}

/// Most of git's message kept as the problem's detail.
const MAX_DETAIL: u64 = 16 * 1024;

/// What became of a command waited for within a time limit.
pub enum Written {
    /// It ended: `Ok`, or its message (stderr) as the problem.
    Done(Result<(), Problem>),
    /// Still running at the limit: it goes on; `Running::finish` waits for its end.
    Running(Running),
}

/// A command still running after its time limit.
pub struct Running {
    child: Child,
    err: PathBuf,
}

impl Running {
    /// Waits for its end (blocking) and gives its result.
    pub fn finish(mut self) -> Result<(), Problem> {
        let status = self.child.wait();
        ended(status, &self.err)
    }
}

/// A git command that writes: normal locking, no prompt, messages in English (the C locale),
/// git's message as the problem. Waited for `write_limit()` at most: past that it goes on in the
/// background (reaped there) and the answer is `GitStillRunning`. Stopping `git worktree remove`
/// half-way would leave a half-deleted folder, and `git worktree add` a half checked out one
/// with git's "initializing" lock: letting it finish leaves git's own end state.
pub fn git_write(dir: &Path, args: &[&str]) -> Result<(), Problem> {
    let limit = write_limit();
    match git_write_within(dir, args, limit) {
        Written::Done(result) => result,
        Written::Running(running) => {
            std::thread::spawn(move || running.finish());
            Err(Problem::GitStillRunning { secs: limit.as_secs() })
        }
    }
}

pub fn git_write_within(dir: &Path, args: &[&str], limit: Duration) -> Written {
    let mut cmd = super::changes::git_command(dir);
    cmd.args(args).env("GIT_TERMINAL_PROMPT", "0").env("LC_ALL", "C");
    run_within(cmd, limit)
}

/// Runs `cmd` with no input, its output thrown away, and its errors in a temporary file rather
/// than a pipe: a process it leaves behind (a hook's background job) may keep the file open, it
/// never keeps anyone waiting for the end of a pipe. Waits for the command itself, `limit` at
/// most.
pub fn run_within(mut cmd: Command, limit: Duration) -> Written {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let failed = |e: std::io::Error| Written::Done(Err(Problem::GitFailed { detail: e.to_string() }));
    let err = std::env::temp_dir().join(format!("tcanvas-agent-{}-{}.err", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let file = match fs::OpenOptions::new().write(true).create_new(true).open(&err) {
        Ok(file) => file,
        Err(e) => return failed(e),
    };
    let child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(file)).spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            let _ = fs::remove_file(&err);
            return failed(e);
        }
    };
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Written::Done(ended(Ok(status), &err)),
            Ok(None) if Instant::now() >= deadline => return Written::Running(Running { child, err }),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Written::Done(ended(Err(e), &err)),
        }
    }
}

/// The result of an ended command whose errors went to `err` (removed here).
fn ended(status: std::io::Result<std::process::ExitStatus>, err: &Path) -> Result<(), Problem> {
    use std::io::Read;
    let mut text = Vec::new();
    if let Ok(file) = fs::File::open(err) {
        let _ = file.take(MAX_DETAIL).read_to_end(&mut text);
    }
    let _ = fs::remove_file(err);
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(Problem::GitFailed { detail: String::from_utf8_lossy(&text).trim().to_string() }),
        Err(e) => Err(Problem::GitFailed { detail: e.to_string() }),
    }
}

/// Current folder of every process of this user but the agent itself (processes whose folder
/// cannot be read are skipped). `None` when `/proc` itself cannot be read.
pub fn process_cwds() -> Option<Vec<String>> {
    process_cwds_in(Path::new("/proc"))
}

pub fn process_cwds_in(proc: &Path) -> Option<Vec<String>> {
    let me = std::process::id().to_string();
    let uid = fs::metadata(proc.join("self")).ok()?.uid();
    let entries = fs::read_dir(proc).ok()?;
    Some(
        entries
            .flatten()
            .filter(|e| e.file_name().to_str().is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()) && n != me))
            .filter(|e| e.metadata().is_ok_and(|m| m.uid() == uid))
            .filter_map(|e| fs::read_link(e.path().join("cwd")).ok())
            .map(|p| cwd_text(&p.to_string_lossy()))
            .collect(),
    )
}

/// A `/proc/<pid>/cwd` target: the kernel appends ` (deleted)` to a folder that is gone.
pub fn cwd_text(link: &str) -> String {
    link.strip_suffix(" (deleted)").unwrap_or(link).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::MergedBranch;

    fn wt(path: &str, branch: &str, main: bool) -> Worktree {
        Worktree {
            path: path.into(),
            head: Some("a".repeat(40)),
            branch: Some(branch.into()),
            detached: false,
            main,
            bare: false,
            locked: false,
            lock_reason: None,
            prunable: false,
            dirty: None,
            ahead: Some(0),
            behind: Some(0),
            merged: Some(!main),
            last_commit: None,
            branch_deletable: true,
            reflog_commits: false,
        }
    }

    fn repo() -> Repo {
        Repo {
            base: Some(worktrees::Base { name: "origin/main".into(), sha: "b".repeat(40) }),
            worktrees: vec![wt("/r", "main", true), wt("/r-x", "x", false)],
            merged_branches: vec![MergedBranch { name: "old".into(), last_commit: None }],
            reflog_held: vec!["rewound".into()],
        }
    }

    #[test]
    fn branches_are_kept_for_a_reason() {
        let r = repo();
        assert_eq!(branch_keep_reason(&r, "old", true, || true), None);
        assert_eq!(branch_keep_reason(&r, "main", true, || false), Some(BranchKeepReason::Default));
        assert_eq!(branch_keep_reason(&r, "x", true, || false), Some(BranchKeepReason::CheckedOut));
        assert_eq!(branch_keep_reason(&r, "wip", true, || false), Some(BranchKeepReason::NotMerged));
        assert_eq!(branch_keep_reason(&r, "behind", true, || true), Some(BranchKeepReason::NotMergedLocally));
        assert_eq!(branch_keep_reason(&r, "rewound", true, || false), Some(BranchKeepReason::ReflogCommits));
        assert_eq!(branch_keep_reason(&r, "gone", false, || true), Some(BranchKeepReason::NotFound));
    }

    #[test]
    fn what_uses_a_worktree_and_what_could_not_be_told() {
        let none = InUse { panes: Some(vec![]), processes: Some(vec![]), watched: vec![] };
        assert_eq!(usage_at("/r-y", &none), Usage::default());
        let pane = |p: &str| InUse { panes: Some(vec!["/home".into(), p.into()]), ..InUse { processes: Some(vec![]), ..InUse::default() } };
        assert!(usage_at("/r-y", &pane("/r-y/src")).terminal);
        assert!(usage_at("/r-y", &pane("/r-y")).terminal);
        assert!(!usage_at("/r-y", &pane("/r-yz")).terminal, "another folder with the same prefix");
        assert!(!usage_at("/r-y", &pane("/r")).terminal, "the main checkout around it");
        let process = InUse { panes: Some(vec![]), processes: Some(vec!["/tmp".into(), "/r-y/target".into()]), watched: vec![] };
        assert!(usage_at("/r-y", &process).process);
        let watched = InUse { panes: Some(vec![]), processes: Some(vec![]), watched: vec!["/r".into(), "/r-y/web".into()] };
        assert!(usage_at("/r-y", &watched).group_folder);
        // tmux could not be asked, or `/proc` could not be read: never "nobody".
        assert!(usage_at("/r-y", &InUse { processes: Some(vec![]), ..InUse::default() }).unknown);
        assert!(usage_at("/r-y", &InUse { panes: Some(vec![]), ..InUse::default() }).unknown);
        assert_eq!(process_cwds_in(Path::new("/nonexistent/proc")), None);
    }

    #[test]
    fn paths_are_compared_resolved() {
        // `/link` -> `/real`: git stored the worktree under the link, the kernel reports panes and
        // processes under the real folder.
        let tmp = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("tcanvas-real-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("real/wt/src")).unwrap();
        std::os::unix::fs::symlink(tmp.join("real"), tmp.join("link")).unwrap();
        let stored = tmp.join("link/wt").display().to_string();
        let real = real_path(&stored).unwrap();
        assert_eq!(real, tmp.join("real/wt").display().to_string());
        let process = InUse { panes: Some(vec![]), processes: Some(vec![tmp.join("real/wt/src").display().to_string()]), watched: vec![] };
        assert!(!path_inside(&stored, &process.processes.as_ref().unwrap()[0]), "missed by the stored path");
        assert!(usage_at(&real, &process).process, "seen by the resolved one");
        assert_eq!(real_path(&tmp.join("link/missing").display().to_string()), None);
        // Another worktree reached through the link is still inside.
        let me = Worktree { dirty: Some(false), ..wt(&tmp.join("real/wt").display().to_string(), "w", false) };
        let inner = wt(&tmp.join("link/wt/src").display().to_string(), "i", false);
        assert!(holds_worktree_resolved(&me, Some(&real), &[me.clone(), inner]));
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn only_listed_linked_worktrees_with_checked_changes_pass() {
        let r = Repo { worktrees: vec![wt("/r", "main", true), Worktree { dirty: Some(false), ..wt("/nonexistent/r-x", "x", false) }], ..repo() };
        let none = || InUse { panes: Some(vec![]), processes: Some(vec![]), watched: vec![] };
        let seen = Ignored::default();
        // The folder does not exist: `git status` fails, so its changes are unknown.
        let err = remove_checks(&r, "/nonexistent/r-x", None, &seen, &none).unwrap_err();
        assert_eq!(err, Problem::WorktreeKept { path: "/nonexistent/r-x".into(), reason: KeepReason::NotChecked });
        assert_eq!(remove_checks(&r, "/etc", None, &seen, &none).unwrap_err(), Problem::NotAWorktree { path: "/etc".into() });
        assert_eq!(remove_checks(&r, "/nonexistent/r-x/", None, &seen, &none).unwrap_err(), Problem::NotAWorktree { path: "/nonexistent/r-x/".into() }, "exact paths only");
        assert_eq!(remove_checks(&r, "/r", None, &seen, &none).unwrap_err(), Problem::WorktreeKept { path: "/r".into(), reason: KeepReason::MainCheckout });
    }

    #[test]
    fn prune_takes_only_what_was_confirmed() {
        let gone = |p: &str| Worktree { prunable: true, ..wt(p, "g", false) };
        let list = [wt("/r", "main", true), gone("/a"), gone("/b")];
        assert_eq!(prune_check(&list, &["/a".into(), "/b".into(), "/c".into()]), Ok(()));
        assert_eq!(prune_check(&list, &["/a".into()]), Err(Problem::PruneNotConfirmed { path: "/b".into() }));
        assert_eq!(prune_check(&list[..1], &["/a".into()]), Err(Problem::NothingToPrune));
    }

    #[test]
    fn ignored_entries_are_listed_whole() {
        let out = "!! target/\0!! .env\0?? new.txt\0!! web/node_modules/\0!! web/.env.local\0!! .claude/\0";
        let ignored = parse_ignored(out);
        assert_eq!(ignored, vec!["target/", ".env", "web/node_modules/", "web/.env.local", ".claude/"]);
        assert_eq!(Ignored::from_paths(ignored).shown, vec![".claude/", ".env", "target/", "web/.env.local", "web/node_modules/"]);
    }

    #[test]
    fn flags_that_hide_changes_from_git_status() {
        assert!(!has_hidden_flags("H a.rs\0H b/c.rs\0"));
        assert!(has_hidden_flags("H a.rs\0h config.toml\0"), "assume-unchanged");
        assert!(has_hidden_flags("S sparse/x\0H a.rs\0"), "skip-worktree");
        assert!(has_hidden_flags("s both\0"), "both");
        assert!(!has_hidden_flags(""));
    }

    #[test]
    fn another_file_system_makes_a_safety_walk_unknown() {
        assert_eq!(device_step(true, Devices::Unknown), Step::Visit);
        assert_eq!(device_step(true, Devices::Skip), Step::Visit);
        assert_eq!(device_step(false, Devices::Skip), Step::Skip, "sizes: like du -x");
        assert_eq!(device_step(false, Devices::Unknown), Step::GiveUp, "git's removal would cross it");
    }

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd
    }

    #[test]
    fn a_background_job_holding_stderr_does_not_keep_anyone_waiting() {
        // Like a hook that starts a job in the background and ends: the job keeps stderr open.
        let start = Instant::now();
        let Written::Done(result) = run_within(sh("(sleep 5 >&2 &); echo 'fatal: refused' >&2; exit 3"), Duration::from_secs(4)) else { panic!("it ended") };
        assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
        assert_eq!(result, Err(Problem::GitFailed { detail: "fatal: refused".into() }), "its message, read from the file");
        let Written::Done(ok) = run_within(sh("(sleep 5 &); exit 0"), Duration::from_secs(4)) else { panic!("it ended") };
        assert_eq!(ok, Ok(()));
    }

    #[test]
    fn a_command_past_its_limit_goes_on_and_can_be_waited_for() {
        let marker = std::env::temp_dir().join(format!("tcanvas-limit-{}", std::process::id()));
        let _ = fs::remove_file(&marker);
        let start = Instant::now();
        let script = format!("sleep 1; echo late >&2; touch '{}'; exit 4", marker.display());
        let Written::Running(running) = run_within(sh(&script), Duration::from_millis(200)) else { panic!("still running at the limit") };
        assert!(start.elapsed() < Duration::from_millis(900));
        assert!(!marker.exists(), "not stopped, not finished yet");
        assert_eq!(running.finish(), Err(Problem::GitFailed { detail: "late".into() }), "its final result");
        assert!(marker.exists(), "it went on to the end");
        fs::remove_file(&marker).unwrap();
        let Written::Done(missing) = run_within(Command::new("/nonexistent/git"), Duration::from_secs(1)) else { panic!("never started") };
        assert!(matches!(missing, Err(Problem::GitFailed { .. })));
    }

    #[test]
    fn deleted_folders_and_processes() {
        assert_eq!(cwd_text("/home/me/x (deleted)"), "/home/me/x");
        assert_eq!(cwd_text("/home/me/x"), "/home/me/x");
        // A real child process sitting in a temporary folder.
        let dir = std::env::temp_dir().join(format!("tcanvas-cwd-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        let mut child = Command::new("sleep").arg("5").current_dir(&dir).spawn().unwrap();
        let cwds = process_cwds().unwrap();
        let _ = child.kill();
        let _ = child.wait();
        fs::remove_dir_all(&dir).unwrap();
        assert!(cwds.iter().any(|c| path_inside(&dir.to_string_lossy(), c)), "{cwds:?}");
    }

    #[test]
    fn disk_usage_skips_symlinks_and_counts_hard_links_once() {
        let root = std::env::temp_dir().join(format!("tcanvas-du-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/data"), vec![7u8; 64 * 1024]).unwrap();
        let base = disk_usage(&root).unwrap();
        assert!(base >= 64 * 1024, "{base}");
        // A symlink to a big folder elsewhere: its target is not counted.
        let elsewhere = std::env::temp_dir().join(format!("tcanvas-du-target-{}", std::process::id()));
        let _ = fs::remove_dir_all(&elsewhere);
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("big"), vec![1u8; 512 * 1024]).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
        // A hard link to the same data: no new blocks.
        fs::hard_link(root.join("a/b/data"), root.join("again")).unwrap();
        let after = disk_usage(&root).unwrap();
        assert!(after < base + 16 * 1024, "{base} -> {after}");
        assert_eq!(disk_usage(&root.join("a/b/data")), None, "not a folder");
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&elsewhere).unwrap();
    }
}
