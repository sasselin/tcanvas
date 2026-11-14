//! Worktrees of the watched repositories: `git worktree list --porcelain`, then for each one
//! whether it has uncommitted changes, how far it is from the default branch, whether its branch
//! is merged, and its last commit time. Read-only and lock-free (`GIT_OPTIONAL_LOCKS=0`, like
//! every git call of the changes thread, which runs this on its own cadence).
//!
//! Cost of one pass over the watched folders: per repository (several groups watching the same
//! one share it), one `git worktree list` (reads a few files) and one `git for-each-ref` (default
//! branch). Distances, commit times and reflog checks are cached by commit, so a worktree whose
//! HEAD and the default branch did not move costs nothing. The dirty check is the expensive part
//! (a `git status` stats every tracked file): a watched worktree reuses the change scan's own
//! status; the others run `git status --untracked-files=normal` (no descent into untracked
//! folders) once per `DIRTY_EVERY` at most, `DIRTY_PER_PASS` per pass, oldest first, so the
//! thread stays free for diffs and folder listings.
//!
//! Merged branches without a worktree: one `git for-each-ref refs/heads/` per repository (no
//! history walk), then the same cached distance and reflog checks as the worktrees' branches, at
//! most `BRANCH_LOOKUPS_PER_PASS` uncached ones per pass (a repository with hundreds of old
//! branches fills its list over a few passes).

use std::collections::HashMap;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use protocol::{MergedBranch, Worktree, WorktreeList};

use super::changes::git_ok;

/// A worktree's dirty state is checked again after this long (sooner when its HEAD moves).
pub const DIRTY_EVERY: Duration = Duration::from_secs(10);
/// `git status` runs per pass, at most (the changes thread also answers diffs and listings).
pub const DIRTY_PER_PASS: usize = 3;
/// Uncached distance or reflog lookups for branches without a worktree, per pass.
pub const BRANCH_LOOKUPS_PER_PASS: usize = 20;

/// `git worktree list --porcelain`: one block per worktree, the main one first.
///
/// ```text
/// worktree /path/main
/// HEAD <sha>
/// branch refs/heads/main
///
/// worktree /path/linked
/// HEAD <sha>
/// detached
/// locked <reason, C-quoted when it has special characters>
/// prunable gitdir file points to non-existent location
/// ```
///
/// Without `-z` (git 2.34 has no `-z` here), a worktree path containing a newline cannot be
/// read back: such a worktree is listed under a truncated path.
pub fn parse_porcelain(out: &str) -> Vec<Worktree> {
    let mut list = Vec::new();
    let mut current: Option<Worktree> = None;
    for line in out.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            list.extend(current.take());
            current = Some(entry(path, list.is_empty()));
            continue;
        }
        let Some(wt) = current.as_mut() else { continue };
        let (word, rest) = match line.split_once(' ') {
            Some((w, r)) => (w, Some(r)),
            None => (line, None),
        };
        match word {
            // All zeros: a branch without any commit yet.
            "HEAD" => wt.head = rest.filter(|sha| is_sha(sha) && !sha.bytes().all(|b| b == b'0')).map(String::from),
            "branch" => wt.branch = rest.map(|r| r.strip_prefix("refs/heads/").unwrap_or(r).to_string()),
            "detached" => wt.detached = true,
            "bare" => wt.bare = true,
            "locked" => {
                wt.locked = true;
                wt.lock_reason = rest.filter(|r| !r.is_empty()).map(unquote_c);
            }
            "prunable" => wt.prunable = true,
            _ => {}
        }
    }
    list.extend(current);
    list
}

/// Undoes git's C-style quoting (`"a\nb \303\251"` -> `a<newline>b <U+00E9>`); unquoted text is
/// returned as is.
pub fn unquote_c(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else { return s.to_string() };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut it = inner.bytes().peekable();
    while let Some(b) = it.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match it.next() {
            Some(d @ b'0'..=b'7') => {
                let mut v = u32::from(d - b'0');
                for _ in 0..2 {
                    match it.peek() {
                        Some(&o @ b'0'..=b'7') => {
                            v = v * 8 + u32::from(o - b'0');
                            it.next();
                        }
                        _ => break,
                    }
                }
                bytes.push(v as u8);
            }
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'r') => bytes.push(b'\r'),
            Some(b'a') => bytes.push(0x07),
            Some(b'b') => bytes.push(0x08),
            Some(b'f') => bytes.push(0x0c),
            Some(b'v') => bytes.push(0x0b),
            Some(other) => bytes.push(other),
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn entry(path: &str, main: bool) -> Worktree {
    Worktree {
        path: path.to_string(),
        head: None,
        branch: None,
        detached: false,
        main,
        bare: false,
        locked: false,
        lock_reason: None,
        prunable: false,
        dirty: None,
        ahead: None,
        behind: None,
        merged: None,
        last_commit: None,
        branch_deletable: false,
        reflog_commits: false,
        new_branch: false,
    }
}

fn is_sha(s: &str) -> bool {
    s.len() >= 7 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The branch the worktrees are compared with.
#[derive(Debug, PartialEq, Eq)]
pub struct Base {
    /// Short name: `origin/main`, `main`, `master`, or the main checkout's branch.
    pub name: String,
    pub sha: String,
}

impl Base {
    /// The local branch it stands for (`origin/main` -> `main`): that branch is not "merged".
    pub fn local(&self) -> &str {
        self.name.strip_prefix("origin/").unwrap_or(&self.name)
    }
}

const ORIGIN_HEAD: &str = "refs/remotes/origin/HEAD";
const ORIGIN_MAIN: &str = "refs/remotes/origin/main";
const ORIGIN_MASTER: &str = "refs/remotes/origin/master";
const MAIN: &str = "refs/heads/main";
const MASTER: &str = "refs/heads/master";

/// Output of `git for-each-ref --format=%(refname)%09%(objectname)%09%(symref)` on the
/// candidate refs. In order: what `origin/HEAD` points to, `origin/main`, `origin/master` (no
/// `origin/HEAD` after `git remote add`), `main`, `master`, and last the main checkout's branch
/// (`main_checkout`: its branch and commit).
pub fn parse_default_branch(out: &str, main_checkout: Option<(&str, &str)>) -> Option<Base> {
    let rows: Vec<Vec<&str>> = out.lines().map(|l| l.split('\t').collect()).filter(|r: &Vec<&str>| r.len() >= 2 && is_sha(r[1])).collect();
    let find = |name: &str| rows.iter().find(|r| r[0] == name);
    let short = |r: &str| r.strip_prefix("refs/remotes/").or_else(|| r.strip_prefix("refs/heads/")).unwrap_or(r).to_string();
    if let Some(r) = find(ORIGIN_HEAD)
        && let Some(target) = r.get(2).filter(|t| !t.is_empty())
    {
        return Some(Base { name: short(target), sha: r[1].to_string() });
    }
    let named = [ORIGIN_MAIN, ORIGIN_MASTER, MAIN, MASTER].into_iter().find_map(|name| find(name).map(|r| Base { name: short(name), sha: r[1].to_string() }));
    named.or_else(|| main_checkout.filter(|(_, sha)| is_sha(sha)).map(|(branch, sha)| Base { name: branch.to_string(), sha: sha.to_string() }))
}

pub fn default_branch(dir: &Path, main_checkout: Option<(&str, &str)>) -> Option<Base> {
    let out = git_ok(dir, &["for-each-ref", "--format=%(refname)%09%(objectname)%09%(symref)", ORIGIN_HEAD, ORIGIN_MAIN, ORIGIN_MASTER, MAIN, MASTER]).unwrap_or_default();
    parse_default_branch(&out, main_checkout)
}

/// Whether a branch counts as merged into the default branch. Having no commit of its own
/// (`ahead == 0`) is not enough: a branch just created from the default branch has none either.
/// It must also have moved since it was created (`moved`: more than one entry in its reflog;
/// `None` when unknown, e.g. reflogs disabled or expired).
pub fn merged(branch: Option<&str>, base_local: &str, ahead: u32, moved: Option<bool>) -> Option<bool> {
    branch.filter(|b| *b != base_local)?;
    match (ahead, moved) {
        (0, Some(true)) => Some(true),
        (0, _) => None,
        _ => Some(false),
    }
}

/// A branch with no commit of its own that never moved since it was created: just made on a
/// commit the default branch already has (what keeps it from counting as merged, told apart
/// from "not merged"). Unknown reflog (`moved` is `None`): not told.
pub fn new_branch(branch: Option<&str>, base_local: &str, ahead: u32, moved: Option<bool>) -> bool {
    branch.is_some_and(|b| b != base_local) && ahead == 0 && moved == Some(false)
}

/// `git reflog show --format=%H -n 2 refs/heads/<branch>`: two lines or more, the branch moved
/// after it was created (a commit, a fast-forward, a reset).
pub fn moved_since_created(reflog: &str) -> bool {
    reflog.lines().filter(|l| !l.trim().is_empty()).count() >= 2
}

/// When a worktree's dirty state was last checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Check {
    /// Time since the last check (`None`: never checked).
    pub age: Option<Duration>,
    /// Its HEAD moved since that check.
    pub head_moved: bool,
}

/// Which worktrees get a `git status` this pass: those due (never checked, HEAD moved, or
/// checked `DIRTY_EVERY` ago), oldest first, at most `budget`.
pub fn due(checks: &[Check], budget: usize) -> Vec<usize> {
    let urgency = |c: &Check| match c.age {
        None => Duration::MAX,
        Some(_) if c.head_moved => Duration::MAX,
        Some(age) => age,
    };
    let mut due: Vec<usize> = (0..checks.len()).filter(|&i| urgency(&checks[i]) >= DIRTY_EVERY).collect();
    // Stable: equally urgent ones keep their order.
    due.sort_by_key(|&i| std::cmp::Reverse(urgency(&checks[i])));
    due.truncate(budget);
    due
}

struct Dirty {
    at: Instant,
    head: Option<String>,
    dirty: Option<bool>,
}

/// One generation of cached results.
#[derive(Default)]
struct Generation {
    /// (head, base) -> (ahead, behind).
    distance: HashMap<(String, String), (u32, u32)>,
    /// Commit -> commit time.
    times: HashMap<String, u64>,
    /// (branch, head) -> the branch moved since it was created.
    moved: HashMap<(String, String), Option<bool>>,
    /// Worktree path -> last dirty check.
    dirty: HashMap<String, Dirty>,
    /// (reflog file, its size and time, base) -> every commit it names is in the base.
    reflogs: HashMap<(PathBuf, Stamp, String), bool>,
}

/// Size and modification time of a reflog file: when they do not change, neither does it.
type Stamp = (u64, i64, i64);

fn stamp(path: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.mtime(), m.mtime_nsec()))
}

/// A reflog longer than this is not read: its commits count as not in the base.
pub const MAX_REFLOG: usize = 1000;

/// `git reflog show --format=%H`: the distinct commits it names, or `None` beyond `max`.
pub fn reflog_commits(out: &str, max: usize) -> Option<Vec<String>> {
    let mut shas: Vec<String> = out.lines().map(str::trim).filter(|l| is_sha(l)).map(String::from).collect();
    if shas.len() > max {
        return None;
    }
    shas.sort();
    shas.dedup();
    Some(shas)
}

/// Results kept from one pass to the next. What a whole pass did not use is dropped at the start
/// of the following one.
#[derive(Default)]
pub struct Cache {
    current: Generation,
    previous: Generation,
}

/// Looks `key` up in this generation, moving it from the previous one when found there.
fn lookup<'a, K: Eq + Hash + Clone, V>(current: &'a mut HashMap<K, V>, previous: &mut HashMap<K, V>, key: &K) -> Option<&'a V> {
    if !current.contains_key(key) {
        let value = previous.remove(key)?;
        current.insert(key.clone(), value);
    }
    current.get(key)
}

impl Cache {
    fn begin_pass(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }

    fn time(&mut self, sha: &str) -> Option<u64> {
        lookup(&mut self.current.times, &mut self.previous.times, &sha.to_string()).copied()
    }

    /// Times of the commits not cached yet: a single `git log` for all of them.
    fn fetch_times(&mut self, dir: &Path, shas: &[&str]) {
        let missing: Vec<&str> = shas.iter().copied().filter(|s| is_sha(s) && self.time(s).is_none()).collect();
        if missing.is_empty() {
            return;
        }
        let mut args = vec!["log", "--no-walk=unsorted", "--format=%H %ct"];
        args.extend(missing);
        for line in git_ok(dir, &args).unwrap_or_default().lines() {
            if let Some((sha, time)) = line.split_once(' ')
                && let Ok(time) = time.trim().parse()
            {
                self.current.times.insert(sha.to_string(), time);
            }
        }
    }

    /// (ahead, behind) of `head` relative to `base`.
    fn distance(&mut self, dir: &Path, head: &str, base: &str) -> Option<(u32, u32)> {
        let mut unlimited = usize::MAX;
        self.distance_within(dir, head, base, &mut unlimited)
    }

    /// Like `distance`, but a lookup that runs git spends one unit of `budget` (none left: `None`).
    fn distance_within(&mut self, dir: &Path, head: &str, base: &str, budget: &mut usize) -> Option<(u32, u32)> {
        let key = (head.to_string(), base.to_string());
        if let Some(d) = lookup(&mut self.current.distance, &mut self.previous.distance, &key) {
            return Some(*d);
        }
        if !is_sha(head) || !is_sha(base) || *budget == 0 {
            return None;
        }
        *budget -= 1;
        let d = if head == base { (0, 0) } else { parse_left_right(&git_ok(dir, &["rev-list", "--left-right", "--count", &format!("{base}...{head}")])?)? };
        self.current.distance.insert(key, d);
        Some(d)
    }

    /// The branch moved since it was created (its reflog only changes when its head does).
    fn moved(&mut self, dir: &Path, branch: &str, head: &str) -> Option<bool> {
        let mut unlimited = usize::MAX;
        self.moved_within(dir, branch, head, &mut unlimited)
    }

    /// Like `moved`, within a lookup budget (`None` when it is spent).
    fn moved_within(&mut self, dir: &Path, branch: &str, head: &str, budget: &mut usize) -> Option<bool> {
        let key = (branch.to_string(), head.to_string());
        if let Some(m) = lookup(&mut self.current.moved, &mut self.previous.moved, &key) {
            return *m;
        }
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let refname = format!("refs/heads/{branch}");
        let m = git_ok(dir, &["reflog", "show", "--format=%H", "-n", "2", &refname]).map(|out| moved_since_created(&out));
        self.current.moved.insert(key, m);
        m
    }

    /// Every commit the reflog of `refname` (read in `cwd`; `log`: its file, for the cache) names
    /// is an ancestor of `base`: `git rev-list <commits> --not <base>` is empty. `None` when it
    /// could not be told (git failed, the reflog is too long, or the lookup budget is spent).
    fn reflog_in_base(&mut self, cwd: &Path, refname: &str, log: Option<&Path>, base: &str, budget: &mut usize) -> Option<bool> {
        let key = log.and_then(|l| Some((l.to_path_buf(), stamp(l)?, base.to_string())));
        if let Some(k) = &key
            && let Some(v) = lookup(&mut self.current.reflogs, &mut self.previous.reflogs, k)
        {
            return Some(*v);
        }
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let limit = (MAX_REFLOG + 1).to_string();
        let out = git_ok(cwd, &["reflog", "show", "--format=%H", "-n", &limit, refname])?;
        let shas = reflog_commits(&out, MAX_REFLOG)?;
        let clean = shas.is_empty() || {
            let mut args = vec!["rev-list", "-n", "1"];
            args.extend(shas.iter().map(String::as_str));
            args.extend(["--not", base]);
            git_ok(cwd, &args)?.trim().is_empty()
        };
        if let Some(k) = key {
            self.current.reflogs.insert(k, clean);
        }
        Some(clean)
    }

    fn record_dirty(&mut self, path: &str, head: Option<String>, dirty: Option<bool>) {
        self.current.dirty.insert(path.to_string(), Dirty { at: Instant::now(), head, dirty });
    }

    fn check_of(&mut self, wt: &Worktree) -> Check {
        match lookup(&mut self.current.dirty, &mut self.previous.dirty, &wt.path) {
            Some(d) => Check { age: Some(d.at.elapsed()), head_moved: d.head != wt.head },
            None => Check { age: None, head_moved: false },
        }
    }

    fn dirty_of(&mut self, path: &str) -> Option<bool> {
        lookup(&mut self.current.dirty, &mut self.previous.dirty, &path.to_string()).and_then(|d| d.dirty)
    }
}

/// `git rev-list --left-right --count base...head`: `behind<TAB>ahead`.
pub fn parse_left_right(out: &str) -> Option<(u32, u32)> {
    let mut parts = out.split_whitespace().map(str::parse::<u32>);
    let (behind, ahead) = (parts.next()?.ok()?, parts.next()?.ok()?);
    Some((ahead, behind))
}

/// One watched folder, as the change scan just saw it.
pub struct Input {
    pub key: String,
    /// Top of the git worktree holding the folder (`None` outside git).
    pub top: Option<String>,
    /// The change scan's status covered that whole worktree: it has changes, or not.
    pub dirty: Option<bool>,
}

/// One repository, scanned once per pass.
pub struct Repo {
    pub base: Option<Base>,
    /// Without their dirty state (`scan_all` adds it).
    pub worktrees: Vec<Worktree>,
    pub merged_branches: Vec<MergedBranch>,
    /// Merged branches left out of `merged_branches` because their reflog names commits the
    /// base does not have (or that could not be checked).
    pub reflog_held: Vec<String>,
}

/// Where the repository's shared files are: the main checkout's `.git` (folder, or the folder a
/// `.git` file points to), or the bare repository itself.
fn common_dir(main: &Worktree) -> Option<PathBuf> {
    let root = Path::new(&main.path);
    if main.bare {
        return Some(root.to_path_buf());
    }
    gitdir_of(root)
}

/// The git folder of a checkout: its `.git` folder, or where its `.git` file points.
fn gitdir_of(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    Some(root.join(target))
}

/// Local and remote-tracking refs: `git for-each-ref
/// --format=%(refname)%09%(objectname)%09%(upstream) refs/heads/ refs/remotes/`.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Refs {
    /// Local branches in order: (name, head, upstream ref if one is configured).
    pub branches: Vec<(String, String, Option<String>)>,
    /// Every ref listed (full name) -> its commit.
    pub shas: HashMap<String, String>,
}

pub fn parse_refs(out: &str) -> Refs {
    let mut refs = Refs::default();
    for line in out.lines() {
        let mut f = line.split('\t');
        let (Some(refname), Some(sha)) = (f.next(), f.next()) else { continue };
        if !is_sha(sha) {
            continue;
        }
        refs.shas.insert(refname.to_string(), sha.to_string());
        if let Some(name) = refname.strip_prefix("refs/heads/").filter(|n| !n.is_empty()) {
            let upstream = f.next().filter(|u| !u.is_empty()).map(String::from);
            refs.branches.push((name.to_string(), sha.to_string(), upstream));
        }
    }
    refs
}

pub fn read_refs(top: &Path) -> Refs {
    parse_refs(&git_ok(top, &["for-each-ref", "--format=%(refname)%09%(objectname)%09%(upstream)", "refs/heads/", "refs/remotes/"]).unwrap_or_default())
}

/// What `git branch -d` checks a branch against: its upstream when that one exists, else the
/// HEAD of where it runs (the main checkout).
pub fn deletion_target<'a>(upstream: Option<&str>, refs: &'a Refs, main_head: Option<&'a str>) -> Option<&'a str> {
    match upstream.and_then(|u| refs.shas.get(u)) {
        Some(sha) => Some(sha),
        None => main_head,
    }
}

/// HEAD of the main checkout (for a bare repository, which `git worktree list` gives none, its
/// own HEAD).
pub fn main_head(top: &Path, worktrees: &[Worktree]) -> Option<String> {
    match worktrees.first() {
        Some(w) if w.head.is_some() => w.head.clone(),
        _ => git_ok(top, &["rev-parse", "--verify", "-q", "HEAD"]).map(|s| s.trim().to_string()).filter(|s| is_sha(s)),
    }
}

/// Local branches merged into `base` that no worktree has checked out (a prunable entry still
/// holds its branch: `git branch -d` would refuse it) and that `git branch -d` accepts, sorted
/// by name. Lookups not cached yet spend `budget`; a branch whose lookups do not fit is left out
/// of this pass.
fn merged_branches(top: &Path, base: &Base, worktrees: &[Worktree], refs: &Refs, main_head: Option<&str>, cache: &mut Cache, budget: &mut usize) -> (Vec<MergedBranch>, Vec<String>) {
    let common = worktrees.first().and_then(common_dir);
    let mut found: Vec<(String, String)> = Vec::new();
    let mut held = Vec::new();
    for (name, head, upstream) in &refs.branches {
        if name == base.local() || worktrees.iter().any(|w| w.branch.as_deref() == Some(name.as_str())) {
            continue;
        }
        let Some((ahead, _)) = cache.distance_within(top, head, &base.sha, budget) else { continue };
        let moved = if ahead == 0 { cache.moved_within(top, name, head, budget) } else { None };
        if merged(Some(name), base.local(), ahead, moved) != Some(true) {
            continue;
        }
        let Some(target) = deletion_target(upstream.as_deref(), refs, main_head) else { continue };
        if !cache.distance_within(top, head, target, budget).is_some_and(|(ahead, _)| ahead == 0) {
            continue;
        }
        let refname = format!("refs/heads/{name}");
        let log = common.as_ref().map(|c| c.join("logs").join(&refname));
        if cache.reflog_in_base(top, &refname, log.as_deref(), &base.sha, budget) == Some(true) {
            found.push((name.clone(), head.clone()));
        } else {
            held.push(name.clone());
        }
    }
    let heads: Vec<&str> = found.iter().map(|(_, h)| h.as_str()).collect();
    cache.fetch_times(top, &heads);
    let mut list: Vec<MergedBranch> = found.iter().map(|(name, head)| MergedBranch { name: name.clone(), last_commit: cache.time(head) }).collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    (list, held)
}

/// The repository holding `dir` as git sees it right now, with no cache and no lookup limit:
/// what the cleanup commands check before acting.
pub fn scan_fresh(dir: &Path) -> Option<Repo> {
    let mut unlimited = usize::MAX;
    scan_repo(dir, &mut Cache::default(), &mut unlimited)
}

fn scan_repo(top: &Path, cache: &mut Cache, budget: &mut usize) -> Option<Repo> {
    let mut worktrees = parse_porcelain(&git_ok(top, &["worktree", "list", "--porcelain"])?);
    worktrees.first()?;
    // git lists the linked ones in directory order: sort them so the list stays stable.
    worktrees[1..].sort_by(|a, b| a.path.cmp(&b.path));
    let main_checkout = worktrees[0].branch.as_deref().zip(worktrees[0].head.as_deref());
    let base = default_branch(top, main_checkout);
    let heads: Vec<&str> = worktrees.iter().filter_map(|w| w.head.as_deref()).collect();
    cache.fetch_times(top, &heads);
    for wt in &mut worktrees {
        let Some(head) = wt.head.clone() else { continue };
        wt.last_commit = cache.time(&head);
        if let Some(base) = &base
            && let Some((ahead, behind)) = cache.distance(top, &head, &base.sha)
        {
            (wt.ahead, wt.behind) = (Some(ahead), Some(behind));
            let branch = wt.branch.clone().filter(|b| b != base.local());
            // The reflog is read only when it decides something.
            let moved = branch.as_deref().filter(|_| ahead == 0).and_then(|b| cache.moved(top, b, &head));
            wt.merged = merged(branch.as_deref(), base.local(), ahead, moved);
            wt.new_branch = new_branch(branch.as_deref(), base.local(), ahead, moved);
        }
    }
    let refs = read_refs(top);
    let main_head = main_head(top, &worktrees);
    for wt in worktrees.iter_mut().filter(|w| w.merged == Some(true)) {
        let (Some(branch), Some(head)) = (wt.branch.as_deref(), wt.head.as_deref()) else { continue };
        let upstream = refs.branches.iter().find(|(name, ..)| name == branch).and_then(|(.., u)| u.as_deref());
        let target = deletion_target(upstream, &refs, main_head.as_deref()).map(String::from);
        wt.branch_deletable = target.and_then(|t| cache.distance(top, head, &t)).is_some_and(|(ahead, _)| ahead == 0);
    }
    // Commits only a reflog still holds: the branch's (made then reset away) and, for a linked
    // worktree, its own HEAD's (made on a detached HEAD there). Removing the worktree, or deleting
    // the branch, would drop them. Unknown counts as holding some.
    if let Some(base) = &base {
        let common = worktrees.first().and_then(common_dir);
        let mut unlimited = usize::MAX;
        for wt in worktrees.iter_mut().filter(|w| w.merged == Some(true) && !w.main) {
            let Some(branch) = wt.branch.clone() else { continue };
            let refname = format!("refs/heads/{branch}");
            let branch_log = common.as_ref().map(|c| c.join("logs").join(&refname));
            let branch_clean = cache.reflog_in_base(top, &refname, branch_log.as_deref(), &base.sha, &mut unlimited);
            let root = Path::new(&wt.path);
            let head_log = gitdir_of(root).map(|g| g.join("logs/HEAD"));
            let head_clean = if root.is_dir() { cache.reflog_in_base(root, "HEAD", head_log.as_deref(), &base.sha, &mut unlimited) } else { None };
            wt.reflog_commits = branch_clean != Some(true) || head_clean != Some(true);
        }
    }
    let (merged_branches, reflog_held) = base.as_ref().map(|b| merged_branches(top, b, &worktrees, &refs, main_head.as_deref(), cache, budget)).unwrap_or_default();
    Some(Repo { base, worktrees, merged_branches, reflog_held })
}

fn checkable(wt: &Worktree) -> bool {
    !wt.bare && !wt.prunable && Path::new(&wt.path).is_dir()
}

/// One pass over every watched folder: their worktree lists, in the order of `inputs`.
pub fn scan_all(inputs: &[Input], cache: &mut Cache) -> Vec<WorktreeList> {
    cache.begin_pass();
    // Folders of the same repository share one scan: a folder's top is one of its worktrees.
    let mut repos: Vec<Repo> = Vec::new();
    let mut of_input: Vec<Option<usize>> = Vec::new();
    let mut budget = BRANCH_LOOKUPS_PER_PASS;
    for input in inputs {
        let Some(top) = input.top.as_deref() else {
            of_input.push(None);
            continue;
        };
        let known = repos.iter().position(|r| r.worktrees.iter().any(|w| w.path == top));
        let index = known.or_else(|| {
            repos.push(scan_repo(Path::new(top), cache, &mut budget)?);
            Some(repos.len() - 1)
        });
        of_input.push(index);
    }

    // What the change scans already know, then a few `git status`, oldest first.
    for (input, repo) in inputs.iter().zip(&of_input) {
        if let (Some(top), Some(dirty), Some(r)) = (&input.top, input.dirty, repo)
            && let Some(wt) = repos[*r].worktrees.iter().find(|w| &w.path == top)
        {
            cache.record_dirty(top, wt.head.clone(), Some(dirty));
        }
    }
    let mut candidates: Vec<(String, Option<String>)> = Vec::new();
    for wt in repos.iter().flat_map(|r| &r.worktrees).filter(|w| checkable(w)) {
        if !candidates.iter().any(|(p, _)| p == &wt.path) {
            candidates.push((wt.path.clone(), wt.head.clone()));
        }
    }
    let checks: Vec<Check> = candidates.iter().map(|(path, head)| cache.check_of(&entry_with(path, head))).collect();
    for i in due(&checks, DIRTY_PER_PASS) {
        let (path, head) = &candidates[i];
        let dirty = git_ok(Path::new(path), &["status", "--porcelain", "-z", "--untracked-files=normal"]).map(|out| !out.is_empty());
        cache.record_dirty(path, head.clone(), dirty);
    }
    for wt in repos.iter_mut().flat_map(|r| &mut r.worktrees) {
        wt.dirty = if checkable(wt) { cache.dirty_of(&wt.path) } else { None };
    }

    inputs
        .iter()
        .zip(of_input)
        .map(|(input, repo)| {
            let mut list = WorktreeList { key: input.key.clone(), repo: None, default_branch: None, worktrees: Vec::new(), merged_branches: Vec::new() };
            if let Some(r) = repo.map(|r| &repos[r]) {
                list.repo = r.worktrees.first().map(|w| w.path.clone());
                list.default_branch = r.base.as_ref().map(|b| b.name.clone());
                list.worktrees = r.worktrees.clone();
                list.merged_branches = r.merged_branches.clone();
            }
            list
        })
        .collect()
}

fn entry_with(path: &str, head: &Option<String>) -> Worktree {
    Worktree { head: head.clone(), ..entry(path, false) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn porcelain_main_linked_and_detached() {
        let out = format!(
            "worktree /home/me/shop\nHEAD {A}\nbranch refs/heads/main\n\nworktree /home/me/shop/.claude/worktrees/invoices\nHEAD {B}\nbranch refs/heads/feat/invoices\n\nworktree /home/me/shop-try\nHEAD {B}\ndetached\n\n"
        );
        let list = parse_porcelain(&out);
        assert_eq!(list.len(), 3);
        let main = &list[0];
        assert_eq!((main.path.as_str(), main.head.as_deref(), main.branch.as_deref(), main.main, main.detached), ("/home/me/shop", Some(A), Some("main"), true, false));
        let linked = &list[1];
        assert_eq!((linked.path.as_str(), linked.branch.as_deref(), linked.main), ("/home/me/shop/.claude/worktrees/invoices", Some("feat/invoices"), false));
        let detached = &list[2];
        assert_eq!((detached.branch.as_deref(), detached.detached, detached.head.as_deref(), detached.main), (None, true, Some(B), false));
        assert!(list.iter().all(|w| !w.locked && !w.prunable && !w.bare && w.dirty.is_none()));
    }

    #[test]
    fn porcelain_locked_with_and_without_reason_and_prunable() {
        let out = format!(
            "worktree /r\nHEAD {A}\nbranch refs/heads/main\n\nworktree /r-a\nHEAD {A}\nbranch refs/heads/a\nlocked\n\nworktree /r-b\nHEAD {A}\nbranch refs/heads/b\nlocked on a USB drive\n\nworktree /r-c\nHEAD {A}\nbranch refs/heads/c\nlocked \"two\\nlines \\\"quoted\\\" \\303\\251\"\n\nworktree /gone\nHEAD {B}\ndetached\nprunable gitdir file points to non-existent location\n"
        );
        let list = parse_porcelain(&out);
        let flags: Vec<(bool, Option<&str>, bool)> = list.iter().map(|w| (w.locked, w.lock_reason.as_deref(), w.prunable)).collect();
        assert_eq!(flags, vec![(false, None, false), (true, None, false), (true, Some("on a USB drive"), false), (true, Some("two\nlines \"quoted\" \u{e9}"), false), (false, None, true)]);
    }

    #[test]
    fn c_quoted_text_is_unquoted() {
        assert_eq!(unquote_c("plain text"), "plain text");
        assert_eq!(unquote_c("\"tab\\there\\\\back\""), "tab\there\\back");
        assert_eq!(unquote_c("\"\\303\\251t\\303\\251\""), "\u{e9}t\u{e9}");
        assert_eq!(unquote_c("\"\""), "");
        assert_eq!(unquote_c("\"open"), "\"open", "not a quoted string");
    }

    #[test]
    fn porcelain_bare_unborn_and_paths_with_spaces() {
        let out = format!(
            "worktree /srv/git/shop.git\nbare\n\nworktree /home/me/my shop/new feature\nHEAD 0000000000000000000000000000000000000000\nbranch refs/heads/new feature\n\nworktree /home/me/x\nHEAD {A}\nbranch refs/heads/x\n"
        );
        let list = parse_porcelain(&out);
        assert_eq!(list.len(), 3);
        assert!(list[0].bare && list[0].main && list[0].head.is_none() && list[0].branch.is_none());
        assert_eq!((list[1].path.as_str(), list[1].branch.as_deref(), list[1].head.as_deref()), ("/home/me/my shop/new feature", Some("new feature"), None), "unborn branch: no head");
        assert!(!list[1].main && !list[1].bare);
        assert!(parse_porcelain("").is_empty());
        assert!(parse_porcelain("HEAD 1234567\n").is_empty(), "lines before any worktree are ignored");
    }

    #[test]
    fn default_branch_order() {
        let base = |name: &str, sha: &str| Some(Base { name: name.into(), sha: sha.into() });
        let origin = format!("{ORIGIN_HEAD}\t{A}\trefs/remotes/origin/trunk\n{ORIGIN_MAIN}\t{B}\t\n{MAIN}\t{B}\t\n");
        assert_eq!(parse_default_branch(&origin, None), base("origin/trunk", A), "origin/HEAD first");
        // No origin/HEAD (`git remote add` without `clone`): origin/main, then origin/master.
        assert_eq!(parse_default_branch(&format!("{ORIGIN_MAIN}\t{A}\t\n{ORIGIN_MASTER}\t{B}\t\n{MAIN}\t{B}\t\n"), None), base("origin/main", A));
        assert_eq!(parse_default_branch(&format!("{ORIGIN_MASTER}\t{B}\t\n{MAIN}\t{A}\t\n"), None), base("origin/master", B));
        assert_eq!(parse_default_branch(&format!("{MAIN}\t{B}\t\n{MASTER}\t{A}\t\n"), None), base("main", B));
        assert_eq!(parse_default_branch(&format!("{MASTER}\t{A}\t\n"), None), base("master", A));
        // None of them: the main checkout's branch.
        assert_eq!(parse_default_branch("", Some(("develop", A))), base("develop", A));
        assert_eq!(parse_default_branch(&format!("refs/heads/main/x\t{A}\t\n"), None), None, "only exact names");
        assert_eq!(parse_default_branch("", None), None);
        assert_eq!(Base { name: "origin/main".into(), sha: A.into() }.local(), "main");
    }

    #[test]
    fn merged_needs_a_branch_that_moved_since_its_creation() {
        // Just created from main: nothing of its own, but nothing merged either.
        assert_eq!(merged(Some("fresh"), "main", 0, Some(false)), None);
        assert_eq!(merged(Some("fresh"), "main", 0, None), None, "reflog unknown");
        // Committed on, then merged (fast-forward or not).
        assert_eq!(merged(Some("done"), "main", 0, Some(true)), Some(true));
        assert_eq!(merged(Some("wip"), "main", 2, Some(true)), Some(false));
        assert_eq!(merged(Some("wip"), "main", 2, None), Some(false));
        assert_eq!(merged(Some("main"), "main", 0, Some(true)), None, "the default branch itself");
        assert_eq!(merged(None, "main", 0, Some(true)), None, "detached");
        // Told apart from "not merged": just created, nothing of its own yet.
        assert!(new_branch(Some("fresh"), "main", 0, Some(false)));
        assert!(!new_branch(Some("fresh"), "main", 0, None), "reflog unknown: not told");
        assert!(!new_branch(Some("done"), "main", 0, Some(true)), "moved: merged");
        assert!(!new_branch(Some("wip"), "main", 2, Some(false)), "commits of its own");
        assert!(!new_branch(Some("main"), "main", 0, Some(false)), "the default branch itself");
        assert!(!new_branch(None, "main", 0, Some(false)), "detached");
        assert!(!moved_since_created(&format!("{A}\n")));
        assert!(moved_since_created(&format!("{B}\n{A}\n")));
        assert!(!moved_since_created(""));
    }

    #[test]
    fn reflog_commits_are_read_distinct_and_bounded() {
        assert_eq!(reflog_commits(&format!("{A}\n{B}\n{A}\n\n"), 10), Some(vec![A.to_string(), B.to_string()]));
        assert_eq!(reflog_commits("", 10), Some(vec![]));
        assert_eq!(reflog_commits(&format!("{A}\n{B}\n{A}\n"), 2), None, "too long to read: unknown");
    }

    #[test]
    fn refs_and_what_branch_d_checks() {
        let out = format!(
            "refs/heads/main\t{A}\trefs/remotes/origin/main\nrefs/heads/feat/a b\t{B}\t\nrefs/heads/gone\t{B}\trefs/remotes/origin/gone\nrefs/remotes/origin/main\t{B}\t\nrefs/heads/broken\tnot-a-sha\t\n"
        );
        let refs = parse_refs(&out);
        let names: Vec<(&str, &str, Option<&str>)> = refs.branches.iter().map(|(n, h, u)| (n.as_str(), h.as_str(), u.as_deref())).collect();
        assert_eq!(names, vec![("main", A, Some("refs/remotes/origin/main")), ("feat/a b", B, None), ("gone", B, Some("refs/remotes/origin/gone"))]);
        assert_eq!(refs.shas.get("refs/remotes/origin/main").map(String::as_str), Some(B));
        // Its upstream when it exists, else the main checkout's HEAD.
        assert_eq!(deletion_target(Some("refs/remotes/origin/main"), &refs, Some(A)), Some(B));
        assert_eq!(deletion_target(None, &refs, Some(A)), Some(A));
        assert_eq!(deletion_target(Some("refs/remotes/origin/gone"), &refs, Some(A)), Some(A), "upstream gone");
        assert_eq!(deletion_target(None, &refs, None), None);
        assert!(parse_refs("").branches.is_empty());
    }

    #[test]
    fn left_right_counts_are_behind_then_ahead() {
        assert_eq!(parse_left_right("12\t6\n"), Some((6, 12)));
        assert_eq!(parse_left_right("0\t0"), Some((0, 0)));
        assert_eq!(parse_left_right("x"), None);
    }

    #[test]
    fn dirty_checks_are_staggered_oldest_first() {
        let s = Duration::from_secs;
        let checks = [
            Check { age: Some(s(3)), head_moved: false },  // fresh: not due
            Check { age: Some(s(12)), head_moved: false }, // due
            Check { age: None, head_moved: false },        // never checked
            Check { age: Some(s(30)), head_moved: false }, // due, older
            Check { age: Some(s(1)), head_moved: true },   // HEAD moved
            Check { age: Some(s(10)), head_moved: false }, // just due
        ];
        assert_eq!(due(&checks, 3), vec![2, 4, 3]);
        assert_eq!(due(&checks, 10), vec![2, 4, 3, 1, 5]);
        assert!(due(&checks, 0).is_empty());
        assert!(due(&[Check { age: Some(s(2)), head_moved: false }], 3).is_empty());
    }

    #[test]
    fn folders_outside_git_have_no_worktrees() {
        let lists = scan_all(&[Input { key: "k".into(), top: None, dirty: None }], &mut Cache::default());
        assert_eq!((lists[0].key.as_str(), lists[0].repo.as_deref(), lists[0].worktrees.len()), ("k", None, 0));
    }

    #[test]
    fn the_cache_keeps_what_the_last_pass_used() {
        let mut c = Cache::default();
        c.current.times.insert(A.into(), 1);
        c.current.times.insert(B.into(), 2);
        c.begin_pass();
        assert_eq!(c.time(A), Some(1), "found in the previous pass");
        c.begin_pass();
        assert_eq!((c.time(A), c.time(B)), (Some(1), None), "B was not used last pass: dropped");
    }
}
