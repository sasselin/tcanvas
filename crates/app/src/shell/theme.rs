//! Colors and a few layout sizes shared by all slices.

use egui::Color32;
use protocol::TermStatus;

pub const CANVAS_BG: Color32 = Color32::from_rgb(22, 24, 29);
pub const GRID_DOT: Color32 = Color32::from_rgb(48, 52, 60);
pub const CARD_BG: Color32 = Color32::from_rgb(16, 17, 21);
pub const CARD_BORDER: Color32 = Color32::from_rgb(58, 62, 72);
pub const TITLE_BG: Color32 = Color32::from_rgb(34, 37, 44);
pub const FOCUS: Color32 = Color32::from_rgb(110, 160, 255);
pub const TEXT: Color32 = Color32::from_rgb(220, 223, 228);
pub const TEXT_DIM: Color32 = Color32::from_rgb(140, 146, 158);
pub const WIRE: Color32 = Color32::from_rgb(120, 130, 150);

/// Height of a group's label above its frame, in screen pixels (readable at any zoom).
pub const HEADER_H: f32 = 24.0;
/// Distance between the pinned group bar (free grid) and the canvas corner, in screen pixels.
pub const PINNED_MARGIN: f32 = 12.0;
/// Height of the row of linked groups under the open group's label, in screen pixels.
pub const LINKED_ROW_H: f32 = 24.0;
/// Gap between a group's label and the row of linked groups, in screen pixels.
pub const LINKED_ROW_GAP: f32 = 6.0;

pub fn status_color(status: Option<TermStatus>) -> Color32 {
    match status {
        None => Color32::from_rgb(90, 95, 105),
        Some(TermStatus::Idle) => Color32::from_rgb(110, 120, 135),
        Some(TermStatus::Running) => Color32::from_rgb(70, 190, 120),
        Some(TermStatus::Stalled) => Color32::from_rgb(240, 170, 50),
        Some(TermStatus::WaitingInput) => Color32::from_rgb(90, 170, 255),
        Some(TermStatus::Exited(_)) => Color32::from_rgb(235, 85, 85),
        Some(TermStatus::Dead) => Color32::from_rgb(150, 60, 60),
    }
}

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}
