//! Task lists, kept outside the repositories so they survive branch switches and worktrees.
//!
//! One file per list in `~/.local/share/tcanvas/tasks/` (`TCANVAS_TEST_HOME` replaces the home,
//! for tests). A list belongs to a repository, keyed by its main checkout's resolved path (the
//! same for every worktree and every folder of it), or to a folder outside git, keyed by its own
//! resolved path. A folder counts as outside git only when git says so; when git fails otherwise,
//! the key is unknown and nothing is read or written (`Problem::TasksKeyUnknown`). The file is
//! named after the key (a readable hint plus a hash) and holds the key, and for a repository its
//! first root commit (`repo_id`), which survives moving or renaming it.
//!
//! Several writers may share a list (this agent's groups, other agent processes, later a CLI):
//! every change is a read-modify-write under an exclusive `flock` on the list's lock file (never
//! deleted; checked after locking to still be the file at that path), held only for that change.
//! The file is replaced atomically (temporary file in the same folder, fsync, rename, fsync of the
//! folder), so a reader never sees half a file; temporary files a crash left are removed under the
//! lock. The folder is 0700 and the files 0600: descriptions may hold private text.
//!
//! Revisions: a task's `rev` goes up on any change of it, its `content_rev` only when its title,
//! description or priority change, and an edit naming a `content_rev` (`expect`) is refused when
//! it moved on: notes, dependencies, moves and status changes never make an edit stale. The
//! list's `revision` goes up on every write. All three saturate, never overflow.
//!
//! Reading: content that is not UTF-8 nor JSON, or lacks its format or key, is set aside
//! (`.corrupt-<unix time>`, never overwritten nor deleted) and the list starts empty, with a
//! notice to the app. A file of a newer format, holding values this version does not know, or
//! with two tasks of one id, is shown read-only and never rewritten.
//!
//! Limits (`protocol::MAX_TASK_*`, `MAX_TASKS`) keep a list small: it is sent whole on changes.
//!
//! Removed tasks go to a trash kept in the file (`Task::removed`): undo is one small change, the
//! dependencies on them stay, and another writer sees them gone. They are purged `TRASH_SECS`
//! after their removal, on the next write; their references are remembered (`retired`, the last
//! `MAX_RETIRED`) and not given again.
//!
//! The subscriptions run on their own thread. Every `SCAN_EVERY` each one costs one `stat` and a
//! read of the file's first bytes (its `revision`); the file is parsed only when that changed. The
//! key is resolved again (git) every `RESOLVE_EVERY`, and before every change. When a folder's key
//! changes, or on subscribing, the lists that may have been its own are looked for (its previous
//! key, the folder's own key, a list of the same repository whose folder is gone) and named in the
//! board (`TaskBoard::found`), so the app can bring their tasks over (`AgentCmd::AdoptTasks`).

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::{
    AgentEvent, Extra, FoundList, FoundWhy, MAX_TASK_DESCRIPTION, MAX_TASK_NOTE, MAX_TASK_NOTES, MAX_TASK_TITLE, MAX_TASKS, MAX_TRASH, Problem, TASKS_FORMAT, Task, TaskBoard, TaskField, TaskList,
    TaskNote, TaskOp,
};

use super::changes::{git_command, resolve};

/// Under the home folder.
pub const TASKS_DIR: &str = ".local/share/tcanvas/tasks";
const SCAN_EVERY: Duration = Duration::from_secs(2);
/// How often a subscription's key is resolved again (a git call) between changes.
const RESOLVE_EVERY: Duration = Duration::from_secs(30);
/// A removed task stays restorable this long (a week), then the next write purges it.
pub const TRASH_SECS: u64 = 7 * 86_400;
/// References of purged tasks remembered, so they are not given again: past that many, the
/// oldest may be reused.
pub const MAX_RETIRED: usize = 5000;
/// Characters of the short references: no `0`/`o`, `1`/`l`/`i` to confuse.
const REF_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
/// Longest hint in a file name, in bytes.
const MAX_HINT: usize = 40;
/// Bytes read from the top of a list to find its `revision`.
const HEAD_BYTES: u64 = 4096;

/// The folder holding the lists.
pub fn data_dir() -> PathBuf {
    let home = std::env::var("TCANVAS_TEST_HOME").ok().filter(|h| !h.is_empty()).or_else(|| std::env::var("HOME").ok()).unwrap_or_else(|| "/".into());
    PathBuf::from(home).join(TASKS_DIR)
}

// --- Which list a folder belongs to --------------------------------------------------------------

/// A watched folder, resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The list's key.
    pub key: String,
    /// The folder itself (resolved path).
    pub dir: String,
    /// The repository's first root commit (`None` outside git, or before the first commit).
    pub repo_id: Option<String>,
}

fn git(dir: &Path, args: &[&str]) -> io::Result<Output> {
    // English messages (one is matched below), no optional lock on the index, and the folder's
    // own repository whatever the environment says.
    git_command(dir).args(args).env("GIT_OPTIONAL_LOCKS", "0").env("LC_ALL", "C").output()
}

/// A `.git` entry (folder, file, or link, even dangling) in the folder or one of its parents.
fn git_entry_above(dir: &Path) -> bool {
    dir.ancestors().any(|a| fs::symlink_metadata(a.join(".git")).is_ok())
}

/// The key `git worktree list --porcelain` in `dir` gives: the main checkout (first entry), or the
/// folder itself only when git says no repository holds it ("(or any of the parent
/// directories)") and no `.git` entry is in it or above it. Any other failure (a worktree whose
/// main checkout is gone, a `.git` file pointing nowhere...) is a problem: guessing would read and
/// write another list.
pub fn classify_key(dir: &Path, out: io::Result<Output>) -> Result<String, Problem> {
    let unknown = |detail: String| Problem::TasksKeyUnknown { detail };
    let folder = || dir.to_string_lossy().into_owned();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let main = text.lines().find_map(|l| l.strip_prefix("worktree ")).ok_or_else(|| unknown("git listed no worktree".into()))?;
            Ok(fs::canonicalize(main).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| main.to_string()))
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let outside = o.status.code() == Some(128) && err.contains("not a git repository (or any of the parent directories)");
            if outside && !git_entry_above(dir) { Ok(folder()) } else { Err(unknown(err)) }
        }
        // No git at all: outside git only if no `.git` is anywhere up the way.
        Err(e) => match git_entry_above(dir) {
            true => Err(unknown(e.to_string())),
            false => Ok(folder()),
        },
    }
}

/// The first root commit of the repository at `key` (sorted, so the same for every clone).
fn repo_id(key: &Path) -> Option<String> {
    let out = git(key, &["rev-list", "--max-parents=0", "HEAD"]).ok().filter(|o| o.status.success())?;
    let mut roots: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).filter(|l| !l.is_empty()).collect();
    roots.sort();
    roots.into_iter().next()
}

/// Resolves a watched folder: its list's key, and the repository's id.
pub fn resolve_folder(path: &str) -> Result<Resolved, Problem> {
    let dir = resolve(path)?;
    let key = classify_key(&dir, git(&dir, &["worktree", "list", "--porcelain"]))?;
    let dir = dir.to_string_lossy().into_owned();
    let repo_id = if key != dir || Path::new(&key).join(".git").exists() { repo_id(Path::new(&key)) } else { None };
    Ok(Resolved { key, dir, repo_id })
}

// --- Files ---------------------------------------------------------------------------------------

/// FNV-1a, 64 bits: stable across builds and machines (unlike the standard hasher).
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// `<hint>-<hash>.json`: the hint is the key's last folder name made plain (letters, digits, `.`,
/// `_`, `-`), the hash tells apart keys with the same last name. `n` > 1: the next names, for the
/// (unlikely) case where a file of that name holds another key.
pub fn file_name(key: &str, n: u32) -> String {
    let last = key.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
    let mut hint: String = last.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect();
    hint = hint.trim_matches(|c| c == '.' || c == '-').to_string();
    hint.truncate(MAX_HINT);
    if hint.is_empty() {
        hint = "list".into();
    }
    let suffix = if n > 1 { format!("-{n}") } else { String::new() };
    format!("{hint}-{:016x}{suffix}.json", fnv1a(key))
}

/// What tells a change of a file without parsing it: its size, times, inode, and the `revision`
/// written at its top (exact even when the rest matches).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    mtime: i128,
    ctime: i128,
    ino: u64,
    revision: Option<u64>,
}

/// The `revision` among the first bytes of a list (written right after its format and key).
pub fn head_revision(head: &[u8]) -> Option<u64> {
    let text = String::from_utf8_lossy(head);
    let at = text.find("\"revision\"")? + "\"revision\"".len();
    let rest = text[at..].trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

pub fn stamp(path: &Path) -> Option<Stamp> {
    let m = fs::metadata(path).ok()?;
    let mut head = Vec::new();
    let _ = File::open(path).and_then(|f| f.take(HEAD_BYTES).read_to_end(&mut head));
    let ns = |s: i64, n: i64| i128::from(s) * 1_000_000_000 + i128::from(n);
    Some(Stamp { len: m.len(), mtime: ns(m.mtime(), m.mtime_nsec()), ctime: ns(m.ctime(), m.ctime_nsec()), ino: m.ino(), revision: head_revision(&head) })
}

/// A list's file, its lock and the prefix of its temporary files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Files {
    pub path: PathBuf,
    lock: PathBuf,
    name: String,
}

/// How a file's bytes read.
enum Parsed {
    List(TaskList),
    /// Shown, never rewritten, for this reason.
    ReadOnly(TaskList, Problem),
    Corrupt(String),
}

const STATUSES: [&str; 6] = ["inbox", "todo", "active", "review", "done", "canceled"];
const PRIORITIES: [&str; 5] = ["none", "low", "medium", "high", "urgent"];
const AUTHORS: [&str; 2] = ["user", "agent"];

/// A value this version does not know becomes a known one, for display only.
fn known_or(o: &mut serde_json::Map<String, serde_json::Value>, field: &str, known: &[&str], fallback: &str) {
    if o.get(field).is_some_and(|v| !v.as_str().is_some_and(|s| known.contains(&s))) {
        o.insert(field.into(), fallback.into());
    }
}

/// As much of a list as reads, to show it read-only.
fn lenient(mut value: serde_json::Value, key: &str, format: u32) -> TaskList {
    for t in value.get_mut("tasks").and_then(|t| t.as_array_mut()).into_iter().flatten() {
        let Some(o) = t.as_object_mut() else { continue };
        known_or(o, "status", &STATUSES, "inbox");
        known_or(o, "priority", &PRIORITIES, "none");
        for n in o.get_mut("notes").and_then(|n| n.as_array_mut()).into_iter().flatten() {
            if let Some(n) = n.as_object_mut() {
                known_or(n, "author", &AUTHORS, "user");
            }
        }
    }
    let list = serde_json::from_value::<TaskList>(value).unwrap_or_else(|_| TaskList::empty(key));
    TaskList { format, ..list }
}

fn parse(bytes: &[u8], key: &str) -> Parsed {
    let text = match std::str::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => return Parsed::Corrupt(format!("not UTF-8: {e}")),
    };
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return Parsed::Corrupt(e.to_string()),
    };
    let format = value.get("format").and_then(|f| f.as_u64());
    let (Some(format), Some(_)) = (format, value.get("key").and_then(|k| k.as_str())) else { return Parsed::Corrupt("no format or key".into()) };
    let format = u32::try_from(format).unwrap_or(u32::MAX);
    if format > TASKS_FORMAT {
        return Parsed::ReadOnly(lenient(value, key, format), Problem::TasksNewerFormat { format });
    }
    let list = match serde_json::from_value::<TaskList>(value.clone()) {
        Ok(list) => list,
        Err(e) => return Parsed::ReadOnly(lenient(value, key, format), Problem::TasksUnknownValues { detail: e.to_string() }),
    };
    let mut ids = HashSet::new();
    if let Some(t) = list.tasks.iter().find(|t| !ids.insert(t.id.as_str())) {
        let problem = Problem::TasksDuplicateId { id: t.id.clone() };
        return Parsed::ReadOnly(list, problem);
    }
    Parsed::List(list)
}

/// What reading a list gave.
#[derive(Debug, PartialEq)]
pub struct Loaded {
    pub list: TaskList,
    /// Shown, never rewritten, for this reason.
    pub read_only: Option<Problem>,
    /// The file was set aside (`Problem::TasksSetAside`).
    pub set_aside: Option<Problem>,
}

/// What a change gave: its result, and the notices of files set aside on the way (reported
/// whatever the result).
#[derive(Debug)]
pub struct Outcome {
    pub result: Result<Done, Problem>,
    pub set_aside: Vec<Problem>,
}

impl Outcome {
    fn failed(problem: Problem) -> Self {
        Outcome { result: Err(problem), set_aside: Vec::new() }
    }
}

/// How long a writer waits for a list's lock before giving up (another program may hold it and be
/// stopped): the thread never hangs on it.
pub const LOCK_WAIT: Duration = Duration::from_secs(5);
/// Before setting content aside, it is read again this long after: a text editor saving in place
/// may have been caught mid-write.
const SETTLE: Duration = Duration::from_millis(250);

#[derive(Debug, PartialEq, Eq)]
pub struct Done {
    /// The task changed (the new one for `Add`).
    pub task: Option<String>,
    /// Its `content_rev` after the change.
    pub content_rev: Option<u64>,
}

fn failed(e: io::Error) -> Problem {
    Problem::TasksFailed { detail: e.to_string() }
}

/// The file's bytes; `None` when it does not exist.
fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, Problem> {
    let mut bytes = Vec::new();
    match File::open(path).and_then(|mut f| f.read_to_end(&mut bytes)) {
        Ok(_) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(failed(e)),
    }
}

/// The lists of one data folder.
pub struct Store {
    dir: PathBuf,
    lock_wait: Duration,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Store { dir, lock_wait: LOCK_WAIT }
    }

    #[cfg(test)]
    pub fn with_lock_wait(dir: PathBuf, lock_wait: Duration) -> Self {
        Store { dir, lock_wait }
    }

    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Closes the folder again (0700) if it exists: on first use, not only on a write.
    pub fn tighten(&self) {
        if self.dir.is_dir() {
            let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700));
        }
    }

    /// The file of `key`: the first name that is free or holds that key (or cannot be read: it
    /// is then this key's, set aside when read).
    pub fn files(&self, key: &str) -> Files {
        let mut n = 1;
        loop {
            let name = file_name(key, n);
            let path = self.dir.join(&name);
            let other = read_bytes(&path).ok().flatten().is_some_and(|bytes| match parse(&bytes, key) {
                Parsed::List(l) | Parsed::ReadOnly(l, _) => l.key != key,
                Parsed::Corrupt(_) => false,
            });
            if !other || n >= 100 {
                let lock = path.with_extension("lock");
                return Files { path, lock, name };
            }
            n += 1;
        }
    }

    /// Reads a list (empty when there is no file yet). Content that does not read is read again a
    /// moment later; only if it still does not, and no writer holds the lock (taken without
    /// waiting), is it set aside. `Err(TasksLocked)`: try again later.
    pub fn read(&self, files: &Files, key: &str) -> Result<Loaded, Problem> {
        match self.settled(files, key)? {
            Some(loaded) => Ok(loaded),
            None => {
                let _lock = self.lock_within(&files.lock, Duration::ZERO)?;
                self.load_locked(key, files)
            }
        }
    }

    /// The list if it reads now or a moment later; `None` when it still does not.
    fn settled(&self, files: &Files, key: &str) -> Result<Option<Loaded>, Problem> {
        for attempt in 0..2 {
            if attempt > 0 {
                thread::sleep(SETTLE);
            }
            match read_bytes(&files.path)?.map(|b| parse(&b, key)) {
                None => return Ok(Some(Loaded { list: TaskList::empty(key), read_only: None, set_aside: None })),
                Some(Parsed::List(list)) => return Ok(Some(Loaded { list, read_only: None, set_aside: None })),
                Some(Parsed::ReadOnly(list, why)) => return Ok(Some(Loaded { list, read_only: Some(why), set_aside: None })),
                Some(Parsed::Corrupt(_)) => {}
            }
        }
        Ok(None)
    }

    /// Reads the list under the lock: content that still does not read a moment later is set
    /// aside and the list starts empty.
    fn load_locked(&self, key: &str, files: &Files) -> Result<Loaded, Problem> {
        if let Some(loaded) = self.settled(files, key)? {
            return Ok(loaded);
        }
        let Some(bytes) = read_bytes(&files.path)? else { return Ok(Loaded { list: TaskList::empty(key), read_only: None, set_aside: None }) };
        match parse(&bytes, key) {
            Parsed::List(list) => Ok(Loaded { list, read_only: None, set_aside: None }),
            Parsed::ReadOnly(list, why) => Ok(Loaded { list, read_only: Some(why), set_aside: None }),
            Parsed::Corrupt(detail) => {
                let aside = rename_aside(&files.path, "corrupt").map_err(failed)?;
                let problem = Problem::TasksSetAside { path: aside.to_string_lossy().into_owned(), detail };
                Ok(Loaded { list: TaskList::empty(key), read_only: None, set_aside: Some(problem) })
            }
        }
    }

    fn ensure_dir(&self) -> Result<(), Problem> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir).map_err(failed)?;
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700)).map_err(failed)
    }

    fn lock(&self, path: &Path) -> Result<File, Problem> {
        self.lock_within(path, self.lock_wait)
    }

    /// An exclusive lock on `path` (created if needed, never deleted), released when dropped.
    /// Never blocks: tried again until `wait` has passed, then `TasksLocked`. After locking, the
    /// path must still be the locked file (if it was replaced meanwhile, another writer may hold
    /// the new one: lock that one instead).
    fn lock_within(&self, path: &Path, wait: Duration) -> Result<File, Problem> {
        self.ensure_dir()?;
        let deadline = Instant::now() + wait;
        for _ in 0..20 {
            let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).open(path).map_err(failed)?;
            loop {
                // SAFETY: a valid descriptor, owned by `file` for the whole call.
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    break;
                }
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::Interrupted => {}
                    io::ErrorKind::WouldBlock if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                    io::ErrorKind::WouldBlock => return Err(Problem::TasksLocked { secs: wait.as_secs() }),
                    _ => return Err(failed(e)),
                }
            }
            let held = file.metadata().map_err(failed)?;
            if fs::metadata(path).is_ok_and(|m| m.ino() == held.ino() && m.dev() == held.dev()) {
                return Ok(file);
            }
        }
        Err(Problem::TasksFailed { detail: "the lock file keeps being replaced".into() })
    }

    /// Temporary files of this list a crash left behind (they may hold task text). Under the
    /// lock: no writer of this list is in the middle of writing one.
    fn clean_temp(&self, files: &Files) {
        let prefix = format!(".{}.tmp-", files.name);
        for entry in fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    fn write(&self, files: &Files, list: &TaskList) -> Result<(), Problem> {
        let mut bytes = serde_json::to_vec_pretty(list).map_err(|e| Problem::TasksFailed { detail: e.to_string() })?;
        bytes.push(b'\n');
        write_atomic(&files.path, &bytes).map_err(failed)
    }

    /// Applies one change under the list's lock and writes the list back when it changed.
    pub fn apply(&self, key: &str, op: &TaskOp, expect: Option<u64>, now: u64, repo_id: Option<&str>) -> Outcome {
        let files = self.files(key);
        let _lock = match self.lock(&files.lock) {
            Ok(l) => l,
            Err(p) => return Outcome::failed(p),
        };
        self.clean_temp(&files);
        let loaded = match self.load_locked(key, &files) {
            Ok(l) => l,
            Err(p) => return Outcome::failed(p),
        };
        let set_aside: Vec<Problem> = loaded.set_aside.iter().cloned().collect();
        let result = (|| {
            if let Some(why) = loaded.read_only {
                return Err(why);
            }
            let mut list = loaded.list;
            let before = list.clone();
            let done = apply_op(&mut list, op, expect, now, &mut random_u64)?;
            purge(&mut list, now);
            if repo_id.is_some() {
                list.repo_id = repo_id.map(String::from);
            }
            if list != before || loaded.set_aside.is_some() {
                list.revision = list.revision.saturating_add(1);
                list.format = TASKS_FORMAT;
                list.key = key.to_string();
                self.write(&files, &list)?;
            }
            Ok(done)
        })();
        Outcome { result, set_aside }
    }

    /// Brings the tasks of the list `from` into the list of `r` (both locked, in a fixed order),
    /// then renames `from`'s file aside (`.adopted-<time>`): kept, never deleted. `from` must still
    /// be one of `found(r, previous)`, checked under the locks. Refused when `r`'s list has live
    /// tasks, unless `merge`, and when `from` has nothing to bring. Tasks already there (same id)
    /// are skipped; a reference already used here gets a new one.
    pub fn adopt(&self, r: &Resolved, previous: Option<&str>, from: &str, merge: bool, now: u64) -> Outcome {
        let key = r.key.as_str();
        let repo_id = r.repo_id.as_deref();
        let (to_files, from_files) = (self.files(key), self.files(from));
        if from == key || to_files.path == from_files.path {
            return Outcome::failed(Problem::TasksSameList);
        }
        let mut locks = [&to_files.lock, &from_files.lock];
        locks.sort();
        let mut held = Vec::new();
        for path in locks {
            match self.lock(path) {
                Ok(l) => held.push(l),
                Err(p) => return Outcome::failed(p),
            }
        }
        // Still a list that may be this folder's (its folder may be back, its tasks gone...).
        if !self.found(r, previous).iter().any(|f| f.key == from) {
            return Outcome::failed(Problem::TasksNotAdoptable { from: from.into() });
        }
        self.clean_temp(&to_files);
        self.clean_temp(&from_files);
        let target = match self.load_locked(key, &to_files) {
            Ok(l) => l,
            Err(p) => return Outcome::failed(p),
        };
        let mut set_aside: Vec<Problem> = target.set_aside.iter().cloned().collect();
        let nothing = || Problem::TasksNothingToAdopt { from: from.into() };
        let result = (|| {
            if let Some(why) = target.read_only {
                return Err(why);
            }
            if !from_files.path.exists() {
                return Err(nothing());
            }
            let source = self.load_locked(from, &from_files)?;
            if let Some(p) = source.set_aside {
                set_aside.push(p);
                return Err(nothing());
            }
            if let Some(why) = source.read_only {
                return Err(why);
            }
            if source.list.live() == 0 {
                return Err(nothing());
            }
            let mut list = target.list;
            let live = list.live();
            if live > 0 && !merge {
                return Err(Problem::TasksNotEmpty { tasks: live });
            }
            let incoming: Vec<Task> = source.list.tasks.into_iter().filter(|t| !list.tasks.iter().any(|x| x.id == t.id)).collect();
            if live + incoming.iter().filter(|t| t.removed.is_none()).count() > MAX_TASKS {
                return Err(Problem::TasksTooMany { max: MAX_TASKS });
            }
            for mut t in incoming {
                if list.tasks.iter().any(|x| x.reference == t.reference) || list.retired.contains(&t.reference) {
                    t.reference = new_reference(&list.tasks, &list.retired, &mut random_u64);
                    t.updated = now;
                    t.rev = t.rev.saturating_add(1);
                }
                list.tasks.push(t);
            }
            for r in source.list.retired {
                if !list.retired.contains(&r) {
                    list.retired.push(r);
                }
            }
            trim_retired(&mut list);
            purge(&mut list, now);
            list.repo_id = repo_id.map(String::from).or(list.repo_id).or(source.list.repo_id);
            list.revision = list.revision.saturating_add(1);
            list.format = TASKS_FORMAT;
            list.key = key.to_string();
            self.write(&to_files, &list)?;
            if from_files.path.exists() {
                rename_aside(&from_files.path, "adopted").map_err(failed)?;
            }
            Ok(Done { task: None, content_rev: None })
        })();
        Outcome { result, set_aside }
    }

    /// Lists that may have been this folder's (see the module's documentation), with live tasks.
    pub fn found(&self, r: &Resolved, previous: Option<&str>) -> Vec<FoundList> {
        let mut keys: Vec<(String, FoundWhy)> = Vec::new();
        keys.extend(previous.map(|p| (p.to_string(), FoundWhy::PreviousKey)));
        keys.push((r.dir.clone(), FoundWhy::OwnFolder));
        // The same repository, whose folder is gone (moved or renamed).
        if let Some(id) = &r.repo_id {
            for entry in fs::read_dir(&self.dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                if let Some(Parsed::List(l) | Parsed::ReadOnly(l, _)) = read_bytes(&path).ok().flatten().map(|b| parse(&b, ""))
                    && l.repo_id.as_ref() == Some(id)
                    && !Path::new(&l.key).exists()
                {
                    keys.push((l.key, FoundWhy::SameHistory));
                }
            }
        }
        let mut found: Vec<FoundList> = Vec::new();
        for (k, why) in keys {
            if k == r.key || found.iter().any(|f| f.key == k) {
                continue;
            }
            let files = self.files(&k);
            if !files.path.exists() {
                continue;
            }
            if let Some(Parsed::List(l) | Parsed::ReadOnly(l, _)) = read_bytes(&files.path).ok().flatten().map(|b| parse(&b, &k))
                && l.live() > 0
            {
                found.push(FoundList { key: k, tasks: l.live(), why });
            }
        }
        found
    }
}

/// Renames a file to `<name>.<what>-<unix time>` (`-2`, `-3`... if taken): kept, never
/// overwritten.
fn rename_aside(path: &Path, what: &str) -> io::Result<PathBuf> {
    let stamp = unix_now();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for n in 1..1000 {
        let suffix = if n > 1 { format!("-{n}") } else { String::new() };
        let aside = path.with_file_name(format!("{name}.{what}-{stamp}{suffix}"));
        if fs::symlink_metadata(&aside).is_err() {
            fs::rename(path, &aside)?;
            return Ok(aside);
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free name to set the file aside"))
}

/// Writes `bytes` to `path` atomically: a temporary file (0600) next to it, flushed to disk,
/// renamed over it, then the folder flushed so the rename itself survives a crash.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp-{}-{}", std::process::id(), random_u64()));
    let written = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Random bits from the system (`/dev/urandom`), or from the clock and the process id.
fn random_u64() -> u64 {
    let mut buf = [0u8; 8];
    if File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).is_ok() {
        return u64::from_le_bytes(buf);
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64;
    fnv1a(&format!("{nanos}-{}", std::process::id()))
}

// --- Changes -------------------------------------------------------------------------------------

/// A short reference no task of `tasks` has, nor a purged one (`retired`): 3 characters, more
/// once those are mostly taken.
pub fn new_reference(tasks: &[Task], retired: &[String], random: &mut dyn FnMut() -> u64) -> String {
    let mut len = 3;
    loop {
        for _ in 0..64 {
            let mut bits = random();
            let reference: String = (0..len)
                .map(|_| {
                    let c = REF_ALPHABET[(bits % REF_ALPHABET.len() as u64) as usize] as char;
                    bits /= REF_ALPHABET.len() as u64;
                    c
                })
                .collect();
            if !tasks.iter().any(|t| t.reference == reference) && !retired.contains(&reference) {
                return reference;
            }
        }
        len += 1;
    }
}

fn live(list: &TaskList, id: &str) -> Result<usize, Problem> {
    list.tasks.iter().position(|t| t.id == id && t.removed.is_none()).ok_or_else(|| Problem::TaskNotFound { task: id.into() })
}

fn check_title(title: &str) -> Result<(), Problem> {
    if title.is_empty() {
        return Err(Problem::TaskTextEmpty);
    }
    if title.chars().count() > MAX_TASK_TITLE {
        return Err(Problem::TaskTooLong { field: TaskField::Title, max: MAX_TASK_TITLE });
    }
    Ok(())
}

fn check_description(description: &str) -> Result<(), Problem> {
    if description.len() > MAX_TASK_DESCRIPTION {
        return Err(Problem::TaskTooLong { field: TaskField::Description, max: MAX_TASK_DESCRIPTION });
    }
    Ok(())
}

/// Any change of a task.
fn touch(t: &mut Task, now: u64) {
    t.updated = now;
    t.rev = t.rev.saturating_add(1);
}

/// Applies `op` to the list in memory. A change that changes nothing (same status, a dependency
/// already there, a move to where it is) is fine and changes nothing.
pub fn apply_op(list: &mut TaskList, op: &TaskOp, expect: Option<u64>, now: u64, random: &mut dyn FnMut() -> u64) -> Result<Done, Problem> {
    // The task named, live (or in the trash, to restore it), and its fields as the caller saw them.
    if let Some(id) = op.task() {
        let i = match op {
            TaskOp::Restore { .. } => list.tasks.iter().position(|t| t.id == id).ok_or_else(|| Problem::TaskNotFound { task: id.into() })?,
            _ => live(list, id)?,
        };
        if let Some(rev) = expect
            && list.tasks[i].content_rev != rev
        {
            return Err(Problem::TaskStale { task: list.tasks[i].reference.clone() });
        }
    }
    match op {
        TaskOp::Add { title, description, priority, status } => {
            let title = title.trim();
            check_title(title)?;
            check_description(description)?;
            if list.live() >= MAX_TASKS {
                return Err(Problem::TasksTooMany { max: MAX_TASKS });
            }
            let id = format!("{:016x}", random());
            let reference = new_reference(&list.tasks, &list.retired, random);
            list.tasks.push(Task {
                id: id.clone(),
                reference,
                title: title.to_string(),
                description: description.clone(),
                status: *status,
                priority: *priority,
                depends_on: Vec::new(),
                notes: Vec::new(),
                created: now,
                updated: now,
                rev: 1,
                content_rev: 1,
                removed: None,
                worktree: None,
                branch: None,
                session: None,
                extra: Extra::new(),
            });
            return Ok(Done { task: Some(id), content_rev: Some(1) });
        }
        TaskOp::Edit { task, title, description, priority } => {
            let title = title.as_deref().map(str::trim);
            if let Some(x) = title {
                check_title(x)?;
            }
            if let Some(d) = description {
                check_description(d)?;
            }
            let i = live(list, task)?;
            let t = &mut list.tasks[i];
            let changed = title.is_some_and(|x| x != t.title) || description.as_ref().is_some_and(|d| *d != t.description) || priority.is_some_and(|p| p != t.priority);
            if changed {
                if let Some(x) = title {
                    t.title = x.to_string();
                }
                if let Some(d) = description {
                    t.description = d.clone();
                }
                if let Some(p) = priority {
                    t.priority = *p;
                }
                touch(t, now);
                t.content_rev = t.content_rev.saturating_add(1);
            }
        }
        TaskOp::SetStatus { task, status } => {
            let i = live(list, task)?;
            if list.tasks[i].status != *status {
                let mut t = list.tasks.remove(i);
                t.status = *status;
                touch(&mut t, now);
                list.tasks.push(t);
            }
        }
        TaskOp::Move { task, before } => {
            let i = live(list, task)?;
            let status = list.tasks[i].status;
            if let Some(b) = before {
                let j = live(list, b)?;
                if list.tasks[j].status != status {
                    return Err(Problem::TaskMoveRefused);
                }
            }
            let mut moved = list.tasks.clone();
            let t = moved.remove(i);
            match before.as_ref().filter(|b| *b != task).and_then(|b| moved.iter().position(|x| &x.id == b)) {
                Some(at) => moved.insert(at, t),
                None if before.as_ref() == Some(task) => moved.insert(i, t),
                // Last of its status: the list's order is the order within each status.
                None => moved.push(t),
            }
            let order = |v: &[Task]| v.iter().filter(|x| x.status == status && x.removed.is_none()).map(|x| x.id.clone()).collect::<Vec<_>>();
            if order(&moved) != order(&list.tasks) {
                list.tasks = moved;
                let at = list.tasks.iter().position(|x| &x.id == task).unwrap_or_default();
                touch(&mut list.tasks[at], now);
            }
        }
        TaskOp::AddNote { task, text, author } => {
            let text = text.trim();
            if text.is_empty() {
                return Err(Problem::TaskTextEmpty);
            }
            if text.len() > MAX_TASK_NOTE {
                return Err(Problem::TaskTooLong { field: TaskField::Note, max: MAX_TASK_NOTE });
            }
            let i = live(list, task)?;
            let t = &mut list.tasks[i];
            if t.notes.len() >= MAX_TASK_NOTES {
                return Err(Problem::TaskTooManyNotes { max: MAX_TASK_NOTES });
            }
            t.notes.push(TaskNote::new(text, now, *author));
            touch(t, now);
        }
        TaskOp::AddDependency { task, on } => {
            let i = live(list, task)?;
            let j = live(list, on)?;
            if !list.tasks[i].depends_on.contains(on) {
                if protocol::task_cycle(&list.tasks, task, on) {
                    return Err(Problem::TaskCycle { task: list.tasks[i].reference.clone(), on: list.tasks[j].reference.clone() });
                }
                let t = &mut list.tasks[i];
                t.depends_on.push(on.clone());
                touch(t, now);
            }
        }
        TaskOp::RemoveDependency { task, on } => {
            let i = live(list, task)?;
            let t = &mut list.tasks[i];
            if t.depends_on.contains(on) {
                t.depends_on.retain(|d| d != on);
                touch(t, now);
            }
        }
        TaskOp::Remove { task } => {
            let i = live(list, task)?;
            let t = &mut list.tasks[i];
            t.removed = Some(now);
            touch(t, now);
        }
        TaskOp::Restore { task } => {
            let i = list.tasks.iter().position(|t| &t.id == task).ok_or_else(|| Problem::TaskNotFound { task: task.clone() })?;
            if list.tasks[i].removed.is_some() && list.live() >= MAX_TASKS {
                return Err(Problem::TasksTooMany { max: MAX_TASKS });
            }
            let t = &mut list.tasks[i];
            if t.removed.take().is_some() {
                touch(t, now);
            }
        }
    }
    let task = op.task().map(String::from);
    let content_rev = task.as_ref().and_then(|id| list.tasks.iter().find(|t| &t.id == id)).map(|t| t.content_rev);
    Ok(Done { task, content_rev })
}

fn trim_retired(list: &mut TaskList) {
    if list.retired.len() > MAX_RETIRED {
        let extra = list.retired.len() - MAX_RETIRED;
        list.retired.drain(..extra);
    }
}

/// Drops the tasks removed more than `TRASH_SECS` ago (and the oldest removed ones past
/// `MAX_TRASH`), and the dependencies on them; their references are retired.
pub fn purge(list: &mut TaskList, now: u64) {
    let mut gone: Vec<(String, String)> = list.tasks.iter().filter(|t| t.removed.is_some_and(|at| now.saturating_sub(at) > TRASH_SECS)).map(|t| (t.id.clone(), t.reference.clone())).collect();
    // A trash past its bound loses its oldest tasks early.
    let mut trash: Vec<(u64, &Task)> = list.tasks.iter().filter_map(|t| t.removed.map(|at| (at, t))).filter(|(_, t)| !gone.iter().any(|(id, _)| *id == t.id)).collect();
    if trash.len() > MAX_TRASH {
        trash.sort_by_key(|(at, _)| *at);
        let extra = trash.len() - MAX_TRASH;
        gone.extend(trash[..extra].iter().map(|(_, t)| (t.id.clone(), t.reference.clone())));
    }
    if gone.is_empty() {
        return;
    }
    list.tasks.retain(|t| !gone.iter().any(|(id, _)| *id == t.id));
    for t in &mut list.tasks {
        t.depends_on.retain(|d| !gone.iter().any(|(id, _)| id == d));
    }
    list.retired.extend(gone.into_iter().map(|(_, r)| r));
    trim_retired(list);
}

// --- The subscriptions' thread -------------------------------------------------------------------

pub enum Cmd {
    Watch { key: String, path: String },
    Unwatch { key: String },
    Edit { id: u64, key: String, op: TaskOp, expect: Option<u64>, list: Option<String> },
    Adopt { id: u64, key: String, from: String, merge: bool, list: Option<String> },
}

struct Sub {
    path: String,
    resolved: Result<Resolved, Problem>,
    resolved_at: Instant,
    /// The key before the last change from one key to another (this session); never set by a
    /// resolution that failed.
    previous: Option<String>,
    /// The last key that resolved.
    last_ok: Option<String>,
    files: Option<Files>,
    stamp: Option<Stamp>,
    found: Vec<FoundList>,
    last: Option<TaskBoard>,
}

impl Sub {
    fn new(store: &Store, path: String) -> Self {
        let resolved = resolve_folder(&path);
        let last_ok = resolved.as_ref().ok().map(|r| r.key.clone());
        let mut sub = Sub { resolved, path, resolved_at: Instant::now(), previous: None, last_ok, files: None, stamp: None, found: Vec::new(), last: None };
        sub.settle(store);
        sub
    }

    fn key(&self) -> Option<&str> {
        self.resolved.as_ref().ok().map(|r| r.key.as_str())
    }

    /// After a new resolution: the list's file, and the lists that may have been this folder's.
    fn settle(&mut self, store: &Store) {
        self.files = self.key().map(|k| store.files(k));
        self.found = match &self.resolved {
            Ok(r) => store.found(r, self.previous.as_deref()),
            Err(_) => Vec::new(),
        };
    }

    /// Resolves the folder again; `true` when its key (or its problem) changed.
    fn refresh(&mut self, store: &Store) -> bool {
        let resolved = resolve_folder(&self.path);
        self.resolved_at = Instant::now();
        if resolved == self.resolved {
            return false;
        }
        // From one resolved key to another only: a failure in between changes nothing.
        if let Ok(r) = &resolved {
            if self.last_ok.as_ref().is_some_and(|k| *k != r.key) {
                self.previous = self.last_ok.clone();
            }
            self.last_ok = Some(r.key.clone());
        }
        self.resolved = resolved;
        self.settle(store);
        true
    }
}

/// Starts the task lists' thread. Produced events go through `send`.
pub fn spawn(send: impl Fn(&AgentEvent) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = mpsc::channel();
    let store = Store::new(data_dir());
    store.tighten();
    thread::spawn(move || run(rx, store, send));
    tx
}

fn run(rx: Receiver<Cmd>, store: Store, send: impl Fn(&AgentEvent)) {
    let mut subs: HashMap<String, Sub> = HashMap::new();
    let mut next_scan = Instant::now() + SCAN_EVERY;
    loop {
        match rx.recv_timeout(next_scan.saturating_duration_since(Instant::now())) {
            Ok(Cmd::Watch { key, path }) => {
                match subs.get_mut(&key) {
                    // Asked again (an agent the app reconnected to, a panel opened again): resolved
                    // afresh, the previous key kept.
                    Some(sub) if sub.path == path => {
                        sub.refresh(&store);
                    }
                    _ => {
                        subs.insert(key.clone(), Sub::new(&store, path));
                    }
                }
                let Some(sub) = subs.get_mut(&key) else { continue };
                sub.last = None;
                publish(&store, &key, sub, &send);
            }
            Ok(Cmd::Unwatch { key }) => {
                subs.remove(&key);
            }
            Ok(Cmd::Edit { id, key, op, expect, list }) => {
                let (outcome, list_key) = match subs.get_mut(&key) {
                    None => (Outcome::failed(Problem::TasksNotWatched), None),
                    Some(sub) => {
                        // Fresh: the folder may belong to another list since the last scan.
                        sub.refresh(&store);
                        match (&sub.resolved, list) {
                            (Err(p), _) => (Outcome::failed(p.clone()), None),
                            (Ok(r), Some(shown)) if shown != r.key => (Outcome::failed(Problem::TasksListChanged { shown, now: r.key.clone() }), Some(r.key.clone())),
                            (Ok(r), _) => (store.apply(&r.key, &op, expect, unix_now(), r.repo_id.as_deref()), Some(r.key.clone())),
                        }
                    }
                };
                answer(&send, id, &key, op.task(), outcome);
                republish(&store, &mut subs, list_key.as_deref(), &key, &send);
            }
            Ok(Cmd::Adopt { id, key, from, merge, list }) => {
                let (outcome, list_key) = match subs.get_mut(&key) {
                    None => (Outcome::failed(Problem::TasksNotWatched), None),
                    Some(sub) => {
                        sub.refresh(&store);
                        match (&sub.resolved, list) {
                            (Err(p), _) => (Outcome::failed(p.clone()), None),
                            (Ok(r), Some(shown)) if shown != r.key => (Outcome::failed(Problem::TasksListChanged { shown, now: r.key.clone() }), Some(r.key.clone())),
                            (Ok(r), _) => (store.adopt(r, sub.previous.as_deref(), &from, merge, unix_now()), Some(r.key.clone())),
                        }
                    }
                };
                answer(&send, id, &key, None, outcome);
                // What was found may be gone now.
                for sub in subs.values_mut() {
                    sub.settle(&store);
                }
                republish(&store, &mut subs, list_key.as_deref(), &key, &send);
            }
            Err(RecvTimeoutError::Timeout) => {
                for (key, sub) in subs.iter_mut() {
                    // Resolved again now and then; the lists found are looked for again too.
                    let due = sub.resolved_at.elapsed() >= RESOLVE_EVERY;
                    if due {
                        sub.refresh(&store);
                    }
                    let now = sub.files.as_ref().and_then(|f| stamp(&f.path));
                    if due || now != sub.stamp {
                        publish(&store, key, sub, &send);
                    }
                }
                next_scan = Instant::now() + SCAN_EVERY;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Answers a change; a file set aside on the way is reported whatever the result.
fn answer(send: &impl Fn(&AgentEvent), id: u64, key: &str, task: Option<&str>, outcome: Outcome) {
    for problem in outcome.set_aside {
        send(&AgentEvent::TasksNotice { key: key.into(), problem });
    }
    let ev = match outcome.result {
        Ok(done) => AgentEvent::TasksEdited { id, key: key.into(), task: done.task, problem: None, content_rev: done.content_rev },
        Err(problem) => AgentEvent::TasksEdited { id, key: key.into(), task: task.map(String::from), problem: Some(problem), content_rev: None },
    };
    send(&ev);
}

/// Every subscription of that list (and the asking one) sees the change now, not at the next scan.
fn republish(store: &Store, subs: &mut HashMap<String, Sub>, list_key: Option<&str>, asking: &str, send: &impl Fn(&AgentEvent)) {
    for (k, s) in subs.iter_mut().filter(|(k, s)| *k == asking || (list_key.is_some() && s.key() == list_key)) {
        publish(store, k, s, send);
    }
}

/// Reads the subscription's list and sends it when it differs from the last one sent. The lists
/// that may be this folder's are looked for again each time. A list another writer holds locked
/// while it does not read is left for the next scan.
fn publish(store: &Store, key: &str, sub: &mut Sub, send: &impl Fn(&AgentEvent)) {
    sub.found = match &sub.resolved {
        Ok(r) => store.found(r, sub.previous.as_deref()),
        Err(_) => Vec::new(),
    };
    let board = match (&sub.resolved, &sub.files) {
        (Err(problem), _) => TaskBoard { key: key.into(), list: None, read_only: true, problem: Some(problem.clone()), found: Vec::new() },
        (Ok(r), Some(files)) => {
            sub.stamp = stamp(&files.path);
            match store.read(files, &r.key) {
                Ok(loaded) => {
                    if let Some(problem) = loaded.set_aside {
                        send(&AgentEvent::TasksNotice { key: key.into(), problem });
                        sub.stamp = stamp(&files.path);
                    }
                    TaskBoard { key: key.into(), list: Some(loaded.list), read_only: loaded.read_only.is_some(), problem: loaded.read_only, found: sub.found.clone() }
                }
                Err(Problem::TasksLocked { .. }) => {
                    sub.stamp = None;
                    return;
                }
                Err(problem) => TaskBoard { key: key.into(), list: None, read_only: true, problem: Some(problem), found: sub.found.clone() },
            }
        }
        (Ok(_), None) => return,
    };
    if sub.last.as_ref() != Some(&board) {
        send(&AgentEvent::Tasks(Box::new(board.clone())));
        sub.last = Some(board);
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{Author, Priority, TaskStatus};
    use std::os::unix::process::ExitStatusExt;

    /// The statuses as written in a file.
    fn status_names() -> Vec<String> {
        TaskStatus::ALL.iter().map(|s| serde_json::to_value(s).unwrap().as_str().unwrap().to_string()).collect()
    }

    /// A scratch data folder, removed even when an assertion fails.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tcanvas-tasks-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            Scratch(dir)
        }

        fn store(&self) -> Store {
            Store::new(self.0.join("tasks"))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const KEY: &str = "/home/me/shop";

    fn read(store: &Store) -> Loaded {
        store.read(&store.files(KEY), KEY).unwrap()
    }

    fn apply(store: &Store, op: TaskOp, expect: Option<u64>) -> Result<Done, Problem> {
        store.apply(KEY, &op, expect, 100, None).result
    }

    fn add_op(title: &str) -> TaskOp {
        TaskOp::Add { title: title.into(), description: String::new(), priority: Priority::None, status: TaskStatus::Inbox }
    }

    fn add(store: &Store, title: &str) -> String {
        apply(store, add_op(title), None).unwrap().task.unwrap()
    }

    fn titles(store: &Store, status: TaskStatus) -> Vec<String> {
        read(store).list.tasks.iter().filter(|t| t.status == status && t.removed.is_none()).map(|t| t.title.clone()).collect()
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn output(code: i32, stdout: &str, stderr: &str) -> io::Result<Output> {
        Ok(Output { status: std::process::ExitStatus::from_raw(code << 8), stdout: stdout.into(), stderr: stderr.into() })
    }

    #[test]
    fn the_key_falls_back_to_the_folder_only_when_git_says_so() {
        let s = Scratch::new("classify");
        let plain = s.0.join("plain");
        fs::create_dir_all(&plain).unwrap();
        let folder = plain.to_string_lossy().into_owned();
        assert_eq!(classify_key(&plain, output(0, "worktree /nowhere/shop\nHEAD abc\n", "")), Ok("/nowhere/shop".into()));
        assert_eq!(classify_key(&plain, output(128, "", "fatal: not a git repository (or any of the parent directories): .git\n")), Ok(folder.clone()));
        // Any other failure: unknown, never another list.
        let dubious = classify_key(&plain, output(128, "", "fatal: detected dubious ownership in repository at '/x'\n"));
        assert!(matches!(dubious, Err(Problem::TasksKeyUnknown { detail }) if detail.contains("dubious")));
        assert!(matches!(classify_key(&plain, output(1, "", "")), Err(Problem::TasksKeyUnknown { .. })));
        assert!(matches!(classify_key(&plain, output(0, "", "")), Err(Problem::TasksKeyUnknown { .. })), "nothing listed");
        // A worktree whose main checkout is gone: git says "not a git repository", but not that
        // no parent holds one.
        let dangling = classify_key(&plain, output(128, "", "fatal: not a git repository: /home/me/shop/.git/worktrees/wt\n"));
        assert!(matches!(dangling, Err(Problem::TasksKeyUnknown { .. })));
        // Git says no repository, but a `.git` entry is there (a file pointing nowhere): unknown.
        let linked = s.0.join("linked");
        fs::create_dir_all(&linked).unwrap();
        fs::write(linked.join(".git"), "gitdir: /nowhere/.git/worktrees/x\n").unwrap();
        let outside = || output(128, "", "fatal: not a git repository (or any of the parent directories): .git\n");
        assert!(matches!(classify_key(&linked, outside()), Err(Problem::TasksKeyUnknown { .. })));
        let below = linked.join("src");
        fs::create_dir_all(&below).unwrap();
        assert!(matches!(classify_key(&below, outside()), Err(Problem::TasksKeyUnknown { .. })), "in a parent too");
        fs::remove_dir_all(&linked).unwrap();
        // No git: outside git only without a `.git` up the way.
        let missing = || Err(io::Error::new(io::ErrorKind::NotFound, "git: not found"));
        if !s.0.ancestors().any(|a| a.join(".git").exists()) {
            assert_eq!(classify_key(&plain, missing()), Ok(folder));
        }
        fs::create_dir_all(s.0.join(".git")).unwrap();
        assert!(matches!(classify_key(&plain, missing()), Err(Problem::TasksKeyUnknown { .. })));
    }

    #[test]
    fn file_names_are_readable_stable_and_distinct() {
        let a = file_name("/home/me/shop", 1);
        assert!(a.starts_with("shop-") && a.ends_with(".json") && a.len() == "shop-".len() + 16 + ".json".len(), "{a}");
        assert_eq!(a, file_name("/home/me/shop", 1), "stable");
        assert_ne!(a, file_name("/srv/shop", 1), "same last name, another key");
        assert!(file_name("/home/me/my shop/", 1).starts_with("my-shop-"));
        assert!(file_name("/", 1).starts_with("list-"));
        assert!(file_name("/home/me/shop", 2).ends_with("-2.json"));
        assert!(file_name(&format!("/x/{}", "a".repeat(300)), 1).len() < 80);
    }

    #[test]
    fn a_missing_list_reads_empty_and_creates_nothing() {
        let s = Scratch::new("missing");
        assert_eq!(read(&s.store()), Loaded { list: TaskList::empty(KEY), read_only: None, set_aside: None });
        assert!(!s.0.exists(), "reading writes nothing");
    }

    #[test]
    fn every_change_and_its_effects() {
        let s = Scratch::new("ops");
        let store = s.store();
        let a = add(&store, "Invoice PDF");
        let b = add(&store, "  Customer search ");
        let list = read(&store).list;
        assert_eq!((list.key.as_str(), list.revision, list.format), (KEY, 2, TASKS_FORMAT));
        let (ta, tb) = (&list.tasks[0], &list.tasks[1]);
        assert_eq!((ta.title.as_str(), tb.title.as_str()), ("Invoice PDF", "Customer search"), "titles trimmed");
        assert_eq!((ta.status, ta.rev, ta.content_rev, ta.created, ta.updated), (TaskStatus::Inbox, 1, 1, 100, 100));
        assert!(ta.reference.len() == 3 && ta.reference != tb.reference && ta.id != tb.id);

        let edit = TaskOp::Edit { task: a.clone(), title: Some("Invoice as PDF".into()), description: Some("## Why\nclients ask".into()), priority: Some(Priority::High) };
        assert_eq!(apply(&store, edit.clone(), Some(1)).unwrap(), Done { task: Some(a.clone()), content_rev: Some(2) });
        let t = read(&store).list.tasks[0].clone();
        assert_eq!((t.title.as_str(), t.description.as_str(), t.priority, t.rev, t.content_rev), ("Invoice as PDF", "## Why\nclients ask", Priority::High, 2, 2));
        // Nothing to change: not written again.
        let revision = read(&store).list.revision;
        apply(&store, edit, None).unwrap();
        assert_eq!(read(&store).list.revision, revision);
        assert_eq!(apply(&store, TaskOp::Edit { task: a.clone(), title: Some(" ".into()), description: None, priority: None }, None).unwrap_err(), Problem::TaskTextEmpty);

        // Status: last of its new status.
        let c = add(&store, "About page");
        for id in [&c, &a] {
            apply(&store, TaskOp::SetStatus { task: id.clone(), status: TaskStatus::Todo }, None).unwrap();
        }
        assert_eq!(titles(&store, TaskStatus::Todo), vec!["About page", "Invoice as PDF"]);
        assert_eq!(titles(&store, TaskStatus::Inbox), vec!["Customer search"]);

        // Notes, appended.
        apply(&store, TaskOp::AddNote { task: a.clone(), text: "asked Bob".into(), author: Author::User }, None).unwrap();
        apply(&store, TaskOp::AddNote { task: a.clone(), text: "draft in invoices/".into(), author: Author::Agent }, None).unwrap();
        assert_eq!(apply(&store, TaskOp::AddNote { task: a.clone(), text: "\n".into(), author: Author::User }, None).unwrap_err(), Problem::TaskTextEmpty);
        let notes = read(&store).list.tasks.iter().find(|t| t.id == a).unwrap().notes.clone();
        assert_eq!(notes, vec![TaskNote::new("asked Bob", 100, Author::User), TaskNote::new("draft in invoices/", 100, Author::Agent)]);

        // Unknown tasks.
        assert_eq!(apply(&store, TaskOp::Remove { task: "nope".into() }, None).unwrap_err(), Problem::TaskNotFound { task: "nope".into() });
        assert_eq!(apply(&store, TaskOp::AddDependency { task: a.clone(), on: "nope".into() }, None).unwrap_err(), Problem::TaskNotFound { task: "nope".into() });

        // Remove, then restore: the trash is in the file.
        apply(&store, TaskOp::Remove { task: b.clone() }, None).unwrap();
        assert!(titles(&store, TaskStatus::Inbox).is_empty());
        assert_eq!(read(&store).list.tasks.iter().find(|t| t.id == b).unwrap().removed, Some(100));
        assert_eq!(apply(&store, TaskOp::Edit { task: b.clone(), title: Some("x".into()), description: None, priority: None }, None).unwrap_err(), Problem::TaskNotFound { task: b.clone() });
        apply(&store, TaskOp::Restore { task: b.clone() }, None).unwrap();
        assert_eq!(titles(&store, TaskStatus::Inbox), vec!["Customer search"]);
    }

    #[test]
    fn manual_order_within_a_status() {
        let s = Scratch::new("order");
        let store = s.store();
        let a = add(&store, "a");
        let b = add(&store, "b");
        let c = add(&store, "c");
        let other = add(&store, "other");
        apply(&store, TaskOp::SetStatus { task: other.clone(), status: TaskStatus::Todo }, None).unwrap();
        let mv = |task: &str, before: Option<&str>| apply(&store, TaskOp::Move { task: task.into(), before: before.map(String::from) }, None);
        mv(&c, Some(&a)).unwrap();
        assert_eq!(titles(&store, TaskStatus::Inbox), vec!["c", "a", "b"]);
        mv(&c, None).unwrap();
        assert_eq!(titles(&store, TaskStatus::Inbox), vec!["a", "b", "c"], "last of its status");
        mv(&a, Some(&c)).unwrap();
        assert_eq!(titles(&store, TaskStatus::Inbox), vec!["b", "a", "c"]);
        // Moves that change nothing write nothing.
        let revision = read(&store).list.revision;
        mv(&b, Some(&b)).unwrap();
        mv(&a, Some(&c)).unwrap();
        mv(&c, None).unwrap();
        assert_eq!(read(&store).list.revision, revision, "no write");
        assert_eq!(titles(&store, TaskStatus::Inbox), vec!["b", "a", "c"]);
        // Before a task of another status: refused.
        assert_eq!(mv(&a, Some(&other)).unwrap_err(), Problem::TaskMoveRefused);
        assert_eq!(titles(&store, TaskStatus::Todo), vec!["other"], "other statuses untouched");
        assert_eq!(mv(&a, Some("gone")).unwrap_err(), Problem::TaskNotFound { task: "gone".into() });
    }

    #[test]
    fn dependencies_and_cycles() {
        let s = Scratch::new("deps");
        let store = s.store();
        let a = add(&store, "a");
        let b = add(&store, "b");
        let c = add(&store, "c");
        let dep = |task: &str, on: &str| apply(&store, TaskOp::AddDependency { task: task.into(), on: on.into() }, None);
        dep(&b, &a).unwrap();
        dep(&c, &b).unwrap();
        dep(&c, &b).unwrap();
        let list = read(&store).list;
        let refs: HashMap<&str, &str> = list.tasks.iter().map(|t| (t.id.as_str(), t.reference.as_str())).collect();
        assert_eq!(list.tasks[2].depends_on, vec![b.clone()], "added once");
        assert_eq!(dep(&a, &c).unwrap_err(), Problem::TaskCycle { task: refs[a.as_str()].into(), on: refs[c.as_str()].into() });
        assert!(matches!(dep(&a, &a).unwrap_err(), Problem::TaskCycle { .. }), "itself");
        apply(&store, TaskOp::RemoveDependency { task: c.clone(), on: b.clone() }, None).unwrap();
        assert!(read(&store).list.tasks[2].depends_on.is_empty());
        dep(&a, &c).unwrap();
    }

    #[test]
    fn only_a_content_change_makes_an_edit_stale() {
        let s = Scratch::new("stale");
        let store = s.store();
        let a = add(&store, "a");
        let b = add(&store, "b");
        let edit = |title: &str, expect| apply(&store, TaskOp::Edit { task: a.clone(), title: Some(title.into()), description: None, priority: None }, expect);
        // Notes, dependencies, moves and status changes leave the content revision alone.
        apply(&store, TaskOp::AddNote { task: a.clone(), text: "n".into(), author: Author::User }, None).unwrap();
        apply(&store, TaskOp::AddDependency { task: a.clone(), on: b.clone() }, None).unwrap();
        apply(&store, TaskOp::Move { task: a.clone(), before: None }, None).unwrap();
        apply(&store, TaskOp::SetStatus { task: a.clone(), status: TaskStatus::Todo }, None).unwrap();
        let t = read(&store).list.tasks.iter().find(|t| t.id == a).unwrap().clone();
        assert!(t.rev > 3 && t.content_rev == 1, "{t:?}");
        assert_eq!(edit("seen at 1", Some(1)).unwrap().content_rev, Some(2), "a note then a save: fine");
        // Someone else read revision 1 of the content too, then writes after that edit.
        assert_eq!(edit("overwrite", Some(1)).unwrap_err(), Problem::TaskStale { task: t.reference.clone() });
        assert_eq!(read(&store).list.tasks.iter().find(|t| t.id == a).unwrap().title, "seen at 1", "not overwritten");
        // Naming the current revision overwrites on purpose.
        edit("overwrite", Some(2)).unwrap();
        edit("no check", None).unwrap();
    }

    #[test]
    fn limits_are_typed_refusals() {
        let s = Scratch::new("limits");
        let store = s.store();
        let a = add(&store, "a");
        let long = "x".repeat(MAX_TASK_TITLE + 1);
        assert_eq!(apply(&store, add_op(&long), None).unwrap_err(), Problem::TaskTooLong { field: TaskField::Title, max: MAX_TASK_TITLE });
        add(&store, &"é".repeat(MAX_TASK_TITLE));
        let big = "x".repeat(MAX_TASK_DESCRIPTION + 1);
        let edit = TaskOp::Edit { task: a.clone(), title: None, description: Some(big), priority: None };
        assert_eq!(apply(&store, edit, None).unwrap_err(), Problem::TaskTooLong { field: TaskField::Description, max: MAX_TASK_DESCRIPTION });
        let note = |text: String| TaskOp::AddNote { task: a.clone(), text, author: Author::User };
        assert_eq!(apply(&store, note("x".repeat(MAX_TASK_NOTE + 1)), None).unwrap_err(), Problem::TaskTooLong { field: TaskField::Note, max: MAX_TASK_NOTE });
        let mut list = read(&store).list;
        list.tasks[0].notes = vec![TaskNote::new("n", 1, Author::User); MAX_TASK_NOTES];
        let model = list.tasks[1].clone();
        list.tasks.extend((list.tasks.len()..MAX_TASKS).map(|i| Task { id: format!("f{i}"), reference: format!("f{i}"), ..model.clone() }));
        fs::write(store.files(KEY).path, serde_json::to_vec(&list).unwrap()).unwrap();
        assert_eq!(apply(&store, note("one more".into()), None).unwrap_err(), Problem::TaskTooManyNotes { max: MAX_TASK_NOTES });
        assert_eq!(apply(&store, add_op("one more"), None).unwrap_err(), Problem::TasksTooMany { max: MAX_TASKS });
    }

    #[test]
    fn counters_saturate_instead_of_overflowing() {
        let s = Scratch::new("overflow");
        let store = s.store();
        let a = add(&store, "a");
        let mut list = read(&store).list;
        list.revision = u64::MAX;
        list.tasks[0].rev = u64::MAX;
        list.tasks[0].content_rev = u64::MAX;
        fs::write(store.files(KEY).path, serde_json::to_vec(&list).unwrap()).unwrap();
        apply(&store, TaskOp::Edit { task: a.clone(), title: Some("b".into()), description: None, priority: None }, Some(u64::MAX)).unwrap();
        let list = read(&store).list;
        assert_eq!((list.revision, list.tasks[0].rev, list.tasks[0].content_rev, list.tasks[0].title.as_str()), (u64::MAX, u64::MAX, u64::MAX, "b"));
    }

    #[test]
    fn the_trash_is_purged_after_a_week_and_its_references_retired() {
        let mut list = TaskList::empty(KEY);
        let mut n = 0;
        let mut random = || {
            n += 1;
            n
        };
        let old = apply_op(&mut list, &add_op("x"), None, 0, &mut random).unwrap().task.unwrap();
        let keep = apply_op(&mut list, &add_op("y"), None, 0, &mut random).unwrap().task.unwrap();
        let old_ref = list.tasks[0].reference.clone();
        apply_op(&mut list, &TaskOp::AddDependency { task: keep.clone(), on: old.clone() }, None, 0, &mut random).unwrap();
        apply_op(&mut list, &TaskOp::Remove { task: old.clone() }, None, 1000, &mut random).unwrap();
        purge(&mut list, 1000 + TRASH_SECS);
        assert_eq!(list.tasks.len(), 2, "still restorable");
        purge(&mut list, 1001 + TRASH_SECS);
        assert_eq!(list.tasks.iter().map(|t| t.id.clone()).collect::<Vec<_>>(), vec![keep]);
        assert!(list.tasks[0].depends_on.is_empty(), "no dangling dependency");
        assert_eq!(list.retired, vec![old_ref.clone()]);
        // A source that would give the retired reference again gives a longer one.
        let mut same = || 0u64;
        let tasks: Vec<Task> = Vec::new();
        let first = new_reference(&tasks, &[], &mut same);
        assert_ne!(new_reference(&tasks, std::slice::from_ref(&first), &mut same), first);
        // The memory is bounded.
        list.retired = (0..MAX_RETIRED + 3).map(|i| i.to_string()).collect();
        trim_retired(&mut list);
        assert_eq!((list.retired.len(), list.retired[0].as_str()), (MAX_RETIRED, "3"));
    }

    #[test]
    fn references_are_unique_and_grow_when_needed() {
        let mut same = || 7u64;
        let first = new_reference(&[], &[], &mut same);
        assert_eq!(first.len(), 3);
        let mut list = TaskList::empty(KEY);
        let mut seq = 0u64;
        let mut ids = || {
            seq += 1;
            if seq % 2 == 1 { seq } else { 7 }
        };
        apply_op(&mut list, &add_op("x"), None, 0, &mut ids).unwrap();
        assert_eq!(list.tasks[0].reference, first);
        let second = new_reference(&list.tasks, &[], &mut same);
        assert_eq!(second.len(), 4, "{second}");
        assert!(second.chars().all(|c| REF_ALPHABET.contains(&(c as u8))));
    }

    #[test]
    fn writes_are_atomic_private_and_clean_up_after_a_crash() {
        let s = Scratch::new("perms");
        let store = s.store();
        add(&store, "secret");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(store.dir()), 0o700);
        let files = store.files(KEY);
        assert_eq!(mode(&files.path), 0o600);
        assert_eq!(mode(&files.lock), 0o600);
        assert_eq!(files_in(store.dir()).len(), 2, "the list and its lock, no temporary file left");
        let text = fs::read_to_string(&files.path).unwrap();
        assert!(text.contains(r#""key": "/home/me/shop""#) && text.contains(r#""format": 1"#), "{text}");
        assert_eq!(head_revision(text.as_bytes()), Some(1), "the revision is at the top");
        // A temporary file left by a crash, holding task text: removed by the next write.
        let leftover = store.dir().join(format!(".{}.tmp-1-2", files.name));
        fs::write(&leftover, "secret").unwrap();
        let other = store.dir().join(".unrelated.json.tmp-1-2");
        fs::write(&other, "x").unwrap();
        add(&store, "again");
        assert!(!leftover.exists() && other.exists(), "only this list's");
        // A folder made too open is closed again on first use, and on the next write.
        fs::set_permissions(store.dir(), fs::Permissions::from_mode(0o755)).unwrap();
        store.tighten();
        assert_eq!(mode(store.dir()), 0o700);
        fs::set_permissions(store.dir(), fs::Permissions::from_mode(0o755)).unwrap();
        add(&store, "third");
        assert_eq!(mode(store.dir()), 0o700);
    }

    #[test]
    fn the_stamp_sees_a_new_revision() {
        assert_eq!(head_revision(br#"{"format": 1, "key": "/k", "revision" : 42, "tasks": []}"#), Some(42));
        assert_eq!(head_revision(br#"{"format": 1, "key": "/k"}"#), None);
        let s = Scratch::new("stamp");
        let store = s.store();
        add(&store, "a");
        let path = store.files(KEY).path;
        let before = stamp(&path).unwrap();
        add(&store, "b");
        assert_ne!(stamp(&path), Some(before));
        assert_eq!(stamp(&path).unwrap().revision, Some(2));
    }

    #[test]
    fn a_corrupt_file_is_set_aside_never_overwritten() {
        let s = Scratch::new("corrupt");
        let store = s.store();
        add(&store, "first");
        let path = store.files(KEY).path;
        fs::write(&path, "{\"format\": 1, \"key\": \"/home/me/shop\", \"tasks\": [{\"id\": ").unwrap();
        let loaded = read(&store);
        assert!(loaded.list.tasks.is_empty() && loaded.read_only.is_none());
        let Some(Problem::TasksSetAside { path: aside, detail }) = loaded.set_aside else { panic!("a notice: {loaded:?}") };
        assert!(aside.contains(".json.corrupt-") && !detail.is_empty(), "{aside} {detail}");
        assert_eq!(fs::read_to_string(&aside).unwrap(), "{\"format\": 1, \"key\": \"/home/me/shop\", \"tasks\": [{\"id\": ", "kept as is");
        assert!(!path.exists());
        add(&store, "after");
        assert_eq!(titles(&store, TaskStatus::Inbox), vec!["after"]);
        assert!(Path::new(&aside).exists());
        // Found by a change that then fails: the set-aside is still reported.
        fs::write(&path, "not json").unwrap();
        let outcome = store.apply(KEY, &TaskOp::Remove { task: "missing".into() }, None, 1, None);
        assert!(matches!(outcome.result, Err(Problem::TaskNotFound { .. })));
        assert!(matches!(&outcome.set_aside[..], [Problem::TasksSetAside { .. }]));
        assert_eq!(files_in(store.dir()).iter().filter(|n| n.contains(".corrupt-")).count(), 2);
        // Not UTF-8, or no format or key: not a list either.
        fs::write(&path, b"{\"format\": 1, \"key\": \"/home/me/shop\", \"x\": \"\xff\"}").unwrap();
        assert!(matches!(read(&store).set_aside, Some(Problem::TasksSetAside { detail, .. }) if detail.contains("UTF-8")));
        fs::write(&path, r#"{"key": "/home/me/shop", "tasks": []}"#).unwrap();
        assert!(read(&store).set_aside.is_some());
    }

    #[test]
    fn values_from_a_newer_writer_are_kept_and_shown_read_only() {
        let s = Scratch::new("unknown");
        let store = s.store();
        let a = add(&store, "first");
        let path = store.files(KEY).path;
        let mut value: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        value["tasks"][0]["notes"] = serde_json::json!([{"text": "from a robot", "at": 1, "author": "robot"}]);
        value["tasks"][0]["status"] = serde_json::json!("someday");
        let text = serde_json::to_string(&value).unwrap();
        fs::write(&path, &text).unwrap();
        let loaded = read(&store);
        assert!(loaded.set_aside.is_none(), "not set aside");
        assert!(matches!(loaded.read_only, Some(Problem::TasksUnknownValues { .. })));
        assert_eq!((loaded.list.tasks[0].title.as_str(), loaded.list.tasks[0].notes[0].text.as_str()), ("first", "from a robot"), "shown");
        let outcome = store.apply(KEY, &TaskOp::Remove { task: a }, None, 1, None);
        assert!(matches!(outcome.result, Err(Problem::TasksUnknownValues { .. })));
        assert_eq!(fs::read_to_string(&path).unwrap(), text, "never rewritten");
        assert!(status_names().contains(&"canceled".to_string()) && STATUSES.len() == TaskStatus::ALL.len());
    }

    #[test]
    fn fields_a_newer_writer_added_to_notes_survive_a_write() {
        let s = Scratch::new("note-extra");
        let store = s.store();
        let a = add(&store, "first");
        apply(&store, TaskOp::AddNote { task: a.clone(), text: "n".into(), author: Author::Agent }, None).unwrap();
        let path = store.files(KEY).path;
        let mut value: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        value["tasks"][0]["notes"][0]["model"] = serde_json::json!("opus");
        fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        apply(&store, TaskOp::AddNote { task: a, text: "m".into(), author: Author::User }, None).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains(r#""model": "opus""#));
    }

    #[test]
    fn duplicate_ids_and_a_newer_format_are_read_only() {
        let s = Scratch::new("newer");
        let store = s.store();
        add(&store, "first");
        let path = store.files(KEY).path;
        let dup = r#"{"format": 1, "key": "/home/me/shop", "tasks": [{"id": "a", "reference": "k3f", "title": "A"}, {"id": "a", "reference": "x9z", "title": "B"}]}"#;
        fs::write(&path, dup).unwrap();
        assert_eq!(read(&store).read_only, Some(Problem::TasksDuplicateId { id: "a".into() }));
        assert!(store.apply(KEY, &TaskOp::Remove { task: "a".into() }, None, 1, None).result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), dup);
        let newer = r#"{"format": 2, "key": "/home/me/shop", "revision": 9, "tasks": [{"id": "a", "reference": "k3f", "title": "From the future", "status": "someday"}]}"#;
        fs::write(&path, newer).unwrap();
        let loaded = read(&store);
        assert_eq!(loaded.read_only, Some(Problem::TasksNewerFormat { format: 2 }));
        assert_eq!(loaded.list.tasks[0].title, "From the future", "shown as far as it reads");
        assert_eq!(store.apply(KEY, &TaskOp::Remove { task: "a".into() }, None, 1, None).result.unwrap_err(), Problem::TasksNewerFormat { format: 2 });
        assert_eq!(fs::read_to_string(&path).unwrap(), newer, "never rewritten");
    }

    #[test]
    fn a_file_holding_another_key_is_left_alone() {
        let s = Scratch::new("collide");
        let store = s.store();
        fs::create_dir_all(store.dir()).unwrap();
        let other = TaskList { key: "/somewhere/else".into(), ..TaskList::empty("x") };
        fs::write(store.dir().join(file_name(KEY, 1)), serde_json::to_vec(&other).unwrap()).unwrap();
        add(&store, "mine");
        assert_eq!(store.files(KEY).path, store.dir().join(file_name(KEY, 2)));
        assert_eq!(titles(&store, TaskStatus::Inbox), vec!["mine"]);
        assert_eq!(serde_json::from_str::<TaskList>(&fs::read_to_string(store.dir().join(file_name(KEY, 1))).unwrap()).unwrap(), other);
    }

    #[test]
    fn concurrent_changes_lose_nothing() {
        let s = Scratch::new("threads");
        let dir = s.0.join("tasks");
        let writers: Vec<_> = (0..4)
            .map(|w| {
                let dir = dir.clone();
                thread::spawn(move || {
                    let store = Store::new(dir);
                    for i in 0..25 {
                        store.apply(KEY, &add_op(&format!("{w}-{i}")), None, 1, None).result.unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let store = Store::new(dir);
        let list = read(&store).list;
        assert_eq!((list.tasks.len(), list.revision), (100, 100));
        let mut refs: Vec<&str> = list.tasks.iter().map(|t| t.reference.as_str()).collect();
        refs.sort();
        refs.dedup();
        assert_eq!(refs.len(), 100, "references stay unique");
        assert!(store.files(KEY).lock.exists(), "the lock file is never deleted");
    }

    #[test]
    fn another_list_is_found_and_adopted() {
        let s = Scratch::new("adopt");
        let store = s.store();
        let old = "/home/me/notes";
        store.apply(old, &add_op("from the folder"), None, 1, None).result.unwrap();
        store.apply(old, &add_op("second"), None, 1, None).result.unwrap();
        // The folder became part of a repository: its own list is found.
        let r = Resolved { key: KEY.into(), dir: old.into(), repo_id: None };
        assert_eq!(store.found(&r, None), vec![FoundList { key: old.into(), tasks: 2, why: FoundWhy::OwnFolder }]);
        // Brought here: refused into a list with tasks unless merging.
        add(&store, "already here");
        assert_eq!(store.adopt(&r, None, old, false, 2).result.unwrap_err(), Problem::TasksNotEmpty { tasks: 1 });
        // Into itself: refused as such.
        assert_eq!(store.adopt(&r, None, KEY, true, 2).result.unwrap_err(), Problem::TasksSameList);
        // A reference already used here gets a new one.
        let mut list = read(&store).list;
        let theirs = Store::new(store.dir().into()).read(&store.files(old), old).unwrap().list;
        list.tasks[0].reference = theirs.tasks[0].reference.clone();
        fs::write(store.files(KEY).path, serde_json::to_vec(&list).unwrap()).unwrap();
        let rooted = Resolved { repo_id: Some("root".into()), ..r.clone() };
        store.adopt(&rooted, None, old, true, 2).result.unwrap();
        let merged = read(&store).list;
        assert_eq!(merged.tasks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), vec!["already here", "from the folder", "second"]);
        let mut refs: Vec<&str> = merged.tasks.iter().map(|t| t.reference.as_str()).collect();
        refs.dedup();
        assert_eq!(refs.len(), 3);
        assert_eq!(merged.repo_id.as_deref(), Some("root"));
        assert!(!store.files(old).path.exists(), "the old file renamed aside");
        assert_eq!(files_in(store.dir()).iter().filter(|n| n.contains(".adopted-")).count(), 1);
        assert!(store.found(&r, None).is_empty(), "nothing left to find");
        // Already adopted: nothing to bring, nothing rewritten.
        let revision = read(&store).list.revision;
        assert_eq!(store.adopt(&r, None, old, true, 3).result.unwrap_err(), Problem::TasksNotAdoptable { from: old.into() });
        assert_eq!(read(&store).list.revision, revision);

        // The previous key of this session is found too.
        let previous = Resolved { key: "/x".into(), dir: "/x".into(), repo_id: None };
        assert_eq!(store.found(&previous, Some(KEY)), vec![FoundList { key: KEY.into(), tasks: 3, why: FoundWhy::PreviousKey }]);
        // A key that is no candidate is refused.
        assert_eq!(store.adopt(&previous, None, KEY, false, 3).result.unwrap_err(), Problem::TasksNotAdoptable { from: KEY.into() });
    }

    #[test]
    fn a_list_of_the_same_history_is_adoptable_only_while_its_folder_is_gone() {
        let s = Scratch::new("same-history");
        let store = s.store();
        let gone = s.0.join("old-place/shop");
        let gone_key = gone.to_string_lossy().into_owned();
        store.apply(&gone_key, &add_op("before the move"), None, 1, Some("root-2")).result.unwrap();
        let here = Resolved { key: s.0.join("new-place/shop").to_string_lossy().into_owned(), dir: s.0.join("new-place/shop").to_string_lossy().into_owned(), repo_id: Some("root-2".into()) };
        assert_eq!(store.found(&here, None), vec![FoundList { key: gone_key.clone(), tasks: 1, why: FoundWhy::SameHistory }]);
        let other_repo = Resolved { repo_id: Some("root-3".into()), ..here.clone() };
        assert!(store.found(&other_repo, None).is_empty());
        // The folder is back (it was only gone for a moment, or it is another clone): no longer
        // a candidate, and adopting it is refused with nothing moved.
        fs::create_dir_all(&gone).unwrap();
        assert!(store.found(&here, None).is_empty());
        assert_eq!(store.adopt(&here, None, &gone_key, false, 2).result.unwrap_err(), Problem::TasksNotAdoptable { from: gone_key.clone() });
        assert!(store.files(&gone_key).path.exists(), "untouched");
        assert!(!store.files(&here.key).path.exists(), "nothing written here");
    }

    #[test]
    fn adopting_from_a_corrupt_or_empty_source_moves_nothing_and_says_so() {
        let s = Scratch::new("adopt-bad");
        let store = s.store();
        let old = "/home/me/notes";
        store.apply(old, &add_op("a"), None, 1, None).result.unwrap();
        let r = Resolved { key: KEY.into(), dir: old.into(), repo_id: None };
        assert_eq!(store.found(&r, None).len(), 1);
        fs::write(store.files(old).path, "{ broken").unwrap();
        let outcome = store.adopt(&r, None, old, false, 2);
        assert_eq!(outcome.result.unwrap_err(), Problem::TasksNotAdoptable { from: old.into() });
        assert!(!store.files(KEY).path.exists(), "the destination is not written");
        assert_eq!(fs::read_to_string(store.files(old).path).unwrap(), "{ broken", "the source is not touched");
        // A source holding only removed tasks: nothing to bring.
        let id = store.apply("/home/me/drafts", &add_op("b"), None, 1, None).result.unwrap().task.unwrap();
        store.apply("/home/me/drafts", &TaskOp::Remove { task: id }, None, 1, None).result.unwrap();
        let drafts = Resolved { key: KEY.into(), dir: "/home/me/drafts".into(), repo_id: None };
        assert_eq!(store.adopt(&drafts, None, "/home/me/drafts", false, 2).result.unwrap_err(), Problem::TasksNotAdoptable { from: "/home/me/drafts".into() });
        assert!(!store.files(KEY).path.exists());
    }

    #[test]
    fn a_read_only_source_is_refused_and_kept() {
        let s = Scratch::new("adopt-aside");
        let store = s.store();
        let old = "/home/me/notes";
        store.apply(old, &add_op("a"), None, 1, None).result.unwrap();
        let files = store.files(old);
        // Values from a newer writer: a candidate (it shows a live task), but read-only.
        let text = fs::read_to_string(&files.path).unwrap().replacen("\"status\": \"inbox\"", "\"status\": \"someday\"", 1);
        fs::write(&files.path, &text).unwrap();
        let r = Resolved { key: KEY.into(), dir: old.into(), repo_id: None };
        assert_eq!(store.found(&r, None).len(), 1);
        let outcome = store.adopt(&r, None, old, false, 2);
        assert!(matches!(outcome.result, Err(Problem::TasksUnknownValues { .. })), "{outcome:?}");
        assert!(!store.files(KEY).path.exists());
        assert_eq!(fs::read_to_string(&files.path).unwrap(), text);
    }

    #[test]
    fn the_trash_does_not_count_and_is_bounded() {
        let mut list = TaskList::empty(KEY);
        let mut n = 0u64;
        let mut random = || {
            n += 1;
            n
        };
        for i in 0..MAX_TASKS {
            let id = apply_op(&mut list, &add_op(&format!("t{i}")), None, 0, &mut random).unwrap().task.unwrap();
            apply_op(&mut list, &TaskOp::Remove { task: id }, None, i as u64, &mut random).unwrap();
        }
        // A thousand removed tasks: the list is empty and takes new ones.
        let kept = apply_op(&mut list, &add_op("new"), None, 0, &mut random).unwrap().task.unwrap();
        assert_eq!(list.live(), 1);
        // Past the trash bound, the oldest removed ones go early, their references retired.
        let oldest = list.tasks[0].reference.clone();
        apply_op(&mut list, &TaskOp::Remove { task: kept.clone() }, None, 5000, &mut random).unwrap();
        purge(&mut list, 5000);
        assert_eq!(list.tasks.iter().filter(|t| t.removed.is_some()).count(), MAX_TRASH);
        assert!(list.retired.contains(&oldest) && list.tasks.iter().any(|t| t.id == kept));
        // A restore that would pass the live bound is refused.
        let mut full = TaskList::empty(KEY);
        let gone = apply_op(&mut full, &add_op("gone"), None, 0, &mut random).unwrap().task.unwrap();
        apply_op(&mut full, &TaskOp::Remove { task: gone.clone() }, None, 0, &mut random).unwrap();
        for i in 0..MAX_TASKS {
            apply_op(&mut full, &add_op(&format!("l{i}")), None, 0, &mut random).unwrap();
        }
        assert_eq!(apply_op(&mut full, &TaskOp::Restore { task: gone }, None, 0, &mut random).unwrap_err(), Problem::TasksTooMany { max: MAX_TASKS });
    }

    /// The list's lock, held through another open file (as another process would hold it).
    fn hold_lock(store: &Store, key: &str) -> File {
        store.ensure_dir().unwrap();
        let f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(store.files(key).lock).unwrap();
        // SAFETY: a valid descriptor, owned by `f`.
        assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) }, 0);
        f
    }

    #[test]
    fn a_held_lock_gives_a_typed_failure_within_the_deadline() {
        let s = Scratch::new("held");
        let store = Store::with_lock_wait(s.0.join("tasks"), Duration::from_millis(300));
        add(&store, "a");
        let held = hold_lock(&store, KEY);
        let start = Instant::now();
        let outcome = store.apply(KEY, &add_op("b"), None, 1, None);
        assert_eq!(outcome.result.unwrap_err(), Problem::TasksLocked { secs: 0 });
        assert!(start.elapsed() < Duration::from_secs(2), "{:?}", start.elapsed());
        // Other lists still work meanwhile.
        store.apply("/home/me/other", &add_op("c"), None, 1, None).result.unwrap();
        drop(held);
        store.apply(KEY, &add_op("b"), None, 1, None).result.unwrap();
        assert_eq!(titles(&store, TaskStatus::Inbox), vec!["a", "b"]);
    }

    #[test]
    fn content_caught_mid_write_is_not_set_aside() {
        let s = Scratch::new("mid-write");
        let store = s.store();
        add(&store, "a");
        let files = store.files(KEY);
        let good = fs::read_to_string(&files.path).unwrap();
        // A text editor saving in place: half written, then whole again a moment later.
        fs::write(&files.path, &good[..good.len() / 2]).unwrap();
        let path = files.path.clone();
        let whole = good.clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            fs::write(&path, whole).unwrap();
        });
        let loaded = read(&store);
        writer.join().unwrap();
        assert!(loaded.set_aside.is_none(), "{loaded:?}");
        assert_eq!(loaded.list.tasks.len(), 1);
        // Still broken while another writer holds the lock: left alone, tried again later.
        fs::write(&files.path, "{ broken").unwrap();
        let held = hold_lock(&store, KEY);
        assert!(matches!(store.read(&files, KEY), Err(Problem::TasksLocked { .. })));
        assert_eq!(fs::read_to_string(&files.path).unwrap(), "{ broken");
        drop(held);
        assert!(read(&store).set_aside.is_some(), "once free, set aside");
    }
}
