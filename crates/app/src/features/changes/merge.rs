//! Merging the reviewed branch into its base, from the review (`AgentCmd::Merge`).
//!
//! The agent prepares the result aside, in a private copy of its own, and only at the end moves
//! the base to it, by a fast-forward that does all or nothing; the branch's worktree is never
//! written. The Merge button of the review header is enabled only when the review allows it
//! (`block` says why not: the base is not a local branch or is checked out nowhere, a coding
//! agent is at work in the base's worktree, the agent cannot merge...). Being behind its base, or
//! uncommitted changes in the branch's worktree, are not reasons: the confirmation says them. The
//! confirmation (a click only, scrollable) says which branch goes into which, where and how (the
//! strategy, remembered per group), which hooks run, what the branch holds, and that nothing is
//! pushed; for a squash it holds the commit message (prefilled from the linked task, else from the
//! branch's commits). The outcome is shown in place, with the follow-ups as separate choices: the
//! cleanup of the worktree and its branch through the cleanup window, the linked task moved to
//! done, or, on a conflict, a terminal in the branch's worktree.

use egui::{Button, Color32, RichText, TextEdit, Ui};
use protocol::{AgentCmd, BaseKind, CommitInfo, MergeOutcome, MergePhase, MergeRecord, MergeStrategy, ReviewState, ReviewSummary, Task, TaskStatus, Worktree, WorktreeList, path_inside};

use super::review::{Act, same_path};
use super::{ADD, DEL, MODIFIED};
use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::world::{ProjectId, World};
use crate::shell::{theme, widgets};

/// The squash message field (it takes Enter as a new line).
const MESSAGE_ID: &str = "merge-message";

/// Why the Merge button cannot be clicked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// The agent cannot merge (an older one).
    Unsupported,
    /// A merge of this review is running.
    Running,
    /// No summary yet.
    Loading,
    Detached,
    /// The base is a remote-tracking branch; `local`: the local branch of the same name, if any.
    RemoteBase {
        base: String,
        local: Option<String>,
    },
    /// The base is a tag or a commit.
    OtherBase,
    NotCheckedOut {
        base: String,
    },
    NothingToMerge,
    Unrelated,
    /// A coding agent is working in the base's worktree (files change there at the end).
    AgentWorking {
        path: String,
    },
}

impl Block {
    pub fn text(&self) -> String {
        match self {
            Block::Unsupported => t().merge_block_unsupported.into(),
            Block::Running => t().merge_running.into(),
            Block::Loading => t().merge_block_loading.into(),
            Block::Detached => t().merge_block_detached.into(),
            Block::RemoteBase { base, .. } => (t().merge_block_remote)(base),
            Block::OtherBase => t().merge_block_other.into(),
            Block::NotCheckedOut { base } => (t().merge_block_not_checked_out)(base),
            Block::NothingToMerge => t().merge_block_nothing.into(),
            Block::Unrelated => t().merge_block_unrelated.into(),
            Block::AgentWorking { path } => (t().merge_block_agent)(path),
        }
    }

    /// Said in the panel under the header (the others are already said there, or are passing).
    pub fn shown(&self) -> bool {
        !matches!(self, Block::Loading | Block::NothingToMerge | Block::Unrelated | Block::Running)
    }
}

/// The worktree of `list` that has the local branch `base` checked out.
pub fn target<'a>(list: &'a WorktreeList, base: &str) -> Option<&'a Worktree> {
    list.worktrees.iter().find(|w| w.branch.as_deref() == Some(base) && !w.bare && !w.prunable)
}

/// What decides whether a merge can be asked.
pub struct Facts<'a> {
    pub summary: Option<&'a ReviewSummary>,
    /// The bases the review offers (local branches).
    pub bases: &'a [String],
    pub list: Option<&'a WorktreeList>,
    pub unsupported: bool,
    pub running: bool,
    /// A coding agent works in this folder (or below it) right now.
    pub agent_in: &'a dyn Fn(&str) -> bool,
}

/// Why a merge cannot be asked now (`None`: it can).
pub fn block(f: &Facts) -> Option<Block> {
    if f.unsupported {
        return Some(Block::Unsupported);
    }
    if f.running {
        return Some(Block::Running);
    }
    let Some(s) = f.summary else { return Some(Block::Loading) };
    let Some(_) = &s.branch else { return Some(Block::Detached) };
    match s.base_kind {
        BaseKind::Branch => {}
        BaseKind::Remote => {
            let local = s.base.split_once('/').map(|(_, name)| name.to_string()).filter(|name| f.bases.contains(name));
            return Some(Block::RemoteBase { base: s.base.clone(), local });
        }
        BaseKind::Other => return Some(Block::OtherBase),
    }
    match s.state {
        ReviewState::Ahead => {}
        ReviewState::UpToDate => return Some(Block::NothingToMerge),
        ReviewState::Unrelated => return Some(Block::Unrelated),
    }
    let Some(list) = f.list else { return Some(Block::Loading) };
    let Some(target) = target(list, &s.base) else { return Some(Block::NotCheckedOut { base: s.base.clone() }) };
    (f.agent_in)(&target.path).then(|| Block::AgentWorking { path: target.path.clone() })
}

/// A card's coding agent is at work in `path` (or below it): its status is running, and it is a
/// coding agent the app knows of (its hooks report its session, or it was started for a task).
/// The agent checks again with what it sees itself.
pub fn agent_busy(world: &World, path: &str) -> bool {
    world.cards.iter().any(|c| {
        let Some(info) = c.info.as_ref() else { return false };
        info.status == protocol::TermStatus::Running && (info.agent_session_id.is_some() || world.card_tasks.contains_key(&c.id)) && c.current_cwd().is_some_and(|cwd| path_inside(path, cwd))
    })
}

/// The task a review's worktree was started for, as it is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedTask {
    pub id: String,
    pub reference: String,
    pub title: String,
    pub status: TaskStatus,
}

/// The live task linked to `worktree` (else to `branch`), in a group's list.
pub fn linked_task(tasks: &[Task], worktree: &str, branch: Option<&str>) -> Option<LinkedTask> {
    let live = || tasks.iter().filter(|t| t.removed.is_none());
    let found = live().find(|t| t.worktree.as_deref().is_some_and(|w| same_path(w, worktree))).or_else(|| live().find(|t| branch.is_some() && t.branch.as_deref() == branch))?;
    Some(LinkedTask { id: found.id.clone(), reference: found.reference.clone(), title: found.title.clone(), status: found.status })
}

/// The squash message offered: the task's title (else the only commit's subject, else the branch's
/// name), then the branch's commit subjects, oldest first.
pub fn default_message(task_title: Option<&str>, branch: &str, commits: &[CommitInfo], more: u32) -> String {
    let title = task_title.map(str::trim).filter(|t| !t.is_empty());
    let first = match (title, commits) {
        (Some(title), _) => title.to_string(),
        (None, [only]) if more == 0 => return only.summary.clone(),
        _ => branch.to_string(),
    };
    let mut lines: Vec<String> = commits.iter().rev().map(|c| format!("* {}", c.summary)).collect();
    if more > 0 {
        lines.insert(0, "* …".into());
    }
    if lines.is_empty() { first } else { format!("{first}\n\n{}", lines.join("\n")) }
}

/// What the confirmation is about, as the user saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub strategy: MergeStrategy,
    /// The squash's commit message.
    pub message: String,
    pub summary: ReviewSummary,
    /// The folder of the worktree that has the base checked out.
    pub target: String,
    pub task: Option<LinkedTask>,
}

impl Draft {
    pub fn branch(&self) -> &str {
        self.summary.branch.as_deref().unwrap_or_default()
    }

    pub fn base(&self) -> &str {
        &self.summary.base
    }

    /// The confirmation can be clicked (a squash needs a message; a branch with merge commits is
    /// never rebased).
    pub fn ready(&self) -> bool {
        match self.strategy {
            MergeStrategy::Squash => !self.message.trim().is_empty(),
            MergeStrategy::Rebase => self.summary.merges.is_empty(),
            _ => true,
        }
    }

    /// The command, with what the user reviewed.
    pub fn request(&self, id: u64, key: &str) -> AgentCmd {
        AgentCmd::Merge {
            id,
            key: key.to_string(),
            worktree: self.summary.worktree.clone(),
            branch: self.branch().to_string(),
            base: self.base().to_string(),
            strategy: self.strategy,
            message: (self.strategy == MergeStrategy::Squash).then(|| self.message.clone()),
            branch_head: self.summary.head.clone(),
            base_head: self.summary.base_sha.clone(),
        }
    }
}

/// How a line is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Dim,
    Warn,
    Error,
    Done,
}

/// The confirmation's opening lines: where, and what the branch holds.
pub fn intro(d: &Draft) -> Vec<String> {
    let s = &d.summary;
    vec![(t().merge_where)(&d.target, d.base()), (t().merge_counts)(s.ahead, s.files.count(), s.files.added, s.files.removed)]
}

/// The confirmation's closing lines for the strategy chosen: what to know, then what always holds.
pub fn notes(d: &Draft) -> Vec<(String, Tone)> {
    let s = &d.summary;
    let mut lines = Vec::new();
    if s.behind > 0 {
        lines.push(((t().merge_base_moved)(d.base(), s.behind), Tone::Warn));
    }
    if s.several_merge_bases {
        lines.push(((t().merge_crossed)(d.base()), Tone::Warn));
    }
    if s.dirty() {
        lines.push((t().merge_branch_dirty_note.to_string(), Tone::Dim));
    }
    if d.strategy == MergeStrategy::Rebase && !s.merges.is_empty() {
        lines.push((t().merge_rebase_merges.to_string(), Tone::Warn));
    }
    if matches!(d.strategy, MergeStrategy::Squash | MergeStrategy::Rebase) {
        lines.push((t().merge_branch_kept.to_string(), Tone::Dim));
    }
    lines.push(((t().merge_hooks_note)(d.base()), Tone::Dim));
    for note in [t().merge_conflict_note, t().merge_no_push] {
        lines.push((note.to_string(), Tone::Dim));
    }
    lines
}

/// A merge of a review, from its confirmation to its outcome.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum State {
    #[default]
    Idle,
    Confirm(Box<Draft>),
    /// Sent with `id`; `still`: the agent said git was still working after that many seconds.
    Running {
        id: u64,
        draft: Box<Draft>,
        still: Option<u64>,
    },
    Done {
        draft: Box<Draft>,
        outcome: MergeOutcome,
    },
    /// The agent went away before the outcome.
    Lost,
}

impl State {
    pub fn running(&self) -> bool {
        matches!(self, State::Running { .. })
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// What a merge became, as lines.
pub fn outcome_lines(d: &Draft, outcome: &MergeOutcome) -> Vec<(String, Tone)> {
    let mut lines = Vec::new();
    match outcome {
        MergeOutcome::Merged { base_head, fast_forward, .. } => {
            let text = if *fast_forward { t().merge_done_ff } else { t().merge_done };
            lines.push((text(d.branch(), d.base(), short(base_head)), Tone::Done));
        }
        MergeOutcome::Refused { reason } => lines.push(((t().merge_refused)(&t().merge_refusal(reason)), Tone::Warn)),
        MergeOutcome::Conflicted { files, more } => {
            lines.push((t().merge_conflict.to_string(), Tone::Warn));
            lines.extend(files.iter().map(|f| (format!("  • {f}"), Tone::Plain)));
            if *more > 0 {
                lines.push(((t().merge_more_files)(*more), Tone::Dim));
            }
            lines.push(((t().merge_conflict_hint)(d.base()), Tone::Dim));
        }
        MergeOutcome::Failed { problem } => {
            lines.push(((t().merge_failed)(&t().problem(problem)), Tone::Warn));
            lines.push((t().merge_untouched.to_string(), Tone::Dim));
        }
        MergeOutcome::NotApplied { why } => lines.push((t().merge_not_applied_why(d.base(), why), Tone::Warn)),
        MergeOutcome::StillRunning { secs } => lines.push(((t().merge_still_running)(*secs), Tone::Dim)),
        MergeOutcome::PartlyWritten { base, base_head, result, detail } => {
            lines.push(((t().merge_partly)(base, short(base_head), short(result), detail), Tone::Error));
            // Full ids: the commands are copied as they are.
            lines.push(((t().merge_partly_finish)(base, result, base_head), Tone::Plain));
            lines.push(((t().merge_partly_back)(result, base_head), Tone::Plain));
        }
        MergeOutcome::Unverified { base_branch, base_tip, moved } => {
            let now = base_branch.as_deref().unwrap_or(t().detached_head);
            lines.push(((t().merge_unverified)(d.base(), now, base_tip.as_deref().map_or("?", short)), Tone::Error));
            if let Some(moved) = moved {
                lines.push(((t().merge_unverified_moved)(moved, &d.summary.base_sha), Tone::Plain));
            }
        }
    }
    lines
}

/// What the agent said at its start about a merge it did not finish, as lines: still running,
/// applied (the result is in the base), cut while applying (its state now), or cut while
/// preparing (nothing applied).
pub fn interrupted_lines(record: &MergeRecord, previous_alive: bool, applied: bool, base_tip: Option<&str>, base_checkout: Option<&str>, base_dirty: Option<bool>) -> Vec<String> {
    let text = if previous_alive {
        (t().merge_interrupted_alive)(&record.branch, &record.base, record.pid)
    } else if applied {
        (t().merge_interrupted_applied)(&record.branch, &record.base, base_tip.map_or("?", short))
    } else if record.phase == MergePhase::Applying {
        let changes = match base_dirty {
            Some(true) => t().merge_with_changes,
            Some(false) => t().merge_without_changes,
            None => "?",
        };
        let checkout = format!("{} ({changes})", base_checkout.unwrap_or(t().detached_head));
        (t().merge_interrupted_applying)(&record.branch, &record.base, base_tip.map_or("?", short), &checkout)
    } else {
        (t().merge_interrupted_not_applied)(&record.branch, &record.base)
    };
    vec![text]
}

/// The notice of an interrupted merge in the panel, until closed.
pub(super) fn interrupted_ui(ui: &mut Ui, lines: &[String]) -> bool {
    ui.label(RichText::new(t().merge_interrupted_title).strong().color(MODIFIED));
    for text in lines {
        line(ui, text, Tone::Warn);
    }
    ui.small_button(t().merge_dismiss).clicked()
}

fn color(tone: Tone) -> Color32 {
    match tone {
        Tone::Plain => theme::TEXT,
        Tone::Dim => theme::TEXT_DIM,
        Tone::Warn => MODIFIED,
        Tone::Error => DEL,
        Tone::Done => ADD,
    }
}

fn line(ui: &mut Ui, text: &str, tone: Tone) {
    ui.add(egui::Label::new(RichText::new(text).color(color(tone))).wrap());
}

/// The merge's part of the review: the outcome and its follow-ups, or the running notice.
/// `list`: the group's worktree list; `task`: the linked task as it is now.
pub(super) fn state_ui(ui: &mut Ui, state: &State, project: &str, list: Option<&WorktreeList>, task: Option<&LinkedTask>, acts: &mut Vec<Act>) {
    match state {
        State::Idle | State::Confirm(_) => {}
        State::Running { still, .. } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(t().merge_running).color(theme::TEXT_DIM));
            });
            if let Some(secs) = still {
                line(ui, &(t().merge_still_running)(*secs), Tone::Dim);
            }
        }
        State::Lost => {
            line(ui, t().merge_lost, Tone::Warn);
            if ui.small_button(t().merge_dismiss).clicked() {
                acts.push(Act::MergeDismiss);
            }
        }
        State::Done { draft, outcome } => {
            ui.separator();
            for (text, tone) in outcome_lines(draft, outcome) {
                line(ui, &text, tone);
            }
            follow_ups(ui, draft, outcome, project, list, task, acts);
            if ui.small_button(t().merge_dismiss).clicked() {
                acts.push(Act::MergeDismiss);
            }
        }
    }
}

/// What to say about the branch after a squash or a rebase (its own commits stay, which git does
/// not count as merged), from its head now (`now`): `git branch -D` discards only what the result
/// holds while the branch did not move since; otherwise the commits made since too.
pub fn branch_cleanup(d: &Draft, now: Option<&str>) -> (String, Tone) {
    let worktree = &d.summary.worktree;
    if now == Some(d.summary.head.as_str()) { ((t().merge_branch_cleanup)(d.branch(), worktree), Tone::Dim) } else { ((t().merge_branch_cleanup_moved)(d.branch(), worktree), Tone::Warn) }
}

fn follow_ups(ui: &mut Ui, d: &Draft, outcome: &MergeOutcome, project: &str, list: Option<&WorktreeList>, task: Option<&LinkedTask>, acts: &mut Vec<Act>) {
    let worktree = &d.summary.worktree;
    match outcome {
        MergeOutcome::Merged { .. } => {
            // The branch's own worktree, if it is a linked one still listed.
            let linked = list.and_then(|l| l.worktrees.iter().find(|w| same_path(&w.path, worktree))).filter(|w| !w.main);
            match (linked, d.strategy) {
                (Some(w), MergeStrategy::Squash | MergeStrategy::Rebase) => {
                    let (text, tone) = branch_cleanup(d, w.head.as_deref());
                    line(ui, &text, tone);
                }
                (Some(w), _) => {
                    ui.horizontal_wrapped(|ui| {
                        let remove = |delete_branch| Act::Event(Box::new(Event::RemoveWorktree { project: project.to_string(), path: w.path.clone(), delete_branch }));
                        if ui.button(t().merge_remove_worktree).on_hover_text(t().merge_remove_hint).clicked() {
                            acts.push(remove(false));
                        }
                        if ui.button((t().merge_remove_with_branch)(d.branch())).on_hover_text(t().merge_remove_hint).clicked() {
                            acts.push(remove(true));
                        }
                    });
                }
                (None, _) => {}
            }
            match task {
                Some(task) if task.status == TaskStatus::Done => line(ui, &(t().merge_task_is_done)(&task.reference), Tone::Done),
                Some(task) if !task.status.finished() && ui.button((t().merge_task_done)(&task.reference)).on_hover_text(&task.title).clicked() => {
                    acts.push(Act::Event(Box::new(Event::SetTaskStatus { project: project.to_string(), task: task.id.clone(), status: TaskStatus::Done, from: task.status })));
                }
                _ => {}
            }
        }
        MergeOutcome::Conflicted { .. } if ui.button(t().merge_open_card).clicked() => {
            acts.push(Act::Event(Box::new(Event::NewCard { at: None, project: Some(project.to_string()), command: None, setup: None, cwd: Some(worktree.clone()) })));
        }
        _ => {}
    }
}

/// The merge button and why it cannot be clicked, in the review header.
pub(super) fn button_ui(ui: &mut Ui, block: Option<&Block>, acts: &mut Vec<Act>) {
    let hint = block.map_or_else(|| t().merge_button_hint.to_string(), Block::text);
    if ui.add_enabled(block.is_none(), Button::new(t().merge_button)).on_hover_text(t().merge_button_hint).on_disabled_hover_text(hint).clicked() {
        acts.push(Act::MergeAsk);
    }
}

/// Why the button is disabled, under the header, with the way out when there is one.
pub(super) fn block_ui(ui: &mut Ui, block: Option<&Block>, acts: &mut Vec<Act>) {
    let Some(block) = block.filter(|b| b.shown()) else { return };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(block.text()).small().color(theme::TEXT_DIM));
        if let Block::RemoteBase { local: Some(local), .. } = block
            && ui.small_button((t().merge_use_local)(local)).clicked()
        {
            acts.push(Act::Base(Some(local.clone())));
        }
    });
}

/// The confirmation. `Some(true)`: confirmed, `Some(false)`: cancelled, `None`: still open.
pub fn dialog(ctx: &egui::Context, draft: &mut Draft) -> Option<bool> {
    let message_id = egui::Id::unique(MESSAGE_ID);
    let typing = ctx.memory(|m| m.has_focus(message_id));
    let title = (t().merge_title)(draft.branch(), draft.base());
    let ready = draft.ready();
    widgets::confirm_by_click_with(ctx, "merge", &title, t().merge_ok, ready, typing, |ui| {
        for text in intro(draft) {
            line(ui, &text, Tone::Plain);
        }
        ui.add_space(6.0);
        ui.label(RichText::new(t().merge_how).strong().color(theme::TEXT));
        let base = draft.base().to_string();
        for strategy in MergeStrategy::ALL {
            let (name, hint) = t().merge_strategy(strategy, &base);
            ui.radio_value(&mut draft.strategy, strategy, name);
            ui.indent(("merge-strategy", strategy as u8), |ui| line(ui, &hint, Tone::Dim));
        }
        if draft.strategy == MergeStrategy::Squash {
            ui.add_space(6.0);
            ui.label(RichText::new(t().merge_message_label).color(theme::TEXT));
            ui.add(TextEdit::multiline(&mut draft.message).id(message_id).desired_rows(4).desired_width(f32::INFINITY));
        }
        ui.add_space(6.0);
        for (text, tone) in notes(draft) {
            line(ui, &text, tone);
        }
    })
}

/// The confirmation of a group's merge, opened with what the review shows now.
pub fn draft(summary: &ReviewSummary, list: &WorktreeList, strategy: Option<MergeStrategy>, task: Option<LinkedTask>) -> Option<Draft> {
    let target = target(list, &summary.base)?.path.clone();
    let branch = summary.branch.as_deref().unwrap_or_default();
    let message = default_message(task.as_ref().map(|t| t.title.as_str()), branch, &summary.commits, summary.more_commits);
    Some(Draft { strategy: strategy.unwrap_or_default(), message, summary: summary.clone(), target, task })
}

/// The group's id with the strategy to remember, when it changed.
pub fn remember(project: &ProjectId, world: &World, strategy: MergeStrategy) -> Option<Event> {
    let p = world.project(project)?;
    (p.merge_strategy != Some(strategy)).then(|| Event::SetProjectMergeStrategy(project.clone(), strategy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::i18n::{self, Lang};
    use protocol::{FileChange, MergePhase, NotApplied, Problem, ReviewFiles};

    fn commit(c: char, summary: &str) -> CommitInfo {
        CommitInfo { sha: c.to_string().repeat(40), summary: summary.into(), author: "me".into(), time: 1 }
    }

    pub fn summary() -> ReviewSummary {
        let file = |path: &str| FileChange { path: path.into(), kind: protocol::ChangeKind::Modified, added: Some(3), removed: Some(1), mtime: 0, binary: false };
        ReviewSummary {
            worktree: "/home/me/shop-invoices".into(),
            branch: Some("feat/invoices".into()),
            head: "h".repeat(40),
            base: "main".into(),
            base_sha: "b".repeat(40),
            merge_base: Some("m".repeat(40)),
            state: ReviewState::Ahead,
            ahead: 2,
            behind: 0,
            commits: vec![commit('2', "feat: totals"), commit('1', "feat: invoice PDF")],
            more_commits: 0,
            files: ReviewFiles { files: vec![file("src/pdf.rs"), file("src/totals.rs")], more: 1, added: 40, removed: 2 },
            uncommitted: ReviewFiles::default(),
            base_kind: BaseKind::Branch,
            several_merge_bases: false,
            merges: vec![],
        }
    }

    fn wt(path: &str, branch: &str, main: bool) -> Worktree {
        Worktree {
            path: path.into(),
            head: Some("b".repeat(40)),
            branch: Some(branch.into()),
            detached: false,
            main,
            bare: false,
            locked: false,
            lock_reason: None,
            prunable: false,
            dirty: Some(false),
            ahead: Some(0),
            behind: Some(0),
            merged: None,
            last_commit: None,
            branch_deletable: false,
            reflog_commits: false,
            status_mark: None,
        }
    }

    pub fn list() -> WorktreeList {
        WorktreeList {
            key: "p".into(),
            repo: Some("/home/me/shop".into()),
            default_branch: Some("main".into()),
            worktrees: vec![wt("/home/me/shop", "main", true), wt("/home/me/shop-invoices", "feat/invoices", false)],
            merged_branches: vec![],
        }
    }

    fn facts<'a>(s: Option<&'a ReviewSummary>, l: Option<&'a WorktreeList>, bases: &'a [String], agent: &'a dyn Fn(&str) -> bool) -> Facts<'a> {
        Facts { summary: s, bases, list: l, unsupported: false, running: false, agent_in: agent }
    }

    #[test]
    fn what_allows_a_merge_and_why_not() {
        let none = |_: &str| false;
        let (s, l) = (summary(), list());
        let bases = vec!["main".to_string(), "develop".to_string()];
        assert_eq!(block(&facts(Some(&s), Some(&l), &bases, &none)), None);
        // Behind its base, uncommitted changes in either worktree: not reasons.
        let mut other = ReviewSummary { behind: 3, ..s.clone() };
        other.uncommitted.more = 1;
        let mut dirty_base = l.clone();
        dirty_base.worktrees[0].dirty = Some(true);
        assert_eq!(block(&facts(Some(&other), Some(&dirty_base), &bases, &none)), None);
        assert_eq!(block(&Facts { unsupported: true, ..facts(Some(&s), Some(&l), &bases, &none) }), Some(Block::Unsupported));
        assert_eq!(block(&Facts { running: true, ..facts(Some(&s), Some(&l), &bases, &none) }), Some(Block::Running));
        assert_eq!(block(&facts(None, Some(&l), &bases, &none)), Some(Block::Loading));
        assert_eq!(block(&facts(Some(&s), None, &bases, &none)), Some(Block::Loading));
        assert_eq!(block(&facts(Some(&ReviewSummary { branch: None, ..s.clone() }), Some(&l), &bases, &none)), Some(Block::Detached));
        let remote = ReviewSummary { base: "origin/main".into(), base_kind: BaseKind::Remote, ..s.clone() };
        assert_eq!(block(&facts(Some(&remote), Some(&l), &bases, &none)), Some(Block::RemoteBase { base: "origin/main".into(), local: Some("main".into()) }));
        assert_eq!(block(&facts(Some(&remote), Some(&l), &[], &none)), Some(Block::RemoteBase { base: "origin/main".into(), local: None }));
        let tag = ReviewSummary { base: "v1.0".into(), base_kind: BaseKind::Other, ..s.clone() };
        assert_eq!(block(&facts(Some(&tag), Some(&l), &bases, &none)), Some(Block::OtherBase));
        let develop = ReviewSummary { base: "develop".into(), ..s.clone() };
        assert_eq!(block(&facts(Some(&develop), Some(&l), &bases, &none)), Some(Block::NotCheckedOut { base: "develop".into() }));
        assert_eq!(block(&facts(Some(&ReviewSummary { state: ReviewState::UpToDate, ..s.clone() }), Some(&l), &bases, &none)), Some(Block::NothingToMerge));
        assert_eq!(block(&facts(Some(&ReviewSummary { state: ReviewState::Unrelated, ..s.clone() }), Some(&l), &bases, &none)), Some(Block::Unrelated));
        let in_main = |p: &str| p == "/home/me/shop";
        assert_eq!(block(&facts(Some(&s), Some(&l), &bases, &in_main)), Some(Block::AgentWorking { path: "/home/me/shop".into() }));
        let in_branch = |p: &str| p == "/home/me/shop-invoices";
        assert_eq!(block(&facts(Some(&s), Some(&l), &bases, &in_branch)), None, "the branch's worktree is never written");
    }

    #[test]
    fn reasons_are_said_in_both_languages() {
        i18n::set(Lang::Fr);
        assert_eq!(
            Block::NotCheckedOut { base: "develop".into() }.text(),
            "develop n'est extraite dans aucun worktree : extrayez-la quelque part (d'habitude le dépôt principal) pour fusionner dedans."
        );
        i18n::set(Lang::En);
        assert_eq!(Block::AgentWorking { path: "/r".into() }.text(), "A coding agent is working in /r: wait until it is done.");
        assert!(!Block::NothingToMerge.shown() && Block::OtherBase.shown());
    }

    #[test]
    fn the_squash_message_comes_from_the_task_else_the_commits() {
        let commits = [commit('2', "feat: totals"), commit('1', "feat: invoice PDF")];
        assert_eq!(default_message(Some(" Invoices as PDF "), "feat/invoices", &commits, 0), "Invoices as PDF\n\n* feat: invoice PDF\n* feat: totals");
        assert_eq!(default_message(None, "feat/invoices", &commits, 0), "feat/invoices\n\n* feat: invoice PDF\n* feat: totals");
        assert_eq!(default_message(None, "feat/invoices", &commits[..1], 0), "feat: totals");
        assert_eq!(default_message(None, "feat/invoices", &commits[..1], 4), "feat/invoices\n\n* …\n* feat: totals");
        assert_eq!(default_message(Some(""), "b", &[], 0), "b");
    }

    #[test]
    fn the_strategy_is_remembered_per_group_and_merge_commit_by_default() {
        let (s, l) = (summary(), list());
        let d = draft(&s, &l, None, None).unwrap();
        assert_eq!((d.strategy, d.target.as_str()), (MergeStrategy::Merge, "/home/me/shop"));
        assert_eq!(draft(&s, &l, Some(MergeStrategy::Rebase), None).unwrap().strategy, MergeStrategy::Rebase);
        assert!(draft(&ReviewSummary { base: "develop".into(), ..s.clone() }, &l, None, None).is_none(), "checked out nowhere");
        let mut world = World::default();
        world.projects.push(crate::shell::world::Project {
            id: "p".into(),
            name: "shop".into(),
            color: [0; 3],
            rect: egui::Rect::ZERO,
            cwd: Some("/home/me/shop".into()),
            templates: vec![],
            messages: vec![],
            worktree_location: None,
            task_settings: Default::default(),
            merge_strategy: Some(MergeStrategy::Squash),
        });
        assert!(remember(&"p".to_string(), &world, MergeStrategy::Squash).is_none());
        assert!(matches!(remember(&"p".to_string(), &world, MergeStrategy::Merge), Some(Event::SetProjectMergeStrategy(p, MergeStrategy::Merge)) if p == "p"));
    }

    #[test]
    fn the_request_carries_what_was_reviewed() {
        let mut d = draft(&summary(), &list(), Some(MergeStrategy::Squash), None).unwrap();
        d.message = "Invoices".into();
        let AgentCmd::Merge { worktree, branch, base, strategy, message, branch_head, base_head, .. } = d.request(4, "p") else { panic!("a merge") };
        assert_eq!((worktree.as_str(), branch.as_str(), base.as_str(), strategy), ("/home/me/shop-invoices", "feat/invoices", "main", MergeStrategy::Squash));
        assert_eq!((message.as_deref(), branch_head, base_head), (Some("Invoices"), "h".repeat(40), "b".repeat(40)));
        d.message = " \n".into();
        assert!(!d.ready());
        d.strategy = MergeStrategy::Merge;
        assert!(d.ready());
        assert!(matches!(d.request(5, "p"), AgentCmd::Merge { message: None, .. }));
        let mut s = summary();
        s.merges = vec!["1".repeat(40)];
        let rebase = draft(&s, &list(), Some(MergeStrategy::Rebase), None).unwrap();
        assert!(!rebase.ready(), "a rebase would drop the merge commits");
    }

    #[test]
    fn the_confirmation_says_what_happens_per_strategy() {
        i18n::set(Lang::En);
        let mut s = summary();
        s.behind = 2;
        s.several_merge_bases = true;
        s.uncommitted.more = 1;
        let mut d = draft(&s, &list(), Some(MergeStrategy::Rebase), None).unwrap();
        assert_eq!(
            intro(&d),
            vec![
                "The result is prepared aside, in a private copy of the agent; only at the end is main moved to it, in /home/me/shop, by a fast-forward that does all or nothing.".to_string(),
                "2 commits, 3 files changed (+40 −2).".to_string()
            ]
        );
        let texts = |d: &Draft| notes(d).into_iter().map(|(t, _)| t).collect::<Vec<_>>().join("\n");
        let rebase = texts(&d);
        for part in [
            "main moved ahead by 2 commits",
            "histories cross",
            "uncommitted changes: they are not part of the merge",
            "keeps its own commits",
            "post-checkout",
            "post-merge in main's worktree",
            "Nothing is pushed.",
        ] {
            assert!(rebase.contains(part), "{part}: {rebase}");
        }
        d.strategy = MergeStrategy::Merge;
        assert!(!texts(&d).contains("keeps its own commits"));
    }

    #[test]
    fn outcomes_say_what_was_and_was_not_touched() {
        i18n::set(Lang::Fr);
        let d = draft(&summary(), &list(), None, None).unwrap();
        let text = |o: &MergeOutcome| outcome_lines(&d, o).into_iter().map(|(t, _)| t).collect::<Vec<_>>().join("\n");
        assert_eq!(text(&MergeOutcome::Merged { base_head: "abcdef123".into(), branch_head: "h".into(), fast_forward: false }), "✔ feat/invoices fusionnée dans main (maintenant à abcdef1).");
        let conflict = text(&MergeOutcome::Conflicted { files: vec!["src/pdf.rs".into()], more: 2 });
        assert!(
            conflict.starts_with("⚠ Conflit pendant la préparation du résultat à part : rien n'a été appliqué") && conflict.contains("  • src/pdf.rs\n… et 2 de plus\nLà, fusionnez main"),
            "{conflict}"
        );
        let failed = text(&MergeOutcome::Failed { problem: Problem::MergeStepFailed { step: protocol::MergeStep::Commit, detail: "commit-msg: no".into() } });
        assert_eq!(failed, "Pas fusionné : Le commit a échoué : commit-msg: no\nRien n'a été appliqué : vos worktrees n'ont pas été touchés.");
        let collisions = text(&MergeOutcome::NotApplied { why: NotApplied::Collisions { files: vec![".env".into()], more: 1 } });
        assert!(
            collisions.starts_with("Préparé, pas appliqué : des fichiers non suivis ou ignorés sont là où le résultat écrit (.env, … et 1 de plus)")
                && collisions.ends_with("Le worktree de main est inchangé."),
            "{collisions}"
        );
        let moved = text(&MergeOutcome::NotApplied { why: NotApplied::BaseMoved { found: Some("hotfix".into()), head: Some("c".repeat(40)) } });
        assert!(moved.contains("(maintenant : hotfix ccccccc)"), "{moved}");
        let unverified = text(&MergeOutcome::Unverified { base_branch: Some("hotfix".into()), base_tip: Some("b".repeat(40)), moved: Some("hotfix".into()) });
        assert!(unverified.contains("est maintenant sur hotfix, et main est à bbbbbbb"), "{unverified}");
        assert!(
            unverified
                .contains(&format!("L'avance rapide a déplacé hotfix à la place (du commit de la base jusqu'au résultat). Pour le remettre : git update-ref refs/heads/hotfix {}", "b".repeat(40))),
            "{unverified}"
        );
        let partly = text(&MergeOutcome::PartlyWritten { base: "main".into(), base_head: "b".repeat(40), result: "r".repeat(40), detail: "cannot lock ref".into() });
        assert!(
            partly.starts_with(
                "⚠ L'avance rapide a échoué à mi-chemin (cannot lock ref) : dans le worktree de main, l'index et les fichiers sont au résultat rrrrrrr, mais la branche main est encore à bbbbbbb."
            ),
            "{partly}"
        );
        assert!(partly.contains(&format!("Pour terminer : git update-ref refs/heads/main {} {}", "r".repeat(40), "b".repeat(40))), "{partly}");
        assert!(partly.contains(&format!("Pour revenir en arrière : git read-tree -m -u {} {}", "r".repeat(40), "b".repeat(40))), "{partly}");
        let hook = text(&MergeOutcome::Failed { problem: Problem::MergeHookChanged });
        assert!(hook.contains("un hook a modifié le contenu") && hook.contains("Rien n'a été appliqué"), "{hook}");
        let locked = text(&MergeOutcome::NotApplied { why: NotApplied::Locked { path: "/r/.git/index.lock".into() } });
        assert!(locked.contains("(/r/.git/index.lock)") && locked.ends_with("Le worktree de main est inchangé."), "{locked}");
        let timed = text(&MergeOutcome::Failed { problem: Problem::MergeTimedOut { secs: 900 } });
        assert!(timed.contains("plus de 900 s"), "{timed}");
    }

    #[test]
    fn interrupted_merges_and_branch_deletion_are_said_honestly() {
        i18n::set(Lang::En);
        let record = MergeRecord {
            id: 1,
            key: "p".into(),
            strategy: MergeStrategy::Squash,
            branch: "feat/invoices".into(),
            base: "main".into(),
            base_path: "/home/me/shop".into(),
            branch_path: "/home/me/shop-invoices".into(),
            base_head: "b".repeat(40),
            branch_head: "h".repeat(40),
            temp: None,
            phase: MergePhase::Applying,
            result: Some("r".repeat(40)),
            pid: 42,
            pid_start: 0,
            git_pid: Some(43),
            git_start: 0,
            started: 0,
        };
        assert!(interrupted_lines(&record, true, false, None, None, None)[0].contains("(process 42) is still merging"));
        assert!(interrupted_lines(&record, false, true, Some(&"r".repeat(40)), Some("main"), Some(false))[0].contains("after moving main to the result (rrrrrrr): the merge is done"));
        // Cut while applying: never "nothing was applied", the state now instead.
        let applying = interrupted_lines(&record, false, false, Some(&"b".repeat(40)), Some("main"), Some(true));
        assert_eq!(
            applying[0],
            "The agent stopped while it moved main to the result of feat/invoices: the fast-forward may have been under way. Now main is at bbbbbbb, and the base's worktree is on main (it has uncommitted changes). Look at it before anything else."
        );
        let preparing = MergeRecord { phase: MergePhase::Preparing, result: None, ..record };
        assert!(interrupted_lines(&preparing, false, false, None, None, None)[0].contains("before moving main: nothing was applied"));
        let d = draft(&summary(), &list(), Some(MergeStrategy::Squash), None).unwrap();
        let (same, tone) = branch_cleanup(&d, Some(&"h".repeat(40)));
        assert!(same.contains("Every one of them went into the result") && tone == Tone::Dim, "{same}");
        let (moved, tone) = branch_cleanup(&d, Some("feedface"));
        assert!(moved.contains("commits made after this merge") && moved.contains("would discard them") && tone == Tone::Warn, "{moved}");
    }

    #[test]
    fn the_linked_task_is_found_by_worktree_then_branch() {
        let task = |id: &str, worktree: Option<&str>, branch: Option<&str>| -> Task {
            let mut t: Task = serde_json::from_value(serde_json::json!({"id": id, "reference": id, "title": format!("T {id}"), "status": "review", "created": 0, "updated": 0})).unwrap();
            t.worktree = worktree.map(String::from);
            t.branch = branch.map(String::from);
            t
        };
        let tasks = vec![task("a", None, Some("feat/invoices")), task("b", Some("/home/me/shop-invoices/"), None)];
        assert_eq!(linked_task(&tasks, "/home/me/shop-invoices", Some("feat/invoices")).map(|t| t.id), Some("b".into()));
        assert_eq!(linked_task(&tasks[..1], "/home/me/shop-invoices", Some("feat/invoices")).map(|t| (t.id, t.status)), Some(("a".into(), TaskStatus::Review)));
        assert_eq!(linked_task(&tasks[..1], "/home/me/shop-invoices", None), None);
        let mut gone = task("c", Some("/home/me/shop-invoices"), None);
        gone.removed = Some(1);
        assert_eq!(linked_task(&[gone], "/home/me/shop-invoices", None), None);
    }
}
