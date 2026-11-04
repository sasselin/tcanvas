//! Minimal glue between slices: draw order, keyboard routing, event bus.

pub mod event;
pub mod fonts;
pub mod i18n;
pub mod launch;
pub mod theme;
pub mod widgets;
pub mod world;

use std::collections::VecDeque;
use std::path::PathBuf;

use egui::{Align, Align2, CentralPanel, FontId, Frame, Id, Key, Layout, Panel, RichText, Ui};

use crate::features::agent_link::{AgentLink, AgentMode};
use crate::features::canvas::Canvas;
use crate::features::changes::Changes;
use crate::features::filters::{self, Filters};
use crate::features::jump::Jump;
use crate::features::notifications::Notifications;
use crate::features::persistence::{self, Persistence};
use crate::features::projects::Projects;
use crate::features::spaces::Spaces;
use crate::features::terminal_card::{Launcher, TerminalCards};
use crate::features::wires::Wires;
use event::Event;
use i18n::t;
use world::{Card, World};

pub struct Config {
    pub launcher: Launcher,
    pub agent: AgentMode,
    pub config_dir: PathBuf,
    pub os_toasts: bool,
}

impl Config {
    pub fn production() -> Self {
        Self { launcher: Launcher::Tmux, agent: AgentMode::Spawn, config_dir: persistence::default_dir(), os_toasts: true }
    }
}

/// Global shortcuts: always for the app, even when a terminal has focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shortcut {
    Jump,
    NewCard,
    Search,
    FitAll,
    /// Project #1 to #9 (creation order).
    Project(usize),
    CloseCard,
}

const DIGITS: [Key; 10] = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];

fn shortcut_of(ev: &egui::Event) -> Option<Shortcut> {
    let egui::Event::Key { key, physical_key, pressed: true, modifiers, .. } = ev else { return None };
    if !(modifiers.ctrl && modifiers.shift && !modifiers.alt) {
        return None;
    }
    // Digits: physical key, so Ctrl+Shift+1 also works on AZERTY (where 1 is on "&").
    let digit = [physical_key.as_ref(), Some(key)].into_iter().flatten().find_map(|k| DIGITS.iter().position(|d| d == k));
    if let Some(n) = digit {
        return Some(if n == 0 { Shortcut::FitAll } else { Shortcut::Project(n) });
    }
    Some(match key {
        Key::K => Shortcut::Jump,
        Key::N => Shortcut::NewCard,
        Key::F => Shortcut::Search,
        Key::W => Shortcut::CloseCard,
        _ => return None,
    })
}

pub struct App {
    pub world: World,
    canvas: Canvas,
    pub cards: TerminalCards,
    pub changes: Changes,
    projects: Projects,
    spaces: Spaces,
    wires: Wires,
    pub filters: Filters,
    jump: Jump,
    notifications: Notifications,
    agent: AgentLink,
    persistence: Persistence,
    shortcuts: Vec<Shortcut>,
}

impl App {
    pub fn new(ctx: &egui::Context, config: Config) -> Self {
        fonts::install(ctx);
        ctx.set_visuals(egui::Visuals::dark());
        // Ctrl+plus/minus must reach the terminal, not zoom the UI.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let mut world = World::default();
        let mut persistence = Persistence::new(&config.config_dir);
        persistence.load(&mut world);
        Self {
            world,
            canvas: Canvas::default(),
            cards: TerminalCards::new(ctx.clone(), config.launcher),
            changes: Changes::default(),
            projects: Projects::default(),
            spaces: Spaces::default(),
            wires: Wires,
            filters: Filters::default(),
            jump: Jump::default(),
            notifications: Notifications::new(ctx.clone(), config.os_toasts),
            agent: AgentLink::new(ctx.clone(), config.agent),
            persistence,
            shortcuts: Vec::new(),
        }
    }

    /// Canvas area on screen (below the toolbar).
    pub fn viewport(&self) -> egui::Rect {
        self.canvas.viewport()
    }

    pub fn pending_toasts(&self) -> usize {
        self.notifications.pending()
    }

    pub fn projects_editing(&self) -> bool {
        self.projects.has_dialog()
    }

    pub fn jump_open(&self) -> bool {
        self.jump.is_open()
    }

    /// Commands addressed to the agent while it is disabled (tests).
    pub fn agent_sent(&self) -> &[protocol::AgentCmd] {
        self.agent.sent()
    }

    pub fn agent_state(&self) -> &crate::features::agent_link::LinkState {
        self.agent.state()
    }

    fn typing_elsewhere(&self, ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.focused().is_some()) || self.jump.is_open() || self.cards.has_dialog() || self.projects.has_dialog()
    }

    /// Removes global shortcuts, then gives the rest of the keyboard input to the focused terminal.
    fn route_input(&mut self, events: &mut Vec<egui::Event>, typing_elsewhere: bool) {
        events.retain(|ev| match shortcut_of(ev) {
            Some(s) => {
                self.shortcuts.push(s);
                false
            }
            None => true,
        });
        if self.world.focused.is_some() && !typing_elsewhere {
            self.cards.capture_keyboard(events);
        }
    }

    fn run_shortcuts(&mut self, ctx: &egui::Context, events: &mut Vec<Event>) {
        for s in std::mem::take(&mut self.shortcuts) {
            match s {
                Shortcut::Jump => self.jump.open(),
                Shortcut::NewCard => events.push(Event::NewCard { at: None, project: None }),
                Shortcut::Search => {
                    events.push(Event::Unfocus);
                    ctx.memory_mut(|m| m.request_focus(Id::unique(filters::SEARCH_ID)));
                }
                Shortcut::FitAll => {
                    if let Some(r) = self.filters.fit_target(&self.world) {
                        events.push(Event::FitRect(r));
                    }
                }
                Shortcut::CloseCard => {
                    if let Some(id) = self.world.focused.clone() {
                        events.push(Event::RequestClose(id));
                    }
                }
                Shortcut::Project(n) => {
                    if let Some(p) = self.world.projects.get(n - 1) {
                        events.push(Event::EnterSpace(Some(p.id.clone())));
                    }
                }
            }
        }
    }

    /// One full frame. `eframe::App::ui` and the tests go through here.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        // In the app, `raw_input_hook` has already done this; this call covers
        // environments that don't call it (tests) and finds nothing otherwise.
        let typing = self.typing_elsewhere(&ctx);
        let mut pending = ctx.input_mut(|i| std::mem::take(&mut i.events));
        self.route_input(&mut pending, typing);
        ctx.input_mut(|i| i.events = pending);

        self.world.window_focused = ctx.input(|i| i.focused);
        let mut events = Vec::new();
        self.run_shortcuts(&ctx, &mut events);
        self.agent.poll(&self.world, &mut events);
        self.cards.poll(&mut self.world, &mut events);
        self.changes.poll(&self.world, &mut events);
        self.notifications.poll(&ctx, &mut events);

        // Two fixed rows: actions and projects, then search and filters. No centered
        // layout or automatic wrapping: the bar keeps its height no matter what.
        Panel::top("toolbar").resizable(false).show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                Filters::actions_ui(ui, &mut events);
                ui.separator();
                let vp = self.canvas.viewport();
                self.spaces.ui(ui, &self.world, vp, &mut events);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.agent.status_ui(ui);
                    self.changes.toolbar_ui(ui, &self.world, &mut events);
                    if let Some(err) = &self.persistence.last_error {
                        ui.label(RichText::new(t().layout_warning).color(theme::status_color(Some(protocol::TermStatus::Exited(1))))).on_hover_text(err);
                    }
                });
            });
            ui.horizontal(|ui| self.filters.ui(ui, &self.world, &mut events));
            ui.add_space(2.0);
        });

        self.changes.panel_ui(ui, &self.world, &mut events);

        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            self.world.refit_projects();
            self.canvas.background(ui, &mut self.world, &mut events);
            let vp = self.canvas.viewport();
            self.projects.ui(ui, &mut self.world, vp, &mut events);
            self.wires.ui(ui, &self.world, vp, &mut events);
            let filters = &self.filters;
            let dimmed = |w: &World, c: &Card| filters.is_active() && !filters.matches(w, c);
            let wheel_taken = self.cards.ui(ui, &mut self.world, &dimmed, &mut events);
            self.canvas.minimap(ui, &mut self.world);
            self.canvas.hand_tool(ui, &mut self.world);
            self.canvas.navigate(ui, &mut self.world, wheel_taken);
            self.canvas.animate(ui, &mut self.world, &mut events);
            if self.world.cards.is_empty() && self.world.projects.is_empty() {
                ui.painter().text(
                    vp.center(),
                    Align2::CENTER_CENTER,
                    t().empty_canvas,
                    FontId::proportional(18.0),
                    theme::TEXT_DIM,
                );
            }
        });

        self.jump.ui(&ctx, &self.world, &mut events);
        self.cards.dialogs(&ctx, &self.world, &mut events);
        self.projects.dialogs(&ctx, &self.world, &mut events);
        self.notifications.ui(&ctx, &self.world, &mut events);
        self.dispatch(events);

        self.persistence.tick(&self.world);
        if self.persistence.wants_repaint() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
    }

    /// Dispatches events to all slices, including the ones they emit in return.
    pub fn dispatch(&mut self, events: Vec<Event>) {
        let mut queue: VecDeque<Event> = events.into();
        let mut budget = 10_000;
        while let Some(ev) = queue.pop_front() {
            budget -= 1;
            if budget == 0 {
                eprintln!("event loop cut short at {ev:?}");
                break;
            }
            let mut out = Vec::new();
            let w = &mut self.world;
            self.cards.handle(&ev, w, &mut out);
            self.projects.handle(&ev, w, &mut out);
            self.changes.handle(&ev, w, &mut out);
            // Before the camera: going to a hidden terminal opens its space first.
            self.spaces.handle(&ev, w, &mut out);
            self.wires.handle(&ev, w, &mut out);
            self.canvas.handle(&ev, w, &mut out);
            self.agent.handle(&ev, w, &mut out);
            self.notifications.handle(&ev, w, &mut out);
            queue.extend(out);
        }
        self.world.refit_projects();
    }

    pub fn save_now(&mut self) {
        self.persistence.save_now(&self.world);
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let typing = self.typing_elsewhere(ctx);
        self.route_input(&mut raw_input.events, typing);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn on_exit(&mut self) {
        self.save_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Modifiers;

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn shortcuts_need_ctrl_shift_and_no_alt() {
        let cs = Modifiers::CTRL | Modifiers::SHIFT;
        assert_eq!(shortcut_of(&key(Key::K, cs)), Some(Shortcut::Jump));
        assert_eq!(shortcut_of(&key(Key::N, cs)), Some(Shortcut::NewCard));
        assert_eq!(shortcut_of(&key(Key::K, Modifiers::CTRL)), None, "Ctrl+K stays with the shell");
        assert_eq!(shortcut_of(&key(Key::K, cs | Modifiers::ALT)), None);
        assert_eq!(shortcut_of(&egui::Event::Text("K".into())), None);
    }

    #[test]
    fn digit_shortcuts_use_the_physical_key() {
        let cs = Modifiers::CTRL | Modifiers::SHIFT;
        assert_eq!(shortcut_of(&key(Key::Num3, cs)), Some(Shortcut::Project(3)));
        assert_eq!(shortcut_of(&key(Key::Num0, cs)), Some(Shortcut::FitAll));
        // AZERTY: the "&" key produces a logical key with no digit, but the physical key is 1.
        let azerty = egui::Event::Key { key: Key::Exclamationmark, physical_key: Some(Key::Num1), pressed: true, repeat: false, modifiers: cs };
        assert_eq!(shortcut_of(&azerty), Some(Shortcut::Project(1)));
        assert_eq!(shortcut_of(&key(Key::Num1, Modifiers::CTRL)), None);
    }
}
