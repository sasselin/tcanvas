//! Changes: modified files in the folder linked to the open group, and their diffs, live.
//! The agent does the work (git in WSL); this slice keeps the list of watched folders
//! up to date, shows the side panel and requests diffs.

use std::collections::{BTreeMap, HashMap};

use egui::{Align, Align2, Button, Color32, CornerRadius, FontId, Layout, Panel, RichText, ScrollArea, Sense, Ui, vec2};
use protocol::{AgentCmd, AgentEvent, ChangeKind, ChangeSet, CommitInfo, FileChange, Problem};

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::{Project, ProjectId, World, unix_now};

/// A file touched more recently than this is highlighted: you see the AI at work.
const RECENT_SECS: u64 = 10;
const ADD: Color32 = Color32::from_rgb(90, 200, 120);
const DEL: Color32 = Color32::from_rgb(235, 95, 95);
const HUNK: Color32 = Color32::from_rgb(110, 160, 255);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Add,
    Del,
    Hunk,
    Meta,
    Context,
}

pub fn classify(line: &str) -> LineKind {
    if line.starts_with("+++")
        || line.starts_with("---")
        || line.starts_with("diff ")
        || line.starts_with("index ")
        || line.starts_with("new file")
        || line.starts_with("deleted file")
        || line.starts_with("similarity")
        || line.starts_with("rename ")
    {
        LineKind::Meta
    } else if line.starts_with('+') {
        LineKind::Add
    } else if line.starts_with('-') {
        LineKind::Del
    } else if line.starts_with("@@") {
        LineKind::Hunk
    } else {
        LineKind::Context
    }
}

/// What, if it changes, makes the displayed diff stale.
type Fingerprint = (u64, Option<u32>, Option<u32>, ChangeKind);

fn fingerprint(f: &FileChange) -> Fingerprint {
    (f.mtime, f.added, f.removed, f.kind.clone())
}

/// What the diff area shows: an uncommitted file, or a recent commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    File(String),
    Commit(String),
}

impl Target {
    /// Value of `file` in the agent's `AgentEvent::Diff` response.
    fn reply_key(&self) -> &str {
        match self {
            Target::File(f) | Target::Commit(f) => f,
        }
    }
}

/// The newer of the last touched file and the last commit: what "Follow" shows.
pub fn newest(set: &ChangeSet) -> Option<Target> {
    let file = set.files.iter().max_by_key(|f| f.mtime).map(|f| (f.mtime, Target::File(f.path.clone())));
    let commit = set.commits.first().map(|c| (c.time, Target::Commit(c.sha.clone())));
    match (file, commit) {
        // On a tie (file touched then committed in the same second), the working file wins.
        (Some(f), Some(c)) => Some(if c.0 > f.0 { c.1 } else { f.1 }),
        (f, c) => f.or(c).map(|x| x.1),
    }
}

fn target_fingerprint(set: &ChangeSet, t: &Target) -> Option<Fingerprint> {
    match t {
        Target::File(path) => set.files.iter().find(|f| &f.path == path).map(fingerprint),
        // A commit never changes.
        Target::Commit(_) => None,
    }
}

fn exists(set: &ChangeSet, t: &Target) -> bool {
    match t {
        Target::File(path) => set.files.iter().any(|f| &f.path == path),
        Target::Commit(sha) => set.commits.iter().any(|c| &c.sha == sha),
    }
}

/// Commands to send so the agent watches exactly the linked folders.
pub fn reconcile(watched: &mut BTreeMap<ProjectId, String>, projects: &[Project]) -> Vec<AgentCmd> {
    let wanted: BTreeMap<&str, &str> = projects.iter().filter_map(|p| p.cwd.as_deref().map(|c| (p.id.as_str(), c))).collect();
    let mut cmds = Vec::new();
    watched.retain(|key, _| {
        let keep = wanted.contains_key(key.as_str());
        if !keep {
            cmds.push(AgentCmd::Unwatch { key: key.clone() });
        }
        keep
    });
    for (key, path) in wanted {
        if watched.get(key).map(String::as_str) != Some(path) {
            watched.insert(key.to_string(), path.to_string());
            cmds.push(AgentCmd::Watch { key: key.to_string(), path: path.to_string() });
        }
    }
    cmds
}

/// "just now", "12 s ago", "3 min ago"…
pub fn age_label(now: u64, mtime: u64) -> String {
    if mtime == 0 {
        return String::new();
    }
    match now.saturating_sub(mtime) {
        0..=2 => t().just_now.into(),
        s @ 3..=59 => (t().secs_ago)(s),
        s @ 60..=3599 => (t().mins_ago)(s / 60),
        s @ 3600..=86_399 => (t().hours_ago)(s / 3600),
        s => (t().days_ago)(s / 86_400),
    }
}

/// `src/features/a.rs` → (`src/features/`, `a.rs`).
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    }
}

struct DiffView {
    key: ProjectId,
    target: Target,
    /// `None` until the agent has answered.
    lines: Option<Vec<(LineKind, String)>>,
    truncated: bool,
    /// Reason given by the agent when there is no diff.
    problem: Option<Problem>,
    fingerprint: Option<Fingerprint>,
}

pub struct Changes {
    /// Folders the agent has been asked to watch (key: project id).
    watched: BTreeMap<ProjectId, String>,
    sets: HashMap<ProjectId, ChangeSet>,
    /// Selected file or commit, per group.
    selected: HashMap<ProjectId, Target>,
    diff: Option<DiffView>,
    /// Automatically select the latest modified file (or the latest commit).
    pub follow: bool,
    /// Group shown in the "All" view: that of the last focused terminal, or of the clicked Δ badge.
    shown: Option<ProjectId>,
    /// Panel group in the previous frame: switching group re-requests its diff.
    last_panel: Option<ProjectId>,
}

impl Default for Changes {
    fn default() -> Self {
        Self { watched: BTreeMap::new(), sets: HashMap::new(), selected: HashMap::new(), diff: None, follow: true, shown: None, last_panel: None }
    }
}

impl Changes {
    pub fn set(&self, project: &str) -> Option<&ChangeSet> {
        self.sets.get(project)
    }

    /// Selected file of the group (not a commit).
    pub fn selected(&self, project: &str) -> Option<&str> {
        match self.selected.get(project) {
            Some(Target::File(f)) => Some(f),
            _ => None,
        }
    }

    pub fn selected_target(&self, project: &str) -> Option<&Target> {
        self.selected.get(project)
    }

    /// Lines of the displayed diff, if it has arrived (tests).
    pub fn diff_lines(&self) -> Option<Vec<(LineKind, String)>> {
        self.diff.as_ref().and_then(|d| d.lines.clone())
    }

    /// Group whose changes the panel shows: the open group, otherwise ("All" view)
    /// that of the last focused terminal or clicked Δ badge. It must have a linked folder.
    pub fn panel_project<'a>(&self, world: &'a World) -> Option<&'a Project> {
        world.space.as_deref().or(self.shown.as_deref()).and_then(|id| world.project(id)).filter(|p| p.cwd.is_some())
    }

    /// Every frame: syncs the agent's watches with the linked folders, and re-requests the
    /// diff when the panel switches to another group.
    pub fn poll(&mut self, world: &World, events: &mut Vec<Event>) {
        events.extend(reconcile(&mut self.watched, &world.projects).into_iter().map(Event::ToAgent));
        let panel = self.panel_project(world).map(|p| p.id.clone());
        if panel != self.last_panel {
            self.last_panel = panel.clone();
            if let Some(id) = panel
                && let Some(t) = self.selected.get(&id).cloned()
                && self.diff.as_ref().is_none_or(|d| d.key != id || d.target != t)
            {
                self.select(&id, t, events);
            }
        }
    }

    fn select(&mut self, key: &str, target: Target, out: &mut Vec<Event>) {
        let fp = self.sets.get(key).and_then(|s| target_fingerprint(s, &target));
        let cmd = match &target {
            Target::File(file) => AgentCmd::Diff { key: key.to_string(), file: file.clone() },
            Target::Commit(sha) => AgentCmd::ShowCommit { key: key.to_string(), sha: sha.clone() },
        };
        self.selected.insert(key.to_string(), target.clone());
        self.diff = Some(DiffView { key: key.to_string(), target, lines: None, truncated: false, problem: None, fingerprint: fp });
        out.push(Event::ToAgent(cmd));
    }

    /// New state of a folder: follow the last touched file (or commit), refresh the displayed diff.
    fn on_changes(&mut self, set: ChangeSet, world: &mut World, out: &mut Vec<Event>) {
        let key = set.key.clone();
        if let Some(t) = set.files.iter().map(|f| f.mtime).max() {
            world.file_activity.insert(key.clone(), t);
        }
        world.change_counts.insert(key.clone(), set.files.len());
        let newest = newest(&set);
        self.sets.insert(key.clone(), set);
        let set = &self.sets[&key];
        let current = self.selected.get(&key).cloned();
        let target = match (&current, &newest) {
            (_, Some(n)) if self.follow => Some(n.clone()),
            (Some(c), _) if exists(set, c) => Some(c.clone()),
            _ => None,
        };
        let Some(target) = target else {
            self.selected.remove(&key);
            if self.diff.as_ref().is_some_and(|d| d.key == key) {
                self.diff = None;
            }
            return;
        };
        let fp = target_fingerprint(set, &target);
        let shown = self.diff.as_ref().is_some_and(|d| d.key == key && d.target == target && d.fingerprint == fp);
        // Only the panel's group shows a diff: no need to request it for the others.
        if !shown && self.panel_project(world).is_some_and(|p| p.id == key) {
            self.select(&key, target, out);
        } else {
            self.selected.insert(key, target);
        }
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, out: &mut Vec<Event>) {
        match ev {
            // Agent (re)started: it watches nothing anymore, so resend everything next frame.
            Event::Agent(AgentEvent::Hello { .. }) => self.watched.clear(),
            Event::ToggleChanges => {
                world.changes_open = !world.changes_open;
                world.touch();
            }
            Event::ShowChanges(id) => {
                self.shown = Some(id.clone());
                if !world.changes_open {
                    world.changes_open = true;
                    world.touch();
                }
            }
            // In the "All" view, the panel follows the group of the terminal being worked in.
            Event::FocusCard(id) => {
                if let Some(p) = world.card(id).and_then(|c| c.project.clone()) {
                    self.shown = Some(p);
                }
            }
            Event::Agent(AgentEvent::Changes(set)) => {
                if world.project(&set.key).is_some_and(|p| p.cwd.is_some()) {
                    self.on_changes(set.clone(), world, out);
                }
            }
            Event::Agent(AgentEvent::Diff { key, file, text, truncated, problem }) => {
                if let Some(d) = self.diff.as_mut().filter(|d| &d.key == key && d.target.reply_key() == file) {
                    d.lines = Some(text.lines().map(|l| (classify(l), l.to_string())).collect());
                    d.truncated = *truncated;
                    d.problem = problem.clone();
                }
            }
            Event::SetProjectCwd(id, _) | Event::RemoveProject(id) => {
                self.sets.remove(id);
                self.selected.remove(id);
                world.file_activity.remove(id);
                world.change_counts.remove(id);
                if self.diff.as_ref().is_some_and(|d| &d.key == id) {
                    self.diff = None;
                }
            }
            _ => {}
        }
    }

    /// Top bar button: opens or closes the panel.
    pub fn toolbar_ui(&mut self, ui: &mut Ui, world: &World, events: &mut Vec<Event>) {
        let Some(p) = self.panel_project(world) else { return };
        let n = self.sets.get(&p.id).map(|s| s.files.len()).unwrap_or(0);
        let on = world.changes_open;
        let text = RichText::new((t().changes_button)(n)).color(if on { theme::CARD_BG } else { theme::TEXT });
        let button = Button::selectable(on, text).fill(if on { theme::TEXT } else { Color32::TRANSPARENT });
        if ui.add(button).on_hover_text(t().changes_button_hint).clicked() {
            events.push(Event::ToggleChanges);
        }
    }

    /// Side panel. Call before the canvas (it takes space from it).
    pub fn panel_ui(&mut self, ui: &mut Ui, world: &World, events: &mut Vec<Event>) {
        if !world.changes_open {
            return;
        }
        let Some(project) = self.panel_project(world).cloned() else { return };
        Panel::right("changes").resizable(true).default_size(440.0).min_size(260.0).show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Δ {}", project.name)).strong().color(theme::rgb(project.color)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("×").on_hover_text(t().close_panel).clicked() {
                        events.push(Event::ToggleChanges);
                    }
                    let follow = ui.checkbox(&mut self.follow, t().follow).on_hover_text(t().follow_hint);
                    if follow.changed()
                        && self.follow
                        && let Some(n) = self.sets.get(&project.id).and_then(newest)
                    {
                        self.select(&project.id, n, events);
                    }
                });
            });
            let Some(set) = self.sets.get(&project.id).cloned() else {
                ui.label(RichText::new(format!("📁 {}", project.cwd.as_deref().unwrap_or_default())).color(theme::TEXT_DIM));
                ui.label(RichText::new(t().waiting_for_agent).color(theme::TEXT_DIM));
                return;
            };
            self.header(ui, &project, &set);
            ui.separator();
            let now = unix_now();
            let list_h = (ui.available_height() * 0.42).max(100.0);
            ScrollArea::vertical().id_salt("changes-list").max_height(list_h).auto_shrink([false, true]).show(ui, |ui| {
                if set.git {
                    ui.label(RichText::new((t().uncommitted)(set.files.len())).small().strong().color(theme::TEXT_DIM));
                }
                if set.files.is_empty() && set.error.is_none() {
                    ui.label(RichText::new(if set.git { t().all_committed } else { t().nothing_modified }).color(theme::TEXT_DIM));
                }
                for f in &set.files {
                    let on = self.selected(&project.id) == Some(f.path.as_str());
                    if self.file_row(ui, f, now, on) {
                        self.follow = false;
                        self.select(&project.id, Target::File(f.path.clone()), events);
                    }
                }
                if set.truncated {
                    ui.label(RichText::new(t().list_truncated).color(theme::TEXT_DIM));
                }
                if !set.commits.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(t().recent_commits).small().strong().color(theme::TEXT_DIM));
                    for c in &set.commits {
                        let on = self.selected_target(&project.id) == Some(&Target::Commit(c.sha.clone()));
                        if self.commit_row(ui, c, now, on) {
                            self.follow = false;
                            self.select(&project.id, Target::Commit(c.sha.clone()), events);
                        }
                    }
                }
            });
            if set.files.iter().any(|f| now.saturating_sub(f.mtime) < RECENT_SECS) {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(5));
            }
            ui.separator();
            self.diff_ui(ui, &project.id, set.git);
        });
    }

    fn header(&self, ui: &mut Ui, project: &Project, set: &ChangeSet) {
        let root = set.root.as_deref().or(project.cwd.as_deref()).unwrap_or_default();
        ui.label(RichText::new(format!("📁 {root}")).color(theme::TEXT_DIM)).on_hover_text(root);
        if let Some(err) = &set.error {
            ui.label(RichText::new(format!("⚠ {}", t().problem(err))).color(DEL));
            return;
        }
        ui.horizontal(|ui| {
            if let Some(b) = set.branch.as_deref().or(set.detached.then_some(t().detached_head)) {
                ui.label(RichText::new(format!("⎇ {b}")).color(theme::TEXT));
            }
            let (a, r) = set.files.iter().fold((0, 0), |(a, r), f| (a + f.added.unwrap_or(0), r + f.removed.unwrap_or(0)));
            let n = set.files.len();
            ui.label(RichText::new((t().file_count)(n)).color(theme::TEXT_DIM));
            if set.git {
                ui.label(RichText::new(format!("+{a}")).color(ADD));
                ui.label(RichText::new(format!("−{r}")).color(DEL));
            }
        });
        if !set.git {
            ui.label(RichText::new(t().not_a_git_repo).color(theme::TEXT_DIM));
        }
    }

    /// One list row. Returns `true` if it was clicked.
    fn file_row(&self, ui: &mut Ui, f: &FileChange, now: u64, selected: bool) -> bool {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
        let resp = resp.on_hover_text(match &f.kind {
            ChangeKind::Renamed(from) => format!("{from} → {}", f.path),
            _ => f.path.clone(),
        });
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, (t().modified_file)(&f.path)));
        let painter = ui.painter_at(rect);
        let age = now.saturating_sub(f.mtime);
        if selected {
            painter.rect_filled(rect, CornerRadius::same(4), theme::FOCUS.gamma_multiply(0.25));
        } else if f.mtime > 0 && age < RECENT_SECS {
            // Highlight that fades out within a few seconds.
            let fade = 1.0 - age as f32 / RECENT_SECS as f32;
            painter.rect_filled(rect, CornerRadius::same(4), ADD.gamma_multiply(0.25 * fade));
        } else if resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(4), theme::TITLE_BG);
        }
        let color = match f.kind {
            ChangeKind::Added | ChangeKind::Untracked => ADD,
            ChangeKind::Deleted => DEL,
            ChangeKind::Renamed(_) => HUNK,
            ChangeKind::Modified => Color32::from_rgb(230, 180, 80),
        };
        let mono = FontId::monospace(12.0);
        let y = rect.center().y;
        painter.text(egui::pos2(rect.min.x + 6.0, y), Align2::LEFT_CENTER, f.kind.letter(), mono.clone(), color);
        // On the right: age then +/−, measured to leave room for the name.
        let mut right = rect.max.x - 6.0;
        let age_text = age_label(now, f.mtime);
        let small = FontId::proportional(11.0);
        let w = painter.text(egui::pos2(right, y), Align2::RIGHT_CENTER, &age_text, small, theme::TEXT_DIM).width();
        right -= w + 10.0;
        if f.binary {
            let w = painter.text(egui::pos2(right, y), Align2::RIGHT_CENTER, "bin", mono.clone(), theme::TEXT_DIM).width();
            right -= w + 8.0;
        } else {
            if let Some(r) = f.removed {
                let w = painter.text(egui::pos2(right, y), Align2::RIGHT_CENTER, format!("−{r}"), mono.clone(), DEL).width();
                right -= w + 6.0;
            }
            if let Some(a) = f.added {
                let w = painter.text(egui::pos2(right, y), Align2::RIGHT_CENTER, format!("+{a}"), mono.clone(), ADD).width();
                right -= w + 8.0;
            }
        }
        let (dir, name) = split_path(&f.path);
        let clip = painter.with_clip_rect(egui::Rect::from_min_max(rect.min, egui::pos2(right, rect.max.y)));
        let x = rect.min.x + 22.0;
        let w = clip.text(egui::pos2(x, y), Align2::LEFT_CENTER, name, FontId::proportional(13.0), theme::TEXT).width();
        clip.text(egui::pos2(x + w + 6.0, y), Align2::LEFT_CENTER, dir, FontId::proportional(11.0), theme::TEXT_DIM);
        resp.clicked()
    }

    /// A commit row. Returns `true` if it was clicked.
    fn commit_row(&self, ui: &mut Ui, c: &CommitInfo, now: u64, selected: bool) -> bool {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
        let resp = resp.on_hover_text(format!("{}\n{} · {}", c.summary, c.author, &c.sha[..c.sha.len().min(12)]));
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, (t().commit_a11y)(&c.summary)));
        let painter = ui.painter_at(rect);
        let age = now.saturating_sub(c.time);
        if selected {
            painter.rect_filled(rect, CornerRadius::same(4), theme::FOCUS.gamma_multiply(0.25));
        } else if age < RECENT_SECS {
            painter.rect_filled(rect, CornerRadius::same(4), HUNK.gamma_multiply(0.25 * (1.0 - age as f32 / RECENT_SECS as f32)));
        } else if resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(4), theme::TITLE_BG);
        }
        let y = rect.center().y;
        let mono = FontId::monospace(12.0);
        let sha = painter.text(egui::pos2(rect.min.x + 6.0, y), Align2::LEFT_CENTER, &c.sha[..c.sha.len().min(7)], mono, HUNK);
        let w = painter.text(egui::pos2(rect.max.x - 6.0, y), Align2::RIGHT_CENTER, age_label(now, c.time), FontId::proportional(11.0), theme::TEXT_DIM).width();
        let clip = painter.with_clip_rect(egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x - w - 16.0, rect.max.y)));
        clip.text(egui::pos2(sha.max.x + 8.0, y), Align2::LEFT_CENTER, &c.summary, FontId::proportional(13.0), theme::TEXT);
        resp.clicked()
    }

    fn diff_ui(&self, ui: &mut Ui, key: &str, git: bool) {
        let Some(d) = self.diff.as_ref().filter(|d| d.key == key) else {
            if git {
                ui.label(RichText::new(t().pick_a_file).color(theme::TEXT_DIM));
            }
            return;
        };
        let title = match &d.target {
            Target::File(f) => f.clone(),
            Target::Commit(sha) => format!("commit {}", &sha[..sha.len().min(7)]),
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
        let mono = FontId::monospace(12.0);
        let row_h = ui.fonts_mut(|f| f.row_height(&mono));
        ScrollArea::both().id_salt("changes-diff").auto_shrink([false, false]).show_rows(ui, row_h, lines.len() + usize::from(d.truncated), |ui, range| {
            for i in range {
                let Some((kind, text)) = lines.get(i) else {
                    ui.label(RichText::new(t().diff_truncated).color(theme::TEXT_DIM));
                    continue;
                };
                let (color, bg) = match kind {
                    LineKind::Add => (ADD, ADD.gamma_multiply(0.10)),
                    LineKind::Del => (DEL, DEL.gamma_multiply(0.10)),
                    LineKind::Hunk => (HUNK, Color32::TRANSPARENT),
                    LineKind::Meta => (theme::TEXT_DIM, Color32::TRANSPARENT),
                    LineKind::Context => (theme::TEXT, Color32::TRANSPARENT),
                };
                let text = RichText::new(text.replace('\t', "    ")).font(mono.clone()).color(color).background_color(bg);
                ui.add(egui::Label::new(text).extend());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect, Vec2};

    fn project(id: &str, cwd: Option<&str>) -> Project {
        Project { id: id.into(), name: id.into(), color: [0; 3], rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)), cwd: cwd.map(Into::into) }
    }

    fn file(path: &str, mtime: u64, added: u32) -> FileChange {
        FileChange { path: path.into(), kind: ChangeKind::Modified, added: Some(added), removed: Some(0), mtime, binary: false }
    }

    fn set(key: &str, files: Vec<FileChange>) -> ChangeSet {
        ChangeSet { key: key.into(), root: Some("/r".into()), git: true, branch: Some("main".into()), detached: false, error: None, files, truncated: false, commits: vec![] }
    }

    fn world() -> World {
        let mut w = World::default();
        w.projects.push(project("api", Some("~/api")));
        w.space = Some("api".into());
        w
    }

    fn diff_requests(out: &[Event]) -> Vec<String> {
        out.iter()
            .filter_map(|e| match e {
                Event::ToAgent(AgentCmd::Diff { file, .. }) => Some(file.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn diff_lines_are_classified() {
        let kinds: Vec<LineKind> = ["diff --git a/x b/x", "index 1..2", "--- a/x", "+++ b/x", "@@ -1 +1 @@", "-old", "+new", " same"].iter().map(|l| classify(l)).collect();
        use LineKind::*;
        assert_eq!(kinds, vec![Meta, Meta, Meta, Meta, Hunk, Del, Add, Context]);
    }

    #[test]
    fn reconcile_watches_linked_folders_only_once() {
        let mut watched = BTreeMap::new();
        let mut projects = vec![project("a", Some("~/a")), project("b", None)];
        assert_eq!(reconcile(&mut watched, &projects), vec![AgentCmd::Watch { key: "a".into(), path: "~/a".into() }]);
        assert!(reconcile(&mut watched, &projects).is_empty(), "already watched");
        projects[0].cwd = Some("~/a2".into());
        projects[1].cwd = Some("~/b".into());
        let mut cmds = reconcile(&mut watched, &projects);
        cmds.sort_by_key(|c| format!("{c:?}"));
        assert_eq!(cmds, vec![AgentCmd::Watch { key: "a".into(), path: "~/a2".into() }, AgentCmd::Watch { key: "b".into(), path: "~/b".into() }]);
        projects.remove(0);
        assert_eq!(reconcile(&mut watched, &projects), vec![AgentCmd::Unwatch { key: "a".into() }]);
    }

    #[test]
    fn agent_restart_resends_watches() {
        let mut c = Changes::default();
        let w = world();
        let mut ev = vec![];
        c.poll(&w, &mut ev);
        assert_eq!(ev.len(), 1);
        c.handle(&Event::Agent(AgentEvent::Hello { version: "x".into(), hooks: protocol::Hooks::Installed }), &mut world(), &mut vec![]);
        let mut ev = vec![];
        c.poll(&w, &mut ev);
        assert!(matches!(ev.as_slice(), [Event::ToAgent(AgentCmd::Watch { .. })]));
    }

    #[test]
    fn follow_selects_the_newest_file_and_refreshes_its_diff() {
        let mut c = Changes::default();
        let mut w = world();
        let mut out = vec![];
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 10, 1), file("b.rs", 20, 1)]))), &mut w, &mut out);
        assert_eq!(diff_requests(&out), vec!["b.rs"]);
        assert_eq!(w.file_activity.get("api"), Some(&20));
        // Same state: nothing to re-request.
        let mut out = vec![];
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 10, 1), file("b.rs", 20, 1)]))), &mut w, &mut out);
        assert!(diff_requests(&out).is_empty());
        // a.rs was just touched: follow it.
        let mut out = vec![];
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 30, 2), file("b.rs", 20, 1)]))), &mut w, &mut out);
        assert_eq!(diff_requests(&out), vec!["a.rs"]);
        c.handle(&Event::Agent(AgentEvent::Diff { key: "api".into(), file: "a.rs".into(), text: "@@ x\n+a\n".into(), truncated: false, problem: None }), &mut w, &mut vec![]);
        assert_eq!(c.diff_lines().unwrap(), vec![(LineKind::Hunk, "@@ x".into()), (LineKind::Add, "+a".into())]);
    }

    #[test]
    fn without_follow_the_selection_stays_and_refreshes_when_its_file_changes() {
        let mut c = Changes { follow: false, ..Default::default() };
        let mut w = world();
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 10, 1), file("b.rs", 20, 1)]))), &mut w, &mut vec![]);
        assert_eq!(c.selected("api"), None, "nothing forced without Follow");
        let mut out = vec![];
        c.select("api", Target::File("a.rs".into()), &mut out);
        let mut out = vec![];
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 10, 1), file("b.rs", 40, 3)]))), &mut w, &mut out);
        assert!(diff_requests(&out).is_empty(), "b.rs changed, not a.rs");
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 50, 4), file("b.rs", 40, 3)]))), &mut w, &mut out);
        assert_eq!(diff_requests(&out), vec!["a.rs"]);
        // a.rs committed: it leaves the list, and so does the selection.
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("b.rs", 40, 3)]))), &mut w, &mut vec![]);
        assert_eq!(c.selected("api"), None);
        assert!(c.diff_lines().is_none());
    }

    fn commit(sha: char, time: u64) -> protocol::CommitInfo {
        protocol::CommitInfo { sha: sha.to_string().repeat(40), summary: format!("commit {sha}"), author: "t".into(), time }
    }

    fn requests(out: &[Event]) -> Vec<AgentCmd> {
        out.iter()
            .filter_map(|e| match e {
                Event::ToAgent(c @ (AgentCmd::Diff { .. } | AgentCmd::ShowCommit { .. })) => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_clean_repo_shows_its_latest_commit() {
        let mut c = Changes::default();
        let mut w = world();
        let mut s = set("api", vec![]);
        s.commits = vec![commit('b', 50), commit('a', 10)];
        let mut out = vec![];
        c.handle(&Event::Agent(AgentEvent::Changes(s.clone())), &mut w, &mut out);
        assert_eq!(requests(&out), vec![AgentCmd::ShowCommit { key: "api".into(), sha: "b".repeat(40) }]);
        assert_eq!(w.change_counts.get("api"), Some(&0));
        let sha = "b".repeat(40);
        c.handle(&Event::Agent(AgentEvent::Diff { key: "api".into(), file: sha, text: "commit x\n+a\n".into(), truncated: false, problem: None }), &mut w, &mut vec![]);
        assert_eq!(c.diff_lines().unwrap()[1], (LineKind::Add, "+a".into()));
        // A file modified after the last commit takes precedence.
        s.files = vec![file("x.rs", 60, 1)];
        let mut out = vec![];
        c.handle(&Event::Agent(AgentEvent::Changes(s)), &mut w, &mut out);
        assert_eq!(requests(&out), vec![AgentCmd::Diff { key: "api".into(), file: "x.rs".into() }]);
        assert_eq!(w.change_counts.get("api"), Some(&1));
    }

    #[test]
    fn in_the_overview_the_panel_follows_the_focused_terminal_or_the_clicked_badge() {
        let mut c = Changes::default();
        let mut w = world();
        w.projects.push(project("web", Some("~/web")));
        w.space = None;
        assert!(c.panel_project(&w).is_none(), "'All' view, nothing chosen");
        // Both groups have changes; no diff is requested while none is shown.
        let mut out = vec![];
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 10, 1)]))), &mut w, &mut out);
        c.handle(&Event::Agent(AgentEvent::Changes(set("web", vec![file("w.rs", 10, 1)]))), &mut w, &mut out);
        assert!(requests(&out).is_empty());

        let mut card = crate::shell::world::Card::new(egui::Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0)), None);
        card.project = Some("web".into());
        let id = card.id.clone();
        w.add_card(card);
        c.handle(&Event::FocusCard(id), &mut w, &mut vec![]);
        assert_eq!(c.panel_project(&w).map(|p| p.id.as_str()), Some("web"));
        let mut out = vec![];
        c.poll(&w, &mut out);
        assert_eq!(diff_requests(&out), vec!["w.rs"], "the shown group's diff is requested");

        c.handle(&Event::ShowChanges("api".into()), &mut w, &mut vec![]);
        assert!(w.changes_open, "the badge opens the panel");
        let mut out = vec![];
        c.poll(&w, &mut out);
        assert_eq!(diff_requests(&out), vec!["a.rs"]);
        let mut out = vec![];
        c.poll(&w, &mut out);
        assert!(diff_requests(&out).is_empty(), "only once");
    }

    #[test]
    fn newest_prefers_the_file_being_edited_on_a_tie() {
        let mut s = set("api", vec![file("a.rs", 10, 1)]);
        assert_eq!(newest(&s), Some(Target::File("a.rs".into())));
        s.commits = vec![commit('c', 10)];
        assert_eq!(newest(&s), Some(Target::File("a.rs".into())));
        s.commits = vec![commit('c', 11)];
        assert_eq!(newest(&s), Some(Target::Commit("c".repeat(40))));
        assert_eq!(newest(&set("api", vec![])), None);
    }

    #[test]
    fn stale_answers_are_ignored() {
        let mut c = Changes::default();
        let mut w = world();
        c.handle(&Event::Agent(AgentEvent::Changes(set("unknown", vec![file("a.rs", 1, 1)]))), &mut w, &mut vec![]);
        assert!(c.set("unknown").is_none());
        c.handle(&Event::Agent(AgentEvent::Diff { key: "api".into(), file: "z.rs".into(), text: "+x".into(), truncated: false, problem: None }), &mut w, &mut vec![]);
        assert!(c.diff_lines().is_none());
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 1, 1)]))), &mut w, &mut vec![]);
        c.handle(&Event::SetProjectCwd("api".into(), Some("~/other".into())), &mut w, &mut vec![]);
        assert!(c.set("api").is_none() && c.selected("api").is_none() && !w.file_activity.contains_key("api"));
    }

    #[test]
    fn labels() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        assert_eq!(age_label(100, 99), "à l'instant");
        assert_eq!(age_label(100, 88), "il y a 12 s");
        assert_eq!(age_label(1000, 100), "il y a 15 min");
        assert_eq!(age_label(100, 0), "");
        assert_eq!(split_path("src/a/b.rs"), ("src/a/", "b.rs"));
        assert_eq!(split_path("b.rs"), ("", "b.rs"));
    }
}
