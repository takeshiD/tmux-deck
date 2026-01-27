use std::time::{Duration, Instant};

use ratatui::widgets::ListState;

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
}

/// Represents a tmux window with captured content
#[derive(Debug, Clone)]
pub struct TmuxWindow {
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub panes: Vec<TmuxPane>,
    /// Captured content of the active pane (for preview)
    // pub content: String,
    /// Width of the active pane
    pub pane_width: u32,
    /// Height of the active pane
    pub pane_height: u32,
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
    pub windows: Vec<TmuxWindow>,
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

/// Popup mode for session operations
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PopupMode {
    /// Creating a new session
    NewSession,
    /// Renaming the selected session
    RenameSession,
    /// Confirming session kill
    ConfirmKill,
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

    // MultiPreview state (session_idx, window_idx)
    pub multi_session: usize,
    pub multi_window: usize,

    // Shared state
    pub pane_content: String,
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

            multi_session: 0,
            multi_window: 0,

            pane_content: String::new(),
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
        self.sessions = sessions;
        self.validate_selections();
        self.last_error = None;
    }

    pub fn update_pane_content(&mut self, content: String) {
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
                if self.selected_session > 0 {
                    self.selected_session -= 1;
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
                if self.selected_session < self.sessions.len().saturating_sub(1) {
                    self.selected_session += 1;
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
