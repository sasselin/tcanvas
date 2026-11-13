//! "Your turn": one ordered list of what needs the user now, across every group and space (agents
//! and terminals asking, tasks in review, alerts not looked at, then ready tasks; order in
//! `logic`). The top bar counts what waits for the user (ready tasks only wait to be started: they
//! are listed, not counted there); Ctrl+Shift+Q opens the list (Up/Down, Enter, Escape),
//! Ctrl+Shift+J goes straight to its next row without opening it. The rows are published in the
//! world, where the "Go to" palette reads its first entries.
//!
//! It sits next to the bell, which it does not replace: the bell counts the alerts not looked at
//! yet (a long command that finished included), and goes to them; the queue is the to-do list,
//! where a question stays until it is answered and tasks wait for a review or a start.

pub mod logic;

use egui::{Align2, Button, Color32, Id, Key, Modal, Modifiers, RichText, ScrollArea};
use protocol::TermStatus;

use crate::features::shortcuts::keymap::{Shortcut, combo_of};
use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::widgets::{age_label, elide_middle, priority_mark};
use crate::shell::world::{QueueEntry, QueueKey, QueueKind, World, unix_now};
use logic::{Since, build, inputs};

/// Longest title in a row, in characters (agents may write long ones).
const TITLE_CHARS: usize = 70;

/// What choosing a row does: a card row flies to its card (its space opened first, its alert
/// acknowledged once looked at); a task row shows the task in its group's Tasks panel.
pub fn go_to(entry: &QueueEntry) -> Option<Event> {
    match (&entry.key, &entry.task, &entry.card) {
        (QueueKey::Task { .. }, Some(task), _) => Some(Event::ShowTask { project: task.project.clone(), task: task.id.clone() }),
        (_, _, Some(card)) => Some(Event::FlyTo(card.clone())),
        _ => None,
    }
}

/// The row's one line: the card's title (and the task it works on), or the task's reference,
/// priority mark and title; long titles elided.
pub fn label(world: &World, entry: &QueueEntry) -> String {
    let task = entry.task.as_ref().map(|t| {
        let mark = priority_mark(t.priority);
        let title = elide_middle(&t.title, TITLE_CHARS);
        if mark.is_empty() || entry.kind != QueueKind::Ready { format!("#{} {title}", t.reference) } else { format!("#{} {mark} {title}", t.reference) }
    });
    match (&entry.key, task) {
        (QueueKey::Task { .. }, Some(task)) => task,
        (_, task) => {
            let title = entry.card.as_deref().and_then(|id| world.card(id)).map_or_else(|| "?".to_string(), |c| elide_middle(&c.title(), TITLE_CHARS));
            match task {
                Some(task) => format!("{title} · {task}"),
                None => title,
            }
        }
    }
}

/// The header of a kind's rows.
pub fn kind_title(kind: QueueKind) -> &'static str {
    match kind {
        QueueKind::Waiting => t().queue_waiting,
        QueueKind::Review => t().queue_review,
        QueueKind::Alert => t().queue_alert,
        QueueKind::Ready => t().queue_ready,
    }
}

/// What the top bar counts: the rows of someone waiting for the user.
pub fn waiting_count(world: &World) -> usize {
    world.queue.iter().filter(|r| r.kind.needs_user()).count()
}

fn kind_color(entry: &QueueEntry) -> Color32 {
    match entry.kind {
        QueueKind::Waiting | QueueKind::Alert => theme::status_color(entry.status),
        // A review whose card failed or stopped unseen: the card's color.
        QueueKind::Review if entry.card_alert => theme::status_color(entry.status),
        QueueKind::Review => theme::status_color(Some(TermStatus::Stalled)),
        QueueKind::Ready => theme::TEXT,
    }
}

#[derive(Default)]
pub struct Queue {
    since: Since,
    /// What the rows were last built from (`logic::inputs`).
    built_from: Option<u64>,
    open: bool,
    /// The selected row, by identity: it stays on it while rows come and go.
    selected: Option<QueueKey>,
    /// Its place, for when it is gone: the row now there is selected.
    index: usize,
    /// The row the last "next" went to, and its place then.
    last_jump: Option<(QueueKey, usize)>,
}

impl Queue {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        (self.selected, self.index) = (None, 0);
    }

    /// Every frame: the rows, built again when what they come from changed, published for the
    /// other slices.
    pub fn poll(&mut self, world: &mut World) {
        let now = inputs(world);
        if self.built_from == Some(now) {
            return;
        }
        self.built_from = Some(now);
        let rows = build(world, &mut self.since, unix_now());
        if rows != world.queue {
            world.queue = rows;
        }
    }

    /// Ctrl+Shift+J: the first row; again, while still on the row it went to, the one after it
    /// (round the list). When that row has left meanwhile (answered), the one now at its place:
    /// never back to an older row left unanswered on purpose.
    pub fn next(&mut self, world: &World) -> Option<Event> {
        let rows = &world.queue;
        if rows.is_empty() {
            return None;
        }
        let on = |key: &QueueKey| match key {
            QueueKey::Card(id) => world.focused.as_ref() == Some(id),
            QueueKey::Task { .. } => true,
        };
        let at = match &self.last_jump {
            Some((key, at)) => match rows.iter().position(|r| &r.key == key) {
                Some(i) if on(key) => (i + 1) % rows.len(),
                Some(i) => i,
                None if *at < rows.len() => *at,
                None => 0,
            },
            None => 0,
        };
        let row = &rows[at];
        self.last_jump = Some((row.key.clone(), at));
        go_to(row)
    }

    /// Top bar button with the count: dimmed at zero (the bar never shifts), still opens the list.
    pub fn toolbar_ui(&mut self, ui: &mut egui::Ui, world: &World) {
        let n = waiting_count(world);
        let color = if self.open {
            theme::CARD_BG
        } else if n == 0 {
            theme::TEXT_DIM
        } else {
            theme::status_color(Some(TermStatus::WaitingInput))
        };
        let button = Button::selectable(self.open, RichText::new((t().queue_button)(n)).color(color)).fill(if self.open { theme::TEXT } else { Color32::TRANSPARENT });
        let hint = format!("{} · {}", t().queue_button_hint, combo_of(Shortcut::Queue).format());
        if ui.add(button).on_hover_text(hint).clicked() {
            self.toggle();
        }
    }

    /// The list, as a modal: rows grouped by kind (each header with its count), Up/Down to select,
    /// Enter or a click to go, Escape or a click outside to close.
    pub fn ui(&mut self, ctx: &egui::Context, world: &World, events: &mut Vec<Event>) {
        if !self.open {
            return;
        }
        // The durations move on.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        let rows = &world.queue;
        let (down, up, enter) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowDown), i.consume_key(Modifiers::NONE, Key::ArrowUp), i.consume_key(Modifiers::NONE, Key::Enter)));
        let last = rows.len().saturating_sub(1);
        let mut index = self.selected.as_ref().and_then(|k| rows.iter().position(|r| &r.key == k)).unwrap_or(self.index.min(last));
        if down {
            index = (index + 1).min(last);
        }
        if up {
            index = index.saturating_sub(1);
        }
        (self.index, self.selected) = (index, rows.get(index).map(|r| r.key.clone()));
        let mut chosen = if enter { rows.get(index).and_then(go_to) } else { None };
        let now = unix_now();
        let area = egui::Area::new(Id::unique("queue-area")).anchor(Align2::CENTER_TOP, [0.0, 80.0]);
        let modal = Modal::new(Id::unique("queue")).area(area).show(ctx, |ui| {
            ui.set_width(620.0_f32.min(ctx.content_rect().width() - 40.0));
            ui.label(RichText::new((t().queue_title)(waiting_count(world))).strong().size(16.0));
            ui.separator();
            if rows.is_empty() {
                ui.label(RichText::new(t().queue_empty).color(theme::TEXT_DIM));
            }
            let max = ctx.content_rect().height() * 0.6;
            ScrollArea::vertical().id_salt("queue-rows").max_height(max).show(ui, |ui| {
                let mut kind = None;
                for (k, row) in rows.iter().enumerate() {
                    if kind != Some(row.kind) {
                        if kind.is_some() {
                            ui.add_space(4.0);
                        }
                        kind = Some(row.kind);
                        let n = rows.iter().filter(|r| r.kind == row.kind).count();
                        ui.label(RichText::new(format!("{} ({n})", kind_title(row.kind))).small().strong().color(theme::TEXT_DIM));
                    }
                    ui.horizontal(|ui| {
                        let group = row.project.as_deref().and_then(|id| world.project(id));
                        let (name, color) = group.map_or((t().queue_no_group, theme::TEXT_DIM), |p| (p.name.as_str(), theme::rgb(p.color)));
                        ui.label(RichText::new(format!("■ {}", elide_middle(name, 18))).color(color));
                        let resp = ui.selectable_label(k == index, RichText::new(label(world, row)).color(kind_color(row)));
                        if k == index && (up || down) {
                            resp.scroll_to_me(None);
                        }
                        if resp.clicked() {
                            chosen = go_to(row);
                        }
                        ui.label(RichText::new(age_label(now, row.since.max(1))).small().color(theme::TEXT_DIM));
                        if row.kind == QueueKind::Review && row.card_alert {
                            let status = row.status.map(|s| t().status(s)).unwrap_or_default();
                            ui.label(RichText::new(format!("{status} · {}", t().queue_alert_on_review)).small().color(kind_color(row)));
                        }
                        if let (QueueKey::Task { .. }, Some(card)) = (&row.key, &row.card)
                            && ui.small_button(t().queue_go_to_card).clicked()
                        {
                            chosen = Some(Event::FlyTo(card.clone()));
                        }
                        if let Some(detail) = &row.detail {
                            ui.label(RichText::new(elide_middle(detail, 60)).small().color(theme::TEXT_DIM)).on_hover_text(detail);
                        }
                    });
                }
            });
            ui.separator();
            ui.label(RichText::new(t().queue_keys).small().color(theme::TEXT_DIM));
        });
        if let Some(event) = chosen {
            events.push(event);
            self.open = false;
        } else if modal.should_close() {
            self.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::world::{Card, QueueTask};
    use egui::{Rect, pos2, vec2};
    use protocol::Priority;

    fn world() -> World {
        let mut w = World::default();
        for id in ["a", "b"] {
            let mut c = Card::with_id(id.into(), Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0)), None);
            c.name = Some(format!("card {id}"));
            w.cards.push(c);
        }
        w
    }

    fn card_row(id: &str) -> QueueEntry {
        QueueEntry { key: QueueKey::Card(id.into()), kind: QueueKind::Waiting, project: None, card: Some(id.into()), task: None, status: None, card_alert: false, detail: None, since: 0 }
    }

    fn task(priority: Priority) -> QueueTask {
        QueueTask { project: "p".into(), id: "t1".into(), reference: "k3f".into(), title: "Invoice PDF".into(), priority }
    }

    fn task_row(kind: QueueKind, card: Option<&str>) -> QueueEntry {
        QueueEntry {
            key: QueueKey::Task { list: "/srv".into(), task: "t1".into() },
            kind,
            project: Some("p".into()),
            card: card.map(String::from),
            task: Some(task(Priority::Urgent)),
            status: None,
            card_alert: false,
            detail: None,
            since: 0,
        }
    }

    #[test]
    fn rows_lead_to_their_card_or_their_task() {
        assert!(matches!(go_to(&card_row("a")), Some(Event::FlyTo(c)) if c == "a"));
        assert!(matches!(go_to(&task_row(QueueKind::Review, Some("a"))), Some(Event::ShowTask { project, task }) if project == "p" && task == "t1"));
        let w = world();
        assert_eq!(label(&w, &task_row(QueueKind::Ready, None)), "#k3f !!! Invoice PDF");
        assert_eq!(label(&w, &task_row(QueueKind::Review, Some("a"))), "#k3f Invoice PDF", "the mark only for ready tasks");
        let mut waiting = card_row("b");
        waiting.task = Some(task(Priority::None));
        assert_eq!(label(&w, &waiting), "card b · #k3f Invoice PDF");
    }

    #[test]
    fn next_goes_to_the_first_row_then_on_while_the_user_is_there() {
        let mut w = world();
        let mut q = Queue::default();
        assert!(q.next(&w).is_none(), "nothing to go to");
        w.queue = vec![card_row("a"), card_row("b")];
        assert!(matches!(q.next(&w), Some(Event::FlyTo(c)) if c == "a"));
        // Not there yet (the flight has not landed): the first again.
        assert!(matches!(q.next(&w), Some(Event::FlyTo(c)) if c == "a"));
        w.focused = Some("a".into());
        assert!(matches!(q.next(&w), Some(Event::FlyTo(c)) if c == "b"));
        w.focused = Some("b".into());
        assert!(matches!(q.next(&w), Some(Event::FlyTo(c)) if c == "a"), "round the list");
        // Answered: gone, the next is the one now at its place.
        w.queue.remove(0);
        w.focused = Some("a".into());
        assert!(matches!(q.next(&w), Some(Event::FlyTo(c)) if c == "b"));
        // An older row left unanswered on purpose is not gone back to: a, then b answered, then c.
        let mut q = Queue::default();
        w.cards.push(Card::with_id("c".into(), Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0)), None));
        w.queue = vec![card_row("a"), card_row("b"), card_row("c")];
        w.focused = None;
        q.next(&w);
        w.focused = Some("a".into());
        assert!(matches!(q.next(&w), Some(Event::FlyTo(c)) if c == "b"));
        w.focused = Some("b".into());
        w.queue.remove(1);
        assert!(matches!(q.next(&w), Some(Event::FlyTo(c)) if c == "c"), "on from b's place, not back to a");
    }

    #[test]
    fn ready_tasks_are_listed_not_counted_and_long_titles_are_elided() {
        let mut w = world();
        w.queue = vec![card_row("a"), task_row(QueueKind::Review, None), task_row(QueueKind::Ready, None), task_row(QueueKind::Ready, None)];
        assert_eq!(waiting_count(&w), 2);
        let mut long = task_row(QueueKind::Review, None);
        long.task.as_mut().unwrap().title = "word ".repeat(40);
        assert!(label(&w, &long).chars().count() < 80);
        w.cards[0].name = Some("n".repeat(200));
        assert!(label(&w, &card_row("a")).chars().count() <= TITLE_CHARS);
    }
}
