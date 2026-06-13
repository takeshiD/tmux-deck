use std::collections::HashSet;
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText;
use ratatui::text::Text;
use ratatui::widgets::ListState;

use crate::config::{BehaviorConfig, Config, HooksConfig, KeyBindings, LayoutConfig, Theme};
use crate::group::GroupStore;

/// Label shown for the implicit group of sessions that have not been assigned
/// to any user group. Only rendered when at least one session *is* grouped.
pub const UNGROUPED_LABEL: &str = "Ungrouped";

/// Maximum number of characters (not bytes) accepted in the session/group name
/// input popups. Keeps names short enough to render in the narrow list panes.
pub const SESSION_NAME_MAX_LEN: usize = 30;

// =============================================================================
// Data Structures
// =============================================================================

/// State reported by Claude Code hooks for a given pane.
///
/// Process detection (`has_claude`) only tells us whether claude is running;
/// these states tell us *what claude is doing*, sourced from Claude Code's
/// hook events (see [`crate::hook`]). Variants are ordered loosely by how much
/// they want the user's attention — see [`ClaudeState::priority`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeState {
    /// Claude is actively working (prompt submitted / tool running).
    Working,
    /// Claude is waiting on the user (permission prompt / idle prompt).
    Waiting,
    /// Claude finished its turn.
    Done,
    /// Claude's turn ended with an error.
    Error,
}

impl ClaudeState {
    /// Map a Claude Code `hook_event_name` to the state it implies.
    /// Returns `None` for events that carry no marker meaning (the caller may
    /// treat `SessionEnd` specially, clearing any existing marker).
    pub fn from_hook_event(event: &str) -> Option<Self> {
        match event {
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PreCompact" => Some(Self::Working),
            "Notification" => Some(Self::Waiting),
            "Stop" | "SubagentStop" => Some(Self::Done),
            // StopFailure is not yet confirmed in the public docs; map it
            // defensively so it lights up red if it ever fires.
            "StopFailure" => Some(Self::Error),
            _ => None,
        }
    }

    /// Stable lowercase token used in the on-disk state files.
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Done => "done",
            Self::Error => "error",
        }
    }

    /// Inverse of [`Self::as_token`].
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "working" => Some(Self::Working),
            "waiting" => Some(Self::Waiting),
            "done" => Some(Self::Done),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// How strongly this state wants the user's attention. Used when rolling
    /// pane states up into a single window / session marker — the highest
    /// priority among children wins.
    pub fn priority(self) -> u8 {
        match self {
            Self::Waiting => 3,
            Self::Error => 2,
            Self::Working => 1,
            Self::Done => 0,
        }
    }

    /// Combine two optional states, keeping the higher-priority one.
    pub fn merge(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(x), Some(y)) => Some(if x.priority() >= y.priority() { x } else { y }),
            (Some(x), None) => Some(x),
            (None, b) => b,
        }
    }
}

/// Represents a tmux pane
#[derive(Debug, Clone)]
pub struct TmuxPane {
    pub id: String,
    pub index: u32,
    #[allow(dead_code)]
    pub width: u32,
    #[allow(dead_code)]
    pub height: u32,
    pub active: bool,
    pub current_command: String,
    pub pid: u32,
    /// True if a claude process is running in this pane (detected via descendant process scan).
    pub has_claude: bool,
    /// Latest state reported by Claude Code hooks for this pane, if any.
    pub claude_state: Option<ClaudeState>,
    /// One-line summary of what Claude is currently doing (e.g. `Edit: src/app.rs`),
    /// sourced from the hook activity digest. `None` when unknown.
    pub claude_activity: Option<String>,
    /// Unix timestamp (secs) when the current Claude state began. Used to show
    /// how long a pane has been working/waiting.
    pub claude_state_since: Option<i64>,
    /// Working directory Claude reported for this pane (repo identification).
    #[allow(dead_code)]
    pub claude_cwd: Option<String>,
}

impl TmuxPane {
    /// Seconds elapsed since the current Claude state began, if known.
    pub fn claude_state_elapsed_secs(&self) -> Option<i64> {
        self.claude_state_since
            .map(|since| crate::hook::now_secs().saturating_sub(since).max(0))
    }
}

/// Represents a tmux window with captured content
#[derive(Debug, Clone)]
pub struct TmuxWindow {
    pub index: u32,
    pub name: String,
    pub panes: Vec<TmuxPane>,
    /// True if any pane in this window has claude running.
    pub has_claude: bool,
    /// Highest-priority Claude hook state across this window's panes.
    pub claude_state: Option<ClaudeState>,
}

impl TmuxWindow {
    pub fn get_active_pane(&self) -> Option<&TmuxPane> {
        self.panes.iter().find(|p| p.active).or(self.panes.first())
    }
}

/// Represents a tmux session
#[derive(Debug, Clone)]
pub struct TmuxSession {
    pub name: String,
    pub windows: Vec<TmuxWindow>,
    /// True if any window in this session has claude running.
    pub has_claude: bool,
    /// Highest-priority Claude hook state across this session's windows.
    pub claude_state: Option<ClaudeState>,
    /// Epoch seconds — kept on the struct so [`SessionSort`] can reorder
    /// the list without re-querying tmux.
    pub last_attached: i64,
    pub activity: i64,
    /// tmux-deck-side group label this session belongs to, if any. This is a
    /// purely organisational tag managed by the deck (see [`crate::group`]),
    /// independent of tmux's native session groups. `None` means ungrouped.
    pub group: Option<String>,
}

// =============================================================================
// Enums
// =============================================================================

/// Main view mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewMode {
    TreeView,
    MultiPreview,
    /// Full-screen fleet dashboard: every pane running Claude across all
    /// sessions, sorted by how much it wants attention.
    Dashboard,
}

/// One row of the fleet dashboard: a single pane running Claude, with the
/// context needed to render it and to jump to it.
#[derive(Debug, Clone)]
pub struct DashboardRow {
    /// tmux target (`session:window.pane`) for switching to this pane.
    pub target: String,
    pub session: String,
    pub pane_id: String,
    pub state: ClaudeState,
    pub activity: Option<String>,
    pub elapsed_secs: Option<i64>,
}

/// Focus area in TreeView mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Focus {
    Sessions,
    Windows,
    Panes,
}

/// Application input mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputMode {
    Normal,
    Input,
}

/// What attribute to sort sessions by.
///
/// To add a new sort attribute:
///   1. Add a variant here.
///   2. Add a comparator branch in [`SessionSortKey::cmp_ascending`].
///   3. Add a short label in [`SessionSortKey::label`].
///   4. Add `SessionSort` entries (one per direction) to [`SessionSort::ALL`]
///      at the position you want users to land on when cycling with `s`.
///
/// Direction handling, UI display and key wiring are all generic over key —
/// no further code needs to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSortKey {
    /// Most recently attached time (`last_attached`, tie-broken by `activity`).
    LastAttached,
    /// Case-insensitive session name.
    Alphabet,
}

impl SessionSortKey {
    /// Short label fragment shown in the Sessions list title.
    pub fn label(self) -> &'static str {
        match self {
            SessionSortKey::LastAttached => "recent",
            SessionSortKey::Alphabet => "abc",
        }
    }

    /// Compare two sessions by this key, with smaller raw values first.
    /// [`SessionSort`] flips this for the [`SortDirection::Desc`] case so the
    /// key implementer only ever has to think about the natural ordering.
    fn cmp_ascending(self, a: &TmuxSession, b: &TmuxSession) -> std::cmp::Ordering {
        match self {
            SessionSortKey::LastAttached => a
                .last_attached
                .cmp(&b.last_attached)
                .then_with(|| a.activity.cmp(&b.activity)),
            SessionSortKey::Alphabet => a
                .name
                .to_lowercase()
                .cmp(&b.name.to_lowercase()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    /// Largest value first — top of the list has the highest raw key.
    /// e.g. `LastAttached + Desc` = newest first; `Alphabet + Desc` = Z first.
    Desc,
    /// Smallest value first — top of the list has the lowest raw key.
    /// e.g. `LastAttached + Asc` = oldest first; `Alphabet + Asc` = A first.
    Asc,
}

impl SortDirection {
    pub fn arrow(self) -> char {
        match self {
            SortDirection::Desc => '↓',
            SortDirection::Asc => '↑',
        }
    }
}

/// A complete sort spec: which attribute, in which direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSort {
    pub key: SessionSortKey,
    pub direction: SortDirection,
}

impl SessionSort {
    /// All sort modes in the order the `s` key cycles through. Default is the
    /// first entry. To add a new key, expand this list with one entry per
    /// direction (typically `Desc` then `Asc`).
    pub const ALL: &'static [SessionSort] = &[
        SessionSort {
            key: SessionSortKey::LastAttached,
            direction: SortDirection::Desc,
        },
        SessionSort {
            key: SessionSortKey::LastAttached,
            direction: SortDirection::Asc,
        },
        SessionSort {
            key: SessionSortKey::Alphabet,
            direction: SortDirection::Desc,
        },
        SessionSort {
            key: SessionSortKey::Alphabet,
            direction: SortDirection::Asc,
        },
    ];

    /// Label shown in the Sessions list title, e.g. "recent↓" / "abc↑".
    pub fn label(self) -> String {
        format!("{}{}", self.key.label(), self.direction.arrow())
    }

    /// Next mode in [`Self::ALL`], wrapping around.
    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    /// Sort `sessions` in-place.
    pub fn apply(self, sessions: &mut [TmuxSession]) {
        sessions.sort_by(|a, b| {
            let ord = self.key.cmp_ascending(a, b);
            let ord = match self.direction {
                SortDirection::Desc => ord.reverse(),
                SortDirection::Asc => ord,
            };
            // Stable, deterministic tie-break — always by name ascending so
            // the list does not jiggle on refresh when the primary key ties.
            ord.then_with(|| a.name.cmp(&b.name))
        });
    }
}

impl Default for SessionSort {
    fn default() -> Self {
        Self::ALL[0]
    }
}

/// Popup mode for session operations
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PopupMode {
    /// Creating a new session
    NewSession,
    /// Renaming the selected session
    RenameSession,
    /// Confirming session kill
    ConfirmKill,
    /// Choosing a group for the selected session from a list of existing
    /// groups (plus "ungroup" and "create new" entries).
    GroupSession,
    /// Typing the name of a brand-new group, reached from the GroupSession
    /// list via the "New group" entry.
    NewGroup,
}

/// The entry highlighted in the [`PopupMode::GroupSession`] selection list.
/// The list shows every existing group, then an "Ungrouped" entry that clears
/// the assignment, then a "New group" entry that switches to text entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupChoice {
    /// Assign the session to an existing group of this name.
    Existing(String),
    /// Remove the session from any group.
    Ungrouped,
    /// Create a new group (switches the popup to text entry).
    New,
}

/// A single rendered row in the Sessions list. Grouping inserts non-selectable
/// [`SessionRow::Header`] rows between the [`SessionRow::Session`] rows; the
/// session rows still map 1:1 onto indices into [`UIState::sessions`], so all
/// navigation continues to operate on session indices and only rendering needs
/// to be group-aware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRow {
    /// A group heading. `group` is `None` for the implicit ungrouped bucket.
    /// `collapsed` drives the fold indicator and means the member session rows
    /// are hidden.
    Header {
        group: Option<String>,
        count: usize,
        collapsed: bool,
    },
    /// A session, identified by its index into [`UIState::sessions`].
    Session { index: usize },
}

// =============================================================================
// UI State (formerly App)
// =============================================================================

pub struct UIState {
    // View mode
    pub view_mode: ViewMode,

    // Space key tracking for double-press
    pub last_space_press: Option<Instant>,

    // TreeView state
    pub sessions: Vec<TmuxSession>,
    pub selected_session: usize,
    pub selected_window: usize,
    pub selected_pane: usize,
    pub focus: Focus,
    pub session_list_state: ListState,
    pub window_list_state: ListState,
    pub pane_list_state: ListState,
    pub session_sort: SessionSort,

    /// Persisted tmux-deck-side session grouping (session name -> group).
    pub groups: GroupStore,
    /// Groups currently folded in the Sessions list. A group key of `None` is
    /// the implicit "Ungrouped" bucket. Fold state is session-runtime only and
    /// is not persisted.
    pub collapsed_groups: HashSet<Option<String>>,
    /// True after `z` is pressed, awaiting the `a` of the `za` fold chord.
    pub pending_z: bool,

    // MultiPreview state (session_idx, window_idx)
    pub multi_session: usize,
    pub multi_window: usize,

    /// Selected row index in the fleet dashboard (`ViewMode::Dashboard`).
    pub dashboard_selected: usize,

    // Shared state
    pub pane_content: String,
    pub pane_content_parsed: Option<Text<'static>>,
    pub last_error: Option<String>,
    #[allow(dead_code)]
    pub interval: Duration,

    // Resolved user configuration.
    /// Semantic UI colour palette.
    pub theme: Theme,
    /// Per-state hook markers (claude / codex).
    pub hooks: HooksConfig,
    /// Remappable key bindings.
    pub keybindings: KeyBindings,
    /// Panel layout ratios.
    pub layout: LayoutConfig,
    /// Behavioural toggles (double-space window, exit-on-switch, …).
    pub behavior: BehaviorConfig,

    pub input_mode: InputMode,
    pub input_buffer: String,
    pub input_cursor: usize,

    // Popup state
    pub popup_mode: Option<PopupMode>,
    pub confirm_yes_selected: bool,
    /// Existing group names offered in the GroupSession selection list,
    /// snapshotted when the popup opens so navigation stays stable.
    pub group_choices: Vec<String>,
    /// Index of the highlighted entry in the GroupSession list. Entries are
    /// `group_choices` followed by the "Ungrouped" and "New group" entries.
    pub group_choice_index: usize,
}

impl UIState {
    pub fn new(config: Config) -> Self {
        let interval_ms = config.preview.interval.unwrap_or(300);
        let theme = config.theme.resolve();
        let view_mode = config.behavior.view_mode();
        let session_sort = config.behavior.session_sort();
        let mut state = Self {
            view_mode,
            last_space_press: None,

            sessions: Vec::new(),
            selected_session: 0,
            selected_window: 0,
            selected_pane: 0,
            focus: Focus::Sessions,
            session_list_state: ListState::default(),
            window_list_state: ListState::default(),
            pane_list_state: ListState::default(),
            session_sort,

            groups: GroupStore::load(),
            collapsed_groups: HashSet::new(),
            pending_z: false,

            multi_session: 0,
            multi_window: 0,

            dashboard_selected: 0,

            pane_content: String::new(),
            pane_content_parsed: None,
            last_error: None,
            interval: Duration::from_millis(interval_ms),

            theme,
            hooks: config.hooks,
            keybindings: config.keybindings,
            layout: config.layout,
            behavior: config.behavior,

            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            input_cursor: 0,

            popup_mode: None,
            group_choices: Vec::new(),
            group_choice_index: 0,
            confirm_yes_selected: false,
        };
        state.session_list_state.select(Some(0));
        state.window_list_state.select(Some(0));
        state.pane_list_state.select(Some(0));
        state
    }

    // =========================================================================
    // View Mode Switching
    // =========================================================================

    /// Re-read Claude hook state files and patch the in-memory session tree.
    ///
    /// Cheap enough to call on every refresh tick: it only reads a small local
    /// state directory. This keeps markers live without a full tmux refresh.
    pub fn refresh_claude_states(&mut self) {
        crate::hook::apply_states(&mut self.sessions);
    }

    /// True if any session currently has a `Working` Claude marker, used to
    /// decide whether the spinner animation needs frequent redraws.
    pub fn has_working_claude(&self) -> bool {
        self.sessions
            .iter()
            .any(|s| s.claude_state == Some(ClaudeState::Working))
    }

    pub fn handle_space_press(&mut self) -> bool {
        let now = Instant::now();
        if let Some(last) = self.last_space_press
            && now.duration_since(last) < Duration::from_millis(self.behavior.double_space_ms)
        {
            // Double space detected
            self.toggle_view_mode();
            self.last_space_press = None;
            return true;
        }
        self.last_space_press = Some(now);
        false
    }

    pub fn toggle_view_mode(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::TreeView => {
                // Sync multi selection with tree selection
                self.multi_session = self.selected_session;
                self.multi_window = self.selected_window;
                ViewMode::MultiPreview
            }
            ViewMode::MultiPreview => {
                // Sync tree selection with multi selection
                self.selected_session = self.multi_session;
                self.selected_window = self.multi_window;
                self.selected_pane = 0;
                self.session_list_state.select(Some(self.selected_session));
                self.window_list_state.select(Some(self.selected_window));
                self.pane_list_state.select(Some(0));
                ViewMode::TreeView
            }
            // Double-space cycles only Tree <-> Multi; leaving the dashboard
            // returns to the tree.
            ViewMode::Dashboard => ViewMode::TreeView,
        };
    }

    // =========================================================================
    // Fleet Dashboard
    // =========================================================================

    /// Toggle the fleet dashboard on/off. Entering it resets the selection.
    pub fn toggle_dashboard(&mut self) {
        if self.view_mode == ViewMode::Dashboard {
            self.view_mode = ViewMode::TreeView;
        } else {
            self.dashboard_selected = 0;
            self.view_mode = ViewMode::Dashboard;
        }
    }

    /// All panes currently running Claude (or carrying a hook state), across
    /// every session, sorted by attention priority (Waiting → Error → Working
    /// → Done) and then by how long they have been in that state (longest
    /// first), so a stuck/old item floats up within its group.
    pub fn dashboard_rows(&self) -> Vec<DashboardRow> {
        let mut rows: Vec<DashboardRow> = Vec::new();
        for session in &self.sessions {
            for window in &session.windows {
                for pane in &window.panes {
                    let Some(state) = pane.claude_state else {
                        continue;
                    };
                    rows.push(DashboardRow {
                        target: format!("{}:{}.{}", session.name, window.index, pane.index),
                        session: session.name.clone(),
                        pane_id: pane.id.clone(),
                        state,
                        activity: pane.claude_activity.clone(),
                        elapsed_secs: pane.claude_state_elapsed_secs(),
                    });
                }
            }
        }
        rows.sort_by(|a, b| {
            b.state
                .priority()
                .cmp(&a.state.priority())
                .then(b.elapsed_secs.cmp(&a.elapsed_secs))
        });
        rows
    }

    pub fn dashboard_move_up(&mut self) {
        self.dashboard_selected = self.dashboard_selected.saturating_sub(1);
    }

    pub fn dashboard_move_down(&mut self) {
        let len = self.dashboard_rows().len();
        if len > 0 {
            self.dashboard_selected = (self.dashboard_selected + 1).min(len - 1);
        }
    }

    /// tmux target of the currently highlighted dashboard row, if any.
    pub fn get_dashboard_target(&self) -> Option<String> {
        self.dashboard_rows()
            .get(self.dashboard_selected)
            .map(|r| r.target.clone())
    }

    // =========================================================================
    // Input Mode
    // =========================================================================

    pub fn enter_input_mode(&mut self) {
        self.input_mode = InputMode::Input;
        self.input_buffer.clear();
        self.input_cursor = 0;
    }

    pub fn exit_input_mode(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.input_cursor = 0;
    }

    pub fn get_current_target(&self) -> Option<String> {
        match self.view_mode {
            ViewMode::TreeView => self.get_selected_pane_target(),
            ViewMode::MultiPreview => self.get_multi_selected_target(),
            ViewMode::Dashboard => self.get_dashboard_target(),
        }
    }

    pub fn get_enter_target(&self) -> Option<String> {
        match self.view_mode {
            ViewMode::TreeView => match self.focus {
                Focus::Sessions => self
                    .sessions
                    .get(self.selected_session)
                    .map(|s| s.name.clone()),
                Focus::Windows => {
                    let session = self.sessions.get(self.selected_session)?;
                    let window = session.windows.get(self.selected_window)?;
                    Some(format!("{}:{}", session.name, window.index))
                }
                Focus::Panes => self.get_selected_pane_target(),
            },
            ViewMode::MultiPreview => self.get_multi_selected_target(),
            ViewMode::Dashboard => self.get_dashboard_target(),
        }
    }

    /// `input_cursor`（char 単位）を `input_buffer` 内のバイトオフセットへ変換する。
    fn input_cursor_byte_offset(&self) -> usize {
        self.input_buffer
            .char_indices()
            .nth(self.input_cursor)
            .map(|(byte_idx, _)| byte_idx)
            .unwrap_or(self.input_buffer.len())
    }

    /// `input_buffer` の文字数（char 単位）。
    fn input_char_count(&self) -> usize {
        self.input_buffer.chars().count()
    }

    pub fn input_char(&mut self, c: char) {
        let byte_offset = self.input_cursor_byte_offset();
        self.input_buffer.insert(byte_offset, c);
        self.input_cursor += 1;
    }

    /// Insert a character only while the buffer holds fewer than `max_chars`
    /// characters; otherwise the keystroke is ignored. Used by the session/group
    /// name popups to cap the name length.
    pub fn input_char_limited(&mut self, c: char, max_chars: usize) {
        if self.input_char_count() < max_chars {
            self.input_char(c);
        }
    }

    pub fn input_backspace(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
            let byte_offset = self.input_cursor_byte_offset();
            self.input_buffer.remove(byte_offset);
        }
    }

    pub fn input_delete(&mut self) {
        if self.input_cursor < self.input_char_count() {
            let byte_offset = self.input_cursor_byte_offset();
            self.input_buffer.remove(byte_offset);
        }
    }

    pub fn input_move_left(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
        }
    }

    pub fn input_move_right(&mut self) {
        if self.input_cursor < self.input_char_count() {
            self.input_cursor += 1;
        }
    }

    pub fn input_move_home(&mut self) {
        self.input_cursor = 0;
    }

    pub fn input_move_end(&mut self) {
        self.input_cursor = self.input_char_count();
    }

    // =========================================================================
    // Session Operations (Popup)
    // =========================================================================

    pub fn open_new_session_popup(&mut self) {
        self.popup_mode = Some(PopupMode::NewSession);
        self.input_buffer.clear();
        self.input_cursor = 0;
    }

    pub fn open_rename_session_popup(&mut self) {
        if let Some(session) = self.sessions.get(self.selected_session) {
            self.popup_mode = Some(PopupMode::RenameSession);
            self.input_buffer = session.name.clone();
            self.input_cursor = self.input_char_count();
        }
    }

    pub fn open_group_session_popup(&mut self) {
        let Some(session) = self.sessions.get(self.selected_session) else {
            return;
        };
        let current = session.group.clone();
        self.popup_mode = Some(PopupMode::GroupSession);
        self.group_choices = self.groups.group_names();
        // Highlight the session's current group by default, falling back to the
        // "Ungrouped" entry (which sits just past the existing groups) when the
        // session is not grouped yet.
        self.group_choice_index = match current {
            Some(g) => self
                .group_choices
                .iter()
                .position(|name| *name == g)
                .unwrap_or(self.group_choices.len()),
            None => self.group_choices.len(),
        };
        self.input_buffer.clear();
        self.input_cursor = 0;
    }

    /// Total number of entries in the GroupSession list: every existing group,
    /// then the "Ungrouped" and "New group" entries.
    pub fn group_choice_count(&self) -> usize {
        self.group_choices.len() + 2
    }

    /// The entry currently highlighted in the GroupSession list.
    pub fn selected_group_choice(&self) -> GroupChoice {
        let n = self.group_choices.len();
        if self.group_choice_index < n {
            GroupChoice::Existing(self.group_choices[self.group_choice_index].clone())
        } else if self.group_choice_index == n {
            GroupChoice::Ungrouped
        } else {
            GroupChoice::New
        }
    }

    pub fn group_choice_up(&mut self) {
        let n = self.group_choice_count();
        self.group_choice_index = (self.group_choice_index + n - 1) % n;
    }

    pub fn group_choice_down(&mut self) {
        let n = self.group_choice_count();
        self.group_choice_index = (self.group_choice_index + 1) % n;
    }

    /// Switch the open GroupSession popup into text entry for a new group name.
    pub fn begin_new_group_entry(&mut self) {
        self.popup_mode = Some(PopupMode::NewGroup);
        self.input_buffer.clear();
        self.input_cursor = 0;
    }

    pub fn open_kill_session_popup(&mut self) {
        if !self.sessions.is_empty() {
            self.popup_mode = Some(PopupMode::ConfirmKill);
            self.confirm_yes_selected = false; // Default to No
        }
    }

    pub fn close_popup(&mut self) {
        self.popup_mode = None;
        self.input_buffer.clear();
        self.input_cursor = 0;
        self.confirm_yes_selected = false;
        self.group_choices.clear();
        self.group_choice_index = 0;
    }

    pub fn toggle_confirm_selection(&mut self) {
        self.confirm_yes_selected = !self.confirm_yes_selected;
    }

    /// Get the session name to create (for NewSession popup)
    pub fn get_new_session_name(&self) -> String {
        self.input_buffer.trim().to_string()
    }

    /// Get the current session name and new name (for RenameSession popup)
    pub fn get_rename_session_info(&self) -> Option<(String, String)> {
        let new_name = self.input_buffer.trim().to_string();
        if new_name.is_empty() {
            return None;
        }
        self.sessions
            .get(self.selected_session)
            .map(|s| (s.name.clone(), new_name))
    }

    /// Get the group name typed in the GroupSession popup. An empty/whitespace
    /// entry means "remove from any group" and is returned as `None`.
    pub fn get_group_session_input(&self) -> Option<String> {
        let trimmed = self.input_buffer.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    /// Get the session name to kill (for ConfirmKill popup)
    pub fn get_kill_session_name(&self) -> Option<String> {
        if self.confirm_yes_selected {
            self.sessions
                .get(self.selected_session)
                .map(|s| s.name.clone())
        } else {
            None
        }
    }

    // =========================================================================
    // Data Update (called when TmuxResponse is received)
    // =========================================================================

    pub fn update_sessions(&mut self, sessions: Vec<TmuxSession>) {
        // Preserve the user's currently-highlighted session across the refresh:
        // it may move to a new index once the new order is applied (e.g. when
        // sort is Alphabet and a session was renamed).
        let current_name = self
            .sessions
            .get(self.selected_session)
            .map(|s| s.name.clone());

        self.sessions = sessions;
        self.apply_group_labels();
        self.order_sessions();

        if let Some(name) = current_name
            && let Some(idx) = self.sessions.iter().position(|s| s.name == name)
        {
            self.selected_session = idx;
        }

        self.validate_selections();
        self.last_error = None;
    }

    /// Stamp each session with its persisted group label. Called whenever fresh
    /// session data arrives from tmux, since the tmux layer is group-agnostic.
    fn apply_group_labels(&mut self) {
        for session in &mut self.sessions {
            session.group = self.groups.group_of(&session.name);
        }
    }

    /// Order the session list: first by the active [`SessionSort`], then cluster
    /// sessions of the same group together. Because the clustering pass is a
    /// *stable* sort keyed only on the group, sessions keep their sort order
    /// within each group, and ungrouped sessions fall to the bottom.
    fn order_sessions(&mut self) {
        self.session_sort.apply(&mut self.sessions);
        self.sessions.sort_by(|a, b| match (&a.group, &b.group) {
            (Some(x), Some(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
    }

    /// Assign the currently-selected session to `group` (or remove it from any
    /// group when `group` is `None`/empty), persist the change, and re-order the
    /// list in place keeping that session highlighted. No tmux round-trip is
    /// needed — grouping is entirely tmux-deck-side.
    pub fn assign_selected_group(&mut self, group: Option<String>) {
        let Some(session) = self.sessions.get(self.selected_session) else {
            return;
        };
        let name = session.name.clone();
        self.groups.set(&name, group.as_deref());
        // Reveal the destination group so the user sees the session land, even
        // if that group was folded.
        self.collapsed_groups.remove(&group);
        self.apply_group_labels();
        self.resort_sessions_preserve_selection();
    }

    /// Whether any session carries a group label. When false there are no
    /// headers and folding is a no-op (there is nothing to organise yet).
    fn any_grouped(&self) -> bool {
        self.sessions.iter().any(|s| s.group.is_some())
    }

    /// Whether `group` is currently folded. Folding only takes effect once
    /// real groups exist, so a stray collapsed entry never hides a flat list.
    fn is_collapsed(&self, group: &Option<String>) -> bool {
        self.any_grouped() && self.collapsed_groups.contains(group)
    }

    /// Whether the session at `index` is the first of its group in the current
    /// ordering — the row a folded group collapses onto.
    fn is_group_head(&self, index: usize) -> bool {
        match self.sessions.get(index) {
            None => false,
            Some(s) => index == 0 || self.sessions[index - 1].group != s.group,
        }
    }

    /// Whether the cursor may rest on the session at `index`. A session is a
    /// stop when it is visible, or when it is the head of a folded group — in
    /// which case the cursor visually sits on that group's (collapsed) header,
    /// so the group can be re-expanded with `za`.
    fn is_cursor_stop(&self, index: usize) -> bool {
        match self.sessions.get(index) {
            None => false,
            Some(s) => !self.is_collapsed(&s.group) || self.is_group_head(index),
        }
    }

    /// Whether the selection currently sits on a folded group's header rather
    /// than a visible session. Used by the renderer to highlight the header.
    pub fn selection_on_folded_header(&self) -> bool {
        self.sessions
            .get(self.selected_session)
            .map(|s| self.is_collapsed(&s.group))
            .unwrap_or(false)
    }

    /// Toggle the fold state of the group containing the selected session.
    /// When folding, the selection collapses onto the group's head so the
    /// cursor stays on the (now folded) header and the group can be reopened
    /// with another `za`.
    pub fn toggle_fold_current_group(&mut self) {
        if !self.any_grouped() {
            return;
        }
        let Some(session) = self.sessions.get(self.selected_session) else {
            return;
        };
        let group = session.group.clone();
        if self.collapsed_groups.contains(&group) {
            self.collapsed_groups.remove(&group);
        } else {
            self.collapsed_groups.insert(group.clone());
            // Park the selection on the group's head so it remains a valid
            // cursor stop (the folded header) instead of a hidden member.
            if let Some(head) = self.sessions.iter().position(|s| s.group == group) {
                self.selected_session = head;
                self.selected_window = 0;
                self.selected_pane = 0;
                self.window_list_state.select(Some(0));
                self.pane_list_state.select(Some(0));
            }
        }
    }

    fn next_cursor_stop(&self, from: usize) -> Option<usize> {
        ((from + 1)..self.sessions.len()).find(|&i| self.is_cursor_stop(i))
    }

    fn prev_cursor_stop(&self, from: usize) -> Option<usize> {
        (0..from).rev().find(|&i| self.is_cursor_stop(i))
    }

    /// Build the rendered Sessions rows, inserting group headers and dropping
    /// the members of folded groups. When no session is grouped the result is a
    /// flat list of [`SessionRow::Session`] rows (no headers), matching the
    /// pre-grouping behaviour exactly.
    pub fn session_rows(&self) -> Vec<SessionRow> {
        let any_grouped = self.any_grouped();
        let mut rows = Vec::with_capacity(self.sessions.len());
        let mut current: Option<&Option<String>> = None;
        for (index, session) in self.sessions.iter().enumerate() {
            let collapsed = any_grouped && self.collapsed_groups.contains(&session.group);
            if any_grouped && current != Some(&session.group) {
                let count = self
                    .sessions
                    .iter()
                    .filter(|s| s.group == session.group)
                    .count();
                rows.push(SessionRow::Header {
                    group: session.group.clone(),
                    count,
                    collapsed,
                });
                current = Some(&session.group);
            }
            if !collapsed {
                rows.push(SessionRow::Session { index });
            }
        }
        rows
    }

    /// Advance to the next [`SessionSort`] and re-sort the list in place,
    /// keeping the currently-highlighted session highlighted.
    pub fn cycle_session_sort(&mut self) {
        self.session_sort = self.session_sort.next();
        self.resort_sessions_preserve_selection();
    }

    fn resort_sessions_preserve_selection(&mut self) {
        let current_name = self
            .sessions
            .get(self.selected_session)
            .map(|s| s.name.clone());

        self.order_sessions();

        if let Some(name) = current_name
            && let Some(idx) = self.sessions.iter().position(|s| s.name == name)
        {
            self.selected_session = idx;
            self.multi_session = self.multi_session.min(self.sessions.len().saturating_sub(1));
            self.session_list_state.select(Some(idx));
        }
    }

    pub fn update_pane_content(&mut self, content: String) {
        self.pane_content_parsed = content.as_bytes().into_text().ok();
        self.pane_content = content;
    }

    pub fn set_error(&mut self, message: String) {
        self.last_error = Some(message);
    }

    pub fn validate_selections(&mut self) {
        if !self.sessions.is_empty() {
            self.selected_session = self.selected_session.min(self.sessions.len() - 1);
            self.multi_session = self.multi_session.min(self.sessions.len() - 1);

            if let Some(session) = self.sessions.get(self.selected_session)
                && !session.windows.is_empty()
            {
                self.selected_window = self.selected_window.min(session.windows.len() - 1);
                if let Some(window) = session.windows.get(self.selected_window)
                    && !window.panes.is_empty()
                {
                    self.selected_pane = self.selected_pane.min(window.panes.len() - 1);
                }
            }

            if let Some(session) = self.sessions.get(self.multi_session)
                && !session.windows.is_empty()
            {
                self.multi_window = self.multi_window.min(session.windows.len() - 1);
            }

            self.session_list_state.select(Some(self.selected_session));
            self.window_list_state.select(Some(self.selected_window));
            self.pane_list_state.select(Some(self.selected_pane));
        } else {
            self.session_list_state.select(None);
            self.window_list_state.select(None);
            self.pane_list_state.select(None);
        }
    }

    // =========================================================================
    // TreeView Navigation
    // =========================================================================

    pub fn get_selected_pane_target(&self) -> Option<String> {
        let session = self.sessions.get(self.selected_session)?;
        let window = session.windows.get(self.selected_window)?;
        let pane = window.panes.get(self.selected_pane)?;
        Some(format!("{}:{}.{}", session.name, window.index, pane.index))
    }

    pub fn get_selected_pane_target_with_capture_range(&self) -> Option<(String, i32, i32)> {
        let session = self.sessions.get(self.selected_session)?;
        let window = session.windows.get(self.selected_window)?;
        let pane = window.panes.get(self.selected_pane)?;
        let target = format!("{}:{}.{}", session.name, window.index, pane.index);
        let height = i32::try_from(pane.height).unwrap_or(i32::MAX);
        let start = 0;
        let end = height;
        Some((target, start, end))
    }

    pub fn tree_move_up(&mut self) {
        match self.focus {
            Focus::Sessions => {
                if let Some(prev) = self.prev_cursor_stop(self.selected_session) {
                    self.selected_session = prev;
                    self.selected_window = 0;
                    self.selected_pane = 0;
                    self.window_list_state.select(Some(0));
                    self.pane_list_state.select(Some(0));
                }
                self.session_list_state.select(Some(self.selected_session));
            }
            Focus::Windows => {
                if self.selected_window > 0 {
                    self.selected_window -= 1;
                    self.selected_pane = 0;
                    self.pane_list_state.select(Some(0));
                }
                self.window_list_state.select(Some(self.selected_window));
            }
            Focus::Panes => {
                if self.selected_pane > 0 {
                    self.selected_pane -= 1;
                }
                self.pane_list_state.select(Some(self.selected_pane));
            }
        }
    }

    pub fn tree_move_down(&mut self) {
        match self.focus {
            Focus::Sessions => {
                if let Some(next) = self.next_cursor_stop(self.selected_session) {
                    self.selected_session = next;
                    self.selected_window = 0;
                    self.selected_pane = 0;
                    self.window_list_state.select(Some(0));
                    self.pane_list_state.select(Some(0));
                }
                self.session_list_state.select(Some(self.selected_session));
            }
            Focus::Windows => {
                if let Some(session) = self.sessions.get(self.selected_session)
                    && self.selected_window < session.windows.len().saturating_sub(1)
                {
                    self.selected_window += 1;
                    self.selected_pane = 0;
                    self.pane_list_state.select(Some(0));
                }
                self.window_list_state.select(Some(self.selected_window));
            }
            Focus::Panes => {
                if let Some(session) = self.sessions.get(self.selected_session)
                    && let Some(window) = session.windows.get(self.selected_window)
                    && self.selected_pane < window.panes.len().saturating_sub(1)
                {
                    self.selected_pane += 1;
                }
                self.pane_list_state.select(Some(self.selected_pane));
            }
        }
    }

    pub fn tree_next_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Sessions => Focus::Windows,
            Focus::Windows => Focus::Panes,
            Focus::Panes => Focus::Sessions,
        };
    }

    pub fn tree_prev_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Sessions => Focus::Panes,
            Focus::Windows => Focus::Sessions,
            Focus::Panes => Focus::Windows,
        };
    }

    // =========================================================================
    // MultiPreview Navigation
    // =========================================================================

    pub fn get_multi_selected_target(&self) -> Option<String> {
        let session = self.sessions.get(self.multi_session)?;
        let window = session.windows.get(self.multi_window)?;
        // Use window-level target (tmux will switch to the active pane)
        Some(format!("{}:{}", session.name, window.index))
    }

    pub fn multi_move_left(&mut self) {
        if self.multi_session > 0 {
            self.multi_session -= 1;
            // Reset window selection for new session
            self.multi_window = 0;
        }
    }

    pub fn multi_move_right(&mut self) {
        if self.multi_session < self.sessions.len().saturating_sub(1) {
            self.multi_session += 1;
            // Reset window selection for new session
            self.multi_window = 0;
        }
    }

    pub fn multi_move_up(&mut self) {
        if self.multi_window > 0 {
            self.multi_window -= 1;
        }
    }

    pub fn multi_move_down(&mut self) {
        if let Some(session) = self.sessions.get(self.multi_session)
            && self.multi_window < session.windows.len().saturating_sub(1)
        {
            self.multi_window += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str) -> TmuxSession {
        TmuxSession {
            name: name.to_string(),
            windows: Vec::new(),
            has_claude: false,
            claude_state: None,
            last_attached: 0,
            activity: 0,
            group: None,
        }
    }

    /// Build a UIState with an in-memory (no-disk) group store and the given
    /// assignments, then load `names` as the session list.
    fn state_with(names: &[&str], groups: &[(&str, &str)]) -> UIState {
        let mut state = UIState::new(Config::default());
        state.groups = GroupStore::default();
        for (sess, grp) in groups {
            state.groups.set(sess, Some(grp));
        }
        state.update_sessions(names.iter().map(|n| session(n)).collect());
        state
    }

    /// Build a session with a single window holding the given panes, each
    /// described as `(pane_id, index, claude_state, state_since)`.
    fn session_with_panes(
        name: &str,
        panes: &[(&str, u32, Option<ClaudeState>, Option<i64>)],
    ) -> TmuxSession {
        let panes = panes
            .iter()
            .map(|(id, index, state, since)| TmuxPane {
                id: id.to_string(),
                index: *index,
                width: 80,
                height: 24,
                active: false,
                current_command: String::new(),
                pid: 0,
                has_claude: state.is_some(),
                claude_state: *state,
                claude_activity: None,
                claude_state_since: *since,
                claude_cwd: None,
            })
            .collect();
        let mut s = session(name);
        s.windows = vec![TmuxWindow {
            index: 0,
            name: "w".to_string(),
            panes,
            has_claude: false,
            claude_state: None,
        }];
        s
    }

    #[test]
    fn dashboard_rows_sort_by_attention_then_age() {
        let mut state = UIState::new(Config::default());
        state.groups = GroupStore::default();
        // working(old) / working(new) / waiting / done — plus a non-Claude pane.
        state.sessions = vec![session_with_panes(
            "s",
            &[
                ("%1", 0, Some(ClaudeState::Working), Some(100)), // older working
                ("%2", 1, Some(ClaudeState::Working), Some(200)), // newer working
                ("%3", 2, Some(ClaudeState::Waiting), Some(150)),
                ("%4", 3, Some(ClaudeState::Done), Some(10)),
                ("%5", 4, None, None), // no Claude -> excluded
            ],
        )];

        let rows = state.dashboard_rows();
        // Waiting first; then the two Working (older = larger elapsed first);
        // then Done. The non-Claude pane is excluded.
        let order: Vec<&str> = rows.iter().map(|r| r.pane_id.as_str()).collect();
        assert_eq!(order, vec!["%3", "%1", "%2", "%4"]);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].target, "s:0.2");
    }

    #[test]
    fn ungrouped_sessions_have_no_headers() {
        let state = state_with(&["a", "b", "c"], &[]);
        let rows = state.session_rows();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| matches!(r, SessionRow::Session { .. })));
    }

    #[test]
    fn grouped_sessions_cluster_with_ungrouped_last() {
        // a, c -> "work"; b ungrouped. Names tie-break ascending within a group.
        let state = state_with(&["a", "b", "c"], &[("a", "work"), ("c", "work")]);
        let ordered: Vec<&str> = state.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(ordered, vec!["a", "c", "b"]);
    }

    #[test]
    fn rows_insert_one_header_per_group() {
        let state = state_with(
            &["a", "b", "c"],
            &[("a", "work"), ("c", "work"), ("b", "play")],
        );
        let rows = state.session_rows();
        // play(b) sorts before work(a,c) alphabetically; ungrouped bucket absent.
        let labels: Vec<String> = rows
            .iter()
            .filter_map(|r| match r {
                SessionRow::Header { group, count, .. } => {
                    Some(format!("{}:{}", group.as_deref().unwrap_or("none"), count))
                }
                SessionRow::Session { .. } => None,
            })
            .collect();
        assert_eq!(labels, vec!["play:1".to_string(), "work:2".to_string()]);
    }

    #[test]
    fn ungrouped_bucket_gets_a_header_when_mixed() {
        let state = state_with(&["a", "b"], &[("a", "work")]);
        let rows = state.session_rows();
        let has_ungrouped_header = rows.iter().any(|r| {
            matches!(r, SessionRow::Header { group: None, count, .. } if *count == 1)
        });
        assert!(has_ungrouped_header);
    }

    #[test]
    fn folding_hides_members_but_keeps_header() {
        // work: a, c ; play: b. Select "a" (in work) and fold its group.
        let mut state = state_with(
            &["a", "b", "c"],
            &[("a", "work"), ("c", "work"), ("b", "play")],
        );
        let work_idx = state.sessions.iter().position(|s| s.name == "a").unwrap();
        state.selected_session = work_idx;
        state.toggle_fold_current_group();

        let rows = state.session_rows();
        // No "work" member sessions remain visible, but its header stays
        // (now marked collapsed); play's member is still shown.
        let work_collapsed = rows.iter().any(|r| matches!(
            r,
            SessionRow::Header { group: Some(g), collapsed: true, .. } if g == "work"
        ));
        assert!(work_collapsed);
        let visible_sessions: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                SessionRow::Session { index } => Some(state.sessions[*index].name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(visible_sessions, vec!["b"]);
        // Selection parks on the folded group's head, so the cursor sits on the
        // (collapsed) "work" header and can re-open it.
        assert!(state.selection_on_folded_header());
        assert_eq!(state.sessions[state.selected_session].group.as_deref(), Some("work"));

        // Toggling again from the folded header re-expands the group — this is
        // the regression that previously had no way to recover.
        state.toggle_fold_current_group();
        let rows = state.session_rows();
        let names: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                SessionRow::Session { index } => Some(state.sessions[*index].name.as_str()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"a") && names.contains(&"c"));
        assert!(!state.selection_on_folded_header());
    }

    #[test]
    fn navigation_lands_on_folded_group_then_reopens() {
        let mut state = state_with(
            &["a", "b", "c"],
            &[("a", "work"), ("c", "work"), ("b", "play")],
        );
        // Order is play(b), work(a,c). Fold work (selection parks on work head).
        state.selected_session = state.sessions.iter().position(|s| s.name == "a").unwrap();
        state.toggle_fold_current_group();
        // From the visible "b", moving down stops on the folded work header
        // rather than skipping it entirely.
        state.selected_session = state.sessions.iter().position(|s| s.name == "b").unwrap();
        state.tree_move_down();
        assert!(state.selection_on_folded_header());
        assert_eq!(state.sessions[state.selected_session].group.as_deref(), Some("work"));
        // And `za` there expands it back.
        state.toggle_fold_current_group();
        assert!(!state.selection_on_folded_header());
    }

    #[test]
    fn fold_is_noop_without_groups() {
        let mut state = state_with(&["a", "b"], &[]);
        state.toggle_fold_current_group();
        let rows = state.session_rows();
        // No headers, all sessions still visible.
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| matches!(r, SessionRow::Session { .. })));
    }

    #[test]
    fn assigning_group_updates_store_and_order() {
        let mut state = state_with(&["a", "b"], &[]);
        state.selected_session = 1; // "b"
        state.assign_selected_group(Some("work".to_string()));
        assert_eq!(state.groups.group_of("b"), Some("work".to_string()));
        // "b" is now grouped and clusters above the ungrouped "a".
        let ordered: Vec<&str> = state.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(ordered, vec!["b", "a"]);
        // Selection still tracks "b" after the reorder.
        assert_eq!(state.sessions[state.selected_session].name, "b");
    }

    #[test]
    fn group_popup_lists_existing_groups_and_highlights_current() {
        let state = state_with(
            &["a", "b", "c"],
            &[("a", "work"), ("b", "personal"), ("c", "work")],
        );
        // Sorted, deduplicated existing groups.
        assert_eq!(state.groups.group_names(), vec!["personal", "work"]);

        let mut state = state;
        state.selected_session = state.sessions.iter().position(|s| s.name == "b").unwrap();
        state.open_group_session_popup();
        assert_eq!(state.popup_mode, Some(PopupMode::GroupSession));
        // "b" is in "personal", so that entry starts highlighted.
        assert_eq!(
            state.selected_group_choice(),
            GroupChoice::Existing("personal".to_string())
        );
        // Entries: 2 groups + Ungrouped + New.
        assert_eq!(state.group_choice_count(), 4);
    }

    #[test]
    fn group_popup_defaults_to_ungrouped_for_ungrouped_session() {
        let mut state = state_with(&["a", "b"], &[("b", "work")]);
        state.selected_session = state.sessions.iter().position(|s| s.name == "a").unwrap();
        state.open_group_session_popup();
        // Index sits on the "Ungrouped" entry, just past the single group.
        assert_eq!(state.selected_group_choice(), GroupChoice::Ungrouped);
    }

    #[test]
    fn group_choice_navigation_wraps_and_reaches_new() {
        let mut state = state_with(&["a"], &[("a", "work")]);
        state.open_group_session_popup();
        // Entries: ["work", Ungrouped, New]. Starts on "work".
        assert_eq!(
            state.selected_group_choice(),
            GroupChoice::Existing("work".to_string())
        );
        state.group_choice_up(); // wraps to last entry
        assert_eq!(state.selected_group_choice(), GroupChoice::New);
        state.group_choice_down(); // wraps back to first
        assert_eq!(
            state.selected_group_choice(),
            GroupChoice::Existing("work".to_string())
        );
        state.group_choice_down();
        assert_eq!(state.selected_group_choice(), GroupChoice::Ungrouped);
    }

    #[test]
    fn input_handles_multibyte_chars_without_panic() {
        let mut state = UIState::new(Config::default());
        // 日本語を複数文字入力（旧実装ではバイト境界パニックしていた）
        state.input_char('あ');
        state.input_char('い');
        state.input_char('う');
        assert_eq!(state.input_buffer, "あいう");
        assert_eq!(state.input_cursor, 3);
    }

    #[test]
    fn input_cursor_movement_and_editing_with_multibyte() {
        let mut state = UIState::new(Config::default());
        for c in "あいう".chars() {
            state.input_char(c);
        }
        // 左へ2つ移動 → カーソルは「い」の前
        state.input_move_left();
        state.input_move_left();
        assert_eq!(state.input_cursor, 1);
        // カーソル位置に「ん」を挿入
        state.input_char('ん');
        assert_eq!(state.input_buffer, "あんいう");
        assert_eq!(state.input_cursor, 2);
        // backspace で「ん」を削除
        state.input_backspace();
        assert_eq!(state.input_buffer, "あいう");
        assert_eq!(state.input_cursor, 1);
        // delete でカーソル位置の「い」を削除
        state.input_delete();
        assert_eq!(state.input_buffer, "あう");
        assert_eq!(state.input_cursor, 1);
    }

    #[test]
    fn input_move_end_uses_char_count() {
        let mut state = UIState::new(Config::default());
        for c in "あい".chars() {
            state.input_char(c);
        }
        state.input_move_home();
        assert_eq!(state.input_cursor, 0);
        state.input_move_end();
        assert_eq!(state.input_cursor, 2);
    }

    #[test]
    fn input_char_limited_caps_char_count() {
        let mut state = UIState::new(Config::default());
        for _ in 0..40 {
            state.input_char_limited('a', SESSION_NAME_MAX_LEN);
        }
        assert_eq!(state.input_buffer.chars().count(), SESSION_NAME_MAX_LEN);
    }

    #[test]
    fn input_char_limited_counts_chars_not_bytes() {
        let mut state = UIState::new(Config::default());
        // マルチバイト文字でもバイト長ではなく文字数で制限される
        for _ in 0..40 {
            state.input_char_limited('あ', SESSION_NAME_MAX_LEN);
        }
        assert_eq!(state.input_buffer.chars().count(), SESSION_NAME_MAX_LEN);
    }
}
