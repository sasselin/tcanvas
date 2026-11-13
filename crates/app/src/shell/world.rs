//! Data shared by all slices: cards, projects, wires between projects, camera.
//! Every slice reads the world; cross-cutting changes go through `Event`.

use std::collections::BTreeMap;

use egui::{Pos2, Rect, Vec2};
use protocol::{DEFAULT_STALL_SECS, SESSION_PREFIX, SessionInfo, TermStatus};
use serde::{Deserialize, Serialize};

pub type CardId = String;
pub type ProjectId = String;

/// Wall clock in Unix seconds (age of modified files).
pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    /// World point shown at the center of the screen.
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
    /// Name chosen by the user; otherwise the title is derived from the command or folder.
    pub name: Option<String>,
    pub rect: Rect,
    /// Starting folder (updated from the agent for restoring).
    pub cwd: Option<String>,
    pub stall_secs: u64,
    /// Group (project) the terminal belongs to. The group's frame fits its members.
    #[serde(default)]
    pub project: Option<ProjectId>,
    #[serde(skip)]
    pub info: Option<SessionInfo>,
    /// A notification is waiting for the user to look at this terminal.
    #[serde(skip)]
    pub attention: bool,
    /// This card's tmux client has exited (session killed, `exit`…).
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
        Self { id, name: None, rect, cwd, stall_secs: DEFAULT_STALL_SECS, project: None, info: None, attention: false, pty_dead: false, z: 0 }
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

/// `/usr/bin/python3 -m http.server` → `python3 -m http.server`, truncated to 40 characters.
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
    /// Frame: computed from the members (`World::refit_projects`); positions the group when empty.
    pub rect: Rect,
    /// Linked folder: the project's new terminals open there, and its changes are tracked.
    pub cwd: Option<String>,
}

pub const PROJECT_COLORS: [[u8; 3]; 8] = [[94, 129, 244], [46, 170, 140], [230, 150, 60], [200, 90, 160], [130, 110, 220], [220, 80, 80], [70, 160, 200], [160, 170, 70]];

/// A link between two groups (projects), drawn from frame to frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wire {
    pub from: ProjectId,
    pub to: ProjectId,
}

impl Wire {
    pub fn touches(&self, id: &str) -> bool {
        self.from == id || self.to == id
    }
    pub fn same_as(&self, a: &str, b: &str) -> bool {
        (self.from == a && self.to == b) || (self.from == b && self.to == a)
    }
    /// The end that is not `id`.
    pub fn other(&self, id: &str) -> &str {
        if self.from == id { &self.to } else { &self.from }
    }
}

/// The git worktree holding a card's folder: what its branch badge shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardWorktree {
    /// Folder of the worktree.
    pub path: String,
    /// `None`: detached HEAD.
    pub branch: Option<String>,
    /// Folder name of a linked worktree; `None` for the main checkout.
    pub linked: Option<String>,
}

#[derive(Default)]
pub struct World {
    pub cards: Vec<Card>,
    pub projects: Vec<Project>,
    pub wires: Vec<Wire>,
    pub camera: Camera,
    /// Displayed space: a single project, or all of them (`None`). The others are hidden.
    pub space: Option<ProjectId>,
    /// Camera remembered per space (key: project id, `""` for "All").
    pub space_cameras: BTreeMap<String, Camera>,
    /// Card that receives keyboard input.
    pub focused: Option<CardId>,
    /// Card being dragged: its group's frame doesn't follow it until it is dropped.
    pub dragging: Option<CardId>,
    /// "Changes" panel open (saved).
    pub changes_open: bool,
    /// Option "free grid" (saved): inside a group, the group is the whole canvas (no frame,
    /// a terminal can't be dragged out of it).
    pub free_grid: bool,
    /// Last file modification seen in each project's linked folder (Unix seconds).
    pub file_activity: BTreeMap<ProjectId, u64>,
    /// Number of uncommitted files in each project's linked folder (Δ badge).
    pub change_counts: BTreeMap<ProjectId, usize>,
    /// The git worktree each card's folder is in (kept by the `worktrees` slice).
    pub card_worktrees: BTreeMap<CardId, CardWorktree>,
    pub window_focused: bool,
    /// Incremented on every change that needs saving.
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

    /// Brings the card to the front.
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
        if self.focused.as_deref() == Some(id) {
            self.focused = None;
        }
        self.touch();
    }

    /// Group the card belongs to.
    pub fn project_of(&self, card: &Card) -> Option<&Project> {
        card.project.as_deref().and_then(|id| self.project(id))
    }

    /// Member terminals of the group.
    pub fn members<'a>(&'a self, project: &'a str) -> impl Iterator<Item = &'a Card> + 'a {
        self.cards.iter().filter(move |c| c.project.as_deref() == Some(project))
    }

    /// Every group reachable from `id` through the wires (directly or through other groups),
    /// without `id` itself, sorted by name.
    pub fn linked_groups(&self, id: &str) -> Vec<&Project> {
        let mut seen: Vec<&str> = vec![id];
        let mut next = 0;
        while next < seen.len() {
            let at = seen[next];
            next += 1;
            for w in self.wires.iter().filter(|w| w.touches(at)) {
                let other = w.other(at);
                if !seen.contains(&other) && self.project(other).is_some() {
                    seen.push(other);
                }
            }
        }
        let mut out: Vec<&Project> = seen[1..].iter().filter_map(|p| self.project(p)).collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// The card is part of the displayed space (member of the open project, or anything in "All").
    pub fn shows_card(&self, card: &Card) -> bool {
        match &self.space {
            None => true,
            Some(id) => card.project.as_ref() == Some(id) && self.project(id).is_some(),
        }
    }

    pub fn shows_project(&self, project: &Project) -> bool {
        self.space.as_ref().is_none_or(|id| &project.id == id)
    }

    /// The terminal `anchor` was just placed (or resized): it keeps its spot and the other
    /// members of its group that overlap it move aside, cascading, along the shortest axis.
    /// Nothing outside the group moves. Returns `true` if a card moved.
    pub fn settle(&mut self, anchor: &str) -> bool {
        let Some(project) = self.card(anchor).and_then(|c| c.project.clone()) else { return false };
        let members: Vec<usize> = (0..self.cards.len()).filter(|&i| self.cards[i].project.as_ref() == Some(&project)).collect();
        let Some(&a) = members.iter().find(|&&i| self.cards[i].id == anchor) else { return false };
        let mut queue = std::collections::VecDeque::from([a]);
        let mut moved = false;
        // Safety net: a pathological layout must never freeze the UI.
        let mut budget = 50 * members.len();
        while let Some(c) = queue.pop_front() {
            for &m in &members {
                if m == c || budget == 0 {
                    continue;
                }
                let (fixed, mobile) = if m == a { (m, c) } else { (c, m) };
                if fixed == mobile {
                    continue;
                }
                let push = separation(self.cards[fixed].rect, self.cards[mobile].rect, CARD_GAP);
                if push != Vec2::ZERO {
                    budget -= 1;
                    self.cards[mobile].rect = self.cards[mobile].rect.translate(push);
                    moved = true;
                    queue.push_back(mobile);
                }
            }
        }
        if moved {
            self.touch();
        }
        moved
    }

    /// A group is open with the "free grid" option: the group is the whole canvas.
    pub fn free_grid_active(&self) -> bool {
        self.free_grid && self.space.as_ref().is_some_and(|id| self.project(id).is_some())
    }

    /// Visible group whose frame contains `at` (the smallest): a card dropped there joins it.
    /// With the free grid, the open group everywhere.
    pub fn drop_target(&self, at: Pos2) -> Option<ProjectId> {
        if self.free_grid_active() {
            return self.space.clone();
        }
        self.projects.iter().filter(|p| self.shows_project(p) && p.rect.contains(at)).min_by(|a, b| a.rect.area().total_cmp(&b.rect.area())).map(|p| p.id.clone())
    }

    /// Fits each frame to its terminals (plus a margin). While a terminal is dragged, its
    /// group's frame is frozen: it doesn't follow, so the terminal can be dragged out. An empty group
    /// keeps a minimum size around its last center. With the free grid nothing is frozen: the
    /// terminal can't leave its group, the frame just follows it.
    /// A frame that changes size pushes away what it touches (see `separate`).
    pub fn refit_projects(&mut self) {
        let frozen = self.dragging.as_deref().and_then(|id| self.card(id)).and_then(|c| c.project.clone()).filter(|_| !self.free_grid_active());
        let mut changed = Vec::new();
        for p in self.projects.iter_mut().filter(|p| frozen.as_ref() != Some(&p.id)) {
            let fit = self
                .cards
                .iter()
                .filter(|c| c.project.as_ref() == Some(&p.id))
                .map(|c| c.rect)
                .reduce(|a, b| a.union(b))
                .map(|r| r.expand(PROJECT_PAD))
                .unwrap_or_else(|| Rect::from_center_size(p.rect.center(), EMPTY_PROJECT));
            if fit != p.rect {
                p.rect = fit;
                changed.push(p.id.clone());
            }
        }
        if !changed.is_empty() {
            self.touch();
        }
        for id in changed {
            self.separate(&Body::Group(id));
        }
    }

    /// What must never overlap: groups (frame and label, with their members)
    /// and terminals outside any group.
    fn bodies(&self) -> Vec<Body> {
        let groups = self.projects.iter().map(|p| Body::Group(p.id.clone()));
        groups.chain(self.cards.iter().filter(|c| c.project.is_none()).map(|c| Body::Card(c.id.clone()))).collect()
    }

    /// Footprint of a body on the canvas (label included for a group).
    pub fn body_rect(&self, body: &Body) -> Option<Rect> {
        match body {
            // The label sticks out above the frame: it counts too.
            Body::Group(id) => self.project(id).map(|p| Rect::from_min_max(p.rect.min - Vec2::new(0.0, GROUP_LABEL), p.rect.max)),
            Body::Card(id) => self.card(id).map(|c| c.rect),
        }
    }

    fn move_body(&mut self, body: &Body, delta: Vec2) {
        match body {
            Body::Group(id) => {
                for c in self.cards.iter_mut().filter(|c| c.project.as_ref() == Some(id)) {
                    c.rect = c.rect.translate(delta);
                }
                if let Some(p) = self.projects.iter_mut().find(|p| &p.id == id) {
                    p.rect = p.rect.translate(delta);
                }
            }
            Body::Card(id) => {
                if let Some(c) = self.card_mut(id) {
                    c.rect = c.rect.translate(delta);
                }
            }
        }
    }

    /// `anchor` just changed (group grown, terminal placed or created): everything it overlaps
    /// moves aside, cascading, along the shortest axis. A terminal outside any group always yields
    /// to a group (we don't push a whole group because a terminal brushed it); between
    /// two bodies of the same kind, the one that didn't move gives way. Returns `true` if
    /// anything moved.
    pub fn separate(&mut self, anchor: &Body) -> bool {
        let bodies = self.bodies();
        let Some(a) = bodies.iter().position(|b| b == anchor) else { return false };
        let mut queue = std::collections::VecDeque::from([a]);
        let mut moved = false;
        let mut budget = 50 * bodies.len();
        while let Some(c) = queue.pop_front() {
            for m in 0..bodies.len() {
                if m == c || budget == 0 {
                    continue;
                }
                let mobile = match (&bodies[c], &bodies[m]) {
                    (Body::Card(_), Body::Group(_)) => c,
                    (Body::Group(_), Body::Card(_)) => m,
                    _ if m == a => c,
                    _ => m,
                };
                let fixed = if mobile == c { m } else { c };
                let (Some(rf), Some(rm)) = (self.body_rect(&bodies[fixed]), self.body_rect(&bodies[mobile])) else { continue };
                let push = separation(rf, rm, BODY_GAP);
                if push != Vec2::ZERO {
                    budget -= 1;
                    // A free terminal jumps to the nearest free spot (pushing it
                    // blindly can wedge it between two obstacles); a group is pushed.
                    let delta = match &bodies[mobile] {
                        Body::Card(_) => self.clear_spot(&bodies[mobile], rm).center() - rm.center(),
                        Body::Group(_) => push,
                    };
                    self.move_body(&bodies[mobile], delta);
                    moved = true;
                    if !queue.contains(&mobile) {
                        queue.push_back(mobile);
                    }
                }
            }
        }
        if moved {
            self.touch();
        }
        moved
    }

    /// Nearest spot to `rect` that touches no other body (gap included).
    fn clear_spot(&self, body: &Body, rect: Rect) -> Rect {
        let obstacles: Vec<Rect> = self.bodies().iter().filter(|b| *b != body).filter_map(|b| self.body_rect(b)).map(|r| r.expand(BODY_GAP)).collect();
        let step = 40.0;
        for ring in 0..400 {
            let n = (ring * 8).max(1);
            for i in 0..n {
                let offset = Vec2::angled(i as f32 / n as f32 * std::f32::consts::TAU) * ring as f32 * step;
                let r = rect.translate(offset);
                if !obstacles.iter().any(|o| o.intersects(r)) {
                    return r;
                }
            }
        }
        rect
    }

    /// A body that just appeared (new group) goes to the nearest free spot, without
    /// pushing anything. Returns `true` if it moved.
    pub fn place_clear(&mut self, body: &Body) -> bool {
        let Some(r) = self.body_rect(body) else { return false };
        let delta = self.clear_spot(body, r).center() - r.center();
        if delta == Vec2::ZERO {
            return false;
        }
        self.move_body(body, delta);
        self.touch();
        true
    }

    /// Puts everything back in order (loading an old layout where things overlap).
    pub fn separate_all(&mut self) {
        for body in self.bodies() {
            self.separate(&body);
        }
    }

    /// Draw order: from back to front.
    pub fn cards_by_z(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.cards.len()).collect();
        order.sort_by_key(|&i| self.cards[i].z);
        order
    }

    /// A free spot near `near` for a card of size `size` (step-by-step spiral).
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

/// What must not overlap on the canvas: a whole group, or a terminal outside any group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Group(ProjectId),
    Card(CardId),
}

/// Minimum spacing between two groups, or a group and a terminal outside any group.
pub const BODY_GAP: f32 = 60.0;
/// Height reserved for the label above a frame (world units).
const GROUP_LABEL: f32 = 40.0;

/// Minimum spacing between two terminals of the same group.
pub const CARD_GAP: f32 = 24.0;

/// Offset to apply to `mobile` so it no longer overlaps `fixed` (`gap` included):
/// along the axis needing the least movement, away from the center of `fixed`.
pub fn separation(fixed: Rect, mobile: Rect, gap: f32) -> Vec2 {
    let f = fixed.expand(gap / 2.0);
    let m = mobile.expand(gap / 2.0);
    if !f.intersects(m) || f.max.x <= m.min.x || m.max.x <= f.min.x || f.max.y <= m.min.y || m.max.y <= f.min.y {
        return Vec2::ZERO;
    }
    let right = f.max.x - m.min.x;
    let left = m.max.x - f.min.x;
    let down = f.max.y - m.min.y;
    let up = m.max.y - f.min.y;
    let dx = if mobile.center().x >= fixed.center().x { right } else { -left };
    let dy = if mobile.center().y >= fixed.center().y { down } else { -up };
    if dx.abs() <= dy.abs() { Vec2::new(dx, 0.0) } else { Vec2::new(0.0, dy) }
}

/// Margin between the terminals and their group's frame.
pub const PROJECT_PAD: f32 = 40.0;
/// Size of a group with no terminal.
pub const EMPTY_PROJECT: Vec2 = Vec2::new(600.0, 400.0);

/// Smallest frame containing the point (drop target, migration of old layouts).
pub fn project_at(projects: &[Project], p: Pos2) -> Option<&Project> {
    projects.iter().filter(|pr| pr.rect.contains(p)).min_by(|a, b| a.rect.area().total_cmp(&b.rect.area()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn card_at(x: f32, y: f32) -> Card {
        Card::new(Rect::from_min_size(pos2(x, y), Vec2::new(100.0, 60.0)), None)
    }

    fn linked_names(w: &World, id: &str) -> Vec<String> {
        w.linked_groups(id).into_iter().map(|p| p.name.clone()).collect()
    }

    #[test]
    fn linked_groups_follow_the_whole_chain_sorted_by_name() {
        let mut w = World::default();
        for (id, name) in [("s", "shop"), ("w", "Wordpress"), ("o", "outils"), ("x", "alone")] {
            w.projects.push(Project { id: id.into(), name: name.into(), color: [0; 3], rect: Rect::ZERO, cwd: None });
        }
        let wire = |a: &str, b: &str| Wire { from: a.into(), to: b.into() };
        w.wires = vec![wire("s", "w"), wire("w", "o"), wire("o", "gone")];
        assert_eq!(linked_names(&w, "s"), ["outils", "Wordpress"], "through wordpress, case-insensitive order");
        assert_eq!(linked_names(&w, "o"), ["shop", "Wordpress"]);
        assert!(linked_names(&w, "x").is_empty(), "isolated group");
        // A cycle does not loop or repeat a group.
        w.wires.push(wire("o", "s"));
        assert_eq!(linked_names(&w, "w"), ["outils", "shop"]);
    }

    #[test]
    fn titles_fall_back_from_name_to_command_to_cwd() {
        let mut c = card_at(0.0, 0.0);
        assert_eq!(c.title(), "terminal");
        c.cwd = Some("/home/me/project/".into());
        assert_eq!(c.title(), "project");
        c.info = Some(SessionInfo {
            session: c.session(),
            status: TermStatus::Running,
            command: Some("/usr/bin/python3 -m http.server".into()),
            cwd: Some("/srv".into()),
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
            agent_session_id: None,
            agent_transcript: None,
            last_prompt: None,
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

    fn project(id: &str, rect: Rect) -> Project {
        Project { id: id.into(), name: id.into(), color: [0; 3], rect, cwd: None }
    }

    fn member(x: f32, y: f32, project: &str) -> Card {
        let mut c = card_at(x, y);
        c.project = Some(project.into());
        c
    }

    #[test]
    fn smallest_containing_project_wins() {
        let projects = [project("big", Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0))), project("small", Rect::from_min_size(pos2(100.0, 100.0), Vec2::splat(300.0)))];
        let at = |x, y| project_at(&projects, pos2(x, y)).map(|p| p.id.as_str());
        assert_eq!((at(150.0, 150.0), at(700.0, 700.0), at(5000.0, 0.0)), (Some("small"), Some("big"), None));
    }

    #[test]
    fn membership_decides_the_group_not_geometry() {
        let mut w = World { projects: vec![project("api", Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)))], ..Default::default() };
        w.add_card(member(5000.0, 0.0, "api"));
        w.add_card(card_at(100.0, 100.0));
        let groups: Vec<Option<&str>> = w.cards.iter().map(|c| w.project_of(c).map(|p| p.id.as_str())).collect();
        assert_eq!(groups, vec![Some("api"), None]);
        assert_eq!(w.members("api").count(), 1);
    }

    #[test]
    fn a_project_space_shows_only_its_members() {
        let mut w = World {
            projects: vec![project("api", Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0))), project("web", Rect::from_min_size(pos2(5000.0, 0.0), Vec2::splat(1000.0)))],
            ..Default::default()
        };
        w.add_card(member(150.0, 150.0, "api"));
        w.add_card(member(5100.0, 100.0, "web"));
        w.add_card(card_at(-9000.0, 0.0));
        let shown = |w: &World| (w.cards.iter().map(|c| w.shows_card(c)).collect::<Vec<_>>(), w.projects.iter().map(|p| w.shows_project(p)).collect::<Vec<_>>());
        assert_eq!(shown(&w), (vec![true; 3], vec![true; 2]), "'All' shows everything");
        w.space = Some("api".into());
        assert_eq!(shown(&w), (vec![true, false, false], vec![true, false]));
        w.space = Some("web".into());
        assert_eq!(shown(&w), (vec![false, true, false], vec![false, true]));
        w.space = Some("gone".into());
        assert_eq!(shown(&w).0, vec![false; 3]);
    }

    #[test]
    fn refit_hugs_members_growing_and_shrinking() {
        let mut w = World { projects: vec![project("api", Rect::from_min_size(Pos2::ZERO, Vec2::splat(5000.0)))], ..Default::default() };
        w.add_card(member(100.0, 100.0, "api"));
        w.add_card(member(400.0, 100.0, "api"));
        w.refit_projects();
        let expected = Rect::from_min_max(pos2(100.0, 100.0), pos2(500.0, 160.0)).expand(PROJECT_PAD);
        assert_eq!(w.projects[0].rect, expected, "shrinks around the members");
        w.cards[1].rect = w.cards[1].rect.translate(vec2(1000.0, 0.0));
        w.refit_projects();
        assert_eq!(w.projects[0].rect.max.x, 1500.0 + PROJECT_PAD, "grows with them");
        let rev = w.revision;
        w.refit_projects();
        assert_eq!(w.revision, rev, "stable: nothing to save");
    }

    #[test]
    fn free_grid_keeps_dragged_cards_in_the_open_group() {
        let mut w = World {
            projects: vec![project("api", Rect::from_center_size(pos2(50.0, 50.0), Vec2::splat(10.0))), project("web", Rect::from_min_size(pos2(5000.0, 0.0), Vec2::splat(400.0)))],
            ..Default::default()
        };
        let (a, b) = (member(0.0, 0.0, "api"), member(200.0, 0.0, "api"));
        let ib = b.id.clone();
        w.add_card(a);
        w.add_card(b);
        w.refit_projects();
        let before = w.projects[0].rect;
        let far = pos2(-3000.0, 900.0);
        assert_eq!(w.drop_target(far), None, "option off: outside any frame");
        w.free_grid = true;
        assert_eq!(w.drop_target(far), None, "option on but in 'All': unchanged");
        w.space = Some("api".into());
        assert!(w.free_grid_active());
        assert_eq!(w.drop_target(far).as_deref(), Some("api"), "anywhere: the open group");
        w.dragging = Some(ib.clone());
        w.card_mut(&ib).unwrap().rect = w.card(&ib).unwrap().rect.translate(vec2(900.0, 0.0));
        w.refit_projects();
        assert!(w.projects[0].rect.width() > before.width(), "the frame follows the dragged card");
        w.free_grid = false;
        assert!(!w.free_grid_active());
    }

    #[test]
    fn refit_freezes_the_group_of_the_dragged_card_and_keeps_empty_groups_visible() {
        let mut w = World { projects: vec![project("api", Rect::from_center_size(pos2(50.0, 50.0), Vec2::splat(10.0)))], ..Default::default() };
        let (a, b) = (member(0.0, 0.0, "api"), member(200.0, 0.0, "api"));
        let ib = b.id.clone();
        w.add_card(a);
        w.add_card(b);
        w.refit_projects();
        let before = w.projects[0].rect;
        w.dragging = Some(ib.clone());
        for _ in 0..20 {
            w.card_mut(&ib).unwrap().rect = w.card(&ib).unwrap().rect.translate(vec2(300.0, 0.0));
            w.refit_projects();
        }
        assert_eq!(w.projects[0].rect, before, "the frame doesn't follow the dragged card");
        w.dragging = None;
        w.refit_projects();
        assert!(w.projects[0].rect.width() > before.width(), "dropped: the frame contains it");
        w.cards.clear();
        w.refit_projects();
        assert_eq!(w.projects[0].rect.size(), EMPTY_PROJECT);
    }

    #[test]
    fn a_dropped_card_keeps_its_place_and_pushes_its_neighbours_aside() {
        let mut w = World::default();
        // a and b side by side; c dropped right on top of a.
        let (a, b, c) = (member(0.0, 0.0, "api"), member(130.0, 0.0, "api"), member(30.0, 20.0, "api"));
        let outsider = card_at(40.0, 10.0);
        let (ia, ic, io) = (a.id.clone(), c.id.clone(), outsider.id.clone());
        for card in [a, b, c, outsider] {
            w.add_card(card);
        }
        let dropped = w.card(&ic).unwrap().rect;
        assert!(w.settle(&ic));
        assert_eq!(w.card(&ic).unwrap().rect, dropped, "the dropped card keeps its spot");
        let rects: Vec<Rect> = w.members("api").map(|c| c.rect).collect();
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                assert!(!rects[i].expand(CARD_GAP / 2.0 - 0.01).intersects(rects[j].expand(CARD_GAP / 2.0 - 0.01)), "overlap {i}/{j}: {rects:?}");
            }
        }
        assert_eq!(w.card(&io).unwrap().rect.min, pos2(40.0, 10.0), "outside the group: untouched");
        assert!(w.card(&ia).unwrap().rect.min != pos2(0.0, 0.0), "a moved aside");
        let rev = w.revision;
        assert!(!w.settle(&ic), "already settled: nothing moves");
        assert_eq!(w.revision, rev);
    }

    /// No group (label included) or free terminal overlaps another.
    fn assert_no_collision(w: &World) {
        let bodies = w.bodies();
        for i in 0..bodies.len() {
            for j in i + 1..bodies.len() {
                let (a, b) = (w.body_rect(&bodies[i]).unwrap(), w.body_rect(&bodies[j]).unwrap());
                assert!(!a.intersects(b), "{:?} {a:?} overlaps {:?} {b:?}", bodies[i], bodies[j]);
            }
        }
    }

    #[test]
    fn a_growing_group_pushes_the_other_groups_with_their_terminals() {
        let mut w = World { projects: vec![project("api", Rect::NOTHING), project("web", Rect::NOTHING), project("db", Rect::NOTHING)], ..Default::default() };
        w.add_card(member(0.0, 0.0, "api"));
        let web_card = member(400.0, 0.0, "web");
        let iw = web_card.id.clone();
        w.add_card(web_card);
        w.add_card(member(800.0, 0.0, "db"));
        w.refit_projects();
        w.separate_all();
        assert_no_collision(&w);
        let web_before = w.card(&iw).unwrap().rect;
        // api gets a big terminal: its frame grows toward web, which pushes db.
        let mut big = member(0.0, 0.0, "api");
        big.rect = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(900.0, 200.0));
        w.add_card(big);
        w.refit_projects();
        assert_no_collision(&w);
        assert!(w.card(&iw).unwrap().rect.min.x > web_before.min.x || w.card(&iw).unwrap().rect.min.y != web_before.min.y, "web and its terminal moved aside together");
        assert!(w.projects[1].rect.contains_rect(w.card(&iw).unwrap().rect), "the terminal stays in its frame");
    }

    #[test]
    fn a_free_terminal_never_overlaps_a_group_or_another_free_terminal() {
        let mut w = World { projects: vec![project("api", Rect::NOTHING)], ..Default::default() };
        w.add_card(member(0.0, 0.0, "api"));
        w.refit_projects();
        let api = w.projects[0].rect;
        // Free terminal dropped across the frame's edge: it moves aside, not the group.
        let free = card_at(api.max.x - 30.0, 0.0);
        let other = card_at(api.max.x + 60.0, 10.0);
        let (ifree, iother) = (free.id.clone(), other.id.clone());
        w.add_card(free);
        w.add_card(other);
        assert!(w.separate(&Body::Card(ifree.clone())));
        assert_eq!(w.projects[0].rect, api, "the group didn't move");
        assert_no_collision(&w);
        assert!(w.card(&iother).is_some());
    }

    /// Pseudo-random (deterministic) scenarios: groups that receive terminals and
    /// grow, free terminals dropped anywhere. Never any overlap at the end.
    #[test]
    fn nothing_ever_overlaps_under_random_growth() {
        let mut seed: u64 = 0x5eed;
        let mut rnd = move |max: f32| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f32 / (1u64 << 31) as f32) * max
        };
        for _ in 0..200 {
            let mut w = World::default();
            let groups = 1 + rnd(4.0) as usize;
            for g in 0..groups {
                w.projects.push(project(&format!("g{g}"), Rect::NOTHING));
                w.add_card(member(rnd(3000.0), rnd(2000.0), &format!("g{g}")));
            }
            w.refit_projects();
            w.separate_all();
            for _ in 0..8 {
                if rnd(1.0) < 0.5 {
                    let g = format!("g{}", rnd(groups as f32) as usize % groups);
                    let mut c = member(rnd(3000.0), rnd(2000.0), &g);
                    c.rect = Rect::from_min_size(c.rect.min, Vec2::new(200.0 + rnd(700.0), 100.0 + rnd(400.0)));
                    let id = c.id.clone();
                    w.add_card(c);
                    w.settle(&id);
                    w.refit_projects();
                } else {
                    let c = card_at(rnd(3000.0), rnd(2000.0));
                    let id = c.id.clone();
                    w.add_card(c);
                    w.separate(&Body::Card(id));
                }
                assert_no_collision(&w);
            }
        }
    }

    #[test]
    fn separation_moves_along_the_shortest_axis_away_from_the_fixed_card() {
        let fixed = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 60.0));
        assert_eq!(separation(fixed, Rect::from_min_size(pos2(90.0, 0.0), Vec2::new(100.0, 60.0)), 0.0), vec2(10.0, 0.0));
        assert_eq!(separation(fixed, Rect::from_min_size(pos2(0.0, -50.0), Vec2::new(100.0, 60.0)), 0.0), vec2(0.0, -10.0));
        assert_eq!(separation(fixed, Rect::from_min_size(pos2(105.0, 0.0), Vec2::new(100.0, 60.0)), 10.0), vec2(5.0, 0.0), "the gap counts");
        assert_eq!(separation(fixed, Rect::from_min_size(pos2(300.0, 0.0), Vec2::new(100.0, 60.0)), 10.0), Vec2::ZERO);
    }

    #[test]
    fn removing_a_card_drops_its_focus() {
        let mut w = World::default();
        let (a, b) = (card_at(0.0, 0.0), card_at(500.0, 0.0));
        let ia = a.id.clone();
        w.add_card(a);
        w.add_card(b);
        w.focused = Some(ia.clone());
        w.remove_card(&ia);
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
    fn a_new_group_dropped_on_another_moves_away_without_pushing_it() {
        let mut w = World { projects: vec![project("old", Rect::from_center_size(Pos2::ZERO, EMPTY_PROJECT))], ..Default::default() };
        w.add_card(member(-100.0, -50.0, "old"));
        w.refit_projects();
        let before = w.projects[0].rect;
        w.projects.push(project("new", Rect::from_center_size(pos2(50.0, 30.0), EMPTY_PROJECT)));
        assert!(w.place_clear(&Body::Group("new".into())));
        let (old, new) = (w.body_rect(&Body::Group("old".into())).unwrap(), w.body_rect(&Body::Group("new".into())).unwrap());
        assert_eq!(w.projects[0].rect, before, "the existing one doesn't move");
        assert!(!old.expand(BODY_GAP / 2.0 - 0.01).intersects(new.expand(BODY_GAP / 2.0 - 0.01)), "{old:?} / {new:?}");
        assert!(!w.place_clear(&Body::Group("new".into())), "already clear: no longer moves");
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
