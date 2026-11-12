//! Task lists end to end: real `tcanvas-agent` binaries, a throwaway git repository with a linked
//! worktree, a folder outside git, and a scratch home (`TCANVAS_TEST_HOME`): nothing is written
//! under the real `~/.local/share/tcanvas`. Skipped cleanly if git is not installed.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{AgentCmd, AgentEvent, Priority, Problem, TaskBoard, TaskOp, TaskStatus};

const AGENT: &str = env!("CARGO_BIN_EXE_tcanvas-agent");
const GIT_ENV: [(&str, &str); 2] = [("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")];

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
    next_id: u64,
    /// Events read while waiting for others, kept for later waits.
    backlog: Vec<AgentEvent>,
}

impl Agent {
    fn spawn(home: &Path, name: &str) -> Self {
        Self::spawn_with(home, name, &[])
    }

    fn spawn_with(home: &Path, name: &str, env: &[(&str, String)]) -> Self {
        // Nonexistent tmux socket: no sessions, nothing gets touched.
        let mut child = Command::new(AGENT)
            .env("TCANVAS_TMUX_SOCKET", format!("tcanvas-tasks-test-{name}-{}", std::process::id()))
            .env("TCANVAS_TEST_HOME", home)
            .envs(GIT_ENV)
            .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
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
        let mut agent = Agent { child, stdin, events, next_id: 1, backlog: Vec::new() };
        agent.wait("Hello with task lists", |ev| matches!(ev, AgentEvent::Hello { tasks: true, .. }));
        agent
    }

    fn send(&mut self, cmd: &AgentCmd) {
        self.stdin.write_all(protocol::encode(cmd).as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn watch(&mut self, key: &str, path: &Path) {
        self.send(&AgentCmd::WatchTasks { key: key.into(), path: path.display().to_string() });
    }

    /// Sends a change without waiting: its request id.
    fn edit(&mut self, key: &str, op: TaskOp) -> u64 {
        self.edit_in(key, op, None)
    }

    /// A change naming the list the caller was shown.
    fn edit_in(&mut self, key: &str, op: TaskOp, list: Option<&str>) -> u64 {
        let id = self.next_id();
        self.send(&AgentCmd::EditTasks { id, key: key.into(), op, expect: None, list: list.map(String::from) });
        id
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The answer to request `id`: its problem.
    fn answer(&mut self, id: u64) -> Option<Problem> {
        match self.wait("an answer", |ev| matches!(ev, AgentEvent::TasksEdited { id: i, .. } if *i == id)) {
            AgentEvent::TasksEdited { problem, .. } => problem,
            _ => unreachable!(),
        }
    }

    /// Sends a change and waits for its answer: the task it concerns.
    fn edit_ok(&mut self, key: &str, op: TaskOp) -> String {
        let id = self.edit(key, op);
        match self.wait("the change's answer", |ev| matches!(ev, AgentEvent::TasksEdited { id: i, .. } if *i == id)) {
            AgentEvent::TasksEdited { problem: None, task: Some(task), .. } => task,
            other => panic!("refused: {other:?}"),
        }
    }

    fn wait(&mut self, what: &str, pred: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        if let Some(i) = self.backlog.iter().position(&pred) {
            return self.backlog.remove(i);
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(ev) if pred(&ev) => return ev,
                Ok(ev) => self.backlog.push(ev),
                Err(_) => break,
            }
        }
        let tasks: Vec<&AgentEvent> = self.backlog.iter().filter(|e| !matches!(e, AgentEvent::Error { .. })).collect();
        panic!("expected: {what}\nreceived: {tasks:#?}");
    }

    /// The next list of `key` that satisfies `pred` (earlier ones are dropped).
    fn board(&mut self, key: &str, what: &str, pred: impl Fn(&TaskBoard) -> bool) -> TaskBoard {
        let key = key.to_string();
        match self.wait(what, |ev| matches!(ev, AgentEvent::Tasks(b) if b.key == key && pred(b))) {
            AgentEvent::Tasks(b) => *b,
            _ => unreachable!(),
        }
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

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"]).args(args).envs(GIT_ENV).output().unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn titles(board: &TaskBoard) -> Vec<String> {
    board.list.as_ref().map(|l| l.tasks.iter().filter(|t| t.removed.is_none()).map(|t| t.title.clone()).collect()).unwrap_or_default()
}

fn add(title: &str) -> TaskOp {
    TaskOp::Add { title: title.into(), description: String::new(), priority: Priority::None, status: TaskStatus::Inbox }
}

#[test]
fn worktrees_of_one_repository_share_a_list_kept_outside_it() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("tasks-live");
    let home = tmp.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let shop = tmp.0.join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    std::fs::write(shop.join("README.md"), "shop").unwrap();
    git(&shop, &["add", "README.md"]);
    git(&shop, &["commit", "-q", "-m", "init"]);
    let taxes = tmp.0.join("shop-taxes");
    git(&shop, &["worktree", "add", "-q", "-b", "taxes", taxes.to_str().unwrap()]);
    let plain = tmp.0.join("notes");
    std::fs::create_dir_all(plain.join("sub")).unwrap();

    // Two groups on two worktrees of one repository (one on a subfolder), one folder outside git.
    let mut agent = Agent::spawn(&home, "a");
    agent.watch("main", &shop);
    agent.watch("taxes", &taxes);
    agent.watch("plain", &plain);
    for key in ["main", "taxes"] {
        let b = agent.board(key, "an empty list keyed by the main checkout", |_| true);
        let list = b.list.expect("a list");
        assert_eq!((list.key.as_str(), list.tasks.len(), b.read_only, b.problem), (shop.to_str().unwrap(), 0, false, None));
    }
    let b = agent.board("plain", "the plain folder's list", |_| true);
    assert_eq!(b.list.unwrap().key, plain.to_str().unwrap(), "keyed by itself");
    assert!(!home.join(".local").exists(), "subscribing writes nothing");

    // Added from the worktree's group: the main checkout's group sees it.
    let invoice = agent.edit_ok("taxes", add("Invoice PDF"));
    let seen = agent.board("main", "the task from the other group", |b| titles(b) == ["Invoice PDF"]);
    assert_eq!(seen.list.as_ref().unwrap().tasks[0].id, invoice);
    agent.board("taxes", "and its own group", |b| titles(b) == ["Invoice PDF"]);
    // ... and the other way around.
    let search = agent.edit_ok("main", add("Customer search"));
    agent.edit_ok("main", TaskOp::AddDependency { task: search.clone(), on: invoice.clone() });
    let b = agent.board("taxes", "the dependency", |b| b.list.as_ref().is_some_and(|l| l.tasks.iter().any(|t| t.depends_on == [invoice.clone()])));
    assert_eq!(titles(&b), ["Invoice PDF", "Customer search"]);
    // A loop is refused, with references the user knows.
    let id = agent.edit("taxes", TaskOp::AddDependency { task: invoice.clone(), on: search.clone() });
    let refs: HashMap<String, String> = b.list.as_ref().unwrap().tasks.iter().map(|t| (t.id.clone(), t.reference.clone())).collect();
    let answer = agent.wait("the refusal", |ev| matches!(ev, AgentEvent::TasksEdited { id: i, .. } if *i == id));
    assert!(matches!(answer, AgentEvent::TasksEdited { problem: Some(Problem::TaskCycle { task, on }), .. } if task == refs[&invoice] && on == refs[&search]));
    // Not subscribed: refused.
    let id = agent.edit("nobody", add("x"));
    let answer = agent.wait("not watched", |ev| matches!(ev, AgentEvent::TasksEdited { id: i, .. } if *i == id));
    assert!(matches!(answer, AgentEvent::TasksEdited { problem: Some(Problem::TasksNotWatched), .. }));

    // The plain folder's list is its own.
    agent.edit_ok("plain", add("Write the talk"));
    agent.board("plain", "its own task", |b| titles(b) == ["Write the talk"]);

    // Stored under the home, not in the repository: git sees nothing new.
    let dir = home.join(".local/share/tcanvas/tasks");
    let files: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".json")).collect();
    assert_eq!(files.len(), 2, "one list per key: {files:?}");
    assert!(files.iter().any(|f| f.starts_with("shop-")) && files.iter().any(|f| f.starts_with("notes-")), "{files:?}");
    assert_eq!(git(&shop, &["status", "--porcelain"]), "");
    assert_eq!(git(&taxes, &["status", "--porcelain"]), "");

    // Another agent process (as after a restart, or a second writer): same list, from a
    // subfolder of the worktree; its change reaches the first one through the file.
    let mut other = Agent::spawn(&home, "b");
    other.watch("sub", &taxes);
    let b = other.board("sub", "the list survives the agent", |b| titles(b).len() == 2);
    assert_eq!(b.list.unwrap().key, shop.to_str().unwrap());
    other.edit_ok("sub", add("Tax export"));
    agent.board("main", "a change made by another process", |b| titles(b).contains(&"Tax export".to_string()));

    // Both write at once: no change is lost.
    let (mut last_a, mut last_b) = (0, 0);
    for i in 0..30 {
        last_a = agent.edit("main", add(&format!("a{i}")));
        last_b = other.edit("sub", add(&format!("b{i}")));
    }
    for (a, last) in [(&mut agent, last_a), (&mut other, last_b)] {
        // Answers come in order: the last one says all are done.
        let answer = a.wait("the last answer", |ev| matches!(ev, AgentEvent::TasksEdited { id, .. } if *id == last));
        assert!(matches!(answer, AgentEvent::TasksEdited { problem: None, .. }), "{answer:?}");
    }
    drop(other);
    let b = agent.board("main", "every task of both writers", |b| titles(b).len() == 63);
    let list = b.list.unwrap();
    let mut refs: Vec<&str> = list.tasks.iter().map(|t| t.reference.as_str()).collect();
    refs.sort();
    refs.dedup();
    assert_eq!(refs.len(), 63, "references unique across writers");

    // A file that cannot be read is set aside, with a notice, and the list starts empty.
    let path = dir.join(files.iter().find(|f| f.starts_with("shop-")).unwrap());
    std::fs::write(&path, "{ broken").unwrap();
    let notice = agent.wait("the notice", |ev| matches!(ev, AgentEvent::TasksNotice { problem: Problem::TasksSetAside { .. }, .. }));
    let AgentEvent::TasksNotice { problem: Problem::TasksSetAside { path: aside, .. }, .. } = notice else { unreachable!() };
    assert_eq!(std::fs::read_to_string(&aside).unwrap(), "{ broken");
    agent.board("main", "an empty list", |b| b.list.as_ref().is_some_and(|l| l.tasks.is_empty()));
}

fn list_files(home: &Path) -> Vec<String> {
    let dir = home.join(".local/share/tcanvas/tasks");
    let mut names: Vec<String> = std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".json")).collect()).unwrap_or_default();
    names.sort();
    names
}

#[test]
fn a_git_failure_refuses_the_change_instead_of_writing_another_list() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("tasks-git-fails");
    let home = tmp.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let shop = tmp.0.join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    // A git that fails (not "not a repository") while a flag file exists.
    let real = String::from_utf8(Command::new("sh").args(["-c", "command -v git"]).output().unwrap().stdout).unwrap().trim().to_string();
    let bin = tmp.0.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let flag = tmp.0.join("fail");
    let script = format!("#!/bin/sh\nif [ -e '{}' ]; then echo 'fatal: unable to read the index' >&2; exit 128; fi\nexec '{real}' \"$@\"\n", flag.display());
    std::fs::write(bin.join("git"), script).unwrap();
    Command::new("chmod").arg("+x").arg(bin.join("git")).status().unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let mut agent = Agent::spawn_with(&home, "git-fails", &[("PATH", path)]);
    agent.watch("shop", &shop);
    agent.board("shop", "the list", |b| b.list.is_some());
    agent.edit_ok("shop", add("Before"));
    assert_eq!(list_files(&home).len(), 1);

    std::fs::write(&flag, "").unwrap();
    let id = agent.edit("shop", add("During"));
    assert!(matches!(agent.answer(id), Some(Problem::TasksKeyUnknown { detail }) if detail.contains("unable to read the index")));
    assert_eq!(list_files(&home).len(), 1, "nothing written to another list");
    agent.board("shop", "the failure shown", |b| matches!(b.problem, Some(Problem::TasksKeyUnknown { .. })));

    std::fs::remove_file(&flag).unwrap();
    // A change naming another list than the folder's now: refused.
    let id = agent.edit_in("shop", add("Elsewhere"), Some("/somewhere/else"));
    assert!(matches!(agent.answer(id), Some(Problem::TasksListChanged { now, .. }) if now == shop.to_str().unwrap()));
    let id = agent.edit_in("shop", add("After"), Some(shop.to_str().unwrap()));
    assert_eq!(agent.answer(id), None);
    agent.board("shop", "the list again", |b| titles(b) == ["Before", "After"]);
}

#[test]
fn a_folder_that_joins_a_repository_finds_its_old_list() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("tasks-rekey");
    let home = tmp.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let outer = tmp.0.join("outer");
    let notes = outer.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    let mut agent = Agent::spawn(&home, "rekey");
    agent.watch("notes", &notes);
    agent.board("notes", "its own list", |b| b.list.as_ref().is_some_and(|l| l.key == notes.to_str().unwrap()));
    agent.edit_ok("notes", add("Outline"));
    agent.edit_ok("notes", add("Slides"));

    // A repository is made around it: its key moves to the repository, and the old list is named.
    git(&outer, &["init", "-q", "-b", "main"]);
    agent.watch("notes", &notes);
    let b = agent.board("notes", "the old list found", |b| !b.found.is_empty());
    assert_eq!(b.list.as_ref().unwrap().key, outer.to_str().unwrap());
    assert_eq!(b.found, vec![protocol::FoundList { key: notes.to_str().unwrap().into(), tasks: 2, why: protocol::FoundWhy::PreviousKey }]);
    // Naming another list than the one shown: refused.
    let id = agent.next_id();
    agent.send(&AgentCmd::AdoptTasks { id, key: "notes".into(), from: notes.to_str().unwrap().into(), merge: false, list: Some("/elsewhere".into()) });
    assert!(matches!(agent.answer(id), Some(Problem::TasksListChanged { .. })));
    // Brought over: the tasks are there, nothing is found any more, the old file is kept aside.
    let id = agent.next_id();
    agent.send(&AgentCmd::AdoptTasks { id, key: "notes".into(), from: notes.to_str().unwrap().into(), merge: false, list: Some(outer.to_str().unwrap().into()) });
    assert_eq!(agent.answer(id), None);
    agent.board("notes", "the tasks brought over", |b| titles(b) == ["Outline", "Slides"] && b.found.is_empty());
    let dir = home.join(".local/share/tcanvas/tasks");
    assert!(std::fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains(".adopted-")));
    // Again: nothing left to bring.
    let id = agent.next_id();
    agent.send(&AgentCmd::AdoptTasks { id, key: "notes".into(), from: notes.to_str().unwrap().into(), merge: true, list: None });
    assert_eq!(agent.answer(id), Some(Problem::TasksNotAdoptable { from: notes.to_str().unwrap().into() }));
}

#[test]
fn a_worktree_whose_main_checkout_is_gone_has_no_list_for_a_while() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("tasks-broken-link");
    let home = tmp.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let shop = tmp.0.join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    std::fs::write(shop.join("a"), "a").unwrap();
    git(&shop, &["add", "a"]);
    git(&shop, &["commit", "-q", "-m", "a"]);
    let wt = tmp.0.join("shop-wt");
    git(&shop, &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()]);
    let mut agent = Agent::spawn(&home, "broken-link");
    agent.watch("wt", &wt);
    agent.edit_ok("wt", add("Invoice PDF"));
    agent.board("wt", "the repository's list", |b| titles(b) == ["Invoice PDF"]);

    // The main checkout renamed away: the worktree's link dangles. Unknown, nothing offered.
    let away = tmp.0.join("shop-away");
    std::fs::rename(&shop, &away).unwrap();
    agent.watch("wt", &wt);
    let b = agent.board("wt", "no list while the link dangles", |b| b.list.is_none());
    assert!(matches!(b.problem, Some(Problem::TasksKeyUnknown { .. })), "{b:?}");
    assert!(b.found.is_empty());
    let id = agent.next_id();
    agent.send(&AgentCmd::AdoptTasks { id, key: "wt".into(), from: shop.to_str().unwrap().into(), merge: true, list: None });
    assert!(matches!(agent.answer(id), Some(Problem::TasksKeyUnknown { .. })));
    assert_eq!(list_files(&home).len(), 1, "nothing written elsewhere");

    // Back: the same list, nothing moved, nothing offered.
    std::fs::rename(&away, &shop).unwrap();
    agent.watch("wt", &wt);
    let b = agent.board("wt", "the same list again", |b| titles(b) == ["Invoice PDF"]);
    assert_eq!(b.list.unwrap().key, shop.to_str().unwrap());
    assert!(b.found.is_empty(), "no previous key from a failed resolution");
    let dir = home.join(".local/share/tcanvas/tasks");
    assert!(!std::fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains(".adopted-")));
}

#[test]
fn a_list_held_locked_by_another_program_gives_a_typed_failure() {
    use std::os::unix::io::AsRawFd;
    let tmp = TempDir::new("tasks-held-lock");
    let home = tmp.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (one, two) = (tmp.0.join("one"), tmp.0.join("two"));
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    let mut agent = Agent::spawn(&home, "held-lock");
    agent.watch("one", &one);
    agent.watch("two", &two);
    agent.edit_ok("one", add("first"));
    let dir = home.join(".local/share/tcanvas/tasks");
    let lock = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "lock")).unwrap();
    let held = std::fs::OpenOptions::new().read(true).write(true).open(&lock).unwrap();
    // SAFETY: a valid descriptor, owned by `held`.
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    let start = Instant::now();
    let id = agent.edit("one", add("second"));
    let other = agent.edit("two", add("elsewhere"));
    assert!(matches!(agent.answer(id), Some(Problem::TasksLocked { .. })));
    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    assert_eq!(agent.answer(other), None, "the other list still answers");
    drop(held);
    agent.edit_ok("one", add("second"));
}

#[test]
fn the_environment_does_not_point_git_elsewhere() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("tasks-git-env");
    let home = tmp.0.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let shop = tmp.0.join("shop");
    std::fs::create_dir_all(shop.join("sub")).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    let plain = tmp.0.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let env = [
        ("GIT_DIR", shop.join(".git").display().to_string()),
        ("GIT_WORK_TREE", shop.display().to_string()),
        ("GIT_CEILING_DIRECTORIES", shop.display().to_string()),
        ("GIT_DISCOVERY_ACROSS_FILESYSTEM", "1".to_string()),
    ];
    let mut agent = Agent::spawn_with(&home, "git-env", &env);
    agent.watch("plain", &plain);
    agent.watch("sub", &shop.join("sub"));
    let b = agent.board("plain", "the plain folder's own list", |b| b.list.is_some());
    assert_eq!(b.list.unwrap().key, plain.to_str().unwrap());
    let b = agent.board("sub", "the repository's list", |b| b.list.is_some());
    assert_eq!(b.list.unwrap().key, shop.to_str().unwrap());
}
