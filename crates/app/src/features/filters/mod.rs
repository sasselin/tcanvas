//! Barre du haut : création, recherche, filtres par statut (dans l'espace ouvert).
//! Les cartes qui ne correspondent pas sont estompées, jamais cachées : on garde ses repères.

use std::collections::BTreeSet;

use egui::{Id, Rect, RichText, TextEdit, Ui};
use protocol::TermStatus;

use crate::shell::event::Event;
use crate::shell::theme;
use crate::shell::world::{Card, World};

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
    pub const ALL: [StatusKind; 6] =
        [StatusKind::Stalled, StatusKind::WaitingInput, StatusKind::Failed, StatusKind::Running, StatusKind::Idle, StatusKind::Dead];

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
}

impl Filters {
    pub fn is_active(&self) -> bool {
        !self.text.trim().is_empty() || !self.statuses.is_empty()
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
        let haystack = [
            Some(card.title()),
            card.current_cwd().map(str::to_string),
            card.info.as_ref().and_then(|i| i.command.clone()),
            project.map(|p| p.name.clone()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
        query.split_whitespace().all(|word| haystack.contains(word))
    }

    /// Zone à cadrer pour « Tout voir » : les cartes de l'espace qui correspondent, sinon tout l'espace.
    pub fn fit_target(&self, world: &World) -> Option<Rect> {
        let matching = world.cards.iter().filter(|c| world.shows_card(c) && self.matches(world, c)).map(|c| c.rect).reduce(|a, b| a.union(b));
        match matching {
            Some(r) if self.is_active() => Some(r),
            _ => crate::features::spaces::space_bounds(world),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, world: &World, events: &mut Vec<Event>) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("+ Terminal").on_hover_text("Ctrl+Maj+N · double-clic sur le canevas").clicked() {
                events.push(Event::NewCard { at: None, project: None });
            }
            if ui.button("+ Projet").clicked() {
                events.push(Event::NewProject { at: None });
            }
            ui.separator();

            ui.add(TextEdit::singleline(&mut self.text).id(Id::unique(SEARCH_ID)).hint_text("Filtrer… (Ctrl+Maj+F)").desired_width(200.0));

            for kind in StatusKind::ALL {
                let count = world.cards.iter().filter(|c| StatusKind::of(c.status()) == Some(kind)).count();
                let on = self.statuses.contains(&kind);
                if count == 0 && !on {
                    continue;
                }
                let status = kind.sample();
                let text = RichText::new(format!("{} {count}", status.label())).color(theme::status_color(Some(status)));
                if ui.selectable_label(on, text).clicked() {
                    toggle(&mut self.statuses, kind);
                }
            }

            if self.is_active() && ui.small_button("× filtres").clicked() {
                *self = Filters::default();
            }
            ui.separator();

            if ui.button("Tout voir").on_hover_text("Ctrl+Maj+0").clicked()
                && let Some(r) = self.fit_target(world) {
                    events.push(Event::FitRect(r));
                }
            let attention: Vec<&Card> = world.cards.iter().filter(|c| c.attention).collect();
            if !attention.is_empty() {
                let label = RichText::new(format!("🔔 {}", attention.len())).color(theme::status_color(Some(TermStatus::Stalled)));
                if ui.button(label).on_hover_text("Aller au prochain terminal qui demande l'attention").clicked() {
                    events.push(Event::FlyTo(attention[0].id.clone()));
                }
            }
        });
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
        w.projects.push(Project { id: "api".into(), name: "Backend API".into(), color: [0; 3], rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)), cwd: None });
        let mut a = Card::new(Rect::from_min_size(pos2(10.0, 10.0), Vec2::splat(100.0)), Some("/srv/api".into()));
        a.info = Some(SessionInfo {
            session: a.session(),
            status: TermStatus::Stalled,
            command: Some("cargo watch -x run".into()),
            cwd: Some("/srv/api".into()),
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
        });
        a.name = Some("serveur".into());
        let b = Card::new(Rect::from_min_size(pos2(5000.0, 10.0), Vec2::splat(100.0)), Some("/home/me/blog".into()));
        w.add_card(a);
        w.add_card(b);
        w
    }

    fn matching(f: &Filters, w: &World) -> Vec<usize> {
        w.cards.iter().enumerate().filter(|(_, c)| f.matches(w, c)).map(|(i, _)| i).collect()
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
        for (q, expected) in [("serveur", vec![0]), ("CARGO watch", vec![0]), ("backend", vec![0]), ("blog", vec![1]), ("api blog", vec![]), ("  ", vec![0, 1])] {
            let f = Filters { text: q.into(), ..Default::default() };
            assert_eq!(matching(&f, &w), expected, "requête {q:?}");
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
        assert_eq!(Filters::default().fit_target(&w), Some(w.projects[0].rect), "le terminal hors projet est ignoré");
        let f = Filters { text: "blog".into(), ..Default::default() };
        assert_eq!(f.fit_target(&w), Some(w.projects[0].rect), "aucune correspondance visible : tout l'espace");
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
