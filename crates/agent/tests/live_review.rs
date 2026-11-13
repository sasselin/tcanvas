//! Review of a branch against its base, end to end: real `tcanvas-agent` binary, real throwaway
//! git repository with linked worktrees. Skipped cleanly if git is not installed.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{AgentCmd, AgentEvent, BaseKind, ChangeKind, FileChange, Problem, ReviewFiles, ReviewScope, ReviewState, ReviewSummary};

const AGENT: &str = env!("CARGO_BIN_EXE_tcanvas-agent");

/// Git sees neither the user's nor the system's configuration.
const GIT_ENV: [(&str, &str); 2] = [("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")];

/// The temporary folder, removed even when an assertion fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tcanvas-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(std::fs::canonicalize(&dir).unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Agent {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<AgentEvent>,
    backlog: RefCell<Vec<AgentEvent>>,
    next: u64,
}

impl Agent {
    fn spawn() -> Self {
        // Nonexistent tmux socket: no sessions, nothing gets touched.
        let mut child =
            Command::new(AGENT).env("TCANVAS_TMUX_SOCKET", format!("tcanvas-review-test-{}", std::process::id())).envs(GIT_ENV).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, events) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(protocol::decode(&line).expect("valid JSON line"));
            }
        });
        Agent { child, stdin, events, backlog: RefCell::new(Vec::new()), next: 0 }
    }

    fn send(&mut self, cmd: &AgentCmd) {
        self.stdin.write_all(protocol::encode(cmd).as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn wait_for(&self, what: &str, pred: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        let mut backlog = self.backlog.borrow_mut();
        if let Some(i) = backlog.iter().position(&pred) {
            return backlog.remove(i);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(ev) if pred(&ev) => return ev,
                Ok(ev) => backlog.push(ev),
                Err(_) => break,
            }
        }
        panic!("expected: {what}\nreceived: {backlog:#?}");
    }

    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    /// Watches `path` as `key` and waits for its first scan.
    fn watch(&mut self, key: &str, path: &Path) {
        self.send(&AgentCmd::Watch { key: key.into(), path: path.display().to_string() });
        self.wait_for("first scan", |ev| matches!(ev, AgentEvent::Changes(s) if s.key == key));
    }

    fn review(&mut self, worktree: &Path, base: Option<&str>) -> (Result<ReviewSummary, Problem>, Vec<String>) {
        let id = self.id();
        self.send(&AgentCmd::Review { id, key: "shop".into(), worktree: worktree.display().to_string(), base: base.map(String::from) });
        match self.wait_for("a review", |ev| matches!(ev, AgentEvent::Review { id: i, .. } if *i == id)) {
            AgentEvent::Review { summary: Some(s), bases, problem: None, .. } => (Ok(*s), bases),
            AgentEvent::Review { problem: Some(p), bases, .. } => (Err(p), bases),
            ev => panic!("{ev:?}"),
        }
    }

    fn summary(&mut self, worktree: &Path, base: Option<&str>) -> ReviewSummary {
        self.review(worktree, base).0.unwrap_or_else(|p| panic!("{worktree:?}: {p:?}"))
    }

    fn files(&mut self, worktree: &Path, scope: ReviewScope) -> Result<ReviewFiles, Problem> {
        let id = self.id();
        self.send(&AgentCmd::ReviewFiles { id, key: "shop".into(), worktree: worktree.display().to_string(), scope });
        match self.wait_for("files", |ev| matches!(ev, AgentEvent::ReviewFiles { id: i, .. } if *i == id)) {
            AgentEvent::ReviewFiles { files, problem: None, .. } => Ok(files),
            AgentEvent::ReviewFiles { problem: Some(p), .. } => Err(p),
            _ => unreachable!(),
        }
    }

    fn diff(&mut self, worktree: &Path, scope: ReviewScope, path: Option<&str>, old_path: Option<&str>) -> Result<(String, bool), Problem> {
        let id = self.id();
        self.send(&AgentCmd::ReviewDiff { id, key: "shop".into(), worktree: worktree.display().to_string(), scope, path: path.map(String::from), old_path: old_path.map(String::from) });
        match self.wait_for("a diff", |ev| matches!(ev, AgentEvent::ReviewDiff { id: i, .. } if *i == id)) {
            AgentEvent::ReviewDiff { text, truncated, problem: None, .. } => Ok((text, truncated)),
            AgentEvent::ReviewDiff { problem: Some(p), .. } => Err(p),
            _ => unreachable!(),
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn has_git() -> bool {
    Command::new("git").arg("--version").stdout(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .envs(GIT_ENV)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn rev(dir: &Path, what: &str) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(["rev-parse", what]).envs(GIT_ENV).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn file<'a>(files: &'a ReviewFiles, path: &str) -> &'a FileChange {
    files.files.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("{path} missing: {files:#?}"))
}

/// `shop` (main: a.txt, gone.txt, logo.bin, `old name.txt`), and a worktree `feat` three commits
/// ahead of where it started; main then moves on by one commit.
fn shop(tmp: &Path) -> (PathBuf, PathBuf) {
    let shop = tmp.join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    std::fs::write(shop.join(".gitignore"), "wt/\n").unwrap();
    std::fs::write(shop.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(shop.join("gone.txt"), "bye\n").unwrap();
    std::fs::write(shop.join("logo.bin"), [0u8, 1, 2, 3, 0, 9]).unwrap();
    std::fs::write(shop.join("old name.txt"), "a long enough text\nto be seen as renamed\nwhen moved\n").unwrap();
    commit_all(&shop, "init");
    let feat = tmp.join("feat");
    git(&shop, &["worktree", "add", "-q", "-b", "feat/invoices", feat.to_str().unwrap()]);
    std::fs::write(feat.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    commit_all(&feat, "edit a");
    std::fs::create_dir_all(feat.join("src")).unwrap();
    std::fs::write(feat.join("src/nouveau \u{e9}t\u{e9}.rs"), "fn main() {}\n// x\n// y\n").unwrap();
    std::fs::remove_file(feat.join("gone.txt")).unwrap();
    commit_all(&feat, "add and delete");
    std::fs::create_dir_all(feat.join("docs")).unwrap();
    std::fs::rename(feat.join("old name.txt"), feat.join("docs/new name.txt")).unwrap();
    std::fs::write(feat.join("logo.bin"), [0u8, 7, 7, 7, 0, 9, 1]).unwrap();
    commit_all(&feat, "move and redraw");
    std::fs::write(shop.join("main-only.txt"), "later\n").unwrap();
    commit_all(&shop, "main moves on");
    (shop, feat)
}

#[test]
fn a_branch_ahead_of_its_base_with_every_kind_of_change() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("review");
    let (shop, feat) = shop(&tmp.0);
    let init = rev(&shop, "HEAD~1");
    let mut agent = Agent::spawn();
    agent.watch("shop", &shop);

    let (summary, bases) = agent.review(&feat, None);
    let s = summary.unwrap();
    assert_eq!((s.branch.as_deref(), s.base.as_str(), s.state, s.ahead, s.behind), (Some("feat/invoices"), "main", ReviewState::Ahead, 3, 1));
    assert_eq!(s.worktree, feat.display().to_string());
    assert_eq!(s.head, rev(&feat, "HEAD"));
    assert_eq!(s.base_sha, rev(&shop, "main"));
    assert_eq!(s.merge_base.as_deref(), Some(init.as_str()), "where the branch started, not the base's tip");
    assert!(s.base_moved());
    assert_eq!(s.commits.iter().map(|c| c.summary.as_str()).collect::<Vec<_>>(), vec!["move and redraw", "add and delete", "edit a"], "newest first");
    assert!(s.commits.iter().all(|c| c.author == "Test" && c.time > 1_600_000_000 && c.sha.len() == 40));
    assert_eq!(s.more_commits, 0);
    assert_eq!(bases, vec!["main".to_string()], "the branch itself is not a base");

    // The files, exact: kinds, counts, renames, binaries, a path with a space and accents.
    let f = &s.files;
    assert_eq!(f.files.len(), 5, "{f:#?}");
    assert_eq!(f.more, 0);
    let a = file(f, "a.txt");
    assert_eq!((&a.kind, a.added, a.removed, a.binary), (&ChangeKind::Modified, Some(2), Some(1), false));
    let new = file(f, "src/nouveau \u{e9}t\u{e9}.rs");
    assert_eq!((&new.kind, new.added, new.removed), (&ChangeKind::Added, Some(3), Some(0)));
    let gone = file(f, "gone.txt");
    assert_eq!((&gone.kind, gone.added, gone.removed), (&ChangeKind::Deleted, Some(0), Some(1)));
    let moved = file(f, "docs/new name.txt");
    assert_eq!((&moved.kind, moved.added, moved.removed), (&ChangeKind::Renamed("old name.txt".into()), Some(0), Some(0)));
    let logo = file(f, "logo.bin");
    assert_eq!((&logo.kind, logo.added, logo.removed, logo.binary), (&ChangeKind::Modified, None, None, true));
    assert_eq!((f.added, f.removed), (5, 2));
    assert!(!s.dirty() && s.uncommitted.files.is_empty());

    // The whole branch's diff of one file, and of the renamed one.
    let branch = ReviewScope::Branch { merge_base: init.clone(), head: s.head.clone() };
    let (text, truncated) = agent.diff(&feat, branch.clone(), Some("a.txt"), None).unwrap();
    assert!(text.contains("-two") && text.contains("+TWO") && text.contains("+four") && !truncated, "{text}");
    let (text, _) = agent.diff(&feat, branch.clone(), Some("docs/new name.txt"), Some("old name.txt")).unwrap();
    assert!(text.contains("rename from old name.txt") && text.contains("rename to docs/new name.txt"), "{text}");
    let (text, _) = agent.diff(&feat, branch.clone(), Some("src/nouveau \u{e9}t\u{e9}.rs"), None).unwrap();
    assert!(text.contains("+fn main() {}"), "{text}");
    // A path the branch did not touch: no difference.
    assert_eq!(agent.diff(&feat, branch.clone(), Some("main-only.txt"), None), Err(Problem::NoDiff));

    // One commit: its own files, and its diff whole or for one file.
    let middle = s.commits[1].sha.clone();
    let one = agent.files(&feat, ReviewScope::Commit { sha: middle.clone() }).unwrap();
    let paths: Vec<(&str, &ChangeKind)> = one.files.iter().map(|f| (f.path.as_str(), &f.kind)).collect();
    assert_eq!(paths, vec![("gone.txt", &ChangeKind::Deleted), ("src/nouveau \u{e9}t\u{e9}.rs", &ChangeKind::Added)]);
    assert_eq!((one.added, one.removed, one.more), (3, 1, 0));
    let (text, _) = agent.diff(&feat, ReviewScope::Commit { sha: middle.clone() }, None, None).unwrap();
    assert!(text.starts_with(&format!("commit {middle}")) && text.contains("add and delete") && text.contains("-bye") && text.contains("+// y"), "{text}");
    let (text, _) = agent.diff(&feat, ReviewScope::Commit { sha: middle.clone() }, Some("gone.txt"), None).unwrap();
    assert!(text.contains("-bye") && !text.contains("fn main"), "limited to that file: {text}");
    let first = s.commits[2].sha.clone();
    let (text, _) = agent.diff(&feat, ReviewScope::Commit { sha: first }, Some("a.txt"), None).unwrap();
    assert!(text.contains("+TWO"), "{text}");

    // Uncommitted work: reported apart, never mixed into the branch's files.
    std::fs::write(feat.join("a.txt"), "one\nTWO\nthree\nfour\nfive\n").unwrap();
    std::fs::write(feat.join("notes draft.txt"), "todo\nlater\n").unwrap();
    let dirty = agent.summary(&feat, None);
    assert_eq!(dirty.files, s.files, "the branch's files do not change");
    assert!(dirty.dirty());
    let u = &dirty.uncommitted;
    assert_eq!(u.files.iter().map(|f| (f.path.as_str(), &f.kind, f.added)).collect::<Vec<_>>(), vec![("a.txt", &ChangeKind::Modified, Some(1)), ("notes draft.txt", &ChangeKind::Untracked, Some(2))]);
    let (text, _) = agent.diff(&feat, ReviewScope::Uncommitted, Some("a.txt"), None).unwrap();
    assert!(text.contains("+five") && !text.contains("+TWO"), "against HEAD: {text}");
    let (text, _) = agent.diff(&feat, ReviewScope::Uncommitted, Some("notes draft.txt"), None).unwrap();
    assert!(text.contains("+todo"), "{text}");
    assert_eq!(agent.files(&feat, ReviewScope::Uncommitted).unwrap(), dirty.uncommitted);
}

#[test]
fn identical_unrelated_detached_and_refused() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("review-states");
    let (shop, feat) = shop(&tmp.0);
    let mut agent = Agent::spawn();
    agent.watch("shop", &shop);

    // Identical to its base: nothing to review (a typed state, not an error).
    let same = tmp.0.join("same");
    git(&shop, &["worktree", "add", "-q", "-b", "same", same.to_str().unwrap(), "main"]);
    let s = agent.summary(&same, None);
    assert_eq!((s.state, s.ahead, s.behind), (ReviewState::UpToDate, 0, 0));
    assert!(s.commits.is_empty() && s.files.files.is_empty());
    // The main checkout reviewed against itself.
    assert_eq!(agent.summary(&shop, None).state, ReviewState::UpToDate);
    // Behind only: still nothing to review.
    let old = tmp.0.join("old");
    git(&shop, &["worktree", "add", "-q", "-b", "old", old.to_str().unwrap(), "main~1"]);
    let s = agent.summary(&old, None);
    assert_eq!((s.state, s.ahead, s.behind), (ReviewState::UpToDate, 0, 1));

    // Another base: the branch against `same` (main), then against `old` (where it started).
    let s = agent.summary(&feat, Some("old"));
    assert_eq!((s.base.as_str(), s.ahead, s.behind, s.state), ("old", 3, 0, ReviewState::Ahead));
    assert!(!s.base_moved());

    // A detached HEAD on the branch's commit.
    let det = tmp.0.join("det");
    git(&shop, &["worktree", "add", "-q", "--detach", det.to_str().unwrap(), "feat/invoices"]);
    let s = agent.summary(&det, None);
    assert_eq!((s.branch.as_deref(), s.state, s.ahead, s.files.files.len()), (None, ReviewState::Ahead, 3, 5));

    // Unrelated histories: no merge base.
    let lonely = tmp.0.join("lonely");
    git(&shop, &["worktree", "add", "-q", "--detach", lonely.to_str().unwrap(), "main"]);
    git(&lonely, &["checkout", "-q", "--orphan", "lonely"]);
    git(&lonely, &["rm", "-rq", "--cached", "."]);
    for entry in std::fs::read_dir(&lonely).unwrap().flatten() {
        if entry.file_name() != ".git" {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    std::fs::write(lonely.join("other.txt"), "x\n").unwrap();
    commit_all(&lonely, "elsewhere");
    let s = agent.summary(&lonely, None);
    assert_eq!((s.state, s.merge_base.as_deref()), (ReviewState::Unrelated, None));
    assert!(s.files.files.is_empty() && s.commits.is_empty());

    // Refused: a folder that is not one of the repository's worktrees, a base that could pass
    // for an option or names nothing, a path that escapes, a sha that is not one.
    let elsewhere = tmp.0.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let (r, _) = agent.review(&elsewhere, None);
    assert_eq!(r, Err(Problem::NotAWorktree { path: elsewhere.display().to_string() }));
    let (r, _) = agent.review(&feat.join("src"), None);
    assert!(matches!(r, Err(Problem::NotAWorktree { .. })), "a subfolder is not a worktree: {r:?}");
    let (r, bases) = agent.review(&feat, Some("--output=/tmp/x"));
    assert_eq!(r, Err(Problem::ReviewBaseRefused { base: "--output=/tmp/x".into() }));
    assert!(bases.contains(&"main".to_string()), "the bases are still listed: {bases:?}");
    let (r, _) = agent.review(&feat, Some("main..feat"));
    assert!(matches!(r, Err(Problem::ReviewBaseRefused { .. })));
    let (r, _) = agent.review(&feat, Some("nothing-here"));
    assert_eq!(r, Err(Problem::ReviewBaseNotFound { base: "nothing-here".into() }));
    let s = agent.summary(&feat, None);
    let branch = ReviewScope::Branch { merge_base: s.merge_base.clone().unwrap(), head: s.head.clone() };
    assert_eq!(agent.diff(&feat, branch.clone(), Some("../shop/a.txt"), None), Err(Problem::PathRefused));
    assert_eq!(agent.diff(&feat, branch.clone(), Some("/etc/passwd"), None), Err(Problem::PathRefused));
    assert_eq!(agent.diff(&feat, branch, Some("a.txt"), Some("../x")), Err(Problem::PathRefused));
    assert_eq!(agent.diff(&feat, ReviewScope::Commit { sha: "--all".into() }, None, None), Err(Problem::CommitRefused));
    assert_eq!(agent.files(&feat, ReviewScope::Commit { sha: "0".repeat(40) }), Err(Problem::CommitNotFound));

    // A key the agent does not watch.
    let id = agent.id();
    agent.send(&AgentCmd::Review { id, key: "nobody".into(), worktree: feat.display().to_string(), base: None });
    let ev = agent.wait_for("refused", |ev| matches!(ev, AgentEvent::Review { id: i, .. } if *i == id));
    assert!(matches!(ev, AgentEvent::Review { problem: Some(Problem::NotAGitRepo), summary: None, .. }), "{ev:?}");
}

#[test]
fn large_branches_are_bounded() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("review-large");
    let shop = tmp.0.join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    std::fs::write(shop.join(".gitignore"), "x\n").unwrap();
    commit_all(&shop, "init");
    let big = tmp.0.join("big");
    git(&shop, &["worktree", "add", "-q", "-b", "big", big.to_str().unwrap()]);
    // 501 files and a file of about 1.2 MB in one commit, then 201 empty commits.
    std::fs::create_dir_all(big.join("many")).unwrap();
    for i in 0..501 {
        std::fs::write(big.join(format!("many/f{i:03}.txt")), format!("{i}\n")).unwrap();
    }
    let line = "x".repeat(99) + "\n";
    std::fs::write(big.join("huge.txt"), line.repeat(12_000)).unwrap();
    commit_all(&big, "lots");
    for i in 0..201 {
        git(&big, &["commit", "-q", "--allow-empty", "-m", &format!("empty {i}")]);
    }

    let mut agent = Agent::spawn();
    agent.watch("shop", &shop);
    let s = agent.summary(&big, None);
    assert_eq!((s.ahead, s.commits.len(), s.more_commits), (202, 200, 2));
    assert_eq!((s.files.files.len(), s.files.more, s.files.count()), (500, 2, 502));
    assert_eq!(s.files.added, 501 + 12_000, "the totals count every file, listed or not");
    // The diff of the huge file stops at the bound, with the notice.
    let branch = ReviewScope::Branch { merge_base: s.merge_base.clone().unwrap(), head: s.head.clone() };
    let (text, truncated) = agent.diff(&big, branch, Some("huge.txt"), None).unwrap();
    assert!(truncated && text.len() <= 512 * 1024 && text.len() > 400 * 1024, "{} bytes", text.len());
    assert!(text.starts_with("diff --git"));
}

/// A file name that is not UTF-8: listed quoted (`"caf\351.txt"`), its diff found from that name,
/// in the review and in the working changes alike.
#[test]
fn names_that_are_not_utf8_keep_their_diff() {
    use std::os::unix::ffi::OsStrExt;
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("review-names");
    let (shop, feat) = shop(&tmp.0);
    std::fs::write(feat.join(std::ffi::OsStr::from_bytes(b"caf\xe9.txt")), "espresso\n").unwrap();
    commit_all(&feat, "a cafe");
    std::fs::write(feat.join(std::ffi::OsStr::from_bytes(b"th\xe9.txt")), "green\n").unwrap();
    let mut agent = Agent::spawn();
    agent.watch("shop", &shop);
    let s = agent.summary(&feat, None);
    let name = "\"caf\\351.txt\"";
    let listed = file(&s.files, name);
    assert_eq!((&listed.kind, listed.added), (&ChangeKind::Added, Some(1)));
    let branch = ReviewScope::Branch { merge_base: s.merge_base.clone().unwrap(), head: s.head.clone() };
    let (text, _) = agent.diff(&feat, branch, Some(name), None).unwrap();
    assert!(text.contains("+espresso"), "{text}");
    let untracked = "\"th\\351.txt\"";
    assert_eq!(file(&s.uncommitted, untracked).added, Some(1));
    let (text, _) = agent.diff(&feat, ReviewScope::Uncommitted, Some(untracked), None).unwrap();
    assert!(text.contains("+green"), "{text}");

    // The working changes of the same folder.
    agent.send(&AgentCmd::Watch { key: "feat".into(), path: feat.display().to_string() });
    agent.wait_for("the odd name listed", |ev| matches!(ev, AgentEvent::Changes(c) if c.key == "feat" && c.files.iter().any(|f| f.path == untracked && f.added == Some(1))));
    agent.send(&AgentCmd::Diff { key: "feat".into(), file: untracked.into() });
    let AgentEvent::Diff { text, problem, .. } = agent.wait_for("its diff", |ev| matches!(ev, AgentEvent::Diff { file, .. } if file == untracked)) else { unreachable!() };
    assert!(problem.is_none() && text.contains("+green"), "{problem:?} {text}");
}

/// A symbolic link to a pipe never blocks anything, and a link out of the worktree is never read.
#[test]
fn links_to_pipes_and_outside_files_are_never_read() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("review-links");
    let (shop, feat) = shop(&tmp.0);
    let ok = Command::new("mkfifo").arg(tmp.0.join("pipe")).status().unwrap().success();
    assert!(ok, "mkfifo");
    std::os::unix::fs::symlink("../pipe", feat.join("fifo")).unwrap();
    std::fs::write(tmp.0.join("secret.txt"), "1\n2\n3\n4\n").unwrap();
    std::os::unix::fs::symlink(tmp.0.join("secret.txt"), feat.join("outside")).unwrap();
    let mut agent = Agent::spawn();
    agent.watch("shop", &shop);
    let s = agent.summary(&feat, None);
    let (fifo, outside) = (file(&s.uncommitted, "fifo"), file(&s.uncommitted, "outside"));
    assert_eq!((fifo.added, outside.added), (None, None), "no line count through a link");
    assert_eq!(agent.diff(&feat, ReviewScope::Uncommitted, Some("fifo"), None), Err(Problem::NotARegularFile { path: "fifo".into() }));
    assert_eq!(agent.diff(&feat, ReviewScope::Uncommitted, Some("outside"), None), Err(Problem::NotARegularFile { path: "outside".into() }));
    // Later requests are still answered.
    assert_eq!(agent.summary(&feat, None).state, ReviewState::Ahead);

    // The working changes of that folder too.
    agent.send(&AgentCmd::Watch { key: "feat".into(), path: feat.display().to_string() });
    let AgentEvent::Changes(set) = agent.wait_for("the links listed", |ev| matches!(ev, AgentEvent::Changes(c) if c.key == "feat" && c.files.len() == 2)) else { unreachable!() };
    assert!(set.files.iter().all(|f| f.added.is_none()), "{set:?}");
    agent.send(&AgentCmd::Diff { key: "feat".into(), file: "fifo".into() });
    let ev = agent.wait_for("refused", |ev| matches!(ev, AgentEvent::Diff { file, .. } if file == "fifo"));
    assert!(matches!(ev, AgentEvent::Diff { problem: Some(Problem::NotARegularFile { .. }), .. }), "{ev:?}");
    assert_eq!(agent.summary(&feat, None).state, ReviewState::Ahead, "still answering");
}

/// A tag named like the base never replaces the branch; a remote-tracking base says so; crossed
/// histories and merge commits are reported.
#[test]
fn bases_by_kind_crossed_histories_and_merges() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("review-kinds");
    let (shop, feat) = shop(&tmp.0);
    git(&shop, &["tag", "main", "main~1"]);
    git(&shop, &["update-ref", "refs/remotes/origin/develop", "refs/heads/main~1"]);
    let mut agent = Agent::spawn();
    agent.watch("shop", &shop);
    let s = agent.summary(&feat, None);
    assert_eq!((s.base.as_str(), s.base_kind, s.base_sha.clone(), s.behind), ("main", BaseKind::Branch, rev(&shop, "refs/heads/main"), 1), "the branch, not the tag");
    let s = agent.summary(&feat, Some("origin/develop"));
    assert_eq!((s.base_kind, s.behind, s.several_merge_bases), (BaseKind::Remote, 0, false));
    assert!(s.merges.is_empty());

    // Criss-cross: main and the branch each merged the other's commit.
    let cross = tmp.0.join("cross");
    git(&shop, &["worktree", "add", "-q", "-b", "cross", cross.to_str().unwrap(), "refs/heads/main"]);
    std::fs::write(cross.join("c.txt"), "c\n").unwrap();
    commit_all(&cross, "on cross");
    let on_cross = rev(&cross, "HEAD");
    std::fs::write(shop.join("m.txt"), "m\n").unwrap();
    commit_all(&shop, "on main");
    let on_main = rev(&shop, "HEAD");
    git(&shop, &["merge", "-q", "--no-edit", &on_cross]);
    git(&cross, &["merge", "-q", "--no-edit", &on_main]);
    let s = agent.summary(&cross, None);
    assert!(s.several_merge_bases, "{s:#?}");
    assert_eq!(s.merges, vec![rev(&cross, "HEAD")], "the merge commit is labeled");
}

/// The worktree list's status mark follows what is not committed, a file modified again included.
#[test]
fn the_status_mark_follows_uncommitted_changes() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("review-mark");
    let (shop, feat) = shop(&tmp.0);
    std::fs::write(feat.join("a.txt"), "dirty\n").unwrap();
    let mut agent = Agent::spawn();
    agent.send(&AgentCmd::Watch { key: "shop".into(), path: shop.display().to_string() });
    let path = feat.display().to_string();
    let mark_of = |ev: &AgentEvent| match ev {
        AgentEvent::Worktrees(l) if l.key == "shop" => l.worktrees.iter().find(|w| w.path == path).and_then(|w| w.status_mark),
        _ => None,
    };
    let first = mark_of(&agent.wait_for("a mark", |ev| mark_of(ev).is_some()));
    // Same status line (` M a.txt`), new content and time.
    std::fs::write(feat.join("a.txt"), "dirty again, longer\n").unwrap();
    agent.wait_for("a new mark", |ev| mark_of(ev).is_some_and(|m| Some(m) != first));
}
