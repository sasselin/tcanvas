//! Modified files of the folders linked to projects: list (with +/− relative to HEAD) and diffs.
//! Runs on its own thread: `git` never delays watching the terminals.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::{AgentEvent, ChangeKind, ChangeSet, CommitInfo, DirEntry, DirListing, FileChange, Problem, WorktreeList};

use super::{cleanup, create, recipe, review, tmux, worktrees};

const SCAN_EVERY: Duration = Duration::from_secs(2);
const MAX_FILES: usize = 500;
/// Recent commits shown in the panel.
const MAX_COMMITS: usize = 20;
/// Walking a non-git folder stops beyond this many entries.
const MAX_WALK: usize = 20_000;
pub const MAX_DIFF: usize = 512 * 1024;
/// Bytes read to count the lines of an untracked file.
const MAX_COUNT: u64 = 1024 * 1024;
/// git's empty tree: diff base for a repository without commits.
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
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
    /// Scan `key` again now, and say when a scan that started after this request is done
    /// (`AgentEvent::WorktreesRefreshed`), even when nothing changed.
    Refresh {
        key: String,
        seq: u64,
    },
    /// A review request (`AgentCmd::Review`, `ReviewFiles`, `ReviewDiff`), answered on the review
    /// thread.
    Review(review::Request),
}

impl Cmd {
    /// The review request `cmd` is, if it is one.
    pub fn review(cmd: protocol::AgentCmd) -> Option<Cmd> {
        review::Request::of(cmd).map(Cmd::Review)
    }
}

/// A command that writes, run on the cleanup thread (one at a time).
enum Job {
    Cleanup { key: String, dir: PathBuf, action: protocol::Cleanup },
    Create { id: u64, key: String, dir: PathBuf, request: protocol::CreateWorktree },
}

/// A recipe to apply to a new worktree, on the recipe thread.
struct RecipeJob {
    id: u64,
    key: String,
    main: PathBuf,
    path: String,
}

/// A plan to compute on the plan thread (`dir`: the watched folder, or why there is none).
struct PlanJob {
    id: u64,
    key: String,
    dir: Result<PathBuf, Problem>,
    folder: String,
    location: Option<String>,
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
    let recipes = spawn_recipes(send.clone(), tx.clone());
    let workers = Workers {
        jobs: spawn_cleaner(send.clone(), tx.clone(), paths.clone(), recipes),
        sizes: spawn_measurer(send.clone(), measuring.clone()),
        plans: spawn_planner(send.clone()),
        reviews: spawn_reviewer(send.clone()),
        paths,
        measuring,
    };
    thread::spawn(move || run(rx, send, workers));
    tx
}

/// The cleanup and size threads, and what they share with the scan thread.
struct Workers {
    jobs: Sender<Job>,
    sizes: Sender<SizeJob>,
    plans: Sender<PlanJob>,
    reviews: Sender<(review::Request, Result<PathBuf, Problem>)>,
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

/// Worktree plans are computed on their own thread (a recipe's searches may take a while: the
/// scans go on meanwhile); of several waiting for one key, only the last is answered.
fn spawn_planner(send: Sink) -> Sender<PlanJob> {
    let (tx, rx) = mpsc::channel::<PlanJob>();
    thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut waiting: Vec<PlanJob> = vec![first];
            waiting.extend(rx.try_iter());
            let last: Vec<&PlanJob> = waiting.iter().enumerate().filter(|(i, j)| !waiting[i + 1..].iter().any(|later| later.key == j.key)).map(|(_, j)| j).collect();
            for job in last {
                let plan = match &job.dir {
                    Ok(dir) => create::plan(dir, job.id, &job.key, &job.folder, job.location.as_deref()),
                    Err(problem) => create::refused_plan(job.id, &job.key, &job.folder, job.location.as_deref(), problem.clone()),
                };
                send(&AgentEvent::WorktreePlan(Box::new(plan)));
            }
        }
    });
    tx
}

/// Reviews are answered on their own thread (a big branch takes a while to list: the scans go on
/// meanwhile); of several waiting of the same kind for one key, only the last is answered.
fn spawn_reviewer(send: Sink) -> Sender<(review::Request, Result<PathBuf, Problem>)> {
    let (tx, rx) = mpsc::channel::<(review::Request, Result<PathBuf, Problem>)>();
    thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut waiting = vec![first];
            waiting.extend(rx.try_iter());
            let n = waiting.len();
            for (i, (request, dir)) in waiting.iter().enumerate() {
                if waiting[i + 1..n].iter().any(|(later, _)| request.superseded_by(later)) {
                    continue;
                }
                send(&review::answer(request.clone(), dir.clone()));
            }
        }
    });
    tx
}

/// Recipes are applied one at a time on their own thread, once the creation is answered: a long
/// copy never holds the cleanups and creations. `RecipeApplied` always follows, even when the
/// recipe went away meanwhile (an empty report), so the app never waits for nothing.
fn spawn_recipes(send: Sink, rescan: Sender<Cmd>) -> Sender<RecipeJob> {
    let (tx, rx) = mpsc::channel::<RecipeJob>();
    thread::spawn(move || {
        // Test only: the copies take this long, to check that nothing waits for them.
        let delay = std::env::var("TCANVAS_RECIPE_TEST_DELAY_MS").ok().and_then(|s| s.parse().ok()).map(Duration::from_millis);
        for job in rx {
            if let Some(delay) = delay {
                thread::sleep(delay);
            }
            let report = recipe::apply(&job.main, Path::new(&job.path)).unwrap_or_default();
            send(&AgentEvent::RecipeApplied { id: job.id, key: job.key, path: job.path, report: Box::new(report) });
            let _ = rescan.send(Cmd::Rescan);
        }
    });
    tx
}

/// Cleanups and worktree creations run one at a time on their own thread (deleting a big folder,
/// or checking out a big one, takes a while; diffs and scans go on meanwhile), then ask for a scan
/// so the lists follow right away. What uses the folders (panes, processes, watched folders) is
/// read by `cleanup::run` right before it writes, after its slow checks.
fn spawn_cleaner(send: Sink, rescan: Sender<Cmd>, paths: WatchedPaths, recipes: Sender<RecipeJob>) -> Sender<Job> {
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
                    let (late_send, late_rescan, late_key, late_recipes) = (send.clone(), rescan.clone(), key.clone(), recipes.clone());
                    let late = move |result| {
                        answer_created(&late_send, &late_recipes, id, late_key, result);
                        let _ = late_rescan.send(Cmd::Rescan);
                    };
                    answer_created(&send, &recipes, id, key.clone(), create::run(&dir, &request, late));
                }
            }
            let _ = rescan.send(Cmd::Rescan);
        }
    });
    tx
}

/// Answers a creation as soon as git is done; its recipe, if any, then goes to the recipe thread
/// (`RecipeApplied` follows).
fn answer_created(send: &Sink, recipes: &Sender<RecipeJob>, id: u64, key: String, result: create::Created) {
    match result {
        Ok(done) => {
            let pending = done.recipe.is_some();
            send(&AgentEvent::WorktreeCreated { id, key: key.clone(), path: Some(done.path.clone()), problem: None, excluded: done.excluded, recipe_pending: pending });
            if let Some(main) = done.recipe {
                let _ = recipes.send(RecipeJob { id, key, main, path: done.path });
            }
        }
        Err(problem) => send(&AgentEvent::WorktreeCreated { id, key, path: None, problem: Some(problem), excluded: false, recipe_pending: false }),
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
    // Refreshes asked: answered at the end of the next scan pass, which starts after them.
    let mut refreshes: Vec<(String, u64)> = Vec::new();
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
                // Read only, but a recipe's searches may take a while: off this thread.
                let dir = watched.get(&key).map(|w| resolve(&w.path)).unwrap_or(Err(Problem::NotAGitRepo));
                let _ = workers.plans.send(PlanJob { id, key, dir, folder, location });
            }
            Ok(Cmd::CreateWorktree { id, key, request }) => match watched.get(&key).map(|w| resolve(&w.path)) {
                Some(Ok(dir)) => {
                    let _ = workers.jobs.send(Job::Create { id, key, dir, request });
                }
                Some(Err(problem)) => send(&AgentEvent::WorktreeCreated { id, key, path: None, problem: Some(problem), excluded: false, recipe_pending: false }),
                None => send(&AgentEvent::WorktreeCreated { id, key, path: None, problem: Some(Problem::NotAGitRepo), excluded: false, recipe_pending: false }),
            },
            Ok(Cmd::Review(request)) => {
                let dir = watched.get(&request.key).map(|w| resolve(&w.path)).unwrap_or(Err(Problem::NotAGitRepo));
                let _ = workers.reviews.send((request, dir));
            }
            Ok(Cmd::Rescan) => next_scan = Instant::now(),
            Ok(Cmd::Refresh { key, seq }) => {
                refreshes.push((key, seq));
                next_scan = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {
                // Asked before this pass started: its lists answer them.
                let answering = std::mem::take(&mut refreshes);
                let mut inputs = Vec::new();
                for (key, w) in watched.iter_mut() {
                    let set = scan(key, &w.path, w.since);
                    // A git folder's top is one of its repository's worktrees. When the folder is
                    // that top, the status just run covers the whole worktree: no second one.
                    let top = set.root.clone().filter(|_| set.git);
                    let whole = top.is_some() && resolve(&w.path).ok().map(|d| d.to_string_lossy().into_owned()) == top;
                    inputs.push(worktrees::Input { key: key.clone(), top, dirty: whole.then_some(!set.files.is_empty()), mark: whole.then(|| files_mark(&set.files)) });
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
                for (key, seq) in answering {
                    send(&AgentEvent::WorktreesRefreshed { key, seq });
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

/// Variables that would make git look somewhere else than the folder it is run in: removed from
/// every git call of the agent, so a stray one in its environment never makes a read, or a write,
/// target another repository.
pub const GIT_REDIRECTS: [&str; 5] = ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_CEILING_DIRECTORIES", "GIT_DISCOVERY_ACROSS_FILESYSTEM"];

/// `git -C <dir>`, with `GIT_REDIRECTS` removed from its environment. Every git call of the agent
/// starts here.
pub fn git_command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    for var in GIT_REDIRECTS {
        cmd.env_remove(var);
    }
    cmd
}

fn git(dir: &Path, args: &[&str]) -> Option<Output> {
    // No optional lock on the index: never get in the way of the user's (or Claude's) git.
    git_command(dir).args(args).env("GIT_OPTIONAL_LOCKS", "0").output().ok()
}

/// `git` with arguments that may not be UTF-8 (file names).
fn git_os(dir: &Path, args: &[OsString]) -> Option<Output> {
    git_command(dir).args(args).env("GIT_OPTIONAL_LOCKS", "0").output().ok()
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
    let entries = parse_porcelain(&status.stdout);
    let numstat = git(dir, &["diff", &repo.base, "--numstat", "-z", "-M", "--no-ext-diff", "--", "."]).map(|o| parse_numstat(&o.stdout)).unwrap_or_default();
    file_changes(&repo.top, entries, &numstat)
}

/// The listed files of a worktree whose top is `top`, with their counts: from the numstat, else,
/// for an untracked regular file, its lines (a symbolic link is never followed, a pipe never read).
pub fn file_changes(top: &Path, entries: Vec<(String, ChangeKind)>, numstat: &HashMap<String, (Option<u32>, Option<u32>)>) -> Vec<FileChange> {
    entries
        .into_iter()
        .map(|(path, kind)| {
            let full = top.join(name_path(&path));
            let (added, removed, binary) = match (&kind, numstat.get(&path)) {
                (_, Some(&(a, r))) => (a, r, a.is_none()),
                (ChangeKind::Untracked, None) if is_regular(&full) => match count_lines(&full) {
                    Some(n) => (Some(n), Some(0), false),
                    None => (None, None, true),
                },
                _ => (None, None, false),
            };
            FileChange { mtime: mtime(&full), path, kind, added, removed, binary }
        })
        .collect()
}

/// A regular file itself (`lstat`: a symbolic link is not one, whatever it points to).
pub fn is_regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// A file name as git gives it (bytes), as text: unchanged when it is UTF-8 and does not start
/// with `"`; otherwise quoted the way git quotes names (`"caf\351.txt"`), which `name_bytes` turns
/// back into the exact bytes.
pub fn name_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.starts_with('"') => text.to_string(),
        _ => {
            let mut out = String::from("\"");
            for &b in bytes {
                match b {
                    b'"' => out.push_str("\\\""),
                    b'\\' => out.push_str("\\\\"),
                    b'\n' => out.push_str("\\n"),
                    b'\t' => out.push_str("\\t"),
                    0x20..=0x7e => out.push(b as char),
                    _ => out.push_str(&format!("\\{b:03o}")),
                }
            }
            out.push('"');
            out
        }
    }
}

/// The exact bytes of a name `name_text` gave.
pub fn name_bytes(text: &str) -> Vec<u8> {
    match text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        Some(inner) if text.len() >= 2 => worktrees::unquote_bytes(inner),
        _ => text.as_bytes().to_vec(),
    }
}

pub fn name_path(text: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(name_bytes(text)))
}

/// Field separator for the `git log` format (the "unit separator" character).
pub const FIELD: char = '\u{1f}';

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
/// Names as `name_text` gives them.
pub fn parse_porcelain(out: &[u8]) -> Vec<(String, ChangeKind)> {
    let mut fields = out.split(|&b| b == 0).filter(|f| !f.is_empty());
    let mut entries = Vec::new();
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue;
        }
        let (x, y, path) = (entry[0], entry[1], name_text(&entry[3..]));
        let kind = if (x, y) == (b'?', b'?') {
            ChangeKind::Untracked
        } else if x == b'R' || x == b'C' {
            let from = name_text(fields.next().unwrap_or_default());
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
/// for a rename. `-` for a binary (→ `None`). Key: target path, as `name_text` gives it.
pub fn parse_numstat(out: &[u8]) -> HashMap<String, (Option<u32>, Option<u32>)> {
    let mut map = HashMap::new();
    let mut fields = out.split(|&b| b == 0);
    while let Some(entry) = fields.next() {
        let mut parts = entry.splitn(3, |&b| b == b'\t');
        let (Some(a), Some(r), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let path = if path.is_empty() {
            let _from = fields.next();
            fields.next().unwrap_or_default()
        } else {
            path
        };
        let count = |c: &[u8]| std::str::from_utf8(c).ok().and_then(|c| c.parse().ok());
        map.insert(name_text(path), (count(a), count(r)));
    }
    map
}

/// Line count of a regular text file; `None` for a binary, unreadable or other file. Opened
/// without following a symbolic link nor waiting on a pipe, then checked on the open file, and
/// read up to `MAX_COUNT` bytes.
pub fn count_lines(path: &Path) -> Option<u32> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).ok()?;
    if !file.metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut buf = Vec::new();
    file.take(MAX_COUNT).read_to_end(&mut buf).ok()?;
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

/// A fingerprint of a scan's files (`Worktree::status_mark` of a watched worktree).
fn files_mark(files: &[FileChange]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for f in files {
        (&f.path, f.kind.letter(), f.mtime, f.added, f.removed).hash(&mut h);
    }
    h.finish()
}

/// A relative path (a name as `name_text` gives it) that does not escape the repository.
pub fn safe_relative(file: &str) -> bool {
    let bytes = name_bytes(file);
    let p = name_path(file);
    !bytes.is_empty() && !bytes.contains(&0) && p.is_relative() && !p.components().any(|c| matches!(c, std::path::Component::ParentDir))
}

fn diff(w: &Watched, file: &str) -> Result<(String, bool), Problem> {
    if !safe_relative(file) {
        return Err(Problem::PathRefused);
    }
    let dir = resolve(&w.path)?;
    let repo = repo(&dir).ok_or(Problem::NotAGitRepo)?;
    let entry = w.last_sent.as_ref().and_then(|s| s.files.iter().find(|f| f.path == file));
    let os = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
    let args = match entry.map(|f| &f.kind) {
        Some(ChangeKind::Untracked) => {
            // `--no-index` reads what a symbolic link points to (a pipe would never end).
            if !is_regular(&repo.top.join(name_path(file))) {
                return Err(Problem::NotARegularFile { path: file.to_string() });
            }
            [os(&["diff", "--no-index", "--no-color", "--no-ext-diff", "--", "/dev/null"]), vec![name_path(file).into_os_string()]].concat()
        }
        Some(ChangeKind::Renamed(from)) => [os(&["diff", &repo.base, "--no-color", "--no-ext-diff", "-M", "--"]), vec![name_path(from).into_os_string(), name_path(file).into_os_string()]].concat(),
        _ => [os(&["diff", &repo.base, "--no-color", "--no-ext-diff", "--"]), vec![name_path(file).into_os_string()]].concat(),
    };
    let out = git_os(&repo.top, &args);
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
            parse_porcelain(out.as_bytes()),
            vec![
                ("src/a.rs".into(), ChangeKind::Modified),
                ("new.rs".into(), ChangeKind::Added),
                ("b.rs".into(), ChangeKind::Renamed("old b.rs".into())),
                ("gone.rs".into(), ChangeKind::Deleted),
                ("notes/todo.md".into(), ChangeKind::Untracked),
                ("both.rs".into(), ChangeKind::Modified),
            ]
        );
        assert!(parse_porcelain(b"").is_empty());
        // Names that are not UTF-8, or start with a quote: quoted, the exact bytes kept.
        let odd = parse_porcelain(b"?? caf\xe9.txt\0R  \"q\0old\xff\0");
        assert_eq!(odd, vec![("\"caf\\351.txt\"".into(), ChangeKind::Untracked), ("\"\\\"q\"".into(), ChangeKind::Renamed("\"old\\377\"".into()))]);
    }

    #[test]
    fn names_round_trip_to_their_exact_bytes() {
        for bytes in [&b"src/a.rs"[..], b"caf\xe9.txt", b"\"quoted\"", b"tab\there", b"back\\slash \xff\x01", "\u{e9}t\u{e9}".as_bytes()] {
            let text = name_text(bytes);
            assert_eq!(name_bytes(&text), bytes, "{text}");
        }
        assert_eq!(name_text("\u{e9}t\u{e9} x.md".as_bytes()), "\u{e9}t\u{e9} x.md", "UTF-8 stays readable");
        assert_eq!(name_text(b"caf\xe9.txt"), "\"caf\\351.txt\"");
        assert!(safe_relative("\"caf\\351.txt\""));
        assert!(!safe_relative("\"..\\057x\""), "an escape hidden in quotes");
        assert!(!safe_relative("\"a\\000b\""), "a NUL byte");
    }

    #[test]
    fn lines_are_counted_in_regular_files_only_never_through_a_link_or_a_pipe() {
        let root = std::env::temp_dir().join(format!("tcanvas-count-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.txt"), "1\n2\n3").unwrap();
        std::os::unix::fs::symlink(root.join("a.txt"), root.join("link")).unwrap();
        let fifo = std::ffi::CString::new(root.join("pipe").to_str().unwrap()).unwrap();
        // SAFETY: a plain system call on a path of our own.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(count_lines(&root.join("a.txt")), Some(3));
        assert_eq!(count_lines(&root.join("link")), None, "a symbolic link is not followed");
        assert_eq!(count_lines(&root.join("pipe")), None, "a pipe is not read (nor waited on)");
        assert!(is_regular(&root.join("a.txt")) && !is_regular(&root.join("link")) && !is_regular(&root.join("pipe")));
        let entries = vec![("link".to_string(), ChangeKind::Untracked), ("pipe".to_string(), ChangeKind::Untracked), ("a.txt".to_string(), ChangeKind::Untracked)];
        let files = file_changes(&root, entries, &HashMap::new());
        let counts: Vec<(Option<u32>, bool)> = files.iter().map(|f| (f.added, f.binary)).collect();
        assert_eq!(counts, vec![(None, false), (None, false), (Some(3), false)]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn numstat_counts_binaries_and_renames() {
        let out = "3\t1\tsrc/a.rs\0-\t-\timg.png\x000\t2\t\0old.rs\0new.rs\0";
        let map = parse_numstat(out.as_bytes());
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
