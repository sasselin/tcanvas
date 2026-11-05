//! Link to `tcanvas-agent` in WSL: installation, launch, event reading, automatic restart.

pub mod install;

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use protocol::{AgentCmd, AgentEvent, DEFAULT_STALL_SECS, Hooks, Problem};

use crate::shell::event::Event;
use crate::shell::i18n::t;
use crate::shell::launch;
use crate::shell::theme;
use crate::shell::world::World;

const RETRY_AFTER: Duration = Duration::from_secs(10);
/// The agent repeats `TmuxMissing` every second while tmux is absent: the warning goes away
/// this long after the last one.
const TMUX_WARNING_FOR: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    Spawn,
    Disabled,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LinkState {
    Off,
    Starting,
    Connected { hooks: Hooks },
    Failed(String),
}

enum Msg {
    Event(AgentEvent),
    Closed(String),
    /// End of the self-installation (`Err`: why it failed).
    Installed(Result<(), String>),
}

/// Where the agent comes from before it can be launched.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Install {
    /// Nothing to install (Linux, `TCANVAS_AGENT`), or already checked this run.
    Ready,
    Needed,
    Running,
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
    /// Agent disabled (tests): commands are kept here instead of being sent.
    sent: Vec<AgentCmd>,
    install: Install,
    /// Last `TmuxMissing` reported by the agent (detail, when).
    tmux_missing: Option<(String, Instant)>,
    /// "Install" was clicked and the agent has not answered yet.
    hooks_pending: bool,
    hooks_error: Option<String>,
}

impl AgentLink {
    pub fn new(ctx: egui::Context, mode: AgentMode) -> Self {
        let (tx, rx) = mpsc::channel();
        let install = if install::embedded().is_some() && std::env::var_os("TCANVAS_AGENT").is_none() { Install::Needed } else { Install::Ready };
        let mut link = Self { mode, ctx, state: LinkState::Off, child: None, stdin: None, tx, rx, retry_at: None, sent: Vec::new(), install, tmux_missing: None, hooks_pending: false, hooks_error: None };
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
        match self.install {
            Install::Ready => {}
            Install::Running => return,
            Install::Needed => return self.start_install(),
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

    /// Copies the embedded agent into WSL in the background (the first `wsl.exe` call may
    /// boot WSL), then launches it.
    fn start_install(&mut self) {
        let Some(agent) = install::embedded() else { return };
        self.install = Install::Running;
        self.state = LinkState::Starting;
        self.retry_at = None;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let result = install::ensure(agent.bytes, agent.id, None).map(|_| ());
            let _ = tx.send(Msg::Installed(result));
            ctx.request_repaint();
        });
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

    pub fn poll(&mut self, events: &mut Vec<Event>) {
        if self.retry_at.is_some_and(|t| Instant::now() >= t) {
            self.start();
        }
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Event(ev) => events.push(Event::Agent(ev)),
                Msg::Closed(why) => self.fail(why),
                Msg::Installed(Ok(())) => {
                    self.install = Install::Ready;
                    self.start();
                }
                Msg::Installed(Err(why)) => {
                    self.install = Install::Needed;
                    self.fail((t().agent_install_failed)(&why));
                }
            }
        }
    }

    pub fn status_ui(&mut self, ui: &mut Ui) {
        if let Some((detail, at)) = &self.tmux_missing {
            if at.elapsed() < TMUX_WARNING_FOR {
                ui.label(RichText::new(t().tmux_missing_label).color(theme::status_color(Some(protocol::TermStatus::Exited(1))))).on_hover_text((t().tmux_missing_hint)(detail));
                ui.ctx().request_repaint_after(TMUX_WARNING_FOR);
            } else {
                self.tmux_missing = None;
            }
        }
        match self.state.clone() {
            LinkState::Off => {}
            LinkState::Starting => {
                ui.label(RichText::new(t().agent_starting).color(theme::TEXT_DIM));
            }
            LinkState::Connected { hooks } => {
                ui.label(RichText::new(t().agent_connected).color(theme::status_color(Some(protocol::TermStatus::Running))))
                    .on_hover_text(t().agent_connected_hint);
                if hooks == Hooks::Missing {
                    let hint = match &self.hooks_error {
                        Some(e) => format!("{}\n\n{}", t().hooks_missing_hint, (t().hooks_failed)(e)),
                        None => t().hooks_missing_hint.to_string(),
                    };
                    ui.label(RichText::new(t().hooks_missing).color(theme::status_color(Some(protocol::TermStatus::Stalled)))).on_hover_text(&hint);
                    if ui.add_enabled(!self.hooks_pending, egui::Button::new(t().install_hooks).small()).on_hover_text(&hint).clicked() {
                        self.hooks_pending = true;
                        self.hooks_error = None;
                        self.send(&AgentCmd::InstallHooks);
                    }
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
            Event::Agent(AgentEvent::Hello { hooks, .. }) => {
                self.state = LinkState::Connected { hooks: *hooks };
                self.hooks_pending = false;
                for card in world.cards.iter().filter(|c| c.stall_secs != DEFAULT_STALL_SECS) {
                    self.send(&AgentCmd::SetStall { session: card.session(), secs: card.stall_secs });
                }
            }
            Event::Agent(AgentEvent::HooksChanged { hooks }) => {
                self.hooks_pending = false;
                if let LinkState::Connected { hooks: h } = &mut self.state {
                    *h = *hooks;
                }
            }
            Event::Agent(AgentEvent::Error { problem: Problem::HooksFailed { detail } }) => {
                self.hooks_pending = false;
                self.hooks_error = Some(detail.clone());
            }
            Event::Agent(AgentEvent::Error { problem: Problem::TmuxMissing { detail } }) => self.tmux_missing = Some((detail.clone(), Instant::now())),
            _ => {}
        }
    }
}

impl Drop for AgentLink {
    fn drop(&mut self) {
        self.stop();
    }
}
