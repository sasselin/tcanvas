//! Modified files of the folders linked to projects: list (with +/− relative to HEAD) and diffs.
//! Runs on its own thread: `git` never delays watching the terminals.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::{AgentEvent, ChangeKind, ChangeSet, CommitInfo, DirEntry, DirListing, FileChange, Problem, WorktreeList};

use super::{cleanup, create, tmux, worktrees};

const SCAN_EVERY: Duration = Duration::from_secs(2);
const MAX_FILES: usize = 500;
/// Recent commits shown in the panel.
const MAX_COMMITS: usize = 20;
/// Walking a non-git folder stops beyond this many entries.
const MAX_WALK: usize = 20_000;
const MAX_DIFF: usize = 512 * 1024;
/// Bytes read to count the lines of an untracked file.
const MAX_COUNT: u64 = 1024 * 1024;
/// git's empty tree: diff base for a repository without commits.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// Folders skipped outside git (dependencies, build outputs).
const SKIP_DIRS: [&str; 4] = ["node_modules", "target", "dist", "build"];

pub enum Cmd {
    Watch {
        key: String,
        path: String,
    },
    Unwatch {
        key: String,
    },
    Diff {
        key: String,
        file: String,
    },
    ShowCommit {
        key: String,
        sha: String,
    },
    List {
        path: String,
    },
    Cleanup {
        key: String,
        action: protocol::Cleanup,
    },
    Sizes {
        key: String,
        paths: Vec<String>,
    },
    PlanWorktree {
        id: u64,
        key: String,
        folder: String,
        location: Option<String>,
    },
    CreateWorktree {
        id: u64,
        key: String,
        request: protocol::CreateWorktree,
    },
    /// Scan again now (a cleanup just changed the repository).
    Rescan,
}

/// A command that writes, run on the cleanup thread (one at a time).
enum Job {
    Cleanup { key: String, dir: PathBuf, action: protocol::Cleanup },
    Create { id: u64, key: String, dir: PathBuf, request: protocol::CreateWorktree },
}

/// A worktree to measure on the size thread.
struct SizeJob {
    key: String,
    dir: PathBuf,
    path: String,
}

/// Where events go (shared with the cleanup and size threads).
type Sink = Arc<dyn Fn(&AgentEvent) + Send + Sync>;
/// The watched folders as given (resolved when used), shared with the cleanup thread.
type WatchedPaths = Arc<Mutex<Vec<String>>>;
/// (key, path) queued or being measured: asking again adds nothing.
type Measuring = Arc<Mutex<HashSet<(String, String)>>>;

struct Watched {
    path: String,
    /// Start of watching: outside git, only files modified since then count.
    since: u64,
    last_sent: Option<ChangeSet>,
    last_worktrees: Option<WorktreeList>,
}

/// Starts the watcher thread. Produced events go through `send`.
pub fn spawn(send: impl Fn(&AgentEvent) + Send + Sync + 'static) -> Sender<Cmd> {
    let (tx, rx) = mpsc::channel();
    let send: Sink = Arc::new(send);
    let paths: WatchedPaths = Arc::default();
    let measuring: Measuring = Arc::default();
    let workers = Workers { jobs: spawn_cleaner(send.clone(), tx.clone(), paths.clone()), sizes: spawn_measurer(send.clone(), measuring.clone()), paths, measuring };
    thread::spawn(move || run(rx, send, workers));
    tx
}

/// The cleanup and size threads, and what they share with the scan thread.
struct Workers {
    jobs: Sender<Job>,
    sizes: Sender<SizeJob>,
    paths: WatchedPaths,
    measuring: Measuring,
}

/// Sizes are measured one folder at a time on their own thread (a big folder takes a while), each
/// walk bounded (`cleanup::WALK_MAX_*`, then the size is unknown); a path already queued or being
/// measured for the same key is not queued again.
fn spawn_measurer(send: Sink, measuring: Measuring) -> Sender<SizeJob> {
    let (tx, rx) = mpsc::channel::<SizeJob>();
    thread::spawn(move || {
        for job in rx {
            let size = cleanup::measure(&job.dir, &job.path);
            if let Ok(mut m) = measuring.lock() {
                m.remove(&(job.key.clone(), job.path.clone()));
            }
            send(&AgentEvent::WorktreeSizes { key: job.key, sizes: vec![size] });
        }
    });
    tx
}

/// Cleanups and worktree creations run one at a time on their own thread (deleting a big folder,
/// or checking out a big one, takes a while; diffs and scans go on meanwhile), then ask for a scan
/// so the lists follow right away. What uses the folders (panes, processes, watched folders) is
/// read by `cleanup::run` right before it writes, after its slow checks.
fn spawn_cleaner(send: Sink, rescan: Sender<Cmd>, paths: WatchedPaths) -> Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    thread::spawn(move || {
        for job in rx {
            match job {
                Job::Cleanup { key, dir, action } => {
                    let in_use = || {
                        let raw = paths.lock().map(|p| p.clone()).unwrap_or_default();
                        let watched = raw.iter().filter_map(|p| resolve(p).ok()).map(|d| d.to_string_lossy().into_owned()).collect();
                        cleanup::InUse { panes: tmux::pane_paths(), processes: cleanup::process_cwds(), watched }
                    };
                    let (deleted_branch, problem) = match cleanup::run(&dir, &action, &in_use) {
                        Ok(branch) => (branch, None),
                        Err(p) => (None, Some(p)),
                    };
                    send(&AgentEvent::Cleaned { key, action, deleted_branch, problem });
                }
                Job::Create { id, key, dir, request } => {
                    // Still running past the time limit: the final answer, with the same id,
                    // follows when git ends, from another thread (the next jobs do not wait).
                    let (late_send, late_rescan, late_key) = (send.clone(), rescan.clone(), key.clone());
                    let late = move |result| {
                        late_send(&created_event(id, late_key, result));
                        let _ = late_rescan.send(Cmd::Rescan);
                    };
                    send(&created_event(id, key, create::run(&dir, &request, late)));
                }
            }
            let _ = rescan.send(Cmd::Rescan);
        }
    });
    tx
}

fn created_event(id: u64, key: String, result: create::Created) -> AgentEvent {
    match result {
        Ok((path, excluded)) => AgentEvent::WorktreeCreated { id, key, path: Some(path), problem: None, excluded },
        Err(problem) => AgentEvent::WorktreeCreated { id, key, path: None, problem: Some(problem), excluded: false },
    }
}

/// A plan request for which a newer one of the same key is already waiting: its answer would be
/// stale before it is sent, so it is dropped (the app only takes the answer to its last one).
fn superseded(cmd: &Cmd, waiting: &VecDeque<Cmd>) -> bool {
    let Cmd::PlanWorktree { key, .. } = cmd else { return false };
    waiting.iter().any(|c| matches!(c, Cmd::PlanWorktree { key: k, .. } if k == key))
}

fn run(rx: Receiver<Cmd>, send: Sink, workers: Workers) {
    let mut watched: HashMap<String, Watched> = HashMap::new();
    let mut cache = worktrees::Cache::default();
    let mut next_scan = Instant::now();
    // Commands taken from the channel early, to look for newer plan requests.
    let mut waiting: VecDeque<Cmd> = VecDeque::new();
    loop {
        let received = match waiting.pop_front() {
            Some(cmd) => Ok(cmd),
            None => rx.recv_timeout(next_scan.saturating_duration_since(Instant::now())),
        };
        if let Ok(cmd @ Cmd::PlanWorktree { .. }) = &received {
            waiting.extend(rx.try_iter());
            if superseded(cmd, &waiting) {
                continue;
            }
        }
        match received {
            Ok(Cmd::Watch { key, path }) => {
                if watched.get(&key).is_none_or(|w| w.path != path) {
                    watched.insert(key.clone(), Watched { path, since: unix_now(), last_sent: None, last_worktrees: None });
                    share_paths(&watched, &workers.paths);
                }
                // Immediate response (folder validation): rescan right away.
                next_scan = Instant::now();
            }
            Ok(Cmd::Unwatch { key }) => {
                watched.remove(&key);
                share_paths(&watched, &workers.paths);
            }
            Ok(Cmd::Diff { key, file }) => {
                if let Some(w) = watched.get(&key) {
                    send(&diff_event(key, file.clone(), diff(w, &file)));
                }
            }
            Ok(Cmd::ShowCommit { key, sha }) => {
                if let Some(w) = watched.get(&key) {
                    send(&diff_event(key, sha.clone(), show_commit(w, &sha)));
                }
            }
            Ok(Cmd::List { path }) => send(&AgentEvent::DirListing(list_dir(&path))),
            Ok(Cmd::Cleanup { key, action }) => match watched.get(&key).map(|w| resolve(&w.path)) {
                Some(Ok(dir)) => {
                    let _ = workers.jobs.send(Job::Cleanup { key, dir, action });
                }
                Some(Err(problem)) => send(&AgentEvent::Cleaned { key, action, deleted_branch: None, problem: Some(problem) }),
                None => send(&AgentEvent::Cleaned { key, action, deleted_branch: None, problem: Some(Problem::NotAGitRepo) }),
            },
            Ok(Cmd::Sizes { key, paths }) => {
                // Walking folders takes a while: off this thread, and only on request.
                if let Some(Ok(dir)) = watched.get(&key).map(|w| resolve(&w.path)) {
                    for path in paths {
                        let new = workers.measuring.lock().is_ok_and(|mut m| m.insert((key.clone(), path.clone())));
                        if new {
                            let _ = workers.sizes.send(SizeJob { key: key.clone(), dir: dir.clone(), path });
                        }
                    }
                }
            }
            Ok(Cmd::PlanWorktree { id, key, folder, location }) => {
                // A few quick read-only git calls: answered right here.
                let plan = match watched.get(&key).map(|w| resolve(&w.path)) {
                    Some(Ok(dir)) => create::plan(&dir, id, &key, &folder, location.as_deref()),
                    Some(Err(problem)) => create::refused_plan(id, &key, &folder, location.as_deref(), problem),
                    None => create::refused_plan(id, &key, &folder, location.as_deref(), Problem::NotAGitRepo),
                };
                send(&AgentEvent::WorktreePlan(Box::new(plan)));
            }
            Ok(Cmd::CreateWorktree { id, key, request }) => match watched.get(&key).map(|w| resolve(&w.path)) {
                Some(Ok(dir)) => {
                    let _ = workers.jobs.send(Job::Create { id, key, dir, request });
                }
                Some(Err(problem)) => send(&AgentEvent::WorktreeCreated { id, key, path: None, problem: Some(problem), excluded: false }),
                None => send(&AgentEvent::WorktreeCreated { id, key, path: None, problem: Some(Problem::NotAGitRepo), excluded: false }),
            },
            Ok(Cmd::Rescan) => next_scan = Instant::now(),
            Err(RecvTimeoutError::Timeout) => {
                let mut inputs = Vec::new();
                for (key, w) in watched.iter_mut() {
                    let set = scan(key, &w.path, w.since);
                    // A git folder's top is one of its repository's worktrees. When the folder is
                    // that top, the status just run covers the whole worktree: no second one.
                    let top = set.root.clone().filter(|_| set.git);
                    let whole = top.is_some() && resolve(&w.path).ok().map(|d| d.to_string_lossy().into_owned()) == top;
                    inputs.push(worktrees::Input { key: key.clone(), top, dirty: whole.then_some(!set.files.is_empty()) });
                    if w.last_sent.as_ref() != Some(&set) {
                        send(&AgentEvent::Changes(set.clone()));
                        w.last_sent = Some(set);
                    }
                }
                // Stable order: which groups share a repository scan does not depend on hashing.
                inputs.sort_by(|a, b| a.key.cmp(&b.key));
                for list in worktrees::scan_all(&inputs, &mut cache) {
                    if let Some(w) = watched.get_mut(&list.key)
                        && w.last_worktrees.as_ref() != Some(&list)
                    {
                        send(&AgentEvent::Worktrees(list.clone()));
                        w.last_worktrees = Some(list);
                    }
                }
                next_scan = Instant::now() + SCAN_EVERY;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn share_paths(watched: &HashMap<String, Watched>, shared: &WatchedPaths) {
    if let Ok(mut paths) = shared.lock() {
        *paths = watched.values().map(|w| w.path.clone()).collect();
    }
}

/// `~` and `~/x` → home folder. Other paths are returned unchanged.
pub fn expand_tilde(path: &str, home: Option<&str>) -> String {
    let path = path.trim();
    match (path, home) {
        ("~", Some(h)) => h.to_string(),
        (p, Some(h)) if p.starts_with("~/") => format!("{}/{}", h.trim_end_matches('/'), &p[2..]),
        (p, _) => p.to_string(),
    }
}

/// Absolute path of an existing folder.
pub fn resolve(path: &str) -> Result<PathBuf, Problem> {
    let expanded = expand_tilde(path, std::env::var("HOME").ok().as_deref());
    match fs::canonicalize(&expanded) {
        Ok(p) if p.is_dir() => Ok(p),
        Ok(_) => Err(Problem::NotADir { path: path.into() }),
        Err(_) => Err(Problem::DirNotFound { path: path.into() }),
    }
}

/// Response to a diff request: the text, or the problem preventing it.
fn diff_event(key: String, file: String, result: Result<(String, bool), Problem>) -> AgentEvent {
    match result {
        Ok((text, truncated)) => AgentEvent::Diff { key, file, text, truncated, problem: None },
        Err(p) => AgentEvent::Diff { key, file, text: String::new(), truncated: false, problem: Some(p) },
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<Output> {
    // No optional lock on the index: never get in the way of the user's (or Claude's) git.
    Command::new("git").arg("-C").arg(dir).args(args).env("GIT_OPTIONAL_LOCKS", "0").output().ok()
}

pub fn git_ok(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args).filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

struct Repo {
    top: PathBuf,
    /// HEAD, or the empty tree if there are no commits.
    base: String,
}

fn repo(dir: &Path) -> Option<Repo> {
    let top = git_ok(dir, &["rev-parse", "--show-toplevel"])?;
    let base = if git_ok(dir, &["rev-parse", "--verify", "-q", "HEAD"]).is_some() { "HEAD".into() } else { EMPTY_TREE.into() };
    Some(Repo { top: PathBuf::from(top.trim_end()), base })
}

pub fn scan(key: &str, path: &str, since: u64) -> ChangeSet {
    let mut set = ChangeSet { key: key.into(), root: None, git: false, branch: None, detached: false, error: None, files: Vec::new(), truncated: false, commits: Vec::new() };
    let dir = match resolve(path) {
        Ok(d) => d,
        Err(e) => {
            set.error = Some(e);
            return set;
        }
    };
    let mut files = match repo(&dir) {
        Some(repo) => {
            set.git = true;
            set.branch = git_ok(&dir, &["symbolic-ref", "--short", "-q", "HEAD"]).map(|b| b.trim().to_string());
            set.detached = set.branch.is_none();
            set.root = Some(repo.top.to_string_lossy().into_owned());
            set.commits = recent_commits(&dir);
            git_files(&dir, &repo)
        }
        None => {
            set.root = Some(dir.to_string_lossy().into_owned());
            walk_recent(&dir, since)
        }
    };
    files.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
    set.truncated = files.len() > MAX_FILES;
    files.truncate(MAX_FILES);
    set.files = files;
    set
}

fn git_files(dir: &Path, repo: &Repo) -> Vec<FileChange> {
    let Some(status) = git(dir, &["status", "--porcelain=v1", "-z", "--untracked-files=all", "--", "."]) else { return Vec::new() };
    let entries = parse_porcelain(&String::from_utf8_lossy(&status.stdout));
    let numstat = git(dir, &["diff", &repo.base, "--numstat", "-z", "-M", "--no-ext-diff", "--", "."]).map(|o| parse_numstat(&String::from_utf8_lossy(&o.stdout))).unwrap_or_default();
    entries
        .into_iter()
        .map(|(path, kind)| {
            let full = repo.top.join(&path);
            let (added, removed, binary) = match (&kind, numstat.get(&path)) {
                (_, Some(&(a, r))) => (a, r, a.is_none()),
                (ChangeKind::Untracked, None) => match count_lines(&full) {
                    Some(n) => (Some(n), Some(0), false),
                    None => (None, None, true),
                },
                _ => (None, None, false),
            };
            FileChange { mtime: mtime(&full), path, kind, added, removed, binary }
        })
        .collect()
}

/// Field separator for the `git log` format (the "unit separator" character).
const FIELD: char = '\u{1f}';

fn recent_commits(dir: &Path) -> Vec<CommitInfo> {
    let format = format!("--format=%H{FIELD}%ct{FIELD}%an{FIELD}%s");
    let n = format!("-n{MAX_COMMITS}");
    git_ok(dir, &["log", &n, &format, "--no-color"]).map(|out| parse_log(&out)).unwrap_or_default()
}

/// Output of `git log --format=%H<US>%ct<US>%an<US>%s`: one commit per line.
pub fn parse_log(out: &str) -> Vec<CommitInfo> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.splitn(4, FIELD);
            let (sha, time, author, summary) = (f.next()?, f.next()?, f.next()?, f.next().unwrap_or_default());
            (sha.len() >= 7 && sha.bytes().all(|b| b.is_ascii_hexdigit())).then(|| CommitInfo { sha: sha.into(), summary: summary.into(), author: author.into(), time: time.parse().unwrap_or(0) })
        })
        .collect()
}

/// `git show` of a commit: message, then each file's patch.
fn show_commit(w: &Watched, sha: &str) -> Result<(String, bool), Problem> {
    // A sha only: nothing that could pass for an option or a range.
    if sha.len() < 7 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Problem::CommitRefused);
    }
    let dir = resolve(&w.path)?;
    // Header in standard git format: git's output is not translated.
    let out = git(&dir, &["show", "--no-color", "--no-ext-diff", "-M", "--stat", "--patch", "--format=commit %H%nAuthor: %an <%ae>%nDate:   %ad%n%n%w(0,4,4)%B", "--date=format:%Y-%m-%d %H:%M", sha]);
    match out.filter(|o| o.status.success()) {
        Some(o) => Ok(truncate(String::from_utf8_lossy(&o.stdout).into_owned(), MAX_DIFF)),
        None => Err(Problem::CommitNotFound),
    }
}

/// `git status --porcelain=v1 -z`: `XY path\0`, followed by `origin\0` for a rename or a copy.
pub fn parse_porcelain(out: &str) -> Vec<(String, ChangeKind)> {
    let mut fields = out.split('\0').filter(|f| !f.is_empty());
    let mut entries = Vec::new();
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue;
        }
        let (xy, path) = (&entry[..2], entry[3..].to_string());
        let (x, y) = (xy.as_bytes()[0], xy.as_bytes()[1]);
        let kind = if xy == "??" {
            ChangeKind::Untracked
        } else if x == b'R' || x == b'C' {
            let from = fields.next().unwrap_or_default().to_string();
            if x == b'R' { ChangeKind::Renamed(from) } else { ChangeKind::Added }
        } else if x == b'D' || y == b'D' {
            ChangeKind::Deleted
        } else if x == b'A' {
            ChangeKind::Added
        } else {
            ChangeKind::Modified
        };
        entries.push((path, kind));
    }
    entries
}

/// `git diff --numstat -z`: `added\tremoved\tpath\0`, or `added\tremoved\t\0origin\0target\0`
/// for a rename. `-` for a binary (→ `None`). Key: target path.
pub fn parse_numstat(out: &str) -> HashMap<String, (Option<u32>, Option<u32>)> {
    let mut map = HashMap::new();
    let mut fields = out.split('\0');
    while let Some(entry) = fields.next() {
        let mut parts = entry.splitn(3, '\t');
        let (Some(a), Some(r), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let path = if path.is_empty() {
            let _from = fields.next();
            fields.next().unwrap_or_default()
        } else {
            path
        };
        map.insert(path.to_string(), (a.parse().ok(), r.parse().ok()));
    }
    map
}

/// Line count of a text file; `None` for a binary or unreadable file.
fn count_lines(path: &Path) -> Option<u32> {
    let mut buf = Vec::new();
    fs::File::open(path).ok()?.take(MAX_COUNT).read_to_end(&mut buf).ok()?;
    if buf.contains(&0) {
        return None;
    }
    let n = buf.iter().filter(|&&b| b == b'\n').count() + usize::from(buf.last().is_some_and(|&b| b != b'\n'));
    Some(n as u32)
}

fn mtime(path: &Path) -> u64 {
    fs::symlink_metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

/// Outside git: files modified since `since` (start of watching).
fn walk_recent(root: &Path, since: u64) -> Vec<FileChange> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_WALK {
                return files;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(ty) = entry.file_type() else { continue };
            if ty.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_ref()) {
                    stack.push(entry.path());
                }
            } else if ty.is_file() {
                let path = entry.path();
                let m = mtime(&path);
                if m >= since {
                    let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
                    files.push(FileChange { path: rel, kind: ChangeKind::Modified, added: None, removed: None, mtime: m, binary: false });
                }
            }
        }
    }
    files
}

/// Beyond this, a folder listing is truncated (huge cache folder…).
const MAX_DIRS: usize = 2000;

/// Subfolders of `path` for the picker: visible ones only, sorted case-insensitively.
pub fn list_dir(path: &str) -> DirListing {
    let mut listing = DirListing { request: path.into(), path: None, parent: None, git: false, worktree: false, dirs: Vec::new(), error: None };
    let dir = match resolve(if path.trim().is_empty() { "~" } else { path }) {
        Ok(d) => d,
        Err(e) => {
            listing.error = Some(e);
            return listing;
        }
    };
    (listing.git, listing.worktree) = git_marker(&dir);
    listing.parent = dir.parent().map(|p| p.to_string_lossy().into_owned());
    match fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                // `metadata` follows symlinks: a link to a folder counts as a folder.
                if name.starts_with('.') || !entry.path().metadata().is_ok_and(|m| m.is_dir()) {
                    continue;
                }
                let (git, worktree) = git_marker(&entry.path());
                listing.dirs.push(DirEntry { git, worktree, name });
                if listing.dirs.len() >= MAX_DIRS {
                    break;
                }
            }
            listing.dirs.sort_by_key(|d| d.name.to_lowercase());
        }
        Err(e) => listing.error = Some(Problem::Unreadable { detail: e.to_string() }),
    }
    listing.path = Some(dir.to_string_lossy().into_owned());
    listing
}

/// (holds a repository, is a linked worktree): a repository has a `.git` folder; a linked
/// worktree and a submodule both have a `.git` file, told apart by where it points.
fn git_marker(dir: &Path) -> (bool, bool) {
    let git = dir.join(".git");
    match fs::symlink_metadata(&git) {
        Ok(m) if m.is_file() => (true, fs::read_to_string(&git).is_ok_and(|text| is_worktree_gitdir(&text))),
        Ok(_) => (true, false),
        Err(_) => (false, false),
    }
}

/// Contents of a `.git` file: `gitdir: <repo>/.git/worktrees/<name>` for a linked worktree,
/// `gitdir: <...>/.git/modules/<name>` for a submodule.
pub fn is_worktree_gitdir(text: &str) -> bool {
    let Some(target) = text.lines().find_map(|l| l.strip_prefix("gitdir:")) else { return false };
    let target = Path::new(target.trim());
    target.parent().and_then(Path::file_name).is_some_and(|name| name == "worktrees")
}

/// A relative path that does not escape the repository.
fn safe_relative(file: &str) -> bool {
    let p = Path::new(file);
    !file.is_empty() && p.is_relative() && !p.components().any(|c| matches!(c, std::path::Component::ParentDir))
}

fn diff(w: &Watched, file: &str) -> Result<(String, bool), Problem> {
    if !safe_relative(file) {
        return Err(Problem::PathRefused);
    }
    let dir = resolve(&w.path)?;
    let repo = repo(&dir).ok_or(Problem::NotAGitRepo)?;
    let entry = w.last_sent.as_ref().and_then(|s| s.files.iter().find(|f| f.path == file));
    let out = match entry.map(|f| &f.kind) {
        Some(ChangeKind::Untracked) => git(&repo.top, &["diff", "--no-index", "--no-color", "--no-ext-diff", "--", "/dev/null", file]),
        Some(ChangeKind::Renamed(from)) => git(&repo.top, &["diff", &repo.base, "--no-color", "--no-ext-diff", "-M", "--", from, file]),
        _ => git(&repo.top, &["diff", &repo.base, "--no-color", "--no-ext-diff", "--", file]),
    };
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    if text.is_empty() {
        return Err(Problem::NoDiff);
    }
    Ok(truncate(text, MAX_DIFF))
}

fn truncate(mut text: String, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    (text, true)
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expands_to_home_only_at_the_start() {
        assert_eq!(expand_tilde("~", Some("/home/me")), "/home/me");
        assert_eq!(expand_tilde(" ~/code/api ", Some("/home/me/")), "/home/me/code/api");
        assert_eq!(expand_tilde("/srv/~x", Some("/home/me")), "/srv/~x");
        assert_eq!(expand_tilde("~/x", None), "~/x");
    }

    #[test]
    fn porcelain_kinds_and_renames() {
        let out = " M src/a.rs\0A  new.rs\0R  b.rs\0old b.rs\0 D gone.rs\0?? notes/todo.md\0MM both.rs\0";
        assert_eq!(
            parse_porcelain(out),
            vec![
                ("src/a.rs".into(), ChangeKind::Modified),
                ("new.rs".into(), ChangeKind::Added),
                ("b.rs".into(), ChangeKind::Renamed("old b.rs".into())),
                ("gone.rs".into(), ChangeKind::Deleted),
                ("notes/todo.md".into(), ChangeKind::Untracked),
                ("both.rs".into(), ChangeKind::Modified),
            ]
        );
        assert!(parse_porcelain("").is_empty());
    }

    #[test]
    fn numstat_counts_binaries_and_renames() {
        let out = "3\t1\tsrc/a.rs\0-\t-\timg.png\x000\t2\t\0old.rs\0new.rs\0";
        let map = parse_numstat(out);
        assert_eq!(map["src/a.rs"], (Some(3), Some(1)));
        assert_eq!(map["img.png"], (None, None));
        assert_eq!(map["new.rs"], (Some(0), Some(2)));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn log_lines_become_commits() {
        let us = FIELD;
        let out = format!("{}{us}1700000000{us}Seb{us}feat: a{us}b\nnot-a-sha{us}1{us}x{us}y\n", "a".repeat(40));
        let c = parse_log(&out);
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].sha.len(), c[0].time, c[0].author.as_str(), c[0].summary.as_str()), (40, 1_700_000_000, "Seb", "feat: a\u{1f}b"));
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn a_gitdir_under_worktrees_marks_a_linked_worktree() {
        assert!(is_worktree_gitdir("gitdir: /home/me/shop/.git/worktrees/invoices\n"));
        assert!(is_worktree_gitdir("gitdir: ../shop/.git/worktrees/my invoices"), "relative, with a space");
        assert!(!is_worktree_gitdir("gitdir: ../.git/modules/vendor/lib\n"), "a submodule");
        assert!(!is_worktree_gitdir("gitdir: /home/me/worktrees/x/.git/modules/y\n"), "`worktrees` elsewhere in the path");
        assert!(!is_worktree_gitdir("garbage"));
    }

    #[test]
    fn diff_paths_must_stay_inside() {
        assert!(safe_relative("src/a.rs"));
        assert!(!safe_relative("../etc/passwd"));
        assert!(!safe_relative("/etc/passwd"));
        assert!(!safe_relative(""));
    }

    #[test]
    fn truncation_respects_utf8() {
        let (t, cut) = truncate("ééé".into(), 3);
        assert_eq!((t.as_str(), cut), ("é", true));
        assert_eq!(truncate("abc".into(), 10), ("abc".into(), false));
    }

    #[test]
    fn listing_shows_visible_subfolders_sorted_with_git_markers() {
        let root = std::env::temp_dir().join(format!("tcanvas-list-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for d in ["beta/.git", "Alpha", ".cache", "gamma"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::write(root.join("file.txt"), "x").unwrap();
        // A linked worktree: `.git` is a file pointing to the repository.
        fs::create_dir_all(root.join("delta")).unwrap();
        fs::write(root.join("delta/.git"), "gitdir: /home/me/shop/.git/worktrees/delta\n").unwrap();
        // A submodule also has a `.git` file, pointing into `modules/`: a plain repository.
        fs::create_dir_all(root.join("epsilon")).unwrap();
        fs::write(root.join("epsilon/.git"), "gitdir: ../.git/modules/epsilon\n").unwrap();
        std::os::unix::fs::symlink(root.join("gamma"), root.join("link")).unwrap();
        let l = list_dir(&root.display().to_string());
        assert_eq!(l.error, None);
        let names: Vec<(&str, bool, bool)> = l.dirs.iter().map(|d| (d.name.as_str(), d.git, d.worktree)).collect();
        assert_eq!(names, vec![("Alpha", false, false), ("beta", true, false), ("delta", true, true), ("epsilon", true, false), ("gamma", false, false), ("link", false, false)]);
        assert_eq!(l.path.as_deref(), Some(fs::canonicalize(&root).unwrap().to_str().unwrap()));
        assert!(l.parent.is_some() && !l.git && !l.worktree);
        let beta = list_dir(&root.join("beta").display().to_string());
        assert!(beta.git && !beta.worktree);
        let delta = list_dir(&root.join("delta").display().to_string());
        assert!(delta.git && delta.worktree);
        let missing = list_dir("/does/not/exist");
        assert_eq!(missing.error, Some(Problem::DirNotFound { path: "/does/not/exist".into() }));
        assert!(missing.path.is_none());
        let file = root.join("file.txt").display().to_string();
        assert_eq!(list_dir(&file).error, Some(Problem::NotADir { path: file }));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_plan_request_is_dropped_when_a_newer_one_waits() {
        let plan = |id: u64, key: &str| Cmd::PlanWorktree { id, key: key.into(), folder: "f".into(), location: None };
        let waiting: VecDeque<Cmd> = VecDeque::from([plan(2, "shop"), Cmd::Rescan, plan(3, "blog")]);
        assert!(superseded(&plan(1, "shop"), &waiting), "a newer one for the same group");
        assert!(!superseded(&plan(1, "other"), &waiting), "another group's");
        assert!(!superseded(&plan(3, "shop"), &VecDeque::from([Cmd::Rescan])), "the last one is answered");
        assert!(!superseded(&Cmd::Rescan, &waiting), "only plans are dropped");
    }

    #[test]
    fn missing_folder_reports_an_error() {
        let set = scan("k", "/does/not/exist/at/all", 0);
        assert_eq!(set.error, Some(Problem::DirNotFound { path: "/does/not/exist/at/all".into() }));
        assert!(set.root.is_none() && set.files.is_empty());
    }
}
