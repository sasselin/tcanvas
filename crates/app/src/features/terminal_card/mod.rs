//! Terminal cards: PTY, rendering, keyboard/mouse, moving, resizing.

mod clipboard;
mod input;
mod pty;
mod render;

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};

use alacritty_terminal::event::Event as TermEvent;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::Rgb;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Sense, Stroke,
    StrokeKind, Ui, Vec2, pos2, vec2,
};
use protocol::{AgentEvent, SESSION_PREFIX};

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::widgets::{confirm, text_prompt};
use crate::shell::world::{Card, CardId, World};
pub use clipboard::{ClipboardSource, Fixed as FixedClipboard};
use input::{ClipAction, MouseButton};
use pty::{GridSize, TermRuntime};
pub use pty::{Launch, attach_command};

/// Dimensions in world units (1 unit = 1 point at zoom 1).
pub const TITLE_H: f32 = 28.0;
pub const PAD: f32 = 6.0;
pub const BASE_FONT: f32 = 14.0;
const GRIP: f32 = 16.0;
/// Below this, the × button is too small to aim at: use the menu or Ctrl+Shift+W.
const CLOSE_MIN_ZOOM: f32 = 0.35;
const MIN_COLS: u16 = 20;
const MIN_ROWS: u16 = 4;
const DEFAULT_COLS: f32 = 120.0;
const DEFAULT_ROWS: f32 = 34.0;
/// Below this font size (pixels), draw a preview instead of the grid.
const LOD_FONT_PX: f32 = 5.5;

/// Launch real terminals, or nothing (UI tests without a PTY).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Launcher {
    Tmux,
    Disabled,
}

pub fn base_cell(ctx: &egui::Context) -> Vec2 {
    let font = FontId::monospace(BASE_FONT);
    ctx.fonts_mut(|f| vec2(f.glyph_width(&font, 'M'), f.row_height(&font)))
}

/// Number of columns and rows that fit in a card (world units).
pub fn grid_for(rect: Rect, cell: Vec2) -> GridSize {
    let cols = ((rect.width() - 2.0 * PAD) / cell.x).floor().max(MIN_COLS as f32) as u16;
    let rows = ((rect.height() - TITLE_H - 2.0 * PAD) / cell.y).floor().max(MIN_ROWS as f32) as u16;
    GridSize { cols, rows }
}

pub fn card_size(cell: Vec2, cols: f32, rows: f32) -> Vec2 {
    vec2(cols * cell.x + 2.0 * PAD, rows * cell.y + TITLE_H + 2.0 * PAD)
}

pub struct TerminalCards {
    ctx: egui::Context,
    launcher: Launcher,
    runtimes: HashMap<CardId, TermRuntime>,
    spawn_errors: HashMap<CardId, String>,
    tx: Sender<(CardId, TermEvent)>,
    rx: Receiver<(CardId, TermEvent)>,
    /// Keyboard captured for the focused card, with the frame's modifiers.
    keys: Vec<(egui::Event, Modifiers)>,
    mods: Modifiers,
    rename: Option<(CardId, String)>,
    /// Close requested, awaiting confirmation.
    confirm_close: Option<CardId>,
    stall: Option<(CardId, String)>,
    /// Sessions killed on purpose: no notification, no re-adoption.
    killed: HashSet<String>,
    viewport: Rect,
    /// Clipboard read for Ctrl+Shift+V and right-click.
    clipboard: Box<dyn ClipboardSource>,
    /// Without a real terminal (tests): what would have been sent to each card.
    written: Vec<(CardId, Vec<u8>)>,
}

impl TerminalCards {
    pub fn new(ctx: egui::Context, launcher: Launcher) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            ctx,
            launcher,
            runtimes: HashMap::new(),
            spawn_errors: HashMap::new(),
            tx,
            rx,
            keys: Vec::new(),
            mods: Modifiers::NONE,
            rename: None,
            confirm_close: None,
            stall: None,
            killed: HashSet::new(),
            viewport: Rect::NOTHING,
            clipboard: match launcher {
                Launcher::Tmux => Box::new(clipboard::System),
                Launcher::Disabled => Box::new(clipboard::Fixed(None)),
            },
            written: Vec::new(),
        }
    }

    /// Replaces the clipboard (tests).
    pub fn set_clipboard(&mut self, clipboard: Box<dyn ClipboardSource>) {
        self.clipboard = clipboard;
    }

    /// Shows fixed terminal output in a card, without a PTY (screenshots, UI tests).
    pub fn show_canned(&mut self, world: &World, id: &str, bytes: &[u8]) {
        let Some(card) = world.card(id) else { return };
        let cell = base_cell(&self.ctx);
        let rt = TermRuntime::canned(&card.id, grid_for(card.rect, cell), cell_px(cell), bytes, self.tx.clone(), self.ctx.clone());
        self.runtimes.insert(card.id.clone(), rt);
    }

    /// Bytes sent to a card without a real terminal (tests, `Launcher::Disabled`).
    pub fn written(&self, id: &str) -> Vec<u8> {
        self.written.iter().filter(|(c, _)| c == id).flat_map(|(_, b)| b.clone()).collect()
    }

    /// Sends bytes to a card's terminal (or records them, without a real terminal).
    fn send_to(&mut self, id: &str, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if let Some(rt) = self.runtimes.get(id) {
            rt.scroll_to_bottom();
            rt.write(bytes);
        } else if self.launcher == Launcher::Disabled {
            self.written.push((id.to_string(), bytes));
        }
    }

    /// Terminal mode of a card (default mode without a real terminal).
    fn mode_of(&self, id: &str) -> TermMode {
        self.runtimes.get(id).map(|rt| *rt.term.lock().mode()).unwrap_or_default()
    }

    /// Pastes the system clipboard into the card.
    fn paste_clipboard(&mut self, id: &str) {
        if let Some(text) = self.clipboard.text() {
            let bracketed = self.mode_of(id).contains(TermMode::BRACKETED_PASTE);
            self.send_to(id, input::paste_bytes(&text, bracketed));
        }
    }

    pub fn has_dialog(&self) -> bool {
        self.rename.is_some() || self.stall.is_some() || self.confirm_close.is_some()
    }

    /// Removes everything keyboard-related from `events` and sends it to the focused terminal.
    /// App shortcuts must have been removed beforehand. Modifiers
    /// are tracked in event order (text events carry none).
    pub fn capture_keyboard(&mut self, events: &mut Vec<egui::Event>) {
        events.retain(|ev| {
            match ev {
                egui::Event::ModifiersChanged(m) => self.mods = *m,
                egui::Event::Key { modifiers, .. } => self.mods = *modifiers,
                _ => {}
            }
            let take = matches!(
                ev,
                egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Copy | egui::Event::Cut | egui::Event::Key { .. }
            );
            if take {
                self.keys.push((ev.clone(), self.mods));
            }
            !take
        });
    }

    /// Visible text of a card (tests).
    pub fn screen_text(&self, id: &str) -> Option<String> {
        self.runtimes.get(id).map(|rt| rt.screen_text())
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.runtimes.contains_key(id)
    }

    /// PTY events: replies to the terminal, clipboard, process exit.
    pub fn poll(&mut self, world: &mut World, events: &mut Vec<Event>) {
        while let Ok((id, ev)) = self.rx.try_recv() {
            let Some(rt) = self.runtimes.get(&id) else { continue };
            match ev {
                TermEvent::PtyWrite(text) => rt.write(text.into_bytes()),
                TermEvent::ClipboardStore(_, text) => {
                    self.ctx.copy_text(text);
                    events.push(Event::Notify(id.clone(), crate::shell::event::Notice::Copied));
                }
                TermEvent::ColorRequest(index, format) => {
                    let c = match index {
                        0..=255 => render::indexed(index as u8),
                        257 => theme::CARD_BG,
                        _ => theme::TEXT,
                    };
                    rt.write(format(Rgb { r: c.r(), g: c.g(), b: c.b() }).into_bytes());
                }
                TermEvent::TextAreaSizeRequest(format) => rt.write(format(rt.window_size()).into_bytes()),
                TermEvent::ChildExit(_) | TermEvent::Exit => {
                    self.runtimes.remove(&id);
                    if let Some(card) = world.card_mut(&id) {
                        card.pty_dead = true;
                        if !self.killed.contains(&card.session()) {
                            events.push(Event::PtyExited(id));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Sends the captured keyboard input to the focused card.
    fn write_keys(&mut self, focused: Option<&str>) {
        let keys = std::mem::take(&mut self.keys);
        let Some(id) = focused else { return };
        if !self.runtimes.contains_key(id) && self.launcher != Launcher::Disabled {
            return;
        }
        let mode = self.mode_of(id);
        let app_cursor = mode.contains(TermMode::APP_CURSOR);
        let mut out = Vec::new();
        for (ev, mods) in keys {
            match ev {
                egui::Event::Text(t) => out.extend(input::text_bytes(&t, mods)),
                egui::Event::Paste(t) => out.extend(input::paste_bytes(&t, mode.contains(TermMode::BRACKETED_PASTE))),
                egui::Event::Copy => out.push(0x03),
                egui::Event::Cut => out.push(0x18),
                egui::Event::Key { key, pressed: true, modifiers, .. } => match input::clip_action(key, modifiers) {
                    Some(ClipAction::Paste) => {
                        if let Some(text) = self.clipboard.text() {
                            out.extend(input::paste_bytes(&text, mode.contains(TermMode::BRACKETED_PASTE)));
                        }
                    }
                    Some(ClipAction::Copy) => {}
                    None => out.extend(input::key_bytes(key, modifiers, app_cursor).unwrap_or_default()),
                },
                _ => {}
            }
        }
        self.send_to(id, out);
    }

    fn ensure_runtimes(&mut self, world: &World, cell: Vec2) {
        if self.launcher == Launcher::Disabled {
            return;
        }
        for card in world.cards.iter().filter(|c| !c.pty_dead) {
            if self.runtimes.contains_key(&card.id) || self.spawn_errors.contains_key(&card.id) {
                continue;
            }
            let launch = attach_command(&card.session(), card.cwd.as_deref());
            let size = grid_for(card.rect, cell);
            match TermRuntime::spawn(&card.id, &launch, size, cell_px(cell), self.tx.clone(), self.ctx.clone()) {
                Ok(rt) => {
                    self.runtimes.insert(card.id.clone(), rt);
                }
                Err(e) => {
                    self.spawn_errors.insert(card.id.clone(), (t().spawn_failed)(&launch.program, &e.to_string()));
                }
            }
        }
    }

    /// Draws all cards. Returns `true` if the wheel was used by a terminal.
    pub fn ui(&mut self, ui: &mut Ui, world: &mut World, dimmed: &dyn Fn(&World, &Card) -> bool, events: &mut Vec<Event>) -> bool {
        let vp = ui.max_rect();
        self.viewport = vp;
        let cell = base_cell(ui.ctx());
        self.ensure_runtimes(world, cell);
        let focused = world.focused.clone();
        self.write_keys(focused.as_deref());

        let mut wheel_taken = false;
        for i in world.cards_by_z() {
            if !world.shows_card(&world.cards[i]) {
                continue;
            }
            let dim = dimmed(world, &world.cards[i]);
            wheel_taken |= self.card_ui(ui, world, i, cell, dim, events);
        }
        // Drag interrupted (card closed, hidden…): don't leave a frame frozen.
        if world.dragging.is_some() && !ui.input(|i| i.pointer.primary_down()) {
            world.dragging = None;
        }
        wheel_taken
    }

    fn card_ui(&mut self, ui: &mut Ui, world: &mut World, i: usize, cell: Vec2, dim: bool, events: &mut Vec<Event>) -> bool {
        let vp = self.viewport;
        let cam = world.camera;
        let zoom = cam.zoom;
        let id = world.cards[i].id.clone();
        let grid = grid_for(world.cards[i].rect, cell);
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.resize(grid, cell_px(cell));
        }

        let screen = cam.rect_to_screen(world.cards[i].rect, vp);
        if !vp.intersects(screen) {
            return false;
        }
        let focused = world.focused.as_deref() == Some(id.as_str());
        let title_rect = Rect::from_min_size(screen.min, vec2(screen.width(), TITLE_H * zoom));
        let body = Rect::from_min_max(pos2(screen.min.x, title_rect.max.y), screen.max);
        let grip_size = (GRIP * zoom).max(8.0);
        let grip = Rect::from_min_size(screen.max - Vec2::splat(grip_size), Vec2::splat(grip_size));

        let eid = Id::unique(("card", &id));
        let title_r = ui.interact(title_rect, eid.with("title"), Sense::click_and_drag());
        // Close button, above the title bar (registered after: it wins the click).
        let close_rect = Rect::from_min_size(pos2(title_rect.max.x - title_rect.height(), title_rect.min.y), Vec2::splat(title_rect.height()));
        let close_r = (zoom >= CLOSE_MIN_ZOOM).then(|| {
            let r = ui.interact(close_rect, eid.with("close"), Sense::click()).on_hover_text(t().close_terminal_hint);
            let a11y = (t().close_terminal_a11y)(&world.cards[i].title());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &a11y));
            r
        });
        if close_r.as_ref().is_some_and(|r| r.clicked()) {
            events.push(Event::RequestClose(id.clone()));
        }
        let body_r = ui.interact(body, eid.with("body"), Sense::click_and_drag());
        let grip_r = ui.interact(grip, eid.with("grip"), Sense::drag());

        // --- Interactions -------------------------------------------------------------
        if title_r.dragged_by(PointerButton::Primary) {
            world.dragging = Some(id.clone());
            world.cards[i].rect = world.cards[i].rect.translate(title_r.drag_delta() / zoom);
            world.raise(&id);
            world.touch();
        }
        if title_r.drag_stopped() && world.dragging.as_ref() == Some(&id) {
            // Dropped in a visible frame: it becomes a member; elsewhere: outside any group.
            world.dragging = None;
            let target = world.drop_target(world.cards[i].rect.center());
            if target != world.cards[i].project {
                events.push(Event::AssignCard { card: id.clone(), project: target, relocate: false });
            }
            events.push(Event::SettleCard(id.clone()));
        }
        if title_r.double_clicked() {
            events.push(Event::FlyTo(id.clone()));
        } else if title_r.clicked() {
            events.push(Event::FocusCard(id.clone()));
        }
        title_r.context_menu(|ui| self.context_menu(ui, world, i, events));

        if grip_r.hovered() || grip_r.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeNwSe);
        }
        if grip_r.drag_stopped() {
            events.push(Event::SettleCard(id.clone()));
        }
        if grip_r.dragged() {
            let min = card_size(cell, MIN_COLS as f32, MIN_ROWS as f32);
            let r = &mut world.cards[i].rect;
            r.max += grip_r.drag_delta() / zoom;
            r.max = r.max.max(r.min + min);
            world.touch();
        }

        let alt = ui.input(|inp| inp.modifiers.alt);
        // Right-click = paste, as in Windows Terminal (Alt+right-click: tmux menu).
        let paste_click = body_r.secondary_clicked() && !alt && !is_preview(zoom);
        if body_r.clicked() && is_preview(zoom) {
            // A preview isn't usable: zoom in on it so the user can type.
            events.push(Event::FlyTo(id.clone()));
        } else if !focused && (body_r.clicked() || body_r.drag_started() || paste_click) {
            events.push(Event::FocusCard(id.clone()));
        }
        if paste_click {
            self.paste_clipboard(&id);
        }

        // --- Drawing ------------------------------------------------------------------
        let painter = ui.painter_at(vp);
        let radius = CornerRadius::same((6.0 * zoom).clamp(1.0, 8.0) as u8);
        painter.rect_filled(screen.translate(vec2(0.0, 4.0 * zoom)).expand(2.0 * zoom), radius, Color32::from_black_alpha(90));
        painter.rect_filled(screen, radius, theme::CARD_BG);
        let top_radius = CornerRadius { sw: 0, se: 0, ..radius };
        painter.rect_filled(title_rect, top_radius, theme::TITLE_BG);

        let card = &world.cards[i];
        let status = card.status();
        let title_font = FontId::proportional(13.0 * zoom);
        let title_painter = painter.with_clip_rect(title_rect.intersect(vp));
        let dot = pos2(title_rect.min.x + 14.0 * zoom, title_rect.center().y);
        title_painter.circle_filled(dot, 5.0 * zoom, theme::status_color(status));
        let label = match status {
            Some(s) => format!("{}  ·  {}", card.title(), t().status(s)),
            None => card.title(),
        };
        title_painter.text(pos2(dot.x + 12.0 * zoom, dot.y), Align2::LEFT_CENTER, label, title_font.clone(), theme::TEXT);
        let mut right = title_rect.max.x - 10.0 * zoom;
        if let Some(close_r) = &close_r {
            let hot = close_r.hovered();
            if hot {
                let r = CornerRadius { sw: 0, se: 0, nw: 0, ..top_radius };
                title_painter.rect_filled(close_rect, r, Color32::from_rgb(190, 60, 60));
            }
            let c = close_rect.center();
            let arm = close_rect.height() * 0.18;
            let stroke = Stroke::new((1.5 * zoom).max(1.0), if hot { Color32::WHITE } else { theme::TEXT_DIM });
            title_painter.line_segment([c - Vec2::splat(arm), c + Vec2::splat(arm)], stroke);
            title_painter.line_segment([c + vec2(-arm, arm), c + vec2(arm, -arm)], stroke);
            right = close_rect.min.x - 6.0 * zoom;
        }
        if let Some(cwd) = card.current_cwd() {
            title_painter.text(pos2(right, dot.y), Align2::RIGHT_CENTER, cwd, FontId::proportional(11.0 * zoom), theme::TEXT_DIM);
        }

        let font_px = quantize(BASE_FONT * zoom);
        let body_inner = body.shrink(PAD * zoom);
        let mut wheel_taken = false;
        if card.pty_dead || self.spawn_errors.contains_key(&id) {
            self.dead_overlay(ui, body, &id, zoom, events);
        } else if is_preview(zoom) || self.launcher == Launcher::Disabled && !self.runtimes.contains_key(&id) {
            paint_preview(&painter, card, body, zoom);
        } else if let Some(rt) = self.runtimes.get(&id) {
            let font = FontId::monospace(font_px);
            let cell_screen = ui.ctx().fonts_mut(|f| vec2(f.glyph_width(&font, 'M'), f.row_height(&font)));
            let metrics = render::GridMetrics { origin: body_inner.min, cell: cell_screen, font, clip: body_inner.intersect(vp) };
            render::paint_grid(&painter, &rt.term.lock(), &metrics, focused);
            // The wheel scrolls the hovered terminal, even without keyboard focus; a click goes to the
            // focused terminal, or to the one it targets (the first drag already selects).
            self.forward_mouse(ui, &id, &body_r, body_inner.min, cell_screen, focused);
        }
        // A readable terminal under the pointer keeps the whole wheel, including the tail of
        // smoothed scrolling in later frames: the canvas doesn't move behind it.
        if !is_preview(zoom) && body_r.contains_pointer() {
            wheel_taken = true;
        }

        // Border: focus, or a status calling for attention (pulsing).
        let card = &world.cards[i];
        if card.attention {
            let t = ui.input(|inp| inp.time) as f32;
            let alpha = 0.55 + 0.45 * (t * 4.0).sin().abs();
            painter.rect_stroke(screen, radius, Stroke::new(3.0, theme::status_color(status).gamma_multiply(alpha)), StrokeKind::Outside);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        } else if focused {
            painter.rect_stroke(screen, radius, Stroke::new(2.0, theme::FOCUS), StrokeKind::Outside);
        } else {
            painter.rect_stroke(screen, radius, Stroke::new(1.0, theme::CARD_BORDER), StrokeKind::Inside);
        }
        let grip_color = if grip_r.hovered() { theme::TEXT } else { theme::TEXT_DIM };
        for k in 1..=3 {
            let o = k as f32 * grip_size / 4.0;
            painter.line_segment([pos2(screen.max.x - o, screen.max.y - 2.0), pos2(screen.max.x - 2.0, screen.max.y - o)], Stroke::new(1.0, grip_color));
        }
        if dim {
            painter.rect_filled(screen.expand(3.0), radius, theme::CANVAS_BG.gamma_multiply(0.8));
        }
        wheel_taken
    }

    fn context_menu(&mut self, ui: &mut Ui, world: &World, i: usize, events: &mut Vec<Event>) {
        let card = &world.cards[i];
        if ui.button(t().rename_menu).clicked() {
            self.rename = Some((card.id.clone(), card.name.clone().unwrap_or_default()));
            ui.close();
        }
        if ui.button(t().stall_delay_menu).clicked() {
            self.stall = Some((card.id.clone(), card.stall_secs.to_string()));
            ui.close();
        }
        if ui.button(t().center_and_zoom).clicked() {
            events.push(Event::FlyTo(card.id.clone()));
            ui.close();
        }
        ui.menu_button(t().move_to, |ui| {
            for p in &world.projects {
                let here = card.project.as_ref() == Some(&p.id);
                let text = egui::RichText::new(&p.name).color(theme::rgb(p.color));
                if ui.add_enabled(!here, egui::Button::new(text)).clicked() {
                    events.push(Event::AssignCard { card: card.id.clone(), project: Some(p.id.clone()), relocate: true });
                    ui.close();
                }
            }
            if !world.projects.is_empty() {
                ui.separator();
            }
            if ui.add_enabled(card.project.is_some(), egui::Button::new(t().no_group)).clicked() {
                events.push(Event::AssignCard { card: card.id.clone(), project: None, relocate: true });
                ui.close();
            }
        });
        ui.separator();
        if ui.button(t().close_terminal_menu).clicked() {
            events.push(Event::RequestClose(card.id.clone()));
            ui.close();
        }
    }

    fn dead_overlay(&mut self, ui: &mut Ui, body: Rect, id: &CardId, zoom: f32, events: &mut Vec<Event>) {
        let painter = ui.painter_at(body);
        let msg = self.spawn_errors.get(id).cloned().unwrap_or_else(|| t().session_ended.into());
        let font = FontId::proportional((14.0 * zoom).max(6.0));
        painter.text(body.center() - vec2(0.0, 24.0 * zoom), Align2::CENTER_CENTER, msg, font, theme::TEXT_DIM);
        if zoom < 0.3 {
            return;
        }
        let size = vec2(110.0, 26.0);
        let left = Rect::from_center_size(body.center() + vec2(-60.0, 12.0 * zoom), size);
        let right = Rect::from_center_size(body.center() + vec2(60.0, 12.0 * zoom), size);
        if ui.put(left, egui::Button::new(t().respawn)).clicked() {
            events.push(Event::RespawnCard(id.clone()));
        }
        if ui.put(right, egui::Button::new(t().remove)).clicked() {
            events.push(Event::RemoveCard(id.clone()));
        }
    }

    /// Mouse → terminal (xterm reports if the application requests them, otherwise scrolling).
    fn forward_mouse(&mut self, ui: &Ui, id: &str, body: &egui::Response, origin: Pos2, cell: Vec2, focused: bool) {
        let Some(rt) = self.runtimes.get_mut(id) else { return };
        let mode = *rt.term.lock().mode();
        let mouse_mode = mode.intersects(TermMode::MOUSE_MODE);
        let sgr = mode.contains(TermMode::SGR_MOUSE);
        let size = rt.size();
        let cell_at = |p: Pos2| {
            let v = (p - origin) / cell;
            ((v.x.max(0.0) as usize).min(size.cols as usize - 1), (v.y.max(0.0) as usize).min(size.rows as usize - 1))
        };
        let mut out = Vec::new();
        for ev in ui.input(|i| i.events.clone()) {
            match ev {
                egui::Event::PointerButton { pos, button, pressed, modifiers, .. } if mouse_mode => {
                    let b = match button {
                        PointerButton::Primary => MouseButton::Left,
                        PointerButton::Middle => MouseButton::Middle,
                        PointerButton::Secondary => MouseButton::Right,
                        _ => continue,
                    };
                    // Plain right-click = paste (Alt + right-click goes to tmux: its menu). The
                    // release always follows the forwarded press.
                    let ours = b == MouseButton::Right && !modifiers.alt;
                    if pressed && ours {
                        continue;
                    }
                    // Shift + click belongs to the hand tool (panning the view), not to the terminal.
                    let aimed = focused || body.hovered();
                    if pressed && aimed && body.rect.contains(pos) && !modifiers.shift {
                        let (col, line) = cell_at(pos);
                        out.extend(input::mouse_report(b, modifiers, col, line, true, false, sgr));
                        rt.mouse_down = Some((b, col, line));
                    } else if !pressed && rt.mouse_down.is_some_and(|(d, ..)| d == b) {
                        let (col, line) = cell_at(pos);
                        out.extend(input::mouse_report(b, modifiers, col, line, false, false, sgr));
                        rt.mouse_down = None;
                    }
                }
                egui::Event::PointerMoved(pos) if mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) => {
                    if let Some((b, c0, l0)) = rt.mouse_down {
                        let (col, line) = cell_at(pos);
                        if (col, line) != (c0, l0) {
                            out.extend(input::mouse_report(b, Modifiers::NONE, col, line, true, true, sgr));
                            rt.mouse_down = Some((b, col, line));
                        }
                    }
                }
                egui::Event::MouseWheel { unit, delta, modifiers, .. } if body.hovered() && !modifiers.ctrl => {
                    let lines = match unit {
                        MouseWheelUnit::Line => delta.y,
                        MouseWheelUnit::Point => delta.y / cell.y,
                        MouseWheelUnit::Page => delta.y * size.rows as f32,
                    };
                    rt.wheel_acc += lines;
                    let n = rt.wheel_acc.trunc() as i32;
                    rt.wheel_acc -= n as f32;
                    if n == 0 {
                        continue;
                    }
                    let (col, line) = ui.input(|i| i.pointer.hover_pos()).map(cell_at).unwrap_or((0, 0));
                    for _ in 0..n.unsigned_abs() {
                        if mouse_mode {
                            let b = if n > 0 { MouseButton::WheelUp } else { MouseButton::WheelDown };
                            out.extend(input::mouse_report(b, modifiers, col, line, true, false, sgr));
                        } else if mode.contains(TermMode::ALT_SCREEN) {
                            let key = if n > 0 { Key::ArrowUp } else { Key::ArrowDown };
                            out.extend(input::key_bytes(key, Modifiers::NONE, mode.contains(TermMode::APP_CURSOR)).unwrap_or_default());
                        }
                    }
                    if !mouse_mode && !mode.contains(TermMode::ALT_SCREEN) {
                        rt.scroll_display(n);
                    }
                }
                _ => {}
            }
        }
        rt.write(out);
    }

    pub fn dialogs(&mut self, ctx: &egui::Context, world: &World, events: &mut Vec<Event>) {
        if let Some(id) = self.confirm_close.clone() {
            match world.card(&id) {
                None => self.confirm_close = None,
                Some(card) => {
                    let (title, body) = close_message(card);
                    match confirm(ctx, "close-card", &title, &body, t().close) {
                        Some(true) => {
                            events.push(Event::KillCard(id));
                            self.confirm_close = None;
                        }
                        Some(false) => self.confirm_close = None,
                        None => {}
                    }
                }
            }
        }
        if let Some((id, text)) = &mut self.rename {
            match text_prompt(ctx, "rename-card", t().terminal_name_prompt, text) {
                Some(true) => {
                    let name = Some(text.trim().to_string()).filter(|s| !s.is_empty());
                    events.push(Event::RenameCard(id.clone(), name));
                    self.rename = None;
                }
                Some(false) => self.rename = None,
                None => {}
            }
        }
        if let Some((id, text)) = &mut self.stall {
            match text_prompt(ctx, "stall-card", t().stall_prompt, text) {
                Some(true) => {
                    if let Ok(secs) = text.trim().parse::<u64>() {
                        events.push(Event::SetStall(id.clone(), secs.max(1)));
                    }
                    self.stall = None;
                }
                Some(false) => self.stall = None,
                None => {}
            }
        }
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, out: &mut Vec<Event>) {
        match ev {
            Event::NewCard { at, project } => {
                let cell = base_cell(&self.ctx);
                let size = card_size(cell, DEFAULT_COLS, DEFAULT_ROWS);
                // With no project given, the terminal is born in the open space.
                let project = project.as_deref().or(world.space.as_deref());
                let project = project.and_then(|p| world.project(p));
                // With no position: right of the group's terminals (or in the center of an empty group).
                let beside = project.map(|p| match world.members(&p.id).map(|c| c.rect).reduce(|a, b| a.union(b)) {
                    Some(r) => pos2(r.max.x + crate::shell::world::CARD_GAP + size.x / 2.0, r.min.y + size.y / 2.0),
                    None => p.rect.center(),
                });
                let near = at.or(beside).unwrap_or(world.camera.center);
                let rect = world.free_spot(near, size);
                let mut card = Card::new(rect, project.and_then(|p| p.cwd.clone()));
                card.project = project.map(|p| p.id.clone());
                let id = card.id.clone();
                world.add_card(card);
                // Always fly to the new terminal: visible and large enough to type in.
                // First make room (nothing overlaps), then fly there.
                out.push(Event::SettleCard(id.clone()));
                out.push(Event::FlyTo(id));
            }
            Event::FocusCard(id) => {
                if let Some(card) = world.card_mut(id) {
                    card.attention = false;
                    world.focused = Some(id.clone());
                    world.raise(id);
                }
            }
            Event::Unfocus => world.focused = None,
            Event::RenameCard(id, name) => {
                if let Some(card) = world.card_mut(id) {
                    card.name = name.clone();
                    world.touch();
                }
            }
            Event::SetStall(id, secs) => {
                if let Some(card) = world.card_mut(id) {
                    card.stall_secs = *secs;
                    world.touch();
                }
            }
            Event::RequestClose(id) => {
                if world.card(id).is_some() {
                    self.confirm_close = Some(id.clone());
                }
            }
            Event::KillCard(id) => {
                if let Some(card) = world.card(id) {
                    let session = card.session();
                    self.killed.insert(session.clone());
                    self.runtimes.remove(id);
                    if self.launcher == Launcher::Tmux {
                        pty::kill_session_in_background(session);
                    }
                }
                world.remove_card(id);
            }
            Event::RemoveCard(id) => {
                self.runtimes.remove(id);
                self.spawn_errors.remove(id);
                world.remove_card(id);
            }
            Event::RespawnCard(id) => {
                self.spawn_errors.remove(id);
                if let Some(card) = world.card_mut(id) {
                    card.pty_dead = false;
                }
            }
            Event::Agent(AgentEvent::Session(info)) => {
                if let Some(card) = world.cards.iter_mut().find(|c| c.session() == info.session) {
                    if info.cwd.is_some() && card.cwd != info.cwd {
                        card.cwd = info.cwd.clone();
                        world.revision += 1;
                    }
                    card.info = Some(info.clone());
                } else if let Some(id) = info.session.strip_prefix(SESSION_PREFIX).filter(|_| !self.killed.contains(&info.session)) {
                    // tmux session without a card (lost layout, created by hand…): adopt it.
                    let cell = base_cell(&self.ctx);
                    let rect = world.free_spot(world.camera.center, card_size(cell, DEFAULT_COLS, DEFAULT_ROWS));
                    let mut card = Card::with_id(id.to_string(), rect, info.cwd.clone());
                    card.info = Some(info.clone());
                    world.add_card(card);
                }
            }
            Event::Agent(AgentEvent::Gone { session }) => {
                if let Some(card) = world.cards.iter_mut().find(|c| &c.session() == session) {
                    card.info = None;
                }
            }
            _ => {}
        }
    }
}

/// Confirmation text: quote the running command, since that is what would be lost.
pub fn close_message(card: &Card) -> (String, String) {
    let title = (t().close_card_title)(&card.title());
    let running = card.info.as_ref().and_then(|i| i.command.as_deref());
    let body = match running {
        Some(cmd) => (t().close_card_running)(cmd),
        None => t().close_card_body.to_string(),
    };
    (title, body)
}

fn cell_px(cell: Vec2) -> (u16, u16) {
    (cell.x.round().max(1.0) as u16, cell.y.round().max(1.0) as u16)
}

/// At this zoom, the card is drawn as a preview (title + status) instead of the terminal.
pub fn is_preview(zoom: f32) -> bool {
    quantize(BASE_FONT * zoom) < LOD_FONT_PX
}

/// Font sizes in quarter points: limits the number of variants in the glyph atlas.
fn quantize(px: f32) -> f32 {
    (px * 4.0).round() / 4.0
}

/// Zoomed-out view: large title, status and last lines.
fn paint_preview(painter: &egui::Painter, card: &Card, body: Rect, zoom: f32) {
    let painter = painter.with_clip_rect(body);
    let status = card.status();
    let big = (body.height() * 0.16).clamp(6.0, 48.0);
    painter.text(body.center() - vec2(0.0, big * 0.6), Align2::CENTER_CENTER, card.title(), FontId::proportional(big), theme::TEXT);
    if let Some(s) = status {
        painter.text(body.center() + vec2(0.0, big * 0.5), Align2::CENTER_CENTER, t().status(s), FontId::proportional(big * 0.6), theme::status_color(status));
    }
    let tail_px = 11.0 * zoom.max(0.0) * 2.0;
    if tail_px >= 6.0
        && let Some(info) = &card.info {
            let font = FontId::monospace(tail_px.min(14.0));
            for (k, line) in info.tail.iter().rev().enumerate() {
                let y = body.max.y - 6.0 - (k as f32 + 1.0) * tail_px * 1.3;
                painter.text(pos2(body.min.x + 8.0, y), Align2::LEFT_TOP, line, font.clone(), theme::TEXT_DIM);
            }
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: Vec2 = Vec2::new(8.0, 16.0);

    #[test]
    fn default_card_holds_default_grid() {
        let size = card_size(CELL, DEFAULT_COLS, DEFAULT_ROWS);
        let g = grid_for(Rect::from_min_size(Pos2::ZERO, size), CELL);
        assert_eq!(g, GridSize { cols: 120, rows: 34 });
    }

    #[test]
    fn grid_never_below_minimum() {
        let g = grid_for(Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0)), CELL);
        assert_eq!(g, GridSize { cols: MIN_COLS, rows: MIN_ROWS });
    }

    #[test]
    fn grid_ignores_partial_cells() {
        let size = card_size(CELL, 80.0, 24.0) + vec2(7.9, 15.9);
        let g = grid_for(Rect::from_min_size(pos2(-500.0, 300.0), size), CELL);
        assert_eq!(g, GridSize { cols: 80, rows: 24 });
    }

    #[test]
    fn close_message_names_the_running_command() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let mut card = Card::new(Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)), None);
        card.name = Some("api".into());
        let (title, body) = close_message(&card);
        assert_eq!(title, "Fermer « api » ?");
        assert!(!body.contains("en cours"));
        card.info = Some(protocol::SessionInfo {
            session: card.session(),
            status: protocol::TermStatus::Running,
            command: Some("cargo run".into()),
            cwd: None,
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
        });
        assert!(close_message(&card).1.starts_with("« cargo run » est en cours"));
    }

    #[test]
    fn preview_threshold() {
        assert!(is_preview(0.369));
        assert!(!is_preview(0.6));
        assert!(!is_preview(1.0));
    }

    #[test]
    fn font_sizes_are_quantized() {
        assert_eq!(quantize(14.0 * 0.333), 4.75);
        assert_eq!(quantize(14.0), 14.0);
    }
}
