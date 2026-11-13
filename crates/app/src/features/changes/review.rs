//! Review mode of the Changes panel: a worktree's whole branch against where it started, like a
//! merge request (the agent computes it: `AgentCmd::Review`, `ReviewFiles`, `ReviewDiff`).
//!
//! Which worktree: each group has at most one review, of a worktree of its repository named when
//! it started (`Event::ReviewWorktree`, sent by the worktrees strip and by a task in review, or the
//! mode switch on the group's own folder). It may be a linked worktree while the group's folder is
//! the main checkout: the header names the worktree, and the mode switch ("Working changes")
//! brings the group's folder back, the review kept for the next switch. The mode is kept per group
//! for the session only, never in the layout: after a restart the panel shows the working changes
//! (a worktree reviewed yesterday may be merged or gone, its base moved).
//!
//! Freshness: the summary is asked again when the worktree list (sent by the agent whenever it
//! changes) shows that the reviewed worktree's head or dirty state, its distance to the default
//! branch, or the head of the base's branch moved; every request has an id and only the answer to
//! the last one counts.

use egui::{Button, CollapsingHeader, RichText, ScrollArea, Ui};
use protocol::{AgentCmd, AgentEvent, BaseKind, ChangeKind, ChangeSet, CommitInfo, FileChange, Problem, ReviewFiles, ReviewScope, ReviewState, ReviewSummary, WorktreeList};

use super::{ADD, Changes, DEL, LineKind, classify, diff_body};
use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::{ProjectId, unix_now};

/// What the file list and the diffs are narrowed to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Selection {
    /// The whole branch.
    #[default]
    All,
    /// One commit of the branch.
    Commit(String),
    /// What is not committed in the worktree (not part of the branch).
    Uncommitted,
}

/// A diff to show: its scope and file (none: the whole commit).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffAsk {
    pub scope: ReviewScope,
    pub path: Option<String>,
    pub old_path: Option<String>,
}

pub struct ReviewDiffView {
    /// The request in flight or answered (0: none: lost with the agent, to ask again).
    pub id: u64,
    pub ask: DiffAsk,
    /// `None` until the agent has answered. While the same file's diff is asked again (a refresh),
    /// the previous lines stay shown: the reader keeps their place.
    pub lines: Option<Vec<(LineKind, String)>>,
    pub truncated: bool,
    pub problem: Option<Problem>,
    /// The agent answered `id`.
    pub answered: bool,
}

/// The files of the selected commit.
pub struct CommitFiles {
    /// The request in flight or answered (0: lost with the agent, to ask again).
    pub id: u64,
    pub sha: String,
    /// `None` until the agent has answered.
    pub files: Option<ReviewFiles>,
    pub problem: Option<Problem>,
}

/// What, in a worktree list, tells that a review's branch may be stale (what is not committed
/// has its own mark, `Worktree::status_mark`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heads {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    /// Head of the worktree that has the base's branch checked out, if any.
    pub base_head: Option<String>,
}

/// Same folder, whatever the trailing slash.
pub fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// The reviewed worktree's state in `list` (`None`: not listed).
pub fn heads(list: &WorktreeList, worktree: &str, base: Option<&str>) -> Option<Heads> {
    let wt = list.worktrees.iter().find(|w| same_path(&w.path, worktree))?;
    let base = base.or(list.default_branch.as_deref()).map(|b| b.strip_prefix("origin/").unwrap_or(b));
    let base_head = base.and_then(|b| list.worktrees.iter().find(|w| w.branch.as_deref() == Some(b))).and_then(|w| w.head.clone());
    Some(Heads { head: wt.head.clone(), branch: wt.branch.clone(), ahead: wt.ahead, behind: wt.behind, base_head })
}

/// The reviewed worktree's status mark in `list` (`None`: not listed or not known).
pub fn status_mark(list: &WorktreeList, worktree: &str) -> Option<u64> {
    list.worktrees.iter().find(|w| same_path(&w.path, worktree)).and_then(|w| w.status_mark)
}

/// The local name of a base (`origin/main` -> `main`).
pub fn local_base(base: &str) -> &str {
    base.strip_prefix("origin/").unwrap_or(base)
}

/// The same file's diff in the same kind of scope: a refresh of what is shown.
fn same_file(a: &DiffAsk, b: &DiffAsk) -> bool {
    a.path == b.path && std::mem::discriminant(&a.scope) == std::mem::discriminant(&b.scope)
}

/// The worktree the mode switch reviews for a group: the one holding its folder, when it is on
/// another branch than the default one (or detached). `None` outside git or on the default branch.
pub fn folder_target(set: &ChangeSet, default_branch: Option<&str>) -> Option<String> {
    let root = set.root.clone().filter(|_| set.git && set.error.is_none())?;
    let base = default_branch.map(local_base);
    (set.detached || (set.branch.is_some() && set.branch.as_deref() != base)).then_some(root)
}

/// Why the mode switch cannot review the group's folder: the agent cannot, the folder is
/// outside git, what it is on is not known yet, or it is on its base (named locally).
pub fn unavailable(set: Option<&ChangeSet>, list: Option<&WorktreeList>, unsupported: bool) -> String {
    match (set, list) {
        _ if unsupported => t().review_agent_too_old.to_string(),
        (Some(s), _) if !s.git => t().review_needs_git.to_string(),
        (None, _) | (_, None) => t().review_waiting.to_string(),
        (Some(_), Some(l)) => match l.default_branch.as_deref() {
            Some(base) => (t().review_on_base)(local_base(base)),
            None => t().review_needs_git.to_string(),
        },
    }
}

/// Files, lines added and removed.
pub fn totals(files: &ReviewFiles) -> (u64, u64, u64) {
    (files.count(), files.added, files.removed)
}

pub struct Review {
    pub worktree: String,
    /// The base chosen (`None`: the default branch).
    pub base: Option<String>,
    /// The last summary asked (its answer is the only one that counts).
    pub asked: u64,
    pub loading: bool,
    pub summary: Option<ReviewSummary>,
    pub bases: Vec<String>,
    pub problem: Option<Problem>,
    pub selection: Selection,
    pub commit_files: Option<CommitFiles>,
    /// The file picked (`None`: the first one, or the whole commit).
    pub file: Option<String>,
    pub diff: Option<ReviewDiffView>,
    /// The worktree list's state when the summary was last asked.
    pub heads: Option<Heads>,
    /// The status mark when what is not committed was last asked.
    pub mark: Option<u64>,
    /// The last request for what is not committed alone (0: none in flight).
    pub uncommitted_asked: u64,
    /// The worktree was seen in a worktree list: its absence from a later one means it is gone.
    pub seen: bool,
}

impl Review {
    pub fn new(worktree: &str) -> Self {
        Review {
            worktree: worktree.to_string(),
            base: None,
            asked: 0,
            loading: false,
            summary: None,
            bases: Vec::new(),
            problem: None,
            selection: Selection::All,
            commit_files: None,
            file: None,
            diff: None,
            heads: None,
            mark: None,
            uncommitted_asked: 0,
            seen: false,
        }
    }

    /// The agent went: what it was asked and has not answered never will be. Each such request
    /// is forgotten, so that it is asked again (by the next refresh, or a click).
    pub fn lose_requests(&mut self) {
        self.asked = 0;
        self.loading = false;
        self.uncommitted_asked = 0;
        if let Some(d) = self.diff.as_mut().filter(|d| !d.answered) {
            d.id = 0;
        }
        if let Some(c) = self.commit_files.as_mut().filter(|c| c.files.is_none() && c.problem.is_none()) {
            c.id = 0;
        }
    }

    /// What is not committed, asked alone: kept apart from the branch's files.
    pub fn uncommitted_answered(&mut self, files: ReviewFiles) {
        self.uncommitted_asked = 0;
        let Some(s) = self.summary.as_mut() else { return };
        s.uncommitted = files;
        if self.selection == Selection::Uncommitted && !s.dirty() {
            self.selection = Selection::All;
            self.file = None;
        }
    }

    /// The base shown: the one compared with, else the one asked.
    pub fn base_name(&self) -> Option<&str> {
        self.summary.as_ref().map(|s| s.base.as_str()).or(self.base.as_deref())
    }

    pub fn state(&self) -> Option<ReviewState> {
        self.summary.as_ref().map(|s| s.state)
    }

    /// The files the list shows for the selection (`None`: not known yet).
    pub fn shown_files(&self) -> Option<&ReviewFiles> {
        let s = self.summary.as_ref()?;
        match &self.selection {
            Selection::All => Some(&s.files),
            Selection::Uncommitted => Some(&s.uncommitted),
            Selection::Commit(sha) => self.commit_files.as_ref().filter(|c| &c.sha == sha).and_then(|c| c.files.as_ref()),
        }
    }

    /// The file the diff is about: the one picked if listed, else the first one (a commit shows
    /// whole until a file is picked).
    pub fn shown_file(&self) -> Option<&FileChange> {
        let files = &self.shown_files()?.files;
        let picked = self.file.as_deref().and_then(|p| files.iter().find(|f| f.path == p));
        match self.selection {
            Selection::Commit(_) => picked,
            _ => picked.or(files.first()),
        }
    }

    /// The diff the selection calls for.
    pub fn wanted_diff(&self) -> Option<DiffAsk> {
        let s = self.summary.as_ref()?;
        let file = self.shown_file();
        let old_path = file.and_then(|f| match &f.kind {
            ChangeKind::Renamed(from) => Some(from.clone()),
            _ => None,
        });
        let path = file.map(|f| f.path.clone());
        let scope = match &self.selection {
            // A whole commit shows without a file; the branch and the uncommitted changes need one.
            Selection::Commit(sha) => ReviewScope::Commit { sha: sha.clone() },
            _ if path.is_none() => return None,
            Selection::All => ReviewScope::Branch { merge_base: s.merge_base.clone().filter(|_| s.state == ReviewState::Ahead)?, head: s.head.clone() },
            Selection::Uncommitted => ReviewScope::Uncommitted,
        };
        Some(DiffAsk { scope, path, old_path })
    }

    /// A new summary: the selection stays when it still makes sense.
    pub fn answered(&mut self, summary: Option<ReviewSummary>, bases: Vec<String>, problem: Option<Problem>) {
        self.loading = false;
        self.bases = bases;
        self.problem = problem;
        self.summary = summary;
        let Some(s) = &self.summary else {
            self.selection = Selection::All;
            self.diff = None;
            return;
        };
        let keep = match &self.selection {
            Selection::All => true,
            Selection::Commit(sha) => s.commits.iter().any(|c| &c.sha == sha),
            Selection::Uncommitted => s.dirty(),
        };
        if !keep {
            self.selection = Selection::All;
            self.commit_files = None;
            self.file = None;
        }
    }
}

/// What the user did in the review this frame.
pub(super) enum Act {
    Working,
    All,
    Commit(String),
    Uncommitted(bool),
    File(String),
    Base(Option<String>),
}

impl Changes {
    /// The group's review (in review mode or not).
    pub fn review(&self, project: &str) -> Option<&Review> {
        self.reviews.get(project)
    }

    /// The panel shows the group's review.
    pub fn reviewing(&self, project: &str) -> bool {
        self.reviewing.contains(project) && self.reviews.contains_key(project)
    }

    /// What the mode switch would review for the group (its own folder's worktree).
    pub fn folder_review_target(&self, project: &str) -> Option<String> {
        let set = self.sets.get(project)?;
        folder_target(set, self.lists.get(project).and_then(|l| l.default_branch.as_deref()))
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Review mode on `worktree` (the same one again keeps its base and selection).
    pub(super) fn start_review(&mut self, project: &str, worktree: &str, out: &mut Vec<Event>) {
        if self.reviews.get(project).is_none_or(|r| !same_path(&r.worktree, worktree)) {
            self.reviews.insert(project.to_string(), Review::new(worktree));
        }
        self.reviewing.insert(project.to_string());
        self.ask_summary(project, out);
    }

    /// The mode switch: the group's review if there is one, else its own folder's.
    pub(super) fn switch_to_review(&mut self, project: &str, out: &mut Vec<Event>) {
        match (self.reviews.get(project), self.folder_review_target(project)) {
            (Some(r), _) => {
                let worktree = r.worktree.clone();
                self.start_review(project, &worktree, out);
            }
            (None, Some(target)) => self.start_review(project, &target, out),
            (None, None) => {}
        }
    }

    pub(super) fn ask_summary(&mut self, project: &str, out: &mut Vec<Event>) {
        let id = self.next_id();
        let list = self.lists.get(project);
        let Some(r) = self.reviews.get_mut(project) else { return };
        r.asked = id;
        r.loading = true;
        r.heads = list.and_then(|l| heads(l, &r.worktree, r.base_name()));
        r.mark = list.and_then(|l| status_mark(l, &r.worktree));
        r.seen |= r.heads.is_some();
        r.uncommitted_asked = 0;
        out.push(Event::ToAgent(AgentCmd::Review { id, key: project.to_string(), worktree: r.worktree.clone(), base: r.base.clone() }));
    }

    /// Asks the diff the selection calls for, unless it is the one shown (`force`: again anyway).
    /// A request lost with the agent (`id` 0) is asked again.
    fn ask_diff(&mut self, project: &str, force: bool, out: &mut Vec<Event>) {
        let Some(r) = self.reviews.get(project) else { return };
        let wanted = r.wanted_diff();
        if !force && wanted.as_ref().is_some_and(|a| r.diff.as_ref().is_some_and(|d| &d.ask == a && d.id != 0)) {
            return;
        }
        let id = self.next_id();
        let r = self.reviews.get_mut(project).expect("checked above");
        let Some(ask) = wanted else {
            r.diff = None;
            return;
        };
        out.push(Event::ToAgent(AgentCmd::ReviewDiff { id, key: project.to_string(), worktree: r.worktree.clone(), scope: ask.scope.clone(), path: ask.path.clone(), old_path: ask.old_path.clone() }));
        // The same file again (new commits, a refresh): its lines stay until the new ones come.
        let kept = r.diff.take().filter(|d| same_file(&d.ask, &ask) && d.lines.is_some());
        let (lines, truncated, problem) = kept.map_or((None, false, None), |d| (d.lines, d.truncated, d.problem));
        r.diff = Some(ReviewDiffView { id, ask, lines, truncated, problem, answered: false });
    }

    /// What is not committed changed: asked alone (cheap), the branch left as it is.
    fn ask_uncommitted(&mut self, project: &str, out: &mut Vec<Event>) {
        let id = self.next_id();
        let Some(r) = self.reviews.get_mut(project) else { return };
        r.uncommitted_asked = id;
        out.push(Event::ToAgent(AgentCmd::ReviewFiles { id, key: project.to_string(), worktree: r.worktree.clone(), scope: ReviewScope::Uncommitted }));
    }

    /// Asks the selected commit's files when they are not there nor on their way.
    fn ask_commit_files(&mut self, project: &str, out: &mut Vec<Event>) {
        let id = self.next_id();
        let Some(r) = self.reviews.get_mut(project) else { return };
        let Selection::Commit(sha) = r.selection.clone() else { return };
        if r.commit_files.as_ref().is_some_and(|c| c.sha == sha && c.problem.is_none() && c.id != 0) {
            return;
        }
        r.commit_files = Some(CommitFiles { id, sha: sha.clone(), files: None, problem: None });
        out.push(Event::ToAgent(AgentCmd::ReviewFiles { id, key: project.to_string(), worktree: r.worktree.clone(), scope: ReviewScope::Commit { sha } }));
    }

    pub(super) fn act(&mut self, project: &str, act: Act, out: &mut Vec<Event>) {
        match act {
            Act::Working => {
                self.reviewing.remove(project);
                return;
            }
            Act::Base(base) => {
                if let Some(r) = self.reviews.get_mut(project) {
                    r.base = base;
                    r.selection = Selection::All;
                    (r.file, r.commit_files, r.diff) = (None, None, None);
                }
                self.ask_summary(project, out);
                return;
            }
            Act::Commit(sha) => {
                let Some(r) = self.reviews.get_mut(project) else { return };
                r.selection = Selection::Commit(sha);
                r.file = None;
                self.ask_commit_files(project, out);
            }
            Act::All => {
                if let Some(r) = self.reviews.get_mut(project) {
                    r.selection = Selection::All;
                    r.file = None;
                }
            }
            Act::Uncommitted(open) => {
                let Some(r) = self.reviews.get_mut(project) else { return };
                r.selection = if open { Selection::Uncommitted } else { Selection::All };
                r.file = None;
                if open {
                    // Fresh when opened.
                    self.ask_uncommitted(project, out);
                }
            }
            Act::File(path) => {
                if let Some(r) = self.reviews.get_mut(project) {
                    r.file = Some(path);
                }
            }
        }
        self.ask_diff(project, false, out);
    }

    /// The agent's answers about reviews; a stale one (not the last asked) changes nothing.
    pub(super) fn on_review_event(&mut self, ev: &AgentEvent, out: &mut Vec<Event>) {
        match ev {
            AgentEvent::Review { id, key, summary, bases, problem } => {
                let Some(r) = self.reviews.get_mut(key).filter(|r| r.asked == *id) else { return };
                let uncommitted = r.selection == Selection::Uncommitted;
                r.answered(summary.as_deref().cloned(), bases.clone(), problem.clone());
                // An uncommitted file's diff may have changed with the same name: asked again.
                self.ask_diff(key, uncommitted, out);
            }
            AgentEvent::ReviewFiles { id, key, files, problem } => {
                let Some(r) = self.reviews.get_mut(key) else { return };
                if *id != 0 && r.uncommitted_asked == *id {
                    if problem.is_none() {
                        r.uncommitted_answered(files.clone());
                    } else {
                        r.uncommitted_asked = 0;
                    }
                    // The open file's text may have changed under the same name.
                    let open = r.selection == Selection::Uncommitted;
                    self.ask_diff(key, open, out);
                    return;
                }
                let Some(c) = r.commit_files.as_mut().filter(|c| c.id == *id && *id != 0) else { return };
                c.files = problem.is_none().then(|| files.clone());
                c.problem = problem.clone();
                self.ask_diff(key, false, out);
            }
            AgentEvent::ReviewDiff { id, key, text, truncated, problem } => {
                if let Some(d) = self.reviews.get_mut(key).and_then(|r| r.diff.as_mut()).filter(|d| d.id == *id && *id != 0) {
                    d.lines = Some(text.lines().map(|l| (classify(l), l.to_string())).collect());
                    d.truncated = *truncated;
                    d.problem = problem.clone();
                    d.answered = true;
                }
            }
            _ => {}
        }
    }

    /// A new worktree list: kept (default branch, heads). A review whose branch moved is asked
    /// again; one whose uncommitted changes moved has those alone asked again; one whose worktree
    /// went (seen before, missing now) is dropped, the panel falling back on the group's folder.
    pub(super) fn on_worktrees(&mut self, list: &WorktreeList, out: &mut Vec<Event>) {
        let key = list.key.clone();
        self.lists.insert(key.clone(), list.clone());
        let Some(r) = self.reviews.get_mut(&key) else { return };
        let Some(now) = heads(list, &r.worktree, r.base_name()) else {
            if r.seen {
                self.reviews.remove(&key);
                if self.reviewing.remove(&key)
                    && let Some(target) = self.folder_review_target(&key)
                {
                    self.start_review(&key, &target, out);
                }
            }
            return;
        };
        r.seen = true;
        if !self.reviewing(&key) {
            return;
        }
        let r = &self.reviews[&key];
        let mark = status_mark(list, &r.worktree);
        if Some(&now) != r.heads.as_ref() {
            self.ask_summary(&key, out);
        } else if mark != r.mark && r.summary.is_some() {
            if let Some(r) = self.reviews.get_mut(&key) {
                r.mark = mark;
            }
            self.ask_uncommitted(&key, out);
        }
    }

    /// The link to the agent closed (or another agent said hello): unanswered requests are
    /// forgotten, to be asked again.
    pub(super) fn lose_review_requests(&mut self) {
        for r in self.reviews.values_mut() {
            r.lose_requests();
        }
    }

    pub(super) fn forget_review(&mut self, project: &str) {
        self.reviews.remove(project);
        self.reviewing.remove(project);
        self.lists.remove(project);
    }

    /// The agent restarted: every review shown is asked again, once it watches the folders, with
    /// what its selection was waiting for.
    pub(super) fn resend_reviews(&mut self, out: &mut Vec<Event>) {
        let shown: Vec<ProjectId> = self.reviewing.iter().filter(|p| self.reviews.contains_key(*p)).cloned().collect();
        for project in shown {
            self.ask_summary(&project, out);
            self.ask_commit_files(&project, out);
            self.ask_diff(&project, false, out);
        }
    }

    /// The mode switch, under the panel's title.
    pub(super) fn mode_ui(&mut self, ui: &mut Ui, project: &str, events: &mut Vec<Event>) {
        let reviewing = self.reviewing(project);
        let can = !self.review_unsupported && (self.reviews.contains_key(project) || self.folder_review_target(project).is_some());
        let mut clicked = None;
        ui.horizontal(|ui| {
            if ui.selectable_label(!reviewing, t().review_mode_working).on_hover_text(t().review_mode_working_hint).clicked() && reviewing {
                clicked = Some(false);
            }
            let why = unavailable(self.sets.get(project), self.lists.get(project), self.review_unsupported);
            let review = ui.add_enabled(can, Button::selectable(reviewing, t().review_mode_review)).on_hover_text(t().review_mode_review_hint).on_disabled_hover_text(why);
            if review.clicked() && !reviewing {
                clicked = Some(true);
            }
        });
        match clicked {
            Some(true) => self.switch_to_review(project, events),
            Some(false) => self.act(project, Act::Working, events),
            None => {}
        }
    }

    /// The review of the panel's group. The drawing reads; what was clicked is done after.
    pub(super) fn review_ui(&mut self, ui: &mut Ui, project: &str, events: &mut Vec<Event>) {
        let Some(r) = self.reviews.get(project) else { return };
        let own = self.sets.get(project).and_then(|s| s.root.as_deref()).is_some_and(|root| same_path(root, &r.worktree));
        let mut acts = Vec::new();
        review_body(self, ui, r, own, &mut acts);
        for act in acts {
            self.act(project, act, events);
        }
    }
}

fn folder_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(path)
}

fn review_body(changes: &Changes, ui: &mut Ui, r: &Review, own: bool, acts: &mut Vec<Act>) {
    let (wt_line, wt_hover) = if own { (format!("📁 {}", r.worktree), r.worktree.clone()) } else { ((t().review_in_worktree)(folder_name(&r.worktree)), r.worktree.clone()) };
    ui.label(RichText::new(wt_line).color(theme::TEXT_DIM)).on_hover_text(wt_hover);
    header(ui, r, acts);
    if let Some(p) = &r.problem {
        ui.label(RichText::new(format!("⚠ {}", t().problem(p))).color(DEL));
    }
    let Some(s) = &r.summary else {
        if r.problem.is_none() {
            ui.label(RichText::new(t().review_loading).color(theme::TEXT_DIM));
        }
        return;
    };
    ui.separator();
    let now = unix_now();
    let list_h = (ui.available_height() * 0.5).max(120.0);
    let branch = s.branch.as_deref().unwrap_or(t().detached_head);
    ScrollArea::vertical().id_salt("review-list").max_height(list_h).auto_shrink([false, true]).show(ui, |ui| {
        match s.state {
            ReviewState::UpToDate => {
                ui.label(RichText::new((t().review_nothing)(branch, &s.base)).color(theme::TEXT_DIM));
            }
            ReviewState::Unrelated => {
                ui.label(RichText::new((t().review_unrelated)(&s.base)).color(DEL));
            }
            ReviewState::Ahead => {
                commits_ui(changes, ui, r, s, now, acts);
                ui.add_space(6.0);
                files_ui(changes, ui, r, now, acts);
            }
        }
        if s.dirty() {
            ui.add_space(6.0);
            uncommitted_ui(changes, ui, r, s, now, own, acts);
        }
    });
    ui.separator();
    diff_ui(ui, r);
}

fn header(ui: &mut Ui, r: &Review, acts: &mut Vec<Act>) {
    let s = r.summary.as_ref();
    ui.horizontal_wrapped(|ui| {
        let branch = s.map(|s| s.branch.as_deref().unwrap_or(t().detached_head));
        if let Some(b) = branch {
            ui.label(RichText::new(format!("⎇ {b}")).strong().color(theme::TEXT));
        }
        let base = r.base_name().unwrap_or("…");
        ui.menu_button((t().review_against)(base), |ui| {
            if r.bases.is_empty() {
                ui.label(RichText::new(t().review_no_other_base).color(theme::TEXT_DIM));
            }
            for (i, b) in r.bases.iter().enumerate() {
                let label = if i == 0 && r.base.is_none() { (t().review_default_base)(b) } else { b.clone() };
                if ui.selectable_label(Some(b.as_str()) == r.base_name(), label).clicked() {
                    // The first one listed is the default branch: choosing it follows the default.
                    acts.push(Act::Base(if i == 0 && r.base.is_none() { None } else { Some(b.clone()) }));
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text(t().review_against_hint);
        if let Some(s) = s {
            match s.base_kind {
                BaseKind::Branch => {}
                BaseKind::Remote => {
                    ui.label(RichText::new(t().review_base_remote).small().color(theme::TEXT_DIM));
                }
                BaseKind::Other => {
                    ui.label(RichText::new(t().review_base_other).small().color(super::MODIFIED));
                }
            }
            ui.label(RichText::new(format!("↑{} ↓{}", s.ahead, s.behind)).color(theme::TEXT_DIM)).on_hover_text((t().ahead_behind_hint)(&s.base));
        }
        if r.loading {
            ui.spinner();
        }
    });
    if let Some(s) = s.filter(|s| s.base_moved() && s.state != ReviewState::Unrelated) {
        ui.label(RichText::new((t().review_base_moved)(&s.base, s.behind)).color(super::MODIFIED));
    }
    if s.is_some_and(|s| s.several_merge_bases) {
        ui.label(RichText::new((t().review_several_bases)(r.base_name().unwrap_or_default())).color(super::MODIFIED));
    }
}

fn commits_ui(changes: &Changes, ui: &mut Ui, r: &Review, s: &ReviewSummary, now: u64, acts: &mut Vec<Act>) {
    let total = s.commits.len() as u32 + s.more_commits;
    let all = r.selection == Selection::All;
    if ui.selectable_label(all, RichText::new((t().review_all_commits)(total)).strong()).clicked() && !all {
        acts.push(Act::All);
    }
    for c in &s.commits {
        let on = r.selection == Selection::Commit(c.sha.clone());
        let merge;
        let shown = if s.merges.contains(&c.sha) {
            merge = CommitInfo { summary: format!("{} {}", t().review_merge_mark, c.summary), ..c.clone() };
            &merge
        } else {
            c
        };
        // The same commit again, its files lost with the agent: asked again.
        let lost = on && r.commit_files.as_ref().is_some_and(|f| f.id == 0);
        if changes.commit_row(ui, shown, now, on) && (!on || lost) {
            acts.push(Act::Commit(c.sha.clone()));
        }
    }
    if s.more_commits > 0 {
        ui.label(RichText::new((t().review_more_commits)(s.more_commits)).small().color(theme::TEXT_DIM));
    }
}

/// The files of the branch, or of the selected commit, with their total.
fn files_ui(changes: &Changes, ui: &mut Ui, r: &Review, now: u64, acts: &mut Vec<Act>) {
    if r.selection == Selection::Uncommitted {
        return;
    }
    let commit = match &r.selection {
        Selection::Commit(sha) => r.summary.as_ref().and_then(|s| s.commits.iter().find(|c| &c.sha == sha)),
        _ => None,
    };
    if let Some(c) = r.commit_files.as_ref().filter(|_| commit.is_some()).and_then(|c| c.problem.as_ref()) {
        ui.label(RichText::new(t().problem(c)).color(DEL));
        return;
    }
    let Some(files) = r.shown_files() else {
        ui.label(RichText::new(t().review_loading).color(theme::TEXT_DIM));
        return;
    };
    files_list(changes, ui, r, files, commit, now, acts);
}

fn files_list(changes: &Changes, ui: &mut Ui, r: &Review, files: &ReviewFiles, commit: Option<&CommitInfo>, now: u64, acts: &mut Vec<Act>) {
    let (n, a, d) = totals(files);
    ui.horizontal(|ui| {
        let title = match commit {
            Some(c) => (t().review_commit_files)(&c.sha[..c.sha.len().min(7)], n),
            None => (t().review_files)(n),
        };
        ui.label(RichText::new(title).small().strong().color(theme::TEXT_DIM));
        ui.label(RichText::new(format!("+{a}")).small().color(ADD));
        ui.label(RichText::new(format!("−{d}")).small().color(DEL));
    });
    let shown = r.shown_file().map(|f| f.path.as_str());
    // The diff shown, lost with the agent: a click on its file asks it again.
    let lost = r.diff.as_ref().is_some_and(|d| d.id == 0);
    for f in &files.files {
        let on = shown == Some(f.path.as_str());
        if changes.file_row(ui, f, now, on) && (!on || lost) {
            acts.push(Act::File(f.path.clone()));
        }
    }
    if files.more > 0 {
        ui.label(RichText::new((t().review_more_files)(files.more)).small().color(theme::TEXT_DIM));
    }
}

/// What is not committed: collapsed; opening it shows those files and their diffs in place of the
/// branch's (the working changes of that worktree).
fn uncommitted_ui(changes: &Changes, ui: &mut Ui, r: &Review, s: &ReviewSummary, now: u64, own: bool, acts: &mut Vec<Act>) {
    let open = r.selection == Selection::Uncommitted;
    let title = RichText::new((t().review_uncommitted)(s.uncommitted.count())).small().strong().color(super::MODIFIED);
    let section = CollapsingHeader::new(title).id_salt("review-uncommitted").open(Some(open)).show(ui, |ui| {
        ui.label(RichText::new(t().review_uncommitted_hint).small().color(theme::TEXT_DIM));
        if own && ui.small_button(t().review_open_working).clicked() {
            acts.push(Act::Working);
        }
        files_list(changes, ui, r, &s.uncommitted, None, now, acts);
    });
    if section.header_response.clicked() {
        acts.push(Act::Uncommitted(!open));
    }
}

fn diff_ui(ui: &mut Ui, r: &Review) {
    let Some(d) = &r.diff else {
        if r.state() == Some(ReviewState::Ahead) {
            ui.label(RichText::new(t().pick_a_file).color(theme::TEXT_DIM));
        }
        return;
    };
    let title = match (&d.ask.path, &d.ask.scope) {
        (Some(p), _) => p.clone(),
        (None, ReviewScope::Commit { sha }) => format!("commit {}", &sha[..sha.len().min(7)]),
        (None, _) => String::new(),
    };
    ui.label(RichText::new(title).strong().color(theme::TEXT));
    let Some(lines) = &d.lines else {
        ui.label(RichText::new(t().loading_diff).color(theme::TEXT_DIM));
        return;
    };
    if let Some(p) = &d.problem {
        ui.label(RichText::new(t().problem(p)).color(theme::TEXT_DIM));
        return;
    }
    diff_body(ui, lines, d.truncated, "review-diff");
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{CommitInfo, Worktree};

    fn file(path: &str, added: u32, removed: u32) -> FileChange {
        FileChange { path: path.into(), kind: ChangeKind::Modified, added: Some(added), removed: Some(removed), mtime: 0, binary: false }
    }

    fn commit(c: char) -> CommitInfo {
        CommitInfo { sha: c.to_string().repeat(40), summary: format!("commit {c}"), author: "me".into(), time: 1 }
    }

    fn summary(state: ReviewState) -> ReviewSummary {
        ReviewSummary {
            worktree: "/home/me/shop/.claude/worktrees/invoices".into(),
            branch: Some("feat/invoices".into()),
            head: "h".repeat(40),
            base: "main".into(),
            base_sha: "b".repeat(40),
            merge_base: (state != ReviewState::Unrelated).then(|| "m".repeat(40)),
            state,
            ahead: if state == ReviewState::Ahead { 2 } else { 0 },
            behind: 1,
            commits: if state == ReviewState::Ahead { vec![commit('2'), commit('1')] } else { vec![] },
            more_commits: 0,
            files: if state == ReviewState::Ahead { ReviewFiles { files: vec![file("a.rs", 3, 1), file("b.rs", 1, 0)], more: 3, added: 40, removed: 2 } } else { ReviewFiles::default() },
            uncommitted: ReviewFiles::default(),
            base_kind: protocol::BaseKind::Branch,
            several_merge_bases: false,
            merges: vec![],
        }
    }

    fn worktree(path: &str, branch: Option<&str>, head: &str) -> Worktree {
        Worktree {
            path: path.into(),
            head: Some(head.into()),
            branch: branch.map(String::from),
            detached: branch.is_none(),
            main: false,
            bare: false,
            locked: false,
            lock_reason: None,
            prunable: false,
            dirty: Some(false),
            ahead: Some(2),
            behind: Some(1),
            merged: None,
            last_commit: None,
            branch_deletable: false,
            reflog_commits: false,
            status_mark: None,
        }
    }

    fn set(root: &str, branch: Option<&str>) -> ChangeSet {
        ChangeSet { key: "p".into(), root: Some(root.into()), git: true, branch: branch.map(String::from), detached: branch.is_none(), error: None, files: vec![], truncated: false, commits: vec![] }
    }

    #[test]
    fn the_switch_says_why_it_cannot_review() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::En);
        let list = WorktreeList { key: "p".into(), repo: Some("/r".into()), default_branch: Some("origin/main".into()), worktrees: vec![], merged_branches: vec![] };
        let on_main = set("/r", Some("main"));
        assert_eq!(unavailable(Some(&on_main), Some(&list), false), "The folder is on main: nothing to review here. Review a worktree from the strip, or a task in review.");
        assert_eq!(unavailable(Some(&on_main), None, false), "Waiting for the agent to list this folder's worktrees…", "not known yet, not \"not git\"");
        assert_eq!(unavailable(None, Some(&list), false), "Waiting for the agent to list this folder's worktrees…");
        let mut plain = on_main.clone();
        plain.git = false;
        assert_eq!(unavailable(Some(&plain), None, false), "Review needs a git folder on another branch than its base.");
        assert_eq!(unavailable(Some(&on_main), Some(&list), true), "The agent is too old to review branches: restart the app to update it.");
    }

    #[test]
    fn the_switch_reviews_the_folder_only_off_its_base() {
        assert_eq!(folder_target(&set("/r", Some("feat")), Some("main")).as_deref(), Some("/r"));
        assert_eq!(folder_target(&set("/r", Some("main")), Some("main")), None, "on the base");
        assert_eq!(folder_target(&set("/r", Some("main")), Some("origin/main")), None, "the remote's base is the same branch");
        assert_eq!(folder_target(&set("/r", None), Some("main")).as_deref(), Some("/r"), "detached");
        assert_eq!(folder_target(&set("/r", Some("feat")), None).as_deref(), Some("/r"), "no default branch known: the agent says");
        let mut plain = set("/r", Some("feat"));
        plain.git = false;
        assert_eq!(folder_target(&plain, Some("main")), None, "outside git");
    }

    #[test]
    fn heads_follow_the_reviewed_worktree_and_the_base_branch() {
        let list = WorktreeList {
            key: "p".into(),
            repo: Some("/home/me/shop".into()),
            default_branch: Some("origin/main".into()),
            worktrees: vec![worktree("/home/me/shop", Some("main"), "1"), worktree("/home/me/shop/wt/inv", Some("inv"), "2")],
            merged_branches: vec![],
        };
        let h = heads(&list, "/home/me/shop/wt/inv/", None).unwrap();
        assert_eq!((h.head.as_deref(), h.base_head.as_deref()), (Some("2"), Some("1")), "the default branch's local checkout");
        assert_eq!(heads(&list, "/home/me/shop/wt/inv", Some("inv")).unwrap().base_head.as_deref(), Some("2"));
        assert_eq!(heads(&list, "/home/me/shop/wt/inv", Some("develop")).unwrap().base_head, None);
        assert!(heads(&list, "/home/me/elsewhere", None).is_none());
    }

    #[test]
    fn selecting_a_commit_narrows_the_files_and_the_diff() {
        let mut r = Review::new("/w");
        r.answered(Some(summary(ReviewState::Ahead)), vec!["main".into()], None);
        assert_eq!(r.shown_files().map(|f| f.files.len()), Some(2));
        assert_eq!(totals(r.shown_files().unwrap()), (5, 40, 2), "listed and not listed");
        let branch = ReviewScope::Branch { merge_base: "m".repeat(40), head: "h".repeat(40) };
        assert_eq!(r.wanted_diff(), Some(DiffAsk { scope: branch.clone(), path: Some("a.rs".into()), old_path: None }), "the first file by default");
        r.file = Some("b.rs".into());
        assert_eq!(r.wanted_diff().unwrap().path.as_deref(), Some("b.rs"));

        // One commit: the whole commit until its files come, then one of them.
        let sha = "1".repeat(40);
        r.selection = Selection::Commit(sha.clone());
        r.file = None;
        r.commit_files = Some(CommitFiles { id: 4, sha: sha.clone(), files: None, problem: None });
        assert!(r.shown_files().is_none(), "not known yet");
        assert_eq!(r.wanted_diff(), Some(DiffAsk { scope: ReviewScope::Commit { sha: sha.clone() }, path: None, old_path: None }));
        let mut renamed = file("new.rs", 0, 0);
        renamed.kind = ChangeKind::Renamed("old.rs".into());
        r.commit_files.as_mut().unwrap().files = Some(ReviewFiles { files: vec![renamed], more: 0, added: 0, removed: 0 });
        assert_eq!(totals(r.shown_files().unwrap()), (1, 0, 0));
        r.file = Some("new.rs".into());
        assert_eq!(r.wanted_diff(), Some(DiffAsk { scope: ReviewScope::Commit { sha: sha.clone() }, path: Some("new.rs".into()), old_path: Some("old.rs".into()) }));
        // Another commit's files do not count for this one.
        r.selection = Selection::Commit("2".repeat(40));
        assert!(r.shown_files().is_none());

        // A refresh where the commit is gone (history rewritten): back to the whole branch.
        r.selection = Selection::Commit(sha);
        let mut s = summary(ReviewState::Ahead);
        s.commits.retain(|c| c.sha.starts_with('2'));
        r.answered(Some(s), vec![], None);
        assert_eq!((r.selection.clone(), r.file.clone()), (Selection::All, None));
    }

    #[test]
    fn nothing_to_review_and_unrelated_histories_ask_no_diff() {
        let mut r = Review::new("/w");
        r.answered(Some(summary(ReviewState::UpToDate)), vec![], None);
        assert_eq!((r.state(), r.wanted_diff()), (Some(ReviewState::UpToDate), None));
        r.answered(Some(summary(ReviewState::Unrelated)), vec![], None);
        assert_eq!((r.state(), r.wanted_diff()), (Some(ReviewState::Unrelated), None));
        r.answered(None, vec!["main".into()], Some(Problem::ReviewBaseNotFound { base: "x".into() }));
        assert!(r.summary.is_none() && r.problem.is_some() && r.bases == ["main"]);
    }

    #[test]
    fn uncommitted_changes_stay_apart() {
        let mut s = summary(ReviewState::UpToDate);
        s.uncommitted = ReviewFiles { files: vec![file("notes.txt", 2, 0)], more: 0, added: 2, removed: 0 };
        let mut r = Review::new("/w");
        r.answered(Some(s.clone()), vec![], None);
        r.selection = Selection::Uncommitted;
        assert_eq!(r.shown_files().map(|f| f.files[0].path.as_str()), Some("notes.txt"));
        assert_eq!(r.wanted_diff(), Some(DiffAsk { scope: ReviewScope::Uncommitted, path: Some("notes.txt".into()), old_path: None }));
        // Committed meanwhile: the section closes.
        s.uncommitted = ReviewFiles::default();
        r.answered(Some(s), vec![], None);
        assert_eq!(r.selection, Selection::All);
    }
}
