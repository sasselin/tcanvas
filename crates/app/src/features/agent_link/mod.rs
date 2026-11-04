//! Lien avec `tcanvas-agent` dans WSL : lancement, lecture des événements, relance automatique.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use protocol::{AgentCmd, AgentEvent, DEFAULT_STALL_SECS};

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::launch;
use crate::shell::theme;
use crate::shell::world::World;

const RETRY_AFTER: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    Spawn,
    Disabled,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LinkState {
    Off,
    Starting,
    Connected { hooks: bool },
    Failed(String),
}

enum Msg {
    Event(AgentEvent),
    Closed(String),
}

pub struct AgentLink {
    mode: AgentMode,
    ctx: egui::Context,
    state: LinkState,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    retry_at: Option<Instant>,
    /// Agent désactivé (tests) : les commandes sont gardées ici au lieu d'être envoyées.
    sent: Vec<AgentCmd>,
}

impl AgentLink {
    pub fn new(ctx: egui::Context, mode: AgentMode) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut link = Self { mode, ctx, state: LinkState::Off, child: None, stdin: None, tx, rx, retry_at: None, sent: Vec::new() };
        link.start();
        link
    }

    pub fn state(&self) -> &LinkState {
        &self.state
    }

    fn start(&mut self) {
        if self.mode == AgentMode::Disabled {
            return;
        }
        self.stop();
        let spawned = launch::agent(&[]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => return self.fail((t().agent_launch_failed)(&e.to_string())),
        };
        let (stdout, mut stderr) = (child.stdout.take().expect("stdout piped"), child.stderr.take().expect("stderr piped"));
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                match protocol::decode::<AgentEvent>(&line) {
                    Ok(ev) => {
                        let _ = tx.send(Msg::Event(ev));
                        ctx.request_repaint();
                    }
                    Err(e) => eprintln!("agent: ignored line ({e}): {line}"),
                }
            }
            let mut err = String::new();
            let _ = stderr.read_to_string(&mut err);
            let last = err.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string).unwrap_or_else(|| t().agent_stopped.to_string());
            let _ = tx.send(Msg::Closed(last));
            ctx.request_repaint();
        });
        self.stdin = child.stdin.take();
        self.child = Some(child);
        self.state = LinkState::Starting;
        self.retry_at = None;
    }

    fn stop(&mut self) {
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn fail(&mut self, why: String) {
        self.stop();
        self.state = LinkState::Failed(why);
        self.retry_at = Some(Instant::now() + RETRY_AFTER);
        self.ctx.request_repaint_after(RETRY_AFTER);
    }

    pub fn sent(&self) -> &[AgentCmd] {
        &self.sent
    }

    pub fn send(&mut self, cmd: &AgentCmd) {
        if self.mode == AgentMode::Disabled {
            self.sent.push(cmd.clone());
            return;
        }
        if let Some(stdin) = &mut self.stdin
            && stdin.write_all(protocol::encode(cmd).as_bytes()).and_then(|_| stdin.flush()).is_err() {
                self.stdin = None;
            }
    }

    pub fn poll(&mut self, world: &World, events: &mut Vec<Event>) {
        if self.retry_at.is_some_and(|t| Instant::now() >= t) {
            self.start();
        }
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Event(ev) => {
                    if let AgentEvent::Hello { hooks_installed, .. } = &ev {
                        self.state = LinkState::Connected { hooks: *hooks_installed };
                        for card in world.cards.iter().filter(|c| c.stall_secs != DEFAULT_STALL_SECS) {
                            self.send(&AgentCmd::SetStall { session: card.session(), secs: card.stall_secs });
                        }
                    }
                    events.push(Event::Agent(ev));
                }
                Msg::Closed(why) => self.fail(why),
            }
        }
    }

    pub fn status_ui(&mut self, ui: &mut Ui) {
        match self.state.clone() {
            LinkState::Off => {}
            LinkState::Starting => {
                ui.label(RichText::new(t().agent_starting).color(theme::TEXT_DIM));
            }
            LinkState::Connected { hooks } => {
                ui.label(RichText::new(t().agent_connected).color(theme::status_color(Some(protocol::TermStatus::Running))))
                    .on_hover_text(t().agent_connected_hint);
                if !hooks {
                    ui.label(RichText::new(t().hooks_missing).color(theme::status_color(Some(protocol::TermStatus::Stalled)))).on_hover_text(t().hooks_missing_hint);
                }
            }
            LinkState::Failed(why) => {
                let label = ui.label(RichText::new(t().agent_missing).color(theme::status_color(Some(protocol::TermStatus::Exited(1)))));
                label.on_hover_text((t().agent_missing_hint)(&why));
                if ui.small_button(t().retry).clicked() {
                    self.start();
                }
            }
        }
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, _out: &mut Vec<Event>) {
        match ev {
            Event::SetStall(id, secs) => {
                if let Some(card) = world.card(id) {
                    let session = card.session();
                    self.send(&AgentCmd::SetStall { session, secs: *secs });
                }
            }
            Event::ToAgent(cmd) => self.send(cmd),
            _ => {}
        }
    }
}

impl Drop for AgentLink {
    fn drop(&mut self) {
        self.stop();
    }
}
