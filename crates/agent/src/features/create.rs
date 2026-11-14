//! Creating a worktree for a watched folder's repository, on a new branch from a base or on an
//! existing local branch that no worktree has checked out.
//!
//! The app names a branch, a folder name and a location template; the agent never trusts them.
//! It computes the folder itself from the main checkout (`protocol::worktree_destination`), then
//! resolves it on disk: the deepest existing folder on the way, symlinks followed, must still be
//! inside the allowed place (the main checkout, or the folder holding it for a `../` template)
//! and outside the repository's git files, and the worktree's folder itself must not exist.
//!
//! The only commands that write are `git worktree add -b <branch> -- <path> <commit>` and `git
//! worktree add -- <path> <branch>` (never `--force`, `-B` nor `--detach`), plus, when asked, a
//! block appended to the repository's local `info/exclude` (never the tracked `.gitignore`).
//! Everything is checked before the first write; if git then fails, the exclude block and the
//! folders created for the worktree's parent are taken back.
//!
//! Known limit: if the agent (or git) is killed while `git worktree add` checks files out, a
//! partial folder and its git entry stay. The plan then reports the folder as existing (and
//! proposes a free name); the leftover shows in the worktrees strip, where the cleanup window
//! deals with it once it is merged or its folder is deleted (then pruned).

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use protocol::{
    CreateWorktree, DEFAULT_WORKTREE_LOCATION, LocalBranch, LocationIssue, MAX_NAME_BYTES, Problem, Worktree, WorktreeBranch, WorktreePlan, folder_for_branch, parse_location, path_inside,
    valid_folder, worktree_destination,
};

use super::changes::git_ok;
use super::cleanup::{Written, git_write_within, write_limit};
use super::{recipe, worktrees};

/// Free folder names tried after the one asked (`<folder>-2` ... `<folder>-<n>`).
const FREE_TRIES: u32 = 50;
/// The line written above the pattern in `info/exclude`.
const EXCLUDE_HEADER: &str = "# Worktrees created by tcanvas";

/// The worktrees as git lists them now (main checkout first). `None` outside git.
fn listed(dir: &Path) -> Option<Vec<Worktree>> {
    let list = worktrees::parse_porcelain(&git_ok(dir, &["worktree", "list", "--porcelain"])?);
    list.first()?;
    Some(list)
}

/// Where the git commands run: the main checkout, or the watched folder if that one is gone.
fn main_dir(main: &Worktree, dir: &Path) -> PathBuf {
    Some(PathBuf::from(&main.path)).filter(|p| p.is_dir()).unwrap_or_else(|| dir.to_path_buf())
}

/// The folder for `folder` with `template`, refused if git still records a worktree there whose
/// folder is missing (git would refuse it too: prune it first).
fn free_destination(list: &[Worktree], at: &Path, template: &str, folder: &str) -> Result<PathBuf, Problem> {
    let path = destination(&list[0], at, template, folder)?;
    match list.iter().find(|w| Path::new(&w.path) == path) {
        Some(w) => Err(Problem::WorktreeRegistered { path: w.path.clone() }),
        None => Ok(path),
    }
}

/// The main checkout whose recipe applies: none for a bare repository or a missing folder.
fn recipe_dir(main: &Worktree) -> Option<PathBuf> {
    Some(PathBuf::from(&main.path)).filter(|p| !main.bare && p.is_dir())
}

/// What the new-worktree dialog needs: branches, the folder it would create, whether git ignores
/// it, the repository's recipe. Read only.
pub fn plan(dir: &Path, id: u64, key: &str, folder: &str, location: Option<&str>) -> WorktreePlan {
    let Some(list) = listed(dir) else { return refused_plan(id, key, folder, location, Problem::NotAGitRepo) };
    let mut plan = WorktreePlan { problem: None, ..refused_plan(id, key, folder, location, Problem::NotAGitRepo) };
    let main = &list[0];
    let at = main_dir(main, dir);
    plan.branches = local_branches(&at, &list);
    plan.current_branch = git_ok(dir, &["symbolic-ref", "--short", "-q", "HEAD"]).map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
    let main_checkout = main.branch.as_deref().zip(main.head.as_deref());
    plan.default_branch = worktrees::default_branch(&at, main_checkout).map(|b| b.name);
    plan.recipe = recipe_dir(main).and_then(|m| recipe::plan(&m));
    let template = location.unwrap_or(DEFAULT_WORKTREE_LOCATION);
    match free_destination(&list, &at, template, folder) {
        Ok(path) => {
            plan.exclude = exclude_pattern(main, &at, &path);
            plan.path = Some(path.to_string_lossy().into_owned());
        }
        Err(problem @ (Problem::DestinationExists { .. } | Problem::WorktreeRegistered { .. })) => {
            plan.free_folder = (2..=FREE_TRIES + 1).map(|n| numbered(folder, n)).find(|f| free_destination(&list, &at, template, f).is_ok());
            plan.problem = Some(problem);
        }
        Err(problem) => plan.problem = Some(problem),
    }
    plan
}

/// A plan with nothing in it but why.
pub fn refused_plan(id: u64, key: &str, folder: &str, location: Option<&str>, problem: Problem) -> WorktreePlan {
    WorktreePlan {
        id,
        key: key.to_string(),
        branches: Vec::new(),
        current_branch: None,
        default_branch: None,
        folder: folder.to_string(),
        location: location.map(String::from),
        path: None,
        problem: Some(problem),
        free_folder: None,
        exclude: None,
        recipe: None,
    }
}

/// `folder-n`, within `MAX_NAME_BYTES`.
pub fn numbered(folder: &str, n: u32) -> String {
    let suffix = format!("-{n}");
    let mut end = folder.len().min(MAX_NAME_BYTES.saturating_sub(suffix.len()));
    while !folder.is_char_boundary(end) {
        end -= 1;
    }
    folder_for_branch(&format!("{}{suffix}", folder[..end].trim_end_matches(['-', '.'])))
}

/// Local branches, sorted by name, with the worktree that has each checked out.
fn local_branches(at: &Path, list: &[Worktree]) -> Vec<LocalBranch> {
    let mut branches: Vec<LocalBranch> = worktrees::read_refs(at)
        .branches
        .into_iter()
        .map(|(name, ..)| {
            let checked_out = list.iter().find(|w| w.branch.as_deref() == Some(name.as_str())).map(|w| w.path.clone());
            LocalBranch { name, checked_out }
        })
        .collect();
    branches.sort_by(|a, b| a.name.cmp(&b.name));
    branches
}

/// A worktree created: its folder, whether a block went to `info/exclude`, what the repository's
/// recipe applies to (`None`: it has none), applied by the caller once the answer is sent.
#[derive(Debug)]
pub struct Done {
    pub path: String,
    pub excluded: bool,
    pub recipe: Option<PathBuf>,
}

pub type Created = Result<Done, Problem>;

/// Creates the worktree; says whether the repository has a recipe to apply to it (whatever the
/// recipe does then, the worktree stays). When git is still running after `write_limit()`, the answer is
/// `GitStillRunning` and `late` gets the final result when git ends (the writes then taken back
/// if it failed); otherwise `late` is never called.
pub fn run(dir: &Path, request: &CreateWorktree, late: impl FnOnce(Created) + Send + 'static) -> Created {
    let list = listed(dir).ok_or(Problem::NotAGitRepo)?;
    let main = &list[0];
    let at = main_dir(main, dir);
    let name = request.branch.name();
    if !branch_name_ok(&at, name) {
        return Err(Problem::InvalidBranchName { branch: name.to_string() });
    }
    let template = request.location.as_deref().unwrap_or(DEFAULT_WORKTREE_LOCATION);
    let path = free_destination(&list, &at, template, &request.folder)?;
    let commit = match &request.branch {
        WorktreeBranch::New { base, .. } => {
            if branch_exists(&at, name) {
                return Err(Problem::BranchExists { branch: name.to_string() });
            }
            Some(resolve_base(&at, base).ok_or_else(|| Problem::BaseNotFound { base: base.clone() })?)
        }
        WorktreeBranch::Existing { .. } => {
            if !branch_exists(&at, name) {
                return Err(Problem::BranchNotFound { branch: name.to_string() });
            }
            if let Some(w) = list.iter().find(|w| w.branch.as_deref() == Some(name)) {
                return Err(Problem::BranchCheckedOut { branch: name.to_string(), path: w.path.clone() });
            }
            None
        }
    };
    let text = path.to_string_lossy().into_owned();

    // Every check passed: the writes, each taken back if a later one fails.
    let appended = match exclude_pattern(main, &at, &path).filter(|_| request.exclude) {
        Some(pattern) => add_exclude(&at, &pattern)?,
        None => None,
    };
    let created = match create_parents(&path) {
        Ok(created) => created,
        Err(e) => {
            undo_exclude(appended);
            return Err(Problem::Unreadable { detail: e.to_string() });
        }
    };
    let take_back = move |appended| {
        remove_created(&created);
        undo_exclude(appended);
    };
    let written = match &commit {
        Some(commit) => git_write_within(&at, &["worktree", "add", "-b", name, "--", &text, commit], write_limit()),
        None => {
            // Checked again right before: with the branch gone, git would fall back on a remote
            // branch of that name (creating one that tracks it) or a tag (detached).
            if !branch_exists(&at, name) {
                take_back(appended);
                return Err(Problem::BranchNotFound { branch: name.to_string() });
            }
            git_write_within(&at, &["worktree", "add", "--", &text, name], write_limit())
        }
    };
    let excluded = appended.is_some();
    let recipe_main = recipe_dir(main);
    let finish = move |result: Result<(), Problem>, appended| match result {
        Ok(()) => {
            let recipe = recipe_main.filter(|m| recipe::present(m));
            Ok(Done { path: text, excluded, recipe })
        }
        Err(problem) => {
            take_back(appended);
            Err(problem)
        }
    };
    match written {
        Written::Done(result) => finish(result, appended),
        Written::Running(running) => {
            std::thread::spawn(move || late(finish(running.finish(), appended)));
            Err(Problem::GitStillRunning { secs: write_limit().as_secs() })
        }
    }
}

/// A branch name git accepts (`git check-ref-format --branch`, which also expands `@{-1}`: refused
/// before), that cannot pass for an option, and that is not `HEAD` or `@`.
pub fn branch_name_ok(at: &Path, name: &str) -> bool {
    if name.is_empty() || name.starts_with('-') || name.contains("@{") || name == "@" || name == "HEAD" {
        return false;
    }
    git_ok(at, &["check-ref-format", "--branch", name]).is_some_and(|out| out.trim_end_matches('\n') == name)
}

fn branch_exists(at: &Path, name: &str) -> bool {
    git_ok(at, &["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")]).is_some()
}

/// The commit a base names (`main`, `origin/main`, a sha...). Never an option.
fn resolve_base(at: &Path, base: &str) -> Option<String> {
    let base = base.trim();
    if base.is_empty() || base.starts_with('-') {
        return None;
    }
    git_ok(at, &["rev-parse", "--verify", "-q", &format!("{base}^{{commit}}")]).map(|s| s.trim().to_string()).filter(|s| s.len() >= 7 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The new worktree's folder: computed from the main checkout, then resolved on disk. The
/// deepest existing folder on the way (symlinks followed) must be inside the anchor (the main
/// checkout, or the folder holding it for a `../` template) and outside the git files; the
/// worktree's own folder must not exist. The result is the resolved path, ready for git.
pub fn destination(main: &Worktree, at: &Path, template: &str, folder: &str) -> Result<PathBuf, Problem> {
    let refuse = |issue| Problem::LocationRefused { location: template.to_string(), issue };
    if !valid_folder(folder) {
        return Err(Problem::InvalidFolder { folder: folder.to_string() });
    }
    let location = parse_location(template).map_err(refuse)?;
    if main.bare && !location.beside {
        return Err(refuse(LocationIssue::InsideBare));
    }
    let (anchor, path) = worktree_destination(&main.path, template, folder).map_err(refuse)?;
    let real_anchor = fs::canonicalize(&anchor).map_err(|_| refuse(LocationIssue::Escapes))?;
    let relative = Path::new(&path).strip_prefix(&anchor).map_err(|_| refuse(LocationIssue::Escapes))?.to_path_buf();
    let parts: Vec<&std::ffi::OsStr> = relative
        .components()
        .map(|c| match c {
            Component::Normal(part) => Ok(part),
            _ => Err(refuse(LocationIssue::ParentDir)),
        })
        .collect::<Result<_, _>>()?;
    let mut current = real_anchor.clone();
    for (i, part) in parts.iter().enumerate() {
        let next = current.join(part);
        if fs::symlink_metadata(&next).is_err() {
            // The rest does not exist yet: it will be created under `current`.
            let rest: PathBuf = parts[i..].iter().collect();
            let result = current.join(rest);
            return outside_git(at, &result).then_some(result).ok_or_else(|| refuse(LocationIssue::GitDir));
        }
        if i == parts.len() - 1 {
            return Err(Problem::DestinationExists { path });
        }
        let real = fs::canonicalize(&next).map_err(|_| refuse(LocationIssue::Escapes))?;
        if !path_inside(&real_anchor.to_string_lossy(), &real.to_string_lossy()) {
            return Err(refuse(LocationIssue::Escapes));
        }
        if !real.is_dir() {
            return Err(refuse(LocationIssue::NotAFolder));
        }
        current = real;
    }
    Err(Problem::DestinationExists { path })
}

/// `path` is not inside the repository's git files (`.git`, or the common folder of a linked
/// checkout). Unknown counts as inside.
fn outside_git(at: &Path, path: &Path) -> bool {
    let Some(common) = git_ok(at, &["rev-parse", "--git-common-dir"]) else { return false };
    let common = at.join(common.trim());
    let Ok(common) = fs::canonicalize(common) else { return false };
    !path_inside(&common.to_string_lossy(), &path.to_string_lossy())
}

/// When `path` (resolved) is inside the main checkout and git does not ignore it: the pattern to
/// add to `info/exclude`, anchored. The location's folder (`/.claude/worktrees/`) when it does
/// not exist yet or holds nothing git sees (no tracked file, no untracked one it shows), so the
/// next worktrees there are covered too; otherwise only the worktree's own folder
/// (`/src/feat-x/`): excluding a folder with files of its own would hide its future files. `None`
/// when git ignores it, when it is outside the main checkout, or when git could not tell.
pub fn exclude_pattern(main: &Worktree, at: &Path, path: &Path) -> Option<String> {
    if main.bare {
        return None;
    }
    let real_main = fs::canonicalize(&main.path).ok()?;
    let relative = path.strip_prefix(&real_main).ok()?;
    let text = relative.to_string_lossy().into_owned();
    if text.is_empty() {
        return None;
    }
    let status = Command::new("git").arg("-C").arg(at).args(["check-ignore", "-q", "--", &text]).env("GIT_OPTIONAL_LOCKS", "0").stdout(Stdio::null()).stderr(Stdio::null()).status().ok()?;
    // 0: ignored, 1: not ignored, anything else: git could not tell.
    if status.code() != Some(1) {
        return None;
    }
    let parent = relative.parent().filter(|p| !p.as_os_str().is_empty());
    let empty = |dir: &Path| {
        let spec = format!(":(literal){}", dir.to_string_lossy());
        git_ok(at, &["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", &spec]).is_some_and(|out| out.is_empty())
    };
    let folder = match parent {
        Some(dir) if empty(dir) => dir,
        _ => relative,
    };
    Some(format!("/{}/", escape_glob(&folder.to_string_lossy())))
}

/// A literal path in a gitignore pattern: glob characters and a leading `#` or `!` escaped.
pub fn escape_glob(path: &str) -> String {
    let mut out = String::new();
    for (i, c) in path.chars().enumerate() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') || (i == 0 && matches!(c, '#' | '!')) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The block appended to `info/exclude`, to take back.
#[derive(Debug, PartialEq, Eq)]
pub struct Appended {
    file: PathBuf,
    block: Vec<u8>,
}

/// The block that adds `pattern` to a file ending with `last` (its last byte, if any).
pub fn exclude_block(last: Option<u8>, pattern: &str) -> Vec<u8> {
    let mut block = Vec::new();
    if last.is_some_and(|b| b != b'\n') {
        block.push(b'\n');
    }
    block.extend_from_slice(format!("{EXCLUDE_HEADER}\n{pattern}\n").as_bytes());
    block
}

/// Whether a line of `bytes` (any encoding) is `pattern`.
fn has_line(bytes: &[u8], pattern: &str) -> bool {
    bytes.split(|b| *b == b'\n').any(|line| line.trim_ascii() == pattern.as_bytes())
}

/// Appends `pattern` to the repository's `info/exclude` (shared by all its worktrees), unless a
/// line already says it: the file is only appended to, never rewritten. `None`: nothing added.
fn add_exclude(at: &Path, pattern: &str) -> Result<Option<Appended>, Problem> {
    let rel = git_ok(at, &["rev-parse", "--git-path", "info/exclude"]).ok_or(Problem::NotAGitRepo)?;
    append_exclude(&at.join(rel.trim()), pattern).map_err(|e| Problem::Unreadable { detail: e.to_string() })
}

pub fn append_exclude(file: &Path, pattern: &str) -> std::io::Result<Option<Appended>> {
    let before = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    if has_line(&before, pattern) {
        return Ok(None);
    }
    let block = exclude_block(before.last().copied(), pattern);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::OpenOptions::new().append(true).create(true).open(file)?.write_all(&block)?;
    Ok(Some(Appended { file: file.to_path_buf(), block }))
}

/// Takes back an appended block: only if it is still the end of the file (something appended
/// after it meanwhile stays, and so does the block then).
pub fn undo_exclude(appended: Option<Appended>) {
    let Some(appended) = appended else { return };
    let Ok(bytes) = fs::read(&appended.file) else { return };
    if bytes.ends_with(&appended.block)
        && let Ok(file) = fs::OpenOptions::new().write(true).open(&appended.file)
    {
        let _ = file.set_len((bytes.len() - appended.block.len()) as u64);
    }
}

/// Creates the missing folders above `path`; returns them, deepest first.
fn create_parents(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut missing = Vec::new();
    let mut up = path.parent();
    while let Some(dir) = up {
        if fs::symlink_metadata(dir).is_ok() {
            break;
        }
        missing.push(dir.to_path_buf());
        up = dir.parent();
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(missing)
}

/// Removes the folders `create_parents` made, if still empty.
fn remove_created(created: &[PathBuf]) {
    for dir in created {
        let _ = fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_folder_names_stay_valid() {
        assert_eq!(numbered("invoices", 2), "invoices-2");
        let long = "a".repeat(MAX_NAME_BYTES);
        let n = numbered(&long, 12);
        assert!(n.ends_with("-12") && n.len() == MAX_NAME_BYTES && valid_folder(&n), "{n}");
        assert_eq!(numbered(&format!("{}-bc", "a".repeat(MAX_NAME_BYTES - 3)), 2), format!("{}-2", "a".repeat(MAX_NAME_BYTES - 3)), "cut right after a dash: no double dash");
        let accents = numbered(&"\u{e9}".repeat(127), 2);
        assert!(accents.ends_with("-2") && accents.len() <= MAX_NAME_BYTES && valid_folder(&accents), "cut on a character boundary");
    }

    #[test]
    fn exclude_patterns_are_literal() {
        assert_eq!(escape_glob(".claude/worktrees"), ".claude/worktrees");
        assert_eq!(escape_glob("a[1]*?"), "a\\[1\\]\\*\\?");
        assert_eq!(escape_glob("#x/!y"), "\\#x/!y");
        assert_eq!(escape_glob("!x"), "\\!x");
    }

    #[test]
    fn options_and_revisions_are_never_branch_names() {
        let here = std::env::temp_dir();
        for name in ["", "-x", "--force", "@{-1}", "a@{1}", "@", "HEAD"] {
            assert!(!branch_name_ok(&here, name), "{name:?}");
        }
    }

    #[test]
    fn created_parents_are_taken_back_when_empty() {
        let root = std::env::temp_dir().join(format!("tcanvas-parents-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("kept")).unwrap();
        let created = create_parents(&root.join("kept/a/b/wt")).unwrap();
        assert_eq!(created, vec![root.join("kept/a/b"), root.join("kept/a")]);
        assert!(root.join("kept/a/b").is_dir());
        remove_created(&created);
        assert!(!root.join("kept/a").exists() && root.join("kept").is_dir(), "only what it made");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_exclude_file_is_only_appended_to_and_taken_back_exactly() {
        let root = std::env::temp_dir().join(format!("tcanvas-exclude-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let file = root.join("info/exclude");
        // No file yet: created with the block, then emptied again.
        let added = append_exclude(&file, "/.claude/worktrees/").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), format!("{EXCLUDE_HEADER}\n/.claude/worktrees/\n"));
        undo_exclude(added);
        assert_eq!(fs::read(&file).unwrap(), b"");
        // Bytes that are not UTF-8, and no final newline: kept as they are.
        let mine: &[u8] = b"*.log\n# caf\xe9 \xff\nbuild";
        fs::write(&file, mine).unwrap();
        let added = append_exclude(&file, "/wt/").unwrap();
        let mut expected = mine.to_vec();
        expected.extend_from_slice(format!("\n{EXCLUDE_HEADER}\n/wt/\n").as_bytes());
        assert_eq!(fs::read(&file).unwrap(), expected);
        undo_exclude(added);
        assert_eq!(fs::read(&file).unwrap(), mine, "exactly as before, bytes included");
        // Someone appended after the block meanwhile: the file is left alone.
        let added = append_exclude(&file, "/wt/").unwrap();
        fs::OpenOptions::new().append(true).open(&file).unwrap().write_all(b"theirs\n").unwrap();
        let before_undo = fs::read(&file).unwrap();
        undo_exclude(added);
        assert_eq!(fs::read(&file).unwrap(), before_undo, "their line kept, and the block with it");
        // Already there: nothing added, nothing to take back.
        assert_eq!(append_exclude(&file, "/wt/").unwrap(), None);
        assert_eq!(fs::read(&file).unwrap(), before_undo);
        assert_eq!(exclude_block(None, "/x/"), format!("{EXCLUDE_HEADER}\n/x/\n").into_bytes());
        assert_eq!(exclude_block(Some(b'\n'), "/x/"), format!("{EXCLUDE_HEADER}\n/x/\n").into_bytes());
        fs::remove_dir_all(&root).unwrap();
    }
}
