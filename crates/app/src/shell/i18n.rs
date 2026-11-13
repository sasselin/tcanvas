//! UI texts, in French or English depending on the Windows language.
//!
//! Each text is a field of [`Texts`]: forgetting one in a language doesn't compile.
//! Parameterized texts are `fn`s (non-capturing closures).

use std::cell::Cell;

use protocol::{Problem, TermStatus};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Fr,
}

/// `fr`, `fr-CA`, `fr_FR.UTF-8` → French; any other language → English.
pub fn from_locale(tag: &str) -> Lang {
    let primary = tag.trim().split(['-', '_', '.', '@']).next().unwrap_or_default();
    if primary.eq_ignore_ascii_case("fr") { Lang::Fr } else { Lang::En }
}

/// `TCANVAS_LANG` (`fr` / `en`) wins; otherwise the Windows user's language (or `LANG` on Linux).
pub fn detect() -> Lang {
    let forced = std::env::var("TCANVAS_LANG").ok().filter(|v| !v.trim().is_empty());
    from_locale(&forced.or_else(sys_locale::get_locale).unwrap_or_default())
}

thread_local! {
    // Per thread: the UI runs on a single thread, and each test can pick its language.
    static LANG: Cell<Option<Lang>> = const { Cell::new(None) };
}

pub fn lang() -> Lang {
    LANG.with(|l| {
        l.get().unwrap_or_else(|| {
            let detected = detect();
            l.set(Some(detected));
            detected
        })
    })
}

pub fn set(lang: Lang) {
    LANG.with(|l| l.set(Some(lang)));
}

pub fn t() -> &'static Texts {
    texts(lang())
}

pub fn texts(lang: Lang) -> &'static Texts {
    match lang {
        Lang::En => &EN,
        Lang::Fr => &FR,
    }
}

impl Texts {
    pub fn status(&self, s: TermStatus) -> &'static str {
        match s {
            TermStatus::Idle => self.status_idle,
            TermStatus::Running => self.status_running,
            TermStatus::Stalled => self.status_stalled,
            TermStatus::WaitingInput => self.status_waiting,
            TermStatus::Exited(_) => self.status_failed,
            TermStatus::Dead => self.status_dead,
        }
    }

    /// Why the agent could not start, with what to install on this platform.
    pub fn agent_missing_help(&self, why: &str) -> String {
        self.agent_missing_help_for(cfg!(windows), why)
    }

    fn agent_missing_help_for(&self, windows: bool, why: &str) -> String {
        if windows { (self.agent_missing_hint)(why) } else { (self.agent_missing_hint_linux)(why) }
    }

    /// Placeholder of the folder picker's path field.
    pub fn folder_path_hint(&self) -> &'static str {
        self.folder_path_hint_for(cfg!(windows))
    }

    fn folder_path_hint_for(&self, windows: bool) -> &'static str {
        if windows { self.wsl_path_hint } else { self.path_hint }
    }

    pub fn problem(&self, p: &Problem) -> String {
        match p {
            Problem::NotADir { path } => (self.not_a_dir)(path),
            Problem::DirNotFound { path } => (self.dir_not_found)(path),
            Problem::Unreadable { detail } => (self.unreadable)(detail),
            Problem::CommitRefused => self.commit_refused.into(),
            Problem::CommitNotFound => self.commit_not_found.into(),
            Problem::PathRefused => self.path_refused.into(),
            Problem::NotAGitRepo => self.not_a_git_repo.into(),
            Problem::NoDiff => self.no_diff.into(),
            Problem::TmuxMissing { detail } => (self.tmux_missing)(detail),
            Problem::HooksFailed { detail } => (self.hooks_failed)(detail),
        }
    }
}

pub struct Texts {
    // Terminal statuses.
    pub status_idle: &'static str,
    pub status_running: &'static str,
    pub status_stalled: &'static str,
    pub status_waiting: &'static str,
    pub status_failed: &'static str,
    pub status_dead: &'static str,

    // Problems reported by the agent.
    pub not_a_dir: fn(&str) -> String,
    pub dir_not_found: fn(&str) -> String,
    pub unreadable: fn(&str) -> String,
    pub commit_refused: &'static str,
    pub commit_not_found: &'static str,
    pub path_refused: &'static str,
    pub not_a_git_repo: &'static str,
    pub no_diff: &'static str,
    pub tmux_missing: fn(&str) -> String,

    // Agent link.
    pub agent_starting: &'static str,
    pub agent_connected: &'static str,
    pub agent_connected_hint: &'static str,
    pub hooks_missing: &'static str,
    pub hooks_missing_hint: &'static str,
    pub install_hooks: &'static str,
    pub hooks_failed: fn(&str) -> String,
    pub agent_missing: &'static str,
    /// Windows: the agent runs in WSL.
    pub agent_missing_hint: fn(&str) -> String,
    /// Native Linux: the agent sits next to the app.
    pub agent_missing_hint_linux: fn(&str) -> String,
    pub retry: &'static str,
    pub agent_launch_failed: fn(&str) -> String,
    pub agent_stopped: &'static str,
    pub agent_install_failed: fn(&str) -> String,
    pub tmux_missing_label: &'static str,
    pub tmux_missing_hint: fn(&str) -> String,

    // Changes panel.
    pub just_now: &'static str,
    pub secs_ago: fn(u64) -> String,
    pub mins_ago: fn(u64) -> String,
    pub hours_ago: fn(u64) -> String,
    pub days_ago: fn(u64) -> String,
    pub changes_button: fn(usize) -> String,
    pub changes_button_hint: &'static str,
    pub close_panel: &'static str,
    pub follow: &'static str,
    pub follow_hint: &'static str,
    pub waiting_for_agent: &'static str,
    pub uncommitted: fn(usize) -> String,
    pub all_committed: &'static str,
    pub nothing_modified: &'static str,
    pub list_truncated: &'static str,
    pub recent_commits: &'static str,
    pub detached_head: &'static str,
    pub file_count: fn(usize) -> String,
    pub modified_file: fn(&str) -> String,
    pub commit_a11y: fn(&str) -> String,
    pub pick_a_file: &'static str,
    pub loading_diff: &'static str,
    pub diff_truncated: &'static str,

    // Toolbar and filters.
    pub new_terminal_button: &'static str,
    pub new_terminal_hint: &'static str,
    pub new_project_button: &'static str,
    pub filter_hint: &'static str,
    pub status_chip_hint: &'static str,
    pub clear_filter: &'static str,
    pub clear_filter_count: fn(usize, usize) -> String,
    pub clear_filter_hint: &'static str,
    pub fit_all: &'static str,
    pub fit_all_hint: &'static str,
    pub attention_hint: &'static str,
    pub empty_canvas: &'static str,
    pub layout_warning: &'static str,

    // Jump palette.
    pub jump_hint: &'static str,
    pub no_results: &'static str,
    pub project_shortcut: fn(usize) -> String,
    pub terminal_count: fn(usize) -> String,

    // Keyboard shortcuts window.
    pub shortcuts_title: &'static str,
    pub shortcuts_read_only: &'static str,
    pub shift_key: &'static str,
    pub mouse_click: &'static str,
    pub mouse_double_click: &'static str,
    pub mouse_right_click: &'static str,
    pub mouse_drag: &'static str,
    pub mouse_middle_drag: &'static str,
    pub mouse_wheel: &'static str,
    pub keys_global: &'static str,
    pub keys_terminal: &'static str,
    pub keys_canvas: &'static str,
    pub keys_cards: &'static str,
    pub key_jump: &'static str,
    pub key_new_card: &'static str,
    pub key_search: &'static str,
    pub key_fit_all: &'static str,
    pub key_project: &'static str,
    pub key_close_card: &'static str,
    pub key_help: &'static str,
    pub key_interrupt: &'static str,
    pub key_copy: &'static str,
    pub key_paste: &'static str,
    pub key_tmux_menu: &'static str,
    pub key_focus_card: &'static str,
    pub key_pan: &'static str,
    pub key_hand: &'static str,
    pub key_zoom: &'static str,
    pub key_new_card_here: &'static str,
    pub key_minimap: &'static str,
    pub key_move_card: &'static str,
    pub key_fly_to: &'static str,
    pub key_card_menu: &'static str,
    pub key_resize: &'static str,
    pub key_move_group: &'static str,
    pub key_link_groups: &'static str,
    pub key_rename_group: &'static str,
    pub key_delete_wire: &'static str,
    pub key_reorder_chips: &'static str,

    // Notifications.
    pub notice_stalled: &'static str,
    pub notice_stalled_body: fn(&str, u64) -> String,
    pub notice_waiting: &'static str,
    pub notice_failed: fn(i32) -> String,
    pub notice_finished: fn(&str) -> String,
    pub notice_stopped: &'static str,
    pub notice_moved: fn(&str) -> String,
    pub notice_moved_body: fn(&str) -> String,
    pub notice_left_group: &'static str,
    pub notice_left_group_body: fn(&str) -> String,
    pub notice_copied: &'static str,
    pub notice_copied_body: &'static str,
    pub duration_secs: fn(u64) -> String,
    pub duration_mins: fn(u64, u64) -> String,
    pub duration_hours: fn(u64, u64) -> String,

    // Layout saving.
    pub layout_read_failed: fn(&str, &str) -> String,
    pub layout_corrupt: fn(&str, &str) -> String,
    pub layout_save_failed: fn(&str) -> String,

    // Folder picker.
    pub home_folder: &'static str,
    pub parent_folder: &'static str,
    pub wsl_path_hint: &'static str,
    pub path_hint: &'static str,
    pub recent_folders: &'static str,
    pub filter_folders_hint: &'static str,
    pub loading: &'static str,
    pub no_subfolders: &'static str,
    pub no_matching_folder: &'static str,
    pub git_repo: &'static str,
    /// Footer of the picker: the folder is a linked worktree.
    pub git_worktree: &'static str,
    /// Marker of a linked worktree in the folder list.
    pub worktree_marker: &'static str,
    pub link_folder: &'static str,
    pub cancel: &'static str,
    pub unlink: &'static str,
    pub unlink_hint: &'static str,

    // Groups (projects).
    pub default_project_name: fn(usize) -> String,
    pub close_group_title: fn(&str) -> String,
    pub close_empty_group: &'static str,
    pub close_one_terminal: &'static str,
    pub close_n_terminals: fn(usize) -> String,
    pub running_command: fn(&str) -> String,
    pub close_group_footer: &'static str,
    pub link_a_folder: &'static str,
    pub folder_linked_hint: &'static str,
    pub folder_unlinked_hint: &'static str,
    pub group_folder_a11y: fn(&str) -> String,
    pub group_new_terminal_hint: &'static str,
    pub group_new_terminal_a11y: fn(&str) -> String,
    pub group_changes_hint: &'static str,
    pub group_changes_a11y: fn(&str) -> String,
    pub open_a_terminal: &'static str,
    pub group_header_hint: &'static str,
    pub close_group_hint: &'static str,
    pub close_group_a11y: fn(&str) -> String,
    pub drop_to_leave_group: &'static str,
    pub picker_title: fn(&str) -> String,
    pub new_terminal_here: &'static str,
    pub rename_group: &'static str,
    pub linked_folder_menu: &'static str,
    pub unlink_folder: &'static str,
    pub color: &'static str,
    pub close_group_menu: &'static str,
    pub remove_frame: &'static str,
    pub close_all: &'static str,

    // Spaces.
    pub all_spaces: &'static str,
    pub all_spaces_hint: &'static str,
    pub all_groups_menu_hint: &'static str,
    pub free_grid: &'static str,
    pub free_grid_hint: &'static str,
    pub open_project: &'static str,
    /// Button of a desktop notification (Linux) that brings the terminal back.
    pub show_terminal: &'static str,
    pub open_project_shortcut: fn(usize) -> String,
    pub sort_projects: &'static str,

    // Terminal cards.
    pub spawn_failed: fn(&str, &str) -> String,
    pub close_terminal_hint: &'static str,
    pub close_terminal_a11y: fn(&str) -> String,
    /// Last prompt sent to the coding agent, on one line in the card header.
    pub last_prompt: fn(&str) -> String,
    pub last_prompt_a11y: fn(&str) -> String,
    pub rename_menu: &'static str,
    pub stall_delay_menu: &'static str,
    pub center_and_zoom: &'static str,
    pub move_to: &'static str,
    pub no_group: &'static str,
    pub close_terminal_menu: &'static str,
    pub session_ended: &'static str,
    pub respawn: &'static str,
    pub remove: &'static str,
    pub close: &'static str,
    pub terminal_name_prompt: &'static str,
    pub stall_prompt: &'static str,
    pub close_card_title: fn(&str) -> String,
    pub close_card_running: fn(&str) -> String,
    pub close_card_body: &'static str,
    pub ok: &'static str,
    /// Branch badge of a card, for screen readers.
    pub branch_badge_a11y: fn(&str) -> String,

    // Worktrees of the open group's repository.
    pub worktrees_title: fn(usize) -> String,
    pub main_checkout: &'static str,
    pub worktree_dirty_hint: &'static str,
    pub ahead_behind_hint: fn(&str) -> String,
    pub merged: &'static str,
    pub merged_hint: fn(&str) -> String,
    pub locked: &'static str,
    pub prunable: &'static str,
    pub prunable_hint: &'static str,
    pub no_terminal_here: &'static str,
    pub last_commit: fn(&str) -> String,
    pub go_to_terminal: &'static str,

    // Links between terminals.
    pub wire_hint: fn(&str, &str) -> String,
    pub linked_group_hint: fn(&str) -> String,

    // Updates.
    pub version_hint: &'static str,
    pub update_button: fn(&str) -> String,
    pub update_button_hint: &'static str,
    pub update_installing: fn(&str) -> String,
    pub update_restarting: &'static str,
    pub update_failed: &'static str,
    pub update_failed_hint: fn(&str) -> String,
    pub update_notice_title: &'static str,
    pub update_notice_body: fn(&str) -> String,
    pub update_title: fn(&str) -> String,
    pub update_running: fn(&str) -> String,
    pub update_restart_note: &'static str,
    pub update_now: &'static str,
    pub update_later: &'static str,
    pub release_page: &'static str,
    pub whats_new_title: fn(&str) -> String,
    pub no_release_notes: &'static str,
    pub check_updates: &'static str,
    pub checking_updates: &'static str,
    pub up_to_date: &'static str,
    pub check_failed: &'static str,
}

pub const EN: Texts = Texts {
    status_idle: "idle",
    status_running: "active",
    status_stalled: "stalled",
    status_waiting: "waiting for input",
    status_failed: "failed",
    status_dead: "stopped",

    not_a_dir: |p| format!("“{p}” is not a folder"),
    dir_not_found: |p| format!("folder not found: {p}"),
    unreadable: |d| format!("cannot read folder: {d}"),
    commit_refused: "invalid commit",
    commit_not_found: "commit not found",
    path_refused: "invalid path",
    not_a_git_repo: "Not a git repository: initialize git in this folder to see diffs.",
    no_diff: "No difference from HEAD.",
    tmux_missing: |d| format!("tmux not found: {d}"),

    agent_starting: "○ agent…",
    agent_connected: "● agent",
    agent_connected_hint: "tcanvas-agent is watching the terminals in WSL",
    hooks_missing: "⚠ hooks",
    hooks_missing_hint: "Without the shell hooks (bash or zsh), no exit codes or command durations. Without the coding agent hooks (Claude Code, Codex, Gemini CLI, Copilot CLI, Qwen Code), an agent waiting for you is only noticed after a few quiet seconds.\nInstalling adds one line to ~/.bashrc or ~/.zshrc and a hook to each agent's settings (the original is kept as .tcanvas.bak); new terminals pick them up.",
    install_hooks: "Install",
    hooks_failed: |e| format!("cannot install the hooks: {e}"),
    agent_missing: "✖ agent missing",
    agent_missing_hint: |why| format!("{why}\n\ntcanvas needs WSL with a Linux distribution (wsl --install in PowerShell).\nRetrying automatically every 10 s."),
    agent_missing_hint_linux: |why| format!("{why}\n\ntcanvas needs tmux and tcanvas-agent (next to the tcanvas executable or in the PATH).\nRetrying automatically every 10 s."),
    retry: "Retry",
    agent_launch_failed: |e| format!("cannot launch: {e}"),
    agent_stopped: "the agent stopped",
    agent_install_failed: |e| format!("cannot install the agent in WSL: {e}"),
    tmux_missing_label: "✖ tmux missing",
    tmux_missing_hint: |d| format!("Terminals need tmux in WSL ({d}).\nInstall it in WSL:\n  sudo apt install tmux"),

    just_now: "just now",
    secs_ago: |n| format!("{n} s ago"),
    mins_ago: |n| format!("{n} min ago"),
    hours_ago: |n| format!("{n} h ago"),
    days_ago: |n| format!("{n} d ago"),
    changes_button: |n| format!("Δ Changes ({n})"),
    changes_button_hint: "Files changed in the linked folder and their diffs, live",
    close_panel: "Close the panel",
    follow: "Follow",
    follow_hint: "Automatically show the latest modified file or the latest commit",
    waiting_for_agent: "Waiting for the agent…",
    uncommitted: |n| format!("Uncommitted ({n})"),
    all_committed: "Nothing pending: everything is committed.",
    nothing_modified: "No files modified since tracking started.",
    list_truncated: "… list truncated (too many files)",
    recent_commits: "Recent commits",
    detached_head: "detached HEAD",
    file_count: |n| if n == 1 { "1 file".into() } else { format!("{n} files") },
    modified_file: |p| format!("Modified file {p}"),
    commit_a11y: |s| format!("Commit {s}"),
    pick_a_file: "Click a file or a commit to see its diff.",
    loading_diff: "Loading diff…",
    diff_truncated: "… diff truncated (file too large)",

    new_terminal_button: "+ Terminal",
    new_terminal_hint: "New terminal, in the open project · Ctrl+Shift+N · double-click the canvas",
    new_project_button: "+ Project",
    filter_hint: "Filter… (Ctrl+Shift+F, Enter: frame)",
    status_chip_hint: "Click: show only this status · Ctrl+click: combine",
    clear_filter: "× filter",
    clear_filter_count: |m, s| format!("× filter ({m}/{s})"),
    clear_filter_hint: "Remove the filter and go back to the previous view",
    fit_all: "Show all",
    fit_all_hint: "Ctrl+Shift+0",
    attention_hint: "Go to the next terminal that needs attention",
    empty_canvas: "Double-click to open a terminal · Ctrl+Shift+N",
    layout_warning: "⚠ layout",

    jump_hint: "Go to a terminal or project…",
    no_results: "No results",
    project_shortcut: |n| format!("   Ctrl+Shift+{n}"),
    terminal_count: |n| if n == 1 { "1 terminal".into() } else { format!("{n} terminals") },

    shortcuts_title: "Keyboard shortcuts",
    shortcuts_read_only: "Mouse gestures included. Not customizable yet.",
    shift_key: "Shift",
    mouse_click: "click",
    mouse_double_click: "double-click",
    mouse_right_click: "right-click",
    mouse_drag: "drag",
    mouse_middle_drag: "middle-drag",
    mouse_wheel: "wheel",
    keys_global: "Everywhere, even in a terminal",
    keys_terminal: "In a terminal",
    keys_canvas: "Canvas",
    keys_cards: "Terminals and groups",
    key_jump: "Go to a terminal or project",
    key_new_card: "New terminal, in the open project",
    key_search: "Filter terminals",
    key_fit_all: "Show everything",
    key_project: "Open project #1 to #9",
    key_close_card: "Close the focused terminal",
    key_help: "This window",
    key_interrupt: "Interrupt (^C), never copy",
    key_copy: "Copy the selection (also copied on mouse release)",
    key_paste: "Paste",
    key_tmux_menu: "tmux menu",
    key_focus_card: "Click a terminal: type in it",
    key_pan: "Move the view (empty canvas)",
    key_hand: "Move the view from anywhere, even over terminals",
    key_zoom: "Zoom around the pointer (or pinch)",
    key_new_card_here: "Empty canvas: new terminal here",
    key_minimap: "Minimap: go to that spot",
    key_move_card: "Title bar: move the terminal (drop it in a group)",
    key_fly_to: "Title bar: fly to the terminal",
    key_card_menu: "Title bar: terminal menu",
    key_resize: "Bottom right corner: resize",
    key_move_group: "Group label: move the group",
    key_link_groups: "Group label onto another group: link them",
    key_rename_group: "Group label: rename",
    key_delete_wire: "Link between groups: delete it",
    key_reorder_chips: "Group chips of the top bar: reorder",

    notice_stalled: "⏸ Terminal stalled",
    notice_stalled_body: |t, s| format!("{t}: no output for {s} s"),
    notice_waiting: "⌨ Waiting for an answer",
    notice_failed: |c| format!("✖ Failed (code {c})"),
    notice_finished: |d| format!("✔ Finished in {d}"),
    notice_stopped: "■ Terminal stopped",
    notice_moved: |g| format!("➜ Moved to “{g}”"),
    notice_moved_body: |t| format!("{t} · click: go there"),
    notice_left_group: "➜ Left the group",
    notice_left_group_body: |t| format!("{t} is in “All” · click: go there"),
    notice_copied: "⧉ Copied",
    notice_copied_body: "Selection copied to the clipboard",
    duration_secs: |s| format!("{s} s"),
    duration_mins: |m, s| format!("{m} min {s:02} s"),
    duration_hours: |h, m| format!("{h} h {m:02} min"),

    layout_read_failed: |p, e| format!("cannot read {p}: {e}"),
    layout_corrupt: |e, p| format!("unreadable layout ({e}), set aside in {p}"),
    layout_save_failed: |e| format!("cannot save: {e}"),

    home_folder: "Home folder",
    parent_folder: "Parent folder",
    wsl_path_hint: "WSL path, e.g. ~/code/api",
    path_hint: "Path, e.g. ~/code/api",
    recent_folders: "Recent:",
    filter_folders_hint: "Filter folders… (Enter: open the only match)",
    loading: "Loading…",
    no_subfolders: "No subfolders.",
    no_matching_folder: "No matching folder.",
    git_repo: "⎇ git repository",
    git_worktree: "⎇ git worktree",
    worktree_marker: "⎇ worktree",
    link_folder: "Link this folder",
    cancel: "Cancel",
    unlink: "Unlink",
    unlink_hint: "The group will no longer have a folder",

    default_project_name: |n| format!("Project {n}"),
    close_group_title: |n| format!("Close the group “{n}”?"),
    close_empty_group: "The group has no terminals. The frame will be removed.",
    close_one_terminal: "This terminal will be closed with all its processes:\n",
    close_n_terminals: |n| format!("These {n} terminals will be closed with all their processes:\n"),
    running_command: |c| format!("  —  “{c}” running"),
    close_group_footer: "\nThe frame will be removed. This cannot be undone.",
    link_a_folder: "📁 Link a folder",
    folder_linked_hint: "Linked folder: new terminals open there and its changes are tracked. Click: change",
    folder_unlinked_hint: "Link a folder: new terminals will open there and its changes will be tracked",
    group_folder_a11y: |n| format!("Folder of group {n}"),
    group_new_terminal_hint: "Open a terminal in this group (Ctrl+Shift+N when the group is open)",
    group_new_terminal_a11y: |n| format!("New terminal in group {n}"),
    group_changes_hint: "See modified files, recent commits and their diffs",
    group_changes_a11y: |n| format!("Changes of group {n}"),
    open_a_terminal: "+ Open a terminal",
    group_header_hint: "Double-click: rename · drag: move with its terminals · Alt + drag onto another group: link them",
    close_group_hint: "Close the group and all its terminals",
    close_group_a11y: |n| format!("Close group {n}"),
    drop_to_leave_group: "Release to take the terminal out of the group",
    picker_title: |n| format!("Folder linked to group “{n}”"),
    new_terminal_here: "New terminal here",
    rename_group: "Rename (double-click the title)",
    linked_folder_menu: "Linked folder…",
    unlink_folder: "Unlink the folder",
    color: "Color",
    close_group_menu: "Close the group and its terminals…",
    remove_frame: "Remove the frame (keep the terminals)",
    close_all: "Close all",

    all_spaces: "All",
    all_spaces_hint: "Overview: all projects",
    all_groups_menu_hint: "Groups out of view: open the list of all groups",
    free_grid: "Free grid",
    free_grid_hint: "Inside a group, use the whole canvas: no frame, terminals stay in the group wherever they are dropped",
    open_project: "Open this project · Drag: reorder · Right-click: sort",
    show_terminal: "Show",
    open_project_shortcut: |n| format!("Open this project · Ctrl+Shift+{n} · Drag: reorder · Right-click: sort"),
    sort_projects: "Sort groups A to Z",

    spawn_failed: |p, e| format!("Cannot launch {p}: {e}"),
    close_terminal_hint: "Close the terminal (Ctrl+Shift+W)",
    close_terminal_a11y: |t| format!("Close terminal {t}"),
    last_prompt: |p| format!("“{p}”"),
    last_prompt_a11y: |p| format!("Last prompt: {p}"),
    rename_menu: "Rename…",
    stall_delay_menu: "Delay before “stalled”…",
    center_and_zoom: "Center and zoom",
    move_to: "Move to",
    no_group: "No group",
    close_terminal_menu: "Close the terminal…  (Ctrl+Shift+W)",
    session_ended: "Session ended",
    respawn: "Restart",
    remove: "Remove",
    close: "Close",
    terminal_name_prompt: "Terminal name (empty: automatic)",
    stall_prompt: "Seconds without output before “stalled”",
    close_card_title: |t| format!("Close “{t}”?"),
    close_card_running: |c| format!("“{c}” is running and will be stopped.\nThe shell and all its processes will be closed. This cannot be undone."),
    close_card_body: "The shell and all its processes will be closed. This cannot be undone.",
    ok: "OK",
    branch_badge_a11y: |b| format!("Branch: {b}"),

    worktrees_title: |n| format!("⎇ {n} worktrees"),
    main_checkout: "main checkout",
    worktree_dirty_hint: "Uncommitted changes",
    ahead_behind_hint: |b| format!("Commits ahead of (↑) and behind (↓) {b}"),
    merged: "✔ merged",
    merged_hint: |b| format!("Every commit of this branch is in {b}"),
    locked: "locked",
    prunable: "missing",
    prunable_hint: "Its folder no longer exists",
    no_terminal_here: "no terminal",
    last_commit: |age| format!("Last commit {age}"),
    go_to_terminal: "Go to this terminal",

    wire_hint: |a, b| format!("{a}  ⇄  {b}\nclick: go to the other group · right-click: delete"),
    linked_group_hint: |name| format!("Linked group: open “{name}”"),

    version_hint: "What's new, and every earlier version",
    update_button: |v| format!("Update to v{v}"),
    update_button_hint: "See what's new, then update",
    update_installing: |v| format!("Downloading v{v}…"),
    update_restarting: "Restarting…",
    update_failed: "Update failed",
    update_failed_hint: |e| format!("{e}\nClick to open the release page"),
    update_notice_title: "Update available",
    update_notice_body: |v| format!("tcanvas v{v} is out: click to see what's new"),
    update_title: |v| format!("tcanvas v{v} is available"),
    update_running: |v| format!("You have v{v}. What's new:"),
    update_restart_note: "tcanvas restarts after the update; terminals keep running.",
    update_now: "Update and restart",
    update_later: "Later",
    release_page: "View on GitHub",
    whats_new_title: |v| format!("What's new in v{v}"),
    no_release_notes: "No release notes for this version.",
    check_updates: "Check for updates",
    checking_updates: "Checking…",
    up_to_date: "You have the latest version",
    check_failed: "Check failed",
};

pub const FR: Texts = Texts {
    status_idle: "inactif",
    status_running: "actif",
    status_stalled: "bloqué",
    status_waiting: "attend saisie",
    status_failed: "échec",
    status_dead: "arrêté",

    not_a_dir: |p| format!("« {p} » n'est pas un dossier"),
    dir_not_found: |p| format!("dossier introuvable : {p}"),
    unreadable: |d| format!("lecture impossible : {d}"),
    commit_refused: "commit refusé",
    commit_not_found: "commit introuvable",
    path_refused: "chemin refusé",
    not_a_git_repo: "Pas un dépôt git : initialisez git dans ce dossier pour voir les diffs.",
    no_diff: "Aucune différence avec HEAD.",
    tmux_missing: |d| format!("tmux introuvable : {d}"),

    agent_starting: "○ agent…",
    agent_connected: "● agent",
    agent_connected_hint: "tcanvas-agent surveille les terminaux dans WSL",
    hooks_missing: "⚠ hooks",
    hooks_missing_hint: "Sans les hooks du shell (bash ou zsh), pas de code de sortie ni de durée de commande. Sans les hooks des agents de code (Claude Code, Codex, Gemini CLI, Copilot CLI, Qwen Code), un agent qui vous attend n'est repéré qu'après quelques secondes sans affichage.\nL'installation ajoute une ligne à ~/.bashrc ou ~/.zshrc et un hook dans les réglages de chaque agent (l'original est gardé en .tcanvas.bak) ; les nouveaux terminaux en profitent.",
    install_hooks: "Installer",
    hooks_failed: |e| format!("installation des hooks impossible : {e}"),
    agent_missing: "✖ agent absent",
    agent_missing_hint: |why| format!("{why}\n\ntcanvas a besoin de WSL avec une distribution Linux (wsl --install dans PowerShell).\nNouvel essai automatique toutes les 10 s."),
    agent_missing_hint_linux: |why| format!("{why}\n\ntcanvas a besoin de tmux et de tcanvas-agent (à côté de l'exécutable tcanvas ou dans le PATH).\nNouvel essai automatique toutes les 10 s."),
    retry: "Réessayer",
    agent_launch_failed: |e| format!("lancement impossible : {e}"),
    agent_stopped: "l'agent s'est arrêté",
    agent_install_failed: |e| format!("installation de l'agent dans WSL impossible : {e}"),
    tmux_missing_label: "✖ tmux absent",
    tmux_missing_hint: |d| format!("Les terminaux ont besoin de tmux dans WSL ({d}).\nInstallez-le dans WSL :\n  sudo apt install tmux"),

    just_now: "à l'instant",
    secs_ago: |n| format!("il y a {n} s"),
    mins_ago: |n| format!("il y a {n} min"),
    hours_ago: |n| format!("il y a {n} h"),
    days_ago: |n| format!("il y a {n} j"),
    changes_button: |n| format!("Δ Changements ({n})"),
    changes_button_hint: "Fichiers modifiés dans le dossier lié et leurs diffs, en direct",
    close_panel: "Fermer le panneau",
    follow: "Suivre",
    follow_hint: "Afficher automatiquement le dernier fichier modifié ou le dernier commit",
    waiting_for_agent: "En attente de l'agent…",
    uncommitted: |n| format!("Non commité ({n})"),
    all_committed: "Rien en cours : tout est commité.",
    nothing_modified: "Aucun fichier modifié depuis le début du suivi.",
    list_truncated: "… liste coupée (trop de fichiers)",
    recent_commits: "Commits récents",
    detached_head: "HEAD détachée",
    file_count: |n| if n > 1 { format!("{n} fichiers") } else { format!("{n} fichier") },
    modified_file: |p| format!("Fichier modifié {p}"),
    commit_a11y: |s| format!("Commit {s}"),
    pick_a_file: "Cliquez un fichier ou un commit pour voir son diff.",
    loading_diff: "Chargement du diff…",
    diff_truncated: "… diff coupé (fichier trop gros)",

    new_terminal_button: "+ Terminal",
    new_terminal_hint: "Nouveau terminal, dans le projet ouvert · Ctrl+Maj+N · double-clic sur le canevas",
    new_project_button: "+ Projet",
    filter_hint: "Filtrer… (Ctrl+Maj+F, Entrée : cadrer)",
    status_chip_hint: "Clic : ne montrer que ce statut · Ctrl+clic : combiner",
    clear_filter: "× filtre",
    clear_filter_count: |m, s| format!("× filtre ({m}/{s})"),
    clear_filter_hint: "Retirer le filtre et revenir à la vue d'avant",
    fit_all: "Tout voir",
    fit_all_hint: "Ctrl+Maj+0",
    attention_hint: "Aller au prochain terminal qui demande l'attention",
    empty_canvas: "Double-cliquez pour ouvrir un terminal · Ctrl+Maj+N",
    layout_warning: "⚠ layout",

    jump_hint: "Aller au terminal ou au projet…",
    no_results: "Aucun résultat",
    project_shortcut: |n| format!("   Ctrl+Maj+{n}"),
    terminal_count: |n| if n > 1 { format!("{n} terminaux") } else { format!("{n} terminal") },

    shortcuts_title: "Raccourcis clavier",
    shortcuts_read_only: "Gestes de la souris inclus. Pas encore personnalisables.",
    shift_key: "Maj",
    mouse_click: "clic",
    mouse_double_click: "double-clic",
    mouse_right_click: "clic droit",
    mouse_drag: "glisser",
    mouse_middle_drag: "glisser clic milieu",
    mouse_wheel: "molette",
    keys_global: "Partout, même dans un terminal",
    keys_terminal: "Dans un terminal",
    keys_canvas: "Canevas",
    keys_cards: "Terminaux et groupes",
    key_jump: "Aller à un terminal ou à un projet",
    key_new_card: "Nouveau terminal, dans le projet ouvert",
    key_search: "Filtrer les terminaux",
    key_fit_all: "Tout afficher",
    key_project: "Ouvrir le projet nº 1 à 9",
    key_close_card: "Fermer le terminal actif",
    key_help: "Cette fenêtre",
    key_interrupt: "Interrompre (^C), jamais copier",
    key_copy: "Copier la sélection (déjà copiée au relâchement)",
    key_paste: "Coller",
    key_tmux_menu: "Menu de tmux",
    key_focus_card: "Clic sur un terminal : y écrire",
    key_pan: "Déplacer la vue (canevas vide)",
    key_hand: "Déplacer la vue de n'importe où, même sur les terminaux",
    key_zoom: "Zoomer autour du pointeur (ou pincer)",
    key_new_card_here: "Canevas vide : nouveau terminal ici",
    key_minimap: "Minicarte : aller à cet endroit",
    key_move_card: "Barre de titre : déplacer le terminal (le déposer dans un groupe)",
    key_fly_to: "Barre de titre : aller au terminal",
    key_card_menu: "Barre de titre : menu du terminal",
    key_resize: "Coin en bas à droite : redimensionner",
    key_move_group: "Étiquette du groupe : déplacer le groupe",
    key_link_groups: "Étiquette du groupe vers un autre groupe : les relier",
    key_rename_group: "Étiquette du groupe : renommer",
    key_delete_wire: "Lien entre groupes : le supprimer",
    key_reorder_chips: "Pastilles de groupe de la barre du haut : réordonner",

    notice_stalled: "⏸ Terminal bloqué",
    notice_stalled_body: |t, s| format!("{t} : aucune sortie depuis {s} s"),
    notice_waiting: "⌨ Attend une réponse",
    notice_failed: |c| format!("✖ Échec (code {c})"),
    notice_finished: |d| format!("✔ Terminé en {d}"),
    notice_stopped: "■ Terminal arrêté",
    notice_moved: |g| format!("➜ Déplacé vers « {g} »"),
    notice_moved_body: |t| format!("{t} · clic : y aller"),
    notice_left_group: "➜ Sorti du groupe",
    notice_left_group_body: |t| format!("{t} est dans « Tous » · clic : y aller"),
    notice_copied: "⧉ Copié",
    notice_copied_body: "Sélection copiée dans le presse-papiers",
    duration_secs: |s| format!("{s} s"),
    duration_mins: |m, s| format!("{m} min {s:02} s"),
    duration_hours: |h, m| format!("{h} h {m:02} min"),

    layout_read_failed: |p, e| format!("lecture de {p} impossible : {e}"),
    layout_corrupt: |e, p| format!("layout illisible ({e}), mis de côté dans {p}"),
    layout_save_failed: |e| format!("sauvegarde impossible : {e}"),

    home_folder: "Dossier personnel",
    parent_folder: "Dossier parent",
    wsl_path_hint: "Chemin WSL, ex. ~/code/api",
    path_hint: "Chemin, ex. ~/code/api",
    recent_folders: "Récents :",
    filter_folders_hint: "Filtrer les dossiers… (Entrée : ouvrir le seul résultat)",
    loading: "Chargement…",
    no_subfolders: "Aucun sous-dossier.",
    no_matching_folder: "Aucun dossier ne correspond.",
    git_repo: "⎇ dépôt git",
    git_worktree: "⎇ worktree git",
    worktree_marker: "⎇ worktree",
    link_folder: "Lier ce dossier",
    cancel: "Annuler",
    unlink: "Délier",
    unlink_hint: "Le groupe n'aura plus de dossier",

    default_project_name: |n| format!("Projet {n}"),
    close_group_title: |n| format!("Fermer le groupe « {n} » ?"),
    close_empty_group: "Le groupe ne contient aucun terminal. Le cadre sera supprimé.",
    close_one_terminal: "Ce terminal sera fermé avec tous ses processus :\n",
    close_n_terminals: |n| format!("Ces {n} terminaux seront fermés avec tous leurs processus :\n"),
    running_command: |c| format!("  —  « {c} » en cours"),
    close_group_footer: "\nLe cadre sera supprimé. C'est définitif.",
    link_a_folder: "📁 Lier un dossier",
    folder_linked_hint: "Dossier lié : les nouveaux terminaux s'y ouvrent et ses changements sont suivis. Clic : changer",
    folder_unlinked_hint: "Lier un dossier : les nouveaux terminaux s'y ouvriront et ses changements seront suivis",
    group_folder_a11y: |n| format!("Dossier du groupe {n}"),
    group_new_terminal_hint: "Ouvrir un terminal dans ce groupe (Ctrl+Maj+N quand le groupe est ouvert)",
    group_new_terminal_a11y: |n| format!("Nouveau terminal dans le groupe {n}"),
    group_changes_hint: "Voir les fichiers modifiés, les commits récents et leurs diffs",
    group_changes_a11y: |n| format!("Changements du groupe {n}"),
    open_a_terminal: "+ Ouvrir un terminal",
    group_header_hint: "Double-clic : renommer · glisser : déplacer avec ses terminaux · Alt + glisser sur un autre groupe : les relier",
    close_group_hint: "Fermer le groupe et tous ses terminaux",
    close_group_a11y: |n| format!("Fermer le groupe {n}"),
    drop_to_leave_group: "Relâcher pour sortir le terminal du groupe",
    picker_title: |n| format!("Dossier lié au groupe « {n} »"),
    new_terminal_here: "Nouveau terminal ici",
    rename_group: "Renommer (double-clic sur le titre)",
    linked_folder_menu: "Dossier lié…",
    unlink_folder: "Délier le dossier",
    color: "Couleur",
    close_group_menu: "Fermer le groupe et ses terminaux…",
    remove_frame: "Supprimer le cadre (garde les terminaux)",
    close_all: "Tout fermer",

    all_spaces: "Tous",
    all_spaces_hint: "Vue d'ensemble : tous les projets",
    all_groups_menu_hint: "Groupes hors de vue : ouvrir la liste de tous les groupes",
    free_grid: "Grille libre",
    free_grid_hint: "Dans un groupe, utiliser tout le canevas : pas de cadre, les terminaux restent dans le groupe où qu'on les lâche",
    open_project: "Ouvrir ce projet · Glisser : réordonner · Clic droit : trier",
    show_terminal: "Afficher",
    open_project_shortcut: |n| format!("Ouvrir ce projet · Ctrl+Maj+{n} · Glisser : réordonner · Clic droit : trier"),
    sort_projects: "Trier les groupes de A à Z",

    spawn_failed: |p, e| format!("Impossible de lancer {p} : {e}"),
    close_terminal_hint: "Fermer le terminal (Ctrl+Maj+W)",
    close_terminal_a11y: |t| format!("Fermer le terminal {t}"),
    last_prompt: |p| format!("« {p} »"),
    last_prompt_a11y: |p| format!("Dernier prompt : {p}"),
    rename_menu: "Renommer…",
    stall_delay_menu: "Délai avant « bloqué »…",
    center_and_zoom: "Centrer et zoomer",
    move_to: "Déplacer vers",
    no_group: "Aucun groupe",
    close_terminal_menu: "Fermer le terminal…  (Ctrl+Maj+W)",
    session_ended: "Session terminée",
    respawn: "Relancer",
    remove: "Retirer",
    close: "Fermer",
    terminal_name_prompt: "Nom du terminal (vide : automatique)",
    stall_prompt: "Secondes sans sortie avant « bloqué »",
    close_card_title: |t| format!("Fermer « {t} » ?"),
    close_card_running: |c| format!("« {c} » est en cours d'exécution et sera arrêté.\nLe shell et tous ses processus seront fermés. C'est définitif."),
    close_card_body: "Le shell et tous ses processus seront fermés. C'est définitif.",
    ok: "OK",
    branch_badge_a11y: |b| format!("Branche : {b}"),

    worktrees_title: |n| format!("⎇ {n} worktrees"),
    main_checkout: "copie principale",
    worktree_dirty_hint: "Modifications non commitées",
    ahead_behind_hint: |b| format!("Commits en avance (↑) et en retard (↓) sur {b}"),
    merged: "✔ fusionnée",
    merged_hint: |b| format!("Tous les commits de cette branche sont dans {b}"),
    locked: "verrouillé",
    prunable: "introuvable",
    prunable_hint: "Son dossier n'existe plus",
    no_terminal_here: "aucun terminal",
    last_commit: |age| format!("Dernier commit {age}"),
    go_to_terminal: "Aller à ce terminal",

    wire_hint: |a, b| format!("{a}  ⇄  {b}\nclic : aller à l'autre groupe · clic droit : supprimer"),
    linked_group_hint: |name| format!("Groupe relié : ouvrir « {name} »"),

    version_hint: "Nouveautés et historique des versions",
    update_button: |v| format!("Mettre à jour vers v{v}"),
    update_button_hint: "Voir les nouveautés, puis mettre à jour",
    update_installing: |v| format!("Téléchargement de v{v}…"),
    update_restarting: "Redémarrage…",
    update_failed: "Échec de la mise à jour",
    update_failed_hint: |e| format!("{e}\nCliquer pour ouvrir la page de la version"),
    update_notice_title: "Mise à jour disponible",
    update_notice_body: |v| format!("tcanvas v{v} est disponible : cliquer pour voir les nouveautés"),
    update_title: |v| format!("tcanvas v{v} est disponible"),
    update_running: |v| format!("Vous avez la v{v}. Les nouveautés :"),
    update_restart_note: "tcanvas redémarre après la mise à jour ; les terminaux continuent de tourner.",
    update_now: "Mettre à jour et redémarrer",
    update_later: "Plus tard",
    release_page: "Voir sur GitHub",
    whats_new_title: |v| format!("Quoi de neuf dans la v{v}"),
    no_release_notes: "Pas de notes pour cette version.",
    check_updates: "Rechercher des mises à jour",
    checking_updates: "Vérification…",
    up_to_date: "Vous avez la dernière version",
    check_failed: "Échec de la vérification",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn french_locales_give_french_everything_else_english() {
        for tag in ["fr", "fr-CA", "fr-FR", "FR-be", "fr_FR.UTF-8", "fr_CA@euro"] {
            assert_eq!(from_locale(tag), Lang::Fr, "{tag}");
        }
        for tag in ["en-US", "en_GB.UTF-8", "de-DE", "es", "C", "POSIX", "", "fra", "frisian"] {
            assert_eq!(from_locale(tag), Lang::En, "{tag}");
        }
    }

    #[test]
    fn language_is_per_thread_and_settable() {
        set(Lang::Fr);
        assert_eq!((t().follow, t().status(TermStatus::Stalled)), ("Suivre", "bloqué"));
        set(Lang::En);
        assert_eq!((t().follow, t().status(TermStatus::Stalled)), ("Follow", "stalled"));
        // Another thread keeps its own language.
        std::thread::spawn(|| {
            set(Lang::Fr);
            assert_eq!(t().follow, "Suivre");
        })
        .join()
        .unwrap();
        assert_eq!(t().follow, "Follow");
    }

    #[test]
    fn agent_problems_are_translated() {
        let p = Problem::DirNotFound { path: "~/x".into() };
        assert_eq!(texts(Lang::En).problem(&p), "folder not found: ~/x");
        assert_eq!(texts(Lang::Fr).problem(&p), "dossier introuvable : ~/x");
        assert!(texts(Lang::En).problem(&Problem::NotAGitRepo).starts_with("Not a git repository"));
    }

    #[test]
    fn platform_texts_mention_wsl_only_on_windows() {
        let (fr, en) = (texts(Lang::Fr), texts(Lang::En));
        assert!(fr.agent_missing_help_for(true, "x").contains("WSL"));
        let linux = fr.agent_missing_help_for(false, "x");
        assert!(linux.starts_with("x\n") && linux.contains("tmux") && !linux.contains("WSL"), "{linux}");
        assert!(en.agent_missing_help_for(false, "x").contains("needs tmux and tcanvas-agent"));
        assert_eq!((fr.folder_path_hint_for(true), fr.folder_path_hint_for(false)), ("Chemin WSL, ex. ~/code/api", "Chemin, ex. ~/code/api"));
        assert_eq!(en.folder_path_hint_for(false), "Path, e.g. ~/code/api");
    }

    #[test]
    fn plurals() {
        let (en, fr) = (texts(Lang::En), texts(Lang::Fr));
        assert_eq!(((en.file_count)(1), (en.file_count)(3)), ("1 file".into(), "3 files".into()));
        assert_eq!(((fr.file_count)(0), (fr.file_count)(1), (fr.file_count)(2)), ("0 fichier".into(), "1 fichier".into(), "2 fichiers".into()));
        assert_eq!(((en.terminal_count)(2), (fr.terminal_count)(2)), ("2 terminals".into(), "2 terminaux".into()));
    }
}
