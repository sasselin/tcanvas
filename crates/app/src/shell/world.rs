//! Données partagées par toutes les slices : cartes, projets, fils, caméra.
//! Chaque slice lit le monde ; les modifications transverses passent par `Event`.

use std::collections::BTreeMap;

use egui::{Pos2, Rect, Vec2};
use protocol::{DEFAULT_STALL_SECS, SESSION_PREFIX, SessionInfo, TermStatus};
use serde::{Deserialize, Serialize};

pub type CardId = String;
pub type ProjectId = String;

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    /// Point du monde affiché au centre de l'écran.
    pub center: Pos2,
    pub zoom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { center: Pos2::ZERO, zoom: 1.0 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Card {
    pub id: CardId,
    /// Nom choisi par l'utilisateur ; sinon le titre est déduit de la commande ou du dossier.
    pub name: Option<String>,
    pub rect: Rect,
    /// Dossier de départ (mis à jour depuis l'agent pour la restauration).
    pub cwd: Option<String>,
    pub stall_secs: u64,
    #[serde(skip)]
    pub info: Option<SessionInfo>,
    /// Une notification attend que l'utilisateur regarde ce terminal.
    #[serde(skip)]
    pub attention: bool,
    /// Le client tmux de cette carte s'est terminé (session tuée, `exit`…).
    #[serde(skip)]
    pub pty_dead: bool,
    #[serde(skip)]
    pub z: u64,
}

impl Card {
    pub fn new(rect: Rect, cwd: Option<String>) -> Self {
        Self::with_id(new_id(), rect, cwd)
    }

    pub fn with_id(id: CardId, rect: Rect, cwd: Option<String>) -> Self {
        Self { id, name: None, rect, cwd, stall_secs: DEFAULT_STALL_SECS, info: None, attention: false, pty_dead: false, z: 0 }
    }

    pub fn session(&self) -> String {
        format!("{SESSION_PREFIX}{}", self.id)
    }

    pub fn status(&self) -> Option<TermStatus> {
        if self.pty_dead {
            return Some(TermStatus::Dead);
        }
        self.info.as_ref().map(|i| i.status)
    }

    pub fn current_cwd(&self) -> Option<&str> {
        self.info.as_ref().and_then(|i| i.cwd.as_deref()).or(self.cwd.as_deref())
    }

    pub fn title(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        if let Some(cmd) = self.info.as_ref().and_then(|i| i.command.as_deref()) {
            return short_command(cmd);
        }
        match self.current_cwd() {
            Some(cwd) => basename(cwd).to_string(),
            None => "terminal".into(),
        }
    }
}

fn basename(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("/")
}

/// `/usr/bin/python3 -m http.server` → `python3 -m http.server`, coupé à 40 caractères.
fn short_command(cmd: &str) -> String {
    let mut parts = cmd.splitn(2, ' ');
    let prog = basename(parts.next().unwrap_or_default());
    let full = match parts.next() {
        Some(rest) => format!("{prog} {rest}"),
        None => prog.to_string(),
    };
    if full.chars().count() > 40 { format!("{}…", full.chars().take(39).collect::<String>()) } else { full }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub color: [u8; 3],
    pub rect: Rect,
    /// Dossier où s'ouvrent les nouveaux terminaux du projet.
    pub cwd: Option<String>,
}

pub const PROJECT_COLORS: [[u8; 3]; 8] = [
    [94, 129, 244],
    [46, 170, 140],
    [230, 150, 60],
    [200, 90, 160],
    [130, 110, 220],
    [220, 80, 80],
    [70, 160, 200],
    [160, 170, 70],
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wire {
    pub from: CardId,
    pub to: CardId,
}

impl Wire {
    pub fn touches(&self, id: &str) -> bool {
        self.from == id || self.to == id
    }
    pub fn same_as(&self, a: &str, b: &str) -> bool {
        (self.from == a && self.to == b) || (self.from == b && self.to == a)
    }
}

#[derive(Default)]
pub struct World {
    pub cards: Vec<Card>,
    pub projects: Vec<Project>,
    pub wires: Vec<Wire>,
    pub camera: Camera,
    /// Espace affiché : un projet seul, ou tous (`None`). Les autres sont cachés.
    pub space: Option<ProjectId>,
    /// Caméra retenue par espace (clé : id du projet, `""` pour « Tous »).
    pub space_cameras: BTreeMap<String, Camera>,
    /// Carte qui reçoit le clavier.
    pub focused: Option<CardId>,
    pub window_focused: bool,
    /// Incrémenté à chaque changement à sauvegarder.
    pub revision: u64,
    next_z: u64,
}

impl World {
    pub fn touch(&mut self) {
        self.revision += 1;
    }

    pub fn card(&self, id: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.id == id)
    }

    pub fn card_mut(&mut self, id: &str) -> Option<&mut Card> {
        self.cards.iter_mut().find(|c| c.id == id)
    }

    pub fn card_by_session(&self, session: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.session() == session)
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// Met la carte au premier plan.
    pub fn raise(&mut self, id: &str) {
        self.next_z += 1;
        let z = self.next_z;
        if let Some(c) = self.card_mut(id) {
            c.z = z;
        }
    }

    pub fn add_card(&mut self, card: Card) {
        let id = card.id.clone();
        self.cards.push(card);
        self.raise(&id);
        self.touch();
    }

    pub fn remove_card(&mut self, id: &str) {
        self.cards.retain(|c| c.id != id);
        self.wires.retain(|w| !w.touches(id));
        if self.focused.as_deref() == Some(id) {
            self.focused = None;
        }
        self.touch();
    }

    /// Projet (cadre) qui contient le centre de la carte ; le plus petit gagne s'ils s'emboîtent.
    pub fn project_of(&self, card: &Card) -> Option<&Project> {
        project_at(&self.projects, card.rect.center())
    }

    /// La carte fait partie de l'espace affiché (son centre est dans le cadre du projet actif).
    pub fn shows_card(&self, card: &Card) -> bool {
        match &self.space {
            None => true,
            Some(id) => self.project(id).is_some_and(|p| p.rect.contains(card.rect.center())),
        }
    }

    /// Le projet actif lui-même, ou un sous-projet entièrement dans son cadre.
    pub fn shows_project(&self, project: &Project) -> bool {
        match &self.space {
            None => true,
            Some(id) => &project.id == id || self.project(id).is_some_and(|p| p.rect.contains_rect(project.rect)),
        }
    }

    /// Dans un espace de projet, agrandit son cadre pour contenir la carte : une carte
    /// déplacée ou créée au bord ne disparaît jamais de l'espace.
    pub fn keep_in_space(&mut self, card_id: &str) {
        let Some(space) = self.space.clone() else { return };
        let Some(rect) = self.card(card_id).map(|c| c.rect) else { return };
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == space)
            && !p.rect.contains_rect(rect) {
                p.rect = p.rect.union(rect.expand(SPACE_MARGIN));
                self.revision += 1;
            }
    }

    /// Ordre de dessin : du fond vers le premier plan.
    pub fn cards_by_z(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.cards.len()).collect();
        order.sort_by_key(|&i| self.cards[i].z);
        order
    }

    /// Une place libre près de `near` pour une carte de taille `size` (spirale de pas en pas).
    pub fn free_spot(&self, near: Pos2, size: Vec2) -> Rect {
        let step = 40.0;
        for ring in 0..60 {
            for i in 0..(ring * 8).max(1) {
                let angle = i as f32 / (ring * 8).max(1) as f32 * std::f32::consts::TAU;
                let offset = Vec2::angled(angle) * ring as f32 * step;
                let r = Rect::from_center_size(near + offset, size);
                if !self.cards.iter().any(|c| c.rect.expand(16.0).intersects(r)) {
                    return r;
                }
            }
        }
        Rect::from_center_size(near, size)
    }
}

/// Marge laissée autour d'une carte quand le cadre du projet s'agrandit pour elle.
const SPACE_MARGIN: f32 = 40.0;

pub fn project_at(projects: &[Project], p: Pos2) -> Option<&Project> {
    projects
        .iter()
        .filter(|pr| pr.rect.contains(p))
        .min_by(|a, b| a.rect.area().total_cmp(&b.rect.area()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn card_at(x: f32, y: f32) -> Card {
        Card::new(Rect::from_min_size(pos2(x, y), Vec2::new(100.0, 60.0)), None)
    }

    #[test]
    fn titles_fall_back_from_name_to_command_to_cwd() {
        let mut c = card_at(0.0, 0.0);
        assert_eq!(c.title(), "terminal");
        c.cwd = Some("/home/me/projet/".into());
        assert_eq!(c.title(), "projet");
        c.info = Some(SessionInfo {
            session: c.session(),
            status: TermStatus::Running,
            command: Some("/usr/bin/python3 -m http.server".into()),
            cwd: Some("/srv".into()),
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
        });
        assert_eq!(c.title(), "python3 -m http.server");
        c.name = Some("API".into());
        assert_eq!(c.title(), "API");
    }

    #[test]
    fn long_commands_are_shortened() {
        let t = short_command(&format!("cargo {}", "x".repeat(80)));
        assert_eq!(t.chars().count(), 40);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn smallest_containing_project_wins() {
        let big = Project { id: "big".into(), name: "big".into(), color: [0; 3], rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)), cwd: None };
        let small = Project { id: "small".into(), rect: Rect::from_min_size(pos2(100.0, 100.0), Vec2::splat(300.0)), ..big.clone() };
        let mut w = World { projects: vec![big, small], ..Default::default() };
        w.add_card(card_at(150.0, 150.0));
        w.add_card(card_at(700.0, 700.0));
        w.add_card(card_at(5000.0, 0.0));
        let ids: Vec<Option<&str>> = w.cards.iter().map(|c| w.project_of(c).map(|p| p.id.as_str())).collect();
        assert_eq!(ids, vec![Some("small"), Some("big"), None]);
    }

    #[test]
    fn a_project_space_shows_only_its_cards_and_subprojects() {
        let api = Project { id: "api".into(), name: "api".into(), color: [0; 3], rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)), cwd: None };
        let sub = Project { id: "sub".into(), rect: Rect::from_min_size(pos2(100.0, 100.0), Vec2::splat(300.0)), ..api.clone() };
        let web = Project { id: "web".into(), rect: Rect::from_min_size(pos2(5000.0, 0.0), Vec2::splat(1000.0)), ..api.clone() };
        let mut w = World { projects: vec![api, sub, web], ..Default::default() };
        w.add_card(card_at(150.0, 150.0));
        w.add_card(card_at(5100.0, 100.0));
        w.add_card(card_at(-9000.0, 0.0));
        let shown = |w: &World| (w.cards.iter().map(|c| w.shows_card(c)).collect::<Vec<_>>(), w.projects.iter().map(|p| w.shows_project(p)).collect::<Vec<_>>());
        assert_eq!(shown(&w), (vec![true; 3], vec![true; 3]), "« Tous » montre tout");
        w.space = Some("api".into());
        assert_eq!(shown(&w), (vec![true, false, false], vec![true, true, false]));
        w.space = Some("web".into());
        assert_eq!(shown(&w), (vec![false, true, false], vec![false, false, true]));
        w.space = Some("disparu".into());
        assert_eq!(shown(&w).0, vec![false; 3]);
    }

    #[test]
    fn keep_in_space_grows_the_active_project_only() {
        let api = Project { id: "api".into(), name: "api".into(), color: [0; 3], rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)), cwd: None };
        let mut w = World { projects: vec![api], ..Default::default() };
        let c = card_at(950.0, 100.0);
        let id = c.id.clone();
        w.add_card(c);
        w.keep_in_space(&id);
        assert_eq!(w.projects[0].rect.max.x, 1000.0, "hors espace de projet, rien ne bouge");
        w.space = Some("api".into());
        let rev = w.revision;
        w.keep_in_space(&id);
        assert!(w.projects[0].rect.contains_rect(w.cards[0].rect));
        assert!(w.revision > rev);
        let before = w.projects[0].rect;
        w.keep_in_space(&id);
        assert_eq!(w.projects[0].rect, before, "déjà dedans : inchangé");
    }

    #[test]
    fn removing_a_card_drops_its_wires_and_focus() {
        let mut w = World::default();
        let (a, b) = (card_at(0.0, 0.0), card_at(500.0, 0.0));
        let (ia, ib) = (a.id.clone(), b.id.clone());
        w.add_card(a);
        w.add_card(b);
        w.wires.push(Wire { from: ia.clone(), to: ib.clone() });
        w.focused = Some(ia.clone());
        w.remove_card(&ia);
        assert!(w.wires.is_empty());
        assert_eq!(w.focused, None);
        assert_eq!(w.cards.len(), 1);
    }

    #[test]
    fn raise_orders_cards() {
        let mut w = World::default();
        let (a, b) = (card_at(0.0, 0.0), card_at(10.0, 0.0));
        let ia = a.id.clone();
        w.add_card(a);
        w.add_card(b);
        w.raise(&ia);
        assert_eq!(w.cards[*w.cards_by_z().last().unwrap()].id, ia);
    }

    #[test]
    fn free_spot_avoids_overlap() {
        let mut w = World::default();
        let size = Vec2::new(100.0, 60.0);
        for _ in 0..5 {
            let r = w.free_spot(Pos2::ZERO, size);
            assert!(w.cards.iter().all(|c| !c.rect.intersects(r)));
            w.add_card(Card::new(r, None));
        }
    }
}
