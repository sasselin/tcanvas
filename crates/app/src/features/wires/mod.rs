//! Wires between groups (projects), drawn from frame to frame in the "All" view. Clicking a
//! wire flies the camera to the farther group. In a single project's space, every group reachable
//! through the wires (directly or through other groups) shows as a chip in one row under the
//! group's label; a chip opens that group's space. Drawing one: Alt + drag from a group's label
//! (see the `projects` slice).

use egui::emath::GuiRounding as _;
use egui::{Align2, CornerRadius, FontId, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, epaint::CubicBezierShape, pos2, vec2};

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme::{self, HEADER_H, LINKED_ROW_GAP, LINKED_ROW_H, PINNED_MARGIN};
use crate::shell::world::{Wire, World};

const SAMPLES: usize = 24;
const HIT_PX: f32 = 6.0;
const CHIP_GAP: f32 = 8.0;

/// Bezier curve between two frames, anchored on the sides that face each other. On the top and
/// bottom sides the anchor sits at three quarters of the width: a group's label and buttons
/// line the left part of its top edge.
pub fn wire_path(a: Rect, b: Rect) -> [Pos2; 4] {
    let at = |r: Rect, y: f32| pos2(r.min.x + 0.75 * r.width(), y);
    let d = b.center() - a.center();
    let (pa, pb, dir) = if d.x.abs() >= d.y.abs() {
        let s = d.x.signum();
        let pa = if s > 0.0 { a.right_center() } else { a.left_center() };
        let pb = if s > 0.0 { b.left_center() } else { b.right_center() };
        (pa, pb, Vec2::new(s, 0.0))
    } else {
        let s = d.y.signum();
        let pa = if s > 0.0 { at(a, a.max.y) } else { at(a, a.min.y) };
        let pb = if s > 0.0 { at(b, b.min.y) } else { at(b, b.max.y) };
        (pa, pb, Vec2::new(0.0, s))
    };
    let reach = ((pb - pa).length() * 0.4).max(60.0);
    [pa, pa + dir * reach, pb - dir * reach, pb]
}

pub fn sample(p: [Pos2; 4], n: usize) -> Vec<Pos2> {
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let v = p[0].to_vec2() * (u * u * u) + p[1].to_vec2() * (3.0 * u * u * t) + p[2].to_vec2() * (3.0 * u * t * t) + p[3].to_vec2() * (t * t * t);
            v.to_pos2()
        })
        .collect()
}

pub fn distance_to_polyline(points: &[Pos2], p: Pos2) -> f32 {
    points
        .windows(2)
        .map(|s| {
            let (a, b) = (s[0], s[1]);
            let ab = b - a;
            let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
            (a + ab * t - p).length()
        })
        .fold(f32::INFINITY, f32::min)
}

/// Screen rects of the linked-group chips from `origin`, left to right, wrapping to a new line
/// when a chip would pass `max_x`.
pub fn row_rects(origin: Pos2, sizes: &[Vec2], max_x: f32) -> Vec<Rect> {
    let mut at = origin;
    sizes
        .iter()
        .map(|&size| {
            if at.x > origin.x && at.x + size.x > max_x {
                at = pos2(origin.x, at.y + LINKED_ROW_H + CHIP_GAP);
            }
            let r = Rect::from_min_size(at, size);
            at.x = r.max.x + CHIP_GAP;
            r
        })
        .collect()
}

/// `r` moved (not resized) so it fits in `area` when it can.
pub fn keep_inside(r: Rect, area: Rect) -> Rect {
    let dx = (area.max.x - r.max.x).min(0.0) + (area.min.x - r.min.x).max(0.0);
    let dy = (area.max.y - r.max.y).min(0.0) + (area.min.y - r.min.y).max(0.0);
    r.translate(vec2(dx, dy))
}

#[derive(Default)]
pub struct Wires;

impl Wires {
    /// Draws the wires between group frames (below the cards) and handles click / right-click.
    /// In a single project's space, the other groups are hidden: linked groups show as chips.
    pub fn ui(&mut self, ui: &mut Ui, world: &World, vp: Rect, events: &mut Vec<Event>) {
        match &world.space {
            None => self.wires_ui(ui, world, vp, events),
            Some(id) => self.linked_row_ui(ui, world, id, vp, events),
        }
    }

    fn wires_ui(&mut self, ui: &mut Ui, world: &World, vp: Rect, events: &mut Vec<Event>) {
        let cam = world.camera;
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let painter = ui.painter_at(vp);
        for wire in &world.wires {
            let (Some(a), Some(b)) = (world.project(&wire.from), world.project(&wire.to)) else { continue };
            let path = wire_path(a.rect, b.rect).map(|p| cam.to_screen(p, vp));
            let points = sample(path, SAMPLES);
            let bounds = Rect::from_points(&points).expand(HIT_PX);
            if !vp.intersects(bounds) {
                continue;
            }
            // Clickable area only around the cursor when it touches the wire: the rest of
            // the bounding box must let clicks through to the canvas background.
            let near = pointer.filter(|p| distance_to_polyline(&points, *p) <= HIT_PX);
            let resp = near.map(|p| ui.interact(Rect::from_center_size(p, Vec2::splat(2.0 * HIT_PX)), Id::unique(("wire", &wire.from, &wire.to)), Sense::click()));
            let hot = resp.as_ref().is_some_and(|r| r.hovered());
            let color = if hot { theme::FOCUS } else { theme::WIRE };
            let width = if hot { 3.5 } else { 2.2 };
            painter.add(CubicBezierShape::from_points_stroke(path, false, egui::Color32::TRANSPARENT, Stroke::new(width, color)));
            painter.circle_filled(path[0], 5.0, theme::rgb(a.color));
            painter.circle_filled(path[3], 5.0, theme::rgb(b.color));
            let Some(resp) = resp else { continue };
            if hot {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                let p = resp.interact_pointer_pos().unwrap_or(path[0]);
                let target = if (path[0] - p).length() > (path[3] - p).length() { a } else { b };
                events.push(Event::FitRect(target.rect));
            }
            if resp.secondary_clicked() {
                events.push(Event::RemoveWire(wire.from.clone(), wire.to.clone()));
            }
            resp.on_hover_text((t().wire_hint)(&a.name, &b.name));
        }
    }

    /// The row of linked groups under the open group's label: under the pinned bar with the
    /// free grid, otherwise between the label (lifted by `projects`) and the frame.
    fn linked_row_ui(&mut self, ui: &mut Ui, world: &World, space: &str, vp: Rect, events: &mut Vec<Event>) {
        let Some(home) = world.project(space) else { return };
        let linked = world.linked_groups(space);
        if linked.is_empty() {
            return;
        }
        let origin = if world.free_grid_active() {
            vp.min + vec2(PINNED_MARGIN, PINNED_MARGIN + HEADER_H + LINKED_ROW_GAP)
        } else {
            let frame = world.camera.rect_to_screen(home.rect, vp);
            pos2(frame.min.x, frame.min.y - 4.0 - LINKED_ROW_H)
        };
        let font = FontId::proportional(13.0);
        let labels: Vec<(String, Vec2)> = linked
            .iter()
            .map(|other| {
                let text = format!("⇄ {}", other.name);
                let w = ui.ctx().fonts_mut(|f| f.layout_no_wrap(text.clone(), font.clone(), theme::TEXT).size().x);
                (text, vec2(w + 18.0, LINKED_ROW_H))
            })
            .collect();
        let sizes: Vec<Vec2> = labels.iter().map(|(_, s)| *s).collect();
        let area = vp.shrink(CHIP_GAP);
        let rects = row_rects(origin, &sizes, area.max.x);
        // A frame bigger than the view would push the row off screen: keep it inside.
        let bounds = rects.iter().copied().reduce(|a, b| a.union(b)).unwrap_or(Rect::NOTHING);
        let shift = keep_inside(bounds, area).min - bounds.min;
        // Above the cards (canvas layer), like the pinned bar, below menus and the palette.
        egui::Area::new(Id::unique(("linked-row", space))).order(egui::Order::Middle).fixed_pos(vp.min).show(ui.ctx(), |ui| {
            ui.set_clip_rect(vp);
            let painter = ui.painter_at(vp);
            for ((other, (text, _)), rect) in linked.iter().zip(&labels).zip(rects) {
                // Rounded to the UI grid, like egui's own widgets.
                let rect = rect.translate(shift).round_ui();
                let resp = ui.interact(rect, Id::unique(("wire-chip", space, &other.id)), Sense::click()).on_hover_text((t().linked_group_hint)(&other.name));
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, text));
                let color = theme::rgb(other.color);
                painter.rect_filled(rect, CornerRadius::same(6), if resp.hovered() { color.gamma_multiply(0.6) } else { color.gamma_multiply(0.3) });
                painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, color), StrokeKind::Inside);
                painter.text(rect.left_center() + vec2(9.0, 0.0), Align2::LEFT_CENTER, text, font.clone(), theme::TEXT);
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    events.push(Event::EnterSpace(Some(other.id.clone())));
                }
                // The area must cover the chips for the pointer to reach them.
                ui.expand_to_include_rect(rect);
            }
        });
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, _out: &mut Vec<Event>) {
        match ev {
            Event::AddWire(a, b) => {
                if a != b && world.project(a).is_some() && world.project(b).is_some() && !world.wires.iter().any(|w| w.same_as(a, b)) {
                    world.wires.push(Wire { from: a.clone(), to: b.clone() });
                    world.touch();
                }
            }
            Event::RemoveWire(a, b) => {
                world.wires.retain(|w| !w.same_as(a, b));
                world.touch();
            }
            Event::RemoveProject(id) if world.wires.iter().any(|w| w.touches(id)) => {
                world.wires.retain(|w| !w.touches(id));
                world.touch();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::world::Project;

    fn r(x: f32, y: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), Vec2::new(100.0, 50.0))
    }

    #[test]
    fn path_uses_facing_sides() {
        let p = wire_path(r(0.0, 0.0), r(500.0, 10.0));
        assert_eq!(p[0], pos2(100.0, 25.0));
        assert_eq!(p[3], pos2(500.0, 35.0));
        let p = wire_path(r(0.0, 0.0), r(0.0, -400.0));
        assert_eq!(p[0], pos2(75.0, 0.0), "top side: right of the label");
        assert_eq!(p[3], pos2(75.0, -350.0));
    }

    #[test]
    fn samples_start_and_end_on_anchors() {
        let p = wire_path(r(0.0, 0.0), r(800.0, 300.0));
        let s = sample(p, 10);
        assert_eq!(s.len(), 11);
        assert!((s[0] - p[0]).length() < 1e-3 && (s[10] - p[3]).length() < 1e-3);
    }

    #[test]
    fn chips_are_kept_on_screen() {
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0));
        let off_right = Rect::from_min_size(pos2(980.0, 100.0), Vec2::new(80.0, 24.0));
        assert_eq!(keep_inside(off_right, area), Rect::from_min_size(pos2(920.0, 100.0), Vec2::new(80.0, 24.0)));
        let below = Rect::from_min_size(pos2(10.0, 790.0), Vec2::new(80.0, 24.0));
        assert_eq!(keep_inside(below, area).max.y, 800.0);
        let inside = Rect::from_min_size(pos2(10.0, 10.0), Vec2::new(80.0, 24.0));
        assert_eq!(keep_inside(inside, area), inside);
    }

    #[test]
    fn hit_testing() {
        let line = [pos2(0.0, 0.0), pos2(100.0, 0.0)];
        assert_eq!(distance_to_polyline(&line, pos2(50.0, 5.0)), 5.0);
        assert_eq!(distance_to_polyline(&line, pos2(-3.0, 4.0)), 5.0);
    }

    fn two_projects() -> World {
        let mut w = World::default();
        for (id, x) in [("api", 0.0), ("web", 900.0)] {
            w.projects.push(Project {
                id: id.into(),
                name: id.into(),
                color: [1, 2, 3],
                rect: r(x, 0.0),
                cwd: None,
                templates: Vec::new(),
                messages: Vec::new(),
                worktree_location: None,
                task_settings: Default::default(),
                merge_strategy: None,
            });
        }
        w
    }

    #[test]
    fn wires_link_existing_groups_once_and_undirected() {
        let mut w = two_projects();
        let mut wires = Wires;
        let s = |a: &str| a.to_string();
        wires.handle(&Event::AddWire(s("api"), s("web")), &mut w, &mut vec![]);
        wires.handle(&Event::AddWire(s("web"), s("api")), &mut w, &mut vec![]);
        wires.handle(&Event::AddWire(s("api"), s("api")), &mut w, &mut vec![]);
        wires.handle(&Event::AddWire(s("api"), s("absent")), &mut w, &mut vec![]);
        assert_eq!(w.wires, vec![Wire { from: s("api"), to: s("web") }]);
        assert_eq!(w.wires[0].other("web"), "api");
        wires.handle(&Event::RemoveWire(s("web"), s("api")), &mut w, &mut vec![]);
        assert!(w.wires.is_empty());
    }

    #[test]
    fn removing_a_group_drops_its_wires() {
        let mut w = two_projects();
        w.projects.push(Project {
            id: "db".into(),
            name: "db".into(),
            color: [0; 3],
            rect: r(0.0, 900.0),
            cwd: None,
            templates: Vec::new(),
            messages: Vec::new(),
            worktree_location: None,
            task_settings: Default::default(),
            merge_strategy: None,
        });
        let mut wires = Wires;
        wires.handle(&Event::AddWire("api".into(), "web".into()), &mut w, &mut vec![]);
        wires.handle(&Event::AddWire("web".into(), "db".into()), &mut w, &mut vec![]);
        wires.handle(&Event::RemoveProject("api".into()), &mut w, &mut vec![]);
        assert_eq!(w.wires, vec![Wire { from: "web".into(), to: "db".into() }]);
    }

    #[test]
    fn linked_chips_line_up_and_wrap_without_overlapping() {
        let size = Vec2::new(80.0, LINKED_ROW_H);
        let row = row_rects(pos2(100.0, 50.0), &[size, size], 1000.0);
        assert_eq!(row[0].min, pos2(100.0, 50.0));
        assert_eq!(row[1].min, pos2(100.0 + 80.0 + CHIP_GAP, 50.0), "same line, left to right");
        let wrapped = row_rects(pos2(100.0, 50.0), &[size, size, size], 300.0);
        assert_eq!(wrapped[1].min.y, 50.0);
        assert_eq!(wrapped[2].min, pos2(100.0, 50.0 + LINKED_ROW_H + CHIP_GAP), "next line from the left");
        assert!(!wrapped[0].intersects(wrapped[1]) && !wrapped[1].intersects(wrapped[2]));
        // A chip wider than the space still goes on the first line.
        let wide = row_rects(pos2(0.0, 0.0), &[Vec2::new(500.0, LINKED_ROW_H)], 300.0);
        assert_eq!(wide[0].min, Pos2::ZERO);
    }
}
