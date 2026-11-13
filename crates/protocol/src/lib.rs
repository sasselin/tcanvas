//! Messages exchanged between the app (Windows) and `tcanvas-agent` (WSL).
//! Transport: one JSON object per line on the agent's stdin/stdout.

use serde::{Deserialize, Serialize};

pub mod task_command;

/// A branch name git accepts, by the rules of `git check-ref-format --branch` (the agent asks
/// git itself), never one that could pass for an option or a revision.
pub fn branch_name_valid(name: &str) -> bool {
    let bad_char = |c: char| c.is_control() || c.is_whitespace() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\');
    !name.is_empty()
        && !name.starts_with('-')
        && name != "@"
        && name != "HEAD"
        && !name.contains("@{")
        && !name.contains("..")
        && !name.contains("//")
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.ends_with('.')
        && !name.chars().any(bad_char)
        && name.split('/').all(|part| !part.starts_with('.') && !part.ends_with(".lock"))
}

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
    /// Worktree not created: git does not accept this branch name (`git check-ref-format
    /// --branch`), or it could pass for an option or a revision (`-x`, `@{-1}`, `HEAD`).
    InvalidBranchName {
        branch: String,
    },
    /// Worktree not created: a new branch was asked, but a local branch has this name.
    BranchExists {
        branch: String,
    },
    /// Worktree not created: no local branch has this name.
    BranchNotFound {
        branch: String,
    },
    /// Worktree not created: this branch is checked out in the worktree at `path`.
    BranchCheckedOut {
        branch: String,
        path: String,
    },
    /// Worktree not created: its folder already exists.
    DestinationExists {
        path: String,
    },
    /// Worktree not created: the base does not name a commit.
    BaseNotFound {
        base: String,
    },
    /// Worktree not created: the folder name is not one `folder_for_branch` would give.
    InvalidFolder {
        folder: String,
    },
    /// Worktree not created: the location template is refused, for this reason.
    LocationRefused {
        location: String,
        issue: LocationIssue,
    },
    /// Worktree not created: git still records a worktree at this path whose folder is missing
    /// (`git worktree prune`, from the cleanup window, forgets it).
    WorktreeRegistered {
        path: String,
    },
    /// A git command that writes was still running after `secs` seconds: the agent stopped
    /// waiting for it, but it goes on (stopping a checkout or a removal half-way would leave
    /// worse). For a worktree creation, the final answer follows when it ends.
    GitStillRunning {
        secs: u64,
    },
    /// The worktree recipe (`file`, at the root of the main checkout) is not valid TOML (the
    /// parser's message, not translated): none of it applies.
    RecipeMalformed {
        file: String,
        #[serde(default)]
        line: Option<u64>,
        detail: String,
    },
    /// The worktree recipe's `key` (`worktree.copy`...) does not have the expected type: none of
    /// the recipe applies.
    RecipeWrongType {
        file: String,
        key: String,
    },
    /// The worktree recipe is larger than `max_bytes`: not read.
    RecipeTooBig {
        file: String,
        max_bytes: u64,
    },
    /// The worktree recipe is not a regular file (a symbolic link, a folder, a FIFO, a device...):
    /// not read.
    RecipeNotAFile {
        file: String,
    },
    /// Task list: no live task has this id (never existed, removed, or purged).
    TaskNotFound {
        task: String,
    },
    /// Task list: `task` waiting for `on` would close a loop (references, not ids).
    TaskCycle {
        task: String,
        on: String,
    },
    /// Task list: the task (its reference) changed since the caller read it: the edit is refused
    /// rather than overwriting someone else's change.
    TaskStale {
        task: String,
    },
    /// Task list: a title or a note cannot be empty.
    TaskTextEmpty,
    /// Task list: the file was written by a newer agent (`format`): shown read-only, never
    /// rewritten.
    TasksNewerFormat {
        format: u32,
    },
    /// Task list: the file could not be read as a task list (the parser's message, not
    /// translated); it was renamed to `path`, kept as is, and the list starts empty.
    TasksSetAside {
        path: String,
        detail: String,
    },
    /// Task list: reading or writing failed (the system's message, not translated).
    TasksFailed {
        detail: String,
    },
    /// Task list: this key has no task list subscribed (`AgentCmd::WatchTasks`).
    TasksNotWatched,
    /// Task list: a text is longer than its limit (`TaskField`: which one; `max` in characters for
    /// a title, in bytes otherwise).
    TaskTooLong {
        field: TaskField,
        max: usize,
    },
    /// Task list: a task has as many notes as it may have.
    TaskTooManyNotes {
        max: usize,
    },
    /// Task list: the list has as many tasks as it may have (the trash included).
    TasksTooMany {
        max: usize,
    },
    /// Task list: `before` is not a task of the same status.
    TaskMoveRefused,
    /// Task list: the folder now belongs to another list than the one the app was shown (`shown`,
    /// `now`: list keys): nothing changed, the new list follows.
    TasksListChanged {
        shown: String,
        now: String,
    },
    /// Task list: which list the folder belongs to could not be told (git failed: its message, not
    /// translated). Nothing is read from or written to another list.
    TasksKeyUnknown {
        detail: String,
    },
    /// Task list: the file holds values this version does not know (a newer writer of the same
    /// format; the parser's message, not translated): shown read-only, never rewritten.
    TasksUnknownValues {
        detail: String,
    },
    /// Task list: two tasks of the file have the same id: shown read-only.
    TasksDuplicateId {
        id: String,
    },
    /// Task list: bringing another list here refused, this one is not empty (`tasks` live tasks)
    /// and no merge was asked.
    TasksNotEmpty {
        tasks: usize,
    },
    /// Task list: `from` is not (or no longer) a list that may be this folder's: nothing moved.
    TasksNotAdoptable {
        from: String,
    },
    /// Task list: `from` has no file, or no task, to bring here: nothing changed.
    TasksNothingToAdopt {
        from: String,
    },
    /// Task list: a list cannot be brought into itself.
    TasksSameList,
    /// Task list: another program held the list's lock past `secs` seconds: nothing changed.
    TasksLocked {
        secs: u64,
    },
    /// Task list: the status change named the status it expected (`TaskOp::SetStatus::from`),
    /// and the task (its reference) has another one now: nothing changed.
    TaskStatusChanged {
        task: String,
    },
    /// Task list: a link (worktree, branch, session) is empty, longer than `MAX_TASK_LINK` bytes,
    /// or holds control characters.
    TaskLinkRefused,
    /// Review: the base is not a name a branch could have (empty, starting with `-`, holding
    /// spaces or revision syntax such as `..` or `^`): never passed to git.
    ReviewBaseRefused {
        base: String,
    },
    /// Review: the base names no commit.
    ReviewBaseNotFound {
        base: String,
    },
    /// Review: no base was asked and the repository has no default branch to compare with.
    ReviewNoBase,
    /// Review: the worktree's branch has no commit yet.
    ReviewNoCommits,
    /// Not shown: the path is not a regular file (a symbolic link, a pipe, a device...), which is
    /// never read.
    NotARegularFile {
        path: String,
    },
    /// Review: git did not answer within `secs` seconds: it was stopped.
    ReviewTimedOut {
        secs: u64,
    },
}

/// Which text of a task a limit is about.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TaskField {
    Title,
    Description,
    Note,
}

/// The file, at the root of a repository's main checkout, that says what a new worktree needs
/// (`[worktree]`: `copy`, `symlink`, `setup`).
pub const RECIPE_FILE: &str = ".tcanvas.toml";
/// Results done as asked a recipe report lists one by one; the others are only counted.
pub const MAX_RECIPE_ITEMS: usize = 200;
/// Results not done a recipe report lists one by one (always listed before any done one is left
/// out); beyond, only counted.
pub const MAX_RECIPE_FAILURES: usize = 1000;

/// Why a recipe pattern is refused: nothing it names is touched.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum PatternIssue {
    Empty,
    /// Starts with `/`: patterns are relative to the main checkout.
    Absolute,
    /// Starts with `~`.
    Home,
    /// A `..` component.
    ParentDir,
    /// A `.git` component: never the repository's own files.
    GitDir,
    /// A character outside the supported syntax (`[`, `]`, `{`, `}`, `\`, control characters).
    BadCharacter,
    /// `**` mixed with other characters in one component.
    BadGlob,
    /// Longer than `MAX_PATTERN_BYTES`.
    TooLong,
}

/// Longest recipe pattern, in bytes.
pub const MAX_PATTERN_BYTES: usize = 1024;

/// What a recipe does with a path of the main checkout.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecipeAction {
    /// Copied into the new worktree (folders whole, permissions kept).
    Copy,
    /// A symbolic link in the new worktree, to the path in the main checkout.
    Symlink,
}

/// A recipe pattern as the agent sees it now, for the new-worktree dialog.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct RecipePattern {
    pub pattern: String,
    /// From the repository's Claude Code settings (`worktree.symlinkDirectories`).
    #[serde(default)]
    pub claude: bool,
    /// Paths of the main checkout it matches now.
    pub matches: usize,
    /// Refused: matches nothing, never applied.
    #[serde(default)]
    pub refused: Option<PatternIssue>,
    /// The search stopped at its bound: there may be more matches.
    #[serde(default)]
    pub incomplete: bool,
}

/// A repository's worktree recipe, as read from its main checkout (`RECIPE_FILE`, plus the
/// Claude Code settings' `worktree.symlinkDirectories`).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Recipe {
    /// The main checkout: acceptance of `setup` is remembered per repository.
    pub main: String,
    pub copy: Vec<RecipePattern>,
    pub symlink: Vec<RecipePattern>,
    /// The command to run in the new worktree's terminal, exactly as written. The agent never
    /// runs it.
    #[serde(default)]
    pub setup: Option<String>,
    /// The recipe file is unreadable or malformed: only the Claude Code settings apply.
    #[serde(default)]
    pub problem: Option<Problem>,
}

/// Why a recipe item was not applied.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind")]
pub enum SkipReason {
    /// The pattern matches nothing.
    NoMatch,
    Refused {
        issue: PatternIssue,
    },
    /// Something is already at that path in the new worktree (a tracked file): never replaced.
    Exists,
    /// A symbolic link whose target is outside the main checkout (or missing): not copied.
    LinkOutside,
    /// The way to it in the new worktree goes through a symbolic link or a file.
    Escapes,
    /// Another repository or worktree (it holds a `.git`): never copied.
    NestedRepository,
    /// Neither a file, a folder nor a symbolic link (a socket, a device...).
    Unsupported,
    /// The copy limits were reached (`max_bytes`, `max_entries`): the copies stopped there.
    CapReached {
        max_bytes: u64,
        max_entries: u64,
    },
    /// The search for matches stopped at its bound.
    SearchLimit,
    /// The system refused (its message, not translated).
    Failed {
        detail: String,
    },
    /// A link would hold the new worktree itself (the path is the worktree or one of its
    /// folders): never made.
    ContainsWorktree,
    /// A folder copied only in part: what was left out is listed before it.
    Partial,
    /// A folder whose entries could not be listed (its permissions...): the system's message.
    Unreadable {
        detail: String,
    },
    /// A name that is not valid UTF-8: left out.
    BadName,
    /// A relative symbolic link copied as is, whose target is missing in the new worktree.
    DanglingLink,
}

/// One result of a recipe applied to a new worktree.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct RecipeItem {
    pub action: RecipeAction,
    /// Relative to the worktree (the pattern itself for `NoMatch`, `Refused`, `SearchLimit`).
    pub path: String,
    /// `None`: done.
    #[serde(default)]
    pub skipped: Option<SkipReason>,
}

/// What the recipe did to a new worktree. Its failures never undo the worktree.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct RecipeReport {
    /// In order: every result not done (at most `MAX_RECIPE_FAILURES`), and the first
    /// `MAX_RECIPE_ITEMS` done.
    pub items: Vec<RecipeItem>,
    /// Results done that are not listed.
    #[serde(default)]
    pub omitted: usize,
    /// Results not done that are not listed.
    #[serde(default)]
    pub omitted_failures: usize,
    /// The recipe file is unreadable or malformed: only the Claude Code settings applied.
    #[serde(default)]
    pub problem: Option<Problem>,
}

impl RecipeReport {
    pub fn push(&mut self, item: RecipeItem) {
        let failed = item.skipped.is_some();
        let listed = self.items.iter().filter(|i| i.skipped.is_some() == failed).count();
        match (failed, listed < if failed { MAX_RECIPE_FAILURES } else { MAX_RECIPE_ITEMS }) {
            (_, true) => self.items.push(item),
            (true, false) => self.omitted_failures += 1,
            (false, false) => self.omitted += 1,
        }
    }
}

/// Why a worktree location template (or the folder it gives) is refused.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum LocationIssue {
    Empty,
    /// Longer than `MAX_LOCATION_CHARS`.
    TooLong,
    /// Starts with `/` or `~`: a location is relative to the repository.
    Absolute,
    /// `..` anywhere but as the single leading component, or a `.` component, or an empty one.
    ParentDir,
    /// `{name}` missing, more than once, or not in the last component.
    NoName,
    /// A `{...}` other than `{name}` and `{repo}`, or a lone brace.
    UnknownPlaceholder,
    /// A character outside letters, digits, `.`, `_`, `-` (glob characters, spaces, `\`...).
    BadCharacter,
    /// A `.git` component: never inside a repository's own files.
    GitDir,
    /// The repository is bare: a location inside it would sit in its git files.
    InsideBare,
    /// Resolved (symlinks followed), the folder is outside the repository (or, for a leading
    /// `../`, outside the folder holding it).
    Escapes,
    /// Something on the way is a file, not a folder.
    NotAFolder,
    /// A component Windows cannot hold (the folder may live on a Windows drive, `/mnt/c/...`):
    /// ending with `.`, or a reserved device name (`CON`, `NUL`, `COM1`, `LPT1`...).
    WindowsName,
}

/// Where new worktrees go when a group sets nothing: inside the main checkout, where coding
/// agents put theirs.
pub const DEFAULT_WORKTREE_LOCATION: &str = ".claude/worktrees/{name}";
/// Longest file name most file systems take, in bytes: the most a folder name (and any
/// component of a location) may have.
pub const MAX_NAME_BYTES: usize = 255;
/// Longest location template, in characters.
pub const MAX_LOCATION_CHARS: usize = 200;

/// A name Windows reserves for a device, whatever its extension (`con`, `NUL.txt`, `com1`).
pub fn windows_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// `s` cut to at most `max` bytes, on a character boundary.
fn cut_bytes(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A folder name for a branch: slashes, spaces and every character other than letters, digits,
/// `.`, `_` and `-` become `-`, runs of `-` are merged, no leading `.` or `-` (nor trailing `.`
/// or `-`), at most `MAX_NAME_BYTES` bytes, a `_` after a Windows device name, never empty
/// (`worktree`). Applying it twice changes nothing.
pub fn folder_for_branch(branch: &str) -> String {
    let mut out = String::new();
    for c in branch.chars() {
        let c = if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' };
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    let trim = |s: &str| s.trim_matches(|c| c == '.' || c == '-').to_string();
    let mut out = trim(&out);
    if out.len() > MAX_NAME_BYTES {
        out = trim(cut_bytes(&out, MAX_NAME_BYTES));
    }
    if windows_reserved(&out) {
        // `con` -> `con_`, `nul.txt` -> `nul_.txt` (the stem is at most 4 bytes: the `_` stays).
        let stem = out.find('.').unwrap_or(out.len());
        out = format!("{}_{}", &out[..stem], &out[stem..]);
        if out.len() > MAX_NAME_BYTES {
            out = trim(cut_bytes(&out, MAX_NAME_BYTES));
        }
    }
    // `.` inside is kept, but never `..` as a whole (trimmed above) nor a name git refuses.
    if out.is_empty() { "worktree".into() } else { out }
}

/// A folder name the agent accepts: one `folder_for_branch` gives for itself.
pub fn valid_folder(folder: &str) -> bool {
    !folder.is_empty() && folder_for_branch(folder) == folder
}

/// A parsed location template: a relative path whose last component holds `{name}` (the folder
/// name) and where `{repo}` stands for the main checkout's folder name. It is relative to the
/// main checkout, or to the folder holding it when it starts with `../` (`../{repo}-{name}`: next
/// to the repository). No other `..`, no `.`, no absolute path, no `.git` component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// Starts with `../`: relative to the folder holding the main checkout.
    pub beside: bool,
    /// The components after the optional `../`, placeholders not replaced yet.
    pub parts: Vec<String>,
}

pub fn parse_location(template: &str) -> Result<Location, LocationIssue> {
    let template = template.trim();
    if template.is_empty() {
        return Err(LocationIssue::Empty);
    }
    if template.chars().count() > MAX_LOCATION_CHARS {
        return Err(LocationIssue::TooLong);
    }
    if template.starts_with('/') || template.starts_with('~') {
        return Err(LocationIssue::Absolute);
    }
    let mut parts: Vec<&str> = template.trim_end_matches('/').split('/').collect();
    let beside = parts.first() == Some(&"..");
    if beside {
        parts.remove(0);
    }
    if parts.is_empty() || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return Err(LocationIssue::ParentDir);
    }
    for part in &parts {
        // Placeholders out, what is left must be plain.
        let bare = part.replace("{name}", "").replace("{repo}", "");
        if bare.contains(['{', '}']) {
            return Err(LocationIssue::UnknownPlaceholder);
        }
        if !bare.chars().all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-')) {
            return Err(LocationIssue::BadCharacter);
        }
        if part.eq_ignore_ascii_case(".git") {
            return Err(LocationIssue::GitDir);
        }
        if part.ends_with('.') || (!part.contains('{') && windows_reserved(part)) {
            return Err(LocationIssue::WindowsName);
        }
    }
    let names = parts.iter().map(|p| p.matches("{name}").count()).sum::<usize>();
    if names != 1 || !parts.last().is_some_and(|p| p.contains("{name}")) {
        return Err(LocationIssue::NoName);
    }
    Ok(Location { beside, parts: parts.into_iter().map(String::from).collect() })
}

impl Location {
    /// The worktree's path relative to its anchor (the main checkout, or the folder holding it
    /// when `beside`), placeholders replaced. `repo`: the main checkout's folder name.
    pub fn relative(&self, repo: &str, folder: &str) -> Result<String, LocationIssue> {
        if !valid_folder(folder) {
            return Err(LocationIssue::BadCharacter);
        }
        let parts: Vec<String> = self.parts.iter().map(|p| p.replace("{repo}", repo).replace("{name}", folder)).collect();
        // `{repo}` comes from the file system: it may still make a component that is not plain.
        if parts.iter().any(|p| p.is_empty() || p == "." || p == ".." || p.contains('/')) {
            return Err(LocationIssue::ParentDir);
        }
        if parts.iter().any(|p| p.eq_ignore_ascii_case(".git")) {
            return Err(LocationIssue::GitDir);
        }
        if parts.iter().any(|p| p.ends_with(['.', ' ']) || windows_reserved(p)) {
            return Err(LocationIssue::WindowsName);
        }
        if parts.iter().any(|p| p.len() > MAX_NAME_BYTES) {
            return Err(LocationIssue::TooLong);
        }
        Ok(parts.join("/"))
    }
}

/// Where a worktree goes, as text (no file system access: the agent then resolves it): `main`
/// is the main checkout's absolute path. `Ok((anchor, path))`: the folder it must stay in, and
/// the worktree's own folder.
pub fn worktree_destination(main: &str, template: &str, folder: &str) -> Result<(String, String), LocationIssue> {
    let location = parse_location(template)?;
    let main = main.trim_end_matches('/');
    let (holder, repo) = main.rsplit_once('/').ok_or(LocationIssue::Escapes)?;
    if repo.is_empty() {
        return Err(LocationIssue::Escapes);
    }
    let anchor = if location.beside { if holder.is_empty() { "/".to_string() } else { holder.to_string() } } else { main.to_string() };
    let relative = location.relative(repo, folder)?;
    let path = format!("{}/{relative}", anchor.trim_end_matches('/'));
    Ok((anchor, path))
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

/// The task integration for coding agents, separate from the status hooks and opt-in: a session
/// start hook that prints the project's open tasks (`tcanvas-agent task prime`), and the
/// `tcanvas-tasks` skill file for the agents that read skills.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskHooks {
    /// Every coding agent found has it (in whatever form the user left it).
    Installed,
    /// At least one coding agent found lacks it: `AgentCmd::SetTaskHooks` adds it.
    Missing,
    /// No coding agent found: nothing to offer.
    Unsupported,
}

/// The kind of the user's login shell.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
    Other,
}

impl ShellKind {
    /// From a shell path (`/usr/bin/zsh`, `-bash`...).
    pub fn from_path(path: &str) -> ShellKind {
        match path.rsplit('/').next().unwrap_or_default().trim_start_matches('-') {
            "bash" => ShellKind::Bash,
            "zsh" => ShellKind::Zsh,
            "fish" => ShellKind::Fish,
            _ => ShellKind::Other,
        }
    }

    /// It runs a card's task script (`export` lines, `$(...)` in double quotes).
    pub fn runs_task_script(self) -> bool {
        matches!(self, ShellKind::Bash | ShellKind::Zsh)
    }
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
    /// Marks a coding agent's hooks write in this pane at each submitted prompt, and at each end
    /// of a turn (the agent's reply over, not a question it asks meanwhile): a new value means a
    /// new prompt (a new turn end) since the last one seen. Opaque, never compared otherwise
    /// (`None`: none written yet, or an older agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_prompt_mark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_turn_mark: Option<String>,
    /// The commit checked out in the hook's folder when it wrote that prompt's (that turn end's)
    /// mark, recorded with the mark (`None`: not a repository, git too slow for the hook, or an
    /// older agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_prompt_head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_turn_head: Option<String>,
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

/// Which part of a branch under review a request is about.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind")]
pub enum ReviewScope {
    /// The whole branch: from its merge base with the base to its head (the commits the
    /// `ReviewSummary` named).
    Branch { merge_base: String, head: String },
    /// One commit of the branch, against its first parent.
    Commit { sha: String },
    /// What is not committed in the worktree, against its HEAD (never part of the branch's files).
    Uncommitted,
}

/// How a branch stands against its base.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReviewState {
    /// It has commits the base does not have: something to review.
    Ahead,
    /// Every commit of it is in the base (identical, or only behind): nothing to review.
    UpToDate,
    /// No common history with the base (no merge base).
    Unrelated,
}

/// Files changed in a review scope (paths relative to the worktree's top; `FileChange::mtime` is
/// 0), at most `files.len()` of them listed, in path order.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct ReviewFiles {
    pub files: Vec<FileChange>,
    /// Changed files not listed (beyond the bound).
    pub more: u32,
    /// Lines added and removed in every changed file, listed or not (binary files count none).
    pub added: u64,
    pub removed: u64,
}

impl ReviewFiles {
    /// Every changed file, listed or not.
    pub fn count(&self) -> u64 {
        self.files.len() as u64 + u64::from(self.more)
    }
}

/// What the base of a review names.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BaseKind {
    /// A local branch (`refs/heads/<base>`).
    #[default]
    Branch,
    /// A remote-tracking branch (`refs/remotes/<base>`).
    Remote,
    /// Anything else git resolves the name to (a tag, a commit).
    Other,
}

/// A worktree's branch against its base, like a merge request: answer to `AgentCmd::Review`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ReviewSummary {
    /// The worktree's folder, as git lists it.
    pub worktree: String,
    /// `None`: detached HEAD.
    pub branch: Option<String>,
    pub head: String,
    /// The base as asked, or the default branch (`origin/main`, `main`...).
    pub base: String,
    pub base_sha: String,
    /// `git merge-base <base> HEAD` (`None`: unrelated histories).
    pub merge_base: Option<String>,
    pub state: ReviewState,
    /// Commits of the branch the base does not have, and the reverse.
    pub ahead: u32,
    pub behind: u32,
    /// The branch's commits since the merge base, newest first, at most a bound.
    pub commits: Vec<CommitInfo>,
    /// Commits of the branch not listed.
    pub more_commits: u32,
    /// Files changed from the merge base to the head (empty unless `state` is `Ahead`).
    pub files: ReviewFiles,
    /// What is not committed in the worktree, reported apart from the branch's files.
    pub uncommitted: ReviewFiles,
    /// What `base` named: a local branch first, then a remote-tracking one, then anything else.
    #[serde(default)]
    pub base_kind: BaseKind,
    /// The histories cross (`git merge-base --all` gives several): the files may include changes
    /// the base already has.
    #[serde(default)]
    pub several_merge_bases: bool,
    /// The listed commits that are merges (more than one parent).
    #[serde(default)]
    pub merges: Vec<String>,
}

impl ReviewSummary {
    /// The base moved on since the branch started from it: the branch is behind.
    pub fn base_moved(&self) -> bool {
        self.behind > 0
    }

    pub fn dirty(&self) -> bool {
        self.uncommitted.count() > 0
    }
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
    /// Changes when what is not committed in it changes (a fingerprint of its status and of the
    /// listed files' times; `None`: not known).
    #[serde(default)]
    pub status_mark: Option<u64>,
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

/// The branch of a new worktree.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind")]
pub enum WorktreeBranch {
    /// `git worktree add -b <name> -- <path> <commit of base>`.
    New { name: String, base: String },
    /// `git worktree add -- <path> <name>`: a local branch no worktree has checked out.
    Existing { name: String },
}

impl WorktreeBranch {
    pub fn name(&self) -> &str {
        match self {
            WorktreeBranch::New { name, .. } | WorktreeBranch::Existing { name } => name,
        }
    }
}

/// A worktree to create for a watched folder's repository. The agent computes its path itself,
/// from the main checkout, `location` and `folder`, and checks everything again.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct CreateWorktree {
    pub branch: WorktreeBranch,
    /// The worktree's folder name (`folder_for_branch`).
    pub folder: String,
    /// Location template (`parse_location`); `None`: `DEFAULT_WORKTREE_LOCATION`.
    #[serde(default)]
    pub location: Option<String>,
    /// When git does not ignore the new folder inside the main checkout, add the pattern the
    /// plan named (`WorktreePlan::exclude`) to the repository's local `info/exclude` first.
    #[serde(default)]
    pub exclude: bool,
}

/// A local branch, and the worktree that has it checked out.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct LocalBranch {
    pub name: String,
    pub checked_out: Option<String>,
}

/// What the new-worktree dialog needs to know, answering `AgentCmd::PlanWorktree`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct WorktreePlan {
    pub id: u64,
    pub key: String,
    /// The repository's local branches, sorted, with where each is checked out.
    pub branches: Vec<LocalBranch>,
    /// The branch the watched folder has checked out.
    pub current_branch: Option<String>,
    /// As in `WorktreeList` (`origin/main`, `main`...).
    pub default_branch: Option<String>,
    /// The folder name and location asked about.
    pub folder: String,
    pub location: Option<String>,
    /// The folder the worktree would get (`None`: see `problem`).
    pub path: Option<String>,
    /// Why there is no such folder to create (refused location, folder already there...).
    pub problem: Option<Problem>,
    /// When the folder already exists: the first free one (`<folder>-2`, `-3`...).
    pub free_folder: Option<String>,
    /// The folder is inside the main checkout and git does not ignore it (the main checkout
    /// would list it as untracked): the pattern `CreateWorktree::exclude` would add to
    /// `info/exclude`.
    pub exclude: Option<String>,
    /// The repository's worktree recipe (`None`: it has none).
    #[serde(default)]
    pub recipe: Option<Recipe>,
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

// --- Tasks ---------------------------------------------------------------------------------------

/// Format of a task list file. A file with a higher one is shown read-only and never rewritten.
pub const TASKS_FORMAT: u32 = 1;

/// Unknown fields of a task, a note or a list, kept as they are when an older writer rewrites the
/// file.
pub type Extra = serde_json::Map<String, serde_json::Value>;

/// Longest task title, in characters.
pub const MAX_TASK_TITLE: usize = 200;
/// Longest task description, in bytes (32 KiB: a long plan, not a dump).
pub const MAX_TASK_DESCRIPTION: usize = 32 * 1024;
/// Longest note, in bytes.
pub const MAX_TASK_NOTE: usize = 8 * 1024;
/// Most notes on one task.
pub const MAX_TASK_NOTES: usize = 200;
/// Most live tasks in one list. With the text limits, a list stays well under a few tens of
/// megabytes even at worst, and a few hundred kilobytes in practice.
pub const MAX_TASKS: usize = 1000;
/// Most removed tasks kept in a list's trash: past that, the oldest are purged early.
pub const MAX_TRASH: usize = 1000;
/// Longest link of a task (a worktree path, a branch, a session name), in bytes.
pub const MAX_TASK_LINK: usize = 4096;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Inbox,
    Todo,
    Active,
    Review,
    Done,
    Canceled,
}

impl TaskStatus {
    pub const ALL: [TaskStatus; 6] = [TaskStatus::Inbox, TaskStatus::Todo, TaskStatus::Active, TaskStatus::Review, TaskStatus::Done, TaskStatus::Canceled];

    /// Done or canceled: no longer holds up the tasks waiting for it.
    pub fn finished(self) -> bool {
        matches!(self, TaskStatus::Done | TaskStatus::Canceled)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash, Default, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    #[default]
    None,
    Low,
    Medium,
    High,
    Urgent,
}

impl Priority {
    pub const ALL: [Priority; 5] = [Priority::None, Priority::Low, Priority::Medium, Priority::High, Priority::Urgent];
}

/// Who wrote a note.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    #[default]
    User,
    /// A coding agent.
    Agent,
}

/// A note on a task: notes are only ever appended.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct TaskNote {
    pub text: String,
    /// Unix seconds.
    pub at: u64,
    #[serde(default)]
    pub author: Author,
    #[serde(flatten)]
    pub extra: Extra,
}

impl TaskNote {
    pub fn new(text: &str, at: u64, author: Author) -> Self {
        TaskNote { text: text.into(), at, author, extra: Extra::new() }
    }
}

/// One task of a list. The order of `TaskList::tasks` is the manual order: within a status, the
/// tasks show in the order they have in the list.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Task {
    /// Stable, random: what dependencies and commands name.
    pub id: String,
    /// Short and readable (`k3f`), unique within its list.
    pub reference: String,
    pub title: String,
    /// Markdown.
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub priority: Priority,
    /// Ids of the tasks of the same list this one waits for.
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub notes: Vec<TaskNote>,
    /// Unix seconds.
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub updated: u64,
    /// Goes up by one on every change of the task (notes, moves, status... included).
    #[serde(default)]
    pub rev: u64,
    /// Goes up by one only when its title, description or priority change: what a caller names
    /// (`AgentCmd::EditTasks::expect`) to refuse an edit made on a stale copy of those fields.
    #[serde(default)]
    pub content_rev: u64,
    /// In the trash since then (Unix seconds): hidden, restorable, purged after a while.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<u64>,
    /// Where the task is being worked on: its worktree's folder, its branch, and the tmux session
    /// of the card started for it (`TaskOp::SetLinks`, from the app only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A task list, as stored in its file (and sent to the app).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct TaskList {
    /// `TASKS_FORMAT` of the writer.
    pub format: u32,
    /// What the list belongs to: the main checkout of a repository, or a folder outside git
    /// (resolved paths).
    pub key: String,
    /// Goes up by one on every write.
    #[serde(default)]
    pub revision: u64,
    /// For a repository: its first root commit, which survives moving or renaming it (how a list
    /// whose key folder is gone is found again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    #[serde(default)]
    pub tasks: Vec<Task>,
    /// References of purged tasks, never given again (the most recent ones, bounded).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired: Vec<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Task {
    /// The worktree a task in review offers to review: its own (`None` for another status, a
    /// removed task, or a task without a worktree).
    pub fn review_worktree(&self) -> Option<&str> {
        self.worktree.as_deref().filter(|_| self.status == TaskStatus::Review && self.removed.is_none())
    }
}

impl TaskList {
    pub fn empty(key: &str) -> Self {
        TaskList { format: TASKS_FORMAT, key: key.into(), revision: 0, repo_id: None, tasks: Vec::new(), retired: Vec::new(), extra: Extra::new() }
    }

    /// Tasks not in the trash.
    pub fn live(&self) -> usize {
        self.tasks.iter().filter(|t| t.removed.is_none()).count()
    }
}

/// Another list with tasks that may be this folder's: the list of its previous key, of the folder
/// itself (before it became part of a repository), or of the same repository whose folder is gone
/// (moved or renamed). `AgentCmd::AdoptTasks` brings its tasks here.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct FoundList {
    pub key: String,
    /// Live tasks in it.
    pub tasks: usize,
    #[serde(default)]
    pub why: FoundWhy,
}

/// Why another list may be this folder's.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum FoundWhy {
    /// The list of the folder's previous key (this session).
    #[default]
    PreviousKey,
    /// The folder's own list, from before it was part of a repository.
    OwnFolder,
    /// A list of a repository with the same history (first root commit) whose folder is gone: a
    /// moved or renamed repository, but maybe another clone. Always confirmed by the user.
    SameHistory,
}

/// The task list of a key the app subscribed to (`AgentCmd::WatchTasks`), sent on subscribing
/// and whenever it changes on disk, whoever wrote it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct TaskBoard {
    /// The app's key.
    pub key: String,
    /// `None`: no list (see `problem`).
    #[serde(default)]
    pub list: Option<TaskList>,
    /// The list can be shown, not changed (see `problem`).
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub problem: Option<Problem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub found: Vec<FoundList>,
}

/// A change to a task list: small and typed, never "replace the whole list". `task`, `on`,
/// `before`: task ids.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TaskOp {
    /// A new task, last of its status.
    Add {
        title: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        priority: Priority,
        #[serde(default)]
        status: TaskStatus,
    },
    /// Changes the fields given.
    Edit {
        task: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        priority: Option<Priority>,
    },
    /// Moves the task to another status, last of it. `from`: the status the caller saw; when
    /// given and the task has another one now, refused with `Problem::TaskStatusChanged` (an
    /// automatic change never overrides one made meanwhile).
    SetStatus {
        task: String,
        status: TaskStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<TaskStatus>,
    },
    /// Sets or clears where the task is worked on; a link not given is left as it is. Sent by the
    /// app when it starts a task; the command line has no way to send it.
    SetLinks {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree: Option<LinkChange>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<LinkChange>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<LinkChange>,
    },
    /// Moves the task just before `before` (a task of the same status), or last of its status.
    Move {
        task: String,
        #[serde(default)]
        before: Option<String>,
    },
    AddNote {
        task: String,
        text: String,
        #[serde(default)]
        author: Author,
    },
    /// `task` waits for `on` (refused if that closes a loop).
    AddDependency {
        task: String,
        on: String,
    },
    RemoveDependency {
        task: String,
        on: String,
    },
    /// Into the trash (restorable).
    Remove {
        task: String,
    },
    /// Out of the trash.
    Restore {
        task: String,
    },
}

impl TaskOp {
    /// The existing task it changes (`None` for `Add`).
    pub fn task(&self) -> Option<&str> {
        match self {
            TaskOp::Add { .. } => None,
            TaskOp::Edit { task, .. }
            | TaskOp::SetStatus { task, .. }
            | TaskOp::SetLinks { task, .. }
            | TaskOp::Move { task, .. }
            | TaskOp::AddNote { task, .. }
            | TaskOp::AddDependency { task, .. }
            | TaskOp::RemoveDependency { task, .. }
            | TaskOp::Remove { task }
            | TaskOp::Restore { task } => Some(task),
        }
    }
}

/// A change of one link of a task (`TaskOp::SetLinks`).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum LinkChange {
    Set(String),
    Clear,
}

/// `task` waiting for `on` would close a loop: `on` is `task`, or already waits for it, directly
/// or through other tasks (removed ones included: restoring them must not make a loop).
pub fn task_cycle(tasks: &[Task], task: &str, on: &str) -> bool {
    let mut stack = vec![on];
    let mut seen: Vec<&str> = Vec::new();
    while let Some(at) = stack.pop() {
        if at == task {
            return true;
        }
        if seen.contains(&at) {
            continue;
        }
        seen.push(at);
        if let Some(t) = tasks.iter().find(|t| t.id == at) {
            stack.extend(t.depends_on.iter().map(String::as_str));
        }
    }
    false
}

/// The live, unfinished tasks `task` waits for, in its own order: what holds it up.
pub fn blocked_by<'a>(task: &Task, tasks: &'a [Task]) -> Vec<&'a Task> {
    task.depends_on.iter().filter_map(|id| tasks.iter().find(|t| &t.id == id)).filter(|d| d.removed.is_none() && !d.status.finished()).collect()
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
        /// The agent keeps task lists (`AgentCmd::WatchTasks`, `EditTasks`).
        #[serde(default)]
        tasks: bool,
        /// The task integration's state (`None` from older agents: they cannot install it).
        #[serde(default)]
        task_hooks: Option<TaskHooks>,
        /// The agent takes `TaskOp::SetLinks` and `TaskOp::SetStatus::from` (missing from older
        /// agents: tasks cannot be started with them).
        #[serde(default)]
        task_links: bool,
        /// The user's login shell, the one the cards run (`None` from older agents).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell: Option<ShellKind>,
        /// The agent answers `AgentCmd::RefreshWorktrees` (missing from older agents).
        #[serde(default)]
        worktree_refresh: bool,
        /// The agent answers `AgentCmd::Review`, `ReviewFiles` and `ReviewDiff` (missing from
        /// older agents).
        #[serde(default)]
        review: bool,
    },
    /// A subscribed task list, on subscribing and whenever it changes.
    Tasks(Box<TaskBoard>),
    /// Answer to `AgentCmd::EditTasks` with its `id`: `problem` is `None` when it was done; `task`:
    /// the task changed (the new one for `TaskOp::Add`).
    TasksEdited {
        id: u64,
        key: String,
        #[serde(default)]
        task: Option<String>,
        #[serde(default)]
        problem: Option<Problem>,
        /// The task's `content_rev` after the change, when it is done.
        #[serde(default)]
        content_rev: Option<u64>,
    },
    /// Something the user should know about a subscribed list (`Problem::TasksSetAside`).
    TasksNotice {
        key: String,
        problem: Problem,
    },
    /// Hooks state after `AgentCmd::InstallHooks`.
    HooksChanged {
        hooks: Hooks,
    },
    /// The task integration's state after `AgentCmd::SetTaskHooks`; `problem`
    /// (`Problem::HooksFailed`): what failed, per agent, while the others were done.
    TaskHooksChanged {
        task_hooks: TaskHooks,
        #[serde(default)]
        problem: Option<Problem>,
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
    /// Answer to `AgentCmd::RefreshWorktrees`: a scan of `key` that started after the request is
    /// done, and its worktree list, if it changed, was sent before this.
    WorktreesRefreshed {
        key: String,
        seq: u64,
    },
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
    /// Answer to `AgentCmd::PlanWorktree`.
    WorktreePlan(Box<WorktreePlan>),
    /// Answer to `AgentCmd::CreateWorktree` with its `id`: `path` is the new worktree's folder,
    /// `problem` why nothing was created. `excluded`: the location was added to `info/exclude`.
    /// `recipe_pending`: the repository has a recipe; the agent applies it now, and
    /// `RecipeApplied` with the same `id` follows once its copies and links are done.
    WorktreeCreated {
        id: u64,
        key: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        problem: Option<Problem>,
        #[serde(default)]
        excluded: bool,
        #[serde(default)]
        recipe_pending: bool,
    },
    /// What the repository's recipe did to the worktree the creation `id` made, once done
    /// (follows `WorktreeCreated` with `recipe_pending`).
    RecipeApplied {
        id: u64,
        key: String,
        path: String,
        report: Box<RecipeReport>,
    },
    /// Answer to `AgentCmd::Review` with its `id`: the summary, or the problem preventing it.
    /// `bases`: the bases it may be compared with instead (the default branch first, then the
    /// local branches), listed whenever the worktree is valid.
    Review {
        id: u64,
        key: String,
        #[serde(default)]
        summary: Option<Box<ReviewSummary>>,
        #[serde(default)]
        bases: Vec<String>,
        #[serde(default)]
        problem: Option<Problem>,
    },
    /// Answer to `AgentCmd::ReviewFiles` with its `id`.
    ReviewFiles {
        id: u64,
        key: String,
        #[serde(default)]
        files: ReviewFiles,
        #[serde(default)]
        problem: Option<Problem>,
    },
    /// Answer to `AgentCmd::ReviewDiff` with its `id`: `text` is empty when `problem` explains
    /// why there is none; `truncated` like `Diff`'s.
    ReviewDiff {
        id: u64,
        key: String,
        text: String,
        truncated: bool,
        #[serde(default)]
        problem: Option<Problem>,
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
    /// Installs (`enabled`) or removes the task integration (`TaskHooks`); response:
    /// `AgentEvent::TaskHooksChanged`.
    SetTaskHooks {
        enabled: bool,
    },
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
    /// What creating a worktree for a watched folder's repository would give: its local
    /// branches, the folder `folder` would get with `location`, whether git ignores it, and the
    /// repository's recipe with what each pattern matches now.
    /// Read only. Response: `AgentEvent::WorktreePlan` with the same `id`.
    PlanWorktree {
        id: u64,
        key: String,
        folder: String,
        #[serde(default)]
        location: Option<String>,
    },
    /// Creates a worktree (never forced; checked again on fresh data first), applies the
    /// repository's recipe copies and links to it (never its `setup`), then scans the repository
    /// again. Response: `AgentEvent::WorktreeCreated` with the same `id`.
    CreateWorktree {
        id: u64,
        key: String,
        request: CreateWorktree,
    },
    /// Scans the watched folder `key` again as soon as possible, and says so when done
    /// (`AgentEvent::WorktreesRefreshed`), even when nothing changed.
    RefreshWorktrees {
        key: String,
        seq: u64,
    },
    /// Subscribes `key` to the task list of `path`: its repository's (keyed by the main
    /// checkout), or the folder's own outside git. Answered by `AgentEvent::Tasks`, then again
    /// whenever the list changes on disk. Read only: nothing is created until a change.
    WatchTasks {
        key: String,
        path: String,
    },
    UnwatchTasks {
        key: String,
    },
    /// Changes the task list `key` is subscribed to. `expect`: the task's `content_rev` the caller
    /// saw; when given and its title, description or priority changed since, refused with
    /// `Problem::TaskStale`. `list`: the key of the list the caller was shown; when given and the
    /// folder now belongs to another list, refused with `Problem::TasksListChanged`. Response:
    /// `AgentEvent::TasksEdited` with the same `id`, then `AgentEvent::Tasks`.
    EditTasks {
        id: u64,
        key: String,
        op: TaskOp,
        #[serde(default)]
        expect: Option<u64>,
        #[serde(default)]
        list: Option<String>,
    },
    /// Brings the tasks of the list `from` (one of `TaskBoard::found`) into the list `key` is
    /// subscribed to; refused with `Problem::TasksNotEmpty` when that one has live tasks, unless
    /// `merge`. The old file is kept, renamed `.adopted-<unix time>`. Response:
    /// `AgentEvent::TasksEdited` with the same `id`.
    /// `list`: the key of the list the caller was shown, checked like `EditTasks::list`. `from`
    /// must still be one of the lists the agent would name (checked again under the locks).
    AdoptTasks {
        id: u64,
        key: String,
        from: String,
        #[serde(default)]
        merge: bool,
        #[serde(default)]
        list: Option<String>,
    },
    /// Reviews the branch of `worktree`, one of the worktrees of the watched folder `key`'s
    /// repository (any other path is refused), against `base` (`None`: the default branch).
    /// Read only. Response: `AgentEvent::Review` with the same `id`.
    Review {
        id: u64,
        key: String,
        worktree: String,
        #[serde(default)]
        base: Option<String>,
    },
    /// The files changed in `scope` of that worktree (one commit's, mostly: the branch's come
    /// with the summary). Response: `AgentEvent::ReviewFiles` with the same `id`.
    ReviewFiles {
        id: u64,
        key: String,
        worktree: String,
        scope: ReviewScope,
    },
    /// The diff of `path` (relative to the worktree's top; `old_path`: where a renamed file came
    /// from) in `scope`, or of the whole scope without `path`. Response: `AgentEvent::ReviewDiff`
    /// with the same `id`.
    ReviewDiff {
        id: u64,
        key: String,
        worktree: String,
        scope: ReviewScope,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        old_path: Option<String>,
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
            agent_prompt_mark: None,
            agent_turn_mark: None,
            agent_prompt_head: None,
            agent_turn_head: None,
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
            agent_prompt_mark: None,
            agent_turn_mark: None,
            agent_prompt_head: None,
            agent_turn_head: None,
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
            agent_prompt_mark: None,
            agent_turn_mark: None,
            agent_prompt_head: None,
            agent_turn_head: None,
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
            status_mark: None,
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
            (Worktree { reflog_commits: true, status_mark: None, ..safe.clone() }, KeepReason::ReflogCommits),
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
    fn branch_names_become_safe_folder_names() {
        let cases = [
            ("invoices", "invoices"),
            ("feat/invoices", "feat-invoices"),
            ("feat//a b\\c:d*e?f", "feat-a-b-c-d-e-f"),
            ("-x", "x"),
            ("--force", "force"),
            (".hidden", "hidden"),
            ("..", "worktree"),
            ("a..b", "a..b"),
            ("fix/typo.", "fix-typo"),
            ("release/1.2.x", "release-1.2.x"),
            ("caf\u{e9}/cr\u{e8}me", "caf\u{e9}-cr\u{e8}me"),
            ("x.lock", "x.lock"),
            ("@{-1}", "1"),
            ("", "worktree"),
            ("///", "worktree"),
            ("a\nb\tc", "a-b-c"),
            // Windows device names, whatever the case or extension.
            ("con", "con_"),
            ("NUL.txt", "NUL_.txt"),
            ("fix/com1", "fix-com1"),
            ("lpt9", "lpt9_"),
            ("com0", "com0"),
        ];
        for (branch, folder) in cases {
            assert_eq!(folder_for_branch(branch), folder, "{branch:?}");
            assert_eq!(folder_for_branch(folder), folder, "{branch:?}: applying it again changes nothing");
            assert!(valid_folder(folder), "{folder:?}");
        }
        let long = folder_for_branch(&format!("feat/{}", "a".repeat(400)));
        assert_eq!(long.len(), MAX_NAME_BYTES);
        // Capped by bytes, cut on a character boundary: 2-byte letters.
        let accents = folder_for_branch(&"\u{e9}".repeat(200));
        assert!(accents.len() <= MAX_NAME_BYTES && accents.len() == 254 && valid_folder(&accents), "{}", accents.len());
        // Cut right after a separator: no trailing dash.
        let cut = folder_for_branch(&format!("{}/b", "a".repeat(MAX_NAME_BYTES - 1)));
        assert_eq!(cut, "a".repeat(MAX_NAME_BYTES - 1));
        for bad in ["", "a/b", "-x", ".x", "x-", "a b", "..", "a--b", "con", "Aux.log", &"a".repeat(MAX_NAME_BYTES + 1)] {
            assert!(!valid_folder(bad), "{bad:?}");
        }
        assert!(windows_reserved("COM9") && windows_reserved("prn.x") && !windows_reserved("console") && !windows_reserved("com10"));
    }

    #[test]
    fn location_templates_stay_in_the_repository_or_next_to_it() {
        assert_eq!(parse_location(DEFAULT_WORKTREE_LOCATION), Ok(Location { beside: false, parts: vec![".claude".into(), "worktrees".into(), "{name}".into()] }));
        assert_eq!(parse_location("../{repo}-{name}"), Ok(Location { beside: true, parts: vec!["{repo}-{name}".into()] }));
        assert_eq!(parse_location(" wt/{name}/ ").map(|l| l.parts.len()), Ok(2), "trimmed, trailing slash ignored");
        let refused = [
            ("", LocationIssue::Empty),
            ("/tmp/{name}", LocationIssue::Absolute),
            ("~/wt/{name}", LocationIssue::Absolute),
            ("../../{name}", LocationIssue::ParentDir),
            ("wt/../../{name}", LocationIssue::ParentDir),
            ("..", LocationIssue::ParentDir),
            ("./{name}", LocationIssue::ParentDir),
            ("wt//{name}", LocationIssue::ParentDir),
            ("wt", LocationIssue::NoName),
            ("{name}/src", LocationIssue::NoName),
            ("{name}/{name}", LocationIssue::NoName),
            ("wt/{branch}", LocationIssue::UnknownPlaceholder),
            ("wt/{name", LocationIssue::UnknownPlaceholder),
            ("wt/*/{name}", LocationIssue::BadCharacter),
            ("wt dir/{name}", LocationIssue::BadCharacter),
            ("wt\\x/{name}", LocationIssue::BadCharacter),
            (".git/{name}", LocationIssue::GitDir),
            ("x/.GIT/{name}", LocationIssue::GitDir),
            ("wt./{name}", LocationIssue::WindowsName),
            ("{name}.", LocationIssue::WindowsName),
            ("con/{name}", LocationIssue::WindowsName),
            ("x/NUL.d/{name}", LocationIssue::WindowsName),
        ];
        for (template, issue) in refused {
            assert_eq!(parse_location(template), Err(issue), "{template:?}");
        }
        assert_eq!(parse_location(&format!("{}/{{name}}", "a".repeat(MAX_LOCATION_CHARS))), Err(LocationIssue::TooLong));

        let dest = |template: &str, folder: &str| worktree_destination("/home/me/shop", template, folder);
        assert_eq!(dest(DEFAULT_WORKTREE_LOCATION, "invoices"), Ok(("/home/me/shop".into(), "/home/me/shop/.claude/worktrees/invoices".into())));
        assert_eq!(dest("../{repo}-{name}", "invoices"), Ok(("/home/me".into(), "/home/me/shop-invoices".into())));
        assert_eq!(dest("../{repo}-worktrees/{name}", "a.b"), Ok(("/home/me".into(), "/home/me/shop-worktrees/a.b".into())));
        // The folder name comes from the app: checked again.
        assert_eq!(dest(DEFAULT_WORKTREE_LOCATION, "../../etc"), Err(LocationIssue::BadCharacter));
        assert_eq!(dest(DEFAULT_WORKTREE_LOCATION, ".."), Err(LocationIssue::BadCharacter));
        assert_eq!(dest(DEFAULT_WORKTREE_LOCATION, ""), Err(LocationIssue::BadCharacter));
        assert_eq!(dest("{name}", ".git"), Err(LocationIssue::BadCharacter), "a leading dot is never a folder name");
        // A repository at the root of the file system: next to it is `/`.
        assert_eq!(worktree_destination("/shop", "../{name}", "x"), Ok(("/".into(), "/x".into())));
        assert_eq!(worktree_destination("/", "{name}", "x"), Err(LocationIssue::Escapes));
        // A repository folder whose name makes a component that is not plain.
        assert_eq!(worktree_destination("/home/me/..", "{repo}/{name}", "x"), Err(LocationIssue::ParentDir));
        assert_eq!(worktree_destination("/mnt/c/con", "../{repo}/{name}", "x"), Err(LocationIssue::WindowsName), "a device name from the repository's folder");
        assert_eq!(worktree_destination("/mnt/c/shop.", "../wt/{repo}", "x").err(), Some(LocationIssue::NoName));
        assert_eq!(worktree_destination("/mnt/c/shop.", "../{repo}/{name}", "x"), Err(LocationIssue::WindowsName), "a component ending with a dot");
        // Every component within the file name limit, placeholders replaced.
        let long = "a".repeat(MAX_NAME_BYTES);
        assert_eq!(worktree_destination("/home/me/shop", "../{repo}-{name}", &long), Err(LocationIssue::TooLong));
        assert!(worktree_destination("/home/me/shop", DEFAULT_WORKTREE_LOCATION, &long).is_ok());
    }

    #[test]
    fn roundtrip_worktree_creation() {
        let requests = [
            CreateWorktree { branch: WorktreeBranch::New { name: "feat/invoices".into(), base: "origin/main".into() }, folder: "feat-invoices".into(), location: None, exclude: false },
            CreateWorktree { branch: WorktreeBranch::Existing { name: "fix/typo".into() }, folder: "fix-typo".into(), location: Some("../{repo}-{name}".into()), exclude: true },
        ];
        for request in requests {
            assert!(request.branch.name().contains('/'));
            let cmd = AgentCmd::CreateWorktree { id: 3, key: "p".into(), request: request.clone() };
            assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
            let ok = AgentEvent::WorktreeCreated { id: 3, key: "p".into(), path: Some("/home/me/shop/.claude/worktrees/x".into()), problem: None, excluded: true, recipe_pending: false };
            assert_eq!(decode::<AgentEvent>(&encode(&ok)).unwrap(), ok);
        }
        let cmd = AgentCmd::PlanWorktree { id: 4, key: "p".into(), folder: "invoices".into(), location: None };
        assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
        let plan = AgentEvent::WorktreePlan(Box::new(WorktreePlan {
            id: 4,
            key: "p".into(),
            branches: vec![LocalBranch { name: "main".into(), checked_out: Some("/home/me/shop".into()) }, LocalBranch { name: "wip".into(), checked_out: None }],
            current_branch: Some("main".into()),
            default_branch: Some("origin/main".into()),
            folder: "invoices".into(),
            location: Some("../{repo}-{name}".into()),
            path: None,
            problem: Some(Problem::DestinationExists { path: "/home/me/shop-invoices".into() }),
            free_folder: Some("invoices-2".into()),
            exclude: Some("/.claude/worktrees/".into()),
            recipe: None,
        }));
        assert_eq!(decode::<AgentEvent>(&encode(&plan)).unwrap(), plan);
        let issues = [
            LocationIssue::Empty,
            LocationIssue::TooLong,
            LocationIssue::Absolute,
            LocationIssue::ParentDir,
            LocationIssue::NoName,
            LocationIssue::UnknownPlaceholder,
            LocationIssue::BadCharacter,
            LocationIssue::GitDir,
            LocationIssue::InsideBare,
            LocationIssue::Escapes,
            LocationIssue::NotAFolder,
            LocationIssue::WindowsName,
        ];
        let mut problems: Vec<Problem> = issues.into_iter().map(|issue| Problem::LocationRefused { location: "x".into(), issue }).collect();
        problems.extend([
            Problem::InvalidBranchName { branch: "-x".into() },
            Problem::BranchExists { branch: "a".into() },
            Problem::BranchNotFound { branch: "a".into() },
            Problem::BranchCheckedOut { branch: "a".into(), path: "/w".into() },
            Problem::DestinationExists { path: "/w".into() },
            Problem::BaseNotFound { base: "nope".into() },
            Problem::InvalidFolder { folder: "../x".into() },
            Problem::WorktreeRegistered { path: "/w".into() },
            Problem::GitStillRunning { secs: 60 },
        ]);
        for problem in problems {
            let ev = AgentEvent::WorktreeCreated { id: 1, key: "p".into(), path: None, problem: Some(problem), excluded: false, recipe_pending: false };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        }
        // Without the optional fields.
        let old = r#"{"type":"CreateWorktree","id":1,"key":"p","request":{"branch":{"kind":"Existing","name":"a"},"folder":"a"}}"#;
        assert!(matches!(decode::<AgentCmd>(old).unwrap(), AgentCmd::CreateWorktree { request: CreateWorktree { location: None, exclude: false, .. }, .. }));
    }

    #[test]
    fn roundtrip_recipes() {
        let pattern = |p: &str, refused| RecipePattern { pattern: p.into(), claude: false, matches: 2, refused, incomplete: false };
        let recipe = Recipe {
            main: "/home/me/shop".into(),
            copy: vec![pattern(".env*", None), pattern("../x", Some(PatternIssue::ParentDir))],
            symlink: vec![RecipePattern { claude: true, incomplete: true, ..pattern("node_modules", None) }],
            setup: Some("npm ci\nnpm run seed -- --name \"it's\"".into()),
            problem: Some(Problem::RecipeMalformed { file: RECIPE_FILE.into(), line: Some(3), detail: "expected `]`".into() }),
        };
        let plan = WorktreePlan {
            id: 1,
            key: "p".into(),
            branches: vec![],
            current_branch: None,
            default_branch: None,
            folder: "x".into(),
            location: None,
            path: Some("/home/me/shop/.claude/worktrees/x".into()),
            problem: None,
            free_folder: None,
            exclude: None,
            recipe: Some(recipe),
        };
        let ev = AgentEvent::WorktreePlan(Box::new(plan));
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let issues =
            [PatternIssue::Empty, PatternIssue::Absolute, PatternIssue::Home, PatternIssue::ParentDir, PatternIssue::GitDir, PatternIssue::BadCharacter, PatternIssue::BadGlob, PatternIssue::TooLong];
        let mut reasons: Vec<Option<SkipReason>> = issues.into_iter().map(|issue| Some(SkipReason::Refused { issue })).collect();
        reasons.extend(
            [
                SkipReason::NoMatch,
                SkipReason::Exists,
                SkipReason::LinkOutside,
                SkipReason::Escapes,
                SkipReason::NestedRepository,
                SkipReason::Unsupported,
                SkipReason::CapReached { max_bytes: 4 << 30, max_entries: 200_000 },
                SkipReason::SearchLimit,
                SkipReason::Failed { detail: "Permission denied".into() },
                SkipReason::ContainsWorktree,
                SkipReason::Partial,
                SkipReason::Unreadable { detail: "Permission denied".into() },
                SkipReason::BadName,
                SkipReason::DanglingLink,
            ]
            .map(Some),
        );
        reasons.push(None);
        let mut report = RecipeReport { problem: Some(Problem::RecipeWrongType { file: RECIPE_FILE.into(), key: "worktree.copy".into() }), ..RecipeReport::default() };
        for (i, skipped) in reasons.into_iter().enumerate() {
            let action = if i % 2 == 0 { RecipeAction::Copy } else { RecipeAction::Symlink };
            report.push(RecipeItem { action, path: format!("p{i}"), skipped });
        }
        let ev = AgentEvent::WorktreeCreated { id: 2, key: "p".into(), path: Some("/w".into()), problem: None, excluded: false, recipe_pending: true };
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let ev = AgentEvent::RecipeApplied { id: 2, key: "p".into(), path: "/w".into(), report: Box::new(report) };
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        for problem in [Problem::RecipeTooBig { file: RECIPE_FILE.into(), max_bytes: 65536 }, Problem::RecipeNotAFile { file: RECIPE_FILE.into() }] {
            let ev = AgentEvent::Error { problem };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        }
        // Past the listed results, only counted.
        let mut big = RecipeReport::default();
        for i in 0..MAX_RECIPE_ITEMS + 5 {
            big.push(RecipeItem { action: RecipeAction::Copy, path: i.to_string(), skipped: None });
        }
        assert_eq!((big.items.len(), big.omitted), (MAX_RECIPE_ITEMS, 5));
        // Failures are listed even once the done ones are only counted.
        big.push(RecipeItem { action: RecipeAction::Symlink, path: "late".into(), skipped: Some(SkipReason::Exists) });
        assert_eq!((big.items.last().map(|i| i.path.as_str()), big.omitted_failures), (Some("late"), 0));
        for i in 0..MAX_RECIPE_FAILURES + 2 {
            big.push(RecipeItem { action: RecipeAction::Copy, path: i.to_string(), skipped: Some(SkipReason::NoMatch) });
        }
        assert_eq!((big.items.len(), big.omitted_failures), (MAX_RECIPE_ITEMS + MAX_RECIPE_FAILURES, 3));
        // An older agent: no recipe in its plan nor its answer.
        let old = r#"{"type":"WorktreeCreated","id":1,"key":"p","path":"/w","problem":null,"excluded":false}"#;
        assert!(matches!(decode::<AgentEvent>(old).unwrap(), AgentEvent::WorktreeCreated { recipe_pending: false, .. }));
        let old = r#"{"type":"WorktreePlan","id":1,"key":"p","branches":[],"current_branch":null,"default_branch":null,"folder":"x","location":null,"path":null,"problem":null,"free_folder":null,"exclude":null}"#;
        assert!(matches!(decode::<AgentEvent>(old).unwrap(), AgentEvent::WorktreePlan(p) if p.recipe.is_none()));
    }

    #[test]
    fn roundtrip_hooks() {
        for hooks in [Hooks::Installed, Hooks::Missing, Hooks::Unsupported] {
            let ev = AgentEvent::Hello {
                version: "1".into(),
                hooks,
                send_text: true,
                tasks: true,
                task_hooks: Some(TaskHooks::Missing),
                task_links: true,
                worktree_refresh: true,
                review: true,
                shell: None,
            };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
            let ev = AgentEvent::HooksChanged { hooks };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        }
        let ev = AgentEvent::Error { problem: Problem::HooksFailed { detail: "read-only".into() } };
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        for task_hooks in [TaskHooks::Installed, TaskHooks::Missing, TaskHooks::Unsupported] {
            let ev = AgentEvent::TaskHooksChanged { task_hooks, problem: Some(Problem::HooksFailed { detail: "Codex: bad JSON".into() }) };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        }
        for enabled in [true, false] {
            assert_eq!(decode::<AgentCmd>(&encode(&AgentCmd::SetTaskHooks { enabled })).unwrap(), AgentCmd::SetTaskHooks { enabled });
        }
        // An older agent's hello: no task integration to offer.
        let old = r#"{"type":"Hello","version":"0.1","hooks":"Installed","send_text":true,"tasks":true}"#;
        assert!(matches!(decode::<AgentEvent>(old).unwrap(), AgentEvent::Hello { task_hooks: None, .. }));
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

    fn task(id: &str, depends_on: &[&str]) -> Task {
        Task {
            id: id.into(),
            reference: format!("r{id}"),
            title: format!("task {id}"),
            description: String::new(),
            status: TaskStatus::Todo,
            priority: Priority::None,
            depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
            notes: vec![],
            created: 1,
            updated: 1,
            rev: 1,
            content_rev: 1,
            removed: None,
            worktree: None,
            branch: None,
            session: None,
            extra: Extra::new(),
        }
    }

    #[test]
    fn task_links_and_guarded_statuses_on_the_wire() {
        // Only the links given travel; a status change without `from` reads as before.
        let op = TaskOp::SetLinks { task: "a1".into(), worktree: Some(LinkChange::Set("/w".into())), branch: None, session: Some(LinkChange::Clear) };
        assert_eq!(serde_json::to_string(&op).unwrap(), r#"{"op":"set_links","task":"a1","worktree":{"set":"/w"},"session":"clear"}"#);
        let old = r#"{"op":"set_status","task":"a1","status":"todo"}"#;
        assert_eq!(decode::<TaskOp>(old).unwrap(), TaskOp::SetStatus { task: "a1".into(), status: TaskStatus::Todo, from: None });
        assert_eq!(serde_json::to_string(&TaskOp::SetStatus { task: "a1".into(), status: TaskStatus::Todo, from: None }).unwrap(), old);
        assert_eq!(op.task(), Some("a1"));
        // An older agent's hello: it cannot start tasks.
        let hello = r#"{"type":"Hello","version":"0.1","hooks":"Installed"}"#;
        assert!(matches!(decode::<AgentEvent>(hello).unwrap(), AgentEvent::Hello { task_links: false, .. }));
        for p in [Problem::TaskStatusChanged { task: "k3f".into() }, Problem::TaskLinkRefused] {
            let ev = AgentEvent::TasksEdited { id: 1, key: "p".into(), task: None, problem: Some(p), content_rev: None };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        }
    }

    #[test]
    fn roundtrip_tasks() {
        let mut full = task("a1", &["b2"]);
        full.description = "## Steps\n- one \"quoted\"\n- caf\u{e9}".into();
        full.status = TaskStatus::Review;
        full.priority = Priority::Urgent;
        full.notes = vec![TaskNote::new("started", 1_700_000_000, Author::Agent)];
        full.removed = Some(1_700_000_100);
        full.worktree = Some("/home/me/shop/.claude/worktrees/taxes".into());
        full.branch = Some("taxes".into());
        full.session = Some("tc_ab12".into());
        let list = TaskList { revision: 7, repo_id: Some("a".repeat(40)), tasks: vec![full, task("b2", &[])], retired: vec!["old".into()], ..TaskList::empty("/home/me/shop") };
        let found = vec![FoundList { key: "/home/me/old-shop".into(), tasks: 4, why: FoundWhy::SameHistory }];
        let ev = AgentEvent::Tasks(Box::new(TaskBoard { key: "p1".into(), list: Some(list), read_only: false, problem: None, found }));
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let ev = AgentEvent::Tasks(Box::new(TaskBoard { key: "p1".into(), list: None, read_only: true, problem: Some(Problem::TasksNewerFormat { format: 9 }), found: vec![] }));
        assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        let ops = [
            TaskOp::Add { title: "Invoice PDF".into(), description: String::new(), priority: Priority::High, status: TaskStatus::Inbox },
            TaskOp::Edit { task: "a1".into(), title: Some("x".into()), description: None, priority: Some(Priority::Low) },
            TaskOp::SetStatus { task: "a1".into(), status: TaskStatus::Done, from: None },
            TaskOp::Move { task: "a1".into(), before: Some("b2".into()) },
            TaskOp::Move { task: "a1".into(), before: None },
            TaskOp::AddNote { task: "a1".into(), text: "n".into(), author: Author::User },
            TaskOp::AddDependency { task: "a1".into(), on: "b2".into() },
            TaskOp::RemoveDependency { task: "a1".into(), on: "b2".into() },
            TaskOp::Remove { task: "a1".into() },
            TaskOp::Restore { task: "a1".into() },
            TaskOp::SetStatus { task: "a1".into(), status: TaskStatus::Review, from: Some(TaskStatus::Active) },
            TaskOp::SetLinks { task: "a1".into(), worktree: Some(LinkChange::Set("/home/me/it's here".into())), branch: Some(LinkChange::Clear), session: None },
        ];
        for op in ops {
            let cmd = AgentCmd::EditTasks { id: 3, key: "p1".into(), op, expect: Some(4), list: Some("/home/me/shop".into()) };
            assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
        }
        for cmd in [
            AgentCmd::WatchTasks { key: "p1".into(), path: "~/shop".into() },
            AgentCmd::UnwatchTasks { key: "p1".into() },
            AgentCmd::AdoptTasks { id: 4, key: "p1".into(), from: "/home/me/old-shop".into(), merge: true, list: Some("/home/me/shop".into()) },
        ] {
            assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
        }
        let problems = [
            Problem::TaskNotFound { task: "a1".into() },
            Problem::TaskCycle { task: "k3f".into(), on: "x9z".into() },
            Problem::TaskStale { task: "k3f".into() },
            Problem::TaskTextEmpty,
            Problem::TasksNewerFormat { format: 2 },
            Problem::TasksSetAside { path: "/x.json.corrupt-1".into(), detail: "EOF".into() },
            Problem::TasksFailed { detail: "disk full".into() },
            Problem::TasksNotWatched,
            Problem::TaskTooLong { field: TaskField::Description, max: MAX_TASK_DESCRIPTION },
            Problem::TaskTooManyNotes { max: MAX_TASK_NOTES },
            Problem::TasksTooMany { max: MAX_TASKS },
            Problem::TaskMoveRefused,
            Problem::TasksListChanged { shown: "/a".into(), now: "/b".into() },
            Problem::TasksKeyUnknown { detail: "fatal".into() },
            Problem::TasksUnknownValues { detail: "unknown variant".into() },
            Problem::TasksDuplicateId { id: "a".into() },
            Problem::TasksNotEmpty { tasks: 3 },
            Problem::TasksNotAdoptable { from: "/a".into() },
            Problem::TasksNothingToAdopt { from: "/a".into() },
            Problem::TasksSameList,
            Problem::TasksLocked { secs: 5 },
        ];
        for problem in problems {
            let ev = AgentEvent::TasksEdited { id: 3, key: "p1".into(), task: Some("a1".into()), problem: Some(problem.clone()), content_rev: Some(2) };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
            let ev = AgentEvent::TasksNotice { key: "p1".into(), problem };
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        }
    }

    #[test]
    fn task_shapes_decode_across_versions() {
        // The smallest task a writer may leave: everything else takes its default.
        let old = r#"{"id":"a","reference":"k3f","title":"Old"}"#;
        let t: Task = decode(old).unwrap();
        assert_eq!((t.status, t.priority, t.rev, t.removed, t.notes.len(), t.worktree), (TaskStatus::Inbox, Priority::None, 0, None, 0, None));
        // A note without its author: the user's.
        let note: TaskNote = decode(r#"{"text":"x","at":5}"#).unwrap();
        assert_eq!(note.author, Author::User);
        // Fields a newer writer put in a note are kept too.
        let note: TaskNote = decode(r#"{"text":"x","at":5,"model":"opus"}"#).unwrap();
        assert!(encode(&note).contains(r#""model":"opus""#));
        // Before content revisions and repository ids.
        assert_eq!(t.content_rev, 0);
        let list: TaskList = decode(r#"{"format":1,"key":"/k"}"#).unwrap();
        assert_eq!((list.repo_id, list.retired.len(), list.tasks.len()), (None, 0, 0));
        let board: TaskBoard = decode(r#"{"key":"p","list":null,"read_only":false,"problem":null}"#).unwrap();
        assert!(board.found.is_empty());
        let found: FoundList = decode(r#"{"key":"/k","tasks":1}"#).unwrap();
        assert_eq!(found.why, FoundWhy::PreviousKey);
        let adopt: AgentCmd = decode(r#"{"type":"AdoptTasks","id":1,"key":"p","from":"/k"}"#).unwrap();
        assert!(matches!(adopt, AgentCmd::AdoptTasks { merge: false, list: None, .. }));
        let edited: AgentEvent = decode(r#"{"type":"TasksEdited","id":1,"key":"p","task":null,"problem":null}"#).unwrap();
        assert!(matches!(edited, AgentEvent::TasksEdited { content_rev: None, .. }));
        // Fields a newer writer added (same format) are kept when the list is written again.
        let newer = r#"{"format":1,"key":"/k","revision":2,"labels":["x"],"tasks":[{"id":"a","reference":"k3f","title":"T","due":"2027-01-01"}]}"#;
        let list: TaskList = decode(newer).unwrap();
        let again = encode(&list);
        assert!(again.contains(r#""due":"2027-01-01""#) && again.contains(r#""labels":["x"]"#), "{again}");
        // The optional links are left out when empty.
        assert!(!encode(&task("a", &[])).contains("worktree"));
        // An edit without `expect` (an older app); an add with only a title.
        let cmd: AgentCmd = decode(r#"{"type":"EditTasks","id":1,"key":"p","op":{"op":"add","title":"T"}}"#).unwrap();
        assert_eq!(
            cmd,
            AgentCmd::EditTasks {
                id: 1,
                key: "p".into(),
                op: TaskOp::Add { title: "T".into(), description: String::new(), priority: Priority::None, status: TaskStatus::Inbox },
                expect: None,
                list: None
            }
        );
        // An older agent's hello: no task lists.
        let old = r#"{"type":"Hello","version":"0.1.7","hooks":"Installed","send_text":true}"#;
        assert!(matches!(decode::<AgentEvent>(old).unwrap(), AgentEvent::Hello { tasks: false, send_text: true, .. }));
        // Statuses and priorities are written as plain words.
        assert_eq!(encode(&TaskStatus::Canceled).trim(), r#""canceled""#);
        assert_eq!(encode(&Priority::Urgent).trim(), r#""urgent""#);
    }

    #[test]
    fn dependency_cycles_are_found() {
        // c waits for b, b waits for a.
        let tasks = vec![task("a", &[]), task("b", &["a"]), task("c", &["b"])];
        assert!(task_cycle(&tasks, "a", "a"), "itself");
        assert!(task_cycle(&tasks, "a", "c"), "a -> c -> b -> a");
        assert!(task_cycle(&tasks, "a", "b"));
        assert!(!task_cycle(&tasks, "c", "a"), "already implied, but no loop");
        assert!(!task_cycle(&tasks, "a", "zz"), "unknown task");
        // A loop elsewhere does not hang the search.
        let looped = vec![task("x", &["y"]), task("y", &["x"]), task("z", &[])];
        assert!(!task_cycle(&looped, "z", "x"));
        assert_eq!(TaskOp::Remove { task: "a".into() }.task(), Some("a"));
        assert!(TaskStatus::Canceled.finished() && !TaskStatus::Review.finished());
    }

    #[test]
    fn roundtrip_review() {
        let file = |path: &str, kind| FileChange { path: path.into(), kind, added: Some(2), removed: Some(1), mtime: 0, binary: false };
        let summary = ReviewSummary {
            worktree: "/home/me/shop/.claude/worktrees/invoices".into(),
            branch: Some("feat/invoices".into()),
            head: "b".repeat(40),
            base: "origin/main".into(),
            base_sha: "c".repeat(40),
            merge_base: Some("a".repeat(40)),
            state: ReviewState::Ahead,
            ahead: 3,
            behind: 1,
            commits: vec![CommitInfo { sha: "b".repeat(40), summary: "feat: totals \u{e9}".into(), author: "Me".into(), time: 1_700_000_000 }],
            more_commits: 2,
            files: ReviewFiles { files: vec![file("src/pdf.rs", ChangeKind::Modified), file("docs/new name.md", ChangeKind::Renamed("docs/old.md".into()))], more: 4, added: 120, removed: 7 },
            uncommitted: ReviewFiles { files: vec![file("notes.txt", ChangeKind::Untracked)], more: 0, added: 2, removed: 0 },
            base_kind: BaseKind::Remote,
            several_merge_bases: true,
            merges: vec!["b".repeat(40)],
        };
        assert!(summary.base_moved() && summary.dirty());
        assert_eq!(summary.files.count(), 6);
        let events = [
            AgentEvent::Review { id: 7, key: "p".into(), summary: Some(Box::new(summary.clone())), bases: vec!["origin/main".into(), "develop".into()], problem: None },
            AgentEvent::Review { id: 8, key: "p".into(), summary: None, bases: vec![], problem: Some(Problem::ReviewBaseNotFound { base: "gone".into() }) },
            AgentEvent::ReviewFiles { id: 9, key: "p".into(), files: summary.files.clone(), problem: None },
            AgentEvent::ReviewDiff { id: 10, key: "p".into(), text: "+a\n".into(), truncated: true, problem: None },
            AgentEvent::Review { id: 11, key: "p".into(), summary: None, bases: vec![], problem: Some(Problem::ReviewBaseRefused { base: "-x".into() }) },
        ];
        for ev in events {
            assert_eq!(decode::<AgentEvent>(&encode(&ev)).unwrap(), ev);
        }
        for problem in [Problem::ReviewNoBase, Problem::ReviewNoCommits, Problem::NotARegularFile { path: "\"caf\\351\"".into() }, Problem::ReviewTimedOut { secs: 20 }] {
            assert_eq!(decode::<Problem>(&encode(&problem)).unwrap(), problem);
        }
        let unrelated = ReviewSummary { state: ReviewState::Unrelated, merge_base: None, ..summary };
        assert_eq!(decode::<ReviewSummary>(&encode(&unrelated)).unwrap(), unrelated);
        let cmds = [
            AgentCmd::Review { id: 1, key: "p".into(), worktree: "/w".into(), base: None },
            AgentCmd::Review { id: 2, key: "p".into(), worktree: "/w".into(), base: Some("develop".into()) },
            AgentCmd::ReviewFiles { id: 3, key: "p".into(), worktree: "/w".into(), scope: ReviewScope::Commit { sha: "b".repeat(40) } },
            AgentCmd::ReviewDiff {
                id: 4,
                key: "p".into(),
                worktree: "/w".into(),
                scope: ReviewScope::Branch { merge_base: "a".repeat(40), head: "b".repeat(40) },
                path: Some("docs/new name.md".into()),
                old_path: Some("docs/old.md".into()),
            },
            AgentCmd::ReviewDiff { id: 5, key: "p".into(), worktree: "/w".into(), scope: ReviewScope::Uncommitted, path: None, old_path: None },
        ];
        for cmd in cmds {
            assert_eq!(decode::<AgentCmd>(&encode(&cmd)).unwrap(), cmd);
        }
        // What older agents send: no review capability, no new summary fields.
        let old = r#"{"type":"Hello","version":"0.1.7","hooks":"Installed","send_text":true}"#;
        assert!(matches!(decode::<AgentEvent>(old).unwrap(), AgentEvent::Hello { review: false, .. }));
        let old = r#"{"worktree":"/w","head":"a","base":"main","base_sha":"b","merge_base":null,"state":"UpToDate","ahead":0,"behind":0,"commits":[],"more_commits":0,"files":{"files":[],"more":0,"added":0,"removed":0},"uncommitted":{"files":[],"more":0,"added":0,"removed":0},"branch":null}"#;
        let old: ReviewSummary = decode(old).unwrap();
        assert_eq!((old.base_kind, old.several_merge_bases, old.merges.len()), (BaseKind::Branch, false, 0));
        // Only a live task in review offers its worktree.
        let mut task: Task = serde_json::from_value(serde_json::json!({"id": "t", "reference": "r", "title": "T", "status": "review", "created": 0, "updated": 0, "worktree": "/w"})).unwrap();
        assert_eq!(task.review_worktree(), Some("/w"));
        task.status = TaskStatus::Active;
        assert_eq!(task.review_worktree(), None);
        // Optional fields may be left out.
        let short = r#"{"type":"ReviewDiff","id":1,"key":"p","worktree":"/w","scope":{"kind":"Uncommitted"}}"#;
        assert!(matches!(decode::<AgentCmd>(short).unwrap(), AgentCmd::ReviewDiff { path: None, old_path: None, .. }));
    }
}
