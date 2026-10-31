//! Fils entre terminaux. Cliquer sur un fil fait voler la caméra jusqu'au bout le plus éloigné.

use egui::{Id, Pos2, Rect, Sense, Stroke, Ui, Vec2, epaint::CubicBezierShape};

use crate::shell::event::Event;
use crate::shell::theme;
use crate::shell::world::{Wire, World};

const SAMPLES: usize = 24;
const HIT_PX: f32 = 6.0;

/// Courbe de Bézier entre deux cartes, ancrée sur les côtés qui se font face.
pub fn wire_path(a: Rect, b: Rect) -> [Pos2; 4] {
    let d = b.center() - a.center();
    let (pa, pb, dir) = if d.x.abs() >= d.y.abs() {
        let s = d.x.signum();
        let pa = if s > 0.0 { a.right_center() } else { a.left_center() };
        let pb = if s > 0.0 { b.left_center() } else { b.right_center() };
        (pa, pb, Vec2::new(s, 0.0))
    } else {
        let s = d.y.signum();
        let pa = if s > 0.0 { a.center_bottom() } else { a.center_top() };
        let pb = if s > 0.0 { b.center_top() } else { b.center_bottom() };
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

#[derive(Default)]
pub struct Wires;

impl Wires {
    /// Dessine les fils (sous les cartes) et gère clic / clic droit.
    pub fn ui(&mut self, ui: &mut Ui, world: &World, vp: Rect, events: &mut Vec<Event>) {
        let cam = world.camera;
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let painter = ui.painter_at(vp);
        for wire in &world.wires {
            let (Some(a), Some(b)) = (world.card(&wire.from), world.card(&wire.to)) else { continue };
            if !world.shows_card(a) || !world.shows_card(b) {
                continue;
            }
            let path = wire_path(a.rect, b.rect).map(|p| cam.to_screen(p, vp));
            let points = sample(path, SAMPLES);
            let bounds = Rect::from_points(&points).expand(HIT_PX);
            if !vp.intersects(bounds) {
                continue;
            }
            // Zone cliquable seulement autour du curseur quand il touche le fil : le reste de
            // la boîte englobante doit laisser passer les clics vers le fond du canevas.
            let near = pointer.filter(|p| distance_to_polyline(&points, *p) <= HIT_PX);
            let resp = near.map(|p| ui.interact(Rect::from_center_size(p, Vec2::splat(2.0 * HIT_PX)), Id::unique(("wire", &wire.from, &wire.to)), Sense::click()));
            let hot = resp.as_ref().is_some_and(|r| r.hovered());
            let color = if hot { theme::FOCUS } else { theme::WIRE };
            let width = if hot { 3.0 } else { 1.8 };
            painter.add(CubicBezierShape::from_points_stroke(path, false, egui::Color32::TRANSPARENT, Stroke::new(width, color)));
            painter.circle_filled(path[0], 4.0, color);
            painter.circle_filled(path[3], 4.0, color);
            let Some(resp) = resp else { continue };
            if hot {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                let p = resp.interact_pointer_pos().unwrap_or(path[0]);
                let target = if (path[0] - p).length() > (path[3] - p).length() { &wire.from } else { &wire.to };
                events.push(Event::FlyTo(target.clone()));
            }
            if resp.secondary_clicked() {
                events.push(Event::RemoveWire(wire.from.clone(), wire.to.clone()));
            }
            resp.on_hover_text(format!("{}  ⇄  {}\nclic : aller au bout opposé · clic droit : supprimer", a.title(), b.title()));
        }
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, _out: &mut Vec<Event>) {
        match ev {
            Event::AddWire(a, b) => {
                if a != b && world.card(a).is_some() && world.card(b).is_some() && !world.wires.iter().any(|w| w.same_as(a, b)) {
                    world.wires.push(Wire { from: a.clone(), to: b.clone() });
                    world.touch();
                }
            }
            Event::RemoveWire(a, b) => {
                world.wires.retain(|w| !w.same_as(a, b));
                world.touch();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::world::Card;
    use egui::pos2;

    fn r(x: f32, y: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), Vec2::new(100.0, 50.0))
    }

    #[test]
    fn path_uses_facing_sides() {
        let p = wire_path(r(0.0, 0.0), r(500.0, 10.0));
        assert_eq!(p[0], pos2(100.0, 25.0));
        assert_eq!(p[3], pos2(500.0, 35.0));
        let p = wire_path(r(0.0, 0.0), r(0.0, -400.0));
        assert_eq!(p[0], pos2(50.0, 0.0));
        assert_eq!(p[3], pos2(50.0, -350.0));
    }

    #[test]
    fn samples_start_and_end_on_anchors() {
        let p = wire_path(r(0.0, 0.0), r(800.0, 300.0));
        let s = sample(p, 10);
        assert_eq!(s.len(), 11);
        assert!((s[0] - p[0]).length() < 1e-3 && (s[10] - p[3]).length() < 1e-3);
    }

    #[test]
    fn hit_testing() {
        let line = [pos2(0.0, 0.0), pos2(100.0, 0.0)];
        assert_eq!(distance_to_polyline(&line, pos2(50.0, 5.0)), 5.0);
        assert_eq!(distance_to_polyline(&line, pos2(-3.0, 4.0)), 5.0);
    }

    #[test]
    fn wires_are_unique_and_undirected() {
        let mut w = World::default();
        let (a, b) = (Card::new(r(0.0, 0.0), None), Card::new(r(300.0, 0.0), None));
        let (ia, ib) = (a.id.clone(), b.id.clone());
        w.add_card(a);
        w.add_card(b);
        let mut wires = Wires;
        wires.handle(&Event::AddWire(ia.clone(), ib.clone()), &mut w, &mut vec![]);
        wires.handle(&Event::AddWire(ib.clone(), ia.clone()), &mut w, &mut vec![]);
        wires.handle(&Event::AddWire(ia.clone(), ia.clone()), &mut w, &mut vec![]);
        wires.handle(&Event::AddWire(ia.clone(), "absent".into()), &mut w, &mut vec![]);
        assert_eq!(w.wires.len(), 1);
        wires.handle(&Event::RemoveWire(ib, ia), &mut w, &mut vec![]);
        assert!(w.wires.is_empty());
    }
}
