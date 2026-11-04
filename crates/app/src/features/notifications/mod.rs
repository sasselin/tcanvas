//! Notifications : quand un terminal se bloque, attend une réponse, échoue, finit une longue
//! commande ou s'arrête. Badge sur la carte, toast dans l'application, toast Windows.

mod os;

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};

use egui::{Align2, Id, RichText, Sense};
use protocol::{AgentEvent, SessionInfo, TermStatus};

use crate::shell::event::{Event, Notice};
use crate::shell::i18n::t;
use crate::shell::theme;
use crate::shell::world::{Card, CardId, World};

/// Une commande qui réussit n'est signalée que si elle a duré au moins ça.
pub const LONG_COMMAND_SECS: u64 = 30;
const TOAST_SECS: f64 = 15.0;
/// La confirmation d'une copie n'a pas besoin de rester.
const COPIED_SECS: f64 = 2.0;
const MAX_TOASTS: usize = 5;
/// Code de sortie d'un Ctrl+C : pas un échec à signaler.
const SIGINT_EXIT: i32 = 130;

/// Transition de statut → notification éventuelle. `prev == None` : première observation
/// (démarrage, reconnexion), on ne signale rien.
pub fn decide(prev: Option<TermStatus>, info: &SessionInfo) -> Option<Notice> {
    let prev = prev?;
    if prev == info.status {
        return None;
    }
    match info.status {
        TermStatus::Stalled => Some(Notice::Stalled),
        TermStatus::WaitingInput => Some(Notice::WaitingInput),
        TermStatus::Exited(code) if code != SIGINT_EXIT => Some(Notice::Failed(code)),
        TermStatus::Dead => Some(Notice::Stopped),
        TermStatus::Idle if matches!(prev, TermStatus::Running | TermStatus::Stalled | TermStatus::WaitingInput) => match (info.last_exit, info.last_duration_secs) {
            (Some(0), Some(secs)) if secs >= LONG_COMMAND_SECS => Some(Notice::Finished { secs }),
            _ => None,
        },
        _ => None,
    }
}

pub fn message(notice: &Notice, card: &Card) -> (String, String) {
    let title = card.title();
    match notice {
        Notice::Stalled => (t().notice_stalled.into(), (t().notice_stalled_body)(&title, card.stall_secs)),
        Notice::WaitingInput => {
            let line = card.info.as_ref().and_then(|i| i.tail.last().cloned()).unwrap_or_default();
            (t().notice_waiting.into(), format!("{title}: {line}"))
        }
        Notice::Failed(code) => ((t().notice_failed)(*code), title),
        Notice::Finished { secs } => ((t().notice_finished)(&human_duration(*secs)), title),
        Notice::Stopped => (t().notice_stopped.into(), title),
        Notice::Moved(Some(group)) => ((t().notice_moved)(group), (t().notice_moved_body)(&title)),
        Notice::Moved(None) => (t().notice_left_group.into(), (t().notice_left_group_body)(&title)),
        Notice::Copied => (t().notice_copied.into(), t().notice_copied_body.into()),
    }
}

pub fn human_duration(secs: u64) -> String {
    match secs {
        0..=59 => (t().duration_secs)(secs),
        60..=3599 => (t().duration_mins)(secs / 60, secs % 60),
        _ => (t().duration_hours)(secs / 3600, (secs % 3600) / 60),
    }
}

struct Toast {
    card: CardId,
    notice: Notice,
    title: String,
    body: String,
    created: Option<f64>,
}

pub struct Notifications {
    prev: HashMap<String, TermStatus>,
    toasts: Vec<Toast>,
    os: os::OsToaster,
    clicked: Receiver<CardId>,
}

impl Notifications {
    pub fn new(ctx: egui::Context, os_toasts: bool) -> Self {
        let (tx, clicked) = mpsc::channel();
        Self { prev: HashMap::new(), toasts: Vec::new(), os: os::OsToaster::new(ctx, tx, os_toasts), clicked }
    }

    /// Clics sur les toasts Windows : on revient dans l'application, sur le bon terminal.
    pub fn poll(&mut self, ctx: &egui::Context, events: &mut Vec<Event>) {
        while let Ok(id) = self.clicked.try_recv() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            events.push(Event::FlyTo(id));
        }
    }

    pub fn pending(&self) -> usize {
        self.toasts.len()
    }

    pub fn handle(&mut self, ev: &Event, world: &mut World, out: &mut Vec<Event>) {
        match ev {
            Event::Agent(AgentEvent::Session(info)) => {
                let prev = self.prev.insert(info.session.clone(), info.status);
                if let (Some(n), Some(card)) = (decide(prev, info), world.card_by_session(&info.session)) {
                    out.push(Event::Notify(card.id.clone(), n));
                }
            }
            Event::Agent(AgentEvent::Gone { session }) => {
                self.prev.remove(session);
            }
            Event::PtyExited(id) => {
                let already = world.card(id).is_some_and(|c| self.prev.get(&c.session()) == Some(&TermStatus::Dead));
                if !already {
                    out.push(Event::Notify(id.clone(), Notice::Stopped));
                }
            }
            Event::Notify(id, notice) => self.notify(world, id, notice),
            Event::FocusCard(id) | Event::KillCard(id) | Event::RemoveCard(id) => self.toasts.retain(|t| &t.card != id),
            _ => {}
        }
    }

    fn notify(&mut self, world: &mut World, id: &CardId, notice: &Notice) {
        let window_focused = world.window_focused;
        let watching = window_focused && world.focused.as_ref() == Some(id);
        let Some(card) = world.card_mut(id) else { return };
        // Un déplacement ou une copie : simple information, ni bordure d'alerte, ni toast Windows.
        let info_only = matches!(notice, Notice::Moved(_) | Notice::Copied);
        if watching && !info_only {
            return;
        }
        card.attention |= !info_only;
        let (title, body) = message(notice, card);
        if !window_focused && !info_only {
            self.os.show(&title, &body, id);
        }
        self.toasts.retain(|t| !(t.card == *id && t.notice == *notice));
        self.toasts.push(Toast { card: id.clone(), notice: notice.clone(), title, body, created: None });
        if self.toasts.len() > MAX_TOASTS {
            self.toasts.remove(0);
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, world: &World, events: &mut Vec<Event>) {
        let now = ctx.input(|i| i.time);
        self.toasts.retain(|t| t.created.is_none_or(|c| now - c < if t.notice == Notice::Copied { COPIED_SECS } else { TOAST_SECS }));
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        let mut dismiss = None;
        egui::Area::new(Id::unique("toasts")).anchor(Align2::LEFT_BOTTOM, [12.0, -12.0]).show(ctx, |ui| {
            for (k, t) in self.toasts.iter_mut().enumerate().rev() {
                t.created.get_or_insert(now);
                let status = world.card(&t.card).and_then(|c| c.status());
                let frame = egui::Frame::popup(ui.style()).stroke(egui::Stroke::new(1.5, theme::status_color(status)));
                let resp = frame
                    .show(ui, |ui| {
                        ui.set_width(300.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&t.title).strong().color(theme::status_color(status)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("×").clicked() {
                                    dismiss = Some(k);
                                }
                            });
                        });
                        ui.label(RichText::new(&t.body).color(theme::TEXT));
                    })
                    .response
                    .interact(Sense::click());
                if resp.clicked() && dismiss != Some(k) {
                    events.push(Event::FlyTo(t.card.clone()));
                }
                ui.add_space(6.0);
            }
        });
        if let Some(k) = dismiss {
            self.toasts.remove(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect, Vec2};

    fn info(status: TermStatus, exit: Option<i32>, secs: Option<u64>) -> SessionInfo {
        SessionInfo { session: "tc_a".into(), status, command: None, cwd: None, last_exit: exit, last_duration_secs: secs, tail: vec!["Continuer ? [y/N]".into()] }
    }

    #[test]
    fn first_observation_is_silent() {
        assert_eq!(decide(None, &info(TermStatus::Stalled, None, None)), None);
    }

    #[test]
    fn transitions() {
        use TermStatus::*;
        assert_eq!(decide(Some(Running), &info(Stalled, None, None)), Some(Notice::Stalled));
        assert_eq!(decide(Some(Stalled), &info(Stalled, None, None)), None);
        assert_eq!(decide(Some(Running), &info(WaitingInput, None, None)), Some(Notice::WaitingInput));
        assert_eq!(decide(Some(Running), &info(Exited(2), Some(2), Some(1))), Some(Notice::Failed(2)));
        assert_eq!(decide(Some(Running), &info(Exited(130), Some(130), Some(1))), None);
        assert_eq!(decide(Some(Idle), &info(Dead, None, None)), Some(Notice::Stopped));
        assert_eq!(decide(Some(Running), &info(Idle, Some(0), Some(45))), Some(Notice::Finished { secs: 45 }));
        assert_eq!(decide(Some(Running), &info(Idle, Some(0), Some(3))), None);
        assert_eq!(decide(Some(Exited(1)), &info(Idle, Some(0), Some(300))), None);
    }

    fn world_with_card() -> (World, CardId) {
        let mut w = World::default();
        let card = Card::new(Rect::from_min_size(Pos2::ZERO, Vec2::splat(50.0)), None);
        let id = card.id.clone();
        w.add_card(card);
        (w, id)
    }

    #[test]
    fn notify_marks_attention_and_dedupes() {
        let (mut w, id) = world_with_card();
        w.window_focused = true;
        let mut n = Notifications::new(egui::Context::default(), false);
        n.handle(&Event::Notify(id.clone(), Notice::Stalled), &mut w, &mut vec![]);
        n.handle(&Event::Notify(id.clone(), Notice::Stalled), &mut w, &mut vec![]);
        assert!(w.cards[0].attention);
        assert_eq!(n.pending(), 1);
        n.handle(&Event::FocusCard(id), &mut w, &mut vec![]);
        assert_eq!(n.pending(), 0);
    }

    #[test]
    fn nothing_when_user_is_watching_that_terminal() {
        let (mut w, id) = world_with_card();
        w.window_focused = true;
        w.focused = Some(id.clone());
        let mut n = Notifications::new(egui::Context::default(), false);
        n.handle(&Event::Notify(id, Notice::Failed(1)), &mut w, &mut vec![]);
        assert!(!w.cards[0].attention);
        assert_eq!(n.pending(), 0);
    }

    #[test]
    fn agent_events_flow_into_notices() {
        let (mut w, id) = world_with_card();
        let session = w.cards[0].session();
        let mut n = Notifications::new(egui::Context::default(), false);
        let mut out = vec![];
        let ev = |status| Event::Agent(AgentEvent::Session(SessionInfo { session: session.clone(), ..info(status, None, None) }));
        n.handle(&ev(TermStatus::Running), &mut w, &mut out);
        n.handle(&ev(TermStatus::Stalled), &mut w, &mut out);
        assert!(matches!(out.as_slice(), [Event::Notify(c, Notice::Stalled)] if *c == id));
        // L'arrêt du PTY après un statut Dead déjà signalé ne notifie pas deux fois.
        out.clear();
        n.handle(&ev(TermStatus::Dead), &mut w, &mut out);
        n.handle(&Event::PtyExited(id), &mut w, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn messages_and_durations() {
        crate::shell::i18n::set(crate::shell::i18n::Lang::Fr);
        assert_eq!(human_duration(42), "42 s");
        assert_eq!(human_duration(125), "2 min 05 s");
        assert_eq!(human_duration(7300), "2 h 01 min");
        let (w, _) = world_with_card();
        let mut card = w.cards[0].clone();
        card.info = Some(info(TermStatus::WaitingInput, None, None));
        let (title, body) = message(&Notice::WaitingInput, &card);
        assert!(title.contains("réponse") && body.ends_with("[y/N]"));
    }

    #[test]
    fn a_copy_is_confirmed_without_alarm_even_while_watching() {
        let (mut w, id) = world_with_card();
        w.window_focused = true;
        w.focused = Some(id.clone());
        let mut n = Notifications::new(egui::Context::default(), false);
        n.handle(&Event::Notify(id.clone(), Notice::Copied), &mut w, &mut vec![]);
        n.handle(&Event::Notify(id, Notice::Copied), &mut w, &mut vec![]);
        assert_eq!(n.pending(), 1);
        assert!(!w.cards[0].attention);
    }
}
