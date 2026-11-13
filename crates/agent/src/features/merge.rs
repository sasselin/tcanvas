//! Merging a reviewed branch into its base, from the review (`AgentCmd::Merge`).
//!
//! The user's worktrees are never where a merge is attempted. The result is prepared aside, in a
//! private worktree of the agent (`git worktree add --detach`, under `~/.local/share/tcanvas/
//! merge/work/`, a name only the agent makes, 0700, hidden from every worktree list of the app),
//! detached at the reviewed base commit. Only once that result `R` exists, has the base as an
//! ancestor and holds exactly the tree reviewed, is the base's worktree B touched, by one
//! fast-forward: `git merge --ff-only R`. The branch's worktree W is never written.
//!
//! Commands (T: the private worktree, B: the base's worktree; `<head>`, `<base>`: the reviewed
//! commits, never names, so nothing landing meanwhile is taken). Every command in T runs with
//! `gc.auto=0` (no automatic gc of the shared repository from there):
//! - T is made by `git -C B worktree add --detach T <base>` (its `post-checkout` hook runs).
//! - merge: T `git merge --no-commit --no-ff <head>` and `git write-tree` (the tree the merge
//!   stages), T `git reset -q --hard <base>`, then T `git merge --no-ff -m "Merge branch
//!   '<branch>'" <head>`, whose tree must equal the staged one (a hook that changes the content
//!   is never taken).
//! - fast-forward: `R = <head>` when the base did not move since the branch started (no T);
//!   otherwise as merge.
//! - squash: T `git merge --squash <head>`, `git write-tree`, then T `git commit
//!   --cleanup=whitespace --file=<msg>`, whose tree must equal the staged one.
//! - rebase: `R = <head>` when the base did not move (no T); otherwise the merge's tree as above,
//!   then T `git -c rebase.backend=merge rebase --onto <base> <merge-base> <head>`; the rebased
//!   tree must equal the merge's. A branch holding merge commits is refused (a rebase drops them).
//! - apply, in B: B still on the base at `<base>`; no untracked or ignored file where R writes;
//!   no `index.lock` nor ref lock; a dry run (`git read-tree -n -m -u <base> R`) that passes; the
//!   record says it is applying (else nothing is applied); B's index, HEAD, branch and `ORIG_HEAD`
//!   recorded; B's branch and the files checked once more; then `git merge --ff-only R`. If it
//!   fails, `ORIG_HEAD` is put back and B compared with what was recorded: unchanged, or written
//!   half-way (index at R, branch at the base), or something else, each said as such.
//!
//! The windows left: a file created at a path R writes, or a switch of B's branch, in the few
//! milliseconds between the last check and git's own start, is not seen beforehand (`merge
//! --ff-only` overwrites an ignored file without a word); a switch is reported afterwards with
//! the branch that moved.
//!
//! Hooks run where git runs them: in T while preparing (`post-checkout`, `pre-merge-commit`,
//! `prepare-commit-msg`, `commit-msg`, `post-commit`, `post-merge`, `pre-commit` for a squash,
//! `pre-rebase`, `post-rewrite`, `reference-transaction`), from the repository's hooks folder or
//! `core.hooksPath`; in B at the end (`post-merge`, `reference-transaction`). Never `--force`
//! on a user's worktree, `-X`, `--no-verify` nor a push. Every command runs with
//! `user.useConfigOnly=true` (no identity is made up), autostash off, the editors set to `true`,
//! `rebase.updateRefs` off, in its own session with no terminal (`GIT_TERMINAL_PROMPT=0`, no
//! `GPG_TTY`), in the C locale, with the redirecting git variables removed.
//!
//! A conflict, a failing hook, a timeout while preparing: T is removed (`git worktree remove
//! --force T`, then its folder and its entry under `.git/worktrees/` if still there, never a
//! `git worktree prune`), nothing else was written. A record of the merge (its phase, T, the
//! agent's process, the git process applying it) is kept in the agent's data folder from before
//! T exists to the final outcome; at its next start an agent reports one left behind (still
//! running while its agent or its git is alive), removes the leftover T, and says from the refs
//! whether the result is in the base. Error and message files of agents that are gone are removed.

// The checks stop with the outcome to answer, once per merge: its size does not matter.
#![allow(clippy::result_large_err)]

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::{
    AgentCmd, AgentEvent, BaseKind, GitOperation, MAX_MERGE_CONFLICTS, MAX_MERGE_MESSAGE, MergeOutcome, MergePhase, MergeRecord, MergeRefusal, MergeStep, MergeStrategy, NotApplied, Problem,
    SessionInfo, TermStatus, path_inside,
};

use super::changes::{self, git_command};
use super::{cleanup, review, status};

/// The canvas sessions as the agent last saw them (shared by the main loop; `None` until its
/// first pass).
pub type Sessions = Arc<Mutex<Option<Vec<SessionInfo>>>>;

/// Settings every git command of a merge runs with.
const WRITE_CONFIG: [&str; 10] = ["-c", "user.useConfigOnly=true", "-c", "merge.autoStash=false", "-c", "rebase.autoStash=false", "-c", "rebase.updateRefs=false", "-c", "rebase.autoSquash=false"];

/// Under the home folder.
const MERGE_DIR: &str = ".local/share/tcanvas/merge";

/// Merges running now.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// The merge running now, for the exit paths.
struct Current {
    phase: MergePhase,
    /// The process group of the git command preparing the result.
    group: Option<i32>,
    temp: Option<PathBuf>,
    base_dir: PathBuf,
    record: Option<PathBuf>,
    /// The agent is exiting: the preparation is dropped, nothing is applied.
    abandoned: bool,
}

static CURRENT: Mutex<Option<Current>> = Mutex::new(None);

fn with_current<T>(f: impl FnOnce(&mut Option<Current>) -> T) -> T {
    let mut guard = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

struct Active;

impl Active {
    fn start() -> Self {
        ACTIVE.fetch_add(1, Ordering::SeqCst);
        Active
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        with_current(|c| *c = None);
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The agent exits: a merge still preparing its result is dropped (its git stopped, its private
/// worktree and record removed: nothing of the user's was touched); one applying it is waited
/// for (one fast-forward, `limit` at most).
pub fn shutdown(limit: Duration) {
    let preparing = with_current(|c| match c.as_mut() {
        Some(cur) if cur.phase == MergePhase::Preparing => {
            cur.abandoned = true;
            if let Some(group) = cur.group.take() {
                // SAFETY: a plain system call on the group of a child of ours.
                unsafe { libc::kill(-group, libc::SIGKILL) };
            }
            Some((cur.base_dir.clone(), cur.temp.clone(), cur.record.clone()))
        }
        _ => None,
    });
    if let Some((base_dir, temp, record)) = preparing {
        if let Some(temp) = temp {
            // Its git may still be dying: give it a moment to let go of its files.
            std::thread::sleep(Duration::from_millis(100));
            remove_temp(&base_dir, &temp);
        }
        if let Some(record) = record {
            let _ = std::fs::remove_file(record);
        }
        return;
    }
    let end = Instant::now() + limit;
    while ACTIVE.load(Ordering::SeqCst) > 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// How long one git command preparing a result may take (`TCANVAS_MERGE_CEILING_SECS` overrides
/// it, for tests): past it, it is stopped (it runs aside). The fast-forward is never stopped.
pub fn ceiling() -> Duration {
    let secs = std::env::var("TCANVAS_MERGE_CEILING_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(900);
    Duration::from_secs(secs)
}

/// A merge asked by the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    pub key: String,
    pub worktree: String,
    pub branch: String,
    pub base: String,
    pub strategy: MergeStrategy,
    pub message: Option<String>,
    pub branch_head: String,
    pub base_head: String,
}

impl Request {
    pub fn of(cmd: AgentCmd) -> Option<Request> {
        match cmd {
            AgentCmd::Merge { id, key, worktree, branch, base, strategy, message, branch_head, base_head } => {
                Some(Request { id, key, worktree, branch, base, strategy, message, branch_head, base_head })
            }
            _ => None,
        }
    }
}

/// The session of a coding agent working in the resolved folder `real` right now.
pub fn agent_working(sessions: &[SessionInfo], real: &str) -> Option<String> {
    sessions
        .iter()
        .filter(|s| s.status == TermStatus::Running && s.command.as_deref().is_some_and(status::is_coding_agent))
        .find(|s| s.cwd.as_deref().is_some_and(|cwd| path_inside(real, cwd) || cleanup::real_path(cwd).is_some_and(|c| path_inside(real, &c))))
        .map(|s| s.session.clone())
}

/// The commit message of a squash, checked.
pub fn squash_message(message: Option<&str>) -> Result<String, MergeRefusal> {
    let text = message.unwrap_or_default();
    if text.contains('\0') {
        return Err(MergeRefusal::MessageInvalid);
    }
    if text.trim().is_empty() {
        return Err(MergeRefusal::MessageEmpty);
    }
    if text.len() > MAX_MERGE_MESSAGE {
        return Err(MergeRefusal::MessageTooLong { max: MAX_MERGE_MESSAGE });
    }
    Ok(text.to_string())
}

/// The message of a merge commit, as git words its own.
pub fn merge_message(branch: &str, base: &str) -> String {
    if matches!(base, "main" | "master") { format!("Merge branch '{branch}'") } else { format!("Merge branch '{branch}' into {base}") }
}

/// What `git rev-parse --git-path` names for each operation in progress.
const IN_PROGRESS: [(&str, GitOperation); 7] = [
    ("MERGE_HEAD", GitOperation::Merge),
    ("rebase-merge", GitOperation::Rebase),
    ("rebase-apply", GitOperation::Rebase),
    ("CHERRY_PICK_HEAD", GitOperation::CherryPick),
    ("REVERT_HEAD", GitOperation::Revert),
    ("sequencer", GitOperation::Sequence),
    ("BISECT_LOG", GitOperation::Bisect),
];

/// The operation in progress in the worktree `dir` (`Err`: git could not tell).
pub fn operation(dir: &Path) -> Result<Option<GitOperation>, ()> {
    let mut args = vec!["rev-parse"];
    for (name, _) in IN_PROGRESS {
        args.extend(["--git-path", name]);
    }
    let out = changes::git_ok(dir, &args).ok_or(())?;
    let paths: Vec<&str> = out.lines().collect();
    if paths.len() != IN_PROGRESS.len() {
        return Err(());
    }
    Ok(IN_PROGRESS.iter().zip(paths).find(|(_, p)| dir.join(p).symlink_metadata().is_ok()).map(|((_, op), _)| *op))
}

/// The branch `dir` is on (`None`: detached, or git failed).
fn on_branch(dir: &Path) -> Option<String> {
    changes::git_ok(dir, &["symbolic-ref", "-q", "HEAD"]).map(|s| s.trim().to_string()).and_then(|s| s.strip_prefix("refs/heads/").map(String::from))
}

fn commit(dir: &Path, rev: &str) -> Option<String> {
    review::commit_of(dir, rev).ok().flatten()
}

/// Everything checked, ready to prepare.
struct Plan {
    id: u64,
    key: String,
    strategy: MergeStrategy,
    message: Option<String>,
    base_dir: PathBuf,
    base_shown: String,
    base: String,
    base_sha: String,
    branch_dir: PathBuf,
    branch: String,
    head: String,
    merge_base: String,
    behind: u64,
}

fn refused(reason: MergeRefusal) -> MergeOutcome {
    MergeOutcome::Refused { reason }
}

fn failed(problem: Problem) -> MergeOutcome {
    MergeOutcome::Failed { problem }
}

/// Merges as asked. `sessions`: the canvas sessions now (`None`: not known yet).
pub fn run(dir: &Path, request: &Request, sessions: &dyn Fn() -> Option<Vec<SessionInfo>>) -> MergeOutcome {
    let _active = Active::start();
    let plan = match check(dir, request, sessions) {
        Ok(plan) => plan,
        Err(outcome) => return outcome,
    };
    pause("checked");
    let record = match write_record(&plan) {
        Ok(record) => record,
        Err(e) => return failed(Problem::Unreadable { detail: e.to_string() }),
    };
    with_current(|c| *c = Some(Current { phase: MergePhase::Preparing, group: None, temp: None, base_dir: plan.base_dir.clone(), record: Some(record.clone()), abandoned: false }));
    let mut temp = None;
    let outcome = match prepare(&plan, &mut temp) {
        Ok(result) => {
            pause("prepared");
            apply(&plan, &result, &record)
        }
        Err(outcome) => outcome,
    };
    if let Some(temp) = temp {
        remove_temp(&plan.base_dir, &temp);
    }
    let _ = std::fs::remove_file(&record);
    outcome
}

/// Test only (`TCANVAS_MERGE_TEST_PAUSE`: a folder): at `point`, says so with a file and waits
/// for the test to let it go on (bounded).
fn pause(point: &str) {
    let Some(dir) = std::env::var_os("TCANVAS_MERGE_TEST_PAUSE").map(PathBuf::from) else { return };
    let _ = std::fs::write(dir.join(format!("{point}.reached")), "");
    let go = dir.join(format!("{point}.go"));
    let end = Instant::now() + Duration::from_secs(60);
    while !go.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn check(dir: &Path, r: &Request, sessions: &dyn Fn() -> Option<Vec<SessionInfo>>) -> Result<Plan, MergeOutcome> {
    let message = match r.strategy {
        MergeStrategy::Squash => Some(squash_message(r.message.as_deref()).map_err(refused)?),
        _ => None,
    };
    let wt = review::worktree_of(dir, &r.worktree).map_err(failed)?;
    let branch_dir = wt.path.clone();
    let Some(branch) = on_branch(&branch_dir) else { return Err(refused(MergeRefusal::Detached)) };
    if branch != r.branch {
        return Err(refused(MergeRefusal::BranchChanged { expected: r.branch.clone(), found: Some(branch) }));
    }
    if !review::base_name_ok(&r.base) {
        return Err(failed(Problem::ReviewBaseRefused { base: r.base.clone() }));
    }
    if r.base == branch {
        return Err(refused(MergeRefusal::BaseIsBranch));
    }
    let Some((base_sha, kind)) = review::resolve_base(&branch_dir, &r.base).map_err(failed)? else {
        return Err(refused(MergeRefusal::BaseNotFound { base: r.base.clone() }));
    };
    if kind != BaseKind::Branch {
        return Err(refused(MergeRefusal::BaseNotLocal { base: r.base.clone(), named: kind }));
    }
    let not_checked_out = || refused(MergeRefusal::BaseNotCheckedOut { base: r.base.clone() });
    let target = wt.list.iter().find(|w| w.branch.as_deref() == Some(r.base.as_str()) && !w.bare && !w.prunable).ok_or_else(not_checked_out)?;
    let base_dir = std::fs::canonicalize(&target.path).ok().filter(|d| d.is_dir()).ok_or_else(not_checked_out)?;
    if base_dir == branch_dir {
        return Err(refused(MergeRefusal::BaseIsBranch));
    }
    let base_shown = target.path.clone();
    if let Some(pid) = previous_agent(&base_dir, &branch_dir) {
        return Err(refused(MergeRefusal::PreviousAgent { pid }));
    }
    let head = commit(&branch_dir, "HEAD").ok_or_else(|| failed(Problem::ReviewNoCommits))?;
    let base_head = commit(&base_dir, "HEAD").unwrap_or_default();
    let branch_moved = head != r.branch_head;
    let base_moved = base_sha != r.base_head || base_head != base_sha;
    if branch_moved || base_moved {
        return Err(refused(MergeRefusal::Stale { branch_moved, base_moved }));
    }
    let merge_base = review::text(&branch_dir, &["merge-base", &base_sha, &head]).map_err(failed)?.map(|m| m.trim().to_string()).filter(|m| review::is_hex_sha(m));
    let Some(merge_base) = merge_base else { return Err(refused(MergeRefusal::Unrelated)) };
    let count = |args: &[&str]| review::text(&branch_dir, args).map(|o| o.and_then(|o| o.trim().parse::<u64>().ok()));
    if count(&["rev-list", "--count", &format!("{base_sha}..{head}")]).map_err(failed)?.unwrap_or(0) == 0 {
        return Err(refused(MergeRefusal::NotAhead));
    }
    let behind = count(&["rev-list", "--count", &format!("{head}..{base_sha}")]).map_err(failed)?.unwrap_or(1);
    if r.strategy == MergeStrategy::Rebase && behind > 0 && count(&["rev-list", "--merges", "--count", &format!("{base_sha}..{head}")]).map_err(failed)?.unwrap_or(1) > 0 {
        return Err(refused(MergeRefusal::RebaseMerges));
    }
    // The base's worktree is where the result lands at the end: its state must be readable.
    match operation(&base_dir) {
        Ok(None) => {}
        Ok(Some(operation)) => return Err(refused(MergeRefusal::InProgress { path: base_shown, operation })),
        Err(()) => return Err(refused(MergeRefusal::NotChecked { path: base_shown })),
    }
    if changes::git_ok(&base_dir, &["config", "--bool", "core.sparseCheckout"]).is_some_and(|v| v.trim() == "true") {
        return Err(refused(MergeRefusal::Sparse { path: base_shown }));
    }
    match cleanup::hidden_changes(&base_dir) {
        Some(false) => {}
        Some(true) => return Err(refused(MergeRefusal::HiddenChanges { path: base_shown })),
        None => return Err(refused(MergeRefusal::NotChecked { path: base_shown })),
    }
    let Some(now) = sessions() else { return Err(refused(MergeRefusal::SessionsUnknown)) };
    if let Some(session) = agent_working(&now, &base_dir.to_string_lossy()) {
        return Err(refused(MergeRefusal::AgentWorking { path: base_shown, session }));
    }
    // A commit is made aside, except when the result is the branch's head itself.
    let commits = !(matches!(r.strategy, MergeStrategy::FastForward | MergeStrategy::Rebase) && behind == 0);
    if commits && !identity(&base_dir) {
        return Err(refused(MergeRefusal::NoIdentity));
    }
    Ok(Plan { id: r.id, key: r.key.clone(), strategy: r.strategy, message, base_dir, base_shown, base: r.base.clone(), base_sha, branch_dir, branch: r.branch.clone(), head, merge_base, behind })
}

/// git has an author and a committer identity from its configuration (or the `GIT_AUTHOR_*` and
/// `GIT_COMMITTER_*` variables), without guessing one from the system.
fn identity(dir: &Path) -> bool {
    ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"].iter().all(|var| changes::git_ok(dir, &["-c", "user.useConfigOnly=true", "var", var]).is_some())
}

// --- Running git ------------------------------------------------------------------------------

/// What became of a git command.
enum Ran {
    Ok,
    /// git's message (its hooks' output included).
    Failed(String),
    /// Past the ceiling.
    TimedOut,
}

/// A git command of a merge, in its own session with no terminal. `aside`: it prepares the
/// result in the private worktree (stopped, with its whole group, past the ceiling or when the
/// agent exits); otherwise it is waited for to its end.
fn git(dir: &Path, args: &[&str], aside: bool) -> Ran {
    git_spawned(dir, args, aside, &|_| {})
}

/// `git`, telling `spawned` the process's id as soon as it runs.
fn git_spawned(dir: &Path, args: &[&str], aside: bool, spawned: &dyn Fn(u32)) -> Ran {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let folder = private_dir();
    let err = folder.join(format!("err-{}-{}.txt", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let file = match private_file(&folder, &err, b"") {
        Ok(f) => f,
        Err(e) => return Ran::Failed(e.to_string()),
    };
    let mut cmd = git_command(dir);
    cmd.args(WRITE_CONFIG);
    if aside {
        // Never an automatic gc of the shared repository from the private worktree.
        cmd.args(["-c", "gc.auto=0"]);
    }
    cmd.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .env_remove("GPG_TTY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(file));
    // SAFETY: `setsid` is async-signal-safe; it detaches the child from any terminal and makes it
    // the leader of its own group.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let ran = match cmd.spawn() {
        Err(e) => Ran::Failed(e.to_string()),
        Ok(mut child) => {
            spawned(child.id());
            let group = child.id() as i32;
            if aside {
                with_current(|c| {
                    if let Some(cur) = c.as_mut() {
                        cur.group = Some(group);
                    }
                });
            }
            let deadline = Instant::now() + ceiling();
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) if aside && Instant::now() >= deadline => {
                        // SAFETY: a plain system call on our child's own group.
                        unsafe { libc::kill(-group, libc::SIGKILL) };
                        let _ = child.wait();
                        break None;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => break None,
                }
            };
            if aside {
                with_current(|c| {
                    if let Some(cur) = c.as_mut() {
                        cur.group = None;
                    }
                });
            }
            match status {
                Some(s) if s.success() => Ran::Ok,
                Some(_) => Ran::Failed(read_detail(&err)),
                None => Ran::TimedOut,
            }
        }
    };
    let _ = std::fs::remove_file(&err);
    ran
}

fn read_detail(path: &Path) -> String {
    let mut text = Vec::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(16 * 1024).read_to_end(&mut text);
    }
    String::from_utf8_lossy(&text).trim().to_string()
}

/// A new file only the user can read, in the agent's private folder.
fn private_file(folder: &Path, path: &Path, content: &[u8]) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(folder)?;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    std::io::Write::write_all(&mut f, content)?;
    Ok(f)
}

// --- Preparing the result aside ------------------------------------------------------------------

/// The result `R`, prepared aside (`temp`: the private worktree made for it, if any).
fn prepare(plan: &Plan, temp: &mut Option<PathBuf>) -> Result<String, MergeOutcome> {
    let fast = matches!(plan.strategy, MergeStrategy::FastForward | MergeStrategy::Rebase) && plan.behind == 0;
    if fast {
        return Ok(plan.head.clone());
    }
    let t = make_temp(plan, temp)?;
    let step = |step: MergeStep, ran: Ran| -> Result<(), MergeOutcome> {
        if with_current(|c| c.as_ref().is_some_and(|cur| cur.abandoned)) {
            return Err(failed(Problem::MergeStepFailed { step, detail: "the agent is stopping".into() }));
        }
        match ran {
            Ran::Ok => Ok(()),
            Ran::TimedOut => Err(failed(Problem::MergeTimedOut { secs: ceiling().as_secs() })),
            Ran::Failed(detail) => {
                let files = unmerged(&t);
                if files.is_empty() {
                    Err(failed(Problem::MergeStepFailed { step, detail }))
                } else {
                    let more = files.len().saturating_sub(MAX_MERGE_CONFLICTS) as u32;
                    Err(MergeOutcome::Conflicted { files: files.into_iter().take(MAX_MERGE_CONFLICTS).collect(), more })
                }
            }
        }
    };
    let tree = |rev: &str| changes::git_ok(&t, &["rev-parse", rev]).map(|s| s.trim().to_string());
    let staged = || changes::git_ok(&t, &["write-tree"]).map(|s| s.trim().to_string());
    match plan.strategy {
        MergeStrategy::Merge | MergeStrategy::FastForward => {
            // The tree the merge stages (no commit: no commit hooks), then the merge commit with
            // its hooks: a hook that changes the content is not taken.
            step(MergeStep::Merge, git(&t, &["merge", "--no-commit", "--no-ff", &plan.head], true))?;
            let expected = staged().ok_or_else(|| failed(Problem::MergeHookChanged))?;
            step(MergeStep::Merge, git(&t, &["reset", "-q", "--hard", &plan.base_sha], true))?;
            let message = merge_message(&plan.branch, &plan.base);
            step(MergeStep::Merge, git(&t, &["merge", "--no-ff", "-m", &message, &plan.head], true))?;
            if tree("HEAD^{tree}").as_deref() != Some(expected.as_str()) {
                return Err(failed(Problem::MergeHookChanged));
            }
        }
        MergeStrategy::Squash => {
            step(MergeStep::Merge, git(&t, &["merge", "--squash", &plan.head], true))?;
            let expected = staged().ok_or_else(|| failed(Problem::MergeHookChanged))?;
            let message = plan.message.as_deref().unwrap_or_default();
            step(MergeStep::Commit, commit_message(&t, message))?;
            if tree("HEAD^{tree}").as_deref() != Some(expected.as_str()) {
                return Err(failed(Problem::MergeHookChanged));
            }
        }
        MergeStrategy::Rebase => {
            // The tree a merge gives (no commit: no commit hooks), then the rebase, compared.
            step(MergeStep::Merge, git(&t, &["merge", "--no-commit", "--no-ff", &plan.head], true))?;
            let expected = changes::git_ok(&t, &["write-tree"]).map(|s| s.trim().to_string()).ok_or_else(|| failed(Problem::MergeRebaseDiffers))?;
            step(MergeStep::Rebase, git(&t, &["reset", "-q", "--hard", &plan.base_sha], true))?;
            step(MergeStep::Rebase, git(&t, &["-c", "rebase.backend=merge", "rebase", "--onto", &plan.base_sha, &plan.merge_base, &plan.head], true))?;
            if changes::git_ok(&t, &["rev-parse", "HEAD^{tree}"]).map(|s| s.trim().to_string()).as_deref() != Some(expected.as_str()) {
                return Err(failed(Problem::MergeRebaseDiffers));
            }
        }
    }
    let result = commit(&t, "HEAD").ok_or_else(|| failed(Problem::CommitNotFound))?;
    // The result builds on the base reviewed.
    if changes::git_ok(&t, &["merge-base", "--is-ancestor", &plan.base_sha, &result]).is_none() {
        return Err(failed(Problem::MergeRebaseDiffers));
    }
    Ok(result)
}

/// The private worktree, detached at the reviewed base commit.
fn make_temp(plan: &Plan, temp: &mut Option<PathBuf>) -> Result<PathBuf, MergeOutcome> {
    use std::os::unix::fs::DirBuilderExt;
    let root = work_root();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&root).map_err(|e| failed(Problem::Unreadable { detail: e.to_string() }))?;
    let root = std::fs::canonicalize(&root).map_err(|e| failed(Problem::Unreadable { detail: e.to_string() }))?;
    // Never inside the repository's worktrees (a home folder inside one).
    if [&plan.base_dir, &plan.branch_dir].iter().any(|d| path_inside(&d.to_string_lossy(), &root.to_string_lossy())) {
        return Err(failed(Problem::MergeStepFailed { step: MergeStep::Prepare, detail: "the agent's data folder is inside the repository".into() }));
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let t = root.join(format!("m{}-{}-{nanos:09}", std::process::id(), plan.id));
    let record = with_current(|c| {
        let cur = c.as_mut()?;
        cur.temp = Some(t.clone());
        cur.record.clone()
    });
    if let Some(record) = record {
        update_record(&record, |r| r.temp = Some(t.to_string_lossy().into_owned())).map_err(|e| failed(Problem::Unreadable { detail: e.to_string() }))?;
    }
    // Removed whatever happens next (a failing `post-checkout` hook leaves it created).
    *temp = Some(t.clone());
    let path = t.to_string_lossy().into_owned();
    match git(&plan.base_dir, &["worktree", "add", "--detach", &path, &plan.base_sha], true) {
        Ran::Ok => Ok(t),
        Ran::Failed(detail) => Err(failed(Problem::MergeStepFailed { step: MergeStep::Prepare, detail })),
        Ran::TimedOut => Err(failed(Problem::MergeTimedOut { secs: ceiling().as_secs() })),
    }
}

/// `git commit` with `message`, from a private file (never on a command line), kept as written
/// (`--cleanup=whitespace`: lines starting with `#` stay, only trailing blank space goes).
fn commit_message(dir: &Path, message: &str) -> Ran {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let folder = private_dir();
    let file = folder.join(format!("message-{}-{}.txt", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let result = match private_file(&folder, &file, message.as_bytes()) {
        Ok(_) => git(dir, &["commit", "--cleanup=whitespace", &format!("--file={}", file.display())], true),
        Err(e) => Ran::Failed(e.to_string()),
    };
    let _ = std::fs::remove_file(&file);
    result
}

/// The files left unmerged in `dir`, as `changes::name_text` names them.
fn unmerged(dir: &Path) -> Vec<String> {
    let out = git_command(dir).args(["diff", "--name-only", "--diff-filter=U", "-z"]).env("GIT_OPTIONAL_LOCKS", "0").output();
    let Ok(out) = out else { return Vec::new() };
    let mut files: Vec<String> = out.stdout.split(|&b| b == 0).filter(|f| !f.is_empty()).map(changes::name_text).collect();
    files.dedup();
    files
}

/// The agent's private folders.
fn private_dir() -> PathBuf {
    let home = std::env::var("TCANVAS_TEST_HOME").ok().filter(|h| !h.is_empty()).or_else(|| std::env::var("HOME").ok()).unwrap_or_else(|| "/".into());
    PathBuf::from(home).join(MERGE_DIR)
}

/// Where the private worktrees go.
pub fn work_root() -> PathBuf {
    private_dir().join("work")
}

/// A worktree of the agent's own (hidden from every list the app gets).
pub fn is_private(path: &str) -> bool {
    let root = work_root();
    let inside = |root: &Path| path_inside(&root.to_string_lossy(), path);
    inside(&root) || std::fs::canonicalize(&root).is_ok_and(|r| inside(&r))
}

/// Removes a private worktree: `git worktree remove --force` on it (only ever a path under the
/// agent's folder), then its folder and its entry under the repository's `worktrees/` if git
/// left them. Nothing else is touched (never a `git worktree prune`).
fn remove_temp(base_dir: &Path, temp: &Path) {
    let text = temp.to_string_lossy().into_owned();
    if !is_private(&text) {
        return;
    }
    let admin = std::fs::read_to_string(temp.join(".git")).ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("gitdir:")).map(|p| PathBuf::from(p.trim())));
    if base_dir.is_dir() {
        let _ = git_command(base_dir).args(["worktree", "remove", "--force", &text]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    if temp.exists() {
        let _ = std::fs::remove_dir_all(temp);
    }
    if let Some(admin) = admin.filter(|a| a.parent().and_then(Path::file_name).is_some_and(|n| n == "worktrees") && a.is_dir()) {
        let _ = std::fs::remove_dir_all(admin);
    }
}

// --- Applying it ----------------------------------------------------------------------------------

/// B as git sees it, to tell afterwards whether a failed fast-forward wrote anything.
#[derive(PartialEq, Eq)]
struct BaseState {
    /// `git ls-files -s -z`: the index's entries.
    index: Option<Vec<u8>>,
    head: Option<String>,
    branch: Option<String>,
    tip: Option<String>,
}

fn base_state(b: &Path, base: &str) -> BaseState {
    let index = git_command(b).args(["ls-files", "-s", "-z"]).env("GIT_OPTIONAL_LOCKS", "0").stderr(Stdio::null()).output().ok().filter(|o| o.status.success()).map(|o| o.stdout);
    BaseState { index, head: commit(b, "HEAD"), branch: on_branch(b), tip: commit(b, &format!("refs/heads/{base}")) }
}

/// Untracked or ignored files at the paths the result writes in B, as an outcome.
fn collisions(b: &Path, paths: &[Vec<u8>], shown: &str) -> Option<MergeOutcome> {
    match untracked_at(b, paths) {
        Some(found) if found.is_empty() => None,
        Some(found) => {
            let more = found.len().saturating_sub(MAX_MERGE_CONFLICTS) as u32;
            Some(MergeOutcome::NotApplied { why: NotApplied::Collisions { files: found.into_iter().take(MAX_MERGE_CONFLICTS).collect(), more } })
        }
        None => Some(MergeOutcome::Refused { reason: MergeRefusal::NotChecked { path: shown.to_string() } }),
    }
}

/// B moves to `result` by a fast-forward, or it is said exactly what it is left as.
fn apply(plan: &Plan, result: &str, record: &Path) -> MergeOutcome {
    let b = &plan.base_dir;
    let moved = || {
        let (found, head) = (on_branch(b), commit(b, "HEAD"));
        let tip = commit(b, &format!("refs/heads/{}", plan.base));
        (found.as_deref() != Some(plan.base.as_str()) || head.as_deref() != Some(plan.base_sha.as_str()) || tip.as_deref() != Some(plan.base_sha.as_str()))
            .then_some(NotApplied::BaseMoved { found, head })
    };
    if let Some(why) = moved() {
        return MergeOutcome::NotApplied { why };
    }
    let Some(paths) = changed(b, &plan.base_sha, result) else { return failed(Problem::CommitNotFound) };
    // Said by name first (the dry run below would only say git refuses), checked again last.
    if let Some(outcome) = collisions(b, &paths, &plan.base_shown) {
        return outcome;
    }
    // Locks another git holds (or left behind when it crashed): the fast-forward would fail
    // half-way on them.
    for name in ["index.lock".to_string(), format!("refs/heads/{}.lock", plan.base)] {
        if let Some(path) = git_path(b, &name).filter(|p| p.symlink_metadata().is_ok()) {
            return MergeOutcome::NotApplied { why: NotApplied::Locked { path: path.to_string_lossy().into_owned() } };
        }
    }
    // A dry run: nothing written, it says whether git would refuse.
    if let Ran::Failed(detail) = git(b, &["read-tree", "-n", "-m", "-u", &plan.base_sha, result], false) {
        return MergeOutcome::NotApplied { why: NotApplied::DryRun { detail } };
    }
    // From here the merge is applied: never abandoned half-way. The record says so first; if it
    // cannot, nothing is applied.
    if with_current(|c| c.as_ref().is_some_and(|cur| cur.abandoned)) {
        return failed(Problem::MergeStepFailed { step: MergeStep::Merge, detail: "the agent is stopping".into() });
    }
    if let Err(e) = update_record(record, |r| {
        r.phase = MergePhase::Applying;
        r.result = Some(result.to_string());
    }) {
        return failed(Problem::Unreadable { detail: e.to_string() });
    }
    let go_on = with_current(|c| match c.as_mut() {
        Some(cur) if cur.abandoned => false,
        Some(cur) => {
            cur.phase = MergePhase::Applying;
            true
        }
        None => true,
    });
    if !go_on {
        return failed(Problem::MergeStepFailed { step: MergeStep::Merge, detail: "the agent is stopping".into() });
    }
    let orig_head = git_path(b, "ORIG_HEAD");
    let orig_before = orig_head.as_ref().and_then(|p| std::fs::read(p).ok());
    let before = base_state(b, &plan.base);
    // The last checks, right before git starts: B's branch and head again, then what is at the
    // paths the result writes. A file created there in the few milliseconds between this check
    // and git's own start is not seen: `merge --ff-only` overwrites an ignored file without a
    // word, and nothing can close that window from outside git.
    if let Some(why) = moved() {
        return MergeOutcome::NotApplied { why };
    }
    if let Some(outcome) = collisions(b, &paths, &plan.base_shown) {
        return outcome;
    }
    if std::env::var_os("TCANVAS_MERGE_TEST_PAUSE_SPAWN").is_some() {
        pause("spawn");
    }
    let spawned = |pid: u32| {
        let start = start_time(pid).unwrap_or(0);
        let _ = update_record(record, |r| {
            r.git_pid = Some(pid);
            r.git_start = start;
        });
    };
    if let Ran::Failed(detail) = git_spawned(b, &["merge", "--ff-only", result], false, &spawned) {
        // git writes ORIG_HEAD even when it refuses: put back as it was.
        if let Some(path) = &orig_head
            && std::fs::read(path).ok() != orig_before
        {
            let _ = match &orig_before {
                Some(bytes) => std::fs::write(path, bytes),
                None => std::fs::remove_file(path),
            };
        }
        let after = base_state(b, &plan.base);
        let orig_back = orig_head.as_ref().and_then(|p| std::fs::read(p).ok()) == orig_before;
        if after == before && orig_back {
            return MergeOutcome::NotApplied { why: NotApplied::Refused { detail } };
        }
        let index_at_result = git_command(b).args(["diff-index", "--cached", "--quiet", result]).env("GIT_OPTIONAL_LOCKS", "0").stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        if index_at_result && after.head.as_deref() == Some(plan.base_sha.as_str()) && after.tip.as_deref() == Some(plan.base_sha.as_str()) {
            return MergeOutcome::PartlyWritten { base: plan.base.clone(), base_head: plan.base_sha.clone(), result: result.to_string(), detail };
        }
        return MergeOutcome::Unverified { base_branch: after.branch, base_tip: after.tip, moved: None };
    }
    pause("applied");
    let base_branch = on_branch(b);
    let base_tip = commit(b, &format!("refs/heads/{}", plan.base));
    if base_branch.as_deref() != Some(plan.base.as_str()) || base_tip.as_deref() != Some(result) || commit(b, "HEAD").as_deref() != Some(result) {
        // Another branch checked out meanwhile got the fast-forward instead.
        let moved = base_branch.clone().filter(|other| other != &plan.base && commit(b, &format!("refs/heads/{other}")).as_deref() == Some(result));
        return MergeOutcome::Unverified { base_branch, base_tip, moved };
    }
    MergeOutcome::Merged { base_head: result.to_string(), branch_head: plan.head.clone(), fast_forward: result == plan.head }
}

/// The paths changed between two commits (both sides of a rename), as git gives them.
fn changed(dir: &Path, from: &str, to: &str) -> Option<Vec<Vec<u8>>> {
    let out = git_command(dir).args(["diff", "--name-only", "--no-renames", "-z", from, to]).env("GIT_OPTIONAL_LOCKS", "0").stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
    Some(out.stdout.split(|&b| b == 0).filter(|p| !p.is_empty()).map(<[u8]>::to_vec).collect())
}

/// Of `paths` the result writes, those in the worktree `dir` without being tracked there: an
/// untracked or ignored file at the path or at a folder above it, or a folder at the path holding
/// anything git does not track. `None`: git failed, or a folder was too big to walk.
pub fn untracked_at(dir: &Path, paths: &[Vec<u8>]) -> Option<Vec<String>> {
    if paths.is_empty() {
        return Some(Vec::new());
    }
    let out = git_command(dir).args(["ls-files", "-z"]).env("GIT_OPTIONAL_LOCKS", "0").stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
    let tracked: HashSet<&[u8]> = out.stdout.split(|&b| b == 0).filter(|p| !p.is_empty()).collect();
    let mut found = Vec::new();
    for path in paths {
        let parents = path.iter().enumerate().filter(|(_, b)| **b == b'/').map(|(i, _)| &path[..i]);
        for prefix in parents {
            if std::fs::symlink_metadata(dir.join(OsStr::from_bytes(prefix))).is_ok_and(|m| !m.is_dir()) && !tracked.contains(prefix) {
                found.push(changes::name_text(prefix));
            }
        }
        let Ok(meta) = std::fs::symlink_metadata(dir.join(OsStr::from_bytes(path))) else { continue };
        if meta.is_dir() {
            untracked_below(dir, path, &tracked, &mut found)?;
        } else if !tracked.contains(path.as_slice()) {
            found.push(changes::name_text(path));
        }
    }
    found.sort();
    found.dedup();
    Some(found)
}

/// What a folder at a written path holds that git does not track.
fn untracked_below(dir: &Path, folder: &[u8], tracked: &HashSet<&[u8]>, found: &mut Vec<String>) -> Option<()> {
    let mut stack = vec![folder.to_vec()];
    let mut seen = 0;
    while let Some(rel) = stack.pop() {
        for entry in std::fs::read_dir(dir.join(OsStr::from_bytes(&rel))).ok()?.flatten() {
            seen += 1;
            if seen > 100_000 {
                return None;
            }
            let mut child = rel.clone();
            child.push(b'/');
            child.extend_from_slice(entry.file_name().as_bytes());
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(child),
                _ if tracked.contains(child.as_slice()) => {}
                _ => found.push(changes::name_text(&child)),
            }
        }
    }
    Some(())
}

// --- Records --------------------------------------------------------------------------------------

fn records_dir() -> PathBuf {
    private_dir().join("in-progress")
}

/// This process's start time (`/proc/<pid>/stat`, field 22): a pid reused by another process has
/// another one.
fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = &stat[stat.rfind(')')? + 2..];
    after.split_whitespace().nth(19)?.parse().ok()
}

/// The merge is still running: its agent, or the git fast-forwarding the base (it runs in its own
/// session and outlives a killed agent), is alive.
fn alive(record: &MergeRecord) -> bool {
    let agent = record.pid != std::process::id() && start_time(record.pid).is_some_and(|s| s == record.pid_start);
    let git = record.git_pid.is_some_and(|pid| start_time(pid).is_some_and(|s| s == record.git_start));
    agent || git
}

/// Where `name` is in the git folder of the worktree `dir`.
fn git_path(dir: &Path, name: &str) -> Option<PathBuf> {
    changes::git_ok(dir, &["rev-parse", "--git-path", name]).map(|p| dir.join(p.trim_end_matches('\n')))
}

fn save(path: &Path, record: &MergeRecord) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let f = private_file(&records_dir(), &tmp, protocol::encode(record).as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Written before the private worktree is made; removed with the final outcome.
fn write_record(plan: &Plan) -> std::io::Result<PathBuf> {
    let pid = std::process::id();
    let record = MergeRecord {
        id: plan.id,
        key: plan.key.clone(),
        strategy: plan.strategy,
        branch: plan.branch.clone(),
        base: plan.base.clone(),
        base_path: plan.base_dir.to_string_lossy().into_owned(),
        branch_path: plan.branch_dir.to_string_lossy().into_owned(),
        base_head: plan.base_sha.clone(),
        branch_head: plan.head.clone(),
        temp: None,
        phase: MergePhase::Preparing,
        result: None,
        pid,
        pid_start: start_time(pid).unwrap_or(0),
        git_pid: None,
        git_start: 0,
        started: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    };
    let path = records_dir().join(format!("{}-{pid}-{}.json", record.started, plan.id));
    save(&path, &record)?;
    Ok(path)
}

fn update_record(path: &Path, change: impl FnOnce(&mut MergeRecord)) -> std::io::Result<()> {
    let mut record: MergeRecord = protocol::decode(&std::fs::read_to_string(path)?).map_err(std::io::Error::other)?;
    change(&mut record);
    save(path, &record)
}

/// The records on disk: read, or the reason one could not be.
fn records() -> Vec<(PathBuf, Result<MergeRecord, String>)> {
    let Ok(entries) = std::fs::read_dir(records_dir()) else { return Vec::new() };
    let mut found: Vec<(PathBuf, Result<MergeRecord, String>)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let read = std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| protocol::decode::<MergeRecord>(&t).map_err(|e| e.to_string()));
            (p, read)
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// The previous agent, still alive, merging with one of these worktrees.
fn previous_agent(base: &Path, branch: &Path) -> Option<u32> {
    records().into_iter().filter_map(|(_, r)| r.ok()).find(|r| alive(r) && [&r.base_path, &r.branch_path].iter().any(|p| Path::new(p) == base || Path::new(p) == branch)).map(|r| r.pid)
}

/// At start: the merges a previous agent recorded and did not finish. One whose agent is still
/// alive is only reported; otherwise its leftover private worktree is removed and whether the
/// base was moved to its result is read from the refs; its record goes. A record that cannot be
/// read is moved aside and reported.
pub fn interrupted() -> Vec<AgentEvent> {
    remove_stale_files();
    let mut events = Vec::new();
    for (path, read) in records() {
        let record = match read {
            Ok(record) => record,
            Err(detail) => {
                let aside = path.with_extension("corrupt");
                let _ = std::fs::rename(&path, &aside);
                events.push(AgentEvent::MergeRecordCorrupt { path: aside.to_string_lossy().into_owned(), detail });
                continue;
            }
        };
        let base_dir = PathBuf::from(&record.base_path);
        let base_tip = commit(&base_dir, &format!("refs/heads/{}", record.base));
        let base_checkout = on_branch(&base_dir);
        let base_dirty = cleanup::dirty_now(&base_dir);
        // Applied: the result is in the base's branch (the user may have committed on it since).
        let applied = record.result.as_deref().zip(base_tip.as_deref()).is_some_and(|(result, tip)| changes::git_ok(&base_dir, &["merge-base", "--is-ancestor", result, tip]).is_some());
        if alive(&record) {
            events.push(AgentEvent::MergeInterrupted { record, previous_alive: true, applied, base_tip, base_checkout, base_dirty });
            continue;
        }
        if let Some(temp) = record.temp.as_deref().map(PathBuf::from).or_else(|| leftover_temp(&record)) {
            remove_temp(&base_dir, &temp);
        }
        let _ = std::fs::remove_file(&path);
        events.push(AgentEvent::MergeInterrupted { record, previous_alive: false, applied, base_tip, base_checkout, base_dirty });
    }
    events
}

/// The error and message files of agents that are gone (killed while a git command ran): a
/// squash's message holds the user's text.
fn remove_stale_files() {
    let Ok(entries) = std::fs::read_dir(private_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("err-").or_else(|| name.strip_prefix("message-")) else { continue };
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<u32>().ok()) else { continue };
        if pid != std::process::id() && start_time(pid).is_none() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The private worktree a record's merge made (named after its agent and request).
fn leftover_temp(record: &MergeRecord) -> Option<PathBuf> {
    let prefix = format!("m{}-{}-", record.pid, record.id);
    std::fs::read_dir(work_root()).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str, status: TermStatus, command: &str, cwd: &str) -> SessionInfo {
        SessionInfo {
            session: name.into(),
            status,
            command: Some(command.into()),
            cwd: Some(cwd.into()),
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
            agent_session_id: None,
            agent_transcript: None,
            last_prompt: None,
            agent_prompt_mark: None,
            agent_turn_mark: None,
            agent_prompt_head: None,
            agent_turn_head: None,
        }
    }

    #[test]
    fn only_a_coding_agent_at_work_in_the_folder_counts() {
        let wt = "/nonexistent/shop";
        let working = session("tc_a", TermStatus::Running, "claude --resume", "/nonexistent/shop/src");
        assert_eq!(agent_working(std::slice::from_ref(&working), wt).as_deref(), Some("tc_a"));
        let waiting = SessionInfo { status: TermStatus::WaitingInput, ..working.clone() };
        assert_eq!(agent_working(&[waiting], wt), None);
        assert_eq!(agent_working(&[session("tc_b", TermStatus::Running, "npm run dev", wt)], wt), None, "a terminal or a process inside is not a reason");
        let elsewhere = SessionInfo { cwd: Some("/nonexistent/shop-2".into()), ..working };
        assert_eq!(agent_working(&[elsewhere], wt), None);
    }

    #[test]
    fn a_squash_needs_a_message_within_bounds() {
        assert_eq!(squash_message(None), Err(MergeRefusal::MessageEmpty));
        assert_eq!(squash_message(Some(" \n\t")), Err(MergeRefusal::MessageEmpty));
        assert_eq!(squash_message(Some("a\0b")), Err(MergeRefusal::MessageInvalid));
        assert_eq!(squash_message(Some(&"x".repeat(MAX_MERGE_MESSAGE + 1))), Err(MergeRefusal::MessageTooLong { max: MAX_MERGE_MESSAGE }));
        assert_eq!(squash_message(Some("Invoices\n\n# kept")).as_deref(), Ok("Invoices\n\n# kept"));
    }

    #[test]
    fn merge_commits_say_their_branch_as_git_would() {
        assert_eq!(merge_message("feat/x", "main"), "Merge branch 'feat/x'");
        assert_eq!(merge_message("feat/x", "develop"), "Merge branch 'feat/x' into develop");
    }

    #[test]
    fn requests_come_from_merge_commands_only() {
        let cmd = AgentCmd::Merge {
            id: 2,
            key: "p".into(),
            worktree: "/w".into(),
            branch: "feat".into(),
            base: "main".into(),
            strategy: MergeStrategy::Rebase,
            message: None,
            branch_head: "a".into(),
            base_head: "b".into(),
        };
        assert_eq!(Request::of(cmd).map(|r| (r.id, r.strategy)), Some((2, MergeStrategy::Rebase)));
        assert_eq!(Request::of(AgentCmd::Unwatch { key: "p".into() }), None);
    }

    #[test]
    fn this_process_is_alive_with_its_start_time() {
        let me = std::process::id();
        let start = start_time(me).expect("our own start time");
        assert_eq!(start_time(me), Some(start), "stable");
        assert_eq!(start_time(u32::MAX), None);
    }

    #[test]
    fn operations_in_progress_are_read_from_the_git_dir() {
        assert_eq!(operation(Path::new("/nonexistent/folder")), Err(()), "git could not tell");
        assert!(IN_PROGRESS.iter().any(|(name, op)| *name == "sequencer" && *op == GitOperation::Sequence));
    }
}
