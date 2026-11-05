//! Self-installation of `tcanvas-agent` in WSL.
//!
//! The Windows exe embeds a static (musl) build of the agent (see `build.rs`) and copies it to
//! `~/.local/bin/tcanvas-agent` in the default WSL distro when the installed one is missing or
//! comes from another build. The installed build is identified by
//! `~/.local/share/tcanvas/agent.id` (content hash computed at build time), so a single check
//! costs one `wsl.exe` call and the copy only happens after an update.

use std::io::Write;
use std::process::{Command, Stdio};

/// The agent shipped inside this executable.
pub struct Embedded {
    pub bytes: &'static [u8],
    pub id: &'static str,
}

/// `Some` only in the Windows build: on Linux the agent sits next to the app or in the PATH.
pub fn embedded() -> Option<Embedded> {
    #[cfg(windows)]
    {
        Some(Embedded { bytes: include_bytes!(concat!(env!("OUT_DIR"), "/tcanvas-agent")), id: include_str!(concat!(env!("OUT_DIR"), "/tcanvas-agent.id")) })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Prints the installed id. `$1` = install prefix (empty: `~/.local`).
const CHECK: &str = r#"cat "${1:-$HOME/.local}/share/tcanvas/agent.id" 2>/dev/null; true"#;

/// Reads the binary on stdin, checks its size, then swaps it in atomically and records its id.
/// `$1` = prefix, `$2` = id, `$3` = size in bytes. `mv` keeps a running agent alive (new inode).
const INSTALL: &str = r#"set -e; b="${1:-$HOME/.local}"; mkdir -p "$b/bin" "$b/share/tcanvas"; t="$b/bin/.tcanvas-agent.$$"; cat > "$t"; if [ "$(wc -c < "$t")" -ne "$3" ]; then rm -f "$t"; echo "incomplete copy" >&2; exit 1; fi; chmod 755 "$t"; mv -f "$t" "$b/bin/tcanvas-agent"; printf '%s\n' "$2" > "$b/share/tcanvas/agent.id""#;

/// `sh -c <script> tcanvas <args>` in WSL (`wsl.exe -e` on Windows, directly elsewhere).
fn sh(script: &str, args: &[&str]) -> Command {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("wsl.exe");
        c.args(["-e", "sh"]);
        c
    } else {
        Command::new("sh")
    };
    cmd.args(["-c", script, "tcanvas"]).args(args);
    crate::shell::launch::no_window(&mut cmd);
    cmd
}

/// Installs `bytes` under `prefix` (`None`: `~/.local`) unless build `id` is already there.
/// `Ok(true)` if it was copied, `Ok(false)` if it was already up to date.
pub fn ensure(bytes: &[u8], id: &str, prefix: Option<&str>) -> Result<bool, String> {
    let prefix = prefix.unwrap_or("");
    let out = sh(CHECK, &[prefix]).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(last_line(&out.stderr));
    }
    if String::from_utf8_lossy(&out.stdout).trim() == id {
        return Ok(false);
    }

    let size = bytes.len().to_string();
    let mut child = sh(INSTALL, &[prefix, id, &size]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let written = child.stdin.take().expect("stdin piped").write_all(bytes);
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(last_line(&out.stderr));
    }
    written.map_err(|e| e.to_string())?;
    Ok(true)
}

/// Last meaningful line of an error output. `wsl.exe` writes its own messages in UTF-16:
/// dropping the NULs makes them readable.
fn last_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr).replace('\0', "");
    text.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("wsl.exe").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a command in the same shell as the installer (WSL on Windows).
    fn run(script: &str, args: &[&str]) -> Vec<u8> {
        sh(script, args).stdin(Stdio::null()).output().expect("sh").stdout
    }

    #[test]
    fn installs_once_then_updates_on_a_new_build() {
        // A path inside Linux/WSL, valid on both test targets.
        let prefix = format!("/tmp/tcanvas-install-test-{}", std::process::id());
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();

        assert_eq!(ensure(&bytes, "build-a", Some(&prefix)), Ok(true));
        assert_eq!(run(r#"cat "$1/bin/tcanvas-agent""#, &[&prefix]), bytes, "binary-safe copy");
        assert_eq!(run(r#"test -x "$1/bin/tcanvas-agent" && echo yes"#, &[&prefix]), b"yes\n");
        assert_eq!(ensure(&bytes, "build-a", Some(&prefix)), Ok(false), "same build: no copy");

        let newer = b"\x7fELF newer".to_vec();
        assert_eq!(ensure(&newer, "build-b", Some(&prefix)), Ok(true));
        assert_eq!(run(r#"cat "$1/bin/tcanvas-agent""#, &[&prefix]), newer);
        assert_eq!(run(r#"ls -A "$1/bin""#, &[&prefix]), b"tcanvas-agent\n", "no temp file left");

        run(r#"rm -rf "$1""#, &[&prefix]);
    }

    #[test]
    fn a_failed_install_reports_why() {
        // The prefix is a file: mkdir fails, and the shell's message comes back.
        let prefix = format!("/tmp/tcanvas-install-file-{}", std::process::id());
        run(r#"echo x > "$1""#, &[&prefix]);
        let err = ensure(b"bin", "id", Some(&prefix)).unwrap_err();
        assert!(err.contains("mkdir") || err.contains("exists") || err.contains("directory"), "{err}");
        run(r#"rm -f "$1""#, &[&prefix]);
    }

    #[test]
    fn wsl_utf16_messages_become_readable() {
        let utf16: Vec<u8> = "no distro\n".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(last_line(&utf16), "no distro");
        assert_eq!(last_line(b""), "wsl.exe");
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_exe_embeds_a_linux_agent() {
        let agent = embedded().expect("embedded agent");
        assert!(agent.bytes.starts_with(b"\x7fELF"), "a Linux binary");
        assert_eq!(agent.id.len(), 16, "{}", agent.id);
    }
}
