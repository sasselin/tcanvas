//! Review of a worktree's branch against its base, like a merge request: the summary (merge base,
//! the branch's commits, the files changed since the merge base with their counts, what is not
//! committed, ahead/behind), the files of one commit, and diffs. Read only and lock-free
//! (`GIT_OPTIONAL_LOCKS=0`, the environment cleaned by `changes::git_command`), on its own thread.
//!
//! Nothing the app sends reaches git unchecked: the worktree must be one of the watched folder's
//! repository's worktrees (compared after resolving both paths), a base must be a name a branch
//! could have (`protocol::branch_name_valid`: never starting with `-`) and is resolved to a commit
//! before use (a local branch first, then a remote-tracking one, then whatever git finds), a
//! commit is a hexadecimal sha resolved to a commit, a path is relative and does not escape
//! (`changes::safe_relative`) and is passed after `--` with literal pathspecs. File names travel
//! as `changes::name_text` gives them: a name that is not UTF-8 is quoted the way git quotes it,
//! and turned back into its exact bytes before it reaches git.
//!
//! Bounds: `MAX_FILES` files and `MAX_COMMITS` commits listed (with how many more), file lists read
//! as a stream (the totals count every file without keeping them), diffs read up to `MAX_DIFF`
//! bytes, git stopped beyond, with the same truncation notice as the Changes panel's diffs. Every
//! git call is stopped past `DEADLINE` (its whole process group killed), answered with
//! `Problem::ReviewTimedOut`: one stuck request never holds the thread. No file is read through a
//! symbolic link, and nothing but a regular file is read at all.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use protocol::{AgentCmd, AgentEvent, BaseKind, ChangeKind, CommitInfo, FileChange, Problem, ReviewFiles, ReviewScope, ReviewState, ReviewSummary};

use super::changes::{self, EMPTY_TREE, FIELD, MAX_DIFF};
use super::worktrees;

/// Files listed per scope, at most.
pub const MAX_FILES: usize = 500;
/// Commits of the branch listed, at most.
pub const MAX_COMMITS: usize = 200;
/// Bases offered, at most.
pub const MAX_BASES: usize = 200;
/// Each git call of a review is stopped past this.
pub const DEADLINE: Duration = Duration::from_secs(20);
/// Output kept from a git call that is not a diff or a streamed list (refs, status...).
const MAX_CAPTURE: usize = 8 << 20;

/// What a review request asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    Summary { base: Option<String> },
    Files { scope: ReviewScope },
    Diff { scope: ReviewScope, path: Option<String>, old_path: Option<String> },
}

impl Ask {
    /// Of several waiting requests of the same kind for one key, only the last is answered.
    fn kind(&self) -> u8 {
        match self {
            Ask::Summary { .. } => 0,
            Ask::Files { .. } => 1,
            Ask::Diff { .. } => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    pub key: String,
    pub worktree: String,
    pub ask: Ask,
}

impl Request {
    pub fn of(cmd: AgentCmd) -> Option<Request> {
        Some(match cmd {
            AgentCmd::Review { id, key, worktree, base } => Request { id, key, worktree, ask: Ask::Summary { base } },
            AgentCmd::ReviewFiles { id, key, worktree, scope } => Request { id, key, worktree, ask: Ask::Files { scope } },
            AgentCmd::ReviewDiff { id, key, worktree, scope, path, old_path } => Request { id, key, worktree, ask: Ask::Diff { scope, path, old_path } },
            _ => return None,
        })
    }

    /// A newer request of the same kind for the same key waits: this one's answer would be stale.
    pub fn superseded_by(&self, later: &Request) -> bool {
        later.key == self.key && later.ask.kind() == self.ask.kind()
    }
}

/// Answers `request`; `dir`: the watched folder of its key, or why there is none.
pub fn answer(request: Request, dir: Result<PathBuf, Problem>) -> AgentEvent {
    let Request { id, key, worktree, ask } = request;
    let checked = dir.and_then(|dir| worktree_of(&dir, &worktree));
    match ask {
        Ask::Summary { base } => match checked.map_err(|p| (p, Vec::new())).and_then(|wt| summary(&wt, base.as_deref())) {
            Ok((summary, bases)) => AgentEvent::Review { id, key, summary: Some(Box::new(summary)), bases, problem: None },
            Err((problem, bases)) => AgentEvent::Review { id, key, summary: None, bases, problem: Some(problem) },
        },
        Ask::Files { scope } => match checked.and_then(|wt| scope_files(&wt.path, &scope)) {
            Ok(files) => AgentEvent::ReviewFiles { id, key, files, problem: None },
            Err(problem) => AgentEvent::ReviewFiles { id, key, files: ReviewFiles::default(), problem: Some(problem) },
        },
        Ask::Diff { scope, path, old_path } => match checked.and_then(|wt| diff(&wt.path, &scope, path.as_deref(), old_path.as_deref())) {
            Ok((text, truncated)) => AgentEvent::ReviewDiff { id, key, text, truncated, problem: None },
            Err(problem) => AgentEvent::ReviewDiff { id, key, text: String::new(), truncated: false, problem: Some(problem) },
        },
    }
}

// --- Running git ------------------------------------------------------------------------------

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// A git command reading the repository, with literal pathspecs (a path is never a pattern).
fn reader(dir: &Path, args: &[OsString]) -> Command {
    let mut cmd = changes::git_command(dir);
    cmd.args(args).env("GIT_OPTIONAL_LOCKS", "0").env("GIT_LITERAL_PATHSPECS", "1");
    cmd
}

/// Runs `cmd` in its own process group, hands its output to `read` (which says whether it stopped
/// before the end), and kills the whole group past `limit` or once `read` stopped. Returns what
/// `read` gave and whether the command succeeded (a stopped one counts as such).
pub fn run_with<T>(mut cmd: Command, limit: Duration, read: impl FnOnce(ChildStdout) -> (T, bool)) -> Result<(T, bool), Problem> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0);
    let mut child = cmd.spawn().map_err(|e| Problem::GitFailed { detail: e.to_string() })?;
    let group = child.id() as i32;
    let kill = move || {
        // SAFETY: a plain system call on the child's own group; the child is not reaped yet, so
        // its id is not reused meanwhile.
        unsafe { libc::kill(-group, libc::SIGKILL) };
    };
    let (cancel, cancelled) = mpsc::channel::<()>();
    let watchdog = thread::spawn(move || {
        let fired = cancelled.recv_timeout(limit) == Err(RecvTimeoutError::Timeout);
        if fired {
            kill();
        }
        fired
    });
    let (value, stopped) = match child.stdout.take() {
        Some(out) => read(out),
        None => {
            kill();
            drop(cancel);
            let _ = watchdog.join();
            let _ = child.wait();
            return Err(Problem::GitFailed { detail: "no output".into() });
        }
    };
    if stopped {
        kill();
    }
    drop(cancel);
    let fired = watchdog.join().unwrap_or(true);
    let status = child.wait();
    if fired {
        return Err(Problem::ReviewTimedOut { secs: limit.as_secs().max(1) });
    }
    Ok((value, stopped || status.is_ok_and(|s| s.success())))
}

/// git's output (at most `MAX_CAPTURE` bytes) when it succeeded.
fn capture(dir: &Path, args: &[OsString]) -> Result<Option<Vec<u8>>, Problem> {
    let (out, ok) = run_with(reader(dir, args), DEADLINE, |out| {
        let mut buf = Vec::new();
        let _ = out.take(MAX_CAPTURE as u64 + 1).read_to_end(&mut buf);
        let over = buf.len() > MAX_CAPTURE;
        buf.truncate(MAX_CAPTURE);
        (buf, over)
    })?;
    Ok(ok.then_some(out))
}

fn text(dir: &Path, args: &[&str]) -> Result<Option<String>, Problem> {
    Ok(capture(dir, &os(args))?.map(|o| String::from_utf8_lossy(&o).into_owned()))
}

/// Hands each NUL-terminated field of git's output to `each`, which returns `false` to stop
/// reading (git is then stopped). `Ok(false)`: git failed.
fn stream_z(dir: &Path, args: &[OsString], mut each: impl FnMut(&[u8]) -> bool) -> Result<bool, Problem> {
    let ((), ok) = run_with(reader(dir, args), DEADLINE, |out| {
        for field in BufReader::new(out).split(0) {
            let Ok(field) = field else { break };
            if !each(&field) {
                return ((), true);
            }
        }
        ((), false)
    })?;
    Ok(ok)
}

/// git's output, at most `max` bytes (git stopped beyond: `true`, cut on a character boundary),
/// whatever its exit code (`git diff --no-index` exits 1 on differences).
fn read_bounded(dir: &Path, args: &[OsString], max: usize) -> Result<(String, bool), Problem> {
    let ((mut buf, over), _) = run_with(reader(dir, args), DEADLINE, |out| {
        let mut buf = Vec::new();
        let _ = out.take(max as u64 + 1).read_to_end(&mut buf);
        let over = buf.len() > max;
        ((buf, over), over)
    })?;
    if over {
        buf.truncate(max);
        // Cut inside a character: drop its first bytes.
        if let Err(e) = std::str::from_utf8(&buf)
            && e.error_len().is_none()
        {
            buf.truncate(e.valid_up_to());
        }
    }
    Ok((String::from_utf8_lossy(&buf).into_owned(), over))
}

// --- Checks -----------------------------------------------------------------------------------

/// A problem, with the bases listed when the worktree itself was valid.
type Refusal = (Problem, Vec<String>);

/// The reviewed worktree, checked.
struct Checked {
    path: PathBuf,
    /// Its repository's worktrees, main checkout first.
    list: Vec<protocol::Worktree>,
}

/// `requested` must be one of the worktrees of the repository holding `dir` (not bare, not
/// missing), compared after resolving both.
fn worktree_of(dir: &Path, requested: &str) -> Result<Checked, Problem> {
    let refused = || Problem::NotAWorktree { path: requested.to_string() };
    let out = text(dir, &["worktree", "list", "--porcelain"])?.ok_or(Problem::NotAGitRepo)?;
    let list = worktrees::parse_porcelain(&out);
    let wanted = std::fs::canonicalize(requested).map_err(|_| refused())?;
    let found = list.iter().filter(|w| !w.bare && !w.prunable).any(|w| std::fs::canonicalize(&w.path).is_ok_and(|p| p == wanted));
    if !found || !wanted.is_dir() {
        return Err(refused());
    }
    Ok(Checked { path: wanted, list })
}

fn is_hex_sha(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A base the app may name: a branch-like name (`main`, `origin/main`, a sha), never an option or
/// a revision expression.
pub fn base_name_ok(base: &str) -> bool {
    base.len() <= 255 && protocol::branch_name_valid(base)
}

/// The commit `rev` names (`rev` checked by the caller: never an option).
fn commit_of(dir: &Path, rev: &str) -> Result<Option<String>, Problem> {
    let out = text(dir, &["rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")])?;
    Ok(out.map(|o| o.trim().to_string()).filter(|s| is_hex_sha(s)))
}

/// What a base name stands for: a local branch first, then a remote-tracking one, then whatever
/// git finds (a tag named like a branch never hides the branch).
fn resolve_base(dir: &Path, base: &str) -> Result<Option<(String, BaseKind)>, Problem> {
    for (prefix, kind) in [("refs/heads/", BaseKind::Branch), ("refs/remotes/", BaseKind::Remote), ("", BaseKind::Other)] {
        if let Some(sha) = commit_of(dir, &format!("{prefix}{base}"))? {
            return Ok(Some((sha, kind)));
        }
    }
    Ok(None)
}

/// A sha sent by the app, resolved to a commit.
fn sha_commit(dir: &Path, sha: &str) -> Result<String, Problem> {
    if !is_hex_sha(sha) {
        return Err(Problem::CommitRefused);
    }
    commit_of(dir, sha)?.ok_or(Problem::CommitNotFound)
}

/// The two ends a scope compares (the first parent, or the empty tree, before a commit).
fn ends(dir: &Path, scope: &ReviewScope) -> Result<(String, String), Problem> {
    match scope {
        ReviewScope::Branch { merge_base, head } => Ok((sha_commit(dir, merge_base)?, sha_commit(dir, head)?)),
        ReviewScope::Commit { sha } => {
            let sha = sha_commit(dir, sha)?;
            let parent = commit_of(dir, &format!("{sha}^1"))?.unwrap_or_else(|| EMPTY_TREE.to_string());
            Ok((parent, sha))
        }
        ReviewScope::Uncommitted => Err(Problem::CommitRefused),
    }
}

// --- Summary ----------------------------------------------------------------------------------

fn summary(wt: &Checked, base: Option<&str>) -> Result<(ReviewSummary, Vec<String>), Refusal> {
    let dir = wt.path.as_path();
    let bare = |p: Problem| (p, Vec::new());
    let branch = text(dir, &["symbolic-ref", "--short", "-q", "HEAD"]).map_err(bare)?.map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
    let main = wt.list.first();
    let main_checkout = main.and_then(|m| m.branch.as_deref().zip(m.head.as_deref()));
    let default = worktrees::parse_default_branch(&text(dir, &worktrees::DEFAULT_BRANCH_ARGS).map_err(bare)?.unwrap_or_default(), main_checkout);
    let refs = worktrees::parse_refs(&text(dir, &worktrees::REFS_ARGS).map_err(bare)?.unwrap_or_default());
    let bases = bases(&refs, default.as_ref().map(|d| d.name.as_str()), branch.as_deref());
    let refuse = |p: Problem| (p, bases.clone());
    let head = commit_of(dir, "HEAD").map_err(refuse)?.ok_or_else(|| refuse(Problem::ReviewNoCommits))?;
    let base = match base {
        Some(b) if !base_name_ok(b) => return Err(refuse(Problem::ReviewBaseRefused { base: b.to_string() })),
        Some(b) => b.to_string(),
        None => default.as_ref().map(|d| d.name.clone()).ok_or_else(|| refuse(Problem::ReviewNoBase))?,
    };
    let (base_sha, base_kind) = resolve_base(dir, &base).map_err(refuse)?.ok_or_else(|| refuse(Problem::ReviewBaseNotFound { base: base.clone() }))?;
    let merge_bases: Vec<String> = text(dir, &["merge-base", "--all", &base_sha, &head]).map_err(refuse)?.unwrap_or_default().lines().map(|l| l.trim().to_string()).filter(|s| is_hex_sha(s)).collect();
    let merge_base = merge_bases.first().cloned();
    let range = format!("{base_sha}...{head}");
    let counts = text(dir, &["rev-list", "--left-right", "--count", &range]).map_err(refuse)?;
    let (ahead, behind) = counts.and_then(|o| worktrees::parse_left_right(&o)).unwrap_or((0, 0));
    let state = match (&merge_base, ahead) {
        (None, _) => ReviewState::Unrelated,
        (Some(_), 0) => ReviewState::UpToDate,
        _ => ReviewState::Ahead,
    };
    let (mut commits, mut merges, mut files) = (Vec::new(), Vec::new(), ReviewFiles::default());
    if let (ReviewState::Ahead, Some(mb)) = (state, &merge_base) {
        (commits, merges) = branch_commits(dir, &base_sha, &head).map_err(refuse)?;
        files = files_between(dir, mb, &head).map_err(refuse)?;
    }
    let more_commits = ahead.saturating_sub(commits.len() as u32);
    let uncommitted = uncommitted(dir, true).map_err(refuse)?;
    let worktree = wt.list.iter().find(|w| std::fs::canonicalize(&w.path).is_ok_and(|p| p == wt.path)).map_or_else(|| wt.path.to_string_lossy().into_owned(), |w| w.path.clone());
    let several_merge_bases = merge_bases.len() > 1 && state == ReviewState::Ahead;
    let summary = ReviewSummary { worktree, branch, head, base, base_sha, merge_base, state, ahead, behind, commits, more_commits, files, uncommitted, base_kind, several_merge_bases, merges };
    Ok((summary, bases))
}

/// The commits of the branch the base does not have (`<base>..HEAD`: the same as from the merge
/// base, and right with several merge bases too), newest first, at most `MAX_COMMITS`, with the
/// merges among them.
fn branch_commits(dir: &Path, base: &str, head: &str) -> Result<(Vec<CommitInfo>, Vec<String>), Problem> {
    let format = format!("--format=%H{FIELD}%ct{FIELD}%an{FIELD}%P{FIELD}%s");
    let n = format!("-n{MAX_COMMITS}");
    let range = format!("{base}..{head}");
    Ok(text(dir, &["log", &n, &format, "--no-color", &range])?.map(|out| parse_commits(&out)).unwrap_or_default())
}

/// `git log --format=%H<US>%ct<US>%an<US>%P<US>%s`: the commits, and those with several parents.
pub fn parse_commits(out: &str) -> (Vec<CommitInfo>, Vec<String>) {
    let (mut commits, mut merges) = (Vec::new(), Vec::new());
    for line in out.lines() {
        let mut f = line.splitn(5, FIELD);
        let (Some(sha), Some(time), Some(author), Some(parents)) = (f.next(), f.next(), f.next(), f.next()) else { continue };
        if !is_hex_sha(sha) {
            continue;
        }
        if parents.split_whitespace().count() > 1 {
            merges.push(sha.to_string());
        }
        commits.push(CommitInfo { sha: sha.into(), summary: f.next().unwrap_or_default().into(), author: author.into(), time: time.parse().unwrap_or(0) });
    }
    (commits, merges)
}

/// What the base can be: the default branch first, then the local branches by name (the reviewed
/// branch left out), at most `MAX_BASES`.
fn bases(refs: &worktrees::Refs, default: Option<&str>, own: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = default.map(String::from).into_iter().collect();
    for (name, ..) in &refs.branches {
        if out.len() >= MAX_BASES {
            break;
        }
        if Some(name.as_str()) != own && !out.contains(name) {
            out.push(name.clone());
        }
    }
    out
}

// --- File lists -------------------------------------------------------------------------------

/// One entry of `git diff --numstat -z`.
#[derive(Debug, PartialEq, Eq)]
pub struct Numstat {
    /// As `changes::name_text` gives it.
    pub path: String,
    /// `None` for a binary file.
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

/// Reads `git diff --numstat -z` one field at a time: `added\tremoved\tpath`, or, for a rename,
/// `added\tremoved\t` then the old path and the new one as their own fields; `-` for a binary.
#[derive(Default)]
pub struct NumstatParser {
    pending: Option<(Option<u32>, Option<u32>)>,
    old: bool,
}

impl NumstatParser {
    pub fn push(&mut self, field: &[u8]) -> Option<Numstat> {
        if let Some((added, removed)) = self.pending {
            if !self.old {
                self.old = true;
                return None;
            }
            self.pending = None;
            self.old = false;
            return Some(Numstat { path: changes::name_text(field), added, removed });
        }
        let mut parts = field.splitn(3, |&b| b == b'\t');
        let (a, r, path) = (parts.next()?, parts.next()?, parts.next()?);
        let count = |c: &[u8]| std::str::from_utf8(c).ok().and_then(|c| c.parse().ok());
        let counts = (count(a), count(r));
        if path.is_empty() {
            self.pending = Some(counts);
            return None;
        }
        Some(Numstat { path: changes::name_text(path), added: counts.0, removed: counts.1 })
    }
}

/// Reads `git diff --name-status -z` one field at a time: the status (`M`, `A`, `D`, `T`, `R087`,
/// `C100`...), then the path, or, for a rename or a copy, the old path and the new one.
#[derive(Default)]
pub struct NameStatusParser {
    status: Option<u8>,
    old: Option<String>,
}

impl NameStatusParser {
    pub fn push(&mut self, field: &[u8]) -> Option<(String, ChangeKind)> {
        let Some(letter) = self.status else {
            self.status = field.first().copied();
            return None;
        };
        if matches!(letter, b'R' | b'C') && self.old.is_none() {
            self.old = Some(changes::name_text(field));
            return None;
        }
        self.status = None;
        let kind = match (letter, self.old.take()) {
            (b'R', Some(from)) => ChangeKind::Renamed(from),
            (b'A' | b'C', _) => ChangeKind::Added,
            (b'D', _) => ChangeKind::Deleted,
            _ => ChangeKind::Modified,
        };
        Some((changes::name_text(field), kind))
    }
}

/// The options of every review diff: plain patches whatever the user's configuration (no color,
/// no external diff, no text conversion: the counts and the patch agree), renames found.
const PLAIN: [&str; 4] = ["--no-color", "--no-ext-diff", "--no-textconv", "-M"];

/// Every changed file between `from` and `to` counted, the first `MAX_FILES` listed (path order).
fn files_between(dir: &Path, from: &str, to: &str) -> Result<ReviewFiles, Problem> {
    let mut out = ReviewFiles::default();
    let mut counts: HashMap<String, (Option<u32>, Option<u32>)> = HashMap::new();
    let mut total: u64 = 0;
    let mut numstat = NumstatParser::default();
    let args = |what: &str| os(&[&["diff", "-z"][..], &PLAIN, &[what, from, to]].concat());
    let ok = stream_z(dir, &args("--numstat"), |field| {
        if let Some(n) = numstat.push(field) {
            total += 1;
            out.added += u64::from(n.added.unwrap_or(0));
            out.removed += u64::from(n.removed.unwrap_or(0));
            if counts.len() < MAX_FILES {
                counts.insert(n.path, (n.added, n.removed));
            }
        }
        true
    })?;
    if !ok {
        return Err(Problem::CommitNotFound);
    }
    let mut names = NameStatusParser::default();
    stream_z(dir, &args("--name-status"), |field| {
        if let Some((path, kind)) = names.push(field) {
            let (added, removed) = counts.get(&path).copied().unwrap_or((None, None));
            let binary = added.is_none() && counts.contains_key(&path);
            out.files.push(FileChange { path, kind, added, removed, mtime: 0, binary });
        }
        out.files.len() < MAX_FILES
    })?;
    out.more = u32::try_from(total.saturating_sub(out.files.len() as u64)).unwrap_or(u32::MAX);
    Ok(out)
}

/// What is not committed in the worktree (the Changes panel's list), at most `MAX_FILES`.
/// `has_head`: the branch has a commit to compare with.
fn uncommitted(dir: &Path, has_head: bool) -> Result<ReviewFiles, Problem> {
    let status = capture(dir, &os(&["status", "--porcelain=v1", "-z", "--untracked-files=all", "--", "."]))?.unwrap_or_default();
    let base = if has_head { "HEAD" } else { EMPTY_TREE };
    let numstat = capture(dir, &os(&["diff", base, "--numstat", "-z", "-M", "--no-ext-diff", "--no-textconv", "--", "."]))?.unwrap_or_default();
    let mut files = changes::file_changes(dir, changes::parse_porcelain(&status), &changes::parse_numstat(&numstat));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let total = files.len();
    let (added, removed) = files.iter().fold((0u64, 0u64), |(a, r), f| (a + u64::from(f.added.unwrap_or(0)), r + u64::from(f.removed.unwrap_or(0))));
    files.truncate(MAX_FILES);
    let more = u32::try_from(total - files.len()).unwrap_or(u32::MAX);
    Ok(ReviewFiles { files: files.into_iter().map(|f| FileChange { mtime: 0, ..f }).collect(), more, added, removed })
}

fn scope_files(dir: &Path, scope: &ReviewScope) -> Result<ReviewFiles, Problem> {
    match scope {
        ReviewScope::Uncommitted => uncommitted(dir, commit_of(dir, "HEAD")?.is_some()),
        _ => {
            let (from, to) = ends(dir, scope)?;
            files_between(dir, &from, &to)
        }
    }
}

// --- Diffs ------------------------------------------------------------------------------------

/// The diff of `path` (or of the whole scope), bounded to `MAX_DIFF` bytes.
fn diff(dir: &Path, scope: &ReviewScope, path: Option<&str>, old_path: Option<&str>) -> Result<(String, bool), Problem> {
    for p in [path, old_path].into_iter().flatten() {
        if !changes::safe_relative(p) {
            return Err(Problem::PathRefused);
        }
    }
    let paths: Vec<OsString> = [old_path, path].into_iter().flatten().map(|p| changes::name_path(p).into_os_string()).collect();
    let dashed = || [os(&["--"]), paths.clone()].concat();
    let args: Vec<OsString> = match (scope, path) {
        (ReviewScope::Uncommitted, Some(p)) => {
            let full = changes::name_path(p);
            let status = capture(dir, &[os(&["status", "--porcelain=v1", "-z", "--untracked-files=all", "--"]), vec![full.clone().into_os_string()]].concat())?.unwrap_or_default();
            let untracked = changes::parse_porcelain(&status).iter().any(|(f, k)| f == p && *k == ChangeKind::Untracked);
            if untracked {
                // `--no-index` reads what a symbolic link points to (a pipe would never end).
                if !changes::is_regular(&dir.join(&full)) {
                    return Err(Problem::NotARegularFile { path: p.to_string() });
                }
                [os(&["diff", "--no-index", "--no-color", "--no-ext-diff", "--no-textconv", "--", "/dev/null"]), vec![full.into_os_string()]].concat()
            } else {
                [os(&["diff", "HEAD"]), os(&PLAIN), dashed()].concat()
            }
        }
        (ReviewScope::Uncommitted, None) => [os(&["diff", "HEAD"]), os(&PLAIN)].concat(),
        (ReviewScope::Commit { .. }, None) => {
            let (_, sha) = ends(dir, scope)?;
            // The same header as the Changes panel's commit view; a merge shows against its first parent.
            let format = "--format=commit %H%nAuthor: %an <%ae>%nDate:   %ad%n%n%w(0,4,4)%B";
            [os(&["show"]), os(&PLAIN), os(&["--diff-merges=first-parent", "--stat", "--patch", format, "--date=format:%Y-%m-%d %H:%M", &sha])].concat()
        }
        (_, _) => {
            let (from, to) = ends(dir, scope)?;
            let mut args = [os(&["diff"]), os(&PLAIN), os(&[&from, &to])].concat();
            if !paths.is_empty() {
                args.extend(dashed());
            }
            args
        }
    };
    let (text, truncated) = read_bounded(dir, &args, MAX_DIFF)?;
    if text.is_empty() {
        return Err(Problem::NoDiff);
    }
    Ok((text, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numstat(out: &[u8]) -> Vec<Numstat> {
        let mut p = NumstatParser::default();
        out.split(|&b| b == 0).filter_map(|f| p.push(f)).collect()
    }

    fn name_status(out: &[u8]) -> Vec<(String, ChangeKind)> {
        let mut p = NameStatusParser::default();
        out.split(|&b| b == 0).filter_map(|f| p.push(f)).collect()
    }

    #[test]
    fn numstat_fields_with_renames_binaries_spaces_and_accents() {
        let out = "3\t1\tsrc/a.rs\0-\t-\tassets/logo.png\x000\t2\t\0docs/old name.md\0docs/new name.md\x005\t0\tnotes/\u{e9}t\u{e9} \u{1f600}.txt\0";
        let n = |path: &str, added, removed| Numstat { path: path.into(), added, removed };
        assert_eq!(
            numstat(out.as_bytes()),
            vec![n("src/a.rs", Some(3), Some(1)), n("assets/logo.png", None, None), n("docs/new name.md", Some(0), Some(2)), n("notes/\u{e9}t\u{e9} \u{1f600}.txt", Some(5), Some(0))]
        );
        assert!(numstat(b"").is_empty());
        // A tab in a path (allowed by git) stays in the path.
        assert_eq!(numstat(b"1\t0\ta\tb.txt\0"), vec![n("a\tb.txt", Some(1), Some(0))]);
        // A name that is not UTF-8: quoted, the exact bytes kept.
        assert_eq!(numstat(b"1\t0\tcaf\xe9.txt\0"), vec![n("\"caf\\351.txt\"", Some(1), Some(0))]);
    }

    #[test]
    fn name_status_fields_with_every_kind() {
        let out = "M\0src/a.rs\0A\0new file.rs\0D\0gone.rs\0R087\0docs/old name.md\0docs/new name.md\0C100\0a.rs\0copy.rs\0T\0link\0M\0\u{e9}t\u{e9}.md\0";
        assert_eq!(
            name_status(out.as_bytes()),
            vec![
                ("src/a.rs".into(), ChangeKind::Modified),
                ("new file.rs".into(), ChangeKind::Added),
                ("gone.rs".into(), ChangeKind::Deleted),
                ("docs/new name.md".into(), ChangeKind::Renamed("docs/old name.md".into())),
                ("copy.rs".into(), ChangeKind::Added),
                ("link".into(), ChangeKind::Modified),
                ("\u{e9}t\u{e9}.md".into(), ChangeKind::Modified),
            ]
        );
        assert_eq!(name_status(b"R100\0old\xff\0new\xfe\0"), vec![("\"new\\376\"".into(), ChangeKind::Renamed("\"old\\377\"".into()))]);
        assert!(name_status(b"").is_empty());
    }

    #[test]
    fn commits_with_their_merges() {
        let us = FIELD;
        let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        let out = format!("{a}{us}10{us}Me{us}{b} {c}{us}Merge branch 'x'\n{b}{us}5{us}Me{us}{c}{us}fix: x{us}y\nbad{us}1{us}x{us}{us}z\n");
        let (commits, merges) = parse_commits(&out);
        assert_eq!(commits.iter().map(|c| c.summary.as_str()).collect::<Vec<_>>(), vec!["Merge branch 'x'", "fix: x\u{1f}y"]);
        assert_eq!(merges, vec![a]);
    }

    #[test]
    fn bases_and_shas_that_could_pass_for_options_are_refused() {
        for ok in ["main", "origin/main", "feat/invoices", "release-1.2", "a1b2c3d4"] {
            assert!(base_name_ok(ok), "{ok}");
        }
        for bad in ["", "-x", "--output=/tmp/x", "main..evil", "main^", "HEAD~1", "a b", "@{-1}", "x:y", &"a".repeat(300)] {
            assert!(!base_name_ok(bad), "{bad}");
        }
        assert!(is_hex_sha("abcdef1") && is_hex_sha(&"a".repeat(40)));
        assert!(!is_hex_sha("abc") && !is_hex_sha("--all") && !is_hex_sha("main"));
    }

    #[test]
    fn only_the_last_request_of_a_kind_per_key_is_answered() {
        let req = |id, key: &str, ask| Request { id, key: key.into(), worktree: "/w".into(), ask };
        let summary = Ask::Summary { base: None };
        let diff = Ask::Diff { scope: ReviewScope::Uncommitted, path: None, old_path: None };
        assert!(req(1, "p", summary.clone()).superseded_by(&req(2, "p", Ask::Summary { base: Some("main".into()) })));
        assert!(!req(1, "p", summary.clone()).superseded_by(&req(2, "p", diff.clone())), "another kind");
        assert!(!req(1, "p", summary).superseded_by(&req(2, "q", Ask::Summary { base: None })), "another key");
        let cmd = AgentCmd::ReviewDiff { id: 4, key: "p".into(), worktree: "/w".into(), scope: ReviewScope::Uncommitted, path: None, old_path: None };
        assert_eq!(Request::of(cmd), Some(req(4, "p", diff)));
        assert_eq!(Request::of(AgentCmd::Unwatch { key: "p".into() }), None);
    }

    #[test]
    fn an_unwatched_key_is_refused() {
        let ev = answer(Request { id: 3, key: "k".into(), worktree: "/tmp".into(), ask: Ask::Summary { base: None } }, Err(Problem::NotAGitRepo));
        assert_eq!(ev, AgentEvent::Review { id: 3, key: "k".into(), summary: None, bases: vec![], problem: Some(Problem::NotAGitRepo) });
    }

    #[test]
    fn a_command_past_its_deadline_is_killed_with_its_children() {
        let started = std::time::Instant::now();
        // A shell whose child keeps the output open: the whole group must go.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & sleep 30"]);
        let got = run_with(cmd, Duration::from_millis(300), |out| {
            let mut buf = Vec::new();
            let _ = BufReader::new(out).read_to_end(&mut buf);
            (buf, false)
        });
        assert_eq!(got.err(), Some(Problem::ReviewTimedOut { secs: 1 }));
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        // In time: its output.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf ok"]);
        let (out, ok) = run_with(cmd, Duration::from_secs(10), |out| {
            let mut buf = Vec::new();
            let _ = BufReader::new(out).read_to_end(&mut buf);
            (buf, false)
        })
        .unwrap();
        assert_eq!((out.as_slice(), ok), (&b"ok"[..], true));
    }
}
