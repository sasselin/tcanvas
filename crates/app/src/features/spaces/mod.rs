//! Spaces: each project is a separate canvas, only one is visible at a time. "All" remains
//! available as an overview for organizing. Each space remembers its own camera. The group
//! chips of the top bar can be dragged to reorder them (the order is saved).

use egui::{Button, Color32, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::{Camera, Project, ProjectId, World};

/// Files modified more recently than this: the group is "writing".
const WRITING_SECS: u64 = 30;
/// Fallback view until the canvas has been measured (first frame).
const FALLBACK_VIEWPORT: Vec2 = Vec2::new(1280.0, 800.0);

#[derive(Default)]
pub struct Spaces {
    viewport: Option<Rect>,
}

fn camera_key(space: Option<&str>) -> String {
    space.unwrap_or_default().to_string()
}

/// Everything the current space shows: its cards and frames.
pub fn space_bounds(world: &World) -> Option<Rect> {
    let cards = world.cards.iter().filter(|c| world.shows_card(c)).map(|c| c.rect);
    let projects = world.projects.iter().filter(|p| world.shows_project(p)).map(|p| p.rect);
    cards.chain(projects).reduce(|a, b| a.union(b))
}

/// Insertion index for a chip dropped at `pointer_x`, given the chips' horizontal centers
/// (left to right): the number of chips whose center is left of the pointer.
pub fn drop_index(centers_x: &[f32], pointer_x: f32) -> usize {
    centers_x.iter().filter(|&&c| c < pointer_x).count()
}

/// Moves the project `id` so that it lands at insertion index `to` (an index in the list
/// before the move, as given by `drop_index`). Returns `false` if nothing changed.
pub fn move_project(projects: &mut Vec<Project>, id: &str, to: usize) -> bool {
    let Some(from) = projects.iter().position(|p| p.id == id) else {
        return false;
    };
    let to = if to > from { to - 1 } else { to }.min(projects.len() - 1);
    if to == from {
        return false;
    }
    let p = projects.remove(from);
    projects.insert(to, p);
    true
}

/// Switches to `target`. A project always opens framed on all its content (centered, ready,
/// in a single click); "All" restores the overview left last time. The focused terminal
/// loses the keyboard if it is not in the new space. Returns `false` if `target`
/// does not exist.
pub fn enter(world: &mut World, target: Option<ProjectId>, vp: Rect) -> bool {
    if target.as_ref().is_some_and(|id| world.project(id).is_none()) {
        return false;
    }
    world.space_cameras.insert(camera_key(world.space.as_deref()), world.camera);
    world.space = target;
    let fitted = || space_bounds(world).map(|r| Camera::fitting(r, vp, 60.0, 1.0)).unwrap_or(world.camera);
    world.camera = match (&world.space, world.space_cameras.get(&camera_key(world.space.as_deref()))) {
        (None, Some(cam)) => *cam,
        _ => fitted(),
    };
    let focused_hidden = world.focused.as_deref().and_then(|id| world.card(id)).is_some_and(|c| !world.shows_card(c));
    if focused_hidden {
        world.focused = None;
    }
    world.touch();
    true
}

impl Spaces {
    fn viewport(&self) -> Rect {
        self.viewport.filter(|r| r.is_positive()).unwrap_or(Rect::from_min_size(Pos2::ZERO, FALLBACK_VIEWPORT))
    }

    /// Top bar chips: "All", then one project per chip. Only one is active.
    pub fn ui(&mut self, ui: &mut Ui, world: &World, vp: Rect, events: &mut Vec<Event>) {
        self.viewport = Some(vp);
        if world.projects.is_empty() && world.space.is_none() {
            return;
        }
        let all = world.space.is_none();
        let now = crate::shell::world::unix_now();
        if world.file_activity.values().any(|&t| now.saturating_sub(t) < WRITING_SECS) {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(2));
        }
        let chip = Button::selectable(all, RichText::new(t().all_spaces).color(if all { theme::CARD_BG } else { theme::TEXT })).fill(if all { theme::TEXT } else { Color32::TRANSPARENT });
        if ui.add(chip).on_hover_text(t().all_spaces_hint).clicked() {
            events.push(Event::EnterSpace(None));
        }
        // Group chips: click opens the space, drag (or hold then move) reorders them.
        let mut chips = Vec::with_capacity(world.projects.len());
        let mut dragged = None;
        for (n, p) in world.projects.iter().enumerate() {
            let on = world.space.as_ref() == Some(&p.id);
            // A bell flags a terminal asking for attention in another space.
            let calling = !on && world.members(&p.id).any(|c| c.attention);
            // A pencil flags files just modified in another group's folder.
            let writing = !on && world.file_activity.get(&p.id).is_some_and(|&t| now.saturating_sub(t) < WRITING_SECS);
            let label = format!("● {}{}{}", p.name, if writing { " ✎" } else { "" }, if calling { " 🔔" } else { "" });
            let text = RichText::new(label).color(if on { theme::CARD_BG } else { theme::rgb(p.color) });
            let chip = Button::selectable(on, text).fill(if on { theme::rgb(p.color) } else { Color32::TRANSPARENT }).sense(Sense::click_and_drag());
            let hint = if n < 9 { (t().open_project_shortcut)(n + 1) } else { t().open_project.into() };
            // Id from the project, not the label: the drag survives the ✎ / 🔔 marks changing.
            let resp = ui.push_id(("space-chip", &p.id), |ui| ui.add(chip)).inner;
            if resp.dragged() || resp.drag_stopped() {
                dragged = Some((n, resp.drag_stopped()));
            }
            let resp = if resp.dragged() { resp } else { resp.on_hover_text(hint) };
            if resp.clicked() {
                events.push(Event::EnterSpace(Some(p.id.clone())));
            }
            chips.push(resp.rect);
        }
        if let Some((n, released)) = dragged {
            let pointer = ui.ctx().pointer_latest_pos();
            let centers: Vec<f32> = chips.iter().map(|r| r.center().x).collect();
            let at = pointer.map(|p| drop_index(&centers, p.x));
            let id = &world.projects[n].id;
            if released {
                if let Some(at) = at {
                    events.push(Event::MoveProject(id.clone(), at));
                }
            } else {
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                let painter = ui.painter();
                painter.rect_filled(chips[n], 4.0, theme::rgb(world.projects[n].color).gamma_multiply(0.25));
                if let Some(at) = at.filter(|&at| at != n && at != n + 1) {
                    let x = match chips.get(at) {
                        Some(r) => r.min.x - ui.spacing().item_spacing.x / 2.0,
                        None => chips[chips.len() - 1].max.x + ui.spacing().item_spacing.x / 2.0,
                    };
                    let r = chips[n];
                    painter.vline(x, r.y_range(), Stroke::new(2.0, theme::rgb(world.projects[n].color)));
                }
            }
        }
        // Inside a group only: the "free grid" option (the group is the whole canvas).
        if !all {
            ui.separator();
            let on = world.free_grid;
            let chip = Button::selectable(on, RichText::new(t().free_grid).color(if on { theme::CARD_BG } else { theme::TEXT })).fill(if on { theme::TEXT } else { Color32::TRANSPARENT });
            if ui.add(chip).on_hover_text(t().free_grid_hint).clicked() {
                events.push(Event::ToggleFreeGrid);
            }
        }
        ui.separator();
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, out: &mut Vec<Event>) {
        let vp = self.viewport();
        match ev {
            Event::EnterSpace(target) => {
                if &world.space == target {
                    // Requesting the current space again reframes it.
                    if let Some(r) = space_bounds(world) {
                        out.push(Event::FitRect(r));
                    }
                } else {
                    enter(world, target.clone(), vp);
                }
            }
            // Going to a hidden terminal: open its space first (before the camera flight).
            Event::FlyTo(id) => {
                if let Some(card) = world.card(id).filter(|c| !world.shows_card(c)) {
                    let target = world.project_of(card).map(|p| p.id.clone());
                    enter(world, target, vp);
                }
            }
            Event::MoveProject(id, to) => {
                if move_project(&mut world.projects, id, *to) {
                    world.touch();
                }
            }
            Event::ToggleFreeGrid => {
                world.free_grid = !world.free_grid;
                world.touch();
            }
            Event::RemoveProject(id) => {
                world.space_cameras.remove(id.as_str());
                if world.space.as_ref() == Some(id) {
                    world.space = None;
                    if let Some(cam) = world.space_cameras.get("") {
                        world.camera = *cam;
                    }
                    world.touch();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::world::{Card, Project};
    use egui::pos2;

    fn vp() -> Rect {
        Rect::from_min_size(pos2(0.0, 30.0), Vec2::new(1200.0, 800.0))
    }

    fn world() -> World {
        let mut w = World::default();
        for (id, x) in [("api", 0.0), ("web", 10_000.0)] {
            w.projects.push(Project { id: id.into(), name: id.into(), color: [0; 3], rect: Rect::from_min_size(pos2(x, 0.0), Vec2::new(2000.0, 1200.0)), cwd: None });
            let mut c = Card::with_id(format!("{id}-1"), Rect::from_min_size(pos2(x + 100.0, 100.0), Vec2::new(500.0, 300.0)), None);
            c.name = Some(id.into());
            c.project = Some(id.into());
            w.add_card(c);
        }
        w.add_card(Card::with_id("alone".into(), Rect::from_min_size(pos2(-9000.0, 0.0), Vec2::new(500.0, 300.0)), None));
        w
    }

    fn handle(s: &mut Spaces, w: &mut World, ev: Event) -> Vec<Event> {
        let mut out = vec![];
        s.handle(&ev, w, &mut out);
        out
    }

    #[test]
    fn entering_a_project_frames_it_the_first_time() {
        let mut w = world();
        let mut s = Spaces { viewport: Some(vp()) };
        handle(&mut s, &mut w, Event::EnterSpace(Some("web".into())));
        assert_eq!(w.space.as_deref(), Some("web"));
        let on_screen = w.camera.rect_to_screen(w.projects[1].rect, vp());
        assert!(vp().contains_rect(on_screen.shrink(1.0)), "{on_screen:?}");
    }

    #[test]
    fn a_project_always_opens_framed_and_the_overview_keeps_its_view() {
        let mut w = world();
        let mut s = Spaces { viewport: Some(vp()) };
        let overview = Camera { center: pos2(123.0, 45.0), zoom: 0.3 };
        w.camera = overview;
        handle(&mut s, &mut w, Event::EnterSpace(Some("api".into())));
        let framed = w.camera;
        // Wander off in api, go through web, then come back: api is framed again.
        w.camera = Camera { center: pos2(-5000.0, 9000.0), zoom: 2.0 };
        handle(&mut s, &mut w, Event::EnterSpace(Some("web".into())));
        assert!(vp().contains_rect(w.camera.rect_to_screen(w.projects[1].rect, vp()).shrink(1.0)), "web framed on the first click");
        handle(&mut s, &mut w, Event::EnterSpace(Some("api".into())));
        assert_eq!(w.camera, framed);
        handle(&mut s, &mut w, Event::EnterSpace(None));
        assert_eq!(w.camera, overview, "'All' restores the overview");
    }

    #[test]
    fn re_entering_the_current_space_reframes_it() {
        let mut w = world();
        let mut s = Spaces { viewport: Some(vp()) };
        handle(&mut s, &mut w, Event::EnterSpace(Some("api".into())));
        let out = handle(&mut s, &mut w, Event::EnterSpace(Some("api".into())));
        assert!(matches!(out.as_slice(), [Event::FitRect(r)] if r.contains_rect(w.projects[0].rect)));
    }

    #[test]
    fn free_grid_toggles_and_is_saved() {
        let mut w = world();
        let mut s = Spaces::default();
        let rev = w.revision;
        handle(&mut s, &mut w, Event::ToggleFreeGrid);
        assert!(w.free_grid && w.revision > rev);
        handle(&mut s, &mut w, Event::ToggleFreeGrid);
        assert!(!w.free_grid);
    }

    fn order(w: &World) -> Vec<&str> {
        w.projects.iter().map(|p| p.id.as_str()).collect()
    }

    #[test]
    fn drop_index_counts_the_chips_left_of_the_pointer() {
        let centers = [10.0, 50.0, 90.0];
        assert_eq!(drop_index(&centers, 0.0), 0, "before the first");
        assert_eq!(drop_index(&centers, 30.0), 1, "between the first two");
        assert_eq!(drop_index(&centers, 70.0), 2);
        assert_eq!(drop_index(&centers, 200.0), 3, "after the last");
        assert_eq!(drop_index(&[], 5.0), 0);
    }

    #[test]
    fn moving_a_project_reorders_the_chips_and_is_saved() {
        let mut w = world();
        w.projects.push(Project { id: "db".into(), name: "db".into(), color: [0; 3], rect: Rect::ZERO, cwd: None });
        let mut s = Spaces::default();
        let rev = w.revision;
        handle(&mut s, &mut w, Event::MoveProject("api".into(), 3));
        assert_eq!(order(&w), ["web", "db", "api"], "to the right: after the last");
        assert!(w.revision > rev);
        handle(&mut s, &mut w, Event::MoveProject("api".into(), 0));
        assert_eq!(order(&w), ["api", "web", "db"], "to the left: first");
        handle(&mut s, &mut w, Event::MoveProject("db".into(), 1));
        assert_eq!(order(&w), ["api", "db", "web"], "between two");
    }

    #[test]
    fn dropping_a_project_in_place_or_unknown_changes_nothing() {
        let mut w = world();
        let mut s = Spaces::default();
        let rev = w.revision;
        for (id, to) in [("api", 0), ("api", 1), ("web", 1), ("web", 2), ("absent", 0)] {
            handle(&mut s, &mut w, Event::MoveProject(id.into(), to));
            assert_eq!(order(&w), ["api", "web"], "{id} -> {to}");
        }
        assert_eq!(w.revision, rev, "nothing to save");
    }

    #[test]
    fn unknown_project_changes_nothing() {
        let mut w = world();
        let mut s = Spaces::default();
        let cam = w.camera;
        handle(&mut s, &mut w, Event::EnterSpace(Some("absent".into())));
        assert_eq!((w.space.as_deref(), w.camera), (None, cam));
    }

    #[test]
    fn leaving_a_space_drops_focus_on_its_terminals() {
        let mut w = world();
        let mut s = Spaces::default();
        w.focused = Some("api-1".into());
        handle(&mut s, &mut w, Event::EnterSpace(Some("api".into())));
        assert_eq!(w.focused.as_deref(), Some("api-1"), "still visible: keeps the keyboard");
        handle(&mut s, &mut w, Event::EnterSpace(Some("web".into())));
        assert_eq!(w.focused, None);
    }

    #[test]
    fn flying_to_a_hidden_terminal_opens_its_space() {
        let mut w = world();
        let mut s = Spaces::default();
        handle(&mut s, &mut w, Event::EnterSpace(Some("api".into())));
        handle(&mut s, &mut w, Event::FlyTo("web-1".into()));
        assert_eq!(w.space.as_deref(), Some("web"));
        handle(&mut s, &mut w, Event::FlyTo("alone".into()));
        assert_eq!(w.space, None, "a terminal outside any project only exists in 'All'");
        handle(&mut s, &mut w, Event::FlyTo("api-1".into()));
        assert_eq!(w.space, None, "already visible: stay");
    }

    #[test]
    fn removing_the_active_project_returns_to_the_overview() {
        let mut w = world();
        let mut s = Spaces::default();
        let overview = Camera { center: pos2(1.0, 2.0), zoom: 0.5 };
        w.camera = overview;
        handle(&mut s, &mut w, Event::EnterSpace(Some("api".into())));
        w.projects.retain(|p| p.id != "api");
        handle(&mut s, &mut w, Event::RemoveProject("api".into()));
        assert_eq!((w.space.as_deref(), w.camera), (None, overview));
        assert!(!w.space_cameras.contains_key("api"));
    }
}
