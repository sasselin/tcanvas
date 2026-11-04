//! End-to-end change tracking: real `tcanvas-agent` binary, real temporary git repository.
//! Skipped cleanly if git is not installed.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{AgentCmd, AgentEvent, ChangeKind, ChangeSet, Problem};

const AGENT: &str = env!("CARGO_BIN_EXE_tcanvas-agent");

struct Agent {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<AgentEvent>,
    /// Events received while waiting for another one: kept for later waits.
    backlog: RefCell<Vec<AgentEvent>>,
}

impl Agent {
    fn spawn() -> Self {
        // Nonexistent tmux socket: no sessions, nothing gets touched.
        let mut child = Command::new(AGENT)
            .env("TCANVAS_TMUX_SOCKET", format!("tcanvas-changes-test-{}", std::process::id()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
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

    fn wait_for(&self, what: &str, pred: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        let mut backlog = self.backlog.borrow_mut();
        if let Some(i) = backlog.iter().position(&pred) {
            return backlog.remove(i);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(ev) if pred(&ev) => return ev,
                Ok(ev) => backlog.push(ev),
                Err(_) => break,
            }
        }
        panic!("expected: {what}\nreceived: {backlog:#?}");
    }

    fn wait_changes(&self, key: &str, what: &str, pred: impl Fn(&ChangeSet) -> bool) -> ChangeSet {
        match self.wait_for(what, |ev| matches!(ev, AgentEvent::Changes(s) if s.key == key && pred(s))) {
            AgentEvent::Changes(s) => s,
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

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tcanvas-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn lists_git_changes_and_serves_diffs() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let repo = temp_dir("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(repo.join("gone.txt"), "x\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let mut agent = Agent::spawn();
    agent.wait_for("Hello", |ev| matches!(ev, AgentEvent::Hello { .. }));
    agent.send(&AgentCmd::Watch { key: "p".into(), path: repo.display().to_string() });
    let clean = agent.wait_changes("p", "clean repo", |_| true);
    assert!(clean.git && clean.files.is_empty() && clean.error.is_none(), "{clean:?}");
    assert_eq!(clean.branch.as_deref(), Some("main"));
    // Clean repo: already committed work stays visible, with its contents.
    assert_eq!(clean.commits.iter().map(|c| (c.summary.as_str(), c.author.as_str())).collect::<Vec<_>>(), vec![("init", "t")]);
    let sha = clean.commits[0].sha.clone();
    agent.send(&AgentCmd::ShowCommit { key: "p".into(), sha: sha.clone() });
    let AgentEvent::Diff { text, .. } = agent.wait_for("commit contents", |ev| matches!(ev, AgentEvent::Diff { file, .. } if *file == sha)) else { unreachable!() };
    assert!(text.contains("init") && text.contains("Author: ") && text.contains("+two") && text.contains("gone.txt"), "{text}");
    agent.send(&AgentCmd::ShowCommit { key: "p".into(), sha: "--output=/tmp/x".into() });
    let AgentEvent::Diff { text, problem, .. } = agent.wait_for("sha refused", |ev| matches!(ev, AgentEvent::Diff { file, .. } if file.starts_with("--"))) else { unreachable!() };
    assert_eq!((text.as_str(), problem), ("", Some(Problem::CommitRefused)));

    // What Claude would do: modify, create, delete.
    std::fs::write(repo.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/new.rs"), "fn main() {}\n// end\n").unwrap();
    std::fs::remove_file(repo.join("gone.txt")).unwrap();

    let set = agent.wait_changes("p", "three changes", |s| s.files.len() == 3);
    let get = |p: &str| set.files.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("{p} missing: {set:?}"));
    let a = get("a.txt");
    assert_eq!((&a.kind, a.added, a.removed), (&ChangeKind::Modified, Some(2), Some(1)));
    let new = get("src/new.rs");
    assert_eq!((&new.kind, new.added, new.removed), (&ChangeKind::Untracked, Some(2), Some(0)));
    assert_eq!(get("gone.txt").kind, ChangeKind::Deleted);

    agent.send(&AgentCmd::Diff { key: "p".into(), file: "a.txt".into() });
    let AgentEvent::Diff { text, truncated, .. } = agent.wait_for("diff of a.txt", |ev| matches!(ev, AgentEvent::Diff { file, .. } if file == "a.txt")) else { unreachable!() };
    assert!(text.contains("-two") && text.contains("+TWO") && text.contains("+four") && !truncated, "{text}");

    agent.send(&AgentCmd::Diff { key: "p".into(), file: "src/new.rs".into() });
    let AgentEvent::Diff { text, .. } = agent.wait_for("diff of the untracked file", |ev| matches!(ev, AgentEvent::Diff { file, .. } if file == "src/new.rs")) else { unreachable!() };
    assert!(text.contains("+fn main() {}"), "{text}");

    // Unwatch: no more events for this key.
    agent.send(&AgentCmd::Unwatch { key: "p".into() });
    thread::sleep(Duration::from_millis(200));
    std::fs::write(repo.join("b.txt"), "b\n").unwrap();
    thread::sleep(Duration::from_millis(2500));
    assert!(agent.events.try_iter().all(|ev| !matches!(ev, AgentEvent::Changes(s) if s.files.len() == 4)));
    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn plain_folder_lists_recent_files_and_missing_folder_errors() {
    let dir = temp_dir("plain");
    std::fs::write(dir.join("old.txt"), "old").unwrap();
    // mtime has one-second resolution: wait for the next second before watching.
    thread::sleep(Duration::from_millis(1100));

    let mut agent = Agent::spawn();
    agent.send(&AgentCmd::Watch { key: "plain".into(), path: dir.display().to_string() });
    agent.send(&AgentCmd::Watch { key: "absent".into(), path: "/does/not/exist".into() });
    let err = agent.wait_changes("absent", "error", |s| s.error.is_some());
    assert!(matches!(err.error, Some(Problem::DirNotFound { .. })), "{err:?}");

    let empty = agent.wait_changes("plain", "nothing new", |_| true);
    assert!(!empty.git && empty.files.is_empty(), "{empty:?}");
    std::fs::create_dir_all(dir.join("node_modules")).unwrap();
    std::fs::write(dir.join("node_modules/ignored.js"), "x").unwrap();
    std::fs::write(dir.join("fresh.txt"), "new").unwrap();
    let set = agent.wait_changes("plain", "fresh.txt", |s| !s.files.is_empty());
    assert_eq!(set.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["fresh.txt"]);

    agent.send(&AgentCmd::Diff { key: "plain".into(), file: "fresh.txt".into() });
    let AgentEvent::Diff { problem, .. } = agent.wait_for("non-git message", |ev| matches!(ev, AgentEvent::Diff { .. })) else { unreachable!() };
    assert_eq!(problem, Some(Problem::NotAGitRepo));

    // Folder picker: the agent lists the subfolders.
    std::fs::create_dir_all(dir.join("subfolder")).unwrap();
    agent.send(&AgentCmd::ListDir { path: dir.display().to_string() });
    let AgentEvent::DirListing(l) = agent.wait_for("folder listing", |ev| matches!(ev, AgentEvent::DirListing(_))) else { unreachable!() };
    let names: Vec<&str> = l.dirs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"subfolder") && !l.git, "{l:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
