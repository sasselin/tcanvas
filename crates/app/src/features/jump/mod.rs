//! "Go to" palette (Ctrl+Shift+P): fuzzy search among terminals and projects,
//! Enter to fly there. Also lists the "Keyboard shortcuts" window. With no query, the first few
//! rows of the "Your turn" queue that wait for the user come first (questions, reviews, alerts;
//! not the ready tasks), in its order (read from `World::queue`, where the `queue` slice
//! publishes it), then the usual entries.

use egui::{Align2, Id, Key, Modal, Modifiers, RichText, TextEdit};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

use crate::features::shortcuts::keymap;
use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::widgets::elide_middle;
use crate::shell::world::{Card, CardId, ProjectId, QueueEntry, QueueKey, World};

/// Queue rows put first, at most: the groups and recent terminals stay in view below them.
pub const QUEUE_FIRST: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Card(CardId),
    Project(ProjectId),
    /// A task of the "Your turn" queue: its group, its id.
    Task(ProjectId, String),
    /// The "Keyboard shortcuts" window.
    Shortcuts,
}

pub struct Candidate {
    pub target: Target,
    pub haystack: String,
    pub attention: bool,
    pub z: u64,
    /// Its place in the "Your turn" queue.
    pub queue: Option<usize>,
}

/// No query: the first rows of the "Your turn" queue (`Candidate::queue`), then other terminals asking for attention, then
/// projects (in the order of their shortcuts), then recently used terminals, then the shortcuts
/// window. With a query: descending fuzzy score; on equal score, the project comes first.
pub fn rank(query: &str, candidates: &[Candidate]) -> Vec<Target> {
    let query = query.trim();
    let group = |c: &Candidate| match (&c.target, c.attention, c.queue) {
        (_, _, Some(at)) => (0, at),
        (Target::Card(_), true, _) => (1, 0),
        (Target::Project(_), _, _) => (2, 0),
        (Target::Card(_), false, _) | (Target::Task(..), _, _) => (3, 0),
        (Target::Shortcuts, _, _) => (4, 0),
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
    let projects = world.projects.iter().map(|p| Candidate {
        target: Target::Project(p.id.clone()),
        haystack: format!("{} {}", p.name, p.cwd.as_deref().unwrap_or_default()),
        attention: false,
        z: 0,
        queue: None,
    });
    // The rows that wait for the user, the first few only.
    let first: Vec<&QueueEntry> = world.queue.iter().filter(|e| e.kind.needs_user()).take(QUEUE_FIRST).collect();
    let queued = |key: &QueueKey| first.iter().position(|e| &e.key == key);
    let cards = world.cards.iter().map(|c: &Card| {
        let project = world.project_of(c).map(|p| p.name.as_str()).unwrap_or_default();
        let cmd = c.info.as_ref().and_then(|i| i.command.as_deref()).unwrap_or_default();
        let queue = queued(&QueueKey::Card(c.id.clone()));
        Candidate { target: Target::Card(c.id.clone()), haystack: format!("{} {} {} {}", c.title(), project, c.current_cwd().unwrap_or_default(), cmd), attention: c.attention, z: c.z, queue }
    });
    // Those tasks only: the palette is no task search.
    let tasks = first.iter().enumerate().filter_map(|(at, e)| {
        let task = e.task.as_ref().filter(|_| matches!(e.key, QueueKey::Task { .. }))?;
        let project = world.project(&task.project).map(|p| p.name.as_str()).unwrap_or_default();
        Some(Candidate { target: Target::Task(task.project.clone(), task.id.clone()), haystack: format!("#{} {} {project}", task.reference, task.title), attention: false, z: 0, queue: Some(at) })
    });
    let help = Candidate { target: Target::Shortcuts, haystack: t().shortcuts_title.to_owned(), attention: false, z: 0, queue: None };
    projects.chain(cards).chain(tasks).chain(std::iter::once(help)).collect()
}

/// The queue row of a task target.
fn queued_task<'a>(world: &'a World, project: &str, task: &str) -> Option<&'a QueueEntry> {
    world.queue.iter().find(|e| matches!(e.key, QueueKey::Task { .. }) && e.task.as_ref().is_some_and(|t| t.project == project && t.id == task))
}

/// What choosing a target does.
pub fn go_to(world: &World, target: &Target) -> Option<Event> {
    match target {
        Target::Card(id) => Some(Event::FlyTo(id.clone())),
        Target::Project(id) => world.project(id).map(|p| Event::EnterSpace(Some(p.id.clone()))),
        Target::Task(project, task) => world.project(project).map(|_| Event::ShowTask { project: project.clone(), task: task.clone() }),
        Target::Shortcuts => Some(Event::ShowShortcuts),
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
        Target::Task(project, task) => {
            let task = queued_task(world, project, task)?.task.as_ref()?;
            let group = world.project(project)?;
            RichText::new(format!("✔ #{} {}  ·  {}", task.reference, elide_middle(&task.title, 60), group.name)).color(theme::rgb(group.color))
        }
        Target::Shortcuts => RichText::new(format!("⌨ {}   {}", t().shortcuts_title, keymap::help_combo().format())).color(theme::TEXT_DIM),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, hay: &str, attention: bool, z: u64) -> Candidate {
        Candidate { target: Target::Card(id.into()), haystack: hay.into(), attention, z, queue: None }
    }

    fn project(id: &str, hay: &str) -> Candidate {
        Candidate { target: Target::Project(id.into()), haystack: hay.into(), attention: false, z: 0, queue: None }
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
    fn empty_query_puts_the_queue_first_in_its_order() {
        use crate::shell::world::{QueueKind, QueueTask};
        let mut w = World::default();
        w.projects.push(crate::shell::world::Project {
            id: "p".into(),
            name: "shop".into(),
            color: [0; 3],
            rect: egui::Rect::NOTHING,
            cwd: Some("/srv/shop".into()),
            templates: Vec::new(),
            messages: Vec::new(),
            worktree_location: None,
            task_settings: Default::default(),
        });
        for (id, z) in [("recent", 9), ("asks", 1), ("alert", 2)] {
            let mut c = Card::with_id(id.into(), egui::Rect::NOTHING, None);
            c.z = z;
            c.attention = id == "alert";
            w.cards.push(c);
        }
        let task = QueueTask { project: "p".into(), id: "t".into(), reference: "k3f".into(), title: "Invoice PDF".into(), priority: protocol::Priority::None };
        let entry = |key: QueueKey, kind, card: Option<&str>, task: Option<QueueTask>| QueueEntry {
            key,
            kind,
            project: Some("p".into()),
            card: card.map(String::from),
            task,
            status: None,
            card_alert: false,
            detail: None,
            since: 0,
        };
        w.queue = vec![
            entry(QueueKey::Card("asks".into()), QueueKind::Waiting, Some("asks"), None),
            entry(QueueKey::Task { list: "/srv/shop".into(), task: "t".into() }, QueueKind::Review, None, Some(task)),
            entry(QueueKey::Card("alert".into()), QueueKind::Alert, Some("alert"), None),
        ];
        let order = rank("", &candidates(&w));
        assert_eq!(order[..5], [c("asks"), Target::Task("p".into(), "t".into()), c("alert"), Target::Project("p".into()), c("recent")]);
        assert!(matches!(go_to(&w, &order[1]), Some(Event::ShowTask { project, task }) if project == "p" && task == "t"));
        assert_eq!(rank("invoice", &candidates(&w)), vec![Target::Task("p".into(), "t".into())], "found by its title");
        assert!(label(&w, &order[1]).is_some_and(|l| l.text() == "✔ #k3f Invoice PDF  ·  shop"));
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

    #[test]
    fn the_shortcuts_window_is_listed_last_and_found_by_name() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let mut list = candidates(&World::default());
        list.insert(0, card("a", "raccord", false, 1));
        assert_eq!(rank("", &list).last(), Some(&Target::Shortcuts));
        assert_eq!(rank("clavier", &list), vec![Target::Shortcuts]);
        assert!(matches!(go_to(&World::default(), &Target::Shortcuts), Some(Event::ShowShortcuts)));
    }
}
