//! Saves the canvas (cards, projects, wires, camera) to `layout.json`.
//! Atomic write; an unreadable file is set aside, never silently overwritten.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::shell::i18n::t;
use crate::shell::world::{Camera, Card, Project, ProjectId, Wire, World, project_at};

/// 2: explicit card membership in groups (`Card::project`), "Changes" panel.
const FORMAT_VERSION: u32 = 2;
const SAVE_DELAY: Duration = Duration::from_millis(800);

#[derive(Serialize, Deserialize)]
pub struct LayoutFile {
    pub version: u32,
    pub camera: Camera,
    pub cards: Vec<Card>,
    pub projects: Vec<Project>,
    pub wires: Vec<Wire>,
    /// Open space and per-space camera (missing from older files).
    #[serde(default)]
    pub space: Option<ProjectId>,
    #[serde(default)]
    pub space_cameras: BTreeMap<String, Camera>,
    #[serde(default)]
    pub changes_open: bool,
    #[serde(default)]
    pub tasks_open: bool,
    #[serde(default)]
    pub free_grid: bool,
}

/// `%APPDATA%\tcanvas\config` on Windows, `~/.config/tcanvas` on Linux.
/// `TCANVAS_CONFIG_DIR` overrides it (tests, separate profiles).
pub fn default_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TCANVAS_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("", "", "tcanvas").map(|d| d.config_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

pub struct Persistence {
    path: PathBuf,
    saved_revision: u64,
    dirty_since: Option<Instant>,
    pub last_error: Option<String>,
}

impl Persistence {
    pub fn new(dir: &Path) -> Self {
        Self { path: dir.join("layout.json"), saved_revision: 0, dirty_since: None, last_error: None }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the canvas. A missing file gives an empty canvas; a corrupt file is
    /// renamed to `layout.json.corrupt-<timestamp>` and reported.
    pub fn load(&mut self, world: &mut World) {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => {
                self.last_error = Some((t().layout_read_failed)(&self.path.display().to_string(), &e.to_string()));
                return;
            }
        };
        match serde_json::from_str::<LayoutFile>(&text) {
            Ok(file) => {
                world.camera = file.camera;
                world.projects = file.projects;
                // Wires link groups; older layouts linked terminals: those are dropped.
                world.wires = file.wires.into_iter().filter(|w| w.from != w.to && world.project(&w.from).is_some() && world.project(&w.to).is_some()).collect();
                world.space = file.space.filter(|id| world.project(id).is_some());
                world.space_cameras = file.space_cameras;
                world.changes_open = file.changes_open;
                world.tasks_open = file.tasks_open;
                world.free_grid = file.free_grid;
                for mut card in file.cards {
                    // Version 1: a card belonged to the frame containing its center.
                    if file.version < 2 {
                        card.project = project_at(&world.projects, card.rect.center()).map(|p| p.id.clone());
                    }
                    card.project = card.project.filter(|id| world.project(id).is_some());
                    world.add_card(card);
                }
                world.refit_projects();
                world.separate_all();
                self.saved_revision = world.revision;
            }
            Err(e) => {
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                let backup = self.path.with_extension(format!("json.corrupt-{stamp}"));
                let _ = fs::rename(&self.path, &backup);
                self.last_error = Some((t().layout_corrupt)(&e.to_string(), &backup.display().to_string()));
            }
        }
    }

    /// Debounced save: batches changes that come close together (dragging a card…).
    pub fn tick(&mut self, world: &World) {
        if world.revision == self.saved_revision {
            self.dirty_since = None;
            return;
        }
        let since = *self.dirty_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= SAVE_DELAY {
            self.save_now(world);
        }
    }

    pub fn save_now(&mut self, world: &World) {
        let file = LayoutFile {
            version: FORMAT_VERSION,
            camera: world.camera,
            cards: world.cards.clone(),
            projects: world.projects.clone(),
            wires: world.wires.clone(),
            space: world.space.clone(),
            space_cameras: world.space_cameras.clone(),
            changes_open: world.changes_open,
            tasks_open: world.tasks_open,
            free_grid: world.free_grid,
        };
        match write_atomic(&self.path, &serde_json::to_vec_pretty(&file).expect("layout serializes")) {
            Ok(()) => {
                self.saved_revision = world.revision;
                self.dirty_since = None;
                self.last_error = None;
            }
            Err(e) => self.last_error = Some((t().layout_save_failed)(&e.to_string())),
        }
    }

    pub fn wants_repaint(&self) -> bool {
        self.dirty_since.is_some()
    }
}

/// Writes to a sibling temp file, then renames: never a half-written layout.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::world::{CommandTemplate, QuickMessage};
    use egui::{Pos2, Rect, Vec2, pos2};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tcanvas-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn sample_templates() -> Vec<CommandTemplate> {
        vec![
            CommandTemplate { name: "Review".into(), command: "claude \"review it's $HOME\"\nnpm test".into(), task: true },
            CommandTemplate { name: "Logs".into(), command: "tail -f log/*.log".into(), task: false },
        ]
    }

    fn sample_messages() -> Vec<QuickMessage> {
        vec![
            QuickMessage { name: "Commit".into(), text: "Commit your changes, it's \"done\"\r\n- with a clear message".into(), enter: true },
            QuickMessage { name: "Draft".into(), text: "café 日本 \t$HOME \x1b[201~".into(), enter: false },
        ]
    }

    fn sample_world() -> World {
        let mut w = World::default();
        w.camera = Camera { center: pos2(12.0, -7.5), zoom: 0.42 };
        let mut a = Card::new(Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 200.0)), Some("~/api".into()));
        a.name = Some("server".into());
        a.stall_secs = 15;
        a.attention = true;
        a.project = Some("p1".into());
        let b = Card::new(Rect::from_min_size(pos2(400.0, 0.0), Vec2::new(300.0, 200.0)), None);
        w.projects.push(Project {
            id: "p1".into(),
            name: "API".into(),
            color: [1, 2, 3],
            rect: Rect::from_min_size(pos2(-50.0, -50.0), Vec2::splat(900.0)),
            cwd: Some("~/api".into()),
            templates: sample_templates(),
            messages: sample_messages(),
            worktree_location: Some("../{repo}-{name}".into()),
            task_settings: crate::shell::world::TaskSettings { prompt: Some("Do {task_title}".into()), branch_prefix: Some("work/".into()), skip_confirm: true },
        });
        w.projects.push(Project {
            id: "p2".into(),
            name: "Web".into(),
            color: [4, 5, 6],
            rect: Rect::from_min_size(pos2(2000.0, 0.0), Vec2::splat(400.0)),
            cwd: None,
            templates: Vec::new(),
            messages: Vec::new(),
            worktree_location: None,
            task_settings: Default::default(),
        });
        w.wires.push(Wire { from: "p1".into(), to: "p2".into() });
        w.add_card(a);
        w.add_card(b);
        w.refit_projects();
        w.changes_open = true;
        w.tasks_open = true;
        w.free_grid = true;
        w
    }

    #[test]
    fn roundtrip_keeps_layout_but_not_runtime_state() {
        let dir = temp_dir("roundtrip");
        let w = sample_world();
        Persistence::new(&dir).save_now(&w);

        let mut loaded = World::default();
        let mut p = Persistence::new(&dir);
        p.load(&mut loaded);
        assert_eq!(p.last_error, None);
        assert_eq!(loaded.camera, w.camera);
        assert_eq!(loaded.projects, w.projects);
        assert_eq!(loaded.projects[0].templates, sample_templates(), "command templates are kept as typed");
        assert_eq!(loaded.projects[0].messages, sample_messages(), "quick messages too, with their Enter flag");
        assert_eq!(loaded.projects[0].worktree_location.as_deref(), Some("../{repo}-{name}"), "and where its worktrees go");
        assert_eq!(loaded.wires, w.wires);
        assert_eq!(loaded.cards.len(), 2);
        let a = &loaded.cards[0];
        assert_eq!((a.id.as_str(), a.name.as_deref(), a.cwd.as_deref(), a.stall_secs), (w.cards[0].id.as_str(), Some("server"), Some("~/api"), 15));
        assert_eq!(a.rect, w.cards[0].rect);
        assert!(!a.attention, "runtime state is not saved");
        assert_eq!((a.project.as_deref(), loaded.cards[1].project.as_deref()), (Some("p1"), None));
        assert!(loaded.changes_open && loaded.tasks_open);
        assert!(loaded.free_grid);
        assert!(!dir.join("layout.json.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    /// Wires used to link terminals; now they link groups: old ones are dropped on load.
    #[test]
    fn wires_between_terminals_are_dropped_on_load() {
        let dir = temp_dir("old-wires");
        let mut w = sample_world();
        let (a, b) = (w.cards[0].id.clone(), w.cards[1].id.clone());
        w.wires.push(Wire { from: a, to: b });
        w.wires.push(Wire { from: "p1".into(), to: "gone".into() });
        Persistence::new(&dir).save_now(&w);
        let mut loaded = World::default();
        Persistence::new(&dir).load(&mut loaded);
        assert_eq!(loaded.wires, vec![Wire { from: "p1".into(), to: "p2".into() }]);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A version 1 layout: no `project` on cards, membership was geometric.
    #[test]
    fn version_1_layouts_get_membership_from_geometry() {
        let dir = temp_dir("v1");
        fs::create_dir_all(&dir).unwrap();
        let card = |id: &str, x: f32| format!(r#"{{"id":"{id}","name":null,"rect":{{"min":{{"x":{x},"y":0.0}},"max":{{"x":{},"y":100.0}}}},"cwd":null,"stall_secs":60}}"#, x + 100.0);
        let json = format!(
            r#"{{"version":1,"camera":{{"center":{{"x":0.0,"y":0.0}},"zoom":1.0}},"cards":[{},{}],
               "projects":[{{"id":"p","name":"API","color":[1,2,3],"rect":{{"min":{{"x":-10.0,"y":-10.0}},"max":{{"x":5000.0,"y":5000.0}}}},"cwd":null}}],"wires":[]}}"#,
            card("inside", 100.0),
            card("outside", 9000.0)
        );
        fs::write(dir.join("layout.json"), json).unwrap();
        let mut w = World::default();
        let mut p = Persistence::new(&dir);
        p.load(&mut w);
        assert_eq!(p.last_error, None);
        assert_eq!(w.card("inside").unwrap().project.as_deref(), Some("p"));
        assert_eq!(w.card("outside").unwrap().project, None);
        assert!(w.projects[0].rect.width() < 1000.0, "the frame fits its only member: {:?}", w.projects[0].rect);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A layout written before command templates: groups load without any, and a group without
    /// templates is saved as before (no new field).
    #[test]
    fn layouts_from_before_templates_still_load() {
        let dir = temp_dir("before-templates");
        fs::create_dir_all(&dir).unwrap();
        let json = r#"{"version":2,"camera":{"center":{"x":0.0,"y":0.0},"zoom":1.0},
            "cards":[{"id":"c1","name":null,"rect":{"min":{"x":0.0,"y":0.0},"max":{"x":100.0,"y":100.0}},"cwd":"~/api","stall_secs":60,"project":"p"}],
            "projects":[{"id":"p","name":"API","color":[1,2,3],"rect":{"min":{"x":-10.0,"y":-10.0},"max":{"x":500.0,"y":500.0}},"cwd":"~/api"}],
            "wires":[],"space":null,"space_cameras":{},"changes_open":false,"free_grid":false}"#;
        fs::write(dir.join("layout.json"), json).unwrap();
        let mut w = World::default();
        let mut p = Persistence::new(&dir);
        p.load(&mut w);
        assert_eq!(p.last_error, None);
        assert_eq!((w.projects.len(), w.cards.len()), (1, 1));
        assert!(w.projects[0].templates.is_empty());
        assert!(w.projects[0].messages.is_empty());
        assert_eq!(w.projects[0].worktree_location, None, "worktrees go to the default location");
        p.save_now(&w);
        let saved = fs::read_to_string(dir.join("layout.json")).unwrap();
        assert!(!saved.contains("templates") && !saved.contains("messages") && !saved.contains("worktree_location"), "{saved}");
        fs::remove_dir_all(dir).unwrap();
    }

    /// A layout written with command templates but before quick messages loads with none; a
    /// message saved without its Enter flag presses Enter.
    #[test]
    fn layouts_from_before_quick_messages_still_load() {
        let dir = temp_dir("before-messages");
        fs::create_dir_all(&dir).unwrap();
        let project = |extra: &str| {
            format!(
                r#"{{"id":"p","name":"API","color":[1,2,3],"rect":{{"min":{{"x":-10.0,"y":-10.0}},"max":{{"x":500.0,"y":500.0}}}},"cwd":"~/api","templates":[{{"name":"Logs","command":"tail -f x"}}]{extra}}}"#
            )
        };
        let layout = |extra: &str| format!(r#"{{"version":2,"camera":{{"center":{{"x":0.0,"y":0.0}},"zoom":1.0}},"cards":[],"projects":[{}],"wires":[]}}"#, project(extra));
        fs::write(dir.join("layout.json"), layout("")).unwrap();
        let mut w = World::default();
        let mut p = Persistence::new(&dir);
        p.load(&mut w);
        assert_eq!(p.last_error, None);
        assert_eq!(w.projects[0].templates.len(), 1);
        assert!(w.projects[0].messages.is_empty());
        fs::write(dir.join("layout.json"), layout(r#","messages":[{"name":"Go","text":"go on"}]"#)).unwrap();
        let mut w = World::default();
        Persistence::new(&dir).load(&mut w);
        assert_eq!(w.projects[0].messages, vec![QuickMessage { name: "Go".into(), text: "go on".into(), enter: true }]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_file_is_an_empty_canvas() {
        let mut w = World::default();
        let mut p = Persistence::new(&temp_dir("missing"));
        p.load(&mut w);
        assert!(w.cards.is_empty() && p.last_error.is_none());
    }

    #[test]
    fn corrupt_file_is_set_aside_not_lost() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let dir = temp_dir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("layout.json"), b"{ pas du json").unwrap();
        let mut w = World::default();
        let mut p = Persistence::new(&dir);
        p.load(&mut w);
        assert!(w.cards.is_empty());
        assert!(p.last_error.as_deref().unwrap().contains("mis de côté"));
        let backups: Vec<_> = fs::read_dir(&dir).unwrap().filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().contains("corrupt")).collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(backups[0].path()).unwrap(), b"{ pas du json");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tick_debounces_saves() {
        let dir = temp_dir("debounce");
        let mut w = sample_world();
        let mut p = Persistence::new(&dir);
        p.tick(&w);
        assert!(!p.path().exists(), "no immediate write");
        assert!(p.wants_repaint());
        std::thread::sleep(SAVE_DELAY + Duration::from_millis(50));
        p.tick(&w);
        assert!(p.path().exists());
        assert!(!p.wants_repaint());
        w.touch();
        p.tick(&w);
        assert!(p.wants_repaint());
        fs::remove_dir_all(dir).unwrap();
    }
}
