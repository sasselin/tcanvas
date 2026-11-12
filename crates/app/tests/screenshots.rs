//! README screenshots: the real app rendered headless with wgpu, with canned terminal output and
//! agent events injected by hand (no PTY, no tmux, no agent), so images are deterministic.
//!
//! Ignored by default (needs a GPU adapter, software ones work). Regenerate with
//! `./scripts/screenshots.sh`; images go to `docs/screenshots/` (or `TCANVAS_SCREENSHOTS_DIR`).

use std::path::PathBuf;

use egui::{Key, Modifiers, Pos2, Rect, Vec2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use protocol::{AgentEvent, ChangeKind, ChangeSet, CommitInfo, FileChange, SessionInfo, TermStatus};
use tcanvas::features::agent_link::AgentMode;
use tcanvas::features::terminal_card::Launcher;
use tcanvas::shell::event::Event;
use tcanvas::shell::i18n::{self, Lang};
use tcanvas::shell::world::{Card, Project, unix_now};
use tcanvas::{App, Config};

/// Window size (points) and scale: 2100 pixels wide images.
const SIZE: Vec2 = vec2(1400.0, 860.0);
const PIXELS_PER_POINT: f32 = 1.5;
/// The overview needs more room so the whole canvas stays above the preview zoom level.
const OVERVIEW_SIZE: Vec2 = vec2(1750.0, 1075.0);
const OVERVIEW_PIXELS_PER_POINT: f32 = 1.2;

// --- ANSI helpers ----------------------------------------------------------------------------

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[1;34m";
const MAGENTA: &str = "\x1b[35m";
const CYAN: &str = "\x1b[36m";
/// Claude's orange.
const CLAUDE: &str = "\x1b[38;5;209m";
const GRAY: &str = "\x1b[38;5;245m";
const DIFF_ADD: &str = "\x1b[48;5;22m";
const DIFF_DEL: &str = "\x1b[48;5;52m";

fn prompt(dir: &str, branch: &str) -> String {
    format!("{BLUE}~/code/{dir}{RESET} {MAGENTA}{branch}{RESET} {GREEN}${RESET} ")
}

/// Visible width of a line (ANSI escapes do not count).
fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut in_escape = false;
    for c in s.chars() {
        match (in_escape, c) {
            (false, '\x1b') => in_escape = true,
            (true, 'm') => in_escape = false,
            (true, _) => {}
            (false, _) => n += 1,
        }
    }
    n
}

/// A rounded box `width` columns wide, as Claude Code draws them.
fn boxed(width: usize, color: &str, lines: &[String]) -> Vec<String> {
    let inner = width - 4;
    let mut out = vec![format!("{color}╭{}╮{RESET}", "─".repeat(width - 2))];
    for l in lines {
        let pad = inner.saturating_sub(visible_len(l));
        out.push(format!("{color}│{RESET} {l}{} {color}│{RESET}", " ".repeat(pad)));
    }
    out.push(format!("{color}╰{}╯{RESET}", "─".repeat(width - 2)));
    out
}

fn screen(lines: Vec<String>) -> String {
    lines.join("\r\n")
}

/// Claude Code's input box and the hint line under it.
fn claude_input(width: usize, hint: &str) -> Vec<String> {
    let mut v = boxed(width, GRAY, &[format!("{BOLD}>{RESET} ")]);
    v.push(format!("  {GRAY}{hint}{RESET}"));
    v
}

fn user(text: &str) -> String {
    format!("{GRAY}> {text}{RESET}")
}

fn say(text: &str) -> String {
    format!("{BOLD}●{RESET} {text}")
}

fn tool(name: &str, arg: &str, result: &str) -> Vec<String> {
    vec![format!("{GREEN}●{RESET} {BOLD}{name}{RESET}({arg})"), format!("  {GRAY}⎿  {result}{RESET}")]
}

// --- Canned terminal output ------------------------------------------------------------------

const CLAUDE_W: usize = 70;

/// Claude Code in the api project, asking before running the tests.
fn claude_api() -> String {
    let mut v = claude_api_steps().concat();
    v.extend(claude_api_question());
    screen(v)
}

/// Claude Code's work in the api project, one step at a time (the demo shows them one by one).
fn claude_api_steps() -> [Vec<String>; 4] {
    let write = tool("Write", "src/middleware/rate_limit.rs", "Wrote 48 lines to src/middleware/rate_limit.rs");
    let mut update = tool("Update", "src/routes/auth.rs", "Updated src/routes/auth.rs with 2 additions and 1 removal");
    update.push(format!("     {GRAY}13{RESET} {DIFF_ADD}+     let limit = RateLimit::per_minute(10);{RESET}"));
    update.push(format!("     {GRAY}16{RESET} {DIFF_DEL}-         .route(\"/login\", post(login)){RESET}"));
    update.push(format!("     {GRAY}16{RESET} {DIFF_ADD}+         .route(\"/login\", post(login).layer(limit)){RESET}"));
    update.push(String::new());
    [
        vec![user("add rate limiting to the login route"), String::new()],
        vec![say("I'll add a per-IP limiter and put it in front of /login."), String::new()].into_iter().chain(tool("Read", "src/routes/auth.rs", "Read 84 lines")).collect(),
        write,
        update,
    ]
}

/// Claude Code asking before running the tests.
fn claude_api_question() -> Vec<String> {
    boxed(
        CLAUDE_W,
        CLAUDE,
        &[
            format!("{BOLD}Bash command{RESET}"),
            String::new(),
            "  cargo test -p shop-api auth".into(),
            format!("  {GRAY}Run the auth tests{RESET}"),
            String::new(),
            "Do you want to proceed?".into(),
            format!("{CLAUDE}❯ 1. Yes{RESET}"),
            "  2. Yes, and don't ask again for cargo test commands".into(),
            format!("  3. No, and tell Claude what to do differently {GRAY}(esc){RESET}"),
        ],
    )
}

/// Claude Code in the web project, at work.
fn claude_web() -> String {
    let mut v = vec![user("show the API's rate-limit error on the login form"), String::new()];
    v.extend(tool("Read", "src/Login.tsx", "Read 112 lines"));
    v.extend(tool("Search", "pattern: \"429\", path: \"src\"", "Found 0 files"));
    v.extend(tool("Update", "src/api.ts", "Updated src/api.ts with 6 additions"));
    v.extend(tool("Update", "src/Login.tsx", "Updated src/Login.tsx with 9 additions and 2 removals"));
    v.push(String::new());
    v.push(say("The API answers 429 with a Retry-After header; the form now shows"));
    v.push("  \"Too many attempts, try again in 42 s\" and disables the button.".into());
    v.push(String::new());
    v.push(format!("{CLAUDE}✻ Checking the error banner...{RESET} {GRAY}(38s · ↓ 2.1k tokens · esc to interrupt){RESET}"));
    v.push(String::new());
    v.extend(claude_input(CLAUDE_W, "⏵⏵ accept edits on (shift+tab to cycle)"));
    screen(v)
}

/// Claude Code in the infra project, done and waiting for the next request.
fn claude_infra() -> String {
    let mut v = vec![user("why does the staging plan want to replace the database?"), String::new()];
    v.push(say("aws_db_instance.main is replaced because engine_version went from"));
    v.push("  15.4 to 16.1 in variables.tf: a major upgrade forces a new instance.".into());
    v.push(String::new());
    v.push("  Kept 15.x for now and pinned the minor version:".into());
    v.push(String::new());
    v.extend(tool("Update", "variables.tf", "Updated variables.tf with 1 addition and 1 removal"));
    v.push(format!("     {GRAY}12{RESET} {DIFF_DEL}-   default = \"16.1\"{RESET}"));
    v.push(format!("     {GRAY}12{RESET} {DIFF_ADD}+   default = \"15.6\"{RESET}"));
    v.push(String::new());
    v.push(say(&format!("The plan is now {GREEN}2 to change, 0 to destroy{RESET}.")));
    v.push(String::new());
    v.extend(claude_input(CLAUDE_W, "? for shortcuts"));
    screen(v)
}

fn cargo_test() -> String {
    let p = prompt("shop-api", "main");
    format!(
        "{p}cargo test\r\n{GREEN}{BOLD}   Compiling{RESET} shop-api v0.4.0 (/home/me/code/shop-api)\r\n{GREEN}{BOLD}    Finished{RESET} `test` profile in 6.12s\r\n{GREEN}{BOLD}     Running{RESET} unittests src/lib.rs\r\n\r\nrunning 42 tests\r\ntest auth::tests::login_ok ... {GREEN}ok{RESET}\r\ntest auth::tests::bad_password ... {GREEN}ok{RESET}\r\ntest db::tests::migrations ... {GREEN}ok{RESET}\r\ntest routes::tests::health ... {GREEN}ok{RESET}\r\n\r\ntest result: {GREEN}ok{RESET}. 42 passed; 0 failed; finished in 0.84s\r\n\r\n{p}"
    )
}

fn git_log() -> String {
    let p = prompt("shop-api", "main");
    format!(
        "{p}git log --oneline --graph -8\r\n* {YELLOW}3f9c2a1{RESET} {CYAN}(HEAD -> main){RESET} auth: hash with argon2\r\n* {YELLOW}8b17d0e{RESET} db: index sessions by user\r\n* {YELLOW}51aa9c4{RESET} routes: health check\r\n*   {YELLOW}c02e7f8{RESET} Merge branch 'feat/tokens'\r\n|\\  \r\n| * {YELLOW}9d4e211{RESET} tokens: refresh flow\r\n| * {YELLOW}77b0c3a{RESET} tokens: signed cookies\r\n|/  \r\n* {YELLOW}e1f5b62{RESET} first API skeleton\r\n{p}"
    )
}

fn dev_server() -> String {
    let p = prompt("shop-web", "feat/login");
    format!(
        "{p}npm run dev\r\n\r\n  {GREEN}{BOLD}VITE v6.2.0{RESET}  ready in 412 ms\r\n\r\n  {GREEN}->{RESET}  Local:   {CYAN}http://localhost:5173/{RESET}\r\n  {GREEN}->{RESET}  Network: use --host to expose\r\n\r\n{DIM}10:42:07{RESET} {CYAN}[vite]{RESET} hmr update /src/Login.tsx\r\n{DIM}10:42:31{RESET} {CYAN}[vite]{RESET} hmr update /src/api.ts\r\n{DIM}10:43:02{RESET} {CYAN}[vite]{RESET} page reload src/main.tsx\r\n"
    )
}

fn failed_build() -> String {
    let p = prompt("shop-web", "feat/login");
    format!(
        "{p}npm run typecheck\r\n\r\n{CYAN}src/Login.tsx{RESET}:{YELLOW}27{RESET}:{YELLOW}9{RESET} - {RED}error{RESET} {DIM}TS2322{RESET}: Type 'string | undefined'\r\n  is not assignable to type 'string'.\r\n\r\n\x1b[7m27{RESET}   token={{session.token}}\r\n\x1b[7m  {RESET}   {RED}~~~~~{RESET}\r\n\r\nFound 1 error in src/Login.tsx{DIM}:27{RESET}\r\n\r\n{p}"
    )
}

fn terraform_plan() -> String {
    let p = prompt("infra", "main");
    format!(
        "{p}terraform plan -var-file=staging.tfvars\r\n\r\n  {YELLOW}~{RESET} aws_ecs_service.api\r\n      desired_count = 2 {YELLOW}->{RESET} 3\r\n  {YELLOW}~{RESET} aws_db_instance.main\r\n      engine_version = \"15.4\" {YELLOW}->{RESET} \"15.6\"\r\n\r\n{BOLD}Plan:{RESET} 0 to add, 2 to change, 0 to destroy.\r\n\r\n{p}"
    )
}

fn deploy_log() -> String {
    format!(
        "{BOLD}${RESET} ./deploy.sh staging\r\n{BLUE}==>{RESET} building image web:3f9c2a1\r\n{BLUE}==>{RESET} pushing to registry\r\n{BLUE}==>{RESET} rolling out 3 replicas\r\n    replica 1/3 {GREEN}ready{RESET}\r\n    replica 2/3 {GREEN}ready{RESET}\r\n    replica 3/3 {YELLOW}waiting for health check{RESET}\r\n"
    )
}

/// Claude Code in the blog, done writing a post.
fn claude_blog() -> String {
    let mut v = vec![user("draft a post about our move to Postgres 15"), String::new()];
    v.extend(tool("Read", "content/posts/2026-10-sqlite-limits.md", "Read 64 lines"));
    v.extend(tool("Write", "content/posts/2026-11-postgres.md", "Wrote 118 lines to content/posts/2026-11-postgres.md"));
    v.push(String::new());
    v.push(say("Draft ready: \"Why we moved to Postgres\", 6 min read, with the"));
    v.push("  benchmark table from last month's post. Tagged `draft: true`.".into());
    v.push(String::new());
    v.extend(claude_input(CLAUDE_W, "? for shortcuts"));
    screen(v)
}

fn hugo_server() -> String {
    let p = prompt("blog", "main");
    format!(
        "{p}hugo server -D\r\nStart building sites ...\r\nhugo v0.139.0+extended linux/amd64\r\n\r\n                   | EN\r\n-------------------+-----\r\n  Pages            | 48\r\n  Paginator pages  |  3\r\n  Static files     | 21\r\n\r\nBuilt in 212 ms\r\nWeb Server is available at {CYAN}http://localhost:1313/{RESET}\r\nChange detected, rebuilding site (#3).\r\n{GRAY}Source changed /posts/2026-11-postgres.md{RESET}\r\n"
    )
}

fn training_log() -> String {
    let p = prompt("churn-model", "exp/gbm");
    let mut s = format!("{p}python train.py --config configs/gbm.yaml\r\n{GRAY}loaded 1,284,330 rows, 42 features{RESET}\r\n\r\n");
    for (epoch, loss, auc) in [(1, 0.512, 0.781), (2, 0.447, 0.812), (3, 0.419, 0.829), (4, 0.405, 0.836), (5, 0.398, 0.841)] {
        s += &format!("epoch {epoch:>2}/12  loss {loss:.3}  val_auc {GREEN}{auc:.3}{RESET}\r\n");
    }
    s += &format!("epoch  6/12  {CYAN}[#############-----------]{RESET} 54%  eta 3:12\r\n");
    s
}

fn gpu_stats() -> String {
    let p = prompt("churn-model", "exp/gbm");
    format!(
        "{p}nvidia-smi --query-gpu=name,utilization.gpu,memory.used --format=csv\r\nname, utilization.gpu [%], memory.used [MiB]\r\nNVIDIA GeForce RTX 4070, {GREEN}97 %{RESET}, 9874 MiB\r\n\r\n{p}"
    )
}

// --- Scene building --------------------------------------------------------------------------

fn out_dir() -> PathBuf {
    std::env::var_os("TCANVAS_SCREENSHOTS_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/screenshots"))
}

fn harness(name: &str) -> Harness<'static, App> {
    harness_sized(name, SIZE, PIXELS_PER_POINT)
}

fn harness_sized(name: &str, size: Vec2, pixels_per_point: f32) -> Harness<'static, App> {
    i18n::set(Lang::En);
    let dir = std::env::temp_dir().join(format!("tcanvas-shot-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    Harness::builder().with_size(size).with_pixels_per_point(pixels_per_point).with_step_dt(1.0 / 60.0).wgpu().build_eframe(move |cc| {
        let mut app = App::new(&cc.egui_ctx, Config { launcher: Launcher::Disabled, agent: AgentMode::Disabled, config_dir: dir, os_toasts: false, update_check: false });
        // A fixed version: the images must not change on every release.
        app.version = "X.X.X";
        add_windows_symbols(&cc.egui_ctx);
        app
    })
}

/// On Windows the app falls back on Segoe UI Symbol, which has every glyph Claude Code draws
/// (`⎿`, `⏵`, ...); the Linux fallback (DejaVu) lacks some. Use the Windows font, read through
/// WSL, so the images look like the real app.
fn add_windows_symbols(ctx: &egui::Context) {
    const SEGOE_SYMBOL: &str = "/mnt/c/Windows/Fonts/seguisym.ttf";
    let bytes = std::fs::read(SEGOE_SYMBOL).unwrap_or_else(|e| panic!("{SEGOE_SYMBOL}: {e} (run the screenshots from WSL)"));
    let families = [egui::FontFamily::Monospace, egui::FontFamily::Proportional].map(|family| egui::epaint::text::InsertFontFamily { family, priority: egui::epaint::text::FontPriority::Lowest });
    ctx.add_font(egui::epaint::text::FontInsert::new("segoe-ui-symbol", egui::FontData::from_owned(bytes), families.to_vec()));
}

fn steps(h: &mut Harness<'_, App>, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

/// A group whose frame starts at `at` (it then fits its members). Frames must not start
/// overlapping: the app would push their terminals apart.
fn add_project(h: &mut Harness<'_, App>, name: &str, color: [u8; 3], at: Pos2) -> String {
    let id = format!("p-{name}");
    let rect = Rect::from_min_size(at, Vec2::splat(100.0));
    h.state_mut().world.projects.push(Project { id: id.clone(), name: name.into(), color, rect, cwd: Some(format!("/home/me/code/{name}")), templates: Vec::new(), messages: Vec::new() });
    id
}

/// One terminal of the demo: its name, group, place (world units) and what it shows.
struct Term<'a> {
    name: &'a str,
    project: Option<&'a str>,
    at: Pos2,
    size: Vec2,
    command: Option<&'a str>,
    status: TermStatus,
    output: String,
}

fn add_card(h: &mut Harness<'_, App>, id: &str, t: Term<'_>) {
    let app = h.state_mut();
    let cwd = format!("/home/me/code/{}", t.project.map_or("ops", |p| p.trim_start_matches("p-")));
    let mut card = Card::with_id(id.into(), Rect::from_min_size(t.at, t.size), Some(cwd));
    card.name = Some(t.name.into());
    card.project = t.project.map(String::from);
    let session = card.session();
    app.world.add_card(card);
    app.cards.show_canned(&app.world, id, t.output.as_bytes());
    let info = SessionInfo {
        session,
        status: t.status,
        command: t.command.map(String::from),
        cwd: None,
        last_exit: None,
        last_duration_secs: None,
        tail: vec![],
        agent_session_id: None,
        agent_transcript: None,
        last_prompt: None,
    };
    app.dispatch(vec![Event::Agent(AgentEvent::Session(info))]);
}

/// Uncommitted files and recent commits of the `shop-api` project (its "Changes" panel).
fn api_changes() -> ChangeSet {
    let now = unix_now();
    let file = |path: &str, kind: ChangeKind, added, removed, age| FileChange { path: path.into(), kind, added: Some(added), removed: Some(removed), mtime: now - age, binary: false };
    let commit = |sha: &str, summary: &str, age| CommitInfo { sha: format!("{sha:0<40}"), summary: summary.into(), author: "me".into(), time: now - age };
    ChangeSet {
        key: "p-shop-api".into(),
        root: Some("/home/me/code/shop-api".into()),
        git: true,
        branch: Some("main".into()),
        detached: false,
        error: None,
        files: vec![
            file("src/middleware/rate_limit.rs", ChangeKind::Untracked, 48, 0, 20),
            file("src/routes/auth.rs", ChangeKind::Modified, 6, 2, 5),
            file("src/middleware/mod.rs", ChangeKind::Modified, 2, 0, 60),
            file("Cargo.toml", ChangeKind::Modified, 1, 0, 300),
        ],
        truncated: false,
        commits: vec![commit("3f9c2a1", "auth: hash with argon2", 3_600), commit("8b17d0e", "db: index sessions by user", 7_200), commit("51aa9c4", "routes: health check", 86_400)],
    }
}

/// The demo canvas: several unrelated projects, each a group linked to its folder, most with a
/// Claude Code session. The shop's API is wired to its front end and to the infra; a new group
/// has no folder yet; one terminal lives outside any group.
fn demo_canvas(h: &mut Harness<'_, App>) {
    let api = add_project(h, "shop-api", [94, 129, 244], pos2(0.0, 0.0));
    let web = add_project(h, "shop-web", [72, 187, 120], pos2(1420.0, 0.0));
    let blog = add_project(h, "blog", [200, 90, 160], pos2(0.0, 780.0));
    let ml = add_project(h, "churn-model", [70, 160, 200], pos2(1420.0, 780.0));
    let infra = add_project(h, "infra", [230, 150, 60], pos2(0.0, 1360.0));
    let claude = vec2(640.0, 470.0);
    let terms = [
        Term { name: "claude", project: Some(&api), at: pos2(0.0, 0.0), size: claude, command: Some("claude"), status: TermStatus::WaitingInput, output: claude_api() },
        Term { name: "tests", project: Some(&api), at: pos2(680.0, 0.0), size: vec2(540.0, 300.0), command: None, status: TermStatus::Idle, output: cargo_test() },
        Term { name: "git log", project: Some(&api), at: pos2(680.0, 330.0), size: vec2(540.0, 260.0), command: None, status: TermStatus::Idle, output: git_log() },
        Term { name: "claude", project: Some(&web), at: pos2(1420.0, 0.0), size: claude, command: Some("claude"), status: TermStatus::Running, output: claude_web() },
        Term { name: "dev server", project: Some(&web), at: pos2(2100.0, 0.0), size: vec2(520.0, 250.0), command: Some("npm run dev"), status: TermStatus::Running, output: dev_server() },
        Term { name: "typecheck", project: Some(&web), at: pos2(2100.0, 290.0), size: vec2(520.0, 250.0), command: None, status: TermStatus::Exited(2), output: failed_build() },
        Term { name: "claude", project: Some(&blog), at: pos2(0.0, 780.0), size: vec2(640.0, 330.0), command: Some("claude"), status: TermStatus::Idle, output: claude_blog() },
        Term { name: "hugo", project: Some(&blog), at: pos2(680.0, 780.0), size: vec2(540.0, 330.0), command: Some("hugo server -D"), status: TermStatus::Running, output: hugo_server() },
        Term { name: "training", project: Some(&ml), at: pos2(1420.0, 780.0), size: vec2(600.0, 300.0), command: Some("python train.py"), status: TermStatus::Running, output: training_log() },
        Term { name: "gpu", project: Some(&ml), at: pos2(2060.0, 780.0), size: vec2(560.0, 200.0), command: None, status: TermStatus::Idle, output: gpu_stats() },
        Term { name: "claude", project: Some(&infra), at: pos2(0.0, 1360.0), size: vec2(640.0, 420.0), command: Some("claude"), status: TermStatus::Idle, output: claude_infra() },
        Term { name: "plan", project: Some(&infra), at: pos2(680.0, 1360.0), size: vec2(540.0, 260.0), command: None, status: TermStatus::Idle, output: terraform_plan() },
        Term { name: "deploy", project: None, at: pos2(2100.0, 1400.0), size: vec2(520.0, 250.0), command: Some("./deploy.sh staging"), status: TermStatus::Stalled, output: deploy_log() },
    ];
    for (i, t) in terms.into_iter().enumerate() {
        // Ids must be unique: the Claude sessions share a name.
        add_card(h, &format!("t{i}"), t);
    }
    // A group just created: no folder, no terminal yet.
    let rect = Rect::from_min_size(pos2(1420.0, 1360.0), vec2(560.0, 300.0));
    h.state_mut().world.projects.push(Project { id: "p-mobile-app".into(), name: "mobile-app".into(), color: [130, 110, 220], rect, cwd: None, templates: Vec::new(), messages: Vec::new() });
    h.state_mut().dispatch(vec![Event::AddWire(api.clone(), web), Event::AddWire(api, infra), Event::Agent(AgentEvent::Changes(api_changes()))]);
    steps(h, 5);
}

fn save(h: &mut Harness<'_, App>, name: &str) {
    steps(h, 60);
    let image = h.render().expect("wgpu render (is a GPU adapter available?)");
    let first = image.get_pixel(0, 0);
    assert!(image.pixels().any(|p| p != first), "{name}: uniform image");
    let dir = out_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.png"));
    image.save(&path).unwrap();
    eprintln!("wrote {}", path.display());
}

fn ctrl_shift(h: &Harness<'_, App>, key: Key) {
    h.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, key);
}

// --- Scenes ----------------------------------------------------------------------------------

#[test]
#[ignore = "renders README screenshots; run ./scripts/screenshots.sh"]
fn overview() {
    let mut h = harness_sized("overview", OVERVIEW_SIZE, OVERVIEW_PIXELS_PER_POINT);
    demo_canvas(&mut h);
    ctrl_shift(&h, Key::Num0);
    save(&mut h, "overview");
}

#[test]
#[ignore = "renders README screenshots; run ./scripts/screenshots.sh"]
fn project() {
    let mut h = harness("project");
    demo_canvas(&mut h);
    ctrl_shift(&h, Key::Num1);
    steps(&mut h, 3);
    assert!(h.query_by_label("⇄ shop-web").is_some() && h.query_by_label("⇄ infra").is_some(), "linked groups shown as chips");
    save(&mut h, "project");
}

#[test]
#[ignore = "renders README screenshots; run ./scripts/screenshots.sh"]
fn changes() {
    let mut h = harness("changes");
    demo_canvas(&mut h);
    ctrl_shift(&h, Key::Num1);
    steps(&mut h, 45);
    h.get_by_label("Δ Changes (4)").click();
    steps(&mut h, 3);
    h.event(egui::Event::PointerGone);
    let diff = "@@ -12,9 +12,13 @@ use crate::middleware::RateLimit;\n pub fn router(state: AppState) -> Router {\n+    let limit = RateLimit::per_minute(10);\n     Router::new()\n         .route(\"/health\", get(health))\n-        .route(\"/login\", post(login))\n+        .route(\"/login\", post(login).layer(limit))\n         .route(\"/logout\", post(logout))\n-        .with_state(state)\n+        .with_state(state.clone())\n+        .layer(TraceLayer::new_for_http())\n }\n";
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Diff { key: "p-shop-api".into(), file: "src/routes/auth.rs".into(), text: diff.into(), truncated: false, problem: None })]);
    save(&mut h, "changes");
}

#[test]
#[ignore = "renders README screenshots; run ./scripts/screenshots.sh"]
fn jump() {
    let mut h = harness("jump");
    demo_canvas(&mut h);
    ctrl_shift(&h, Key::Num0);
    steps(&mut h, 45);
    ctrl_shift(&h, Key::P);
    steps(&mut h, 2);
    assert!(h.state().jump_open());
    h.event(egui::Event::Text("cla".into()));
    save(&mut h, "jump");
}

#[test]
#[ignore = "renders README screenshots; run ./scripts/screenshots.sh"]
fn shortcuts() {
    let mut h = harness("shortcuts");
    demo_canvas(&mut h);
    ctrl_shift(&h, Key::Num0);
    steps(&mut h, 45);
    ctrl_shift(&h, Key::K);
    steps(&mut h, 3);
    assert!(h.state().shortcuts_open());
    h.event(egui::Event::PointerGone);
    save(&mut h, "shortcuts");
}

#[test]
#[ignore = "renders README screenshots; run ./scripts/screenshots.sh"]
fn update() {
    use tcanvas::features::updates::release::{self, Asset, Release};
    let mut h = harness("update");
    demo_canvas(&mut h);
    ctrl_shift(&h, Key::Num0);
    steps(&mut h, 45);
    h.state_mut().version = "1.4.2";
    h.state_mut().updates.pin_running_version("1.4.2");
    let make = |version: &str, notes: &str| {
        let files = [release::asset_name(cfg!(windows), version), release::SUMS.to_string()];
        Release {
            version: version.into(),
            notes: notes.into(),
            page: release::release_page(version),
            assets: files.into_iter().map(|name| Asset { url: format!("https://example.com/{name}"), name }).collect(),
        }
    };
    h.state().updates.offer(vec![
        make("1.5.0", "### Added\n\n- Update button in the toolbar, with the release notes before updating\n- Search inside a terminal's scrollback\n\n### Fixed\n\n- Wires no longer flicker while a group is dragged"),
        make("1.4.3", "### Fixed\n\n- Paste with right-click keeps line breaks in bracketed paste mode"),
    ]);
    steps(&mut h, 3);
    h.get_by_label("Update to v1.5.0").click();
    steps(&mut h, 3);
    h.event(egui::Event::PointerGone);
    save(&mut h, "update");
}

#[test]
#[ignore = "renders README screenshots; run ./scripts/screenshots.sh"]
fn folder() {
    let mut h = harness("folder");
    demo_canvas(&mut h);
    ctrl_shift(&h, Key::Num0);
    steps(&mut h, 45);
    h.get_by_label("Folder of group mobile-app").click();
    steps(&mut h, 2);
    // The agent's answer: the WSL folders, git repositories marked.
    let answer = |h: &mut Harness<'_, App>, shown: &str, parent: &str, git: bool, dirs: &[(&str, bool)]| {
        let Some(protocol::AgentCmd::ListDir { path }) = h.state().agent_sent().last().cloned() else { panic!("the picker asks the agent for a folder") };
        let listing = protocol::DirListing {
            request: path,
            path: Some(shown.into()),
            parent: Some(parent.into()),
            git,
            worktree: false,
            dirs: dirs.iter().map(|(n, g)| protocol::DirEntry { name: (*n).into(), git: *g, worktree: false }).collect(),
            error: None,
        };
        h.state_mut().dispatch(vec![Event::Agent(AgentEvent::DirListing(listing))]);
        steps(h, 2);
    };
    let repos = [("blog", true), ("churn-model", true), ("infra", true), ("mobile-app", true), ("notes", false), ("shop-api", true), ("shop-web", true)];
    answer(&mut h, "/home/me/code", "/home/me", false, &repos);
    // Into the repository, ready to link.
    h.get_by_label("📁 mobile-app").click();
    steps(&mut h, 2);
    answer(&mut h, "/home/me/code/mobile-app", "/home/me/code", true, &[("android", false), ("assets", false), ("ios", false), ("src", false)]);
    assert!(h.query_by_label("Link this folder").is_some());
    h.event(egui::Event::PointerGone);
    save(&mut h, "folder");
}

#[test]
#[ignore = "renders README screenshots; run ./scripts/screenshots.sh"]
fn worktrees() {
    let mut h = harness("worktrees");
    worktrees_canvas(&mut h, false);
    assert!(h.query_by_label("⎇ 3 worktrees").is_some() && h.query_by_label_contains("Branch: feat/rate-limit").is_some());
    h.event(egui::Event::PointerGone);
    save(&mut h, "worktrees");
}

#[test]
#[ignore = "renders README screenshots; run ./scripts/screenshots.sh"]
fn cleanup() {
    let mut h = harness("cleanup");
    worktrees_canvas(&mut h, true);
    h.get_by_label("♻ 2 to clean up").click();
    steps(&mut h, 3);
    // The agent's answer to the size request sent when the window opened.
    let sizes = [("/home/me/code/shop-api-argon2", 412), ("/home/me/code/shop-api-csv-export", 1_310)]
        .map(|(path, mb)| protocol::WorktreeSize { path: path.into(), bytes: Some(mb * 1024 * 1024), ignored: Some(protocol::Ignored { shown: vec![".env".into(), "target/".into()], total: 2 }) })
        .to_vec();
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::WorktreeSizes { key: "p-shop-api".into(), sizes })]);
    steps(&mut h, 3);
    assert!(h.query_by_label("1.7 GB to reclaim").is_some());
    h.event(egui::Event::PointerGone);
    save(&mut h, "cleanup");
}

/// The `shop-api` group open with its worktrees. `cleanup`: the argon2 terminal moved back to
/// the main checkout, another merged worktree, a missing one and merged branches, so there is
/// something to clean up.
fn worktrees_canvas(h: &mut Harness<'_, App>, cleanup: bool) {
    demo_canvas(h);
    // Claude works in a worktree nested in the repository, the tests run in another one.
    let root = "/home/me/code/shop-api";
    let rate_limit = format!("{root}/.claude/worktrees/rate-limit");
    let argon2 = "/home/me/code/shop-api-argon2";
    h.state_mut().world.card_mut("t0").unwrap().cwd = Some(rate_limit.clone());
    h.state_mut().world.card_mut("t1").unwrap().cwd = Some(if cleanup { root.into() } else { argon2.into() });
    let now = unix_now();
    let worktree = |path: &str, main: bool, branch: &str, age: u64| protocol::Worktree {
        path: path.into(),
        head: Some("3f9c2a1".repeat(6)[..40].into()),
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
        last_commit: Some(now - age),
        branch_deletable: true,
        reflog_commits: false,
    };
    let mut list = protocol::WorktreeList {
        key: "p-shop-api".into(),
        repo: Some(root.into()),
        default_branch: Some("origin/main".into()),
        worktrees: vec![
            protocol::Worktree { dirty: Some(true), ..worktree(root, true, "main", 3_600) },
            protocol::Worktree { dirty: Some(true), ahead: Some(3), behind: Some(1), merged: Some(false), ..worktree(&rate_limit, false, "feat/rate-limit-login-and-signup", 300) },
            protocol::Worktree { ahead: Some(0), behind: Some(4), merged: Some(true), ..worktree(argon2, false, "auth/argon2", 86_400) },
        ],
        merged_branches: vec![],
    };
    if cleanup {
        list.worktrees.push(protocol::Worktree { ahead: Some(0), behind: Some(9), merged: Some(true), ..worktree("/home/me/code/shop-api-csv-export", false, "feat/csv-export", 5 * 86_400) });
        list.worktrees.push(protocol::Worktree {
            prunable: true,
            dirty: None,
            ahead: Some(0),
            behind: Some(15),
            merged: Some(true),
            ..worktree("/home/me/code/shop-api-spike", false, "spike/grpc", 20 * 86_400)
        });
        list.merged_branches = ["chore/bump-axum", "fix/readme-typo"].map(|name| protocol::MergedBranch { name: name.into(), last_commit: Some(now - 9 * 86_400) }).to_vec();
    }
    h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Worktrees(list))]);
    ctrl_shift(h, Key::Num1);
    steps(h, 45);
}

/// The README's animated demo: frames rendered here, assembled into `demo.gif` by
/// `./scripts/screenshots.sh` (ffmpeg). Smaller than the stills: the GIF must stay light.
const DEMO_SIZE: Vec2 = vec2(1280.0, 760.0);
const DEMO_PIXELS_PER_POINT: f32 = 1.0;
/// One frame kept every `DEMO_STEPS_PER_FRAME` app steps (60 per second): 12 frames per second.
const DEMO_STEPS_PER_FRAME: usize = 5;

/// Where the demo frames go (the script reads them from there).
fn demo_frames_dir() -> PathBuf {
    std::env::var_os("TCANVAS_DEMO_FRAMES").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/demo-frames"))
}

/// A file of the demo's changes: path, kind, lines added and removed, age in seconds.
type DemoFile = (&'static str, ChangeKind, u32, u32, u64);

/// Records the app's frames while it runs, and plays the agent's part for the diffs.
struct Recorder {
    dir: PathBuf,
    frame: usize,
    /// Agent commands already answered.
    answered: usize,
    /// The uncommitted files of `shop-api`. Sent again with fresh dates between frames: the panel
    /// reads the wall clock, and rendering takes far longer than the time the demo shows.
    files: Vec<DemoFile>,
}

impl Recorder {
    fn new() -> Self {
        let dir = demo_frames_dir();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir, frame: 0, answered: 0, files: vec![] }
    }

    /// The agent reports the uncommitted files of `shop-api`.
    fn set_files(&mut self, h: &mut Harness<'_, App>, files: &[DemoFile]) {
        self.files = files.to_vec();
        self.send_files(h);
    }

    fn send_files(&self, h: &mut Harness<'_, App>) {
        let now = unix_now();
        let files = self.files.iter().map(|(path, kind, added, removed, age)| FileChange {
            path: (*path).into(),
            kind: kind.clone(),
            added: Some(*added),
            removed: Some(*removed),
            mtime: now - age,
            binary: false,
        });
        let set = ChangeSet { files: files.collect(), ..api_changes() };
        h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Changes(set))]);
    }

    /// Answers the diffs the panel asked for since last time.
    fn answer_diffs(&mut self, h: &mut Harness<'_, App>) {
        let asked: Vec<_> = h.state().agent_sent()[self.answered..].to_vec();
        self.answered += asked.len();
        for cmd in asked {
            if let protocol::AgentCmd::Diff { key, file } = cmd {
                let text = demo_diff(&file).into();
                h.state_mut().dispatch(vec![Event::Agent(AgentEvent::Diff { key, file, text, truncated: false, problem: None })]);
            }
        }
    }

    /// Runs the app for `secs` (app time) and keeps 12 frames per second.
    fn play(&mut self, h: &mut Harness<'_, App>, secs: f32) {
        for i in 0..(secs * 60.0).round() as usize {
            // Right after a frame, so the panel has settled before the next one.
            if i % DEMO_STEPS_PER_FRAME == 1 {
                self.send_files(h);
            }
            self.answer_diffs(h);
            h.step();
            if i % DEMO_STEPS_PER_FRAME == 0 {
                let image = h.render().expect("wgpu render (is a GPU adapter available?)");
                image.save(self.dir.join(format!("{:04}.png", self.frame))).unwrap();
                self.frame += 1;
            }
        }
    }
}

/// Shows `lines` in the api project's Claude Code card, with that status.
fn api_claude(h: &mut Harness<'_, App>, lines: Vec<String>, status: TermStatus) {
    let app = h.state_mut();
    app.cards.show_canned(&app.world, "t0", screen(lines).as_bytes());
    let session = app.world.card("t0").unwrap().session();
    let tail = vec!["Do you want to proceed?".into()];
    let info =
        SessionInfo { session, status, command: Some("claude".into()), cwd: None, last_exit: None, last_duration_secs: None, tail, agent_session_id: None, agent_transcript: None, last_prompt: None };
    app.dispatch(vec![Event::Agent(AgentEvent::Session(info))]);
}

fn demo_diff(file: &str) -> &'static str {
    match file {
        "src/middleware/rate_limit.rs" => RATE_LIMIT_DIFF,
        "src/routes/auth.rs" => AUTH_DIFF,
        _ => "@@ -14,6 +14,7 @@ edition = \"2024\"\n [dependencies]\n axum = \"0.8\"\n+dashmap = \"6\"\n serde = { version = \"1\", features = [\"derive\"] }\n",
    }
}

const RATE_LIMIT_DIFF: &str = "@@ -0,0 +1,48 @@\n+//! Per-IP rate limiting for sensitive routes.\n+\n+use std::net::IpAddr;\n+use std::time::{Duration, Instant};\n+\n+use dashmap::DashMap;\n+\n+pub struct RateLimit {\n+    per: Duration,\n+    max: u32,\n+    hits: DashMap<IpAddr, (Instant, u32)>,\n+}\n+\n+impl RateLimit {\n+    pub fn per_minute(max: u32) -> Self {\n+        Self { per: Duration::from_secs(60), max, hits: DashMap::new() }\n+    }\n+\n+    /// `Err(retry_after)` once `ip` went over the limit.\n+    pub fn check(&self, ip: IpAddr) -> Result<(), Duration> {\n";

const AUTH_DIFF: &str = "@@ -12,9 +12,10 @@ use crate::middleware::RateLimit;\n pub fn router(state: AppState) -> Router {\n+    let limit = RateLimit::per_minute(10);\n     Router::new()\n         .route(\"/health\", get(health))\n-        .route(\"/login\", post(login))\n+        .route(\"/login\", post(login).layer(limit))\n         .route(\"/logout\", post(logout))\n         .with_state(state)\n }\n";

#[test]
#[ignore = "renders README screenshots; run ./scripts/screenshots.sh"]
fn demo() {
    let mut h = harness_sized("demo", DEMO_SIZE, DEMO_PIXELS_PER_POINT);
    demo_canvas(&mut h);
    let [ask, read, write, update] = claude_api_steps();
    api_claude(&mut h, ask.clone(), TermStatus::Running);
    let mut rec = Recorder::new();
    let cargo = || -> DemoFile { ("Cargo.toml", ChangeKind::Modified, 1, 0, 300) };
    rec.set_files(&mut h, &[cargo()]);
    ctrl_shift(&h, Key::Num0);
    steps(&mut h, 45);

    // The overview, then a flight into the shop's API.
    rec.play(&mut h, 1.5);
    ctrl_shift(&h, Key::Num1);
    rec.play(&mut h, 1.2);
    h.get_by_label("Δ Changes (1)").click();
    h.step();
    h.event(egui::Event::PointerGone);
    rec.play(&mut h, 0.8);

    // Claude at work: every file it writes shows up in the panel, with its diff.
    let mut lines = [ask, read].concat();
    api_claude(&mut h, lines.clone(), TermStatus::Running);
    rec.play(&mut h, 1.2);
    lines.extend(write);
    api_claude(&mut h, lines.clone(), TermStatus::Running);
    rec.set_files(&mut h, &[("src/middleware/rate_limit.rs", ChangeKind::Untracked, 48, 0, 0), cargo()]);
    rec.play(&mut h, 2.0);
    lines.extend(update);
    api_claude(&mut h, lines.clone(), TermStatus::Running);
    rec.set_files(&mut h, &[("src/routes/auth.rs", ChangeKind::Modified, 2, 1, 0), ("src/middleware/rate_limit.rs", ChangeKind::Untracked, 48, 0, 3), cargo()]);
    rec.play(&mut h, 2.0);

    // It asks before running the tests: the card pulses and a notice says so.
    lines.extend(claude_api_question());
    api_claude(&mut h, lines, TermStatus::WaitingInput);
    rec.play(&mut h, 3.5);
    eprintln!("wrote {} frames to {}", rec.frame, rec.dir.display());
}

/// Keeps the fixtures honest in the normal test run, without rendering.
#[test]
fn claude_boxes_are_aligned() {
    for text in [claude_api(), claude_web(), claude_infra()] {
        for line in text.split("\r\n").filter(|l| l.contains('│')) {
            assert_eq!(visible_len(line), CLAUDE_W, "{line:?}");
        }
    }
}
