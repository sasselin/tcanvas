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

/// A card whose coding agent reports `prompt` as its last one.
fn card_with_prompt(h: &mut Harness<'_, App>, prompt: &str) -> String {
    let id = add_card(h, "api", Pos2::ZERO);
    let session = h.state().world.card(&id).unwrap().session();
    let info = SessionInfo {
        session,
        status: TermStatus::Running,
        command: Some("claude".into()),
        cwd: Some("/home/me/api".into()),
        last_exit: None,
        last_duration_secs: None,
        tail: vec![],
        agent_session_id: Some("4f1c".into()),
        agent_transcript: Some("/home/me/.claude/projects/-home-me-api/4f1c.jsonl".into()),
        last_prompt: Some(prompt.into()),
    };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Session(info))]);
    steps(h, 2);
    id
}

#[test]
fn the_header_shows_the_last_prompt_elided_with_the_full_text_on_hover() {
    let dir = temp_dir("prompt");
    let mut h = harness(&dir);
    let prompt = format!("ajoute le total TTC au PDF {}", "et vérifie les arrondis ".repeat(10));
    let prompt = prompt.trim_end();
    let id = card_with_prompt(&mut h, prompt);
    let card_rect = screen_of(&h, h.state().world.card(&id).unwrap().rect);
    let shown = h.get_by_label(&format!("Dernier prompt : {prompt}")).rect();
    assert!(shown.min.y >= card_rect.min.y && shown.max.y <= card_rect.min.y + 28.0 * h.state().world.camera.zoom + 1.0, "in the title bar: {shown:?}");
    assert!(shown.max.x < card_rect.max.x, "elided inside the card: {shown:?} {card_rect:?}");

    h.hover_at(shown.center());
    steps(&mut h, 60);
    assert!(h.query_by_label(prompt).is_some(), "full prompt in the tooltip");

    // The prompt does not get in the way of the title bar: dragging from it moves the card.
    let before = h.state().world.card(&id).unwrap().rect;
    let from = shown.center();
    h.drag_at(from);
    h.step();
    for k in 1..=10 {
        h.hover_at(from + Vec2::new(10.0 * k as f32, 5.0 * k as f32));
        h.step();
    }
    h.drop_at(from + Vec2::new(100.0, 50.0));
    steps(&mut h, 2);
    assert_ne!(h.state().world.card(&id).unwrap().rect.min, before.min, "dragged from the prompt");

    // No agent prompt: nothing shown.
    let session = h.state().world.card(&id).unwrap().session();
    let info = SessionInfo {
        session,
        status: TermStatus::Idle,
        command: None,
        cwd: None,
        last_exit: None,
        last_duration_secs: None,
        tail: vec![],
        agent_session_id: None,
        agent_transcript: None,
        last_prompt: None,
    };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Session(info))]);
    steps(&mut h, 2);
    assert!(h.query_by_label_contains("Dernier prompt").is_none());
}

#[test]
fn a_narrow_card_keeps_the_prompt_reachable() {
    let dir = temp_dir("prompt-narrow");
    let mut h = harness(&dir);
    let prompt = "ajoute le total TTC au PDF";
    let id = card_with_prompt(&mut h, prompt);
    {
        let card = h.state_mut().world.card_mut(&id).unwrap();
        card.rect = Rect::from_min_size(card.rect.min, Vec2::new(200.0, 300.0));
    }
    steps(&mut h, 2);
    let card_rect = screen_of(&h, h.state().world.card(&id).unwrap().rect);
    let shown = h.get_by_label(&format!("Dernier prompt : {prompt}")).rect();
    assert!(card_rect.contains_rect(shown), "inside the card: {shown:?} {card_rect:?}");
    assert!(shown.width() >= 8.0, "a hover target: {shown:?}");
    h.hover_at(shown.center());
    steps(&mut h, 60);
    assert!(h.query_by_label(prompt).is_some(), "full prompt in the tooltip");
}

#[test]
fn the_header_prompt_in_english() {
    let dir = temp_dir("prompt-en");
    let mut h = harness_in(&dir, Lang::En);
    card_with_prompt(&mut h, "add the total with taxes to the PDF");
    assert!(h.query_by_label("Last prompt: add the total with taxes to the PDF").is_some());
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
    let info = |status| SessionInfo {
        session: session.clone(),
        status,
        command: Some("make".into()),
        cwd: None,
        last_exit: None,
        last_duration_secs: None,
        tail: vec![],
        agent_session_id: None,
        agent_transcript: None,
        last_prompt: None,
    };
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
    let hello = |hooks| Event::Agent(AgentEvent::Hello { version: "1".into(), hooks, send_text: true });
    let installs = |h: &Harness<'_, App>| h.state().agent_sent().iter().filter(|c| **c == protocol::AgentCmd::InstallHooks).count();
    for (lang, label, button) in [(Lang::Fr, "⚠ hooks", "Installer"), (Lang::En, "⚠ hooks", "Install")] {
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
    let info = SessionInfo {
        session: "tc_orphan1".into(),
        status: TermStatus::Idle,
        command: None,
        cwd: Some("/srv".into()),
        last_exit: None,
        last_duration_secs: None,
        tail: vec![],
        agent_session_id: None,
        agent_transcript: None,
        last_prompt: None,
    };
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
    h.state_mut().world.projects.push(tcanvas::shell::world::Project { id: id.clone(), name: name.into(), color: [94, 129, 244], rect, cwd: None, templates: Vec::new(), messages: Vec::new() });
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
            worktree: false,
            dirs: dirs.iter().map(|(n, g)| protocol::DirEntry { name: (*n).into(), git: *g, worktree: false }).collect(),
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
    let info = SessionInfo { session, status, command: None, cwd: None, last_exit: None, last_duration_secs: None, tail: vec![], agent_session_id: None, agent_transcript: None, last_prompt: None };
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
fn pasting_an_image_pastes_the_path_of_a_saved_png() {
    let dir = temp_dir("paste-image");
    let mut h = harness(&dir);
    let pasted = dir.join("pasted");
    h.state_mut().cards.set_paste_dir(pasted.clone());
    let image = egui::ColorImage::new([2, 2], vec![egui::Color32::RED; 4]);
    h.state_mut().cards.set_clipboard(Box::new(tcanvas::features::terminal_card::FixedImageClipboard(image.clone())));
    let id = add_card(&mut h, "api", Pos2::ZERO);
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(250.0, 150.0), zoom: 1.0 };
    h.state_mut().dispatch(vec![Event::FocusCard(id.clone())]);
    steps(&mut h, 2);

    // Ctrl+V with an image: egui sends PasteImage. Then Ctrl+Shift+V reads the clipboard's image.
    h.event(egui::Event::PasteImage(std::sync::Arc::new(image)));
    ctrl_shift(&h, Key::V);
    steps(&mut h, 2);
    let written = String::from_utf8(h.state().cards.written(&id)).unwrap();
    let paths: Vec<&str> = written.split(".png").filter(|p| !p.is_empty()).collect();
    assert_eq!(paths.len(), 2, "one path per paste: {written}");
    assert!(written.starts_with(pasted.to_str().unwrap()) || cfg!(windows), "{written}");
    let files: Vec<_> = std::fs::read_dir(&pasted).unwrap().flatten().collect();
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|f| std::fs::read(f.path()).unwrap().starts_with(b"\x89PNG")));
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
    // The modal grows with the history for a few frames, then settles where it is centered.
    steps(&mut h, 10);
    let base = tcanvas::features::updates::version::base(tcanvas::shell::version::current());
    h.get_by_label(&format!("Quoi de neuf dans la v{base}"));
    // The whole history, not only the running version.
    h.get_by_label("v0.1.0");
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
    // The modal grows with the history for a few frames, then settles where it is centered.
    steps(&mut h, 10);
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
    // The modal grows with the history for a few frames, then settles where it is centered.
    steps(&mut h, 10);
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

#[test]
fn ctrl_shift_k_lists_the_shortcuts_and_escape_closes_them() {
    let dir = temp_dir("shortcuts-key");
    let mut h = harness(&dir);
    add_card(&mut h, "api", Pos2::ZERO);
    steps(&mut h, 2);
    ctrl_shift(&h, Key::K);
    steps(&mut h, 2);
    assert!(h.state().shortcuts_open());
    h.get_by_label("Raccourcis clavier");
    h.get_by_label("Ctrl+Maj+P");
    h.get_by_label("Alt+clic droit");
    h.key_press(Key::Escape);
    steps(&mut h, 2);
    assert!(!h.state().shortcuts_open());
}

#[test]
fn the_toolbar_button_and_the_palette_open_the_shortcuts() {
    let dir = temp_dir("shortcuts-open");
    let mut h = harness(&dir);
    steps(&mut h, 2);
    h.get_by_label("⌨").click();
    steps(&mut h, 2);
    assert!(h.state().shortcuts_open());
    h.get_by_label("OK").click();
    steps(&mut h, 2);
    assert!(!h.state().shortcuts_open());

    ctrl_shift(&h, Key::P);
    steps(&mut h, 2);
    h.event(egui::Event::Text("raccourcis".into()));
    steps(&mut h, 2);
    h.key_press(Key::Enter);
    steps(&mut h, 2);
    assert!(!h.state().jump_open());
    assert!(h.state().shortcuts_open());
}

#[test]
fn shortcuts_window_in_english() {
    let dir = temp_dir("shortcuts-en");
    let mut h = harness_in(&dir, Lang::En);
    steps(&mut h, 2);
    ctrl_shift(&h, Key::K);
    steps(&mut h, 2);
    h.get_by_label("Keyboard shortcuts");
    h.get_by_label("Ctrl+Shift+K");
    h.get_by_label("This window");
}

/// Many groups: the chips scroll inside the room left of the right-hand widgets, and the
/// "▾ N" menu reaches the ones out of view.
fn many_groups_scroll_and_open_from_the_menu(lang: Lang) {
    let dir = temp_dir(&format!("many-groups-{lang:?}"));
    let mut h = harness_in(&dir, lang);
    for n in 0..20 {
        add_project(&mut h, &format!("a-rather-long-group-name-{n:02}"), Rect::from_min_size(pos2(0.0, n as f32 * 1000.0), Vec2::new(800.0, 500.0)));
    }
    steps(&mut h, 4);
    let help = widget_rect(&h, "⌨");
    let menu = h.get_by_label_contains("▾ ").rect();
    assert!(menu.max.x < help.min.x, "the menu stays left of the right-hand widgets: {menu:?} vs {help:?}");

    h.get_by_label_contains("▾ ").click();
    steps(&mut h, 2);
    h.get_by_label("a-rather-long-group-name-19").click();
    steps(&mut h, 4);
    assert_eq!(h.state().world.space.as_deref(), Some("p-a-rather-long-group-name-19"));
    // The open group's chip was scrolled into view, left of the menu.
    let chip = widget_rect(&h, "● a-rather-long-group-name-19");
    let menu = h.get_by_label_contains("▾ ").rect();
    assert!(chip.max.x <= menu.min.x + 1.0 && chip.min.x > 0.0, "{chip:?} vs {menu:?}");
}

#[test]
fn many_groups_scroll_and_open_from_the_menu_fr() {
    many_groups_scroll_and_open_from_the_menu(Lang::Fr);
}

#[test]
fn many_groups_scroll_and_open_from_the_menu_en() {
    many_groups_scroll_and_open_from_the_menu(Lang::En);
}

#[test]
fn few_groups_need_no_menu() {
    let dir = temp_dir("few-groups");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0)));
    steps(&mut h, 4);
    assert!(h.query_by_label_contains("▾ ").is_none());
}

// --- Worktrees -------------------------------------------------------------------------------

fn worktree(path: &str, main: bool, branch: &str) -> protocol::Worktree {
    protocol::Worktree {
        path: path.into(),
        head: Some("a".repeat(40)),
        branch: Some(branch.into()),
        detached: false,
        main,
        bare: false,
        locked: false,
        lock_reason: None,
        prunable: false,
        dirty: Some(false),
        ahead: Some(0),
        behind: Some(0),
        merged: None,
        last_commit: Some(tcanvas::shell::world::unix_now() - 3600),
        branch_deletable: false,
        reflog_commits: false,
        new_branch: false,
    }
}

/// The `shop` repository: its main checkout, an unmerged and dirty worktree nested in it, and a
/// merged one next to it.
fn shop_worktrees(key: &str) -> Event {
    let invoices = protocol::Worktree { dirty: Some(true), ahead: Some(6), behind: Some(12), merged: Some(false), ..worktree("/home/me/shop/.claude/worktrees/invoices", false, "feat/invoices") };
    let taxes = protocol::Worktree { ahead: Some(0), behind: Some(3), merged: Some(true), branch_deletable: true, ..worktree("/home/me/shop-taxes", false, "taxes") };
    let list = protocol::WorktreeList {
        key: key.into(),
        repo: Some("/home/me/shop".into()),
        default_branch: Some("main".into()),
        worktrees: vec![worktree("/home/me/shop", true, "main"), invoices, taxes],
        merged_branches: vec![],
    };
    Event::Agent(AgentEvent::Worktrees(list))
}

/// Group `shop` linked to its repository and opened, with a terminal in the main checkout and
/// one in the nested worktree.
fn shop_group(h: &mut Harness<'_, App>) -> (String, String) {
    let id = add_project(h, "shop", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    h.state_mut().world.projects[0].cwd = Some("/home/me/shop".into());
    let main = add_card(h, "front", pos2(40.0, 40.0));
    let nested = add_card(h, "pdf", pos2(700.0, 40.0));
    h.state_mut().world.card_mut(&main).unwrap().cwd = Some("/home/me/shop/src".into());
    h.state_mut().world.card_mut(&nested).unwrap().cwd = Some("/home/me/shop/.claude/worktrees/invoices/".into());
    h.state_mut().dispatch(vec![Event::EnterSpace(Some(id))]);
    steps(h, 3);
    (main, nested)
}

#[test]
fn the_worktrees_strip_lists_the_repository_and_its_terminals() {
    let dir = temp_dir("worktrees");
    let mut h = harness(&dir);
    let (main, nested) = shop_group(&mut h);
    assert!(h.query_by_label_contains("worktrees").is_none(), "nothing before the agent answers");
    h.state_mut().dispatch(vec![shop_worktrees("p-shop")]);
    steps(&mut h, 3);

    assert!(h.query_by_label("⎇ 3 worktrees").is_some());
    for label in ["copie principale", "invoices", "shop-taxes", "⎇ feat/invoices", "↑6 ↓12", "✔ fusionnée", "aucun terminal"] {
        assert!(h.query_by_label(label).is_some(), "{label} missing");
    }
    // Each terminal under its worktree, the nested one under the linked worktree.
    let attributed = |id: &str| h.state().world.card_worktrees.get(id).map(|w| w.path.clone());
    assert_eq!(attributed(&main).as_deref(), Some("/home/me/shop"));
    assert_eq!(attributed(&nested).as_deref(), Some("/home/me/shop/.claude/worktrees/invoices"));

    // The badges in the card headers.
    let badge = h.get_by_label("Branche : feat/invoices · invoices").rect();
    let card = screen_of(&h, h.state().world.card(&nested).unwrap().rect);
    assert!(card.contains_rect(badge) && badge.max.y <= card.min.y + 28.0 * h.state().world.camera.zoom + 1.0, "in the title bar: {badge:?} {card:?}");
    assert!(h.query_by_label("Branche : main").is_some());

    // Clicking a terminal in the strip flies to it.
    h.get_by_label("pdf").click();
    steps(&mut h, 60);
    assert_eq!(h.state().world.focused.as_deref(), Some(nested.as_str()));

    // With only the main checkout left, the strip goes away; the badge stays.
    let only_main = protocol::WorktreeList {
        key: "p-shop".into(),
        repo: Some("/home/me/shop".into()),
        default_branch: Some("main".into()),
        worktrees: vec![worktree("/home/me/shop", true, "main")],
        merged_branches: vec![],
    };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Worktrees(only_main))]);
    steps(&mut h, 3);
    assert!(h.query_by_label_contains("worktrees").is_none());
    assert_eq!(h.query_all_by_label("Branche : main").count(), 2, "both terminals now in the main checkout");
    assert!(h.query_by_label_contains("Branche : feat/invoices").is_none());
}

#[test]
fn cards_stay_put_when_the_worktrees_strip_shows_or_hides() {
    let dir = temp_dir("worktrees-still");
    let mut h = harness(&dir);
    let (main, _) = shop_group(&mut h);
    let on_screen = |h: &Harness<'_, App>| screen_of(h, h.state().world.card(&main).unwrap().rect);
    let before = on_screen(&h);
    let viewport = h.state().viewport();
    h.state_mut().dispatch(vec![shop_worktrees("p-shop")]);
    steps(&mut h, 3);
    assert!(h.query_by_label("⎇ 3 worktrees").is_some());
    assert!(h.state().viewport().min.y > viewport.min.y, "the strip takes room above the canvas");
    assert!((on_screen(&h).min - before.min).length() < 0.5, "moved: {before:?} -> {:?}", on_screen(&h));
    // And back when the linked worktrees are gone.
    let only_main = protocol::WorktreeList {
        key: "p-shop".into(),
        repo: Some("/home/me/shop".into()),
        default_branch: Some("main".into()),
        worktrees: vec![worktree("/home/me/shop", true, "main")],
        merged_branches: vec![],
    };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Worktrees(only_main))]);
    steps(&mut h, 3);
    assert_eq!(h.state().viewport(), viewport);
    assert!((on_screen(&h).min - before.min).length() < 0.5, "moved: {before:?} -> {:?}", on_screen(&h));
}

#[test]
fn relinking_the_same_folder_keeps_the_worktrees() {
    let dir = temp_dir("worktrees-relink");
    let mut h = harness(&dir);
    shop_group(&mut h);
    h.state_mut().dispatch(vec![shop_worktrees("p-shop")]);
    steps(&mut h, 3);
    h.state_mut().dispatch(vec![Event::SetProjectCwd("p-shop".into(), Some("/home/me/shop".into()))]);
    steps(&mut h, 3);
    assert!(h.query_by_label("⎇ 3 worktrees").is_some(), "same folder: strip kept");
    assert!(h.query_by_label("Branche : feat/invoices · invoices").is_some(), "badges kept");
    h.state_mut().dispatch(vec![Event::SetProjectCwd("p-shop".into(), Some("/home/me/blog".into()))]);
    steps(&mut h, 3);
    assert!(h.query_by_label("⎇ 3 worktrees").is_none(), "another folder: wait for its own list");
}

#[test]
fn the_worktrees_strip_in_english() {
    let dir = temp_dir("worktrees-en");
    let mut h = harness_in(&dir, Lang::En);
    shop_group(&mut h);
    h.state_mut().dispatch(vec![shop_worktrees("p-shop")]);
    steps(&mut h, 3);
    for label in ["⎇ 3 worktrees", "main checkout", "✔ merged", "no terminal", "Branch: main", "Branch: feat/invoices · invoices"] {
        assert!(h.query_by_label(label).is_some(), "{label} missing");
    }
}

#[test]
fn the_folder_picker_tells_a_worktree_from_a_repository() {
    let dir = temp_dir("picker-worktree");
    let mut h = harness(&dir);
    add_project(&mut h, "api", Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 500.0)));
    steps(&mut h, 3);
    h.get_by_label("Dossier du groupe api").click();
    steps(&mut h, 2);
    let Some(protocol::AgentCmd::ListDir { path }) = h.state().agent_sent().last().cloned() else { panic!("the picker asks the agent") };
    let listing = |worktree: bool| {
        Event::Agent(AgentEvent::DirListing(protocol::DirListing {
            request: path.clone(),
            path: Some("/home/me/shop-taxes".into()),
            parent: Some("/home/me".into()),
            git: true,
            worktree,
            dirs: vec![],
            error: None,
        }))
    };
    h.state_mut().dispatch(vec![listing(true)]);
    steps(&mut h, 2);
    assert!(h.query_by_label("⎇ worktree git").is_some());
    assert!(h.query_by_label("⎇ dépôt git").is_none());
    // A later answer for the same request is ignored once received: navigate again first.
    h.get_by_label("⌂").click();
    steps(&mut h, 2);
    let Some(protocol::AgentCmd::ListDir { path: home }) = h.state().agent_sent().last().cloned() else { panic!("the picker asks the agent") };
    let repo = protocol::DirListing { request: home, path: Some("/home/me/shop".into()), parent: Some("/home/me".into()), git: true, worktree: false, dirs: vec![], error: None };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::DirListing(repo))]);
    steps(&mut h, 2);
    assert!(h.query_by_label("⎇ dépôt git").is_some());
    assert!(h.query_by_label("⎇ worktree git").is_none());
}

// --- Worktree cleanup ------------------------------------------------------------------------

fn sent_cleanups(h: &Harness<'_, App>) -> Vec<protocol::Cleanup> {
    h.state()
        .agent_sent()
        .iter()
        .filter_map(|c| match c {
            protocol::AgentCmd::Cleanup { key, action } if key == "p-shop" => Some(action.clone()),
            _ => None,
        })
        .collect()
}

fn cleaned(action: protocol::Cleanup, deleted_branch: Option<&str>, problem: Option<protocol::Problem>) -> Event {
    Event::Agent(AgentEvent::Cleaned { key: "p-shop".into(), action, deleted_branch: deleted_branch.map(String::from), problem })
}

/// Group `shop` open with its worktrees, and the cleanup window open.
fn cleanup_window(h: &mut Harness<'_, App>, list: Event, button: &str) {
    shop_group(h);
    h.state_mut().dispatch(vec![list]);
    steps(h, 3);
    h.get_by_label(button).click();
    steps(h, 3);
    assert!(h.state().worktrees.cleanup_open());
    h.event(egui::Event::PointerGone);
    steps(h, 1);
}

/// Text of the label containing `start` (a multi-line label carries it as its value).
fn text_of(h: &Harness<'_, App>, start: &str) -> String {
    use egui_kittest::kittest::NodeT;
    let node = h.get_by_label_contains(start);
    node.value().or_else(|| node.accesskit_node().label()).unwrap_or_default()
}

fn seen(ignored: &[&str]) -> protocol::Ignored {
    protocol::Ignored::from_paths(ignored.iter().map(|s| s.to_string()).collect())
}

fn measured(path: &str, mb: u64, ignored: &[&str]) -> Event {
    let size = protocol::WorktreeSize { path: path.into(), bytes: Some(mb * 1024 * 1024), ignored: Some(seen(ignored)) };
    Event::Agent(AgentEvent::WorktreeSizes { key: "p-shop".into(), sizes: vec![size] })
}

/// The agent's answers for these folders: no ignored file in any.
fn measured_all(h: &mut Harness<'_, App>, paths: &[&str]) {
    let events = paths.iter().map(|p| measured(p, 1, &[])).collect();
    h.state_mut().dispatch(events);
    steps(h, 2);
}

#[test]
fn a_merged_worktree_is_removed_only_after_confirmation() {
    let dir = temp_dir("cleanup");
    let mut h = harness(&dir);
    cleanup_window(&mut h, shop_worktrees("p-shop"), "♻ 1 à nettoyer");
    assert!(h.query_by_label("Nettoyer les worktrees - shop").is_some(), "titled with its group");
    assert!(h.query_by_label("Supprimables sans risque").is_some() && h.query_by_label("Conservés").is_some());
    // The kept ones, with their reason.
    assert!(h.query_by_label("pas fusionnée dans main").is_some(), "invoices: unmerged");
    assert_eq!(h.query_all_by_label("copie principale").count(), 3, "in the strip, then its name and its reason in the window");

    // Sizes: asked when the window opens, for the removable ones only.
    let sizes: Vec<_> = h.state().agent_sent().iter().filter(|c| matches!(c, protocol::AgentCmd::WorktreeSizes { .. })).cloned().collect();
    assert_eq!(sizes, vec![protocol::AgentCmd::WorktreeSizes { key: "p-shop".into(), paths: vec!["/home/me/shop-taxes".into()] }]);
    assert!(h.query_by_label("… à récupérer").is_some());
    h.state_mut().dispatch(vec![measured("/home/me/shop-taxes", 340, &[".env"])]);
    steps(&mut h, 2);
    assert!(h.query_by_label("340 Mo").is_some() && h.query_by_label("340 Mo à récupérer").is_some());

    // One click asks; nothing is sent before the confirmation, and Enter does not confirm.
    h.get_by_label("Supprimer…").click();
    steps(&mut h, 2);
    assert!(sent_cleanups(&h).is_empty());
    assert!(h.query_by_label("Supprimer définitivement ?").is_some());
    // Its ignored files are listed again: until the answer, the button does nothing.
    let asked = h.state().agent_sent().iter().filter(|c| matches!(c, protocol::AgentCmd::WorktreeSizes { .. })).count();
    assert_eq!(asked, 2, "asked again when the confirmation opens");
    assert!(text_of(&h, "Ceci supprime :").contains("fichiers ignorés : pas encore listés"));
    h.get_by_label("Supprimer").click();
    steps(&mut h, 2);
    assert!(sent_cleanups(&h).is_empty(), "not clickable before the ignored files are listed");
    h.state_mut().dispatch(vec![measured("/home/me/shop-taxes", 340, &[".env", "target/"])]);
    steps(&mut h, 2);
    let text = text_of(&h, "Ceci supprime :");
    assert!(text.contains("le dossier /home/me/shop-taxes") && text.contains("la branche taxes"), "{text}");
    assert!(text.contains("ignorés, supprimés aussi : .env, target/") && text.contains("bases de données locales"), "{text}");
    h.key_press(Key::Enter);
    steps(&mut h, 2);
    assert!(sent_cleanups(&h).is_empty(), "Enter does not delete");
    assert!(h.query_by_label("Supprimer définitivement ?").is_some(), "still asking");
    h.get_by_label("Supprimer").click();
    steps(&mut h, 2);
    let removal = protocol::Cleanup::RemoveWorktree { path: "/home/me/shop-taxes".into(), delete_branch: Some("taxes".into()), ignored: seen(&[".env", "target/"]) };
    assert_eq!(sent_cleanups(&h), vec![removal.clone()], "with the ignored files the user saw");
    assert!(h.query_by_label("Supprimer définitivement ?").is_none());
    assert!(h.query_by_label("en cours…").is_some(), "waiting for the agent");
    assert!(h.query_by_label("Tout supprimer (0)…").is_some(), "remove all does not send it again");

    // The agent refuses (a terminal opened there meanwhile): shown in place, translated.
    let refusal = protocol::Problem::WorktreeKept { path: "/home/me/shop-taxes".into(), reason: protocol::KeepReason::HasTerminal };
    h.state_mut().dispatch(vec![cleaned(removal.clone(), None, Some(refusal))]);
    steps(&mut h, 2);
    assert!(h.query_by_label("⚠ /home/me/shop-taxes conservé : un terminal y est ouvert").is_some());
    assert!(h.query_by_label("en cours…").is_none() && h.query_by_label("Supprimer…").is_some(), "offered again");
    // Then it goes through: the item is gone at once, before the agent's next scan.
    h.state_mut().dispatch(vec![cleaned(removal, Some("taxes"), None)]);
    steps(&mut h, 2);
    assert!(h.query_by_label("✔ Supprimés : /home/me/shop-taxes et la branche taxes").is_some());
    assert!(h.query_by_label("Supprimer…").is_none(), "not offered while the list catches up");
    h.state_mut().dispatch(vec![shop_worktrees("p-shop")]);
    steps(&mut h, 2);
    assert!(h.query_by_label("Supprimer…").is_none(), "a stale list does not bring it back");
}

#[test]
fn bulk_removal_lists_everything_and_the_branch_option_is_honored() {
    let dir = temp_dir("cleanup-bulk");
    let mut h = harness(&dir);
    let Event::Agent(AgentEvent::Worktrees(mut list)) = shop_worktrees("p-shop") else { unreachable!() };
    let extra = protocol::Worktree { merged: Some(true), branch_deletable: true, ..worktree("/home/me/shop-ship", false, "ship") };
    let gone = protocol::Worktree { prunable: true, dirty: None, ..worktree("/home/me/shop-old", false, "old") };
    // Merged into origin/main only: its branch would be refused by `git branch -d`.
    let behind = protocol::Worktree { merged: Some(true), branch_deletable: false, ..worktree("/home/me/shop-behind", false, "behind") };
    list.worktrees.extend([extra, gone, behind]);
    list.merged_branches = vec![protocol::MergedBranch { name: "fix/typo".into(), last_commit: None }];
    cleanup_window(&mut h, Event::Agent(AgentEvent::Worktrees(list)), "♻ 3 à nettoyer");
    assert!(h.query_by_label("dossier disparu : à élaguer").is_some());
    assert!(h.query_by_label("⎇ fix/typo").is_some());

    // With the branches: only those git will accept.
    let three = ["/home/me/shop-taxes", "/home/me/shop-ship", "/home/me/shop-behind"];
    h.get_by_label("Tout supprimer (3)…").click();
    steps(&mut h, 2);
    measured_all(&mut h, &three);
    let text = text_of(&h, "Ceci supprime :");
    assert!(text.contains("la branche taxes") && text.contains("la branche ship") && !text.contains("la branche behind"), "{text}");
    // Cancelling sends nothing.
    h.get_by_label("Annuler").click();
    steps(&mut h, 2);
    assert!(sent_cleanups(&h).is_empty());

    // Without the branches.
    h.get_by_label("Supprimer aussi la branche fusionnée").click();
    steps(&mut h, 2);
    h.get_by_label("Tout supprimer (3)…").click();
    steps(&mut h, 2);
    measured_all(&mut h, &three);
    let text = text_of(&h, "Ceci supprime :");
    assert!(text.contains("le dossier /home/me/shop-taxes") && text.contains("le dossier /home/me/shop-ship") && text.contains("le dossier /home/me/shop-behind"), "{text}");
    assert!(!text.contains("la branche"), "{text}");
    h.get_by_label("Supprimer").click();
    steps(&mut h, 2);
    let without = |p: &str| protocol::Cleanup::RemoveWorktree { path: p.into(), delete_branch: None, ignored: seen(&[]) };
    assert_eq!(sent_cleanups(&h), vec![without("/home/me/shop-taxes"), without("/home/me/shop-ship"), without("/home/me/shop-behind")]);

    // Prune, then the merged branch: each confirmed with what goes.
    h.get_by_label("Élaguer 1 entrée introuvable…").click();
    steps(&mut h, 2);
    let text = text_of(&h, "Ceci supprime :");
    assert!(text.contains("le suivi git de /home/me/shop-old : son dossier est introuvable en ce moment") && text.contains("disque démonté ou amovible"), "{text}");
    h.get_by_label("Supprimer").click();
    steps(&mut h, 2);
    h.get_by_label("Supprimer…").click();
    steps(&mut h, 2);
    assert_eq!(text_of(&h, "Ceci supprime :"), "Ceci supprime :\n  • la branche fix/typo");
    h.get_by_label("Supprimer").click();
    steps(&mut h, 2);
    let prune = protocol::Cleanup::Prune { paths: vec!["/home/me/shop-old".into()] };
    assert_eq!(sent_cleanups(&h)[3..], [prune, protocol::Cleanup::DeleteBranch { branch: "fix/typo".into() }]);
}

#[test]
fn a_terminal_moving_in_takes_a_worktree_off_the_list() {
    let dir = temp_dir("cleanup-card");
    let mut h = harness(&dir);
    let Event::Agent(AgentEvent::Worktrees(mut list)) = shop_worktrees("p-shop") else { unreachable!() };
    list.worktrees.push(protocol::Worktree { merged: Some(true), ..worktree("/home/me/shop-ship", false, "ship") });
    cleanup_window(&mut h, Event::Agent(AgentEvent::Worktrees(list)), "♻ 2 à nettoyer");
    let id = h.state().world.cards[0].id.clone();
    h.state_mut().world.card_mut(&id).unwrap().cwd = Some("/home/me/shop-ship/src".into());
    steps(&mut h, 3);
    assert!(h.query_by_label("♻ 1 à nettoyer").is_some());
    assert!(h.query_by_label("un terminal y est ouvert").is_some());
}

#[test]
fn a_long_confirmation_scrolls_and_keeps_its_buttons_on_screen() {
    let dir = temp_dir("cleanup-long");
    let mut h = harness(&dir);
    let Event::Agent(AgentEvent::Worktrees(mut list)) = shop_worktrees("p-shop") else { unreachable!() };
    for i in 0..24 {
        list.worktrees.push(protocol::Worktree { merged: Some(true), ..worktree(&format!("/home/me/shop-feature-{i:02}"), false, &format!("feature/{i:02}")) });
    }
    cleanup_window(&mut h, Event::Agent(AgentEvent::Worktrees(list)), "♻ 25 à nettoyer");
    h.get_by_label("Tout supprimer (25)…").click();
    steps(&mut h, 2);
    let paths: Vec<String> = std::iter::once("/home/me/shop-taxes".to_string()).chain((0..24).map(|i| format!("/home/me/shop-feature-{i:02}"))).collect();
    measured_all(&mut h, &paths.iter().map(String::as_str).collect::<Vec<_>>());
    // The modal settles its height over a few frames.
    steps(&mut h, 30);
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 900.0));
    for button in ["Supprimer", "Annuler"] {
        let r = h.get_by_label(button).rect();
        assert!(screen.contains_rect(r), "{button} off screen: {r:?}");
    }
    h.get_by_label("Supprimer").click();
    steps(&mut h, 2);
    assert_eq!(sent_cleanups(&h).len(), 25);
}

#[test]
fn a_lone_main_checkout_with_merged_branches() {
    let dir = temp_dir("cleanup-lone");
    let mut h = harness(&dir);
    shop_group(&mut h);
    let list = protocol::WorktreeList {
        key: "p-shop".into(),
        repo: Some("/home/me/shop".into()),
        default_branch: Some("main".into()),
        worktrees: vec![worktree("/home/me/shop", true, "main")],
        merged_branches: vec![protocol::MergedBranch { name: "fix/typo".into(), last_commit: None }],
    };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Worktrees(list))]);
    steps(&mut h, 3);
    assert!(h.query_by_label("⎇ 1 worktree").is_some(), "singular");
    h.get_by_label("♻ Nettoyer").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("⎇ fix/typo").is_some());
}

#[test]
fn terminal_buttons_take_clicks_in_an_overflowing_strip() {
    let dir = temp_dir("cleanup-overflow");
    let mut h = harness(&dir);
    let (_, nested) = shop_group(&mut h);
    let Event::Agent(AgentEvent::Worktrees(mut list)) = shop_worktrees("p-shop") else { unreachable!() };
    for i in 0..6 {
        list.worktrees.push(worktree(&format!("/home/me/shop-a-rather-long-feature-name-{i}"), false, &format!("feature/a-rather-long-branch-{i}")));
    }
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Worktrees(list))]);
    steps(&mut h, 3);
    // The strip overflows (its scroll bar shows), and the terminal button still takes the click.
    h.get_by_label("pdf").click();
    steps(&mut h, 60);
    assert_eq!(h.state().world.focused.as_deref(), Some(nested.as_str()));
}

#[test]
fn the_cleanup_window_in_english() {
    let dir = temp_dir("cleanup-en");
    let mut h = harness_in(&dir, Lang::En);
    cleanup_window(&mut h, shop_worktrees("p-shop"), "♻ 1 to clean up");
    for label in ["Clean up worktrees - shop", "Safe to remove", "Kept", "not merged into main", "Also delete the merged branch", "Remove all 1…"] {
        assert!(h.query_by_label(label).is_some(), "{label} missing");
    }
    let size = |h: &mut Harness<'_, App>| {
        let ignored = protocol::Ignored { shown: vec![".env".into(), "web/.env.local".into()], total: 5 };
        h.state_mut().dispatch(vec![Event::Agent(AgentEvent::WorktreeSizes {
            key: "p-shop".into(),
            sizes: vec![protocol::WorktreeSize { path: "/home/me/shop-taxes".into(), bytes: None, ignored: Some(ignored) }],
        })]);
        steps(h, 2);
    };
    size(&mut h);
    assert!(h.query_by_label("unknown").is_some());
    h.get_by_label("Remove…").click();
    steps(&mut h, 2);
    assert!(text_of(&h, "This deletes:").contains("ignored files: not listed yet"));
    size(&mut h);
    let text = text_of(&h, "This deletes:");
    assert!(text.contains("folder /home/me/shop-taxes") && text.contains("branch taxes") && text.contains("ignored, deleted too: .env, web/.env.local, and 3 more"), "{text}");
    assert!(text.contains("configuration files (.env), local databases"), "{text}");
    h.get_by_label("Delete").click();
    steps(&mut h, 2);
    let removal = protocol::Cleanup::RemoveWorktree {
        path: "/home/me/shop-taxes".into(),
        delete_branch: Some("taxes".into()),
        ignored: protocol::Ignored { shown: vec![".env".into(), "web/.env.local".into()], total: 5 },
    };
    assert_eq!(sent_cleanups(&h), vec![removal.clone()]);
    let refusal = protocol::Problem::WorktreeKept { path: "/home/me/shop-taxes".into(), reason: protocol::KeepReason::Dirty };
    h.state_mut().dispatch(vec![cleaned(removal, None, Some(refusal))]);
    steps(&mut h, 2);
    assert!(h.query_by_label("⚠ /home/me/shop-taxes kept: uncommitted or untracked changes").is_some());
}

/// Group "api" (linked to `/home/me/api`), open, with the given command templates.
fn group_with_templates(h: &mut Harness<'_, App>, templates: &[(&str, &str)]) {
    linked_groups(h);
    let p = h.state_mut().world.projects.iter_mut().find(|p| p.id == "p-api").unwrap();
    p.templates = templates.iter().map(|(name, command)| tcanvas::shell::world::CommandTemplate { name: (*name).into(), command: (*command).into() }).collect();
    steps(h, 2);
    h.get_by_label("● api").click();
    steps(h, 45);
}

fn newest_in_api(h: &Harness<'_, App>) -> Card {
    h.state().world.members("p-api").last().unwrap().clone()
}

#[test]
fn the_group_button_still_opens_a_plain_shell_in_one_click() {
    let dir = temp_dir("plain-card");
    let mut h = harness(&dir);
    group_with_templates(&mut h, &[("Revue", "claude \"relis\"")]);
    h.get_by_label("Nouveau terminal dans le groupe api").click();
    steps(&mut h, 3);
    let card = newest_in_api(&h);
    assert_eq!(card.cwd.as_deref(), Some("/home/me/api"));
    assert_eq!(h.state().cards.pending_command(&card.id), None, "no command: the plain shell, as before");
    assert!(h.query_by_label("Revue").is_none(), "the templates stay out of the way");
}

#[test]
fn the_arrow_starts_a_terminal_from_a_template_or_a_plain_shell() {
    let dir = temp_dir("template-menu");
    let mut h = harness(&dir);
    let command = "claude \"relis l'API\"\necho $HOME";
    group_with_templates(&mut h, &[("Revue", command), ("Logs", "tail -f log/app.log")]);
    h.get_by_label("Modèles de terminal du groupe api").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Shell").is_some());
    assert!(h.query_by_label("Logs").is_some());
    assert!(h.query_by_label("Modifier les modèles…").is_some());
    h.get_by_label("Revue").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("Logs").is_none(), "the menu closes");
    let card = newest_in_api(&h);
    assert_eq!(card.cwd.as_deref(), Some("/home/me/api"), "in the group's folder");
    assert_eq!(h.state().cards.pending_command(&card.id), Some(command), "the template's command, as typed");

    h.get_by_label("Modèles de terminal du groupe api").click();
    steps(&mut h, 2);
    h.get_by_label("Shell").click();
    steps(&mut h, 3);
    let plain = newest_in_api(&h);
    assert_ne!(plain.id, card.id);
    assert_eq!(h.state().cards.pending_command(&plain.id), None);
    assert_eq!(h.state().world.members("p-api").count(), 2);

    // Closing the card forgets its command.
    h.state_mut().dispatch(vec![Event::RemoveCard(card.id.clone())]);
    assert_eq!(h.state().cards.pending_command(&card.id), None);
}

#[test]
fn templates_are_added_edited_and_removed_in_their_dialog() {
    use egui::accesskit::Role;
    let dir = temp_dir("template-editor");
    let mut h = harness(&dir);
    group_with_templates(&mut h, &[("Vieux", "echo vieux")]);
    h.get_by_label("Modèles de terminal du groupe api").click();
    steps(&mut h, 2);
    h.get_by_label("Modifier les modèles…").click();
    steps(&mut h, 2);
    assert!(h.state().projects_editing(), "the dialog keeps the keyboard");
    assert!(h.query_by_label("Modèles de commande · api").is_some());
    // Remove the old one, add a new one.
    h.get_by_label("Retirer").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Retirer").is_none(), "removed");
    // The dialog shrank: let it settle in its new place before clicking.
    steps(&mut h, 4);
    h.get_by_label("+ Ajouter un modèle").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Retirer").is_some(), "added");
    // The search box of the toolbar comes first.
    let name = h.get_all_by_role(Role::TextInput).last().unwrap();
    name.focus();
    name.type_text("Tests");
    steps(&mut h, 2);
    let command = h.get_by_role(Role::MultilineTextInput);
    command.focus();
    command.type_text("cargo test 'it''s' \"$X\"");
    steps(&mut h, 2);
    h.get_by_label("Enregistrer").click();
    steps(&mut h, 2);
    assert!(!h.state().projects_editing());
    let templates = &h.state().world.project("p-api").unwrap().templates;
    assert_eq!(templates.len(), 1, "{templates:?}");
    assert_eq!((templates[0].name.as_str(), templates[0].command.as_str()), ("Tests", "cargo test 'it''s' \"$X\""));

    // Cancel changes nothing.
    h.get_by_label("Modèles de terminal du groupe api").click();
    steps(&mut h, 2);
    h.get_by_label("Modifier les modèles…").click();
    steps(&mut h, 2);
    h.get_by_label("Retirer").click();
    steps(&mut h, 2);
    h.get_by_label("Annuler").click();
    steps(&mut h, 2);
    assert_eq!(h.state().world.project("p-api").unwrap().templates.len(), 1);
}

#[test]
fn the_template_editor_never_loses_typed_text_by_accident() {
    use egui::accesskit::Role;
    let dir = temp_dir("template-editor-keep");
    let mut h = harness(&dir);
    group_with_templates(&mut h, &[]);
    h.get_by_label("Modèles de terminal du groupe api").click();
    steps(&mut h, 2);
    h.get_by_label("Modifier les modèles…").click();
    steps(&mut h, 2);
    // Unchanged: Escape just closes.
    h.key_press(Key::Escape);
    steps(&mut h, 2);
    assert!(!h.state().projects_editing());

    h.get_by_label("Modèles de terminal du groupe api").click();
    steps(&mut h, 2);
    h.get_by_label("Modifier les modèles…").click();
    steps(&mut h, 2);
    let command = h.get_by_role(Role::MultilineTextInput);
    command.focus();
    command.type_text("claude \"un long prompt\"");
    steps(&mut h, 2);
    // A click outside the dialog keeps it.
    h.event(egui::Event::PointerButton { pos: pos2(5.0, 890.0), button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.event(egui::Event::PointerButton { pos: pos2(5.0, 890.0), button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    steps(&mut h, 3);
    assert!(h.state().projects_editing(), "still open after a click outside");
    // Escape asks first; "Continuer" goes back to the text.
    h.key_press(Key::Escape);
    steps(&mut h, 2);
    assert!(h.query_by_label("Abandonner les modifications de ces modèles ?").is_some());
    h.get_by_label("Continuer").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Abandonner les modifications de ces modèles ?").is_none());
    assert_eq!(h.get_by_role(Role::MultilineTextInput).value().as_deref(), Some("claude \"un long prompt\""));
    // The Cancel button asks too, exactly as Escape does.
    h.get_by_label("Annuler").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Abandonner les modifications de ces modèles ?").is_some());
    h.get_by_label("Continuer").click();
    steps(&mut h, 2);
    assert!(h.state().projects_editing(), "Continuer keeps the editor open");
    assert_eq!(h.get_by_role(Role::MultilineTextInput).value().as_deref(), Some("claude \"un long prompt\""));
    h.get_by_label("Annuler").click();
    steps(&mut h, 2);
    h.get_by_label("Abandonner").click();
    steps(&mut h, 2);
    assert!(!h.state().projects_editing());
    assert!(h.state().world.project("p-api").unwrap().templates.is_empty());
    // Nothing typed: Cancel closes at once.
    h.get_by_label("Modèles de terminal du groupe api").click();
    steps(&mut h, 2);
    h.get_by_label("Modifier les modèles…").click();
    steps(&mut h, 2);
    h.get_by_label("Annuler").click();
    steps(&mut h, 2);
    assert!(!h.state().projects_editing(), "Cancel with nothing typed closes directly");
}

#[test]
fn the_template_menu_in_english() {
    let dir = temp_dir("template-menu-en");
    let mut h = harness_in(&dir, Lang::En);
    group_with_templates(&mut h, &[]);
    h.get_by_label("Terminal templates of group api").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Shell").is_some());
    assert!(h.query_by_label("No template yet").is_some());
    h.get_by_label("Edit templates…").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Command templates · api").is_some());
    assert!(h.query_by_label("Save").is_some());
}

// --- Sending text: quick messages --------------------------------------------------------------

/// Group "api" with these quick messages and a card "srv" in it whose session the agent reported,
/// open, with an agent that delivers text; returns the card.
fn group_with_messages(h: &mut Harness<'_, App>, messages: &[(&str, &str, bool)]) -> String {
    linked_groups(h);
    let p = h.state_mut().world.projects.iter_mut().find(|p| p.id == "p-api").unwrap();
    p.messages = messages.iter().map(|(name, text, enter)| tcanvas::shell::world::QuickMessage { name: (*name).into(), text: (*text).into(), enter: *enter }).collect();
    let id = add_card(h, "srv", pos2(40.0, 40.0));
    let session = format!("tc_{id}");
    h.state_mut().dispatch(vec![
        Event::Agent(AgentEvent::Hello { version: "1".into(), hooks: protocol::Hooks::Installed, send_text: true }),
        Event::Agent(AgentEvent::Session(SessionInfo {
            session,
            status: TermStatus::Idle,
            command: None,
            cwd: Some("/home/me/api".into()),
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
            agent_session_id: None,
            agent_transcript: None,
            last_prompt: None,
        })),
    ]);
    steps(h, 2);
    h.get_by_label("● api").click();
    steps(h, 45);
    id
}

/// Right-click on the card's title bar: its menu.
fn card_menu(h: &mut Harness<'_, App>, id: &str) {
    let title = screen_of(h, h.state().world.card(id).unwrap().rect).min + Vec2::new(150.0, 8.0);
    right_click_at(h, title);
}

/// Text handed to the agent so far: (request, session, text, enter).
fn texts_sent(h: &Harness<'_, App>) -> Vec<(u64, String, String, bool)> {
    h.state()
        .agent_sent()
        .iter()
        .filter_map(|c| match c {
            protocol::AgentCmd::SendText { id, session, text, enter, .. } => Some((*id, session.clone(), text.clone(), *enter)),
            _ => None,
        })
        .collect()
}

/// The agent's answer to a text it was handed.
fn text_sent(h: &mut Harness<'_, App>, request: u64, problem: Option<protocol::Problem>) {
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::TextSent { id: request, session: String::new(), problem, pasted: false })]);
    steps(h, 2);
}

#[test]
fn a_quick_message_goes_to_its_card_session_through_the_agent() {
    use tcanvas::shell::event::SendOutcome;
    let dir = temp_dir("quick-message");
    let mut h = harness(&dir);
    let id = group_with_messages(&mut h, &[("Commit", "Commite tes changements\navec un message clair\n", true), ("Brouillon", "à relire\n", false)]);
    let other = add_card(&mut h, "autre", pos2(700.0, 40.0));
    steps(&mut h, 2);
    card_menu(&mut h, &id);
    assert!(h.state().cards.written(&id).is_empty(), "a right-click on the title bar opens the menu, it does not paste");
    h.get_by_label("Envoyer un message").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Brouillon").is_some());
    assert!(h.query_by_label("Modifier les messages…").is_some());
    h.get_by_label("Commit").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Brouillon").is_none(), "the menu closes");
    // To this card's session only, through the agent (never through the terminal's keys); the
    // trailing line break goes, the Enter submits it once.
    let session = format!("tc_{id}");
    assert_eq!(texts_sent(&h), vec![(0, session.clone(), "Commite tes changements\navec un message clair".into(), true)]);
    assert!(h.state().cards.written(&id).is_empty() && h.state().cards.written(&other).is_empty());
    assert_eq!(h.state().cards.last_outcome(&id), None, "sent once tmux took it");
    text_sent(&mut h, 0, None);
    assert_eq!(h.state().cards.last_outcome(&id), Some(&SendOutcome::Sent));
    assert_eq!(h.state().pending_toasts(), 0, "no notice when it got through");

    // Without "press Enter after": the text as written, line break included.
    card_menu(&mut h, &id);
    h.get_by_label("Envoyer un message").click();
    steps(&mut h, 2);
    h.get_by_label("Brouillon").click();
    steps(&mut h, 2);
    assert_eq!(texts_sent(&h)[1], (1, session, "à relire\n".into(), false));
}

#[test]
fn a_card_that_cannot_take_a_message_says_so_and_keeps_the_text() {
    use tcanvas::shell::event::SendRefusal;
    let dir = temp_dir("quick-message-refused");
    let mut h = harness(&dir);
    let id = group_with_messages(&mut h, &[("Relance", "continue", true)]);
    h.state_mut().world.card_mut(&id).unwrap().pty_dead = true;
    steps(&mut h, 2);
    card_menu(&mut h, &id);
    h.get_by_label("Envoyer un message").click();
    steps(&mut h, 2);
    h.get_by_label("Relance").click();
    steps(&mut h, 2);
    assert!(texts_sent(&h).is_empty(), "nothing sent");
    assert_eq!(h.state().cards.unsent_text(&id), Some(("continue", &SendRefusal::Ended)));
    // On the card, and in a toast (seen even when the card is off screen or zoomed out).
    let message = "Texte non envoyé à « srv » : sa session est terminée.";
    // The toast shows from the frame after its first (sized, then drawn).
    h.step();
    assert_eq!(h.query_all_by_label(message).count(), 2, "the strip and the toast");
    assert!(h.query_by_label("Texte non envoyé").is_some(), "the toast's title");
    h.get_by_label("Copier le texte").click();
    steps(&mut h, 2);
    // Dismissed: gone.
    h.get_by_label("Fermer le texte non envoyé").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Copier le texte").is_none());
    assert_eq!(h.state().cards.unsent_text(&id), None);

    // What tmux could not take is kept too, and a later success clears it.
    h.state_mut().world.card_mut(&id).unwrap().pty_dead = false;
    h.state_mut().dispatch(vec![Event::SendText { card: id.clone(), text: "go".into(), enter: true }]);
    text_sent(&mut h, 0, Some(protocol::Problem::SessionNotFound { session: format!("tc_{id}") }));
    assert_eq!(h.state().cards.unsent_text(&id), Some(("go", &SendRefusal::NoSession)));
    assert!(h.query_all_by_label("Texte non envoyé à « srv » : sa session tmux n'existe plus.").count() >= 1);
    h.state_mut().dispatch(vec![Event::SendText { card: id.clone(), text: "go".into(), enter: true }]);
    text_sent(&mut h, 1, None);
    assert_eq!(h.state().cards.unsent_text(&id), None, "stale once a text got through");

    // Pasted, but its Enter would have landed in a tmux screen: told, not offered again.
    h.state_mut().dispatch(vec![Event::SendText { card: id.clone(), text: "go".into(), enter: true }]);
    let busy = protocol::Problem::PaneBusy { session: format!("tc_{id}"), mode: "clock-mode".into() };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::TextSent { id: 2, session: String::new(), problem: Some(busy), pasted: true })]);
    steps(&mut h, 3);
    assert_eq!(h.state().cards.last_outcome(&id), Some(&tcanvas::shell::event::SendOutcome::PastedNotSubmitted(SendRefusal::Busy("clock-mode".into()))));
    assert_eq!(h.state().cards.unsent_text(&id), None);
    assert!(h.query_by_label("Texte non validé").is_some());
    assert!(h.query_by_label("Texte tapé dans « srv » mais pas validé : son volet affiche un écran tmux (clock-mode) : fermez-le d'abord.").is_some());

    // Still starting (its start command not handed over yet): refused too.
    h.state_mut().dispatch(vec![Event::NewCard { at: None, project: Some("p-api".into()), command: Some("claude".into()) }]);
    steps(&mut h, 45);
    let starting = h.state().world.members("p-api").last().unwrap().id.clone();
    h.state_mut().dispatch(vec![Event::SendText { card: starting.clone(), text: "go".into(), enter: true }]);
    steps(&mut h, 2);
    assert_eq!(h.state().cards.unsent_text(&starting), Some(("go", &SendRefusal::Starting)));
    assert_eq!(texts_sent(&h).len(), 3, "the starting card got none");
}

#[test]
fn without_the_agent_a_message_is_refused_and_kept() {
    use tcanvas::shell::event::SendRefusal;
    let dir = temp_dir("quick-message-no-agent");
    let mut h = harness(&dir);
    let id = group_with_messages(&mut h, &[("Relance", "continue", true)]);
    h.state_mut().dispatch(vec![Event::AgentLost]);
    card_menu(&mut h, &id);
    h.get_by_label("Envoyer un message").click();
    steps(&mut h, 2);
    h.get_by_label("Relance").click();
    steps(&mut h, 2);
    assert!(texts_sent(&h).is_empty());
    assert_eq!(h.state().cards.unsent_text(&id), Some(("continue", &SendRefusal::NoAgent)));
}

#[test]
fn quick_messages_are_added_and_edited_in_their_dialog() {
    use egui::accesskit::Role;
    let dir = temp_dir("quick-message-editor");
    let mut h = harness(&dir);
    let id = group_with_messages(&mut h, &[]);
    card_menu(&mut h, &id);
    h.get_by_label("Envoyer un message").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Aucun message pour l'instant").is_some());
    h.get_by_label("+ Ajouter un message…").click();
    // Opened through an event: one more frame for the dialog to show.
    steps(&mut h, 3);
    assert!(h.state().projects_editing(), "the dialog keeps the keyboard");
    assert!(h.query_by_label("Messages rapides · api").is_some());
    let name = h.get_all_by_role(Role::TextInput).last().unwrap();
    name.focus();
    name.type_text("Relance");
    steps(&mut h, 2);
    let text = h.get_by_role(Role::MultilineTextInput);
    text.focus();
    text.type_text("continue, s'il te plaît");
    steps(&mut h, 2);
    // Enter after: on by default; turned off here.
    h.get_by_label("Appuyer sur Entrée après").click();
    steps(&mut h, 2);
    // Escape with changes asks first.
    h.key_press(Key::Escape);
    steps(&mut h, 2);
    assert!(h.query_by_label("Abandonner les modifications de ces messages ?").is_some());
    h.get_by_label("Continuer").click();
    steps(&mut h, 2);
    h.get_by_label("Enregistrer").click();
    steps(&mut h, 2);
    assert!(!h.state().projects_editing());
    let messages = &h.state().world.project("p-api").unwrap().messages;
    assert_eq!(messages, &vec![tcanvas::shell::world::QuickMessage { name: "Relance".into(), text: "continue, s'il te plaît".into(), enter: false }]);
    assert!(h.state().world.project("p-api").unwrap().templates.is_empty(), "templates untouched");

    // Now offered in the menu, with the editor.
    card_menu(&mut h, &id);
    h.get_by_label("Envoyer un message").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Relance").is_some());
    h.get_by_label("Modifier les messages…").click();
    steps(&mut h, 3);
    h.get_by_label("Retirer").click();
    steps(&mut h, 2);
    h.get_by_label("Annuler").click();
    steps(&mut h, 2);
    assert_eq!(h.state().world.project("p-api").unwrap().messages.len(), 1, "cancel changes nothing");
}

#[test]
fn a_card_outside_any_group_has_no_quick_messages() {
    let dir = temp_dir("quick-message-no-group");
    let mut h = harness(&dir);
    let id = add_card(&mut h, "seul", Pos2::ZERO);
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(250.0, 150.0), zoom: 1.0 };
    steps(&mut h, 2);
    card_menu(&mut h, &id);
    h.get_by_label("Envoyer un message").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("Les messages rapides appartiennent à un groupe : placez ce terminal dans un groupe.").is_some());
    assert!(h.query_by_label("+ Ajouter un message…").is_none());
}

#[test]
fn quick_messages_in_english() {
    let dir = temp_dir("quick-message-en");
    let mut h = harness_in(&dir, Lang::En);
    let id = group_with_messages(&mut h, &[]);
    card_menu(&mut h, &id);
    h.get_by_label("Send a message").click();
    steps(&mut h, 2);
    assert!(h.query_by_label("No message yet").is_some());
    h.get_by_label("+ Add a message…").click();
    steps(&mut h, 3);
    assert!(h.query_by_label("Quick messages · api").is_some());
    assert!(h.query_by_label("Press Enter after").is_some());
    h.get_by_label("Cancel").click();
    steps(&mut h, 2);
    h.state_mut().world.card_mut(&id).unwrap().pty_dead = true;
    h.state_mut().dispatch(vec![Event::SendText { card: id.clone(), text: "go on".into(), enter: true }]);
    steps(&mut h, 2);
    assert_eq!(h.query_all_by_label("Text not sent to “srv”: its session has ended.").count(), 2, "the strip and the toast");
    assert!(h.query_by_label("Copy the text").is_some());
    assert!(h.query_by_label("Dismiss the text not sent").is_some());
}

/// Sending text added no new way for keys to reach a terminal: Ctrl+C is ^C, Ctrl+V pastes,
/// Ctrl+Shift+C sends nothing, Alt+right-click goes to tmux (no paste), right-click pastes.
#[test]
fn keys_and_clicks_still_do_what_they_did() {
    let dir = temp_dir("keys-unchanged");
    let mut h = harness(&dir);
    h.state_mut().cards.set_clipboard(Box::new(tcanvas::features::terminal_card::FixedClipboard(Some("clip".into()))));
    let id = add_card(&mut h, "api", Pos2::ZERO);
    h.state_mut().world.camera = tcanvas::shell::world::Camera { center: pos2(250.0, 150.0), zoom: 1.0 };
    h.state_mut().dispatch(vec![Event::FocusCard(id.clone())]);
    steps(&mut h, 2);
    h.event(egui::Event::Copy);
    steps(&mut h, 2);
    assert_eq!(h.state().cards.written(&id), b"\x03", "Ctrl+C: ^C, never a copy");
    h.event(egui::Event::Paste("a\nb".into()));
    steps(&mut h, 2);
    assert_eq!(h.state().cards.written(&id), b"\x03a\rb", "Ctrl+V: egui's paste");
    ctrl_shift(&h, Key::C);
    steps(&mut h, 2);
    assert_eq!(h.state().cards.written(&id), b"\x03a\rb", "Ctrl+Shift+C: nothing sent");
    let body = screen_of(&h, h.state().world.card(&id).unwrap().rect).center();
    h.hover_at(body);
    h.step();
    h.event(egui::Event::ModifiersChanged(Modifiers::ALT));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: body, button: egui::PointerButton::Secondary, pressed, modifiers: Modifiers::ALT });
        h.step();
    }
    h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    steps(&mut h, 2);
    assert_eq!(h.state().cards.written(&id), b"\x03a\rb", "Alt+right-click: not a paste");
    right_click_at(&mut h, body);
    assert_eq!(h.state().cards.written(&id), b"\x03a\rbclip", "right-click: paste");
    assert!(h.query_by_label("Envoyer un message").is_none(), "no menu in the terminal");
}

/// A second group, `blog`, with its own repository and a merged worktree.
fn blog_group(h: &mut Harness<'_, App>) -> String {
    let id = add_project(h, "blog", Rect::from_min_size(pos2(0.0, 700.0), Vec2::new(800.0, 400.0)));
    h.state_mut().world.projects.iter_mut().find(|p| p.id == id).unwrap().cwd = Some("/home/me/blog".into());
    let posts = protocol::Worktree { merged: Some(true), branch_deletable: true, ..worktree("/home/me/blog-posts", false, "posts") };
    let list = protocol::WorktreeList {
        key: id.clone(),
        repo: Some("/home/me/blog".into()),
        default_branch: Some("main".into()),
        worktrees: vec![worktree("/home/me/blog", true, "main"), posts],
        merged_branches: vec![],
    };
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Worktrees(list))]);
    id
}

fn the_cleanup_window_follows_its_group(lang: Lang) {
    let fr = lang == Lang::Fr;
    let dir = temp_dir(if fr { "cleanup-follow-fr" } else { "cleanup-follow-en" });
    let mut h = harness_in(&dir, lang);
    let (title, button, remove, ask, delete) = if fr {
        ("Nettoyer les worktrees - shop", "♻ 1 à nettoyer", "Supprimer…", "Supprimer définitivement ?", "Supprimer")
    } else {
        ("Clean up worktrees - shop", "♻ 1 to clean up", "Remove…", "Delete for good?", "Delete")
    };
    cleanup_window(&mut h, shop_worktrees("p-shop"), button);
    let blog = blog_group(&mut h);
    steps(&mut h, 2);
    assert!(h.query_by_label(title).is_some());

    // A confirmation up, then another group shown: the window and its confirmation go, nothing sent.
    h.get_by_label(remove).click();
    steps(&mut h, 2);
    assert!(h.query_by_label(ask).is_some());
    h.state_mut().dispatch(vec![Event::EnterSpace(Some(blog.clone()))]);
    steps(&mut h, 3);
    assert!(!h.state().worktrees.cleanup_open() && !h.state().worktrees.has_dialog());
    assert!(h.query_by_label(title).is_none() && h.query_by_label(ask).is_none());
    assert!(sent_cleanups(&h).is_empty());

    // Back in the group: closed until asked again. A removal sent, then "All" shown.
    h.state_mut().dispatch(vec![Event::EnterSpace(Some("p-shop".into()))]);
    steps(&mut h, 3);
    assert!(!h.state().worktrees.cleanup_open());
    h.get_by_label(button).click();
    steps(&mut h, 3);
    h.event(egui::Event::PointerGone);
    h.get_by_label(remove).click();
    steps(&mut h, 2);
    measured_all(&mut h, &["/home/me/shop-taxes"]);
    h.get_by_label(delete).click();
    steps(&mut h, 2);
    let removal = protocol::Cleanup::RemoveWorktree { path: "/home/me/shop-taxes".into(), delete_branch: Some("taxes".into()), ignored: seen(&[]) };
    assert_eq!(sent_cleanups(&h), vec![removal.clone()]);
    h.state_mut().dispatch(vec![Event::EnterSpace(None)]);
    steps(&mut h, 3);
    assert!(!h.state().worktrees.cleanup_open(), "'All' closes it too");

    // In another group, its own window open: the answer is a toast about `shop`, not applied to
    // `blog`'s window.
    h.state_mut().dispatch(vec![Event::EnterSpace(Some(blog))]);
    steps(&mut h, 3);
    h.get_by_label(button).click();
    steps(&mut h, 3);
    let blog_title = if fr { "Nettoyer les worktrees - blog" } else { "Clean up worktrees - blog" };
    assert!(h.query_by_label(blog_title).is_some());
    h.state_mut().dispatch(vec![cleaned(removal, Some("taxes"), None)]);
    steps(&mut h, 2);
    let done = if fr { "✔ Supprimés : /home/me/shop-taxes et la branche taxes" } else { "✔ Removed /home/me/shop-taxes and branch taxes" };
    assert_eq!(h.query_all_by_label(done).count(), 1, "in the toast only");
    assert!(h.query_by_label(title).is_some(), "the toast names the group");
    assert!(h.query_by_label(remove).is_some(), "blog's worktree still offered");
}

#[test]
fn the_cleanup_window_follows_its_group_fr() {
    the_cleanup_window_follows_its_group(Lang::Fr);
}

#[test]
fn the_cleanup_window_follows_its_group_en() {
    the_cleanup_window_follows_its_group(Lang::En);
}

#[test]
fn a_branch_with_no_commit_of_its_own_says_so() {
    for (lang, button, reason, not_merged) in [
        (Lang::Fr, "♻ 1 à nettoyer", "pas encore de commit à elle (nouvelle branche)", "pas fusionnée dans main"),
        (Lang::En, "♻ 1 to clean up", "no commit of its own yet (new branch)", "not merged into main"),
    ] {
        let dir = temp_dir(&format!("cleanup-new-branch-{lang:?}"));
        let mut h = harness_in(&dir, lang);
        let Event::Agent(AgentEvent::Worktrees(mut list)) = shop_worktrees("p-shop") else { unreachable!() };
        list.worktrees.push(protocol::Worktree { merged: None, new_branch: true, ..worktree("/home/me/shop-fresh", false, "fresh") });
        cleanup_window(&mut h, Event::Agent(AgentEvent::Worktrees(list)), button);
        assert!(h.query_by_label(reason).is_some(), "{reason} missing");
        assert_eq!(h.query_all_by_label(not_merged).count(), 1, "only invoices");
    }
}
