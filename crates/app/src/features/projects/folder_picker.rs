//! Folder picker (WSL) for linking a group: the agent lists subfolders, you navigate
//! with the mouse (breadcrumb, recent folders, filter) or by typing a path.

use egui::{Align, Color32, Id, Key, Layout, Modal, RichText, ScrollArea, TextEdit, Ui};
use protocol::{AgentCmd, DirEntry, DirListing};

use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::ProjectId;

const GIT: Color32 = Color32::from_rgb(230, 150, 60);
/// A linked worktree: another checkout of a repository listed elsewhere.
const WORKTREE: Color32 = Color32::from_rgb(110, 160, 255);

/// Marker of a subfolder holding a repository: a linked worktree reads differently.
pub fn git_marker(d: &DirEntry) -> (&'static str, Color32) {
    if d.worktree { (t().worktree_marker, WORKTREE) } else { ("⎇ git", GIT) }
}

/// What the user decided this frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    /// Go into this folder (requests its listing from the agent).
    Navigate(String),
    Link(String),
    Unlink,
    Cancel,
}

pub struct FolderPicker {
    pub project: ProjectId,
    pub title: String,
    /// The group already has a folder: also offer to unlink it.
    pub linked: bool,
    /// Path field, editable (Enter: go there).
    pub input: String,
    /// Requested path whose listing we are waiting for.
    pub pending: Option<String>,
    pub listing: Option<DirListing>,
    pub filter: String,
    /// Shortcuts: folders of the group's terminals, of other groups, home folder.
    pub shortcuts: Vec<String>,
    /// First frame: the filter takes the keyboard.
    fresh: bool,
}

/// `/home/me/code` → [(`/`, `/`), (`home`, `/home`), (`me`, `/home/me`), (`code`, `/home/me/code`)].
pub fn breadcrumb(path: &str) -> Vec<(String, String)> {
    let mut out = vec![("/".to_string(), "/".to_string())];
    let mut acc = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        acc.push('/');
        acc.push_str(part);
        out.push((part.to_string(), acc.clone()));
    }
    out
}

pub fn join(dir: &str, name: &str) -> String {
    format!("{}/{name}", dir.trim_end_matches('/'))
}

impl FolderPicker {
    /// Opens the picker on `start` (empty: home folder); returns the command to send.
    pub fn open(project: ProjectId, title: String, linked: bool, start: &str, shortcuts: Vec<String>) -> (Self, AgentCmd) {
        let start = if start.trim().is_empty() { "~".to_string() } else { start.trim().to_string() };
        let picker = Self { project, title, linked, input: start.clone(), pending: Some(start.clone()), listing: None, filter: String::new(), shortcuts, fresh: true };
        (picker, AgentCmd::ListDir { path: start })
    }

    pub fn navigate(&mut self, path: &str) -> AgentCmd {
        self.input = path.to_string();
        self.filter.clear();
        self.pending = Some(path.to_string());
        AgentCmd::ListDir { path: path.to_string() }
    }

    /// Agent response: only the one for the last requested path is kept.
    pub fn on_listing(&mut self, listing: &DirListing) {
        if self.pending.as_deref() != Some(listing.request.as_str()) {
            return;
        }
        self.pending = None;
        if let Some(path) = &listing.path {
            self.input = path.clone();
        }
        self.listing = Some(listing.clone());
    }

    /// Subfolders matching the filter (case-insensitive).
    pub fn visible(&self) -> Vec<&DirEntry> {
        let f = self.filter.trim().to_lowercase();
        self.listing.iter().flat_map(|l| &l.dirs).filter(|d| f.is_empty() || d.name.to_lowercase().contains(&f)).collect()
    }

    /// Folder that will be linked: the displayed one (resolved absolute path). Until the agent has
    /// answered (missing, slow), the typed path. Nothing if the agent says the folder doesn't exist.
    pub fn target(&self) -> Option<String> {
        match self.listing.as_ref().filter(|_| self.pending.is_none()) {
            Some(l) if l.error.is_some() => None,
            Some(l) => l.path.clone(),
            None => Some(self.input.trim().to_string()).filter(|s| !s.is_empty()),
        }
    }

    /// Modal window. Escape or click outside: cancel.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<Choice> {
        let mut choice = None;
        let modal = Modal::new(Id::unique("folder-picker")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.label(RichText::new(&self.title).strong().size(16.0));
            ui.add_space(6.0);
            self.path_row(ui, &mut choice);
            self.breadcrumb_row(ui, &mut choice);
            self.shortcuts_row(ui, &mut choice);
            ui.separator();
            self.list(ui, &mut choice);
            ui.separator();
            self.footer(ui, &mut choice);
        });
        if modal.should_close() && choice.is_none() {
            choice = Some(Choice::Cancel);
        }
        choice
    }

    fn path_row(&mut self, ui: &mut Ui, choice: &mut Option<Choice>) {
        ui.horizontal(|ui| {
            if ui.button("⌂").on_hover_text(t().home_folder).clicked() {
                *choice = Some(Choice::Navigate("~".into()));
            }
            let parent = self.listing.as_ref().and_then(|l| l.parent.clone());
            if ui.add_enabled(parent.is_some(), egui::Button::new("⬆")).on_hover_text(t().parent_folder).clicked()
                && let Some(p) = parent
            {
                *choice = Some(Choice::Navigate(p));
            }
            let edit = ui.add(TextEdit::singleline(&mut self.input).id(Id::unique("folder-picker-path")).desired_width(f32::INFINITY).hint_text(t().folder_path_hint()));
            if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                *choice = Some(Choice::Navigate(self.input.trim().to_string()));
            }
        });
    }

    fn breadcrumb_row(&self, ui: &mut Ui, choice: &mut Option<Choice>) {
        let Some(path) = self.listing.as_ref().and_then(|l| l.path.as_deref()) else { return };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let crumbs = breadcrumb(path);
            let last = crumbs.len() - 1;
            for (k, (name, full)) in crumbs.into_iter().enumerate() {
                if k > 1 {
                    ui.label(RichText::new("›").color(theme::TEXT_DIM));
                }
                let text = if k == last { RichText::new(&name).strong() } else { RichText::new(&name) };
                if ui.add(egui::Button::new(text).frame(false)).clicked() && k != last {
                    *choice = Some(Choice::Navigate(full));
                }
            }
        });
    }

    fn shortcuts_row(&self, ui: &mut Ui, choice: &mut Option<Choice>) {
        if self.shortcuts.is_empty() {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(t().recent_folders).color(theme::TEXT_DIM));
            for s in &self.shortcuts {
                let name = s.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(s);
                if ui.small_button(format!("📁 {name}")).on_hover_text(s).clicked() {
                    *choice = Some(Choice::Navigate(s.clone()));
                }
            }
        });
    }

    fn list(&mut self, ui: &mut Ui, choice: &mut Option<Choice>) {
        let filter = ui.add(TextEdit::singleline(&mut self.filter).id(Id::unique("folder-picker-filter")).hint_text(t().filter_folders_hint).desired_width(f32::INFINITY));
        if self.fresh {
            self.fresh = false;
            filter.request_focus();
        }
        let visible: Vec<DirEntry> = self.visible().into_iter().cloned().collect();
        let current = self.listing.as_ref().and_then(|l| l.path.clone()).unwrap_or_else(|| self.input.clone());
        if filter.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && visible.len() == 1 {
            *choice = Some(Choice::Navigate(join(&current, &visible[0].name)));
        }
        ScrollArea::vertical().id_salt("folder-picker-list").max_height(300.0).min_scrolled_height(300.0).auto_shrink([false, false]).show(ui, |ui| {
            if self.pending.is_some() {
                ui.label(RichText::new(t().loading).color(theme::TEXT_DIM));
                return;
            }
            let Some(listing) = &self.listing else { return };
            if let Some(err) = &listing.error {
                ui.label(RichText::new(format!("⚠ {}", t().problem(err))).color(Color32::from_rgb(235, 95, 95)));
                return;
            }
            if visible.is_empty() {
                ui.label(RichText::new(if listing.dirs.is_empty() { t().no_subfolders } else { t().no_matching_folder }).color(theme::TEXT_DIM));
            }
            for d in &visible {
                let resp = ui.add(egui::Button::new(format!("📁 {}", d.name)).frame(false).min_size(egui::vec2(ui.available_width(), 22.0)));
                if d.git {
                    let (marker, color) = git_marker(d);
                    ui.painter().text(resp.rect.right_center() - egui::vec2(8.0, 0.0), egui::Align2::RIGHT_CENTER, marker, egui::FontId::proportional(12.0), color);
                }
                if resp.clicked() {
                    *choice = Some(Choice::Navigate(join(&current, &d.name)));
                }
            }
        });
    }

    fn footer(&self, ui: &mut Ui, choice: &mut Option<Choice>) {
        let target = self.target();
        let git = self.listing.as_ref().filter(|l| l.git && self.pending.is_none());
        ui.horizontal(|ui| {
            ui.label(RichText::new(target.as_deref().unwrap_or("—")).color(theme::TEXT));
            match git {
                Some(l) if l.worktree => ui.label(RichText::new(t().git_worktree).color(WORKTREE)),
                Some(_) => ui.label(RichText::new(t().git_repo).color(GIT)),
                None => return,
            };
        });
        ui.horizontal(|ui| {
            let link = egui::Button::new(RichText::new(t().link_folder).color(theme::CARD_BG)).fill(theme::FOCUS);
            if ui.add_enabled(target.is_some(), link).clicked()
                && let Some(t) = target
            {
                *choice = Some(Choice::Link(t));
            }
            if ui.button(t().cancel).clicked() {
                *choice = Some(Choice::Cancel);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.linked && ui.button(t().unlink).on_hover_text(t().unlink_hint).clicked() {
                    *choice = Some(Choice::Unlink);
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(request: &str, path: &str, dirs: &[(&str, bool)]) -> DirListing {
        DirListing {
            request: request.into(),
            path: Some(path.into()),
            parent: Some("/home".into()),
            git: false,
            worktree: false,
            dirs: dirs.iter().map(|(n, g)| DirEntry { name: (*n).into(), git: *g, worktree: false }).collect(),
            error: None,
        }
    }

    #[test]
    fn linked_worktrees_get_their_own_marker() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let repo = DirEntry { name: "shop".into(), git: true, worktree: false };
        let linked = DirEntry { name: "shop-invoices".into(), git: true, worktree: true };
        assert_eq!(git_marker(&repo), ("⎇ git", GIT));
        assert_eq!(git_marker(&linked), ("⎇ worktree", WORKTREE));
    }

    #[test]
    fn breadcrumb_and_join() {
        let names: Vec<String> = breadcrumb("/home/me/code").into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["/", "home", "me", "code"]);
        assert_eq!(breadcrumb("/home/me/code")[2].1, "/home/me");
        assert_eq!(breadcrumb("/"), vec![("/".to_string(), "/".to_string())]);
        assert_eq!(join("/home/me/", "api"), "/home/me/api");
        assert_eq!(join("/", "home"), "/home");
    }

    #[test]
    fn opening_asks_the_agent_and_only_the_latest_answer_counts() {
        let (mut p, cmd) = FolderPicker::open("p".into(), "t".into(), false, "  ", vec![]);
        assert_eq!(cmd, AgentCmd::ListDir { path: "~".into() });
        assert_eq!(p.target().as_deref(), Some("~"), "no response: the typed path");
        p.navigate("/srv");
        p.on_listing(&listing("~", "/home/me", &[("old", false)]));
        assert!(p.listing.is_none(), "stale response ignored");
        p.on_listing(&listing("/srv", "/srv", &[("api", true)]));
        assert_eq!((p.input.as_str(), p.target().as_deref()), ("/srv", Some("/srv")));
    }

    #[test]
    fn filter_narrows_folders_ignoring_case() {
        let (mut p, _) = FolderPicker::open("p".into(), "t".into(), true, "~", vec![]);
        p.on_listing(&listing("~", "/home/me", &[("Amimoucheur-rust", true), ("blog", false), ("api", true)]));
        p.filter = "AMI".into();
        assert_eq!(p.visible().iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["Amimoucheur-rust"]);
        p.navigate("/home/me/api");
        assert!(p.filter.is_empty(), "changing folder clears the filter");
    }

    #[test]
    fn a_missing_folder_cannot_be_linked_as_listed() {
        let (mut p, _) = FolderPicker::open("p".into(), "t".into(), false, "/missing", vec![]);
        p.on_listing(&DirListing {
            request: "/missing".into(),
            path: None,
            parent: None,
            git: false,
            worktree: false,
            dirs: vec![],
            error: Some(protocol::Problem::DirNotFound { path: "/missing".into() }),
        });
        assert_eq!(p.target(), None);
    }
}
