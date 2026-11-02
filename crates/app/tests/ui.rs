//! Tests d'interface sans écran (egui_kittest) : vraie application, sans PTY ni agent.

use std::path::PathBuf;

use egui::{Key, Modifiers, Pos2, Rect, Vec2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use protocol::{AgentEvent, SessionInfo, TermStatus};
use tcanvas::features::agent_link::AgentMode;
use tcanvas::features::terminal_card::Launcher;
use tcanvas::shell::event::Event;
use tcanvas::shell::world::{Card, World};
use tcanvas::{App, Config};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tcanvas-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn harness(dir: &std::path::Path) -> Harness<'static, App> {
    let dir = dir.to_path_buf();
    Harness::builder().with_size(Vec2::new(1400.0, 900.0)).with_step_dt(1.0 / 60.0).build_eframe(move |cc| {
        App::new(&cc.egui_ctx, Config { launcher: Launcher::Disabled, agent: AgentMode::Disabled, config_dir: dir, os_toasts: false })
    })
}

fn steps(h: &mut Harness<'_, App>, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn screen_of(h: &Harness<'_, App>, world_rect: Rect) -> Rect {
    let app = h.state();
    app.world.camera.rect_to_screen(world_rect, app.viewport())
}

fn add_card(h: &mut Harness<'_, App>, name: &str, at: Pos2) -> String {
    let world: &mut World = &mut h.state_mut().world;
    let mut card = Card::new(Rect::from_min_size(at, Vec2::new(500.0, 300.0)), Some(format!("/home/me/{name}")));
    card.name = Some(name.into());
    // Posée dans un cadre : membre de ce groupe (comme si on l'y avait lâchée).
    card.project = tcanvas::shell::world::project_at(&world.projects, card.rect.center()).map(|p| p.id.clone());
    let id = card.id.clone();
    world.add_card(card);
    id
}

fn ctrl_shift(h: &Harness<'_, App>, key: Key) {
    h.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, key);
}

#[test]
fn toolbar_button_creates_and_focuses_a_terminal() {
    let dir = temp_dir("button");
    let mut h = harness(&dir);
    steps(&mut h, 2);
    assert!(h.state().world.cards.is_empty());
    h.get_by_label("+ Terminal").click();
    steps(&mut h, 45);
    let app = h.state();
    assert_eq!(app.world.cards.len(), 1);
    assert_eq!(app.world.focused.as_deref(), Some(app.world.cards[0].id.as_str()));
}

#[test]
fn double_click_on_empty_canvas_opens_a_terminal_there() {
    let dir = temp_dir("dblclick");
    let mut h = harness(&dir);
    steps(&mut h, 2);
    let vp = h.state().viewport();
    let p = vp.center() + Vec2::new(-300.0, 150.0);
    for _ in 0..2 {
        h.drag_at(p);
        h.step();
        h.drop_at(p);
        h.step();
    }
    steps(&mut h, 2);
    let app = h.state();
    assert_eq!(app.world.cards.len(), 1);
    let world_point = app.world.camera.to_world(p, vp);
    assert!(app.world.cards[0].rect.expand(200.0).contains(world_point), "carte créée près du double-clic");
}

#[test]
fn keyboard_goes_to_focused_terminal_but_shortcuts_stay_global() {
    let dir = temp_dir("keys");
    let mut h = harness(&dir);
    let id = add_card(&mut h, "api", Pos2::ZERO);
    h.state_mut().dispatch(vec![Event::FocusCard(id)]);
    steps(&mut h, 2);
    h.event(egui::Event::Text("abc".into()));
    steps(&mut h, 2);
    assert_eq!(h.state().filters.text, "", "le texte tapé ne fuit pas dans la recherche");
    ctrl_shift(&h, Key::N);
    steps(&mut h, 3);
    assert_eq!(h.state().world.cards.len(), 2, "Ctrl+Maj+N fonctionne même terminal focalisé");
}

#[test]
fn search_shortcut_filters_cards() {
    let dir = temp_dir("search");
    let mut h = harness(&dir);
    add_card(&mut h, "api", Pos2::ZERO);
    add_card(&mut h, "blog", pos2(800.0, 0.0));
    steps(&mut h, 2);
    ctrl_shift(&h, Key::F);
    steps(&mut h, 2);
    h.event(egui::Event::Text("blog".into()));
    steps(&mut h, 2);
    let app = h.state();
    assert_eq!(app.filters.text, "blog");
    let matching: Vec<&str> = app.world.cards.iter().filter(|c| app.filters.matches(&app.world, c)).map(|c| c.name.as_deref().unwrap()).collect();
    assert_eq!(matching, vec!["blog"]);
}

#[test]
fn jump_palette_flies_to_the_chosen_terminal() {
    let dir = temp_dir("jump");
    let mut h = harness(&dir);
    add_card(&mut h, "api", Pos2::ZERO);
    let far = add_card(&mut h, "base-de-donnees", pos2(20_000.0, 9_000.0));
    steps(&mut h, 2);
    ctrl_shift(&h, Key::K);
    steps(&mut h, 2);
    assert!(h.state().jump_open());
    h.event(egui::Event::Text("donnees".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 45);
    let app = h.state();
    assert!(!app.jump_open());
    assert_eq!(app.world.focused.as_deref(), Some(far.as_str()), "focus après le vol");
    let card = app.world.card(&far).unwrap();
    assert!(app.viewport().contains_rect(screen_of(&h, card.rect).shrink(1.0)), "la carte est entièrement à l'écran");
}

#[test]
fn dragging_the_title_bar_moves_the_card() {
    let dir = temp_dir("drag");
    let mut h = harness(&dir);
    let id = add_card(&mut h, "api", Pos2::ZERO);
    steps(&mut h, 2);
    let before = h.state().world.card(&id).unwrap().rect;
    let title = screen_of(&h, before).min + Vec2::new(150.0, 10.0);
    h.drag_at(title);
    h.step();
    for k in 1..=10 {
        h.hover_at(title + Vec2::new(20.0 * k as f32, 10.0 * k as f32));
        h.step();
    }
    h.drop_at(title + Vec2::new(200.0, 100.0));
    steps(&mut h, 2);
    let after = h.state().world.card(&id).unwrap().rect;
    let moved = after.min - before.min;
    let zoom = h.state().world.camera.zoom;
    assert!((moved - Vec2::new(200.0, 100.0) / zoom).length() < 25.0, "déplacement {moved:?}");
    assert_eq!(after.size(), before.size());
}

#[test]
fn clicking_a_wire_flies_to_its_far_end() {
    let dir = temp_dir("wire");
    let mut h = harness(&dir);
    let a = add_card(&mut h, "a", pos2(-700.0, -150.0));
    let b = add_card(&mut h, "b", pos2(300.0, -150.0));
    h.state_mut().world.camera.zoom = 0.6;
    h.state_mut().dispatch(vec![Event::AddWire(a.clone(), b.clone())]);
    steps(&mut h, 2);
    // Point du fil tout près de la carte A : le bout le plus éloigné est B.
    let app = h.state();
    let ra = screen_of(&h, app.world.card(&a).unwrap().rect);
    let near_a = ra.right_center() + Vec2::new(12.0, 0.0);
    h.hover_at(near_a);
    h.step();
    h.drag_at(near_a);
    h.step();
    h.drop_at(near_a);
    steps(&mut h, 45);
    assert_eq!(h.state().world.focused.as_deref(), Some(b.as_str()));
}

#[test]
fn zooming_out_switches_to_previews_without_panicking() {
    let dir = temp_dir("zoom");
    let mut h = harness(&dir);
    for k in 0..30 {
        add_card(&mut h, &format!("t{k}"), pos2((k % 6) as f32 * 600.0, (k / 6) as f32 * 400.0));
    }
    steps(&mut h, 2);
    let center = h.state().viewport().center();
    h.hover_at(center);
    for _ in 0..12 {
        h.event(egui::Event::Zoom(0.7));
        h.step();
    }
    let zoom = h.state().world.camera.zoom;
    assert_eq!(zoom, tcanvas::features::canvas::MIN_ZOOM);
    for _ in 0..30 {
        h.event(egui::Event::Zoom(1.25));
        h.step();
    }
    assert_eq!(h.state().world.camera.zoom, tcanvas::features::canvas::MAX_ZOOM);
}

#[test]
fn agent_status_changes_raise_a_clickable_toast() {
    let dir = temp_dir("toast");
    let mut h = harness(&dir);
    let id = add_card(&mut h, "build", pos2(5000.0, 5000.0));
    let session = h.state().world.card(&id).unwrap().session();
    let info = |status| SessionInfo { session: session.clone(), status, command: Some("make".into()), cwd: None, last_exit: None, last_duration_secs: None, tail: vec![] };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Session(info(TermStatus::Running)))]);
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Session(info(TermStatus::Stalled)))]);
    steps(&mut h, 2);
    assert!(h.state().world.card(&id).unwrap().attention);
    assert_eq!(h.state().pending_toasts(), 1);
    h.get_by_label("⏸ Terminal bloqué").click();
    steps(&mut h, 45);
    let app = h.state();
    assert_eq!(app.world.focused.as_deref(), Some(id.as_str()));
    assert!(!app.world.card(&id).unwrap().attention, "regarder le terminal efface l'alerte");
    assert_eq!(app.pending_toasts(), 0);
}

#[test]
fn unknown_canvas_sessions_are_adopted() {
    let dir = temp_dir("adopt");
    let mut h = harness(&dir);
    let info = SessionInfo { session: "tc_orphan1".into(), status: TermStatus::Idle, command: None, cwd: Some("/srv".into()), last_exit: None, last_duration_secs: None, tail: vec![] };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Session(info))]);
    steps(&mut h, 2);
    let app = h.state();
    assert_eq!(app.world.cards.len(), 1);
    assert_eq!(app.world.cards[0].id, "orphan1");
    assert_eq!(app.world.cards[0].cwd.as_deref(), Some("/srv"));
}

#[test]
fn layout_survives_a_restart() {
    let dir = temp_dir("restart");
    let id;
    {
        let mut h = harness(&dir);
        id = add_card(&mut h, "persistant", pos2(123.0, 456.0));
        h.state_mut().dispatch(vec![Event::NewProject { at: Some(Pos2::ZERO) }]);
        h.state_mut().world.camera.zoom = 0.5;
        steps(&mut h, 2);
        h.state_mut().save_now();
    }
    let mut h = harness(&dir);
    steps(&mut h, 2);
    let app = h.state();
    let card = app.world.card(&id).expect("carte restaurée");
    assert_eq!(card.name.as_deref(), Some("persistant"));
    assert_eq!(card.rect.min, pos2(123.0, 456.0));
    assert_eq!(app.world.projects.len(), 1);
    assert_eq!(app.world.camera.zoom, 0.5);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn new_terminal_when_zoomed_out_flies_in_to_a_usable_size() {
    // Cas réel : vue dézoomée à 0,37 → la nouvelle carte restait un aperçu « sass · inactif ».
    let dir = temp_dir("zoomed-out-new");
    let mut h = harness(&dir);
    h.state_mut().world.camera.zoom = 0.37;
    steps(&mut h, 2);
    h.get_by_label("+ Terminal").click();
    steps(&mut h, 45);
    let app = h.state();
    let id = app.world.cards[0].id.clone();
    assert_eq!(app.world.focused.as_deref(), Some(id.as_str()));
    assert!(!tcanvas::features::terminal_card::is_preview(app.world.camera.zoom), "zoom {}", app.world.camera.zoom);
}

#[test]
fn clicking_a_preview_zooms_in_on_it() {
    let dir = temp_dir("preview-click");
    let mut h = harness(&dir);
    let id = add_card(&mut h, "loin", Pos2::ZERO);
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(250.0, 150.0), zoom: 0.3 };
    steps(&mut h, 2);
    let body = screen_of(&h, h.state().world.card(&id).unwrap().rect).center();
    h.drag_at(body);
    h.step();
    h.drop_at(body);
    steps(&mut h, 45);
    let app = h.state();
    assert_eq!(app.world.focused.as_deref(), Some(id.as_str()));
    assert!(!tcanvas::features::terminal_card::is_preview(app.world.camera.zoom));
}

fn add_project(h: &mut Harness<'_, App>, name: &str, rect: Rect) -> String {
    let id = format!("p-{name}");
    h.state_mut().world.projects.push(tcanvas::shell::world::Project { id: id.clone(), name: name.into(), color: [94, 129, 244], rect, cwd: None });
    id
}

/// Cadre actuel du groupe (ajusté à ses membres).
fn project_rect(h: &Harness<'_, App>, id: &str) -> Rect {
    h.state().world.project(id).unwrap().rect
}

fn sees_whole(h: &Harness<'_, App>, world_rect: Rect) -> bool {
    h.state().viewport().contains_rect(screen_of(h, world_rect).shrink(1.0))
}

#[test]
fn ctrl_shift_digit_flies_to_that_project_even_from_a_terminal() {
    let dir = temp_dir("project-digit");
    let mut h = harness(&dir);
    let far = Rect::from_min_size(pos2(30_000.0, -8_000.0), Vec2::new(2000.0, 1200.0));
    add_project(&mut h, "un", Rect::from_min_size(Pos2::ZERO, Vec2::splat(800.0)));
    let deux = add_project(&mut h, "deux", far);
    let id = add_card(&mut h, "dans-un", pos2(100.0, 100.0));
    h.state_mut().dispatch(vec![Event::FocusCard(id)]);
    steps(&mut h, 2);
    let far = project_rect(&h, &deux);
    assert!(!sees_whole(&h, far));
    // Touche physique « 2 » (en AZERTY la touche logique est « é »).
    h.event(egui::Event::Key { key: Key::Quote, physical_key: Some(Key::Num2), pressed: true, repeat: false, modifiers: Modifiers::CTRL | Modifiers::SHIFT });
    steps(&mut h, 45);
    assert!(sees_whole(&h, far), "le projet 2 est cadré");
}

#[test]
fn palette_finds_projects() {
    let dir = temp_dir("project-palette");
    let mut h = harness(&dir);
    let far = Rect::from_min_size(pos2(-25_000.0, 12_000.0), Vec2::new(1500.0, 900.0));
    let facturation = add_project(&mut h, "facturation", far);
    add_card(&mut h, "api", Pos2::ZERO);
    steps(&mut h, 2);
    let far = project_rect(&h, &facturation);
    ctrl_shift(&h, Key::K);
    steps(&mut h, 2);
    h.event(egui::Event::Text("factu".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 45);
    assert!(sees_whole(&h, far));
}

#[test]
fn clicking_a_project_chip_opens_that_project_alone() {
    let dir = temp_dir("project-chip");
    let mut h = harness(&dir);
    let infra = Rect::from_min_size(pos2(9_000.0, 9_000.0), Vec2::new(1200.0, 800.0));
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0)));
    add_project(&mut h, "infra", infra);
    add_card(&mut h, "serveur", pos2(100.0, 100.0));
    add_card(&mut h, "deploiement", pos2(9_100.0, 9_100.0));
    add_card(&mut h, "orphelin", pos2(-20_000.0, 0.0));
    steps(&mut h, 2);
    let shown = |h: &Harness<'_, App>| ["serveur", "deploiement", "orphelin"].map(|n| h.query_by_label(&format!("Fermer le terminal {n}")).is_some());

    h.get_by_label("● infra").click();
    steps(&mut h, 3);
    assert_eq!(h.state().world.space.as_deref(), Some("p-infra"));
    assert!(sees_whole(&h, project_rect(&h, "p-infra")), "le projet est cadré dès l'ouverture");
    assert_eq!(shown(&h), [false, true, false], "un seul projet à la fois");
    assert!(h.query_by_label("Fermer le groupe api").is_none(), "le cadre des autres projets est caché");
    // Même en regardant exactement là où sont les autres, rien n'apparaît ni ne se clique.
    for at in [pos2(600.0, 400.0), pos2(-19_750.0, 150.0)] {
        h.state_mut().world.camera = tcanvas::shell::world::Camera { center: at, zoom: 1.0 };
        steps(&mut h, 2);
        assert_eq!(shown(&h), [false, false, false], "vue sur {at:?}");
        assert!(h.query_by_label("Fermer le groupe api").is_none());
    }
    // Contrôle : dans « Tous », la même vue montre bien le terminal.
    h.get_by_label("Tous").click();
    steps(&mut h, 2);
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(600.0, 400.0), zoom: 1.0 };
    steps(&mut h, 2);
    assert_eq!(shown(&h), [true, false, false]);
    h.get_by_label("● infra").click();
    steps(&mut h, 3);

    h.get_by_label("● api").click();
    steps(&mut h, 3);
    assert_eq!(h.state().world.space.as_deref(), Some("p-api"));
    assert!(h.query_by_label("Fermer le groupe infra").is_none());

    h.get_by_label("Tous").click();
    steps(&mut h, 3);
    assert_eq!(h.state().world.space, None);
    h.state_mut().dispatch(vec![Event::FitRect(Rect::from_min_max(pos2(-20_000.0, 0.0), pos2(10_200.0, 9_800.0)))]);
    steps(&mut h, 45);
    assert!(h.query_by_label("Fermer le groupe api").is_some() && h.query_by_label("Fermer le groupe infra").is_some(), "« Tous » montre l'ensemble");
}

#[test]
fn a_new_terminal_opens_in_the_open_project() {
    let dir = temp_dir("space-new-card");
    let mut h = harness(&dir);
    let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0));
    add_project(&mut h, "api", rect);
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 3);
    // Pousser la vue loin du cadre : le terminal créé là doit quand même rester dans le projet.
    h.state_mut().world.camera.center = pos2(4_000.0, 3_000.0);
    ctrl_shift(&h, Key::N);
    steps(&mut h, 45);
    let app = h.state();
    let card = app.world.cards.last().expect("terminal créé");
    assert!(app.world.shows_card(card), "visible dans l'espace ouvert");
    assert_eq!(app.world.project_of(card).map(|p| p.id.as_str()), Some("p-api"));
}

#[test]
fn going_to_a_hidden_terminal_opens_its_project() {
    let dir = temp_dir("space-jump");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0)));
    add_project(&mut h, "infra", Rect::from_min_size(pos2(9_000.0, 9_000.0), Vec2::new(1200.0, 800.0)));
    let target = add_card(&mut h, "deploiement", pos2(9_100.0, 9_100.0));
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 3);
    ctrl_shift(&h, Key::K);
    steps(&mut h, 2);
    h.event(egui::Event::Text("deploiement".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 45);
    let app = h.state();
    assert_eq!(app.world.space.as_deref(), Some("p-infra"));
    assert_eq!(app.world.focused.as_deref(), Some(target.as_str()));
}

#[test]
fn the_open_project_and_its_view_survive_a_restart() {
    let dir = temp_dir("space-restart");
    let cam;
    {
        let mut h = harness(&dir);
        add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0)));
        add_project(&mut h, "infra", Rect::from_min_size(pos2(9_000.0, 9_000.0), Vec2::new(1200.0, 800.0)));
        steps(&mut h, 2);
        h.get_by_label("● infra").click();
        steps(&mut h, 3);
        h.state_mut().world.camera.zoom = 0.42;
        cam = h.state().world.camera;
        h.state_mut().save_now();
    }
    let mut h = harness(&dir);
    steps(&mut h, 2);
    assert_eq!(h.state().world.space.as_deref(), Some("p-infra"));
    assert_eq!(h.state().world.camera, cam);
    std::fs::remove_dir_all(dir).unwrap();
}

fn double_click_project_title(h: &mut Harness<'_, App>, id: &str) {
    let p = screen_of(h, project_rect(h, id)).min + Vec2::new(20.0, -16.0);
    for _ in 0..2 {
        h.drag_at(p);
        h.step();
        h.drop_at(p);
        h.step();
    }
    steps(h, 3);
}

fn project_name(h: &Harness<'_, App>, id: &str) -> String {
    h.state().world.project(id).unwrap().name.clone()
}

#[test]
fn double_click_on_project_title_renames_it_in_place() {
    let dir = temp_dir("rename-project");
    let mut h = harness(&dir);
    let rect = Rect::from_min_size(pos2(-400.0, -200.0), Vec2::new(900.0, 500.0));
    let id = add_project(&mut h, "Projet 1", rect);
    steps(&mut h, 2);
    double_click_project_title(&mut h, &id);
    // Le nom est tout sélectionné : taper le remplace.
    h.event(egui::Event::Text("Backend".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 3);
    assert_eq!(project_name(&h, &id), "Backend");
    assert!(h.state().world.cards.is_empty(), "le double-clic ne crée pas de terminal");
}

#[test]
fn escape_cancels_and_clicking_elsewhere_keeps_the_new_name() {
    let dir = temp_dir("rename-cancel");
    let mut h = harness(&dir);
    let rect = Rect::from_min_size(pos2(-400.0, -200.0), Vec2::new(900.0, 500.0));
    let id = add_project(&mut h, "Original", rect);
    steps(&mut h, 2);

    double_click_project_title(&mut h, &id);
    h.event(egui::Event::Text("Abandonné".into()));
    steps(&mut h, 2);
    h.key_press(Key::Escape);
    steps(&mut h, 3);
    assert_eq!(project_name(&h, &id), "Original");

    steps(&mut h, 30); // sinon les deux doubles-clics forment un triple-clic
    double_click_project_title(&mut h, &id);
    assert!(h.state().projects_editing(), "le second double-clic ouvre l'édition");
    h.event(egui::Event::Text("Gardé".into()));
    steps(&mut h, 2);
    let elsewhere = h.state().viewport().max - Vec2::new(400.0, 300.0);
    h.drag_at(elsewhere);
    h.step();
    h.drop_at(elsewhere);
    steps(&mut h, 3);
    assert_eq!(project_name(&h, &id), "Gardé");
}

#[test]
fn typing_a_project_name_never_reaches_the_focused_terminal() {
    let dir = temp_dir("rename-focus");
    let mut h = harness(&dir);
    let rect = Rect::from_min_size(pos2(-400.0, -200.0), Vec2::new(900.0, 500.0));
    let id = add_project(&mut h, "P", rect);
    let card = add_card(&mut h, "t", pos2(2000.0, 0.0));
    h.state_mut().dispatch(vec![Event::FocusCard(card)]);
    steps(&mut h, 2);
    double_click_project_title(&mut h, &id);
    h.event(egui::Event::Text("Nom".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 3);
    assert_eq!(project_name(&h, &id), "Nom");
}

fn click_at(h: &mut Harness<'_, App>, p: Pos2) {
    h.hover_at(p);
    h.step();
    h.drag_at(p);
    h.step();
    h.drop_at(p);
    steps(h, 3);
}

/// Centre du bouton × de la carte (carré à droite de la barre de titre).
fn close_button(h: &Harness<'_, App>, id: &str) -> Pos2 {
    let r = screen_of(h, h.state().world.card(id).unwrap().rect);
    let side = tcanvas::features::terminal_card::TITLE_H * h.state().world.camera.zoom;
    pos2(r.max.x - side / 2.0, r.min.y + side / 2.0)
}

#[test]
fn close_button_asks_before_closing() {
    let dir = temp_dir("close-button");
    let mut h = harness(&dir);
    let id = add_card(&mut h, "api", pos2(-300.0, -200.0));
    steps(&mut h, 2);

    let at = close_button(&h, &id);
    click_at(&mut h, at);
    assert!(h.query_by_label("Fermer « api » ?").is_some(), "confirmation affichée");
    h.get_by_label("Annuler").click();
    steps(&mut h, 3);
    assert!(h.state().world.card(&id).is_some(), "Annuler garde le terminal");

    let at = close_button(&h, &id);
    click_at(&mut h, at);
    h.get_by_label("Fermer").click();
    steps(&mut h, 3);
    assert!(h.state().world.card(&id).is_none(), "Fermer retire le terminal");
}

#[test]
fn ctrl_shift_w_closes_the_focused_terminal_after_confirmation() {
    let dir = temp_dir("close-shortcut");
    let mut h = harness(&dir);
    let keep = add_card(&mut h, "garde", pos2(-900.0, 0.0));
    let id = add_card(&mut h, "jetable", pos2(200.0, 0.0));
    h.state_mut().dispatch(vec![Event::FocusCard(id.clone())]);
    steps(&mut h, 2);

    ctrl_shift(&h, Key::W);
    steps(&mut h, 3);
    assert!(h.query_by_label("Fermer « jetable » ?").is_some());
    h.key_press(Key::Escape);
    steps(&mut h, 3);
    assert!(h.state().world.card(&id).is_some(), "Échap annule");

    ctrl_shift(&h, Key::W);
    steps(&mut h, 3);
    h.key_press(Key::Enter);
    steps(&mut h, 3);
    assert!(h.state().world.card(&id).is_none(), "Entrée confirme");
    assert!(h.state().world.card(&keep).is_some(), "seul le terminal focalisé est fermé");
}

#[test]
fn ctrl_shift_w_without_focus_does_nothing() {
    let dir = temp_dir("close-nofocus");
    let mut h = harness(&dir);
    add_card(&mut h, "x", Pos2::ZERO);
    steps(&mut h, 2);
    ctrl_shift(&h, Key::W);
    steps(&mut h, 3);
    assert!(h.query_by_label_contains("Fermer «").is_none());
    assert_eq!(h.state().world.cards.len(), 1);
}

#[test]
fn closing_a_group_closes_its_terminals_after_confirmation() {
    let dir = temp_dir("close-group");
    let mut h = harness(&dir);
    let rect = Rect::from_min_size(pos2(-600.0, -300.0), Vec2::new(1300.0, 500.0));
    let group = add_project(&mut h, "jetable", rect);
    let inside_a = add_card(&mut h, "a", pos2(-550.0, -250.0));
    let inside_b = add_card(&mut h, "b", pos2(100.0, -250.0));
    let outside = add_card(&mut h, "dehors", pos2(-550.0, 400.0));
    h.state_mut().dispatch(vec![Event::AddWire(inside_a.clone(), outside.clone())]);
    steps(&mut h, 2);

    h.get_by_label("Fermer le groupe jetable").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("Fermer le groupe « jetable » ?").is_some(), "confirmation affichée");
    h.get_by_label("Annuler").click();
    steps(&mut h, 3);
    assert_eq!(h.state().world.cards.len(), 3);

    h.get_by_label("Fermer le groupe jetable").click();
    steps(&mut h, 3);
    h.get_by_label("Tout fermer").click();
    steps(&mut h, 3);
    let app = h.state();
    assert!(app.world.project(&group).is_none(), "cadre supprimé");
    assert!(app.world.card(&inside_a).is_none() && app.world.card(&inside_b).is_none(), "terminaux du groupe fermés");
    assert!(app.world.card(&outside).is_some(), "le terminal hors du groupe reste");
    assert!(app.world.wires.is_empty(), "les fils vers les terminaux fermés disparaissent");
}

fn cursor(h: &Harness<'_, App>) -> egui::CursorIcon {
    h.output().platform_output.cursor_icon
}

fn hold_shift(h: &Harness<'_, App>, held: bool) {
    h.event(egui::Event::ModifiersChanged(if held { Modifiers::SHIFT } else { Modifiers::NONE }));
}

#[test]
fn holding_shift_shows_the_hand_and_drags_the_view_even_over_a_terminal() {
    let dir = temp_dir("hand");
    let mut h = harness(&dir);
    let id = add_card(&mut h, "api", pos2(-250.0, -150.0));
    steps(&mut h, 2);
    let over_card = screen_of(&h, h.state().world.card(&id).unwrap().rect).center();
    h.hover_at(over_card);
    steps(&mut h, 2);
    assert_ne!(cursor(&h), egui::CursorIcon::Grab, "pas de main sans Maj");

    hold_shift(&h, true);
    steps(&mut h, 2);
    assert_eq!(cursor(&h), egui::CursorIcon::Grab, "main ouverte avec Maj");

    let cam_before = h.state().world.camera.center;
    let card_before = h.state().world.card(&id).unwrap().rect;
    h.event(egui::Event::PointerButton { pos: over_card, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::SHIFT });
    h.step();
    for k in 1..=10 {
        h.hover_at(over_card + Vec2::new(15.0 * k as f32, 8.0 * k as f32));
        h.step();
    }
    assert_eq!(cursor(&h), egui::CursorIcon::Grabbing, "main fermée pendant le glissement");
    h.event(egui::Event::PointerButton { pos: over_card + Vec2::new(150.0, 80.0), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::SHIFT });
    hold_shift(&h, false);
    steps(&mut h, 2);

    let app = h.state();
    let moved = (app.world.camera.center - cam_before) * app.world.camera.zoom;
    assert!((moved + Vec2::new(150.0, 80.0)).length() < 20.0, "la vue suit la main : {moved:?}");
    assert_eq!(app.world.card(&id).unwrap().rect, card_before, "la carte ne bouge pas");
    assert_ne!(app.world.focused.as_deref(), Some(id.as_str()), "Maj + glisser ne sélectionne pas le terminal");
    assert_ne!(cursor(&h), egui::CursorIcon::Grab, "plus de main une fois Maj relâchée");
}

#[test]
fn dragging_empty_canvas_shows_the_closed_hand() {
    let dir = temp_dir("hand-bg");
    let mut h = harness(&dir);
    steps(&mut h, 2);
    let p = h.state().viewport().center();
    h.drag_at(p);
    h.step();
    for k in 1..=5 {
        h.hover_at(p + Vec2::new(10.0 * k as f32, 0.0));
        h.step();
    }
    assert_eq!(cursor(&h), egui::CursorIcon::Grabbing);
    h.drop_at(p + Vec2::new(50.0, 0.0));
    steps(&mut h, 2);
}

// --- Groupes ajustés au contenu, terminaux qui changent de groupe ----------------------------

/// Glisse la barre de titre de la carte de `delta` (pixels écran), par petits pas, puis la lâche.
fn drag_card_by(h: &mut Harness<'_, App>, id: &str, delta: Vec2) {
    let title = screen_of(h, h.state().world.card(id).unwrap().rect).min + Vec2::new(150.0, 8.0);
    h.drag_at(title);
    h.step();
    for k in 1..=10 {
        h.hover_at(title + delta * (k as f32 / 10.0));
        h.step();
    }
    h.drop_at(title + delta);
    steps(h, 3);
}

fn group_of(h: &Harness<'_, App>, card: &str) -> Option<String> {
    h.state().world.card(card).unwrap().project.clone()
}

/// Union des membres + marge : ce que doit être le cadre.
fn members_fit(h: &Harness<'_, App>, project: &str) -> Rect {
    let w = &h.state().world;
    w.members(project).map(|c| c.rect).reduce(|a, b| a.union(b)).unwrap().expand(tcanvas::shell::world::PROJECT_PAD)
}

fn right_click_at(h: &mut Harness<'_, App>, p: Pos2) {
    h.hover_at(p);
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Secondary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    steps(h, 2);
}

#[test]
fn nudging_a_card_against_the_edge_never_grows_the_group_beyond_its_content() {
    let dir = temp_dir("group-fit");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 600.0)));
    add_card(&mut h, "a", pos2(40.0, 40.0));
    let b = add_card(&mut h, "b", pos2(700.0, 40.0));
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 45);
    // Vingt petits coups vers le bord droit : le cadre suit la carte, sans jamais la dépasser.
    for _ in 0..20 {
        drag_card_by(&mut h, &b, Vec2::new(30.0, 0.0));
        assert_eq!(group_of(&h, &b).as_deref(), Some("p-api"), "reste dans le groupe");
        assert_eq!(project_rect(&h, "p-api"), members_fit(&h, "p-api"));
    }
    let wide = project_rect(&h, "p-api");
    // Elle a fini par sortir de la vue : on la recentre avant de la ramener.
    h.state_mut().world.camera.center = h.state().world.card(&b).unwrap().rect.center();
    steps(&mut h, 2);
    // Et en la ramenant, le cadre rétrécit.
    drag_card_by(&mut h, &b, Vec2::new(-200.0, 0.0));
    assert!(project_rect(&h, "p-api").width() < wide.width());
    assert_eq!(project_rect(&h, "p-api"), members_fit(&h, "p-api"));
}

#[test]
fn dragging_a_card_out_of_its_group_frees_it_and_onto_another_group_joins_it() {
    let dir = temp_dir("group-move");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    add_project(&mut h, "web", Rect::from_min_size(pos2(0.0, 1200.0), Vec2::new(800.0, 500.0)));
    add_card(&mut h, "a", pos2(40.0, 40.0));
    let b = add_card(&mut h, "b", pos2(700.0, 40.0));
    add_card(&mut h, "w", pos2(40.0, 1240.0));
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(700.0, 900.0), zoom: 0.4 };
    steps(&mut h, 3);
    let before = project_rect(&h, "p-api");

    // Vers le vide (« Tous ») : hors groupe, et le cadre d'api rétrécit autour de « a ».
    drag_card_by(&mut h, &b, Vec2::new(0.0, 230.0));
    assert_eq!(group_of(&h, &b), None);
    assert!(project_rect(&h, "p-api").width() < before.width());
    assert!(!project_rect(&h, "p-api").intersects(h.state().world.card(&b).unwrap().rect));

    // Sur le cadre de web : il en devient membre et le cadre l'englobe.
    let target = screen_of(&h, project_rect(&h, "p-web")).center();
    let from = screen_of(&h, h.state().world.card(&b).unwrap().rect).center();
    drag_card_by(&mut h, &b, target - from);
    assert_eq!(group_of(&h, &b).as_deref(), Some("p-web"));
    assert!(project_rect(&h, "p-web").contains_rect(h.state().world.card(&b).unwrap().rect));
}

#[test]
fn move_to_menu_sends_a_terminal_to_another_group_from_its_space() {
    let dir = temp_dir("group-menu");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    add_project(&mut h, "web", Rect::from_min_size(pos2(9000.0, 0.0), Vec2::new(800.0, 500.0)));
    let a = add_card(&mut h, "a", pos2(40.0, 40.0));
    add_card(&mut h, "w", pos2(9040.0, 40.0));
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 45);

    let title = screen_of(&h, h.state().world.card(&a).unwrap().rect).min + Vec2::new(150.0, 8.0);
    right_click_at(&mut h, title);
    h.get_by_label("Déplacer vers").click();
    steps(&mut h, 2);
    h.get_by_label("web").click();
    steps(&mut h, 3);
    assert_eq!(group_of(&h, &a).as_deref(), Some("p-web"));
    assert!(project_rect(&h, "p-web").contains_rect(h.state().world.card(&a).unwrap().rect), "posé dans le cadre de web");
    assert!(h.query_by_label("Fermer le terminal a").is_none(), "il a quitté l'espace ouvert");
    // Le toast dit où il est parti ; un clic y emmène.
    h.get_by_label("➜ Déplacé vers « web »").click();
    steps(&mut h, 45);
    assert_eq!(h.state().world.space.as_deref(), Some("p-web"));
    assert_eq!(h.state().world.focused.as_deref(), Some(a.as_str()));
}

// --- Dossier lié et panneau « Changements » --------------------------------------------------

fn changes(key: &str, files: &[(&str, u64)]) -> AgentEvent {
    AgentEvent::Changes(protocol::ChangeSet {
        key: key.into(),
        root: Some("/home/me/api".into()),
        git: true,
        branch: Some("main".into()),
        error: None,
        files: files
            .iter()
            .map(|(path, mtime)| protocol::FileChange { path: (*path).into(), kind: protocol::ChangeKind::Modified, added: Some(3), removed: Some(1), mtime: *mtime, binary: false })
            .collect(),
        truncated: false,
    })
}

fn diff_requests(h: &Harness<'_, App>) -> Vec<String> {
    h.state()
        .agent_sent()
        .iter()
        .filter_map(|c| match c {
            protocol::AgentCmd::Diff { file, .. } => Some(file.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn linking_a_folder_from_the_label_opens_new_terminals_there_and_watches_it() {
    let dir = temp_dir("link-folder");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    let a = add_card(&mut h, "a", pos2(40.0, 40.0));
    h.state_mut().world.card_mut(&a).unwrap().cwd = Some("/home/me/code/api".into());
    steps(&mut h, 3);

    // Le sélecteur s'ouvre sur le dossier du terminal du groupe et le demande à l'agent.
    h.get_by_label("Dossier du groupe api").click();
    steps(&mut h, 2);
    assert_eq!(h.state().agent_sent().last(), Some(&protocol::AgentCmd::ListDir { path: "/home/me/code/api".into() }));
    let listing = |path: &str, dirs: &[(&str, bool)]| {
        Event::Agent(AgentEvent::DirListing(protocol::DirListing {
            request: path.into(),
            path: Some(path.into()),
            parent: Some("/home/me/code".into()),
            git: path.ends_with("web"),
            dirs: dirs.iter().map(|(n, g)| protocol::DirEntry { name: (*n).into(), git: *g }).collect(),
            error: None,
        }))
    };
    h.state_mut().dispatch(vec![listing("/home/me/code/api", &[("docs", false), ("web", true)])]);
    steps(&mut h, 2);
    assert!(h.query_by_label("📁 docs").is_some(), "sous-dossiers listés");
    assert!(h.query_by_label("Récents :").is_some(), "raccourcis vers les dossiers récents");

    // Entrer dans « web » (dépôt git), puis le lier.
    h.get_by_label("📁 web").click();
    steps(&mut h, 2);
    assert_eq!(h.state().agent_sent().last(), Some(&protocol::AgentCmd::ListDir { path: "/home/me/code/api/web".into() }));
    h.state_mut().dispatch(vec![listing("/home/me/code/api/web", &[])]);
    steps(&mut h, 2);
    assert!(h.query_by_label("⎇ dépôt git").is_some());
    h.get_by_label("Lier ce dossier").click();
    steps(&mut h, 3);
    assert!(!h.state().projects_editing(), "sélecteur fermé");
    let linked = "/home/me/code/api/web";
    assert_eq!(h.state().world.project("p-api").unwrap().cwd.as_deref(), Some(linked));
    assert!(h.state().agent_sent().contains(&protocol::AgentCmd::Watch { key: "p-api".into(), path: linked.into() }), "l'agent surveille le dossier");

    // Un terminal créé dans le groupe démarre dans ce dossier.
    steps(&mut h, 30); // sinon le clic sur l'étiquette et le double-clic forment un triple-clic
    // Dans la marge du cadre, coin bas-gauche (le bas-droit de l'écran est à la mini-carte).
    let inside = screen_of(&h, project_rect(&h, "p-api")).left_bottom() + Vec2::new(10.0, -10.0);
    h.drag_at(inside);
    h.step();
    h.drop_at(inside);
    h.step();
    h.drag_at(inside);
    h.step();
    h.drop_at(inside);
    steps(&mut h, 3);
    let card = h.state().world.cards.last().unwrap().clone();
    assert_ne!(card.id, a, "terminal créé");
    assert_eq!((card.cwd.as_deref(), card.project.as_deref()), (Some(linked), Some("p-api")));
}

#[test]
fn changes_panel_follows_the_ai_and_shows_diffs() {
    let dir = temp_dir("changes-panel");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    h.state_mut().world.projects[0].cwd = Some("~/api".into());
    add_card(&mut h, "a", pos2(40.0, 40.0));
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 3);
    let now = tcanvas::shell::world::unix_now();
    h.state_mut().dispatch(vec![Event::Agent(changes("p-api", &[("src/old.rs", now - 600), ("src/main.rs", now - 1)]))]);
    steps(&mut h, 2);

    h.get_by_label("Δ Changements (2)").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("Fichier modifié src/main.rs").is_some() && h.query_by_label("Fichier modifié src/old.rs").is_some());
    assert!(h.query_by_label("⎇ main").is_some());
    // « Suivre » : le dernier fichier touché est affiché d'office.
    assert_eq!(diff_requests(&h), vec!["src/main.rs"]);
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Diff { key: "p-api".into(), file: "src/main.rs".into(), text: "@@ -1 +1 @@\n-ancien\n+nouveau\n".into(), truncated: false })]);
    steps(&mut h, 3);
    assert!(h.query_by_label("+nouveau").is_some() && h.query_by_label("-ancien").is_some(), "diff affiché");

    // Choisir un fichier à la main arrête de suivre.
    h.get_by_label("Fichier modifié src/old.rs").click();
    steps(&mut h, 3);
    assert_eq!(diff_requests(&h).last().map(String::as_str), Some("src/old.rs"));
    assert!(!h.state().changes.follow);
    assert_eq!(h.state().changes.selected("p-api"), Some("src/old.rs"));

    // L'état du panneau est sauvegardé ; le canevas lui laisse la place.
    assert!(h.state().world.changes_open);
    assert!(h.state().viewport().max.x < 1400.0 - 200.0, "le canevas laisse la place au panneau");
    h.get_by_label("Δ Changements (2)").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("Fichier modifié src/main.rs").is_none(), "panneau fermé");
}

#[test]
fn a_terminal_dropped_onto_another_takes_its_place_and_the_group_rearranges() {
    let dir = temp_dir("group-settle");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    let big = add_card(&mut h, "gros", pos2(40.0, 40.0));
    h.state_mut().world.card_mut(&big).unwrap().rect = Rect::from_min_size(pos2(40.0, 40.0), Vec2::new(900.0, 500.0));
    let new = add_card(&mut h, "nouveau", pos2(1200.0, 40.0));
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(800.0, 300.0), zoom: 0.6 };
    steps(&mut h, 3);

    // Comme sur la capture : le nouveau terminal lâché en plein sur l'autre.
    let target = screen_of(&h, h.state().world.card(&big).unwrap().rect).center();
    let from = screen_of(&h, h.state().world.card(&new).unwrap().rect).center();
    drag_card_by(&mut h, &new, target - from);
    let w = &h.state().world;
    let (a, b) = (w.card(&big).unwrap().rect, w.card(&new).unwrap().rect);
    assert!(a.center().distance(b.center()) > 10.0 && !a.intersects(b), "aucun chevauchement : {a:?} / {b:?}");
    assert!((b.center() - w.camera.to_world(target, h.state().viewport())).length() < 40.0, "le terminal posé reste où on l'a lâché");
    assert_eq!(project_rect(&h, "p-api"), members_fit(&h, "p-api"), "le cadre se réajuste");
}

// --- Filtre de la barre du haut --------------------------------------------------------------

fn set_status(h: &mut Harness<'_, App>, id: &str, status: TermStatus) {
    let session = h.state().world.card(id).unwrap().session();
    let info = SessionInfo { session, status, command: None, cwd: None, last_exit: None, last_duration_secs: None, tail: vec![] };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Session(info))]);
}

/// Clic souris réel au centre d'un widget, pendant que `tick` change l'état à chaque frame.
fn click_label_while(h: &mut Harness<'_, App>, label: &str, mut tick: impl FnMut(&mut Harness<'_, App>, usize)) {
    let p = h.get_by_label_contains(label).rect().center();
    for (k, pressed) in [None, Some(true), Some(false)].into_iter().enumerate() {
        h.event(egui::Event::PointerMoved(p));
        if let Some(pressed) = pressed {
            h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        }
        tick(h, k);
        h.step();
    }
}

#[test]
fn status_filter_frames_its_terminals_and_clearing_it_brings_the_view_back() {
    let dir = temp_dir("filter-status");
    let mut h = harness(&dir);
    let stuck = add_card(&mut h, "bloque", pos2(6000.0, 4000.0));
    let busy = add_card(&mut h, "occupe", pos2(0.0, 0.0));
    let idle = add_card(&mut h, "calme", pos2(700.0, 0.0));
    set_status(&mut h, &stuck, TermStatus::Running);
    set_status(&mut h, &stuck, TermStatus::Stalled);
    set_status(&mut h, &idle, TermStatus::Idle);
    let before = tcanvas::shell::world::Camera { center: pos2(300.0, 150.0), zoom: 0.8 };
    h.state_mut().world.camera = before;
    steps(&mut h, 3);

    // Pendant le clic, « occupe » alterne actif / inactif (Claude au travail) : la barre ne bouge pas.
    let flip = |h: &mut Harness<'_, App>, k: usize| set_status(h, &busy, if k.is_multiple_of(2) { TermStatus::Running } else { TermStatus::Idle });
    click_label_while(&mut h, "bloqué 1", flip);
    steps(&mut h, 45);
    let stuck_rect = h.state().world.card(&stuck).unwrap().rect;
    assert!(h.state().filters.is_active());
    assert!(sees_whole(&h, stuck_rect), "la vue est cadrée sur le terminal bloqué");
    assert!(h.query_by_label_contains("× filtre (1/3)").is_some(), "compte des correspondances");

    // Re-cliquer la puce retire le filtre : tout réapparaît, et la vue d'avant revient.
    click_label_while(&mut h, "bloqué 1", flip);
    steps(&mut h, 45);
    assert!(!h.state().filters.is_active(), "filtre retiré");
    assert_eq!(h.state().world.camera, before, "retour à la vue d'avant le filtre");
}

#[test]
fn clear_button_removes_the_text_filter_too() {
    let dir = temp_dir("filter-clear");
    let mut h = harness(&dir);
    add_card(&mut h, "api", pos2(0.0, 0.0));
    add_card(&mut h, "blog", pos2(3000.0, 0.0));
    let before = h.state().world.camera;
    steps(&mut h, 2);
    ctrl_shift(&h, Key::F);
    steps(&mut h, 2);
    h.event(egui::Event::Text("blog".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 45);
    assert!(sees_whole(&h, h.state().world.cards[1].rect), "Entrée cadre les correspondances");
    h.get_by_label_contains("× filtre (1/2)").click();
    steps(&mut h, 45);
    assert!(!h.state().filters.is_active() && h.state().filters.text.is_empty());
    assert_eq!(h.state().world.camera, before);
}

// --- Ouvrir un terminal dans le groupe ouvert ------------------------------------------------

fn linked_groups(h: &mut Harness<'_, App>) {
    for (i, name) in ["api", "ai-screen-console", "facturation", "site-web"].iter().enumerate() {
        let id = add_project(h, name, Rect::from_min_size(pos2(i as f32 * 5000.0, 0.0), Vec2::new(1400.0, 500.0)));
        h.state_mut().world.projects.iter_mut().find(|p| p.id == id).unwrap().cwd = Some(format!("/home/me/{name}"));
    }
}

#[test]
fn the_toolbar_stays_two_rows_high_in_an_open_project() {
    let dir = temp_dir("toolbar-height");
    let mut h = harness(&dir);
    linked_groups(&mut h);
    // Un autre projet « en train d'écrire » : sa puce porte un ✎ (le glyphe doit exister).
    let now = tcanvas::shell::world::unix_now();
    h.state_mut().world.file_activity.insert("p-site-web".into(), now);
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    for _ in 0..30 {
        h.step();
        assert!(h.state().viewport().min.y < 70.0, "barre trop haute : {:?}", h.state().viewport());
    }
    assert!(h.query_by_label("● site-web ✎").is_some());
    let button = h.get_by_label("+ Terminal").rect();
    assert!(button.max.y < 70.0, "« + Terminal » visible en haut : {button:?}");
}

#[test]
fn every_way_to_open_a_terminal_works_inside_an_open_group() {
    let dir = temp_dir("group-new-terminal");
    let mut h = harness(&dir);
    linked_groups(&mut h);
    let first = add_card(&mut h, "premier", pos2(40.0, 40.0));
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 45);
    let count = |h: &Harness<'_, App>| h.state().world.members("p-api").count();
    let newest = |h: &Harness<'_, App>| h.state().world.cards.last().unwrap().clone();

    // 1. Le bouton de l'étiquette du groupe.
    h.get_by_label("Nouveau terminal dans le groupe api").click();
    steps(&mut h, 45);
    assert_eq!(count(&h), 2);
    let card = newest(&h);
    assert_eq!(card.cwd.as_deref(), Some("/home/me/api"), "dans le dossier lié");
    let first_rect = h.state().world.card(&first).unwrap().rect;
    assert!(card.rect.min.x > first_rect.max.x && !card.rect.intersects(first_rect), "posé à côté, sans chevauchement");

    // 2. « + Terminal » de la barre du haut, et 3. Ctrl+Maj+N : dans le groupe ouvert.
    h.get_by_label("+ Terminal").click();
    steps(&mut h, 45);
    ctrl_shift(&h, Key::N);
    steps(&mut h, 45);
    assert_eq!(count(&h), 4);
    let rects: Vec<Rect> = h.state().world.members("p-api").map(|c| c.rect).collect();
    for i in 0..rects.len() {
        for j in i + 1..rects.len() {
            assert!(!rects[i].intersects(rects[j]), "chevauchement {i}/{j}");
        }
    }
    assert!(h.state().world.members("p-api").all(|c| h.state().world.shows_card(c)), "tous visibles dans l'espace");
}

#[test]
fn an_empty_group_offers_to_open_a_terminal() {
    let dir = temp_dir("group-empty");
    let mut h = harness(&dir);
    linked_groups(&mut h);
    steps(&mut h, 2);
    h.get_by_label("● facturation").click();
    steps(&mut h, 45);
    h.get_by_label("+ Ouvrir un terminal").click();
    steps(&mut h, 45);
    let card = h.state().world.cards.last().expect("terminal créé").clone();
    assert_eq!((card.project.as_deref(), card.cwd.as_deref()), (Some("p-facturation"), Some("/home/me/facturation")));
    assert!(h.query_by_label("+ Ouvrir un terminal").is_none(), "plus vide : le gros bouton disparaît");
    assert_eq!(h.state().world.focused.as_deref(), Some(card.id.as_str()), "le clavier va au nouveau terminal");
}

// --- Molette : seul ce qui est sous le pointeur défile ---------------------------------------

/// Un cran de molette à `p`, puis assez de frames pour écouler le défilement lissé.
fn wheel_at(h: &mut Harness<'_, App>, p: Pos2, dy: f32) {
    h.event(egui::Event::PointerMoved(p));
    h.step();
    h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, dy), modifiers: Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel });
    steps(h, 40);
}

#[test]
fn scrolling_over_a_terminal_or_a_window_never_moves_the_canvas() {
    let dir = temp_dir("wheel");
    let mut h = harness(&dir);
    let focused = add_card(&mut h, "focalise", pos2(-600.0, -200.0));
    let other = add_card(&mut h, "autre", pos2(100.0, -200.0));
    h.state_mut().dispatch(vec![Event::FocusCard(focused.clone())]);
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(0.0, 0.0), zoom: 1.0 };
    steps(&mut h, 3);
    let cam = h.state().world.camera;
    let center_of = |h: &Harness<'_, App>, id: &str| screen_of(h, h.state().world.card(id).unwrap().rect).center();

    for id in [&focused, &other] {
        let p = center_of(&h, id);
        wheel_at(&mut h, p, -120.0);
        assert_eq!(h.state().world.camera, cam, "molette sur le terminal {id} : la vue ne bouge pas");
    }

    // Une fenêtre par-dessus le canevas (palette « aller à ») : elle garde la molette.
    ctrl_shift(&h, Key::K);
    steps(&mut h, 2);
    let p = h.state().viewport().center();
    wheel_at(&mut h, p, -120.0);
    assert_eq!(h.state().world.camera, cam, "molette sur la palette");
    h.key_press(Key::Escape);
    steps(&mut h, 3);

    // Témoin : sur le vide, la molette déplace bien la vue.
    let empty = h.state().viewport().center() + Vec2::new(0.0, 330.0);
    wheel_at(&mut h, empty, -120.0);
    assert_ne!(h.state().world.camera, cam, "sur le vide, la vue défile");
}

#[test]
fn scrolling_the_changes_panel_never_moves_the_canvas() {
    let dir = temp_dir("wheel-panel");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    h.state_mut().world.projects[0].cwd = Some("~/api".into());
    add_card(&mut h, "a", pos2(40.0, 40.0));
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 3);
    let now = tcanvas::shell::world::unix_now();
    let files: Vec<(String, u64)> = (0..60).map(|i| (format!("src/f{i}.rs"), now - i)).collect();
    let files: Vec<(&str, u64)> = files.iter().map(|(p, t)| (p.as_str(), *t)).collect();
    h.state_mut().dispatch(vec![Event::Agent(changes("p-api", &files))]);
    steps(&mut h, 2);
    h.get_by_label("Δ Changements (60)").click();
    steps(&mut h, 45);
    let cam = h.state().world.camera;
    let p = h.get_by_label("Fichier modifié src/f3.rs").rect().center();
    wheel_at(&mut h, p, -300.0);
    assert_eq!(h.state().world.camera, cam);
}

// --- Rien ne se chevauche jamais --------------------------------------------------------------

/// Groupes (avec leur étiquette) et terminaux hors groupe : aucun chevauchement.
fn assert_nothing_overlaps(h: &Harness<'_, App>) {
    let w = &h.state().world;
    let mut bodies: Vec<(String, Rect)> = w.projects.iter().map(|p| (p.name.clone(), Rect::from_min_max(p.rect.min - Vec2::new(0.0, 40.0), p.rect.max))).collect();
    bodies.extend(w.cards.iter().filter(|c| c.project.is_none()).map(|c| (c.title(), c.rect)));
    for i in 0..bodies.len() {
        for j in i + 1..bodies.len() {
            assert!(!bodies[i].1.intersects(bodies[j].1), "{} chevauche {}", bodies[i].0, bodies[j].0);
        }
    }
    for p in &w.projects {
        let members: Vec<Rect> = w.members(&p.id).map(|c| c.rect).collect();
        for i in 0..members.len() {
            for j in i + 1..members.len() {
                assert!(!members[i].intersects(members[j]), "deux terminaux de {} se chevauchent", p.name);
            }
        }
    }
}

#[test]
fn opening_terminals_in_a_group_pushes_neighbouring_groups_instead_of_overlapping_them() {
    let dir = temp_dir("no-overlap");
    let mut h = harness(&dir);
    // Deux groupes serrés côte à côte, et un terminal libre juste en dessous.
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0)));
    add_project(&mut h, "web", Rect::from_min_size(pos2(700.0, 0.0), Vec2::new(600.0, 400.0)));
    add_card(&mut h, "a", pos2(40.0, 40.0));
    add_card(&mut h, "w", pos2(740.0, 40.0));
    add_card(&mut h, "libre", pos2(0.0, 900.0));
    steps(&mut h, 3);
    h.get_by_label("● api").click();
    steps(&mut h, 45);
    for _ in 0..3 {
        // Re-cliquer la puce du groupe ouvert le recadre en entier (étiquette visible).
        h.get_by_label("● api").click();
        steps(&mut h, 45);
        h.get_by_label("Nouveau terminal dans le groupe api").click();
        steps(&mut h, 45);
        assert_nothing_overlaps(&h);
    }
    assert_eq!(h.state().world.members("p-api").count(), 4);

    // Dans « Tous » : un nouveau terminal libre double-cliqué contre un cadre s'écarte aussi.
    h.get_by_label("Tous").click();
    steps(&mut h, 3);
    ctrl_shift(&h, Key::Num0);
    steps(&mut h, 45);
    let api = screen_of(&h, project_rect(&h, "p-api"));
    // Juste sous le cadre d'api : un terminal centré là déborderait sur le groupe.
    let p = pos2(api.left_bottom().x + 30.0, api.max.y + 12.0);
    assert!(h.state().world.drop_target(h.state().world.camera.to_world(p, h.state().viewport())).is_none(), "point hors de tout cadre");
    h.drag_at(p);
    h.step();
    h.drop_at(p);
    h.step();
    h.drag_at(p);
    h.step();
    h.drop_at(p);
    steps(&mut h, 45);
    assert_eq!(h.state().world.cards.len(), 7, "terminal créé");
    assert_nothing_overlaps(&h);
}

/// Comme l'application réelle : on ne redessine que si quelque chose l'a demandé. Un clic qui
/// lance un vol de caméra doit suffire, sans autre mouvement de souris.
#[test]
fn one_click_on_a_filter_is_enough_to_frame_its_terminals() {
    let dir = temp_dir("filter-one-click");
    let mut h = harness(&dir);
    let stuck = add_card(&mut h, "bloque", pos2(6000.0, 4000.0));
    add_card(&mut h, "calme", pos2(0.0, 0.0));
    set_status(&mut h, &stuck, TermStatus::Running);
    set_status(&mut h, &stuck, TermStatus::Stalled);
    steps(&mut h, 3);
    let p = h.get_by_label_contains("bloqué 1").rect().center();
    h.event(egui::Event::PointerMoved(p));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    // Plus aucun événement : seules les frames demandées par l'application s'exécutent.
    let _ = h.try_run();
    let stuck_rect = h.state().world.card(&stuck).unwrap().rect;
    assert!(sees_whole(&h, stuck_rect), "un seul clic cadre le terminal bloqué");
}

#[test]
fn one_click_on_a_filter_frames_its_terminals_inside_an_open_group() {
    let dir = temp_dir("filter-one-click-space");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(2400.0, 500.0)));
    h.state_mut().world.projects[0].cwd = Some("~/api".into());
    let a = add_card(&mut h, "a", pos2(40.0, 40.0));
    let b = add_card(&mut h, "b", pos2(700.0, 40.0));
    let c = add_card(&mut h, "c", pos2(1400.0, 40.0));
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 45);
    for id in [&a, &b] {
        set_status(&mut h, id, TermStatus::Idle);
    }
    set_status(&mut h, &c, TermStatus::Running);
    h.state_mut().dispatch(vec![Event::FocusCard(a.clone())]);
    // On regarde ailleurs dans le groupe, zoomé sur « a ».
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: h.state().world.card(&a).unwrap().rect.center(), zoom: 1.0 };
    let _ = h.try_run();
    let p = h.get_by_label_contains("actif 1").rect().center();
    h.event(egui::Event::PointerMoved(p));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    let _ = h.try_run();
    let rect = h.state().world.card(&c).unwrap().rect;
    let cam = h.state().world.camera;
    assert!(sees_whole(&h, rect), "un seul clic cadre le terminal actif : cam={cam:?} rect={rect:?}");
}

#[test]
fn one_click_on_a_project_chip_opens_it_centered_even_after_wandering_off() {
    let dir = temp_dir("chip-one-click");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    add_project(&mut h, "web", Rect::from_min_size(pos2(9000.0, 0.0), Vec2::new(1400.0, 500.0)));
    add_card(&mut h, "a", pos2(40.0, 40.0));
    add_card(&mut h, "w", pos2(9040.0, 40.0));
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 3);
    // On se promène loin dans api, on passe à web, puis un seul clic pour revenir à api.
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(-40_000.0, 30_000.0), zoom: 3.0 };
    steps(&mut h, 2);
    h.get_by_label("● web").click();
    steps(&mut h, 3);
    assert!(sees_whole(&h, project_rect(&h, "p-web")));
    h.get_by_label("● api").click();
    steps(&mut h, 3);
    assert!(sees_whole(&h, project_rect(&h, "p-api")), "un seul clic : api centré et entier");
}
