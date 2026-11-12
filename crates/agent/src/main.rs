//! `tcanvas-agent`: runs in WSL and reports the state of the terminals to the app.
//!
//! - `tcanvas-agent`               : server mode (JSON lines on stdin/stdout)
//! - `tcanvas-agent install-hooks` : installs the hooks of the user's shell, bash or zsh
//!   (exit codes, durations), and of the coding agents found (Claude Code, Codex, Gemini CLI,
//!   Copilot CLI, Qwen Code: working / waiting for the user)
//! - `tcanvas-agent install-hooks --tasks`: installs the task integration instead (opt-in): a
//!   session start hook running `task prime` and the tcanvas-tasks skill
//! - `tcanvas-agent uninstall-hooks [--tasks]`: removes everything of ours from the coding
//!   agents (or only the task integration)
//! - `tcanvas-agent hook <event>`  : called by a coding agent's hook (working / waiting)
//! - `tcanvas-agent hooks [bash|zsh]`: prints the hooks without installing anything
//! - `tcanvas-agent kill <session>`: stops every process of the session, then the session
//! - `tcanvas-agent task <command>`: reads and appends to the current project's task list
//!   (`task --help`); `task prime` is what coding agents get at the start of a session
//!
//! In server mode, it also pastes text the app sends into a session (`AgentCmd::SendText`), and
//! keeps the task lists of the watched folders (`features::tasks`).

mod features {
    pub mod agent_hooks;
    pub mod changes;
    pub mod cleanup;
    pub mod create;
    pub mod hooks;
    pub mod kill;
    pub mod recipe;
    pub mod send;
    pub mod serve;
    pub mod status;
    pub mod task_cli;
    pub mod task_skill;
    pub mod tasks;
    pub mod tmux;
    pub mod worktrees;
}

use features::{hooks, serve};

fn main() -> std::io::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve::run(),
        Some(cmd @ ("install-hooks" | "uninstall-hooks")) => {
            let tasks = match std::env::args().nth(2).as_deref() {
                None => false,
                Some("--tasks") => true,
                Some(other) => {
                    eprintln!("unknown option: {other}\nusage: tcanvas-agent {cmd} [--tasks]");
                    std::process::exit(2);
                }
            };
            if cmd == "install-hooks" { hooks::install_cli(tasks) } else { hooks::uninstall_cli(tasks) }
        }
        Some("hook") => features::agent_hooks::hook_cli(std::env::args().nth(2).as_deref()),
        Some("hooks") => {
            let shell = std::env::args().nth(2).or_else(hooks::user_shell_path).unwrap_or_default();
            let Some(shell) = hooks::Shell::from_path(&shell) else {
                eprintln!("unsupported shell: {shell}\nusage: tcanvas-agent hooks [bash|zsh]");
                std::process::exit(2);
            };
            print!("{}", shell.hooks());
            Ok(())
        }
        Some("kill") => {
            let Some(session) = std::env::args().nth(2) else {
                eprintln!("usage: tcanvas-agent kill <session>");
                std::process::exit(2);
            };
            let report = features::kill::kill_session(&session);
            println!("{} processes stopped ({} needed SIGKILL)", report.targeted, report.needed_sigkill);
            if !report.survivors.is_empty() {
                eprintln!("processes still alive: {:?}", report.survivors);
                std::process::exit(1);
            }
            Ok(())
        }
        Some("task") => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            std::process::exit(features::task_cli::main(&args));
        }
        Some(other) => {
            eprintln!("unknown command: {other}\nusage: tcanvas-agent [serve|install-hooks|uninstall-hooks|hook <event>|hooks [bash|zsh]|kill <session>|task <command>]");
            std::process::exit(2);
        }
    }
}
