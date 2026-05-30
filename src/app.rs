use std::collections::HashSet;
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText;
use ratatui::text::Text;
use ratatui::widgets::ListState;

use crate::group::GroupStore;

/// Label shown for the implicit group of sessions that have not been assigned
/// to any user group. Only rendered when at least one session *is* grouped.
pub const UNGROUPED_LABEL: &str = "Ungrouped";

// =============================================================================
// Data Structures
// =============================================================================

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
}

/// Represents a tmux window with captured content
#[derive(Debug, Clone)]
pub struct TmuxWindow {
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub panes: Vec<TmuxPane>,
    /// True if any pane in this window has claude running.
    pub has_claude: bool,
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
    pub attached: bool,
    /// True if the session has activity newer than its last_attached and
    /// is not currently attached — i.e. something happened in there that
    /// the user has not seen yet.
    pub unread: bool,
    pub windows: Vec<TmuxWindow>,
    /// True if any window in this session has claude running.
    pub has_claude: bool,
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
    /// Assigning the selected session to a tmux-deck group
    GroupSession,
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

    // Shared state
    pub pane_content: String,
    pub pane_content_parsed: Option<Text<'static>>,
    pub last_error: Option<String>,
    #[allow(dead_code)]
    pub interval: Duration,
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub input_cursor: usize,

    // Popup state
    pub popup_mode: Option<PopupMode>,
    pub confirm_yes_selected: bool,
}

impl UIState {
    pub fn new(interval_ms: u64) -> Self {
        let mut state = Self {
            view_mode: ViewMode::TreeView,
            last_space_press: None,

            sessions: Vec::new(),
            selected_session: 0,
            selected_window: 0,
            selected_pane: 0,
            focus: Focus::Sessions,
            session_list_state: ListState::default(),
            window_list_state: ListState::default(),
            pane_list_state: ListState::default(),
            session_sort: SessionSort::default(),

            groups: GroupStore::load(),
            collapsed_groups: HashSet::new(),
            pending_z: false,

            multi_session: 0,
            multi_window: 0,

            pane_content: String::new(),
            pane_content_parsed: None,
            last_error: None,
            interval: Duration::from_millis(interval_ms),
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            input_cursor: 0,

            popup_mode: None,
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

    pub fn handle_space_press(&mut self) -> bool {
        let now = Instant::now();
        if let Some(last) = self.last_space_press
            && now.duration_since(last) < Duration::from_millis(300)
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
        };
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
        }
    }

    pub fn input_char(&mut self, c: char) {
        self.input_buffer.insert(self.input_cursor, c);
        self.input_cursor += 1;
    }

    pub fn input_backspace(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
            self.input_buffer.remove(self.input_cursor);
        }
    }

    pub fn input_delete(&mut self) {
        if self.input_cursor < self.input_buffer.len() {
            self.input_buffer.remove(self.input_cursor);
        }
    }

    pub fn input_move_left(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
        }
    }

    pub fn input_move_right(&mut self) {
        if self.input_cursor < self.input_buffer.len() {
            self.input_cursor += 1;
        }
    }

    pub fn input_move_home(&mut self) {
        self.input_cursor = 0;
    }

    pub fn input_move_end(&mut self) {
        self.input_cursor = self.input_buffer.len();
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
            self.input_cursor = self.input_buffer.len();
        }
    }

    pub fn open_group_session_popup(&mut self) {
        if let Some(session) = self.sessions.get(self.selected_session) {
            self.popup_mode = Some(PopupMode::GroupSession);
            // Prefill with the current group so editing/clearing is natural.
            self.input_buffer = session.group.clone().unwrap_or_default();
            self.input_cursor = self.input_buffer.len();
        }
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
            attached: false,
            unread: false,
            windows: Vec::new(),
            has_claude: false,
            last_attached: 0,
            activity: 0,
            group: None,
        }
    }

    /// Build a UIState with an in-memory (no-disk) group store and the given
    /// assignments, then load `names` as the session list.
    fn state_with(names: &[&str], groups: &[(&str, &str)]) -> UIState {
        let mut state = UIState::new(100);
        state.groups = GroupStore::default();
        for (sess, grp) in groups {
            state.groups.set(sess, Some(grp));
        }
        state.update_sessions(names.iter().map(|n| session(n)).collect());
        state
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
}
