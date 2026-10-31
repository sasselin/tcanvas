//! Test de bout en bout : vrai serveur tmux jetable + vrai binaire `tcanvas-agent`.
//! Ignoré proprement si tmux ou zsh ne sont pas installés.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use protocol::{AgentCmd, AgentEvent, SessionInfo, TermStatus};

const AGENT: &str = env!("CARGO_BIN_EXE_tcanvas-agent");

struct Server {
    socket: String,
}

impl Server {
    fn tmux(&self, args: &[&str]) {
        let ok = Command::new("tmux").arg("-L").arg(&self.socket).args(args).status().unwrap().success();
        assert!(ok, "tmux {args:?} a échoué");
    }
    fn type_line(&self, session: &str, line: &str) {
        self.tmux(&["send-keys", "-t", session, "-l", line]);
        self.tmux(&["send-keys", "-t", session, "Enter"]);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("tmux").arg("-L").arg(&self.socket).arg("kill-server").status();
        remove_socket_file(&self.socket);
    }
}

/// Supprime le fichier de socket que tmux laisse après `kill-server`.
fn remove_socket_file(socket: &str) {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
    let tmp = std::env::var("TMUX_TMPDIR").unwrap_or_else(|_| "/tmp".into());
    let _ = std::fs::remove_file(format!("{tmp}/tmux-{uid}/{socket}"));
}


struct Agent {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<AgentEvent>,
}

impl Agent {
    fn spawn(socket: &str) -> Self {
        let mut child = Command::new(AGENT)
            .env("TCANVAS_TMUX_SOCKET", socket)
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
        Agent { child, stdin, events }
    }

    fn send(&mut self, cmd: &AgentCmd) {
        self.stdin.write_all(protocol::encode(cmd).as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn wait_for(&self, what: &str, pred: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut seen = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(left) {
                Ok(ev) if pred(&ev) => return ev,
                Ok(ev) => seen.push(ev),
                Err(_) => break,
            }
        }
        panic!("attendu : {what}\nreçu : {seen:#?}");
    }

    fn wait_status(&self, session: &str, what: &str, pred: impl Fn(&SessionInfo) -> bool) -> SessionInfo {
        match self.wait_for(what, |ev| matches!(ev, AgentEvent::Session(i) if i.session == session && pred(i))) {
            AgentEvent::Session(info) => info,
            _ => unreachable!(),
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn has(bin: &str) -> bool {
    Command::new(bin).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

#[test]
fn reports_running_stalled_failed_question_and_gone() {
    if !has("tmux") || !has("zsh") {
        eprintln!("tmux ou zsh absent : test ignoré");
        return;
    }
    let server = Server { socket: format!("tcanvas-test-{}", std::process::id()) };
    let session = "tc_live";

    // Hooks dans un fichier temporaire. Le nom du socket contient « tcanvas », ce qui les active.
    let hooks = std::env::temp_dir().join(format!("tcanvas-hooks-{}.zsh", std::process::id()));
    let out = Command::new(AGENT).arg("hooks").output().unwrap();
    std::fs::write(&hooks, &out.stdout).unwrap();

    server.tmux(&["new-session", "-d", "-s", session, "-x", "100", "-y", "20", "zsh -f -i"]);
    server.tmux(&["new-session", "-d", "-s", "perso", "zsh -f -i"]);
    thread::sleep(Duration::from_millis(300));
    server.type_line(session, &format!("source {}", hooks.display()));

    let mut agent = Agent::spawn(&server.socket);
    agent.wait_for("Hello", |ev| matches!(ev, AgentEvent::Hello { .. }));
    agent.send(&AgentCmd::SetStall { session: session.into(), secs: 2 });
    agent.wait_status(session, "Idle", |i| i.status == TermStatus::Idle);

    server.type_line(session, "sleep 4");
    let running = agent.wait_status(session, "Running", |i| i.status == TermStatus::Running);
    assert_eq!(running.command.as_deref(), Some("sleep 4"));
    agent.wait_status(session, "Stalled", |i| i.status == TermStatus::Stalled);
    let done = agent.wait_status(session, "Idle après sleep", |i| i.status == TermStatus::Idle && i.last_exit == Some(0));
    assert!(matches!(done.last_duration_secs, Some(3..=5)), "durée {:?}", done.last_duration_secs);

    server.type_line(session, "false");
    let failed = agent.wait_status(session, "Exited(1)", |i| i.status == TermStatus::Exited(1));
    assert_eq!(failed.last_exit, Some(1));

    server.type_line(session, r#"sh -c 'printf "Continuer ? [y/N] "; read x'"#);
    let asking = agent.wait_status(session, "WaitingInput", |i| i.status == TermStatus::WaitingInput);
    assert!(asking.tail.last().unwrap().ends_with("[y/N]"), "tail {:?}", asking.tail);
    server.type_line(session, "y");
    agent.wait_status(session, "Idle après réponse", |i| i.status == TermStatus::Idle);

    // La session « perso » (sans préfixe tc_) n'est jamais rapportée.
    agent.send(&AgentCmd::Kill { session: session.into() });
    agent.wait_for("Gone", |ev| matches!(ev, AgentEvent::Gone { session: s } if s == session));
    assert!(agent.events.try_iter().all(|ev| !matches!(ev, AgentEvent::Session(i) if i.session == "perso")));

    let _ = std::fs::remove_file(hooks);
}

fn proc_alive(cmd: &str) -> bool {
    Command::new("pgrep").args(["-fx", cmd]).output().is_ok_and(|o| o.status.success())
}

#[test]
fn kill_stops_every_process_even_stubborn_ones() {
    if !has("tmux") || !has("zsh") {
        eprintln!("tmux ou zsh absent : test ignoré");
        return;
    }
    let server = Server { socket: format!("tcanvas-kill-{}", std::process::id()) };
    let session = "tc_stubborn";
    // Durées uniques pour ne viser que nos processus.
    let base = 700_000 + std::process::id() % 100_000;
    let cmds: Vec<String> = (0..4).map(|k| format!("sleep {}", base + k)).collect();

    server.tmux(&["new-session", "-d", "-s", session, "-x", "120", "-y", "30", "zsh -f -i"]);
    server.tmux(&["new-session", "-d", "-s", "tc_voisin", "zsh -f -i"]);
    thread::sleep(Duration::from_millis(300));
    // Shell et enfants qui ignorent SIGHUP et SIGTERM, nohup, setsid, et une commande au premier plan.
    server.type_line(session, &format!("trap '' HUP TERM; {} &", cmds[0]));
    server.type_line(session, &format!("nohup {} >/dev/null 2>&1 &", cmds[1]));
    server.type_line(session, &format!("setsid {} &", cmds[2]));
    server.type_line(session, &cmds[3]);
    server.type_line("tc_voisin", &format!("sleep {}", base + 9));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cmds.iter().all(|c| proc_alive(c)) {
        assert!(Instant::now() < deadline, "processus de test non démarrés");
        thread::sleep(Duration::from_millis(100));
    }

    let out = Command::new(AGENT).args(["kill", session]).env("TCANVAS_TMUX_SOCKET", &server.socket).output().unwrap();
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "kill : {report} {}", String::from_utf8_lossy(&out.stderr));
    assert!(report.contains("SIGKILL"), "{report}");

    for c in &cmds {
        assert!(!proc_alive(c), "{c} a survécu");
    }
    let has_session = |s: &str| Command::new("tmux").args(["-L", &server.socket, "has-session", "-t", s]).status().unwrap().success();
    assert!(!has_session(session), "session supprimée");
    assert!(has_session("tc_voisin"), "les autres sessions ne sont pas touchées");
    assert!(proc_alive(&format!("sleep {}", base + 9)), "ni leurs processus");
}
