//! tcanvas is open source: everything in the repository is written in English.
//!
//! The only French allowed is the `FR: Texts` UI translation table in `shell/i18n.rs`,
//! and string literals in test code that check that table.

use std::path::{Path, PathBuf};

const ACCENTED: &str = "àâäéèêëîïôöùûüçœÀÂÄÉÈÊËÎÏÔÖÙÛÜÇŒ";
/// Common French words that carry no accent, checked in comments and prose.
const FRENCH_WORDS: &[&str] =
    &["les", "des", "pour", "avec", "dans", "une", "est", "sont", "sinon", "quand", "aussi", "mais", "donc", "chaque", "selon", "sans", "fichier", "dossier", "carte", "cartes"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if matches!(path.extension().and_then(|e| e.to_str()), Some("rs" | "sh" | "svg" | "toml" | "md" | "yml" | "nsi" | "desktop")) {
            out.push(path);
        }
    }
}

fn has_accent(s: &str) -> bool {
    s.chars().any(|c| ACCENTED.contains(c))
}

fn french_word(s: &str) -> Option<&'static str> {
    s.split(|c: char| !c.is_alphanumeric()).find_map(|w| FRENCH_WORDS.iter().copied().find(|f| w.eq_ignore_ascii_case(f)))
}

/// Splits a Rust line into (code-string contents, trailing `//` comment).
fn split_rust_line(line: &str) -> (String, &str) {
    let b = line.as_bytes();
    let (mut strings, mut i, mut in_str) = (String::new(), 0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            match c {
                b'\\' => i += 1,
                b'"' => in_str = false,
                _ => strings.push(line[i..].chars().next().unwrap()),
            }
        } else {
            match c {
                b'"' => in_str = true,
                // Char literals such as '"' or '\'' must not open a string.
                b'\'' if b.get(i + 2) == Some(&b'\'') => i += 2,
                b'\'' if b.get(i + 1) == Some(&b'\\') && b.get(i + 3) == Some(&b'\'') => i += 3,
                b'/' if b.get(i + 1) == Some(&b'/') => return (strings, &line[i..]),
                _ => {}
            }
        }
        i += line[i..].chars().next().map_or(1, char::len_utf8);
    }
    (strings, "")
}

fn check_rust(rel: &str, text: &str, bad: &mut Vec<String>) {
    let in_tests_dir = rel.contains("/tests/");
    let test_mod_start = text.lines().position(|l| l.trim() == "#[cfg(test)]").unwrap_or(usize::MAX);
    let mut in_fr_table = false;
    for (n, line) in text.lines().enumerate() {
        if line.starts_with("pub const FR: Texts") {
            in_fr_table = true;
        }
        let (strings, comment) = split_rust_line(line);
        let french_comment = has_accent(comment) || french_word(comment).is_some();
        let french_code_string = has_accent(&strings) && !in_fr_table && !in_tests_dir && n < test_mod_start;
        if french_comment || french_code_string {
            bad.push(format!("{rel}:{}: {line}", n + 1));
        }
        if in_fr_table && line.starts_with("};") {
            in_fr_table = false;
        }
    }
}

fn check_prose(rel: &str, text: &str, bad: &mut Vec<String>) {
    for (n, line) in text.lines().enumerate() {
        if has_accent(line) || french_word(line).is_some() {
            bad.push(format!("{rel}:{}: {line}", n + 1));
        }
    }
}

#[test]
fn the_repository_is_written_in_english() {
    let root = root();
    let mut files: Vec<PathBuf> =
        ["README.md", "CLAUDE.md", "CONTRIBUTING.md", "SECURITY.md", "Cargo.toml", "release-plz.toml", "rustfmt.toml"].iter().map(|f| root.join(f)).filter(|p| p.exists()).collect();
    for dir in ["crates", "scripts", "assets", "installer", ".github"] {
        collect(&root.join(dir), &mut files);
    }
    let mut bad = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{rel}: {e}"));
        if rel.ends_with(".rs") {
            check_rust(&rel, &text, &mut bad);
        } else {
            check_prose(&rel, &text, &mut bad);
        }
    }
    assert!(bad.is_empty(), "French found outside the FR UI table:\n{}", bad.join("\n"));
}

#[test]
fn the_checker_tells_comments_from_strings() {
    let (strings, comment) = split_rust_line(r#"let c = '"'; let s = "a // b"; // trailing"#);
    assert_eq!(strings, "a // b");
    assert_eq!(comment, "// trailing");
    assert_eq!(french_word("// les cartes"), Some("les"));
    assert_eq!(french_word("// unless the cards"), None);
}
