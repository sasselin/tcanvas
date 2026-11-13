//! Infinite canvas: background, panning, zoom, camera flights and minimap.

mod camera;
mod minimap;

pub use camera::{FIT_MARGIN, FIT_MAX_ZOOM, MAX_ZOOM, MIN_ZOOM};

use egui::{Color32, CornerRadius, CursorIcon, Id, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::shell::event::Event;
use crate::shell::theme;
use crate::shell::world::{Camera, CardId, World, project_at};
use camera::{ease_in_out, leading_edge_moved};
use minimap::MiniMap;

const FLIGHT_SECS: f64 = 0.45;
const GRID_STEP: f32 = 40.0;
const MINIMAP_SIZE: Vec2 = Vec2::new(220.0, 150.0);

struct Flight {
    from: Camera,
    to: Camera,
    start: Option<f64>,
    focus: Option<CardId>,
    /// The world area this flight frames (`FitRect`, `RevealRect`): the view is framed on it
    /// once landed.
    frames: Option<Rect>,
}

/// The view is the app's framing of `target` (a space just opened, a "show all", a group or
/// matches flown to), and was left as `camera` since.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Framed {
    target: Rect,
    camera: Camera,
}

pub struct Canvas {
    viewport: Rect,
    flight: Option<Flight>,
    /// Empty space is being dragged (left or middle button).
    bg_panning: bool,
    /// A hand drag (Shift) is in progress: it continues even if Shift is released.
    hand_dragging: bool,
    /// Set while the view is still the app's framing; cleared by anything the user does to the
    /// view (pan, zoom, minimap) and by flights that frame nothing (to a card, back to a view).
    framed: Option<Framed>,
    /// The space shown at the last event: a new one opens framed on its content.
    space: Option<Option<String>>,
}

impl Default for Canvas {
    fn default() -> Self {
        Self { viewport: Rect::NOTHING, flight: None, bg_panning: false, hand_dragging: false, framed: None, space: None }
    }
}

impl Canvas {
    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    /// Background and interactions on empty space. Call before any other canvas element.
    pub fn background(&mut self, ui: &mut Ui, world: &mut World, events: &mut Vec<Event>) {
        let vp = ui.max_rect();
        self.viewport_changed(world, vp);
        let painter = ui.painter_at(vp);
        painter.rect_filled(vp, CornerRadius::ZERO, theme::CANVAS_BG);
        paint_grid(&painter, &world.camera, vp);

        let bg = ui.interact(vp, Id::unique("canvas-background"), Sense::click_and_drag());
        self.bg_panning = bg.dragged_by(PointerButton::Primary) || bg.dragged_by(PointerButton::Middle);
        if self.bg_panning {
            world.camera.pan_screen(bg.drag_delta());
            self.user_moved();
            world.touch();
        }
        if bg.double_clicked() {
            if let Some(p) = bg.interact_pointer_pos() {
                let at = world.camera.to_world(p, vp);
                let project = project_at(&world.projects, at).filter(|p| world.shows_project(p)).map(|p| p.id.clone()).or(world.space.clone());
                events.push(Event::NewCard { at: Some(at), project, command: None, setup: None, cwd: None });
            }
        } else if bg.clicked() {
            events.push(Event::Unfocus);
        }
    }

    /// Hand tool: holding Shift shows the open hand, and Shift + drag pans the view
    /// anywhere, even over terminals (closed hand while dragging).
    /// Call after the cards: the last registered area gets the pointer.
    pub fn hand_tool(&mut self, ui: &mut Ui, world: &mut World) {
        let vp = self.viewport;
        let shift = ui.input(|i| i.modifiers.shift);
        let over = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| vp.contains(p));
        let mut grabbing = self.bg_panning;
        if shift || self.hand_dragging {
            let hand = ui.interact(vp, Id::unique("canvas-hand"), Sense::drag());
            self.hand_dragging = hand.dragged();
            if self.hand_dragging {
                world.camera.pan_screen(hand.drag_delta());
                self.user_moved();
                world.touch();
                grabbing = true;
            }
        }
        if grabbing {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else if shift && over {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
    }

    /// Wheel: pans the view; Ctrl + wheel (or pinch): zooms around the cursor.
    /// Nothing if the pointer is over something other than the canvas: panel, menu, list, window,
    /// toast… (they scroll their own content) or a terminal (`wheel_taken`).
    pub fn navigate(&mut self, ui: &mut Ui, world: &mut World, wheel_taken: bool) {
        let vp = self.viewport;
        let Some(pointer) = ui.input(|i| i.pointer.hover_pos()).filter(|p| vp.contains(*p)) else {
            return;
        };
        if ui.ctx().layer_id_at(pointer).is_some_and(|layer| layer != ui.layer_id()) {
            return;
        }
        let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        if zoom != 1.0 {
            world.camera.zoom_around(pointer, zoom, vp);
            self.user_moved();
            world.touch();
        } else if scroll != Vec2::ZERO && !wheel_taken {
            world.camera.pan_screen(scroll);
            self.user_moved();
            world.touch();
        }
    }

    pub fn animate(&mut self, ui: &Ui, world: &mut World, events: &mut Vec<Event>) {
        let Some(flight) = &mut self.flight else { return };
        let now = ui.input(|i| i.time);
        let start = *flight.start.get_or_insert(now);
        let t = ((now - start) / FLIGHT_SECS) as f32;
        world.camera = Camera::lerp(flight.from, flight.to, ease_in_out(t));
        world.touch();
        if t >= 1.0 {
            if let Some(id) = flight.focus.take() {
                events.push(Event::FocusCard(id));
            }
            self.framed = flight.frames.map(|target| Framed { target, camera: world.camera });
            self.flight = None;
        } else {
            ui.ctx().request_repaint();
        }
    }

    pub fn minimap(&mut self, ui: &mut Ui, world: &mut World) {
        if world.cards.is_empty() && world.projects.is_empty() {
            return;
        }
        let vp = self.viewport;
        let screen = Rect::from_min_size(vp.max - MINIMAP_SIZE - Vec2::splat(12.0), MINIMAP_SIZE);
        let view = world.camera.visible_world(vp);
        let bounds = crate::features::spaces::space_bounds(world).map_or(view, |r| r.union(view)).expand(200.0);
        let map = MiniMap::new(screen.shrink(6.0), bounds);

        let resp = ui.interact(screen, Id::unique("minimap"), Sense::click_and_drag());
        if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.clicked() || resp.dragged()) {
            world.camera.center = map.to_world(p);
            self.user_moved();
            world.touch();
        }

        let painter = ui.painter_at(screen);
        painter.rect_filled(screen, CornerRadius::same(6), Color32::from_black_alpha(200));
        for p in world.projects.iter().filter(|p| world.shows_project(p)) {
            painter.rect_stroke(map.rect_to_map(p.rect), CornerRadius::same(2), Stroke::new(1.0, theme::rgb(p.color)), StrokeKind::Inside);
        }
        for c in world.cards.iter().filter(|c| world.shows_card(c)) {
            let r = map.rect_to_map(c.rect);
            let r = Rect::from_center_size(r.center(), r.size().max(Vec2::splat(3.0)));
            painter.rect_filled(r, CornerRadius::same(1), theme::status_color(c.status()));
        }
        painter.rect_stroke(map.rect_to_map(view), CornerRadius::same(2), Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, _out: &mut Vec<Event>) {
        let vp = self.viewport;
        if self.space.as_ref() != Some(&world.space) {
            // Another space was just opened (its slice set the camera): when that camera frames
            // the space's content, the view is that framing until the user moves it.
            self.space = Some(world.space.clone());
            self.flight = None;
            self.framed = crate::features::spaces::space_bounds(world)
                .filter(|r| vp.is_positive() && world.camera.same_view(&Camera::fitting(*r, vp, FIT_MARGIN, FIT_MAX_ZOOM)))
                .map(|target| Framed { target, camera: world.camera });
        }
        match ev {
            Event::FlyTo(id) => {
                if let Some(card) = world.card(id) {
                    let to = Camera::fitting(card.rect, vp, FIT_MARGIN, FIT_MAX_ZOOM);
                    self.fly(world.camera, to, Some(id.clone()), None);
                }
            }
            Event::FitRect(r) => self.fly(world.camera, Camera::fitting(*r, vp, FIT_MARGIN, FIT_MAX_ZOOM), None, Some(*r)),
            Event::RevealRect(r) => {
                if !world.camera.visible_world(vp).contains_rect(*r) {
                    self.fly(world.camera, Camera::fitting(*r, vp, FIT_MARGIN, FIT_MAX_ZOOM), None, Some(*r));
                }
            }
            Event::FlyCamera(to) => self.fly(world.camera, *to, None, None),
            // Switching space replaces the camera: an in-progress flight no longer makes sense.
            Event::EnterSpace(_) => self.flight = None,
            _ => {}
        }
    }

    fn fly(&mut self, from: Camera, to: Camera, focus: Option<CardId>, frames: Option<Rect>) {
        self.framed = None;
        self.flight = Some(Flight { from, to, start: None, focus, frames });
    }

    /// The user moved the view: it is no longer the app's framing, and a flight stops.
    fn user_moved(&mut self) {
        self.flight = None;
        self.framed = None;
    }

    /// The canvas got another viewport (a panel or a strip shown or hidden, the window resized).
    /// The view follows it (`Camera::follow_viewport`): a view still framed by the app is framed
    /// again when the top or left edge moved, any other view keeps the cards where they are on
    /// screen. A flight in progress keeps going: its start follows like the view, and a flight
    /// that frames an area lands on that area framed in the new viewport.
    fn viewport_changed(&mut self, world: &mut World, vp: Rect) {
        let old = self.viewport;
        self.viewport = vp;
        if !old.is_positive() || old == vp {
            return;
        }
        // Still framed only if nothing else moved the camera since.
        let framed = self.framed.filter(|f| world.camera.same_view(&f.camera));
        world.camera.follow_viewport(old, vp, framed.map(|f| f.target));
        self.framed = framed.map(|f| Framed { camera: world.camera, ..f });
        if let Some(f) = &mut self.flight {
            f.from.keep_anchor(old, vp);
            match f.frames.filter(|_| leading_edge_moved(old, vp)) {
                Some(target) => f.to = Camera::fitting(target, vp, FIT_MARGIN, FIT_MAX_ZOOM),
                None => f.to.keep_anchor(old, vp),
            }
        }
        world.touch();
    }

    /// Center of the view in world coordinates.
    pub fn view_center(&self, world: &World) -> Pos2 {
        world.camera.center
    }
}

/// Dot grid whose spacing adapts to the zoom to stay readable.
fn paint_grid(painter: &egui::Painter, cam: &Camera, vp: Rect) {
    let mut step = GRID_STEP;
    while step * cam.zoom < 18.0 {
        step *= 4.0;
    }
    let world = cam.visible_world(vp);
    let (x0, y0) = ((world.min.x / step).floor() as i64, (world.min.y / step).floor() as i64);
    let (x1, y1) = ((world.max.x / step).ceil() as i64, (world.max.y / step).ceil() as i64);
    if (x1 - x0) * (y1 - y0) > 20_000 {
        return;
    }
    let radius = (1.2 * cam.zoom).clamp(0.8, 1.6);
    for xi in x0..=x1 {
        for yi in y0..=y1 {
            let p = cam.to_screen(Pos2::new(xi as f32 * step, yi as f32 * step), vp);
            painter.circle_filled(p, radius, theme::GRID_DOT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::world::Project;
    use egui::pos2;

    fn vp() -> Rect {
        Rect::from_min_size(pos2(0.0, 60.0), Vec2::new(1200.0, 800.0))
    }

    /// Below a strip that appeared under the toolbar.
    fn below_strip() -> Rect {
        Rect::from_min_max(vp().min + Vec2::new(0.0, 40.0), vp().max)
    }

    fn group() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(1600.0, 700.0))
    }

    /// A world with one group, its space open and framed as the `spaces` slice does it.
    fn opened() -> (Canvas, World) {
        let mut w = World::default();
        w.projects.push(Project {
            id: "api".into(),
            name: "api".into(),
            color: [0; 3],
            rect: group(),
            cwd: None,
            templates: Vec::new(),
            messages: Vec::new(),
            worktree_location: None,
            task_settings: Default::default(),
            merge_strategy: None,
        });
        let mut c = Canvas { viewport: vp(), ..Canvas::default() };
        c.handle(&Event::Unfocus, &mut w, &mut vec![]);
        w.space = Some("api".into());
        w.camera = Camera::fitting(group(), vp(), FIT_MARGIN, FIT_MAX_ZOOM);
        c.handle(&Event::EnterSpace(Some("api".into())), &mut w, &mut vec![]);
        (c, w)
    }

    #[test]
    fn an_opened_space_is_framed_again_below_a_strip_or_beside_a_left_panel() {
        let (mut c, mut w) = opened();
        c.viewport_changed(&mut w, below_strip());
        assert_eq!(w.camera, Camera::fitting(group(), below_strip(), FIT_MARGIN, FIT_MAX_ZOOM));
        assert!(below_strip().contains_rect(w.camera.rect_to_screen(group(), below_strip())));
        // Still the framing: a panel on the left frames it again too.
        let beside = Rect::from_min_max(below_strip().min + Vec2::new(350.0, 0.0), below_strip().max);
        c.viewport_changed(&mut w, beside);
        assert_eq!(w.camera, Camera::fitting(group(), beside, FIT_MARGIN, FIT_MAX_ZOOM));
    }

    #[test]
    fn a_view_the_user_moved_is_not_framed_again() {
        let (mut c, mut w) = opened();
        w.camera.pan_screen(Vec2::new(-80.0, 25.0));
        c.user_moved();
        let p = pos2(100.0, 100.0);
        let before = w.camera.to_screen(p, vp());
        c.viewport_changed(&mut w, below_strip());
        assert!((w.camera.to_screen(p, below_strip()) - before).length() < 1e-3);
        // Moved by something other than the canvas: not framed either.
        let (mut c, mut w) = opened();
        w.camera.center += Vec2::new(300.0, 0.0);
        let before = w.camera.to_screen(p, vp());
        c.viewport_changed(&mut w, below_strip());
        assert!((w.camera.to_screen(p, below_strip()) - before).length() < 1e-3);
    }

    #[test]
    fn a_flight_in_progress_keeps_going() {
        let (mut c, mut w) = opened();
        let target = Rect::from_min_size(pos2(5000.0, 0.0), Vec2::new(800.0, 400.0));
        c.handle(&Event::FitRect(target), &mut w, &mut vec![]);
        assert!(c.framed.is_none(), "not framed while flying");
        c.viewport_changed(&mut w, below_strip());
        let f = c.flight.as_ref().expect("still flying");
        assert_eq!(f.to, Camera::fitting(target, below_strip(), FIT_MARGIN, FIT_MAX_ZOOM), "it lands framed in the new viewport");
        // A flight to a view (no area to frame) keeps its destination's screen positions.
        let (mut c, mut w) = opened();
        let to = Camera { center: pos2(-3000.0, 50.0), zoom: 0.5 };
        c.handle(&Event::FlyCamera(to), &mut w, &mut vec![]);
        c.viewport_changed(&mut w, below_strip());
        let mut expected = to;
        expected.keep_anchor(vp(), below_strip());
        assert_eq!(c.flight.as_ref().expect("still flying").to, expected);
        assert!(c.framed.is_none());
    }
}
