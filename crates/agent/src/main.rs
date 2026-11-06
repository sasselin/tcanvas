//! `tcanvas-agent`: runs in WSL and reports the state of the terminals to the app.
//!
//! - `tcanvas-agent`               : server mode (JSON lines on stdin/stdout)
//! - `tcanvas-agent install-hooks` : installs the hooks of the user's shell, bash or zsh
//!   (exit codes, durations), and of the coding agents found (Claude Code, Codex, Gemini CLI,
//!   Copilot CLI, Qwen Code: working / waiting for the user)
//! - `tcanvas-agent uninstall-hooks`: removes the coding agents' hooks
//! - `tcanvas-agent hook <event>`  : called by a coding agent's hook (working / waiting)
//! - `tcanvas-agent hooks [bash|zsh]`: prints the hooks without installing anything
//! - `tcanvas-agent kill <session>`: stops every process of the session, then the session

mod features {
    pub mod agent_hooks;
    pub mod changes;
    pub mod hooks;
    pub mod kill;
    pub mod serve;
    pub mod status;
    pub mod tmux;
}

use features::{hooks, serve};

fn main() -> std::io::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve::run(),
        Some("install-hooks") => hooks::install_cli(),
        Some("uninstall-hooks") => hooks::uninstall_cli(),
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
        Some(other) => {
            eprintln!("unknown command: {other}\nusage: tcanvas-agent [serve|install-hooks|uninstall-hooks|hook <event>|hooks [bash|zsh]|kill <session>]");
            std::process::exit(2);
        }
    }
}
