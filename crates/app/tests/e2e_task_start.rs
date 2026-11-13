//! End to end: a task started from the Tasks panel. Real PTYs (ConPTY + wsl.exe on Windows),
//! real tmux on a throwaway socket, real `tcanvas-agent`, a throwaway git repository (git sees
//! neither the user's nor the system's configuration) and a scratch `HOME` for everything the
//! cards run and for the task list.
//!
//! The group's task command is a fake coding agent: a script that records its arguments and
//! environment, waits until the task records its session, runs `task prime`, then does what a
//! coding agent's hooks report (`tcanvas-agent hook UserPromptSubmit` with a payload), waits for
//! the task to be active, commits, reports its stop (`hook Stop`), and waits for the user.
//!
//! Checks: Start creates the worktree and the card; the fake agent received exactly the task's
//! prompt (byte for byte, hostile text included) and nothing in it ran; the task became active,
//! then review; its links are set; `task prime` in that session shows it as the current task;
//! closing the card keeps the worktree, the task, its links and its status; Resume reopens a card
//! in the same worktree (none created) and the agent gets the same prompt again.
//!
//! On Windows, the Linux agent must be provided: `TCANVAS_E2E_AGENT=/wsl/path/tcanvas-agent`
//! (see `scripts/test-windows.sh`). Without it, the test is skipped.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use egui::Vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use tcanvas::features::agent_link::{AgentMode, LinkState};
use tcanvas::features::tasks::start::build_prompt;
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

/// A file's bytes on the WSL side (empty when missing).
fn read_wsl(path: &str) -> Vec<u8> {
    in_wsl("cat").arg(path).output().map(|o| o.stdout).unwrap_or_default()
}

fn exists_wsl(path: &str) -> bool {
    in_wsl("test").args(["-e", path]).status().is_ok_and(|s| s.success())
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
    let said: Vec<String> = ["not started", "Asking", "stopped before", "refused", "exists", "Start task", "New worktree", "Folder"]
        .iter()
        .filter_map(|l| h.query_by_label_contains(l).map(|n| n.accesskit_node().label().unwrap_or_default()))
        .collect();
    panic!("timed out: {what}\nagent: {:?}\nsaid: {said:?}\ncreations waiting: {}\nscreens:\n{}", app.agent_state(), app.worktrees.creations_waiting(), screens.join("\n-----\n"));
}

/// Clicks `label` once it stays in place from one frame to the next (a dialog centers itself
/// again when its content changes).
fn click(h: &mut Harness<'_, App>, label: &str) {
    let mut last = None;
    for _ in 0..120 {
        h.step();
        let rect = h.query_by_label(label).map(|n| n.rect());
        if rect.is_some() && rect == last {
            break;
        }
        last = rect;
    }
    h.get_by_label(label).click();
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

/// The fake coding agent. `$1`..`$4`: `{task}`, `{task_ref}`, `{task_title}`, `{task_file}`.
/// Each run records into its own folder (`run-<n>` in the home); it stays until `release-<n>` is
/// there, as an agent waiting for the user would.
const FAKE_AGENT: &str = r#"#!/bin/bash
agent="__AGENT__"
n=1; while [ -e "$HOME/run-$n" ]; do n=$((n+1)); done
d="$HOME/run-$n"; mkdir "$d"
printf '%s' "$#" > "$d/argc"
printf '%s' "$1" > "$d/arg1"; printf '%s' "$2" > "$d/arg2"; printf '%s' "$3" > "$d/arg3"; printf '%s' "$4" > "$d/arg4"
cat -- "$4" > "$d/file"
printf '%s|%s|%s' "$TCANVAS_TASK_REF" "$TCANVAS_TASK_FILE" "$PWD" > "$d/env"
# The task records where it runs: then `task prime` names it.
i=0; until "$agent" task show "$2" | grep -q '^worktree: '; do i=$((i+1)); [ $i -lt 300 ] || break; sleep 0.1; done
"$agent" task prime > "$d/prime" 2>&1
printf '{"session_id":"fake-%s","prompt":"fake prompt %s"}' "$n" "$n" | "$agent" hook UserPromptSubmit > /dev/null
i=0; until "$agent" task show "$2" | grep -q '^status: active'; do i=$((i+1)); [ $i -lt 300 ] || break; sleep 0.1; done
"$agent" task show "$2" | grep '^status: ' > "$d/seen"
git -c user.name=Fake -c user.email=fake@example.com -c commit.gpgsign=false commit -q --allow-empty -m "fake work $n"
printf '{"session_id":"fake-%s"}' "$n" | "$agent" hook Stop > /dev/null
touch "$d/stopped"
i=0; while [ ! -e "$HOME/release-$n" ] && [ $i -lt 1200 ]; do i=$((i+1)); sleep 0.1; done
"#;

/// The task as the agent's task list has it now (`task show --json`).
fn stored(repo: &str, home: &str, agent: &str, reference: &str) -> serde_json::Value {
    let json = sh(r#"cd "$1" && TCANVAS_TEST_HOME="$2" HOME="$2" "$3" task show "$4" --json"#, &[repo, home, agent, reference]);
    serde_json::from_str(&json).unwrap()
}

#[test]
fn a_task_started_from_the_panel() {
    if !in_wsl("tmux").arg("-V").output().is_ok_and(|o| o.status.success()) || !in_wsl("git").arg("--version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("tmux or git missing: test skipped");
        return;
    }
    if !in_wsl("bash").arg("--version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("bash missing (the fake agent and the card's shell): test skipped");
        return;
    }
    let Some(agent) = agent_path() else {
        eprintln!("TCANVAS_E2E_AGENT missing: test skipped");
        return;
    };
    let socket = format!("tcanvas-e2e-task-{}", std::process::id());
    let dir = std::env::temp_dir().join(format!("tcanvas-e2e-task-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY: only test in this binary, set before any other app thread exists.
    unsafe {
        std::env::set_var("TCANVAS_TMUX_SOCKET", &socket);
        std::env::set_var("TCANVAS_AGENT", &agent);
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        let wslenv = std::env::var("WSLENV").unwrap_or_default();
        let forwarded = "GIT_CONFIG_GLOBAL/u:GIT_CONFIG_NOSYSTEM/u";
        std::env::set_var("WSLENV", format!("{forwarded}{}", if wslenv.is_empty() { String::new() } else { format!(":{wslenv}") }));
        if !cfg!(windows)
            && let Some(bash) = ["/usr/bin/bash", "/bin/bash"].into_iter().find(|p| std::path::Path::new(p).exists())
        {
            std::env::set_var("SHELL", bash);
        }
    }
    let mut cleanup = Cleanup { socket, dir: dir.clone(), folders: Vec::new() };
    let home = sh("mktemp -d /tmp/tcanvas-e2e-task-home-XXXXXX", &[]);
    cleanup.folders.push(home.clone());
    sh(r#"cd "$1" && : > .zshrc && : > .bashrc && echo '. ~/.bashrc' > .bash_profile"#, &[&home]);
    // SAFETY: as above.
    unsafe { std::env::set_var("TCANVAS_TEST_HOME", &home) };
    let repo = sh("mktemp -d /tmp/tcanvas-e2e-task-repo-XXXXXX", &[]);
    cleanup.folders.push(repo.clone());
    let init = r#"cd "$1" && git init -q -b main && printf '.claude/\n' > .gitignore && git add . && git -c user.name=T -c user.email=t@example.com -c commit.gpgsign=false commit -q -m init"#;
    sh(init, &[&repo]);
    let script = format!("{home}/fake-agent.sh");
    sh(r#"printf '%s' "$2" > "$1""#, &[&script, &FAKE_AGENT.replace("__AGENT__", &agent)]);

    // A task written to hurt, filed as an agent would (the command line).
    let title = "-x Fix \"quotes\" 'single' $(touch pwned-title) `touch pwned-tick`; rm -rf ~ {task} !! \\ end";
    let mut description = String::from("$(touch pwned-desc)\n`touch pwned-back`; touch pwned-semi\n-rf --force\n{task} {task_file} {task_ref} ${HOME}\n");
    description.push_str("# a comment line\nEOF\n'\"'\"'\n!important !! !$\nback\\slash \\\\ \\n\ttab trailing   \n\n\ncaf\u{e9} \u{65e5}\u{672c}\n");
    description.push_str(&"long line of a task description, ".repeat(600));
    let add = r#"cd "$1" && printf '%s' "$4" | TCANVAS_TEST_HOME="$2" HOME="$2" "$3" task add --description-stdin --status todo -- "$5""#;
    let reference = sh(add, &[&repo, &home, &agent, &description, title]).trim_start_matches('#').to_string();
    assert!(!reference.is_empty());

    let mut h = harness(&dir);
    h.step();
    let command = format!("bash '{script}' {{task}} {{task_ref}} {{task_title}} {{task_file}}");
    h.state_mut().world.projects.push(tcanvas::shell::world::Project {
        id: "shop".into(),
        name: "shop".into(),
        color: [94, 129, 244],
        rect: egui::Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(3000.0, 1500.0)),
        cwd: Some(repo.clone()),
        templates: vec![tcanvas::shell::world::CommandTemplate { name: "Fake agent".into(), command, task: true }],
        messages: Vec::new(),
        worktree_location: None,
        task_settings: Default::default(),
    });
    h.state_mut().dispatch(vec![Event::EnterSpace(Some("shop".into())), Event::ToggleTasks]);
    wait(&mut h, "the agent, the repository and the task list", 30, |h| {
        matches!(h.state().agent_state(), LinkState::Connected { .. }) && h.state().worktrees.list("shop").is_some() && h.query_by_label("Start").is_some_and(|n| !n.accesskit_node().is_disabled())
    });
    let task = h.state().tasks.board("shop").and_then(|b| b.list.as_ref()).and_then(|l| l.tasks.iter().find(|t| t.reference == reference)).expect("the task").clone();
    assert_eq!((task.title.as_str(), task.description.as_str()), (title, description.trim_end()), "stored as written (the command line trims the end)");
    let prompt = build_prompt(None, &task, false);

    // 1. Start: the confirmation, then the worktree, the card, the agent with the task.
    click(&mut h, "Start");
    wait(&mut h, "the confirmation, once the agent planned the worktree", 30, |h| h.query_by_label("Start task").is_some_and(|n| !n.accesskit_node().is_disabled()));
    click(&mut h, "Start task");
    let branch = format!("task/x-fix-quotes-single-touch-pwned-title-{reference}");
    let path = format!("{repo}/.claude/worktrees/{}", protocol::folder_for_branch(&branch));
    wait(&mut h, "the card in the new worktree", 60, |h| h.state().world.cards.iter().any(|c| c.cwd.as_deref() == Some(path.as_str()) && h.state().cards.is_running(&c.id)));
    let card = h.state().world.cards.iter().find(|c| c.cwd.as_deref() == Some(path.as_str())).unwrap().clone();
    let run = format!("{home}/run-1");
    wait(&mut h, "the fake agent stopped", 60, |_| exists_wsl(&format!("{run}/stopped")));
    assert_eq!(read_wsl(&format!("{run}/argc")), b"4");
    assert!(read_wsl(&format!("{run}/arg1")) == prompt.as_bytes(), "the prompt, byte for byte:\n{}", String::from_utf8_lossy(&read_wsl(&format!("{run}/arg1"))));
    assert_eq!(read_wsl(&format!("{run}/arg2")), reference.as_bytes());
    assert_eq!(read_wsl(&format!("{run}/arg3")), title.as_bytes());
    assert!(read_wsl(&format!("{run}/file")) == format!("{prompt}\n").as_bytes(), "the prompt file");
    let env = String::from_utf8(read_wsl(&format!("{run}/env"))).unwrap();
    assert_eq!(env, format!("{reference}|{home}/.local/share/tcanvas/commands/{}-task|{path}", card.session()));
    let pwned = sh(r#"find "$1" "$2" -maxdepth 1 -name 'pwned*'"#, &[&home, &path]);
    assert!(pwned.is_empty(), "the task's text ran: {pwned}");
    // `task prime` in that session showed it as the current task.
    let prime = String::from_utf8(read_wsl(&format!("{run}/prime"))).unwrap();
    assert!(prime.contains(&format!("current task (linked to this session):\n#{reference}  ")), "{prime}");

    // 2. Its status followed the agent: active on the prompt, review once stopped with a commit.
    assert_eq!(read_wsl(&format!("{run}/seen")), b"status: active\n", "active after the prompt");
    let status = |h: &Harness<'_, App>| h.state().tasks.board("shop").and_then(|b| b.list.as_ref()).and_then(|l| l.tasks.iter().find(|t| t.reference == reference)).map(|t| t.status);
    wait(&mut h, "the task in review", 30, |h| status(h) == Some(protocol::TaskStatus::Review));
    let stored_task = stored(&repo, &home, &agent, &reference);
    assert_eq!(stored_task["status"], "review");
    assert_eq!((stored_task["worktree"].as_str(), stored_task["branch"].as_str(), stored_task["session"].as_str()), (Some(path.as_str()), Some(branch.as_str()), Some(card.session().as_str())));
    wait(&mut h, "the card's header names the task", 10, |h| h.query_by_label(&format!("Task #{reference}: {}", task.title)).is_some());

    // 3. Closing the card keeps the worktree, the task, its links and its status.
    sh(r#"touch "$1/release-1""#, &[&home]);
    h.state_mut().dispatch(vec![Event::KillCard(card.id.clone())]);
    wait(&mut h, "the card closed, Resume offered", 30, |h| h.query_by_label("Resume").is_some_and(|n| !n.accesskit_node().is_disabled()) && h.query_by_label("card closed").is_some());
    let kept = stored(&repo, &home, &agent, &reference);
    assert_eq!((kept["status"].as_str(), kept["worktree"].as_str(), kept["session"].as_str()), (Some("review"), Some(path.as_str()), Some(card.session().as_str())));
    assert!(exists_wsl(&path), "the worktree stays");

    // 4. Resume: a card in the same worktree, none created; the agent gets the same prompt again.
    click(&mut h, "Resume");
    wait(&mut h, "the confirmation of the reuse", 10, |h| h.query_by_label(&format!("Reuses its worktree {path} (branch {branch}).")).is_some());
    click(&mut h, "Start task");
    wait(&mut h, "a new card in the same worktree", 60, |h| h.state().world.cards.iter().any(|c| c.id != card.id && c.cwd.as_deref() == Some(path.as_str()) && h.state().cards.is_running(&c.id)));
    let again = h.state().world.cards.iter().find(|c| c.id != card.id && c.cwd.as_deref() == Some(path.as_str())).unwrap().clone();
    let run = format!("{home}/run-2");
    wait(&mut h, "the fake agent stopped again", 60, |_| exists_wsl(&format!("{run}/stopped")));
    assert!(read_wsl(&format!("{run}/arg1")) == prompt.as_bytes(), "the same prompt");
    assert_eq!(read_wsl(&format!("{run}/seen")), b"status: active\n", "a new prompt in review: active again");
    let worktrees = sh(r#"cd "$1" && git worktree list --porcelain | grep -c '^worktree '"#, &[&repo]);
    assert_eq!(worktrees, "2", "the main checkout and the task's worktree only");
    wait(&mut h, "the new session recorded", 30, |_| stored(&repo, &home, &agent, &reference)["session"].as_str() == Some(again.session().as_str()));
    wait(&mut h, "back in review after the new turn", 30, |h| status(h) == Some(protocol::TaskStatus::Review));
    sh(r#"touch "$1/release-2""#, &[&home]);
    drop(h);
    drop(cleanup);
}
