//! Worktree setup recipe: what a new worktree needs that git does not check out (ignored files,
//! installed dependencies...). The repository describes it in `RECIPE_FILE`, at the root of its
//! main checkout:
//!
//! ```toml
//! [worktree]
//! copy = [".env", "config/*.local"]
//! symlink = ["node_modules"]
//! setup = "npm run db:seed"
//! ```
//!
//! plus the folders its Claude Code settings link (`.claude/settings.json`,
//! `worktree.symlinkDirectories`). Unknown keys are ignored. The agent applies `copy` and
//! `symlink` right after `git worktree add`; it never runs `setup` (the app runs it in the new
//! card's shell, once the user accepted that exact command).
//!
//! Patterns are relative to the main checkout, components separated by `/`: `*` (any characters
//! but `/`), `?` (one character), `**` (any number of components, alone in its component), every
//! other character literal. Wildcards never match a leading `.` (write `.*`), `**` does not enter
//! hidden folders, nothing ever matches `.git`, and no match goes through a symbolic link or into
//! another repository or worktree (a folder holding a `.git`). Refused: absolute, `~`, `..`,
//! `.git`, `[`, `]`, `{`, `}`, `\`.
//!
//! Applying: the destination is the same relative path in the new worktree. Anything already
//! there (a tracked file) is kept and reported; the way to it must be real folders of the
//! worktree, never a symbolic link. `copy`: files and folders whole, permissions kept, symbolic
//! links copied as links when their target is inside the main checkout (skipped otherwise), within
//! the limits (`limits`) in all. `symlink`: one absolute link per match, to the path in the main
//! checkout (still right after `git worktree move`; `git worktree remove` deletes the link, never
//! what it points to). A failure is reported, never undoes the worktree.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use protocol::{MAX_PATTERN_BYTES, PatternIssue, Problem, RECIPE_FILE, Recipe, RecipeAction, RecipeItem, RecipePattern, RecipeReport, SkipReason, path_inside};

/// Largest recipe file read.
pub const MAX_RECIPE_BYTES: u64 = 64 * 1024;
/// What all the copies of one recipe may add up to.
pub const MAX_COPY_BYTES: u64 = 4 << 30;
pub const MAX_COPY_ENTRIES: u64 = 200_000;
/// Folder entries one pattern's search may look at, before it stops.
pub const MAX_SEARCH_ENTRIES: usize = 200_000;
/// The Claude Code project settings, in the main checkout.
pub const CLAUDE_SETTINGS: &str = ".claude/settings.json";

/// The recipe file's `[worktree]` table.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub copy: Vec<String>,
    pub symlink: Vec<String>,
    pub setup: Option<String>,
}

/// Reads the recipe file's text. No `[worktree]` table: an empty recipe. Unknown keys are ignored;
/// a known key of the wrong type makes the whole file refused.
pub fn parse(text: &str) -> Result<Parsed, Problem> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| Problem::RecipeMalformed {
        file: RECIPE_FILE.into(),
        line: e.span().map(|s| text[..s.start.min(text.len())].matches('\n').count() as u64 + 1),
        detail: e.message().to_string(),
    })?;
    let wrong = |key: &str| Problem::RecipeWrongType { file: RECIPE_FILE.into(), key: key.into() };
    let Some(worktree) = table.get("worktree") else { return Ok(Parsed::default()) };
    let worktree = worktree.as_table().ok_or_else(|| wrong("worktree"))?;
    let list = |key: &str| -> Result<Vec<String>, Problem> {
        let name = format!("worktree.{key}");
        match worktree.get(key) {
            None => Ok(Vec::new()),
            Some(toml::Value::Array(items)) => items.iter().map(|v| v.as_str().map(String::from).ok_or_else(|| wrong(&name))).collect(),
            Some(_) => Err(wrong(&name)),
        }
    };
    let setup = match worktree.get("setup") {
        None => None,
        Some(toml::Value::String(s)) => Some(s.clone()).filter(|s| !s.trim().is_empty()),
        Some(_) => return Err(wrong("worktree.setup")),
    };
    Ok(Parsed { copy: list("copy")?, symlink: list("symlink")?, setup })
}

/// The Claude Code settings' `worktree.symlinkDirectories` (its strings). Anything malformed:
/// nothing, silently.
pub fn claude_symlinks(text: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let dirs = value.get("worktree").and_then(|w| w.get("symlinkDirectories")).and_then(|d| d.as_array());
    dirs.map(|d| d.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// A pattern with empty and `.` components dropped: two patterns naming the same paths compare
/// equal.
fn normalized(pattern: &str) -> String {
    pattern.split('/').filter(|s| !s.is_empty() && *s != ".").collect::<Vec<_>>().join("/")
}

/// The symlink patterns: the recipe's, then Claude Code's that it does not already have.
/// (pattern, from Claude Code).
pub fn merge(recipe: &[String], claude: &[String]) -> Vec<(String, bool)> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (pattern, from_claude) in recipe.iter().map(|p| (p, false)).chain(claude.iter().map(|p| (p, true))) {
        if seen.insert(normalized(pattern)) {
            out.push((pattern.clone(), from_claude));
        }
    }
    out
}

/// One component of a pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seg {
    Literal(String),
    /// With `*` or `?`.
    Glob(String),
    /// `**`: any number of components.
    AnyDepth,
}

fn is_git(name: &str) -> bool {
    name.eq_ignore_ascii_case(".git")
}

/// The pattern's components, or why it is refused.
pub fn validate(pattern: &str) -> Result<Vec<Seg>, PatternIssue> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(PatternIssue::TooLong);
    }
    if pattern.trim().is_empty() {
        return Err(PatternIssue::Empty);
    }
    if pattern.starts_with('/') {
        return Err(PatternIssue::Absolute);
    }
    if pattern.starts_with('~') {
        return Err(PatternIssue::Home);
    }
    let mut segs: Vec<Seg> = Vec::new();
    for part in pattern.split('/').filter(|s| !s.is_empty() && *s != ".") {
        if part == ".." {
            return Err(PatternIssue::ParentDir);
        }
        if is_git(part) {
            return Err(PatternIssue::GitDir);
        }
        if part.chars().any(|c| c.is_control() || matches!(c, '[' | ']' | '{' | '}' | '\\')) {
            return Err(PatternIssue::BadCharacter);
        }
        let seg = if part == "**" {
            Seg::AnyDepth
        } else if part.contains("**") {
            return Err(PatternIssue::BadGlob);
        } else if part.contains(['*', '?']) {
            Seg::Glob(part.to_string())
        } else {
            Seg::Literal(part.to_string())
        };
        if seg == Seg::AnyDepth && segs.last() == Some(&Seg::AnyDepth) {
            continue;
        }
        segs.push(seg);
    }
    if segs.is_empty() {
        return Err(PatternIssue::Empty);
    }
    Ok(segs)
}

/// Whether a file name matches a component with `*` and `?`. A leading `.` must be matched by a
/// literal `.`; `.git` never matches.
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    if is_git(name) || (name.starts_with('.') && !pattern.starts_with('.')) {
        return false;
    }
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    // Classic wildcard matching, backtracking to the last `*`.
    let (mut i, mut j) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        if i < p.len() && (p[i] == '?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some((i, j));
            i += 1;
        } else if let Some((si, sj)) = star {
            i = si + 1;
            j = sj + 1;
            star = Some((si, sj + 1));
        } else {
            return false;
        }
    }
    p[i..].iter().all(|c| *c == '*')
}

/// A folder holding a `.git` (folder or file): another repository, or a worktree.
fn holds_git(dir: &Path) -> bool {
    fs::symlink_metadata(dir.join(".git")).is_ok()
}

/// Counts one entry looked at; `false` once the bound is reached.
fn take(left: &mut usize) -> bool {
    if *left == 0 {
        return false;
    }
    *left -= 1;
    true
}

/// The entries of a folder for a search, read one at a time and each counted against `left`
/// (`None`: it ran out, nothing more is read), without `.git` nor names that are not UTF-8 (no
/// pattern can name them). Metadata not following symbolic links. Not sorted: the search sorts
/// what it found.
fn entries_within(dir: &Path, left: &mut usize) -> Option<Vec<(String, PathBuf, fs::Metadata)>> {
    let Ok(read) = fs::read_dir(dir) else { return Some(Vec::new()) };
    let mut out = Vec::new();
    for entry in read.flatten() {
        if !take(left) {
            return None;
        }
        let Ok(name) = entry.file_name().into_string() else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !is_git(&name) {
            out.push((name, entry.path(), meta));
        }
    }
    Some(out)
}

/// Entries one pattern's search may look at (`TCANVAS_RECIPE_SEARCH_LIMIT` lowers it, for tests).
pub fn search_limit() -> usize {
    std::env::var("TCANVAS_RECIPE_SEARCH_LIMIT").ok().and_then(|s| s.parse().ok()).unwrap_or(MAX_SEARCH_ENTRIES)
}

/// The paths of `root` (relative, `/`-separated) that `segs` matches, sorted, none inside another
/// one; and whether the search went to its end (`false`: it looked at `limit` folder entries
/// and stopped). Each pattern has its own bound; literal components are looked up directly and
/// cost nothing.
pub fn search(root: &Path, segs: &[Seg], limit: usize) -> (Vec<String>, bool) {
    let mut found = Vec::new();
    let mut left = limit;
    let complete = walk(root, "", segs, &mut found, &mut left);
    (outermost(found), complete)
}

fn walk(dir: &Path, rel: &str, segs: &[Seg], out: &mut Vec<String>, left: &mut usize) -> bool {
    let Some((first, rest)) = segs.split_first() else {
        if !rel.is_empty() {
            out.push(rel.to_string());
        }
        return true;
    };
    let join = |name: &str| if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") };
    // Going further down: only into real folders that are not another repository.
    let enter = |path: &Path, meta: &fs::Metadata| meta.is_dir() && !holds_git(path);
    match first {
        Seg::Literal(name) => {
            let path = dir.join(name);
            let Ok(meta) = fs::symlink_metadata(&path) else { return true };
            if rest.is_empty() {
                out.push(join(name));
                true
            } else if enter(&path, &meta) {
                walk(&path, &join(name), rest, out, left)
            } else {
                true
            }
        }
        Seg::Glob(pattern) => {
            let Some(entries) = entries_within(dir, left) else { return false };
            for (name, path, meta) in entries {
                if !glob_matches(pattern, &name) {
                    continue;
                }
                if rest.is_empty() {
                    out.push(join(&name));
                } else if enter(&path, &meta) && !walk(&path, &join(&name), rest, out, left) {
                    return false;
                }
            }
            true
        }
        Seg::AnyDepth => {
            // No component at all, then one more folder at a time.
            if !walk(dir, rel, rest, out, left) {
                return false;
            }
            let Some(entries) = entries_within(dir, left) else { return false };
            for (name, path, meta) in entries {
                if name.starts_with('.') || !enter(&path, &meta) {
                    continue;
                }
                if !walk(&path, &join(&name), segs, out, left) {
                    return false;
                }
            }
            true
        }
    }
}

/// Sorted, without duplicates, nor paths inside another one of the list.
pub fn outermost(mut paths: Vec<String>) -> Vec<String> {
    paths.sort();
    paths.dedup();
    let all: HashSet<&str> = paths.iter().map(String::as_str).collect();
    let inside = |p: &str| p.match_indices('/').any(|(i, _)| all.contains(&p[..i]));
    paths.iter().filter(|p| !inside(p)).cloned().collect()
}

/// What the main checkout says about new worktrees.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Source {
    pub parsed: Parsed,
    pub claude: Vec<String>,
    /// The recipe file could not be used (its contents are then left out).
    pub problem: Option<Problem>,
}

/// Why a file of the main checkout was not read.
#[derive(Debug)]
pub enum ReadError {
    Missing,
    /// A symbolic link, a folder, a FIFO, a device...
    NotAFile,
    TooBig,
    Failed(io::Error),
}

/// Opens a file without following a symbolic link and without blocking (a FIFO opens at once),
/// then asks the opened file what it is (`fstat`).
fn open_regular(path: &Path) -> Result<(fs::File, fs::Metadata), ReadError> {
    let file = match fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(ReadError::Missing),
        // `O_NOFOLLOW` on a symbolic link; a socket cannot be opened.
        Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENXIO | libc::ENOTDIR)) => return Err(ReadError::NotAFile),
        Err(e) => return Err(ReadError::Failed(e)),
    };
    let meta = file.metadata().map_err(ReadError::Failed)?;
    if !meta.file_type().is_file() {
        return Err(ReadError::NotAFile);
    }
    Ok((file, meta))
}

/// A regular file's text, at most `max` bytes: never through a symbolic link, never a FIFO or a
/// device, never more than `max + 1` bytes read whatever its size claims.
pub fn read_regular(path: &Path, max: u64) -> Result<String, ReadError> {
    use std::io::Read;
    let (file, _) = open_regular(path)?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes).map_err(ReadError::Failed)?;
    if bytes.len() as u64 > max {
        return Err(ReadError::TooBig);
    }
    String::from_utf8(bytes).map_err(|e| ReadError::Failed(io::Error::new(io::ErrorKind::InvalidData, e)))
}

/// The Claude Code settings' linked folders, read the same guarded way (a symbolic `.claude`
/// folder is not followed); anything wrong: none, silently.
fn claude_settings(main: &Path) -> Vec<String> {
    let real_folder = fs::symlink_metadata(main.join(".claude")).is_ok_and(|m| m.is_dir());
    if !real_folder {
        return Vec::new();
    }
    read_regular(&main.join(CLAUDE_SETTINGS), MAX_RECIPE_BYTES).map(|t| claude_symlinks(&t)).unwrap_or_default()
}

/// Reads the recipe file and the Claude Code settings of the main checkout `main`. `None`: the
/// repository has neither (no recipe file, no linked folder in the settings).
pub fn read(main: &Path) -> Option<Source> {
    let mut source = Source::default();
    let file = || RECIPE_FILE.to_string();
    let present = match read_regular(&main.join(RECIPE_FILE), MAX_RECIPE_BYTES) {
        Err(ReadError::Missing) => false,
        Err(ReadError::NotAFile) => {
            source.problem = Some(Problem::RecipeNotAFile { file: file() });
            true
        }
        Err(ReadError::TooBig) => {
            source.problem = Some(Problem::RecipeTooBig { file: file(), max_bytes: MAX_RECIPE_BYTES });
            true
        }
        Err(ReadError::Failed(e)) => {
            source.problem = Some(Problem::Unreadable { detail: format!("{RECIPE_FILE}: {e}") });
            true
        }
        Ok(text) => {
            match parse(&text) {
                Ok(parsed) => source.parsed = parsed,
                Err(problem) => source.problem = Some(problem),
            }
            true
        }
    };
    source.claude = claude_settings(main);
    (present || !source.claude.is_empty()).then_some(source)
}

/// The recipe of the main checkout `main` for the new-worktree dialog: each pattern with what it
/// matches now. Read only (run off the scan thread: the searches may take a while).
pub fn plan(main: &Path) -> Option<Recipe> {
    let real = fs::canonicalize(main).ok()?;
    let source = read(&real)?;
    let limit = search_limit();
    let describe = |pattern: &str, claude: bool| {
        let mut described = RecipePattern { pattern: pattern.to_string(), claude, matches: 0, refused: None, incomplete: false };
        match validate(pattern) {
            Err(issue) => described.refused = Some(issue),
            Ok(segs) => {
                let (found, complete) = search(&real, &segs, limit);
                described.matches = found.len();
                described.incomplete = !complete;
            }
        }
        described
    };
    let copy = source.parsed.copy.iter().map(|p| describe(p, false)).collect();
    let symlink = merge(&source.parsed.symlink, &source.claude).iter().map(|(p, claude)| describe(p, *claude)).collect();
    Some(Recipe { main: real.to_string_lossy().into_owned(), copy, symlink, setup: source.parsed.setup, problem: source.problem })
}

/// The copy limits (`TCANVAS_RECIPE_MAX_BYTES` and `TCANVAS_RECIPE_MAX_ENTRIES` lower them, for
/// tests).
pub fn limits() -> (u64, u64) {
    let var = |name: &str, default: u64| std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default);
    (var("TCANVAS_RECIPE_MAX_BYTES", MAX_COPY_BYTES), var("TCANVAS_RECIPE_MAX_ENTRIES", MAX_COPY_ENTRIES))
}

/// Whether the main checkout `main` has a recipe to apply.
pub fn present(main: &Path) -> bool {
    fs::canonicalize(main).ok().and_then(|real| read(&real)).is_some()
}

/// Applies the recipe of the main checkout `main` to the new worktree `worktree`. `None`: the
/// repository has no recipe.
pub fn apply(main: &Path, worktree: &Path) -> Option<RecipeReport> {
    let real_main = fs::canonicalize(main).ok()?;
    let source = read(&real_main)?;
    let mut report = RecipeReport { problem: source.problem.clone(), ..RecipeReport::default() };
    let real_wt = match fs::canonicalize(worktree) {
        Ok(real) => real,
        Err(e) => {
            report.push(RecipeItem { action: RecipeAction::Copy, path: String::new(), skipped: Some(SkipReason::Failed { detail: format!("{}: {e}", worktree.display()) }) });
            return Some(report);
        }
    };
    let (max_bytes, max_entries) = limits();
    let mut applier = Applier { main: real_main, wt: real_wt, max_bytes, max_entries, bytes: 0, entries: 0, stopped: false, report };
    let limit = search_limit();
    for pattern in &source.parsed.copy {
        applier.pattern(RecipeAction::Copy, pattern, limit);
    }
    for (pattern, _) in merge(&source.parsed.symlink, &source.claude) {
        applier.pattern(RecipeAction::Symlink, &pattern, limit);
    }
    Some(applier.report)
}

fn failed(e: io::Error) -> SkipReason {
    SkipReason::Failed { detail: e.to_string() }
}

/// Known limit: paths are checked, then used, by name. A process of the user's swapping a folder
/// of the main checkout or of the new worktree for a symbolic link between the check and the use
/// could redirect one read or write; files are still opened without following a final symbolic
/// link, and nothing is ever replaced.
struct Applier {
    /// Both resolved.
    main: PathBuf,
    wt: PathBuf,
    max_bytes: u64,
    max_entries: u64,
    bytes: u64,
    entries: u64,
    /// A copy limit was reached: no more copies.
    stopped: bool,
    report: RecipeReport,
}

impl Applier {
    fn cap(&self) -> SkipReason {
        SkipReason::CapReached { max_bytes: self.max_bytes, max_entries: self.max_entries }
    }

    fn pattern(&mut self, action: RecipeAction, pattern: &str, limit: usize) {
        let item = |path: &str, skipped| RecipeItem { action, path: path.to_string(), skipped };
        let segs = match validate(pattern) {
            Ok(segs) => segs,
            Err(issue) => return self.report.push(item(pattern, Some(SkipReason::Refused { issue }))),
        };
        let (found, complete) = search(&self.main, &segs, limit);
        if !complete {
            self.report.push(item(pattern, Some(SkipReason::SearchLimit)));
        } else if found.is_empty() {
            self.report.push(item(pattern, Some(SkipReason::NoMatch)));
        }
        for rel in found {
            let skipped = match action {
                RecipeAction::Copy => self.copy(&rel),
                RecipeAction::Symlink => self.link(&rel),
            }
            .err();
            self.report.push(item(&rel, skipped));
        }
    }

    /// The destination of `rel` in the worktree, its missing folders created: only through real
    /// folders of the worktree; nothing there yet.
    fn destination(&self, rel: &str) -> Result<PathBuf, SkipReason> {
        let parts: Vec<&str> = rel.split('/').collect();
        let (last, parents) = parts.split_last().ok_or(SkipReason::Escapes)?;
        let mut current = self.wt.clone();
        for part in parents {
            let next = current.join(part);
            match fs::symlink_metadata(&next) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(SkipReason::Escapes),
                Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&next).map_err(failed)?,
                Err(e) => return Err(failed(e)),
            }
            current = next;
        }
        let dst = current.join(last);
        match fs::symlink_metadata(&dst) {
            Ok(_) => Err(SkipReason::Exists),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(dst),
            Err(e) => Err(failed(e)),
        }
    }

    /// A link to the path in the main checkout; never to another repository or worktree, nor to
    /// the new worktree itself or a folder holding it.
    fn link(&mut self, rel: &str) -> Result<(), SkipReason> {
        let target = self.main.join(rel);
        if self.wt.starts_with(&target) {
            return Err(SkipReason::ContainsWorktree);
        }
        if fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir()) && holds_git(&target) {
            return Err(SkipReason::NestedRepository);
        }
        let dst = self.destination(rel)?;
        symlink(target, &dst).map_err(failed)
    }

    fn copy(&mut self, rel: &str) -> Result<(), SkipReason> {
        if self.stopped {
            return Err(self.cap());
        }
        let dst = self.destination(rel)?;
        self.copy_entry(&self.main.join(rel), &dst, rel)
    }

    /// Counts one entry against the limits.
    fn count(&mut self) -> Result<(), SkipReason> {
        if self.stopped || self.entries + 1 > self.max_entries {
            self.stopped = true;
            return Err(self.cap());
        }
        self.entries += 1;
        Ok(())
    }

    fn copy_entry(&mut self, src: &Path, dst: &Path, rel: &str) -> Result<(), SkipReason> {
        let meta = fs::symlink_metadata(src).map_err(failed)?;
        let kind = meta.file_type();
        if kind.is_symlink() {
            let target = fs::read_link(src).map_err(failed)?;
            let inside = fs::canonicalize(src).is_ok_and(|real| path_inside(&self.main.to_string_lossy(), &real.to_string_lossy()));
            if !inside {
                return Err(SkipReason::LinkOutside);
            }
            self.count()?;
            symlink(&target, dst).map_err(failed)?;
            // Copied as written: a relative target may not exist in the new worktree.
            if fs::metadata(dst).is_err() { Err(SkipReason::DanglingLink) } else { Ok(()) }
        } else if kind.is_file() {
            self.copy_file(src, dst)
        } else if kind.is_dir() {
            self.copy_dir(src, dst, rel, &meta)
        } else {
            Err(SkipReason::Unsupported)
        }
    }

    /// A folder whole; what could not be copied in it is listed, and the folder is then `Partial`.
    fn copy_dir(&mut self, src: &Path, dst: &Path, rel: &str, meta: &fs::Metadata) -> Result<(), SkipReason> {
        if holds_git(src) {
            return Err(SkipReason::NestedRepository);
        }
        let read = fs::read_dir(src).map_err(|e| SkipReason::Unreadable { detail: e.to_string() })?;
        self.count()?;
        fs::create_dir(dst).map_err(failed)?;
        let mut partial = false;
        let mut skip = |this: &mut Self, path: String, reason: SkipReason| {
            partial = true;
            this.report.push(RecipeItem { action: RecipeAction::Copy, path, skipped: Some(reason) });
        };
        // Read one at a time, never more than the entry limit allows (then sorted, for a stable
        // report).
        let room = self.max_entries.saturating_sub(self.entries) as usize + 1;
        let mut names = Vec::new();
        for entry in read {
            if names.len() >= room {
                break;
            }
            match entry {
                Ok(entry) => names.push(entry.file_name()),
                Err(e) => skip(self, format!("{rel}/?"), SkipReason::Unreadable { detail: e.to_string() }),
            }
        }
        names.sort();
        for name in names {
            let Some(text) = name.to_str().filter(|n| !is_git(n)) else {
                if name.to_str().is_none() {
                    skip(self, format!("{rel}/{}", name.to_string_lossy()), SkipReason::BadName);
                }
                continue;
            };
            let inner = format!("{rel}/{text}");
            match self.copy_entry(&src.join(&name), &dst.join(&name), &inner) {
                Ok(()) => {}
                Err(stop @ SkipReason::CapReached { .. }) => return Err(stop),
                Err(reason) => skip(self, inner, reason),
            }
        }
        // Last: a folder without write permission still gets its contents.
        fs::set_permissions(dst, fs::Permissions::from_mode(meta.permissions().mode() & 0o777)).map_err(failed)?;
        if partial { Err(SkipReason::Partial) } else { Ok(()) }
    }

    /// Copies a regular file into a new one, never replacing anything: opened without following a
    /// symbolic link nor blocking, checked on the opened file (`fstat`: a FIFO or a device swapped
    /// in is refused), and never more bytes than the limit leaves, even if it grows meanwhile.
    fn copy_file(&mut self, src: &Path, dst: &Path) -> Result<(), SkipReason> {
        let (from, meta) = match open_regular(src) {
            Ok(opened) => opened,
            Err(ReadError::Failed(e)) => return Err(failed(e)),
            Err(_) => return Err(SkipReason::Unsupported),
        };
        let room = self.max_bytes.saturating_sub(self.bytes);
        if meta.len() > room {
            self.stopped = true;
            return Err(self.cap());
        }
        self.count()?;
        let mut to = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(dst).map_err(failed)?;
        let copied = io::copy(&mut io::Read::take(from, room + 1), &mut to).map_err(failed)?;
        if copied > room {
            drop(to);
            let _ = fs::remove_file(dst);
            self.stopped = true;
            return Err(self.cap());
        }
        self.bytes += copied;
        to.set_permissions(fs::Permissions::from_mode(meta.permissions().mode() & 0o777)).map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_recipe_is_read_from_its_worktree_table() {
        let text = "[worktree]\ncopy = [\".env\", \"config/*.local\"]\nsymlink = [\"node_modules\"]\nsetup = \"npm ci && npm run seed\"\n";
        assert_eq!(parse(text), Ok(Parsed { copy: s(&[".env", "config/*.local"]), symlink: s(&["node_modules"]), setup: Some("npm ci && npm run seed".into()) }));
        // A multi-line setup, kept exactly.
        let multi = "[worktree]\nsetup = '''\ncp .env.example .env\nnpm ci\n'''\n";
        assert_eq!(parse(multi).unwrap().setup.as_deref(), Some("cp .env.example .env\nnpm ci\n"));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let text = "title = \"shop\"\n[worktree]\ncopy = [\".env\"]\nports = [3000]\n[worktree.later]\nx = 1\n[other]\ny = true\n";
        assert_eq!(parse(text), Ok(Parsed { copy: s(&[".env"]), ..Parsed::default() }));
    }

    #[test]
    fn an_empty_recipe_does_nothing() {
        for text in ["", "# nothing yet\n", "[worktree]\n", "[worktree]\nsetup = \"  \"\n", "[other]\nx = 1\n"] {
            assert_eq!(parse(text), Ok(Parsed::default()), "{text:?}");
        }
    }

    #[test]
    fn a_malformed_recipe_is_a_typed_problem() {
        let Err(Problem::RecipeMalformed { file, line, detail }) = parse("[worktree]\ncopy = [\".env\"\n") else { panic!("malformed") };
        assert_eq!((file.as_str(), line.is_some(), detail.is_empty()), (RECIPE_FILE, true, false));
        let Err(Problem::RecipeMalformed { line, .. }) = parse("[worktree]\n\nsetup = = 1\n") else { panic!("malformed") };
        assert_eq!(line, Some(3));
        let wrong = |key: &str| Err(Problem::RecipeWrongType { file: RECIPE_FILE.into(), key: key.into() });
        assert_eq!(parse("worktree = 1\n"), wrong("worktree"));
        assert_eq!(parse("[worktree]\ncopy = \".env\"\n"), wrong("worktree.copy"));
        assert_eq!(parse("[worktree]\nsymlink = [\"a\", 2]\n"), wrong("worktree.symlink"));
        assert_eq!(parse("[worktree]\nsetup = [\"npm ci\"]\n"), wrong("worktree.setup"));
    }

    #[test]
    fn claude_code_directories_are_merged_without_duplicates() {
        let settings = r#"{"permissions": {}, "worktree": {"symlinkDirectories": ["node_modules", ".venv", 3, "./vendor/"]}}"#;
        let claude = claude_symlinks(settings);
        assert_eq!(claude, s(&["node_modules", ".venv", "./vendor/"]), "non-strings left out");
        let merged = merge(&s(&["node_modules/", "assets/big"]), &claude);
        assert_eq!(merged, vec![("node_modules/".into(), false), ("assets/big".into(), false), (".venv".into(), true), ("./vendor/".into(), true)]);
        assert_eq!(merge(&s(&["vendor"]), &s(&["./vendor/"])), vec![("vendor".into(), false)], "the same folder written differently");
        for malformed in ["", "{", "[]", r#"{"worktree": 1}"#, r#"{"worktree": {"symlinkDirectories": "node_modules"}}"#] {
            assert!(claude_symlinks(malformed).is_empty(), "{malformed:?}");
        }
    }

    #[test]
    fn patterns_cannot_leave_the_repository() {
        for (pattern, issue) in [
            ("", PatternIssue::Empty),
            ("  ", PatternIssue::Empty),
            ("./", PatternIssue::Empty),
            ("/etc/passwd", PatternIssue::Absolute),
            ("~/.ssh", PatternIssue::Home),
            ("~", PatternIssue::Home),
            ("..", PatternIssue::ParentDir),
            ("../shop-secrets/.env", PatternIssue::ParentDir),
            ("config/../../x", PatternIssue::ParentDir),
            (".git", PatternIssue::GitDir),
            (".git/config", PatternIssue::GitDir),
            ("sub/.GIT/hooks", PatternIssue::GitDir),
            ("a[b]", PatternIssue::BadCharacter),
            ("{a,b}", PatternIssue::BadCharacter),
            ("C:\\x", PatternIssue::BadCharacter),
            ("a\nb", PatternIssue::BadCharacter),
            ("a**", PatternIssue::BadGlob),
            ("**b/x", PatternIssue::BadGlob),
        ] {
            assert_eq!(validate(pattern), Err(issue), "{pattern:?}");
        }
        assert_eq!(validate(&"a".repeat(MAX_PATTERN_BYTES + 1)), Err(PatternIssue::TooLong));
        let lit = |s: &str| Seg::Literal(s.into());
        assert_eq!(validate(".env"), Ok(vec![lit(".env")]));
        assert_eq!(validate("./config//app.local/"), Ok(vec![lit("config"), lit("app.local")]));
        assert_eq!(validate("**/**/*.env"), Ok(vec![Seg::AnyDepth, Seg::Glob("*.env".into())]));
        assert_eq!(validate("my dir/.env.*"), Ok(vec![lit("my dir"), Seg::Glob(".env.*".into())]));
    }

    #[test]
    fn wildcards_match_within_a_component() {
        assert!(glob_matches("*.local", "app.local"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("a?c", "abc") && !glob_matches("a?c", "ac"));
        assert!(glob_matches("*a*b*", "xxaxxbxx") && !glob_matches("*a*b", "xxbxxa"));
        assert!(glob_matches("caf\u{e9}?", "caf\u{e9}s"), "characters, not bytes");
        // Hidden files only with a literal leading dot; `.git` never.
        assert!(!glob_matches("*", ".env") && !glob_matches("?env", ".env"));
        assert!(glob_matches(".*", ".env") && glob_matches(".env*", ".env.local"));
        assert!(!glob_matches(".*", ".git") && !glob_matches(".g?t", ".GIT"));
    }

    #[test]
    fn the_outermost_matches_are_kept() {
        assert_eq!(outermost(s(&["a/b", "a", "a-b", "a/b/c", "c", "a"])), s(&["a", "a-b", "c"]));
    }

    /// A scratch folder, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tcanvas-recipe-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(fs::canonicalize(&dir).unwrap())
        }

        fn file(&self, rel: &str) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, rel).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn found(root: &Path, pattern: &str) -> Vec<String> {
        let (found, complete) = search(root, &validate(pattern).unwrap(), MAX_SEARCH_ENTRIES);
        assert!(complete);
        found
    }

    #[test]
    fn a_fifo_swapped_in_for_a_file_is_refused_without_blocking() {
        let dir = Scratch::new("fifo");
        let fifo = dir.0.join("pipe");
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        assert!(matches!(read_regular(&fifo, 10), Err(ReadError::NotAFile)));
        fs::create_dir(dir.0.join("wt")).unwrap();
        let mut applier = Applier { main: dir.0.clone(), wt: dir.0.join("wt"), max_bytes: 100, max_entries: 10, bytes: 0, entries: 0, stopped: false, report: RecipeReport::default() };
        assert_eq!(applier.copy_file(&fifo, &dir.0.join("wt/pipe")), Err(SkipReason::Unsupported));
        assert!(!dir.0.join("wt/pipe").exists());
        // A regular file is read up to its bound, and no further.
        dir.file("small");
        assert_eq!(read_regular(&dir.0.join("small"), 5).unwrap(), "small");
        assert!(matches!(read_regular(&dir.0.join("small"), 4), Err(ReadError::TooBig)));
        symlink(dir.0.join("small"), dir.0.join("link")).unwrap();
        assert!(matches!(read_regular(&dir.0.join("link"), 10), Err(ReadError::NotAFile)), "never through a link");
        assert!(matches!(read_regular(&dir.0.join("missing"), 10), Err(ReadError::Missing)));
    }

    #[test]
    fn each_wildcard_finds_what_it_should() {
        let repo = Scratch::new("search");
        for f in [".env", ".env.local", "a.env", "config/app.local", "config/db.local", "config/app.toml", "web/.env", "web/src/.env", ".hidden/.env", "node_modules/x/.env"] {
            repo.file(f);
        }
        fs::create_dir_all(repo.0.join(".git")).unwrap();
        repo.file(".git/config");
        // Another worktree inside the repository: never entered.
        repo.file("wt/feat/.env");
        repo.file("wt/feat/.git");
        // A symbolic link to a folder: matched itself, never gone through.
        std::os::unix::fs::symlink(repo.0.join("config"), repo.0.join("linked")).unwrap();

        assert_eq!(found(&repo.0, ".env"), s(&[".env"]));
        assert_eq!(found(&repo.0, "*.env"), s(&["a.env"]), "no hidden file for a bare `*`");
        assert_eq!(found(&repo.0, ".env*"), s(&[".env", ".env.local"]));
        assert_eq!(found(&repo.0, "config/*.local"), s(&["config/app.local", "config/db.local"]));
        assert_eq!(found(&repo.0, "config/???.local"), s(&["config/app.local"]));
        assert_eq!(found(&repo.0, "**/.env"), s(&[".env", "node_modules/x/.env", "web/.env", "web/src/.env"]), "no hidden folder, no other worktree");
        assert_eq!(found(&repo.0, "web/**"), s(&["web"]));
        assert_eq!(found(&repo.0, ".*"), s(&[".env", ".env.local", ".hidden"]), "never .git");
        assert_eq!(found(&repo.0, "linked"), s(&["linked"]));
        assert!(found(&repo.0, "linked/*").is_empty(), "not through a symbolic link");
        assert!(found(&repo.0, "wt/feat/.env").is_empty(), "not into another worktree");
        assert!(found(&repo.0, "missing/*").is_empty());
        // The search stops at its bound.
        let (_, complete) = search(&repo.0, &validate("**/.env").unwrap(), 3);
        assert!(!complete);
        // A literal path costs nothing: found whatever the bound.
        assert_eq!(search(&repo.0, &validate("web/src/.env").unwrap(), 0), (s(&["web/src/.env"]), true));
    }
}
