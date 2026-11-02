//! Bus d'événements : seul moyen pour une slice d'agir sur une autre.

use egui::{Pos2, Rect};
use protocol::{AgentCmd, AgentEvent};

use super::world::{Camera, CardId, ProjectId};

#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    Stalled,
    WaitingInput,
    Failed(i32),
    Finished { secs: u64 },
    Stopped,
    /// Le terminal a été déplacé vers ce groupe (`None` : hors groupe). Simple information.
    Moved(Option<String>),
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
    /// Ramène la caméra (en vol) à une vue précise, par exemple celle d'avant un filtre.
    FlyCamera(Camera),
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
    /// Change le groupe d'un terminal (`None` : hors groupe). `relocate` : le poser à côté des
    /// membres du groupe cible (menu « Déplacer vers »), sinon il reste où on l'a lâché.
    AssignCard { card: CardId, project: Option<ProjectId>, relocate: bool },
    /// Le terminal vient d'être posé ou redimensionné : il garde sa place, ses voisins du groupe s'écartent.
    SettleCard(CardId),
    AddWire(CardId, CardId),
    RemoveWire(CardId, CardId),
    Agent(AgentEvent),
    /// Ouvre ou ferme le panneau « Changements ».
    ToggleChanges,
    /// Commande à transmettre à l'agent.
    ToAgent(AgentCmd),
    /// Le client tmux d'une carte s'est arrêté sans qu'on l'ait demandé.
    PtyExited(CardId),
    Notify(CardId, Notice),
}
