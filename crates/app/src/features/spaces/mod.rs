//! Spaces: each project is a separate canvas, only one is visible at a time. "All" remains
//! available as an overview for organizing. Each space remembers its own camera.

use egui::{Button, Color32, Pos2, Rect, RichText, Ui, Vec2};

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::{Camera, ProjectId, World};

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
        for (n, p) in world.projects.iter().enumerate() {
            let on = world.space.as_ref() == Some(&p.id);
            // A bell flags a terminal asking for attention in another space.
            let calling = !on && world.members(&p.id).any(|c| c.attention);
            // A pencil flags files just modified in another group's folder.
            let writing = !on && world.file_activity.get(&p.id).is_some_and(|&t| now.saturating_sub(t) < WRITING_SECS);
            let label = format!("● {}{}{}", p.name, if writing { " ✎" } else { "" }, if calling { " 🔔" } else { "" });
            let text = RichText::new(label).color(if on { theme::CARD_BG } else { theme::rgb(p.color) });
            let chip = Button::selectable(on, text).fill(if on { theme::rgb(p.color) } else { Color32::TRANSPARENT });
            let hint = if n < 9 { (t().open_project_shortcut)(n + 1) } else { t().open_project.into() };
            if ui.add(chip).on_hover_text(hint).clicked() {
                events.push(Event::EnterSpace(Some(p.id.clone())));
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
