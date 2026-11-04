//! Infinite canvas: background, panning, zoom, camera flights and minimap.

mod camera;
mod minimap;

pub use camera::{MAX_ZOOM, MIN_ZOOM};

use egui::{Color32, CornerRadius, CursorIcon, Id, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::shell::event::Event;
use crate::shell::theme;
use crate::shell::world::{Camera, CardId, World, project_at};
use camera::ease_in_out;
use minimap::MiniMap;

const FLIGHT_SECS: f64 = 0.45;
const GRID_STEP: f32 = 40.0;
const MINIMAP_SIZE: Vec2 = Vec2::new(220.0, 150.0);

struct Flight {
    from: Camera,
    to: Camera,
    start: Option<f64>,
    focus: Option<CardId>,
}

pub struct Canvas {
    viewport: Rect,
    flight: Option<Flight>,
    /// Empty space is being dragged (left or middle button).
    bg_panning: bool,
    /// A hand drag (Shift) is in progress: it continues even if Shift is released.
    hand_dragging: bool,
}

impl Default for Canvas {
    fn default() -> Self {
        Self { viewport: Rect::NOTHING, flight: None, bg_panning: false, hand_dragging: false }
    }
}

impl Canvas {
    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    /// Background and interactions on empty space. Call before any other canvas element.
    pub fn background(&mut self, ui: &mut Ui, world: &mut World, events: &mut Vec<Event>) {
        let vp = ui.max_rect();
        let old = self.viewport;
        if old.is_positive() && old != vp {
            // Resizing the Changes panel (or the window) must not slide the cards around.
            world.camera.keep_anchor(old, vp);
            if let Some(f) = &mut self.flight {
                f.from.keep_anchor(old, vp);
                f.to.keep_anchor(old, vp);
            }
            world.touch();
        }
        self.viewport = vp;
        let painter = ui.painter_at(vp);
        painter.rect_filled(vp, CornerRadius::ZERO, theme::CANVAS_BG);
        paint_grid(&painter, &world.camera, vp);

        let bg = ui.interact(vp, Id::unique("canvas-background"), Sense::click_and_drag());
        self.bg_panning = bg.dragged_by(PointerButton::Primary) || bg.dragged_by(PointerButton::Middle);
        if self.bg_panning {
            world.camera.pan_screen(bg.drag_delta());
            self.flight = None;
            world.touch();
        }
        if bg.double_clicked() {
            if let Some(p) = bg.interact_pointer_pos() {
                let at = world.camera.to_world(p, vp);
                let project = project_at(&world.projects, at).filter(|p| world.shows_project(p)).map(|p| p.id.clone()).or(world.space.clone());
                events.push(Event::NewCard { at: Some(at), project });
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
                self.flight = None;
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
            self.flight = None;
            world.touch();
        } else if scroll != Vec2::ZERO && !wheel_taken {
            world.camera.pan_screen(scroll);
            self.flight = None;
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
            self.flight = None;
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
        match ev {
            Event::FlyTo(id) => {
                if let Some(card) = world.card(id) {
                    let to = Camera::fitting(card.rect, vp, 60.0, 1.0);
                    self.fly(world.camera, to, Some(id.clone()));
                }
            }
            Event::FitRect(r) => self.fly(world.camera, Camera::fitting(*r, vp, 60.0, 1.0), None),
            Event::RevealRect(r) => {
                if !world.camera.visible_world(vp).contains_rect(*r) {
                    self.fly(world.camera, Camera::fitting(*r, vp, 60.0, 1.0), None);
                }
            }
            Event::FlyCamera(to) => self.fly(world.camera, *to, None),
            // Switching space replaces the camera: an in-progress flight no longer makes sense.
            Event::EnterSpace(_) => self.flight = None,
            _ => {}
        }
    }

    fn fly(&mut self, from: Camera, to: Camera, focus: Option<CardId>) {
        self.flight = Some(Flight { from, to, start: None, focus });
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
