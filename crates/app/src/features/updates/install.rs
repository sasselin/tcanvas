//! Network and file side of updates: list the releases, download this platform's file, check
//! it against `SHA256SUMS`, then swap the binaries next to the running one and relaunch it.
//!
//! The running file is renamed to `<name>.old` (Windows allows renaming a running exe, Linux
//! keeps the old inode alive) and the new one takes its place, so the update works wherever the
//! app lives: installer folder, portable exe, `deploy.sh` folder, `~/.local/bin`. The `.old`
//! files are removed on the next start. tmux sessions live in WSL and survive the restart; the
//! new exe installs its own agent there on startup (`agent_link::install`).

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::release::{self, Release};

const OLD_SUFFIX: &str = ".old";

fn agent(timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder().timeout_global(Some(timeout)).user_agent(concat!("tcanvas/", env!("CARGO_PKG_VERSION"))).build();
    ureq::Agent::new_with_config(config)
}

/// The repository's published releases (one GitHub API call).
pub fn fetch_releases() -> Result<Vec<Release>, String> {
    let mut resp = agent(Duration::from_secs(30)).get(&release::api_url()).header("Accept", "application/vnd.github+json").call().map_err(|e| e.to_string())?;
    let body = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    release::parse(&body)
}

/// Downloads, checks and puts `release` in place of the running build. Returns the path to
/// relaunch.
pub fn install(release: &Release) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().and_then(|p| p.canonicalize()).map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("no folder")?.to_path_buf();
    let name = release::asset_name(cfg!(windows), &release.version);
    let asset = release.asset(&name).ok_or_else(|| format!("{name}: missing from the release"))?;
    let sums_url = &release.asset(release::SUMS).ok_or("SHA256SUMS: missing from the release")?.url;

    let mut sums = agent(Duration::from_secs(30)).get(sums_url).call().map_err(|e| e.to_string())?;
    let sums = sums.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let expected = release::checksum_of(&sums, &name).ok_or_else(|| format!("{name}: not in SHA256SUMS"))?;

    // Next to the exe: the final rename stays on one filesystem.
    let download = dir.join(format!(".tcanvas-update-{}", std::process::id()));
    let result = download_checked(&asset.url, &download, &expected).and_then(|()| put_in_place(&exe, &download, &release.version));
    let _ = fs::remove_file(&download);
    result?;
    Ok(exe)
}

fn download_checked(url: &str, to: &Path, expected: &str) -> Result<(), String> {
    let resp = agent(Duration::from_secs(15 * 60)).get(url).call().map_err(|e| e.to_string())?;
    let file = fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let actual = copy_hashing(resp.into_body().into_reader(), file).map_err(|e| e.to_string())?;
    verify(&actual, expected)
}

/// Copies everything and returns the SHA-256 (hex) of what was written.
fn copy_hashing(mut from: impl Read, mut to: impl Write) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        to.write_all(&buf[..n])?;
    }
    to.flush()?;
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn verify(actual: &str, expected: &str) -> Result<(), String> {
    if actual.eq_ignore_ascii_case(expected) { Ok(()) } else { Err(format!("checksum mismatch: got {actual}, expected {expected}")) }
}

#[cfg(windows)]
fn put_in_place(exe: &Path, download: &Path, version: &str) -> Result<(), String> {
    swap(exe, download).map_err(|e| format!("{}: {e}", exe.display()))?;
    record_installed_version(exe, version);
    Ok(())
}

/// Linux: the archive holds `tcanvas-vX.Y.Z-linux-x86_64/{tcanvas,tcanvas-agent,...}`. The agent
/// is replaced too when it sits next to the app (where `shell::launch` looks first).
#[cfg(not(windows))]
fn put_in_place(exe: &Path, download: &Path, version: &str) -> Result<(), String> {
    let dir = exe.parent().ok_or("no folder")?;
    let unpacked = dir.join(format!(".tcanvas-update-{}.d", std::process::id()));
    let _ = fs::remove_dir_all(&unpacked);
    fs::create_dir_all(&unpacked).map_err(|e| format!("{}: {e}", unpacked.display()))?;
    let result = (|| {
        let out = Command::new("tar").arg("-xzf").arg(download).arg("-C").arg(&unpacked).output().map_err(|e| format!("tar: {e}"))?;
        if !out.status.success() {
            return Err(format!("tar: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        let root = unpacked.join(format!("tcanvas-v{version}-linux-x86_64"));
        swap(exe, &root.join("tcanvas")).map_err(|e| format!("{}: {e}", exe.display()))?;
        let agent = dir.join("tcanvas-agent");
        if agent.exists() {
            swap(&agent, &root.join("tcanvas-agent")).map_err(|e| format!("{}: {e}", agent.display()))?;
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&unpacked);
    result
}

fn old_of(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(OLD_SUFFIX);
    target.with_file_name(name)
}

/// `target` becomes `<target>.old`, `new` takes its place. On failure, `target` is restored.
pub fn swap(target: &Path, new: &Path) -> io::Result<()> {
    let old = old_of(target);
    // A leftover from an earlier update whose old process was still running at cleanup.
    let _ = fs::remove_file(&old);
    fs::rename(target, &old)?;
    if let Err(e) = fs::rename(new, target) {
        let _ = fs::rename(&old, target);
        return Err(e);
    }
    Ok(())
}

/// Removes the `.old` files an update left in `dir`. The previous process may still be exiting
/// (Windows keeps its exe locked until then): a few tries over several seconds.
pub fn cleanup_old(dir: &Path) {
    for _ in 0..10 {
        if remove_old_files(dir) {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// `true` when none is left.
fn remove_old_files(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else { return true };
    let mut done = true;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let ours = ["tcanvas", "tcanvas.exe", "tcanvas-agent"].iter().any(|n| name == format!("{n}{OLD_SUFFIX}"));
        if ours && fs::remove_file(entry.path()).is_err() {
            done = false;
        }
    }
    done
}

/// Starts the new build with the same arguments; the caller then closes this one.
pub fn relaunch(exe: &Path) -> io::Result<()> {
    Command::new(exe).args(std::env::args_os().skip(1)).spawn().map(drop)
}

/// The installer records the version in "Installed apps": keep it right when the updated exe
/// is the installed one (not a portable copy).
#[cfg(windows)]
fn record_installed_version(exe: &Path, version: &str) {
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\tcanvas";
    let reg = |args: &[&str]| {
        let mut cmd = Command::new("reg.exe");
        cmd.args(args);
        crate::shell::launch::no_window(&mut cmd);
        cmd.output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    };
    let Some(out) = reg(&["query", KEY, "/v", "InstallLocation"]) else { return };
    let Some(location) = out.lines().find_map(|l| l.split_once("REG_SZ").filter(|(k, _)| k.trim() == "InstallLocation").map(|(_, v)| v.trim().to_string())) else { return };
    let same = Path::new(&location).canonicalize().ok().zip(exe.parent()).is_some_and(|(a, b)| a == b);
    if same {
        reg(&["add", KEY, "/v", "DisplayVersion", "/t", "REG_SZ", "/d", version, "/f"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tcanvas-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hashes_while_copying() {
        let mut out = Vec::new();
        let hash = copy_hashing(&b"abc"[..], &mut out).unwrap();
        assert_eq!(out, b"abc");
        assert_eq!(hash, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(verify(&hash, &hash.to_uppercase()).is_ok());
        assert!(verify(&hash, &"0".repeat(64)).unwrap_err().contains("checksum mismatch"));
    }

    #[test]
    fn swap_keeps_the_old_file_aside_and_cleanup_removes_it() {
        let dir = temp("swap");
        let (target, new) = (dir.join("tcanvas.exe"), dir.join(".tcanvas-update-1"));
        fs::write(&target, "v1").unwrap();
        fs::write(&new, "v2").unwrap();
        swap(&target, &new).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "v2");
        assert_eq!(fs::read_to_string(dir.join("tcanvas.exe.old")).unwrap(), "v1");
        assert!(!new.exists());

        // A second update replaces the leftover `.old`.
        fs::write(&new, "v3").unwrap();
        swap(&target, &new).unwrap();
        assert_eq!(fs::read_to_string(dir.join("tcanvas.exe.old")).unwrap(), "v2");

        fs::write(dir.join("tcanvas-agent.old"), "").unwrap();
        fs::write(dir.join("notes.old"), "").unwrap();
        cleanup_old(&dir);
        assert!(!dir.join("tcanvas.exe.old").exists() && !dir.join("tcanvas-agent.old").exists());
        assert!(dir.join("notes.old").exists(), "only our own files are removed");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_swap_restores_the_running_file() {
        let dir = temp("swap-fail");
        let target = dir.join("tcanvas");
        fs::write(&target, "v1").unwrap();
        assert!(swap(&target, &dir.join("missing")).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "v1");
        assert!(!dir.join("tcanvas.old").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// The release archive layout, unpacked with the system `tar` like a real update.
    #[cfg(not(windows))]
    #[test]
    fn linux_update_replaces_the_app_and_its_agent() {
        let dir = temp("linux");
        let src = dir.join("src/tcanvas-v9.9.9-linux-x86_64");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("tcanvas"), "new app").unwrap();
        fs::write(src.join("tcanvas-agent"), "new agent").unwrap();
        let archive = dir.join("a.tar.gz");
        let ok = Command::new("tar").arg("-czf").arg(&archive).arg("-C").arg(dir.join("src")).arg("tcanvas-v9.9.9-linux-x86_64").status().unwrap();
        assert!(ok.success());
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("tcanvas"), "old app").unwrap();
        fs::write(bin.join("tcanvas-agent"), "old agent").unwrap();

        put_in_place(&bin.join("tcanvas"), &archive, "9.9.9").unwrap();
        assert_eq!(fs::read_to_string(bin.join("tcanvas")).unwrap(), "new app");
        assert_eq!(fs::read_to_string(bin.join("tcanvas-agent")).unwrap(), "new agent");
        let left: Vec<_> = fs::read_dir(&bin).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.starts_with(".tcanvas-update")).collect();
        assert!(left.is_empty(), "{left:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}
