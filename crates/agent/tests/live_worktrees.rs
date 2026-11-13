//! Worktrees end to end: real `tcanvas-agent` binary, real throwaway git repository with linked
//! worktrees (merged, unmerged, dirty), no remote. Skipped cleanly if git is not installed.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{AgentCmd, AgentEvent, Worktree, WorktreeList};

const AGENT: &str = env!("CARGO_BIN_EXE_tcanvas-agent");

/// Git sees neither the user's nor the system's configuration (`init.defaultBranch`,
/// `status.showUntrackedFiles`...): the test and the agent behave the same everywhere.
const GIT_ENV: [(&str, &str); 2] = [("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")];

/// The temporary folder, removed even when an assertion fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tcanvas-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Canonical: git reports real paths.
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
    backlog: RefCell<Vec<WorktreeList>>,
}

impl Agent {
    fn spawn() -> Self {
        // Nonexistent tmux socket: no sessions, nothing gets touched.
        let mut child =
            Command::new(AGENT).env("TCANVAS_TMUX_SOCKET", format!("tcanvas-worktrees-test-{}", std::process::id())).envs(GIT_ENV).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, events) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(protocol::decode(&line).expect("valid JSON line"));
            }
        });
        Agent { child, stdin, events, backlog: RefCell::new(Vec::new()) }
    }

    fn send(&mut self, cmd: &AgentCmd) {
        self.stdin.write_all(protocol::encode(cmd).as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Next worktree list of `key` that satisfies `pred`. Lists of other keys received meanwhile
    /// are kept for later waits; earlier lists of `key` are superseded and dropped.
    fn wait_worktrees(&self, key: &str, what: &str, pred: impl Fn(&WorktreeList) -> bool) -> WorktreeList {
        let mut backlog = self.backlog.borrow_mut();
        if let Some(i) = backlog.iter().position(|l| l.key == key && pred(l)) {
            return backlog.remove(i);
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(AgentEvent::Worktrees(l)) if l.key == key && pred(&l) => return l,
                Ok(AgentEvent::Worktrees(l)) if l.key == key => seen.push(l),
                Ok(AgentEvent::Worktrees(l)) => backlog.push(l),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("expected: {what}\nreceived for {key}: {seen:#?}\nothers: {backlog:#?}");
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

fn commit(dir: &Path, file: &str, message: &str) {
    std::fs::write(dir.join(file), message).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn find<'a>(list: &'a WorktreeList, path: &Path) -> &'a Worktree {
    let path = path.display().to_string();
    list.worktrees.iter().find(|w| w.path == path).unwrap_or_else(|| panic!("{path} missing: {list:#?}"))
}

#[test]
fn lists_worktrees_with_their_state_and_follows_changes_on_disk() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("worktrees");
    let shop = tmp.0.join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    // Nested worktrees would show as untracked in the main checkout otherwise.
    commit(&shop, ".gitignore", ".claude/worktrees/\n");

    // `done`: one commit, then merged into main. `my wip` (a path with a space): one commit
    // main does not have, main one it does not have, and an untracked file.
    let done = shop.join(".claude/worktrees/done");
    git(&shop, &["worktree", "add", "-q", "-b", "done", done.to_str().unwrap()]);
    let wip = tmp.0.join("my wip");
    git(&shop, &["worktree", "add", "-q", "-b", "wip", wip.to_str().unwrap()]);
    commit(&done, "done.txt", "done");
    commit(&wip, "wip.txt", "wip");
    git(&shop, &["merge", "-q", "--ff-only", "done"]);
    std::fs::write(wip.join("notes.txt"), "todo").unwrap();
    // `fresh`: just created from main, no commit of its own: not "merged".
    let fresh = tmp.0.join("fresh");
    git(&shop, &["worktree", "add", "-q", "-b", "fresh", fresh.to_str().unwrap()]);

    let mut agent = Agent::spawn();
    agent.send(&AgentCmd::Watch { key: "shop".into(), path: shop.display().to_string() });
    agent.send(&AgentCmd::Watch { key: "wip".into(), path: wip.display().to_string() });
    // The whole expectation is in the wait: dirty checks are staggered over a few passes.
    type State = (Option<String>, Option<bool>, Option<u32>, Option<u32>, Option<bool>);
    let state = |l: &WorktreeList, path: &Path| -> Option<State> {
        let w = l.worktrees.iter().find(|w| w.path == path.display().to_string())?;
        Some((w.branch.clone(), w.dirty, w.ahead, w.behind, w.merged))
    };
    let s = |branch: &str, dirty, ahead, behind, merged| Some((Some(branch.to_string()), Some(dirty), Some(ahead), Some(behind), merged));
    let complete = |l: &WorktreeList| {
        l.worktrees.len() == 4
            && state(l, &shop) == s("main", false, 0, 0, None)
            && state(l, &done) == s("done", false, 0, 0, Some(true))
            && state(l, &wip) == s("wip", true, 1, 1, Some(false))
            && state(l, &fresh) == s("fresh", false, 0, 0, None)
    };
    let list = agent.wait_worktrees("shop", "four worktrees with their state", complete);
    assert_eq!(list.repo.as_deref(), Some(shop.to_str().unwrap()));
    assert_eq!(list.default_branch.as_deref(), Some("main"), "no remote: main");
    assert_eq!(list.worktrees.iter().map(|w| w.main).collect::<Vec<_>>(), vec![true, false, false, false], "main checkout first");
    assert!(list.worktrees.iter().all(|w| w.last_commit.is_some_and(|t| t > 1_600_000_000) && w.head.is_some() && !w.locked && !w.prunable && !w.bare));

    // Watched from inside a linked worktree: the same repository, the same list.
    let from_wip = agent.wait_worktrees("wip", "same list from the linked worktree", |l| l.repo == list.repo && l.worktrees == list.worktrees);
    assert_eq!(from_wip.default_branch, list.default_branch);

    // Added outside the app, detached and locked: the list follows on its own. A scan may land
    // while `git worktree add` runs (locked "initializing", files not checked out yet) or
    // before the lock: wait for the final state.
    let extra = tmp.0.join("extra");
    git(&shop, &["worktree", "add", "-q", "--detach", extra.to_str().unwrap(), "HEAD"]);
    git(&shop, &["worktree", "lock", "--reason", "keep it", extra.to_str().unwrap()]);
    let extra_state = |l: &WorktreeList| {
        l.worktrees.len() == 5
            && l.worktrees.iter().any(|w| {
                w.path == extra.display().to_string()
                    && (w.detached, w.branch.as_deref(), w.merged, w.locked, w.lock_reason.as_deref(), w.dirty) == (true, None, None, true, Some("keep it"), Some(false))
            })
    };
    let list = agent.wait_worktrees("shop", "the new worktree, locked and clean", extra_state);
    assert!(!find(&list, &extra).main);
    git(&shop, &["worktree", "unlock", extra.to_str().unwrap()]);
    git(&shop, &["worktree", "remove", extra.to_str().unwrap()]);
    let gone = extra.display().to_string();
    agent.wait_worktrees("shop", "back to four", |l| l.worktrees.len() == 4 && l.worktrees.iter().all(|w| w.path != gone));

    // A folder outside git: an empty list, no error.
    let plain = tmp.0.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    agent.send(&AgentCmd::Watch { key: "plain".into(), path: plain.display().to_string() });
    let none = agent.wait_worktrees("plain", "nothing outside git", |_| true);
    assert_eq!((none.repo, none.default_branch, none.worktrees.len()), (None, None, 0));
}

/// Events until the refresh `seq` of `key` is answered: the worktree lists of `key` sent before it.
fn until_refreshed(agent: &Agent, key: &str, seq: u64) -> Vec<WorktreeList> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut lists = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match agent.events.recv_timeout(left) {
            Ok(AgentEvent::WorktreesRefreshed { key: k, seq: s }) if k == key && s == seq => return lists,
            Ok(AgentEvent::Worktrees(l)) if l.key == key => lists.push(l),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    panic!("refresh {seq} of {key} not answered; lists meanwhile: {lists:#?}");
}

#[test]
fn a_refresh_is_answered_after_a_new_scan_even_when_nothing_changed() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("worktrees-refresh");
    let repo = tmp.0.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "a", "first");
    let mut agent = Agent::spawn();
    agent.send(&AgentCmd::Watch { key: "r".into(), path: repo.display().to_string() });
    let first = agent.wait_worktrees("r", "the first list", |l| l.worktrees.len() == 1);
    // Nothing changed: no list, but an answer.
    agent.send(&AgentCmd::RefreshWorktrees { key: "r".into(), seq: 7 });
    assert!(until_refreshed(&agent, "r", 7).is_empty(), "nothing new to send");
    // A commit: the list showing it comes before the answer, whatever the scan period.
    commit(&repo, "b", "second");
    agent.send(&AgentCmd::RefreshWorktrees { key: "r".into(), seq: 8 });
    let lists = until_refreshed(&agent, "r", 8);
    let head = lists.last().and_then(|l| l.worktrees[0].head.clone());
    assert!(head.is_some() && head != first.worktrees[0].head, "{lists:#?}");
}
