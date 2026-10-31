//! Projets : cadres nommés sur le canevas. Une carte appartient au cadre qui contient son centre.

use egui::text::CCursor;
use egui::text_selection::CCursorRange;
use egui::{Align2, CornerRadius, FontId, Id, Key, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2, pos2, vec2};

use crate::shell::event::Event;
use crate::shell::theme;
use crate::shell::widgets::{confirm, text_prompt};
use crate::shell::world::{CardId, PROJECT_COLORS, Project, ProjectId, World, new_id};

/// Hauteur de l'étiquette au-dessus du cadre, en pixels écran (lisible à tout zoom).
const HEADER_H: f32 = 24.0;
const DEFAULT_SIZE: Vec2 = Vec2::new(1800.0, 1100.0);
const MIN_SIZE: Vec2 = Vec2::new(200.0, 120.0);
/// Largeur du bouton × au bout de l'étiquette.
const CLOSE_W: f32 = 24.0;

/// Renommage en cours, directement dans l'étiquette du cadre.
struct Editing {
    id: ProjectId,
    text: String,
    /// Première frame : donner le focus et tout sélectionner.
    fresh: bool,
}

#[derive(Default)]
pub struct Projects {
    /// Dossier des nouveaux terminaux, édité dans une boîte de saisie.
    cwd_dialog: Option<(ProjectId, String)>,
    editing: Option<Editing>,
    /// Fermeture du groupe demandée, en attente de confirmation.
    confirm_close: Option<ProjectId>,
}

/// Terminaux du groupe : ceux dont le centre est dans le cadre (sous-groupes compris).
pub fn cards_in(world: &World, id: &str) -> Vec<CardId> {
    let Some(rect) = world.project(id).map(|p| p.rect) else { return Vec::new() };
    world.cards.iter().filter(|c| rect.contains(c.rect.center())).map(|c| c.id.clone()).collect()
}

/// Texte de la confirmation : chaque terminal, et ce qui y tourne.
pub fn close_message(world: &World, id: &str) -> (String, String) {
    let name = world.project(id).map(|p| p.name.as_str()).unwrap_or("?");
    let cards = cards_in(world, id);
    let title = format!("Fermer le groupe « {name} » ?");
    if cards.is_empty() {
        return (title, "Le groupe ne contient aucun terminal. Le cadre sera supprimé.".into());
    }
    let mut body = match cards.len() {
        1 => "Ce terminal sera fermé avec tous ses processus :\n".to_string(),
        n => format!("Ces {n} terminaux seront fermés avec tous leurs processus :\n"),
    };
    for card in cards.iter().filter_map(|c| world.card(c)) {
        let running = card.info.as_ref().and_then(|i| i.command.as_deref()).map(|c| format!("  —  « {c} » en cours")).unwrap_or_default();
        body.push_str(&format!("  •  {}{running}\n", card.title()));
    }
    body.push_str("\nLe cadre sera supprimé. C'est définitif.");
    (title, body)
}

/// Centre d'un nouveau cadre posé à droite de tout le contenu existant.
fn free_area(world: &World) -> Pos2 {
    let all = world.cards.iter().map(|c| c.rect).chain(world.projects.iter().map(|p| p.rect)).reduce(|a, b| a.union(b));
    match all {
        Some(r) => pos2(r.max.x + 400.0 + DEFAULT_SIZE.x / 2.0, r.min.y + DEFAULT_SIZE.y / 2.0),
        None => world.camera.center,
    }
}

/// Déplace un cadre et toutes les cartes dont le centre est à l'intérieur.
pub fn move_project(world: &mut World, id: &str, delta: Vec2) {
    let Some(rect) = world.project(id).map(|p| p.rect) else { return };
    for card in world.cards.iter_mut().filter(|c| rect.contains(c.rect.center())) {
        card.rect = card.rect.translate(delta);
    }
    if let Some(p) = world.projects.iter_mut().find(|p| p.id == id) {
        p.rect = p.rect.translate(delta);
    }
    world.touch();
}

impl Projects {
    pub fn has_dialog(&self) -> bool {
        self.cwd_dialog.is_some() || self.editing.is_some() || self.confirm_close.is_some()
    }

    pub fn start_rename(&mut self, p: &Project) {
        self.editing = Some(Editing { id: p.id.clone(), text: p.name.clone(), fresh: true });
    }

    /// Cadres (sous les cartes) et leurs étiquettes.
    pub fn ui(&mut self, ui: &mut Ui, world: &mut World, vp: Rect, events: &mut Vec<Event>) {
        let painter = ui.painter_at(vp);
        let cam = world.camera;
        for i in 0..world.projects.len() {
            let p = &world.projects[i];
            if !world.shows_project(p) {
                continue;
            }
            let id = p.id.clone();
            let color = theme::rgb(p.color);
            let screen = cam.rect_to_screen(p.rect, vp);
            if !vp.intersects(screen.expand(HEADER_H)) {
                continue;
            }
            painter.rect_filled(screen, CornerRadius::same(10), color.gamma_multiply(0.07));
            painter.rect_stroke(screen, CornerRadius::same(10), Stroke::new(1.5, color.gamma_multiply(0.6)), StrokeKind::Inside);

            // Le numéro rappelle le raccourci Ctrl+Maj+<n>.
            let number = if i < 9 { format!("{}  ", i + 1) } else { String::new() };
            let label = match &p.cwd {
                Some(cwd) => format!("{number}{}   {}", p.name, cwd),
                None => format!("{number}{}", p.name),
            };
            let font = FontId::proportional(14.0);
            let width = ui.ctx().fonts_mut(|f| f.layout_no_wrap(label.clone(), font.clone(), theme::TEXT).size().x) + 20.0 + CLOSE_W;
            let header = Rect::from_min_size(pos2(screen.min.x, screen.min.y - HEADER_H - 4.0), vec2(width, HEADER_H));
            let eid = Id::unique(("project", &id));

            if self.editing.as_ref().is_some_and(|e| e.id == id) {
                let header = Rect::from_min_size(header.min, vec2(width.max(240.0), HEADER_H));
                painter.rect_filled(header, CornerRadius::same(6), color.gamma_multiply(0.85));
                painter.rect_stroke(header, CornerRadius::same(6), Stroke::new(1.5, theme::TEXT), StrokeKind::Outside);
                self.edit_name(ui, header, font, events);
                continue;
            }

            painter.rect_filled(header, CornerRadius::same(6), color.gamma_multiply(0.85));
            painter.text(header.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, label, font, theme::CARD_BG);

            let head = ui.interact(header, eid.with("head"), Sense::click_and_drag()).on_hover_text("Double-clic : renommer · glisser : déplacer avec ses terminaux");
            // Bouton × au bout de l'étiquette, enregistré après : il gagne le clic.
            let close_rect = Rect::from_min_size(pos2(header.max.x - CLOSE_W, header.min.y), vec2(CLOSE_W, HEADER_H));
            let close = ui.interact(close_rect, eid.with("close"), Sense::click()).on_hover_text("Fermer le groupe et tous ses terminaux");
            let a11y = format!("Fermer le groupe {}", world.projects[i].name);
            close.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &a11y));
            let hot = close.hovered();
            if hot {
                painter.rect_filled(close_rect.shrink(3.0), CornerRadius::same(4), egui::Color32::from_rgb(190, 60, 60));
            }
            let c = close_rect.center();
            let stroke = Stroke::new(1.5, if hot { egui::Color32::WHITE } else { theme::CARD_BG });
            painter.line_segment([c - Vec2::splat(4.0), c + Vec2::splat(4.0)], stroke);
            painter.line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], stroke);
            if close.clicked() {
                events.push(Event::RequestCloseProject(id.clone()));
            }
            if head.dragged_by(PointerButton::Primary) {
                move_project(world, &id, head.drag_delta() / cam.zoom);
            }
            if head.double_clicked() {
                self.start_rename(&world.projects[i]);
            }
            head.context_menu(|ui| self.menu(ui, &world.projects[i], events));

            let grip = Rect::from_min_size(screen.max - Vec2::splat(18.0), Vec2::splat(18.0));
            let grip_r = ui.interact(grip, eid.with("grip"), Sense::drag());
            if grip_r.hovered() || grip_r.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
            }
            if grip_r.dragged() {
                let r = &mut world.projects[i].rect;
                r.max += grip_r.drag_delta() / cam.zoom;
                r.max = r.max.max(r.min + MIN_SIZE);
                world.touch();
            }
        }
    }

    /// Champ de saisie posé sur l'étiquette. Entrée ou clic ailleurs : valide ; Échap : annule.
    fn edit_name(&mut self, ui: &mut Ui, header: Rect, font: FontId, events: &mut Vec<Event>) {
        let Some(editing) = &mut self.editing else { return };
        let text_id = Id::unique(("project-name", &editing.id));
        let edit = TextEdit::singleline(&mut editing.text).id(text_id).frame(egui::Frame::NONE).font(font).text_color(theme::CARD_BG).desired_width(header.width() - 20.0);
        let mut output = ui.scope_builder(egui::UiBuilder::new().max_rect(header.shrink2(vec2(10.0, 3.0))), |ui| edit.show(ui)).inner;
        if editing.fresh {
            editing.fresh = false;
            output.response.request_focus();
            let all = CCursorRange::two(CCursor::new(0), CCursor::new(editing.text.chars().count()));
            output.state.cursor.set_char_range(Some(all));
            output.state.store(ui.ctx(), text_id);
            return;
        }
        if output.response.lost_focus() {
            let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
            let editing = self.editing.take().expect("édition en cours");
            let name = editing.text.trim().to_string();
            if !cancelled && !name.is_empty() {
                events.push(Event::RenameProject(editing.id, name));
            }
        }
    }

    fn menu(&mut self, ui: &mut Ui, p: &Project, events: &mut Vec<Event>) {
        if ui.button("Nouveau terminal ici").clicked() {
            events.push(Event::NewCard { at: Some(p.rect.center()), project: Some(p.id.clone()) });
            ui.close();
        }
        if ui.button("Renommer (double-clic sur le titre)").clicked() {
            self.start_rename(p);
            ui.close();
        }
        if ui.button("Dossier des nouveaux terminaux…").clicked() {
            self.cwd_dialog = Some((p.id.clone(), p.cwd.clone().unwrap_or_default()));
            ui.close();
        }
        ui.menu_button("Couleur", |ui| {
            for c in PROJECT_COLORS {
                let (rect, resp) = ui.allocate_exact_size(vec2(60.0, 18.0), Sense::click());
                ui.painter().rect_filled(rect, CornerRadius::same(4), theme::rgb(c));
                if resp.clicked() {
                    events.push(Event::SetProjectColor(p.id.clone(), c));
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button("Fermer le groupe et ses terminaux…").clicked() {
            events.push(Event::RequestCloseProject(p.id.clone()));
            ui.close();
        }
        if ui.button("Supprimer le cadre (garde les terminaux)").clicked() {
            events.push(Event::RemoveProject(p.id.clone()));
            ui.close();
        }
    }

    pub fn dialogs(&mut self, ctx: &egui::Context, world: &World, events: &mut Vec<Event>) {
        if let Some(id) = self.confirm_close.clone() {
            if world.project(&id).is_none() {
                self.confirm_close = None;
            } else {
                let (title, body) = close_message(world, &id);
                match confirm(ctx, "close-project", &title, &body, "Tout fermer") {
                    Some(true) => {
                        events.extend(cards_in(world, &id).into_iter().map(Event::KillCard));
                        events.push(Event::RemoveProject(id));
                        self.confirm_close = None;
                    }
                    Some(false) => self.confirm_close = None,
                    None => {}
                }
            }
            return;
        }
        let Some((id, text)) = &mut self.cwd_dialog else { return };
        match text_prompt(ctx, "project-cwd", "Dossier WSL des nouveaux terminaux (ex. ~/code/api)", text) {
            Some(true) => {
                let value = Some(text.trim().to_string()).filter(|v| !v.is_empty());
                events.push(Event::SetProjectCwd(id.clone(), value));
                self.cwd_dialog = None;
            }
            Some(false) => self.cwd_dialog = None,
            None => {}
        }
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, out: &mut Vec<Event>) {
        match ev {
            Event::NewProject { at } => {
                // Depuis un espace de projet, le nouveau projet est un espace à part : on le pose
                // à l'écart de tout, puis on l'ouvre.
                let in_space = world.space.is_some();
                let center = match at {
                    Some(at) => *at,
                    None if in_space => free_area(world),
                    None => world.camera.center,
                };
                let n = world.projects.len();
                let p = Project {
                    id: new_id(),
                    name: format!("Projet {}", n + 1),
                    color: PROJECT_COLORS[n % PROJECT_COLORS.len()],
                    rect: Rect::from_center_size(center, DEFAULT_SIZE),
                    cwd: None,
                };
                self.start_rename(&p);
                if in_space {
                    out.push(Event::EnterSpace(Some(p.id.clone())));
                }
                world.projects.push(p);
                world.touch();
            }
            Event::RenameProject(id, name) => {
                if let Some(p) = world.projects.iter_mut().find(|p| &p.id == id)
                    && !name.is_empty() {
                        p.name = name.clone();
                    }
                world.touch();
            }
            Event::SetProjectCwd(id, cwd) => {
                if let Some(p) = world.projects.iter_mut().find(|p| &p.id == id) {
                    p.cwd = cwd.clone();
                }
                world.touch();
            }
            Event::SetProjectColor(id, color) => {
                if let Some(p) = world.projects.iter_mut().find(|p| &p.id == id) {
                    p.color = *color;
                }
                world.touch();
            }
            Event::RequestCloseProject(id) => {
                if world.project(id).is_some() {
                    self.confirm_close = Some(id.clone());
                }
            }
            Event::RemoveProject(id) => {
                world.projects.retain(|p| &p.id != id);
                world.touch();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::world::Card;

    #[test]
    fn moving_a_project_carries_its_cards_only() {
        let mut w = World::default();
        w.projects.push(Project { id: "p".into(), name: "p".into(), color: [0; 3], rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)), cwd: None });
        w.add_card(Card::new(Rect::from_min_size(pos2(100.0, 100.0), Vec2::splat(50.0)), None));
        w.add_card(Card::new(Rect::from_min_size(pos2(2000.0, 100.0), Vec2::splat(50.0)), None));
        move_project(&mut w, "p", vec2(500.0, -20.0));
        assert_eq!(w.projects[0].rect.min, pos2(500.0, -20.0));
        assert_eq!(w.cards[0].rect.min, pos2(600.0, 80.0));
        assert_eq!(w.cards[1].rect.min, pos2(2000.0, 100.0));
    }

    #[test]
    fn group_members_and_close_message() {
        let mut w = World::default();
        w.projects.push(Project { id: "p".into(), name: "API".into(), color: [0; 3], rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)), cwd: None });
        let mut a = Card::new(Rect::from_min_size(pos2(100.0, 100.0), Vec2::splat(50.0)), None);
        a.name = Some("serveur".into());
        a.info = Some(protocol::SessionInfo {
            session: a.session(),
            status: protocol::TermStatus::Running,
            command: Some("cargo run".into()),
            cwd: None,
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
        });
        let (ia, outside) = (a.id.clone(), Card::new(Rect::from_min_size(pos2(5000.0, 0.0), Vec2::splat(50.0)), None));
        w.add_card(a);
        w.add_card(outside);
        assert_eq!(cards_in(&w, "p"), vec![ia]);
        let (title, body) = close_message(&w, "p");
        assert_eq!(title, "Fermer le groupe « API » ?");
        assert!(body.contains("serveur  —  « cargo run » en cours"), "{body}");
        assert!(cards_in(&w, "absent").is_empty());
    }

    #[test]
    fn new_project_lifecycle() {
        let mut w = World::default();
        let mut p = Projects::default();
        p.handle(&Event::NewProject { at: Some(pos2(10.0, 10.0)) }, &mut w, &mut vec![]);
        assert_eq!(w.projects.len(), 1);
        assert!(p.has_dialog(), "on propose de nommer le projet tout de suite");
        let id = w.projects[0].id.clone();
        p.handle(&Event::RenameProject(id.clone(), "API".into()), &mut w, &mut vec![]);
        p.handle(&Event::SetProjectCwd(id.clone(), Some("~/api".into())), &mut w, &mut vec![]);
        assert_eq!((w.projects[0].name.as_str(), w.projects[0].cwd.as_deref()), ("API", Some("~/api")));
        p.handle(&Event::RemoveProject(id), &mut w, &mut vec![]);
        assert!(w.projects.is_empty());
    }

    #[test]
    fn a_project_created_from_a_space_opens_apart_from_the_rest() {
        let mut w = World::default();
        w.projects.push(Project { id: "api".into(), name: "API".into(), color: [0; 3], rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)), cwd: None });
        w.space = Some("api".into());
        let mut out = vec![];
        Projects::default().handle(&Event::NewProject { at: None }, &mut w, &mut out);
        let new = &w.projects[1];
        assert!(!new.rect.intersects(w.projects[0].rect));
        assert!(matches!(out.as_slice(), [Event::EnterSpace(Some(id))] if id == &new.id));
    }
}
