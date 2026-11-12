//! Event bus: the only way for one slice to act on another.

use egui::{Pos2, Rect};
use protocol::{AgentCmd, AgentEvent};

use super::world::{Camera, CardId, CommandTemplate, ProjectId, QuickMessage};

#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    Stalled,
    WaitingInput,
    Failed(i32),
    Finished {
        secs: u64,
    },
    Stopped,
    /// The terminal was moved to this group (`None`: outside any group). Information only.
    Moved(Option<String>),
    /// Text copied to the clipboard from this terminal (tmux selection).
    Copied,
    /// Text sent to this terminal by the app did not arrive, for this reason (kept on the card).
    NotSent(SendRefusal),
    /// Text sent to this terminal by the app was typed in but not submitted, for this reason.
    NotSubmitted(SendRefusal),
}

/// Why a card did not take the text sent to it (`Event::SendText`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendRefusal {
    /// No such card (closed meanwhile).
    NoCard,
    /// Its session is not up yet: terminal still starting, its shell not shown yet, or its start
    /// command not taken yet.
    Starting,
    /// Its terminal could not start.
    NotStarted,
    /// Its session ended (or its tmux client stopped).
    Ended,
    /// The agent, which delivers text through tmux, is not connected (or stopped meanwhile).
    NoAgent,
    /// The agent is too old to deliver text.
    AgentTooOld,
    /// The agent did not answer in time: the text may or may not have arrived.
    NoAnswer,
    /// tmux knows no such session.
    NoSession,
    /// Its pane shows a tmux screen (tmux's mode name: `clock-mode`, `tree-mode`...) that would
    /// take the keys.
    Busy(String),
    /// The agent could not get to it within its time limit: never sent.
    Expired,
    /// tmux refused it (its message).
    Failed(String),
}

/// What became of an `Event::SendText`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// tmux pasted it into the card's session (and pressed Enter, when asked).
    Sent,
    /// Empty text: nothing to send, nothing sent.
    Empty,
    /// Not sent: the card says why, with the text kept for copying.
    Refused(SendRefusal),
    /// Pasted, but the Enter could not be pressed, for this reason: the text waits in the input
    /// line (not offered again: sending it again would type it twice).
    PastedNotSubmitted(SendRefusal),
}

#[derive(Clone, Debug)]
pub enum Event {
    /// New terminal; `at` in world coordinates (otherwise at the view center). `command`: a
    /// command line its shell runs once, in its folder, when its session is created (`None`:
    /// just the shell). `cwd`: its folder (`None`: the group's linked folder).
    NewCard {
        at: Option<Pos2>,
        project: Option<ProjectId>,
        command: Option<String>,
        cwd: Option<String>,
    },
    /// Sets where a group's new worktrees go (`None`: the default location).
    SetProjectWorktreeLocation(ProjectId, Option<String>),
    /// Opens the new-worktree dialog of a group.
    NewWorktree(ProjectId),
    /// A toast about nothing on the canvas (texts already translated).
    Inform {
        title: String,
        body: String,
    },
    /// Replaces a group's command templates.
    SetProjectTemplates(ProjectId, Vec<CommandTemplate>),
    /// Replaces a group's quick messages.
    SetProjectMessages(ProjectId, Vec<QuickMessage>),
    /// Opens the editor of a group's quick messages.
    EditQuickMessages(ProjectId),
    /// Sends text to this card's session, and to no other: one paste through tmux (bracketed when
    /// the application asked for it), then Enter as its own key when `enter` is set. The only way
    /// for a slice to type into a terminal; answered by `TextSendOutcome`, once tmux took it.
    SendText {
        card: CardId,
        text: String,
        enter: bool,
    },
    /// What became of a `SendText` to this card.
    TextSendOutcome {
        card: CardId,
        outcome: SendOutcome,
    },
    FocusCard(CardId),
    Unfocus,
    /// Smoothly moves the camera to the card, then focuses it.
    FlyTo(CardId),
    /// Opens a project's space (the only one visible), or "All" with `None`.
    EnterSpace(Option<ProjectId>),
    /// Frames the camera on a world area.
    FitRect(Rect),
    /// Like `FitRect`, but only if the area isn't already fully visible.
    RevealRect(Rect),
    /// Flies the camera back to a given view, e.g. the one from before a filter.
    FlyCamera(Camera),
    RenameCard(CardId, Option<String>),
    SetStall(CardId, u64),
    /// Asks for confirmation before closing the terminal.
    RequestClose(CardId),
    /// Kills the tmux session (the shell and all its processes) and removes the card.
    KillCard(CardId),
    /// Reattaches (or recreates) the session of a card whose tmux client stopped.
    RespawnCard(CardId),
    /// Removes the card without touching the tmux session.
    RemoveCard(CardId),
    NewProject {
        at: Option<Pos2>,
    },
    RenameProject(ProjectId, String),
    SetProjectCwd(ProjectId, Option<String>),
    SetProjectColor(ProjectId, [u8; 3]),
    /// Moves a group to this position in the top bar order (and its Ctrl+Shift+<n> shortcut).
    MoveProject(ProjectId, usize),
    /// Sorts the groups by name (A to Z, ignoring case), once: they can still be dragged after.
    SortProjects,
    /// Deletes the frame; the terminals remain.
    RemoveProject(ProjectId),
    /// Asks for confirmation before closing the group and all its terminals.
    RequestCloseProject(ProjectId),
    /// Changes a terminal's group (`None`: outside any group). `relocate`: place it next to the
    /// target group's members ("Move to" menu), otherwise it stays where it was dropped.
    AssignCard {
        card: CardId,
        project: Option<ProjectId>,
        relocate: bool,
    },
    /// The terminal was just placed or resized: it keeps its spot, its group neighbors move aside.
    SettleCard(CardId),
    /// Links two groups (undirected, at most one wire per pair).
    AddWire(ProjectId, ProjectId),
    RemoveWire(ProjectId, ProjectId),
    Agent(AgentEvent),
    /// Opens or closes the "Changes" panel.
    ToggleChanges,
    /// Turns the "free grid" option on or off (inside a group: the group is the whole canvas).
    ToggleFreeGrid,
    /// Opens the "Changes" panel on this group.
    ShowChanges(ProjectId),
    /// Command to forward to the agent.
    ToAgent(AgentCmd),
    /// The link to the agent closed: whatever it was asked and has not answered never will be.
    AgentLost,
    /// A card's tmux client stopped without being asked to.
    PtyExited(CardId),
    Notify(CardId, Notice),
    /// A newer release is out (version without the `v`): warn once.
    UpdateAvailable(String),
    /// Opens the release notes of the pending update, with its "Update" button.
    ShowReleaseNotes,
    /// Opens the "Keyboard shortcuts" window.
    ShowShortcuts,
}
