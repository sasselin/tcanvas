//! Updates: checks GitHub for a newer release (at startup, then every 6 hours), warns once
//! per version (toast), shows its release notes, and on request downloads it, swaps the
//! binaries in place and restarts. After an update, the first start shows "What's new" from
//! the CHANGELOG embedded in the new build.
//!
//! `TCANVAS_NO_UPDATE_CHECK=1` turns the check off; tests turn it off through `Config`.

pub mod changelog;
pub mod install;
pub mod release;
pub mod version;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use egui::{Id, Modal, RichText, ScrollArea, Sense};
use serde::{Deserialize, Serialize};

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::theme;
use changelog::{Line, Section};
use release::Release;

const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const STATE_FILE: &str = "updates.json";

enum Msg {
    Checked(Vec<Release>),
    Installed(Result<PathBuf, String>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    /// Downloading and swapping this version.
    Installing(String),
    /// Installed: relaunch this exe at the end of the frame.
    Ready(PathBuf),
    Restarting,
    Failed {
        version: String,
        error: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    /// Before updating: the notes of every pending release.
    ReleaseNotes,
    /// The notes of the running version (after an update, or from the version label).
    WhatsNew(Vec<Section>),
}

/// What the app remembers between runs (`updates.json` in the config folder).
#[derive(Serialize, Deserialize, Default)]
struct State {
    /// The version that last ran: a newer one is an update to show "What's new" for.
    last_seen_version: Option<String>,
}

pub struct Updates {
    ctx: egui::Context,
    /// The running build's version (`shell::version::current()`).
    running: String,
    /// Newer releases, newest first (empty: up to date).
    pending: Vec<Release>,
    phase: Phase,
    pub dialog: Option<Dialog>,
    /// The version a toast was already shown for.
    notified: Option<String>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl Updates {
    /// `check`: look for releases on GitHub and clean up after a previous update.
    pub fn new(ctx: egui::Context, config_dir: &Path, running: &str, check: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let dialog = whats_new_since_last_run(config_dir, running).map(Dialog::WhatsNew);
        if check {
            if let Some(dir) = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.to_path_buf())) {
                std::thread::spawn(move || install::cleanup_old(&dir));
            }
            if std::env::var_os("TCANVAS_NO_UPDATE_CHECK").is_none_or(|v| v.is_empty() || v == "0") {
                spawn_checker(ctx.clone(), tx.clone());
            }
        }
        Self { ctx, running: running.to_string(), pending: Vec::new(), phase: Phase::Idle, dialog, notified: None, tx, rx }
    }

    pub fn pending(&self) -> &[Release] {
        &self.pending
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Pretends to run `version` (screenshots: images must not change on every release).
    pub fn pin_running_version(&mut self, version: &str) {
        self.running = version.to_string();
    }

    /// Hands releases over as if GitHub had answered (tests, screenshots).
    pub fn offer(&self, releases: Vec<Release>) {
        let _ = self.tx.send(Msg::Checked(releases));
    }

    pub fn poll(&mut self, events: &mut Vec<Event>) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Checked(list) => {
                    self.pending = release::pending(list, &self.running, cfg!(windows));
                    if let Some(latest) = self.pending.first()
                        && self.notified.as_ref() != Some(&latest.version)
                        && self.phase == Phase::Idle
                    {
                        self.notified = Some(latest.version.clone());
                        events.push(Event::UpdateAvailable(latest.version.clone()));
                    }
                }
                Msg::Installed(Ok(exe)) => self.phase = Phase::Ready(exe),
                Msg::Installed(Err(error)) => {
                    let version = match &self.phase {
                        Phase::Installing(v) => v.clone(),
                        _ => String::new(),
                    };
                    eprintln!("update to {version} failed: {error}");
                    self.phase = Phase::Failed { version, error };
                }
            }
        }
    }

    /// Once the new build is in place: `save` (layout), start it, close this one.
    pub fn restart_when_ready(&mut self, ctx: &egui::Context, save: impl FnOnce()) {
        let Phase::Ready(exe) = &self.phase else { return };
        let exe = exe.clone();
        save();
        match install::relaunch(&exe) {
            Ok(()) => {
                self.phase = Phase::Restarting;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => self.phase = Phase::Failed { version: self.pending.first().map(|r| r.version.clone()).unwrap_or_default(), error: e.to_string() },
        }
    }

    pub fn handle(&mut self, ev: &Event) {
        if matches!(ev, Event::ShowReleaseNotes) && !self.pending.is_empty() {
            self.dialog = Some(Dialog::ReleaseNotes);
        }
    }

    fn start_install(&mut self) {
        let Some(latest) = self.pending.first().cloned() else { return };
        self.phase = Phase::Installing(latest.version.clone());
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Installed(install::install(&latest)));
            ctx.request_repaint();
        });
    }

    /// The version label (`shown`: what the toolbar displays) and, when there is one, the
    /// update button. Right-to-left layout: the label is added first, at the far right.
    pub fn toolbar_ui(&mut self, ui: &mut egui::Ui, shown: &str) {
        let label = ui.add(egui::Label::new(RichText::new(format!("v{shown}")).small().color(theme::TEXT_DIM)).sense(Sense::click())).on_hover_text(t().version_hint);
        if label.clicked() {
            self.dialog = Some(Dialog::WhatsNew(changelog::sections_between(changelog::EMBEDDED, None, &self.running)));
        }
        match self.phase.clone() {
            Phase::Idle => {
                if let Some(latest) = self.pending.first() {
                    let text = RichText::new((t().update_button)(&latest.version)).color(egui::Color32::WHITE);
                    if ui.add(egui::Button::new(text).fill(theme::FOCUS.gamma_multiply(0.7))).on_hover_text(t().update_button_hint).clicked() {
                        self.dialog = Some(Dialog::ReleaseNotes);
                    }
                }
            }
            Phase::Installing(v) => {
                ui.label(RichText::new((t().update_installing)(&v)).color(theme::TEXT_DIM));
                ui.spinner();
            }
            Phase::Ready(_) | Phase::Restarting => {
                ui.label(RichText::new(t().update_restarting).color(theme::TEXT_DIM));
            }
            Phase::Failed { version, error } => {
                let red = theme::status_color(Some(protocol::TermStatus::Exited(1)));
                let resp = ui.add(egui::Label::new(RichText::new(t().update_failed).color(red)).sense(Sense::click())).on_hover_text((t().update_failed_hint)(&error));
                if resp.clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(release::release_page(&version)));
                }
            }
        }
    }

    pub fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.clone() else { return };
        let mut close = false;
        let modal = Modal::new(Id::unique("update-dialog")).show(ctx, |ui| {
            ui.set_width(520.0);
            match &dialog {
                Dialog::ReleaseNotes => {
                    let Some(latest) = self.pending.first() else {
                        close = true;
                        return;
                    };
                    ui.label(RichText::new((t().update_title)(&latest.version)).strong().size(16.0));
                    ui.label(RichText::new((t().update_running)(&version::base(&self.running))).color(theme::TEXT_DIM));
                    ui.add_space(6.0);
                    let sections: Vec<Section> = self.pending.iter().map(|r| Section { version: r.version.clone(), body: r.notes.clone() }).collect();
                    notes_ui(ui, &sections);
                    ui.add_space(6.0);
                    ui.label(RichText::new(t().update_restart_note).small().color(theme::TEXT_DIM));
                    ui.add_space(6.0);
                    let page = latest.page.clone();
                    ui.horizontal(|ui| {
                        let go = egui::Button::new(RichText::new(t().update_now).color(egui::Color32::WHITE)).fill(theme::FOCUS.gamma_multiply(0.7));
                        if ui.add(go).clicked() {
                            self.start_install();
                            close = true;
                        }
                        if ui.button(t().update_later).clicked() {
                            close = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.hyperlink_to(t().release_page, page));
                    });
                }
                Dialog::WhatsNew(sections) => {
                    ui.label(RichText::new((t().whats_new_title)(&version::base(&self.running))).strong().size(16.0));
                    ui.add_space(6.0);
                    if sections.is_empty() {
                        ui.label(RichText::new(t().no_release_notes).color(theme::TEXT_DIM));
                    } else {
                        notes_ui(ui, sections);
                    }
                    ui.add_space(8.0);
                    if ui.button(t().close).clicked() {
                        close = true;
                    }
                }
            }
        });
        if close || modal.should_close() {
            self.dialog = None;
        }
    }
}

fn notes_ui(ui: &mut egui::Ui, sections: &[Section]) {
    ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
        for (k, s) in sections.iter().enumerate() {
            if k > 0 {
                ui.add_space(8.0);
            }
            ui.label(RichText::new(format!("v{}", s.version)).strong().size(15.0).color(theme::FOCUS));
            let lines = changelog::lines(&s.body);
            if lines.is_empty() {
                ui.label(RichText::new(t().no_release_notes).color(theme::TEXT_DIM));
            }
            for line in lines {
                match line {
                    Line::Heading(h) => {
                        ui.add_space(4.0);
                        ui.label(RichText::new(h).strong().color(theme::TEXT));
                    }
                    Line::Bullet(b) => {
                        ui.label(RichText::new(format!("•  {b}")).color(theme::TEXT));
                    }
                    Line::Text(p) => {
                        ui.label(RichText::new(p).color(theme::TEXT));
                    }
                }
            }
        }
    });
}

fn spawn_checker(ctx: egui::Context, tx: Sender<Msg>) {
    std::thread::spawn(move || {
        loop {
            match install::fetch_releases() {
                Ok(list) => {
                    if tx.send(Msg::Checked(list)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
                Err(e) => eprintln!("update check failed: {e}"),
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

/// Records the running version and returns the CHANGELOG sections since the previous run when
/// this is the first start after an update. First run ever: nothing to show.
fn whats_new_since_last_run(config_dir: &Path, running: &str) -> Option<Vec<Section>> {
    let path = config_dir.join(STATE_FILE);
    let mut state: State = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let previous = state.last_seen_version.clone();
    let current = version::base(running);
    if previous.as_deref() != Some(current.as_str()) && version::parse(&current).is_some() {
        state.last_seen_version = Some(current);
        let _ = std::fs::create_dir_all(config_dir);
        if let Err(e) = std::fs::write(&path, serde_json::to_vec_pretty(&state).expect("state serializes")) {
            eprintln!("{}: {e}", path.display());
        }
    }
    let previous = previous?;
    if !version::is_newer(running, &previous) {
        return None;
    }
    let sections = changelog::sections_between(changelog::EMBEDDED, Some(&previous), running);
    (!sections.is_empty()).then_some(sections)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tcanvas-updates-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn release(v: &str) -> Release {
        let file = release::asset_name(cfg!(windows), v);
        Release {
            version: v.into(),
            notes: format!("### Added\n\n- Thing in {v}"),
            page: release::release_page(v),
            assets: [file.as_str(), release::SUMS].iter().map(|n| release::Asset { name: n.to_string(), url: format!("https://x/{n}") }).collect(),
        }
    }

    #[test]
    fn whats_new_shows_once_after_an_update_and_never_on_first_run() {
        let dir = temp("seen");
        assert_eq!(whats_new_since_last_run(&dir, "0.1.0"), None, "first run");
        let after = whats_new_since_last_run(&dir, "0.1.1").expect("updated from 0.1.0");
        assert_eq!(after.iter().map(|s| s.version.as_str()).collect::<Vec<_>>(), ["0.1.1"]);
        assert_eq!(whats_new_since_last_run(&dir, "0.1.1"), None, "only once");
        assert_eq!(whats_new_since_last_run(&dir, "0.1.1+2-gabcdef0"), None, "a dev build of the same release");
        assert_eq!(whats_new_since_last_run(&dir, "0.1.0"), None, "a downgrade");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_update_is_announced_once_per_version() {
        let dir = temp("notify");
        let mut u = Updates::new(egui::Context::default(), &dir, "0.1.0", false);
        let mut events = vec![];
        u.offer(vec![release("0.1.1")]);
        u.poll(&mut events);
        u.offer(vec![release("0.1.1")]);
        u.poll(&mut events);
        assert!(matches!(events.as_slice(), [Event::UpdateAvailable(v)] if v == "0.1.1"));
        u.offer(vec![release("0.1.1"), release("0.2.0")]);
        u.poll(&mut events);
        assert_eq!(events.len(), 2);
        assert_eq!(u.pending().iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), ["0.2.0", "0.1.1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_newer_nothing_shown() {
        let dir = temp("none");
        let mut u = Updates::new(egui::Context::default(), &dir, "0.2.0", false);
        let mut events = vec![];
        u.offer(vec![release("0.1.1")]);
        u.poll(&mut events);
        assert!(events.is_empty() && u.pending().is_empty());
        u.handle(&Event::ShowReleaseNotes);
        assert_eq!(u.dialog, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_install_is_reported_with_its_version() {
        let dir = temp("fail");
        let mut u = Updates::new(egui::Context::default(), &dir, "0.1.0", false);
        u.phase = Phase::Installing("0.1.1".into());
        u.tx.send(Msg::Installed(Err("disk full".into()))).unwrap();
        u.poll(&mut vec![]);
        assert_eq!(u.phase, Phase::Failed { version: "0.1.1".into(), error: "disk full".into() });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
