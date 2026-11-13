//! Worktrees: the git worktrees of the open group's repository, with the terminals living in
//! each one. The agent lists them (`AgentEvent::Worktrees`, for every watched folder); this slice
//! keeps the lists, tells which worktree each card's folder is in (`World::card_worktrees`, read
//! by the card's branch badge) and draws a one-line strip under the toolbar when the open group's
//! repository has more than its main checkout. The strip opens the cleanup window (`cleanup`),
//! the only place that asks the agent to delete anything, always after a confirmation, and the
//! new-worktree dialog (`create`), also opened from the group's menus (`Event::NewWorktree`) since
//! the strip is hidden while the repository has only its main checkout. What the repository's
//! recipe did to a worktree this app created is shown once it is there (`report_lines`); the
//! recipe setup commands the user accepted are remembered in the app's config (`trust`).

pub mod cleanup;
pub mod create;
pub mod trust;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use egui::{Color32, Panel, RichText, ScrollArea, Ui};
use protocol::{AgentCmd, AgentEvent, Problem, RecipeAction, RecipeReport, Worktree, WorktreeList, WorktreePlan};

use cleanup::CleanupWindow;
use create::CreateDialog;
use trust::Trust;

use crate::shell::event::{Event, TaskWorktreeSpec};
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::{CardWorktree, ProjectId, World, unix_now};

const DIRTY: Color32 = Color32::from_rgb(230, 180, 80);
const MERGED: Color32 = Color32::from_rgb(90, 200, 120);

/// `root` is `path` or one of its parents, compared by whole path components (`/a/b` holds
/// `/a/b/c` and `/a/b/`, not `/a/bc`).
pub fn contains(root: &str, path: &str) -> bool {
    let (root, path) = (root.trim_end_matches('/'), path.trim_end_matches('/'));
    path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
}

/// The worktree holding `cwd`: the deepest one, so a worktree nested in another's folder
/// (`.claude/worktrees/x` inside the main checkout) wins. A bare entry has no files.
pub fn worktree_of<'a>(cwd: &str, worktrees: impl IntoIterator<Item = &'a Worktree>) -> Option<&'a Worktree> {
    worktrees.into_iter().filter(|w| !w.bare && contains(&w.path, cwd)).max_by_key(|w| w.path.trim_end_matches('/').len())
}

/// Folder name of a worktree.
pub fn folder_name(wt: &Worktree) -> &str {
    wt.path.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("/")
}

fn card_worktree(wt: &Worktree) -> CardWorktree {
    CardWorktree { path: wt.path.clone(), branch: wt.branch.clone(), linked: (!wt.main).then(|| folder_name(wt).to_string()) }
}

/// "3 min ago", "2 d ago"... for the last commit.
fn age(now: u64, time: u64) -> String {
    match now.saturating_sub(time) {
        0..=59 => t().just_now.into(),
        s @ 60..=3599 => (t().mins_ago)(s / 60),
        s @ 3600..=86_399 => (t().hours_ago)(s / 3600),
        s => (t().days_ago)(s / 86_400),
    }
}

/// What a recipe did, line by line (text, whether it went as asked): the recipe file's problem
/// first, then each item, then how many are not listed.
pub fn report_lines(report: &RecipeReport) -> Vec<(String, bool)> {
    let mut lines = Vec::new();
    if let Some(problem) = &report.problem {
        lines.push((t().problem(problem), false));
    }
    for item in &report.items {
        let line = match (&item.skipped, item.action) {
            (None, RecipeAction::Copy) => ((t().recipe_copied)(&item.path), true),
            (None, RecipeAction::Symlink) => ((t().recipe_linked)(&item.path), true),
            (Some(reason), RecipeAction::Copy) => ((t().recipe_not_copied)(&item.path, &t().skip_reason(reason)), false),
            (Some(reason), RecipeAction::Symlink) => ((t().recipe_not_linked)(&item.path, &t().skip_reason(reason)), false),
        };
        lines.push(line);
    }
    if report.omitted_failures > 0 {
        lines.push(((t().recipe_omitted_failures)(report.omitted_failures), false));
    }
    if report.omitted > 0 {
        lines.push(((t().recipe_omitted)(report.omitted), true));
    }
    lines
}

/// A recipe the agent stopped applying: its worktree exists, its copies and links may be
/// incomplete, and what its card was to run did not run (offered for copying instead).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interrupted {
    pub path: String,
    pub setup: Option<String>,
    pub command: Option<String>,
}

#[derive(Default)]
pub struct Worktrees {
    /// Latest list of each group's repository (key: project id), with the group folder it was
    /// received for.
    lists: BTreeMap<ProjectId, (String, WorktreeList)>,
    cleanup: Option<CleanupWindow>,
    create: Option<CreateDialog>,
    /// Creations sent and not answered for good: their card opens whenever they go through,
    /// with or without the dialog.
    sent: Vec<create::Sent>,
    /// What the recipe did to the last worktree created (its folder, the report: `None` while it
    /// is being applied), until closed.
    report: Option<(String, Option<RecipeReport>)>,
    /// Creations whose recipe is being applied: (request id, the new worktree's folder, the
    /// creation whose card waits for it).
    applying: Vec<(u64, String, Option<create::Sent>)>,
    /// The agent stopped while a recipe was being applied: what the report window says instead.
    interrupted: Option<Interrupted>,
    /// The recipe setup commands accepted.
    trust: Trust,
    /// Plans asked for tasks' starts, by request id.
    task_plans: BTreeMap<u64, TaskWorktreeSpec>,
    /// The last plan answered for each task (by id), checked again when its creation is asked.
    task_planned: BTreeMap<String, WorktreePlan>,
}

impl Worktrees {
    /// With the setup commands accepted so far, kept in `config_dir`.
    pub fn new(config_dir: &Path) -> Self {
        Self { trust: Trust::load(config_dir), ..Self::default() }
    }

    /// What the recipe did to the last worktree created, while it is shown.
    pub fn recipe_report(&self) -> Option<&(String, Option<RecipeReport>)> {
        self.report.as_ref()
    }

    /// The recipe the agent stopped applying, while its report window is open.
    pub fn recipe_interrupted(&self) -> Option<&Interrupted> {
        self.interrupted.as_ref()
    }

    pub fn list(&self, project: &str) -> Option<&WorktreeList> {
        self.lists.get(project).map(|(_, l)| l)
    }

    pub fn cleanup_open(&self) -> bool {
        self.cleanup.is_some()
    }

    /// A confirmation or the new-worktree dialog is up (it takes Enter and Escape).
    pub fn has_dialog(&self) -> bool {
        self.cleanup.as_ref().is_some_and(|c| c.confirm.is_some()) || self.create.is_some()
    }

    pub fn create_open(&self) -> bool {
        self.create.is_some()
    }

    /// Creations sent and still waited for.
    pub fn creations_waiting(&self) -> usize {
        self.sent.len()
    }

    /// The new-worktree dialog's form, while it is open.
    pub fn create_form(&self) -> Option<create::Form> {
        self.create.as_ref().map(CreateDialog::form)
    }

    /// Opens the new-worktree dialog of a group whose folder is in a git repository.
    pub fn open_create(&mut self, world: &World, project: &str) {
        let Some(p) = world.project(project).filter(|_| self.lists.contains_key(project)) else { return };
        if self.create.as_ref().is_none_or(|c| c.project != project) {
            self.create = Some(CreateDialog::new(project.to_string(), p.worktree_location.clone()));
        }
    }

    pub fn open_cleanup(&mut self, project: &str) {
        if self.cleanup.as_ref().is_none_or(|c| c.project != project) {
            self.cleanup = Some(CleanupWindow::new(project.to_string()));
        }
    }

    /// The cleanup window and its confirmation. Call with the other dialogs.
    pub fn dialogs(&mut self, ctx: &egui::Context, world: &World, events: &mut Vec<Event>) {
        if let Some(dialog) = self.create.as_mut() {
            // Closed, or the group went or left git.
            let open = self.lists.contains_key(&dialog.project) && dialog.ui(ctx, world, &self.trust, events, &mut self.sent);
            if !open {
                self.create = None;
            }
        }
        self.report_ui(ctx);
        let Some(window) = self.cleanup.as_mut() else { return };
        let open = match self.lists.get(&window.project) {
            Some((_, list)) => window.ui(ctx, list, world, events),
            // The group went, or left git.
            None => false,
        };
        if !open {
            self.cleanup = None;
        }
    }

    /// Every frame: which worktree each card's folder is in, across every known repository.
    pub fn poll(&self, world: &mut World) {
        let all: Vec<&Worktree> = self.lists.values().flat_map(|(_, l)| &l.worktrees).collect();
        let found: BTreeMap<_, _> = world.cards.iter().filter_map(|c| Some((c.id.clone(), card_worktree(worktree_of(c.current_cwd()?, all.iter().copied())?)))).collect();
        if found != world.card_worktrees {
            world.card_worktrees = found;
        }
        let git: BTreeSet<ProjectId> = self.lists.keys().cloned().collect();
        if git != world.git_groups {
            world.git_groups = git;
        }
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, out: &mut Vec<Event>) {
        match ev {
            Event::NewWorktree(project) => self.open_create(world, project),
            // A task's start: the same plan, the same checks and the same creation as the dialog's,
            // answered with events; its card opens in the worktree once it is there.
            Event::PlanTaskWorktree(spec) => {
                let id = create::next_id();
                self.task_plans.insert(id, (**spec).clone());
                out.push(Event::ToAgent(AgentCmd::PlanWorktree { id, key: spec.project.clone(), folder: spec.folder.clone(), location: spec.location.clone() }));
            }
            Event::CreateTaskWorktree(order) => {
                let id = create::next_id();
                match create::task_creation(order, self.task_planned.get(&order.spec.task), id) {
                    Ok((request, sent)) => {
                        out.push(Event::ToAgent(AgentCmd::CreateWorktree { id, key: order.spec.project.clone(), request }));
                        self.sent.push(sent);
                    }
                    Err(problem) => out.push(Event::TaskWorktreeFailed { project: order.spec.project.clone(), task: order.spec.task.clone(), problem }),
                }
            }
            Event::Agent(AgentEvent::WorktreePlan(plan)) if self.task_plans.contains_key(&plan.id) => {
                let spec = self.task_plans.remove(&plan.id).expect("asked");
                self.task_planned.insert(spec.task.clone(), (**plan).clone());
                out.push(Event::TaskWorktreePlanned(Box::new(create::task_plan(&spec, plan, &self.trust))));
            }
            Event::Agent(ev @ (AgentEvent::WorktreePlan(_) | AgentEvent::WorktreeCreated { .. })) => {
                if let AgentEvent::WorktreeCreated { id, path, problem, recipe_pending, .. } = ev
                    && let Some(i) = self.sent.iter().position(|s| s.id == *id)
                {
                    let sent = self.sent[i].clone();
                    // The card's setup or command may need what the recipe copies and links: it
                    // opens once `RecipeApplied` says they are done.
                    let hold = *recipe_pending && sent.starts_something();
                    let (events, keep) = create::on_created(&sent, path.as_deref(), problem.as_ref(), hold, world);
                    out.extend(events);
                    if let Some(task) = sent.task.as_ref().filter(|_| !keep) {
                        let (project, id) = (sent.project.clone(), task.task.clone());
                        out.push(match (path, problem) {
                            (Some(path), None) => Event::TaskWorktreeCreated { project, task: id, path: path.clone(), branch: task.branch.clone() },
                            (_, problem) => Event::TaskWorktreeFailed { project, task: id, problem: problem.clone().unwrap_or(Problem::NotAGitRepo) },
                        });
                    }
                    if let (Some(path), None) = (path, problem) {
                        // Accepted only now that the creation went through.
                        if let (Some(setup), Some(repo)) = (&sent.setup, &sent.repo) {
                            self.trust.accept(repo, setup);
                        }
                        if *recipe_pending {
                            self.report = Some((path.clone(), None));
                            self.interrupted = None;
                            self.applying.push((sent.id, path.clone(), hold.then_some(sent)));
                        }
                    }
                    if !keep {
                        self.sent.remove(i);
                    }
                }
                if let Some(dialog) = self.create.as_mut()
                    && dialog.handle(ev)
                {
                    self.create = None;
                }
            }
            // The agent stopped (or another one started): what it was asked will never be answered.
            Event::AgentLost | Event::Agent(AgentEvent::Hello { .. }) => self.agent_gone(world, out),
            Event::Agent(AgentEvent::RecipeApplied { id, path, report, .. }) => {
                if let Some(i) = self.applying.iter().position(|(sent, _, _)| sent == id) {
                    let (_, _, held) = self.applying.remove(i);
                    self.report = Some((path.clone(), Some((**report).clone())));
                    if let Some(sent) = held {
                        out.extend(create::open_card(&sent, path, world));
                    }
                }
            }
            Event::Agent(ev @ (AgentEvent::Cleaned { .. } | AgentEvent::WorktreeSizes { .. })) => {
                if let Some(window) = self.cleanup.as_mut() {
                    window.handle(ev);
                }
            }
            Event::Agent(ev @ AgentEvent::Worktrees(list)) => {
                if let Some(window) = self.cleanup.as_mut() {
                    window.handle(ev);
                }
                // Outside git (empty list) or for a group that no longer watches: forget it.
                match world.project(&list.key).and_then(|p| p.cwd.clone()) {
                    Some(folder) if !list.worktrees.is_empty() => {
                        self.lists.insert(list.key.clone(), (folder, list.clone()));
                    }
                    _ => {
                        self.lists.remove(&list.key);
                    }
                }
            }
            // Linking the same folder again changes nothing (and the agent, still watching it,
            // sends nothing new): keep the list. Another folder, or none: forget it.
            Event::SetProjectCwd(id, cwd) => {
                if self.lists.get(id).is_some_and(|(folder, _)| cwd.as_deref() != Some(folder.as_str())) {
                    self.lists.remove(id);
                }
            }
            Event::RemoveProject(id) => {
                self.lists.remove(id);
            }
            _ => {}
        }
    }

    /// The agent went away. A creation not answered yet ends now, in its dialog (it may or may
    /// not have been made), and is forgotten: a late answer with its id changes nothing. A recipe
    /// being applied is not waited for any more: its worktree exists, so a card held for it opens,
    /// as a plain shell (the copies and links may be incomplete: its setup and command do not run,
    /// they are offered for copying). A task's card does not open: the task keeps its links, and
    /// its row offers to resume it (the tasks slice ends its start).
    fn agent_gone(&mut self, world: &World, out: &mut Vec<Event>) {
        self.sent.clear();
        self.task_plans.clear();
        if let Some(dialog) = self.create.as_mut() {
            dialog.agent_lost();
        }
        for (_, path, held) in std::mem::take(&mut self.applying) {
            out.push(Event::Inform { title: t().recipe_interrupted_title.into(), body: (t().recipe_interrupted_body)(&path) });
            let (setup, command) = match &held {
                Some(sent) => (sent.setup.clone(), sent.task.as_ref().map(|t| t.card.command.clone()).or(sent.command.clone())),
                None => (None, None),
            };
            if let Some(sent) = held.filter(|s| s.task.is_none() && world.project(&s.project).is_some()) {
                out.push(Event::NewCard { at: None, project: Some(sent.project.clone()), command: None, setup: None, cwd: Some(path.clone()) });
            }
            self.report = Some((path.clone(), None));
            self.interrupted = Some(Interrupted { path, setup, command });
        }
    }

    /// What the recipe did to the worktree just created, until the user closes it.
    fn report_ui(&mut self, ctx: &egui::Context) {
        let Some((path, report)) = &self.report else { return };
        let interrupted = self.interrupted.as_ref().filter(|i| &i.path == path);
        let mut open = true;
        egui::Window::new(t().recipe_report_title).id(egui::Id::unique("recipe-report")).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label((t().recipe_report_intro)(path));
            match report {
                _ if interrupted.is_some() => {
                    let i = interrupted.expect("checked");
                    ui.label(RichText::new(t().recipe_interrupted_unknown).color(DIRTY));
                    for (label, text) in [(t().recipe_interrupted_setup, &i.setup), (t().recipe_interrupted_command, &i.command)] {
                        let Some(text) = text else { continue };
                        ui.label(RichText::new(label).color(theme::TEXT_DIM));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(text).monospace());
                            if ui.small_button(t().recipe_interrupted_copy).clicked() {
                                ui.ctx().copy_text(text.clone());
                            }
                        });
                    }
                }
                Some(report) => {
                    ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                        for (line, ok) in report_lines(report) {
                            ui.label(RichText::new(line).color(if ok { theme::TEXT } else { DIRTY }));
                        }
                    });
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(t().recipe_applying).color(theme::TEXT_DIM));
                    });
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
            }
            if ui.button(t().recipe_report_ok).clicked() {
                open = false;
            }
        });
        if !open {
            self.report = None;
            self.interrupted = None;
        }
    }

    /// The open group's list, when its repository has more than its main checkout, or merged
    /// branches to clean up (otherwise the card badges say all there is to say).
    pub fn shown(&self, world: &World) -> Option<&WorktreeList> {
        let id = world.space.as_deref().filter(|id| world.project(id).is_some())?;
        self.list(id).filter(|l| l.worktrees.iter().filter(|w| !w.bare).count() > 1 || !l.merged_branches.is_empty())
    }

    /// One-line strip under the toolbar. Call after the toolbar, before the side panels.
    pub fn ui(&mut self, ui: &mut Ui, world: &World, events: &mut Vec<Event>) {
        let Some(list) = self.shown(world) else { return };
        let worktrees: Vec<&Worktree> = list.worktrees.iter().filter(|w| !w.bare).collect();
        let removable = cleanup::removable(list, world).len();
        let mut open_cleanup = false;
        let mut open_create = false;
        Panel::top("worktrees").resizable(false).show(ui, |ui| {
            // A floating scroll bar registers its drag area over the bottom of the content, which
            // in this short strip is the lower half of its buttons: the bar gets its own room.
            ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
            // The title and the cleanup entry stay put, outside the scrolling part.
            ui.horizontal(|ui| {
                ui.label(RichText::new((t().worktrees_title)(worktrees.len())).strong().color(theme::TEXT_DIM)).on_hover_text(list.repo.as_deref().unwrap_or_default());
                let label = RichText::new((t().cleanup_button)(removable)).color(if removable > 0 { MERGED } else { theme::TEXT_DIM });
                open_cleanup = ui.small_button(label).on_hover_text(t().cleanup_button_hint).clicked();
                open_create = ui.small_button(t().new_worktree_button).on_hover_text(t().new_worktree_hint).clicked();
                ScrollArea::horizontal().id_salt("worktrees-strip").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for wt in worktrees {
                            ui.separator();
                            worktree_ui(ui, world, list, wt, events);
                        }
                    });
                });
            });
        });
        let project = list.key.clone();
        if open_cleanup {
            self.open_cleanup(&project);
        }
        if open_create {
            self.open_create(world, &project);
        }
    }
}

fn worktree_ui(ui: &mut Ui, world: &World, list: &WorktreeList, wt: &Worktree, events: &mut Vec<Event>) {
    let base = list.default_branch.as_deref().unwrap_or_default();
    let name = if wt.main { t().main_checkout } else { folder_name(wt) };
    let hover = match wt.last_commit {
        Some(time) => format!("{}\n{}", wt.path, (t().last_commit)(&age(unix_now(), time))),
        None => wt.path.clone(),
    };
    ui.label(RichText::new(name).strong().color(theme::TEXT)).on_hover_text(hover);
    ui.label(RichText::new(format!("⎇ {}", wt.branch.as_deref().unwrap_or(t().detached_head))).color(theme::TEXT_DIM));
    if wt.dirty == Some(true) {
        ui.label(RichText::new("●").color(DIRTY)).on_hover_text(t().worktree_dirty_hint);
    }
    if let (Some(ahead), Some(behind)) = (wt.ahead, wt.behind)
        && (ahead, behind) != (0, 0)
    {
        ui.label(RichText::new(format!("↑{ahead} ↓{behind}")).color(theme::TEXT_DIM)).on_hover_text((t().ahead_behind_hint)(base));
    }
    // Commits of its own: the Changes panel reviews them (that slice handles the event).
    if wt.ahead.is_some_and(|a| a > 0) && !wt.prunable {
        let button = ui.add_enabled(!world.review_unsupported, egui::Button::new(t().review_button).small());
        if button.on_hover_text(t().review_button_hint).on_disabled_hover_text(t().review_agent_too_old).clicked() {
            events.push(Event::ReviewWorktree { project: list.key.clone(), path: wt.path.clone() });
        }
    }
    if wt.merged == Some(true) {
        ui.label(RichText::new(t().merged).color(MERGED)).on_hover_text((t().merged_hint)(base));
    }
    if wt.locked {
        let label = ui.label(RichText::new(t().locked).color(theme::TEXT_DIM));
        if let Some(reason) = &wt.lock_reason {
            label.on_hover_text(reason);
        }
    }
    if wt.prunable {
        ui.label(RichText::new(t().prunable).color(theme::TEXT_DIM)).on_hover_text(t().prunable_hint);
    }
    let mut any = false;
    for card in world.cards.iter().filter(|c| world.card_worktrees.get(&c.id).is_some_and(|w| w.path == wt.path)) {
        any = true;
        ui.label(RichText::new("●").color(theme::status_color(card.status())));
        if ui.small_button(card.title()).on_hover_text(t().go_to_terminal).clicked() {
            events.push(Event::FlyTo(card.id.clone()));
        }
    }
    if !any {
        ui.label(RichText::new(t().no_terminal_here).small().color(theme::TEXT_DIM));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::world::{Card, Project};
    use egui::{Pos2, Rect, Vec2};

    fn wt(path: &str, main: bool, branch: Option<&str>) -> Worktree {
        Worktree {
            path: path.into(),
            head: Some("a".repeat(40)),
            branch: branch.map(String::from),
            detached: branch.is_none(),
            main,
            bare: false,
            locked: false,
            lock_reason: None,
            prunable: false,
            dirty: Some(false),
            ahead: Some(0),
            behind: Some(0),
            merged: None,
            last_commit: Some(1),
            branch_deletable: false,
            reflog_commits: false,
            status_mark: None,
        }
    }

    fn shop() -> Vec<Worktree> {
        vec![wt("/home/me/shop", true, Some("main")), wt("/home/me/shop/.claude/worktrees/invoices", false, Some("feat/invoices")), wt("/home/me/shop-taxes/", false, None)]
    }

    fn of(cwd: &str, list: &[Worktree]) -> Option<String> {
        worktree_of(cwd, list).map(|w| w.path.clone())
    }

    #[test]
    fn paths_match_by_whole_components() {
        assert!(contains("/home/me/shop", "/home/me/shop"));
        assert!(contains("/home/me/shop/", "/home/me/shop/src"));
        assert!(contains("/home/me/shop", "/home/me/shop/src/"));
        assert!(!contains("/home/me/shop", "/home/me/shop-taxes"));
        assert!(!contains("/home/me/shop/src", "/home/me/shop"));
        assert!(contains("/", "/home"), "the root holds everything");
    }

    #[test]
    fn the_deepest_worktree_wins() {
        let list = shop();
        assert_eq!(of("/home/me/shop/src", &list).as_deref(), Some("/home/me/shop"));
        assert_eq!(
            of("/home/me/shop/.claude/worktrees/invoices/src/pdf", &list).as_deref(),
            Some("/home/me/shop/.claude/worktrees/invoices"),
            "nested in the main checkout, attributed to the linked one"
        );
        assert_eq!(of("/home/me/shop/.claude/worktrees", &list).as_deref(), Some("/home/me/shop"));
        assert_eq!(of("/home/me/shop-taxes", &list).as_deref(), Some("/home/me/shop-taxes/"), "trailing slash on the worktree");
        assert_eq!(of("/home/me/shop-taxes/", &list).as_deref(), Some("/home/me/shop-taxes/"), "trailing slash on the folder");
        assert_eq!(of("/home/me/blog", &list), None, "outside any worktree");
        assert_eq!(of("/home/me", &list), None, "a parent is not inside");
        let mut bare = wt("/srv/shop.git", true, None);
        bare.bare = true;
        assert_eq!(of("/srv/shop.git/hooks", &[bare]), None, "a bare repository has no checkout");
    }

    fn world_with_cards(cwds: &[&str]) -> (World, Vec<String>) {
        let mut w = World::default();
        w.projects.push(Project {
            id: "p".into(),
            name: "shop".into(),
            color: [0; 3],
            rect: Rect::ZERO,
            cwd: Some("~/shop".into()),
            templates: Vec::new(),
            messages: Vec::new(),
            worktree_location: None,
            task_settings: Default::default(),
        });
        let mut ids = vec![];
        for cwd in cwds {
            let card = Card::new(Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0)), Some(cwd.to_string()));
            ids.push(card.id.clone());
            w.add_card(card);
        }
        (w, ids)
    }

    fn list(key: &str, worktrees: Vec<Worktree>) -> WorktreeList {
        WorktreeList { key: key.into(), repo: Some("/home/me/shop".into()), default_branch: Some("main".into()), worktrees, merged_branches: vec![] }
    }

    #[test]
    fn cards_get_the_worktree_of_their_folder() {
        let (mut w, ids) = world_with_cards(&["/home/me/shop", "/home/me/shop/.claude/worktrees/invoices", "/home/me/blog"]);
        let mut s = Worktrees::default();
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        s.poll(&mut w);
        assert_eq!(w.card_worktrees.get(&ids[0]), Some(&CardWorktree { path: "/home/me/shop".into(), branch: Some("main".into()), linked: None }));
        assert_eq!(
            w.card_worktrees.get(&ids[1]),
            Some(&CardWorktree { path: "/home/me/shop/.claude/worktrees/invoices".into(), branch: Some("feat/invoices".into()), linked: Some("invoices".into()) })
        );
        assert_eq!(w.card_worktrees.get(&ids[2]), None);

        // Unlinking the folder forgets the repository, and the badges go.
        s.handle(&Event::SetProjectCwd("p".into(), None), &mut w, &mut vec![]);
        s.poll(&mut w);
        assert!(w.card_worktrees.is_empty());
    }

    #[test]
    fn relinking_the_same_folder_keeps_the_list() {
        let (mut w, _) = world_with_cards(&[]);
        let mut s = Worktrees::default();
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        s.handle(&Event::SetProjectCwd("p".into(), Some("~/shop".into())), &mut w, &mut vec![]);
        assert!(s.list("p").is_some(), "same folder: the agent sends nothing new, keep it");
        s.handle(&Event::SetProjectCwd("p".into(), Some("~/blog".into())), &mut w, &mut vec![]);
        assert!(s.list("p").is_none(), "another folder");
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        s.handle(&Event::RemoveProject("p".into()), &mut w, &mut vec![]);
        assert!(s.list("p").is_none(), "group removed");
    }

    #[test]
    fn the_strip_shows_only_for_the_open_group_with_linked_worktrees() {
        let (mut w, _) = world_with_cards(&[]);
        let mut s = Worktrees::default();
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        assert!(s.shown(&w).is_none(), "'All' view");
        w.space = Some("p".into());
        assert_eq!(s.shown(&w).map(|l| l.worktrees.len()), Some(3));
        // Only the main checkout: nothing to show.
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", vec![wt("/home/me/shop", true, Some("main"))]))), &mut w, &mut vec![]);
        assert!(s.shown(&w).is_none());
        // Outside git: the list is dropped.
        s.handle(&Event::Agent(AgentEvent::Worktrees(WorktreeList { key: "p".into(), repo: None, default_branch: None, worktrees: vec![], merged_branches: vec![] })), &mut w, &mut vec![]);
        assert!(s.list("p").is_none());
        // A group without a folder (or unknown) keeps nothing.
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("ghost", shop()))), &mut w, &mut vec![]);
        assert!(s.list("ghost").is_none());
    }

    /// Opens the dialog of group `p`, answers its plan, and sends a creation of `name`.
    fn send_creation(s: &mut Worktrees, w: &World, name: &str, now: std::time::Instant) -> u64 {
        let dialog = s.create.as_mut().expect("open");
        dialog.name = name.into();
        let mut events = vec![];
        dialog.ask_plan(&mut events, now);
        let Some(Event::ToAgent(protocol::AgentCmd::PlanWorktree { id, folder, location, .. })) = events.pop() else { panic!("a plan") };
        let plan = protocol::WorktreePlan {
            id,
            key: "p".into(),
            branches: vec![],
            current_branch: None,
            default_branch: Some("main".into()),
            path: Some(format!("/home/me/shop/.claude/worktrees/{folder}")),
            folder,
            location,
            problem: None,
            free_folder: None,
            exclude: None,
            recipe: None,
        };
        dialog.handle(&AgentEvent::WorktreePlan(Box::new(plan)));
        let sent = dialog.submit(w, &Trust::default(), &mut vec![], now).expect("sent");
        let id = sent.id;
        s.sent.push(sent);
        id
    }

    fn created(id: u64, path: &str) -> Event {
        Event::Agent(AgentEvent::WorktreeCreated { id, key: "p".into(), path: Some(path.into()), problem: None, excluded: false, recipe_pending: false })
    }

    #[test]
    fn a_creation_answered_after_its_dialog_expired_or_closed_still_opens_its_card() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::En);
        let (mut w, _) = world_with_cards(&[]);
        let mut s = Worktrees::default();
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        let now = std::time::Instant::now();
        let new_card = |out: &[Event], path: &str| out.iter().any(|e| matches!(e, Event::NewCard { project: Some(p), cwd: Some(c), .. } if p == "p" && c == path));

        // The dialog gave up waiting (no answer in time); the answer comes later.
        s.create = None;
        s.handle(&Event::NewWorktree("p".into()), &mut w, &mut vec![]);
        let id = send_creation(&mut s, &w, "slow", now);
        s.create.as_mut().unwrap().expire(now + cleanup::PENDING_TIMEOUT);
        assert!(!s.create.as_ref().unwrap().creating());
        let mut out = vec![];
        s.handle(&created(id, "/home/me/shop/.claude/worktrees/slow"), &mut w, &mut out);
        assert!(new_card(&out, "/home/me/shop/.claude/worktrees/slow"), "{out:?}");
        assert_eq!(s.creations_waiting(), 0);

        // git still running, then the dialog closed: the final answer still opens the card.
        s.create = None;
        s.handle(&Event::NewWorktree("p".into()), &mut w, &mut vec![]);
        let id = send_creation(&mut s, &w, "later", now);
        let still =
            Event::Agent(AgentEvent::WorktreeCreated { id, key: "p".into(), path: None, problem: Some(protocol::Problem::GitStillRunning { secs: 60 }), excluded: false, recipe_pending: false });
        let mut out = vec![];
        s.handle(&still, &mut w, &mut out);
        assert!(out.is_empty() && s.creations_waiting() == 1, "still waited for");
        s.create = None;
        s.handle(&created(id, "/home/me/shop/.claude/worktrees/later"), &mut w, &mut out);
        assert!(new_card(&out, "/home/me/shop/.claude/worktrees/later"), "{out:?}");

        // The group went meanwhile: no card, a notice naming the folder.
        s.create = None;
        s.handle(&Event::NewWorktree("p".into()), &mut w, &mut vec![]);
        let id = send_creation(&mut s, &w, "orphan", now);
        w.projects.clear();
        let mut out = vec![];
        s.handle(&created(id, "/home/me/shop/.claude/worktrees/orphan"), &mut w, &mut out);
        assert!(!out.iter().any(|e| matches!(e, Event::NewCard { .. })));
        assert!(matches!(&out[..], [Event::Inform { body, .. }] if body.contains("/home/me/shop/.claude/worktrees/orphan")), "{out:?}");
        // An answer nobody waits for opens nothing.
        let mut out = vec![];
        s.handle(&created(999, "/x"), &mut w, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn the_recipe_report_says_what_was_done_and_why_not() {
        use protocol::{RecipeItem, SkipReason};
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let item = |action, path: &str, skipped| RecipeItem { action, path: path.into(), skipped };
        let report = RecipeReport {
            items: vec![
                item(RecipeAction::Copy, ".env", None),
                item(RecipeAction::Copy, "tracked.txt", Some(SkipReason::Exists)),
                item(RecipeAction::Copy, "data", Some(SkipReason::CapReached { max_bytes: 4 << 30, max_entries: 200_000 })),
                item(RecipeAction::Symlink, "node_modules", None),
                item(RecipeAction::Symlink, "vendor", Some(SkipReason::NoMatch)),
            ],
            omitted: 3,
            omitted_failures: 2,
            problem: Some(protocol::Problem::RecipeWrongType { file: ".tcanvas.toml".into(), key: "worktree.copy".into() }),
        };
        assert_eq!(
            report_lines(&report),
            vec![
                (".tcanvas.toml : worktree.copy n'a pas le type attendu (une liste de textes, ou un texte pour setup) : sa recette est ignorée".to_string(), false),
                ("Copié : .env".into(), true),
                ("Pas copié : tracked.txt (déjà dans le worktree, gardé)".into(), false),
                ("Pas copié : data (limite de copie atteinte (4,0 Go ou 200000 entrées en tout) : copies arrêtées)".into(), false),
                ("Lié : node_modules".into(), true),
                ("Pas lié : vendor (rien ne correspond)".into(), false),
                ("... et 2 de plus non faits".into(), false),
                ("... et 3 de plus".into(), true),
            ]
        );
    }

    /// The agent's answers: the worktree created (recipe pending or not), then the recipe's report.
    fn answer(id: u64, recipe_pending: bool) -> Event {
        Event::Agent(AgentEvent::WorktreeCreated { id, key: "p".into(), path: Some("/w".into()), problem: None, excluded: false, recipe_pending })
    }

    fn applied(id: u64, report: &RecipeReport) -> Event {
        Event::Agent(AgentEvent::RecipeApplied { id, key: "p".into(), path: "/w".into(), report: Box::new(report.clone()) })
    }

    fn new_cards(out: &[Event]) -> usize {
        out.iter().filter(|e| matches!(e, Event::NewCard { .. })).count()
    }

    #[test]
    fn a_creation_the_agent_never_answered_ends_in_its_dialog_and_is_forgotten() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let (mut w, _) = world_with_cards(&[]);
        let mut s = Worktrees::default();
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        s.handle(&Event::NewWorktree("p".into()), &mut w, &mut vec![]);
        let id = send_creation(&mut s, &w, "r", std::time::Instant::now());
        assert!(s.create.as_ref().unwrap().creating() && s.creations_waiting() == 1);
        let mut out = vec![];
        s.handle(&Event::AgentLost, &mut w, &mut out);
        let dialog = s.create.as_ref().expect("still open, saying why");
        assert!(!dialog.creating() && dialog.lost(), "not creating any more");
        assert_eq!(s.creations_waiting(), 0);
        assert!(out.is_empty());
        // A late answer with that id (the old agent's, replayed) changes nothing.
        let mut out = vec![];
        s.handle(&answer(id, false), &mut w, &mut out);
        assert!(out.is_empty() && s.create.is_some(), "{out:?}");
        // A new agent's hello does the same.
        let id = send_creation(&mut s, &w, "r2", std::time::Instant::now());
        let hello = AgentEvent::Hello {
            version: "1".into(),
            hooks: protocol::Hooks::Installed,
            send_text: true,
            tasks: true,
            task_hooks: None,
            task_links: true,
            worktree_refresh: true,
            review: true,
            shell: None,
        };
        s.handle(&Event::Agent(hello), &mut w, &mut vec![]);
        assert!(s.create.as_ref().unwrap().lost() && s.creations_waiting() == 0);
        let mut out = vec![];
        s.handle(&answer(id, true), &mut w, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_recipe_the_agent_stopped_applying_opens_a_plain_shell_and_offers_what_did_not_run() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let (mut w, _) = world_with_cards(&[]);
        w.projects[0].templates = vec![crate::shell::world::CommandTemplate { name: "Claude".into(), command: "claude".into(), task: false }];
        let mut s = Worktrees::default();
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        s.handle(&Event::NewWorktree("p".into()), &mut w, &mut vec![]);
        s.create.as_mut().unwrap().template = Some(0);
        let id = send_creation(&mut s, &w, "r", std::time::Instant::now());
        s.handle(&answer(id, true), &mut w, &mut vec![]);
        let mut out = vec![];
        s.handle(&Event::AgentLost, &mut w, &mut out);
        assert!(
            matches!(&out[..], [Event::Inform { title, body }, Event::NewCard { command: None, setup: None, cwd: Some(p), .. }] if title == "Recette interrompue" && body.contains("/w") && p == "/w"),
            "{out:?}"
        );
        assert_eq!(s.recipe_interrupted(), Some(&Interrupted { path: "/w".into(), setup: None, command: Some("claude".into()) }));
        assert_eq!(s.recipe_report(), Some(&("/w".to_string(), None)), "the report window stays, saying what is unknown");
        // The report that never came, coming anyway: nothing opens twice.
        let mut out = vec![];
        s.handle(&applied(id, &RecipeReport::default()), &mut w, &mut out);
        assert!(out.is_empty());

        // A task's start: its card does not open (the task keeps its links: Resume), what it would
        // have run is offered.
        let card = crate::shell::event::TaskCard {
            card: "c1".into(),
            project: "p".into(),
            cwd: "/w".into(),
            command: "claude \"$TCANVAS_TASK\"".into(),
            setup: Some("npm ci".into()),
            task: crate::shell::event::TaskEnv { reference: "k3f".into(), title: "Invoice".into(), prompt: "p".into() },
        };
        let task = create::SentTask { task: "pdf".into(), branch: "task/invoice-k3f".into(), card };
        s.sent.push(create::Sent { id: 77, project: "p".into(), command: None, setup: Some("npm ci".into()), repo: None, location: None, task: Some(Box::new(task)) });
        let mut out = vec![];
        s.handle(&answer(77, true), &mut w, &mut out);
        assert!(out.iter().any(|e| matches!(e, Event::TaskWorktreeCreated { task, .. } if task == "pdf")), "its links are set: {out:?}");
        assert!(!out.iter().any(|e| matches!(e, Event::NewTaskCard(_))));
        let mut out = vec![];
        s.handle(&Event::AgentLost, &mut w, &mut out);
        assert!(matches!(&out[..], [Event::Inform { .. }]), "no card: {out:?}");
        assert_eq!(s.recipe_interrupted().map(|i| (i.setup.as_deref(), i.command.as_deref())), Some((Some("npm ci"), Some("claude \"$TCANVAS_TASK\""))));
    }

    #[test]
    fn a_card_with_something_to_run_waits_for_the_recipe() {
        let (mut w, _) = world_with_cards(&[]);
        w.projects[0].templates = vec![crate::shell::world::CommandTemplate { name: "Claude".into(), command: "claude".into(), task: false }];
        let mut s = Worktrees::default();
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        s.handle(&Event::NewWorktree("p".into()), &mut w, &mut vec![]);
        s.create.as_mut().unwrap().template = Some(0);
        let id = send_creation(&mut s, &w, "r", std::time::Instant::now());
        let report = RecipeReport { items: vec![protocol::RecipeItem { action: RecipeAction::Copy, path: ".env".into(), skipped: None }], ..RecipeReport::default() };
        // Someone else's: nothing.
        let mut out = vec![];
        s.handle(&applied(999, &report), &mut w, &mut out);
        assert!(out.is_empty() && s.recipe_report().is_none());
        // Created, recipe pending: the dialog is done, the card waits, the report says it runs.
        s.handle(&answer(id, true), &mut w, &mut out);
        assert_eq!(new_cards(&out), 0, "the command never starts before the copies: {out:?}");
        assert!(s.create.is_none(), "the dialog is done: no \"no answer\"");
        assert_eq!(s.recipe_report(), Some(&("/w".to_string(), None)));
        // The copies are done: now the card opens, with its command.
        let mut out = vec![];
        s.handle(&applied(id, &report), &mut w, &mut out);
        assert!(matches!(&out[..], [Event::NewCard { command: Some(c), cwd: Some(p), .. }] if c == "claude" && p == "/w"), "{out:?}");
        assert_eq!(s.recipe_report(), Some(&("/w".to_string(), Some(report.clone()))));
        let mut out = vec![];
        s.handle(&applied(id, &report), &mut w, &mut out);
        assert!(out.is_empty(), "once");
    }

    #[test]
    fn a_plain_shell_opens_at_once_and_the_setup_is_accepted_only_on_success() {
        let (mut w, _) = world_with_cards(&[]);
        let mut s = Worktrees::default();
        s.handle(&Event::Agent(AgentEvent::Worktrees(list("p", shop()))), &mut w, &mut vec![]);
        s.handle(&Event::NewWorktree("p".into()), &mut w, &mut vec![]);
        let id = send_creation(&mut s, &w, "r", std::time::Instant::now());
        let mut out = vec![];
        s.handle(&answer(id, true), &mut w, &mut out);
        assert_eq!(new_cards(&out), 1, "nothing to run: no need to wait");
        // A ticked setup: remembered when the creation goes through, not when it is refused.
        let setup = |id| create::Sent { id, project: "p".into(), command: None, setup: Some("npm ci".into()), repo: Some("/home/me/shop".into()), location: None, task: None };
        s.sent.push(setup(50));
        let refused = Event::Agent(AgentEvent::WorktreeCreated {
            id: 50,
            key: "p".into(),
            path: None,
            problem: Some(protocol::Problem::BranchExists { branch: "a".into() }),
            excluded: false,
            recipe_pending: false,
        });
        s.handle(&refused, &mut w, &mut vec![]);
        assert!(!s.trust.accepted("/home/me/shop", "npm ci"));
        s.sent.push(setup(51));
        let mut out = vec![];
        s.handle(&answer(51, true), &mut w, &mut out);
        assert!(s.trust.accepted("/home/me/shop", "npm ci"));
        assert_eq!(new_cards(&out), 0, "the setup waits for the copies");
    }

    #[test]
    fn names_and_ages() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        assert_eq!(folder_name(&wt("/home/me/shop-taxes/", false, None)), "shop-taxes");
        assert_eq!(age(10_000, 9_990), "à l'instant");
        assert_eq!(age(10_000, 10_000 - 7_200), "il y a 2 h");
        assert_eq!((t().last_commit)(&age(1_000_000, 1_000_000 - 3 * 86_400)), "Dernier commit il y a 3 j");
    }
}
