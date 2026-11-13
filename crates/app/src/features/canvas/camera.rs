//! World ↔ screen transforms. The world is infinite; the camera is just a center and a zoom.

use egui::{Pos2, Rect, Vec2};

use crate::shell::world::Camera;

pub const MIN_ZOOM: f32 = 0.05;
pub const MAX_ZOOM: f32 = 3.0;
/// Screen margin and largest zoom of the app's framings (a space, a card, a group...).
pub const FIT_MARGIN: f32 = 60.0;
pub const FIT_MAX_ZOOM: f32 = 1.0;

/// The viewport's top or left edge moved.
pub fn leading_edge_moved(old_vp: Rect, new_vp: Rect) -> bool {
    (old_vp.min.x - new_vp.min.x).abs() > 0.01 || (old_vp.min.y - new_vp.min.y).abs() > 0.01
}

impl Camera {
    pub fn to_screen(&self, p: Pos2, vp: Rect) -> Pos2 {
        vp.center() + (p - self.center) * self.zoom
    }

    pub fn to_world(&self, s: Pos2, vp: Rect) -> Pos2 {
        self.center + (s - vp.center()) / self.zoom
    }

    pub fn rect_to_screen(&self, r: Rect, vp: Rect) -> Rect {
        Rect::from_min_max(self.to_screen(r.min, vp), self.to_screen(r.max, vp))
    }

    pub fn visible_world(&self, vp: Rect) -> Rect {
        Rect::from_min_max(self.to_world(vp.min, vp), self.to_world(vp.max, vp))
    }

    /// Zooms while keeping the world point under `anchor` (screen) fixed.
    pub fn zoom_around(&mut self, anchor: Pos2, factor: f32, vp: Rect) {
        let before = self.to_world(anchor, vp);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.to_world(anchor, vp);
        self.center += before - after;
    }

    /// Pans the view by a screen drag (the content follows the mouse).
    pub fn pan_screen(&mut self, delta: Vec2) {
        self.center -= delta / self.zoom;
    }

    /// Follows a viewport resize (side panel, window, a bar appearing above) so that content
    /// does not drift with the viewport's center. On each axis it stays fixed relative to the
    /// viewport's top-left corner, except when only the top (left) edge moved: then it stays
    /// where it is on screen, and the bar simply covers or uncovers a strip of it.
    pub fn keep_anchor(&mut self, old_vp: Rect, new_vp: Rect) {
        let moved = |a: f32, b: f32| (a - b).abs() > 0.01;
        let follow = |old_min: f32, new_min: f32, old_max: f32, new_max: f32| if moved(old_min, new_min) && !moved(old_max, new_max) { 0.0 } else { new_min - old_min };
        let corner = Vec2::new(follow(old_vp.min.x, new_vp.min.x, old_vp.max.x, new_vp.max.x), follow(old_vp.min.y, new_vp.min.y, old_vp.max.y, new_vp.max.y));
        let delta = (new_vp.center() - old_vp.center()) - corner;
        self.center += delta / self.zoom;
    }

    /// Follows a viewport change. `framed`: the view is still the app's framing of that world
    /// area (a space just opened, a "show all"...), untouched by the user since. When the top or
    /// left edge moved (a strip above, a panel on the left), a framed view is framed again for the
    /// new viewport, at once (nothing would be hidden under the strip or the panel); any other
    /// view keeps its screen positions (`keep_anchor`).
    pub fn follow_viewport(&mut self, old_vp: Rect, new_vp: Rect, framed: Option<Rect>) {
        match framed.filter(|_| leading_edge_moved(old_vp, new_vp)) {
            Some(target) => *self = Camera::fitting(target, new_vp, FIT_MARGIN, FIT_MAX_ZOOM),
            None => self.keep_anchor(old_vp, new_vp),
        }
    }

    /// Roughly the same view (to tell whether something else moved the camera).
    pub fn same_view(&self, other: &Camera) -> bool {
        (self.center - other.center).length() * self.zoom < 0.5 && (self.zoom / other.zoom - 1.0).abs() < 1e-4
    }

    /// Camera that shows all of `target` with a margin (screen pixels).
    pub fn fitting(target: Rect, vp: Rect, margin: f32, max_zoom: f32) -> Camera {
        let avail = (vp.size() - Vec2::splat(2.0 * margin)).max(Vec2::splat(1.0));
        let size = target.size().max(Vec2::splat(1.0));
        let zoom = (avail.x / size.x).min(avail.y / size.y).clamp(MIN_ZOOM, max_zoom);
        Camera { center: target.center(), zoom }
    }

    /// Interpolation; zoom varies on a log scale for a smooth flight.
    pub fn lerp(a: Camera, b: Camera, t: f32) -> Camera {
        if t <= 0.0 {
            return a;
        }
        if t >= 1.0 {
            return b;
        }
        Camera { center: a.center + (b.center - a.center) * t, zoom: (a.zoom.ln() + (b.zoom.ln() - a.zoom.ln()) * t).exp() }
    }
}

/// Speeds up, then slows down.
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn vp() -> Rect {
        Rect::from_min_size(pos2(0.0, 30.0), Vec2::new(800.0, 600.0))
    }

    fn close(a: Pos2, b: Pos2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn world_screen_roundtrip() {
        let cam = Camera { center: pos2(120.0, -40.0), zoom: 0.37 };
        let p = pos2(999.0, 12.5);
        assert!(close(cam.to_world(cam.to_screen(p, vp()), vp()), p));
        assert!(close(cam.to_screen(cam.center, vp()), vp().center()));
    }

    #[test]
    fn zoom_keeps_point_under_cursor() {
        let mut cam = Camera { center: pos2(10.0, 10.0), zoom: 1.0 };
        let anchor = pos2(700.0, 100.0);
        let world_before = cam.to_world(anchor, vp());
        cam.zoom_around(anchor, 2.5, vp());
        assert!((cam.zoom - 2.5).abs() < 1e-6);
        assert!(close(cam.to_world(anchor, vp()), world_before));
    }

    #[test]
    fn zoom_is_clamped() {
        let mut cam = Camera::default();
        cam.zoom_around(vp().center(), 1e6, vp());
        assert_eq!(cam.zoom, MAX_ZOOM);
        cam.zoom_around(vp().center(), 1e-9, vp());
        assert_eq!(cam.zoom, MIN_ZOOM);
    }

    #[test]
    fn pan_follows_the_mouse() {
        let mut cam = Camera { center: Pos2::ZERO, zoom: 2.0 };
        let grabbed = cam.to_world(pos2(100.0, 100.0), vp());
        cam.pan_screen(Vec2::new(50.0, -20.0));
        assert!(close(cam.to_world(pos2(150.0, 80.0), vp()), grabbed));
    }

    #[test]
    fn resizing_the_viewport_keeps_content_in_place() {
        let mut cam = Camera { center: pos2(300.0, -80.0), zoom: 0.6 };
        let p = pos2(512.0, 77.0);
        let before = cam.to_screen(p, vp());
        // A side panel widens on the right: the canvas loses 300 px.
        let narrower = Rect::from_min_max(vp().min, vp().max - Vec2::new(300.0, 0.0));
        cam.keep_anchor(vp(), narrower);
        assert!(close(cam.to_screen(p, narrower), before));
        // The whole viewport moves down: content follows the corner.
        let lower = narrower.translate(Vec2::new(0.0, 20.0));
        cam.keep_anchor(narrower, lower);
        assert!(close(cam.to_screen(p, lower), before + Vec2::new(0.0, 20.0)));
    }

    #[test]
    fn a_bar_appearing_above_leaves_content_in_place() {
        let mut cam = Camera { center: pos2(300.0, -80.0), zoom: 0.6 };
        let p = pos2(512.0, 77.0);
        let before = cam.to_screen(p, vp());
        // Only the top edge moves down (a strip under the toolbar), then back up.
        let shorter = Rect::from_min_max(vp().min + Vec2::new(0.0, 28.0), vp().max);
        cam.keep_anchor(vp(), shorter);
        assert!(close(cam.to_screen(p, shorter), before));
        cam.keep_anchor(shorter, vp());
        assert!(close(cam.to_screen(p, vp()), before));
        // Same on the left edge.
        let narrower = Rect::from_min_max(vp().min + Vec2::new(40.0, 0.0), vp().max);
        cam.keep_anchor(vp(), narrower);
        assert!(close(cam.to_screen(p, narrower), before));
    }

    #[test]
    fn a_framed_view_is_framed_again_when_the_top_or_left_edge_moves() {
        let target = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(1000.0, 500.0));
        for new_vp in [
            // A strip under the toolbar.
            Rect::from_min_max(vp().min + Vec2::new(0.0, 50.0), vp().max),
            // A panel on the left.
            Rect::from_min_max(vp().min + Vec2::new(300.0, 0.0), vp().max),
        ] {
            let mut cam = Camera::fitting(target, vp(), FIT_MARGIN, FIT_MAX_ZOOM);
            cam.follow_viewport(vp(), new_vp, Some(target));
            assert_eq!(cam, Camera::fitting(target, new_vp, FIT_MARGIN, FIT_MAX_ZOOM));
            let on_screen = cam.rect_to_screen(target, new_vp);
            assert!(new_vp.shrink(FIT_MARGIN - 1.0).contains_rect(on_screen), "{on_screen:?} in {new_vp:?}");
        }
    }

    #[test]
    fn a_view_the_user_moved_keeps_its_screen_positions() {
        let target = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(1000.0, 500.0));
        let mut cam = Camera::fitting(target, vp(), FIT_MARGIN, FIT_MAX_ZOOM);
        cam.pan_screen(Vec2::new(35.0, -12.0));
        let p = pos2(10.0, 20.0);
        let before = cam.to_screen(p, vp());
        let shorter = Rect::from_min_max(vp().min + Vec2::new(0.0, 50.0), vp().max);
        cam.follow_viewport(vp(), shorter, None);
        assert!(close(cam.to_screen(p, shorter), before));
        // A framed view whose right edge moves (the Changes panel) keeps its positions too.
        let mut framed = Camera::fitting(target, vp(), FIT_MARGIN, FIT_MAX_ZOOM);
        let before = framed.to_screen(p, vp());
        let narrower = Rect::from_min_max(vp().min, vp().max - Vec2::new(300.0, 0.0));
        framed.follow_viewport(vp(), narrower, Some(target));
        assert!(close(framed.to_screen(p, narrower), before));
    }

    #[test]
    fn fitting_shows_whole_target() {
        let target = Rect::from_min_size(pos2(5000.0, 5000.0), Vec2::new(4000.0, 1000.0));
        let cam = Camera::fitting(target, vp(), 40.0, 1.0);
        let on_screen = cam.rect_to_screen(target, vp());
        assert!(vp().shrink(39.0).contains_rect(on_screen), "{on_screen:?}");
        // A small target does not exceed the requested max zoom.
        let small = Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0));
        assert_eq!(Camera::fitting(small, vp(), 40.0, 1.0).zoom, 1.0);
    }

    #[test]
    fn lerp_hits_both_ends() {
        let a = Camera { center: Pos2::ZERO, zoom: 0.1 };
        let b = Camera { center: pos2(100.0, 50.0), zoom: 2.0 };
        assert_eq!(Camera::lerp(a, b, 0.0), a);
        let end = Camera::lerp(a, b, 1.0);
        assert!(close(end.center, b.center) && (end.zoom - b.zoom).abs() < 1e-5);
        let mid = Camera::lerp(a, b, 0.5);
        assert!((mid.zoom - (0.1f32 * 2.0).sqrt()).abs() < 1e-4);
    }

    #[test]
    fn easing_is_monotonic() {
        let samples: Vec<f32> = (0..=20).map(|i| ease_in_out(i as f32 / 20.0)).collect();
        assert_eq!(samples[0], 0.0);
        assert_eq!(samples[20], 1.0);
        assert!(samples.windows(2).all(|w| w[1] >= w[0]));
    }
}
