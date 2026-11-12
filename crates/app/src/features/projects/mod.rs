//! Projects (groups): named frames on the canvas. A card explicitly belongs to a group
//! (`Card::project`) and the frame fits its members. A group can be linked to a folder.

use egui::text::CCursor;
use egui::text_selection::CCursorRange;
use egui::{Align2, CornerRadius, FontId, Id, Key, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2, pos2, vec2};

mod folder_picker;

use crate::shell::event::{Event, Notice};
use crate::shell::i18n::t;
use crate::shell::theme::{self, HEADER_H, LINKED_ROW_GAP, LINKED_ROW_H, PINNED_MARGIN};
use crate::shell::widgets::confirm;
use crate::shell::world::{Body, CardId, CommandTemplate, EMPTY_PROJECT, PROJECT_COLORS, Project, ProjectId, World, new_id};
use folder_picker::{Choice, FolderPicker};

/// Width of the × button at the end of the label.
const CLOSE_W: f32 = 24.0;
/// Width of the arrow next to "+ Terminal" (its menu of command templates).
const MORE_W: f32 = 20.0;

/// Templates being edited for a group, in a dialog.
struct TemplateEditor {
    project: ProjectId,
    rows: Vec<CommandTemplate>,
    /// The group's templates when the editor opened: anything else is unsaved.
    saved: Vec<CommandTemplate>,
    /// Escape with unsaved changes: asking whether to discard them.
    confirm_discard: bool,
}

/// Longest template name shown whole in the menu.
const MENU_NAME_CHARS: usize = 32;

/// Templates as saved from the editor: rows without a command are dropped, a row without a name
/// is named after the first line of its command, and a name already taken gets a number (` (2)`).
/// The command itself is kept as typed.
pub fn clean_templates(rows: &[CommandTemplate]) -> Vec<CommandTemplate> {
    let mut out: Vec<CommandTemplate> = Vec::new();
    for r in rows.iter().filter(|r| !r.command.trim().is_empty()) {
        let base: String = match r.name.trim() {
            "" => r.command.trim().lines().next().unwrap_or_default().chars().take(40).collect(),
            name => name.to_string(),
        };
        let taken = |n: &str| out.iter().any(|t| t.name == n);
        let name = (1..).map(|k| if k == 1 { base.clone() } else { format!("{base} ({k})") }).find(|n| !taken(n)).expect("a free name");
        out.push(CommandTemplate { name, command: r.command.clone() });
    }
    out
}

/// A template's name in the menu: cut at `MENU_NAME_CHARS`, with an ellipsis.
pub fn menu_name(name: &str) -> String {
    if name.chars().count() <= MENU_NAME_CHARS {
        return name.to_string();
    }
    format!("{}…", name.chars().take(MENU_NAME_CHARS - 1).collect::<String>())
}

/// Rename in progress, directly in the frame's label.
struct Editing {
    id: ProjectId,
    text: String,
    /// First frame: give focus and select everything.
    fresh: bool,
}

#[derive(Default)]
pub struct Projects {
    /// Linked-folder picker, open for a group.
    picker: Option<FolderPicker>,
    editing: Option<Editing>,
    /// Group close requested, awaiting confirmation.
    confirm_close: Option<ProjectId>,
    /// Alt + drag in progress from this group's label: pulling a wire to another group.
    wire_from: Option<ProjectId>,
    /// Command templates being edited.
    templates: Option<TemplateEditor>,
}

/// Group (other than `from`) whose frame or label is under `at` (world units): the end of a
/// wire pulled from `from`. `label_h` is the label height in world units.
pub fn link_target(world: &World, from: &str, at: Pos2, label_h: f32) -> Option<ProjectId> {
    world.projects.iter().filter(|p| p.id != from && world.shows_project(p)).find(|p| Rect::from_min_max(p.rect.min - vec2(0.0, label_h), p.rect.max).contains(at)).map(|p| p.id.clone())
}

/// Member terminals of the group.
pub fn cards_in(world: &World, id: &str) -> Vec<CardId> {
    world.members(id).map(|c| c.id.clone()).collect()
}

/// Confirmation text: each terminal, and what is running in it.
pub fn close_message(world: &World, id: &str) -> (String, String) {
    let name = world.project(id).map(|p| p.name.as_str()).unwrap_or("?");
    let cards = cards_in(world, id);
    let title = (t().close_group_title)(name);
    if cards.is_empty() {
        return (title, t().close_empty_group.into());
    }
    let mut body = match cards.len() {
        1 => t().close_one_terminal.to_string(),
        n => (t().close_n_terminals)(n),
    };
    for card in cards.iter().filter_map(|c| world.card(c)) {
        let running = card.info.as_ref().and_then(|i| i.command.as_deref()).map(t().running_command).unwrap_or_default();
        body.push_str(&format!("  •  {}{running}\n", card.title()));
    }
    body.push_str(t().close_group_footer);
    (title, body)
}

/// Where to place a card joining a group via the menu: right of its members, or in the center
/// of the frame if it is empty.
pub fn arrival_spot(world: &World, project: &str, card: &str) -> Option<Rect> {
    let p = world.project(project)?;
    let size = world.card(card)?.rect.size();
    let members = world.members(project).filter(|c| c.id != card).map(|c| c.rect).reduce(|a, b| a.union(b));
    let near = match members {
        Some(r) => pos2(r.max.x + 40.0 + size.x / 2.0, r.min.y + size.y / 2.0),
        None => p.rect.center(),
    };
    Some(world.free_spot(near, size))
}

/// Linked-folder label, shortened from the left to keep the end of the path.
pub fn folder_label(cwd: Option<&str>) -> String {
    match cwd {
        None => t().link_a_folder.into(),
        Some(c) if c.chars().count() > 40 => format!("📁 …{}", c.chars().skip(c.chars().count() - 39).collect::<String>()),
        Some(c) => format!("📁 {c}"),
    }
}

/// Value suggested in the "Linked folder" box: the current folder, otherwise that of a terminal
/// of the group (the focused one first) — the common case: "I'm already in the repo, link it".
pub fn suggested_folder(world: &World, id: &str) -> String {
    if let Some(cwd) = world.project(id).and_then(|p| p.cwd.clone()) {
        return cwd;
    }
    let focused = world.focused.as_deref().and_then(|f| world.card(f)).filter(|c| c.project.as_deref() == Some(id));
    focused.into_iter().chain(world.members(id)).find_map(|c| c.current_cwd().map(str::to_string)).unwrap_or_default()
}

/// Picker shortcuts: folders of the group's terminals, then linked folders of other groups.
pub fn shortcuts(world: &World, id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let members = world.members(id).filter_map(|c| c.current_cwd().map(str::to_string));
    let others = world.projects.iter().filter(|p| p.id != id).filter_map(|p| p.cwd.clone());
    for path in members.chain(others) {
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out.truncate(8);
    out
}

/// Center of a new frame placed to the right of all existing content.
fn free_area(world: &World) -> Pos2 {
    let all = world.cards.iter().map(|c| c.rect).chain(world.projects.iter().map(|p| p.rect)).reduce(|a, b| a.union(b));
    match all {
        Some(r) => pos2(r.max.x + 400.0 + EMPTY_PROJECT.x / 2.0, r.min.y + EMPTY_PROJECT.y / 2.0),
        None => world.camera.center,
    }
}

/// Moves a frame and all its members.
pub fn move_project(world: &mut World, id: &str, delta: Vec2) {
    for card in world.cards.iter_mut().filter(|c| c.project.as_deref() == Some(id)) {
        card.rect = card.rect.translate(delta);
    }
    if let Some(p) = world.projects.iter_mut().find(|p| p.id == id) {
        p.rect = p.rect.translate(delta);
    }
    world.touch();
}

impl Projects {
    pub fn has_dialog(&self) -> bool {
        self.picker.is_some() || self.editing.is_some() || self.confirm_close.is_some() || self.templates.is_some()
    }

    /// Opens the editor of a group's command templates.
    pub fn edit_templates(&mut self, p: &Project) {
        let mut rows = p.templates.clone();
        if rows.is_empty() {
            rows.push(CommandTemplate::default());
        }
        self.templates = Some(TemplateEditor { project: p.id.clone(), rows, saved: p.templates.clone(), confirm_discard: false });
    }

    /// Menu of the arrow next to "+ Terminal": a plain shell, each template, then the editor.
    fn new_terminal_menu(&mut self, ui: &mut Ui, p: &Project, events: &mut Vec<Event>) {
        if ui.button(t().plain_shell).clicked() {
            events.push(Event::NewCard { at: None, project: Some(p.id.clone()), command: None });
            ui.close();
        }
        ui.separator();
        if p.templates.is_empty() {
            ui.label(egui::RichText::new(t().no_templates).color(theme::TEXT_DIM));
        }
        for tpl in &p.templates {
            let hover = egui::RichText::new(format!("{}\n\n{}", tpl.name, tpl.command)).monospace();
            if ui.button(menu_name(&tpl.name)).on_hover_text(hover).clicked() {
                events.push(Event::NewCard { at: None, project: Some(p.id.clone()), command: Some(tpl.command.clone()) });
                ui.close();
            }
        }
        ui.separator();
        if ui.button(t().edit_templates).clicked() {
            self.edit_templates(p);
            ui.close();
        }
    }

    /// The template editor: one name and one command per row. `Some(templates)` when saved.
    fn templates_dialog(&mut self, ctx: &egui::Context, world: &World) -> Option<Vec<CommandTemplate>> {
        let editor = self.templates.as_mut()?;
        let name = world.project(&editor.project).map(|p| p.name.clone()).unwrap_or_default();
        let mut result = None;
        egui::Modal::new(Id::unique("command-templates")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.label(egui::RichText::new((t().templates_title)(&name)).strong().size(16.0));
            ui.add_space(2.0);
            ui.label(egui::RichText::new(t().templates_help).color(theme::TEXT_DIM));
            ui.add_space(6.0);
            let mut remove = None;
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() * 0.6).max(160.0)).show(ui, |ui| {
                for (k, row) in editor.rows.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(t().template_name);
                        ui.add(TextEdit::singleline(&mut row.name).hint_text(t().template_name_hint).desired_width(180.0).id(Id::unique(("template-name", k))));
                        if ui.button(t().remove_template).clicked() {
                            remove = Some(k);
                        }
                    });
                    ui.label(t().template_command);
                    let command = TextEdit::multiline(&mut row.command).hint_text(t().template_command_hint).code_editor().desired_rows(2).desired_width(f32::INFINITY);
                    ui.add(command.id(Id::unique(("template-command", k))));
                    ui.separator();
                }
            });
            if let Some(k) = remove {
                editor.rows.remove(k);
            }
            if ui.button(t().add_template).clicked() {
                editor.rows.push(CommandTemplate::default());
            }
            ui.add_space(8.0);
            if editor.confirm_discard {
                ui.label(egui::RichText::new(t().discard_template_changes).strong());
                ui.horizontal(|ui| {
                    if ui.button(t().discard).clicked() {
                        result = Some(false);
                    }
                    if ui.button(t().keep_editing).clicked() {
                        editor.confirm_discard = false;
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    if ui.button(t().save).clicked() {
                        result = Some(true);
                    }
                    if ui.button(t().cancel).clicked() {
                        result = Some(false);
                    }
                });
            }
        });
        // A click outside never closes it (a long prompt would be lost); Escape closes it, after
        // asking when there are unsaved changes (a second Escape keeps editing).
        if result.is_none() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            if editor.confirm_discard {
                editor.confirm_discard = false;
            } else if clean_templates(&editor.rows) != editor.saved {
                editor.confirm_discard = true;
            } else {
                result = Some(false);
            }
        }
        match result {
            Some(true) => self.templates.take().map(|e| clean_templates(&e.rows)),
            Some(false) => {
                self.templates = None;
                None
            }
            None => None,
        }
    }

    pub fn start_rename(&mut self, p: &Project) {
        self.editing = Some(Editing { id: p.id.clone(), text: p.name.clone(), fresh: true });
    }

    /// Frames (below the cards) and their labels. With the free grid, the open group has no
    /// frame and its label is a bar pinned at the top left of the canvas, above the cards.
    pub fn ui(&mut self, ui: &mut Ui, world: &mut World, vp: Rect, events: &mut Vec<Event>) {
        if world.free_grid_active() {
            let Some(i) = world.projects.iter().position(|p| world.space.as_ref() == Some(&p.id)) else { return };
            let id = Id::unique(("pinned-group", &world.projects[i].id));
            // Above the cards (canvas layer), below menus and the palette.
            egui::Area::new(id).order(egui::Order::Middle).fixed_pos(vp.min).show(ui.ctx(), |ui| {
                ui.set_clip_rect(vp);
                let bar = self.label(ui, world, i, vp.min + Vec2::splat(PINNED_MARGIN), vp, vp, true, events);
                // The area must cover the bar for the pointer to reach it (no widget added).
                ui.expand_to_include_rect(bar);
            });
            return;
        }
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
            if !vp.intersects(screen.expand(HEADER_H + LINKED_ROW_H + LINKED_ROW_GAP)) {
                continue;
            }
            painter.rect_filled(screen, CornerRadius::same(10), color.gamma_multiply(0.07));
            painter.rect_stroke(screen, CornerRadius::same(10), Stroke::new(1.5, color.gamma_multiply(0.6)), StrokeKind::Inside);
            self.drag_feedback(ui, world, &id, screen, color);
            // Open group with links: its label rises to leave room for the row of linked
            // groups (see the `wires` slice) between the label and the frame.
            let lift = if world.space.as_ref() == Some(&id) && !world.linked_groups(&id).is_empty() { LINKED_ROW_H + LINKED_ROW_GAP } else { 0.0 };
            self.label(ui, world, i, pos2(screen.min.x, screen.min.y - HEADER_H - 4.0 - lift), screen, vp, false, events);
        }
        self.wire_drag(ui, world, vp, events);
    }

    /// A group's label (name, folder, new terminal, changes, ×) from `header_min`, and the
    /// empty-group button in the middle of `body`, clipped to `clip`. `pinned`: fixed bar of the
    /// free grid. Returns the screen area covered by the label.
    #[allow(clippy::too_many_arguments)]
    fn label(&mut self, ui: &mut Ui, world: &mut World, i: usize, header_min: Pos2, body: Rect, clip: Rect, pinned: bool, events: &mut Vec<Event>) -> Rect {
        let painter = ui.painter_at(clip);
        let p = &world.projects[i];
        let id = p.id.clone();
        let color = theme::rgb(p.color);

        // The number recalls the Ctrl+Shift+<n> shortcut.
        let number = if i < 9 { format!("{}  ", i + 1) } else { String::new() };
        let label = format!("{number}{}", p.name);
        let folder = folder_label(p.cwd.as_deref());
        let font = FontId::proportional(14.0);
        let measure = |text: &str, font: &FontId| ui.ctx().fonts_mut(|f| f.layout_no_wrap(text.to_string(), font.clone(), theme::TEXT).size().x);
        let width = measure(&label, &font) + 20.0 + CLOSE_W;
        let header = Rect::from_min_size(header_min, vec2(width, HEADER_H));
        let folder_font = FontId::proportional(12.0);
        let folder_rect = Rect::from_min_size(pos2(header.max.x + 6.0, header.min.y), vec2(measure(&folder, &folder_font) + 16.0, HEADER_H));
        let add_label = t().new_terminal_button;
        let add_rect = Rect::from_min_size(pos2(folder_rect.max.x + 6.0, header.min.y), vec2(measure(add_label, &folder_font) + 16.0, HEADER_H));
        let eid = Id::unique(("project", &id));

        if self.editing.as_ref().is_some_and(|e| e.id == id) {
            let header = Rect::from_min_size(header.min, vec2(width.max(240.0), HEADER_H));
            painter.rect_filled(header, CornerRadius::same(6), color.gamma_multiply(0.85));
            painter.rect_stroke(header, CornerRadius::same(6), Stroke::new(1.5, theme::TEXT), StrokeKind::Outside);
            self.edit_name(ui, header, font, events);
            return header;
        }

        painter.rect_filled(header, CornerRadius::same(6), color.gamma_multiply(0.85));
        painter.text(header.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, label, font, theme::CARD_BG);

        // Linked folder, right of the label: click = change folder.
        let folder_r = ui.interact(folder_rect, eid.with("folder"), Sense::click()).on_hover_text(match &p.cwd {
            Some(_) => t().folder_linked_hint,
            None => t().folder_unlinked_hint,
        });
        let a11y = (t().group_folder_a11y)(&p.name);
        folder_r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &a11y));
        let folder_bg = if folder_r.hovered() { color.gamma_multiply(0.5) } else { color.gamma_multiply(0.22) };
        painter.rect_filled(folder_rect, CornerRadius::same(6), folder_bg);
        let folder_color = if p.cwd.is_some() { theme::TEXT } else { theme::TEXT_DIM };
        painter.text(folder_rect.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, folder, folder_font.clone(), folder_color);
        if folder_r.clicked() {
            self.open_picker(world, &id, events);
        }

        // New terminal in this group (in its linked folder), placed next to the others.
        let add_r = ui.interact(add_rect, eid.with("add"), Sense::click()).on_hover_text(t().group_new_terminal_hint);
        let a11y = (t().group_new_terminal_a11y)(&p.name);
        add_r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &a11y));
        painter.rect_filled(add_rect, CornerRadius::same(6), if add_r.hovered() { color.gamma_multiply(0.5) } else { color.gamma_multiply(0.22) });
        painter.text(add_rect.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, add_label, folder_font.clone(), theme::TEXT);
        if add_r.clicked() {
            events.push(Event::NewCard { at: None, project: Some(id.clone()), command: None });
        }
        // Its arrow: the same, or a terminal started from one of the group's command templates.
        let more_rect = Rect::from_min_size(pos2(add_rect.max.x + 2.0, header.min.y), vec2(MORE_W, HEADER_H));
        let more_r = ui.interact(more_rect, eid.with("more"), Sense::click()).on_hover_text(t().new_terminal_menu_hint);
        let a11y = (t().new_terminal_menu_a11y)(&p.name);
        more_r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &a11y));
        painter.rect_filled(more_rect, CornerRadius::same(6), if more_r.hovered() { color.gamma_multiply(0.5) } else { color.gamma_multiply(0.22) });
        let c = more_rect.center();
        painter.add(egui::Shape::convex_polygon(vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)], theme::TEXT, Stroke::NONE));
        egui::Popup::menu(&more_r).show(|ui| self.new_terminal_menu(ui, p, events));

        let mut bar = header.union(folder_rect).union(add_rect).union(more_rect);
        // Linked-folder changes: number of uncommitted files; click = diff panel.
        if p.cwd.is_some() {
            let n = world.change_counts.get(&id).copied().unwrap_or(0);
            let delta_label = format!("Δ {n}");
            let delta_rect = Rect::from_min_size(pos2(more_rect.max.x + 6.0, header.min.y), vec2(measure(&delta_label, &folder_font) + 16.0, HEADER_H));
            let delta_r = ui.interact(delta_rect, eid.with("changes"), Sense::click()).on_hover_text(t().group_changes_hint);
            let a11y = (t().group_changes_a11y)(&p.name);
            delta_r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &a11y));
            let bg = if delta_r.hovered() {
                color.gamma_multiply(0.5)
            } else if n > 0 {
                color.gamma_multiply(0.35)
            } else {
                color.gamma_multiply(0.22)
            };
            painter.rect_filled(delta_rect, CornerRadius::same(6), bg);
            painter.text(delta_rect.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, delta_label, folder_font.clone(), if n > 0 { theme::TEXT } else { theme::TEXT_DIM });
            if delta_r.clicked() {
                events.push(Event::ShowChanges(id.clone()));
            }
            bar = bar.union(delta_rect);
        }

        // Empty group: the obvious action, in the middle of the frame.
        if world.members(&id).next().is_none() && body.width() > 220.0 && body.height() > 80.0 {
            let empty_rect = Rect::from_center_size(body.center(), vec2(220.0, 44.0));
            let empty_r = ui.interact(empty_rect, eid.with("empty-add"), Sense::click());
            empty_r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, t().open_a_terminal));
            painter.rect_filled(empty_rect, CornerRadius::same(8), if empty_r.hovered() { color.gamma_multiply(0.6) } else { color.gamma_multiply(0.35) });
            painter.text(empty_rect.center(), Align2::CENTER_CENTER, t().open_a_terminal, FontId::proportional(16.0), theme::TEXT);
            if empty_r.clicked() {
                events.push(Event::NewCard { at: None, project: Some(id.clone()), command: None });
            }
        }

        let head = ui.interact(header, eid.with("head"), Sense::click_and_drag()).on_hover_text(t().group_header_hint);
        // × button at the end of the label, registered after: it wins the click.
        let close_rect = Rect::from_min_size(pos2(header.max.x - CLOSE_W, header.min.y), vec2(CLOSE_W, HEADER_H));
        let close = ui.interact(close_rect, eid.with("close"), Sense::click()).on_hover_text(t().close_group_hint);
        let a11y = (t().close_group_a11y)(&world.projects[i].name);
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
        // Pinned bar (free grid): the group is the whole canvas, there is nothing to move it
        // against nor to link it to.
        if !pinned && head.drag_started_by(PointerButton::Primary) && ui.input(|inp| inp.modifiers.alt) {
            self.wire_from = Some(id.clone());
        }
        if !pinned && head.dragged_by(PointerButton::Primary) && self.wire_from.as_deref() != Some(id.as_str()) {
            move_project(world, &id, head.drag_delta() / world.camera.zoom);
        }
        if head.double_clicked() {
            self.start_rename(&world.projects[i]);
        }
        let mut pick = false;
        head.context_menu(|ui| pick = self.menu(ui, &world.projects[i], events));
        if pick {
            self.open_picker(world, &id, events);
        }
        bar
    }

    /// Alt + drag from a label: a line follows the pointer; released over another group, it
    /// links the two.
    fn wire_drag(&mut self, ui: &Ui, world: &World, vp: Rect, events: &mut Vec<Event>) {
        let Some(from) = self.wire_from.clone() else { return };
        let Some(p) = world.project(&from) else {
            self.wire_from = None;
            return;
        };
        let cam = world.camera;
        let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) else { return };
        let label_h = (HEADER_H + 4.0) / cam.zoom;
        let target = link_target(world, &from, cam.to_world(pointer, vp), label_h);
        let painter = ui.painter_at(vp);
        if let Some(t) = target.as_deref().and_then(|t| world.project(t)) {
            painter.rect_stroke(cam.rect_to_screen(t.rect, vp), CornerRadius::same(10), Stroke::new(3.0, theme::FOCUS), StrokeKind::Outside);
        }
        let start = cam.rect_to_screen(p.rect, vp).center_top() - vec2(0.0, HEADER_H / 2.0 + 4.0);
        painter.line_segment([start, pointer], Stroke::new(2.5, theme::FOCUS));
        if ui.input(|i| i.pointer.any_released()) {
            self.wire_from = None;
            if let Some(target) = target {
                events.push(Event::AddWire(from, target));
            }
        }
    }

    /// While a terminal is dragged: the frame it will land in lights up; leaving the open
    /// group is signaled (red dashes), since the terminal will then leave this space.
    fn drag_feedback(&self, ui: &Ui, world: &World, id: &str, screen: Rect, color: egui::Color32) {
        let Some(card) = world.dragging.as_deref().and_then(|c| world.card(c)) else { return };
        let target = world.drop_target(card.rect.center());
        let painter = ui.painter();
        let joining = target.as_deref() == Some(id) && card.project.as_deref() != Some(id);
        let leaving = target.is_none() && card.project.as_deref() == Some(id);
        if joining {
            painter.rect_filled(screen, CornerRadius::same(10), color.gamma_multiply(0.12));
            painter.rect_stroke(screen, CornerRadius::same(10), Stroke::new(3.0, color), StrokeKind::Inside);
        } else if leaving {
            let red = egui::Color32::from_rgb(220, 80, 80);
            let pts = [screen.left_top(), screen.right_top(), screen.right_bottom(), screen.left_bottom(), screen.left_top()];
            painter.extend(egui::Shape::dashed_line(&pts, Stroke::new(2.0, red), 10.0, 6.0));
            painter.text(screen.center_top() + vec2(0.0, 16.0), Align2::CENTER_TOP, t().drop_to_leave_group, FontId::proportional(14.0), red);
        }
    }

    /// Input field over the label. Enter or click elsewhere: confirm; Escape: cancel.
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
            let editing = self.editing.take().expect("editing in progress");
            let name = editing.text.trim().to_string();
            if !cancelled && !name.is_empty() {
                events.push(Event::RenameProject(editing.id, name));
            }
        }
    }

    /// Opens the group's folder picker, on its folder or that of one of its terminals.
    fn open_picker(&mut self, world: &World, id: &str, events: &mut Vec<Event>) {
        let Some(p) = world.project(id) else { return };
        let title = (t().picker_title)(&p.name);
        let (picker, cmd) = FolderPicker::open(id.to_string(), title, p.cwd.is_some(), &suggested_folder(world, id), shortcuts(world, id));
        self.picker = Some(picker);
        events.push(Event::ToAgent(cmd));
    }

    /// Group menu. Returns `true` to open the folder picker.
    fn menu(&mut self, ui: &mut Ui, p: &Project, events: &mut Vec<Event>) -> bool {
        let mut pick = false;
        if ui.button(t().new_terminal_here).clicked() {
            events.push(Event::NewCard { at: Some(p.rect.center()), project: Some(p.id.clone()), command: None });
            ui.close();
        }
        if ui.button(t().rename_group).clicked() {
            self.start_rename(p);
            ui.close();
        }
        if ui.button(t().linked_folder_menu).clicked() {
            pick = true;
            ui.close();
        }
        if p.cwd.is_some() && ui.button(t().unlink_folder).clicked() {
            events.push(Event::SetProjectCwd(p.id.clone(), None));
            ui.close();
        }
        ui.menu_button(t().color, |ui| {
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
        if ui.button(t().close_group_menu).clicked() {
            events.push(Event::RequestCloseProject(p.id.clone()));
            ui.close();
        }
        if ui.button(t().remove_frame).clicked() {
            events.push(Event::RemoveProject(p.id.clone()));
            ui.close();
        }
        pick
    }

    pub fn dialogs(&mut self, ctx: &egui::Context, world: &World, events: &mut Vec<Event>) {
        if let Some(id) = self.confirm_close.clone() {
            if world.project(&id).is_none() {
                self.confirm_close = None;
            } else {
                let (title, body) = close_message(world, &id);
                match confirm(ctx, "close-project", &title, &body, t().close_all) {
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
        if let Some(editor) = &self.templates {
            let project = editor.project.clone();
            if world.project(&project).is_none() {
                self.templates = None;
            } else if let Some(templates) = self.templates_dialog(ctx, world) {
                events.push(Event::SetProjectTemplates(project, templates));
            }
            return;
        }
        let Some(picker) = &mut self.picker else { return };
        if world.project(&picker.project).is_none() {
            self.picker = None;
            return;
        }
        match picker.ui(ctx) {
            Some(Choice::Navigate(path)) => events.push(Event::ToAgent(picker.navigate(&path))),
            Some(Choice::Link(path)) => {
                events.push(Event::SetProjectCwd(picker.project.clone(), Some(path)));
                self.picker = None;
            }
            Some(Choice::Unlink) => {
                events.push(Event::SetProjectCwd(picker.project.clone(), None));
                self.picker = None;
            }
            Some(Choice::Cancel) => self.picker = None,
            None => {}
        }
    }

    fn assign(&mut self, world: &mut World, card: &str, project: Option<ProjectId>, relocate: bool, out: &mut Vec<Event>) {
        let Some(c) = world.card(card) else { return };
        if c.project == project {
            return;
        }
        let was_visible = world.shows_card(c);
        let spot = match (&project, relocate) {
            (Some(p), true) => arrival_spot(world, p, card),
            // Moved out of the group via the menu: placed away from the frame it leaves.
            (None, true) => {
                c.project.as_deref().and_then(|p| world.project(p)).map(|p| world.free_spot(pos2(p.rect.max.x + 80.0 + c.rect.width() / 2.0, p.rect.min.y + c.rect.height() / 2.0), c.rect.size()))
            }
            _ => None,
        };
        let name = project.as_deref().and_then(|p| world.project(p)).map(|p| p.name.clone());
        let c = world.card_mut(card).expect("card exists");
        c.project = project;
        if let Some(r) = spot {
            c.rect = r;
        }
        let hidden_now = !world.shows_card(world.card(card).expect("card exists"));
        if relocate {
            out.push(Event::SettleCard(card.to_string()));
        }
        if was_visible && hidden_now {
            // The terminal leaves the open space: say where it went (click on the toast: go there).
            out.push(Event::Notify(card.to_string(), Notice::Moved(name)));
        }
        world.touch();
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, out: &mut Vec<Event>) {
        match ev {
            Event::NewProject { at } => {
                // From a project space, the new project is a separate space: place it
                // away from everything, then open it.
                let in_space = world.space.is_some();
                let center = match at {
                    Some(at) => *at,
                    None if in_space => free_area(world),
                    None => world.camera.center,
                };
                let n = world.projects.len();
                let p = Project {
                    id: new_id(),
                    name: (t().default_project_name)(n + 1),
                    color: PROJECT_COLORS[n % PROJECT_COLORS.len()],
                    rect: Rect::from_center_size(center, EMPTY_PROJECT),
                    cwd: None,
                    templates: Vec::new(),
                };
                self.start_rename(&p);
                let id = p.id.clone();
                world.projects.push(p);
                // Never on top of a group or a terminal: at the nearest free spot.
                world.place_clear(&Body::Group(id.clone()));
                world.touch();
                if in_space {
                    out.push(Event::EnterSpace(Some(id.clone())));
                }
                // Renaming happens on the label: it must be on screen.
                if let Some(r) = world.body_rect(&Body::Group(id)) {
                    out.push(Event::RevealRect(r));
                }
            }
            Event::RenameProject(id, name) => {
                if let Some(p) = world.projects.iter_mut().find(|p| &p.id == id)
                    && !name.is_empty()
                {
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
            Event::SetProjectTemplates(id, templates) => {
                if let Some(p) = world.projects.iter_mut().find(|p| &p.id == id) {
                    p.templates = templates.clone();
                }
                world.touch();
            }
            Event::SetProjectColor(id, color) => {
                if let Some(p) = world.projects.iter_mut().find(|p| &p.id == id) {
                    p.color = *color;
                }
                world.touch();
            }
            Event::Agent(protocol::AgentEvent::DirListing(listing)) => {
                if let Some(p) = &mut self.picker {
                    p.on_listing(listing);
                }
            }
            // In a group: its neighbors move aside (then the refitted frame pushes the other
            // groups). Outside a group: it moves away from groups itself, and pushes free terminals.
            Event::SettleCard(id) => match world.card(id).map(|c| c.project.is_some()) {
                Some(true) => {
                    world.settle(id);
                }
                Some(false) => {
                    world.separate(&Body::Card(id.clone()));
                }
                None => {}
            },
            Event::RequestCloseProject(id) => {
                if world.project(id).is_some() {
                    self.confirm_close = Some(id.clone());
                }
            }
            Event::RemoveProject(id) => {
                world.projects.retain(|p| &p.id != id);
                for card in world.cards.iter_mut().filter(|c| c.project.as_ref() == Some(id)) {
                    card.project = None;
                }
                world.touch();
            }
            // Group vanished in the meantime: do nothing.
            Event::AssignCard { card, project, relocate } if project.as_ref().is_none_or(|p| world.project(p).is_some()) => {
                self.assign(world, card, project.clone(), *relocate, out);
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
        w.projects.push(Project { id: "p".into(), name: "p".into(), color: [0; 3], rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)), cwd: None, templates: Vec::new() });
        let mut member = Card::new(Rect::from_min_size(pos2(100.0, 100.0), Vec2::splat(50.0)), None);
        member.project = Some("p".into());
        w.add_card(member);
        // Under the frame but not a member: does not move.
        w.add_card(Card::new(Rect::from_min_size(pos2(200.0, 100.0), Vec2::splat(50.0)), None));
        move_project(&mut w, "p", vec2(500.0, -20.0));
        assert_eq!(w.projects[0].rect.min, pos2(500.0, -20.0));
        assert_eq!(w.cards[0].rect.min, pos2(600.0, 80.0));
        assert_eq!(w.cards[1].rect.min, pos2(200.0, 100.0));
    }

    #[test]
    fn group_members_and_close_message() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let mut w = World::default();
        w.projects.push(Project { id: "p".into(), name: "API".into(), color: [0; 3], rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)), cwd: None, templates: Vec::new() });
        let mut a = Card::new(Rect::from_min_size(pos2(100.0, 100.0), Vec2::splat(50.0)), None);
        a.name = Some("server".into());
        a.project = Some("p".into());
        a.info = Some(protocol::SessionInfo {
            session: a.session(),
            status: protocol::TermStatus::Running,
            command: Some("cargo run".into()),
            cwd: None,
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
            agent_session_id: None,
            agent_transcript: None,
            last_prompt: None,
        });
        let (ia, outside) = (a.id.clone(), Card::new(Rect::from_min_size(pos2(5000.0, 0.0), Vec2::splat(50.0)), None));
        w.add_card(a);
        w.add_card(outside);
        assert_eq!(cards_in(&w, "p"), vec![ia]);
        let (title, body) = close_message(&w, "p");
        assert_eq!(title, "Fermer le groupe « API » ?");
        assert!(body.contains("server  —  « cargo run » en cours"), "{body}");
        assert!(cards_in(&w, "absent").is_empty());
    }

    fn two_groups() -> (World, CardId) {
        let mut w = World::default();
        for (id, x) in [("api", 0.0), ("web", 5000.0)] {
            w.projects.push(Project { id: id.into(), name: id.to_uppercase(), color: [0; 3], rect: Rect::from_min_size(pos2(x, 0.0), Vec2::splat(1000.0)), cwd: None, templates: Vec::new() });
        }
        let mut c = Card::new(Rect::from_min_size(pos2(100.0, 100.0), Vec2::new(300.0, 200.0)), None);
        c.project = Some("api".into());
        let id = c.id.clone();
        w.add_card(c);
        let mut other = Card::new(Rect::from_min_size(pos2(5100.0, 100.0), Vec2::new(300.0, 200.0)), None);
        other.project = Some("web".into());
        w.add_card(other);
        w.refit_projects();
        (w, id)
    }

    fn assign(w: &mut World, card: &str, project: Option<&str>, relocate: bool) -> Vec<Event> {
        let mut out = vec![];
        Projects::default().handle(&Event::AssignCard { card: card.into(), project: project.map(Into::into), relocate }, w, &mut out);
        w.refit_projects();
        out
    }

    #[test]
    fn dropping_keeps_position_and_menu_relocates_next_to_members() {
        let (mut w, id) = two_groups();
        assert!(assign(&mut w, &id, Some("web"), false).is_empty(), "'All': it stays visible, no toast");
        assert_eq!(w.card(&id).unwrap().project.as_deref(), Some("web"));
        // WEB grew up to the card, which stayed where it was dropped: API, now empty, moved aside.
        assert!(!w.projects[0].rect.intersects(w.projects[1].rect), "{:?} / {:?}", w.projects[0].rect, w.projects[1].rect);

        let (mut w, id) = two_groups();
        assign(&mut w, &id, Some("web"), true);
        let rect = w.card(&id).unwrap().rect;
        assert!(rect.min.x >= 5400.0, "placed right of WEB's members: {rect:?}");
        assert!(w.projects[1].rect.contains_rect(rect), "WEB's frame contains it");
        assert_eq!(w.projects[0].rect.size(), crate::shell::world::EMPTY_PROJECT, "API, empty, keeps a minimum size");
    }

    #[test]
    fn leaving_the_open_space_says_where_the_terminal_went() {
        let (mut w, id) = two_groups();
        w.space = Some("api".into());
        let notices = |out: &[Event]| out.iter().filter(|e| matches!(e, Event::Notify(..))).cloned().collect::<Vec<_>>();
        let out = assign(&mut w, &id, Some("web"), true);
        assert!(matches!(notices(&out).as_slice(), [Event::Notify(c, Notice::Moved(Some(g)))] if *c == id && g == "WEB"), "{out:?}");
        assert!(out.iter().any(|e| matches!(e, Event::SettleCard(c) if *c == id)), "it makes room for itself");
        let out = assign(&mut w, &id, None, true);
        assert!(notices(&out).is_empty(), "already hidden: nothing to announce");
        assert_eq!(w.card(&id).unwrap().project, None);
        assert!(!w.projects[1].rect.intersects(w.card(&id).unwrap().rect), "moved out via the menu: placed outside the frame");
        assert!(notices(&assign(&mut w, &id, Some("gone"), true)).is_empty());
        assert_eq!(w.card(&id).unwrap().project, None, "unknown group: nothing changes");
    }

    #[test]
    fn removing_a_frame_frees_its_terminals() {
        let (mut w, id) = two_groups();
        Projects::default().handle(&Event::RemoveProject("api".into()), &mut w, &mut vec![]);
        assert_eq!(w.card(&id).unwrap().project, None);
    }

    #[test]
    fn folder_suggestion_and_label() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let (mut w, id) = two_groups();
        assert_eq!(suggested_folder(&w, "api"), "");
        w.card_mut(&id).unwrap().cwd = Some("/home/me/api".into());
        assert_eq!(suggested_folder(&w, "api"), "/home/me/api", "folder of a terminal in the group");
        assert_eq!(suggested_folder(&w, "web"), "", "not that of another group");
        w.projects[0].cwd = Some("~/api".into());
        assert_eq!(suggested_folder(&w, "api"), "~/api");
        assert_eq!(folder_label(None), "📁 Lier un dossier");
        assert_eq!(folder_label(Some("~/api")), "📁 ~/api");
        let long = folder_label(Some(&format!("/home/{}/fin", "x".repeat(60))));
        assert!(long.starts_with("📁 …") && long.ends_with("/fin") && long.chars().count() == 42, "{long}");
    }

    #[test]
    fn saved_templates_drop_empty_rows_and_name_the_unnamed() {
        let row = |name: &str, command: &str| CommandTemplate { name: name.into(), command: command.into() };
        let rows = [row("Review", "claude \"review\"\n"), row("  ", "  npm test -- --watch\nsecond line"), row("empty", "  \n"), row("", "")];
        assert_eq!(clean_templates(&rows), vec![row("Review", "claude \"review\"\n"), row("npm test -- --watch", "  npm test -- --watch\nsecond line")]);
        // Names stay apart in the menu.
        let same = [row("Review", "a"), row(" Review ", "b"), row("Review (2)", "c"), row("", "Review")];
        let names: Vec<String> = clean_templates(&same).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["Review", "Review (2)", "Review (2) (2)", "Review (3)"]);
        assert_eq!(menu_name("Logs"), "Logs");
        let long = menu_name(&"x".repeat(80));
        assert!(long.ends_with('…') && long.chars().count() == MENU_NAME_CHARS, "{long}");
        let (mut w, _) = two_groups();
        Projects::default().handle(&Event::SetProjectTemplates("api".into(), vec![row("a", "b")]), &mut w, &mut vec![]);
        assert_eq!(w.project("api").unwrap().templates, vec![row("a", "b")]);
        assert!(w.project("web").unwrap().templates.is_empty());
    }

    #[test]
    fn new_project_lifecycle() {
        let mut w = World::default();
        let mut p = Projects::default();
        p.handle(&Event::NewProject { at: Some(pos2(10.0, 10.0)) }, &mut w, &mut vec![]);
        assert_eq!(w.projects.len(), 1);
        assert!(p.has_dialog(), "offers to name the project right away");
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
        w.projects.push(Project { id: "api".into(), name: "API".into(), color: [0; 3], rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.0)), cwd: None, templates: Vec::new() });
        w.space = Some("api".into());
        let mut out = vec![];
        Projects::default().handle(&Event::NewProject { at: None }, &mut w, &mut out);
        let new = &w.projects[1];
        assert!(!new.rect.intersects(w.projects[0].rect));
        assert!(matches!(out.as_slice(), [Event::EnterSpace(Some(id)), Event::RevealRect(_)] if id == &new.id));
    }

    #[test]
    fn a_new_group_never_lands_on_an_existing_one() {
        let mut w = World::default();
        w.projects.push(Project { id: "api".into(), name: "API".into(), color: [0; 3], rect: Rect::from_center_size(Pos2::ZERO, EMPTY_PROJECT), cwd: None, templates: Vec::new() });
        // "All" view, camera centered on the existing group, or creation at a precise point inside it.
        for at in [None, Some(pos2(20.0, 10.0))] {
            Projects::default().handle(&Event::NewProject { at }, &mut w, &mut vec![]);
        }
        let rects: Vec<Rect> = w.projects.iter().map(|p| w.body_rect(&Body::Group(p.id.clone())).unwrap()).collect();
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                assert!(!rects[i].intersects(rects[j]), "{i}/{j}: {rects:?}");
            }
        }
        assert_eq!(w.projects[0].rect, Rect::from_center_size(Pos2::ZERO, EMPTY_PROJECT), "the existing one stays in place");
    }
}
