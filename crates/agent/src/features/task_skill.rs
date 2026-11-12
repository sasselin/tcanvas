//! The `tcanvas-tasks` skill: a short `SKILL.md` that tells a coding agent what tcanvas tasks are
//! and how to use `tcanvas-agent task`, installed with the task integration for the agents that
//! read skills from a folder (see `agent_hooks::Adapter::skills`).
//!
//! The text is embedded in the binary (`task_skill.md`, with the binary's path filled in) and ends
//! with a marker line holding a hash of everything above it. Line endings and trailing whitespace
//! are normalized before hashing, so an editor that only adds a final newline or CRLF does not
//! make it "edited". That tells three cases apart:
//! - the file is ours and untouched (its hash matches its own text): an install replaces it when it
//!   differs from the current text (an older version, another binary path), an uninstall removes
//!   it;
//! - the file was edited (no marker, or a hash that no longer matches): never overwritten nor
//!   removed, and it counts as installed (the user's choice wins);
//! - no file: an install writes it.
//!
//! A `SKILL.md` that is a symbolic link (dotfile managers) is written through, and stays a link.

use std::fs;
use std::io;
use std::path::Path;

use super::tasks::fnv1a;

/// Folder name of the skill, under an agent's skills folder.
pub const SKILL_NAME: &str = "tcanvas-tasks";
pub const SKILL_FILE: &str = "SKILL.md";
const TEMPLATE: &str = include_str!("task_skill.md");
const MARKER: &str = "<!-- tcanvas-skill ";

/// `text` with LF line endings, no trailing whitespace on any line, no trailing empty lines, and
/// one final newline.
fn normalize(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').map(str::trim_end).collect();
    let end = lines.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1);
    let mut out = lines[..end].join("\n");
    out.push('\n');
    out
}

fn marker(body: &str) -> String {
    format!("{MARKER}{:016x} -->", fnv1a(body))
}

/// The skill's full text for the binary at `exe`, marker line included.
pub fn render(exe: &str) -> String {
    let body = normalize(&TEMPLATE.replace("{{exe}}", exe));
    format!("{body}{}\n", marker(&body))
}

/// What is at a skill's path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SkillState {
    Missing,
    /// Ours, untouched, the current text.
    Current,
    /// Ours, untouched, another text (an older version or another binary path).
    Outdated,
    /// Not ours as written: edited (or written by someone else). Left alone.
    Edited,
}

/// Whether `text` is a skill we wrote and nobody changed since (up to line endings and
/// trailing whitespace).
fn untouched(text: &str) -> bool {
    let text = normalize(text);
    let rest = &text[..text.len() - 1];
    let Some(i) = rest.rfind('\n') else { return false };
    let (body, last) = (&text[..=i], &rest[i + 1..]);
    last == marker(body)
}

pub fn state_of(text: Option<&str>, exe: &str) -> SkillState {
    match text {
        None => SkillState::Missing,
        Some(t) if normalize(t) == render(exe) => SkillState::Current,
        Some(t) if untouched(t) => SkillState::Outdated,
        Some(_) => SkillState::Edited,
    }
}

/// What an install or an uninstall did to a skill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SkillChange {
    /// Written (new or updated) / removed.
    Changed,
    /// Already as wanted.
    Unchanged,
    /// Edited by the user: kept as it is.
    KeptEdited,
}

fn read(file: &Path) -> io::Result<Option<String>> {
    match fs::read(file) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The state of the skill in folder `dir`.
pub fn state_in(dir: &Path, exe: &str) -> SkillState {
    match read(&dir.join(SKILL_FILE)) {
        Ok(text) => state_of(text.as_deref(), exe),
        // Unreadable: not ours to touch.
        Err(_) => SkillState::Edited,
    }
}

/// Writes the current skill into `dir` unless an edited one is there: atomically, through a
/// symbolic link, the temporary file removed if anything fails.
pub fn install_in(dir: &Path, exe: &str) -> io::Result<SkillChange> {
    let file = dir.join(SKILL_FILE);
    match state_of(read(&file)?.as_deref(), exe) {
        SkillState::Current => Ok(SkillChange::Unchanged),
        SkillState::Edited => Ok(SkillChange::KeptEdited),
        SkillState::Missing | SkillState::Outdated => {
            fs::create_dir_all(dir)?;
            let target = fs::canonicalize(&file).unwrap_or(file);
            let tmp = target.with_file_name(format!(".{SKILL_FILE}.tcanvas-tmp"));
            let written = fs::write(&tmp, render(exe)).and_then(|_| fs::rename(&tmp, &target));
            if written.is_err() {
                let _ = fs::remove_file(&tmp);
            }
            written.map(|_| SkillChange::Changed)
        }
    }
}

/// Removes the skill from `dir` if it is ours and untouched (any version; a link is removed, not
/// what it points to), and the folder if nothing else is left in it.
pub fn uninstall_in(dir: &Path) -> io::Result<SkillChange> {
    let file = dir.join(SKILL_FILE);
    let Some(text) = read(&file)? else { return Ok(SkillChange::Unchanged) };
    if !untouched(&text) {
        return Ok(SkillChange::KeptEdited);
    }
    fs::remove_file(&file)?;
    // Only an empty folder goes: anything the user put next to the skill stays.
    let _ = fs::remove_dir(dir);
    Ok(SkillChange::Changed)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const EXE: &str = "/home/me/.local/bin/tcanvas-agent";

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tcanvas-skill-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_skill_has_its_frontmatter_the_binary_and_a_marker() {
        let text = render(EXE);
        assert!(text.starts_with("---\nname: tcanvas-tasks\ndescription: "), "{text}");
        assert!(text.contains(&format!("`{EXE}`")));
        assert!(!text.contains("{{"), "every placeholder filled");
        let last = text.lines().last().unwrap();
        assert!(last.starts_with("<!-- tcanvas-skill ") && last.ends_with(" -->"), "{last}");
        assert!(untouched(&text));
        for rule in ["Append, never rewrite", "becomes a new task", "`review` when you are done", "task note"] {
            assert!(text.contains(rule), "{rule}");
        }
    }

    #[test]
    fn states_tell_ours_from_edited() {
        assert_eq!(state_of(None, EXE), SkillState::Missing);
        assert_eq!(state_of(Some(&render(EXE)), EXE), SkillState::Current);
        assert_eq!(state_of(Some(&render("/opt/tcanvas-agent")), EXE), SkillState::Outdated, "another binary path");
        // An older version: other text, its own matching marker.
        let old_body = "---\nname: tcanvas-tasks\ndescription: old\n---\nold text\n";
        let old = format!("{old_body}{MARKER}{:016x} -->\n", fnv1a(old_body));
        assert_eq!(state_of(Some(&old), EXE), SkillState::Outdated);
        // Edited: a word changed above the marker, or the marker removed.
        let edited = render(EXE).replace("Append, never rewrite", "Append");
        assert_eq!(state_of(Some(&edited), EXE), SkillState::Edited);
        let no_marker = TEMPLATE.replace("{{exe}}", EXE);
        assert_eq!(state_of(Some(&no_marker), EXE), SkillState::Edited);
        assert_eq!(state_of(Some(&format!("{}more\n", render(EXE))), EXE), SkillState::Edited, "text added after the marker");
        assert_eq!(state_of(Some(""), EXE), SkillState::Edited, "emptied on purpose");
    }

    #[test]
    fn what_an_editor_adds_on_save_is_not_an_edit() {
        let ours = render(EXE);
        for saved in [format!("{ours}\n"), format!("{ours}\n\n"), ours.replace('\n', "\r\n"), ours.replace("# tcanvas tasks\n", "# tcanvas tasks  \n"), ours.trim_end().to_string()] {
            assert_eq!(state_of(Some(&saved), EXE), SkillState::Current, "{saved:?}");
            assert_eq!(state_of(Some(&saved), "/opt/tcanvas-agent"), SkillState::Outdated, "still ours");
        }
        assert_eq!(state_of(Some(&ours.replace("# tcanvas tasks\n", "# tcanvas  tasks\n")), EXE), SkillState::Edited, "inner spaces are text");
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_skill_is_written_through_and_stays_a_link() {
        let root = temp("link");
        let dir = root.join(SKILL_NAME);
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(root.join("dotfiles")).unwrap();
        let real = root.join("dotfiles/SKILL.md");
        fs::write(&real, render("/old/tcanvas-agent")).unwrap();
        std::os::unix::fs::symlink(&real, dir.join(SKILL_FILE)).unwrap();
        assert_eq!(install_in(&dir, EXE).unwrap(), SkillChange::Changed);
        assert!(fs::symlink_metadata(dir.join(SKILL_FILE)).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), render(EXE));
        let leftovers: Vec<_> = fs::read_dir(root.join("dotfiles")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "no temporary file left: {leftovers:?}");
        assert_eq!(uninstall_in(&dir).unwrap(), SkillChange::Changed);
        assert!(real.exists(), "the link goes, not what it points to");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn install_update_and_uninstall_respect_edits() {
        let dir = temp("cycle").join(SKILL_NAME);
        assert_eq!(install_in(&dir, EXE).unwrap(), SkillChange::Changed);
        assert_eq!(fs::read_to_string(dir.join(SKILL_FILE)).unwrap(), render(EXE));
        assert_eq!(install_in(&dir, EXE).unwrap(), SkillChange::Unchanged);
        assert_eq!(install_in(&dir, "/opt/tcanvas-agent").unwrap(), SkillChange::Changed, "updated");
        assert_eq!(state_in(&dir, "/opt/tcanvas-agent"), SkillState::Current);

        let edited = format!("{}\nMy own rule.\n", render(EXE));
        fs::write(dir.join(SKILL_FILE), &edited).unwrap();
        assert_eq!(install_in(&dir, EXE).unwrap(), SkillChange::KeptEdited);
        assert_eq!(uninstall_in(&dir).unwrap(), SkillChange::KeptEdited);
        assert_eq!(fs::read_to_string(dir.join(SKILL_FILE)).unwrap(), edited, "never overwritten nor removed");

        fs::write(dir.join(SKILL_FILE), render(EXE)).unwrap();
        fs::write(dir.join("extra.md"), "mine").unwrap();
        assert_eq!(uninstall_in(&dir).unwrap(), SkillChange::Changed);
        assert!(!dir.join(SKILL_FILE).exists() && dir.join("extra.md").exists(), "the user's file stays");
        fs::remove_file(dir.join("extra.md")).unwrap();
        install_in(&dir, EXE).unwrap();
        assert_eq!(uninstall_in(&dir).unwrap(), SkillChange::Changed);
        assert!(!dir.exists(), "an empty folder goes");
        assert_eq!(uninstall_in(&dir).unwrap(), SkillChange::Unchanged);
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }
}
