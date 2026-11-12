//! Starting a task from the panel: worktree, card and agent in one action.
//!
//! `Start` (or `Resume`) opens a `Starter`: the group's task command is chosen first if it has
//! none (`Stage::Template`), then the confirmation says what will happen (`Stage::Confirm`): the
//! worktree reused, the one created, or a plain card outside git; the command and the prompt. The
//! worktree is planned, checked and created by the `worktrees` slice, through events
//! (`PlanTaskWorktree`, `CreateTaskWorktree`, and its answers): same agent command, same checks,
//! same recipe and setup consent as its own dialog. A group may skip the confirmation, except when
//! a dependency is not finished, a branch would be checked out again, the start would be a plain
//! card, or something went wrong.
//!
//! The card opens in the task's group with the task for its command to read
//! (`Event::NewTaskCard`), and the task records its worktree, branch and session
//! (`TaskOp::SetLinks`, retried when refused). Until the list shows that session, the task is
//! "starting": another Start for it does nothing.

use std::time::{Duration, Instant};

use egui::{Button, CollapsingHeader, Color32, ComboBox, Id, Key, Modal, Modifiers, RichText, ScrollArea, TextEdit, Ui};
use protocol::{DEFAULT_WORKTREE_LOCATION, LinkChange, Problem, RECIPE_FILE, Task, TaskOp, WorktreeBranch, folder_for_branch};

use super::logic::{blocked_by, find};
use super::start::{self, Branch, Target, TaskCommand};
use super::{Pending, Tasks};
use crate::shell::event::{Event, TaskCard, TaskWorktreeOrder, TaskWorktreePlan, TaskWorktreeSpec};
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::{CommandTemplate, Project, ProjectId, TaskSettings, World, new_id};

const ERROR: Color32 = Color32::from_rgb(235, 95, 95);
const WARNING: Color32 = Color32::from_rgb(230, 180, 80);
/// A start without confirmation shows its dialog anyway when the agent takes longer than this.
const SILENT_WAIT: Duration = Duration::from_secs(5);
/// A plan left unanswered this long is asked again.
const PLAN_RETRY: Duration = Duration::from_secs(10);
/// A link write refused is sent again, up to this many times in all.
const LINK_TRIES: u32 = 3;

/// A start sent, until the list shows the task's session (or it definitely failed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Starting {
    pub project: ProjectId,
    pub reference: String,
    /// The tmux session of the card opened for it.
    pub session: String,
    /// The link write sent (`None`: the worktree is still being created).
    pub link: Option<TaskOp>,
    pub tries: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The group has no task command yet: pick or write one.
    Template,
    /// What will happen, and the button that does it.
    Confirm,
}

/// A start being prepared.
pub struct Starter {
    pub project: ProjectId,
    /// The task's id.
    pub task: String,
    pub stage: Stage,
    /// Always show the confirmation (asked from the menu, or a refusal to show).
    pub ask: bool,
    /// The template picked in the template choice.
    pub pick: Option<usize>,
    /// A new template, written in the template choice (prefilled with an example).
    pub new_name: String,
    pub new_command: String,
    /// The group's settings, as edited in the confirmation.
    pub settings: TaskSettings,
    /// The user wants a new branch rather than checking out the existing one.
    pub fresh: bool,
    /// Add the worktree's folder to `info/exclude` (off unless ticked).
    pub exclude: bool,
    /// The recipe's setup tick as the user set it (`None`: on when already accepted).
    pub run_setup: Option<bool>,
    /// The last plan asked, and when.
    asked: Option<(TaskWorktreeSpec, Instant)>,
    /// The `worktrees` slice's last answer.
    planned: Option<TaskWorktreePlan>,
    /// A free folder to use instead of the derived one: (derived, free).
    folder_choice: Option<(String, String)>,
    /// Why the last attempt did not go (translated), shown in the confirmation.
    pub error: Option<String>,
    /// When it opened.
    opened: Instant,
    /// A dialog is on screen (a start without confirmation shows none while it waits).
    pub shown: bool,
}

impl Starter {
    pub fn new(project: ProjectId, task: String, ask: bool, settings: TaskSettings) -> Self {
        Starter {
            project,
            task,
            stage: Stage::Confirm,
            ask,
            pick: None,
            new_name: t().start_template_example_name.into(),
            new_command: start::EXAMPLE_COMMAND.into(),
            settings,
            fresh: false,
            exclude: false,
            run_setup: None,
            asked: None,
            planned: None,
            folder_choice: None,
            error: None,
            opened: Instant::now(),
            shown: false,
        }
    }
}

/// What the confirmation shows and the start does, computed each frame.
struct Prepared {
    target: Target,
    /// For a creation: its branch, and the request it is checked as.
    branch: Option<Branch>,
    spec: Option<TaskWorktreeSpec>,
    /// The `worktrees` slice's answer about this very request.
    plan: Option<TaskWorktreePlan>,
    template: Option<CommandTemplate>,
    command: Option<TaskCommand>,
    prompt: String,
    /// The tasks it waits for that are not finished.
    waits: Vec<String>,
}

impl Prepared {
    /// Everything is known and nothing is wrong: the start can go.
    fn ready(&self) -> bool {
        self.template.is_some()
            && self.command.as_ref().is_some_and(TaskCommand::accepted)
            && match self.target {
                Target::Plain | Target::Reuse { .. } => true,
                Target::Create => self.plan.as_ref().is_some_and(|p| p.problems.is_empty()) && matches!(self.branch, Some(Branch::New { .. } | Branch::Existing { .. })),
            }
    }

    /// Something is wrong that the user must see.
    fn refused(&self) -> bool {
        self.command.as_ref().is_some_and(|c| !c.accepted()) || matches!(self.branch, Some(Branch::NoBase)) || self.plan.as_ref().is_some_and(|p| !p.problems.is_empty())
    }
}

/// The location a group's worktrees go to, as sent (`None`: the default one).
fn location_of(project: &Project) -> Option<String> {
    project.worktree_location.clone().map(|l| l.trim().to_string()).filter(|l| l != DEFAULT_WORKTREE_LOCATION)
}

impl Tasks {
    /// Opens the start of a task (from its row, its menu or its editor). Nothing when that task is
    /// already being started, or its start is open.
    pub(super) fn begin(&mut self, world: &World, key: &str, task: &str, ask: bool) {
        if self.starts.contains_key(task) || self.starter.as_ref().is_some_and(|s| s.task == task) {
            return;
        }
        // Another start is being prepared (a silent one waits for its plan): this one comes next,
        // never dropped.
        if self.starter.is_some() {
            if !self.queued.iter().any(|(_, t, _)| t == task) {
                self.queued.push_back((key.to_string(), task.to_string(), ask));
            }
            return;
        }
        let Some(project) = world.project(key) else { return };
        let mut starter = Starter::new(key.to_string(), task.to_string(), ask, project.task_settings.clone());
        if start::task_template(project).is_none() {
            starter.stage = Stage::Template;
        }
        self.starter = Some(starter);
    }

    /// The start dialogs; a start that needs no confirmation goes at once.
    pub(super) fn starter_ui(&mut self, ctx: &egui::Context, world: &World, events: &mut Vec<Event>) {
        let Some(mut s) = self.starter.take() else { return };
        let task = self.boards.get(&s.project).and_then(|b| b.list.as_ref()).and_then(|l| find(&l.tasks, &s.task)).filter(|t| t.removed.is_none()).cloned();
        let (Some(project), Some(task)) = (world.project(&s.project).cloned(), task) else {
            if let Some((key, task, ask)) = self.queued.pop_front() {
                self.begin(world, &key, &task, ask);
            }
            return;
        };
        let keep = match s.stage {
            Stage::Template => self.template_ui(ctx, &mut s, &project, events),
            Stage::Confirm => self.confirm_ui(ctx, &mut s, &project, &task, world, events),
        };
        if keep {
            self.starter = Some(s);
        } else if let Some((key, task, ask)) = self.queued.pop_front() {
            // The next start asked meanwhile.
            self.begin(world, &key, &task, ask);
        }
    }

    /// The group has no task command: pick one of its templates, or write one (an example to
    /// edit). `false`: closed.
    fn template_ui(&mut self, ctx: &egui::Context, s: &mut Starter, project: &Project, events: &mut Vec<Event>) -> bool {
        s.shown = true;
        let mut open = true;
        let mut chosen = None;
        Modal::new(Id::unique("task-template")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.label(RichText::new((t().start_template_title)(&project.name)).strong().size(16.0));
            ui.add_space(4.0);
            ui.label(RichText::new(t().start_template_help).color(theme::TEXT_DIM));
            ui.add_space(6.0);
            if !project.templates.is_empty() {
                for (i, tpl) in project.templates.iter().enumerate() {
                    ui.radio_value(&mut s.pick, Some(i), &tpl.name).on_hover_text(RichText::new(&tpl.command).monospace());
                }
                if ui.add_enabled(s.pick.is_some(), Button::new(t().start_template_use)).clicked()
                    && let Some(i) = s.pick
                {
                    chosen = Some(Event::UseTaskTemplate(project.id.clone(), i));
                }
                ui.separator();
            }
            ui.label(RichText::new(t().start_template_new).strong());
            ui.horizontal(|ui| {
                ui.label(t().template_name);
                ui.add(TextEdit::singleline(&mut s.new_name).desired_width(240.0).id(Id::unique("task-template-name")));
            });
            ui.label(t().template_command);
            ui.add(TextEdit::multiline(&mut s.new_command).code_editor().desired_rows(2).desired_width(f32::INFINITY).id(Id::unique("task-template-command")));
            let command = start::expand_task_command(&s.new_command);
            placeholder_warnings(ui, &command);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!s.new_command.trim().is_empty(), Button::new(t().start_template_create)).clicked() {
                    chosen = Some(Event::AddTaskTemplate(project.id.clone(), CommandTemplate { name: s.new_name.clone(), command: s.new_command.clone(), task: true }));
                }
                if ui.button(t().cancel).clicked() {
                    open = false;
                }
            });
        });
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            open = false;
        }
        if let Some(event) = chosen {
            events.push(event);
            s.pick = None;
            s.stage = Stage::Confirm;
            // Chosen just now: the user sees what it does before anything runs.
            s.ask = true;
        }
        open
    }

    /// Everything a start needs, from what is known now; asks the `worktrees` slice for the plan
    /// of the worktree to create when due.
    fn prepare(&self, s: &mut Starter, project: &Project, task: &Task, events: &mut Vec<Event>, now: Instant) -> Prepared {
        let prefix = s.settings.branch_prefix.as_deref().filter(|p| start::prefix_valid(p)).unwrap_or(start::DEFAULT_BRANCH_PREFIX).to_string();
        let derived_branch = start::task_branch(&prefix, &task.title, &task.reference, &[]);
        let list = self.repos.get(&project.id).and_then(|r| r.as_ref());
        let target = start::target(task, list, &derived_branch);
        let template = start::task_template(project).cloned();
        let command = template.as_ref().map(|tpl| start::expand_task_command(&tpl.command));
        let integration = self.task_hooks == Some(protocol::TaskHooks::Installed);
        let prompt = start::build_prompt(s.settings.prompt.as_deref(), task, integration);
        let waits = blocked_by(task, self.tasks_of(&project.id)).iter().map(|d| format!("#{} {} ({})", d.reference, d.title, t().task_status(d.status))).collect();
        let mut p = Prepared { target: target.clone(), branch: None, spec: None, plan: None, template, command, prompt, waits };
        if target != Target::Create {
            return p;
        }
        let planned = s.planned.clone().filter(|plan| plan.spec.task == task.id);
        let branch = planned.as_ref().map(|plan| start::branch_for(task, &plan.branches, plan.default_branch.as_deref(), &prefix, s.fresh));
        let (name, branch_request) = match &branch {
            Some(Branch::New { name, base }) => (name.clone(), WorktreeBranch::New { name: name.clone(), base: base.clone() }),
            Some(Branch::Existing { name, .. }) => (name.clone(), WorktreeBranch::Existing { name: name.clone() }),
            // Before any plan: the branch the task recorded, or the one it would get.
            _ => {
                let guess = task.branch.clone().unwrap_or_else(|| derived_branch.clone());
                let base = list.and_then(|l| l.default_branch.clone()).unwrap_or_default();
                (guess.clone(), WorktreeBranch::New { name: guess, base })
            }
        };
        let derived = folder_for_branch(&name);
        let folder = match &s.folder_choice {
            Some((d, free)) if *d == derived => free.clone(),
            _ => derived.clone(),
        };
        let spec = TaskWorktreeSpec { project: project.id.clone(), task: task.id.clone(), branch: branch_request, folder, location: location_of(project) };
        let answer = planned.filter(|plan| plan.spec == spec);
        // The folder is taken: the first free one, like the dialog's button, but at once.
        if let Some(plan) = &answer
            && plan.problems.iter().any(|p| matches!(p, Problem::DestinationExists { .. }))
            && let Some(free) = plan.free_folder.clone()
        {
            s.folder_choice = Some((derived, free));
        }
        // Asked at once for a new request, again when left unanswered.
        let due = match &s.asked {
            Some((asked, at)) if *asked == spec => answer.is_none() && now.duration_since(*at) >= PLAN_RETRY,
            _ => answer.is_none(),
        };
        if due {
            events.push(Event::PlanTaskWorktree(Box::new(spec.clone())));
            s.asked = Some((spec.clone(), now));
        }
        p.branch = branch;
        p.spec = Some(spec);
        p.plan = answer;
        p
    }

    /// The `worktrees` slice's answer to a start's plan.
    pub(super) fn starter_planned(&mut self, plan: &TaskWorktreePlan) {
        if let Some(s) = self.starter.as_mut().filter(|s| s.task == plan.spec.task) {
            s.planned = Some(plan.clone());
        }
    }

    fn confirm_ui(&mut self, ctx: &egui::Context, s: &mut Starter, project: &Project, task: &Task, world: &World, events: &mut Vec<Event>) -> bool {
        let now = Instant::now();
        let p = self.prepare(s, project, task, events, now);
        let blocked = self.start_blocked(world, &project.id, &task.id);
        let ready = p.ready() && blocked.is_none();
        let checks_out = matches!(p.branch, Some(Branch::Existing { .. }));
        // Asked when a dependency is not finished, a branch would be checked out again, there would
        // be no worktree, something went wrong, or the group wants it.
        let ask = s.ask || !p.waits.is_empty() || checks_out || p.target == Target::Plain || !project.task_settings.skip_confirm || s.error.is_some();
        if !ask && ready {
            self.launch(s, project, task, &p, events);
            return false;
        }
        if !ready {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        // Without a confirmation, nothing shows while the agent answers, unless something is wrong
        // or it takes long; no dialog is claimed then (the keyboard stays with the terminals).
        if !ask && blocked.is_none() && !p.refused() && p.template.is_some() && now.duration_since(s.opened) < SILENT_WAIT {
            s.shown = false;
            return true;
        }
        s.shown = true;
        let mut open = true;
        let mut go = false;
        Modal::new(Id::unique("task-start")).show(ctx, |ui| {
            ui.set_width(600.0);
            ui.label(RichText::new((t().start_title)(&task.reference)).strong().size(16.0));
            ui.label(RichText::new(start::one_line(&task.title)).color(theme::TEXT));
            ui.add_space(6.0);
            if !p.waits.is_empty() {
                ui.label(RichText::new(format!("⚠ {}", (t().start_waits)(&p.waits.join(", ")))).color(WARNING));
                ui.add_space(4.0);
            }
            ScrollArea::vertical().id_salt("task-start-body").max_height((ctx.content_rect().height() * 0.6).max(200.0)).show(ui, |ui| {
                where_ui(ui, s, project, &p);
                ui.add_space(6.0);
                command_ui(ui, &p);
                ui.add_space(6.0);
                settings_ui(ui, s, project);
            });
            ui.add_space(6.0);
            ui.checkbox(&mut s.settings.skip_confirm, t().start_skip);
            if let Some(b) = &blocked {
                ui.label(RichText::new(super::blocked_text(b)).color(ERROR));
            }
            if let Some(e) = &s.error {
                ui.label(RichText::new(e).color(ERROR));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(ready, Button::new(t().start_confirm)).clicked() {
                    go = true;
                }
                if ui.button(t().cancel).clicked() {
                    open = false;
                }
                if !ready && blocked.is_none() && !p.refused() && p.template.is_some() {
                    ui.spinner();
                }
            });
        });
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            open = false;
        }
        // Another task command picked in the settings.
        if let Some(i) = s.pick.take() {
            events.push(Event::UseTaskTemplate(project.id.clone(), i));
        }
        if go {
            self.launch(s, project, task, &p, events);
            return false;
        }
        open
    }

    /// Starts the task: its card opens (now, or once its worktree is created) and it records
    /// where it is worked on. Nothing when it is already being started.
    fn launch(&mut self, s: &Starter, project: &Project, task: &Task, p: &Prepared, events: &mut Vec<Event>) {
        let Some(command) = p.command.as_ref().filter(|c| c.accepted()) else { return };
        if self.starts.contains_key(&task.id) {
            return;
        }
        if s.settings != project.task_settings {
            events.push(Event::SetProjectTaskSettings(project.id.clone(), s.settings.clone()));
        }
        let card_id = new_id();
        let session = format!("{}{card_id}", protocol::SESSION_PREFIX);
        let card = TaskCard { card: card_id, project: project.id.clone(), cwd: String::new(), command: command.command.clone(), setup: None, task: start::task_env(task, p.prompt.clone()) };
        self.fresh.insert(session.clone());
        self.follows.remove(&(self.list_key(&project.id), task.id.clone()));
        let starting = Starting { project: project.id.clone(), reference: task.reference.clone(), session: session.clone(), link: None, tries: 0 };
        self.starts.insert(task.id.clone(), starting);
        let set = |v: &str| Some(LinkChange::Set(v.to_string()));
        match &p.target {
            Target::Plain => {
                let cwd = project.cwd.clone().unwrap_or_default();
                events.push(Event::NewTaskCard(Box::new(TaskCard { cwd, ..card })));
                self.send_link(&task.id, TaskOp::SetLinks { task: task.id.clone(), worktree: None, branch: None, session: set(&session) }, events);
            }
            Target::Reuse { path, branch } => {
                events.push(Event::NewTaskCard(Box::new(TaskCard { cwd: path.clone(), ..card })));
                self.send_link(&task.id, TaskOp::SetLinks { task: task.id.clone(), worktree: set(path), branch: branch.as_deref().and_then(set), session: set(&session) }, events);
            }
            Target::Create => {
                let Some(spec) = p.spec.clone() else { return };
                let accepted = p.plan.as_ref().and_then(|plan| plan.recipe.as_ref()).is_some_and(|r| r.setup_accepted);
                let order = TaskWorktreeOrder { spec, exclude: s.exclude, run_setup: s.run_setup.unwrap_or(accepted), card };
                events.push(Event::CreateTaskWorktree(Box::new(order)));
            }
        }
    }

    /// Sends (or sends again) a start's link write.
    fn send_link(&mut self, task: &str, op: TaskOp, events: &mut Vec<Event>) {
        let Some(st) = self.starts.get_mut(task) else { return };
        st.link = Some(op.clone());
        st.tries += 1;
        let project = st.project.clone();
        self.send(&project, op, None, Pending::StartLink { task: task.to_string() }, events);
    }

    /// The agent answered a start's link write: refused, it is sent again a few times, then the
    /// start is said to have failed (its card stays, unlinked).
    pub(super) fn link_answered(&mut self, task: &str, problem: Option<&Problem>, events: &mut Vec<Event>) {
        let Some(problem) = problem else { return };
        let Some(st) = self.starts.get(task).cloned() else { return };
        match st.link.clone().filter(|_| st.tries < LINK_TRIES) {
            Some(op) => self.send_link(task, op, events),
            None => {
                self.starts.remove(task);
                self.error = Some((st.project, (t().start_link_failed)(&st.reference, &t().problem(problem))));
            }
        }
    }

    /// The `worktrees` slice created a task's worktree: the task records it.
    pub(super) fn worktree_created(&mut self, task: &str, path: &str, branch: &str, events: &mut Vec<Event>) {
        let Some(st) = self.starts.get(task) else { return };
        let set = |v: &str| Some(LinkChange::Set(v.to_string()));
        let op = TaskOp::SetLinks { task: task.to_string(), worktree: set(path), branch: set(branch), session: set(&st.session.clone()) };
        // Created just now: a list from before it must not clear its link.
        self.linked_unseen.insert(path.to_string());
        self.send_link(task, op, events);
    }

    /// The task's worktree was not created: said (a branch or folder found taken opens the start
    /// again, which then proposes the existing branch or a free folder).
    pub(super) fn worktree_failed(&mut self, world: &World, project: &str, task: &str, problem: &Problem) {
        let Some(st) = self.starts.remove(task) else { return };
        let text = (t().start_failed)(&st.reference, &t().problem(problem));
        let again = matches!(problem, Problem::BranchExists { .. } | Problem::DestinationExists { .. } | Problem::BranchCheckedOut { .. });
        if again && self.starter.is_none() {
            self.begin(world, project, task, true);
            if let Some(s) = self.starter.as_mut() {
                s.error = Some(text);
            }
        } else {
            self.error = Some((project.to_string(), text));
        }
    }

    /// The list shows the tasks: a start whose session is recorded is done.
    pub(super) fn starts_seen(&mut self, tasks: &[Task]) {
        self.starts.retain(|id, st| !tasks.iter().any(|t| &t.id == id && t.session.as_deref() == Some(st.session.as_str())));
    }

    /// The link to the agent closed: starts in flight will never be answered.
    pub(super) fn starts_lost(&mut self) {
        for (_, st) in std::mem::take(&mut self.starts) {
            self.error = Some((st.project, (t().start_agent_lost)(&st.reference)));
        }
    }

    /// Why `task` of the group `key` cannot be started now.
    pub(super) fn start_blocked(&self, world: &World, key: &str, task: &str) -> Option<start::Blocked> {
        let project = world.project(key)?;
        let writable = self.boards.get(key).is_some_and(|b| !b.read_only && b.list.is_some()) && self.capable == Some(true);
        start::blocked(start::Readiness { starting: self.starts.contains_key(task), ..self.readiness(project, writable) })
    }

    /// What the app knows that decides whether the group's tasks can start (`writable`: its list
    /// can be changed).
    pub(super) fn readiness(&self, project: &Project, writable: bool) -> start::Readiness {
        start::Readiness {
            folder: project.cwd.is_some(),
            agent: match self.capable {
                None => None,
                Some(false) => Some(false),
                Some(true) => Some(self.task_links),
            },
            writable,
            repo_known: self.repos.contains_key(&project.id),
            starting: false,
            shell: self.shell_ok,
        }
    }
}

/// Where the card goes: the worktree reused, the one created (branch, folder, recipe), or the
/// group's folder outside git.
fn where_ui(ui: &mut Ui, s: &mut Starter, project: &Project, p: &Prepared) {
    match &p.target {
        Target::Plain => {
            ui.label(RichText::new((t().start_plain)(project.cwd.as_deref().unwrap_or_default())).color(WARNING));
        }
        Target::Reuse { path, branch } => {
            ui.label(RichText::new((t().start_reuse)(path, branch.as_deref().unwrap_or(t().detached_head))).color(theme::TEXT));
        }
        Target::Create => {
            match &p.branch {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(t().start_preparing).color(theme::TEXT_DIM));
                    });
                }
                Some(Branch::NoBase) => {
                    ui.label(RichText::new(t().start_no_base).color(ERROR));
                }
                Some(Branch::New { name, base }) => {
                    ui.label(RichText::new((t().start_new_branch)(name, base)).color(theme::TEXT));
                }
                Some(Branch::Existing { name, again }) => {
                    let text = if *again { (t().start_branch_again)(name) } else { (t().start_branch_exists)(name) };
                    ui.label(RichText::new(text).color(WARNING));
                    if ui.button(t().start_new_branch_instead).clicked() {
                        s.fresh = true;
                        s.folder_choice = None;
                    }
                }
            }
            let Some(plan) = &p.plan else { return };
            if let Some(path) = &plan.path {
                ui.label(RichText::new((t().start_folder)(path)).monospace().color(theme::TEXT_DIM));
            }
            for problem in &plan.problems {
                ui.label(RichText::new(t().problem(problem)).color(ERROR));
            }
            if let Some(pattern) = &plan.exclude {
                ui.label(RichText::new(format!("⚠ {}", t().worktree_not_ignored)).color(WARNING));
                ui.checkbox(&mut s.exclude, (t().worktree_exclude_option)(pattern));
            }
            if let Some(recipe) = &plan.recipe {
                ui.add_space(4.0);
                ui.label(RichText::new((t().recipe_title)(RECIPE_FILE)).strong());
                for (line, refused) in &recipe.lines {
                    ui.label(RichText::new(line).color(if *refused { ERROR } else { theme::TEXT }));
                }
                if let Some(setup) = &recipe.setup {
                    ui.label(format!("{} ({})", t().recipe_setup_label, (t().recipe_setup_lines)(setup.lines().count().max(1))));
                    egui::Frame::new().fill(theme::CARD_BG).inner_margin(6.0).show(ui, |ui| {
                        ScrollArea::vertical().id_salt("task-start-setup").max_height(100.0).show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(setup).monospace().color(theme::TEXT)).wrap());
                        });
                    });
                    let mut ticked = s.run_setup.unwrap_or(recipe.setup_accepted);
                    if ui.checkbox(&mut ticked, t().recipe_setup_tick).changed() {
                        s.run_setup = Some(ticked);
                    }
                    let (text, color) = if ticked { (t().recipe_setup_warning, WARNING) } else { (t().recipe_setup_off, theme::TEXT_DIM) };
                    ui.label(RichText::new(text).small().color(color));
                }
            }
        }
    }
}

/// The command the card starts with, its placeholders expanded, and the prompt it gets.
fn command_ui(ui: &mut Ui, p: &Prepared) {
    let (Some(template), Some(command)) = (&p.template, &p.command) else { return };
    ui.label(RichText::new((t().start_command)(&template.name)).strong());
    egui::Frame::new().fill(theme::CARD_BG).inner_margin(6.0).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(&command.command).monospace().color(theme::TEXT)).wrap());
    });
    placeholder_warnings(ui, command);
    CollapsingHeader::new(t().start_prompt_label).id_salt("task-start-prompt").show(ui, |ui| {
        ScrollArea::vertical().id_salt("task-start-prompt-text").max_height(140.0).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(&p.prompt).monospace().color(theme::TEXT_DIM)).wrap());
        });
    });
}

/// What is wrong with how a command refers to the task.
fn placeholder_warnings(ui: &mut Ui, command: &TaskCommand) {
    for (what, spot) in &command.refused {
        ui.label(RichText::new((t().start_placeholder_refused)(what, (t().task_spot)(*spot))).small().color(ERROR));
    }
    if !command.uses_task && command.accepted() {
        ui.label(RichText::new(t().start_no_placeholder).small().color(WARNING));
    }
}

/// The group's settings for its tasks: its task command, branch prefix and prompt.
fn settings_ui(ui: &mut Ui, s: &mut Starter, project: &Project) {
    CollapsingHeader::new(t().start_settings).id_salt("task-start-settings").show(ui, |ui| {
        let current = start::task_template(project).map(|tpl| tpl.name.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(t().start_template_label);
            ComboBox::from_id_salt("task-start-template").selected_text(current).show_ui(ui, |ui| {
                for (i, tpl) in project.templates.iter().enumerate() {
                    if ui.selectable_label(tpl.task, &tpl.name).clicked() && !tpl.task {
                        s.pick = Some(i);
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label(t().start_prefix_label);
            let mut prefix = s.settings.branch_prefix.clone().unwrap_or_else(|| start::DEFAULT_BRANCH_PREFIX.into());
            if ui.add(TextEdit::singleline(&mut prefix).code_editor().desired_width(200.0).id(Id::unique("task-start-prefix"))).changed() {
                s.settings.branch_prefix = (prefix != start::DEFAULT_BRANCH_PREFIX).then_some(prefix.clone());
            }
            if !start::prefix_valid(&prefix) {
                ui.label(RichText::new((t().start_prefix_invalid)(&prefix, start::DEFAULT_BRANCH_PREFIX)).small().color(WARNING));
            }
        });
        ui.label(t().start_prompt_template_label);
        let mut prompt = s.settings.prompt.clone().unwrap_or_else(|| start::DEFAULT_PROMPT.into());
        if ui.add(TextEdit::multiline(&mut prompt).code_editor().desired_rows(3).desired_width(f32::INFINITY).id(Id::unique("task-start-prompt-template"))).changed() {
            s.settings.prompt = (prompt != start::DEFAULT_PROMPT).then_some(prompt.clone());
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(t().start_prompt_help).small().color(theme::TEXT_DIM));
            if ui.add_enabled(s.settings.prompt.is_some(), Button::new(t().start_prompt_reset).small()).clicked() {
                s.settings.prompt = None;
            }
        });
    });
}
