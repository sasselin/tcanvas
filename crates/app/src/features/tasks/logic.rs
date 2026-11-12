//! What the Tasks panel shows and asks, as pure functions: grouping by status, the manual order,
//! what holds a task up, and the changes its buttons send. No UI.

use protocol::{Priority, Task, TaskOp, TaskStatus};

/// The panel's sections, top to bottom: what needs a look first. The last two start collapsed.
pub const DISPLAY: [TaskStatus; 6] = [TaskStatus::Review, TaskStatus::Active, TaskStatus::Todo, TaskStatus::Inbox, TaskStatus::Done, TaskStatus::Canceled];

/// Tasks not in the trash.
pub fn live(tasks: &[Task]) -> impl Iterator<Item = &Task> {
    tasks.iter().filter(|t| t.removed.is_none())
}

/// The live tasks of each status, in `DISPLAY` order, each in the list's (manual) order. Empty
/// statuses are left out.
pub fn sections(tasks: &[Task]) -> Vec<(TaskStatus, Vec<&Task>)> {
    DISPLAY.iter().map(|s| (*s, live(tasks).filter(|t| t.status == *s).collect::<Vec<_>>())).filter(|(_, v)| !v.is_empty()).collect()
}

/// Live tasks not done nor canceled: the count on the toolbar button.
pub fn open_count(tasks: &[Task]) -> usize {
    live(tasks).filter(|t| !t.status.finished()).count()
}

pub fn find<'a>(tasks: &'a [Task], id: &str) -> Option<&'a Task> {
    tasks.iter().find(|t| t.id == id)
}

/// The live, unfinished tasks `task` waits for, in its own order: what holds it up.
pub fn blocked_by<'a>(task: &Task, tasks: &'a [Task]) -> Vec<&'a Task> {
    task.depends_on.iter().filter_map(|id| find(tasks, id)).filter(|d| d.removed.is_none() && !d.status.finished()).collect()
}

/// An unfinished task that waits for others, all of them done: it can start now.
pub fn ready(task: &Task, tasks: &[Task]) -> bool {
    let waits = task.depends_on.iter().filter_map(|id| find(tasks, id)).any(|d| d.removed.is_none());
    !task.status.finished() && waits && blocked_by(task, tasks).is_empty()
}

/// The change "move up" (or down) sends: before the previous task of its status, or before the
/// one after the next (last of its status when there is none). `None` at the top (bottom).
pub fn move_op(tasks: &[Task], id: &str, up: bool) -> Option<TaskOp> {
    let task = find(tasks, id)?;
    let same: Vec<&Task> = live(tasks).filter(|t| t.status == task.status).collect();
    let i = same.iter().position(|t| t.id == id)?;
    let before = if up {
        Some(same.get(i.checked_sub(1)?)?.id.clone())
    } else {
        same.get(i + 1)?;
        same.get(i + 2).map(|t| t.id.clone())
    };
    Some(TaskOp::Move { task: id.into(), before })
}

/// The tasks `task` could wait for: the other live ones, by reference, with whether that would
/// close a loop (the panel says so before asking the agent, which checks again).
pub fn dependency_choices<'a>(tasks: &'a [Task], task: &str) -> Vec<(&'a Task, bool)> {
    let current = find(tasks, task).map(|t| t.depends_on.clone()).unwrap_or_default();
    live(tasks).filter(|t| t.id != task && !current.contains(&t.id)).map(|t| (t, protocol::task_cycle(tasks, task, &t.id))).collect()
}

/// The mark of a priority in a row (empty for none).
pub fn priority_mark(p: Priority) -> &'static str {
    match p {
        Priority::None => "",
        Priority::Low => "↓",
        Priority::Medium => "!",
        Priority::High => "!!",
        Priority::Urgent => "!!!",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Extra;

    fn task(id: &str, status: TaskStatus, depends_on: &[&str]) -> Task {
        Task {
            id: id.into(),
            reference: format!("r{id}"),
            title: id.to_uppercase(),
            description: String::new(),
            status,
            priority: Priority::None,
            depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
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

    fn ids(v: &[&Task]) -> Vec<String> {
        v.iter().map(|t| t.id.clone()).collect()
    }

    #[test]
    fn sections_follow_the_display_order_and_the_manual_order() {
        use TaskStatus::*;
        let mut gone = task("gone", Todo, &[]);
        gone.removed = Some(1);
        let tasks = vec![task("a", Inbox, &[]), task("b", Todo, &[]), task("c", Review, &[]), gone, task("d", Todo, &[]), task("e", Done, &[]), task("f", Active, &[])];
        let s = sections(&tasks);
        let order: Vec<(TaskStatus, Vec<String>)> = s.iter().map(|(st, v)| (*st, ids(v))).collect();
        assert_eq!(
            order,
            vec![(Review, vec!["c".to_string()]), (Active, vec!["f".into()]), (Todo, vec!["b".into(), "d".into()]), (Inbox, vec!["a".into()]), (Done, vec!["e".into()])],
            "no canceled section, the removed task left out"
        );
        assert_eq!(open_count(&tasks), 5);
    }

    #[test]
    fn blocked_and_ready() {
        use TaskStatus::*;
        let mut removed = task("x", Todo, &[]);
        removed.removed = Some(1);
        let tasks = vec![task("pdf", Active, &[]), task("about", Todo, &["pdf", "x"]), task("done", Done, &[]), task("next", Todo, &["done"]), task("free", Todo, &[]), task("closed", Done, &["pdf"])];
        let about = &tasks[1];
        assert_eq!(ids(&blocked_by(about, &tasks)), vec!["pdf"], "the removed one does not hold it up");
        assert!(!ready(about, &tasks));
        assert!(ready(&tasks[3], &tasks), "all it waits for is done");
        assert!(!ready(&tasks[4], &tasks), "waits for nothing: no mark");
        assert!(!ready(&tasks[5], &tasks), "already done");
        let mut all = tasks.clone();
        all.push(removed);
        let only_removed = task("y", Todo, &["x"]);
        assert!(!ready(&only_removed, &all) && blocked_by(&only_removed, &all).is_empty());
    }

    #[test]
    fn moves_stay_within_the_status() {
        use TaskStatus::*;
        let tasks = vec![task("a", Todo, &[]), task("i", Inbox, &[]), task("b", Todo, &[]), task("c", Todo, &[])];
        let mv = |id: &str, before: Option<&str>| Some(TaskOp::Move { task: id.into(), before: before.map(String::from) });
        assert_eq!(move_op(&tasks, "b", true), mv("b", Some("a")));
        assert_eq!(move_op(&tasks, "a", true), None, "already first");
        assert_eq!(move_op(&tasks, "a", false), mv("a", Some("c")), "after b: before c");
        assert_eq!(move_op(&tasks, "b", false), mv("b", None), "after c: last");
        assert_eq!(move_op(&tasks, "c", false), None, "already last");
        assert_eq!(move_op(&tasks, "i", true), None, "alone in its status");
        assert_eq!(move_op(&tasks, "zz", true), None);
    }

    #[test]
    fn dependency_choices_flag_loops() {
        use TaskStatus::*;
        // b waits for a; c waits for b.
        let tasks = vec![task("a", Todo, &[]), task("b", Todo, &["a"]), task("c", Todo, &["b"]), task("d", Todo, &[])];
        let choices: Vec<(String, bool)> = dependency_choices(&tasks, "a").into_iter().map(|(t, cycle)| (t.id.clone(), cycle)).collect();
        assert_eq!(choices, vec![("b".into(), true), ("c".into(), true), ("d".into(), false)]);
        let choices: Vec<String> = dependency_choices(&tasks, "c").into_iter().map(|(t, _)| t.id.clone()).collect();
        assert_eq!(choices, vec!["a", "d"], "not itself, not what it already waits for");
        assert_eq!((priority_mark(Priority::Urgent), priority_mark(Priority::None)), ("!!!", ""));
    }
}
