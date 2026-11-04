//! End to end: real PTYs (ConPTY + wsl.exe on Windows) + real tmux (throwaway socket) + real
//! `tcanvas-agent`. Checks typing, status, the stall notification, terminals surviving an
//! app restart, and closing.
//!
//! On Windows, the Linux agent must be provided: `TCANVAS_E2E_AGENT=/wsl/path/tcanvas-agent`
//! (see `scripts/test-windows.sh`). Without it, the test is skipped.

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

/// Command run in WSL (directly on Linux).
fn in_wsl(program: &str) -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("wsl.exe");
        cmd.args(["-e", program]);
        cmd
    } else {
        Command::new(program)
    }
}

/// Path (on the WSL side) of the agent binary.
fn agent_path() -> Option<String> {
    if cfg!(windows) {
        return std::env::var("TCANVAS_E2E_AGENT").ok();
    }
    let ok = Command::new(env!("CARGO")).args(["build", "-q", "-p", "tcanvas-agent"]).current_dir(workspace()).status().unwrap().success();
    assert!(ok, "building the agent");
    let target = std::env::var("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|_| workspace().join("target"));
    Some(target.join("debug/tcanvas-agent").display().to_string())
}

/// Every session of every tmux socket of the user, for diagnostics.
fn tmux_report() -> String {
    let script = r#"for s in "${TMUX_TMPDIR:-/tmp}"/tmux-$(id -u)/*; do echo "$s:"; tmux -S "$s" ls 2>&1 | sed 's/^/  /'; done"#;
    in_wsl("sh").args(["-c", script]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Is a process whose command line is exactly `cmd` running in WSL?
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

/// Runs the app in real time until `cond` is true.
fn wait(h: &mut Harness<'_, App>, what: &str, secs: u64, cond: impl Fn(&App) -> bool) {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(secs);
    while Instant::now() < deadline {
        h.step();
        if cond(h.state()) {
            eprintln!("✓ {what} in {} ms", start.elapsed().as_millis());
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let app = h.state();
    let screens: Vec<String> = app.world.cards.iter().map(|c| app.cards.screen_text(&c.id).unwrap_or_default()).collect();
    panic!("timed out: {what}\nagent: {:?}\ncards: {:#?}\nscreens:\n{}", app.agent_state(), app.world.cards, screens.join("\n-----\n"));
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
        eprintln!("tmux missing: test skipped");
        return;
    }
    let socket = format!("tcanvas-e2e-{}", std::process::id());
    let dir = std::env::temp_dir().join(format!("tcanvas-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let Some(agent) = agent_path() else {
        eprintln!("TCANVAS_E2E_AGENT missing: test skipped");
        return;
    };
    // SAFETY: only test in this binary, set before any other app thread exists.
    unsafe {
        std::env::set_var("TCANVAS_TMUX_SOCKET", &socket);
        std::env::set_var("TCANVAS_AGENT", &agent);
    }
    let cleanup = scopeguard(socket.clone(), dir.clone());

    // 1. A new terminal starts a tmux session and shows a shell.
    let mut h = harness(&dir);
    h.step();
    h.state_mut().dispatch(vec![Event::NewCard { at: None, project: None }]);
    h.step();
    let id = h.state().world.cards[0].id.clone();
    let session = format!("tc_{id}");
    wait(&mut h, "shell ready", 30, |app| app.cards.is_running(&id) && !screen(app, &id).trim().is_empty());
    // Checked through a separate wsl.exe call, which can fail occasionally: retry.
    let deadline = Instant::now() + Duration::from_secs(10);
    let t = Instant::now();
    while !tmux_ok(&socket, &["has-session", "-t", &session]) {
        assert!(Instant::now() < deadline, "session {session} missing; screen:\n{}\ntmux:\n{}", screen(h.state(), &id), tmux_report());
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!("✓ session seen by tmux in {} ms", t.elapsed().as_millis());
    wait(&mut h, "focus after the camera flight", 5, |app| app.world.focused.as_deref() == Some(id.as_str()));

    // 2. Keystrokes reach the shell and the output is displayed.
    std::thread::sleep(Duration::from_millis(500));
    type_line(&mut h, "echo tcanvas-$((40+2))");
    wait(&mut h, "echo output", 15, |app| screen(app, &id).contains("tcanvas-42"));

    // 3. The agent is connected and describes the session.
    wait(&mut h, "agent connected + idle status", 20, |app| {
        matches!(app.agent_state(), LinkState::Connected { .. }) && app.world.card(&id).is_some_and(|c| c.status() == Some(TermStatus::Idle))
    });

    // 4. A silent command becomes "stalled" and triggers a notification.
    type_line(&mut h, "export TCANVAS_X=persist; sleep 6");
    h.state_mut().dispatch(vec![Event::SetStall(id.clone(), 1), Event::Unfocus]);
    wait(&mut h, "stalled status + alert", 15, |app| {
        app.world.card(&id).is_some_and(|c| c.status() == Some(TermStatus::Stalled) && c.attention) && app.pending_toasts() == 1
    });
    wait(&mut h, "back to the prompt", 15, |app| app.world.card(&id).is_some_and(|c| c.status() == Some(TermStatus::Idle)));

    // 5. Close the app: the tmux session survives and is found again on reopen.
    h.state_mut().save_now();
    drop(h);
    std::thread::sleep(Duration::from_millis(500));
    assert!(tmux_ok(&socket, &["has-session", "-t", &session]), "the session survives closing");

    let mut h = harness(&dir);
    h.step();
    assert_eq!(h.state().world.cards.len(), 1, "card restored from layout.json");
    assert_eq!(h.state().world.cards[0].stall_secs, 1, "setting restored");
    wait(&mut h, "reattach", 30, |app| app.cards.is_running(&id) && !screen(app, &id).trim().is_empty());
    h.state_mut().dispatch(vec![Event::FocusCard(id.clone())]);
    h.step();
    type_line(&mut h, "echo got-$TCANVAS_X");
    wait(&mut h, "same shell as before the restart", 15, |app| screen(app, &id).contains("got-persist"));

    // 6. Closing the terminal (Ctrl+Shift+W then Enter) stops the session and ALL its processes,
    //    even those that ignore SIGHUP/SIGTERM or detach, with no re-adoption by the agent.
    let base = 800_000 + std::process::id() % 100_000;
    let procs = start_stubborn(&mut h, base);
    h.key_press_modifiers(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, Key::W);
    h.step();
    h.step();
    h.key_press(Key::Enter);
    h.step();
    wait_closed(&mut h, &socket, &session, &procs);
    assert!(h.state().world.cards.is_empty(), "no re-adoption of a killed session");

    // 7. A group linked to a folder (git repo): its terminals open there and what is written there
    //    shows up in the changes. Then closing the group closes all its terminals, not the others.
    let out = in_wsl("mktemp").args(["-d", "/tmp/tcanvas-e2e-folder-XXXXXX"]).output().unwrap();
    let linked = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(out.status.success() && !linked.is_empty(), "mktemp in WSL");
    let has_git = in_wsl("git").args(["init", "-q", &linked]).status().is_ok_and(|s| s.success());
    let group = tcanvas::shell::world::Project {
        id: "group".into(),
        name: "group".into(),
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
        wait(h, "shell ready", 30, |app| app.cards.is_running(&id) && !screen(app, &id).trim().is_empty());
        wait(h, "focus", 5, |app| app.world.focused.as_deref() == Some(id.as_str()));
        std::thread::sleep(Duration::from_millis(500));
        id
    };
    let in_a = new_card(&mut h, egui::pos2(-2500.0, -2500.0), Some("group".into()));
    wait(&mut h, "terminal opened in the linked folder", 15, |app| {
        app.world.card(&in_a).and_then(|c| c.info.as_ref()).and_then(|i| i.cwd.as_deref()) == Some(linked.as_str())
    });
    if has_git {
        type_line(&mut h, "printf 'one\\ntwo\\n' > note.txt");
        wait(&mut h, "written file seen in the changes", 15, |app| {
            app.changes.set("group").is_some_and(|s| s.git && s.files.iter().any(|f| f.path == "note.txt" && f.kind == protocol::ChangeKind::Untracked && f.added == Some(2)))
        });
    } else {
        eprintln!("git missing in WSL: change tracking not checked");
    }
    let procs_a = start_stubborn(&mut h, base + 10);
    let in_b = new_card(&mut h, egui::pos2(-500.0, -2500.0), Some("group".into()));
    let procs_b = start_stubborn(&mut h, base + 20);
    let outside = new_card(&mut h, egui::pos2(5000.0, 5000.0), None);
    type_line(&mut h, &format!("sleep {}", base + 30));
    wait(&mut h, "process outside the group", 10, |_| proc_alive(&format!("sleep {}", base + 30)));
    for id in [&in_a, &in_b] {
        assert_eq!(h.state().world.card(id).unwrap().project.as_deref(), Some("group"));
    }
    assert_eq!(h.state().world.card(&outside).unwrap().project, None);

    h.state_mut().dispatch(vec![Event::RequestCloseProject("group".into())]);
    h.step();
    h.step();
    h.key_press(Key::Enter);
    h.step();
    wait_closed(&mut h, &socket, &format!("tc_{in_a}"), &procs_a);
    wait_closed(&mut h, &socket, &format!("tc_{in_b}"), &procs_b);
    let app = h.state();
    assert!(app.world.project("group").is_none(), "frame removed");
    assert_eq!(app.world.cards.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![outside.clone()], "only the terminal outside the group remains");
    assert!(tmux_ok(&socket, &["has-session", "-t", &format!("tc_{outside}")]), "its session is intact");
    assert!(proc_alive(&format!("sleep {}", base + 30)), "and so are its processes");
    let _ = in_wsl("rm").args(["-rf", &linked]).status();
    drop(cleanup);
}

/// Starts four hard-to-stop processes in the focused terminal: one that ignores
/// SIGHUP and SIGTERM, a `nohup`, a `setsid` (detached from the tree and the session) and a
/// foreground command. Waits until they are all running.
fn start_stubborn(h: &mut Harness<'_, App>, base: u32) -> Vec<String> {
    let procs: Vec<String> = (0..4).map(|k| format!("sleep {}", base + k)).collect();
    type_line(h, &format!("trap '' HUP TERM; {} &", procs[0]));
    type_line(h, &format!("nohup {} >/dev/null 2>&1 &", procs[1]));
    type_line(h, &format!("setsid {} &", procs[2]));
    type_line(h, &procs[3]);
    wait(h, "stubborn processes started", 15, |_| procs.iter().all(|p| proc_alive(p)));
    procs
}

/// Waits for the session and each of its processes to disappear.
fn wait_closed(h: &mut Harness<'_, App>, socket: &str, session: &str, procs: &[String]) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let session_alive = tmux_ok(socket, &["has-session", "-t", session]);
        let alive: Vec<&String> = procs.iter().filter(|p| proc_alive(p)).collect();
        if !session_alive && alive.is_empty() {
            eprintln!("✓ {session} and its {} processes stopped", procs.len());
            return;
        }
        assert!(Instant::now() < deadline, "{session}: session alive = {session_alive}, surviving processes = {alive:?}");
        h.step();
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Cleans up the tmux server and the folder even if the test fails.
struct Cleanup(String, PathBuf);

fn scopeguard(socket: String, dir: PathBuf) -> Cleanup {
    Cleanup(socket, dir)
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = in_wsl("tmux").arg("-L").arg(&self.0).arg("kill-server").output();
        // tmux leaves the socket file behind.
        let rm = format!("rm -f \"${{TMUX_TMPDIR:-/tmp}}/tmux-$(id -u)/{}\"", self.0);
        let _ = in_wsl("sh").args(["-c", &rm]).output();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

