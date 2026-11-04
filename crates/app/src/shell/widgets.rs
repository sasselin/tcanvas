//! Small shared UI components.

use egui::{Id, Key, Modal, Modifiers, RichText};

use crate::shell::i18n::t;

/// Modal confirmation. `Some(true)`: confirmed (button or Enter), `Some(false)`: cancelled
/// (button, Escape or click outside), `None`: still open.
pub fn confirm(ctx: &egui::Context, id: &str, title: &str, body: &str, ok_label: &str) -> Option<bool> {
    let mut result = None;
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
        return Some(true);
    }
    let modal = Modal::new(Id::unique(("confirm", id))).show(ctx, |ui| {
        ui.set_width(380.0);
        ui.label(RichText::new(title).strong().size(16.0));
        ui.add_space(4.0);
        ui.label(body);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let danger = egui::Button::new(RichText::new(ok_label).color(egui::Color32::WHITE)).fill(egui::Color32::from_rgb(190, 60, 60));
            if ui.add(danger).clicked() {
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
