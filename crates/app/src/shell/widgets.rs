//! Small shared UI components.

use egui::{Id, Key, Modal, Modifiers, RichText};

use crate::shell::i18n::t;

/// Modal confirmation. `Some(true)`: confirmed (button or Enter), `Some(false)`: cancelled
/// (button, Escape or click outside), `None`: still open.
pub fn confirm(ctx: &egui::Context, id: &str, title: &str, body: &str, ok_label: &str) -> Option<bool> {
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        return Some(true);
    }
    confirm_dialog(ctx, id, title, body, ok_label, 380.0, None, true)
}

/// Like `confirm`, for a deletion: only a click on the button confirms (Enter does nothing), and
/// a long body scrolls instead of pushing the buttons off screen. `ready`: the button can be
/// clicked (`false` while what it would delete is not fully listed yet).
pub fn confirm_by_click(ctx: &egui::Context, id: &str, title: &str, body: &str, ok_label: &str, ready: bool) -> Option<bool> {
    // Swallowed: Enter must neither confirm nor reach a terminal.
    ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    let max = (ctx.content_rect().height() * 0.5).max(120.0);
    confirm_dialog(ctx, id, title, body, ok_label, 460.0, Some(max), ready)
}

#[allow(clippy::too_many_arguments)]
fn confirm_dialog(ctx: &egui::Context, id: &str, title: &str, body: &str, ok_label: &str, width: f32, max_body: Option<f32>, ready: bool) -> Option<bool> {
    let mut result = None;
    let modal = Modal::new(Id::unique(("confirm", id))).show(ctx, |ui| {
        ui.set_width(width);
        ui.label(RichText::new(title).strong().size(16.0));
        ui.add_space(4.0);
        match max_body {
            Some(max) => {
                egui::ScrollArea::vertical().max_height(max).show(ui, |ui| ui.label(body));
            }
            None => {
                ui.label(body);
            }
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let danger = egui::Button::new(RichText::new(ok_label).color(egui::Color32::WHITE)).fill(egui::Color32::from_rgb(190, 60, 60));
            if ui.add_enabled(ready, danger).clicked() {
                result = Some(true);
            }
            if ui.button(t().cancel).clicked() {
                result = Some(false);
            }
        });
    });
    if modal.should_close() && result.is_none() {
        result = Some(false);
    }
    result
}

/// Modal input box. `Some(true)`: confirmed, `Some(false)`: cancelled, `None`: still open.
pub fn text_prompt(ctx: &egui::Context, id: &str, title: &str, text: &mut String) -> Option<bool> {
    let mut result = None;
    let modal = Modal::new(Id::unique(("prompt", id))).show(ctx, |ui| {
        ui.set_width(360.0);
        ui.label(title);
        let edit = ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY).id(Id::unique(("prompt-edit", id))));
        if !edit.has_focus() && !edit.lost_focus() {
            edit.request_focus();
        }
        if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            result = Some(true);
        }
        ui.horizontal(|ui| {
            if ui.button(t().ok).clicked() {
                result = Some(true);
            }
            if ui.button(t().cancel).clicked() {
                result = Some(false);
            }
        });
    });
    if modal.should_close() && result.is_none() {
        result = Some(false);
    }
    result
}

/// `feat/very-long-branch-name` -> `feat/ver…anch-name`: keeps both ends, which tell branches apart.
pub fn elide_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max < 3 {
        return s.to_string();
    }
    let tail = (max - 1) / 2;
    let head = max - 1 - tail;
    let chars: Vec<char> = s.chars().collect();
    format!("{}…{}", chars[..head].iter().collect::<String>(), chars[n - tail..].iter().collect::<String>())
}
