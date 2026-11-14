//! Launching `tcanvas-agent` in WSL, shared by the slices that need it
//! (continuous watching, full shutdown of a session).

use std::process::{Command, Stdio};

/// `tcanvas-agent <args>`. `TCANVAS_AGENT` overrides the default path
/// (`~/.local/bin/tcanvas-agent` in WSL, or next to the executable on Linux).
pub fn agent(args: &[&str]) -> Command {
    let custom = std::env::var("TCANVAS_AGENT").ok();
    if cfg!(windows) {
        // Windows variables don't cross wsl.exe: pass the socket explicitly.
        let mut cmd = Command::new("wsl.exe");
        cmd.args(["-e", "env", &format!("TCANVAS_TMUX_SOCKET={}", protocol::tmux_socket())]);
        // Test-only: the home the agent keeps its data under (task lists).
        if let Some(home) = std::env::var("TCANVAS_TEST_HOME").ok().filter(|h| !h.is_empty()) {
            cmd.arg(format!("TCANVAS_TEST_HOME={home}"));
        }
        match custom {
            Some(path) => cmd.arg(path),
            // `sh -l` loads the profile; "$@" receives our arguments ($0 = displayed name).
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

/// No black console flashing on Windows.
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Runs and waits in the background; `true` if the command succeeded.
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
