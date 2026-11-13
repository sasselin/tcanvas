//! The order of the "Your turn" queue, as one pure function over the world: what needs the user
//! now, across every group and space. No UI.
//!
//! Rows, kind by kind:
//! 1. `Waiting`: a terminal (or its coding agent) asking for permission or input, for as long as
//!    it asks.
//! 2. `Review`: a task in review.
//! 3. `Alert`: a terminal stalled, failed (not a Ctrl+C) or ended, while its alert is not looked
//!    at (`Card::attention`): the same statuses `event::alert` reports, and gone once the user
//!    looked at the card, as the alert is. A stalled `tail -f` or an old failed command does not
//!    stay in the queue forever.
//! 4. `Ready`: a task to do whose dependencies are all finished, and that no live card works on
//!    yet (between its start and its agent's first prompt, or moved back to todo while its card
//!    runs, it is not "ready to start").
//!
//! Within the first three kinds the longest-waiting row comes first: the point is to unblock work,
//! and the oldest cause has held something up the longest. A card row's age is how long this run
//! has seen its cause; a task row's is the task's own last change (`Task::updated`), so it
//! survives restarts and lists arriving empty for a moment. Ready tasks go by priority (urgent
//! first), then by group (top bar order), then by their list's manual order.
//!
//! A card linked to a task in review is shown once, as the task's row (its card attached, its
//! alert, if any, shown on that row). A waiting card started for a task names it. Groups of one
//! repository share a list: its tasks show once, under the first of those groups.

use std::collections::{HashMap, HashSet};

use protocol::{Task, TaskStatus, blocked_by};

use crate::shell::event::{Notice, alert};
use crate::shell::widgets::visible_text;
use crate::shell::world::{Card, QueueEntry, QueueKey, QueueKind, QueueTask, World};

/// What put a card in the queue: a new cause starts its clock again.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Cause {
    Waiting,
    Stalled,
    Failed(i32),
    Stopped,
}

/// When each card row's cause was first seen in this run (Unix seconds), kept while it stays.
#[derive(Default)]
pub struct Since(HashMap<(String, Cause), u64>);

/// The card's row kind, if it needs the user: waiting while it asks, the other alerts until looked
/// at.
fn card_cause(card: &Card) -> Option<(QueueKind, Cause)> {
    let status = card.status()?;
    match alert(status)? {
        Notice::WaitingInput => Some((QueueKind::Waiting, Cause::Waiting)),
        _ if !card.attention => None,
        Notice::Stalled => Some((QueueKind::Alert, Cause::Stalled)),
        Notice::Failed(code) => Some((QueueKind::Alert, Cause::Failed(code))),
        _ => Some((QueueKind::Alert, Cause::Stopped)),
    }
}

/// One line the terminal shows: what it asks (its last line), else the last prompt it was given.
fn card_detail(card: &Card, kind: QueueKind) -> Option<String> {
    let info = card.info.as_ref()?;
    let tail = info.tail.iter().rev().map(|l| l.trim()).find(|l| !l.is_empty()).map(visible_text);
    let prompt = info.last_prompt.as_deref().map(visible_text);
    match kind {
        QueueKind::Waiting => tail.or(prompt),
        _ => tail,
    }
    .filter(|s| !s.is_empty())
}

fn queue_task(project: &str, t: &Task) -> QueueTask {
    QueueTask { project: project.to_string(), id: t.id.clone(), reference: t.reference.clone(), title: visible_text(&t.title), priority: t.priority }
}

/// What orders rows of the same kind (see the module's doc).
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Order {
    /// Longest waiting first, then group, then place.
    Oldest { since: u64, group: usize, at: usize },
    /// Priority (reversed: urgent first), then group, then the list's order.
    Ready { priority: std::cmp::Reverse<protocol::Priority>, group: usize, at: usize },
}

/// The queue: every row, in order. `since` keeps when each card's cause appeared; causes gone are
/// forgotten. Each shared list is walked once, finished tasks skipped first, and cards are found
/// by session through one map.
pub fn build(world: &World, since: &mut Since, now: u64) -> Vec<QueueEntry> {
    let group_at = |id: Option<&str>| id.and_then(|id| world.projects.iter().position(|p| p.id == id)).unwrap_or(usize::MAX);
    let by_session: HashMap<String, &Card> = world.cards.iter().map(|c| (c.session(), c)).collect();
    let mut rows: Vec<(QueueEntry, Order)> = Vec::new();
    let mut walked: HashSet<&str> = HashSet::new();
    let mut in_review: HashSet<&str> = HashSet::new();
    // The unfinished task each card works on, for a waiting card's row.
    let mut started_for: HashMap<&str, (&str, &Task)> = HashMap::new();
    for (group, p) in world.projects.iter().enumerate().filter(|(_, p)| p.cwd.is_some()) {
        let Some(list) = world.task_lists.get(&p.id) else { continue };
        if !walked.insert(list.key.as_str()) {
            continue;
        }
        for (at, t) in list.tasks.iter().enumerate() {
            if t.removed.is_some() || t.status.finished() {
                continue;
            }
            let card = t.session.as_deref().and_then(|s| by_session.get(s).copied());
            if let Some(card) = card {
                started_for.entry(card.id.as_str()).or_insert((p.id.as_str(), t));
            }
            let kind = match t.status {
                TaskStatus::Review => QueueKind::Review,
                TaskStatus::Todo if card.is_none() && blocked_by(t, &list.tasks).is_empty() => QueueKind::Ready,
                _ => continue,
            };
            if let Some(card) = card.filter(|_| kind == QueueKind::Review) {
                in_review.insert(card.id.as_str());
            }
            let changed = t.updated.max(t.created);
            let entry = QueueEntry {
                key: QueueKey::Task { list: list.key.clone(), task: t.id.clone() },
                kind,
                project: Some(p.id.clone()),
                card: card.map(|c| c.id.clone()),
                task: Some(queue_task(&p.id, t)),
                status: card.and_then(|c| c.status()),
                card_alert: card.is_some_and(|c| card_cause(c).is_some_and(|(k, _)| k == QueueKind::Alert)),
                detail: None,
                since: if changed == 0 { now } else { changed },
            };
            let order = match kind {
                QueueKind::Ready => Order::Ready { priority: std::cmp::Reverse(t.priority), group, at },
                _ => Order::Oldest { since: entry.since, group, at },
            };
            rows.push((entry, order));
        }
    }
    // Cards: every group's and those outside any group, all spaces.
    let mut kept = HashMap::new();
    for (at, card) in world.cards.iter().enumerate() {
        let Some((kind, cause)) = card_cause(card) else { continue };
        if in_review.contains(card.id.as_str()) {
            continue;
        }
        let key = (card.id.clone(), cause);
        let first = since.0.get(&key).copied().unwrap_or(now);
        kept.insert(key, first);
        let project = world.project_of(card).map(|p| p.id.clone());
        let order = Order::Oldest { since: first, group: group_at(project.as_deref()), at };
        let entry = QueueEntry {
            key: QueueKey::Card(card.id.clone()),
            kind,
            project,
            card: Some(card.id.clone()),
            task: started_for.get(card.id.as_str()).map(|(p, t)| queue_task(p, t)),
            status: card.status(),
            card_alert: kind == QueueKind::Alert,
            detail: card_detail(card, kind),
            since: first,
        };
        rows.push((entry, order));
    }
    since.0 = kept;
    rows.sort_by(|a, b| a.0.kind.cmp(&b.0.kind).then_with(|| a.1.cmp(&b.1)));
    rows.into_iter().map(|(e, _)| e).collect()
}

/// What the queue is built from, as one number: it is built again only when this changes (a
/// card's status, attention, group or last lines, the groups and their order, a task list).
pub fn inputs(world: &World) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    world.task_lists_rev.hash(&mut h);
    for p in &world.projects {
        (&p.id, &p.cwd).hash(&mut h);
    }
    for c in &world.cards {
        (&c.id, &c.project, c.attention, c.pty_dead).hash(&mut h);
        if let Some(i) = &c.info {
            std::mem::discriminant(&i.status).hash(&mut h);
            if let protocol::TermStatus::Exited(code) = i.status {
                code.hash(&mut h);
            }
            (i.tail.last(), &i.last_prompt).hash(&mut h);
        }
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::world::Project;
    use egui::{Rect, pos2, vec2};
    use protocol::{Extra, Priority, SessionInfo, TaskList, TermStatus};

    fn project(id: &str, cwd: Option<&str>) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            color: [1, 2, 3],
            rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0)),
            cwd: cwd.map(Into::into),
            templates: Vec::new(),
            messages: Vec::new(),
            worktree_location: None,
            task_settings: Default::default(),
            merge_strategy: None,
        }
    }

    fn task(id: &str, status: TaskStatus) -> Task {
        Task {
            id: id.into(),
            reference: format!("r{id}"),
            title: id.to_uppercase(),
            description: String::new(),
            status,
            priority: Priority::None,
            depends_on: vec![],
            notes: vec![],
            created: 0,
            updated: 0,
            rev: 1,
            content_rev: 1,
            removed: None,
            worktree: None,
            branch: None,
            session: None,
            extra: Extra::new(),
        }
    }

    fn info(session: String, status: TermStatus, tail: &[&str]) -> SessionInfo {
        SessionInfo {
            session,
            status,
            command: None,
            cwd: None,
            last_exit: None,
            last_duration_secs: None,
            tail: tail.iter().map(|s| s.to_string()).collect(),
            agent_session_id: None,
            agent_transcript: None,
            last_prompt: Some("add a rate limit".into()),
            agent_prompt_mark: None,
            agent_turn_mark: None,
            agent_prompt_head: None,
            agent_turn_head: None,
        }
    }

    /// Groups `api` and `web` (linked), `notes` (no folder), a card per (id, group, status,
    /// attention).
    fn world(cards: &[(&str, Option<&str>, TermStatus, bool)]) -> World {
        let mut w = World::default();
        w.projects = vec![project("api", Some("/srv/api")), project("web", Some("/srv/web")), project("notes", None)];
        for (id, group, status, attention) in cards {
            let mut c = Card::with_id(id.to_string(), Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0)), None);
            c.name = Some(id.to_string());
            c.project = group.map(String::from);
            c.info = Some(info(c.session(), *status, &["", "Continue? [y/N]", "  "]));
            c.attention = *attention;
            w.cards.push(c);
        }
        w
    }

    fn list(w: &mut World, group: &str, key: &str, tasks: Vec<Task>) {
        w.task_lists.insert(group.into(), TaskList { tasks, ..TaskList::empty(key) });
    }

    fn keys(rows: &[QueueEntry]) -> Vec<String> {
        rows.iter()
            .map(|r| match &r.key {
                QueueKey::Card(c) => c.clone(),
                QueueKey::Task { task, .. } => format!("#{task}"),
            })
            .collect()
    }

    #[test]
    fn each_kind_in_its_place() {
        use TermStatus::*;
        let mut w = world(&[
            ("idle", Some("api"), Idle, true),
            ("stalled", Some("api"), Stalled, true),
            ("asks", Some("web"), WaitingInput, false),
            ("failed", None, Exited(2), true),
            ("ctrl-c", None, Exited(130), true),
            ("dead", Some("notes"), Dead, true),
        ]);
        list(
            &mut w,
            "api",
            "/srv/api",
            vec![
                task("todo", TaskStatus::Todo),
                task("review", TaskStatus::Review),
                task("inbox", TaskStatus::Inbox),
                task("active", TaskStatus::Active),
                task("done", TaskStatus::Done),
                task("gone", TaskStatus::Canceled),
            ],
        );
        let rows = build(&w, &mut Since::default(), 100);
        // Alerts of the same age: by group, outside any group last.
        assert_eq!(keys(&rows), ["asks", "#review", "stalled", "dead", "failed", "#todo"]);
        let kinds: Vec<QueueKind> = rows.iter().map(|r| r.kind).collect();
        use QueueKind::*;
        assert_eq!(kinds, [Waiting, Review, Alert, Alert, Alert, Ready]);
        assert_eq!(rows[0].detail.as_deref(), Some("Continue? [y/N]"), "the question, blank lines skipped");
        assert_eq!((rows[0].project.as_deref(), rows[3].project.as_deref(), rows[4].project.as_deref()), (Some("web"), Some("notes"), None));
        assert!(rows.iter().all(|r| r.since == 100));
    }

    #[test]
    fn alerts_leave_once_looked_at_but_a_question_stays_until_answered() {
        use TermStatus::*;
        let mut w = world(&[("asks", None, WaitingInput, false), ("stalled", None, Stalled, false), ("failed", None, Exited(1), false)]);
        assert_eq!(keys(&build(&w, &mut Since::default(), 0)), ["asks"]);
        w.cards[0].info.as_mut().unwrap().status = Running;
        assert!(build(&w, &mut Since::default(), 0).is_empty(), "answered");
        w.cards[0].pty_dead = true;
        w.cards[0].attention = true;
        let rows = build(&w, &mut Since::default(), 0);
        assert_eq!((keys(&rows), rows[0].status), (vec!["asks".to_string()], Some(Dead)), "its terminal stopped");
    }

    #[test]
    fn the_longest_waiting_first_card_clocks_kept_and_task_rows_aged_by_their_last_change() {
        use TermStatus::*;
        let mut w = world(&[("a", Some("api"), Running, false), ("b", Some("web"), WaitingInput, false)]);
        let mut x = task("x", TaskStatus::Review);
        x.updated = 5;
        let mut y = task("y", TaskStatus::Review);
        y.updated = 3;
        list(&mut w, "api", "/srv/api", vec![x.clone(), y]);
        let mut since = Since::default();
        build(&w, &mut since, 10);
        // `a` asks later: after `b`, whatever the card order.
        w.cards[0].info.as_mut().unwrap().status = WaitingInput;
        let rows = build(&w, &mut since, 50);
        assert_eq!(keys(&rows), ["b", "a", "#y", "#x"], "the review waiting since longest first");
        assert_eq!(rows.iter().map(|r| r.since).collect::<Vec<_>>(), [10, 50, 3, 5]);
        // A new cause restarts a card's clock.
        w.cards[1].info.as_mut().unwrap().status = Stalled;
        w.cards[1].attention = true;
        let rows = build(&w, &mut since, 80);
        assert_eq!(keys(&rows), ["a", "#y", "#x", "b"]);
        assert_eq!(rows.iter().map(|r| r.since).collect::<Vec<_>>(), [50, 3, 5, 80]);
        // Gone, then back: a new clock.
        w.cards[0].info.as_mut().unwrap().status = Running;
        build(&w, &mut since, 90);
        w.cards[0].info.as_mut().unwrap().status = WaitingInput;
        assert_eq!(build(&w, &mut since, 95)[0].since, 95);
        // A task's age is its own: a restart (a new clock) or a list arriving empty for a moment
        // does not reset it.
        w.task_lists.clear();
        build(&w, &mut since, 100);
        list(&mut w, "api", "/srv/api", vec![x]);
        let rows = build(&w, &mut Since::default(), 200);
        assert_eq!(rows.iter().find(|r| r.task.is_some()).map(|r| r.since), Some(5));
    }

    #[test]
    fn a_card_in_review_shows_once_as_its_task_and_a_waiting_card_names_its_task() {
        use TermStatus::*;
        let mut w = world(&[("agent", Some("api"), WaitingInput, true), ("worker", Some("api"), WaitingInput, false)]);
        let mut review = task("rl", TaskStatus::Review);
        review.session = Some(w.cards[0].session());
        let mut active = task("pdf", TaskStatus::Active);
        active.session = Some(w.cards[1].session());
        list(&mut w, "api", "/srv/api", vec![review, active]);
        let rows = build(&w, &mut Since::default(), 0);
        assert_eq!(keys(&rows), ["worker", "#rl"], "the active task shows as its waiting card only");
        assert_eq!(rows[0].task.as_ref().map(|t| (t.reference.as_str(), t.title.as_str())), Some(("rpdf", "PDF")));
        assert_eq!(rows[1].card.as_deref(), Some("agent"), "the review row carries its card");
        assert_eq!(rows[1].status, Some(WaitingInput));
    }

    #[test]
    fn ready_tasks_by_priority_then_group_then_manual_order_and_blocked_ones_left_out() {
        let mut w = world(&[]);
        let prio = |id: &str, p: Priority| Task { priority: p, ..task(id, TaskStatus::Todo) };
        let mut blocked = prio("blocked", Priority::Urgent);
        blocked.depends_on = vec!["active".into()];
        let mut unblocked = task("unblocked", TaskStatus::Todo);
        unblocked.depends_on = vec!["done".into()];
        let mut removed = task("removed", TaskStatus::Todo);
        removed.removed = Some(1);
        list(
            &mut w,
            "api",
            "/srv/api",
            vec![task("plain", TaskStatus::Todo), blocked, task("active", TaskStatus::Active), unblocked, prio("high", Priority::High), task("done", TaskStatus::Done), removed],
        );
        list(&mut w, "web", "/srv/web", vec![prio("web-urgent", Priority::Urgent), task("web-plain", TaskStatus::Todo), prio("web-high", Priority::High)]);
        let rows = build(&w, &mut Since::default(), 0);
        assert_eq!(keys(&rows), ["#web-urgent", "#high", "#web-high", "#plain", "#unblocked", "#web-plain"]);
        // The blocking task finished: it is ready, first (urgent).
        let tasks = &mut w.task_lists.get_mut("api").unwrap().tasks;
        tasks[2].status = TaskStatus::Done;
        assert_eq!(keys(&build(&w, &mut Since::default(), 0))[0], "#blocked");
    }

    #[test]
    fn rows_from_every_group_and_space_and_a_shared_list_once() {
        use TermStatus::*;
        let mut w = world(&[("api-1", Some("api"), WaitingInput, false), ("web-1", Some("web"), WaitingInput, false), ("free", None, WaitingInput, false)]);
        w.space = Some("web".into());
        // `notes` has no folder: its list (if any) is not read. `api` and a worktree group share one.
        w.projects.push(project("api-wt", Some("/srv/api/.claude/worktrees/x")));
        list(&mut w, "api", "/srv/api", vec![task("shared", TaskStatus::Review)]);
        list(&mut w, "api-wt", "/srv/api", vec![task("shared", TaskStatus::Review)]);
        list(&mut w, "notes", "/notes", vec![task("ignored", TaskStatus::Review)]);
        let rows = build(&w, &mut Since::default(), 0);
        assert_eq!(keys(&rows), ["api-1", "web-1", "free", "#shared"]);
        assert_eq!(rows[3].project.as_deref(), Some("api"), "under the first group showing it");
    }

    #[test]
    fn a_todo_task_with_a_live_card_is_not_ready_and_never_twice() {
        use TermStatus::*;
        let mut w = world(&[("worker", Some("api"), WaitingInput, false)]);
        let mut started = task("pdf", TaskStatus::Todo);
        started.session = Some(w.cards[0].session());
        list(&mut w, "api", "/srv/api", vec![started, task("free", TaskStatus::Todo)]);
        let rows = build(&w, &mut Since::default(), 0);
        assert_eq!(keys(&rows), ["worker", "#free"], "started (or moved back to todo while its card runs): not ready to start");
        assert_eq!(rows[0].task.as_ref().map(|t| t.id.as_str()), Some("pdf"), "the waiting card names it");
        // Its card closed: ready again.
        w.cards.clear();
        assert_eq!(keys(&build(&w, &mut Since::default(), 0)), ["#pdf", "#free"]);
    }

    #[test]
    fn a_review_whose_card_failed_unseen_shows_the_alert_on_its_row() {
        use TermStatus::*;
        let mut w = world(&[("agent", Some("api"), Exited(1), true)]);
        let mut review = task("rl", TaskStatus::Review);
        review.session = Some(w.cards[0].session());
        list(&mut w, "api", "/srv/api", vec![review]);
        let rows = build(&w, &mut Since::default(), 0);
        assert_eq!(keys(&rows), ["#rl"], "once");
        assert!(rows[0].card_alert && rows[0].status == Some(Exited(1)));
        w.cards[0].attention = false;
        assert!(!build(&w, &mut Since::default(), 0)[0].card_alert, "looked at");
    }

    #[test]
    fn the_inputs_change_with_what_the_rows_come_from_only() {
        use TermStatus::*;
        let mut w = world(&[("a", Some("api"), Running, false)]);
        let before = inputs(&w);
        assert_eq!(inputs(&w), before, "stable");
        w.camera.zoom = 2.0;
        w.cards[0].rect = w.cards[0].rect.translate(egui::vec2(5.0, 0.0));
        assert_eq!(inputs(&w), before, "moving around changes nothing");
        for change in [
            |w: &mut World| w.cards[0].info.as_mut().unwrap().status = WaitingInput,
            |w: &mut World| w.cards[0].attention = true,
            |w: &mut World| w.cards[0].info.as_mut().unwrap().tail.push("Continue? [y/N]".into()),
            |w: &mut World| w.cards[0].project = None,
            |w: &mut World| w.task_lists_rev += 1,
            |w: &mut World| w.projects.swap(0, 1),
        ] {
            let mut w2 = world(&[("a", Some("api"), Running, false)]);
            change(&mut w2);
            assert_ne!(inputs(&w2), before);
        }
        w.cards[0].info.as_mut().unwrap().status = Exited(1);
        let one = inputs(&w);
        w.cards[0].info.as_mut().unwrap().status = Exited(2);
        assert_ne!(inputs(&w), one, "the exit code counts");
    }

    #[test]
    fn a_thousand_linked_tasks_are_walked_once_per_shared_list() {
        // Many groups of one repository share a list: built in one walk, each task once.
        let mut w = world(&[]);
        let tasks: Vec<Task> = (0..1000).map(|n| task(&format!("t{n}"), if n % 2 == 0 { TaskStatus::Done } else { TaskStatus::Review })).collect();
        for n in 0..20 {
            let id = format!("wt{n}");
            w.projects.push(project(&id, Some("/srv/api/wt")));
            list(&mut w, &id, "/srv/api", tasks.clone());
        }
        let rows = build(&w, &mut Since::default(), 0);
        assert_eq!(rows.len(), 500, "finished tasks skipped, the shared list once");
        assert!(rows.iter().all(|r| r.project.as_deref() == Some("wt0")));
    }
}
