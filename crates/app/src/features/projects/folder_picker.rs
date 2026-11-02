//! Sélecteur de dossier (WSL) pour lier un groupe : l'agent liste les sous-dossiers, on navigue
//! à la souris (fil d'Ariane, dossiers récents, filtre) ou en tapant un chemin.

use egui::{Align, Color32, Id, Key, Layout, Modal, RichText, ScrollArea, TextEdit, Ui};
use protocol::{AgentCmd, DirEntry, DirListing};

use crate::shell::theme;
use crate::shell::world::ProjectId;

const GIT: Color32 = Color32::from_rgb(230, 150, 60);

/// Ce que l'utilisateur a décidé dans la frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    /// Aller dans ce dossier (demande sa liste à l'agent).
    Navigate(String),
    Link(String),
    Unlink,
    Cancel,
}

pub struct FolderPicker {
    pub project: ProjectId,
    pub title: String,
    /// Le groupe a déjà un dossier : on propose aussi de le délier.
    pub linked: bool,
    /// Champ de chemin, éditable (Entrée : y aller).
    pub input: String,
    /// Chemin demandé dont on attend la liste.
    pub pending: Option<String>,
    pub listing: Option<DirListing>,
    pub filter: String,
    /// Raccourcis : dossiers des terminaux du groupe, des autres groupes, dossier personnel.
    pub shortcuts: Vec<String>,
    /// Première frame : le filtre prend le clavier.
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
    /// Ouvre le sélecteur sur `start` (vide : dossier personnel) ; renvoie la commande à envoyer.
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

    /// Réponse de l'agent : on ne garde que celle du dernier chemin demandé.
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

    /// Sous-dossiers qui correspondent au filtre (sans tenir compte de la casse).
    pub fn visible(&self) -> Vec<&DirEntry> {
        let f = self.filter.trim().to_lowercase();
        self.listing.iter().flat_map(|l| &l.dirs).filter(|d| f.is_empty() || d.name.to_lowercase().contains(&f)).collect()
    }

    /// Dossier qui sera lié : celui affiché (chemin absolu résolu). Tant que l'agent n'a pas
    /// répondu (absent, lent), le chemin tapé. Rien si l'agent dit que le dossier n'existe pas.
    pub fn target(&self) -> Option<String> {
        match self.listing.as_ref().filter(|_| self.pending.is_none()) {
            Some(l) if l.error.is_some() => None,
            Some(l) => l.path.clone(),
            None => Some(self.input.trim().to_string()).filter(|s| !s.is_empty()),
        }
    }

    /// Fenêtre modale. Échap ou clic à côté : annuler.
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
            if ui.button("⌂").on_hover_text("Dossier personnel").clicked() {
                *choice = Some(Choice::Navigate("~".into()));
            }
            let parent = self.listing.as_ref().and_then(|l| l.parent.clone());
            if ui.add_enabled(parent.is_some(), egui::Button::new("⬆")).on_hover_text("Dossier parent").clicked()
                && let Some(p) = parent
            {
                *choice = Some(Choice::Navigate(p));
            }
            let edit = ui.add(TextEdit::singleline(&mut self.input).id(Id::unique("folder-picker-path")).desired_width(f32::INFINITY).hint_text("Chemin WSL, ex. ~/code/api"));
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
            ui.label(RichText::new("Récents :").color(theme::TEXT_DIM));
            for s in &self.shortcuts {
                let name = s.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(s);
                if ui.small_button(format!("📁 {name}")).on_hover_text(s).clicked() {
                    *choice = Some(Choice::Navigate(s.clone()));
                }
            }
        });
    }

    fn list(&mut self, ui: &mut Ui, choice: &mut Option<Choice>) {
        let filter = ui.add(TextEdit::singleline(&mut self.filter).id(Id::unique("folder-picker-filter")).hint_text("Filtrer les dossiers… (Entrée : ouvrir le seul résultat)").desired_width(f32::INFINITY));
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
                ui.label(RichText::new("Chargement…").color(theme::TEXT_DIM));
                return;
            }
            let Some(listing) = &self.listing else { return };
            if let Some(err) = &listing.error {
                ui.label(RichText::new(format!("⚠ {err}")).color(Color32::from_rgb(235, 95, 95)));
                return;
            }
            if visible.is_empty() {
                ui.label(RichText::new(if listing.dirs.is_empty() { "Aucun sous-dossier." } else { "Aucun dossier ne correspond." }).color(theme::TEXT_DIM));
            }
            for d in &visible {
                let resp = ui.add(egui::Button::new(format!("📁 {}", d.name)).frame(false).min_size(egui::vec2(ui.available_width(), 22.0)));
                if d.git {
                    ui.painter().text(resp.rect.right_center() - egui::vec2(8.0, 0.0), egui::Align2::RIGHT_CENTER, "⎇ git", egui::FontId::proportional(12.0), GIT);
                }
                if resp.clicked() {
                    *choice = Some(Choice::Navigate(join(&current, &d.name)));
                }
            }
        });
    }

    fn footer(&self, ui: &mut Ui, choice: &mut Option<Choice>) {
        let target = self.target();
        let git = self.listing.as_ref().is_some_and(|l| l.git && self.pending.is_none());
        ui.horizontal(|ui| {
            ui.label(RichText::new(target.as_deref().unwrap_or("—")).color(theme::TEXT));
            if git {
                ui.label(RichText::new("⎇ dépôt git").color(GIT));
            }
        });
        ui.horizontal(|ui| {
            let link = egui::Button::new(RichText::new("Lier ce dossier").color(theme::CARD_BG)).fill(theme::FOCUS);
            if ui.add_enabled(target.is_some(), link).clicked()
                && let Some(t) = target
            {
                *choice = Some(Choice::Link(t));
            }
            if ui.button("Annuler").clicked() {
                *choice = Some(Choice::Cancel);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.linked && ui.button("Délier").on_hover_text("Le groupe n'aura plus de dossier").clicked() {
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
            dirs: dirs.iter().map(|(n, g)| DirEntry { name: (*n).into(), git: *g }).collect(),
            error: None,
        }
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
        assert_eq!(p.target().as_deref(), Some("~"), "sans réponse : le chemin tapé");
        p.navigate("/srv");
        p.on_listing(&listing("~", "/home/me", &[("vieux", false)]));
        assert!(p.listing.is_none(), "réponse périmée ignorée");
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
        assert!(p.filter.is_empty(), "changer de dossier efface le filtre");
    }

    #[test]
    fn a_missing_folder_cannot_be_linked_as_listed() {
        let (mut p, _) = FolderPicker::open("p".into(), "t".into(), false, "/nexiste", vec![]);
        p.on_listing(&DirListing { request: "/nexiste".into(), path: None, parent: None, git: false, dirs: vec![], error: Some("dossier introuvable".into()) });
        assert_eq!(p.target(), None);
    }
}
