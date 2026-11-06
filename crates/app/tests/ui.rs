//! Headless UI tests (egui_kittest): the real app, without PTYs or agent.

use std::path::PathBuf;

use egui::{Key, Modifiers, Pos2, Rect, Vec2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use protocol::{AgentEvent, SessionInfo, TermStatus};
use tcanvas::features::agent_link::AgentMode;
use tcanvas::features::terminal_card::Launcher;
use tcanvas::shell::event::Event;
use tcanvas::shell::i18n::{self, Lang};
use tcanvas::shell::world::{Card, World};
use tcanvas::{App, Config};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tcanvas-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Tests check the French UI, whatever the machine's language.
fn harness(dir: &std::path::Path) -> Harness<'static, App> {
    harness_in(dir, Lang::Fr)
}

fn harness_in(dir: &std::path::Path, lang: Lang) -> Harness<'static, App> {
    i18n::set(lang);
    let dir = dir.to_path_buf();
    Harness::builder()
        .with_size(Vec2::new(1400.0, 900.0))
        .with_step_dt(1.0 / 60.0)
        .build_eframe(move |cc| App::new(&cc.egui_ctx, Config { launcher: Launcher::Disabled, agent: AgentMode::Disabled, config_dir: dir, os_toasts: false, update_check: false }))
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
    // Placed inside a frame: member of that group (as if dropped there).
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
    assert!(app.world.cards[0].rect.expand(200.0).contains(world_point), "card created near the double-click");
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
    assert_eq!(h.state().filters.text, "", "typed text does not leak into the search");
    ctrl_shift(&h, Key::N);
    steps(&mut h, 3);
    assert_eq!(h.state().world.cards.len(), 2, "Ctrl+Shift+N works even with a focused terminal");
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
    let far = add_card(&mut h, "database", pos2(20_000.0, 9_000.0));
    steps(&mut h, 2);
    ctrl_shift(&h, Key::P);
    steps(&mut h, 2);
    assert!(h.state().jump_open());
    h.event(egui::Event::Text("data".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 45);
    let app = h.state();
    assert!(!app.jump_open());
    assert_eq!(app.world.focused.as_deref(), Some(far.as_str()), "focus after the flight");
    let card = app.world.card(&far).unwrap();
    assert!(app.viewport().contains_rect(screen_of(&h, card.rect).shrink(1.0)), "the card is fully on screen");
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
    assert!((moved - Vec2::new(200.0, 100.0) / zoom).length() < 25.0, "moved {moved:?}");
    assert_eq!(after.size(), before.size());
}

/// Two groups side by side, each with a terminal, seen in "All".
fn two_linked_groups(h: &mut Harness<'_, App>) -> (String, String) {
    let api = add_project(h, "api", Rect::from_min_size(pos2(-900.0, -200.0), Vec2::new(600.0, 400.0)));
    add_card(h, "a", pos2(-850.0, -150.0));
    let web = add_project(h, "web", Rect::from_min_size(pos2(300.0, -200.0), Vec2::new(600.0, 400.0)));
    add_card(h, "b", pos2(350.0, -150.0));
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: Pos2::ZERO, zoom: 0.6 };
    steps(h, 2);
    (api, web)
}

/// Left button press, moves and release, with `mods` held.
fn drag_with(h: &mut Harness<'_, App>, from: Pos2, to: Pos2, mods: Modifiers) {
    h.event(egui::Event::ModifiersChanged(mods));
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: mods });
    h.step();
    for k in 1..=10 {
        h.hover_at(from + (to - from) * (k as f32 / 10.0));
        h.step();
    }
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: mods });
    h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    steps(h, 2);
}

/// A point on a group's label (left part, away from its × button).
fn label_of(h: &Harness<'_, App>, name: &str) -> Pos2 {
    let folder = h.get_by_label(&format!("Dossier du groupe {name}")).rect();
    let frame = screen_of(h, h.state().world.project(&format!("p-{name}")).unwrap().rect);
    pos2(frame.min.x + 12.0, folder.center().y)
}

#[test]
fn alt_dragging_a_group_label_onto_another_group_links_them() {
    let dir = temp_dir("wire-draw");
    let mut h = harness(&dir);
    let (api, web) = two_linked_groups(&mut h);
    let rect_before = h.state().world.project(&api).unwrap().rect;
    let into_web = screen_of(&h, h.state().world.project(&web).unwrap().rect).center();

    let label = label_of(&h, "api");
    drag_with(&mut h, label, into_web, Modifiers::ALT);
    let app = h.state();
    assert_eq!(app.world.wires, vec![tcanvas::shell::world::Wire { from: api.clone(), to: web.clone() }]);
    assert_eq!(app.world.project(&api).unwrap().rect, rect_before, "Alt + drag does not move the group");

    // Released on empty canvas: no wire; a plain drag still moves the group.
    h.state_mut().world.wires.clear();
    let empty = screen_of(&h, Rect::from_min_size(pos2(-100.0, 400.0), Vec2::splat(10.0))).center();
    drag_with(&mut h, label, empty, Modifiers::ALT);
    assert!(h.state().world.wires.is_empty());
    drag_with(&mut h, label, label + Vec2::new(0.0, 60.0), Modifiers::NONE);
    assert_ne!(h.state().world.project(&api).unwrap().rect, rect_before, "plain drag moves the group");
}

#[test]
fn clicking_a_wire_frames_the_far_group_and_right_click_deletes_it() {
    let dir = temp_dir("wire");
    let mut h = harness(&dir);
    let (api, web) = two_linked_groups(&mut h);
    h.state_mut().dispatch(vec![Event::AddWire(api.clone(), web.clone())]);
    h.state_mut().world.camera.center = pos2(-600.0, 0.0);
    steps(&mut h, 2);
    // Point on the wire right next to the api frame: the far end is web.
    let near_api = screen_of(&h, h.state().world.project(&api).unwrap().rect).right_center() + Vec2::new(12.0, 0.0);
    h.hover_at(near_api);
    h.step();
    h.drag_at(near_api);
    h.step();
    h.drop_at(near_api);
    steps(&mut h, 45);
    assert!(sees_whole(&h, h.state().world.project(&web).unwrap().rect), "the camera frames the other group");

    let near_web = screen_of(&h, h.state().world.project(&web).unwrap().rect).left_center() - Vec2::new(12.0, 0.0);
    h.hover_at(near_web);
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: near_web, button: egui::PointerButton::Secondary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    steps(&mut h, 2);
    assert!(h.state().world.wires.is_empty(), "right-click deletes the wire");
}

#[test]
fn a_project_space_shows_every_group_of_its_chain_in_one_row_under_its_label() {
    let dir = temp_dir("wire-chip");
    let mut h = harness(&dir);
    let (api, web) = two_linked_groups(&mut h);
    let db = add_project(&mut h, "db", Rect::from_min_size(pos2(1500.0, 900.0), Vec2::new(400.0, 300.0)));
    add_project(&mut h, "alone", Rect::from_min_size(pos2(-900.0, 900.0), Vec2::new(400.0, 300.0)));
    h.state_mut().dispatch(vec![Event::AddWire(api.clone(), web.clone()), Event::AddWire(web.clone(), db.clone())]);
    steps(&mut h, 2);
    assert!(h.query_by_label("⇄ web").is_none(), "no chips in \"All\": the wires are drawn");

    ctrl_shift(&h, Key::Num1);
    steps(&mut h, 45);
    assert_eq!(h.state().world.space.as_deref(), Some(api.as_str()));
    // db is linked through web: it can be reached directly. Unlinked groups are not listed.
    let (db_chip, web_chip) = (widget_rect(&h, "⇄ db"), widget_rect(&h, "⇄ web"));
    assert!(h.query_by_label("⇄ alone").is_none());
    assert!((db_chip.min.y - web_chip.min.y).abs() < 1.0 && db_chip.max.x < web_chip.min.x, "one row, by name: {db_chip:?} {web_chip:?}");
    let label = widget_rect(&h, "Fermer le groupe api");
    let frame = screen_of(&h, project_rect(&h, &api));
    assert!(db_chip.min.y > label.max.y && db_chip.max.y <= frame.min.y + 1.0, "between the label and the frame: {label:?} {db_chip:?} {frame:?}");

    h.get_by_label("⇄ db").click();
    steps(&mut h, 3);
    assert_eq!(h.state().world.space.as_deref(), Some(db.as_str()));
    assert!(h.query_by_label("⇄ api").is_some() && h.query_by_label("⇄ web").is_some());
}

#[test]
fn with_the_free_grid_the_linked_groups_sit_under_the_pinned_bar() {
    let dir = temp_dir("wire-chip-grid");
    let mut h = harness_in(&dir, Lang::En);
    let (api, web) = two_linked_groups(&mut h);
    h.state_mut().dispatch(vec![Event::AddWire(api.clone(), web.clone())]);
    ctrl_shift(&h, Key::Num1);
    steps(&mut h, 45);
    h.get_by_label("Free grid").click();
    steps(&mut h, 3);
    let bar = widget_rect(&h, "Close group api");
    let chip = widget_rect(&h, "⇄ web");
    assert!(chip.min.y > bar.max.y && chip.min.y < bar.max.y + 20.0 && chip.min.x < bar.min.x + 200.0, "under the pinned bar: {bar:?} {chip:?}");
    // Still there wherever the camera goes.
    h.state_mut().world.camera.center += Vec2::new(3000.0, 2000.0);
    steps(&mut h, 2);
    assert_eq!(widget_rect(&h, "⇄ web"), chip);
    h.get_by_label("⇄ web").click();
    steps(&mut h, 3);
    assert_eq!(h.state().world.space.as_deref(), Some(web.as_str()));
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
    assert!(!app.world.card(&id).unwrap().attention, "looking at the terminal clears the alert");
    assert_eq!(app.pending_toasts(), 0);
}

#[test]
fn missing_tmux_is_shown_in_the_status_bar() {
    let tmux_missing = || Event::Agent(AgentEvent::Error { problem: protocol::Problem::TmuxMissing { detail: "No such file or directory".into() } });
    for (lang, label) in [(Lang::Fr, "✖ tmux absent"), (Lang::En, "✖ tmux missing")] {
        let dir = temp_dir("tmux-missing");
        let mut h = harness_in(&dir, lang);
        steps(&mut h, 2);
        assert!(h.query_by_label(label).is_none());
        h.state_mut().dispatch(vec![tmux_missing()]);
        steps(&mut h, 2);
        h.get_by_label(label);
    }
}

#[test]
fn missing_shell_hooks_can_be_installed_from_the_status_bar() {
    let hello = |hooks| Event::Agent(AgentEvent::Hello { version: "1".into(), hooks });
    let installs = |h: &Harness<'_, App>| h.state().agent_sent().iter().filter(|c| **c == protocol::AgentCmd::InstallHooks).count();
    for (lang, label, button) in [(Lang::Fr, "⚠ hooks du shell", "Installer"), (Lang::En, "⚠ shell hooks", "Install")] {
        let dir = temp_dir("hooks");
        let mut h = harness_in(&dir, lang);
        h.state_mut().dispatch(vec![hello(protocol::Hooks::Unsupported)]);
        steps(&mut h, 2);
        assert!(h.query_by_label(label).is_none(), "fish & co: nothing to offer");

        h.state_mut().dispatch(vec![hello(protocol::Hooks::Missing)]);
        steps(&mut h, 2);
        h.get_by_label(label);
        h.get_by_label(button).click();
        steps(&mut h, 2);
        assert_eq!(installs(&h), 1);
        h.get_by_label(button).click();
        steps(&mut h, 2);
        assert_eq!(installs(&h), 1, "disabled while the agent works");

        // A failure re-enables the button.
        h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Error { problem: protocol::Problem::HooksFailed { detail: "read-only".into() } })]);
        steps(&mut h, 2);
        h.get_by_label(button).click();
        steps(&mut h, 2);
        assert_eq!(installs(&h), 2);

        h.state_mut().dispatch(vec![Event::Agent(AgentEvent::HooksChanged { hooks: protocol::Hooks::Installed })]);
        steps(&mut h, 2);
        assert!(h.query_by_label(label).is_none());
        assert!(h.query_by_label(button).is_none());
    }
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
        id = add_card(&mut h, "persistent", pos2(123.0, 456.0));
        h.state_mut().dispatch(vec![Event::NewProject { at: Some(Pos2::ZERO) }]);
        h.state_mut().world.camera.zoom = 0.5;
        steps(&mut h, 2);
        h.state_mut().save_now();
    }
    let mut h = harness(&dir);
    steps(&mut h, 2);
    let app = h.state();
    let card = app.world.card(&id).expect("card restored");
    assert_eq!(card.name.as_deref(), Some("persistent"));
    assert_eq!(card.rect.min, pos2(123.0, 456.0));
    assert_eq!(app.world.projects.len(), 1);
    assert_eq!(app.world.camera.zoom, 0.5);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn new_terminal_when_zoomed_out_flies_in_to_a_usable_size() {
    // Real case: view zoomed out to 0.37 → the new card stayed a "sass · idle" preview.
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
    let id = add_card(&mut h, "far", Pos2::ZERO);
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

/// Current frame of the group (fitted to its members).
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
    add_project(&mut h, "one", Rect::from_min_size(Pos2::ZERO, Vec2::splat(800.0)));
    let two = add_project(&mut h, "two", far);
    let id = add_card(&mut h, "in-one", pos2(100.0, 100.0));
    h.state_mut().dispatch(vec![Event::FocusCard(id)]);
    steps(&mut h, 2);
    let far = project_rect(&h, &two);
    assert!(!sees_whole(&h, far));
    // Physical key "2" (on AZERTY the logical key is U+00E9).
    h.event(egui::Event::Key { key: Key::Quote, physical_key: Some(Key::Num2), pressed: true, repeat: false, modifiers: Modifiers::CTRL | Modifiers::SHIFT });
    steps(&mut h, 45);
    assert!(sees_whole(&h, far), "project 2 is framed");
}

#[test]
fn palette_finds_projects() {
    let dir = temp_dir("project-palette");
    let mut h = harness(&dir);
    let far = Rect::from_min_size(pos2(-25_000.0, 12_000.0), Vec2::new(1500.0, 900.0));
    let billing = add_project(&mut h, "billing", far);
    add_card(&mut h, "api", Pos2::ZERO);
    steps(&mut h, 2);
    let far = project_rect(&h, &billing);
    ctrl_shift(&h, Key::P);
    steps(&mut h, 2);
    h.event(egui::Event::Text("bill".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 45);
    assert!(sees_whole(&h, far));
}

#[test]
fn dragging_a_project_chip_reorders_the_groups() {
    let dir = temp_dir("project-chip-drag");
    let mut h = harness(&dir);
    for (n, name) in ["api", "web", "db"].into_iter().enumerate() {
        add_project(&mut h, name, Rect::from_min_size(pos2(n as f32 * 2000.0, 0.0), Vec2::new(600.0, 400.0)));
    }
    steps(&mut h, 2);
    let order = |h: &Harness<'_, App>| h.state().world.projects.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    let (api, db) = (widget_rect(&h, "● api"), widget_rect(&h, "● db"));

    drag_with(&mut h, api.center(), pos2(db.max.x + 10.0, db.center().y), Modifiers::NONE);
    assert_eq!(order(&h), ["web", "db", "api"], "dropped after the last chip");
    assert_eq!(h.state().world.space, None, "a drag does not open the space");
    let (web, api) = (widget_rect(&h, "● web"), widget_rect(&h, "● api"));
    drag_with(&mut h, api.center(), pos2(web.min.x - 2.0, web.center().y), Modifiers::NONE);
    assert_eq!(order(&h), ["api", "web", "db"], "dropped before the first chip");

    // A plain click still opens the space without moving anything.
    h.get_by_label("● db").click();
    steps(&mut h, 2);
    assert_eq!((order(&h), h.state().world.space.as_deref()), (vec!["api".to_string(), "web".into(), "db".into()], Some("p-db")));
}

#[test]
fn right_clicking_a_project_chip_sorts_the_groups_by_name() {
    for (lang, item) in [(Lang::Fr, "Trier les groupes de A à Z"), (Lang::En, "Sort groups A to Z")] {
        let dir = temp_dir("project-chip-sort");
        let mut h = harness_in(&dir, lang);
        for (n, name) in ["web", "db", "api"].into_iter().enumerate() {
            add_project(&mut h, name, Rect::from_min_size(pos2(n as f32 * 2000.0, 0.0), Vec2::new(600.0, 400.0)));
        }
        steps(&mut h, 2);
        let chip = widget_rect(&h, "● db").center();
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: chip, button: egui::PointerButton::Secondary, pressed, modifiers: Modifiers::NONE });
        }
        steps(&mut h, 2);
        h.get_by_label(item).click();
        steps(&mut h, 2);
        let order: Vec<_> = h.state().world.projects.iter().map(|p| p.name.clone()).collect();
        assert_eq!(order, ["api", "db", "web"], "{lang:?}");
        assert_eq!(h.state().world.space, None, "sorting does not open a space");
    }
}

#[test]
fn clicking_a_project_chip_opens_that_project_alone() {
    let dir = temp_dir("project-chip");
    let mut h = harness(&dir);
    let infra = Rect::from_min_size(pos2(9_000.0, 9_000.0), Vec2::new(1200.0, 800.0));
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0)));
    add_project(&mut h, "infra", infra);
    add_card(&mut h, "server", pos2(100.0, 100.0));
    add_card(&mut h, "deploy", pos2(9_100.0, 9_100.0));
    add_card(&mut h, "orphan", pos2(-20_000.0, 0.0));
    steps(&mut h, 2);
    let shown = |h: &Harness<'_, App>| ["server", "deploy", "orphan"].map(|n| h.query_by_label(&format!("Fermer le terminal {n}")).is_some());

    h.get_by_label("● infra").click();
    steps(&mut h, 3);
    assert_eq!(h.state().world.space.as_deref(), Some("p-infra"));
    assert!(sees_whole(&h, project_rect(&h, "p-infra")), "the project is framed as soon as it opens");
    assert_eq!(shown(&h), [false, true, false], "one project at a time");
    assert!(h.query_by_label("Fermer le groupe api").is_none(), "the other projects' frames are hidden");
    // Even looking exactly where the others are, nothing shows up or can be clicked.
    for at in [pos2(600.0, 400.0), pos2(-19_750.0, 150.0)] {
        h.state_mut().world.camera = tcanvas::shell::world::Camera { center: at, zoom: 1.0 };
        steps(&mut h, 2);
        assert_eq!(shown(&h), [false, false, false], "view on {at:?}");
        assert!(h.query_by_label("Fermer le groupe api").is_none());
    }
    // Control: in "All", the same view does show the terminal.
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
    assert!(h.query_by_label("Fermer le groupe api").is_some() && h.query_by_label("Fermer le groupe infra").is_some(), "\"All\" shows everything");
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
    // Push the view far from the frame: a terminal created there must still stay in the project.
    h.state_mut().world.camera.center = pos2(4_000.0, 3_000.0);
    ctrl_shift(&h, Key::N);
    steps(&mut h, 45);
    let app = h.state();
    let card = app.world.cards.last().expect("terminal created");
    assert!(app.world.shows_card(card), "visible in the open space");
    assert_eq!(app.world.project_of(card).map(|p| p.id.as_str()), Some("p-api"));
}

#[test]
fn going_to_a_hidden_terminal_opens_its_project() {
    let dir = temp_dir("space-jump");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0)));
    add_project(&mut h, "infra", Rect::from_min_size(pos2(9_000.0, 9_000.0), Vec2::new(1200.0, 800.0)));
    let target = add_card(&mut h, "deploy", pos2(9_100.0, 9_100.0));
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 3);
    ctrl_shift(&h, Key::P);
    steps(&mut h, 2);
    h.event(egui::Event::Text("deploy".into()));
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
    let id = add_project(&mut h, "Project 1", rect);
    steps(&mut h, 2);
    double_click_project_title(&mut h, &id);
    // The whole name is selected: typing replaces it.
    h.event(egui::Event::Text("Backend".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 3);
    assert_eq!(project_name(&h, &id), "Backend");
    assert!(h.state().world.cards.is_empty(), "the double-click does not create a terminal");
}

#[test]
fn escape_cancels_and_clicking_elsewhere_keeps_the_new_name() {
    let dir = temp_dir("rename-cancel");
    let mut h = harness(&dir);
    let rect = Rect::from_min_size(pos2(-400.0, -200.0), Vec2::new(900.0, 500.0));
    let id = add_project(&mut h, "Original", rect);
    steps(&mut h, 2);

    double_click_project_title(&mut h, &id);
    h.event(egui::Event::Text("Discarded".into()));
    steps(&mut h, 2);
    h.key_press(Key::Escape);
    steps(&mut h, 3);
    assert_eq!(project_name(&h, &id), "Original");

    steps(&mut h, 30); // otherwise the two double-clicks make a triple-click
    double_click_project_title(&mut h, &id);
    assert!(h.state().projects_editing(), "the second double-click starts editing");
    h.event(egui::Event::Text("Kept".into()));
    steps(&mut h, 2);
    let elsewhere = h.state().viewport().max - Vec2::new(400.0, 300.0);
    h.drag_at(elsewhere);
    h.step();
    h.drop_at(elsewhere);
    steps(&mut h, 3);
    assert_eq!(project_name(&h, &id), "Kept");
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
    h.event(egui::Event::Text("Name".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 3);
    assert_eq!(project_name(&h, &id), "Name");
}

fn click_at(h: &mut Harness<'_, App>, p: Pos2) {
    h.hover_at(p);
    h.step();
    h.drag_at(p);
    h.step();
    h.drop_at(p);
    steps(h, 3);
}

/// Center of the card's × button (square at the right of the title bar).
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
    assert!(h.query_by_label("Fermer « api » ?").is_some(), "confirmation shown");
    h.get_by_label("Annuler").click();
    steps(&mut h, 3);
    assert!(h.state().world.card(&id).is_some(), "Cancel keeps the terminal");

    let at = close_button(&h, &id);
    click_at(&mut h, at);
    h.get_by_label("Fermer").click();
    steps(&mut h, 3);
    assert!(h.state().world.card(&id).is_none(), "Close removes the terminal");
}

#[test]
fn ctrl_shift_w_closes_the_focused_terminal_after_confirmation() {
    let dir = temp_dir("close-shortcut");
    let mut h = harness(&dir);
    let keep = add_card(&mut h, "kept", pos2(-900.0, 0.0));
    let id = add_card(&mut h, "disposable", pos2(200.0, 0.0));
    h.state_mut().dispatch(vec![Event::FocusCard(id.clone())]);
    steps(&mut h, 2);

    ctrl_shift(&h, Key::W);
    steps(&mut h, 3);
    assert!(h.query_by_label("Fermer « disposable » ?").is_some());
    h.key_press(Key::Escape);
    steps(&mut h, 3);
    assert!(h.state().world.card(&id).is_some(), "Escape cancels");

    ctrl_shift(&h, Key::W);
    steps(&mut h, 3);
    h.key_press(Key::Enter);
    steps(&mut h, 3);
    assert!(h.state().world.card(&id).is_none(), "Enter confirms");
    assert!(h.state().world.card(&keep).is_some(), "only the focused terminal is closed");
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
    let group = add_project(&mut h, "disposable", rect);
    let inside_a = add_card(&mut h, "a", pos2(-550.0, -250.0));
    let inside_b = add_card(&mut h, "b", pos2(100.0, -250.0));
    let outside = add_card(&mut h, "outside", pos2(-550.0, 400.0));
    let other = add_project(&mut h, "other", Rect::from_min_size(pos2(2000.0, 0.0), Vec2::splat(400.0)));
    h.state_mut().dispatch(vec![Event::AddWire(group.clone(), other.clone())]);
    steps(&mut h, 2);

    h.get_by_label("Fermer le groupe disposable").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("Fermer le groupe « disposable » ?").is_some(), "confirmation shown");
    h.get_by_label("Annuler").click();
    steps(&mut h, 3);
    assert_eq!(h.state().world.cards.len(), 3);

    h.get_by_label("Fermer le groupe disposable").click();
    steps(&mut h, 3);
    h.get_by_label("Tout fermer").click();
    steps(&mut h, 3);
    let app = h.state();
    assert!(app.world.project(&group).is_none(), "frame removed");
    assert!(app.world.card(&inside_a).is_none() && app.world.card(&inside_b).is_none(), "group terminals closed");
    assert!(app.world.card(&outside).is_some(), "the terminal outside the group remains");
    assert!(app.world.wires.is_empty(), "the closed group's wires disappear");
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
    assert_ne!(cursor(&h), egui::CursorIcon::Grab, "no hand without Shift");

    hold_shift(&h, true);
    steps(&mut h, 2);
    assert_eq!(cursor(&h), egui::CursorIcon::Grab, "open hand with Shift");

    let cam_before = h.state().world.camera.center;
    let card_before = h.state().world.card(&id).unwrap().rect;
    h.event(egui::Event::PointerButton { pos: over_card, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::SHIFT });
    h.step();
    for k in 1..=10 {
        h.hover_at(over_card + Vec2::new(15.0 * k as f32, 8.0 * k as f32));
        h.step();
    }
    assert_eq!(cursor(&h), egui::CursorIcon::Grabbing, "closed hand while dragging");
    h.event(egui::Event::PointerButton { pos: over_card + Vec2::new(150.0, 80.0), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::SHIFT });
    hold_shift(&h, false);
    steps(&mut h, 2);

    let app = h.state();
    let moved = (app.world.camera.center - cam_before) * app.world.camera.zoom;
    assert!((moved + Vec2::new(150.0, 80.0)).length() < 20.0, "the view follows the hand: {moved:?}");
    assert_eq!(app.world.card(&id).unwrap().rect, card_before, "the card does not move");
    assert_ne!(app.world.focused.as_deref(), Some(id.as_str()), "Shift + drag does not select the terminal");
    assert_ne!(cursor(&h), egui::CursorIcon::Grab, "no hand once Shift is released");
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

// --- Groups fitted to their content, terminals changing groups ------------------------------

/// Drags the card's title bar by `delta` (screen pixels), in small steps, then drops it.
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

/// Union of the members + padding: what the frame must be.
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
    // Twenty small nudges toward the right edge: the frame follows the card, never beyond it.
    for _ in 0..20 {
        drag_card_by(&mut h, &b, Vec2::new(30.0, 0.0));
        assert_eq!(group_of(&h, &b).as_deref(), Some("p-api"), "stays in the group");
        assert_eq!(project_rect(&h, "p-api"), members_fit(&h, "p-api"));
    }
    let wide = project_rect(&h, "p-api");
    // It ended up out of view: recenter on it before bringing it back.
    h.state_mut().world.camera.center = h.state().world.card(&b).unwrap().rect.center();
    steps(&mut h, 2);
    // And bringing it back shrinks the frame.
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

    // Onto empty space ("All"): out of the group, and api's frame shrinks around "a".
    drag_card_by(&mut h, &b, Vec2::new(0.0, 230.0));
    assert_eq!(group_of(&h, &b), None);
    assert!(project_rect(&h, "p-api").width() < before.width());
    assert!(!project_rect(&h, "p-api").intersects(h.state().world.card(&b).unwrap().rect));

    // Onto web's frame: it becomes a member and the frame encloses it.
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
    assert!(project_rect(&h, "p-web").contains_rect(h.state().world.card(&a).unwrap().rect), "placed inside web's frame");
    assert!(h.query_by_label("Fermer le terminal a").is_none(), "it left the open space");
    // The toast says where it went; a click takes you there.
    h.get_by_label("➜ Déplacé vers « web »").click();
    steps(&mut h, 45);
    assert_eq!(h.state().world.space.as_deref(), Some("p-web"));
    assert_eq!(h.state().world.focused.as_deref(), Some(a.as_str()));
}

// --- Linked folder and "Changes" panel -------------------------------------------------------

fn changes(key: &str, files: &[(&str, u64)]) -> AgentEvent {
    AgentEvent::Changes(protocol::ChangeSet {
        key: key.into(),
        root: Some("/home/me/api".into()),
        git: true,
        branch: Some("main".into()),
        detached: false,
        error: None,
        files: files
            .iter()
            .map(|(path, mtime)| protocol::FileChange { path: (*path).into(), kind: protocol::ChangeKind::Modified, added: Some(3), removed: Some(1), mtime: *mtime, binary: false })
            .collect(),
        truncated: false,
        commits: vec![],
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

    // The picker opens on the group terminal's folder and asks the agent for it.
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
    assert!(h.query_by_label("📁 docs").is_some(), "subfolders listed");
    assert!(h.query_by_label("Récents :").is_some(), "shortcuts to recent folders");

    // Enter "web" (git repo), then link it.
    h.get_by_label("📁 web").click();
    steps(&mut h, 2);
    assert_eq!(h.state().agent_sent().last(), Some(&protocol::AgentCmd::ListDir { path: "/home/me/code/api/web".into() }));
    h.state_mut().dispatch(vec![listing("/home/me/code/api/web", &[])]);
    steps(&mut h, 2);
    assert!(h.query_by_label("⎇ dépôt git").is_some());
    h.get_by_label("Lier ce dossier").click();
    steps(&mut h, 3);
    assert!(!h.state().projects_editing(), "picker closed");
    let linked = "/home/me/code/api/web";
    assert_eq!(h.state().world.project("p-api").unwrap().cwd.as_deref(), Some(linked));
    assert!(h.state().agent_sent().contains(&protocol::AgentCmd::Watch { key: "p-api".into(), path: linked.into() }), "the agent watches the folder");

    // A terminal created in the group starts in that folder.
    steps(&mut h, 30); // otherwise the label click and the double-click make a triple-click
    // In the frame's padding, bottom-left corner (the screen's bottom-right is the minimap's).
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
    assert_ne!(card.id, a, "terminal created");
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
    steps(&mut h, 40);
    let card_rect = |h: &Harness<'_, App>| screen_of(h, h.state().world.cards[0].rect);
    let before = card_rect(&h);

    h.get_by_label("Δ Changements (2)").click();
    steps(&mut h, 3);
    let after = card_rect(&h);
    assert!((after.min - before.min).length() < 0.5, "opening the panel does not move the cards: {before:?} -> {after:?}");
    assert!(h.query_by_label("Fichier modifié src/main.rs").is_some() && h.query_by_label("Fichier modifié src/old.rs").is_some());
    assert!(h.query_by_label("⎇ main").is_some());
    // "Follow": the last touched file is shown automatically.
    assert_eq!(diff_requests(&h), vec!["src/main.rs"]);
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Diff { key: "p-api".into(), file: "src/main.rs".into(), text: "@@ -1 +1 @@\n-old\n+new\n".into(), truncated: false, problem: None })]);
    steps(&mut h, 3);
    assert!(h.query_by_label("+new").is_some() && h.query_by_label("-old").is_some(), "diff shown");

    // Picking a file by hand stops following.
    h.get_by_label("Fichier modifié src/old.rs").click();
    steps(&mut h, 3);
    assert_eq!(diff_requests(&h).last().map(String::as_str), Some("src/old.rs"));
    assert!(!h.state().changes.follow);
    assert_eq!(h.state().changes.selected("p-api"), Some("src/old.rs"));

    // The panel state is saved; the canvas makes room for it.
    assert!(h.state().world.changes_open);
    assert!(h.state().viewport().max.x < 1400.0 - 200.0, "the canvas makes room for the panel");
    h.get_by_label("Δ Changements (2)").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("Fichier modifié src/main.rs").is_none(), "panel closed");
}

#[test]
fn the_interface_follows_the_system_language() {
    let dir = temp_dir("english");
    let mut h = harness_in(&dir, Lang::En);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    h.state_mut().world.projects[0].cwd = Some("~/api".into());
    add_card(&mut h, "a", pos2(40.0, 40.0));
    steps(&mut h, 2);
    assert!(h.query_by_label("+ Project").is_some() && h.query_by_label("Show all").is_some());
    assert!(h.query_by_label("All").is_some(), "the overview chip");
    h.get_by_label("● api").click();
    steps(&mut h, 3);
    let now = tcanvas::shell::world::unix_now();
    h.state_mut().dispatch(vec![Event::Agent(changes("p-api", &[("src/main.rs", now - 1)]))]);
    steps(&mut h, 2);
    h.get_by_label("Δ Changes (1)").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("Follow").is_some() && h.query_by_label("Modified file src/main.rs").is_some());
    assert!(h.query_by_label("Suivre").is_none(), "no French left in the panel");
    // A problem reported by the agent is translated on the app side.
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Diff { key: "p-api".into(), file: "src/main.rs".into(), text: String::new(), truncated: false, problem: Some(protocol::Problem::NoDiff) })]);
    steps(&mut h, 3);
    assert!(h.query_by_label("No difference from HEAD.").is_some());
    h.get_by_label("Close terminal a").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("Close “a”?").is_some() && h.query_by_label("Cancel").is_some(), "confirmation dialog in English");
}

#[test]
fn a_terminal_dropped_onto_another_takes_its_place_and_the_group_rearranges() {
    let dir = temp_dir("group-settle");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    let big = add_card(&mut h, "big", pos2(40.0, 40.0));
    h.state_mut().world.card_mut(&big).unwrap().rect = Rect::from_min_size(pos2(40.0, 40.0), Vec2::new(900.0, 500.0));
    let new = add_card(&mut h, "new", pos2(1200.0, 40.0));
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(800.0, 300.0), zoom: 0.6 };
    steps(&mut h, 3);

    // As in the screenshot: the new terminal dropped right on top of the other.
    let target = screen_of(&h, h.state().world.card(&big).unwrap().rect).center();
    let from = screen_of(&h, h.state().world.card(&new).unwrap().rect).center();
    drag_card_by(&mut h, &new, target - from);
    let w = &h.state().world;
    let (a, b) = (w.card(&big).unwrap().rect, w.card(&new).unwrap().rect);
    assert!(a.center().distance(b.center()) > 10.0 && !a.intersects(b), "no overlap: {a:?} / {b:?}");
    assert!((b.center() - w.camera.to_world(target, h.state().viewport())).length() < 40.0, "the dropped terminal stays where it was dropped");
    assert_eq!(project_rect(&h, "p-api"), members_fit(&h, "p-api"), "the frame refits");
}

// --- Top bar filter ---------------------------------------------------------------------------

fn set_status(h: &mut Harness<'_, App>, id: &str, status: TermStatus) {
    let session = h.state().world.card(id).unwrap().session();
    let info = SessionInfo { session, status, command: None, cwd: None, last_exit: None, last_duration_secs: None, tail: vec![] };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Session(info))]);
}

/// Real mouse click at a widget's center, while `tick` changes the state every frame.
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
    let stuck = add_card(&mut h, "stuck", pos2(6000.0, 4000.0));
    let busy = add_card(&mut h, "busy", pos2(0.0, 0.0));
    let idle = add_card(&mut h, "calm", pos2(700.0, 0.0));
    set_status(&mut h, &stuck, TermStatus::Running);
    set_status(&mut h, &stuck, TermStatus::Stalled);
    set_status(&mut h, &idle, TermStatus::Idle);
    let before = tcanvas::shell::world::Camera { center: pos2(300.0, 150.0), zoom: 0.8 };
    h.state_mut().world.camera = before;
    steps(&mut h, 3);

    // During the click, "busy" flips between running / idle (Claude at work): the bar does not move.
    let flip = |h: &mut Harness<'_, App>, k: usize| set_status(h, &busy, if k.is_multiple_of(2) { TermStatus::Running } else { TermStatus::Idle });
    click_label_while(&mut h, "bloqué 1", flip);
    steps(&mut h, 45);
    let stuck_rect = h.state().world.card(&stuck).unwrap().rect;
    assert!(h.state().filters.is_active());
    assert!(sees_whole(&h, stuck_rect), "the view frames the stalled terminal");
    assert!(h.query_by_label_contains("× filtre (1/3)").is_some(), "match count");

    // Clicking the chip again clears the filter: everything reappears and the previous view returns.
    click_label_while(&mut h, "bloqué 1", flip);
    steps(&mut h, 45);
    assert!(!h.state().filters.is_active(), "filter cleared");
    assert_eq!(h.state().world.camera, before, "back to the view from before the filter");
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
    assert!(sees_whole(&h, h.state().world.cards[1].rect), "Enter frames the matches");
    h.get_by_label_contains("× filtre (1/2)").click();
    steps(&mut h, 45);
    assert!(!h.state().filters.is_active() && h.state().filters.text.is_empty());
    assert_eq!(h.state().world.camera, before);
}

// --- Opening a terminal in the open group ----------------------------------------------------

fn linked_groups(h: &mut Harness<'_, App>) {
    for (i, name) in ["api", "ai-screen-console", "billing", "site-web"].iter().enumerate() {
        let id = add_project(h, name, Rect::from_min_size(pos2(i as f32 * 5000.0, 0.0), Vec2::new(1400.0, 500.0)));
        h.state_mut().world.projects.iter_mut().find(|p| p.id == id).unwrap().cwd = Some(format!("/home/me/{name}"));
    }
}

#[test]
fn the_toolbar_stays_two_rows_high_in_an_open_project() {
    let dir = temp_dir("toolbar-height");
    let mut h = harness(&dir);
    linked_groups(&mut h);
    // Another project "writing": its chip shows a ✎ (the glyph must exist).
    let now = tcanvas::shell::world::unix_now();
    h.state_mut().world.file_activity.insert("p-site-web".into(), now);
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    for _ in 0..30 {
        h.step();
        assert!(h.state().viewport().min.y < 70.0, "bar too tall: {:?}", h.state().viewport());
    }
    assert!(h.query_by_label("● site-web ✎").is_some());
    let button = h.get_by_label("+ Terminal").rect();
    assert!(button.max.y < 70.0, "\"+ Terminal\" visible at the top: {button:?}");
}

#[test]
fn every_way_to_open_a_terminal_works_inside_an_open_group() {
    let dir = temp_dir("group-new-terminal");
    let mut h = harness(&dir);
    linked_groups(&mut h);
    let first = add_card(&mut h, "first", pos2(40.0, 40.0));
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 45);
    let count = |h: &Harness<'_, App>| h.state().world.members("p-api").count();
    let newest = |h: &Harness<'_, App>| h.state().world.cards.last().unwrap().clone();

    // 1. The group label's button.
    h.get_by_label("Nouveau terminal dans le groupe api").click();
    steps(&mut h, 45);
    assert_eq!(count(&h), 2);
    let card = newest(&h);
    assert_eq!(card.cwd.as_deref(), Some("/home/me/api"), "in the linked folder");
    let first_rect = h.state().world.card(&first).unwrap().rect;
    assert!(card.rect.min.x > first_rect.max.x && !card.rect.intersects(first_rect), "placed next to it, without overlap");

    // 2. "+ Terminal" in the top bar, and 3. Ctrl+Shift+N: in the open group.
    h.get_by_label("+ Terminal").click();
    steps(&mut h, 45);
    ctrl_shift(&h, Key::N);
    steps(&mut h, 45);
    assert_eq!(count(&h), 4);
    let rects: Vec<Rect> = h.state().world.members("p-api").map(|c| c.rect).collect();
    for i in 0..rects.len() {
        for j in i + 1..rects.len() {
            assert!(!rects[i].intersects(rects[j]), "overlap {i}/{j}");
        }
    }
    assert!(h.state().world.members("p-api").all(|c| h.state().world.shows_card(c)), "all visible in the space");
}

#[test]
fn an_empty_group_offers_to_open_a_terminal() {
    let dir = temp_dir("group-empty");
    let mut h = harness(&dir);
    linked_groups(&mut h);
    steps(&mut h, 2);
    h.get_by_label("● billing").click();
    steps(&mut h, 45);
    h.get_by_label("+ Ouvrir un terminal").click();
    steps(&mut h, 45);
    let card = h.state().world.cards.last().expect("terminal created").clone();
    assert_eq!((card.project.as_deref(), card.cwd.as_deref()), (Some("p-billing"), Some("/home/me/billing")));
    assert!(h.query_by_label("+ Ouvrir un terminal").is_none(), "no longer empty: the big button disappears");
    assert_eq!(h.state().world.focused.as_deref(), Some(card.id.as_str()), "the keyboard goes to the new terminal");
}

// --- Mouse wheel: only what is under the pointer scrolls --------------------------------------

/// One wheel notch at `p`, then enough frames for the smooth scroll to finish.
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
    let focused = add_card(&mut h, "focused", pos2(-600.0, -200.0));
    let other = add_card(&mut h, "other", pos2(100.0, -200.0));
    h.state_mut().dispatch(vec![Event::FocusCard(focused.clone())]);
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(0.0, 0.0), zoom: 1.0 };
    steps(&mut h, 3);
    let cam = h.state().world.camera;
    let center_of = |h: &Harness<'_, App>, id: &str| screen_of(h, h.state().world.card(id).unwrap().rect).center();

    for id in [&focused, &other] {
        let p = center_of(&h, id);
        wheel_at(&mut h, p, -120.0);
        assert_eq!(h.state().world.camera, cam, "wheel over terminal {id}: the view does not move");
    }

    // A window over the canvas ("go to" palette): it keeps the wheel.
    ctrl_shift(&h, Key::P);
    steps(&mut h, 2);
    let p = h.state().viewport().center();
    wheel_at(&mut h, p, -120.0);
    assert_eq!(h.state().world.camera, cam, "wheel over the palette");
    h.key_press(Key::Escape);
    steps(&mut h, 3);

    // Control: over empty space, the wheel does move the view.
    let empty = h.state().viewport().center() + Vec2::new(0.0, 330.0);
    wheel_at(&mut h, empty, -120.0);
    assert_ne!(h.state().world.camera, cam, "over empty space, the view scrolls");
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

// --- Nothing ever overlaps -------------------------------------------------------------------

/// Groups (with their label) and terminals outside groups: no overlap.
fn assert_nothing_overlaps(h: &Harness<'_, App>) {
    let w = &h.state().world;
    let mut bodies: Vec<(String, Rect)> = w.projects.iter().map(|p| (p.name.clone(), Rect::from_min_max(p.rect.min - Vec2::new(0.0, 40.0), p.rect.max))).collect();
    bodies.extend(w.cards.iter().filter(|c| c.project.is_none()).map(|c| (c.title(), c.rect)));
    for i in 0..bodies.len() {
        for j in i + 1..bodies.len() {
            assert!(!bodies[i].1.intersects(bodies[j].1), "{} overlaps {}", bodies[i].0, bodies[j].0);
        }
    }
    for p in &w.projects {
        let members: Vec<Rect> = w.members(&p.id).map(|c| c.rect).collect();
        for i in 0..members.len() {
            for j in i + 1..members.len() {
                assert!(!members[i].intersects(members[j]), "two terminals of {} overlap", p.name);
            }
        }
    }
}

#[test]
fn opening_terminals_in_a_group_pushes_neighbouring_groups_instead_of_overlapping_them() {
    let dir = temp_dir("no-overlap");
    let mut h = harness(&dir);
    // Two groups packed side by side, and a free terminal just below.
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0)));
    add_project(&mut h, "web", Rect::from_min_size(pos2(700.0, 0.0), Vec2::new(600.0, 400.0)));
    add_card(&mut h, "a", pos2(40.0, 40.0));
    add_card(&mut h, "w", pos2(740.0, 40.0));
    add_card(&mut h, "free", pos2(0.0, 900.0));
    steps(&mut h, 3);
    h.get_by_label("● api").click();
    steps(&mut h, 45);
    for _ in 0..3 {
        // Clicking the open group's chip again reframes all of it (label visible).
        h.get_by_label("● api").click();
        steps(&mut h, 45);
        h.get_by_label("Nouveau terminal dans le groupe api").click();
        steps(&mut h, 45);
        assert_nothing_overlaps(&h);
    }
    assert_eq!(h.state().world.members("p-api").count(), 4);

    // In "All": a new free terminal double-clicked against a frame moves aside too.
    h.get_by_label("Tous").click();
    steps(&mut h, 3);
    ctrl_shift(&h, Key::Num0);
    steps(&mut h, 45);
    let api = screen_of(&h, project_rect(&h, "p-api"));
    // Just below api's frame: a terminal centered there would spill over the group.
    let p = pos2(api.left_bottom().x + 30.0, api.max.y + 12.0);
    assert!(h.state().world.drop_target(h.state().world.camera.to_world(p, h.state().viewport())).is_none(), "point outside every frame");
    h.drag_at(p);
    h.step();
    h.drop_at(p);
    h.step();
    h.drag_at(p);
    h.step();
    h.drop_at(p);
    steps(&mut h, 45);
    assert_eq!(h.state().world.cards.len(), 7, "terminal created");
    assert_nothing_overlaps(&h);
}

/// Like the real app: repaint only when something asked for it. A click that starts
/// a camera flight must be enough, with no further mouse movement.
#[test]
fn one_click_on_a_filter_is_enough_to_frame_its_terminals() {
    let dir = temp_dir("filter-one-click");
    let mut h = harness(&dir);
    let stuck = add_card(&mut h, "stuck", pos2(6000.0, 4000.0));
    add_card(&mut h, "calm", pos2(0.0, 0.0));
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
    // No more events: only the frames the app requested run.
    let _ = h.try_run();
    let stuck_rect = h.state().world.card(&stuck).unwrap().rect;
    assert!(sees_whole(&h, stuck_rect), "a single click frames the stalled terminal");
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
    // Looking elsewhere in the group, zoomed in on "a".
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
    assert!(sees_whole(&h, rect), "a single click frames the running terminal: cam={cam:?} rect={rect:?}");
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
    // Wander far off in api, switch to web, then a single click to go back to api.
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(-40_000.0, 30_000.0), zoom: 3.0 };
    steps(&mut h, 2);
    h.get_by_label("● web").click();
    steps(&mut h, 3);
    assert!(sees_whole(&h, project_rect(&h, "p-web")));
    h.get_by_label("● api").click();
    steps(&mut h, 3);
    assert!(sees_whole(&h, project_rect(&h, "p-api")), "a single click: api centered and whole");
}

// --- Copy / paste and diffs in the "All" view ---------------------------------------------------

#[test]
fn terminal_clipboard_shortcuts_and_right_click_paste() {
    let dir = temp_dir("clipboard");
    let mut h = harness(&dir);
    h.state_mut().cards.set_clipboard(Box::new(tcanvas::features::terminal_card::FixedClipboard(Some("echo one\necho two".into()))));
    let id = add_card(&mut h, "api", Pos2::ZERO);
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(250.0, 150.0), zoom: 1.0 };
    h.state_mut().dispatch(vec![Event::FocusCard(id.clone())]);
    steps(&mut h, 2);

    // Ctrl+Shift+V pastes (and does not send ^V); Ctrl+Shift+C does not send ^C.
    ctrl_shift(&h, Key::V);
    ctrl_shift(&h, Key::C);
    steps(&mut h, 2);
    assert_eq!(h.state().cards.written(&id), b"echo one\recho two");

    // Right-click in the terminal: pastes too, without a menu.
    let body = screen_of(&h, h.state().world.card(&id).unwrap().rect).center();
    h.hover_at(body);
    h.step();
    h.event(egui::Event::PointerButton { pos: body, button: egui::PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.event(egui::Event::PointerButton { pos: body, button: egui::PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
    steps(&mut h, 2);
    assert_eq!(h.state().cards.written(&id), b"echo one\recho twoecho one\recho two");
}

#[test]
fn the_changes_badge_shows_diffs_and_commits_from_the_overview() {
    let dir = temp_dir("changes-badge");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 500.0)));
    h.state_mut().world.projects[0].cwd = Some("~/api".into());
    add_card(&mut h, "a", pos2(40.0, 40.0));
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(500.0, 250.0), zoom: 0.8 };
    steps(&mut h, 2);
    assert!(h.state().world.space.is_none(), "\"All\" view");

    // Clean repo: the work is in the commits.
    let now = tcanvas::shell::world::unix_now();
    let AgentEvent::Changes(mut set) = changes("p-api", &[]) else { unreachable!() };
    let sha = "c".repeat(40);
    set.commits = vec![protocol::CommitInfo { sha: sha.clone(), summary: "feat: login".into(), author: "Claude".into(), time: now - 120 }];
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Changes(set))]);
    steps(&mut h, 2);

    h.get_by_label("Changements du groupe api").click();
    steps(&mut h, 3);
    assert!(h.state().world.changes_open);
    assert!(h.query_by_label("Rien en cours : tout est commité.").is_some());
    assert!(h.query_by_label("Commit feat: login").is_some());
    assert!(h.state().agent_sent().contains(&protocol::AgentCmd::ShowCommit { key: "p-api".into(), sha: sha.clone() }), "\"Follow\" shows the latest commit");
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Diff { key: "p-api".into(), file: sha, text: "commit ccc\n+new\n".into(), truncated: false, problem: None })]);
    steps(&mut h, 3);
    assert!(h.query_by_label("+new").is_some(), "commit diff shown");
}

#[test]
fn a_new_group_from_the_overview_never_covers_an_existing_one() {
    let dir = temp_dir("new-group");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 500.0)));
    add_card(&mut h, "a", pos2(40.0, 40.0));
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(500.0, 250.0), zoom: 0.8 };
    steps(&mut h, 2);
    h.state_mut().dispatch(vec![Event::NewProject { at: None }]);
    steps(&mut h, 40);
    let w = &h.state().world;
    let (a, b) = (w.projects[0].rect, w.projects[1].rect);
    assert!(!a.expand(20.0).intersects(b), "{a:?} / {b:?}");
    assert!(sees_whole(&h, b), "the new frame is on screen to name it");
}

#[test]
fn a_canned_card_shows_fixed_output_and_ignores_typing() {
    let dir = temp_dir("canned");
    let mut h = harness(&dir);
    let id = add_card(&mut h, "demo", Pos2::ZERO);
    let app = h.state_mut();
    app.cards.show_canned(&app.world, &id, b"\x1b[1mcargo test\x1b[0m\r\ntest result: ok");
    steps(&mut h, 2);
    h.state_mut().world.focused = Some(id.clone());
    h.event(egui::Event::Text("ls".into()));
    steps(&mut h, 2);
    let text = h.state().cards.screen_text(&id).unwrap();
    assert!(text.starts_with("cargo test\ntest result: ok"), "{text}");
    assert!(h.state().cards.written(&id).is_empty(), "input goes to the (inert) terminal, not the test log");
}

/// Screen rectangle of the widget with this accessible label.
fn widget_rect(h: &Harness<'_, App>, label: &str) -> Rect {
    h.get_by_label(label).rect()
}

#[test]
fn free_grid_makes_the_open_group_the_whole_canvas() {
    let dir = temp_dir("free-grid");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    add_project(&mut h, "web", Rect::from_min_size(pos2(0.0, 1200.0), Vec2::new(800.0, 500.0)));
    add_card(&mut h, "a", pos2(40.0, 40.0));
    let b = add_card(&mut h, "b", pos2(700.0, 40.0));
    steps(&mut h, 2);
    assert!(h.query_by_label("Grille libre").is_none(), "no option in \"All\"");

    h.get_by_label("● api").click();
    steps(&mut h, 45);
    h.get_by_label("Grille libre").click();
    steps(&mut h, 3);
    assert!(h.state().world.free_grid);

    // The label is a bar pinned at the top left of the canvas, whatever the camera.
    let vp = h.state().viewport();
    let pinned = |h: &Harness<'_, App>| {
        let close = widget_rect(h, "Fermer le groupe api");
        close.min.y < vp.min.y + 50.0 && close.min.x < vp.min.x + 400.0
    };
    assert!(pinned(&h));
    h.state_mut().world.camera.center += Vec2::new(3000.0, 2000.0);
    steps(&mut h, 2);
    assert!(pinned(&h), "does not follow the camera");
    h.state_mut().world.camera.center -= Vec2::new(3000.0, 2000.0);
    steps(&mut h, 2);

    // Dragged far below, way past the old frame: still in the group, the frame follows.
    drag_card_by(&mut h, &b, Vec2::new(0.0, 600.0));
    assert_eq!(group_of(&h, &b).as_deref(), Some("p-api"));
    assert_eq!(project_rect(&h, "p-api"), members_fit(&h, "p-api"));

    // The pinned bar still renames the group.
    let title = widget_rect(&h, "Fermer le groupe api").min - Vec2::new(40.0, -8.0);
    for _ in 0..2 {
        h.drag_at(title);
        h.step();
        h.drop_at(title);
        h.step();
    }
    steps(&mut h, 3);
    h.event(egui::Event::Text("Backend".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 3);
    assert_eq!(project_name(&h, "p-api"), "Backend");

    // Back to "All": same layout, the option is hidden, the label is on the frame again.
    h.get_by_label("Tous").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("Grille libre").is_none());
    assert_eq!(group_of(&h, &b).as_deref(), Some("p-api"));
    let frame = screen_of(&h, project_rect(&h, "p-api"));
    let close = widget_rect(&h, "Fermer le groupe Backend");
    assert!((close.max.y - frame.min.y).abs() < 10.0, "label above the frame: {close:?} vs {frame:?}");
}

#[test]
fn free_grid_toggle_in_english() {
    let dir = temp_dir("free-grid-en");
    let mut h = harness_in(&dir, Lang::En);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    add_card(&mut h, "a", pos2(40.0, 40.0));
    steps(&mut h, 2);
    h.get_by_label("● api").click();
    steps(&mut h, 3);
    h.get_by_label("Free grid").click();
    steps(&mut h, 3);
    assert!(h.state().world.free_grid);
}

#[test]
fn the_toolbar_shows_the_running_version() {
    let dir = temp_dir("version");
    let mut h = harness(&dir);
    steps(&mut h, 2);
    h.get_by_label(&format!("v{}", tcanvas::shell::version::current()));
}

fn release(version: &str, notes: &str) -> tcanvas::features::updates::release::Release {
    use tcanvas::features::updates::release::{self, Asset, Release};
    let files = [release::asset_name(cfg!(windows), version), release::SUMS.to_string()];
    Release {
        version: version.into(),
        notes: notes.into(),
        page: release::release_page(version),
        assets: files.into_iter().map(|name| Asset { url: format!("https://example.com/{name}"), name }).collect(),
    }
}

#[test]
fn a_newer_release_shows_the_update_button_a_toast_and_its_notes() {
    let dir = temp_dir("update");
    let mut h = harness(&dir);
    steps(&mut h, 2);
    assert!(h.query_by_label("Mettre à jour vers v99.0.1").is_none());
    h.state().updates.offer(vec![release("99.0.0", "### Added\n\n- **Older** thing"), release("99.0.1", "### Fixed\n\n- Newest fix")]);
    steps(&mut h, 3);
    assert_eq!(h.state().pending_toasts(), 1, "warned once");
    h.get_by_label("Mise à jour disponible");

    // Before updating: the notes of every version since the running one, newest first.
    h.get_by_label("Mettre à jour vers v99.0.1").click();
    steps(&mut h, 3);
    h.get_by_label("tcanvas v99.0.1 est disponible");
    h.get_by_label("•  Newest fix");
    h.get_by_label("•  Older thing");
    h.get_by_label("Mettre à jour et redémarrer");
    h.get_by_label("Plus tard").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("•  Newest fix").is_none());
    assert_eq!(*h.state().updates.phase(), tcanvas::features::updates::Phase::Idle, "nothing downloaded");
}

#[test]
fn the_update_button_in_english() {
    let dir = temp_dir("update-en");
    let mut h = harness_in(&dir, Lang::En);
    steps(&mut h, 2);
    h.state().updates.offer(vec![release("99.0.0", "- Something new")]);
    steps(&mut h, 3);
    h.get_by_label("Update to v99.0.0").click();
    steps(&mut h, 3);
    h.get_by_label("tcanvas v99.0.0 is available");
    h.get_by_label("Update and restart");
}

#[test]
fn the_version_label_opens_whats_new() {
    let dir = temp_dir("whats-new-label");
    let mut h = harness(&dir);
    steps(&mut h, 2);
    h.get_by_label(&format!("v{}", tcanvas::shell::version::current())).click();
    steps(&mut h, 3);
    let base = tcanvas::features::updates::version::base(tcanvas::shell::version::current());
    h.get_by_label(&format!("Quoi de neuf dans la v{base}"));
    h.get_by_label("Fermer").click();
    steps(&mut h, 3);
    assert!(h.state().updates.dialog.is_none());
}

#[test]
fn whats_new_checks_for_updates_on_request() {
    let dir = temp_dir("check-now");
    let mut h = harness(&dir);
    steps(&mut h, 2);
    h.state_mut().updates.set_fetcher(|| Ok(vec![]));
    h.get_by_label(&format!("v{}", tcanvas::shell::version::current())).click();
    steps(&mut h, 3);
    h.get_by_label("Rechercher des mises à jour").click();
    wait_for_label(&mut h, "Vous avez la dernière version");

    // A newer release turns up: its notes replace "What's new", with no toast.
    h.state_mut().updates.set_fetcher(|| Ok(vec![release("99.0.1", "### Fixed\n\n- Newest fix")]));
    h.get_by_label("Rechercher des mises à jour").click();
    wait_for_label(&mut h, "tcanvas v99.0.1 est disponible");
    h.get_by_label("•  Newest fix");
    h.get_by_label("Mettre à jour et redémarrer");
    assert_eq!(h.state().pending_toasts(), 0);
}

#[test]
fn whats_new_check_button_in_english() {
    let dir = temp_dir("check-now-en");
    let mut h = harness_in(&dir, Lang::En);
    steps(&mut h, 2);
    h.state_mut().updates.set_fetcher(|| Err("offline".into()));
    h.get_by_label(&format!("v{}", tcanvas::shell::version::current())).click();
    steps(&mut h, 3);
    h.get_by_label("Check for updates").click();
    wait_for_label(&mut h, "Check failed");
}

/// Steps the harness until `label` shows up (an answer from a background thread).
fn wait_for_label(h: &mut Harness<'_, App>, label: &str) {
    for _ in 0..200 {
        steps(h, 1);
        if h.query_by_label(label).is_some() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("{label}: never shown");
}

#[test]
fn whats_new_shows_once_after_an_update() {
    let dir = temp_dir("whats-new");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("updates.json"), r#"{"last_seen_version": "0.0.1"}"#).unwrap();
    let mut h = harness(&dir);
    steps(&mut h, 3);
    let base = tcanvas::features::updates::version::base(tcanvas::shell::version::current());
    h.get_by_label(&format!("Quoi de neuf dans la v{base}"));
    h.get_by_label("v0.1.0");
    drop(h);
    // Next start: already seen.
    let mut h = harness(&dir);
    steps(&mut h, 3);
    assert!(h.state().updates.dialog.is_none());
}
