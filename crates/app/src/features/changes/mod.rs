//! Changements : fichiers modifiés dans le dossier lié au groupe ouvert, et leurs diffs, en direct.
//! L'agent fait le travail (git dans WSL) ; cette slice tient la liste des dossiers à surveiller
//! à jour, affiche le panneau latéral et demande les diffs.

use std::collections::{BTreeMap, HashMap};

use egui::{Align, Align2, Button, Color32, CornerRadius, FontId, Layout, Panel, RichText, ScrollArea, Sense, Ui, vec2};
use protocol::{AgentCmd, AgentEvent, ChangeKind, ChangeSet, FileChange};

use crate::shell::event::Event;
use crate::shell::theme;
use crate::shell::world::{Project, ProjectId, World, unix_now};

/// Un fichier touché depuis moins longtemps est surligné : on voit l'IA travailler.
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
    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff ") || line.starts_with("index ") || line.starts_with("new file") || line.starts_with("deleted file") || line.starts_with("similarity") || line.starts_with("rename ") {
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

/// Ce qui, s'il change, rend le diff affiché périmé.
type Fingerprint = (u64, Option<u32>, Option<u32>, ChangeKind);

fn fingerprint(f: &FileChange) -> Fingerprint {
    (f.mtime, f.added, f.removed, f.kind.clone())
}

/// Commandes à envoyer pour que l'agent surveille exactement les dossiers liés.
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

/// « à l'instant », « il y a 12 s », « il y a 3 min »…
pub fn age_label(now: u64, mtime: u64) -> String {
    if mtime == 0 {
        return String::new();
    }
    match now.saturating_sub(mtime) {
        0..=2 => "à l'instant".into(),
        s @ 3..=59 => format!("il y a {s} s"),
        s @ 60..=3599 => format!("il y a {} min", s / 60),
        s @ 3600..=86_399 => format!("il y a {} h", s / 3600),
        s => format!("il y a {} j", s / 86_400),
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
    file: String,
    /// `None` tant que l'agent n'a pas répondu.
    lines: Option<Vec<(LineKind, String)>>,
    truncated: bool,
    fingerprint: Option<Fingerprint>,
}

pub struct Changes {
    /// Dossiers dont la surveillance a été demandée à l'agent (clé : id du projet).
    watched: BTreeMap<ProjectId, String>,
    sets: HashMap<ProjectId, ChangeSet>,
    /// Fichier sélectionné par groupe.
    selected: HashMap<ProjectId, String>,
    diff: Option<DiffView>,
    /// Sélectionner automatiquement le dernier fichier modifié.
    pub follow: bool,
}

impl Default for Changes {
    fn default() -> Self {
        Self { watched: BTreeMap::new(), sets: HashMap::new(), selected: HashMap::new(), diff: None, follow: true }
    }
}

impl Changes {
    pub fn set(&self, project: &str) -> Option<&ChangeSet> {
        self.sets.get(project)
    }

    pub fn selected(&self, project: &str) -> Option<&str> {
        self.selected.get(project).map(String::as_str)
    }

    /// Lignes du diff affiché, s'il est arrivé (tests).
    pub fn diff_lines(&self) -> Option<Vec<(LineKind, String)>> {
        self.diff.as_ref().and_then(|d| d.lines.clone())
    }

    /// Le panneau a de quoi s'afficher : un groupe ouvert, lié à un dossier.
    fn panel_project<'a>(&self, world: &'a World) -> Option<&'a Project> {
        world.space.as_deref().and_then(|id| world.project(id)).filter(|p| p.cwd.is_some())
    }

    /// À chaque frame : aligne les surveillances de l'agent sur les dossiers liés.
    pub fn poll(&mut self, world: &World, events: &mut Vec<Event>) {
        events.extend(reconcile(&mut self.watched, &world.projects).into_iter().map(Event::ToAgent));
    }

    fn select(&mut self, key: &str, file: &str, out: &mut Vec<Event>) {
        self.selected.insert(key.to_string(), file.to_string());
        let fp = self.sets.get(key).and_then(|s| s.files.iter().find(|f| f.path == file)).map(fingerprint);
        self.diff = Some(DiffView { key: key.to_string(), file: file.to_string(), lines: None, truncated: false, fingerprint: fp });
        out.push(Event::ToAgent(AgentCmd::Diff { key: key.to_string(), file: file.to_string() }));
    }

    /// Nouvel état d'un dossier : suivre le dernier fichier touché, rafraîchir le diff affiché.
    fn on_changes(&mut self, set: ChangeSet, world: &mut World, out: &mut Vec<Event>) {
        let key = set.key.clone();
        let newest = set.files.iter().max_by_key(|f| f.mtime).cloned();
        if let Some(n) = &newest {
            world.file_activity.insert(key.clone(), n.mtime);
        }
        self.sets.insert(key.clone(), set);
        let set = &self.sets[&key];
        let current = self.selected.get(&key).cloned();
        let target = match (&current, &newest) {
            (_, Some(n)) if self.follow => Some(n.path.clone()),
            (Some(c), _) if set.files.iter().any(|f| &f.path == c) => Some(c.clone()),
            _ => None,
        };
        let Some(file) = target else {
            self.selected.remove(&key);
            if self.diff.as_ref().is_some_and(|d| d.key == key) {
                self.diff = None;
            }
            return;
        };
        let fp = set.files.iter().find(|f| f.path == file).map(fingerprint);
        let shown = self.diff.as_ref().is_some_and(|d| d.key == key && d.file == file && d.fingerprint == fp);
        // Seul le groupe ouvert affiche un diff : inutile de le demander pour les autres.
        if !shown && world.space.as_ref() == Some(&key) {
            self.select(&key, &file, out);
        } else {
            self.selected.insert(key, file);
        }
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, out: &mut Vec<Event>) {
        match ev {
            // Agent (re)lancé : il ne surveille plus rien, on renvoie tout à la prochaine frame.
            Event::Agent(AgentEvent::Hello { .. }) => self.watched.clear(),
            Event::ToggleChanges => {
                world.changes_open = !world.changes_open;
                world.touch();
            }
            Event::Agent(AgentEvent::Changes(set)) => {
                if world.project(&set.key).is_some_and(|p| p.cwd.is_some()) {
                    self.on_changes(set.clone(), world, out);
                }
            }
            Event::Agent(AgentEvent::Diff { key, file, text, truncated }) => {
                if let Some(d) = self.diff.as_mut().filter(|d| &d.key == key && &d.file == file) {
                    d.lines = Some(text.lines().map(|l| (classify(l), l.to_string())).collect());
                    d.truncated = *truncated;
                }
            }
            Event::SetProjectCwd(id, _) | Event::RemoveProject(id) => {
                self.sets.remove(id);
                self.selected.remove(id);
                world.file_activity.remove(id);
                if self.diff.as_ref().is_some_and(|d| &d.key == id) {
                    self.diff = None;
                }
            }
            // En ouvrant un groupe, on affiche le diff du fichier sélectionné.
            Event::EnterSpace(Some(id)) => {
                if let Some(file) = self.selected.get(id).cloned()
                    && self.diff.as_ref().is_none_or(|d| &d.key != id || d.file != file)
                {
                    self.select(id, &file, out);
                }
            }
            _ => {}
        }
    }

    /// Bouton de la barre du haut : ouvre ou ferme le panneau.
    pub fn toolbar_ui(&mut self, ui: &mut Ui, world: &World, events: &mut Vec<Event>) {
        let Some(p) = self.panel_project(world) else { return };
        let n = self.sets.get(&p.id).map(|s| s.files.len()).unwrap_or(0);
        let on = world.changes_open;
        let text = RichText::new(format!("Δ Changements ({n})")).color(if on { theme::CARD_BG } else { theme::TEXT });
        let button = Button::selectable(on, text).fill(if on { theme::TEXT } else { Color32::TRANSPARENT });
        if ui.add(button).on_hover_text("Fichiers modifiés dans le dossier lié et leurs diffs, en direct").clicked() {
            events.push(Event::ToggleChanges);
        }
    }

    /// Panneau latéral. À appeler avant le canevas (il lui prend sa place).
    pub fn panel_ui(&mut self, ui: &mut Ui, world: &World, events: &mut Vec<Event>) {
        if !world.changes_open {
            return;
        }
        let Some(project) = self.panel_project(world).cloned() else { return };
        Panel::right("changes").resizable(true).default_size(440.0).min_size(260.0).show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Δ Changements").strong().color(theme::TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("×").on_hover_text("Fermer le panneau").clicked() {
                        events.push(Event::ToggleChanges);
                    }
                    let follow = ui.checkbox(&mut self.follow, "Suivre").on_hover_text("Afficher automatiquement le dernier fichier modifié");
                    if follow.changed() && self.follow
                        && let Some(n) = self.sets.get(&project.id).and_then(|s| s.files.iter().max_by_key(|f| f.mtime)).map(|f| f.path.clone())
                    {
                        self.select(&project.id, &n, events);
                    }
                });
            });
            let Some(set) = self.sets.get(&project.id).cloned() else {
                ui.label(RichText::new(format!("📁 {}", project.cwd.as_deref().unwrap_or_default())).color(theme::TEXT_DIM));
                ui.label(RichText::new("En attente de l'agent…").color(theme::TEXT_DIM));
                return;
            };
            self.header(ui, &project, &set);
            ui.separator();
            let now = unix_now();
            let list_h = (ui.available_height() * 0.38).max(80.0);
            ScrollArea::vertical().id_salt("changes-list").max_height(list_h).auto_shrink([false, true]).show(ui, |ui| {
                if set.files.is_empty() && set.error.is_none() {
                    ui.label(RichText::new(if set.git { "Aucun changement par rapport au dernier commit." } else { "Aucun fichier modifié depuis le début du suivi." }).color(theme::TEXT_DIM));
                }
                for f in &set.files {
                    if self.file_row(ui, f, now, self.selected(&project.id) == Some(f.path.as_str())) {
                        self.follow = false;
                        self.select(&project.id, &f.path, events);
                    }
                }
                if set.truncated {
                    ui.label(RichText::new("… liste coupée (trop de fichiers)").color(theme::TEXT_DIM));
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
            ui.label(RichText::new(format!("⚠ {err}")).color(DEL));
            return;
        }
        ui.horizontal(|ui| {
            if let Some(b) = &set.branch {
                ui.label(RichText::new(format!("⎇ {b}")).color(theme::TEXT));
            }
            let (a, r) = set.files.iter().fold((0, 0), |(a, r), f| (a + f.added.unwrap_or(0), r + f.removed.unwrap_or(0)));
            let n = set.files.len();
            ui.label(RichText::new(format!("{n} fichier{}", if n > 1 { "s" } else { "" })).color(theme::TEXT_DIM));
            if set.git {
                ui.label(RichText::new(format!("+{a}")).color(ADD));
                ui.label(RichText::new(format!("−{r}")).color(DEL));
            }
        });
        if !set.git {
            ui.label(RichText::new("Pas un dépôt git : initialisez git dans ce dossier pour voir les diffs.").color(theme::TEXT_DIM));
        }
    }

    /// Une ligne de la liste. Renvoie `true` si on a cliqué dessus.
    fn file_row(&self, ui: &mut Ui, f: &FileChange, now: u64, selected: bool) -> bool {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
        let resp = resp.on_hover_text(match &f.kind {
            ChangeKind::Renamed(from) => format!("{from} → {}", f.path),
            _ => f.path.clone(),
        });
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Fichier modifié {}", f.path)));
        let painter = ui.painter_at(rect);
        let age = now.saturating_sub(f.mtime);
        if selected {
            painter.rect_filled(rect, CornerRadius::same(4), theme::FOCUS.gamma_multiply(0.25));
        } else if f.mtime > 0 && age < RECENT_SECS {
            // Surlignage qui s'éteint en quelques secondes.
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
        // À droite : âge puis +/−, mesurés pour laisser la place au nom.
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

    fn diff_ui(&self, ui: &mut Ui, key: &str, git: bool) {
        let Some(d) = self.diff.as_ref().filter(|d| d.key == key) else {
            if git {
                ui.label(RichText::new("Cliquez un fichier pour voir son diff.").color(theme::TEXT_DIM));
            }
            return;
        };
        ui.label(RichText::new(&d.file).strong().color(theme::TEXT));
        let Some(lines) = &d.lines else {
            ui.label(RichText::new("Chargement du diff…").color(theme::TEXT_DIM));
            return;
        };
        let mono = FontId::monospace(12.0);
        let row_h = ui.fonts_mut(|f| f.row_height(&mono));
        ScrollArea::both().id_salt("changes-diff").auto_shrink([false, false]).show_rows(ui, row_h, lines.len() + usize::from(d.truncated), |ui, range| {
            for i in range {
                let Some((kind, text)) = lines.get(i) else {
                    ui.label(RichText::new("… diff coupé (fichier trop gros)").color(theme::TEXT_DIM));
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
        ChangeSet { key: key.into(), root: Some("/r".into()), git: true, branch: Some("main".into()), error: None, files, truncated: false }
    }

    fn world() -> World {
        let mut w = World::default();
        w.projects.push(project("api", Some("~/api")));
        w.space = Some("api".into());
        w
    }

    fn diff_requests(out: &[Event]) -> Vec<String> {
        out.iter().filter_map(|e| match e {
            Event::ToAgent(AgentCmd::Diff { file, .. }) => Some(file.clone()),
            _ => None,
        }).collect()
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
        assert!(reconcile(&mut watched, &projects).is_empty(), "déjà surveillé");
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
        c.handle(&Event::Agent(AgentEvent::Hello { version: "x".into(), hooks_installed: true }), &mut world(), &mut vec![]);
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
        // Même état : rien à redemander.
        let mut out = vec![];
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 10, 1), file("b.rs", 20, 1)]))), &mut w, &mut out);
        assert!(diff_requests(&out).is_empty());
        // a.rs vient d'être touché : on le suit.
        let mut out = vec![];
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 30, 2), file("b.rs", 20, 1)]))), &mut w, &mut out);
        assert_eq!(diff_requests(&out), vec!["a.rs"]);
        c.handle(&Event::Agent(AgentEvent::Diff { key: "api".into(), file: "a.rs".into(), text: "@@ x\n+a\n".into(), truncated: false }), &mut w, &mut vec![]);
        assert_eq!(c.diff_lines().unwrap(), vec![(LineKind::Hunk, "@@ x".into()), (LineKind::Add, "+a".into())]);
    }

    #[test]
    fn without_follow_the_selection_stays_and_refreshes_when_its_file_changes() {
        let mut c = Changes { follow: false, ..Default::default() };
        let mut w = world();
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 10, 1), file("b.rs", 20, 1)]))), &mut w, &mut vec![]);
        assert_eq!(c.selected("api"), None, "rien d'imposé sans « Suivre »");
        let mut out = vec![];
        c.select("api", "a.rs", &mut out);
        let mut out = vec![];
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 10, 1), file("b.rs", 40, 3)]))), &mut w, &mut out);
        assert!(diff_requests(&out).is_empty(), "b.rs a changé, pas a.rs");
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 50, 4), file("b.rs", 40, 3)]))), &mut w, &mut out);
        assert_eq!(diff_requests(&out), vec!["a.rs"]);
        // a.rs commité : il disparaît de la liste, la sélection aussi.
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("b.rs", 40, 3)]))), &mut w, &mut vec![]);
        assert_eq!(c.selected("api"), None);
        assert!(c.diff_lines().is_none());
    }

    #[test]
    fn stale_answers_are_ignored() {
        let mut c = Changes::default();
        let mut w = world();
        c.handle(&Event::Agent(AgentEvent::Changes(set("inconnu", vec![file("a.rs", 1, 1)]))), &mut w, &mut vec![]);
        assert!(c.set("inconnu").is_none());
        c.handle(&Event::Agent(AgentEvent::Diff { key: "api".into(), file: "z.rs".into(), text: "+x".into(), truncated: false }), &mut w, &mut vec![]);
        assert!(c.diff_lines().is_none());
        c.handle(&Event::Agent(AgentEvent::Changes(set("api", vec![file("a.rs", 1, 1)]))), &mut w, &mut vec![]);
        c.handle(&Event::SetProjectCwd("api".into(), Some("~/autre".into())), &mut w, &mut vec![]);
        assert!(c.set("api").is_none() && c.selected("api").is_none() && !w.file_activity.contains_key("api"));
    }

    #[test]
    fn labels() {
        assert_eq!(age_label(100, 99), "à l'instant");
        assert_eq!(age_label(100, 88), "il y a 12 s");
        assert_eq!(age_label(1000, 100), "il y a 15 min");
        assert_eq!(age_label(100, 0), "");
        assert_eq!(split_path("src/a/b.rs"), ("src/a/", "b.rs"));
        assert_eq!(split_path("b.rs"), ("", "b.rs"));
    }
}
