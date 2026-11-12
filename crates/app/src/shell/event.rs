//! Event bus: the only way for one slice to act on another.

use egui::{Pos2, Rect};
use protocol::{AgentCmd, AgentEvent};

use super::world::{Camera, CardId, CommandTemplate, ProjectId};

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
}

#[derive(Clone, Debug)]
pub enum Event {
    /// New terminal; `at` in world coordinates (otherwise at the view center). `command`: a
    /// command line its shell runs once, in its folder, when its session is created (`None`:
    /// just the shell).
    NewCard {
        at: Option<Pos2>,
        project: Option<ProjectId>,
        command: Option<String>,
    },
    /// Replaces a group's command templates.
    SetProjectTemplates(ProjectId, Vec<CommandTemplate>),
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
