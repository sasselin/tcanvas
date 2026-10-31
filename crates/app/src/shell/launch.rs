//! Lancement de `tcanvas-agent` dans WSL, partagé par les slices qui en ont besoin
//! (surveillance continue, arrêt complet d'une session).

use std::process::{Command, Stdio};

/// `tcanvas-agent <args>`. `TCANVAS_AGENT` remplace le chemin par défaut
/// (`~/.local/bin/tcanvas-agent` dans WSL, ou à côté de l'exécutable sous Linux).
pub fn agent(args: &[&str]) -> Command {
    let custom = std::env::var("TCANVAS_AGENT").ok();
    if cfg!(windows) {
        // Les variables Windows ne traversent pas wsl.exe : on transmet le socket explicitement.
        let mut cmd = Command::new("wsl.exe");
        cmd.args(["-e", "env", &format!("TCANVAS_TMUX_SOCKET={}", protocol::tmux_socket())]);
        match custom {
            Some(path) => cmd.arg(path),
            // `sh -l` charge le profil ; "$@" reçoit nos arguments ($0 = nom affiché).
            None => cmd.args(["sh", "-lc", r#"exec "$HOME/.local/bin/tcanvas-agent" "$@""#, "tcanvas-agent"]),
        };
        cmd.args(args);
        no_window(&mut cmd);
        cmd
    } else {
        let path = custom.unwrap_or_else(|| {
            let sibling = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("tcanvas-agent"))).filter(|p| p.exists());
            match sibling {
                Some(p) => p.display().to_string(),
                None => "tcanvas-agent".into(),
            }
        });
        let mut cmd = Command::new(path);
        cmd.args(args);
        cmd
    }
}

/// Pas de console noire qui clignote sous Windows.
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Lance et attend en arrière-plan ; `true` si la commande a réussi.
pub fn succeeds(mut cmd: Command) -> bool {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.status().is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_reach_the_agent() {
        let cmd = agent(&["kill", "tc_x"]);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.ends_with(&["kill".to_string(), "tc_x".to_string()]), "{args:?}");
        if cfg!(windows) {
            assert_eq!(cmd.get_program(), "wsl.exe");
            assert!(args.iter().any(|a| a.starts_with("TCANVAS_TMUX_SOCKET=")));
        }
    }
}
