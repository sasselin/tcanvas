//! Merging a branch from the review, end to end: real `tcanvas-agent` binary on throwaway git
//! repositories in temporary folders (never the repository under test), isolated from the user's
//! and the system's git configuration, with a scratch home and a nonexistent tmux socket.
//!
//! The guarantee checked throughout: the result is prepared aside, and the user's worktrees are
//! only ever touched by the final fast-forward of the base. Whenever the outcome is not `Merged`,
//! both worktrees (files, index, status, refs, operations in progress) are byte for byte what they
//! were, the user's own changes made meanwhile included, and no private worktree is left behind.
//! Skipped cleanly if git is not installed.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{AgentCmd, AgentEvent, BaseKind, GitOperation, MergeOutcome, MergeRefusal, MergeStep, MergeStrategy, NotApplied, Problem};

const AGENT: &str = env!("CARGO_BIN_EXE_tcanvas-agent");

/// Git sees neither the user's nor the system's configuration.
const GIT_ENV: [(&str, &str); 2] = [("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")];
/// Variables that would give git an identity or another repository: never inherited.
const GIT_IDENTITY_VARS: [&str; 9] = ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL", "EMAIL", "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"];

/// The temporary folder, removed even when an assertion fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tcanvas-merge-{name}-{}", std::process::id()));
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
    stdin: Option<ChildStdin>,
    events: Receiver<AgentEvent>,
    backlog: RefCell<Vec<AgentEvent>>,
    next: u64,
}

impl Agent {
    fn spawn(home: &Path, envs: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(AGENT);
        for var in GIT_IDENTITY_VARS {
            cmd.env_remove(var);
        }
        // A scratch home: the merge records and private worktrees never go to the real one.
        cmd.env("TCANVAS_TMUX_SOCKET", format!("tcanvas-merge-test-{}", std::process::id())).env("TCANVAS_TEST_HOME", home).envs(GIT_ENV).envs(envs.iter().copied());
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, events) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(protocol::decode(&line).expect("valid JSON line"));
            }
        });
        Agent { child, stdin: Some(stdin), events, backlog: RefCell::new(Vec::new()), next: 0 }
    }

    fn send(&mut self, cmd: &AgentCmd) {
        let stdin = self.stdin.as_mut().expect("input open");
        stdin.write_all(protocol::encode(cmd).as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    fn wait_for(&self, what: &str, pred: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        let mut backlog = self.backlog.borrow_mut();
        if let Some(i) = backlog.iter().position(&pred) {
            return backlog.remove(i);
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(ev) if pred(&ev) => return ev,
                Ok(ev) => backlog.push(ev),
                Err(_) => break,
            }
        }
        panic!("expected: {what}\nreceived: {backlog:#?}");
    }

    /// Every event received so far (kept for later waits).
    fn seen(&self) -> Vec<AgentEvent> {
        let mut backlog = self.backlog.borrow_mut();
        backlog.extend(self.events.try_iter());
        backlog.clone()
    }

    fn watch(&mut self, path: &Path) {
        self.send(&AgentCmd::Watch { key: "shop".into(), path: path.display().to_string() });
        self.wait_for("first scan", |ev| matches!(ev, AgentEvent::Changes(s) if s.key == "shop"));
    }

    /// Sends a merge and returns its id.
    fn ask(&mut self, m: &Merge) -> u64 {
        self.next += 1;
        let id = self.next;
        self.send(&AgentCmd::Merge {
            id,
            key: "shop".into(),
            worktree: m.worktree.display().to_string(),
            branch: m.branch.clone(),
            base: m.base.clone(),
            strategy: m.strategy,
            message: m.message.clone(),
            branch_head: m.branch_head.clone(),
            base_head: m.base_head.clone(),
        });
        id
    }

    fn answer(&self, id: u64) -> MergeOutcome {
        match self.wait_for("a merge answer", |ev| matches!(ev, AgentEvent::Merged { id: i, .. } if *i == id)) {
            AgentEvent::Merged { outcome, .. } => outcome,
            _ => unreachable!(),
        }
    }

    fn merge(&mut self, m: &Merge) -> MergeOutcome {
        let id = self.ask(m);
        self.answer(id)
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn has_git() -> bool {
    Command::new("git").arg("--version").stdout(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// git for the test's own setup: its own identity on the command line, nothing else inherited.
fn git_out(dir: &Path, args: &[&str]) -> (bool, String) {
    let mut cmd = Command::new("git");
    for var in GIT_IDENTITY_VARS {
        cmd.env_remove(var);
    }
    let out = cmd
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Setup", "-c", "user.email=setup@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .envs(GIT_ENV)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (ok, out) = git_out(dir, args);
    assert!(ok, "git {args:?} in {dir:?}");
    out
}

fn rev(dir: &Path, what: &str) -> String {
    git(dir, &["rev-parse", what])
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// Everything of a worktree a merge could change: its branch and head, its index (`ls-files -s`),
/// its status (ignored files included), every ref, the files of an operation in progress, and
/// every file of its folder byte for byte.
fn snapshot(dir: &Path) -> String {
    let mut out = String::new();
    for args in [&["symbolic-ref", "-q", "HEAD"][..], &["rev-parse", "HEAD"], &["ls-files", "-s"], &["status", "--porcelain=v1", "--untracked-files=all", "--ignored"], &["for-each-ref"]] {
        out.push_str(&format!("$ git {args:?}\n{}\n", git_out(dir, args).1));
    }
    for name in ["MERGE_HEAD", "MERGE_MSG", "SQUASH_MSG", "CHERRY_PICK_HEAD", "rebase-merge", "rebase-apply", "BISECT_LOG", "ORIG_HEAD"] {
        let path = dir.join(git(dir, &["rev-parse", "--git-path", name]));
        out.push_str(&format!("{name}: {:?}\n", std::fs::read(&path).ok()));
    }
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if d == dir && e.file_name() == ".git" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                stack.push(p);
            } else {
                files.push((p.strip_prefix(dir).unwrap().display().to_string(), std::fs::read(&p).unwrap()));
            }
        }
    }
    files.sort();
    for (name, bytes) in files {
        out.push_str(&format!("{name}: {}\n", String::from_utf8_lossy(&bytes)));
    }
    out
}

/// The branch's worktree as `snapshot` gives it, the base's ref aside (it moves when merged).
fn branch_snapshot(dir: &Path) -> String {
    snapshot(dir).lines().filter(|l| !l.ends_with("refs/heads/main")).collect::<Vec<_>>().join("\n")
}

/// `shop` (main: a.txt, shared.txt) with the identity of its user in its own configuration, and a
/// worktree `feat` on `feat/invoices` two commits ahead (a new file, an edit of a.txt).
struct Shop {
    tmp: TempDir,
    shop: PathBuf,
    feat: PathBuf,
}

impl Shop {
    fn new(name: &str) -> Self {
        Self::with_identity(name, true)
    }

    fn with_identity(name: &str, identity: bool) -> Self {
        let tmp = TempDir::new(name);
        let shop = tmp.0.join("shop");
        std::fs::create_dir_all(&shop).unwrap();
        git(&shop, &["init", "-q", "-b", "main"]);
        if identity {
            git(&shop, &["config", "user.name", "Ada Lovelace"]);
            git(&shop, &["config", "user.email", "ada@example.com"]);
        }
        git(&shop, &["config", "commit.gpgsign", "false"]);
        write(&shop.join("a.txt"), "one\ntwo\nthree\n");
        write(&shop.join("shared.txt"), "base\n");
        commit_all(&shop, "init");
        let feat = tmp.0.join("feat");
        git(&shop, &["worktree", "add", "-q", "-b", "feat/invoices", feat.to_str().unwrap()]);
        write(&feat.join("invoice.txt"), "pdf\n");
        commit_all(&feat, "add invoices");
        write(&feat.join("a.txt"), "one\nTWO\nthree\n");
        commit_all(&feat, "edit a");
        Shop { tmp, shop, feat }
    }

    /// main moves on by one commit that does not touch the branch's files.
    fn main_moves_on(&self) {
        write(&self.shop.join("later.txt"), "later\n");
        commit_all(&self.shop, "main moves on");
    }

    /// Both change shared.txt: any strategy conflicts.
    fn conflicting(&self) {
        write(&self.feat.join("shared.txt"), "branch\n");
        commit_all(&self.feat, "branch side");
        write(&self.shop.join("shared.txt"), "main\n");
        commit_all(&self.shop, "main side");
    }

    /// The merge of `feat/invoices` into `main` as the user reviewed it now.
    fn merge(&self, strategy: MergeStrategy) -> Merge {
        Merge {
            worktree: self.feat.clone(),
            branch: "feat/invoices".into(),
            base: "main".into(),
            strategy,
            message: (strategy == MergeStrategy::Squash).then(|| "Invoices as PDF\n\nOne commit for the whole branch.\n".into()),
            branch_head: rev(&self.feat, "HEAD"),
            base_head: rev(&self.shop, "refs/heads/main"),
        }
    }

    fn agent(&self) -> Agent {
        self.agent_with(&[])
    }

    fn agent_with(&self, envs: &[(&str, &str)]) -> Agent {
        let mut agent = Agent::spawn(&self.home(), envs);
        agent.watch(&self.shop);
        agent
    }

    /// Both worktrees, to compare.
    fn snap(&self) -> (String, String) {
        (snapshot(&self.shop), snapshot(&self.feat))
    }

    /// Nothing of the agent's left: no private worktree, no record, no extra worktree entry.
    fn assert_clean_aside(&self) {
        let work = self.home().join(".local/share/tcanvas/merge/work");
        assert_eq!(std::fs::read_dir(&work).map(|d| d.count()).unwrap_or(0), 0, "no private worktree left");
        assert_eq!(self.records(), 0, "no record left");
        let listed = git(&self.shop, &["worktree", "list", "--porcelain"]);
        assert!(!listed.contains(".local/share/tcanvas"), "no worktree entry left: {listed}");
    }

    /// The agent's scratch home.
    fn home(&self) -> PathBuf {
        let home = self.tmp.0.join("home");
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn records(&self) -> usize {
        std::fs::read_dir(self.home().join(".local/share/tcanvas/merge/in-progress")).map(|d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).count()).unwrap_or(0)
    }

    /// A folder where the agent pauses at its test points (`TCANVAS_MERGE_TEST_PAUSE`).
    fn pauses(&self) -> PathBuf {
        let dir = self.tmp.0.join("pauses");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn paused_agent(&self, envs: &[(&str, &str)]) -> (Agent, PathBuf) {
        let pauses = self.pauses();
        let mut all = vec![("TCANVAS_MERGE_TEST_PAUSE", pauses.to_str().unwrap().to_string())];
        all.extend(envs.iter().map(|(k, v)| (*k, v.to_string())));
        let refs: Vec<(&str, &str)> = all.iter().map(|(k, v)| (*k, v.as_str())).collect();
        (self.agent_with(&refs), pauses)
    }
}

#[derive(Clone)]
struct Merge {
    worktree: PathBuf,
    branch: String,
    base: String,
    strategy: MergeStrategy,
    message: Option<String>,
    branch_head: String,
    base_head: String,
}

fn parents(dir: &Path, rev: &str) -> Vec<String> {
    git(dir, &["log", "-1", "--format=%P", rev]).split_whitespace().map(String::from).collect()
}

fn refused(outcome: MergeOutcome) -> MergeRefusal {
    match outcome {
        MergeOutcome::Refused { reason } => reason,
        other => panic!("refused expected: {other:?}"),
    }
}

/// Writes an executable hook into the repository's common hooks folder.
fn hook(shop: &Path, name: &str, script: &str) {
    let path = shop.join(".git/hooks").join(name);
    write(&path, script);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A hook that waits (bounded) for `go` to exist, then says `said` and ends with `code`.
fn slow_hook(go: &Path, said: &str, code: u8) -> String {
    format!("#!/bin/sh\ni=0\nwhile [ ! -e '{}' ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done\necho '{said}' >&2\nexit {code}\n", go.display())
}

/// Waits (bounded) until the agent reached `point`, without letting it go yet.
fn reached(pauses: &Path, point: &str) {
    until(&format!("{point} reached"), || pauses.join(format!("{point}.reached")).exists());
}

fn go(pauses: &Path, point: &str) {
    write(&pauses.join(format!("{point}.go")), "");
}

/// Lets every pause point go at once.
fn go_all(pauses: &Path) {
    for point in ["checked", "prepared", "applied"] {
        go(pauses, point);
    }
}

fn reset_pauses(pauses: &Path) {
    for e in std::fs::read_dir(pauses).unwrap().flatten() {
        std::fs::remove_file(e.path()).unwrap();
    }
}

/// Waits (bounded) for `cond`.
fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !cond() {
        assert!(Instant::now() < end, "{what}");
        thread::sleep(Duration::from_millis(20));
    }
}

// --- Strategies ------------------------------------------------------------------------------------

#[test]
fn a_merge_commit_prepared_aside_then_the_base_fast_forwarded() {
    if !has_git() {
        return;
    }
    let s = Shop::new("commit");
    s.main_moves_on();
    let mut agent = s.agent();
    let feat_before = branch_snapshot(&s.feat);
    let m = s.merge(MergeStrategy::Merge);
    let MergeOutcome::Merged { base_head, branch_head, fast_forward: false } = agent.merge(&m) else { panic!("merged") };
    assert_eq!((base_head.clone(), rev(&s.shop, "HEAD")), (rev(&s.shop, "refs/heads/main"), base_head));
    assert_eq!(branch_head, m.branch_head);
    assert_eq!(parents(&s.shop, "HEAD"), vec![m.base_head.clone(), m.branch_head.clone()]);
    assert_eq!(git(&s.shop, &["log", "-1", "--format=%an <%ae>|%s"]), "Ada Lovelace <ada@example.com>|Merge branch 'feat/invoices'");
    assert_eq!(git(&s.shop, &["status", "--porcelain", "--untracked-files=all"]), "", "B's files are the result");
    assert_eq!(read(&s.shop.join("invoice.txt")), "pdf\n");
    assert_eq!(branch_snapshot(&s.feat), feat_before, "the branch's worktree is never written");
    s.assert_clean_aside();
    agent.wait_for(
        "the branch merged in the worktree list",
        |ev| matches!(ev, AgentEvent::Worktrees(l) if l.worktrees.iter().any(|w| w.branch.as_deref() == Some("feat/invoices") && w.merged == Some(true))),
    );
    assert_eq!(refused(agent.merge(&s.merge(MergeStrategy::Merge))), MergeRefusal::NotAhead);
}

#[test]
fn fast_forward_and_rebase_give_the_head_itself_when_the_base_did_not_move() {
    if !has_git() {
        return;
    }
    for strategy in [MergeStrategy::FastForward, MergeStrategy::Rebase] {
        let s = Shop::new(&format!("ff-{strategy:?}"));
        let mut agent = s.agent();
        let m = s.merge(strategy);
        let MergeOutcome::Merged { base_head, fast_forward: true, .. } = agent.merge(&m) else { panic!("fast-forwarded") };
        assert_eq!((base_head, rev(&s.shop, "HEAD")), (m.branch_head.clone(), m.branch_head.clone()), "{strategy:?}");
        s.assert_clean_aside();
    }
    // The base moved: a merge commit.
    let s = Shop::new("ff-moved");
    s.main_moves_on();
    let mut agent = s.agent();
    let m = s.merge(MergeStrategy::FastForward);
    let MergeOutcome::Merged { fast_forward: false, .. } = agent.merge(&m) else { panic!("merged") };
    assert_eq!(parents(&s.shop, "HEAD"), vec![m.base_head, m.branch_head]);
}

#[test]
fn a_squash_makes_one_commit_with_the_message_kept_as_written() {
    if !has_git() {
        return;
    }
    let s = Shop::new("squash");
    s.main_moves_on();
    let mut agent = s.agent();
    let mut m = s.merge(MergeStrategy::Squash);
    m.message = Some("Invoices as PDF\n\n# not a comment: kept\n".into());
    let feat_before = branch_snapshot(&s.feat);
    let MergeOutcome::Merged { base_head, branch_head, fast_forward: false } = agent.merge(&m) else { panic!("merged") };
    assert_eq!((base_head, branch_head.clone()), (rev(&s.shop, "HEAD"), m.branch_head.clone()));
    assert_eq!(rev(&s.feat, "HEAD"), branch_head, "the branch keeps its own commits");
    assert_eq!(parents(&s.shop, "HEAD"), vec![m.base_head.clone()]);
    assert_eq!(git(&s.shop, &["log", "-1", "--format=%B"]), "Invoices as PDF\n\n# not a comment: kept");
    assert_eq!(git(&s.shop, &["log", "-1", "--format=%an <%ae>|%cn <%ce>"]), "Ada Lovelace <ada@example.com>|Ada Lovelace <ada@example.com>");
    assert_eq!(git(&s.shop, &["diff", "--name-only", "HEAD~1", "HEAD"]), "a.txt\ninvoice.txt");
    assert_eq!(branch_snapshot(&s.feat), feat_before);
    s.assert_clean_aside();
    let leftover = std::fs::read_dir(s.home().join(".local/share/tcanvas/merge")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("message-")).count();
    assert_eq!(leftover, 0, "the message file is removed");
    let mut empty = s.merge(MergeStrategy::Squash);
    empty.message = Some("  \n".into());
    assert_eq!(refused(agent.merge(&empty)), MergeRefusal::MessageEmpty);
}

#[test]
fn a_rebase_is_replayed_aside_and_checked_against_a_real_merge() {
    if !has_git() {
        return;
    }
    let s = Shop::new("rebase");
    s.main_moves_on();
    // The duplicate-block case: whatever backend the user configured, the merge backend runs,
    // and the result must have a merge's tree.
    git(&s.shop, &["config", "rebase.backend", "apply"]);
    let mut agent = s.agent();
    let feat_before = branch_snapshot(&s.feat);
    let m = s.merge(MergeStrategy::Rebase);
    let MergeOutcome::Merged { base_head, branch_head, fast_forward: false } = agent.merge(&m) else { panic!("merged") };
    assert_eq!(branch_head, m.branch_head, "the branch is not rewritten");
    assert_eq!(rev(&s.shop, "HEAD"), base_head);
    assert_eq!(git(&s.shop, &["log", "--format=%s", "-4"]), "edit a\nadd invoices\nmain moves on\ninit", "linear, on top of main");
    assert_eq!(git(&s.shop, &["log", "-1", "--format=%an|%cn"]), "Setup|Ada Lovelace");
    assert_eq!(branch_snapshot(&s.feat), feat_before);
    // The tree is the one a merge of the reviewed head gives.
    let check = s.tmp.0.join("check");
    git(&s.shop, &["worktree", "add", "-q", "--detach", check.to_str().unwrap(), &m.base_head]);
    git(&check, &["merge", "-q", "--no-commit", "--no-ff", &m.branch_head]);
    assert_eq!(git(&check, &["write-tree"]), rev(&s.shop, "HEAD^{tree}"));
    s.assert_clean_aside();
}

#[test]
fn a_branch_with_merge_commits_is_never_rebased() {
    if !has_git() {
        return;
    }
    let s = Shop::new("evil");
    s.conflicting();
    let (merged, _) = git_out(&s.feat, &["merge", "-q", "main"]);
    assert!(!merged, "a conflict to resolve");
    write(&s.feat.join("shared.txt"), "resolved\n");
    write(&s.feat.join("a.txt"), "fixed while merging\n");
    git(&s.feat, &["add", "-A"]);
    git(&s.feat, &["commit", "-q", "--no-edit"]);
    write(&s.shop.join("later.txt"), "later\n");
    commit_all(&s.shop, "main moves on");
    let mut agent = s.agent();
    let before = s.snap();
    assert_eq!(refused(agent.merge(&s.merge(MergeStrategy::Rebase))), MergeRefusal::RebaseMerges);
    assert_eq!(s.snap(), before);
    assert!(matches!(agent.merge(&s.merge(MergeStrategy::Merge)), MergeOutcome::Merged { .. }));
    assert_eq!(read(&s.shop.join("a.txt")), "fixed while merging\n");
}

// --- Nothing of the user's is touched unless the result is applied ----------------------------------

#[test]
fn each_conflict_is_found_aside_and_leaves_nothing() {
    if !has_git() {
        return;
    }
    for strategy in [MergeStrategy::Merge, MergeStrategy::Squash, MergeStrategy::Rebase] {
        let s = Shop::new(&format!("c-{strategy:?}"));
        s.conflicting();
        let mut agent = s.agent();
        let before = s.snap();
        assert_eq!(agent.merge(&s.merge(strategy)), MergeOutcome::Conflicted { files: vec!["shared.txt".into()], more: 0 }, "{strategy:?}");
        assert_eq!(s.snap(), before, "{strategy:?}: both worktrees byte for byte, every ref");
        s.assert_clean_aside();
    }
}

#[test]
fn untracked_or_ignored_files_where_the_result_writes_are_never_touched() {
    if !has_git() {
        return;
    }
    // The branch tracks a `.env` the base ignores, where the user keeps a secret.
    let s = Shop::new("env");
    write(&s.shop.join(".gitignore"), ".env\n");
    commit_all(&s.shop, "ignore .env");
    write(&s.shop.join(".env"), "SECRET=mine\n");
    write(&s.feat.join(".env"), "SECRET=branch\n");
    git(&s.feat, &["add", "-f", ".env"]);
    commit_all(&s.feat, "the branch's .env");
    let mut agent = s.agent();
    for strategy in [MergeStrategy::Merge, MergeStrategy::Squash, MergeStrategy::Rebase] {
        let before = s.snap();
        assert_eq!(agent.merge(&s.merge(strategy)), MergeOutcome::NotApplied { why: NotApplied::Collisions { files: vec![".env".into()], more: 0 } }, "{strategy:?}");
        assert_eq!(s.snap(), before, "{strategy:?}");
        assert_eq!(read(&s.shop.join(".env")), "SECRET=mine\n");
        s.assert_clean_aside();
    }
    // Directory renames: main moves `olddir/` to `newdir/`, the branch adds `olddir/new.txt`, and
    // the user has an ignored `newdir/new.txt`. Whatever git decides, the file stays.
    let s = Shop::new("dir-rename");
    write(&s.shop.join("olddir/kept.txt"), "kept\n");
    write(&s.shop.join("olddir/other.txt"), "other\n");
    commit_all(&s.shop, "olddir");
    git(&s.feat, &["merge", "-q", "main"]);
    write(&s.feat.join("olddir/new.txt"), "from the branch\n");
    commit_all(&s.feat, "a new file in olddir");
    git(&s.shop, &["mv", "olddir", "newdir"]);
    write(&s.shop.join(".gitignore"), "newdir/new.txt\n");
    commit_all(&s.shop, "olddir becomes newdir");
    write(&s.shop.join("newdir/new.txt"), "the user's own\n");
    let mut agent = s.agent();
    let before = s.snap();
    match agent.merge(&s.merge(MergeStrategy::Merge)) {
        MergeOutcome::Conflicted { .. } => {}
        MergeOutcome::NotApplied { why: NotApplied::Collisions { files, .. } } => assert_eq!(files, vec!["newdir/new.txt".to_string()]),
        other => panic!("{other:?}"),
    }
    assert_eq!(s.snap(), before);
    assert_eq!(read(&s.shop.join("newdir/new.txt")), "the user's own\n");
    s.assert_clean_aside();
}

#[test]
fn a_folder_replaced_by_a_file_counts_only_what_git_does_not_track() {
    if !has_git() {
        return;
    }
    let s = Shop::new("dir-file");
    write(&s.shop.join("docs/a.md"), "a\n");
    commit_all(&s.shop, "docs folder");
    git(&s.feat, &["merge", "-q", "main"]);
    git(&s.feat, &["rm", "-q", "-r", "docs"]);
    write(&s.feat.join("docs"), "now a file\n");
    commit_all(&s.feat, "docs becomes a file");
    let mut agent = s.agent();
    // An untracked note inside the folder: it would be lost.
    write(&s.shop.join("docs/notes.txt"), "mine\n");
    let before = s.snap();
    assert_eq!(agent.merge(&s.merge(MergeStrategy::Merge)), MergeOutcome::NotApplied { why: NotApplied::Collisions { files: vec!["docs/notes.txt".into()], more: 0 } });
    assert_eq!(s.snap(), before);
    // Only tracked files there: no false collision.
    std::fs::remove_file(s.shop.join("docs/notes.txt")).unwrap();
    assert!(matches!(agent.merge(&s.merge(MergeStrategy::Merge)), MergeOutcome::Merged { .. }));
    assert_eq!(read(&s.shop.join("docs")), "now a file\n");
}

#[test]
fn the_users_changes_in_the_base_meanwhile_survive_or_stop_the_apply() {
    if !has_git() {
        return;
    }
    let s = Shop::new("staged");
    s.main_moves_on();
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "checked");
    go(&pauses, "applied");
    // Staged and edited again, at a path the result does not write: it survives the merge.
    let id = agent.ask(&s.merge(MergeStrategy::Merge));
    reached(&pauses, "prepared");
    write(&s.shop.join("shared.txt"), "staged\n");
    git(&s.shop, &["add", "shared.txt"]);
    write(&s.shop.join("shared.txt"), "staged, then edited\n");
    go(&pauses, "prepared");
    assert!(matches!(agent.answer(id), MergeOutcome::Merged { .. }));
    assert_eq!(read(&s.shop.join("shared.txt")), "staged, then edited\n");
    assert!(git(&s.shop, &["diff", "--cached", "--name-only"]).contains("shared.txt"), "still staged");
    git(&s.shop, &["checkout", "HEAD", "--", "shared.txt"]);
    // At a path the result writes: git refuses, nothing changes.
    let s = Shop::new("staged-2");
    s.main_moves_on();
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "checked");
    go(&pauses, "applied");
    let id = agent.ask(&s.merge(MergeStrategy::Merge));
    reached(&pauses, "prepared");
    write(&s.shop.join("a.txt"), "the user's\n");
    git(&s.shop, &["add", "a.txt"]);
    let before = s.snap();
    go(&pauses, "prepared");
    assert!(matches!(agent.answer(id), MergeOutcome::NotApplied { why: NotApplied::DryRun { .. } }));
    assert_eq!(s.snap(), before);
    s.assert_clean_aside();
    // Their own merge started in the base meanwhile: refused, untouched.
    git(&s.shop, &["reset", "-q", "--hard"]);
    let other = s.tmp.0.join("other");
    git(&s.shop, &["worktree", "add", "-q", "-b", "other", other.to_str().unwrap()]);
    write(&other.join("other.txt"), "theirs\n");
    commit_all(&other, "other side");
    git(&s.shop, &["worktree", "remove", other.to_str().unwrap()]);
    reset_pauses(&pauses);
    go(&pauses, "checked");
    go(&pauses, "applied");
    let id = agent.ask(&s.merge(MergeStrategy::Merge));
    reached(&pauses, "prepared");
    git(&s.shop, &["merge", "-q", "--no-commit", "--no-ff", "other"]);
    write(&s.shop.join("other.txt"), "mine, being resolved\n");
    let before = s.snap();
    go(&pauses, "prepared");
    assert!(matches!(agent.answer(id), MergeOutcome::NotApplied { .. }));
    assert_eq!(s.snap(), before, "their merge, their edit");
    // The base switched to another branch meanwhile: nothing applied there either.
    git(&s.shop, &["reset", "-q", "--hard"]);
    reset_pauses(&pauses);
    go(&pauses, "checked");
    go(&pauses, "applied");
    let m = s.merge(MergeStrategy::Merge);
    let id = agent.ask(&m);
    reached(&pauses, "prepared");
    git(&s.shop, &["switch", "-q", "-c", "hotfix"]);
    let before = s.snap();
    go(&pauses, "prepared");
    assert_eq!(agent.answer(id), MergeOutcome::NotApplied { why: NotApplied::BaseMoved { found: Some("hotfix".into()), head: Some(m.base_head.clone()) } });
    assert_eq!(s.snap(), before);
    assert_eq!(rev(&s.shop, "refs/heads/main"), m.base_head);
}

#[test]
fn a_failing_slow_hook_aside_leaves_the_users_staged_work_alone() {
    if !has_git() {
        return;
    }
    let s = Shop::new("slow-fail");
    s.main_moves_on();
    let go_file = s.tmp.0.join("go");
    hook(&s.shop, "pre-merge-commit", &slow_hook(&go_file, "slow hook says no", 1));
    let mut agent = s.agent_with(&[("TCANVAS_WRITE_LIMIT_SECS", "1")]);
    let m = s.merge(MergeStrategy::Merge);
    let id = agent.ask(&m);
    assert_eq!(agent.answer(id), MergeOutcome::StillRunning { secs: 1 });
    assert_eq!(refused(agent.merge(&m)), MergeRefusal::Busy);
    // Meanwhile the user stages work in both worktrees.
    write(&s.shop.join("shared.txt"), "staged in main\n");
    git(&s.shop, &["add", "shared.txt"]);
    write(&s.feat.join("notes.txt"), "draft\n");
    let before = s.snap();
    write(&go_file, "");
    match agent.answer(id) {
        MergeOutcome::Failed { problem: Problem::MergeStepFailed { step: MergeStep::Merge, detail } } => assert!(detail.contains("slow hook says no"), "{detail}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(s.snap(), before);
    s.assert_clean_aside();
}

#[test]
fn the_branchs_worktree_is_never_written_whatever_the_user_does_there() {
    if !has_git() {
        return;
    }
    // A rebase conflicting aside while the user edits the branch's worktree.
    let s = Shop::new("w-edit");
    s.conflicting();
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "prepared");
    go(&pauses, "applied");
    let id = agent.ask(&s.merge(MergeStrategy::Rebase));
    reached(&pauses, "checked");
    write(&s.feat.join("a.txt"), "being edited\n");
    git(&s.feat, &["add", "a.txt"]);
    write(&s.feat.join("a.txt"), "being edited, more\n");
    let before = s.snap();
    go(&pauses, "checked");
    assert_eq!(agent.answer(id), MergeOutcome::Conflicted { files: vec!["shared.txt".into()], more: 0 });
    assert_eq!(s.snap(), before);
    s.assert_clean_aside();
    // The branch's worktree switched to another branch right before: the reviewed commits are
    // what is merged; the other branch is never rewritten.
    let s = Shop::new("w-switch");
    s.main_moves_on();
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "prepared");
    go(&pauses, "applied");
    let m = s.merge(MergeStrategy::Rebase);
    let id = agent.ask(&m);
    reached(&pauses, "checked");
    git(&s.feat, &["switch", "-q", "-c", "elsewhere"]);
    write(&s.feat.join("x.txt"), "x\n");
    commit_all(&s.feat, "elsewhere");
    let elsewhere = rev(&s.feat, "refs/heads/elsewhere");
    let feat_before = branch_snapshot(&s.feat);
    go(&pauses, "checked");
    assert!(matches!(agent.answer(id), MergeOutcome::Merged { .. }));
    assert_eq!((rev(&s.feat, "refs/heads/elsewhere"), rev(&s.feat, "refs/heads/feat/invoices")), (elsewhere, m.branch_head.clone()));
    assert_eq!(branch_snapshot(&s.feat), feat_before);
    assert!(!s.shop.join("x.txt").exists(), "only what was reviewed");
}

#[test]
fn what_is_merged_is_the_commit_reviewed() {
    if !has_git() {
        return;
    }
    let s = Shop::new("race");
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "prepared");
    go(&pauses, "applied");
    let m = s.merge(MergeStrategy::Merge);
    let id = agent.ask(&m);
    reached(&pauses, "checked");
    write(&s.feat.join("late.txt"), "not reviewed\n");
    commit_all(&s.feat, "late");
    go(&pauses, "checked");
    assert!(matches!(agent.answer(id), MergeOutcome::Merged { .. }));
    assert_eq!(parents(&s.shop, "HEAD"), vec![m.base_head, m.branch_head]);
    assert!(!s.shop.join("late.txt").exists());
    // A tag named like the branch changes nothing: shas are merged.
    reset_pauses(&pauses);
    go_all(&pauses);
    git(&s.feat, &["tag", "feat/invoices", "HEAD~1"]);
    let m = s.merge(MergeStrategy::Merge);
    assert!(matches!(agent.merge(&m), MergeOutcome::Merged { .. }));
    assert_eq!(parents(&s.shop, "HEAD")[1], m.branch_head);
}

// --- Refusals, hooks, identity -------------------------------------------------------------------------

#[test]
fn refusals_before_anything_is_done() {
    if !has_git() {
        return;
    }
    let s = Shop::new("refuse");
    let mut agent = s.agent();
    let m = s.merge(MergeStrategy::Merge);
    git(&s.shop, &["branch", "develop"]);
    assert_eq!(refused(agent.merge(&Merge { base: "develop".into(), ..m.clone() })), MergeRefusal::BaseNotCheckedOut { base: "develop".into() });
    git(&s.shop, &["update-ref", "refs/remotes/origin/main", &m.base_head]);
    assert_eq!(refused(agent.merge(&Merge { base: "origin/main".into(), ..m.clone() })), MergeRefusal::BaseNotLocal { base: "origin/main".into(), named: BaseKind::Remote });
    git(&s.shop, &["tag", "v1"]);
    assert_eq!(refused(agent.merge(&Merge { base: "v1".into(), ..m.clone() })), MergeRefusal::BaseNotLocal { base: "v1".into(), named: BaseKind::Other });
    assert_eq!(refused(agent.merge(&Merge { base: "nowhere".into(), ..m.clone() })), MergeRefusal::BaseNotFound { base: "nowhere".into() });
    assert_eq!(refused(agent.merge(&Merge { base: "feat/invoices".into(), ..m.clone() })), MergeRefusal::BaseIsBranch);
    assert!(matches!(agent.merge(&Merge { base: "--help".into(), ..m.clone() }), MergeOutcome::Failed { problem: Problem::ReviewBaseRefused { .. } }));
    assert_eq!(refused(agent.merge(&Merge { branch_head: "0".repeat(40), ..m.clone() })), MergeRefusal::Stale { branch_moved: true, base_moved: false });
    assert_eq!(refused(agent.merge(&Merge { base_head: "0".repeat(40), ..m.clone() })), MergeRefusal::Stale { branch_moved: false, base_moved: true });
    assert_eq!(refused(agent.merge(&Merge { branch: "feat/other".into(), ..m.clone() })), MergeRefusal::BranchChanged { expected: "feat/other".into(), found: Some("feat/invoices".into()) });
    let elsewhere = Merge { worktree: s.tmp.0.clone(), ..m.clone() };
    assert!(matches!(agent.merge(&elsewhere), MergeOutcome::Failed { problem: Problem::NotAWorktree { .. } }));
    let shown = s.shop.display().to_string();
    git(&s.shop, &["update-index", "--skip-worktree", "a.txt"]);
    assert_eq!(refused(agent.merge(&m)), MergeRefusal::HiddenChanges { path: shown.clone() });
    git(&s.shop, &["update-index", "--no-skip-worktree", "a.txt"]);
    git(&s.shop, &["bisect", "start"]);
    assert_eq!(refused(agent.merge(&m)), MergeRefusal::InProgress { path: shown.clone(), operation: GitOperation::Bisect });
    git(&s.shop, &["bisect", "reset"]);
    git(&s.shop, &["config", "core.sparseCheckout", "true"]);
    assert_eq!(refused(agent.merge(&m)), MergeRefusal::Sparse { path: shown });
    git(&s.shop, &["config", "core.sparseCheckout", "false"]);
    let empty = s.tmp.0.join("empty");
    git(&s.shop, &["worktree", "add", "-q", "-b", "feat/empty", empty.to_str().unwrap()]);
    assert_eq!(refused(agent.merge(&Merge { worktree: empty.clone(), branch: "feat/empty".into(), branch_head: rev(&empty, "HEAD"), ..m.clone() })), MergeRefusal::NotAhead);
    let lonely = s.tmp.0.join("lonely");
    git(&s.shop, &["worktree", "add", "-q", "--detach", lonely.to_str().unwrap()]);
    git(&lonely, &["checkout", "-q", "--orphan", "lonely"]);
    git(&lonely, &["rm", "-r", "-q", "--cached", "."]);
    for name in ["a.txt", "shared.txt"] {
        std::fs::remove_file(lonely.join(name)).unwrap();
    }
    write(&lonely.join("other.txt"), "other\n");
    commit_all(&lonely, "another history");
    let unrelated = Merge { worktree: lonely.clone(), branch: "lonely".into(), branch_head: rev(&lonely, "HEAD"), ..m.clone() };
    assert_eq!(refused(agent.merge(&unrelated)), MergeRefusal::Unrelated);
    git(&lonely, &["checkout", "-q", "--detach"]);
    assert_eq!(refused(agent.merge(&unrelated)), MergeRefusal::Detached);
    // Uncommitted work in the branch's worktree is not a reason (it is not part of the merge).
    write(&s.feat.join("a.txt"), "not committed\n");
    assert!(matches!(agent.merge(&m), MergeOutcome::Merged { .. }));
    s.assert_clean_aside();
}

#[test]
fn failing_hooks_and_signing_are_failures_with_their_output() {
    if !has_git() {
        return;
    }
    for (name, hook_name, strategy, step, said) in [
        ("hook-merge", "pre-merge-commit", MergeStrategy::Merge, MergeStep::Merge, "tests must pass"),
        ("hook-squash", "commit-msg", MergeStrategy::Squash, MergeStep::Commit, "needs a ticket number"),
        ("hook-rebase", "pre-rebase", MergeStrategy::Rebase, MergeStep::Rebase, "not this branch"),
        ("hook-prepare", "post-checkout", MergeStrategy::Merge, MergeStep::Prepare, "no checkout here"),
    ] {
        let s = Shop::new(name);
        s.main_moves_on();
        hook(&s.shop, hook_name, &format!("#!/bin/sh\necho '{hook_name}: {said}'\nexit 1\n"));
        let mut agent = s.agent();
        let before = s.snap();
        match agent.merge(&s.merge(strategy)) {
            MergeOutcome::Failed { problem: Problem::MergeStepFailed { step: got, detail } } => assert!(got == step && detail.contains(said), "{name}: {got:?} {detail}"),
            other => panic!("{name}: {other:?}"),
        }
        assert_eq!(s.snap(), before, "{name}");
        s.assert_clean_aside();
    }
    let s = Shop::new("gpg");
    let fake = s.tmp.0.join("fake-gpg.sh");
    write(&fake, "#!/bin/sh\necho 'fake gpg: no secret key' >&2\nexit 2\n");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&s.shop, &["config", "commit.gpgsign", "true"]);
    git(&s.shop, &["config", "gpg.program", fake.to_str().unwrap()]);
    let mut agent = s.agent();
    let before = s.snap();
    match agent.merge(&s.merge(MergeStrategy::Squash)) {
        MergeOutcome::Failed { problem: Problem::MergeStepFailed { step: MergeStep::Commit, detail } } => assert!(detail.contains("gpg"), "{detail}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(s.snap(), before);
    s.assert_clean_aside();
}

#[test]
fn no_identity_is_refused_and_never_made_up() {
    if !has_git() {
        return;
    }
    let s = Shop::with_identity("identity", false);
    s.main_moves_on();
    let mut agent = s.agent();
    let before = s.snap();
    for strategy in [MergeStrategy::Merge, MergeStrategy::Squash, MergeStrategy::Rebase] {
        assert_eq!(refused(agent.merge(&s.merge(strategy))), MergeRefusal::NoIdentity, "{strategy:?}");
    }
    assert_eq!(s.snap(), before);
    let s = Shop::with_identity("identity-ff", false);
    let mut agent = s.agent();
    assert!(matches!(agent.merge(&s.merge(MergeStrategy::FastForward)), MergeOutcome::Merged { fast_forward: true, .. }));
}

#[test]
fn stray_git_variables_never_send_the_merge_elsewhere() {
    if !has_git() {
        return;
    }
    let s = Shop::new("env-vars");
    let other = s.tmp.0.join("other");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q", "-b", "main"]);
    write(&other.join("x.txt"), "x\n");
    commit_all(&other, "other repository");
    let other_before = snapshot(&other);
    let git_dir = other.join(".git").display().to_string();
    let work_tree = other.display().to_string();
    let mut agent = s.agent_with(&[("GIT_DIR", &git_dir), ("GIT_WORK_TREE", &work_tree), ("GIT_COMMON_DIR", &git_dir)]);
    let m = s.merge(MergeStrategy::Merge);
    assert!(matches!(agent.merge(&m), MergeOutcome::Merged { .. }));
    assert_eq!(parents(&s.shop, "HEAD"), vec![m.base_head, m.branch_head]);
    assert_eq!(snapshot(&other), other_before, "the other repository untouched");
}

// --- The private worktree, the agent stopping, the records -------------------------------------------

#[test]
fn the_private_worktree_never_reaches_the_app() {
    if !has_git() {
        return;
    }
    let s = Shop::new("hidden");
    s.main_moves_on();
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "checked");
    go(&pauses, "applied");
    let id = agent.ask(&s.merge(MergeStrategy::Merge));
    reached(&pauses, "prepared");
    assert!(git(&s.shop, &["worktree", "list", "--porcelain"]).contains(".local/share/tcanvas/merge/work"), "it exists, for git");
    agent.send(&AgentCmd::RefreshWorktrees { key: "shop".into(), seq: 9 });
    agent.wait_for("a scan done", |ev| matches!(ev, AgentEvent::WorktreesRefreshed { seq: 9, .. }));
    go(&pauses, "prepared");
    assert!(matches!(agent.answer(id), MergeOutcome::Merged { .. }));
    for ev in agent.seen() {
        if let AgentEvent::Worktrees(list) = ev {
            assert!(list.worktrees.iter().all(|w| !w.path.contains(".local/share/tcanvas")), "{list:?}");
        }
    }
}

#[test]
fn a_closing_input_drops_a_merge_still_preparing_and_leaves_nothing() {
    if !has_git() {
        return;
    }
    let s = Shop::new("eof");
    s.main_moves_on();
    let go_file = s.tmp.0.join("go");
    hook(&s.shop, "pre-merge-commit", &slow_hook(&go_file, "no", 1));
    let mut agent = s.agent_with(&[("TCANVAS_WRITE_LIMIT_SECS", "1")]);
    let before = s.snap();
    let id = agent.ask(&s.merge(MergeStrategy::Merge));
    assert_eq!(agent.answer(id), MergeOutcome::StillRunning { secs: 1 });
    let closed = Instant::now();
    agent.stdin = None;
    until("the agent ends without waiting for the hook", || agent.child.try_wait().unwrap().is_some());
    assert!(closed.elapsed() < Duration::from_secs(10), "{:?}", closed.elapsed());
    assert_eq!(s.snap(), before);
    s.assert_clean_aside();
}

#[test]
fn a_killed_agent_is_reported_by_the_next_one_from_the_refs() {
    if !has_git() {
        return;
    }
    // Killed while preparing: the base did not move, the leftover is removed.
    let s = Shop::new("kill-prep");
    s.main_moves_on();
    let before = s.snap();
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "checked");
    let m = s.merge(MergeStrategy::Merge);
    let id = agent.ask(&m);
    reached(&pauses, "prepared");
    agent.child.kill().unwrap();
    let _ = agent.child.wait();
    assert_eq!(s.records(), 1);
    let next = s.agent();
    let AgentEvent::MergeInterrupted { record, previous_alive, applied, base_tip, .. } = next.wait_for("the interrupted merge", |ev| matches!(ev, AgentEvent::MergeInterrupted { .. })) else {
        unreachable!()
    };
    assert_eq!((record.id, previous_alive, applied, base_tip), (id, false, false, Some(m.base_head.clone())));
    assert_eq!(s.snap(), before);
    s.assert_clean_aside();
    // Killed right after applying: said as applied.
    let s = Shop::new("kill-apply");
    s.main_moves_on();
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "checked");
    go(&pauses, "prepared");
    let id = agent.ask(&s.merge(MergeStrategy::Merge));
    reached(&pauses, "applied");
    let result = rev(&s.shop, "refs/heads/main");
    agent.child.kill().unwrap();
    let _ = agent.child.wait();
    let next = s.agent();
    let AgentEvent::MergeInterrupted { record, previous_alive, applied, base_tip, .. } = next.wait_for("the interrupted merge", |ev| matches!(ev, AgentEvent::MergeInterrupted { .. })) else {
        unreachable!()
    };
    assert_eq!((record.id, record.result.as_deref(), previous_alive, applied, base_tip.as_deref()), (id, Some(result.as_str()), false, true, Some(result.as_str())));
    s.assert_clean_aside();
}

#[test]
fn an_older_agent_still_merging_is_waited_for() {
    if !has_git() {
        return;
    }
    let s = Shop::new("old-alive");
    s.main_moves_on();
    let (mut old, pauses) = s.paused_agent(&[]);
    go(&pauses, "checked");
    go(&pauses, "applied");
    let m = s.merge(MergeStrategy::Merge);
    let id = old.ask(&m);
    reached(&pauses, "prepared");
    let mut next = s.agent();
    let AgentEvent::MergeInterrupted { previous_alive, record, .. } = next.wait_for("the merge of the older agent", |ev| matches!(ev, AgentEvent::MergeInterrupted { .. })) else { unreachable!() };
    assert!(previous_alive);
    assert_eq!(refused(next.merge(&m)), MergeRefusal::PreviousAgent { pid: record.pid });
    go(&pauses, "prepared");
    assert!(matches!(old.answer(id), MergeOutcome::Merged { .. }));
    s.assert_clean_aside();
    // Once it ended, nothing is reported any more.
    let third = s.agent();
    assert!(!third.seen().iter().any(|ev| matches!(ev, AgentEvent::MergeInterrupted { .. })));
}

#[test]
fn a_corrupt_record_is_reported_and_moved_aside() {
    if !has_git() {
        return;
    }
    let s = Shop::new("corrupt");
    let dir = s.home().join(".local/share/tcanvas/merge/in-progress");
    write(&dir.join("1-2-3.json"), "{not json");
    let agent = s.agent();
    let AgentEvent::MergeRecordCorrupt { path, .. } = agent.wait_for("the corrupt record", |ev| matches!(ev, AgentEvent::MergeRecordCorrupt { .. })) else { unreachable!() };
    assert!(path.ends_with("1-2-3.corrupt") && Path::new(&path).exists() && !dir.join("1-2-3.json").exists(), "{path}");
}

#[test]
fn a_hanging_hook_is_stopped_at_the_ceiling() {
    if !has_git() {
        return;
    }
    let s = Shop::new("ceiling");
    s.main_moves_on();
    let go_file = s.tmp.0.join("go");
    hook(&s.shop, "pre-merge-commit", &slow_hook(&go_file, "late", 0));
    let mut agent = s.agent_with(&[("TCANVAS_WRITE_LIMIT_SECS", "1"), ("TCANVAS_MERGE_CEILING_SECS", "2")]);
    let before = s.snap();
    let m = s.merge(MergeStrategy::Merge);
    let id = agent.ask(&m);
    assert_eq!(agent.answer(id), MergeOutcome::StillRunning { secs: 1 });
    assert_eq!(agent.answer(id), MergeOutcome::Failed { problem: Problem::MergeTimedOut { secs: 2 } });
    assert_eq!(s.snap(), before);
    s.assert_clean_aside();
    // Not busy: the next merge goes.
    std::fs::remove_file(s.shop.join(".git/hooks/pre-merge-commit")).unwrap();
    assert!(matches!(agent.merge(&m), MergeOutcome::Merged { .. }));
}

// --- The final fast-forward ------------------------------------------------------------------------

/// A `reference-transaction` hook that acts on `refs/heads/main` moving (`action`: shell code).
fn on_main_moving(shop: &Path, action: &str) {
    hook(
        shop,
        "reference-transaction",
        &format!("#!/bin/sh\n[ \"$1\" = prepared ] || exit 0\nwhile read old new ref; do\n  if [ \"$ref\" = refs/heads/main ] && [ \"$old\" != \"$new\" ]; then\n{action}\n  fi\ndone\nexit 0\n"),
    );
}

fn orig_head(dir: &Path) -> Option<Vec<u8>> {
    std::fs::read(dir.join(git(dir, &["rev-parse", "--git-path", "ORIG_HEAD"]))).ok()
}

#[test]
fn a_lock_held_on_the_base_refuses_the_fast_forward_beforehand() {
    if !has_git() {
        return;
    }
    let s = Shop::new("lock");
    s.main_moves_on();
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "checked");
    go(&pauses, "applied");
    let id = agent.ask(&s.merge(MergeStrategy::Merge));
    reached(&pauses, "prepared");
    // A crashed git (or a concurrent commit) left the base's ref lock.
    write(&s.shop.join(".git/refs/heads/main.lock"), "");
    let before = s.snap();
    go(&pauses, "prepared");
    match agent.answer(id) {
        MergeOutcome::NotApplied { why: NotApplied::Locked { path } } => assert!(path.ends_with("refs/heads/main.lock"), "{path}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(s.snap(), before, "byte for byte, ORIG_HEAD included");
    s.assert_clean_aside();
}

#[test]
fn a_fast_forward_failing_half_way_is_said_as_it_is() {
    if !has_git() {
        return;
    }
    let s = Shop::new("half");
    s.main_moves_on();
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "checked");
    go(&pauses, "applied");
    let m = s.merge(MergeStrategy::Merge);
    let id = agent.ask(&m);
    reached(&pauses, "prepared");
    // The ref update is rejected after git wrote the index and the files.
    on_main_moving(&s.shop, "    echo 'main is frozen' >&2; exit 1");
    let orig_before = orig_head(&s.shop);
    go(&pauses, "prepared");
    let MergeOutcome::PartlyWritten { base, base_head, result, detail } = agent.answer(id) else { panic!("partly written") };
    assert_eq!((base.as_str(), base_head.as_str()), ("main", m.base_head.as_str()));
    assert!(detail.contains("main is frozen"), "{detail}");
    // Exactly the state reported: the branch and HEAD at the base, the index at the result.
    assert_eq!((rev(&s.shop, "HEAD"), rev(&s.shop, "refs/heads/main")), (m.base_head.clone(), m.base_head.clone()));
    assert!(git_out(&s.shop, &["diff-index", "--cached", "--quiet", &result]).0, "the index holds the result");
    assert_eq!(orig_head(&s.shop), orig_before, "ORIG_HEAD put back");
    s.assert_clean_aside();
}

#[test]
fn an_agent_killed_during_the_fast_forward_is_never_said_to_have_applied_nothing() {
    if !has_git() {
        return;
    }
    let s = Shop::new("kill-ff");
    s.main_moves_on();
    let started = s.tmp.0.join("ff-started");
    let go_file = s.tmp.0.join("ff-go");
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "checked");
    go(&pauses, "applied");
    let m = s.merge(MergeStrategy::Merge);
    agent.ask(&m);
    reached(&pauses, "prepared");
    // A slow hook while the base's ref moves: git is mid fast-forward when the agent is killed.
    on_main_moving(&s.shop, &format!("    touch '{}'; i=0; while [ ! -e '{}' ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done", started.display(), go_file.display()));
    go(&pauses, "prepared");
    until("git mid fast-forward", || started.exists());
    agent.child.kill().unwrap();
    let _ = agent.child.wait();
    // The next agent, at once: still running (its git is alive), and merges there wait.
    let mut next = s.agent();
    let AgentEvent::MergeInterrupted { record, previous_alive, .. } = next.wait_for("the merge still running", |ev| matches!(ev, AgentEvent::MergeInterrupted { .. })) else { unreachable!() };
    assert!(previous_alive && record.git_pid.is_some(), "{record:?}");
    assert_eq!(refused(next.merge(&m)), MergeRefusal::PreviousAgent { pid: record.pid });
    drop(next);
    write(&go_file, "");
    let result = record.result.clone().unwrap();
    until("the fast-forward ended", || rev(&s.shop, "refs/heads/main") == result);
    // Once it ended: applied, from the refs.
    let third = s.agent();
    let AgentEvent::MergeInterrupted { previous_alive, applied, record, .. } = third.wait_for("the merge applied", |ev| matches!(ev, AgentEvent::MergeInterrupted { .. })) else { unreachable!() };
    assert!(!previous_alive && applied && record.phase == protocol::MergePhase::Applying);
    s.assert_clean_aside();
}

#[test]
fn applied_means_the_result_is_in_the_base_even_after_more_commits() {
    if !has_git() {
        return;
    }
    let s = Shop::new("kill-then-commit");
    s.main_moves_on();
    let (mut agent, pauses) = s.paused_agent(&[]);
    go(&pauses, "checked");
    go(&pauses, "prepared");
    agent.ask(&s.merge(MergeStrategy::Merge));
    reached(&pauses, "applied");
    agent.child.kill().unwrap();
    let _ = agent.child.wait();
    // The user commits on the base before the next start.
    write(&s.shop.join("after.txt"), "after\n");
    commit_all(&s.shop, "after the merge");
    let next = s.agent();
    let AgentEvent::MergeInterrupted { applied, base_tip, base_checkout, .. } = next.wait_for("the interrupted merge", |ev| matches!(ev, AgentEvent::MergeInterrupted { .. })) else { unreachable!() };
    assert!(applied);
    assert_eq!((base_tip, base_checkout.as_deref()), (Some(rev(&s.shop, "HEAD")), Some("main")));
}

#[test]
fn a_hook_that_changes_the_content_is_never_applied() {
    if !has_git() {
        return;
    }
    let s = Shop::new("fmt-squash");
    s.main_moves_on();
    // A formatter in `pre-commit`: rewrites a file and stages it again.
    hook(&s.shop, "pre-commit", "#!/bin/sh\necho 'formatted' >> a.txt\ngit add a.txt\nexit 0\n");
    let mut agent = s.agent();
    let before = s.snap();
    assert_eq!(agent.merge(&s.merge(MergeStrategy::Squash)), MergeOutcome::Failed { problem: Problem::MergeHookChanged });
    assert_eq!(s.snap(), before);
    s.assert_clean_aside();
}

#[test]
fn a_branch_switched_in_the_last_window_is_said_with_the_branch_that_moved() {
    if !has_git() {
        return;
    }
    let s = Shop::new("last-window");
    s.main_moves_on();
    let (mut agent, pauses) = s.paused_agent(&[("TCANVAS_MERGE_TEST_PAUSE_SPAWN", "1")]);
    go(&pauses, "checked");
    go(&pauses, "prepared");
    go(&pauses, "applied");
    let m = s.merge(MergeStrategy::Merge);
    let id = agent.ask(&m);
    reached(&pauses, "spawn");
    git(&s.shop, &["switch", "-q", "-c", "hotfix"]);
    go(&pauses, "spawn");
    match agent.answer(id) {
        MergeOutcome::Unverified { base_branch, base_tip, moved } => {
            assert_eq!((base_branch.as_deref(), base_tip, moved.as_deref()), (Some("hotfix"), Some(m.base_head.clone()), Some("hotfix")));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rev(&s.shop, "refs/heads/main"), m.base_head, "main did not move");
    assert_eq!(parents(&s.shop, "refs/heads/hotfix"), vec![m.base_head, m.branch_head], "hotfix did");
}

#[test]
fn files_of_killed_agents_are_removed_at_start() {
    if !has_git() {
        return;
    }
    let s = Shop::new("stale");
    let folder = s.home().join(".local/share/tcanvas/merge");
    write(&folder.join("err-999999-1.txt"), "fatal: x\n");
    write(&folder.join("message-999999-2.txt"), "a private commit message\n");
    write(&folder.join("notes.txt"), "not ours to remove\n");
    let _agent = s.agent();
    assert!(!folder.join("err-999999-1.txt").exists() && !folder.join("message-999999-2.txt").exists());
    assert!(folder.join("notes.txt").exists());
}
