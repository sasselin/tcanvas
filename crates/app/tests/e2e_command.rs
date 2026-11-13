//! End to end: cards started with a command. Real PTYs (ConPTY + wsl.exe on Windows) + real tmux
//! on throwaway sockets. On Linux, each scenario runs with bash, then zsh, as the user's shell
//! (`$SHELL` of the tmux server); on Windows, with the WSL user's own shell.
//!
//! Checks: an external command runs in the group's folder, exactly as written, then the shell
//! stays usable; Ctrl+C and Ctrl+Z on the command leave a working shell (Ctrl+Z: a stopped job,
//! `fg` brings it back); a long command (beyond any command-line limit) arrives whole; an app
//! restart does not run it again; a command whose session never starts is reported, with its
//! text kept.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tcanvas::features::agent_link::AgentMode;
use tcanvas::features::terminal_card::Launcher;
use tcanvas::shell::event::Event;
use tcanvas::{App, Config};

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

fn proc_alive(cmd: &str) -> bool {
    in_wsl("pgrep").args(["-fx", cmd]).output().is_ok_and(|o| o.status.success())
}

fn harness(dir: &Path) -> Harness<'static, App> {
    let dir = dir.to_path_buf();
    Harness::builder()
        .with_size(Vec2::new(1400.0, 900.0))
        .with_step_dt(1.0 / 60.0)
        .allow_missing_glyphs()
        .build_eframe(move |cc| App::new(&cc.egui_ctx, Config { launcher: Launcher::Tmux, agent: AgentMode::Disabled, config_dir: dir, os_toasts: false, update_check: false }))
}

fn wait(h: &mut Harness<'_, App>, what: &str, secs: u64, cond: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        h.step();
        if cond(h.state()) {
            eprintln!("✓ {what} in {} ms", start.elapsed().as_millis());
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let app = h.state();
    let screens: Vec<String> = app.world.cards.iter().map(|c| app.cards.screen_text(&c.id).unwrap_or_default()).collect();
    panic!("timed out: {what}\nscreens:\n{}", screens.join("\n-----\n"));
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

/// Kills the throwaway tmux server, removes its socket, the config dir and the WSL folder, even
/// when the test fails.
struct Cleanup {
    socket: String,
    dir: PathBuf,
    folder: String,
    /// Scratch `HOME` (Linux side) of everything the cards run: rc files and staged commands.
    home: String,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = in_wsl("tmux").args(["-L", &self.socket, "kill-server"]).output();
        let rm = format!("rm -f \"${{TMUX_TMPDIR:-/tmp}}/tmux-$(id -u)/{}\"", self.socket);
        let _ = in_wsl("sh").args(["-c", &rm]).output();
        for path in [&self.folder, &self.home].into_iter().filter(|p| !p.is_empty()) {
            let _ = in_wsl("rm").args(["-rf", path]).output();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A group linked to `folder`, and a card created in it with `command`; waits for its terminal.
fn command_card(h: &mut Harness<'_, App>, folder: &str, command: &str) -> String {
    if h.state().world.project("cmd").is_none() {
        let group = tcanvas::shell::world::Project {
            id: "cmd".into(),
            name: "cmd".into(),
            color: [94, 129, 244],
            rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(2000.0, 1500.0)),
            cwd: Some(folder.into()),
            templates: Vec::new(),
            messages: Vec::new(),
            worktree_location: None,
            task_settings: Default::default(),
            merge_strategy: None,
        };
        h.state_mut().world.projects.push(group);
    }
    let before: Vec<String> = h.state().world.cards.iter().map(|c| c.id.clone()).collect();
    h.state_mut().dispatch(vec![Event::NewCard { at: None, project: Some("cmd".into()), command: Some(command.into()), setup: None, cwd: None }]);
    h.step();
    let id = h.state().world.cards.iter().map(|c| c.id.clone()).find(|c| !before.contains(c)).unwrap();
    wait(h, "terminal up", 30, |app| app.cards.is_running(&id));
    wait(h, "focus", 5, |app| app.world.focused.as_deref() == Some(id.as_str()));
    id
}

/// The shells to try: on Linux bash and zsh when installed (path), on Windows the user's own.
fn shells() -> Vec<Option<String>> {
    if cfg!(windows) {
        return vec![None];
    }
    ["bash", "zsh"].iter().filter_map(|s| ["/usr/bin/", "/bin/"].iter().map(|d| format!("{d}{s}")).find(|p| Path::new(p).exists())).map(Some).collect()
}

#[test]
fn cards_started_with_a_command() {
    if !in_wsl("tmux").arg("-V").output().is_ok_and(|o| o.status.success()) {
        eprintln!("tmux missing: test skipped");
        return;
    }
    for (k, shell) in shells().into_iter().enumerate() {
        let name = shell.as_deref().map(|s| s.rsplit('/').next().unwrap_or(s)).unwrap_or("default").to_string();
        eprintln!("--- shell: {name}");
        let socket = format!("tcanvas-e2e-cmd-{name}-{}", std::process::id());
        let dir = std::env::temp_dir().join(format!("tcanvas-e2e-cmd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: no app thread is running between two scenarios (each harness is dropped).
        unsafe {
            std::env::set_var("TCANVAS_TMUX_SOCKET", &socket);
            if let Some(path) = &shell {
                std::env::set_var("SHELL", path);
            }
        }
        let mut cleanup = Cleanup { socket: socket.clone(), dir: dir.clone(), folder: String::new(), home: String::new() };
        let folder = mktemp_wsl("/tmp/tcanvas-e2e-cmd it's-XXXXXX");
        cleanup.folder = folder.clone();
        // Never the user's real home: neither their rc files nor their data folder. The app
        // forwards this `HOME` to everything a card runs on the Linux side (through `wsl.exe`).
        let home = mktemp_wsl("/tmp/tcanvas-e2e-home-XXXXXX");
        cleanup.home = home.clone();
        let rc = r#"cd "$1" && : > .zshrc && : > .bashrc && echo '. ~/.bashrc' > .bash_profile"#;
        assert!(in_wsl("sh").args(["-c", rc, "rc", &home]).status().unwrap().success());
        // SAFETY: as above.
        unsafe { std::env::set_var("TCANVAS_TEST_HOME", &home) };
        scenario(&dir, &folder, &name, k == 0);
        drop(cleanup);
    }
}

/// A new folder on the Linux side.
fn mktemp_wsl(template: &str) -> String {
    let out = in_wsl("mktemp").args(["-d", template]).output().unwrap();
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(out.status.success() && !path.is_empty(), "mktemp in WSL");
    path
}

fn scenario(dir: &Path, folder: &str, shell: &str, first: bool) {
    let mut h = harness(dir);
    h.step();
    let base = 700_000 + std::process::id() % 100_000;

    // 1. A single external command, quotes and `$` it must not expand: its output, in the
    //    group's folder, then the shell takes typing.
    let a = command_card(&mut h, folder, "env printf '%s|%s|%s\\n' \"it's \\\"q\\\" \\$HOME\" '$HOME' \"$(pwd)\"");
    wait(&mut h, "command output", 30, |app| screen(app, &a).contains(&format!("it's \"q\" $HOME|$HOME|{folder}")));
    wait(&mut h, "session seen", 10, |app| !app.cards.awaiting_session(&a));
    std::thread::sleep(Duration::from_millis(500));
    type_line(&mut h, "echo typed-$((1+2))");
    wait(&mut h, "typing in the shell that follows", 15, |app| screen(app, &a).contains("typed-3"));

    // 2. Ctrl+C on the card's command: back at a usable prompt.
    let sleep_int = format!("sleep {base}");
    let b = command_card(&mut h, folder, &sleep_int);
    wait(&mut h, "command running", 30, |_| proc_alive(&sleep_int));
    h.event(egui::Event::Copy);
    h.step();
    wait(&mut h, "interrupted", 15, |_| !proc_alive(&sleep_int));
    std::thread::sleep(Duration::from_millis(300));
    type_line(&mut h, "echo after-int-$((2+2))");
    wait(&mut h, "prompt after Ctrl+C", 15, |app| screen(app, &b).contains("after-int-4"));

    // 3. Ctrl+Z on the card's command: a stopped job of the shell; `fg` brings it back.
    let sleep_stop = format!("sleep {}", base + 1);
    let c = command_card(&mut h, folder, &sleep_stop);
    wait(&mut h, "command running", 30, |_| proc_alive(&sleep_stop));
    h.key_press_modifiers(Modifiers::CTRL, Key::Z);
    h.step();
    std::thread::sleep(Duration::from_millis(300));
    type_line(&mut h, "jobs; echo jobs-listed");
    wait(&mut h, "stopped job listed", 15, |app| {
        let s = screen(app, &c);
        let stopped = match shell {
            "bash" => s.contains("Stopped"),
            "zsh" => s.contains("suspended"),
            _ => s.contains("Stopped") || s.contains("suspended"),
        };
        s.contains("jobs-listed") && stopped
    });
    type_line(&mut h, "fg");
    std::thread::sleep(Duration::from_millis(500));
    assert!(proc_alive(&sleep_stop), "brought back");
    h.event(egui::Event::Copy);
    h.step();
    wait(&mut h, "ended", 15, |_| !proc_alive(&sleep_stop));
    std::thread::sleep(Duration::from_millis(300));
    type_line(&mut h, "echo after-stop-$((3+3))");
    wait(&mut h, "prompt after Ctrl+Z", 15, |app| screen(app, &c).contains("after-stop-6"));
    for id in [&a, &b, &c] {
        assert_eq!(h.state().cards.lost_command(id), None);
    }
    if !first {
        return;
    }

    // 4. A long command (9000 bytes of CJK text: far beyond what tmux or Windows accept on a
    //    command line) on two lines, counted on the Linux side; then a restart does not run it
    //    again.
    let long = "日".repeat(3000);
    let d = command_card(&mut h, folder, &format!("printf '%s' '{long}' | wc -c\necho second-line; echo once >> ran.txt"));
    wait(&mut h, "long command output", 30, |app| {
        let s = screen(app, &d);
        s.lines().any(|l| l.trim() == "9000") && s.lines().any(|l| l.trim() == "second-line")
    });
    let runs = || in_wsl("cat").arg(format!("{folder}/ran.txt")).output().map(|o| String::from_utf8_lossy(&o.stdout).lines().count()).unwrap_or(0);
    assert_eq!(runs(), 1);
    h.state_mut().save_now();
    drop(h);
    std::thread::sleep(Duration::from_millis(500));
    let mut h = harness(dir);
    h.step();
    wait(&mut h, "reattach", 30, |app| app.cards.is_running(&d) && screen(app, &d).contains("second-line"));
    h.state_mut().dispatch(vec![Event::FocusCard(d.clone())]);
    h.step();
    type_line(&mut h, "echo again-$((1+1))");
    wait(&mut h, "same shell after the restart", 15, |app| screen(app, &d).contains("again-2"));
    assert_eq!(runs(), 1, "not run again by the restart");
    h.state_mut().save_now();
    drop(h);

    // 5. tmux cannot start (its socket folder does not exist): the card says its command did
    //    not run, and keeps the text for copying.
    // SAFETY: the previous harness is dropped, no app thread runs.
    unsafe { std::env::set_var("TCANVAS_TMUX_SOCKET", "missing-dir/socket") };
    let mut h = harness(dir);
    h.step();
    let before: Vec<String> = h.state().world.cards.iter().map(|c| c.id.clone()).collect();
    h.state_mut().dispatch(vec![Event::NewCard { at: None, project: None, command: Some("echo never".into()), setup: None, cwd: None }]);
    h.step();
    let e = h.state().world.cards.iter().map(|c| c.id.clone()).find(|c| !before.contains(c)).unwrap();
    wait(&mut h, "command reported as not run", 30, |app| app.cards.lost_command(&e) == Some("echo never"));
    steps_until_label(&mut h, (tcanvas::shell::i18n::t()).command_not_run);
    assert!(h.query_by_label((tcanvas::shell::i18n::t()).copy_command).is_some());
}

fn steps_until_label(h: &mut Harness<'_, App>, label: &str) {
    for _ in 0..120 {
        h.step();
        if h.query_by_label(label).is_some() {
            return;
        }
    }
    panic!("{label:?} never shown");
}
