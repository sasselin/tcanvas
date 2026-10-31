//! `tcanvas-agent` : tourne dans WSL et décrit à l'application l'état des terminaux.
//!
//! - `tcanvas-agent`               : mode serveur (JSON lignes sur stdin/stdout)
//! - `tcanvas-agent install-hooks` : installe les hooks zsh (codes de sortie, durées)
//! - `tcanvas-agent hooks`         : affiche les hooks sans rien installer
//! - `tcanvas-agent kill <session>`: arrête tous les processus de la session, puis la session

mod features {
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
        Some("install-hooks") => hooks::install(),
        Some("hooks") => {
            print!("{}", hooks::ZSH_HOOKS);
            Ok(())
        }
        Some("kill") => {
            let Some(session) = std::env::args().nth(2) else {
                eprintln!("usage : tcanvas-agent kill <session>");
                std::process::exit(2);
            };
            let report = features::kill::kill_session(&session);
            println!("{} processus arrêtés ({} ont exigé SIGKILL)", report.targeted, report.needed_sigkill);
            if !report.survivors.is_empty() {
                eprintln!("processus toujours vivants : {:?}", report.survivors);
                std::process::exit(1);
            }
            Ok(())
        }
        Some(other) => {
            eprintln!("commande inconnue : {other}\nusage : tcanvas-agent [serve|install-hooks|hooks|kill <session>]");
            std::process::exit(2);
        }
    }
}
