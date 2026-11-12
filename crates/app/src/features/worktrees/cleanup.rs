//! Cleanup window, opened from the worktrees strip: the open group's linked worktrees that are
//! safe to remove (with their size and the total to reclaim), the others with the reason they
//! are kept, the merged branches without a worktree, and a prune action for entries whose folder
//! is missing. Every removal goes through a confirmation, by click only, listing exactly what
//! will be deleted (ignored files included); the agent checks the same rule again on its own
//! fresh data before it deletes anything.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use egui::{Color32, Grid, Id, RichText, ScrollArea, Ui, Window};
use protocol::{AgentCmd, AgentEvent, Cleanup, Ignored, KeepReason, Problem, Usage, Worktree, WorktreeList, WorktreeSize, holds_worktree, keep_reason};

use super::{contains, folder_name};
use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::world::{ProjectId, World};
use crate::shell::{theme, widgets};

const DONE: Color32 = Color32::from_rgb(90, 200, 120);
const REFUSED: Color32 = Color32::from_rgb(230, 180, 80);
/// Results kept in the window, newest last.
const MAX_RESULTS: usize = 8;
/// An action the agent has not answered by then is given up (an older agent ignores the
/// command; a restarted one forgot it).
pub const PENDING_TIMEOUT: Duration = Duration::from_secs(120);

/// The safe-to-remove rule as the app sees it: another worktree inside, a card's folder in it,
/// or a group's.
pub fn verdict(wt: &Worktree, list: &WorktreeList, world: &World) -> Option<KeepReason> {
    let usage = Usage {
        holds_worktree: holds_worktree(wt, &list.worktrees),
        terminal: world.card_worktrees.values().any(|w| w.path == wt.path),
        group_folder: world.projects.iter().any(|p| p.cwd.as_deref().is_some_and(|c| contains(&wt.path, c))),
        ..Usage::default()
    };
    keep_reason(wt, &usage)
}

/// The worktrees safe to remove, in list order.
pub fn removable<'a>(list: &'a WorktreeList, world: &World) -> Vec<&'a Worktree> {
    list.worktrees.iter().filter(|w| verdict(w, list, world).is_none()).collect()
}

/// The branch to delete along with a worktree: when asked, and only if `git branch -d` will
/// accept it.
pub fn branch_to_delete(wt: &Worktree, with_branch: bool) -> Option<String> {
    wt.branch.clone().filter(|_| with_branch && wt.branch_deletable)
}

/// What the confirmation lists: exactly the folders (with what git ignores in each), branches and
/// entries that will be deleted. `measured`: the agent's answers to the size requests.
pub fn confirmation(actions: &[Cleanup], measured: &BTreeMap<String, WorktreeSize>) -> String {
    let mut lines = vec![t().cleanup_confirm_intro.to_string()];
    let mut folders = false;
    for action in actions {
        match action {
            Cleanup::RemoveWorktree { path, delete_branch, .. } => {
                folders = true;
                lines.push(format!("  • {}", (t().cleanup_confirm_folder)(path)));
                let ignored = match measured.get(path).and_then(|m| m.ignored.as_ref()) {
                    Some(i) if i.total == 0 => t().cleanup_confirm_ignored_none.to_string(),
                    Some(i) => {
                        let mut list = i.shown.join(", ");
                        if i.total > i.shown.len() {
                            list = format!("{list}, {}", (t().cleanup_ignored_more)(i.total - i.shown.len()));
                        }
                        (t().cleanup_confirm_ignored)(&list)
                    }
                    None => t().cleanup_confirm_ignored_unknown.to_string(),
                };
                lines.push(format!("      {ignored}"));
                if let Some(branch) = delete_branch {
                    lines.push(format!("  • {}", (t().cleanup_confirm_branch)(branch)));
                }
            }
            Cleanup::Prune { paths } => {
                for path in paths {
                    lines.push(format!("  • {}", (t().cleanup_confirm_entry)(path)));
                }
            }
            Cleanup::DeleteBranch { branch } => lines.push(format!("  • {}", (t().cleanup_confirm_branch)(branch))),
        }
    }
    if folders {
        lines.push(String::new());
        lines.push(t().cleanup_confirm_note.to_string());
    }
    lines.join("\n")
}

/// The actions as sent, each removal carrying the ignored files the confirmation listed (the
/// agent refuses if they changed). `None` while a folder's ignored files are not listed yet: the
/// confirmation cannot be clicked then.
pub fn ready(actions: &[Cleanup], measured: &BTreeMap<String, WorktreeSize>) -> Option<Vec<Cleanup>> {
    actions
        .iter()
        .map(|a| match a {
            Cleanup::RemoveWorktree { path, delete_branch, .. } => {
                let ignored = measured.get(path)?.ignored.clone()?;
                Some(Cleanup::RemoveWorktree { path: path.clone(), delete_branch: delete_branch.clone(), ignored })
            }
            other => Some(other.clone()),
        })
        .collect()
}

/// What the agent answered (or did not).
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub action: Cleanup,
    pub deleted_branch: Option<String>,
    pub problem: Option<Problem>,
    /// No answer in `PENDING_TIMEOUT`.
    pub timed_out: bool,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        self.problem.is_none() && !self.timed_out
    }

    pub fn text(&self) -> String {
        if self.timed_out {
            return t().cleanup_no_answer.into();
        }
        if let Some(problem) = &self.problem {
            return t().problem(problem);
        }
        match (&self.action, &self.deleted_branch) {
            (Cleanup::RemoveWorktree { path, .. }, Some(branch)) => (t().cleanup_removed_with_branch)(path, branch),
            (Cleanup::RemoveWorktree { path, .. }, None) => (t().cleanup_removed)(path),
            (Cleanup::Prune { .. }, _) => t().cleanup_pruned.into(),
            (Cleanup::DeleteBranch { branch }, _) => (t().cleanup_branch_deleted)(branch),
        }
    }
}

/// What an action deletes, whatever its options.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Worktree(String),
    Branch(String),
    Prune,
}

fn target(action: &Cleanup) -> Target {
    match action {
        Cleanup::RemoveWorktree { path, .. } => Target::Worktree(path.clone()),
        Cleanup::DeleteBranch { branch } => Target::Branch(branch.clone()),
        Cleanup::Prune { .. } => Target::Prune,
    }
}

/// The list still shows what the action deleted (the agent's next scan has not come yet).
fn still_listed(action: &Cleanup, list: &WorktreeList) -> bool {
    match action {
        Cleanup::RemoveWorktree { path, .. } => list.worktrees.iter().any(|w| &w.path == path),
        Cleanup::DeleteBranch { branch } => list.merged_branches.iter().any(|b| &b.name == branch),
        Cleanup::Prune { paths } => list.worktrees.iter().any(|w| w.prunable && paths.contains(&w.path)),
    }
}

/// The open cleanup window of one group.
pub struct CleanupWindow {
    pub project: ProjectId,
    /// Delete the merged branch along with its worktree.
    pub delete_branch: bool,
    /// Actions waiting for the confirmation.
    pub confirm: Option<Vec<Cleanup>>,
    /// Sent, not answered yet, with when.
    pub pending: Vec<(Cleanup, Instant)>,
    /// Done, but the list has not caught up yet: hidden meanwhile.
    pub done: Vec<Cleanup>,
    pub results: Vec<Outcome>,
    measured: BTreeMap<String, WorktreeSize>,
    /// Paths whose size was asked since the window opened.
    asked: BTreeSet<String>,
}

impl CleanupWindow {
    pub fn new(project: ProjectId) -> Self {
        Self { project, delete_branch: true, confirm: None, pending: Vec::new(), done: Vec::new(), results: Vec::new(), measured: BTreeMap::new(), asked: BTreeSet::new() }
    }

    pub fn handle(&mut self, ev: &AgentEvent) {
        match ev {
            AgentEvent::Cleaned { key, action, deleted_branch, problem } if *key == self.project => {
                self.pending.retain(|(a, _)| a != action);
                if problem.is_none() {
                    self.done.push(action.clone());
                }
                self.push(Outcome { action: action.clone(), deleted_branch: deleted_branch.clone(), problem: problem.clone(), timed_out: false });
            }
            AgentEvent::WorktreeSizes { key, sizes } if *key == self.project => {
                for s in sizes {
                    self.measured.insert(s.path.clone(), s.clone());
                }
            }
            AgentEvent::Worktrees(list) if list.key == self.project => self.done.retain(|a| still_listed(a, list)),
            _ => {}
        }
    }

    fn push(&mut self, outcome: Outcome) {
        self.results.push(outcome);
        let extra = self.results.len().saturating_sub(MAX_RESULTS);
        self.results.drain(..extra);
    }

    /// Gives up on actions the agent never answered.
    pub fn expire(&mut self, now: Instant) {
        let (late, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending).into_iter().partition(|(_, at)| now.duration_since(*at) >= PENDING_TIMEOUT);
        self.pending = waiting;
        for (action, _) in late {
            self.push(Outcome { action, deleted_branch: None, problem: None, timed_out: true });
        }
    }

    fn busy(&self, action: &Cleanup) -> bool {
        self.pending.iter().any(|(p, _)| target(p) == target(action))
    }

    fn gone(&self, action: &Cleanup) -> bool {
        self.done.iter().any(|d| target(d) == target(action))
    }

    /// The removable worktrees still to offer (not removed meanwhile).
    fn offered<'a>(&self, list: &'a WorktreeList, world: &World) -> Vec<&'a Worktree> {
        removable(list, world).into_iter().filter(|w| !self.gone(&self.remove_action(w))).collect()
    }

    fn remove_action(&self, wt: &Worktree) -> Cleanup {
        // `ignored` is filled with what the confirmation shows, when it is confirmed.
        Cleanup::RemoveWorktree { path: wt.path.clone(), delete_branch: branch_to_delete(wt, self.delete_branch), ignored: Ignored::default() }
    }

    /// Sizes (and ignored files) are measured by the agent on request only: asked once per
    /// opening, for the removable worktrees not asked yet.
    fn ask_sizes(&mut self, removable: &[&Worktree], events: &mut Vec<Event>) {
        let paths: Vec<String> = removable.iter().map(|w| w.path.clone()).filter(|p| !self.asked.contains(p)).collect();
        if !paths.is_empty() {
            self.asked.extend(paths.iter().cloned());
            events.push(Event::ToAgent(AgentCmd::WorktreeSizes { key: self.project.clone(), paths }));
        }
    }

    /// "…" while measuring, "unknown" when the agent could not tell.
    fn size_text(&self, path: &str) -> String {
        match self.measured.get(path) {
            Some(WorktreeSize { bytes: Some(bytes), .. }) => (t().size)(*bytes),
            Some(_) => t().size_unknown.into(),
            None => "…".into(),
        }
    }

    fn total_text(&self, removable: &[&Worktree]) -> String {
        let answers: Vec<Option<u64>> = removable.iter().filter_map(|w| self.measured.get(&w.path).map(|m| m.bytes)).collect();
        if answers.len() < removable.len() {
            return "…".into();
        }
        let known = (t().size)(answers.iter().flatten().sum());
        if answers.iter().any(Option::is_none) { format!("{known} + {}", t().size_unknown) } else { known }
    }

    /// Opens the confirmation. The folders to remove are listed again (ignored files may have
    /// changed since the window opened): until the answer, the confirmation cannot be clicked.
    fn ask(&mut self, actions: Vec<Cleanup>) {
        if actions.is_empty() {
            return;
        }
        for action in &actions {
            if let Cleanup::RemoveWorktree { path, .. } = action {
                self.measured.remove(path);
                self.asked.remove(path);
            }
        }
        self.confirm = Some(actions);
    }

    /// Draws the window (and its confirmation). `false`: closed.
    pub fn ui(&mut self, ctx: &egui::Context, list: &WorktreeList, world: &World, events: &mut Vec<Event>) -> bool {
        self.expire(Instant::now());
        if !self.pending.is_empty() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        let offered = self.offered(list, world);
        self.ask_sizes(&offered, events);
        let mut open = true;
        Window::new(t().cleanup_title).id(Id::unique("worktree-cleanup")).collapsible(false).resizable(false).default_width(560.0).open(&mut open).show(ctx, |ui| {
            let max = ctx.content_rect().height() * 0.6;
            ScrollArea::vertical().max_height(max).show(ui, |ui| self.body(ui, list, world, &offered));
        });
        if let Some(actions) = self.confirm.clone() {
            let ready = ready(&actions, &self.measured);
            match widgets::confirm_by_click(ctx, "worktree-cleanup", t().cleanup_confirm_title, &confirmation(&actions, &self.measured), t().cleanup_confirm_ok, ready.is_some()) {
                Some(true) => {
                    for action in ready.unwrap_or_default() {
                        events.push(Event::ToAgent(AgentCmd::Cleanup { key: self.project.clone(), action: action.clone() }));
                        self.pending.push((action, Instant::now()));
                    }
                    self.confirm = None;
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }
        open
    }

    fn body(&mut self, ui: &mut Ui, list: &WorktreeList, world: &World, offered: &[&Worktree]) {
        let base = list.default_branch.as_deref();
        let kept: Vec<(&Worktree, KeepReason)> = list.worktrees.iter().filter_map(|w| verdict(w, list, world).map(|r| (w, r))).collect();
        let prunable: Vec<String> = list.worktrees.iter().filter(|w| w.prunable).map(|w| w.path.clone()).collect();
        let prune = Cleanup::Prune { paths: prunable.clone() };
        let prune_offered = !prunable.is_empty() && !self.gone(&prune);
        let branches: Vec<&protocol::MergedBranch> = list.merged_branches.iter().filter(|b| !self.gone(&Cleanup::DeleteBranch { branch: b.name.clone() })).collect();
        if offered.is_empty() && !prune_offered && branches.is_empty() {
            ui.label(RichText::new(t().cleanup_nothing).color(theme::TEXT_DIM));
        }

        if !offered.is_empty() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(t().cleanup_removable).strong().color(theme::TEXT));
                ui.label(RichText::new((t().cleanup_total)(&self.total_text(offered))).color(theme::TEXT_DIM));
            });
            // Above the list: a long list does not push them out of view.
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.delete_branch, t().cleanup_with_branch);
                let all: Vec<Cleanup> = offered.iter().map(|w| self.remove_action(w)).filter(|a| !self.busy(a)).collect();
                if ui.add_enabled(!all.is_empty(), egui::Button::new((t().cleanup_remove_all)(all.len()))).clicked() {
                    self.ask(all);
                }
            });
            Grid::new("cleanup-removable").num_columns(4).spacing([12.0, 4.0]).show(ui, |ui| {
                for wt in offered {
                    ui.label(RichText::new(folder_name(wt)).color(theme::TEXT)).on_hover_text(&wt.path);
                    let branch = RichText::new(format!("⎇ {}", wt.branch.as_deref().unwrap_or(t().detached_head)));
                    if self.delete_branch && !wt.branch_deletable {
                        ui.label(branch.color(REFUSED)).on_hover_text(t().cleanup_branch_stays);
                    } else {
                        ui.label(branch.color(theme::TEXT_DIM));
                    }
                    ui.label(RichText::new(self.size_text(&wt.path)).color(theme::TEXT_DIM));
                    let action = self.remove_action(wt);
                    self.action_button(ui, action);
                    ui.end_row();
                }
            });
            ui.add_space(6.0);
        }

        if !kept.is_empty() {
            ui.label(RichText::new(t().cleanup_kept).strong().color(theme::TEXT));
            Grid::new("cleanup-kept").num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
                for (wt, reason) in &kept {
                    if wt.prunable && !prune_offered {
                        continue;
                    }
                    let name = if wt.main { t().main_checkout } else { folder_name(wt) };
                    ui.label(RichText::new(name).color(theme::TEXT)).on_hover_text(&wt.path);
                    ui.label(RichText::new(format!("⎇ {}", wt.branch.as_deref().unwrap_or(t().detached_head))).color(theme::TEXT_DIM));
                    ui.label(RichText::new(t().keep_reason(*reason, base)).color(theme::TEXT_DIM));
                    ui.end_row();
                }
            });
            if prune_offered {
                let button = egui::Button::new((t().cleanup_prune)(prunable.len()));
                if ui.add_enabled(!self.busy(&prune), button).on_hover_text(t().cleanup_prune_hint).clicked() {
                    self.ask(vec![prune]);
                }
            }
            ui.add_space(6.0);
        }

        if !branches.is_empty() {
            ui.label(RichText::new(t().cleanup_branches).strong().color(theme::TEXT));
            Grid::new("cleanup-branches").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                for branch in branches {
                    ui.label(RichText::new(format!("⎇ {}", branch.name)).color(theme::TEXT));
                    self.action_button(ui, Cleanup::DeleteBranch { branch: branch.name.clone() });
                    ui.end_row();
                }
            });
        }

        if !self.results.is_empty() {
            ui.separator();
            for outcome in &self.results {
                let (mark, color) = if outcome.ok() { ("✔", DONE) } else { ("⚠", REFUSED) };
                ui.label(RichText::new(format!("{mark} {}", outcome.text())).color(color));
            }
        }
    }

    /// "Remove…" for one item, or "in progress…" while the agent works on it.
    fn action_button(&mut self, ui: &mut Ui, action: Cleanup) {
        if self.busy(&action) {
            ui.label(RichText::new(t().cleanup_pending).small().color(theme::TEXT_DIM));
        } else if ui.button(t().cleanup_remove).clicked() {
            self.ask(vec![action]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::i18n::{self, Lang};
    use crate::shell::world::{Card, CardWorktree, Project};
    use egui::Rect;
    use protocol::MergedBranch;

    fn wt(path: &str, branch: &str) -> Worktree {
        Worktree {
            path: path.into(),
            head: Some("a".repeat(40)),
            branch: Some(branch.into()),
            detached: false,
            main: false,
            bare: false,
            locked: false,
            lock_reason: None,
            prunable: false,
            dirty: Some(false),
            ahead: Some(0),
            behind: Some(2),
            merged: Some(true),
            last_commit: None,
            branch_deletable: true,
            reflog_commits: false,
        }
    }

    fn list() -> WorktreeList {
        WorktreeList {
            key: "p".into(),
            repo: Some("/home/me/shop".into()),
            default_branch: Some("main".into()),
            worktrees: vec![
                Worktree { main: true, merged: None, ..wt("/home/me/shop", "main") },
                wt("/home/me/shop-taxes", "taxes"),
                Worktree { dirty: Some(true), ..wt("/home/me/shop-pdf", "pdf") },
                Worktree { prunable: true, dirty: None, ..wt("/home/me/shop-old", "old") },
                wt("/home/me/shop/.claude/worktrees/busy", "busy"),
            ],
            merged_branches: vec![MergedBranch { name: "fix/typo".into(), last_commit: None }],
        }
    }

    fn measured(path: &str, ignored: Option<Vec<&str>>, total: usize) -> BTreeMap<String, WorktreeSize> {
        let ignored = ignored.map(|names| names.into_iter().map(String::from).collect());
        let ignored = ignored.map(|shown: Vec<String>| Ignored { total: total.max(shown.len()), shown });
        BTreeMap::from([(path.to_string(), WorktreeSize { path: path.into(), bytes: Some(1), ignored })])
    }

    #[test]
    fn the_app_applies_the_rule_with_its_cards_groups_and_nested_worktrees() {
        let mut w = World::default();
        let mut l = list();
        let paths = |l: &WorktreeList, w: &World| removable(l, w).iter().map(|w| w.path.clone()).collect::<Vec<_>>();
        assert_eq!(paths(&l, &w), vec!["/home/me/shop-taxes", "/home/me/shop/.claude/worktrees/busy"]);
        // A card in the nested worktree.
        let card = Card::new(Rect::ZERO, Some("/home/me/shop/.claude/worktrees/busy/src".into()));
        w.card_worktrees.insert(card.id.clone(), CardWorktree { path: "/home/me/shop/.claude/worktrees/busy".into(), branch: Some("busy".into()), linked: Some("busy".into()) });
        assert_eq!(verdict(&l.worktrees[4], &l, &w), Some(KeepReason::HasTerminal));
        // A group whose folder is in a worktree.
        w.projects.push(Project {
            id: "q".into(),
            name: "taxes".into(),
            color: [0; 3],
            rect: Rect::ZERO,
            cwd: Some("/home/me/shop-taxes/web".into()),
            templates: Vec::new(),
            messages: Vec::new(),
            worktree_location: None,
            task_settings: Default::default(),
        });
        assert_eq!(verdict(&l.worktrees[1], &l, &w), Some(KeepReason::GroupFolder));
        assert!(paths(&l, &w).is_empty());
        assert_eq!(verdict(&l.worktrees[0], &l, &w), Some(KeepReason::MainCheckout));
        assert_eq!(verdict(&l.worktrees[2], &l, &w), Some(KeepReason::Dirty));
        assert_eq!(verdict(&l.worktrees[3], &l, &w), Some(KeepReason::Missing));
        // Another worktree inside a merged, clean one.
        let w = World::default();
        l.worktrees.push(wt("/home/me/shop-taxes/.claude/worktrees/inner", "inner"));
        assert_eq!(verdict(&l.worktrees[1], &l, &w), Some(KeepReason::HoldsWorktree));
    }

    #[test]
    fn a_removal_is_ready_only_once_its_ignored_files_are_listed() {
        let taxes = Cleanup::RemoveWorktree { path: "/home/me/shop-taxes".into(), delete_branch: None, ignored: Ignored::default() };
        let prune = Cleanup::Prune { paths: vec!["/home/me/shop-old".into()] };
        assert_eq!(ready(std::slice::from_ref(&taxes), &BTreeMap::new()), None, "not measured yet");
        assert_eq!(ready(std::slice::from_ref(&taxes), &measured("/home/me/shop-taxes", None, 0)), None, "measured, but git could not list them");
        let sent = ready(&[taxes, prune.clone()], &measured("/home/me/shop-taxes", Some(vec![".env", "web/.env.local"]), 0)).unwrap();
        let seen = Ignored { shown: vec![".env".into(), "web/.env.local".into()], total: 2 };
        assert_eq!(sent, vec![Cleanup::RemoveWorktree { path: "/home/me/shop-taxes".into(), delete_branch: None, ignored: seen }, prune.clone()], "what the user saw goes with the command");
        assert_eq!(ready(std::slice::from_ref(&prune), &BTreeMap::new()), Some(vec![prune]), "nothing to list for a prune");
    }

    #[test]
    fn opening_a_confirmation_lists_the_ignored_files_again() {
        let mut c = CleanupWindow::new("p".into());
        c.measured = measured("/home/me/shop-taxes", Some(vec![".env"]), 0);
        c.asked.insert("/home/me/shop-taxes".into());
        c.ask(vec![Cleanup::RemoveWorktree { path: "/home/me/shop-taxes".into(), delete_branch: None, ignored: Ignored::default() }]);
        assert!(c.measured.is_empty() && c.asked.is_empty(), "asked again; the confirmation waits for the answer");
    }

    #[test]
    fn the_branch_goes_only_when_asked_and_git_accepts_it() {
        let taxes = wt("/home/me/shop-taxes", "taxes");
        assert_eq!(branch_to_delete(&taxes, true).as_deref(), Some("taxes"));
        assert_eq!(branch_to_delete(&taxes, false), None);
        assert_eq!(branch_to_delete(&Worktree { branch_deletable: false, ..taxes }, true), None);
    }

    #[test]
    fn the_confirmation_lists_exactly_what_goes() {
        i18n::set(Lang::Fr);
        let taxes = |branch: Option<&str>| Cleanup::RemoveWorktree { path: "/home/me/shop-taxes".into(), delete_branch: branch.map(String::from), ignored: Ignored::default() };
        let one = confirmation(&[taxes(Some("taxes"))], &measured("/home/me/shop-taxes", Some(vec![".env", "target/", "web/.env.local"]), 0));
        assert!(
            one.starts_with("Ceci supprime :\n  • le dossier /home/me/shop-taxes\n      ignorés, supprimés aussi : .env, target/, web/.env.local\n  • la branche taxes\n\nLes fichiers ignorés"),
            "{one}"
        );
        assert!(one.contains("fichiers de configuration (.env), bases de données locales"), "{one}");
        let folder_only = confirmation(&[taxes(None)], &measured("/home/me/shop-taxes", Some(vec![]), 0));
        assert!(!folder_only.contains("branche") && folder_only.contains("aucun fichier ignoré"), "{folder_only}");
        let more = confirmation(&[taxes(None)], &measured("/home/me/shop-taxes", Some(vec![".env"]), 4));
        assert!(more.contains("ignorés, supprimés aussi : .env, et 3 de plus"), "{more}");
        let unknown = confirmation(&[taxes(None)], &BTreeMap::new());
        assert!(unknown.contains("fichiers ignorés : pas encore listés"), "{unknown}");
        let unreadable = confirmation(&[taxes(None)], &measured("/home/me/shop-taxes", None, 0));
        assert!(unreadable.contains("fichiers ignorés : pas encore listés"), "{unreadable}");
        let all = confirmation(
            &[taxes(Some("taxes")), Cleanup::RemoveWorktree { path: "/home/me/shop/.claude/worktrees/busy".into(), delete_branch: Some("busy".into()), ignored: Ignored::default() }],
            &BTreeMap::new(),
        );
        for line in ["le dossier /home/me/shop-taxes", "la branche taxes", "le dossier /home/me/shop/.claude/worktrees/busy", "la branche busy"] {
            assert!(all.contains(line), "{line} missing: {all}");
        }
        let prune = confirmation(&[Cleanup::Prune { paths: vec!["/home/me/shop-old".into()] }], &BTreeMap::new());
        assert!(prune.starts_with("Ceci supprime :\n  • le suivi git de /home/me/shop-old : son dossier est introuvable en ce moment"), "{prune}");
        assert!(prune.contains("disque démonté ou amovible") && prune.contains("index, son HEAD et son reflog"), "{prune}");
        assert_eq!(confirmation(&[Cleanup::DeleteBranch { branch: "fix/typo".into() }], &BTreeMap::new()), "Ceci supprime :\n  • la branche fix/typo");
        i18n::set(Lang::En);
        let prune = confirmation(&[Cleanup::Prune { paths: vec!["/home/me/shop-old".into()] }], &BTreeMap::new());
        assert!(prune.starts_with("This deletes:\n  • git's record of /home/me/shop-old: its folder is missing right now") && prune.contains("unmounted or removable disk"), "{prune}");
    }

    #[test]
    fn answered_items_stay_hidden_until_the_list_catches_up() {
        i18n::set(Lang::Fr);
        let l = list();
        let w = World::default();
        let mut c = CleanupWindow::new("p".into());
        let action = Cleanup::RemoveWorktree { path: "/home/me/shop-taxes".into(), delete_branch: Some("taxes".into()), ignored: Ignored::default() };
        c.pending.push((action.clone(), Instant::now()));
        assert!(c.busy(&Cleanup::RemoveWorktree { path: "/home/me/shop-taxes".into(), delete_branch: None, ignored: Ignored::default() }), "the same worktree, whatever the branch option");
        c.handle(&AgentEvent::Cleaned { key: "other".into(), action: action.clone(), deleted_branch: None, problem: None });
        assert_eq!(c.pending.len(), 1, "another group's answer");
        c.handle(&AgentEvent::Cleaned { key: "p".into(), action: action.clone(), deleted_branch: Some("taxes".into()), problem: None });
        assert!(c.pending.is_empty());
        assert_eq!(c.results[0].text(), "Supprimés : /home/me/shop-taxes et la branche taxes");
        // The stale list still has it: not offered again (nor in "remove all").
        assert_eq!(c.offered(&l, &w).iter().map(|w| w.path.as_str()).collect::<Vec<_>>(), vec!["/home/me/shop/.claude/worktrees/busy"]);
        c.handle(&AgentEvent::Worktrees(l.clone()));
        assert_eq!(c.done.len(), 1, "still listed: still hidden");
        let mut fresh = l.clone();
        fresh.worktrees.retain(|w| w.path != "/home/me/shop-taxes");
        c.handle(&AgentEvent::Worktrees(fresh));
        assert!(c.done.is_empty(), "gone from the list: forgotten");
        // A refusal does not hide it.
        c.handle(&AgentEvent::Cleaned { key: "p".into(), action: action.clone(), deleted_branch: None, problem: Some(Problem::NothingToPrune) });
        assert!(c.done.is_empty());
        let refused = Outcome { action, deleted_branch: None, problem: Some(Problem::WorktreeKept { path: "/x".into(), reason: KeepReason::HasTerminal }), timed_out: false };
        assert_eq!(refused.text(), "/x conservé : un terminal y est ouvert");
        for _ in 0..20 {
            c.handle(&AgentEvent::Cleaned { key: "p".into(), action: Cleanup::Prune { paths: vec![] }, deleted_branch: None, problem: Some(Problem::NothingToPrune) });
        }
        assert_eq!(c.results.len(), MAX_RESULTS);
    }

    #[test]
    fn unanswered_actions_time_out() {
        i18n::set(Lang::Fr);
        let mut c = CleanupWindow::new("p".into());
        let start = Instant::now();
        c.pending.push((Cleanup::DeleteBranch { branch: "fix/typo".into() }, start));
        c.expire(start + PENDING_TIMEOUT / 2);
        assert_eq!(c.pending.len(), 1);
        c.expire(start + PENDING_TIMEOUT);
        assert!(c.pending.is_empty());
        let last = c.results.last().unwrap();
        assert!(!last.ok());
        assert_eq!(last.text(), "Pas de réponse de l'agent : réessayer");
    }

    #[test]
    fn sizes_are_asked_once_per_opening() {
        let l = list();
        let w = World::default();
        let mut c = CleanupWindow::new("p".into());
        let mut events = vec![];
        c.ask_sizes(&removable(&l, &w), &mut events);
        c.ask_sizes(&removable(&l, &w), &mut events);
        assert_eq!(events.len(), 1);
        let Event::ToAgent(AgentCmd::WorktreeSizes { key, paths }) = &events[0] else { panic!("a size request") };
        assert_eq!((key.as_str(), paths.len()), ("p", 2));
        let size = |path: &str, bytes| WorktreeSize { path: path.into(), bytes, ignored: None };
        c.handle(&AgentEvent::WorktreeSizes { key: "p".into(), sizes: vec![size("/home/me/shop-taxes", Some(2048))] });
        i18n::set(Lang::En);
        let offered = removable(&l, &w);
        assert_eq!((c.size_text("/home/me/shop-taxes"), c.size_text("/elsewhere")), ("2.0 KB".into(), "…".into()));
        assert_eq!(c.total_text(&offered), "…", "one not answered yet");
        c.handle(&AgentEvent::WorktreeSizes { key: "p".into(), sizes: vec![size("/home/me/shop/.claude/worktrees/busy", None)] });
        assert_eq!(c.size_text("/home/me/shop/.claude/worktrees/busy"), "unknown");
        assert_eq!(c.total_text(&offered), "2.0 KB + unknown");
    }
}
