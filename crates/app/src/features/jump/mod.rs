//! "Go to" palette (Ctrl+Shift+P): fuzzy search among terminals and projects,
//! Enter to fly there.

use egui::{Align2, Id, Key, Modal, Modifiers, RichText, TextEdit};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::{Card, CardId, ProjectId, World};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Card(CardId),
    Project(ProjectId),
}

pub struct Candidate {
    pub target: Target,
    pub haystack: String,
    pub attention: bool,
    pub z: u64,
}

/// No query: terminals asking for attention, then projects (in the order of their
/// shortcuts), then recently used terminals. With a query: descending fuzzy score;
/// on equal score, the project comes first.
pub fn rank(query: &str, candidates: &[Candidate]) -> Vec<Target> {
    let query = query.trim();
    let group = |c: &Candidate| match (&c.target, c.attention) {
        (Target::Card(_), true) => 0,
        (Target::Project(_), _) => 1,
        (Target::Card(_), false) => 2,
    };
    if query.is_empty() {
        let mut order: Vec<(usize, &Candidate)> = candidates.iter().enumerate().collect();
        order.sort_by(|(ia, a), (ib, b)| group(a).cmp(&group(b)).then(b.z.cmp(&a.z)).then(ia.cmp(ib)));
        return order.into_iter().map(|(_, c)| c.target.clone()).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let items = candidates.iter().enumerate().map(|(i, c)| Item(i, c.haystack.as_str()));
    let mut scored: Vec<(usize, u32)> = pattern.match_list(items, &mut matcher).into_iter().map(|(item, score)| (item.0, score)).collect();
    let is_project = |i: usize| matches!(candidates[i].target, Target::Project(_));
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(is_project(b.0).cmp(&is_project(a.0))).then(candidates[b.0].z.cmp(&candidates[a.0].z)));
    scored.into_iter().map(|(i, _)| candidates[i].target.clone()).collect()
}

struct Item<'a>(usize, &'a str);

impl AsRef<str> for Item<'_> {
    fn as_ref(&self) -> &str {
        self.1
    }
}

pub fn candidates(world: &World) -> Vec<Candidate> {
    let projects = world.projects.iter().map(|p| Candidate { target: Target::Project(p.id.clone()), haystack: format!("{} {}", p.name, p.cwd.as_deref().unwrap_or_default()), attention: false, z: 0 });
    let cards = world.cards.iter().map(|c: &Card| {
        let project = world.project_of(c).map(|p| p.name.as_str()).unwrap_or_default();
        let cmd = c.info.as_ref().and_then(|i| i.command.as_deref()).unwrap_or_default();
        Candidate { target: Target::Card(c.id.clone()), haystack: format!("{} {} {} {}", c.title(), project, c.current_cwd().unwrap_or_default(), cmd), attention: c.attention, z: c.z }
    });
    projects.chain(cards).collect()
}

/// What choosing a target does.
pub fn go_to(world: &World, target: &Target) -> Option<Event> {
    match target {
        Target::Card(id) => Some(Event::FlyTo(id.clone())),
        Target::Project(id) => world.project(id).map(|p| Event::EnterSpace(Some(p.id.clone()))),
    }
}

#[derive(Default)]
pub struct Jump {
    open: bool,
    query: String,
    selected: usize,
}

impl Jump {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
    }

    pub fn ui(&mut self, ctx: &egui::Context, world: &World, events: &mut Vec<Event>) {
        if !self.open {
            return;
        }
        let results = rank(&self.query, &candidates(world));
        let (down, up, enter) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowDown), i.consume_key(Modifiers::NONE, Key::ArrowUp), i.consume_key(Modifiers::NONE, Key::Enter)));
        if down {
            self.selected = (self.selected + 1).min(results.len().saturating_sub(1));
        }
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        self.selected = self.selected.min(results.len().saturating_sub(1));

        let mut chosen = enter.then(|| results.get(self.selected).cloned()).flatten();
        let area = egui::Area::new(Id::unique("jump-area")).anchor(Align2::CENTER_TOP, [0.0, 80.0]);
        let modal = Modal::new(Id::unique("jump")).area(area).show(ctx, |ui| {
            ui.set_width(520.0);
            let edit = ui.add(TextEdit::singleline(&mut self.query).id(Id::unique("jump-query")).hint_text(t().jump_hint).desired_width(f32::INFINITY));
            edit.request_focus();
            if edit.changed() {
                self.selected = 0;
            }
            ui.separator();
            if results.is_empty() {
                ui.label(RichText::new(t().no_results).color(theme::TEXT_DIM));
            }
            for (k, target) in results.iter().take(12).enumerate() {
                let Some(text) = label(world, target) else { continue };
                if ui.selectable_label(k == self.selected, text).clicked() {
                    chosen = Some(target.clone());
                }
            }
        });
        if let Some(event) = chosen.and_then(|t| go_to(world, &t)) {
            events.push(event);
            self.open = false;
        } else if modal.should_close() {
            self.open = false;
        }
    }
}

fn label(world: &World, target: &Target) -> Option<RichText> {
    Some(match target {
        Target::Card(id) => {
            let card = world.card(id)?;
            let project = world.project_of(card).map(|p| format!("  ·  {}", p.name)).unwrap_or_default();
            RichText::new(format!("● {}{}  —  {}", card.title(), project, card.current_cwd().unwrap_or(""))).color(theme::status_color(card.status()))
        }
        Target::Project(id) => {
            let n = world.projects.iter().position(|p| &p.id == id)?;
            let p = &world.projects[n];
            let count = world.cards.iter().filter(|c| world.project_of(c).is_some_and(|q| q.id == p.id)).count();
            let key = if n < 9 { (t().project_shortcut)(n + 1) } else { String::new() };
            RichText::new(format!("■ {}  —  {}{key}", p.name, (t().terminal_count)(count))).color(theme::rgb(p.color)).strong()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, hay: &str, attention: bool, z: u64) -> Candidate {
        Candidate { target: Target::Card(id.into()), haystack: hay.into(), attention, z }
    }

    fn project(id: &str, hay: &str) -> Candidate {
        Candidate { target: Target::Project(id.into()), haystack: hay.into(), attention: false, z: 0 }
    }

    fn c(id: &str) -> Target {
        Target::Card(id.into())
    }

    #[test]
    fn empty_query_orders_attention_then_projects_then_recent() {
        let list = [card("a", "a", false, 1), project("p", "p"), card("b", "b", true, 0), card("c", "c", false, 9)];
        assert_eq!(rank("", &list), vec![c("b"), Target::Project("p".into()), c("c"), c("a")]);
    }

    #[test]
    fn fuzzy_query_ranks_best_match_first() {
        let list = [card("blog", "blog  /home/me/blog zola serve", false, 5), card("api", "server Backend API /srv/api cargo watch", false, 1), card("db", "psql  /srv/db psql", false, 3)];
        assert_eq!(rank("api", &list)[0], c("api"));
        assert_eq!(rank("crgwtch", &list), vec![c("api")]);
        assert_eq!(rank("psql", &list), vec![c("db")]);
        assert!(rank("zzzzqq", &list).is_empty());
    }

    #[test]
    fn projects_are_found_by_name() {
        let list = [card("t", "server Backend /srv/api", false, 1), project("p", "Backend ~/api")];
        assert_eq!(rank("backend", &list)[0], Target::Project("p".into()), "on equal score, the project first");
    }
}
