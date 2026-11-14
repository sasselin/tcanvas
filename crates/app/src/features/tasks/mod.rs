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

use std::collections::{BTreeMap, HashMap};

use egui::{Button, CollapsingHeader, Color32, ComboBox, Id, Key, Modal, Panel, RichText, ScrollArea, Sides, TextEdit, Ui};
use protocol::{AgentCmd, AgentEvent, Author, FoundList, FoundWhy, Priority, Problem, Task, TaskBoard, TaskOp, TaskStatus};

use crate::features::changes::age_label;
use crate::features::shortcuts::keymap::tasks_combo;
use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::widgets;
use crate::shell::world::{Project, ProjectId, World, unix_now};
use logic::{blocked_by, dependency_choices, find, move_op, open_count, priority_mark, ready, sections};

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
    Adopt { from: String, merge: bool },
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
}

impl Tasks {
    pub fn board(&self, key: &str) -> Option<&TaskBoard> {
        self.boards.get(key)
    }

    pub fn has_dialog(&self) -> bool {
        self.editor.is_some() || self.merge.is_some()
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

    /// Every frame: the agent follows exactly the panel group's list and the edited one (the
    /// count shows on the toolbar even when the panel is closed).
    pub fn poll(&mut self, world: &World, events: &mut Vec<Event>) {
        let mut wanted: BTreeMap<ProjectId, String> = BTreeMap::new();
        if self.capable != Some(false) {
            let edited = self.editor.as_ref().and_then(|e| world.project(&e.key));
            for p in self.panel_project(world).into_iter().chain(edited) {
                if let Some(cwd) = &p.cwd {
                    wanted.insert(p.id.clone(), cwd.clone());
                }
            }
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
            ids.push(self.send(&ed.key, TaskOp::SetStatus { task: ed.task.clone(), status: now.status }, None, Pending::Save { content: false }, out));
        }
        match &mut self.editor {
            Some(e) if !ids.is_empty() => {
                e.saving = ids;
                e.error = None;
            }
            _ => self.editor = None,
        }
    }

    fn act(&mut self, key: &str, action: Action, out: &mut Vec<Event>) {
        match action {
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

    fn answered(&mut self, id: u64, key: &str, problem: Option<&Problem>, content_rev: Option<u64>) {
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
            Event::Agent(AgentEvent::Hello { tasks, .. }) => {
                // A new agent: it follows nothing yet, and what the last one was asked is lost.
                self.capable = Some(*tasks);
                self.watched.clear();
                self.lost();
            }
            Event::AgentLost => {
                self.capable = None;
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
                    self.boards.insert(board.key.clone(), (**board).clone());
                }
            }
            Event::Agent(AgentEvent::TasksEdited { id, key, problem, content_rev, .. }) => self.answered(*id, key, problem.as_ref(), *content_rev),
            Event::Agent(AgentEvent::TasksNotice { key, problem }) => {
                if world.project(key).is_some() {
                    out.push(Event::Inform { title: t().tasks_notice_title.into(), body: t().problem(problem) });
                }
            }
            Event::SetProjectCwd(id, _) | Event::RemoveProject(id) => {
                self.boards.remove(id);
                self.undo.retain(|u| &u.key != id);
                if let Some(ed) = self.editor.as_mut().filter(|e| &e.key == id) {
                    // Kept open with its text, to be copied.
                    ed.gone = true;
                }
            }
            _ => {}
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
        Panel::left("tasks").resizable(true).default_size(360.0).min_size(260.0).show(ui, |ui| self.panel_body(ui, &project, board.as_ref(), focus, &mut actions, events));
        if let Some(board) = board {
            self.boards.entry(project.id.clone()).or_insert(board);
        }
        for action in actions {
            self.act(&project.id, action, events);
        }
    }

    fn panel_body(&mut self, ui: &mut Ui, project: &Project, board: Option<&TaskBoard>, focus: bool, actions: &mut Vec<Action>, events: &mut Vec<Event>) {
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
        ScrollArea::vertical().id_salt("tasks-list").auto_shrink([false, false]).show(ui, |ui| {
            for (status, tasks) in sections(&list.tasks) {
                let header = format!("{} ({})", t().task_status(status), tasks.len());
                CollapsingHeader::new(RichText::new(header).small().strong().color(theme::TEXT_DIM)).id_salt(("tasks-section", status as u8)).default_open(!status.finished()).show(ui, |ui| {
                    for task in tasks {
                        actions.extend(row(ui, task, &list.tasks, writable));
                    }
                });
            }
        });
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

    /// The task editor (a modal window) and the merge confirmation. Call after the panels.
    pub fn dialogs(&mut self, ctx: &egui::Context, events: &mut Vec<Event>) {
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
        self.editor_ui(ctx, board.as_ref(), events);
        if let Some(board) = board {
            self.boards.entry(key).or_insert(board);
        }
    }

    fn editor_ui(&mut self, ctx: &egui::Context, board: Option<&TaskBoard>, events: &mut Vec<Event>) {
        let empty: &[Task] = &[];
        let tasks = board.and_then(|b| b.list.as_ref()).map_or(empty, |l| l.tasks.as_slice());
        let writable = self.capable == Some(true) && board.is_some_and(|b| !b.read_only && b.list.is_some());
        let Some(e) = self.editor.as_mut() else { return };
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
            });
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
            return;
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
    }
}

/// One task row: reference, priority mark, title (click: edit; right-click: actions), then what
/// holds it up (or "ready"), its notes and the actions menu.
fn row(ui: &mut Ui, task: &Task, tasks: &[Task], writable: bool) -> Vec<Action> {
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
                    if let Some(a) = row_menu(ui, task, tasks) {
                        pick = Some(a);
                    }
                });
            }
            pick
        },
        |ui| {
            let mut pick = None;
            if writable {
                ui.menu_button("…", |ui| pick = row_menu(ui, task, tasks)).response.on_hover_text(t().task_actions);
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

/// The actions on one task: order, status, removal.
fn row_menu(ui: &mut Ui, task: &Task, tasks: &[Task]) -> Option<Action> {
    let mut pick = None;
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
            pick = Some(Action::Op(TaskOp::SetStatus { task: task.id.clone(), status: *status }, Pending::Panel));
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
        Event::Agent(AgentEvent::Hello { version: "x".into(), hooks: protocol::Hooks::Installed, send_text: true, tasks })
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
        tasks.poll(&w, &mut out);
        assert_eq!(cmds(&out), vec![AgentCmd::WatchTasks { key: "shop".into(), path: "~/shop".into() }]);
        let mut out = vec![];
        tasks.poll(&w, &mut out);
        assert!(out.is_empty(), "once");
        // Another group, without a folder: nothing to follow.
        w.space = Some("draft".into());
        let mut out = vec![];
        tasks.poll(&w, &mut out);
        assert_eq!(cmds(&out), vec![AgentCmd::UnwatchTasks { key: "shop".into() }]);
        // A task of shop being edited keeps shop followed while another space is open.
        w.space = Some("shop".into());
        tasks.poll(&w, &mut vec![]);
        tasks.handle(&board("shop", vec![task("a")]), &mut w, &mut vec![]);
        tasks.open_editor("shop", "a");
        w.space = Some("blog".into());
        let mut out = vec![];
        tasks.poll(&w, &mut out);
        assert_eq!(cmds(&out), vec![AgentCmd::WatchTasks { key: "blog".into(), path: "~/blog".into() }], "shop stays");
        // A new agent: subscribed again.
        tasks.handle(&hello(true), &mut w, &mut vec![]);
        let mut out = vec![];
        tasks.poll(&w, &mut out);
        assert_eq!(cmds(&out).len(), 2);
        // An agent without task lists is asked nothing.
        tasks.handle(&hello(false), &mut w, &mut vec![]);
        let mut out = vec![];
        tasks.poll(&w, &mut out);
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
