//! Worktree cleanup end to end: real `tcanvas-agent` binary on a throwaway git repository in a
//! temporary folder (never the repository under test), isolated from the user's git
//! configuration, and a throwaway tmux socket for the "a terminal is in it" refusal. Skipped
//! cleanly if git is not installed; the pane case is skipped if tmux is not.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{AgentCmd, AgentEvent, BranchKeepReason, Cleanup, Ignored, KeepReason, Problem, WorktreeList, WorktreeSize};

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

/// A throwaway tmux server, killed (and its socket file removed) on drop.
struct Tmux {
    socket: String,
}

impl Tmux {
    fn run(&self, args: &[&str]) -> bool {
        Command::new("tmux").arg("-L").arg(&self.socket).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        self.run(&["kill-server"]);
        use std::os::unix::fs::MetadataExt;
        let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
        let tmp = std::env::var("TMUX_TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let _ = std::fs::remove_file(format!("{tmp}/tmux-{uid}/{}", self.socket));
    }
}

struct Agent {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<AgentEvent>,
    backlog: RefCell<Vec<AgentEvent>>,
}

impl Agent {
    fn spawn(socket: &str) -> Self {
        Self::spawn_with(socket, &[])
    }

    fn spawn_with(socket: &str, env: &[(&str, String)]) -> Self {
        let mut child =
            Command::new(AGENT).env("TCANVAS_TMUX_SOCKET", socket).envs(GIT_ENV).envs(env.iter().map(|(k, v)| (*k, v.as_str()))).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
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

    /// Next event satisfying `pred`; others are kept for later waits.
    fn wait<T>(&self, what: &str, pick: impl Fn(&AgentEvent) -> Option<T>) -> T {
        let mut backlog = self.backlog.borrow_mut();
        if let Some((i, found)) = backlog.iter().enumerate().find_map(|(i, e)| pick(e).map(|f| (i, f))) {
            backlog.remove(i);
            return found;
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(ev) => match pick(&ev) {
                    Some(found) => return found,
                    None => backlog.push(ev),
                },
                Err(_) => break,
            }
        }
        let cleaned: Vec<&AgentEvent> = backlog.iter().filter(|e| matches!(e, AgentEvent::Cleaned { .. })).collect();
        panic!("expected: {what}\ncleanup answers received: {cleaned:#?}");
    }

    /// Sends a cleanup and returns its answer: (deleted branch, problem).
    fn cleanup(&mut self, key: &str, action: Cleanup) -> (Option<String>, Option<Problem>) {
        self.send(&AgentCmd::Cleanup { key: key.into(), action: action.clone() });
        self.wait(&format!("answer to {action:?}"), |e| match e {
            AgentEvent::Cleaned { key: k, action: a, deleted_branch, problem } if k == key && *a == action => Some((deleted_branch.clone(), problem.clone())),
            _ => None,
        })
    }

    fn wait_list(&self, key: &str, what: &str, pred: impl Fn(&WorktreeList) -> bool) -> WorktreeList {
        self.wait(what, |e| match e {
            AgentEvent::Worktrees(l) if l.key == key && pred(l) => Some(l.clone()),
            _ => None,
        })
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn has(cmd: &str, arg: &str) -> bool {
    Command::new(cmd).arg(arg).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
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

fn branch_exists(repo: &Path, branch: &str) -> bool {
    Command::new("git").arg("-C").arg(repo).args(["rev-parse", "--verify", "-q", &format!("refs/heads/{branch}")]).envs(GIT_ENV).stdout(Stdio::null()).status().unwrap().success()
}

fn commit(dir: &Path, file: &str, message: &str) {
    std::fs::write(dir.join(file), message).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// A linked worktree next to the repository, on a new branch with one commit.
fn worktree(repo: &Path, name: &str) -> PathBuf {
    let path = repo.parent().unwrap().join(name);
    git(repo, &["worktree", "add", "-q", "-b", name, path.to_str().unwrap()]);
    commit(&path, &format!("{name}.txt"), name);
    path
}

/// Same, then merged into `main` (fast-forward).
fn merged_worktree(repo: &Path, name: &str) -> PathBuf {
    let path = worktree(repo, name);
    git(repo, &["merge", "-q", "--ff-only", name]);
    path
}

/// A removal whose confirmation listed no ignored file.
fn remove(path: &Path, branch: Option<&str>) -> Cleanup {
    remove_seen(path, branch, Ignored::default())
}

fn remove_seen(path: &Path, branch: Option<&str>, ignored: Ignored) -> Cleanup {
    Cleanup::RemoveWorktree { path: path.display().to_string(), delete_branch: branch.map(String::from), ignored }
}

/// The agent's size answer for `path`.
fn measured(agent: &mut Agent, path: &Path) -> WorktreeSize {
    let path = path.display().to_string();
    agent.send(&AgentCmd::WorktreeSizes { key: "shop".into(), paths: vec![path.clone()] });
    agent.wait(&format!("size of {path}"), |e| match e {
        AgentEvent::WorktreeSizes { key, sizes } if key == "shop" && sizes[0].path == path => Some(sizes[0].clone()),
        _ => None,
    })
}

fn kept(path: &Path, reason: KeepReason) -> Option<Problem> {
    Some(Problem::WorktreeKept { path: path.display().to_string(), reason })
}

/// A process sitting in a folder, killed on drop.
struct Sitting(Child);

impl Drop for Sitting {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn removes_only_what_is_safe_and_refuses_the_rest() {
    if !has("git", "--version") {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("cleanup");
    let shop = tmp.0.join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    commit(&shop, ".gitignore", "target/\n.claude/\n.env\n");

    // Safe ones: merged and clean. `done1` also holds ignored files.
    let done1 = merged_worktree(&shop, "done1");
    std::fs::create_dir_all(done1.join("target")).unwrap();
    std::fs::write(done1.join("target/out.bin"), vec![0u8; 256 * 1024]).unwrap();
    std::fs::write(done1.join(".env"), "TOKEN=x").unwrap();
    let done2 = merged_worktree(&shop, "done2");
    let done3 = merged_worktree(&shop, "done3");
    // Merged, but with a change, an untracked file deep in a new folder, a lock, a terminal in
    // it, a process in it, a group's folder in it.
    let dirty = merged_worktree(&shop, "dirty");
    std::fs::write(dirty.join("dirty.txt"), "changed").unwrap();
    let untracked = merged_worktree(&shop, "untracked");
    std::fs::create_dir_all(untracked.join("notes/new")).unwrap();
    std::fs::write(untracked.join("notes/new/todo.md"), "todo").unwrap();
    let locked = merged_worktree(&shop, "locked");
    git(&shop, &["worktree", "lock", locked.to_str().unwrap()]);
    let busy = merged_worktree(&shop, "busy");
    let running = merged_worktree(&shop, "running");
    let grouped = merged_worktree(&shop, "grouped");
    // Merged and clean for git, but another worktree with uncommitted work lives in its ignored
    // `.claude/`, or a whole repository does.
    let outer = merged_worktree(&shop, "outer");
    let inner = outer.join(".claude/worktrees/inner");
    git(&shop, &["worktree", "add", "-q", "-b", "inner", inner.to_str().unwrap()]);
    std::fs::write(inner.join("work.txt"), "unsaved").unwrap();
    // Merged and clean for `git status`, but a tracked file flagged so that git does not look at
    // it was edited.
    let skipped = merged_worktree(&shop, "skipped");
    git(&skipped, &["update-index", "--skip-worktree", "skipped.txt"]);
    std::fs::write(skipped.join("skipped.txt"), "edited").unwrap();
    let assumed = merged_worktree(&shop, "assumed");
    git(&assumed, &["update-index", "--assume-unchanged", "assumed.txt"]);
    std::fs::write(assumed.join("assumed.txt"), "edited").unwrap();
    // Merged, but a reflog holds commits no branch has: made then reset away, or made on a
    // detached HEAD in the worktree.
    let rewound = merged_worktree(&shop, "rewound");
    commit(&rewound, "lost.txt", "lost");
    git(&rewound, &["reset", "-q", "--hard", "HEAD~1"]);
    let detour = merged_worktree(&shop, "detour");
    git(&detour, &["checkout", "-q", "--detach"]);
    commit(&detour, "detached.txt", "detached");
    git(&detour, &["checkout", "-q", "detour"]);
    // Its ignored files change after the user saw them.
    let changing = merged_worktree(&shop, "changing");
    let holder = merged_worktree(&shop, "holder");
    let scratch = holder.join(".claude/scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    git(&scratch, &["init", "-q"]);
    std::fs::write(scratch.join("notes.md"), "mine").unwrap();
    // Not merged; detached.
    let wip = worktree(&shop, "wip");
    let detached = tmp.0.join("detached");
    git(&shop, &["worktree", "add", "-q", "--detach", detached.to_str().unwrap(), "HEAD"]);
    // Folder deleted by hand: prunable.
    let gone = merged_worktree(&shop, "gone");
    std::fs::remove_dir_all(&gone).unwrap();
    // Branches without a worktree: `old` merged, `topic` not, `fresh` never moved.
    git(&shop, &["checkout", "-q", "-b", "old"]);
    commit(&shop, "old.txt", "old");
    git(&shop, &["checkout", "-q", "-b", "topic"]);
    commit(&shop, "topic.txt", "topic");
    git(&shop, &["checkout", "-q", "main"]);
    git(&shop, &["merge", "-q", "--ff-only", "old"]);
    git(&shop, &["branch", "fresh"]);
    // A branch without a worktree, merged, whose reflog holds a commit reset away.
    git(&shop, &["checkout", "-q", "-b", "lost"]);
    commit(&shop, "lost-a.txt", "lost a");
    git(&shop, &["checkout", "-q", "main"]);
    git(&shop, &["merge", "-q", "--ff-only", "lost"]);
    git(&shop, &["checkout", "-q", "lost"]);
    commit(&shop, "lost-b.txt", "lost b");
    git(&shop, &["reset", "-q", "--hard", "HEAD~1"]);
    git(&shop, &["checkout", "-q", "main"]);
    // A remote whose main is ahead of the local main: `pushed` and `pushed-away` are merged into
    // origin/main, not into the main checkout's HEAD, which `git branch -d` checks.
    let origin = tmp.0.join("origin.git");
    git(&tmp.0, &["init", "-q", "--bare", origin.to_str().unwrap()]);
    git(&shop, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&shop, &["push", "-q", "origin", "main"]);
    let pushed = worktree(&shop, "pushed");
    git(&shop, &["push", "-q", "origin", "pushed:main"]);
    git(&shop, &["branch", "pushed-away", "pushed"]);
    // Moved since created, like a branch that got commits.
    git(&shop, &["update-ref", "-m", "move", "refs/heads/pushed-away", "main"]);
    git(&shop, &["update-ref", "-m", "move", "refs/heads/pushed-away", "pushed"]);
    git(&shop, &["fetch", "-q", "origin"]);
    // Another repository, with a worktree of its own; and a bare one.
    let elsewhere = tmp.0.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    git(&elsewhere, &["init", "-q", "-b", "main"]);
    commit(&elsewhere, "a.txt", "a");
    let foreign = tmp.0.join("elsewhere-wt");
    git(&elsewhere, &["worktree", "add", "-q", "-b", "x", foreign.to_str().unwrap()]);
    let bare = tmp.0.join("bare.git");
    git(&tmp.0, &["clone", "-q", "--bare", elsewhere.to_str().unwrap(), bare.to_str().unwrap()]);
    let bare_wt = tmp.0.join("bare-wt");
    git(&bare, &["worktree", "add", "-q", "-b", "bw", bare_wt.to_str().unwrap()]);

    let tmux = Tmux { socket: format!("tcanvas-cleanup-test-{}", std::process::id()) };
    let pane = has("tmux", "-V") && tmux.run(&["new-session", "-d", "-s", "tc_busy", "-c", busy.to_str().unwrap()]);
    let _sitting = Sitting(Command::new("sleep").arg("60").current_dir(&running).spawn().unwrap());
    let mut agent = Agent::spawn(&tmux.socket);
    agent.send(&AgentCmd::Watch { key: "shop".into(), path: shop.display().to_string() });
    agent.send(&AgentCmd::Watch { key: "grouped".into(), path: grouped.display().to_string() });
    agent.send(&AgentCmd::Watch { key: "bare".into(), path: bare_wt.display().to_string() });

    // The scan lists the merged branches without a worktree, and only those.
    let list = agent.wait_list("shop", "merged branches", |l| !l.merged_branches.is_empty());
    let names: Vec<&str> = list.merged_branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, vec!["old"], "not main, topic (unmerged), fresh (never moved), pushed-away (git -d would refuse), lost (reflog), nor checked out ones");
    assert!(list.merged_branches[0].last_commit.is_some());
    assert_eq!(list.default_branch.as_deref(), Some("origin/main"));
    assert!(list.worktrees.iter().any(|w| w.prunable && w.path == gone.display().to_string()));
    let state = |p: &Path| list.worktrees.iter().find(|w| w.path == p.display().to_string()).map(|w| (w.merged, w.branch_deletable, w.reflog_commits)).unwrap();
    assert_eq!(state(&done2), (Some(true), true, false));
    assert_eq!(state(&pushed), (Some(true), false, false), "merged into origin/main only");
    assert_eq!(state(&rewound), (Some(true), true, true), "a commit reset away");
    assert_eq!(state(&detour), (Some(true), true, true), "a commit made on a detached HEAD there");

    // Sizes and ignored files: only the repository's worktrees.
    let done1_seen = measured(&mut agent, &done1);
    assert!(done1_seen.bytes.is_some_and(|b| b >= 256 * 1024), "{done1_seen:?}");
    let done1_seen = done1_seen.ignored.unwrap();
    assert_eq!(done1_seen, Ignored { shown: vec![".env".into(), "target/".into()], total: 2 });
    for path in [PathBuf::from("/etc"), gone.clone()] {
        let s = measured(&mut agent, &path);
        assert_eq!((s.bytes, s.ignored), (None, None), "not a worktree, folder gone");
    }

    // Refusals: nothing is touched.
    let b = Some;
    assert_eq!(agent.cleanup("shop", remove(&dirty, b("dirty"))), (None, kept(&dirty, KeepReason::Dirty)));
    assert_eq!(agent.cleanup("shop", remove(&untracked, b("untracked"))), (None, kept(&untracked, KeepReason::Dirty)), "untracked file in a new folder");
    assert_eq!(agent.cleanup("shop", remove(&wip, b("wip"))), (None, kept(&wip, KeepReason::NotMerged)));
    assert_eq!(agent.cleanup("shop", remove(&detached, None)), (None, kept(&detached, KeepReason::NotMerged)), "detached head");
    assert_eq!(agent.cleanup("shop", remove(&locked, b("locked"))), (None, kept(&locked, KeepReason::Locked)));
    assert_eq!(agent.cleanup("shop", remove(&shop, None)), (None, kept(&shop, KeepReason::MainCheckout)));
    assert_eq!(agent.cleanup("shop", remove(&grouped, b("grouped"))), (None, kept(&grouped, KeepReason::GroupFolder)));
    assert_eq!(agent.cleanup("shop", remove(&gone, b("gone"))), (None, kept(&gone, KeepReason::Missing)));
    assert_eq!(agent.cleanup("shop", remove(&outer, b("outer"))), (None, kept(&outer, KeepReason::HoldsWorktree)));
    assert_eq!(agent.cleanup("shop", remove(&holder, b("holder"))), (None, kept(&holder, KeepReason::HoldsRepository)));
    assert_eq!(agent.cleanup("shop", remove(&skipped, b("skipped"))), (None, kept(&skipped, KeepReason::HiddenChanges)), "skip-worktree");
    assert_eq!(agent.cleanup("shop", remove(&assumed, b("assumed"))), (None, kept(&assumed, KeepReason::HiddenChanges)), "assume-unchanged");
    assert_eq!(agent.cleanup("shop", remove(&rewound, None)), (None, kept(&rewound, KeepReason::ReflogCommits)));
    assert_eq!(agent.cleanup("shop", remove(&detour, None)), (None, kept(&detour, KeepReason::ReflogCommits)));
    // The user saw no ignored file; one appeared since.
    assert_eq!(measured(&mut agent, &changing).ignored, Some(Ignored::default()));
    std::fs::write(changing.join(".env"), "SECRET=1").unwrap();
    assert_eq!(agent.cleanup("shop", remove(&changing, b("changing"))), (None, Some(Problem::IgnoredChanged { path: changing.display().to_string() })));
    assert_eq!(agent.cleanup("shop", remove(&running, b("running"))), (None, kept(&running, KeepReason::ProcessInside)));
    if pane {
        assert_eq!(agent.cleanup("shop", remove(&busy, b("busy"))), (None, kept(&busy, KeepReason::HasTerminal)));
    } else {
        eprintln!("tmux missing: pane refusal skipped (unit-tested in cleanup.rs)");
    }
    assert_eq!(agent.cleanup("bare", remove(&bare, None)), (None, kept(&bare, KeepReason::MainCheckout)), "the bare repository itself");
    // The branch confirmed is not the worktree's.
    let changed = Problem::BranchChanged { path: done3.display().to_string(), expected: "done2".into(), found: Some("done3".into()) };
    assert_eq!(agent.cleanup("shop", remove(&done3, b("done2"))), (None, Some(changed)));
    // `git branch -d` would refuse its branch: refused before anything is removed.
    let behind = Problem::BranchKept { branch: "pushed".into(), reason: BranchKeepReason::NotMergedLocally };
    assert_eq!(agent.cleanup("shop", remove(&pushed, b("pushed"))), (None, Some(behind)));
    let other = tmp.0.join("other");
    std::fs::create_dir_all(&other).unwrap();
    for path in [other.clone(), shop.join("src"), foreign.clone(), PathBuf::from("/")] {
        assert_eq!(agent.cleanup("shop", remove(&path, None)), (None, Some(Problem::NotAWorktree { path: path.display().to_string() })));
    }
    assert!(other.is_dir() && foreign.is_dir());
    for (path, branch) in [
        (&dirty, "dirty"),
        (&untracked, "untracked"),
        (&wip, "wip"),
        (&locked, "locked"),
        (&busy, "busy"),
        (&running, "running"),
        (&grouped, "grouped"),
        (&outer, "outer"),
        (&holder, "holder"),
        (&skipped, "skipped"),
        (&assumed, "assumed"),
        (&rewound, "rewound"),
        (&detour, "detour"),
        (&changing, "changing"),
        (&done3, "done3"),
        (&pushed, "pushed"),
    ] {
        assert!(path.is_dir() && branch_exists(&shop, branch), "{branch} kept");
    }
    assert!(skipped.join("skipped.txt").exists() && changing.join(".env").exists());
    assert!(dirty.join("dirty.txt").exists() && untracked.join("notes/new/todo.md").exists() && inner.join("work.txt").exists() && scratch.join("notes.md").exists());
    let unknown = agent.cleanup("nobody", Cleanup::Prune { paths: vec![] });
    assert_eq!(unknown, (None, Some(Problem::NotAGitRepo)), "a key the agent does not watch");

    // Safe removal, branch kept: the folder goes, ignored files with it.
    assert_eq!(agent.cleanup("shop", remove(&done1, None)), (None, Some(Problem::IgnoredChanged { path: done1.display().to_string() })), "the user was not shown its ignored files");
    assert!(done1.join(".env").exists());
    assert_eq!(agent.cleanup("shop", remove_seen(&done1, None, done1_seen)), (None, None));
    assert!(!done1.exists(), "folder deleted, ignored files included");
    assert!(branch_exists(&shop, "done1"));
    // With its branch.
    assert_eq!(agent.cleanup("shop", remove(&done2, b("done2"))), (Some("done2".into()), None));
    assert!(!done2.exists() && !branch_exists(&shop, "done2"));
    // Without its branch when git -d would refuse it.
    assert_eq!(agent.cleanup("shop", remove(&pushed, None)), (None, None));
    // The branch deletion fails after the removal (its ref is locked): reported as such.
    let lock = shop.join(".git/refs/heads/done3.lock");
    std::fs::write(&lock, "").unwrap();
    let (deleted, problem) = agent.cleanup("shop", remove(&done3, b("done3")));
    std::fs::remove_file(&lock).unwrap();
    assert!(deleted.is_none() && matches!(&problem, Some(Problem::BranchNotDeleted { branch, detail }) if branch == "done3" && detail.contains("lock")), "{problem:?}");
    assert!(!done3.exists() && branch_exists(&shop, "done3"));
    let (p1, p2) = (done1.display().to_string(), done2.display().to_string());
    let list = agent.wait_list("shop", "the list follows", |l| l.worktrees.iter().all(|w| w.path != p1 && w.path != p2));
    // `done1` lost its worktree: now a merged branch without one; `pushed` is not offered.
    let names: Vec<&str> = list.merged_branches.iter().map(|b| b.name.as_str()).collect();
    assert!(names.contains(&"done1") && !names.contains(&"pushed"), "{names:?}");

    // Prune: only the confirmed entries; once done, nothing left to prune.
    let g = gone.display().to_string();
    assert_eq!(agent.cleanup("shop", Cleanup::Prune { paths: vec![] }), (None, Some(Problem::PruneNotConfirmed { path: g.clone() })));
    assert_eq!(agent.cleanup("shop", Cleanup::Prune { paths: vec![g.clone()] }), (None, None));
    agent.wait_list("shop", "pruned", |l| l.worktrees.iter().all(|w| w.path != g));
    assert_eq!(agent.cleanup("shop", Cleanup::Prune { paths: vec![g] }), (None, Some(Problem::NothingToPrune)));

    // Branches without a worktree.
    let delete = |b: &str| Cleanup::DeleteBranch { branch: b.into() };
    assert_eq!(agent.cleanup("shop", delete("old")), (None, None));
    assert!(!branch_exists(&shop, "old"));
    let refused = |b: &str, reason| (None, Some(Problem::BranchKept { branch: b.into(), reason }));
    assert_eq!(agent.cleanup("shop", delete("topic")), refused("topic", BranchKeepReason::NotMerged));
    assert_eq!(agent.cleanup("shop", delete("fresh")), refused("fresh", BranchKeepReason::NotMerged), "never moved: not offered");
    assert_eq!(agent.cleanup("shop", delete("pushed-away")), refused("pushed-away", BranchKeepReason::NotMergedLocally));
    assert_eq!(agent.cleanup("shop", delete("lost")), refused("lost", BranchKeepReason::ReflogCommits), "its reflog holds a commit reset away");
    assert_eq!(agent.cleanup("shop", delete("main")), refused("main", BranchKeepReason::Default));
    assert_eq!(agent.cleanup("shop", delete("wip")), refused("wip", BranchKeepReason::CheckedOut));
    assert_eq!(agent.cleanup("shop", delete("old")), refused("old", BranchKeepReason::NotFound), "already deleted");
    assert_eq!(agent.cleanup("shop", delete("--force")), refused("--force", BranchKeepReason::NotFound), "never an option");
    for b in ["topic", "fresh", "main", "wip", "pushed", "pushed-away", "lost"] {
        assert!(branch_exists(&shop, b), "{b} kept");
    }
}

/// Every file under `dir` (links as their target), with its bytes, sorted by path.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let path = entry.path();
            let ty = entry.file_type().unwrap();
            let rel = path.strip_prefix(dir).unwrap().to_path_buf();
            if ty.is_dir() {
                out.push((rel, Vec::new()));
                stack.push(path);
            } else if ty.is_symlink() {
                out.push((rel, std::fs::read_link(&path).unwrap().into_os_string().into_encoded_bytes()));
            } else {
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn git_variables_in_the_environment_never_point_at_another_repository() {
    if !has("git", "--version") {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("cleanup-git-env");
    let shop = tmp.0.join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    commit(&shop, ".gitignore", ".claude/\n");
    let done = merged_worktree(&shop, "done");
    // Another repository, with a worktree and a merged branch of its own, that every variable
    // points at.
    let decoy = tmp.0.join("decoy");
    std::fs::create_dir_all(&decoy).unwrap();
    git(&decoy, &["init", "-q", "-b", "main"]);
    commit(&decoy, "a.txt", "decoy");
    let decoy_wt = merged_worktree(&decoy, "decoy-done");
    let before = (snapshot(&decoy), snapshot(&decoy_wt));
    let env = [
        ("GIT_DIR", decoy.join(".git").display().to_string()),
        ("GIT_WORK_TREE", decoy.display().to_string()),
        ("GIT_COMMON_DIR", decoy.join(".git").display().to_string()),
        ("GIT_CEILING_DIRECTORIES", tmp.0.display().to_string()),
        ("GIT_DISCOVERY_ACROSS_FILESYSTEM", "1".to_string()),
    ];
    let mut agent = Agent::spawn_with(&format!("tcanvas-cleanup-env-{}", std::process::id()), &env);
    agent.send(&AgentCmd::Watch { key: "shop".into(), path: shop.display().to_string() });
    // Lists the watched repository's worktrees only.
    let (shop_s, done_s) = (shop.display().to_string(), done.display().to_string());
    let list = agent.wait_list("shop", "the shop's worktrees", |l| l.worktrees.len() == 2 && l.worktrees.iter().any(|w| w.path == done_s && w.merged == Some(true) && w.dirty == Some(false)));
    assert_eq!(list.repo.as_deref(), Some(shop_s.as_str()));

    // Creates one there.
    let request = protocol::CreateWorktree { branch: protocol::WorktreeBranch::New { name: "feat".into(), base: "main".into() }, folder: "feat".into(), location: None, exclude: false };
    agent.send(&AgentCmd::CreateWorktree { id: 1, key: "shop".into(), request });
    let created = agent.wait("the creation", |e| match e {
        AgentEvent::WorktreeCreated { id: 1, path, problem, .. } => Some((path.clone(), problem.clone())),
        _ => None,
    });
    let feat = shop.join(".claude/worktrees/feat");
    assert_eq!(created, (Some(feat.display().to_string()), None));
    assert!(feat.join(".gitignore").exists() && branch_exists(&shop, "feat"));

    // Removes one there, with its branch.
    assert_eq!(agent.cleanup("shop", remove(&done, Some("done"))), (Some("done".into()), None));
    assert!(!done.exists() && !branch_exists(&shop, "done"));
    let feat_s = feat.display().to_string();
    agent.wait_list("shop", "the new list", |l| l.worktrees.len() == 2 && l.worktrees.iter().any(|w| w.path == feat_s));

    // The other repository is untouched, byte for byte.
    assert_eq!((snapshot(&decoy), snapshot(&decoy_wt)), before);
    assert!(branch_exists(&decoy, "decoy-done"));
}
