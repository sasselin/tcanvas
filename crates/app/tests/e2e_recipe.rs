//! End to end: a worktree created from the dialog with the repository's recipe. Real PTYs
//! (ConPTY + wsl.exe on Windows), real tmux on a throwaway socket, real `tcanvas-agent`, a
//! throwaway git repository (git sees neither the user's nor the system's configuration) and a
//! scratch `HOME` for everything the cards run.
//!
//! Checks: the agent copies what the recipe says; without the tick, the setup command does not
//! run and the template command does; ticked, the setup runs once in the new card, before the
//! template command.
//!
//! On Windows, the Linux agent must be provided: `TCANVAS_E2E_AGENT=/wsl/path/tcanvas-agent`
//! (see `scripts/test-windows.sh`). Without it, the test is skipped.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use egui::Vec2;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use tcanvas::features::agent_link::{AgentMode, LinkState};
use tcanvas::features::terminal_card::Launcher;
use tcanvas::shell::event::Event;
use tcanvas::shell::i18n::{self, Lang};
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

fn sh(script: &str, args: &[&str]) -> String {
    let out = in_wsl("sh").args(["-c", script, "sh"]).args(args).output().unwrap();
    assert!(out.status.success(), "{script}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn read_wsl(path: &str) -> String {
    in_wsl("cat").arg(path).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn harness(dir: &std::path::Path) -> Harness<'static, App> {
    i18n::set(Lang::En);
    let dir = dir.to_path_buf();
    Harness::builder()
        .with_size(Vec2::new(1400.0, 900.0))
        .with_step_dt(1.0 / 60.0)
        .allow_missing_glyphs()
        .build_eframe(move |cc| App::new(&cc.egui_ctx, Config { launcher: Launcher::Tmux, agent: AgentMode::Spawn, config_dir: dir, os_toasts: false, update_check: false }))
}

/// Runs the app in real time until `cond` is true.
fn wait(h: &mut Harness<'_, App>, what: &str, secs: u64, cond: impl Fn(&Harness<'_, App>) -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        h.step();
        if cond(h) {
            eprintln!("✓ {what} in {} ms", start.elapsed().as_millis());
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let app = h.state();
    let screens: Vec<String> = app.world.cards.iter().map(|c| app.cards.screen_text(&c.id).unwrap_or_default()).collect();
    panic!("timed out: {what}\nagent: {:?}\nscreens:\n{}", app.agent_state(), screens.join("\n-----\n"));
}

/// Kills the throwaway tmux server, removes its socket, the config dir and the WSL folders, even
/// when the test fails.
struct Cleanup {
    socket: String,
    dir: PathBuf,
    folders: Vec<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = in_wsl("tmux").args(["-L", &self.socket, "kill-server"]).output();
        let rm = format!("rm -f \"${{TMUX_TMPDIR:-/tmp}}/tmux-$(id -u)/{}\"", self.socket);
        let _ = in_wsl("sh").args(["-c", &rm]).output();
        for path in self.folders.iter().filter(|p| !p.is_empty()) {
            let _ = in_wsl("rm").args(["-rf", path]).output();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Opens the new-worktree dialog of `shop`, names the branch, picks the template, ticks the
/// setup command or not, creates; returns the new worktree's folder once its card runs.
fn create(h: &mut Harness<'_, App>, repo: &str, branch: &str, tick: bool) -> String {
    h.state_mut().dispatch(vec![Event::NewWorktree("shop".into())]);
    wait(h, "the dialog", 10, |h| h.state().worktrees.create_open() && h.get_all_by_role(Role::TextInput).count() >= 2);
    h.event(egui::Event::PointerGone);
    let inputs = h.get_all_by_role(Role::TextInput).collect::<Vec<_>>();
    let field = &inputs[inputs.len() - 2];
    field.focus();
    field.type_text(branch);
    let path = format!("{repo}/.claude/worktrees/{branch}");
    wait(h, "the plan for the folder, with the recipe", 30, |h| h.query_by_label(&path).is_some() && h.query_by_label("Run this setup command").is_some());
    h.get_all_by_role(Role::ComboBox).find(|n| n.value().as_deref() == Some("Shell")).expect("the template list").click();
    h.step();
    h.step();
    h.get_by_label("Log").click();
    h.step();
    if tick {
        h.get_by_label("Run this setup command").click();
        h.step();
    }
    assert_eq!(h.query_by_label("The setup command will not run: only the copies and links are made.").is_none(), tick);
    h.get_by_label("Create").click();
    h.step();
    // Answered as soon as git is done: the dialog closes, never "no answer", while the copies run.
    wait(h, "the dialog closed", 30, |h| !h.state().worktrees.create_open());
    wait(h, "the recipe being applied", 5, |h| h.query_by_label("Copying and linking...").is_some());
    assert!(!h.state().world.cards.iter().any(|c| c.cwd.as_deref().is_some_and(|d| d.ends_with(&format!("/{branch}")))), "the card waits for the copies");
    wait(h, "the card in the new worktree", 60, |h| h.state().world.cards.iter().any(|c| c.cwd.as_deref() == Some(path.as_str()) && h.state().cards.is_running(&c.id)));
    path
}

#[test]
fn a_worktree_created_with_its_recipe() {
    if !in_wsl("tmux").arg("-V").output().is_ok_and(|o| o.status.success()) || !in_wsl("git").arg("--version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("tmux or git missing: test skipped");
        return;
    }
    let Some(agent) = agent_path() else {
        eprintln!("TCANVAS_E2E_AGENT missing: test skipped");
        return;
    };
    let socket = format!("tcanvas-e2e-recipe-{}", std::process::id());
    let dir = std::env::temp_dir().join(format!("tcanvas-e2e-recipe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY: only test in this binary, set before any other app thread exists.
    unsafe {
        std::env::set_var("TCANVAS_TMUX_SOCKET", &socket);
        std::env::set_var("TCANVAS_AGENT", &agent);
        // Git, here and in the agent, never sees the user's configuration (Windows passes these
        // to WSL through `WSLENV`).
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        // The agent takes this long before copying: a card started before the copies are done
        // would find no `.env`.
        std::env::set_var("TCANVAS_RECIPE_TEST_DELAY_MS", "1500");
        let wslenv = std::env::var("WSLENV").unwrap_or_default();
        let forwarded = "GIT_CONFIG_GLOBAL/u:GIT_CONFIG_NOSYSTEM/u:TCANVAS_RECIPE_TEST_DELAY_MS/u";
        std::env::set_var("WSLENV", format!("{forwarded}{}", if wslenv.is_empty() { String::new() } else { format!(":{wslenv}") }));
        if !cfg!(windows)
            && let Some(bash) = ["/usr/bin/bash", "/bin/bash"].into_iter().find(|p| std::path::Path::new(p).exists())
        {
            std::env::set_var("SHELL", bash);
        }
    }
    let mut cleanup = Cleanup { socket, dir: dir.clone(), folders: Vec::new() };
    let home = sh("mktemp -d /tmp/tcanvas-e2e-recipe-home-XXXXXX", &[]);
    cleanup.folders.push(home.clone());
    sh(r#"cd "$1" && : > .zshrc && : > .bashrc && echo '. ~/.bashrc' > .bash_profile"#, &[&home]);
    // SAFETY: as above.
    unsafe { std::env::set_var("TCANVAS_TEST_HOME", &home) };
    let repo = sh("mktemp -d /tmp/tcanvas-e2e-recipe-repo-XXXXXX", &[]);
    cleanup.folders.push(repo.clone());
    // Both commands need the copied `.env`: they run only after the copies.
    let recipe = "[worktree]\ncopy = [\".env\"]\nsetup = \"test -f .env && echo setup >> order.txt\"\n";
    let init = r#"cd "$1" && git init -q -b main && printf '.env\n.claude/\norder.txt\n' > .gitignore && printf '%s' "$2" > .tcanvas.toml && git add . && git -c user.name=T -c user.email=t@example.com -c commit.gpgsign=false commit -q -m init && printf 'TOKEN=x' > .env"#;
    sh(init, &[&repo, recipe]);

    let mut h = harness(&dir);
    h.step();
    h.state_mut().world.projects.push(tcanvas::shell::world::Project {
        id: "shop".into(),
        name: "shop".into(),
        color: [94, 129, 244],
        rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(3000.0, 1500.0)),
        cwd: Some(repo.clone()),
        templates: vec![tcanvas::shell::world::CommandTemplate { name: "Log".into(), command: "test -f .env && echo template >> order.txt".into(), task: false }],
        messages: Vec::new(),
        worktree_location: None,
        task_settings: Default::default(),
    });
    h.state_mut().dispatch(vec![Event::EnterSpace(Some("shop".into()))]);
    wait(&mut h, "the agent and the repository's worktrees", 30, |h| matches!(h.state().agent_state(), LinkState::Connected { .. }) && h.state().worktrees.list("shop").is_some());

    // 1. Not ticked: the copies, the template command, not the setup.
    let plain = create(&mut h, &repo, "plain", false);
    wait(&mut h, "the template command ran", 30, |_| read_wsl(&format!("{plain}/order.txt")) == "template\n");
    assert_eq!(read_wsl(&format!("{plain}/.env")), "TOKEN=x", "copied by the agent");
    wait(&mut h, "the report", 10, |h| h.query_by_label("Copied .env").is_some());
    h.get_by_label("OK").click();
    h.step();

    // 2. Ticked: the setup runs once in the new card, then the template command.
    let seeded = create(&mut h, &repo, "seeded", true);
    wait(&mut h, "setup, then the template command", 30, |_| read_wsl(&format!("{seeded}/order.txt")) == "setup\ntemplate\n");
    std::thread::sleep(Duration::from_millis(500));
    for _ in 0..5 {
        h.step();
    }
    assert_eq!(read_wsl(&format!("{seeded}/order.txt")), "setup\ntemplate\n", "once");
    assert_eq!(read_wsl(&format!("{plain}/order.txt")), "template\n", "the first worktree never ran it");
    assert!(sh(r#"ls -A "$1/.local/share/tcanvas/commands" 2>/dev/null || true"#, &[&home]).is_empty(), "nothing left staged");
    drop(h);
    drop(cleanup);
}
