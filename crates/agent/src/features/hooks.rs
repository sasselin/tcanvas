//! Shell hooks that publish the start, end and exit code of every command as tmux pane
//! options (`@tc_started`, `@tc_ended`, `@tc_exit`). Available for zsh and bash; the hooks
//! only act inside the tcanvas tmux server (its socket path contains "tcanvas").

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use protocol::Hooks;

pub const ZSH_HOOKS: &str = r#"# tcanvas: command status (installed by `tcanvas-agent install-hooks`)
if [[ -n "$TMUX" && "$TMUX" == *tcanvas* ]]; then
  zmodload zsh/datetime 2>/dev/null
  _tcanvas_preexec() {
    _tcanvas_running=1
    tmux set -p -t "$TMUX_PANE" @tc_started "$EPOCHSECONDS" 2>/dev/null
  }
  _tcanvas_precmd() {
    local code=$?
    (( ${_tcanvas_running:-0} )) || return 0
    _tcanvas_running=0
    tmux set -p -t "$TMUX_PANE" @tc_exit "$code" \; set -p -t "$TMUX_PANE" @tc_ended "$EPOCHSECONDS" 2>/dev/null
  }
  autoload -Uz add-zsh-hook
  add-zsh-hook preexec _tcanvas_preexec
  add-zsh-hook precmd _tcanvas_precmd
fi
"#;

/// bash >= 5.0 (`EPOCHSECONDS`). `PS0` is expanded once a command line is read, never for
/// an empty one: its array subscript is arithmetic evaluated in the shell itself, which
/// records the start time without a subshell. `PROMPT_COMMAND` then publishes everything
/// in one tmux call; it runs first, so it sees the command's `$?`, and gives it back.
pub const BASH_HOOKS: &str = r#"# tcanvas: command status (installed by `tcanvas-agent install-hooks`)
if [[ -n "$TMUX" && "$TMUX" == *tcanvas* && -z "${_tcanvas_hooked:-}" ]]; then
  _tcanvas_hooked=1
  _tcanvas_started=
  _tcanvas_precmd() {
    local code=$?
    [[ -n "$_tcanvas_started" ]] || return $code
    tmux set -p -t "$TMUX_PANE" @tc_started "$_tcanvas_started" \; set -p -t "$TMUX_PANE" @tc_exit "$code" \; set -p -t "$TMUX_PANE" @tc_ended "$EPOCHSECONDS" 2>/dev/null
    _tcanvas_started=
    return $code
  }
  PS0="${PS0:-}"'${_tcanvas_ps0[_tcanvas_started=EPOCHSECONDS]}'
  PROMPT_COMMAND="_tcanvas_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi
"#;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shell {
    Zsh,
    Bash,
}

impl Shell {
    /// From a shell path (`/usr/bin/zsh`, `-bash`...); `None` for unsupported shells.
    pub fn from_path(path: &str) -> Option<Shell> {
        match path.rsplit('/').next()?.trim_start_matches('-') {
            "zsh" => Some(Shell::Zsh),
            "bash" => Some(Shell::Bash),
            _ => None,
        }
    }

    pub fn hooks(self) -> &'static str {
        match self {
            Shell::Zsh => ZSH_HOOKS,
            Shell::Bash => BASH_HOOKS,
        }
    }

    /// Hooks file, relative to the home directory.
    fn hooks_file(self) -> &'static str {
        match self {
            Shell::Zsh => ".config/tcanvas/hooks.zsh",
            Shell::Bash => ".config/tcanvas/hooks.bash",
        }
    }

    /// Startup file of interactive shells (tmux starts login shells, and Ubuntu's
    /// `~/.profile` sources `~/.bashrc`).
    fn rc_file(self) -> &'static str {
        match self {
            Shell::Zsh => ".zshrc",
            Shell::Bash => ".bashrc",
        }
    }

    fn source_line(self) -> String {
        let f = self.hooks_file();
        // Same line as earlier versions for zsh: existing installs are recognized.
        format!("[ -f ~/{f} ] && source ~/{f}")
    }
}

/// The user's shell: `$SHELL`, else the passwd entry (what tmux starts in new panes).
pub fn user_shell_path() -> Option<String> {
    if let Ok(shell) = std::env::var("SHELL")
        && !shell.is_empty()
    {
        return Some(shell);
    }
    // SAFETY: getpwuid returns a pointer to static storage or null; read right away.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*pw).pw_shell).to_string_lossy().into_owned())
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

/// Hooks state for `shell` in `home`.
pub fn state_in(home: &Path, shell: Option<Shell>) -> Hooks {
    let Some(shell) = shell else { return Hooks::Unsupported };
    let installed = home.join(shell.hooks_file()).exists() && fs::read_to_string(home.join(shell.rc_file())).is_ok_and(|rc| rc.contains(&shell.source_line()));
    if installed { Hooks::Installed } else { Hooks::Missing }
}

/// Hooks state for the current user.
pub fn state() -> Hooks {
    state_in(&home(), user_shell_path().as_deref().and_then(Shell::from_path))
}

/// Writes the hooks file and adds its source line to the rc file exactly once.
/// Returns the paths of the hooks file and of the rc file, and whether the rc file changed.
pub fn install_in(home: &Path, shell: Shell) -> io::Result<(PathBuf, PathBuf, bool)> {
    let path = home.join(shell.hooks_file());
    fs::create_dir_all(path.parent().expect("hooks path has a parent"))?;
    fs::write(&path, shell.hooks())?;

    let rc_path = home.join(shell.rc_file());
    let rc = fs::read_to_string(&rc_path).unwrap_or_default();
    let line = shell.source_line();
    if rc.contains(&line) {
        return Ok((path, rc_path, false));
    }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&rc_path)?;
    let sep = if rc.is_empty() || rc.ends_with('\n') { "" } else { "\n" };
    writeln!(f, "{sep}{line}")?;
    Ok((path, rc_path, true))
}

/// Installs the hooks for the current user's shell.
pub fn install() -> io::Result<(PathBuf, PathBuf, bool)> {
    let path = user_shell_path().unwrap_or_default();
    let Some(shell) = Shell::from_path(&path) else {
        return Err(io::Error::new(io::ErrorKind::Unsupported, format!("unsupported shell: {path} (bash or zsh only)")));
    };
    install_in(&home(), shell)
}

/// `tcanvas-agent install-hooks`.
pub fn install_cli() -> io::Result<()> {
    let (hooks, rc, changed) = install()?;
    println!("Hooks written to {}", hooks.display());
    if changed {
        println!("Line added to {}. New terminals will pick it up.", rc.display());
    } else {
        println!("{} already sources the hooks.", rc.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tcanvas-hooks-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn shells_are_recognized_from_their_path() {
        assert_eq!(Shell::from_path("/usr/bin/zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::from_path("/bin/bash"), Some(Shell::Bash));
        assert_eq!(Shell::from_path("-bash"), Some(Shell::Bash));
        assert_eq!(Shell::from_path("/usr/bin/fish"), None);
        assert_eq!(Shell::from_path(""), None);
    }

    #[test]
    fn install_adds_the_line_once_and_keeps_the_rc_file() {
        let home = temp_home("bash");
        fs::write(home.join(".bashrc"), "alias ll='ls -l'").unwrap();
        assert_eq!(state_in(&home, Some(Shell::Bash)), Hooks::Missing);

        let (hooks, rc, changed) = install_in(&home, Shell::Bash).unwrap();
        assert!(changed);
        assert_eq!(fs::read_to_string(hooks).unwrap(), BASH_HOOKS);
        assert_eq!(state_in(&home, Some(Shell::Bash)), Hooks::Installed);
        assert_eq!(state_in(&home, Some(Shell::Zsh)), Hooks::Missing, "per shell");

        assert!(!install_in(&home, Shell::Bash).unwrap().2, "second install: no change");
        assert_eq!(fs::read_to_string(rc).unwrap(), "alias ll='ls -l'\n[ -f ~/.config/tcanvas/hooks.bash ] && source ~/.config/tcanvas/hooks.bash\n");
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn zsh_install_creates_the_rc_file_if_needed() {
        let home = temp_home("zsh");
        install_in(&home, Shell::Zsh).unwrap();
        assert_eq!(fs::read_to_string(home.join(".zshrc")).unwrap(), "[ -f ~/.config/tcanvas/hooks.zsh ] && source ~/.config/tcanvas/hooks.zsh\n");
        assert_eq!(state_in(&home, Some(Shell::Zsh)), Hooks::Installed);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn other_shells_are_unsupported() {
        assert_eq!(state_in(Path::new("/nonexistent"), None), Hooks::Unsupported);
    }
}
