//! Suivi des changements de bout en bout : vrai binaire `tcanvas-agent`, vrai dépôt git temporaire.
//! Ignoré proprement si git n'est pas installé.

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
    /// Événements reçus pendant qu'on en attendait un autre : gardés pour les attentes suivantes.
    backlog: RefCell<Vec<AgentEvent>>,
}

impl Agent {
    fn spawn() -> Self {
        // Socket tmux qui n'existe pas : aucune session, on ne touche à rien.
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
                let _ = tx.send(protocol::decode(&line).expect("ligne JSON valide"));
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
        panic!("attendu : {what}\nreçu : {backlog:#?}");
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
        eprintln!("git absent : test ignoré");
        return;
    }
    let repo = temp_dir("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "un\ndeux\ntrois\n").unwrap();
    std::fs::write(repo.join("gone.txt"), "x\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let mut agent = Agent::spawn();
    agent.wait_for("Hello", |ev| matches!(ev, AgentEvent::Hello { .. }));
    agent.send(&AgentCmd::Watch { key: "p".into(), path: repo.display().to_string() });
    let clean = agent.wait_changes("p", "dépôt propre", |_| true);
    assert!(clean.git && clean.files.is_empty() && clean.error.is_none(), "{clean:?}");
    assert_eq!(clean.branch.as_deref(), Some("main"));
    // Dépôt propre : le travail déjà commité reste visible, avec son contenu.
    assert_eq!(clean.commits.iter().map(|c| (c.summary.as_str(), c.author.as_str())).collect::<Vec<_>>(), vec![("init", "t")]);
    let sha = clean.commits[0].sha.clone();
    agent.send(&AgentCmd::ShowCommit { key: "p".into(), sha: sha.clone() });
    let AgentEvent::Diff { text, .. } = agent.wait_for("contenu du commit", |ev| matches!(ev, AgentEvent::Diff { file, .. } if *file == sha)) else { unreachable!() };
    assert!(text.contains("init") && text.contains("Author: ") && text.contains("+deux") && text.contains("gone.txt"), "{text}");
    agent.send(&AgentCmd::ShowCommit { key: "p".into(), sha: "--output=/tmp/x".into() });
    let AgentEvent::Diff { text, problem, .. } = agent.wait_for("sha refusé", |ev| matches!(ev, AgentEvent::Diff { file, .. } if file.starts_with("--"))) else { unreachable!() };
    assert_eq!((text.as_str(), problem), ("", Some(Problem::CommitRefused)));

    // Ce que ferait Claude : modifier, créer, supprimer.
    std::fs::write(repo.join("a.txt"), "un\nDEUX\ntrois\nquatre\n").unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/new.rs"), "fn main() {}\n// fin\n").unwrap();
    std::fs::remove_file(repo.join("gone.txt")).unwrap();

    let set = agent.wait_changes("p", "trois changements", |s| s.files.len() == 3);
    let get = |p: &str| set.files.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("{p} absent : {set:?}"));
    let a = get("a.txt");
    assert_eq!((&a.kind, a.added, a.removed), (&ChangeKind::Modified, Some(2), Some(1)));
    let new = get("src/new.rs");
    assert_eq!((&new.kind, new.added, new.removed), (&ChangeKind::Untracked, Some(2), Some(0)));
    assert_eq!(get("gone.txt").kind, ChangeKind::Deleted);

    agent.send(&AgentCmd::Diff { key: "p".into(), file: "a.txt".into() });
    let AgentEvent::Diff { text, truncated, .. } = agent.wait_for("diff de a.txt", |ev| matches!(ev, AgentEvent::Diff { file, .. } if file == "a.txt")) else { unreachable!() };
    assert!(text.contains("-deux") && text.contains("+DEUX") && text.contains("+quatre") && !truncated, "{text}");

    agent.send(&AgentCmd::Diff { key: "p".into(), file: "src/new.rs".into() });
    let AgentEvent::Diff { text, .. } = agent.wait_for("diff du fichier non suivi", |ev| matches!(ev, AgentEvent::Diff { file, .. } if file == "src/new.rs")) else { unreachable!() };
    assert!(text.contains("+fn main() {}"), "{text}");

    // Ne plus surveiller : plus aucun événement pour cette clé.
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
    std::fs::write(dir.join("old.txt"), "vieux").unwrap();
    // L'horloge de mtime est à la seconde : on attend la suivante avant de surveiller.
    thread::sleep(Duration::from_millis(1100));

    let mut agent = Agent::spawn();
    agent.send(&AgentCmd::Watch { key: "plain".into(), path: dir.display().to_string() });
    agent.send(&AgentCmd::Watch { key: "absent".into(), path: "/nexiste/pas".into() });
    let err = agent.wait_changes("absent", "erreur", |s| s.error.is_some());
    assert!(matches!(err.error, Some(Problem::DirNotFound { .. })), "{err:?}");

    let empty = agent.wait_changes("plain", "rien de neuf", |_| true);
    assert!(!empty.git && empty.files.is_empty(), "{empty:?}");
    std::fs::create_dir_all(dir.join("node_modules")).unwrap();
    std::fs::write(dir.join("node_modules/ignored.js"), "x").unwrap();
    std::fs::write(dir.join("fresh.txt"), "neuf").unwrap();
    let set = agent.wait_changes("plain", "fresh.txt", |s| !s.files.is_empty());
    assert_eq!(set.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["fresh.txt"]);

    agent.send(&AgentCmd::Diff { key: "plain".into(), file: "fresh.txt".into() });
    let AgentEvent::Diff { problem, .. } = agent.wait_for("message hors git", |ev| matches!(ev, AgentEvent::Diff { .. })) else { unreachable!() };
    assert_eq!(problem, Some(Problem::NotAGitRepo));

    // Sélecteur de dossiers : l'agent liste les sous-dossiers.
    std::fs::create_dir_all(dir.join("sous-dossier")).unwrap();
    agent.send(&AgentCmd::ListDir { path: dir.display().to_string() });
    let AgentEvent::DirListing(l) = agent.wait_for("liste du dossier", |ev| matches!(ev, AgentEvent::DirListing(_))) else { unreachable!() };
    let names: Vec<&str> = l.dirs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"sous-dossier") && !l.git, "{l:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
