//! Tasks: what to do next in the open group's project. The list lives in the agent, outside the
//! repository (one per repository, shared by its worktrees); this slice subscribes the open
//! group's folder (and the folder of a task being edited), shows the side panel and the task
//! editor, and sends small typed changes.
//!
//! Every change waits for the agent's answer: the panel shows the list the agent sends back, so
//! what is on screen is what is on disk, and typed text stays where it was typed until the agent
//! took it. Without the agent the panel is read-only. Removed tasks can be put back (Undo, last
//! removed first) for the rest of the session; the agent keeps them in the list's trash for a
//! week.
//!
//! The editor checks its save against the task's `content_rev` (title, description, priority)
//! as it was when opened or last saved: notes, dependencies, moves and status changes never make
//! it stale. When someone else changed those fields meanwhile, the editor stays open with the
//! user's text and offers to load their version or to overwrite it.

pub mod logic;
pub mod start;
mod starter;

use std::collections::{BTreeMap, HashMap, HashSet};

use egui::{Button, CollapsingHeader, Color32, ComboBox, Id, Key, Modal, Panel, RichText, ScrollArea, Sides, TextEdit, Ui};
use protocol::{AgentCmd, AgentEvent, Author, FoundList, FoundWhy, LinkChange, Priority, Problem, SessionInfo, Task, TaskBoard, TaskHooks, TaskOp, TaskStatus, WorktreeList};

use crate::features::changes::age_label;
use crate::features::shortcuts::keymap::tasks_combo;
use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::widgets;
use crate::shell::world::{CardId, CardTask, Project, ProjectId, World, unix_now};
use logic::{blocked_by, dependency_choices, find, move_op, open_count, priority_mark, ready, sections};
use start::{Blocked, RowAction};

const ERROR: Color32 = Color32::from_rgb(235, 95, 95);
const READY: Color32 = Color32::from_rgb(90, 200, 120);
const QUICK_ADD_ID: &str = "tasks-quick-add";

pub fn priority_color(p: Priority) -> Color32 {
    match p {
        Priority::None | Priority::Low => theme::TEXT_DIM,
        Priority::Medium => Color32::from_rgb(230, 180, 80),
        Priority::High => Color32::from_rgb(240, 140, 60),
        Priority::Urgent => ERROR,
    }
}

/// What an answer from the agent closes.
#[derive(Clone, Debug, PartialEq)]
enum Pending {
    /// A change from the panel: a refusal shows there.
    Panel,
    /// A quick add: its text leaves the field once added.
    Quick { text: String },
    /// A removal: once done, it can be undone.
    Remove { key: ProjectId, task: String, title: String },
    /// One of the editor's saves (`content`: the title, description, priority one).
    Save { content: bool },
    /// A dependency change from the editor: a refusal shows there.
    Editor,
    /// A note from the editor: its text is cleared once added.
    Note { text: String },
    /// A task's links, set by its start or cleared when its worktree went: a refusal shows in the
    /// panel.
    ClearLink { reference: String },
    /// A started task's links (`starter::Starting`): refused, sent again.
    StartLink { task: String },
    /// An automatic status change (`start::Follow`), for the task `task` of the list `list`: a
    /// refusal because the status changed meanwhile is expected (manual wins), others show.
    Auto { list: String, task: String },
}

/// A removal that can be undone.
#[derive(Clone, Debug, PartialEq)]
pub struct Undo {
    pub key: ProjectId,
    pub task: String,
    pub title: String,
}

/// The fields of the editor as they are on disk (as last loaded or saved).
#[derive(Clone, Debug, PartialEq)]
struct Fields {
    title: String,
    description: String,
    priority: Priority,
    status: TaskStatus,
}

impl Fields {
    fn of(t: &Task) -> Self {
        Fields { title: t.title.clone(), description: t.description.clone(), priority: t.priority, status: t.status }
    }
}

/// The task being edited, with the fields as typed.
#[derive(Clone, Debug)]
pub struct Editor {
    pub key: ProjectId,
    pub task: String,
    pub title: String,
    pub description: String,
    pub priority: Priority,
    pub status: TaskStatus,
    /// The task's `content_rev` the fields were loaded (or last saved) at.
    pub rev: u64,
    saved: Fields,
    pub note: String,
    pub error: Option<String>,
    /// Saves not answered yet.
    saving: Vec<u64>,
    /// Someone else changed the content meanwhile: load theirs, or overwrite.
    pub conflict: bool,
    /// Escape with unsaved changes: asking before discarding them.
    pub confirm_discard: bool,
    /// The task is not in the list any more: the text stays, to be copied.
    pub gone: bool,
}

impl Editor {
    fn fields(&self) -> Fields {
        Fields { title: self.title.trim().to_string(), description: self.description.clone(), priority: self.priority, status: self.status }
    }

    /// Typed and not saved.
    pub fn dirty(&self) -> bool {
        let mut saved = self.saved.clone();
        saved.title = saved.title.trim().to_string();
        self.fields() != saved || !self.note.trim().is_empty()
    }

    fn load(&mut self, t: &Task) {
        let f = Fields::of(t);
        (self.title, self.description, self.priority, self.status) = (f.title.clone(), f.description.clone(), f.priority, f.status);
        self.rev = t.content_rev;
        self.saved = f;
        self.conflict = false;
        self.error = None;
    }
}

/// What a click in the panel asks for: done once the frame's board is back in place.
enum Action {
    Open(String),
    Op(TaskOp, Pending),
    Adopt {
        from: String,
        merge: bool,
    },
    /// Start (or resume) a task; `ask`: always show the confirmation.
    Start {
        task: String,
        ask: bool,
    },
    GoTo(CardId),
}

/// Why a task cannot be started now, translated.
pub fn blocked_text(b: &Blocked) -> &'static str {
    match b {
        Blocked::NoFolder => t().start_blocked_no_folder,
        Blocked::AgentOffline => t().start_blocked_offline,
        Blocked::AgentTooOld => t().start_blocked_too_old,
        Blocked::ReadOnly => t().start_blocked_read_only,
        Blocked::Waiting => t().start_blocked_waiting,
        Blocked::Starting => t().start_blocked_starting,
        Blocked::Shell => t().start_blocked_shell,
    }
}

#[derive(Default)]
pub struct Tasks {
    /// Folders subscribed with the agent (key: project id).
    watched: BTreeMap<ProjectId, String>,
    boards: HashMap<ProjectId, TaskBoard>,
    /// The agent keeps task lists; `None` while it is not connected.
    capable: Option<bool>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    quick: String,
    /// A quick add not answered yet.
    quick_pending: Option<u64>,
    /// Give the quick add the keyboard: asked by opening the panel, valid for one frame only.
    focus_quick: bool,
    editor: Option<Editor>,
    /// Removals that can be undone, last one last.
    undo: Vec<Undo>,
    /// The last refusal of a change from the panel, translated.
    error: Option<(ProjectId, String)>,
    /// In the "All" view: the group of the last focused terminal.
    shown: Option<ProjectId>,
    /// Merging another list into a group's, waiting for confirmation.
    merge: Option<(ProjectId, FoundList)>,
    /// Whether coding agents get these tasks (a session start hook, a skill): `None` while no
    /// agent able to say is connected.
    task_hooks: Option<TaskHooks>,
    /// Enabling or disabling it, not answered yet.
    task_hooks_pending: bool,
    /// What failed last time, translated.
    task_hooks_error: Option<String>,
    /// The agent records where tasks are worked on (`TaskOp::SetLinks`).
    task_links: bool,
    /// Each group's repository as the agent lists it (`None`: its folder is outside git; missing:
    /// not known yet).
    repos: BTreeMap<ProjectId, Option<WorktreeList>>,
    /// Worktree folders linked to tasks during this run and not yet seen in a list: a list from
    /// before their creation does not clear those links.
    linked_unseen: HashSet<String>,
    /// Tasks whose worktree link is being cleared.
    clearing: HashSet<String>,
    /// The login shell runs the card's task script (bash or zsh), as the agent says.
    shell_ok: bool,
    /// The start being prepared (its dialogs).
    starter: Option<starter::Starter>,
    /// Starts asked while another one was being prepared: (group, task, ask), in order.
    queued: std::collections::VecDeque<(ProjectId, String, bool)>,
    /// Starts sent, until the list shows their session (by task id).
    starts: HashMap<String, starter::Starting>,
    /// How each task's status follows its agent, by (list key, task id).
    follows: HashMap<(String, String), start::Follow>,
    /// The last marks counted of each card's session.
    sessions: HashMap<String, start::Obs>,
    /// Marks reported before the list showed their session's link: counted once it does.
    unlinked: HashMap<String, start::Obs>,
    /// Sessions of the cards started for tasks during this run: their first observation counts
    /// from nothing (a prompt already there is a new one).
    fresh: HashSet<String>,
}

impl Tasks {
    pub fn board(&self, key: &str) -> Option<&TaskBoard> {
        self.boards.get(key)
    }

    pub fn has_dialog(&self) -> bool {
        self.editor.is_some() || self.merge.is_some() || self.starter.as_ref().is_some_and(|s| s.shown)
    }

    /// A start is being prepared (its template choice or its confirmation is up).
    pub fn starting(&self) -> bool {
        self.starter.is_some()
    }

    /// The key of the list `key` is subscribed to (the repository's main checkout), else `key`.
    fn list_key(&self, key: &str) -> String {
        self.boards.get(key).and_then(|b| b.list.as_ref()).map_or_else(|| key.to_string(), |l| l.key.clone())
    }

    pub fn editor(&self) -> Option<&Editor> {
        self.editor.as_ref()
    }

    pub fn undo(&self) -> &[Undo] {
        &self.undo
    }

    pub fn quick_text(&self) -> &str {
        &self.quick
    }

    /// The group the panel is about: the open one, else (in the "All" view) that of the last
    /// focused terminal.
    pub fn panel_project<'a>(&self, world: &'a World) -> Option<&'a Project> {
        world.space.as_deref().or(self.shown.as_deref()).and_then(|id| world.project(id))
    }

    fn tasks_of(&self, key: &str) -> &[Task] {
        self.boards.get(key).and_then(|b| b.list.as_ref()).map(|l| l.tasks.as_slice()).unwrap_or_default()
    }

    /// Every frame: the agent follows exactly the panel group's list, the edited one, and those
    /// of the groups holding terminals (a task's card shows its task, and its status follows its
    /// agent); the count shows on the toolbar even when the panel is closed. Which task each card
    /// was started for goes to the world.
    pub fn poll(&mut self, world: &mut World, events: &mut Vec<Event>) {
        let mut wanted: BTreeMap<ProjectId, String> = BTreeMap::new();
        if self.capable != Some(false) {
            let edited = self.editor.as_ref().and_then(|e| world.project(&e.key));
            let with_cards = world.projects.iter().filter(|p| world.members(&p.id).next().is_some());
            let starting = self.starter.as_ref().and_then(|s| world.project(&s.project));
            for p in self.panel_project(world).into_iter().chain(edited).chain(with_cards).chain(starting) {
                if let Some(cwd) = &p.cwd {
                    wanted.insert(p.id.clone(), cwd.clone());
                }
            }
        }
        let tasks = card_tasks(&self.boards, world);
        if tasks != world.card_tasks {
            world.card_tasks = tasks;
        }
        let mut cmds = Vec::new();
        self.watched.retain(|key, path| {
            let keep = wanted.get(key) == Some(path);
            if !keep {
                cmds.push(AgentCmd::UnwatchTasks { key: key.clone() });
            }
            keep
        });
        for (key, path) in wanted {
            if !self.watched.contains_key(&key) {
                self.watched.insert(key.clone(), path.clone());
                cmds.push(AgentCmd::WatchTasks { key, path });
            }
        }
        events.extend(cmds.into_iter().map(Event::ToAgent));
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Sends a change of `key`'s list, naming the list it was shown.
    fn send(&mut self, key: &str, op: TaskOp, expect: Option<u64>, pending: Pending, out: &mut Vec<Event>) -> u64 {
        let id = self.next_id();
        self.pending.insert(id, pending);
        let list = self.boards.get(key).and_then(|b| b.list.as_ref()).map(|l| l.key.clone());
        out.push(Event::ToAgent(AgentCmd::EditTasks { id, key: key.to_string(), op, expect, list }));
        id
    }

    fn adopt(&mut self, key: &str, from: &str, merge: bool, out: &mut Vec<Event>) {
        let id = self.next_id();
        self.pending.insert(id, Pending::Panel);
        let list = self.boards.get(key).and_then(|b| b.list.as_ref()).map(|l| l.key.clone());
        out.push(Event::ToAgent(AgentCmd::AdoptTasks { id, key: key.into(), from: from.into(), merge, list }));
    }

    fn open_editor(&mut self, key: &str, task: &str) {
        let Some(t) = find(self.tasks_of(key), task) else { return };
        let saved = Fields::of(t);
        self.editor = Some(Editor {
            key: key.to_string(),
            task: t.id.clone(),
            title: saved.title.clone(),
            description: saved.description.clone(),
            priority: saved.priority,
            status: saved.status,
            rev: t.content_rev,
            saved,
            note: String::new(),
            error: None,
            saving: Vec::new(),
            conflict: false,
            confirm_discard: false,
            gone: false,
        });
    }

    /// Sends what the editor changed since it was loaded or saved: the content checked against
    /// that version (another writer's change is never overwritten unawares), the status as is.
    fn save(&mut self, out: &mut Vec<Event>) {
        let Some(ed) = self.editor.clone() else { return };
        let now = ed.fields();
        let title = Some(now.title.clone()).filter(|x| *x != ed.saved.title);
        let description = Some(now.description.clone()).filter(|d| *d != ed.saved.description);
        let priority = Some(now.priority).filter(|p| *p != ed.saved.priority);
        let mut ids = Vec::new();
        if title.is_some() || description.is_some() || priority.is_some() {
            let op = TaskOp::Edit { task: ed.task.clone(), title, description, priority };
            ids.push(self.send(&ed.key, op, Some(ed.rev), Pending::Save { content: true }, out));
        }
        if now.status != ed.saved.status {
            ids.push(self.send(&ed.key, TaskOp::SetStatus { task: ed.task.clone(), status: now.status, from: None }, None, Pending::Save { content: false }, out));
        }
        match &mut self.editor {
            Some(e) if !ids.is_empty() => {
                e.saving = ids;
                e.error = None;
            }
            _ => self.editor = None,
        }
    }

    fn act(&mut self, world: &World, key: &str, action: Action, out: &mut Vec<Event>) {
        match action {
            Action::Start { task, ask } => self.begin(world, key, &task, ask),
            Action::GoTo(card) => out.push(Event::FlyTo(card)),
            Action::Open(task) => self.open_editor(key, &task),
            Action::Op(op, mut pending) => {
                if let Pending::Remove { key: k, .. } = &mut pending {
                    *k = key.to_string();
                }
                self.error = None;
                let quick = matches!(pending, Pending::Quick { .. });
                let id = self.send(key, op, None, pending, out);
                if quick {
                    self.quick_pending = Some(id);
                }
            }
            Action::Adopt { from, merge } => self.adopt(key, &from, merge, out),
        }
    }

    fn answered(&mut self, id: u64, key: &str, problem: Option<&Problem>, content_rev: Option<u64>, out: &mut Vec<Event>) {
        let Some(pending) = self.pending.remove(&id) else { return };
        let text = problem.map(|p| t().problem(p));
        match pending {
            Pending::Quick { text: sent } => {
                self.quick_pending = None;
                match text {
                    // Typed meanwhile: kept.
                    None if self.quick.trim() == sent => self.quick.clear(),
                    None => {}
                    Some(text) => self.error = Some((key.to_string(), text)),
                }
            }
            Pending::Remove { key, task, title } if problem.is_none() => self.undo.push(Undo { key, task, title }),
            Pending::Panel | Pending::Remove { .. } => {
                if let Some(text) = text {
                    self.error = Some((key.to_string(), text));
                }
            }
            Pending::Save { content } => {
                let Some(ed) = self.editor.as_mut() else { return };
                ed.saving.retain(|s| *s != id);
                match problem {
                    Some(Problem::TaskStale { .. }) => ed.conflict = true,
                    Some(_) => ed.error = text,
                    None if content => {
                        // Saved: this is the version the next save is checked against.
                        if let Some(rev) = content_rev {
                            ed.rev = rev;
                        }
                        let now = ed.fields();
                        (ed.saved.title, ed.saved.description, ed.saved.priority) = (now.title, now.description, now.priority);
                    }
                    None => ed.saved.status = ed.status,
                }
                if ed.saving.is_empty() && ed.error.is_none() && !ed.conflict && !ed.dirty() {
                    self.editor = None;
                }
            }
            Pending::ClearLink { .. } => {
                if let Some(text) = text {
                    self.error = Some((key.to_string(), text));
                }
            }
            Pending::StartLink { task } => self.link_answered(&task, problem, out),
            Pending::Auto { list, task } => {
                if let Some(follow) = self.follows.get_mut(&(list, task)).filter(|_| problem.is_some()) {
                    follow.refused();
                }
                // The status changed meanwhile: what was done by hand stays, nothing to say.
                if let Some(text) = text.filter(|_| !matches!(problem, Some(Problem::TaskStatusChanged { .. }))) {
                    self.error = Some((key.to_string(), text));
                }
            }
            Pending::Editor | Pending::Note { .. } => {
                let Some(ed) = self.editor.as_mut() else { return };
                match (text, pending) {
                    (Some(text), _) => ed.error = Some(text),
                    (None, Pending::Note { text: sent }) if ed.note.trim() == sent => ed.note.clear(),
                    _ => {}
                }
            }
        }
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, out: &mut Vec<Event>) {
        match ev {
            Event::Agent(AgentEvent::Hello { tasks, task_hooks, task_links, shell, .. }) => {
                // A new agent: it follows nothing yet, and what the last one was asked is lost.
                self.capable = Some(*tasks);
                self.task_links = *task_links;
                self.shell_ok = shell.is_none_or(|s| s.runs_task_script());
                self.starts_lost();
                self.reset_follow();
                (self.task_hooks, self.task_hooks_pending, self.task_hooks_error) = (*task_hooks, false, None);
                self.watched.clear();
                self.lost();
            }
            Event::Agent(AgentEvent::TaskHooksChanged { task_hooks, problem }) => {
                (self.task_hooks, self.task_hooks_pending) = (Some(*task_hooks), false);
                self.task_hooks_error = problem.as_ref().map(|p| t().problem(p));
            }
            Event::AgentLost => {
                self.capable = None;
                self.starts_lost();
                self.reset_follow();
                (self.task_hooks, self.task_hooks_pending) = (None, false);
                self.watched.clear();
                self.lost();
            }
            Event::ToggleTasks => {
                world.tasks_open = !world.tasks_open;
                self.focus_quick = world.tasks_open;
                world.touch();
            }
            Event::FocusCard(id) => {
                if let Some(p) = world.card(id).and_then(|c| c.project.clone()) {
                    self.shown = Some(p);
                }
            }
            Event::Agent(AgentEvent::Tasks(board)) => {
                if world.project(&board.key).is_some_and(|p| p.cwd.is_some()) {
                    if let Some(list) = &board.list {
                        // Statuses changed by anyone: what the app follows sees them.
                        for t in list.tasks.iter().filter(|t| t.removed.is_none()) {
                            if let Some(follow) = self.follows.get_mut(&(list.key.clone(), t.id.clone())) {
                                follow.seen(t.status);
                            }
                        }
                    }
                    if let Some(list) = &board.list {
                        self.starts_seen(&list.tasks);
                        let shown: HashSet<String> = list.tasks.iter().filter(|t| t.worktree.is_some()).map(|t| t.id.clone()).collect();
                        self.clearing.retain(|id| shown.contains(id));
                    }
                    self.boards.insert(board.key.clone(), (**board).clone());
                    self.reconcile(&board.key, out);
                    self.follow_unlinked(out);
                }
            }
            Event::Agent(AgentEvent::Worktrees(list)) => {
                if world.project(&list.key).is_some_and(|p| p.cwd.is_some()) {
                    if !list.worktrees.is_empty() {
                        for path in start::worktree_paths(list) {
                            self.linked_unseen.remove(&path);
                        }
                        self.repos.insert(list.key.clone(), Some(list.clone()));
                        self.reconcile(&list.key, out);
                        self.follow_branches(&list.key, out);
                    } else if !matches!(self.repos.get(&list.key), Some(Some(_))) {
                        // Outside git. A group known to be in a repository is never downgraded by an
                        // empty list (git failing for a moment): it keeps its last list.
                        self.repos.insert(list.key.clone(), None);
                    }
                }
            }
            Event::TaskWorktreePlanned(plan) => self.starter_planned(plan),
            Event::TaskWorktreeCreated { task, path, branch, .. } => self.worktree_created(task, path, branch, out),
            Event::TaskWorktreeFailed { project, task, problem } => self.worktree_failed(world, project, task, problem),
            Event::Agent(AgentEvent::Session(info)) => self.follow_session(info, out),
            Event::Agent(AgentEvent::TasksEdited { id, key, problem, content_rev, .. }) => self.answered(*id, key, problem.as_ref(), *content_rev, out),
            Event::Agent(AgentEvent::TasksNotice { key, problem }) => {
                if world.project(key).is_some() {
                    out.push(Event::Inform { title: t().tasks_notice_title.into(), body: t().problem(problem) });
                }
            }
            Event::SetProjectCwd(id, _) | Event::RemoveProject(id) => {
                self.boards.remove(id);
                self.repos.remove(id);
                self.undo.retain(|u| &u.key != id);
                if let Some(ed) = self.editor.as_mut().filter(|e| &e.key == id) {
                    // Kept open with its text, to be copied.
                    ed.gone = true;
                }
            }
            _ => {}
        }
    }

    /// The live task the session `session` was started for, with its group and list key: the first
    /// group showing it (groups of one repository share its list).
    fn task_of_session(&self, session: &str) -> Option<(ProjectId, String, Task)> {
        self.boards.iter().find_map(|(project, board)| {
            let list = board.list.as_ref()?;
            let mut linked: Vec<&Task> = list.tasks.iter().filter(|t| t.removed.is_none() && t.session.as_deref() == Some(session)).collect();
            linked.sort_by_key(|t| t.status.finished());
            linked.first().map(|t| (project.clone(), list.key.clone(), (*t).clone()))
        })
    }

    /// An automatic status change, naming the status it expects.
    fn send_move(&mut self, project: &str, list: &str, task: &str, (from, to): start::Move, out: &mut Vec<Event>) {
        let op = TaskOp::SetStatus { task: task.to_string(), status: to, from: Some(from) };
        self.send(project, op, None, Pending::Auto { list: list.to_string(), task: task.to_string() }, out);
    }

    /// The agent changed: what the app followed starts over (no signal is computed from a status
    /// change that never applied, nor from counters of another agent's run).
    fn reset_follow(&mut self) {
        self.follows.clear();
        self.sessions.clear();
        self.unlinked.clear();
        // From now on, the first report of every session is a baseline only: a card started
        // before does not count its marks from nothing again.
        self.fresh.clear();
    }

    /// A session's report: the signals its marks give (a prompt, a turn end) move its task's
    /// status by the rules (`start::Follow`). Only with an agent that records task links.
    fn follow_session(&mut self, info: &SessionInfo, out: &mut Vec<Event>) {
        if !self.task_links {
            return;
        }
        let Some(now) = start::observe(info) else { return };
        self.follow_marks(&info.session, now, out);
    }

    /// The marks of a session's report, against the last ones counted. Counted only once its task
    /// is known: a report coming before the list shows the session's link waits for it.
    fn follow_marks(&mut self, session: &str, now: start::Obs, out: &mut Vec<Event>) {
        let prev = match self.sessions.get(session) {
            Some(prev) => prev.clone(),
            // Started here: from nothing. Otherwise (seen first now, after a restart or a new
            // agent...) a baseline only.
            None if self.fresh.contains(session) => start::Obs::default(),
            None => {
                self.sessions.insert(session.to_string(), now);
                return;
            }
        };
        let Some((project, list, task)) = self.task_of_session(session) else {
            self.unlinked.insert(session.to_string(), now);
            return;
        };
        self.unlinked.remove(session);
        let signals = start::signals(&prev, &now);
        self.sessions.insert(session.to_string(), now);
        let branch = start::branch_state(self.repos.get(&project).and_then(|r| r.as_ref()), task.worktree.as_deref());
        let instant = std::time::Instant::now();
        for signal in signals {
            let follow = self.follows.entry((list.clone(), task.id.clone())).or_default();
            if let Some(change) = follow.signal(task.status, signal, &branch, instant) {
                self.send_move(&project, &list, &task.id, change, out);
            }
        }
        self.follows.entry((list, task.id)).or_default().reported(&branch);
    }

    /// The list changed: reports that waited for their session's link are counted now.
    fn follow_unlinked(&mut self, out: &mut Vec<Event>) {
        if !self.task_links {
            return;
        }
        for (session, now) in std::mem::take(&mut self.unlinked) {
            self.follow_marks(&session, now, out);
        }
    }

    /// A new state of a group's repository: a turn end's commit may show up in it.
    fn follow_branches(&mut self, project: &str, out: &mut Vec<Event>) {
        if !self.task_links {
            return;
        }
        let Some(list) = self.boards.get(project).and_then(|b| b.list.clone()) else { return };
        let repo = self.repos.get(project).and_then(|r| r.clone());
        let instant = std::time::Instant::now();
        for task in list.tasks.iter().filter(|t| t.removed.is_none()) {
            let Some(follow) = self.follows.get_mut(&(list.key.clone(), task.id.clone())).filter(|f| f.awaits_branch()) else { continue };
            if let Some(change) = follow.branch(task.status, &start::branch_state(repo.as_ref(), task.worktree.as_deref()), instant) {
                self.send_move(project, &list.key, &task.id, change, out);
            }
        }
    }

    /// Once both the group's list and its repository are known: a task linked to a worktree the
    /// repository no longer has (the cleanup removed it, even while the app was closed) loses that
    /// link, never the task, its branch or its status. A folder linked during this run and not yet
    /// seen in any list is left alone (the list may predate it).
    fn reconcile(&mut self, project: &str, out: &mut Vec<Event>) {
        if !self.task_links {
            return;
        }
        let (Some(Some(repo)), Some(tasks)) = (self.repos.get(project), self.boards.get(project).and_then(|b| b.list.as_ref()).map(|l| l.tasks.clone())) else { return };
        let paths = start::worktree_paths(repo);
        for task in tasks.iter().filter(|t| t.removed.is_none()) {
            if self.clearing.contains(&task.id) || self.starts.contains_key(&task.id) {
                continue;
            }
            let Some(w) = task.worktree.as_deref() else { continue };
            if paths.iter().any(|p| start::same_path(p, w)) || self.linked_unseen.iter().any(|p| start::same_path(p, w)) {
                continue;
            }
            self.clearing.insert(task.id.clone());
            let op = TaskOp::SetLinks { task: task.id.clone(), worktree: Some(LinkChange::Clear), branch: None, session: None };
            self.send(project, op, None, Pending::ClearLink { reference: task.reference.clone() }, out);
        }
    }

    /// Answers that will never come: nothing waits for them any more, typed text stays.
    fn lost(&mut self) {
        self.pending.clear();
        self.quick_pending = None;
        if let Some(ed) = &mut self.editor
            && !ed.saving.is_empty()
        {
            ed.saving.clear();
            ed.error = Some(t().tasks_agent_offline.into());
        }
    }

    /// Top bar button: opens or closes the panel, with the count of open tasks.
    pub fn toolbar_ui(&mut self, ui: &mut Ui, world: &World, events: &mut Vec<Event>) {
        let Some(p) = self.panel_project(world) else { return };
        let n = open_count(self.tasks_of(&p.id));
        let on = world.tasks_open;
        let text = RichText::new((t().tasks_button)(n)).color(if on { theme::CARD_BG } else { theme::TEXT });
        let button = Button::selectable(on, text).fill(if on { theme::TEXT } else { Color32::TRANSPARENT });
        let hint = format!("{} · {}", t().tasks_button_hint, tasks_combo().format());
        if ui.add(button).on_hover_text(hint).clicked() {
            events.push(Event::ToggleTasks);
        }
    }

    /// Side panel, on the left of the canvas. Call before the canvas (it takes space from it).
    pub fn panel_ui(&mut self, ui: &mut Ui, world: &World, events: &mut Vec<Event>) {
        // Valid for this frame only: never later, when the user may be typing in a terminal.
        let focus = std::mem::take(&mut self.focus_quick);
        if !world.tasks_open {
            return;
        }
        let Some(project) = self.panel_project(world).cloned() else { return };
        // Borrowed out for the frame (no copy), put back after.
        let board = self.boards.remove(&project.id);
        let mut actions = Vec::new();
        Panel::left("tasks").resizable(true).default_size(360.0).min_size(260.0).show(ui, |ui| self.panel_body(ui, world, &project, board.as_ref(), focus, &mut actions, events));
        if let Some(board) = board {
            self.boards.entry(project.id.clone()).or_insert(board);
        }
        for action in actions {
            self.act(world, &project.id, action, events);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn panel_body(&mut self, ui: &mut Ui, world: &World, project: &Project, board: Option<&TaskBoard>, focus: bool, actions: &mut Vec<Action>, events: &mut Vec<Event>) {
        ui.add_space(6.0);
        Sides::new().shrink_left().show(
            ui,
            |ui| ui.label(RichText::new((t().tasks_title)(&project.name)).strong().color(theme::rgb(project.color))),
            |ui| {
                if ui.small_button("×").on_hover_text(t().close_panel).clicked() {
                    events.push(Event::ToggleTasks);
                }
            },
        );
        if project.cwd.is_none() {
            ui.label(RichText::new(t().tasks_unlinked).color(theme::TEXT_DIM));
            return;
        }
        if self.capable == Some(false) {
            ui.label(RichText::new(t().tasks_agent_too_old).color(theme::TEXT_DIM));
            return;
        }
        if self.capable.is_none() {
            ui.label(RichText::new(t().tasks_agent_offline).color(theme::TEXT_DIM));
        }
        self.task_hooks_bar(ui, events);
        let Some(board) = board else {
            ui.label(RichText::new(t().waiting_for_agent).color(theme::TEXT_DIM));
            return;
        };
        if let Some(p) = &board.problem {
            ui.label(RichText::new(format!("⚠ {}", t().problem(p))).color(ERROR));
        }
        let writable = self.capable == Some(true) && !board.read_only && board.list.is_some();
        for found in &board.found {
            self.found_bar(ui, &project.id, board, found, writable, actions);
        }
        let Some(list) = &board.list else { return };
        self.undo_bar(ui, &project.id, writable, actions);
        if let Some((_, text)) = self.error.clone().filter(|(k, _)| *k == project.id) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(text).color(ERROR));
                if ui.small_button("×").clicked() {
                    self.error = None;
                }
            });
        }
        if writable {
            self.quick_add(ui, focus, actions);
        }
        ui.separator();
        if logic::live(&list.tasks).next().is_none() {
            ui.label(RichText::new(t().tasks_empty).color(theme::TEXT_DIM));
            return;
        }
        let base = self.readiness(project, writable);
        let launching: Vec<String> = self.starts.keys().cloned().collect();
        let blocked = move |t: &Task| start::blocked(start::Readiness { starting: launching.contains(&t.id), ..base });
        let ctx = StartCtx { world, blocked: &blocked };
        ScrollArea::vertical().id_salt("tasks-list").auto_shrink([false, false]).show(ui, |ui| {
            for (status, tasks) in sections(&list.tasks) {
                let header = format!("{} ({})", t().task_status(status), tasks.len());
                CollapsingHeader::new(RichText::new(header).small().strong().color(theme::TEXT_DIM)).id_salt(("tasks-section", status as u8)).default_open(!status.finished()).show(ui, |ui| {
                    for task in tasks {
                        actions.extend(row(ui, task, &list.tasks, writable, &ctx));
                    }
                });
            }
        });
    }

    /// Whether coding agents see these tasks, and the action that changes it (opt-in, separate
    /// from the status bar's hooks). Nothing to offer without a coding agent, or an older agent.
    fn task_hooks_bar(&mut self, ui: &mut Ui, events: &mut Vec<Event>) {
        let (text, action, enabled) = match self.task_hooks {
            Some(TaskHooks::Missing) => (t().task_hooks_missing, t().task_hooks_enable, true),
            Some(TaskHooks::Installed) => (t().task_hooks_on, t().task_hooks_disable, false),
            Some(TaskHooks::Unsupported) | None => return,
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(text).small().color(theme::TEXT_DIM)).on_hover_text(t().task_hooks_hint);
            if ui.add_enabled(!self.task_hooks_pending, Button::new(RichText::new(action).small())).on_hover_text(t().task_hooks_hint).clicked() {
                self.task_hooks_pending = true;
                self.task_hooks_error = None;
                events.push(Event::ToAgent(AgentCmd::SetTaskHooks { enabled }));
            }
        });
        if let Some(e) = &self.task_hooks_error {
            ui.label(RichText::new(e).small().color(ERROR));
        }
    }

    /// Another list that may be this project's: bring its tasks here. A merge asks first, and so
    /// does a list matched only by its history (it may be another clone).
    fn found_bar(&mut self, ui: &mut Ui, key: &str, board: &TaskBoard, found: &FoundList, writable: bool, actions: &mut Vec<Action>) {
        ui.label(RichText::new((t().tasks_found)(&found.key, found.tasks)).color(priority_color(Priority::Medium)));
        if !writable {
            return;
        }
        let here = board.list.as_ref().map_or(0, |l| l.live());
        if found.why == FoundWhy::SameHistory {
            if ui.button(if here == 0 { t().tasks_adopt } else { t().tasks_merge }).clicked() {
                self.merge = Some((key.to_string(), found.clone()));
            }
        } else if here == 0 {
            if ui.button(t().tasks_adopt).clicked() {
                actions.push(Action::Adopt { from: found.key.clone(), merge: false });
            }
        } else if ui.button(t().tasks_merge).clicked() {
            self.merge = Some((key.to_string(), found.clone()));
        }
    }

    fn undo_bar(&mut self, ui: &mut Ui, key: &str, writable: bool, actions: &mut Vec<Action>) {
        let Some(at) = self.undo.iter().rposition(|u| u.key == key) else { return };
        let undo = self.undo[at].clone();
        let more = self.undo.iter().filter(|u| u.key == key).count() - 1;
        ui.horizontal_wrapped(|ui| {
            let text = if more > 0 { format!("{} (+{more})", (t().task_removed)(&undo.title)) } else { (t().task_removed)(&undo.title) };
            ui.label(RichText::new(text).color(theme::TEXT_DIM));
            if ui.add_enabled(writable, Button::new(t().task_undo).small()).clicked() {
                self.undo.remove(at);
                actions.push(Action::Op(TaskOp::Restore { task: undo.task.clone() }, Pending::Panel));
            }
        });
    }

    /// One line: Enter adds an inbox task; its text leaves the field once the agent added it.
    fn quick_add(&mut self, ui: &mut Ui, focus: bool, actions: &mut Vec<Action>) {
        let edit = ui.add(TextEdit::singleline(&mut self.quick).hint_text(t().tasks_quick_add_hint).desired_width(f32::INFINITY).id(Id::unique(QUICK_ADD_ID)));
        if focus {
            edit.request_focus();
        }
        if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            let title = self.quick.trim().to_string();
            if !title.is_empty() && self.quick_pending.is_none() {
                let op = TaskOp::Add { title: title.clone(), description: String::new(), priority: Priority::None, status: TaskStatus::Inbox };
                actions.push(Action::Op(op, Pending::Quick { text: title }));
            }
            edit.request_focus();
        }
    }

    /// The task editor (a modal window), the merge confirmation, and a start's dialogs. Call after
    /// the panels.
    pub fn dialogs(&mut self, ctx: &egui::Context, world: &World, events: &mut Vec<Event>) {
        if self.starter.is_some() {
            self.starter_ui(ctx, world, events);
            return;
        }
        if let Some((key, found)) = self.merge.clone() {
            let list = self.boards.get(&key).and_then(|b| b.list.as_ref());
            let here = list.map_or(0, |l| l.live());
            let mut body = (t().tasks_merge_body)(&found.key, found.tasks, here);
            if found.why == FoundWhy::SameHistory {
                let this = list.map(|l| l.key.clone()).unwrap_or_default();
                body = format!("{}\n\n{body}", (t().tasks_same_history)(&found.key, &this));
            }
            match widgets::confirm_by_click(ctx, "tasks-merge", t().tasks_merge_title, &body, t().tasks_merge_ok, true) {
                Some(true) => {
                    self.merge = None;
                    self.adopt(&key, &found.key, here > 0, events);
                }
                Some(false) => self.merge = None,
                None => {}
            }
            return;
        }
        let Some(key) = self.editor.as_ref().map(|e| e.key.clone()) else { return };
        // Borrowed out for the frame (no copy), put back after.
        let board = self.boards.remove(&key);
        let start = self.editor_ui(ctx, world, board.as_ref(), events);
        if let Some(board) = board {
            self.boards.entry(key.clone()).or_insert(board);
        }
        if let Some(action) = start {
            self.editor = None;
            self.act(world, &key, action, events);
        }
    }

    /// The editor; its start button's action (the editor then closes).
    fn editor_ui(&mut self, ctx: &egui::Context, world: &World, board: Option<&TaskBoard>, events: &mut Vec<Event>) -> Option<Action> {
        let empty: &[Task] = &[];
        let tasks = board.and_then(|b| b.list.as_ref()).map_or(empty, |l| l.tasks.as_slice());
        let writable = self.capable == Some(true) && board.is_some_and(|b| !b.read_only && b.list.is_some());
        let base = self.editor.as_ref().and_then(|e| world.project(&e.key)).map(|p| self.readiness(p, writable));
        let launching: Vec<String> = self.starts.keys().cloned().collect();
        let blocked = move |t: &Task| match base {
            Some(base) => start::blocked(start::Readiness { starting: launching.contains(&t.id), ..base }),
            None => Some(Blocked::NoFolder),
        };
        let start_ctx = StartCtx { world, blocked: &blocked };
        let mut start_action = None;
        let e = self.editor.as_mut()?;
        let task = find(tasks, &e.task).filter(|t| t.removed.is_none());
        // Gone (removed elsewhere, list unreadable for a moment...): the text stays; back if it is.
        e.gone = task.is_none();
        let reference = task.map_or_else(|| "?".to_string(), |t| t.reference.clone());
        let can_save = writable && !e.gone && e.saving.is_empty() && !e.conflict;
        let mut actions: Vec<(TaskOp, Pending)> = Vec::new();
        let (mut save, mut close, mut overwrite) = (false, false, false);
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if e.confirm_discard || !e.dirty() {
                close = !e.confirm_discard;
                e.confirm_discard = false;
            } else {
                e.confirm_discard = true;
            }
        }
        // A click outside does nothing: only Close, Save or Escape leave.
        Modal::new(Id::unique("task-editor")).show(ctx, |ui| {
            ui.set_width(540.0);
            ui.heading((t().task_editor_title)(&reference));
            if e.gone {
                ui.label(RichText::new(t().task_gone).color(ERROR));
            }
            ui.add_space(4.0);
            ui.label(RichText::new(t().task_title_label).color(theme::TEXT_DIM));
            ui.add(TextEdit::singleline(&mut e.title).desired_width(f32::INFINITY));
            ui.label(RichText::new(t().task_description_label).color(theme::TEXT_DIM));
            ui.add(TextEdit::multiline(&mut e.description).desired_rows(5).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                ui.label(RichText::new(t().task_priority_label).color(theme::TEXT_DIM));
                ComboBox::from_id_salt("task-priority").selected_text(t().priority(e.priority)).show_ui(ui, |ui| {
                    for p in Priority::ALL {
                        ui.selectable_value(&mut e.priority, p, t().priority(p));
                    }
                });
                ui.add_space(12.0);
                ui.label(RichText::new(t().task_status_label).color(theme::TEXT_DIM));
                ComboBox::from_id_salt("task-status").selected_text(t().task_status(e.status)).show_ui(ui, |ui| {
                    for s in TaskStatus::ALL {
                        ui.selectable_value(&mut e.status, s, t().task_status(s));
                    }
                });
                // Started from what is saved: not while the fields hold unsaved text.
                if let Some(task) = task {
                    ui.add_space(12.0);
                    let dirty = e.dirty();
                    ui.add_enabled_ui(!dirty, |ui| start_action = start_button(ui, task, &start_ctx)).response.on_disabled_hover_text(t().task_start_save_first);
                }
            });
            if let Some(branch) = task.and_then(|t| t.branch.as_deref()) {
                let place = task.and_then(|t| t.worktree.as_deref()).map_or_else(|| branch.to_string(), |w| format!("{branch} · {w}"));
                ui.label(RichText::new(format!("⎇ {place}")).small().color(theme::TEXT_DIM));
            }
            if e.conflict {
                ui.add_space(4.0);
                ui.label(RichText::new(t().task_conflict).color(ERROR));
                if let Some(theirs) = task {
                    ui.label(RichText::new((t().task_their_version)(&theirs.title)).color(theme::TEXT_DIM));
                    if !theirs.description.is_empty() {
                        ScrollArea::vertical().id_salt("task-theirs").max_height(80.0).show(ui, |ui| ui.label(RichText::new(&theirs.description).color(theme::TEXT_DIM)));
                    }
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(task.is_some(), Button::new(t().task_load_theirs)).clicked()
                        && let Some(theirs) = task
                    {
                        e.load(theirs);
                    }
                    if ui.add_enabled(task.is_some() && writable, Button::new(t().task_overwrite)).clicked() {
                        overwrite = true;
                    }
                });
            }
            if let Some(task) = task {
                ui.separator();
                ui.label(RichText::new(t().task_dependencies_label).strong().color(theme::TEXT_DIM));
                if task.depends_on.is_empty() {
                    ui.label(RichText::new(t().task_no_dependencies).color(theme::TEXT_DIM));
                }
                for dep in task.depends_on.iter().filter_map(|id| find(tasks, id)).filter(|d| d.removed.is_none()) {
                    ui.horizontal(|ui| {
                        let color = if dep.status.finished() { theme::TEXT_DIM } else { theme::TEXT };
                        ui.label(RichText::new(format!("#{} {}", dep.reference, dep.title)).color(color));
                        ui.label(RichText::new(t().task_status(dep.status)).small().color(theme::TEXT_DIM));
                        if ui.add_enabled(writable, Button::new((t().task_remove_dependency)(&dep.reference)).small()).clicked() {
                            actions.push((TaskOp::RemoveDependency { task: task.id.clone(), on: dep.id.clone() }, Pending::Editor));
                        }
                    });
                }
                if writable {
                    ComboBox::from_id_salt("task-add-dependency").selected_text(t().task_add_dependency).width(320.0).show_ui(ui, |ui| {
                        for (candidate, cycle) in dependency_choices(tasks, &task.id) {
                            if ui.selectable_label(false, format!("#{} {}", candidate.reference, candidate.title)).clicked() {
                                if cycle {
                                    // Said right away; the agent would refuse it the same way.
                                    e.error = Some(t().problem(&Problem::TaskCycle { task: task.reference.clone(), on: candidate.reference.clone() }));
                                } else {
                                    actions.push((TaskOp::AddDependency { task: task.id.clone(), on: candidate.id.clone() }, Pending::Editor));
                                }
                            }
                        }
                    });
                }
                ui.separator();
                ui.label(RichText::new(t().task_notes_label).strong().color(theme::TEXT_DIM));
                if task.notes.is_empty() {
                    ui.label(RichText::new(t().task_no_notes).color(theme::TEXT_DIM));
                }
                let now = unix_now();
                ScrollArea::vertical().id_salt("task-notes").max_height(160.0).show(ui, |ui| {
                    for note in &task.notes {
                        let who = match note.author {
                            Author::User => t().task_note_by_user,
                            Author::Agent => t().task_note_by_agent,
                        };
                        ui.label(RichText::new(format!("{who} · {}", age_label(now, note.at))).small().color(theme::TEXT_DIM));
                        ui.label(RichText::new(&note.text).color(theme::TEXT));
                    }
                });
            }
            ui.add(TextEdit::multiline(&mut e.note).hint_text(t().task_note_hint).desired_rows(2).desired_width(f32::INFINITY));
            if ui.add_enabled(writable && task.is_some() && !e.note.trim().is_empty(), Button::new(t().task_add_note)).clicked() {
                let text = e.note.trim().to_string();
                actions.push((TaskOp::AddNote { task: e.task.clone(), text: text.clone(), author: Author::User }, Pending::Note { text }));
            }
            if let Some(err) = &e.error {
                ui.add_space(4.0);
                ui.label(RichText::new(err).color(ERROR));
            }
            ui.add_space(6.0);
            if e.confirm_discard {
                ui.label(RichText::new(t().task_discard_question).strong());
                ui.horizontal(|ui| {
                    if ui.button(t().task_discard).clicked() {
                        close = true;
                    }
                    if ui.button(t().task_keep_editing).clicked() {
                        e.confirm_discard = false;
                    }
                });
                return;
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(can_save, Button::new(t().task_save)).clicked() {
                    save = true;
                }
                if ui.button(t().task_close).clicked() {
                    if e.dirty() {
                        e.confirm_discard = true;
                    } else {
                        close = true;
                    }
                }
                if !e.saving.is_empty() {
                    ui.label(RichText::new(t().task_saving).color(theme::TEXT_DIM));
                }
            });
        });
        if close {
            self.editor = None;
            return None;
        }
        if overwrite && let Some(theirs) = task {
            // Keep mine: checked against their version now, saved again.
            if let Some(e) = self.editor.as_mut() {
                e.rev = theirs.content_rev;
                e.saved.title = theirs.title.clone();
                e.saved.description = theirs.description.clone();
                e.saved.priority = theirs.priority;
                e.conflict = false;
            }
            save = true;
        }
        let key = self.editor.as_ref().map(|e| e.key.clone()).unwrap_or_default();
        for (op, pending) in actions {
            if let Some(e) = self.editor.as_mut() {
                e.error = None;
            }
            self.send(&key, op, None, pending, events);
        }
        if save {
            self.save(events);
        }
        start_action
    }
}

/// The task each card was started for: the cards whose session a live task records (an
/// unfinished one first, when two do).
fn card_tasks(boards: &HashMap<ProjectId, TaskBoard>, world: &World) -> BTreeMap<CardId, CardTask> {
    let mut out: BTreeMap<CardId, (bool, CardTask)> = BTreeMap::new();
    for list in boards.values().filter_map(|b| b.list.as_ref()) {
        for t in list.tasks.iter().filter(|t| t.removed.is_none()) {
            let Some(card) = t.session.as_deref().and_then(|s| world.card_by_session(s)) else { continue };
            let finished = t.status.finished();
            if out.get(&card.id).is_none_or(|(f, _)| *f && !finished) {
                out.insert(card.id.clone(), (finished, CardTask { reference: t.reference.clone(), title: start::visible_text(&t.title) }));
            }
        }
    }
    out.into_iter().map(|(k, (_, v))| (k, v)).collect()
}

/// What a row may do about starting its task: the action, and why it cannot go now.
struct StartCtx<'a> {
    world: &'a World,
    blocked: &'a dyn Fn(&Task) -> Option<Blocked>,
}

/// The start button of a task (`Start`, `Resume` or `Go to`), disabled with its reason.
fn start_button(ui: &mut Ui, task: &Task, ctx: &StartCtx) -> Option<Action> {
    let action = start::row_action(task, ctx.world);
    let (label, hint) = match &action {
        RowAction::Start => (t().task_start, t().task_start_hint),
        RowAction::Resume => (t().task_resume, t().task_resume_hint),
        RowAction::GoTo(_) => (t().task_go_to, t().task_go_to_hint),
        RowAction::None => return None,
    };
    let blocked = match action {
        RowAction::GoTo(_) => None,
        _ => (ctx.blocked)(task),
    };
    let hint = blocked.as_ref().map_or(hint, blocked_text);
    let clicked = ui.add_enabled(blocked.is_none(), Button::new(RichText::new(label).small())).on_hover_text(hint).on_disabled_hover_text(hint).clicked();
    if matches!(action, RowAction::Resume) {
        ui.label(RichText::new(t().task_card_closed).small().color(theme::TEXT_DIM));
    }
    clicked.then(|| match action {
        RowAction::GoTo(card) => Action::GoTo(card),
        _ => Action::Start { task: task.id.clone(), ask: false },
    })
}

/// One task row: reference, priority mark, title (click: edit; right-click: actions), then what
/// holds it up (or "ready"), its notes and the actions menu.
fn row(ui: &mut Ui, task: &Task, tasks: &[Task], writable: bool, start_ctx: &StartCtx) -> Vec<Action> {
    let mut actions = Vec::new();
    let (left, right) = Sides::new().shrink_left().show(
        ui,
        |ui| {
            let mut pick = None;
            ui.label(RichText::new(format!("#{}", task.reference)).monospace().color(theme::TEXT_DIM));
            let mark = priority_mark(task.priority);
            if !mark.is_empty() {
                ui.label(RichText::new(mark).strong().color(priority_color(task.priority))).on_hover_text(t().priority(task.priority));
            }
            let color = if task.status.finished() { theme::TEXT_DIM } else { theme::TEXT };
            let title = ui.add(Button::new(RichText::new(&task.title).color(color)).frame(false).truncate()).on_hover_text(&task.title);
            if title.clicked() {
                pick = Some(Action::Open(task.id.clone()));
            }
            if writable {
                title.context_menu(|ui| {
                    if let Some(a) = row_menu(ui, task, tasks, start_ctx) {
                        pick = Some(a);
                    }
                });
            }
            pick
        },
        |ui| {
            let mut pick = None;
            if writable {
                ui.menu_button("…", |ui| pick = row_menu(ui, task, tasks, start_ctx)).response.on_hover_text(t().task_actions);
            }
            // Shown read-only too: disabled, with the reason.
            if let Some(a) = start_button(ui, task, start_ctx) {
                pick = Some(a);
            }
            if let Some(branch) = &task.branch {
                let hover = task.worktree.as_deref().map_or_else(|| branch.clone(), |w| format!("{branch}\n{w}"));
                ui.label(RichText::new(format!("⎇ {}", crate::shell::widgets::elide_middle(branch, 16))).small().color(theme::TEXT_DIM)).on_hover_text(hover);
            }
            if !task.notes.is_empty() {
                ui.label(RichText::new((t().task_notes)(task.notes.len())).small().color(theme::TEXT_DIM));
            }
            let blockers = blocked_by(task, tasks);
            if !blockers.is_empty() && !task.status.finished() {
                let refs = blockers.iter().map(|b| b.reference.as_str()).collect::<Vec<_>>().join(", #");
                let titles = blockers.iter().map(|b| format!("#{} {}", b.reference, b.title)).collect::<Vec<_>>().join("\n");
                ui.label(RichText::new((t().task_waits_for)(&refs)).small().color(priority_color(Priority::Medium))).on_hover_text(titles);
            } else if ready(task, tasks) {
                ui.label(RichText::new(t().task_ready).small().color(READY)).on_hover_text(t().task_ready_hint);
            }
            pick
        },
    );
    actions.extend(left);
    actions.extend(right);
    actions
}

/// The actions on one task: start (always asking first), order, status, removal.
fn row_menu(ui: &mut Ui, task: &Task, tasks: &[Task], start_ctx: &StartCtx) -> Option<Action> {
    let mut pick = None;
    if matches!(start::row_action(task, start_ctx.world), RowAction::Start | RowAction::Resume) {
        let blocked = (start_ctx.blocked)(task);
        let button = ui.add_enabled(blocked.is_none(), Button::new(t().task_start_options));
        if let Some(b) = &blocked {
            button.clone().on_disabled_hover_text(blocked_text(b));
        }
        if button.clicked() {
            pick = Some(Action::Start { task: task.id.clone(), ask: true });
        }
        ui.separator();
    }
    for (up, label) in [(true, t().task_move_up), (false, t().task_move_down)] {
        let op = move_op(tasks, &task.id, up);
        if ui.add_enabled(op.is_some(), Button::new(label)).clicked()
            && let Some(op) = op
        {
            pick = Some(Action::Op(op, Pending::Panel));
        }
    }
    ui.separator();
    for status in logic::DISPLAY.iter().rev().filter(|s| **s != task.status) {
        if ui.button((t().task_move_to)(t().task_status(*status))).clicked() {
            pick = Some(Action::Op(TaskOp::SetStatus { task: task.id.clone(), status: *status, from: None }, Pending::Panel));
        }
    }
    ui.separator();
    if ui.button(t().task_remove).clicked() {
        pick = Some(Action::Op(TaskOp::Remove { task: task.id.clone() }, Pending::Remove { key: String::new(), task: task.id.clone(), title: task.title.clone() }));
    }
    if pick.is_some() {
        ui.close();
    }
    pick
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect, Vec2};
    use protocol::{Extra, TaskList};

    fn project(id: &str, cwd: Option<&str>) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            color: [0; 3],
            rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)),
            cwd: cwd.map(Into::into),
            templates: Vec::new(),
            messages: Vec::new(),
            worktree_location: None,
            task_settings: Default::default(),
        }
    }

    fn world() -> World {
        let mut w = World::default();
        w.projects.push(project("shop", Some("~/shop")));
        w.projects.push(project("draft", None));
        w.projects.push(project("blog", Some("~/blog")));
        w.space = Some("shop".into());
        w
    }

    fn hello(tasks: bool) -> Event {
        Event::Agent(AgentEvent::Hello { version: "x".into(), hooks: protocol::Hooks::Installed, send_text: true, tasks, task_hooks: None, task_links: true, shell: None })
    }

    fn task(id: &str) -> Task {
        Task {
            id: id.into(),
            reference: format!("r{id}"),
            title: id.into(),
            description: String::new(),
            status: TaskStatus::Inbox,
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

    fn board(key: &str, tasks: Vec<Task>) -> Event {
        let list = TaskList { tasks, ..TaskList::empty("/home/me/shop") };
        Event::Agent(AgentEvent::Tasks(Box::new(TaskBoard { key: key.into(), list: Some(list), read_only: false, problem: None, found: vec![] })))
    }

    fn edited(id: u64, problem: Option<Problem>, content_rev: Option<u64>) -> Event {
        Event::Agent(AgentEvent::TasksEdited { id, key: "shop".into(), task: Some("a".into()), problem, content_rev })
    }

    fn cmds(out: &[Event]) -> Vec<AgentCmd> {
        out.iter()
            .filter_map(|e| match e {
                Event::ToAgent(c) => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_panel_group_and_the_edited_one_are_followed() {
        let mut tasks = Tasks::default();
        let mut w = world();
        let mut out = vec![];
        tasks.poll(&mut w, &mut out);
        assert_eq!(cmds(&out), vec![AgentCmd::WatchTasks { key: "shop".into(), path: "~/shop".into() }]);
        let mut out = vec![];
        tasks.poll(&mut w, &mut out);
        assert!(out.is_empty(), "once");
        // Another group, without a folder: nothing to follow.
        w.space = Some("draft".into());
        let mut out = vec![];
        tasks.poll(&mut w, &mut out);
        assert_eq!(cmds(&out), vec![AgentCmd::UnwatchTasks { key: "shop".into() }]);
        // A task of shop being edited keeps shop followed while another space is open.
        w.space = Some("shop".into());
        tasks.poll(&mut w, &mut vec![]);
        tasks.handle(&board("shop", vec![task("a")]), &mut w, &mut vec![]);
        tasks.open_editor("shop", "a");
        w.space = Some("blog".into());
        let mut out = vec![];
        tasks.poll(&mut w, &mut out);
        assert_eq!(cmds(&out), vec![AgentCmd::WatchTasks { key: "blog".into(), path: "~/blog".into() }], "shop stays");
        // A new agent: subscribed again.
        tasks.handle(&hello(true), &mut w, &mut vec![]);
        let mut out = vec![];
        tasks.poll(&mut w, &mut out);
        assert_eq!(cmds(&out).len(), 2);
        // An agent without task lists is asked nothing.
        tasks.handle(&hello(false), &mut w, &mut vec![]);
        let mut out = vec![];
        tasks.poll(&mut w, &mut out);
        assert!(cmds(&out).is_empty());
    }

    #[test]
    fn answers_undo_and_notices() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let mut tasks = Tasks::default();
        let mut w = world();
        tasks.handle(&board("shop", vec![task("a"), task("b")]), &mut w, &mut vec![]);
        tasks.handle(&board("nobody", vec![task("x")]), &mut w, &mut vec![]);
        assert!(tasks.board("nobody").is_none(), "unknown group ignored");
        assert_eq!(open_count(tasks.tasks_of("shop")), 2);

        // Changes name the list they were shown.
        let mut out = vec![];
        tasks.send("shop", TaskOp::Remove { task: "a".into() }, None, Pending::Remove { key: "shop".into(), task: "a".into(), title: "a".into() }, &mut out);
        assert!(matches!(&cmds(&out)[..], [AgentCmd::EditTasks { list: Some(l), .. }] if l == "/home/me/shop"));
        tasks.send("shop", TaskOp::Remove { task: "b".into() }, None, Pending::Remove { key: "shop".into(), task: "b".into(), title: "b".into() }, &mut vec![]);
        tasks.handle(&edited(1, None, None), &mut w, &mut vec![]);
        tasks.handle(&edited(2, None, None), &mut w, &mut vec![]);
        assert_eq!(tasks.undo().iter().map(|u| u.task.as_str()).collect::<Vec<_>>(), vec!["a", "b"], "a stack, last removed last");

        // A refusal from the panel shows there, translated.
        let id = tasks.send("shop", TaskOp::Remove { task: "zz".into() }, None, Pending::Panel, &mut vec![]);
        tasks.handle(&edited(id, Some(Problem::TaskNotFound { task: "zz".into() }), None), &mut w, &mut vec![]);
        assert_eq!(tasks.error.as_ref().map(|(_, e)| e.as_str()), Some("Cette tâche n'existe plus (supprimée entre-temps ?)."));

        // A file set aside: a toast.
        let mut out = vec![];
        let notice = AgentEvent::TasksNotice { key: "shop".into(), problem: Problem::TasksSetAside { path: "/x.corrupt-1".into(), detail: "EOF".into() } };
        tasks.handle(&Event::Agent(notice), &mut w, &mut out);
        assert!(matches!(&out[..], [Event::Inform { title, body }] if title == "Liste de tâches mise de côté" && body.contains("/x.corrupt-1")));

        // Unlinking the group forgets its list; an editor on it stays, its text kept.
        tasks.open_editor("shop", "a");
        tasks.editor.as_mut().unwrap().description = "my text".into();
        tasks.handle(&Event::SetProjectCwd("shop".into(), None), &mut w, &mut vec![]);
        assert!(tasks.board("shop").is_none() && tasks.undo().is_empty());
        assert!(tasks.editor().is_some_and(|e| e.gone && e.description == "my text"));
    }

    #[test]
    fn a_note_then_a_save_is_not_stale_and_the_revision_follows_saves() {
        let mut tasks = Tasks::default();
        let mut w = world();
        let mut a = task("a");
        a.content_rev = 4;
        tasks.handle(&board("shop", vec![a.clone()]), &mut w, &mut vec![]);
        tasks.open_editor("shop", "a");
        let ed = tasks.editor.as_mut().unwrap();
        ed.title = "  Invoice PDF ".into();
        ed.status = TaskStatus::Todo;
        let mut out = vec![];
        tasks.save(&mut out);
        let sent = cmds(&out);
        assert_eq!(sent.len(), 2);
        assert!(matches!(&sent[0], AgentCmd::EditTasks { op: TaskOp::Edit { title: Some(t), description: None, priority: None, .. }, expect: Some(4), .. } if t == "Invoice PDF"));
        assert!(matches!(&sent[1], AgentCmd::EditTasks { op: TaskOp::SetStatus { status: TaskStatus::Todo, .. }, expect: None, .. }));
        // The status refused, the content saved: the editor stays, checked against the new revision.
        tasks.handle(&edited(1, None, Some(5)), &mut w, &mut vec![]);
        tasks.handle(&edited(2, Some(Problem::TasksFailed { detail: "disk full".into() }), None), &mut w, &mut vec![]);
        let ed = tasks.editor().expect("still open");
        assert_eq!(ed.rev, 5);
        // A note in between does not change the content revision: the next save is not stale.
        a.rev = 9;
        a.content_rev = 5;
        a.title = "Invoice PDF".into();
        tasks.handle(&board("shop", vec![a]), &mut w, &mut vec![]);
        tasks.editor.as_mut().unwrap().description = "Totals".into();
        let mut out = vec![];
        tasks.save(&mut out);
        assert!(cmds(&out).iter().any(|c| matches!(c, AgentCmd::EditTasks { op: TaskOp::Edit { description: Some(_), title: None, .. }, expect: Some(5), .. })));
        // Nothing changed: closing, nothing sent.
        tasks.open_editor("shop", "a");
        let mut out = vec![];
        tasks.save(&mut out);
        assert!(out.is_empty() && tasks.editor().is_none());
    }

    #[test]
    fn a_real_conflict_offers_their_version_or_an_overwrite() {
        let mut tasks = Tasks::default();
        let mut w = world();
        tasks.handle(&board("shop", vec![task("a")]), &mut w, &mut vec![]);
        tasks.open_editor("shop", "a");
        tasks.editor.as_mut().unwrap().title = "mine".into();
        tasks.save(&mut vec![]);
        tasks.handle(&edited(1, Some(Problem::TaskStale { task: "ra".into() }), None), &mut w, &mut vec![]);
        let ed = tasks.editor().expect("open");
        assert!(ed.conflict && ed.title == "mine" && ed.dirty());
        // Theirs, now in the list.
        let mut theirs = task("a");
        theirs.title = "theirs".into();
        theirs.content_rev = 2;
        let ed = tasks.editor.as_mut().unwrap();
        ed.load(&theirs);
        assert!(!ed.conflict && ed.title == "theirs" && ed.rev == 2 && !ed.dirty());
    }

    #[test]
    fn quick_add_text_stays_until_the_agent_took_it() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let mut tasks = Tasks::default();
        let mut w = world();
        tasks.handle(&hello(true), &mut w, &mut vec![]);
        tasks.quick = "Invoice PDF".into();
        let id = tasks.send("shop", TaskOp::Remove { task: "x".into() }, None, Pending::Quick { text: "Invoice PDF".into() }, &mut vec![]);
        tasks.quick_pending = Some(id);
        // The agent goes away: the text stays, and nothing waits any more.
        tasks.handle(&Event::AgentLost, &mut w, &mut vec![]);
        assert_eq!((tasks.quick.as_str(), tasks.quick_pending), ("Invoice PDF", None));
        let id = tasks.send("shop", TaskOp::Remove { task: "x".into() }, None, Pending::Quick { text: "Invoice PDF".into() }, &mut vec![]);
        tasks.handle(&edited(id, Some(Problem::TaskTooLong { field: protocol::TaskField::Title, max: 200 }), None), &mut w, &mut vec![]);
        assert_eq!(tasks.quick, "Invoice PDF", "refused: kept");
        let id = tasks.send("shop", TaskOp::Remove { task: "x".into() }, None, Pending::Quick { text: "Invoice PDF".into() }, &mut vec![]);
        tasks.handle(&edited(id, None, Some(1)), &mut w, &mut vec![]);
        assert_eq!(tasks.quick, "", "added: cleared");
    }
}
