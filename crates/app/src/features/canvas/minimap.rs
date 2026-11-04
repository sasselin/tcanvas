//! Mapping between the minimap (screen corner) and the world.

use egui::{Pos2, Rect, Vec2};

pub struct MiniMap {
    pub screen: Rect,
    world_center: Pos2,
    scale: f32,
}

impl MiniMap {
    /// `world` is enlarged to keep its aspect ratio; it is centered in `screen`.
    pub fn new(screen: Rect, world: Rect) -> Self {
        let size = world.size().max(Vec2::splat(1.0));
        let scale = (screen.width() / size.x).min(screen.height() / size.y);
        Self { screen, world_center: world.center(), scale }
    }

    pub fn to_map(&self, p: Pos2) -> Pos2 {
        self.screen.center() + (p - self.world_center) * self.scale
    }

    pub fn rect_to_map(&self, r: Rect) -> Rect {
        Rect::from_min_max(self.to_map(r.min), self.to_map(r.max))
    }

    pub fn to_world(&self, s: Pos2) -> Pos2 {
        self.world_center + (s - self.screen.center()) / self.scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn whole_world_fits_and_roundtrips() {
        let screen = Rect::from_min_size(pos2(600.0, 400.0), Vec2::new(200.0, 140.0));
        let world = Rect::from_min_max(pos2(-3000.0, -500.0), pos2(9000.0, 2500.0));
        let m = MiniMap::new(screen, world);
        let mapped = m.rect_to_map(world);
        assert!(screen.expand(0.01).contains_rect(mapped), "{mapped:?}");
        let p = pos2(1234.0, 77.0);
        assert!((m.to_world(m.to_map(p)) - p).length() < 1e-2);
    }
}
