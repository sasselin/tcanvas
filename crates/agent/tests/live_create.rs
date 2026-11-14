//! Worktree creation end to end: real `tcanvas-agent` binary on a throwaway git repository in a
//! temporary folder (never the repository under test), isolated from the user's git
//! configuration, with a nonexistent tmux socket. Skipped cleanly if git is not installed.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{AgentCmd, AgentEvent, CreateWorktree, LocalBranch, LocationIssue, Problem, WorktreeBranch, WorktreeList, WorktreePlan};

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
    next_id: u64,
}

impl Agent {
    fn spawn() -> Self {
        Self::spawn_with(&[])
    }

    fn spawn_with(env: &[(&str, &str)]) -> Self {
        // Nonexistent tmux socket: no sessions, nothing gets touched.
        let socket = format!("tcanvas-create-test-{}", std::process::id());
        let mut child = Command::new(AGENT).env("TCANVAS_TMUX_SOCKET", socket).envs(GIT_ENV).envs(env.iter().copied()).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, events) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(protocol::decode(&line).expect("valid JSON line"));
            }
        });
        Agent { child, stdin, events, backlog: RefCell::new(Vec::new()), next_id: 0 }
    }

    fn send(&mut self, cmd: &AgentCmd) {
        self.stdin.write_all(protocol::encode(cmd).as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Next event `pick` takes; others are kept for later waits.
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
        panic!("expected: {what}");
    }

    fn plan(&mut self, key: &str, folder: &str, location: Option<&str>) -> WorktreePlan {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&AgentCmd::PlanWorktree { id, key: key.into(), folder: folder.into(), location: location.map(String::from) });
        self.wait("a plan", |e| match e {
            AgentEvent::WorktreePlan(p) if p.id == id => Some((**p).clone()),
            _ => None,
        })
    }

    /// Sends a creation and returns its answer: (path, problem, excluded).
    fn create(&mut self, request: CreateWorktree) -> (Option<String>, Option<Problem>, bool) {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&AgentCmd::CreateWorktree { id, key: "shop".into(), request: request.clone() });
        self.answer(id)
    }

    /// The next answer to the creation `id`.
    fn answer(&self, id: u64) -> (Option<String>, Option<Problem>, bool) {
        self.wait(&format!("answer to creation {id}"), |e| match e {
            AgentEvent::WorktreeCreated { id: i, key, path, problem, excluded, .. } if *i == id && key == "shop" => Some((path.clone(), problem.clone(), *excluded)),
            _ => None,
        })
    }

    fn wait_list(&self, what: &str, pred: impl Fn(&WorktreeList) -> bool) -> WorktreeList {
        self.wait(what, |e| match e {
            AgentEvent::Worktrees(l) if l.key == "shop" && pred(l) => Some(l.clone()),
            _ => None,
        })
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

fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .envs(GIT_ENV)
        .stderr(Stdio::null())
        .output()
        .unwrap();
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git(dir: &Path, args: &[&str]) {
    assert!(git_out(dir, args).is_some(), "git {args:?}");
}

fn commit(dir: &Path, file: &str, message: &str) {
    std::fs::write(dir.join(file), message).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn new_branch(name: &str, base: &str, folder: &str, location: Option<&str>) -> CreateWorktree {
    CreateWorktree { branch: WorktreeBranch::New { name: name.into(), base: base.into() }, folder: folder.into(), location: location.map(String::from), exclude: false }
}

fn existing(name: &str, folder: &str, location: Option<&str>) -> CreateWorktree {
    CreateWorktree { branch: WorktreeBranch::Existing { name: name.into() }, folder: folder.into(), location: location.map(String::from), exclude: false }
}

/// What a refusal must leave untouched: the worktrees, the branches, the exclude file and the
/// folders around the repository.
fn snapshot(shop: &Path) -> (String, String, String, Vec<String>) {
    let worktrees = git_out(shop, &["worktree", "list", "--porcelain"]).unwrap();
    let branches = git_out(shop, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads/"]).unwrap();
    let exclude = std::fs::read_to_string(shop.join(".git/info/exclude")).unwrap_or_default();
    let mut folders = Vec::new();
    for root in [shop.parent().unwrap().to_path_buf(), shop.join(".claude"), shop.join(".claude/worktrees")] {
        if let Ok(entries) = std::fs::read_dir(&root) {
            folders.extend(entries.flatten().map(|e| e.path().display().to_string()));
        }
    }
    folders.sort();
    (worktrees, branches, exclude, folders)
}

#[test]
fn creates_worktrees_and_refuses_anything_unsafe() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("create");
    let shop = tmp.0.join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    commit(&shop, "a.txt", "a");
    // `develop`: one commit main does not have. `topic`: a branch no worktree has. `busy`:
    // checked out in a worktree next to the repository.
    git(&shop, &["branch", "develop"]);
    git(&shop, &["checkout", "-q", "develop"]);
    commit(&shop, "d.txt", "d");
    git(&shop, &["checkout", "-q", "main"]);
    git(&shop, &["branch", "topic"]);
    let busy = tmp.0.join("shop-busy");
    git(&shop, &["worktree", "add", "-q", "-b", "busy", busy.to_str().unwrap()]);
    let main_sha = git_out(&shop, &["rev-parse", "main"]).unwrap();
    let develop_sha = git_out(&shop, &["rev-parse", "develop"]).unwrap();

    let mut agent = Agent::spawn();
    agent.send(&AgentCmd::Watch { key: "shop".into(), path: shop.display().to_string() });
    agent.wait_list("the first list", |l| l.worktrees.len() == 2);

    // The plan: branches, the folder, and that git does not ignore it yet.
    let plan = agent.plan("shop", "invoices", None);
    let invoices = shop.join(".claude/worktrees/invoices");
    assert_eq!(plan.problem, None);
    assert_eq!(plan.path.as_deref(), Some(invoices.to_str().unwrap()));
    assert_eq!(plan.exclude.as_deref(), Some("/.claude/worktrees/"));
    assert_eq!((plan.current_branch.as_deref(), plan.default_branch.as_deref()), (Some("main"), Some("main")));
    let branch = |name: &str, at: Option<&Path>| LocalBranch { name: name.into(), checked_out: at.map(|p| p.display().to_string()) };
    assert_eq!(plan.branches, vec![branch("busy", Some(&busy)), branch("develop", None), branch("main", Some(&shop)), branch("topic", None)]);
    assert_eq!(agent.plan("nobody", "x", None).problem, Some(Problem::NotAGitRepo), "a key the agent does not watch");

    // Refusals first: each leaves everything as it was.
    let before = snapshot(&shop);
    let refuse = |agent: &mut Agent, request: CreateWorktree, problem: Problem| {
        let (path, got, excluded) = agent.create(request.clone());
        assert_eq!((path, got, excluded), (None, Some(problem), false), "{request:?}");
        assert_eq!(snapshot(&shop), before, "{request:?} left something behind");
    };
    for name in ["-x", "--orphan", "a..b", "x.lock", "@{-1}", "HEAD", "@", "a b", "a~1", "fix/", "/x"] {
        let mut request = new_branch(name, "main", "x", None);
        request.exclude = true;
        refuse(&mut agent, request, Problem::InvalidBranchName { branch: name.into() });
    }
    refuse(&mut agent, existing("-x", "x", None), Problem::InvalidBranchName { branch: "-x".into() });
    refuse(&mut agent, new_branch("develop", "main", "develop", None), Problem::BranchExists { branch: "develop".into() });
    refuse(&mut agent, existing("nope", "nope", None), Problem::BranchNotFound { branch: "nope".into() });
    refuse(&mut agent, existing("busy", "busy", None), Problem::BranchCheckedOut { branch: "busy".into(), path: busy.display().to_string() });
    refuse(&mut agent, existing("main", "main", None), Problem::BranchCheckedOut { branch: "main".into(), path: shop.display().to_string() });
    for base in ["nope", "--orphan", "-b", ""] {
        refuse(&mut agent, new_branch("fresh", base, "fresh", None), Problem::BaseNotFound { base: base.into() });
    }
    let location = |template: &str, issue| Problem::LocationRefused { location: template.into(), issue };
    for (template, issue) in [
        ("../../{name}", LocationIssue::ParentDir),
        ("wt/../../../{name}", LocationIssue::ParentDir),
        ("/tmp/{name}", LocationIssue::Absolute),
        ("~/{name}", LocationIssue::Absolute),
        (".git/{name}", LocationIssue::GitDir),
        ("wt", LocationIssue::NoName),
        ("wt/*/{name}", LocationIssue::BadCharacter),
    ] {
        refuse(&mut agent, new_branch("fresh", "main", "fresh", Some(template)), location(template, issue));
    }
    for folder in ["../escape", "..", "", "a/b", "-x", ".hidden"] {
        refuse(&mut agent, new_branch("fresh", "main", folder, None), Problem::InvalidFolder { folder: folder.into() });
    }
    // A symlink inside the repository pointing outside it, and one into its git files.
    let elsewhere = tmp.0.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, shop.join("out")).unwrap();
    std::os::unix::fs::symlink(shop.join(".git"), shop.join("gitlink")).unwrap();
    std::fs::write(shop.join("plain-file"), "x").unwrap();
    let before = snapshot(&shop);
    let refuse = |agent: &mut Agent, request: CreateWorktree, problem: Problem| {
        let (path, got, _) = agent.create(request.clone());
        assert_eq!((path, got), (None, Some(problem)), "{request:?}");
        assert_eq!(snapshot(&shop), before, "{request:?} left something behind");
    };
    refuse(&mut agent, new_branch("fresh", "main", "fresh", Some("out/{name}")), location("out/{name}", LocationIssue::Escapes));
    refuse(&mut agent, new_branch("fresh", "main", "fresh", Some("out/deeper/{name}")), location("out/deeper/{name}", LocationIssue::Escapes));
    refuse(&mut agent, new_branch("fresh", "main", "fresh", Some("gitlink/{name}")), location("gitlink/{name}", LocationIssue::GitDir));
    refuse(&mut agent, new_branch("fresh", "main", "fresh", Some("plain-file/{name}")), location("plain-file/{name}", LocationIssue::NotAFolder));
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing written through the symlink");
    for p in ["out", "gitlink", "plain-file"] {
        std::fs::remove_file(shop.join(p)).unwrap();
    }
    assert_eq!(git_out(&shop, &["status", "--porcelain"]).as_deref(), Some(""), "the main checkout is clean");

    // A new branch from the default branch, the location added to the local exclude file.
    let mut request = new_branch("feat/invoices", "main", "invoices", None);
    request.exclude = true;
    let (path, problem, excluded) = agent.create(request);
    assert_eq!((path.as_deref(), problem, excluded), (Some(invoices.to_str().unwrap()), None, true));
    assert!(invoices.join("a.txt").is_file());
    assert_eq!(git_out(&invoices, &["symbolic-ref", "--short", "HEAD"]).as_deref(), Some("feat/invoices"));
    assert_eq!(git_out(&shop, &["rev-parse", "feat/invoices"]), Some(main_sha.clone()));
    assert_eq!(git_out(&shop, &["config", "--get", "branch.feat/invoices.merge"]), None, "no upstream: it does not track its base");
    let exclude = std::fs::read_to_string(shop.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.lines().filter(|l| *l == "/.claude/worktrees/").count(), 1, "{exclude}");
    assert_eq!(git_out(&shop, &["status", "--porcelain"]).as_deref(), Some(""), "the main checkout stays clean");
    assert!(!std::path::Path::new(&shop.join(".gitignore")).exists(), "the tracked .gitignore is never written");
    // The strip follows without waiting for the next pass.
    let p = invoices.display().to_string();
    agent.wait_list("the new worktree", |l| l.worktrees.iter().any(|w| w.path == p && w.branch.as_deref() == Some("feat/invoices")));

    // Its folder is taken now: a free one is proposed; the location is ignored now.
    let plan = agent.plan("shop", "invoices", None);
    assert_eq!(plan.problem, Some(Problem::DestinationExists { path: invoices.display().to_string() }));
    assert_eq!((plan.free_folder.as_deref(), plan.path), (Some("invoices-2"), None));
    let plan = agent.plan("shop", "invoices-2", None);
    assert_eq!((plan.problem, plan.exclude), (None, None), "ignored now: nothing to add");
    let before = snapshot(&shop);
    let (_, problem, _) = agent.create(new_branch("other", "main", "invoices", None));
    assert_eq!(problem, Some(Problem::DestinationExists { path: invoices.display().to_string() }));
    assert_eq!(snapshot(&shop), before);
    let (_, problem, _) = agent.create(new_branch("feat/invoices", "main", "invoices-2", None));
    assert_eq!(problem, Some(Problem::BranchExists { branch: "feat/invoices".into() }));

    // From another base, without the exclude option (already ignored anyway).
    let (path, problem, excluded) = agent.create(new_branch("on-develop", "develop", "on-develop", None));
    assert_eq!((problem, excluded), (None, false));
    let on_develop = PathBuf::from(path.unwrap());
    assert_eq!(git_out(&on_develop, &["rev-parse", "HEAD"]), Some(develop_sha));
    assert!(on_develop.join("d.txt").is_file());

    // An existing branch, next to the repository.
    let (path, problem, _) = agent.create(existing("topic", "topic", Some("../{repo}-{name}")));
    assert_eq!((path.as_deref(), problem), (Some(tmp.0.join("shop-topic").to_str().unwrap()), None));
    assert_eq!(git_out(&tmp.0.join("shop-topic"), &["symbolic-ref", "--short", "HEAD"]).as_deref(), Some("topic"), "on the branch, not detached");
    let (_, problem, _) = agent.create(existing("topic", "topic-again", None));
    assert_eq!(problem, Some(Problem::BranchCheckedOut { branch: "topic".into(), path: tmp.0.join("shop-topic").display().to_string() }));

    // git itself fails after every check passed (the branch's ref is locked): the exclude line
    // and the folders made for it are taken back.
    let lock = shop.join(".git/refs/heads/locked.lock");
    std::fs::write(&lock, "").unwrap();
    let before = snapshot(&shop);
    let mut request = new_branch("locked", "main", "locked", Some("deep/er/{name}"));
    request.exclude = true;
    let (path, problem, excluded) = agent.create(request);
    assert!(path.is_none() && !excluded && matches!(&problem, Some(Problem::GitFailed { detail }) if detail.contains("lock")), "{problem:?}");
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(snapshot(&shop), before);
    assert!(!shop.join("deep").exists(), "the folders made for it are gone");
    assert_eq!(git_out(&shop, &["rev-parse", "--verify", "-q", "refs/heads/locked"]), None);

    let list = agent.wait_list("every new worktree listed", |l| l.worktrees.len() == 5);
    assert!(list.worktrees.iter().all(|w| !w.locked && !w.prunable));
}

/// A throwaway repository `shop` with one commit, watched by `agent`.
fn watched_repo(tmp: &TempDir, agent: &mut Agent) -> PathBuf {
    let shop = tmp.0.join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    commit(&shop, "a.txt", "a");
    agent.send(&AgentCmd::Watch { key: "shop".into(), path: shop.display().to_string() });
    agent.wait_list("the first list", |l| l.worktrees.len() == 1);
    shop
}

fn hook(shop: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let file = shop.join(".git/hooks/post-checkout");
    std::fs::write(&file, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_hook_leaving_a_job_behind_does_not_hold_the_agent() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("create-hook");
    let mut agent = Agent::spawn();
    let shop = watched_repo(&tmp, &mut agent);
    // The hook starts a job that keeps git's stderr open long after git is done.
    hook(&shop, "(sleep 20 >&2 &)\nexit 0");
    let start = Instant::now();
    let (path, problem, _) = agent.create(new_branch("hooked", "main", "hooked", None));
    assert_eq!(problem, None);
    assert!(start.elapsed() < Duration::from_secs(10), "answered when git ended: {:?}", start.elapsed());
    assert!(PathBuf::from(path.unwrap()).join("a.txt").is_file());
    // The next job does not wait for the job either.
    let (_, problem, _) = agent.create(new_branch("next", "main", "next", None));
    assert_eq!(problem, None);
    assert!(start.elapsed() < Duration::from_secs(15));
}

#[test]
fn a_creation_past_the_time_limit_is_answered_again_when_git_ends() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("create-late");
    let mut agent = Agent::spawn_with(&[("TCANVAS_WRITE_LIMIT_SECS", "1")]);
    let shop = watched_repo(&tmp, &mut agent);
    hook(&shop, "sleep 4");
    agent.next_id += 1;
    let id = agent.next_id;
    let start = Instant::now();
    agent.send(&AgentCmd::CreateWorktree { id, key: "shop".into(), request: new_branch("slow", "main", "slow", None) });
    assert_eq!(agent.answer(id), (None, Some(Problem::GitStillRunning { secs: 1 }), false));
    assert!(start.elapsed() < Duration::from_secs(4), "not kept waiting: {:?}", start.elapsed());
    // Meanwhile the agent answers other requests.
    assert_eq!(agent.plan("shop", "other", None).problem, None);
    // git was not stopped: the final answer, with the same id.
    let (path, problem, _) = agent.answer(id);
    assert_eq!((path.as_deref(), problem), (Some(shop.join(".claude/worktrees/slow").to_str().unwrap()), None));
    assert_eq!(git_out(&shop.join(".claude/worktrees/slow"), &["symbolic-ref", "--short", "HEAD"]).as_deref(), Some("slow"));
    let p = shop.join(".claude/worktrees/slow").display().to_string();
    agent.wait_list("the late worktree listed", |l| l.worktrees.iter().any(|w| w.path == p && !w.locked));
}

#[test]
fn the_exclude_line_never_hides_a_folder_with_files_of_its_own() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("create-exclude");
    let mut agent = Agent::spawn();
    let shop = watched_repo(&tmp, &mut agent);
    std::fs::create_dir_all(shop.join("src")).unwrap();
    commit(&shop, "src/lib.rs", "lib");
    // `src/` holds tracked files: only the worktree's own folder is excluded.
    let plan = agent.plan("shop", "feat-x", Some("src/{name}"));
    assert_eq!(plan.exclude.as_deref(), Some("/src/feat-x/"), "what the dialog shows is what is written");
    let mut request = new_branch("feat-x", "main", "feat-x", Some("src/{name}"));
    request.exclude = true;
    let (_, problem, excluded) = agent.create(request);
    assert_eq!((problem, excluded), (None, true));
    let exclude = std::fs::read_to_string(shop.join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|l| l == "/src/feat-x/") && !exclude.lines().any(|l| l == "/src/"), "{exclude}");
    std::fs::write(shop.join("src/new.rs"), "new").unwrap();
    assert_eq!(git_out(&shop, &["status", "--porcelain"]).as_deref(), Some("?? src/new.rs"), "a new file in src/ still shows");
    // A folder with only untracked files of its own: the same.
    std::fs::create_dir_all(shop.join("notes")).unwrap();
    std::fs::write(shop.join("notes/todo.md"), "todo").unwrap();
    assert_eq!(agent.plan("shop", "n", Some("notes/{name}")).exclude.as_deref(), Some("/notes/n/"));
    // A location folder that does not exist yet: the whole folder, for the next ones too.
    assert_eq!(agent.plan("shop", "y", Some("wt/{name}")).exclude.as_deref(), Some("/wt/"));
}

#[test]
fn a_recorded_worktree_whose_folder_is_gone_is_named_and_a_tag_is_never_checked_out() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("create-registered");
    let mut agent = Agent::spawn();
    let shop = watched_repo(&tmp, &mut agent);
    // A worktree whose folder was deleted by hand: git still records it.
    let gone = shop.join(".claude/worktrees/gone");
    git(&shop, &["worktree", "add", "-q", "-b", "gone", gone.to_str().unwrap()]);
    std::fs::remove_dir_all(&gone).unwrap();
    let plan = agent.plan("shop", "gone", None);
    assert_eq!(plan.problem, Some(Problem::WorktreeRegistered { path: gone.display().to_string() }));
    assert_eq!(plan.free_folder.as_deref(), Some("gone-2"));
    let (_, problem, _) = agent.create(new_branch("other", "main", "gone", None));
    assert_eq!(problem, Some(Problem::WorktreeRegistered { path: gone.display().to_string() }));
    assert!(!gone.exists());
    // A tag (or a remote branch) with that name but no local branch: refused, not detached.
    git(&shop, &["tag", "v1"]);
    let (_, problem, _) = agent.create(existing("v1", "v1", None));
    assert_eq!(problem, Some(Problem::BranchNotFound { branch: "v1".into() }));
    assert!(!shop.join(".claude/worktrees/v1").exists());
}
