//! Worktree recipes end to end: real `tcanvas-agent` binary on a throwaway git repository in a
//! temporary folder (never the repository under test), isolated from the user's git
//! configuration, with a nonexistent tmux socket. Skipped cleanly if git is not installed.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{
    AgentCmd, AgentEvent, Cleanup, CreateWorktree, KeepReason, PatternIssue, Problem, RECIPE_FILE, Recipe, RecipeAction, RecipeItem, RecipePattern, RecipeReport, SkipReason, WorktreeBranch,
    WorktreeList, WorktreeSize,
};

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
        // Folders copied without write permission: given it back first.
        let _ = Command::new("chmod").args(["-R", "u+rwX"]).arg(&self.0).status();
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
    fn spawn_with(env: &[(&str, &str)]) -> Self {
        // Nonexistent tmux socket: no sessions, nothing gets touched.
        let socket = format!("tcanvas-recipe-test-{}", std::process::id());
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

    fn plan_recipe(&mut self) -> Option<Recipe> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&AgentCmd::PlanWorktree { id, key: "shop".into(), folder: "x".into(), location: None });
        self.wait("a plan", |e| match e {
            AgentEvent::WorktreePlan(p) if p.id == id => Some(p.recipe.clone()),
            _ => None,
        })
    }

    /// Creates a worktree on a new branch from main: (its folder, the recipe's report).
    fn create(&mut self, name: &str) -> (PathBuf, Option<RecipeReport>) {
        self.next_id += 1;
        let id = self.next_id;
        let request = CreateWorktree { branch: WorktreeBranch::New { name: name.into(), base: "main".into() }, folder: name.into(), location: None, exclude: false };
        self.send(&AgentCmd::CreateWorktree { id, key: "shop".into(), request });
        let (path, problem, pending) = self.wait("the creation's answer", |e| match e {
            AgentEvent::WorktreeCreated { id: i, path, problem, recipe_pending, .. } if *i == id => Some((path.clone(), problem.clone(), *recipe_pending)),
            _ => None,
        });
        assert_eq!(problem, None, "created");
        let path = path.expect("a folder");
        let report = pending.then(|| self.recipe_applied(id, &path));
        (PathBuf::from(path), report)
    }

    /// The report that follows the creation `id`.
    fn recipe_applied(&self, id: u64, path: &str) -> RecipeReport {
        self.wait("the recipe's report", |e| match e {
            AgentEvent::RecipeApplied { id: i, path: p, report, .. } if *i == id && p == path => Some((**report).clone()),
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

fn write(root: &Path, rel: &str, text: &str, mode: u32) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

/// A throwaway repository `shop` (tracked `.gitignore` and `tracked.txt`), watched by a new agent.
fn repo(tmp: &TempDir, gitignore: &str, env: &[(&str, &str)]) -> (Agent, PathBuf) {
    let shop = tmp.0.join("shop");
    std::fs::create_dir_all(&shop).unwrap();
    git(&shop, &["init", "-q", "-b", "main"]);
    write(&shop, ".gitignore", gitignore, 0o644);
    write(&shop, "tracked.txt", "tracked", 0o644);
    git(&shop, &["add", "."]);
    git(&shop, &["commit", "-q", "-m", "init"]);
    let mut agent = Agent::spawn_with(env);
    agent.send(&AgentCmd::Watch { key: "shop".into(), path: shop.display().to_string() });
    agent.wait_list("the first list", |l| l.worktrees.len() == 1);
    (agent, shop)
}

fn done(action: RecipeAction, path: &str) -> RecipeItem {
    RecipeItem { action, path: path.into(), skipped: None }
}

fn skipped(action: RecipeAction, path: &str, reason: SkipReason) -> RecipeItem {
    RecipeItem { action, path: path.into(), skipped: Some(reason) }
}

const IGNORED: &str = ".env\nconfig/*.local\ndata/\nnode_modules\nvendor/cache\nsecret\n.claude/\n";

#[test]
fn a_recipe_copies_and_links_what_a_new_worktree_needs() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("recipe");
    let (mut agent, shop) = repo(&tmp, IGNORED, &[]);
    assert_eq!(agent.plan_recipe(), None, "no recipe: the plan says nothing about one");

    // The main checkout's own files, ignored by git.
    write(&shop, ".env", "TOKEN=x", 0o600);
    write(&shop, "config/app.local", "app", 0o640);
    write(&shop, "config/db.local", "db", 0o644);
    write(&shop, "data/seed.sql", "insert", 0o644);
    write(&shop, "data/bin/run.sh", "#!/bin/sh", 0o755);
    symlink("../.env", shop.join("data/env-link")).unwrap();
    let elsewhere = tmp.0.join("elsewhere");
    write(&elsewhere, "private.key", "private", 0o600);
    symlink(&elsewhere, shop.join("data/outside-link")).unwrap();
    symlink(elsewhere.join("private.key"), shop.join("secret")).unwrap();
    write(&shop, "node_modules/left-pad/index.js", "module.exports = 1", 0o644);
    write(&shop, "vendor/cache/blob", "blob", 0o644);
    write(&shop, "tracked.txt", "tracked", 0o644);
    let recipe = r#"
# Unknown keys are ignored.
version = 1
[worktree]
copy = [".env", "config/*.local", "data", "tracked.txt", "nothing/*", "secret", "../escape", "/etc/passwd"]
symlink = ["node_modules"]
setup = "touch setup-ran"
later = "ignored"
"#;
    write(&shop, RECIPE_FILE, recipe, 0o644);
    write(&shop, ".claude/settings.json", r#"{"worktree": {"symlinkDirectories": ["node_modules", "vendor/cache"]}}"#, 0o644);

    // The plan shows what it will do, the setup command as written.
    let plan = agent.plan_recipe().expect("a recipe");
    let pattern = |p: &str, claude, matches, refused| RecipePattern { pattern: p.into(), claude, matches, refused, incomplete: false };
    assert_eq!(plan.main, shop.display().to_string());
    assert_eq!(
        plan.copy,
        vec![
            pattern(".env", false, 1, None),
            pattern("config/*.local", false, 2, None),
            pattern("data", false, 1, None),
            pattern("tracked.txt", false, 1, None),
            pattern("nothing/*", false, 0, None),
            pattern("secret", false, 1, None),
            pattern("../escape", false, 0, Some(PatternIssue::ParentDir)),
            pattern("/etc/passwd", false, 0, Some(PatternIssue::Absolute)),
        ]
    );
    assert_eq!(plan.symlink, vec![pattern("node_modules", false, 1, None), pattern("vendor/cache", true, 1, None)], "merged without duplicates");
    assert_eq!((plan.setup.as_deref(), plan.problem), (Some("touch setup-ran"), None));

    let (wt, report) = agent.create("feat");
    let report = report.expect("a report");
    use RecipeAction::{Copy, Symlink};
    assert_eq!(
        report.items,
        vec![
            done(Copy, ".env"),
            done(Copy, "config/app.local"),
            done(Copy, "config/db.local"),
            skipped(Copy, "data/outside-link", SkipReason::LinkOutside),
            skipped(Copy, "data", SkipReason::Partial),
            skipped(Copy, "tracked.txt", SkipReason::Exists),
            skipped(Copy, "nothing/*", SkipReason::NoMatch),
            skipped(Copy, "secret", SkipReason::LinkOutside),
            skipped(Copy, "../escape", SkipReason::Refused { issue: PatternIssue::ParentDir }),
            skipped(Copy, "/etc/passwd", SkipReason::Refused { issue: PatternIssue::Absolute }),
            done(Symlink, "node_modules"),
            done(Symlink, "vendor/cache"),
        ]
    );
    assert_eq!((report.omitted, report.problem), (0, None));

    // Copies: contents and permissions; a folder whole, its inside link kept as a link.
    assert_eq!(std::fs::read_to_string(wt.join(".env")).unwrap(), "TOKEN=x");
    assert_eq!(mode(&wt.join(".env")), 0o600);
    assert_eq!(mode(&wt.join("config/app.local")), 0o640);
    assert_eq!(std::fs::read_to_string(wt.join("data/seed.sql")).unwrap(), "insert");
    assert_eq!(mode(&wt.join("data/bin/run.sh")), 0o755);
    assert_eq!(std::fs::read_link(wt.join("data/env-link")).unwrap(), PathBuf::from("../.env"));
    assert!(std::fs::symlink_metadata(wt.join("data/outside-link")).is_err(), "a link leaving the main checkout is not copied");
    assert!(std::fs::symlink_metadata(wt.join("secret")).is_err());
    assert_eq!(std::fs::read_to_string(wt.join("tracked.txt")).unwrap(), "tracked", "the checked-out file is kept");
    // Links: absolute, to the folder in the main checkout.
    assert_eq!(std::fs::read_link(wt.join("node_modules")).unwrap(), shop.join("node_modules"));
    assert_eq!(std::fs::read_link(wt.join("vendor/cache")).unwrap(), shop.join("vendor/cache"));
    assert!(std::fs::symlink_metadata(wt.join("vendor")).unwrap().is_dir(), "its parent made as a real folder");
    // The agent never runs the setup command; nothing written outside the worktree.
    assert!(!wt.join("setup-ran").exists() && !shop.join("setup-ran").exists());
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 1);
    assert!(!tmp.0.join("escape").exists());
}

#[test]
fn a_malformed_recipe_still_lets_the_worktree_be_created() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("recipe-malformed");
    let (mut agent, shop) = repo(&tmp, IGNORED, &[]);
    write(&shop, RECIPE_FILE, "[worktree]\ncopy = [\".env\"\n", 0o644);
    write(&shop, ".env", "x", 0o600);
    write(&shop, "node_modules/a.js", "a", 0o644);
    // Claude Code's settings still apply; malformed settings would just be ignored.
    write(&shop, ".claude/settings.json", r#"{"worktree": {"symlinkDirectories": ["node_modules"]}}"#, 0o644);
    let plan = agent.plan_recipe().expect("a recipe");
    assert!(matches!(&plan.problem, Some(Problem::RecipeMalformed { file, line: Some(_), .. }) if file == RECIPE_FILE), "{:?}", plan.problem);
    assert!(plan.copy.is_empty() && plan.setup.is_none());
    let (wt, report) = agent.create("feat");
    let report = report.unwrap();
    assert!(matches!(report.problem, Some(Problem::RecipeMalformed { .. })));
    assert_eq!(report.items, vec![done(RecipeAction::Symlink, "node_modules")]);
    assert!(wt.join("tracked.txt").is_file() && !wt.join(".env").exists());

    write(&shop, ".claude/settings.json", "{ not json", 0o644);
    write(&shop, RECIPE_FILE, "[worktree]\nsymlink = \"node_modules\"\n", 0o644);
    let plan = agent.plan_recipe().expect("the recipe file is there");
    assert_eq!(plan.problem, Some(Problem::RecipeWrongType { file: RECIPE_FILE.into(), key: "worktree.symlink".into() }));
    assert!(plan.symlink.is_empty(), "the settings are ignored silently");
    let (_, report) = agent.create("other");
    assert!(report.unwrap().items.is_empty());
}

#[test]
fn copies_stop_at_their_limits() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("recipe-caps");
    let (mut agent, shop) = repo(&tmp, IGNORED, &[("TCANVAS_RECIPE_MAX_ENTRIES", "5"), ("TCANVAS_RECIPE_MAX_BYTES", "1000")]);
    for i in 0..10 {
        write(&shop, &format!("data/{i}.txt"), "x", 0o644);
    }
    write(&shop, ".env", "x", 0o600);
    write(&shop, "node_modules/a.js", "a", 0o644);
    write(&shop, RECIPE_FILE, "[worktree]\ncopy = [\"data\", \".env\"]\nsymlink = [\"node_modules\"]\n", 0o644);
    let (wt, report) = agent.create("capped");
    let cap = SkipReason::CapReached { max_bytes: 1000, max_entries: 5 };
    use RecipeAction::{Copy, Symlink};
    assert_eq!(report.unwrap().items, vec![skipped(Copy, "data", cap.clone()), skipped(Copy, ".env", cap), done(Symlink, "node_modules")], "links are not copies");
    assert_eq!(std::fs::read_dir(wt.join("data")).unwrap().count(), 4, "the folder and 4 files, then it stopped");

    // The byte limit.
    write(&shop, "big.bin", &"x".repeat(2000), 0o644);
    write(&shop, RECIPE_FILE, "[worktree]\ncopy = [\".env\", \"big.bin\"]\n", 0o644);
    let (wt, report) = agent.create("big");
    assert_eq!(report.unwrap().items, vec![done(Copy, ".env"), skipped(Copy, "big.bin", SkipReason::CapReached { max_bytes: 1000, max_entries: 5 })]);
    assert!(!wt.join("big.bin").exists());
}

/// The agent's size answer (with the ignored files) for `path`.
fn measured(agent: &mut Agent, path: &Path) -> WorktreeSize {
    let path = path.display().to_string();
    agent.send(&AgentCmd::WorktreeSizes { key: "shop".into(), paths: vec![path.clone()] });
    agent.wait(&format!("size of {path}"), |e| match e {
        AgentEvent::WorktreeSizes { key, sizes } if key == "shop" && sizes[0].path == path => Some(sizes[0].clone()),
        _ => None,
    })
}

#[test]
fn cleanup_removes_recipe_links_never_what_they_point_to() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("recipe-cleanup");
    // `node_modules` ignored as a name (a link is not a folder for git: `node_modules/` would not
    // match it); `cache/` only as a folder.
    let (mut agent, shop) = repo(&tmp, "node_modules\ncache/\n", &[]);
    write(&shop, "node_modules/dep/index.js", "kept", 0o644);
    // A repository inside the linked folder: walking through the link would see it.
    git(&shop.join("node_modules/dep"), &["init", "-q"]);
    write(&shop, "cache/blob", "kept too", 0o644);
    write(&shop, RECIPE_FILE, "[worktree]\nsymlink = [\"node_modules\", \"cache\"]\n", 0o644);
    git(&shop, &["add", RECIPE_FILE]);
    git(&shop, &["commit", "-q", "-m", "recipe"]);

    // Merged once it has a commit of its own on main; its link to `cache` is not ignored.
    let (dirty, _) = agent.create("dirty");
    write(&dirty, "work.txt", "work", 0o644);
    git(&dirty, &["add", "work.txt"]);
    git(&dirty, &["commit", "-q", "-m", "work"]);
    git(&shop, &["merge", "-q", "--ff-only", "dirty"]);
    let p = dirty.display().to_string();
    let list = agent.wait_list("the merged worktree", |l| l.worktrees.iter().any(|w| w.path == p && w.merged == Some(true)));
    let wt = list.worktrees.iter().find(|w| w.path == p).unwrap();
    assert_eq!(wt.dirty, Some(true), "an untracked link: the worktree is not offered");
    let seen = measured(&mut agent, &dirty).ignored.unwrap();
    let (_, problem) = cleanup(&mut agent, Cleanup::RemoveWorktree { path: p.clone(), delete_branch: None, ignored: seen });
    assert_eq!(problem, Some(Problem::WorktreeKept { path: p, reason: KeepReason::Dirty }));

    // With every link ignored: removable; git deletes the links, not the folders they point to.
    write(&shop, ".gitignore", "node_modules\ncache\n", 0o644);
    git(&shop, &["commit", "-q", "-am", "ignore links"]);
    let (clean, _) = agent.create("clean");
    write(&clean, "more.txt", "more", 0o644);
    git(&clean, &["add", "more.txt"]);
    git(&clean, &["commit", "-q", "-m", "more"]);
    git(&shop, &["merge", "-q", "--ff-only", "clean"]);
    let p = clean.display().to_string();
    agent.wait_list("clean and merged", |l| l.worktrees.iter().any(|w| w.path == p && w.merged == Some(true) && w.dirty == Some(false)));
    let seen = measured(&mut agent, &clean).ignored.unwrap();
    assert_eq!(seen.shown, vec!["cache".to_string(), "node_modules".to_string()]);
    let (_, problem) = cleanup(&mut agent, Cleanup::RemoveWorktree { path: p, delete_branch: None, ignored: seen });
    assert_eq!(problem, None, "the repository behind the link is not looked into");
    assert!(!clean.exists());
    assert_eq!(std::fs::read_to_string(shop.join("node_modules/dep/index.js")).unwrap(), "kept");
    assert!(shop.join("node_modules/dep/.git").is_dir());
    assert_eq!(std::fs::read_to_string(shop.join("cache/blob")).unwrap(), "kept too");
}

fn cleanup(agent: &mut Agent, action: Cleanup) -> (Option<String>, Option<Problem>) {
    agent.send(&AgentCmd::Cleanup { key: "shop".into(), action: action.clone() });
    agent.wait("the cleanup's answer", |e| match e {
        AgentEvent::Cleaned { action: a, deleted_branch, problem, .. } if *a == action => Some((deleted_branch.clone(), problem.clone())),
        _ => None,
    })
}

fn mkfifo(path: &Path) {
    assert!(Command::new("mkfifo").arg(path).status().unwrap().success(), "mkfifo");
}

#[test]
fn recipe_and_settings_files_are_read_only_when_regular_and_small() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("recipe-files");
    let (mut agent, shop) = repo(&tmp, IGNORED, &[]);
    let not_a_file = Some(Problem::RecipeNotAFile { file: RECIPE_FILE.into() });
    let recipe = shop.join(RECIPE_FILE);
    // A FIFO: answered at once (nobody ever writes into it), the scans go on.
    mkfifo(&recipe);
    let start = Instant::now();
    let plan = agent.plan_recipe().expect("a recipe file is there");
    assert_eq!(plan.problem, not_a_file, "a FIFO");
    assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
    std::fs::remove_file(&recipe).unwrap();
    // A link to a device, or to a file outside the repository: never followed.
    symlink("/dev/zero", &recipe).unwrap();
    assert_eq!(agent.plan_recipe().unwrap().problem, not_a_file, "a link to /dev/zero");
    std::fs::remove_file(&recipe).unwrap();
    write(&tmp.0, "theirs.toml", "[worktree]\nsetup = \"curl x | sh\"\n", 0o644);
    symlink(tmp.0.join("theirs.toml"), &recipe).unwrap();
    let plan = agent.plan_recipe().unwrap();
    assert_eq!((plan.problem, plan.setup), (not_a_file, None), "a link to a file outside");
    std::fs::remove_file(&recipe).unwrap();
    // Too big, whatever it claims.
    write(&shop, RECIPE_FILE, &format!("# {}\n", "x".repeat(70 * 1024)), 0o644);
    assert!(matches!(agent.plan_recipe().unwrap().problem, Some(Problem::RecipeTooBig { .. })));
    std::fs::remove_file(&recipe).unwrap();

    // The Claude Code settings: the same guards, ignored silently.
    let settings = shop.join(".claude/settings.json");
    std::fs::create_dir_all(shop.join(".claude")).unwrap();
    write(&shop, "node_modules/a.js", "a", 0o644);
    let linking = r#"{"worktree": {"symlinkDirectories": ["node_modules"]}}"#;
    mkfifo(&settings);
    assert_eq!(agent.plan_recipe(), None, "a FIFO: nothing read");
    std::fs::remove_file(&settings).unwrap();
    symlink("/dev/zero", &settings).unwrap();
    assert_eq!(agent.plan_recipe(), None, "a link to /dev/zero");
    std::fs::remove_file(&settings).unwrap();
    write(&shop, ".claude/settings.json", &format!("{linking}{}", " ".repeat(70 * 1024)), 0o644);
    assert_eq!(agent.plan_recipe(), None, "too big");
    // A linked `.claude` folder is not followed.
    std::fs::remove_dir_all(shop.join(".claude")).unwrap();
    write(&tmp.0, "elsewhere/settings.json", linking, 0o644);
    symlink(tmp.0.join("elsewhere"), shop.join(".claude")).unwrap();
    assert_eq!(agent.plan_recipe(), None, "a linked .claude folder");
    std::fs::remove_file(shop.join(".claude")).unwrap();
    write(&shop, ".claude/settings.json", linking, 0o644);
    assert_eq!(agent.plan_recipe().map(|r| r.symlink.len()), Some(1), "a regular one is read");
}

#[test]
fn one_wide_pattern_does_not_starve_the_others() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("recipe-starve");
    let (mut agent, shop) = repo(&tmp, IGNORED, &[("TCANVAS_RECIPE_SEARCH_LIMIT", "50")]);
    for i in 0..120 {
        write(&shop, &format!("big/{i}.txt"), "x", 0o644);
    }
    write(&shop, ".env", "x", 0o600);
    write(&shop, "node_modules/a.js", "a", 0o644);
    write(&shop, RECIPE_FILE, "[worktree]\ncopy = [\"**/.env\", \".env\"]\nsymlink = [\"node_modules\"]\n", 0o644);
    let plan = agent.plan_recipe().unwrap();
    assert!(plan.copy[0].incomplete && !plan.copy[1].incomplete && plan.copy[1].matches == 1 && plan.symlink[0].matches == 1, "{plan:?}");
    let (wt, report) = agent.create("feat");
    let items = report.unwrap().items;
    assert!(items.contains(&skipped(RecipeAction::Copy, "**/.env", SkipReason::SearchLimit)), "{items:?}");
    assert!(items.contains(&done(RecipeAction::Symlink, "node_modules")), "{items:?}");
    assert!(wt.join(".env").is_file() && wt.join("node_modules").is_symlink());
}

#[test]
fn the_answer_comes_when_git_is_done_and_the_report_once_the_copies_are() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("recipe-later");
    let (mut agent, shop) = repo(&tmp, IGNORED, &[("TCANVAS_RECIPE_TEST_DELAY_MS", "1500")]);
    write(&shop, ".env", "x", 0o600);
    write(&shop, RECIPE_FILE, "[worktree]\ncopy = [\".env\"]\n", 0o644);
    agent.next_id += 1;
    let id = agent.next_id;
    let request = CreateWorktree { branch: WorktreeBranch::New { name: "slow".into(), base: "main".into() }, folder: "slow".into(), location: None, exclude: false };
    let start = Instant::now();
    agent.send(&AgentCmd::CreateWorktree { id, key: "shop".into(), request });
    let (path, pending) = agent.wait("the creation's answer", |e| match e {
        AgentEvent::WorktreeCreated { id: i, path, recipe_pending, .. } if *i == id => Some((path.clone().unwrap(), *recipe_pending)),
        _ => None,
    });
    assert!(pending && start.elapsed() < Duration::from_millis(1500), "answered before the copies: {:?}", start.elapsed());
    assert!(!Path::new(&path).join(".env").exists(), "not copied yet");
    // Meanwhile, a cleanup is answered: it does not wait behind the copies.
    agent.send(&AgentCmd::Cleanup { key: "shop".into(), action: Cleanup::Prune { paths: vec![] } });
    agent.wait("the cleanup's answer", |e| matches!(e, AgentEvent::Cleaned { .. }).then_some(()));
    assert!(start.elapsed() < Duration::from_millis(1500), "{:?}", start.elapsed());
    let report = agent.recipe_applied(id, &path);
    assert_eq!(report.items, vec![done(RecipeAction::Copy, ".env")]);
    assert!(Path::new(&path).join(".env").is_file(), "copied when reported");
}

#[test]
fn incomplete_copies_and_unsafe_links_are_reported() {
    if !has_git() {
        eprintln!("git missing: test skipped");
        return;
    }
    let tmp = TempDir::new("recipe-partial");
    let (mut agent, shop) = repo(&tmp, IGNORED, &[("TCANVAS_RECIPE_MAX_BYTES", "1000")]);
    use std::os::unix::ffi::OsStrExt;
    write(&shop, "data/ok.txt", "ok", 0o644);
    write(&shop, "data/locked/secret.txt", "s", 0o644);
    std::fs::set_permissions(shop.join("data/locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::write(shop.join("data").join(std::ffi::OsStr::from_bytes(b"bad-\xff")), "x").unwrap();
    mkfifo(&shop.join("data/pipe"));
    write(&shop, "not-copied.txt", "x", 0o644);
    symlink("../not-copied.txt", shop.join("data/dangling")).unwrap();
    // A file growing while it is copied: never more than the limit.
    let mut grower = Command::new("sh").arg("-c").arg("exec yes > grow.txt").current_dir(&shop).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // Links to the worktree itself, a folder holding it, another worktree.
    git(&shop, &["worktree", "add", "-q", "-b", "other", shop.join(".claude/worktrees/other").to_str().unwrap()]);
    write(&shop, RECIPE_FILE, "[worktree]\ncopy = [\"data\", \"grow.txt\"]\nsymlink = [\".claude\", \".claude/worktrees/*\"]\n", 0o644);
    let (wt, report) = agent.create("feat");
    let _ = grower.kill();
    let _ = grower.wait();
    std::fs::set_permissions(shop.join("data/locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let items = report.unwrap().items;
    use RecipeAction::{Copy, Symlink};
    let reason = |path: &str| items.iter().find(|i| i.path == path).and_then(|i| i.skipped.clone());
    assert!(matches!(reason("data/locked"), Some(SkipReason::Unreadable { .. })), "{items:?}");
    assert_eq!(reason("data/bad-\u{fffd}"), Some(SkipReason::BadName));
    assert_eq!(reason("data/pipe"), Some(SkipReason::Unsupported), "a FIFO never blocks the copy");
    assert_eq!(reason("data/dangling"), Some(SkipReason::DanglingLink));
    assert!(std::fs::symlink_metadata(wt.join("data/dangling")).is_ok(), "kept, as written");
    assert_eq!(reason("data"), Some(SkipReason::Partial), "not \"copied\"");
    assert!(wt.join("data/ok.txt").is_file());
    assert_eq!(reason("grow.txt"), Some(SkipReason::CapReached { max_bytes: 1000, max_entries: 200_000 }));
    assert!(!wt.join("grow.txt").exists());
    assert_eq!(reason(".claude"), Some(SkipReason::ContainsWorktree));
    assert_eq!(reason(".claude/worktrees/feat"), Some(SkipReason::ContainsWorktree));
    assert_eq!(reason(".claude/worktrees/other"), Some(SkipReason::NestedRepository));
    assert!(items.iter().all(|i| i.action == Copy || i.skipped.is_some()), "no link made: {items:?}");
    assert!(!items.iter().any(|i| i.action == Symlink && i.skipped.is_none()));
    assert!(std::fs::symlink_metadata(wt.join(".claude")).map(|m| !m.is_symlink()).unwrap_or(true));
}
