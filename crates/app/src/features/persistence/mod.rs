//! Sauvegarde du canevas (cartes, projets, fils, caméra) dans `layout.json`.
//! Écriture atomique ; un fichier illisible est mis de côté, jamais écrasé en silence.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::shell::world::{Camera, Card, Project, ProjectId, Wire, World, project_at};

/// 2 : appartenance explicite des cartes aux groupes (`Card::project`), panneau « Changements ».
const FORMAT_VERSION: u32 = 2;
const SAVE_DELAY: Duration = Duration::from_millis(800);

#[derive(Serialize, Deserialize)]
pub struct LayoutFile {
    pub version: u32,
    pub camera: Camera,
    pub cards: Vec<Card>,
    pub projects: Vec<Project>,
    pub wires: Vec<Wire>,
    /// Espace ouvert et caméra de chaque espace (absents des fichiers plus anciens).
    #[serde(default)]
    pub space: Option<ProjectId>,
    #[serde(default)]
    pub space_cameras: BTreeMap<String, Camera>,
    #[serde(default)]
    pub changes_open: bool,
}

/// `%APPDATA%\tcanvas\config` sous Windows, `~/.config/tcanvas` sous Linux.
/// `TCANVAS_CONFIG_DIR` le remplace (tests, profils séparés).
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

    /// Charge le canevas. Un fichier absent donne un canevas vide ; un fichier corrompu est
    /// renommé en `layout.json.corrompu-<horodatage>` et signalé.
    pub fn load(&mut self, world: &mut World) {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => {
                self.last_error = Some(format!("lecture de {} impossible : {e}", self.path.display()));
                return;
            }
        };
        match serde_json::from_str::<LayoutFile>(&text) {
            Ok(file) => {
                world.camera = file.camera;
                world.projects = file.projects;
                world.wires = file.wires;
                world.space = file.space.filter(|id| world.project(id).is_some());
                world.space_cameras = file.space_cameras;
                world.changes_open = file.changes_open;
                for mut card in file.cards {
                    // Version 1 : une carte appartenait au cadre qui contenait son centre.
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
                let backup = self.path.with_extension(format!("json.corrompu-{stamp}"));
                let _ = fs::rename(&self.path, &backup);
                self.last_error = Some(format!("layout illisible ({e}), mis de côté dans {}", backup.display()));
            }
        }
    }

    /// Sauvegarde différée : regroupe les changements rapprochés (glisser une carte…).
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
        };
        match write_atomic(&self.path, &serde_json::to_vec_pretty(&file).expect("layout serializes")) {
            Ok(()) => {
                self.saved_revision = world.revision;
                self.dirty_since = None;
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(format!("sauvegarde impossible : {e}")),
        }
    }

    pub fn wants_repaint(&self) -> bool {
        self.dirty_since.is_some()
    }
}

/// Écrit dans un fichier temporaire voisin puis renomme : jamais de layout à moitié écrit.
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
    use egui::{Pos2, Rect, Vec2, pos2};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tcanvas-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn sample_world() -> World {
        let mut w = World::default();
        w.camera = Camera { center: pos2(12.0, -7.5), zoom: 0.42 };
        let mut a = Card::new(Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 200.0)), Some("~/api".into()));
        a.name = Some("serveur".into());
        a.stall_secs = 15;
        a.attention = true;
        a.project = Some("p1".into());
        let b = Card::new(Rect::from_min_size(pos2(400.0, 0.0), Vec2::new(300.0, 200.0)), None);
        w.wires.push(Wire { from: a.id.clone(), to: b.id.clone() });
        w.projects.push(Project { id: "p1".into(), name: "API".into(), color: [1, 2, 3], rect: Rect::from_min_size(pos2(-50.0, -50.0), Vec2::splat(900.0)), cwd: Some("~/api".into()) });
        w.add_card(a);
        w.add_card(b);
        w.refit_projects();
        w.changes_open = true;
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
        assert_eq!(loaded.wires, w.wires);
        assert_eq!(loaded.cards.len(), 2);
        let a = &loaded.cards[0];
        assert_eq!((a.id.as_str(), a.name.as_deref(), a.cwd.as_deref(), a.stall_secs), (w.cards[0].id.as_str(), Some("serveur"), Some("~/api"), 15));
        assert_eq!(a.rect, w.cards[0].rect);
        assert!(!a.attention, "l'état d'exécution n'est pas sauvegardé");
        assert_eq!((a.project.as_deref(), loaded.cards[1].project.as_deref()), (Some("p1"), None));
        assert!(loaded.changes_open);
        assert!(!dir.join("layout.json.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    /// Un layout de la version 1 : pas de `project` sur les cartes, l'appartenance était géométrique.
    #[test]
    fn version_1_layouts_get_membership_from_geometry() {
        let dir = temp_dir("v1");
        fs::create_dir_all(&dir).unwrap();
        let card = |id: &str, x: f32| format!(r#"{{"id":"{id}","name":null,"rect":{{"min":{{"x":{x},"y":0.0}},"max":{{"x":{},"y":100.0}}}},"cwd":null,"stall_secs":60}}"#, x + 100.0);
        let json = format!(
            r#"{{"version":1,"camera":{{"center":{{"x":0.0,"y":0.0}},"zoom":1.0}},"cards":[{},{}],
               "projects":[{{"id":"p","name":"API","color":[1,2,3],"rect":{{"min":{{"x":-10.0,"y":-10.0}},"max":{{"x":5000.0,"y":5000.0}}}},"cwd":null}}],"wires":[]}}"#,
            card("dedans", 100.0),
            card("dehors", 9000.0)
        );
        fs::write(dir.join("layout.json"), json).unwrap();
        let mut w = World::default();
        let mut p = Persistence::new(&dir);
        p.load(&mut w);
        assert_eq!(p.last_error, None);
        assert_eq!(w.card("dedans").unwrap().project.as_deref(), Some("p"));
        assert_eq!(w.card("dehors").unwrap().project, None);
        assert!(w.projects[0].rect.width() < 1000.0, "le cadre s'ajuste à son seul membre : {:?}", w.projects[0].rect);
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
        let dir = temp_dir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("layout.json"), b"{ pas du json").unwrap();
        let mut w = World::default();
        let mut p = Persistence::new(&dir);
        p.load(&mut w);
        assert!(w.cards.is_empty());
        assert!(p.last_error.as_deref().unwrap().contains("mis de côté"));
        let backups: Vec<_> = fs::read_dir(&dir).unwrap().filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().contains("corrompu")).collect();
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
        assert!(!p.path().exists(), "pas d'écriture immédiate");
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
