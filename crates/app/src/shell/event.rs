//! Bus d'événements : seul moyen pour une slice d'agir sur une autre.

use egui::{Pos2, Rect};
use protocol::AgentEvent;

use super::world::{CardId, ProjectId};

#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    Stalled,
    WaitingInput,
    Failed(i32),
    Finished { secs: u64 },
    Stopped,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// Nouveau terminal ; `at` en coordonnées monde (sinon au centre de la vue).
    NewCard { at: Option<Pos2>, project: Option<ProjectId> },
    FocusCard(CardId),
    Unfocus,
    /// Déplace la caméra en douceur jusqu'à la carte puis lui donne le focus.
    FlyTo(CardId),
    /// Ouvre l'espace d'un projet (seul visible), ou « Tous » avec `None`.
    EnterSpace(Option<ProjectId>),
    /// Cadre la caméra sur une zone du monde.
    FitRect(Rect),
    RenameCard(CardId, Option<String>),
    SetStall(CardId, u64),
    /// Demande confirmation avant de fermer le terminal.
    RequestClose(CardId),
    /// Tue la session tmux (le shell et tous ses processus) et retire la carte.
    KillCard(CardId),
    /// Rattache (ou recrée) la session d'une carte dont le client tmux s'est arrêté.
    RespawnCard(CardId),
    /// Retire la carte sans toucher à la session tmux.
    RemoveCard(CardId),
    NewProject { at: Option<Pos2> },
    RenameProject(ProjectId, String),
    SetProjectCwd(ProjectId, Option<String>),
    SetProjectColor(ProjectId, [u8; 3]),
    /// Supprime le cadre ; les terminaux restent.
    RemoveProject(ProjectId),
    /// Demande confirmation avant de fermer le groupe et tous ses terminaux.
    RequestCloseProject(ProjectId),
    AddWire(CardId, CardId),
    RemoveWire(CardId, CardId),
    Agent(AgentEvent),
    /// Le client tmux d'une carte s'est arrêté sans qu'on l'ait demandé.
    PtyExited(CardId),
    Notify(CardId, Notice),
}
