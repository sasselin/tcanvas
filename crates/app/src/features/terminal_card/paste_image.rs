//! Pasting an image (screenshot, photo) into a terminal: a terminal only carries text, so the
//! image is saved as a PNG and its path is pasted instead. Coding agents (Claude Code, Codex...)
//! turn a pasted image path into an attachment.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::ColorImage;

/// Pasted images older than this are removed on the next paste.
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// Default folder for pasted images.
pub fn default_dir() -> PathBuf {
    std::env::temp_dir().join("tcanvas").join("pasted")
}

/// Encodes an image as an RGBA PNG.
pub fn encode_png(image: &ColorImage) -> Option<Vec<u8>> {
    let [w, h] = image.size;
    let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, u32::try_from(w).ok()?, u32::try_from(h).ok()?);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().ok()?;
    writer.write_image_data(&rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// Windows path as seen from WSL: `C:\Users\me\x.png` -> `/mnt/c/Users/me/x.png`.
/// `None` for paths without a drive letter (UNC...).
pub fn wsl_path(win: &str) -> Option<String> {
    let mut chars = win.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    if chars.next() != Some(':') {
        return None;
    }
    let rest = chars.as_str().replace('\\', "/");
    Some(format!("/mnt/{}{}", drive.to_ascii_lowercase(), if rest.starts_with('/') { rest } else { format!("/{rest}") }))
}

/// Path of a saved file as the terminal sees it (WSL on Windows, unchanged on Linux).
fn terminal_path(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    if cfg!(windows) { wsl_path(s) } else { Some(s.to_string()) }
}

/// Removes files older than `max_age` from `dir`.
fn purge(dir: &Path, now: SystemTime, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > max_age));
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Saves the image in `dir` and returns the path to paste into the terminal.
pub fn save(image: &ColorImage, dir: &Path) -> Option<String> {
    let png = encode_png(image)?;
    let now = SystemTime::now();
    std::fs::create_dir_all(dir).ok()?;
    purge(dir, now, MAX_AGE);
    let stamp = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let mut path = dir.join(format!("paste-{stamp}.png"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("paste-{stamp}-{n}.png"));
        n += 1;
    }
    std::fs::write(&path, png).ok()?;
    terminal_path(&path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tcanvas-paste-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn windows_paths_become_wsl_paths() {
        assert_eq!(wsl_path(r"C:\Users\me\AppData\Local\Temp\x.png").as_deref(), Some("/mnt/c/Users/me/AppData/Local/Temp/x.png"));
        assert_eq!(wsl_path(r"d:\My Pictures\a b.png").as_deref(), Some("/mnt/d/My Pictures/a b.png"));
        assert_eq!(wsl_path(r"\\server\share\x.png"), None);
        assert_eq!(wsl_path("/tmp/x.png"), None);
    }

    #[test]
    fn png_round_trip() {
        let img = ColorImage::new([3, 2], vec![Color32::from_rgb(10, 20, 30); 6]);
        let bytes = encode_png(&img).unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(&buf[..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn save_writes_a_png_and_purges_old_files() {
        let dir = tmp("save");
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("paste-old.png");
        std::fs::write(&old, b"x").unwrap();
        let img = ColorImage::new([1, 1], vec![Color32::RED]);
        let a = save(&img, &dir).unwrap();
        let b = save(&img, &dir).unwrap();
        assert_ne!(a, b, "two pastes, two files");
        if !cfg!(windows) {
            assert!(Path::new(&a).exists() && a.ends_with(".png"));
        }
        assert!(old.exists(), "a fresh file stays");
        purge(&dir, SystemTime::now() + Duration::from_secs(48 * 3600), MAX_AGE);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
