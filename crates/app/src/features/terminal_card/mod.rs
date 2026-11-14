//! Terminal cards: PTY, rendering, keyboard/mouse, moving, resizing.

mod clipboard;
mod input;
mod paste_image;
mod pty;
mod render;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use alacritty_terminal::event::Event as TermEvent;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::Rgb;
use egui::{Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use protocol::{AgentCmd, AgentEvent, Problem, SESSION_PREFIX};

use crate::shell::event::{Event, Notice, SendOutcome, SendRefusal};
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::widgets::{confirm, text_prompt};
use crate::shell::world::{Card, CardId, CardWorktree, World};
pub use clipboard::{ClipboardSource, Fixed as FixedClipboard, FixedImage as FixedImageClipboard};
use input::{ClipAction, MouseButton};
use pty::{GridSize, TermRuntime};
pub use pty::{Launch, Start, attach_command};

/// Dimensions in world units (1 unit = 1 point at zoom 1).
pub const TITLE_H: f32 = 28.0;
pub const PAD: f32 = 6.0;
/// Background of the branch badge in the title bar.
const BADGE_BG: Color32 = Color32::from_rgb(48, 54, 68);
pub const BASE_FONT: f32 = 14.0;
const GRIP: f32 = 16.0;
/// Below this, the × button is too small to aim at: use the menu or Ctrl+Shift+W.
const CLOSE_MIN_ZOOM: f32 = 0.35;
const MIN_COLS: u16 = 20;
const MIN_ROWS: u16 = 4;
const DEFAULT_COLS: f32 = 120.0;
const DEFAULT_ROWS: f32 = 34.0;
/// Below this font size (pixels), draw a preview instead of the grid.
const LOD_FONT_PX: f32 = 5.5;

/// How long the agent gets to answer a `SendText` (it pastes, waits a moment, presses Enter: a
/// fraction of a second). Past that, the text is reported as maybe not sent, and the keys held
/// for the card go.
const SEND_ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// The agent never pastes a text this long after it got it (answered as expired): shorter than
/// `SEND_ANSWER_TIMEOUT`, so a text the app gave up on can never arrive later.
const SEND_LIMIT: Duration = Duration::from_secs(8);

/// How far a text the agent answered for got.
enum Sent {
    /// Not pasted.
    Not(SendRefusal),
    /// Pasted, but the Enter could not be pressed.
    Pasted(SendRefusal),
}

/// The app's reading of a problem the agent reports for a text.
fn refusal_of(problem: &Problem) -> SendRefusal {
    match problem {
        Problem::SessionNotFound { .. } => SendRefusal::NoSession,
        Problem::PaneDead { .. } => SendRefusal::Ended,
        Problem::PaneBusy { mode, .. } => SendRefusal::Busy(mode.clone()),
        Problem::Expired => SendRefusal::Expired,
        Problem::SendFailed { detail } => SendRefusal::Failed(detail.clone()),
        other => SendRefusal::Failed(t().problem(other)),
    }
}

/// Whether the agent can deliver text (`AgentCmd::SendText`), from its last hello.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentSupport {
    /// Not connected (yet, or any more).
    Down,
    TooOld,
    Ready,
}

/// Text handed to the agent, waiting for its answer.
struct InFlight {
    card: CardId,
    text: String,
    since: Instant,
}

/// Where a new card's command stands.
enum CommandRun {
    /// To be staged on the Linux side before the first attach.
    Pending(Start),
    /// Being staged (background process); the flag, set when the card goes meanwhile, makes the
    /// staging job delete what it staged.
    Staging(Start, Receiver<Result<(), String>>, Arc<AtomicBool>),
    /// Given to the first attach; kept until its shell has taken it (`true`) or it never will
    /// (`false`), as `pty::confirm_command` tells (`None`: cannot tell).
    Launched(Start, Receiver<Option<bool>>),
}

/// After a staging job: when its card went meanwhile, delete what it staged.
fn finish_staging(cancelled: &AtomicBool, discard: impl FnOnce()) {
    if cancelled.load(Ordering::SeqCst) {
        discard();
    }
}

fn discard_in_background(session: String) {
    std::thread::spawn(move || {
        pty::discard_command(&session);
    });
}

/// Launch real terminals, or nothing (UI tests without a PTY).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Launcher {
    Tmux,
    Disabled,
}

pub fn base_cell(ctx: &egui::Context) -> Vec2 {
    let font = FontId::monospace(BASE_FONT);
    ctx.fonts_mut(|f| vec2(f.glyph_width(&font, 'M'), f.row_height(&font)))
}

/// Number of columns and rows that fit in a card (world units).
pub fn grid_for(rect: Rect, cell: Vec2) -> GridSize {
    let cols = ((rect.width() - 2.0 * PAD) / cell.x).floor().max(MIN_COLS as f32) as u16;
    let rows = ((rect.height() - TITLE_H - 2.0 * PAD) / cell.y).floor().max(MIN_ROWS as f32) as u16;
    GridSize { cols, rows }
}

pub fn card_size(cell: Vec2, cols: f32, rows: f32) -> Vec2 {
    vec2(cols * cell.x + 2.0 * PAD, rows * cell.y + TITLE_H + 2.0 * PAD)
}

pub struct TerminalCards {
    ctx: egui::Context,
    launcher: Launcher,
    runtimes: HashMap<CardId, TermRuntime>,
    spawn_errors: HashMap<CardId, String>,
    /// Command a new card runs, until its session is seen: it goes with the first attach only
    /// (which creates the session) and is never saved, so neither a reattach, a respawn nor an
    /// app restart can run it again.
    commands: HashMap<CardId, CommandRun>,
    /// Command of a card whose session never started (it did not run): offered for copying.
    lost_commands: HashMap<CardId, String>,
    /// Text sent by the app that a card did not take, and why: offered for copying.
    unsent: HashMap<CardId, (String, SendRefusal)>,
    agent: AgentSupport,
    /// Text handed to the agent, by request id.
    in_flight: HashMap<u64, InFlight>,
    next_send: u64,
    /// The user's keys for a card while text sent to it is in flight: written once it is done, so
    /// they never land between the paste and its Enter.
    held: HashMap<CardId, Vec<u8>>,
    /// The last outcome of text sent to each card.
    outcomes: HashMap<CardId, SendOutcome>,
    /// Cards whose terminal has shown something since it was attached: their shell is up.
    shown: HashSet<CardId>,
    tx: Sender<(CardId, TermEvent)>,
    rx: Receiver<(CardId, TermEvent)>,
    /// Keyboard captured for the focused card, with the frame's modifiers.
    keys: Vec<(egui::Event, Modifiers)>,
    mods: Modifiers,
    rename: Option<(CardId, String)>,
    /// Close requested, awaiting confirmation.
    confirm_close: Option<CardId>,
    stall: Option<(CardId, String)>,
    /// Sessions killed on purpose: no notification, no re-adoption.
    killed: HashSet<String>,
    viewport: Rect,
    /// Clipboard read for Ctrl+Shift+V and right-click.
    clipboard: Box<dyn ClipboardSource>,
    /// Without a real terminal (tests): what would have been sent to each card.
    written: Vec<(CardId, Vec<u8>)>,
    /// Where pasted images are saved (their path is pasted into the terminal).
    paste_dir: std::path::PathBuf,
}

impl TerminalCards {
    pub fn new(ctx: egui::Context, launcher: Launcher) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            ctx,
            launcher,
            runtimes: HashMap::new(),
            spawn_errors: HashMap::new(),
            commands: HashMap::new(),
            lost_commands: HashMap::new(),
            unsent: HashMap::new(),
            agent: AgentSupport::Down,
            in_flight: HashMap::new(),
            next_send: 0,
            held: HashMap::new(),
            outcomes: HashMap::new(),
            shown: HashSet::new(),
            tx,
            rx,
            keys: Vec::new(),
            mods: Modifiers::NONE,
            rename: None,
            confirm_close: None,
            stall: None,
            killed: HashSet::new(),
            viewport: Rect::NOTHING,
            clipboard: match launcher {
                Launcher::Tmux => Box::new(clipboard::System),
                Launcher::Disabled => Box::new(clipboard::Fixed(None)),
            },
            written: Vec::new(),
            paste_dir: paste_image::default_dir(),
        }
    }

    /// Replaces the clipboard (tests).
    pub fn set_clipboard(&mut self, clipboard: Box<dyn ClipboardSource>) {
        self.clipboard = clipboard;
    }

    /// Replaces the folder for pasted images (tests).
    pub fn set_paste_dir(&mut self, dir: std::path::PathBuf) {
        self.paste_dir = dir;
    }

    /// Paste bytes for the clipboard: its text, or else the path of its image saved as a PNG.
    fn clipboard_paste(&mut self, bracketed: bool) -> Vec<u8> {
        if let Some(text) = self.clipboard.text() {
            return input::paste_bytes(&text, bracketed);
        }
        self.clipboard.image().map(|img| self.image_paste(&img, bracketed)).unwrap_or_default()
    }

    /// Saves an image and returns the paste of its path (nothing if it cannot be saved).
    fn image_paste(&self, image: &egui::ColorImage, bracketed: bool) -> Vec<u8> {
        paste_image::save(image, &self.paste_dir).map(|path| input::paste_bytes(&path, bracketed)).unwrap_or_default()
    }

    /// Shows fixed terminal output in a card, without a PTY (screenshots, UI tests).
    pub fn show_canned(&mut self, world: &World, id: &str, bytes: &[u8]) {
        let Some(card) = world.card(id) else { return };
        let cell = base_cell(&self.ctx);
        let rt = TermRuntime::canned(&card.id, grid_for(card.rect, cell), cell_px(cell), bytes, self.tx.clone(), self.ctx.clone());
        self.runtimes.insert(card.id.clone(), rt);
    }

    /// Bytes sent to a card without a real terminal (tests, `Launcher::Disabled`).
    pub fn written(&self, id: &str) -> Vec<u8> {
        self.written.iter().filter(|(c, _)| c == id).flat_map(|(_, b)| b.clone()).collect()
    }

    /// Sends bytes to a card's terminal (or records them, without a real terminal).
    fn send_to(&mut self, id: &str, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if let Some(rt) = self.runtimes.get(id) {
            rt.scroll_to_bottom();
            rt.write(bytes);
        } else if self.launcher == Launcher::Disabled {
            self.written.push((id.to_string(), bytes));
        }
    }

    /// Terminal mode of a card (default mode without a real terminal).
    fn mode_of(&self, id: &str) -> TermMode {
        self.runtimes.get(id).map(|rt| *rt.term.lock().mode()).unwrap_or_default()
    }

    /// Pastes the system clipboard into the card.
    fn paste_clipboard(&mut self, id: &str) {
        let bracketed = self.mode_of(id).contains(TermMode::BRACKETED_PASTE);
        let bytes = self.clipboard_paste(bracketed);
        self.type_into(id, bytes);
    }

    /// The user's input to a card. While text sent by the app to it is in flight, held until it
    /// is done (the user's keys never land between that paste and its Enter, and the Enter never
    /// submits what the user was typing). Ctrl+C is never held: it goes at once, with what was
    /// held before it.
    fn type_into(&mut self, id: &str, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if !self.in_flight.values().any(|f| f.card == id) {
            return self.send_to(id, bytes);
        }
        let held = self.held.entry(id.to_string()).or_default();
        held.extend(bytes);
        if held.contains(&0x03) {
            let held = self.held.remove(id).unwrap_or_default();
            self.send_to(id, held);
        }
    }

    /// Writes the keys held for a card once nothing sent to it is in flight any more.
    fn release_held(&mut self, id: &str) {
        if !self.in_flight.values().any(|f| f.card == id)
            && let Some(bytes) = self.held.remove(id)
        {
            self.send_to(id, bytes);
        }
    }

    /// Why a card cannot take text now (`None`: its session is up and shows its shell).
    fn refusal(&self, card: &Card) -> Option<SendRefusal> {
        // Up: the agent saw its session, and its terminal has shown something (the shell's
        // prompt) since it was attached: text pasted into a shell still reading its rc files
        // would go plain, line by line. Once shown, a blank screen (`clear`) does not count.
        let shown = self.launcher == Launcher::Disabled || self.shown.contains(&card.id);
        // A dead pane (its program exited, the pane kept by `remain-on-exit`): the agent says so.
        if card.status() == Some(protocol::TermStatus::Dead) {
            Some(SendRefusal::Ended)
        } else if self.spawn_errors.contains_key(&card.id) {
            Some(SendRefusal::NotStarted)
        } else if self.commands.contains_key(&card.id) || card.info.is_none() || !shown {
            // A start command not taken yet: text sent now would land in front of it.
            Some(SendRefusal::Starting)
        } else {
            None
        }
    }

    /// `Event::SendText`: handed to the agent, which has tmux paste it into this card's session
    /// only (then Enter). A card that cannot take it keeps the text, with why, for the user to
    /// copy. `None`: in flight, the outcome comes with the agent's answer.
    fn send_text(&mut self, world: &World, id: &str, text: &str, enter: bool, out: &mut Vec<Event>) -> Option<SendOutcome> {
        let Some(card) = world.card(id) else { return Some(SendOutcome::Refused(SendRefusal::NoCard)) };
        let refused = self.refusal(card).or(match self.agent {
            AgentSupport::Ready => None,
            AgentSupport::Down => Some(SendRefusal::NoAgent),
            AgentSupport::TooOld => Some(SendRefusal::AgentTooOld),
        });
        if let Some(why) = refused {
            return Some(self.refuse(id, text.to_string(), why, out));
        }
        let Some(message) = input::message_text(text, enter) else { return Some(SendOutcome::Empty) };
        let request = self.next_send;
        self.next_send += 1;
        self.outcomes.remove(id);
        self.in_flight.insert(request, InFlight { card: id.to_string(), text: text.to_string(), since: Instant::now() });
        let limit_ms = Some(SEND_LIMIT.as_millis() as u64);
        out.push(Event::ToAgent(AgentCmd::SendText { id: request, session: card.session(), text: message, enter, limit_ms }));
        self.ctx.request_repaint_after(Duration::from_secs(1));
        None
    }

    /// Keeps the text on the card, says why with a notice naming it; the outcome for the sender.
    fn refuse(&mut self, id: &str, text: String, why: SendRefusal, out: &mut Vec<Event>) -> SendOutcome {
        self.unsent.insert(id.to_string(), (text, why.clone()));
        out.push(Event::Notify(id.to_string(), Notice::NotSent(why.clone())));
        SendOutcome::Refused(why)
    }

    /// The end of a text in flight: its outcome, then the keys held for its card.
    fn settle_send(&mut self, request: u64, result: Result<(), Sent>, out: &mut Vec<Event>) {
        let Some(f) = self.in_flight.remove(&request) else { return };
        let outcome = match result {
            Ok(()) => {
                // A refusal kept from before is stale now.
                self.unsent.remove(&f.card);
                SendOutcome::Sent
            }
            // The text is in the input line: told, but not offered again (a retry would type it
            // twice).
            Err(Sent::Pasted(why)) => {
                self.unsent.remove(&f.card);
                out.push(Event::Notify(f.card.clone(), Notice::NotSubmitted(why.clone())));
                SendOutcome::PastedNotSubmitted(why)
            }
            Err(Sent::Not(why)) => self.refuse(&f.card, f.text, why, out),
        };
        self.outcomes.insert(f.card.clone(), outcome.clone());
        out.push(Event::TextSendOutcome { card: f.card.clone(), outcome });
        self.release_held(&f.card);
    }

    /// Cards whose terminal shows something for the first time since it was attached.
    fn note_shown(&mut self) {
        let fresh: Vec<CardId> = self.runtimes.iter().filter(|(id, rt)| !self.shown.contains(*id) && !rt.screen_text().trim().is_empty()).map(|(id, _)| id.clone()).collect();
        self.shown.extend(fresh);
    }

    /// Settles the texts in flight: all of them as `gone` (the agent went), or else those past
    /// their time, as maybe not sent.
    fn settle_all(&mut self, gone: Option<SendRefusal>, out: &mut Vec<Event>) {
        let now = Instant::now();
        let mut due: Vec<u64> = self.in_flight.iter().filter(|(_, f)| gone.is_some() || now.duration_since(f.since) >= SEND_ANSWER_TIMEOUT).map(|(id, _)| *id).collect();
        due.sort_unstable();
        for request in due {
            self.settle_send(request, Err(Sent::Not(gone.clone().unwrap_or(SendRefusal::NoAnswer))), out);
        }
        if !self.in_flight.is_empty() {
            self.ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    /// Text sent by the app that this card did not take, and why.
    pub fn unsent_text(&self, id: &str) -> Option<(&str, &SendRefusal)> {
        self.unsent.get(id).map(|(text, why)| (text.as_str(), why))
    }

    /// The last outcome of text sent to this card (none while in flight).
    pub fn last_outcome(&self, id: &str) -> Option<&SendOutcome> {
        self.outcomes.get(id)
    }

    /// Whether the card's terminal application asked for bracketed paste (tests).
    pub fn bracketed_paste(&self, id: &str) -> bool {
        self.runtimes.contains_key(id) && self.mode_of(id).contains(TermMode::BRACKETED_PASTE)
    }

    pub fn has_dialog(&self) -> bool {
        self.rename.is_some() || self.stall.is_some() || self.confirm_close.is_some()
    }

    /// Removes everything keyboard-related from `events` and sends it to the focused terminal.
    /// App shortcuts must have been removed beforehand. Modifiers
    /// are tracked in event order (text events carry none).
    pub fn capture_keyboard(&mut self, events: &mut Vec<egui::Event>) {
        events.retain(|ev| {
            match ev {
                egui::Event::ModifiersChanged(m) => self.mods = *m,
                egui::Event::Key { modifiers, .. } => self.mods = *modifiers,
                _ => {}
            }
            let take = matches!(ev, egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::PasteImage(_) | egui::Event::Copy | egui::Event::Cut | egui::Event::Key { .. });
            if take {
                self.keys.push((ev.clone(), self.mods));
            }
            !take
        });
    }

    /// Visible text of a card (tests).
    pub fn screen_text(&self, id: &str) -> Option<String> {
        self.runtimes.get(id).map(|rt| rt.screen_text())
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.runtimes.contains_key(id)
    }

    /// Command a card will run when its terminal starts (tests: nothing starts without a launcher).
    pub fn pending_command(&self, id: &str) -> Option<&str> {
        self.pending_start(id)?.command.as_deref()
    }

    /// Setup a card will run before its command when its terminal starts.
    pub fn pending_setup(&self, id: &str) -> Option<&str> {
        self.pending_start(id)?.setup.as_deref()
    }

    fn pending_start(&self, id: &str) -> Option<&Start> {
        match self.commands.get(id)? {
            CommandRun::Pending(start) | CommandRun::Staging(start, ..) => Some(start),
            CommandRun::Launched(..) => None,
        }
    }

    /// The card goes: a command not yet taken by its shell is deleted on the Linux side (a staging
    /// still running deletes what it stages). Returns whether a staged file may exist.
    fn forget_command(&mut self, id: &str, session: &str) -> bool {
        let staged = match self.commands.remove(id) {
            None | Some(CommandRun::Pending(_)) => false,
            Some(CommandRun::Staging(_, _, cancelled)) => {
                cancelled.store(true, Ordering::SeqCst);
                true
            }
            Some(CommandRun::Launched(..)) => true,
        };
        if staged && self.launcher == Launcher::Tmux {
            discard_in_background(session.to_string());
        }
        staged
    }

    /// The app quits: commands staged but never attached would never run, delete them now (those
    /// already attached are settled by their confirmation process, which outlives the app).
    pub fn discard_unattached(&mut self, world: &World) {
        let staging: Vec<CardId> = self.commands.iter().filter(|(_, run)| matches!(run, CommandRun::Staging(..))).map(|(id, _)| id.clone()).collect();
        for id in staging {
            if let Some(CommandRun::Staging(_, _, cancelled)) = self.commands.remove(&id) {
                cancelled.store(true, Ordering::SeqCst);
                if let Some(card) = world.card(&id).filter(|_| self.launcher == Launcher::Tmux) {
                    pty::discard_command(&card.session());
                }
            }
        }
    }

    /// The card goes: the texts kept for it (not run, not sent, not yet written) go too.
    fn forget_texts(&mut self, id: &str, out: &mut Vec<Event>) {
        self.lost_commands.remove(id);
        self.unsent.remove(id);
        self.held.remove(id);
        self.outcomes.remove(id);
        self.shown.remove(id);
        // Its sender is told now; the agent's late answer is ignored.
        let gone: Vec<u64> = self.in_flight.iter().filter(|(_, f)| f.card == id).map(|(r, _)| *r).collect();
        for request in gone {
            self.in_flight.remove(&request);
            out.push(Event::TextSendOutcome { card: id.to_string(), outcome: SendOutcome::Refused(SendRefusal::NoCard) });
        }
    }

    /// A card's command went to tmux, and its session has not shown up yet.
    pub fn awaiting_session(&self, id: &str) -> bool {
        matches!(self.commands.get(id), Some(CommandRun::Launched(..)))
    }

    /// Command of a card that did not run because its session never started.
    pub fn lost_command(&self, id: &str) -> Option<&str> {
        self.lost_commands.get(id).map(String::as_str)
    }

    /// Settles launched commands: taken by their shell, or reported as not run (with their text).
    fn confirm_launches(&mut self) {
        let lost = &mut self.lost_commands;
        self.commands.retain(|id, run| {
            let CommandRun::Launched(start, rx) = run else { return true };
            match rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => true,
                Ok(Some(false)) => {
                    lost.insert(id.clone(), start.text());
                    false
                }
                _ => false,
            }
        });
    }

    /// PTY events: replies to the terminal, clipboard, process exit.
    pub fn poll(&mut self, world: &mut World, events: &mut Vec<Event>) {
        self.confirm_launches();
        self.settle_all(None, events);
        self.note_shown();
        while let Ok((id, ev)) = self.rx.try_recv() {
            let Some(rt) = self.runtimes.get(&id) else { continue };
            match ev {
                TermEvent::PtyWrite(text) => rt.write(text.into_bytes()),
                TermEvent::ClipboardStore(_, text) => {
                    self.ctx.copy_text(text);
                    events.push(Event::Notify(id.clone(), Notice::Copied));
                }
                TermEvent::ColorRequest(index, format) => {
                    let c = match index {
                        0..=255 => render::indexed(index as u8),
                        257 => theme::CARD_BG,
                        _ => theme::TEXT,
                    };
                    rt.write(format(Rgb { r: c.r(), g: c.g(), b: c.b() }).into_bytes());
                }
                TermEvent::TextAreaSizeRequest(format) => rt.write(format(rt.window_size()).into_bytes()),
                TermEvent::ChildExit(_) | TermEvent::Exit => {
                    self.runtimes.remove(&id);
                    self.shown.remove(&id);
                    if let Some(card) = world.card_mut(&id) {
                        card.pty_dead = true;
                        if !self.killed.contains(&card.session()) {
                            events.push(Event::PtyExited(id));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Sends the captured keyboard input to the focused card.
    fn write_keys(&mut self, focused: Option<&str>) {
        let keys = std::mem::take(&mut self.keys);
        let Some(id) = focused else { return };
        if !self.runtimes.contains_key(id) && self.launcher != Launcher::Disabled {
            return;
        }
        let mode = self.mode_of(id);
        let app_cursor = mode.contains(TermMode::APP_CURSOR);
        let mut out = Vec::new();
        for (ev, mods) in keys {
            match ev {
                egui::Event::Text(t) => out.extend(input::text_bytes(&t, mods)),
                egui::Event::Paste(t) => out.extend(input::paste_bytes(&t, mode.contains(TermMode::BRACKETED_PASTE))),
                egui::Event::PasteImage(img) => out.extend(self.image_paste(&img, mode.contains(TermMode::BRACKETED_PASTE))),
                egui::Event::Copy => out.push(0x03),
                egui::Event::Cut => out.push(0x18),
                egui::Event::Key { key, pressed: true, modifiers, .. } => match input::clip_action(key, modifiers) {
                    Some(ClipAction::Paste) => out.extend(self.clipboard_paste(mode.contains(TermMode::BRACKETED_PASTE))),
                    Some(ClipAction::Copy) => {}
                    None => out.extend(input::key_bytes(key, modifiers, app_cursor).unwrap_or_default()),
                },
                _ => {}
            }
        }
        self.type_into(id, out);
    }

    fn ensure_runtimes(&mut self, world: &World, cell: Vec2) {
        if self.launcher == Launcher::Disabled {
            return;
        }
        for card in world.cards.iter().filter(|c| !c.pty_dead) {
            if self.runtimes.contains_key(&card.id) || self.spawn_errors.contains_key(&card.id) {
                continue;
            }
            let size = grid_for(card.rect, cell);
            // A card with a command attaches once it is staged on the Linux side.
            let staged = match self.commands.remove(&card.id) {
                None => None,
                Some(CommandRun::Pending(text)) => {
                    let (tx, rx) = mpsc::channel();
                    let cancelled = Arc::new(AtomicBool::new(false));
                    let (session, staged, ctx, flag) = (card.session(), text.clone(), self.ctx.clone(), cancelled.clone());
                    std::thread::spawn(move || {
                        let result = pty::stage_command(&session, &staged).map_err(|e| e.to_string());
                        finish_staging(&flag, || {
                            pty::discard_command(&session);
                        });
                        let _ = tx.send(result);
                        ctx.request_repaint();
                    });
                    self.commands.insert(card.id.clone(), CommandRun::Staging(text, rx, cancelled));
                    continue;
                }
                Some(CommandRun::Staging(text, rx, cancelled)) => match rx.try_recv() {
                    Ok(Ok(())) => Some(text),
                    Err(mpsc::TryRecvError::Empty) => {
                        self.commands.insert(card.id.clone(), CommandRun::Staging(text, rx, cancelled));
                        continue;
                    }
                    failed => {
                        let e = match failed {
                            Ok(Err(e)) => e,
                            _ => "?".into(),
                        };
                        self.spawn_errors.insert(card.id.clone(), (t().command_not_staged)(&e));
                        self.lost_commands.insert(card.id.clone(), text.text());
                        discard_in_background(card.session());
                        continue;
                    }
                },
                // Already attached once with it: a respawn attaches plainly.
                Some(CommandRun::Launched(..)) => None,
            };
            let launch = attach_command(&card.session(), card.cwd.as_deref(), staged.as_ref().map(|_| size));
            match TermRuntime::spawn(&card.id, &launch, size, cell_px(cell), self.tx.clone(), self.ctx.clone()) {
                Ok(rt) => {
                    // Handed to tmux: from now on, attaching never passes it again. Kept until
                    // the session shows up, to tell the user if it never does.
                    if let Some(text) = staged {
                        let (tx, rx) = mpsc::channel();
                        let (session, ctx) = (card.session(), self.ctx.clone());
                        std::thread::spawn(move || {
                            let _ = tx.send(pty::confirm_command(&session));
                            ctx.request_repaint();
                        });
                        self.commands.insert(card.id.clone(), CommandRun::Launched(text, rx));
                    }
                    self.runtimes.insert(card.id.clone(), rt);
                }
                Err(e) => {
                    self.spawn_errors.insert(card.id.clone(), (t().spawn_failed)(&launch.program, &e.to_string()));
                    if let Some(text) = staged {
                        self.lost_commands.insert(card.id.clone(), text.text());
                        discard_in_background(card.session());
                    }
                }
            }
        }
    }

    /// Draws all cards. Returns `true` if the wheel was used by a terminal.
    pub fn ui(&mut self, ui: &mut Ui, world: &mut World, dimmed: &dyn Fn(&World, &Card) -> bool, events: &mut Vec<Event>) -> bool {
        let vp = ui.max_rect();
        self.viewport = vp;
        let cell = base_cell(ui.ctx());
        self.ensure_runtimes(world, cell);
        let focused = world.focused.clone();
        self.write_keys(focused.as_deref());

        let mut wheel_taken = false;
        for i in world.cards_by_z() {
            if !world.shows_card(&world.cards[i]) {
                continue;
            }
            let dim = dimmed(world, &world.cards[i]);
            wheel_taken |= self.card_ui(ui, world, i, cell, dim, events);
        }
        // Drag interrupted (card closed, hidden…): don't leave a frame frozen.
        if world.dragging.is_some() && !ui.input(|i| i.pointer.primary_down()) {
            world.dragging = None;
        }
        wheel_taken
    }

    fn card_ui(&mut self, ui: &mut Ui, world: &mut World, i: usize, cell: Vec2, dim: bool, events: &mut Vec<Event>) -> bool {
        let vp = self.viewport;
        let cam = world.camera;
        let zoom = cam.zoom;
        let id = world.cards[i].id.clone();
        let grid = grid_for(world.cards[i].rect, cell);
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.resize(grid, cell_px(cell));
        }

        let screen = cam.rect_to_screen(world.cards[i].rect, vp);
        if !vp.intersects(screen) {
            return false;
        }
        let focused = world.focused.as_deref() == Some(id.as_str());
        let title_rect = Rect::from_min_size(screen.min, vec2(screen.width(), TITLE_H * zoom));
        let body = Rect::from_min_max(pos2(screen.min.x, title_rect.max.y), screen.max);
        let grip_size = (GRIP * zoom).max(8.0);
        let grip = Rect::from_min_size(screen.max - Vec2::splat(grip_size), Vec2::splat(grip_size));

        let eid = Id::unique(("card", &id));
        let title_r = ui.interact(title_rect, eid.with("title"), Sense::click_and_drag());
        // Close button, above the title bar (registered after: it wins the click).
        let close_rect = Rect::from_min_size(pos2(title_rect.max.x - title_rect.height(), title_rect.min.y), Vec2::splat(title_rect.height()));
        let close_r = (zoom >= CLOSE_MIN_ZOOM).then(|| {
            let r = ui.interact(close_rect, eid.with("close"), Sense::click()).on_hover_text(t().close_terminal_hint);
            let a11y = (t().close_terminal_a11y)(&world.cards[i].title());
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &a11y));
            r
        });
        if close_r.as_ref().is_some_and(|r| r.clicked()) {
            events.push(Event::RequestClose(id.clone()));
        }
        let body_r = ui.interact(body, eid.with("body"), Sense::click_and_drag());
        let grip_r = ui.interact(grip, eid.with("grip"), Sense::drag());

        // --- Interactions -------------------------------------------------------------
        if title_r.dragged_by(PointerButton::Primary) {
            world.dragging = Some(id.clone());
            world.cards[i].rect = world.cards[i].rect.translate(title_r.drag_delta() / zoom);
            world.raise(&id);
            world.touch();
        }
        if title_r.drag_stopped() && world.dragging.as_ref() == Some(&id) {
            // Dropped in a visible frame: it becomes a member; elsewhere: outside any group.
            world.dragging = None;
            let target = world.drop_target(world.cards[i].rect.center());
            if target != world.cards[i].project {
                events.push(Event::AssignCard { card: id.clone(), project: target, relocate: false });
            }
            events.push(Event::SettleCard(id.clone()));
        }
        if title_r.double_clicked() {
            events.push(Event::FlyTo(id.clone()));
        } else if title_r.clicked() {
            events.push(Event::FocusCard(id.clone()));
        }
        title_r.context_menu(|ui| self.context_menu(ui, world, i, events));

        if grip_r.hovered() || grip_r.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeNwSe);
        }
        if grip_r.drag_stopped() {
            events.push(Event::SettleCard(id.clone()));
        }
        if grip_r.dragged() {
            let min = card_size(cell, MIN_COLS as f32, MIN_ROWS as f32);
            let r = &mut world.cards[i].rect;
            r.max += grip_r.drag_delta() / zoom;
            r.max = r.max.max(r.min + min);
            world.touch();
        }

        let alt = ui.input(|inp| inp.modifiers.alt);
        // Right-click = paste, as in Windows Terminal (Alt+right-click: tmux menu).
        let paste_click = body_r.secondary_clicked() && !alt && !is_preview(zoom);
        if body_r.clicked() && is_preview(zoom) {
            // A preview isn't usable: zoom in on it so the user can type.
            events.push(Event::FlyTo(id.clone()));
        } else if !focused && (body_r.clicked() || body_r.drag_started() || paste_click) {
            events.push(Event::FocusCard(id.clone()));
        }
        if paste_click {
            self.paste_clipboard(&id);
        }

        // --- Drawing ------------------------------------------------------------------
        let painter = ui.painter_at(vp);
        let radius = CornerRadius::same((6.0 * zoom).clamp(1.0, 8.0) as u8);
        painter.rect_filled(screen.translate(vec2(0.0, 4.0 * zoom)).expand(2.0 * zoom), radius, Color32::from_black_alpha(90));
        painter.rect_filled(screen, radius, theme::CARD_BG);
        let top_radius = CornerRadius { sw: 0, se: 0, ..radius };
        painter.rect_filled(title_rect, top_radius, theme::TITLE_BG);

        let card = &world.cards[i];
        let status = card.status();
        let title_font = FontId::proportional(13.0 * zoom);
        let title_painter = painter.with_clip_rect(title_rect.intersect(vp));
        let dot = pos2(title_rect.min.x + 14.0 * zoom, title_rect.center().y);
        title_painter.circle_filled(dot, 5.0 * zoom, theme::status_color(status));
        let label = match status {
            Some(s) => format!("{}  ·  {}", card.title(), t().status(s)),
            None => card.title(),
        };
        let label_rect = title_painter.text(pos2(dot.x + 12.0 * zoom, dot.y), Align2::LEFT_CENTER, label, title_font.clone(), theme::TEXT);
        let mut right = title_rect.max.x - 10.0 * zoom;
        if let Some(close_r) = &close_r {
            let hot = close_r.hovered();
            if hot {
                let r = CornerRadius { sw: 0, se: 0, nw: 0, ..top_radius };
                title_painter.rect_filled(close_rect, r, Color32::from_rgb(190, 60, 60));
            }
            let c = close_rect.center();
            let arm = close_rect.height() * 0.18;
            let stroke = Stroke::new((1.5 * zoom).max(1.0), if hot { Color32::WHITE } else { theme::TEXT_DIM });
            title_painter.line_segment([c - Vec2::splat(arm), c + Vec2::splat(arm)], stroke);
            title_painter.line_segment([c + vec2(-arm, arm), c + vec2(arm, -arm)], stroke);
            right = close_rect.min.x - 6.0 * zoom;
        }
        let prompt = header_prompt(card);
        let cwd = card.current_cwd().map(|cwd| title_painter.layout_no_wrap(cwd.to_string(), FontId::proportional(11.0 * zoom), theme::TEXT_DIM));
        // Branch badge: the git worktree the card's folder is in.
        let worktree = world.card_worktrees.get(&id);
        let badge_pad = 6.0 * zoom;
        let badge = worktree.map(|w| (badge_label(w), w)).map(|(label, w)| (title_painter.layout_no_wrap(format!("⎇ {label}"), FontId::proportional(11.0 * zoom), theme::TEXT), label, w));
        let layout = header_layout(label_rect.max.x, right, cwd.as_ref().map(|g| g.size().x), badge.as_ref().map(|(g, ..)| g.size().x + 2.0 * badge_pad), prompt.is_some(), zoom);
        if let Some(cwd) = cwd.filter(|_| layout.cwd) {
            title_painter.galley(pos2(right - cwd.size().x, dot.y - cwd.size().y / 2.0), cwd, theme::TEXT_DIM);
        }
        if let Some((galley, label, w)) = badge
            && let Some(x) = layout.badge
        {
            let size = galley.size();
            let rect = Rect::from_min_size(pos2(x, dot.y - size.y / 2.0 - 2.0 * zoom), vec2(size.x + 2.0 * badge_pad, size.y + 4.0 * zoom));
            title_painter.rect_filled(rect, CornerRadius::same((rect.height() / 2.0).min(255.0) as u8), BADGE_BG);
            title_painter.galley(pos2(x + badge_pad, dot.y - size.y / 2.0), galley, theme::TEXT);
            // Hover only, like the prompt: the title bar underneath keeps clicks and drags.
            let full = format!("⎇ {}\n{}", w.branch.as_deref().unwrap_or(t().detached_head), w.path);
            let r = ui.interact(rect, eid.with("badge"), Sense::hover()).on_hover_text(full);
            let a11y = (t().branch_badge_a11y)(&label);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &a11y));
        }
        // The coding agent's last prompt, elided between the title and the folder.
        if let Some(prompt) = prompt
            && let Some((x0, x1)) = layout.prompt
        {
            let mut job = egui::text::LayoutJob::single_section((t().last_prompt)(prompt), egui::TextFormat::simple(FontId::proportional(12.0 * zoom), theme::TEXT_DIM));
            job.wrap = egui::text::TextWrapping::truncate_at_width(x1 - x0);
            let galley = title_painter.layout_job(job);
            let rect = Rect::from_min_size(pos2(x0, dot.y - galley.size().y / 2.0), galley.size());
            title_painter.galley(rect.min, galley, theme::TEXT_DIM);
            // Hover only: clicks and drags still go to the title bar underneath.
            let r = ui.interact(rect, eid.with("prompt"), Sense::hover()).on_hover_text(prompt);
            let a11y = (t().last_prompt_a11y)(prompt);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &a11y));
        }

        let font_px = quantize(BASE_FONT * zoom);
        let body_inner = body.shrink(PAD * zoom);
        let mut wheel_taken = false;
        if card.pty_dead || self.spawn_errors.contains_key(&id) {
            self.dead_overlay(ui, body, &id, zoom, events);
        } else if is_preview(zoom) || self.launcher == Launcher::Disabled && !self.runtimes.contains_key(&id) {
            paint_preview(&painter, card, body, zoom);
        } else if let Some(rt) = self.runtimes.get(&id) {
            let font = FontId::monospace(font_px);
            let cell_screen = ui.ctx().fonts_mut(|f| vec2(f.glyph_width(&font, 'M'), f.row_height(&font)));
            let metrics = render::GridMetrics { origin: body_inner.min, cell: cell_screen, font, clip: body_inner.intersect(vp) };
            render::paint_grid(&painter, &rt.term.lock(), &metrics, focused);
            // The wheel scrolls the hovered terminal, even without keyboard focus; a click goes to the
            // focused terminal, or to the one it targets (the first drag already selects).
            self.forward_mouse(ui, &id, &body_r, body_inner.min, cell_screen, focused);
        }
        if !is_preview(zoom) {
            let title = world.cards[i].title();
            self.kept_text_banners(ui, body, &id, &title);
        }
        // A readable terminal under the pointer keeps the whole wheel, including the tail of
        // smoothed scrolling in later frames: the canvas doesn't move behind it.
        if !is_preview(zoom) && body_r.contains_pointer() {
            wheel_taken = true;
        }

        // Border: focus, or a status calling for attention (pulsing).
        let card = &world.cards[i];
        if card.attention {
            let t = ui.input(|inp| inp.time) as f32;
            let alpha = 0.55 + 0.45 * (t * 4.0).sin().abs();
            painter.rect_stroke(screen, radius, Stroke::new(3.0, theme::status_color(status).gamma_multiply(alpha)), StrokeKind::Outside);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        } else if focused {
            painter.rect_stroke(screen, radius, Stroke::new(2.0, theme::FOCUS), StrokeKind::Outside);
        } else {
            painter.rect_stroke(screen, radius, Stroke::new(1.0, theme::CARD_BORDER), StrokeKind::Inside);
        }
        let grip_color = if grip_r.hovered() { theme::TEXT } else { theme::TEXT_DIM };
        for k in 1..=3 {
            let o = k as f32 * grip_size / 4.0;
            painter.line_segment([pos2(screen.max.x - o, screen.max.y - 2.0), pos2(screen.max.x - 2.0, screen.max.y - o)], Stroke::new(1.0, grip_color));
        }
        if dim {
            painter.rect_filled(screen.expand(3.0), radius, theme::CANVAS_BG.gamma_multiply(0.8));
        }
        wheel_taken
    }

    fn context_menu(&mut self, ui: &mut Ui, world: &World, i: usize, events: &mut Vec<Event>) {
        let card = &world.cards[i];
        if ui.button(t().rename_menu).clicked() {
            self.rename = Some((card.id.clone(), card.name.clone().unwrap_or_default()));
            ui.close();
        }
        if ui.button(t().stall_delay_menu).clicked() {
            self.stall = Some((card.id.clone(), card.stall_secs.to_string()));
            ui.close();
        }
        if ui.button(t().center_and_zoom).clicked() {
            events.push(Event::FlyTo(card.id.clone()));
            ui.close();
        }
        ui.menu_button(t().send_message_menu, |ui| quick_messages_menu(ui, world, card, events));
        ui.menu_button(t().move_to, |ui| {
            for p in &world.projects {
                let here = card.project.as_ref() == Some(&p.id);
                let text = egui::RichText::new(&p.name).color(theme::rgb(p.color));
                if ui.add_enabled(!here, egui::Button::new(text)).clicked() {
                    events.push(Event::AssignCard { card: card.id.clone(), project: Some(p.id.clone()), relocate: true });
                    ui.close();
                }
            }
            if !world.projects.is_empty() {
                ui.separator();
            }
            if ui.add_enabled(card.project.is_some(), egui::Button::new(t().no_group)).clicked() {
                events.push(Event::AssignCard { card: card.id.clone(), project: None, relocate: true });
                ui.close();
            }
        });
        ui.separator();
        if ui.button(t().close_terminal_menu).clicked() {
            events.push(Event::RequestClose(card.id.clone()));
            ui.close();
        }
    }

    fn dead_overlay(&mut self, ui: &mut Ui, body: Rect, id: &CardId, zoom: f32, events: &mut Vec<Event>) {
        let painter = ui.painter_at(body);
        let msg = self.spawn_errors.get(id).cloned().unwrap_or_else(|| t().session_ended.into());
        let font = FontId::proportional((14.0 * zoom).max(6.0));
        painter.text(body.center() - vec2(0.0, 24.0 * zoom), Align2::CENTER_CENTER, msg, font, theme::TEXT_DIM);
        if zoom < 0.3 {
            return;
        }
        let size = vec2(110.0, 26.0);
        let left = Rect::from_center_size(body.center() + vec2(-60.0, 12.0 * zoom), size);
        let right = Rect::from_center_size(body.center() + vec2(60.0, 12.0 * zoom), size);
        if ui.put(left, egui::Button::new(t().respawn)).clicked() {
            events.push(Event::RespawnCard(id.clone()));
        }
        if ui.put(right, egui::Button::new(t().remove)).clicked() {
            events.push(Event::RemoveCard(id.clone()));
        }
    }

    /// Text the card did not take (its command never ran, text sent to it was refused): strips at
    /// the bottom of the card say so and keep the text at hand (a long prompt), until dismissed.
    fn kept_text_banners(&mut self, ui: &mut Ui, body: Rect, id: &CardId, title: &str) {
        let mut bottom = body.max.y;
        if let Some(text) = self.lost_commands.get(id).cloned() {
            if kept_text_banner(ui, Rect::from_min_max(pos2(body.min.x, bottom - BANNER_H), pos2(body.max.x, bottom)).intersect(body), t().command_not_run, t().copy_command, &text, None) {
                self.lost_commands.remove(id);
            }
            bottom -= BANNER_H;
        }
        if let Some((text, why)) = self.unsent.get(id).cloned() {
            let message = (t().text_not_sent)(title, &t().send_refusal(&why));
            if kept_text_banner(ui, Rect::from_min_max(pos2(body.min.x, bottom - BANNER_H), pos2(body.max.x, bottom)).intersect(body), &message, t().copy_text, &text, Some(t().dismiss_text_a11y)) {
                self.unsent.remove(id);
            }
        }
    }

    /// Mouse → terminal (xterm reports if the application requests them, otherwise scrolling).
    fn forward_mouse(&mut self, ui: &Ui, id: &str, body: &egui::Response, origin: Pos2, cell: Vec2, focused: bool) {
        let Some(rt) = self.runtimes.get_mut(id) else { return };
        let mode = *rt.term.lock().mode();
        let mouse_mode = mode.intersects(TermMode::MOUSE_MODE);
        let sgr = mode.contains(TermMode::SGR_MOUSE);
        let size = rt.size();
        let cell_at = |p: Pos2| {
            let v = (p - origin) / cell;
            ((v.x.max(0.0) as usize).min(size.cols as usize - 1), (v.y.max(0.0) as usize).min(size.rows as usize - 1))
        };
        let mut out = Vec::new();
        for ev in ui.input(|i| i.events.clone()) {
            match ev {
                egui::Event::PointerButton { pos, button, pressed, modifiers, .. } if mouse_mode => {
                    let b = match button {
                        PointerButton::Primary => MouseButton::Left,
                        PointerButton::Middle => MouseButton::Middle,
                        PointerButton::Secondary => MouseButton::Right,
                        _ => continue,
                    };
                    // Plain right-click = paste (Alt + right-click goes to tmux: its menu). The
                    // release always follows the forwarded press.
                    let ours = b == MouseButton::Right && !modifiers.alt;
                    if pressed && ours {
                        continue;
                    }
                    // Shift + click belongs to the hand tool (panning the view), not to the terminal.
                    let aimed = focused || body.hovered();
                    if pressed && aimed && body.rect.contains(pos) && !modifiers.shift {
                        let (col, line) = cell_at(pos);
                        out.extend(input::mouse_report(b, modifiers, col, line, true, false, sgr));
                        rt.mouse_down = Some((b, col, line));
                    } else if !pressed && rt.mouse_down.is_some_and(|(d, ..)| d == b) {
                        let (col, line) = cell_at(pos);
                        out.extend(input::mouse_report(b, modifiers, col, line, false, false, sgr));
                        rt.mouse_down = None;
                    }
                }
                egui::Event::PointerMoved(pos) if mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) => {
                    if let Some((b, c0, l0)) = rt.mouse_down {
                        let (col, line) = cell_at(pos);
                        if (col, line) != (c0, l0) {
                            out.extend(input::mouse_report(b, Modifiers::NONE, col, line, true, true, sgr));
                            rt.mouse_down = Some((b, col, line));
                        }
                    }
                }
                egui::Event::MouseWheel { unit, delta, modifiers, .. } if body.hovered() && !modifiers.ctrl => {
                    let lines = match unit {
                        MouseWheelUnit::Line => delta.y,
                        MouseWheelUnit::Point => delta.y / cell.y,
                        MouseWheelUnit::Page => delta.y * size.rows as f32,
                    };
                    rt.wheel_acc += lines;
                    let n = rt.wheel_acc.trunc() as i32;
                    rt.wheel_acc -= n as f32;
                    if n == 0 {
                        continue;
                    }
                    let (col, line) = ui.input(|i| i.pointer.hover_pos()).map(cell_at).unwrap_or((0, 0));
                    for _ in 0..n.unsigned_abs() {
                        if mouse_mode {
                            let b = if n > 0 { MouseButton::WheelUp } else { MouseButton::WheelDown };
                            out.extend(input::mouse_report(b, modifiers, col, line, true, false, sgr));
                        } else if mode.contains(TermMode::ALT_SCREEN) {
                            let key = if n > 0 { Key::ArrowUp } else { Key::ArrowDown };
                            out.extend(input::key_bytes(key, Modifiers::NONE, mode.contains(TermMode::APP_CURSOR)).unwrap_or_default());
                        }
                    }
                    if !mouse_mode && !mode.contains(TermMode::ALT_SCREEN) {
                        rt.scroll_display(n);
                    }
                }
                _ => {}
            }
        }
        rt.write(out);
    }

    pub fn dialogs(&mut self, ctx: &egui::Context, world: &World, events: &mut Vec<Event>) {
        if let Some(id) = self.confirm_close.clone() {
            match world.card(&id) {
                None => self.confirm_close = None,
                Some(card) => {
                    let (title, body) = close_message(card);
                    match confirm(ctx, "close-card", &title, &body, t().close) {
                        Some(true) => {
                            events.push(Event::KillCard(id));
                            self.confirm_close = None;
                        }
                        Some(false) => self.confirm_close = None,
                        None => {}
                    }
                }
            }
        }
        if let Some((id, text)) = &mut self.rename {
            match text_prompt(ctx, "rename-card", t().terminal_name_prompt, text) {
                Some(true) => {
                    let name = Some(text.trim().to_string()).filter(|s| !s.is_empty());
                    events.push(Event::RenameCard(id.clone(), name));
                    self.rename = None;
                }
                Some(false) => self.rename = None,
                None => {}
            }
        }
        if let Some((id, text)) = &mut self.stall {
            match text_prompt(ctx, "stall-card", t().stall_prompt, text) {
                Some(true) => {
                    if let Ok(secs) = text.trim().parse::<u64>() {
                        events.push(Event::SetStall(id.clone(), secs.max(1)));
                    }
                    self.stall = None;
                }
                Some(false) => self.stall = None,
                None => {}
            }
        }
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, out: &mut Vec<Event>) {
        match ev {
            Event::NewCard { at, project, command, setup, cwd } => {
                let cell = base_cell(&self.ctx);
                let size = card_size(cell, DEFAULT_COLS, DEFAULT_ROWS);
                // With no project given, the terminal is born in the open space.
                let project = project.as_deref().or(world.space.as_deref());
                let project = project.and_then(|p| world.project(p));
                // With no position: right of the group's terminals (or in the center of an empty group).
                let beside = project.map(|p| match world.members(&p.id).map(|c| c.rect).reduce(|a, b| a.union(b)) {
                    Some(r) => pos2(r.max.x + crate::shell::world::CARD_GAP + size.x / 2.0, r.min.y + size.y / 2.0),
                    None => p.rect.center(),
                });
                let near = at.or(beside).unwrap_or(world.camera.center);
                let rect = world.free_spot(near, size);
                let mut card = Card::new(rect, cwd.clone().or_else(|| project.and_then(|p| p.cwd.clone())));
                card.project = project.map(|p| p.id.clone());
                let id = card.id.clone();
                if let Some(start) = Start::new(setup.as_deref(), command.as_deref()) {
                    self.commands.insert(id.clone(), CommandRun::Pending(start));
                }
                world.add_card(card);
                // Always fly to the new terminal: visible and large enough to type in.
                // First make room (nothing overlaps), then fly there.
                out.push(Event::SettleCard(id.clone()));
                out.push(Event::FlyTo(id));
            }
            Event::FocusCard(id) => {
                if let Some(card) = world.card_mut(id) {
                    card.attention = false;
                    world.focused = Some(id.clone());
                    world.raise(id);
                }
            }
            Event::Unfocus => world.focused = None,
            Event::RenameCard(id, name) => {
                if let Some(card) = world.card_mut(id) {
                    card.name = name.clone();
                    world.touch();
                }
            }
            Event::SetStall(id, secs) => {
                if let Some(card) = world.card_mut(id) {
                    card.stall_secs = *secs;
                    world.touch();
                }
            }
            Event::RequestClose(id) => {
                if world.card(id).is_some() {
                    self.confirm_close = Some(id.clone());
                }
            }
            Event::KillCard(id) => {
                if let Some(card) = world.card(id) {
                    let session = card.session();
                    self.killed.insert(session.clone());
                    self.runtimes.remove(id);
                    self.forget_command(id, &session);
                    self.forget_texts(id, out);
                    if self.launcher == Launcher::Tmux {
                        pty::kill_session_in_background(session);
                    }
                }
                world.remove_card(id);
            }
            Event::RemoveCard(id) => {
                self.runtimes.remove(id);
                self.spawn_errors.remove(id);
                if let Some(card) = world.card(id) {
                    let session = card.session();
                    self.forget_command(id, &session);
                }
                self.forget_texts(id, out);
                world.remove_card(id);
            }
            Event::SendText { card, text, enter } => {
                if let Some(outcome) = self.send_text(world, card, text, *enter, out) {
                    self.outcomes.insert(card.clone(), outcome.clone());
                    out.push(Event::TextSendOutcome { card: card.clone(), outcome });
                }
            }
            Event::Agent(AgentEvent::TextSent { id, problem, pasted, .. }) => {
                let result = match problem {
                    None => Ok(()),
                    Some(problem) if *pasted => Err(Sent::Pasted(refusal_of(problem))),
                    Some(problem) => Err(Sent::Not(refusal_of(problem))),
                };
                self.settle_send(*id, result, out);
            }
            Event::Agent(AgentEvent::Hello { send_text, .. }) => {
                // A new agent: the previous one will never answer what it was asked.
                self.settle_all(Some(SendRefusal::NoAgent), out);
                self.agent = if *send_text { AgentSupport::Ready } else { AgentSupport::TooOld };
            }
            Event::AgentLost => {
                self.agent = AgentSupport::Down;
                self.settle_all(Some(SendRefusal::NoAgent), out);
            }
            Event::RespawnCard(id) => {
                self.spawn_errors.remove(id);
                self.lost_commands.remove(id);
                self.unsent.remove(id);
                self.shown.remove(id);
                if let Some(card) = world.card_mut(id) {
                    card.pty_dead = false;
                }
            }
            Event::Agent(AgentEvent::Session(info)) => {
                if let Some(card) = world.cards.iter_mut().find(|c| c.session() == info.session) {
                    if info.cwd.is_some() && card.cwd != info.cwd {
                        card.cwd = info.cwd.clone();
                        world.revision += 1;
                    }
                    card.info = Some(info.clone());
                } else if let Some(id) = info.session.strip_prefix(SESSION_PREFIX).filter(|_| !self.killed.contains(&info.session)) {
                    // tmux session without a card (lost layout, created by hand…): adopt it.
                    let cell = base_cell(&self.ctx);
                    let rect = world.free_spot(world.camera.center, card_size(cell, DEFAULT_COLS, DEFAULT_ROWS));
                    let mut card = Card::with_id(id.to_string(), rect, info.cwd.clone());
                    card.info = Some(info.clone());
                    world.add_card(card);
                }
            }
            Event::Agent(AgentEvent::Gone { session }) => {
                if let Some(card) = world.cards.iter_mut().find(|c| &c.session() == session) {
                    card.info = None;
                }
            }
            _ => {}
        }
    }
}

/// The card menu's quick messages: those of its group, each sent to this card only, then the
/// way to add or edit them.
fn quick_messages_menu(ui: &mut Ui, world: &World, card: &Card, events: &mut Vec<Event>) {
    let Some(group) = card.project.as_deref().and_then(|p| world.project(p)) else {
        ui.label(egui::RichText::new(t().messages_need_group).color(theme::TEXT_DIM));
        return;
    };
    if group.messages.is_empty() {
        ui.label(egui::RichText::new(t().no_messages).color(theme::TEXT_DIM));
    }
    for m in &group.messages {
        let then = if m.enter { format!("\n\n{}", t().then_enter) } else { String::new() };
        let hover = egui::RichText::new(format!("{}\n\n{}{then}", m.name, m.text)).monospace();
        if ui.button(crate::features::projects::menu_name(&m.name)).on_hover_text(hover).clicked() {
            events.push(Event::SendText { card: card.id.clone(), text: m.text.clone(), enter: m.enter });
            ui.close();
        }
    }
    ui.separator();
    let edit = if group.messages.is_empty() { t().add_message } else { t().edit_messages };
    if ui.button(edit).clicked() {
        events.push(Event::EditQuickMessages(group.id.clone()));
        ui.close();
    }
}

/// Confirmation text: quote the running command, since that is what would be lost.
pub fn close_message(card: &Card) -> (String, String) {
    let title = (t().close_card_title)(&card.title());
    let running = card.info.as_ref().and_then(|i| i.command.as_deref());
    let body = match running {
        Some(cmd) => (t().close_card_running)(cmd),
        None => t().close_card_body.to_string(),
    };
    (title, body)
}

/// Last prompt the card's coding agent received (empty when no agent reports one).
pub fn header_prompt(card: &Card) -> Option<&str> {
    if card.pty_dead {
        return None;
    }
    card.info.as_ref()?.last_prompt.as_deref().map(str::trim).filter(|p| !p.is_empty())
}

/// Longest branch and worktree names shown whole in a card's badge.
const BADGE_BRANCH_CHARS: usize = 24;
const BADGE_WORKTREE_CHARS: usize = 16;

/// `feat/very-long-branch-name` -> `feat/ver…anch-name`: keeps both ends, which tell branches apart.
pub fn elide_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max < 3 {
        return s.to_string();
    }
    let tail = (max - 1) / 2;
    let head = max - 1 - tail;
    let chars: Vec<char> = s.chars().collect();
    format!("{}…{}", chars[..head].iter().collect::<String>(), chars[n - tail..].iter().collect::<String>())
}

/// What a card's branch badge says: its branch (or detached HEAD), then the linked worktree's
/// folder name; nothing more for the main checkout.
pub fn badge_label(w: &CardWorktree) -> String {
    let branch = w.branch.as_deref().map_or_else(|| t().detached_head.to_string(), |b| elide_middle(b, BADGE_BRANCH_CHARS));
    match &w.linked {
        Some(name) => format!("{branch} · {}", elide_middle(name, BADGE_WORKTREE_CHARS)),
        None => branch,
    }
}

/// What the title bar shows right of the title.
#[derive(Debug, PartialEq)]
struct HeaderLayout {
    /// The folder, right-aligned against the close button.
    cwd: bool,
    /// Left edge (screen x) of the branch badge, which sits just left of the folder.
    badge: Option<f32>,
    /// Horizontal span (screen x) of the prompt.
    prompt: Option<(f32, f32)>,
}

/// Lays out the folder, the branch badge and the prompt between the end of the title and
/// `right` (the close button). When space is short, the folder goes first, then the badge, and
/// narrowing never brings either back. The prompt keeps at least 120 points next to them,
/// otherwise it takes the whole room, down to a small hover target (an ellipsis) so its full text
/// stays reachable.
fn header_layout(title_end: f32, right: f32, cwd_width: Option<f32>, badge_width: Option<f32>, has_prompt: bool, zoom: f32) -> HeaderLayout {
    let gap = 14.0 * zoom;
    // Between the badge and the folder.
    let inner = 8.0 * zoom;
    let x0 = title_end + gap;
    // Room left of `left` (the leftmost extra): what the prompt (or nothing) needs.
    let need = if has_prompt { 120.0 * zoom + gap } else { 0.0 };
    let fits = |left: f32| left - x0 >= need;
    let prompt = |left: f32| has_prompt.then_some((x0, left - gap));
    if let Some(b) = badge_width {
        if let Some(c) = cwd_width {
            let left = right - c - inner - b;
            if fits(left) {
                return HeaderLayout { cwd: true, badge: Some(left), prompt: prompt(left) };
            }
        }
        if fits(right - b) {
            return HeaderLayout { cwd: false, badge: Some(right - b), prompt: prompt(right - b) };
        }
    }
    // The folder went before the badge: once the badge is gone, it does not come back.
    let cwd_width = cwd_width.filter(|_| badge_width.is_none());
    if !has_prompt {
        // Without a badge, as before badges: the folder stays, even over a long title.
        return HeaderLayout { cwd: cwd_width.is_some(), badge: None, prompt: None };
    }
    if let Some(c) = cwd_width
        && fits(right - c)
    {
        return HeaderLayout { cwd: true, badge: None, prompt: prompt(right - c) };
    }
    let min_target = 16.0 * zoom;
    let span = if right - x0 >= min_target { (x0, right) } else { (right - min_target, right) };
    HeaderLayout { cwd: false, badge: None, prompt: Some(span) }
}

/// Height of a strip at the bottom of a card (screen points).
const BANNER_H: f32 = 34.0;

/// A red strip: a message, a button that copies `text`, and ×. Returns `true` when dismissed.
/// `dismiss_a11y`: what the × says to screen readers (just "×" when `None`).
fn kept_text_banner(ui: &mut Ui, strip: Rect, message: &str, copy_label: &str, text: &str, dismiss_a11y: Option<&str>) -> bool {
    ui.painter_at(strip).rect_filled(strip, CornerRadius::ZERO, Color32::from_rgb(110, 40, 40));
    let mut dismiss = false;
    // Right to left: the buttons always fit, a long message is cut (whole on hover).
    ui.scope_builder(egui::UiBuilder::new().max_rect(strip.shrink2(vec2(10.0, 4.0))).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
        let close = ui.small_button("×");
        if let Some(label) = dismiss_a11y {
            close.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
        }
        if close.clicked() {
            dismiss = true;
        }
        if ui.button(copy_label).clicked() {
            ui.ctx().copy_text(text.to_string());
        }
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(egui::RichText::new(message).color(Color32::WHITE)).truncate()).on_hover_text(message);
        });
    });
    dismiss
}

fn cell_px(cell: Vec2) -> (u16, u16) {
    (cell.x.round().max(1.0) as u16, cell.y.round().max(1.0) as u16)
}

/// At this zoom, the card is drawn as a preview (title + status) instead of the terminal.
pub fn is_preview(zoom: f32) -> bool {
    quantize(BASE_FONT * zoom) < LOD_FONT_PX
}

/// Font sizes in quarter points: limits the number of variants in the glyph atlas.
fn quantize(px: f32) -> f32 {
    (px * 4.0).round() / 4.0
}

/// Zoomed-out view: large title, status and last lines.
fn paint_preview(painter: &egui::Painter, card: &Card, body: Rect, zoom: f32) {
    let painter = painter.with_clip_rect(body);
    let status = card.status();
    let big = (body.height() * 0.16).clamp(6.0, 48.0);
    painter.text(body.center() - vec2(0.0, big * 0.6), Align2::CENTER_CENTER, card.title(), FontId::proportional(big), theme::TEXT);
    if let Some(s) = status {
        painter.text(body.center() + vec2(0.0, big * 0.5), Align2::CENTER_CENTER, t().status(s), FontId::proportional(big * 0.6), theme::status_color(status));
    }
    let tail_px = 11.0 * zoom.max(0.0) * 2.0;
    if tail_px >= 6.0
        && let Some(info) = &card.info
    {
        let font = FontId::monospace(tail_px.min(14.0));
        for (k, line) in info.tail.iter().rev().enumerate() {
            let y = body.max.y - 6.0 - (k as f32 + 1.0) * tail_px * 1.3;
            painter.text(pos2(body.min.x + 8.0, y), Align2::LEFT_TOP, line, font.clone(), theme::TEXT_DIM);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: Vec2 = Vec2::new(8.0, 16.0);

    #[test]
    fn a_card_closed_while_its_command_is_staged_deletes_it() {
        let mut cards = TerminalCards::new(egui::Context::default(), Launcher::Disabled);
        // Not staged yet: nothing to delete on the Linux side.
        cards.commands.insert("a".into(), CommandRun::Pending(Start::command("echo a")));
        assert!(!cards.forget_command("a", "tc_a"));
        assert!(cards.commands.is_empty());
        // Staging: the job is told to delete what it staged.
        let cancelled = Arc::new(AtomicBool::new(false));
        let (_tx, rx) = mpsc::channel();
        cards.commands.insert("b".into(), CommandRun::Staging(Start::command("echo b"), rx, cancelled.clone()));
        assert!(cards.forget_command("b", "tc_b"), "a staged file may exist: discarded");
        assert!(cancelled.load(Ordering::SeqCst));
        assert_eq!(cards.pending_command("b"), None);
        // Attached, not yet taken: discarded too.
        let (_tx, rx) = mpsc::channel();
        cards.commands.insert("c".into(), CommandRun::Launched(Start::command("echo c"), rx));
        assert!(cards.forget_command("c", "tc_c"));
        assert!(!cards.forget_command("none", "tc_none"));
    }

    fn session_info(card: &Card) -> protocol::SessionInfo {
        protocol::SessionInfo {
            session: card.session(),
            status: protocol::TermStatus::Idle,
            command: None,
            cwd: None,
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
            agent_session_id: None,
            agent_transcript: None,
            last_prompt: None,
        }
    }

    /// Two cards whose sessions the agent reported, and an agent that delivers text.
    fn ready() -> (TerminalCards, World, CardId, CardId) {
        let mut cards = TerminalCards::new(egui::Context::default(), Launcher::Disabled);
        let mut world = World::default();
        let mut ids = Vec::new();
        for x in [0.0, 500.0] {
            let mut card = Card::new(Rect::from_min_size(pos2(x, 0.0), Vec2::splat(100.0)), None);
            card.info = Some(session_info(&card));
            ids.push(card.id.clone());
            world.add_card(card);
        }
        cards.handle(&Event::Agent(AgentEvent::Hello { version: "1".into(), hooks: protocol::Hooks::Installed, send_text: true }), &mut world, &mut vec![]);
        (cards, world, ids[0].clone(), ids[1].clone())
    }

    fn send(cards: &mut TerminalCards, world: &mut World, id: &str, text: &str, enter: bool) -> Vec<Event> {
        let mut out = vec![];
        cards.handle(&Event::SendText { card: id.into(), text: text.into(), enter }, world, &mut out);
        out
    }

    fn answer(cards: &mut TerminalCards, world: &mut World, request: u64, problem: Option<Problem>) -> Vec<Event> {
        answer_pasted(cards, world, request, problem, false)
    }

    fn answer_pasted(cards: &mut TerminalCards, world: &mut World, request: u64, problem: Option<Problem>, pasted: bool) -> Vec<Event> {
        let mut out = vec![];
        cards.handle(&Event::Agent(AgentEvent::TextSent { id: request, session: "tc_x".into(), problem, pasted }), world, &mut out);
        out
    }

    /// The outcome the sender got, and whether a notice names the card.
    fn outcome_of(out: &[Event], id: &str) -> (Option<SendOutcome>, bool) {
        let outcome = out.iter().find_map(|e| match e {
            Event::TextSendOutcome { card, outcome } if card == id => Some(outcome.clone()),
            _ => None,
        });
        (outcome, out.iter().any(|e| matches!(e, Event::Notify(c, Notice::NotSent(_)) if c == id)))
    }

    #[test]
    fn text_goes_to_the_agent_for_that_session_and_is_sent_once_tmux_took_it() {
        let (mut cards, mut world, id, other) = ready();
        let session = world.card(&id).unwrap().session();
        let out = send(&mut cards, &mut world, &id, "echo one\r\necho two\n", true);
        // Its session only; CRLF as LF, the trailing line break gone (the Enter submits once).
        assert!(matches!(out.as_slice(), [Event::ToAgent(AgentCmd::SendText { id: 0, session: s, text, enter: true, .. })] if *s == session && text == "echo one\necho two"), "{out:?}");
        assert_eq!(cards.last_outcome(&id), None, "in flight: no outcome yet");
        cards.unsent.insert(id.clone(), ("old".into(), SendRefusal::NoAgent));
        let out = answer(&mut cards, &mut world, 0, None);
        assert_eq!(outcome_of(&out, &id), (Some(SendOutcome::Sent), false));
        assert_eq!(cards.last_outcome(&id), Some(&SendOutcome::Sent));
        assert_eq!(cards.unsent_text(&id), None, "an older refusal is stale now");
        assert!(answer(&mut cards, &mut world, 0, None).is_empty(), "answered once");
        // Without Enter, the text's own line break stays.
        let out = send(&mut cards, &mut world, &other, "draft\n", false);
        assert!(matches!(out.as_slice(), [Event::ToAgent(AgentCmd::SendText { id: 1, text, enter: false, .. })] if text == "draft\n"), "{out:?}");
        // Nothing left to send: nothing sent.
        assert_eq!(outcome_of(&send(&mut cards, &mut world, &id, "\r\n", true), &id), (Some(SendOutcome::Empty), false));
        assert!(cards.written(&id).is_empty() && cards.written(&other).is_empty(), "never through the terminal's keys");
    }

    #[test]
    fn keys_typed_while_text_is_in_flight_wait_for_its_enter_but_ctrl_c_does_not() {
        let (mut cards, mut world, id, other) = ready();
        send(&mut cards, &mut world, &id, "go on", true);
        cards.type_into(&id, b"ab".to_vec());
        cards.type_into(&other, b"x".to_vec());
        assert!(cards.written(&id).is_empty(), "held: it would land between the paste and its Enter");
        assert_eq!(cards.written(&other), b"x", "other cards are not held");
        cards.type_into(&id, vec![0x03]);
        assert_eq!(cards.written(&id), b"ab\x03", "Ctrl+C goes at once, with what was held before it");
        cards.type_into(&id, b"c".to_vec());
        answer(&mut cards, &mut world, 0, None);
        assert_eq!(cards.written(&id), b"ab\x03c", "the rest once the text is done");
        cards.type_into(&id, b"d".to_vec());
        assert_eq!(cards.written(&id), b"ab\x03cd");
    }

    #[test]
    fn a_card_that_cannot_take_text_refuses_it_and_keeps_it() {
        let (mut cards, mut world, id, _) = ready();
        let refused = |out: &[Event], id: &str, why: SendRefusal| outcome_of(out, id) == (Some(SendOutcome::Refused(why)), true);
        assert_eq!(outcome_of(&send(&mut cards, &mut world, "gone", "x", true), "gone"), (Some(SendOutcome::Refused(SendRefusal::NoCard)), false));
        assert_eq!(cards.unsent_text("gone"), None, "no card to keep it on: only its sender is told");
        // Its start command not taken yet: text sent now would land in front of it.
        cards.commands.insert(id.clone(), CommandRun::Pending(Start::command("claude")));
        assert!(refused(&send(&mut cards, &mut world, &id, "go on", true), &id, SendRefusal::Starting));
        assert_eq!(cards.unsent_text(&id), Some(("go on", &SendRefusal::Starting)));
        let (_tx, rx) = mpsc::channel();
        cards.commands.insert(id.clone(), CommandRun::Launched(Start::command("claude"), rx));
        assert!(refused(&send(&mut cards, &mut world, &id, "go on", true), &id, SendRefusal::Starting), "until its shell took it");
        cards.commands.clear();
        // Its session not seen by the agent yet.
        world.card_mut(&id).unwrap().info = None;
        assert!(refused(&send(&mut cards, &mut world, &id, "x", true), &id, SendRefusal::Starting));
        let info = session_info(world.card(&id).unwrap());
        world.card_mut(&id).unwrap().info = Some(info);
        cards.spawn_errors.insert(id.clone(), "no wsl.exe".into());
        assert!(refused(&send(&mut cards, &mut world, &id, "a", false), &id, SendRefusal::NotStarted));
        world.card_mut(&id).unwrap().pty_dead = true;
        assert!(refused(&send(&mut cards, &mut world, &id, "b\nc", true), &id, SendRefusal::Ended));
        assert_eq!(cards.unsent_text(&id), Some(("b\nc", &SendRefusal::Ended)), "the latest text, as written");
        // Respawned: the strip goes.
        cards.handle(&Event::RespawnCard(id.clone()), &mut world, &mut vec![]);
        assert_eq!(cards.unsent_text(&id), None);
        assert!(cards.in_flight.is_empty(), "nothing was handed to the agent");

        // A real terminal still blank (its shell not shown yet): starting.
        let mut real = TerminalCards::new(egui::Context::default(), Launcher::Tmux);
        real.agent = AgentSupport::Ready;
        assert!(refused(&send(&mut real, &mut world, &id, "x", false), &id, SendRefusal::Starting));
        // A pane the agent reports dead (kept by `remain-on-exit`, its client still attached).
        let info = protocol::SessionInfo { status: protocol::TermStatus::Dead, ..session_info(world.card(&id).unwrap()) };
        world.card_mut(&id).unwrap().info = Some(info);
        assert!(refused(&send(&mut cards, &mut world, &id, "x", true), &id, SendRefusal::Ended));
    }

    #[test]
    fn a_card_once_shown_stays_ready_through_a_blank_screen_until_it_is_attached_again() {
        let (_, mut world, id, _) = ready();
        let canned = |cards: &mut TerminalCards, id: &CardId, bytes: &[u8]| {
            let rt = TermRuntime::canned(id, GridSize { cols: 20, rows: 3 }, (8, 16), bytes, cards.tx.clone(), cards.ctx.clone());
            cards.runtimes.insert(id.clone(), rt);
        };
        let mut cards = TerminalCards::new(egui::Context::default(), Launcher::Tmux);
        cards.agent = AgentSupport::Ready;
        canned(&mut cards, &id, b"");
        cards.poll(&mut world, &mut vec![]);
        assert_eq!(outcome_of(&send(&mut cards, &mut world, &id, "x", true), &id).0, Some(SendOutcome::Refused(SendRefusal::Starting)), "nothing shown yet");
        canned(&mut cards, &id, b"$ ");
        cards.poll(&mut world, &mut vec![]);
        // `clear`: blank again, but its shell is up.
        canned(&mut cards, &id, b"");
        cards.poll(&mut world, &mut vec![]);
        assert!(matches!(send(&mut cards, &mut world, &id, "x", true).as_slice(), [Event::ToAgent(AgentCmd::SendText { .. })]));
        // Respawned: it must show again.
        cards.handle(&Event::RespawnCard(id.clone()), &mut world, &mut vec![]);
        assert_eq!(outcome_of(&send(&mut cards, &mut world, &id, "y", true), &id).0, Some(SendOutcome::Refused(SendRefusal::Starting)));
    }

    #[test]
    fn a_text_pasted_but_not_submitted_is_told_and_not_offered_again() {
        let (mut cards, mut world, id, _) = ready();
        let out = send(&mut cards, &mut world, &id, "go on", true);
        assert!(
            matches!(out.as_slice(), [Event::ToAgent(AgentCmd::SendText { limit_ms: Some(ms), .. })] if *ms < SEND_ANSWER_TIMEOUT.as_millis() as u64),
            "the agent gives up before the app does: {out:?}"
        );
        let busy = Problem::PaneBusy { session: "tc_x".into(), mode: "clock-mode".into() };
        let out = answer_pasted(&mut cards, &mut world, 0, Some(busy), true);
        let why = SendRefusal::Busy("clock-mode".into());
        assert_eq!(outcome_of(&out, &id).0, Some(SendOutcome::PastedNotSubmitted(why.clone())));
        assert!(out.iter().any(|e| matches!(e, Event::Notify(c, Notice::NotSubmitted(w)) if *c == id && *w == why)));
        assert_eq!(cards.unsent_text(&id), None, "already in the input line: no copy offered");
        // Not pasted at all: refused and kept, each with its reason.
        for (k, (problem, why)) in [
            (Problem::PaneDead { session: "tc_x".into() }, SendRefusal::Ended),
            (Problem::PaneBusy { session: "tc_x".into(), mode: "tree-mode".into() }, SendRefusal::Busy("tree-mode".into())),
            (Problem::Expired, SendRefusal::Expired),
        ]
        .into_iter()
        .enumerate()
        {
            send(&mut cards, &mut world, &id, "again", true);
            let out = answer(&mut cards, &mut world, k as u64 + 1, Some(problem));
            assert_eq!(outcome_of(&out, &id), (Some(SendOutcome::Refused(why.clone())), true));
            assert_eq!(cards.unsent_text(&id), Some(("again", &why)));
        }
    }

    #[test]
    fn without_an_agent_that_delivers_text_it_is_refused_and_kept() {
        let (_, mut world, id, _) = ready();
        let mut cards = TerminalCards::new(egui::Context::default(), Launcher::Disabled);
        assert_eq!(outcome_of(&send(&mut cards, &mut world, &id, "x", true), &id).0, Some(SendOutcome::Refused(SendRefusal::NoAgent)));
        cards.handle(&Event::Agent(AgentEvent::Hello { version: "0.1".into(), hooks: protocol::Hooks::Installed, send_text: false }), &mut world, &mut vec![]);
        assert_eq!(outcome_of(&send(&mut cards, &mut world, &id, "x", true), &id).0, Some(SendOutcome::Refused(SendRefusal::AgentTooOld)));
        assert_eq!(cards.unsent_text(&id), Some(("x", &SendRefusal::AgentTooOld)));
    }

    #[test]
    fn what_the_agent_could_not_deliver_is_refused_and_kept() {
        let (mut cards, mut world, id, other) = ready();
        send(&mut cards, &mut world, &id, "a", true);
        send(&mut cards, &mut world, &other, "b", true);
        send(&mut cards, &mut world, &id, "c", true);
        let out = answer(&mut cards, &mut world, 0, Some(Problem::SessionNotFound { session: "tc_x".into() }));
        assert_eq!(outcome_of(&out, &id), (Some(SendOutcome::Refused(SendRefusal::NoSession)), true));
        let out = answer(&mut cards, &mut world, 1, Some(Problem::SendFailed { detail: "no space".into() }));
        assert_eq!(outcome_of(&out, &other).0, Some(SendOutcome::Refused(SendRefusal::Failed("no space".into()))));
        assert_eq!(cards.unsent_text(&other), Some(("b", &SendRefusal::Failed("no space".into()))));
        // The agent goes: what it was asked will never be answered.
        cards.type_into(&id, b"k".to_vec());
        let mut out = vec![];
        cards.handle(&Event::AgentLost, &mut world, &mut out);
        assert_eq!(outcome_of(&out, &id).0, Some(SendOutcome::Refused(SendRefusal::NoAgent)));
        assert_eq!(cards.written(&id), b"k", "held keys go");
        assert_eq!(outcome_of(&send(&mut cards, &mut world, &id, "d", true), &id).0, Some(SendOutcome::Refused(SendRefusal::NoAgent)));
    }

    #[test]
    fn an_agent_that_does_not_answer_in_time_releases_the_card() {
        let (mut cards, mut world, id, _) = ready();
        send(&mut cards, &mut world, &id, "a", true);
        cards.type_into(&id, b"k".to_vec());
        let mut out = vec![];
        cards.poll(&mut world, &mut out);
        assert!(out.is_empty() && cards.written(&id).is_empty(), "still in time");
        cards.in_flight.get_mut(&0).unwrap().since -= SEND_ANSWER_TIMEOUT;
        cards.poll(&mut world, &mut out);
        assert_eq!(outcome_of(&out, &id), (Some(SendOutcome::Refused(SendRefusal::NoAnswer)), true));
        assert_eq!(cards.written(&id), b"k");
        assert!(answer(&mut cards, &mut world, 0, None).is_empty(), "a late answer is ignored");
    }

    #[test]
    fn a_card_closed_while_text_is_in_flight_tells_its_sender() {
        let (mut cards, mut world, id, _) = ready();
        send(&mut cards, &mut world, &id, "a", true);
        let mut out = vec![];
        cards.handle(&Event::RemoveCard(id.clone()), &mut world, &mut out);
        assert_eq!(outcome_of(&out, &id).0, Some(SendOutcome::Refused(SendRefusal::NoCard)));
        assert!(answer(&mut cards, &mut world, 0, None).is_empty());
    }

    #[test]
    fn refusals_name_the_card_and_say_why() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        assert_eq!(
            (t().text_not_submitted)("api", &t().send_refusal(&SendRefusal::Busy("clock-mode".into()))),
            "Texte tapé dans « api » mais pas validé : son volet affiche un écran tmux (clock-mode) : fermez-le d'abord."
        );
        assert_eq!((t().text_not_sent)("api", &t().send_refusal(&SendRefusal::Ended)), "Texte non envoyé à « api » : sa session est terminée.");
        assert_eq!(t().send_refusal(&SendRefusal::Failed("no space".into())), "tmux l'a refusé (no space).");
        crate::shell::i18n::set(crate::shell::i18n::Lang::En);
        assert_eq!((t().text_not_sent)("api", &t().send_refusal(&SendRefusal::NoAgent)), "Text not sent to “api”: the agent (tcanvas-agent), which delivers it, is not connected.");
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
    }

    #[test]
    fn a_staging_job_deletes_what_it_staged_once_its_card_is_gone() {
        let mut discarded = false;
        finish_staging(&AtomicBool::new(false), || discarded = true);
        assert!(!discarded, "card still there: the command waits for its attach");
        finish_staging(&AtomicBool::new(true), || discarded = true);
        assert!(discarded);
    }

    #[test]
    fn default_card_holds_default_grid() {
        let size = card_size(CELL, DEFAULT_COLS, DEFAULT_ROWS);
        let g = grid_for(Rect::from_min_size(Pos2::ZERO, size), CELL);
        assert_eq!(g, GridSize { cols: 120, rows: 34 });
    }

    #[test]
    fn grid_never_below_minimum() {
        let g = grid_for(Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0)), CELL);
        assert_eq!(g, GridSize { cols: MIN_COLS, rows: MIN_ROWS });
    }

    #[test]
    fn grid_ignores_partial_cells() {
        let size = card_size(CELL, 80.0, 24.0) + vec2(7.9, 15.9);
        let g = grid_for(Rect::from_min_size(pos2(-500.0, 300.0), size), CELL);
        assert_eq!(g, GridSize { cols: 80, rows: 24 });
    }

    #[test]
    fn close_message_names_the_running_command() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let mut card = Card::new(Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)), None);
        card.name = Some("api".into());
        let (title, body) = close_message(&card);
        assert_eq!(title, "Fermer « api » ?");
        assert!(!body.contains("en cours"));
        card.info = Some(protocol::SessionInfo {
            session: card.session(),
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
        assert!(close_message(&card).1.starts_with("« cargo run » est en cours"));
    }

    #[test]
    fn the_header_prompt_comes_from_the_agent_while_the_terminal_lives() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let mut card = Card::new(Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)), None);
        assert_eq!(header_prompt(&card), None);
        let session = card.session();
        let info = |prompt: Option<&str>| protocol::SessionInfo {
            session: session.clone(),
            status: protocol::TermStatus::Running,
            command: Some("claude".into()),
            cwd: None,
            last_exit: None,
            last_duration_secs: None,
            tail: vec![],
            agent_session_id: None,
            agent_transcript: None,
            last_prompt: prompt.map(String::from),
        };
        card.info = Some(info(Some("  ajoute le total TTC ")));
        assert_eq!(header_prompt(&card), Some("ajoute le total TTC"));
        assert_eq!((t().last_prompt)("ajoute le total TTC"), "« ajoute le total TTC »");
        card.info = Some(info(Some("  ")));
        assert_eq!(header_prompt(&card), None);
        card.info = Some(info(Some("x")));
        card.pty_dead = true;
        assert_eq!(header_prompt(&card), None, "nothing on a dead terminal");
    }

    #[test]
    fn the_prompt_wins_over_the_folder_when_space_is_short() {
        let layout = |cwd, prompt| HeaderLayout { cwd, badge: None, prompt };
        // No prompt: the folder, as before.
        assert_eq!(header_layout(100.0, 500.0, Some(80.0), None, false, 1.0), layout(true, None));
        // Room for both: the prompt sits between the title and the folder.
        assert_eq!(header_layout(100.0, 500.0, Some(80.0), None, true, 1.0), layout(true, Some((114.0, 406.0))));
        assert_eq!(header_layout(100.0, 500.0, None, None, true, 1.0), layout(false, Some((114.0, 500.0))));
        // Short: the folder goes, the prompt takes its room.
        assert_eq!(header_layout(100.0, 300.0, Some(80.0), None, true, 1.0), layout(false, Some((114.0, 300.0))));
        // No room at all: still a small hover target against the close button.
        assert_eq!(header_layout(290.0, 300.0, Some(80.0), None, true, 1.0), layout(false, Some((284.0, 300.0))));
        assert_eq!(header_layout(290.0, 300.0, Some(80.0), None, true, 0.5), layout(false, Some((292.0, 300.0))));
    }

    #[test]
    fn the_branch_badge_sits_left_of_the_folder_and_gives_way_to_the_prompt() {
        let at = |cwd, badge: Option<f32>, prompt| HeaderLayout { cwd, badge, prompt };
        // Wide card, no prompt: badge then folder against the close button.
        assert_eq!(header_layout(100.0, 600.0, Some(80.0), Some(60.0), false, 1.0), at(true, Some(452.0), None));
        // With a prompt: prompt | badge | folder.
        assert_eq!(header_layout(100.0, 600.0, Some(80.0), Some(60.0), true, 1.0), at(true, Some(452.0), Some((114.0, 438.0))));
        // Twice the zoom: every gap doubles.
        assert_eq!(header_layout(100.0, 1200.0, Some(160.0), Some(120.0), true, 2.0), at(true, Some(904.0), Some((128.0, 876.0))));
        // Less room: the folder goes first, the badge moves against the close button.
        assert_eq!(header_layout(100.0, 380.0, Some(80.0), Some(60.0), true, 1.0), at(false, Some(320.0), Some((114.0, 306.0))));
        assert_eq!(header_layout(100.0, 220.0, Some(80.0), Some(60.0), false, 1.0), at(false, Some(160.0), None));
        // Narrow card: no badge; the prompt keeps its hover target.
        assert_eq!(header_layout(100.0, 300.0, Some(80.0), Some(60.0), true, 1.0), at(false, None, Some((114.0, 300.0))));
        assert_eq!(header_layout(290.0, 300.0, Some(80.0), Some(60.0), true, 1.0), at(false, None, Some((284.0, 300.0))));
        // Narrow card without a prompt: neither, the folder does not come back once the badge went.
        assert_eq!(header_layout(100.0, 160.0, Some(80.0), Some(60.0), false, 1.0), at(false, None, None));
        // No badge (cwd outside any known worktree): the folder as before badges.
        assert_eq!(header_layout(100.0, 160.0, Some(80.0), None, false, 1.0), at(true, None, None));
        // No folder known: the badge alone.
        assert_eq!(header_layout(100.0, 600.0, None, Some(60.0), true, 1.0), at(false, Some(540.0), Some((114.0, 526.0))));
    }

    #[test]
    fn narrowing_a_card_only_ever_drops_header_extras() {
        // Folder wider than the badge, then a short folder next to a long badge.
        for (cwd, badge) in [(80.0, 60.0), (30.0, 90.0)] {
            for prompt in [false, true] {
                let mut shown = (true, true);
                for right in (110..=700).rev().step_by(5) {
                    let l = header_layout(100.0, right as f32, Some(cwd), Some(badge), prompt, 1.0);
                    let now = (l.cwd, l.badge.is_some());
                    assert!((now.0 <= shown.0) && (now.1 <= shown.1), "came back at right={right} (cwd {cwd}, badge {badge}, prompt {prompt}): {shown:?} -> {now:?}");
                    shown = now;
                }
                assert_eq!(shown, (false, false), "all gone on the narrowest card");
            }
        }
    }

    #[test]
    fn long_names_are_elided_in_the_middle() {
        assert_eq!(elide_middle("main", 24), "main");
        assert_eq!(elide_middle("feat/very-long-branch-name", 12), "feat/v…-name");
        assert_eq!(elide_middle("feat/very-long-branch-name", 12).chars().count(), 12);
        assert_eq!(elide_middle("éééééé", 5), "éé…éé");
        assert_eq!(elide_middle("abcdef", 2), "abcdef", "too short to elide");
    }

    #[test]
    fn the_badge_names_the_branch_and_a_linked_worktree() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        let main = CardWorktree { path: "/home/me/shop".into(), branch: Some("main".into()), linked: None };
        assert_eq!(badge_label(&main), "main");
        let linked = CardWorktree { path: "/home/me/shop/.claude/worktrees/invoices".into(), branch: Some("feat/invoices-with-taxes-and-rounding".into()), linked: Some("invoices".into()) };
        assert_eq!(badge_label(&linked), "feat/invoice…nd-rounding · invoices");
        let detached = CardWorktree { linked: Some("a-very-long-worktree-folder".into()), branch: None, ..main };
        assert_eq!(badge_label(&detached), "HEAD détachée · a-very-l…-folder");
    }

    #[test]
    fn preview_threshold() {
        assert!(is_preview(0.369));
        assert!(!is_preview(0.6));
        assert!(!is_preview(1.0));
    }

    #[test]
    fn font_sizes_are_quantized() {
        assert_eq!(quantize(14.0 * 0.333), 4.75);
        assert_eq!(quantize(14.0), 14.0);
    }
}
