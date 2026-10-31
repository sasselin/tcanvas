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

fn sees_whole(h: &Harness<'_, App>, world_rect: Rect) -> bool {
    h.state().viewport().contains_rect(screen_of(h, world_rect).shrink(1.0))
}

#[test]
fn ctrl_shift_digit_flies_to_that_project_even_from_a_terminal() {
    let dir = temp_dir("project-digit");
    let mut h = harness(&dir);
    let far = Rect::from_min_size(pos2(30_000.0, -8_000.0), Vec2::new(2000.0, 1200.0));
    add_project(&mut h, "un", Rect::from_min_size(Pos2::ZERO, Vec2::splat(800.0)));
    add_project(&mut h, "deux", far);
    let id = add_card(&mut h, "dans-un", pos2(100.0, 100.0));
    h.state_mut().dispatch(vec![Event::FocusCard(id)]);
    steps(&mut h, 2);
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
    add_project(&mut h, "facturation", far);
    add_card(&mut h, "api", Pos2::ZERO);
    steps(&mut h, 2);
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
    assert!(sees_whole(&h, infra), "le projet est cadré dès l'ouverture");
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

fn double_click_project_title(h: &mut Harness<'_, App>, rect: Rect) {
    let p = screen_of(h, rect).min + Vec2::new(20.0, -16.0);
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
    double_click_project_title(&mut h, rect);
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

    double_click_project_title(&mut h, rect);
    h.event(egui::Event::Text("Abandonné".into()));
    steps(&mut h, 2);
    h.key_press(Key::Escape);
    steps(&mut h, 3);
    assert_eq!(project_name(&h, &id), "Original");

    steps(&mut h, 30); // sinon les deux doubles-clics forment un triple-clic
    double_click_project_title(&mut h, rect);
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
    double_click_project_title(&mut h, rect);
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
