//! "Keyboard shortcuts" window: every shortcut and mouse gesture, read-only. Opened from the
//! keyboard button of the top bar, Ctrl+Shift+K, or the "Go to" palette.

pub mod keymap;

use egui::{Grid, Id, Modal, RichText, ScrollArea};

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;

#[derive(Default)]
pub struct Shortcuts {
    open: bool,
}

impl Shortcuts {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn handle(&mut self, ev: &Event) {
        if matches!(ev, Event::ShowShortcuts) {
            self.open();
        }
    }

    pub fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        let button = egui::Button::new(RichText::new("⌨").color(theme::TEXT_DIM)).frame(false);
        let hint = format!("{} · {}", t().shortcuts_title, keymap::help_combo().format());
        if ui.add(button).on_hover_text(hint).clicked() {
            self.open();
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let mut close = false;
        let modal = Modal::new(Id::unique("shortcuts")).show(ctx, |ui| {
            ui.set_max_width(620.0);
            ui.heading(t().shortcuts_title);
            ui.add_space(4.0);
            let max = ctx.content_rect().height() * 0.75;
            // A fixed height: otherwise the modal grows a little more on each frame up to `max`.
            ScrollArea::vertical().max_height(max).min_scrolled_height(max).show(ui, |ui| {
                // One grid for every section: the columns line up from top to bottom.
                Grid::new("shortcuts-grid").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                    for (n, section) in keymap::sections().iter().enumerate() {
                        if n > 0 {
                            // A blank row between sections (no `add_space` in a grid).
                            ui.label("");
                            ui.end_row();
                        }
                        ui.label(RichText::new(section.title).strong().color(theme::FOCUS));
                        ui.end_row();
                        for row in &section.rows {
                            let keys = row.combos.iter().map(|c| c.format()).collect::<Vec<_>>().join(" / ");
                            ui.label(RichText::new(keys).color(theme::TEXT));
                            ui.label(RichText::new(row.action).color(theme::TEXT_DIM));
                            ui.end_row();
                        }
                    }
                });
            });
            ui.add_space(8.0);
            ui.separator();
            // `Sides`, not a right-to-left layout in a row: that one grows the modal every frame.
            egui::Sides::new().show(ui, |ui| ui.label(RichText::new(t().shortcuts_read_only).small().color(theme::TEXT_DIM)), |ui| close = ui.button(t().ok).clicked());
        });
        if close || modal.should_close() {
            self.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_show_event_opens_the_window() {
        let mut s = Shortcuts::default();
        s.handle(&Event::Unfocus);
        assert!(!s.is_open());
        s.handle(&Event::ShowShortcuts);
        assert!(s.is_open());
    }
}
