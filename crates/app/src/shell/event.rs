//! Event bus: the only way for one slice to act on another.

use egui::{Pos2, Rect};
use protocol::{AgentCmd, AgentEvent, LocalBranch, MergeStrategy, Problem, TaskStatus, TermStatus, WorktreeBranch};

use super::world::{Camera, CardId, CommandTemplate, ProjectId, QuickMessage, TaskSettings};

/// The task a card is started for, as its staged file hands it to the command: never spliced
/// into a command line, only written to private files the shell reads into variables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskEnv {
    pub reference: String,
    /// One line.
    pub title: String,
    /// The whole prompt (`features::tasks::start::build_prompt`).
    pub prompt: String,
}

/// A card started for a task: its id is chosen up front (the task records its session).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskCard {
    pub card: CardId,
    pub project: ProjectId,
    /// Its folder: the task's worktree, or the group's folder outside git.
    pub cwd: String,
    /// The group's task command, its placeholders expanded (`features::tasks::start::expand_command`).
    pub command: String,
    /// A worktree recipe's setup command, accepted: run first, the command only if it succeeded.
    pub setup: Option<String>,
    pub task: TaskEnv,
}

/// The worktree a task's start would create (the `tasks` slice asks, the `worktrees` slice
/// answers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskWorktreeSpec {
    pub project: ProjectId,
    /// The task's id.
    pub task: String,
    pub branch: WorktreeBranch,
    /// Its folder name (`protocol::folder_for_branch`).
    pub folder: String,
    /// The group's location template (`None`: the default one).
    pub location: Option<String>,
}

/// What a repository's recipe will do to a new worktree, as the start's confirmation shows it
/// (translated by the `worktrees` slice).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeSummary {
    /// The recipe's file and its problem, if any, then one line per pattern: (text, refused).
    pub lines: Vec<(String, bool)>,
    /// The setup command, every character shown for what it is.
    pub setup: Option<String>,
    /// The user accepted that very setup command for this repository before.
    pub setup_accepted: bool,
}

/// The `worktrees` slice's answer to `Event::PlanTaskWorktree`, with its checks of that very
/// request (the same as the new-worktree dialog's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskWorktreePlan {
    pub spec: TaskWorktreeSpec,
    /// The repository's local branches, sorted, with where each is checked out.
    pub branches: Vec<LocalBranch>,
    pub default_branch: Option<String>,
    /// The folder the worktree would get.
    pub path: Option<String>,
    /// When the folder is taken: the first free one.
    pub free_folder: Option<String>,
    /// The folder is not ignored by git: the pattern that would be added to `info/exclude`.
    pub exclude: Option<String>,
    /// What is wrong with the request (empty: it can be created).
    pub problems: Vec<Problem>,
    pub recipe: Option<RecipeSummary>,
}

/// A worktree to create for a task, through the same creation as the new-worktree dialog: once it
/// went through (and the recipe's copies and links are done), its card opens in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskWorktreeOrder {
    pub spec: TaskWorktreeSpec,
    /// Add the folder to `info/exclude` (ticked by the user).
    pub exclude: bool,
    /// Run the recipe's setup command in the card first (ticked by the user).
    pub run_setup: bool,
    /// The card to open (its `cwd` is the new worktree's folder once it is there).
    pub card: TaskCard,
}

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

/// Exit code of a Ctrl+C: not a failure worth reporting.
pub const SIGINT_EXIT: i32 = 130;

/// The alert a status calls for, whatever came before: stalled, waiting for an answer, failed
/// (not a Ctrl+C), stopped. `None` for a status that needs nobody.
pub fn alert(status: TermStatus) -> Option<Notice> {
    match status {
        TermStatus::Stalled => Some(Notice::Stalled),
        TermStatus::WaitingInput => Some(Notice::WaitingInput),
        TermStatus::Exited(code) if code != SIGINT_EXIT => Some(Notice::Failed(code)),
        TermStatus::Dead => Some(Notice::Stopped),
        _ => None,
    }
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
    /// just the shell). `setup`: run by its shell first, the command only if it succeeded (a
    /// worktree recipe's, accepted by the user). `cwd`: its folder (`None`: the group's linked
    /// folder).
    NewCard {
        at: Option<Pos2>,
        project: Option<ProjectId>,
        command: Option<String>,
        setup: Option<String>,
        cwd: Option<String>,
    },
    /// A card started for a task, in a folder that exists (a worktree reused, a folder outside
    /// git), or once its worktree was created.
    NewTaskCard(Box<TaskCard>),
    /// Asks what creating a task's worktree would give (`worktrees` slice); answered by
    /// `TaskWorktreePlanned`.
    PlanTaskWorktree(Box<TaskWorktreeSpec>),
    TaskWorktreePlanned(Box<TaskWorktreePlan>),
    /// Creates a worktree for a task, then opens its card (`worktrees` slice); answered by
    /// `TaskWorktreeCreated` or `TaskWorktreeFailed`.
    CreateTaskWorktree(Box<TaskWorktreeOrder>),
    TaskWorktreeCreated {
        project: ProjectId,
        task: String,
        path: String,
        branch: String,
    },
    TaskWorktreeFailed {
        project: ProjectId,
        task: String,
        problem: Problem,
    },
    /// Makes the group's template `n` the one that starts its tasks (`projects` slice).
    UseTaskTemplate(ProjectId, usize),
    /// Adds a command template to a group, as the one that starts its tasks.
    AddTaskTemplate(ProjectId, CommandTemplate),
    /// Replaces how a group starts its tasks.
    SetProjectTaskSettings(ProjectId, TaskSettings),
    /// Sets where a group's new worktrees go (`None`: the default location).
    SetProjectWorktreeLocation(ProjectId, Option<String>),
    /// Remembers how the group's branches are merged from the review (`projects` slice).
    SetProjectMergeStrategy(ProjectId, MergeStrategy),
    /// Opens the group's cleanup window on one of its repository's worktrees, with its removal's
    /// confirmation (and its branch's, with `delete_branch`) when the cleanup's own rule allows it;
    /// otherwise the window says why it is kept (`worktrees` slice).
    RemoveWorktree {
        project: ProjectId,
        path: String,
        delete_branch: bool,
    },
    /// Moves a task of the group's list to `status`, refused if it is no longer `from` (`tasks`
    /// slice: a normal change of the list, by the user's click).
    SetTaskStatus {
        project: ProjectId,
        task: String,
        status: TaskStatus,
        from: TaskStatus,
    },
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
    /// Opens or closes the "Tasks" panel.
    ToggleTasks,
    /// Turns the "free grid" option on or off (inside a group: the group is the whole canvas).
    ToggleFreeGrid,
    /// Opens the group's space and its "Tasks" panel, on this task (its id).
    ShowTask {
        project: ProjectId,
        task: String,
    },
    /// Opens the "Changes" panel on this group.
    ShowChanges(ProjectId),
    /// Opens the "Changes" panel of a group in review mode on one of its repository's worktrees
    /// (`path`: its folder, which may differ from the group's), against the default branch
    /// (`changes` slice).
    ReviewWorktree {
        project: ProjectId,
        path: String,
    },
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
