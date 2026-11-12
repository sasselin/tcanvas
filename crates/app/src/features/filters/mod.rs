//! Top bar: creation, search, status filters (within the open space).
//! Non-matching cards are dimmed, never hidden: you keep your bearings.
//! Enabling a filter frames the view on the matches; removing it restores the previous view.
//! Status chips are always all there (disabled at 0): the bar never shifts under
//! the mouse, even when statuses keep changing.

use std::collections::BTreeSet;

use egui::{Id, Rect, RichText, TextEdit, Ui};
use protocol::TermStatus;

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::{Camera, Card, ProjectId, World};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum StatusKind {
    Stalled,
    WaitingInput,
    Failed,
    Running,
    Idle,
    Dead,
}

impl StatusKind {
    pub const ALL: [StatusKind; 6] = [StatusKind::Stalled, StatusKind::WaitingInput, StatusKind::Failed, StatusKind::Running, StatusKind::Idle, StatusKind::Dead];

    pub fn of(s: Option<TermStatus>) -> Option<Self> {
        Some(match s? {
            TermStatus::Idle => StatusKind::Idle,
            TermStatus::Running => StatusKind::Running,
            TermStatus::Stalled => StatusKind::Stalled,
            TermStatus::WaitingInput => StatusKind::WaitingInput,
            TermStatus::Exited(_) => StatusKind::Failed,
            TermStatus::Dead => StatusKind::Dead,
        })
    }

    fn sample(self) -> TermStatus {
        match self {
            StatusKind::Stalled => TermStatus::Stalled,
            StatusKind::WaitingInput => TermStatus::WaitingInput,
            StatusKind::Failed => TermStatus::Exited(1),
            StatusKind::Running => TermStatus::Running,
            StatusKind::Idle => TermStatus::Idle,
            StatusKind::Dead => TermStatus::Dead,
        }
    }
}

pub const SEARCH_ID: &str = "filters-search";

#[derive(Default)]
pub struct Filters {
    pub text: String,
    pub statuses: BTreeSet<StatusKind>,
    /// View (and space) before the filter was enabled, restored when it is removed.
    saved: Option<(Option<ProjectId>, Camera)>,
}

/// Number of terminals of the open space in each status (order of `StatusKind::ALL`).
pub fn counts(world: &World) -> [usize; 6] {
    let mut n = [0; 6];
    for c in world.cards.iter().filter(|c| world.shows_card(c)) {
        if let Some(k) = StatusKind::of(c.status()) {
            n[StatusKind::ALL.iter().position(|x| *x == k).expect("known status")] += 1;
        }
    }
    n
}

impl Filters {
    /// Click on a chip: keep only that status (click again: remove it). With Ctrl: add it to
    /// or remove it from the statuses already chosen.
    pub fn click_status(&mut self, kind: StatusKind, additive: bool) {
        if additive || self.statuses.contains(&kind) {
            toggle(&mut self.statuses, kind);
        } else {
            self.statuses = BTreeSet::from([kind]);
        }
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.statuses.clear();
    }

    /// After a filter change: remember the view on activation, frame the matches
    /// (`fit`), and restore the previous view once nothing is filtered anymore.
    pub fn after_change(&mut self, was_active: bool, fit: bool, world: &World, events: &mut Vec<Event>) {
        let active = self.is_active();
        if active && !was_active {
            self.saved = Some((world.space.clone(), world.camera));
        }
        if active {
            let matching = world.cards.iter().filter(|c| world.shows_card(c) && self.matches(world, c)).map(|c| c.rect).reduce(|a, b| a.union(b));
            if let Some(r) = matching.filter(|_| fit) {
                events.push(Event::FitRect(r));
            }
        } else if let Some((space, cam)) = self.saved.take()
            && space == world.space
        {
            events.push(Event::FlyCamera(cam));
        }
    }

    pub fn is_active(&self) -> bool {
        !self.text.trim().is_empty() || !self.statuses.is_empty()
    }

    /// Whether `card` is drawn dimmed. The focused card never is: you are working in it.
    pub fn dims(&self, world: &World, card: &Card) -> bool {
        self.is_active() && world.focused.as_ref() != Some(&card.id) && !self.matches(world, card)
    }

    pub fn matches(&self, world: &World, card: &Card) -> bool {
        let project = world.project_of(card);
        if !self.statuses.is_empty() && !StatusKind::of(card.status()).is_some_and(|s| self.statuses.contains(&s)) {
            return false;
        }
        let query = self.text.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let haystack = [Some(card.title()), card.current_cwd().map(str::to_string), card.info.as_ref().and_then(|i| i.command.clone()), project.map(|p| p.name.clone())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        query.split_whitespace().all(|word| haystack.contains(word))
    }

    /// Area to frame for "Show all": the space's matching cards, otherwise the whole space.
    pub fn fit_target(&self, world: &World) -> Option<Rect> {
        let matching = world.cards.iter().filter(|c| world.shows_card(c) && self.matches(world, c)).map(|c| c.rect).reduce(|a, b| a.union(b));
        match matching {
            Some(r) if self.is_active() => Some(r),
            _ => crate::features::spaces::space_bounds(world),
        }
    }

    /// Creation buttons (first row of the bar).
    pub fn actions_ui(ui: &mut Ui, events: &mut Vec<Event>) {
        if ui.button(t().new_terminal_button).on_hover_text(t().new_terminal_hint).clicked() {
            events.push(Event::NewCard { at: None, project: None, command: None, setup: None, cwd: None });
        }
        if ui.button(t().new_project_button).clicked() {
            events.push(Event::NewProject { at: None });
        }
    }

    /// Search, status filters, "Show all" (second row of the bar).
    pub fn ui(&mut self, ui: &mut Ui, world: &World, events: &mut Vec<Event>) {
        let was_active = self.is_active();
        {
            let search = ui.add(TextEdit::singleline(&mut self.text).id(Id::unique(SEARCH_ID)).hint_text(t().filter_hint).desired_width(200.0));
            // While typing we only dim; Enter frames the matches.
            let mut fit = search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let mut changed = search.changed() || fit;

            let n = counts(world);
            for (k, kind) in StatusKind::ALL.into_iter().enumerate() {
                let on = self.statuses.contains(&kind);
                let status = kind.sample();
                let color = theme::status_color(Some(status));
                let text = RichText::new(format!("{} {}", t().status(status), n[k])).color(if on { theme::CARD_BG } else { color });
                let chip = egui::Button::selectable(on, text).fill(if on { color } else { egui::Color32::TRANSPARENT });
                let resp = ui.push_id(("status-chip", k), |ui| ui.add_enabled(on || n[k] > 0, chip)).inner;
                let resp = resp.on_hover_text(t().status_chip_hint);
                if resp.clicked() {
                    self.click_status(kind, ui.input(|i| i.modifiers.command));
                    changed = true;
                    fit = true;
                }
            }

            // Always present (disabled without a filter): nothing shifts when filtering.
            let shown = world.cards.iter().filter(|c| world.shows_card(c)).count();
            let matching = world.cards.iter().filter(|c| world.shows_card(c) && self.matches(world, c)).count();
            let clear_label = if self.is_active() { (t().clear_filter_count)(matching, shown) } else { t().clear_filter.into() };
            if ui.add_enabled(self.is_active(), egui::Button::new(clear_label)).on_hover_text(t().clear_filter_hint).clicked() {
                self.clear();
                changed = true;
            }
            if changed {
                self.after_change(was_active, fit, world, events);
            }
            ui.separator();

            if ui.button(t().fit_all).on_hover_text(t().fit_all_hint).clicked()
                && let Some(r) = self.fit_target(world)
            {
                events.push(Event::FitRect(r));
            }
            let attention: Vec<&Card> = world.cards.iter().filter(|c| c.attention).collect();
            if !attention.is_empty() {
                let label = RichText::new(format!("🔔 {}", attention.len())).color(theme::status_color(Some(TermStatus::Stalled)));
                if ui.button(label).on_hover_text(t().attention_hint).clicked() {
                    events.push(Event::FlyTo(attention[0].id.clone()));
                }
            }
        }
    }
}

fn toggle<T: Ord>(set: &mut BTreeSet<T>, v: T) {
    if !set.remove(&v) {
        set.insert(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::world::Project;
    use egui::{Pos2, Vec2, pos2};
    use protocol::SessionInfo;

    fn world() -> World {
        let mut w = World::default();
        w.projects.push(Project {
            id: "api".into(),
            name: "Backend API".into(),
            color: [0; 3],
            rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)),
            cwd: None,
            templates: Vec::new(),
            messages: Vec::new(),
            worktree_location: None,
        });
        let mut a = Card::new(Rect::from_min_size(pos2(10.0, 10.0), Vec2::splat(100.0)), Some("/srv/api".into()));
        a.info = Some(SessionInfo {
            session: a.session(),
            status: TermStatus::Stalled,
            command: Some("cargo watch -x run".into()),
            cwd: Some("/srv/api".into()),
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
            agent_session_id: None,
            agent_transcript: None,
            last_prompt: None,
        });
        a.name = Some("server".into());
        a.project = Some("api".into());
        let b = Card::new(Rect::from_min_size(pos2(5000.0, 10.0), Vec2::splat(100.0)), Some("/home/me/blog".into()));
        w.add_card(a);
        w.add_card(b);
        w
    }

    fn matching(f: &Filters, w: &World) -> Vec<usize> {
        w.cards.iter().enumerate().filter(|(_, c)| f.matches(w, c)).map(|(i, _)| i).collect()
    }

    #[test]
    fn the_focused_card_is_never_dimmed() {
        let mut w = world();
        let mut f = Filters::default();
        assert!(!f.dims(&w, &w.cards[1]), "no filter, nothing dimmed");
        f.statuses.insert(StatusKind::WaitingInput);
        assert!(f.dims(&w, &w.cards[1]));
        w.focused = Some(w.cards[1].id.clone());
        assert!(!f.dims(&w, &w.cards[1]));
        assert!(f.dims(&w, &w.cards[0]), "other cards still follow the filter");
    }

    #[test]
    fn no_filter_matches_everything() {
        let w = world();
        let f = Filters::default();
        assert!(!f.is_active());
        assert_eq!(matching(&f, &w), vec![0, 1]);
    }

    #[test]
    fn text_matches_title_cwd_command_and_project_words() {
        let w = world();
        for (q, expected) in [("server", vec![0]), ("CARGO watch", vec![0]), ("backend", vec![0]), ("blog", vec![1]), ("api blog", vec![]), ("  ", vec![0, 1])] {
            let f = Filters { text: q.into(), ..Default::default() };
            assert_eq!(matching(&f, &w), expected, "query {q:?}");
        }
    }

    #[test]
    fn text_and_status_filters_combine() {
        let w = world();
        let mut f = Filters { text: "backend".into(), ..Default::default() };
        assert_eq!(matching(&f, &w), vec![0]);
        f.statuses.insert(StatusKind::Running);
        assert_eq!(matching(&f, &w), Vec::<usize>::new());
        f.statuses.insert(StatusKind::Stalled);
        assert_eq!(matching(&f, &w), vec![0]);
    }

    #[test]
    fn unknown_status_never_matches_a_status_filter() {
        let w = world();
        let f = Filters { statuses: [StatusKind::Idle].into(), ..Default::default() };
        assert_eq!(matching(&f, &w), Vec::<usize>::new());
    }

    #[test]
    fn fit_target_prefers_matches() {
        let w = world();
        let f = Filters { text: "blog".into(), ..Default::default() };
        assert_eq!(f.fit_target(&w), Some(w.cards[1].rect));
        let all = Filters::default().fit_target(&w).unwrap();
        assert!(all.contains_rect(w.cards[0].rect) && all.contains_rect(w.cards[1].rect) && all.contains_rect(w.projects[0].rect));
        assert_eq!(Filters::default().fit_target(&World::default()), None);
    }

    #[test]
    fn fit_target_stays_in_the_open_space() {
        let mut w = world();
        w.space = Some("api".into());
        assert_eq!(Filters::default().fit_target(&w), Some(w.projects[0].rect), "the terminal outside any project is ignored");
        let f = Filters { text: "blog".into(), ..Default::default() };
        assert_eq!(f.fit_target(&w), Some(w.projects[0].rect), "no visible match: the whole space");
    }

    #[test]
    fn a_click_keeps_only_that_status_and_ctrl_combines() {
        let mut f = Filters::default();
        f.click_status(StatusKind::Stalled, false);
        f.click_status(StatusKind::Running, false);
        assert_eq!(f.statuses, BTreeSet::from([StatusKind::Running]), "a click replaces");
        f.click_status(StatusKind::Idle, true);
        assert_eq!(f.statuses, BTreeSet::from([StatusKind::Running, StatusKind::Idle]), "Ctrl+click combines");
        f.click_status(StatusKind::Idle, false);
        f.click_status(StatusKind::Running, false);
        assert!(f.statuses.is_empty(), "clicking again removes");
    }

    #[test]
    fn counts_only_what_the_open_space_shows() {
        let mut w = world();
        let stalled = StatusKind::ALL.iter().position(|k| *k == StatusKind::Stalled).unwrap();
        assert_eq!(counts(&w)[stalled], 1);
        w.projects.push(Project { id: "other".into(), name: "other".into(), color: [0; 3], rect: Rect::NOTHING, cwd: None, templates: Vec::new(), messages: Vec::new(), worktree_location: None });
        w.space = Some("other".into());
        assert_eq!(counts(&w), [0; 6], "the stalled terminal is in another space");
    }

    #[test]
    fn filtering_frames_matches_and_clearing_restores_the_view() {
        let mut w = world();
        let before = Camera { center: pos2(-777.0, 42.0), zoom: 0.33 };
        w.camera = before;
        let mut f = Filters::default();
        let mut ev = vec![];
        f.click_status(StatusKind::Stalled, false);
        f.after_change(false, true, &w, &mut ev);
        assert!(matches!(ev.as_slice(), [Event::FitRect(r)] if *r == w.cards[0].rect), "{ev:?}");
        // The view moved during the filter; removing it restores the earlier one.
        w.camera = Camera { center: pos2(1.0, 1.0), zoom: 1.0 };
        let mut ev = vec![];
        f.clear();
        f.after_change(true, false, &w, &mut ev);
        assert!(matches!(ev.as_slice(), [Event::FlyCamera(c)] if *c == before), "{ev:?}");
        // No remembered view anymore: a second clear does nothing.
        let mut ev = vec![];
        f.after_change(false, false, &w, &mut ev);
        assert!(ev.is_empty());
    }

    #[test]
    fn the_saved_view_is_not_restored_in_another_space() {
        let mut w = world();
        let mut f = Filters::default();
        f.click_status(StatusKind::Stalled, false);
        f.after_change(false, true, &w, &mut vec![]);
        w.space = Some("api".into());
        f.clear();
        let mut ev = vec![];
        f.after_change(true, false, &w, &mut ev);
        assert!(ev.is_empty(), "the remembered view belonged to 'All'");
    }

    #[test]
    fn status_kinds() {
        assert_eq!(StatusKind::of(Some(TermStatus::Exited(2))), Some(StatusKind::Failed));
        assert_eq!(StatusKind::of(None), None);
        for k in StatusKind::ALL {
            assert_eq!(StatusKind::of(Some(k.sample())), Some(k));
        }
    }
}
