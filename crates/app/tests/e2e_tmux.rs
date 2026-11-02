//! Bout en bout : vrais PTY (ConPTY + wsl.exe sous Windows) + vrai tmux (socket jetable) + vrai
//! `tcanvas-agent`. Vérifie la saisie, le statut, la notification de blocage, la survie des
//! terminaux à un redémarrage de l'application, et la fermeture.
//!
//! Sous Windows, l'agent Linux doit être fourni : `TCANVAS_E2E_AGENT=/chemin/wsl/tcanvas-agent`
//! (voir `scripts/test-windows.sh`). Sans lui, le test est ignoré.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use egui::{Key, Vec2};
use egui_kittest::Harness;
use protocol::TermStatus;
use tcanvas::features::agent_link::{AgentMode, LinkState};
use tcanvas::features::terminal_card::Launcher;
use tcanvas::shell::event::Event;
use tcanvas::{App, Config};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Commande exécutée dans WSL (directement sous Linux).
fn in_wsl(program: &str) -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("wsl.exe");
        cmd.args(["-e", program]);
        cmd
    } else {
        Command::new(program)
    }
}

/// Chemin (côté WSL) du binaire de l'agent.
fn agent_path() -> Option<String> {
    if cfg!(windows) {
        return std::env::var("TCANVAS_E2E_AGENT").ok();
    }
    let ok = Command::new(env!("CARGO")).args(["build", "-q", "-p", "tcanvas-agent"]).current_dir(workspace()).status().unwrap().success();
    assert!(ok, "compilation de l'agent");
    let target = std::env::var("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|_| workspace().join("target"));
    Some(target.join("debug/tcanvas-agent").display().to_string())
}

/// Toutes les sessions de tous les sockets tmux de l'utilisateur, pour les diagnostics.
fn tmux_report() -> String {
    let script = r#"for s in "${TMUX_TMPDIR:-/tmp}"/tmux-$(id -u)/*; do echo "$s:"; tmux -S "$s" ls 2>&1 | sed 's/^/  /'; done"#;
    in_wsl("sh").args(["-c", script]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Un processus dont la ligne de commande est exactement `cmd` tourne-t-il dans WSL ?
fn proc_alive(cmd: &str) -> bool {
    in_wsl("pgrep").args(["-fx", cmd]).output().is_ok_and(|o| o.status.success())
}

fn tmux_ok(socket: &str, args: &[&str]) -> bool {
    in_wsl("tmux").arg("-L").arg(socket).args(args).output().is_ok_and(|o| o.status.success())
}

fn harness(dir: &std::path::Path) -> Harness<'static, App> {
    let dir = dir.to_path_buf();
    Harness::builder().with_size(Vec2::new(1400.0, 900.0)).with_step_dt(1.0 / 60.0).allow_missing_glyphs().build_eframe(move |cc| {
        App::new(&cc.egui_ctx, Config { launcher: Launcher::Tmux, agent: AgentMode::Spawn, config_dir: dir, os_toasts: false })
    })
}

/// Fait tourner l'application en temps réel jusqu'à ce que `cond` soit vrai.
fn wait(h: &mut Harness<'_, App>, what: &str, secs: u64, cond: impl Fn(&App) -> bool) {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(secs);
    while Instant::now() < deadline {
        h.step();
        if cond(h.state()) {
            eprintln!("✓ {what} en {} ms", start.elapsed().as_millis());
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let app = h.state();
    let screens: Vec<String> = app.world.cards.iter().map(|c| app.cards.screen_text(&c.id).unwrap_or_default()).collect();
    panic!("délai dépassé : {what}\nagent : {:?}\ncartes : {:#?}\nécrans :\n{}", app.agent_state(), app.world.cards, screens.join("\n-----\n"));
}

fn type_line(h: &mut Harness<'_, App>, line: &str) {
    h.event(egui::Event::Text(line.into()));
    h.step();
    h.key_press(Key::Enter);
    h.step();
}

fn screen(app: &App, id: &str) -> String {
    app.cards.screen_text(id).unwrap_or_default()
}

#[test]
fn real_terminals_end_to_end() {
    if !tmux_ok("tcanvas-probe-absent", &["-V"]) {
        eprintln!("tmux absent : test ignoré");
        return;
    }
    let socket = format!("tcanvas-e2e-{}", std::process::id());
    let dir = std::env::temp_dir().join(format!("tcanvas-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let Some(agent) = agent_path() else {
        eprintln!("TCANVAS_E2E_AGENT absent : test ignoré");
        return;
    };
    // SAFETY : seul test de ce binaire, positionné avant tout autre thread de l'application.
    unsafe {
        std::env::set_var("TCANVAS_TMUX_SOCKET", &socket);
        std::env::set_var("TCANVAS_AGENT", &agent);
    }
    let cleanup = scopeguard(socket.clone(), dir.clone());

    // 1. Un nouveau terminal démarre une session tmux et affiche un shell.
    let mut h = harness(&dir);
    h.step();
    h.state_mut().dispatch(vec![Event::NewCard { at: None, project: None }]);
    h.step();
    let id = h.state().world.cards[0].id.clone();
    let session = format!("tc_{id}");
    wait(&mut h, "shell prêt", 30, |app| app.cards.is_running(&id) && !screen(app, &id).trim().is_empty());
    // Vérifié par un appel wsl.exe séparé, qui peut échouer ponctuellement : on réessaie.
    let deadline = Instant::now() + Duration::from_secs(10);
    let t = Instant::now();
    while !tmux_ok(&socket, &["has-session", "-t", &session]) {
        assert!(Instant::now() < deadline, "session {session} absente ; écran :\n{}\ntmux :\n{}", screen(h.state(), &id), tmux_report());
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!("✓ session vue par tmux en {} ms", t.elapsed().as_millis());
    wait(&mut h, "focus après le vol de caméra", 5, |app| app.world.focused.as_deref() == Some(id.as_str()));

    // 2. Le clavier arrive au shell et la sortie s'affiche.
    std::thread::sleep(Duration::from_millis(500));
    type_line(&mut h, "echo tcanvas-$((40+2))");
    wait(&mut h, "sortie de echo", 15, |app| screen(app, &id).contains("tcanvas-42"));

    // 3. L'agent est connecté et décrit la session.
    wait(&mut h, "agent connecté + statut inactif", 20, |app| {
        matches!(app.agent_state(), LinkState::Connected { .. }) && app.world.card(&id).is_some_and(|c| c.status() == Some(TermStatus::Idle))
    });

    // 4. Une commande muette devient « bloquée » et déclenche une notification.
    type_line(&mut h, "export TCANVAS_X=persist; sleep 6");
    h.state_mut().dispatch(vec![Event::SetStall(id.clone(), 1), Event::Unfocus]);
    wait(&mut h, "statut bloqué + alerte", 15, |app| {
        app.world.card(&id).is_some_and(|c| c.status() == Some(TermStatus::Stalled) && c.attention) && app.pending_toasts() == 1
    });
    wait(&mut h, "retour à l'invite", 15, |app| app.world.card(&id).is_some_and(|c| c.status() == Some(TermStatus::Idle)));

    // 5. On ferme l'application : la session tmux survit, et on la retrouve à la réouverture.
    h.state_mut().save_now();
    drop(h);
    std::thread::sleep(Duration::from_millis(500));
    assert!(tmux_ok(&socket, &["has-session", "-t", &session]), "la session survit à la fermeture");

    let mut h = harness(&dir);
    h.step();
    assert_eq!(h.state().world.cards.len(), 1, "carte restaurée depuis layout.json");
    assert_eq!(h.state().world.cards[0].stall_secs, 1, "réglage restauré");
    wait(&mut h, "rattachement", 30, |app| app.cards.is_running(&id) && !screen(app, &id).trim().is_empty());
    h.state_mut().dispatch(vec![Event::FocusCard(id.clone())]);
    h.step();
    type_line(&mut h, "echo got-$TCANVAS_X");
    wait(&mut h, "même shell qu'avant le redémarrage", 15, |app| screen(app, &id).contains("got-persist"));

    // 6. Fermer le terminal (Ctrl+Maj+W puis Entrée) arrête la session et TOUS ses processus,
    //    même ceux qui ignorent SIGHUP/SIGTERM ou se détachent, sans réadoption par l'agent.
    let base = 800_000 + std::process::id() % 100_000;
    let procs = start_stubborn(&mut h, base);
    h.key_press_modifiers(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, Key::W);
    h.step();
    h.step();
    h.key_press(Key::Enter);
    h.step();
    wait_closed(&mut h, &socket, &session, &procs);
    assert!(h.state().world.cards.is_empty(), "pas de réadoption d'une session tuée");

    // 7. Un groupe lié à un dossier (dépôt git) : ses terminaux s'y ouvrent et ce qu'on y écrit
    //    apparaît dans les changements. Puis fermer le groupe ferme tous ses terminaux, pas les autres.
    let out = in_wsl("mktemp").args(["-d", "/tmp/tcanvas-e2e-dossier-XXXXXX"]).output().unwrap();
    let linked = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(out.status.success() && !linked.is_empty(), "mktemp dans WSL");
    let has_git = in_wsl("git").args(["init", "-q", &linked]).status().is_ok_and(|s| s.success());
    let group = tcanvas::shell::world::Project {
        id: "groupe".into(),
        name: "groupe".into(),
        color: [94, 129, 244],
        rect: egui::Rect::from_min_size(egui::pos2(-3000.0, -3000.0), Vec2::new(4000.0, 2000.0)),
        cwd: Some(linked.clone()),
    };
    h.state_mut().world.projects.push(group.clone());
    let new_card = |h: &mut Harness<'_, App>, at: egui::Pos2, project: Option<String>| {
        let before: Vec<String> = h.state().world.cards.iter().map(|c| c.id.clone()).collect();
        h.state_mut().dispatch(vec![Event::NewCard { at: Some(at), project }]);
        h.step();
        let id = h.state().world.cards.iter().map(|c| c.id.clone()).find(|c| !before.contains(c)).unwrap();
        wait(h, "shell prêt", 30, |app| app.cards.is_running(&id) && !screen(app, &id).trim().is_empty());
        wait(h, "focus", 5, |app| app.world.focused.as_deref() == Some(id.as_str()));
        std::thread::sleep(Duration::from_millis(500));
        id
    };
    let in_a = new_card(&mut h, egui::pos2(-2500.0, -2500.0), Some("groupe".into()));
    wait(&mut h, "terminal ouvert dans le dossier lié", 15, |app| {
        app.world.card(&in_a).and_then(|c| c.info.as_ref()).and_then(|i| i.cwd.as_deref()) == Some(linked.as_str())
    });
    if has_git {
        type_line(&mut h, "printf 'un\\ndeux\\n' > note.txt");
        wait(&mut h, "fichier écrit vu dans les changements", 15, |app| {
            app.changes.set("groupe").is_some_and(|s| s.git && s.files.iter().any(|f| f.path == "note.txt" && f.kind == protocol::ChangeKind::Untracked && f.added == Some(2)))
        });
    } else {
        eprintln!("git absent dans WSL : suivi des changements non vérifié");
    }
    let procs_a = start_stubborn(&mut h, base + 10);
    let in_b = new_card(&mut h, egui::pos2(-500.0, -2500.0), Some("groupe".into()));
    let procs_b = start_stubborn(&mut h, base + 20);
    let outside = new_card(&mut h, egui::pos2(5000.0, 5000.0), None);
    type_line(&mut h, &format!("sleep {}", base + 30));
    wait(&mut h, "processus hors groupe", 10, |_| proc_alive(&format!("sleep {}", base + 30)));
    for id in [&in_a, &in_b] {
        assert_eq!(h.state().world.card(id).unwrap().project.as_deref(), Some("groupe"));
    }
    assert_eq!(h.state().world.card(&outside).unwrap().project, None);

    h.state_mut().dispatch(vec![Event::RequestCloseProject("groupe".into())]);
    h.step();
    h.step();
    h.key_press(Key::Enter);
    h.step();
    wait_closed(&mut h, &socket, &format!("tc_{in_a}"), &procs_a);
    wait_closed(&mut h, &socket, &format!("tc_{in_b}"), &procs_b);
    let app = h.state();
    assert!(app.world.project("groupe").is_none(), "cadre supprimé");
    assert_eq!(app.world.cards.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![outside.clone()], "seul le terminal hors groupe reste");
    assert!(tmux_ok(&socket, &["has-session", "-t", &format!("tc_{outside}")]), "sa session est intacte");
    assert!(proc_alive(&format!("sleep {}", base + 30)), "et ses processus aussi");
    let _ = in_wsl("rm").args(["-rf", &linked]).status();
    drop(cleanup);
}

/// Lance dans le terminal focalisé quatre processus difficiles à arrêter : un qui ignore
/// SIGHUP et SIGTERM, un `nohup`, un `setsid` (détaché de l'arbre et de la session) et une
/// commande au premier plan. Attend qu'ils tournent tous.
fn start_stubborn(h: &mut Harness<'_, App>, base: u32) -> Vec<String> {
    let procs: Vec<String> = (0..4).map(|k| format!("sleep {}", base + k)).collect();
    type_line(h, &format!("trap '' HUP TERM; {} &", procs[0]));
    type_line(h, &format!("nohup {} >/dev/null 2>&1 &", procs[1]));
    type_line(h, &format!("setsid {} &", procs[2]));
    type_line(h, &procs[3]);
    wait(h, "processus récalcitrants lancés", 15, |_| procs.iter().all(|p| proc_alive(p)));
    procs
}

/// Attend la disparition de la session et de chacun de ses processus.
fn wait_closed(h: &mut Harness<'_, App>, socket: &str, session: &str, procs: &[String]) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let session_alive = tmux_ok(socket, &["has-session", "-t", session]);
        let alive: Vec<&String> = procs.iter().filter(|p| proc_alive(p)).collect();
        if !session_alive && alive.is_empty() {
            eprintln!("✓ {session} et ses {} processus arrêtés", procs.len());
            return;
        }
        assert!(Instant::now() < deadline, "{session} : session vivante = {session_alive}, processus survivants = {alive:?}");
        h.step();
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Nettoie le serveur tmux et le dossier même si le test échoue.
struct Cleanup(String, PathBuf);

fn scopeguard(socket: String, dir: PathBuf) -> Cleanup {
    Cleanup(socket, dir)
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = in_wsl("tmux").arg("-L").arg(&self.0).arg("kill-server").output();
        // tmux laisse le fichier de socket derrière lui.
        let rm = format!("rm -f \"${{TMUX_TMPDIR:-/tmp}}/tmux-$(id -u)/{}\"", self.0);
        let _ = in_wsl("sh").args(["-c", &rm]).output();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

