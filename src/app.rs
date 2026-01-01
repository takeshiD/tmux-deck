use std::process::Command;
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
    pub width: u32,
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
    pub content: String,
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
// Application State
// =============================================================================

pub struct App {
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
    pub interval: Duration,
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub input_cursor: usize,

    // Popup state
    pub popup_mode: Option<PopupMode>,
    pub confirm_yes_selected: bool,
}

impl App {
    pub fn new(interval_ms: u64) -> Self {
        let mut app = Self {
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
        app.session_list_state.select(Some(0));
        app.window_list_state.select(Some(0));
        app.pane_list_state.select(Some(0));
        app
    }

    // =========================================================================
    // View Mode Switching
    // =========================================================================

    pub fn handle_space_press(&mut self) -> bool {
        let now = Instant::now();
        if let Some(last) = self.last_space_press {
            if now.duration_since(last) < Duration::from_millis(300) {
                // Double space detected
                self.toggle_view_mode();
                self.last_space_press = None;
                return true;
            }
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

    pub fn send_input_to_pane(&mut self) {
        if let Some(target) = self.get_current_target() {
            let message = self.input_buffer.clone();
            let result = Command::new("tmux")
                .args(["send-keys", "-t", &target, &message, "Enter"])
                .output();

            if let Err(e) = result {
                self.last_error = Some(format!("Failed to send keys: {}", e));
            }
        }
        self.exit_input_mode();
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

    pub fn confirm_new_session(&mut self) {
        let session_name = self.input_buffer.trim().to_string();
        if !session_name.is_empty() {
            let result = Command::new("tmux")
                .args(["new-session", "-d", "-s", &session_name])
                .output();

            match result {
                Ok(output) => {
                    if output.status.success() {
                        self.refresh_all();
                        // Select the new session
                        if let Some(idx) = self.sessions.iter().position(|s| s.name == session_name) {
                            self.selected_session = idx;
                            self.session_list_state.select(Some(idx));
                        }
                    } else {
                        self.last_error = Some(String::from_utf8_lossy(&output.stderr).to_string());
                    }
                }
                Err(e) => {
                    self.last_error = Some(format!("Failed to create session: {}", e));
                }
            }
        }
        self.close_popup();
    }

    pub fn confirm_rename_session(&mut self) {
        let new_name = self.input_buffer.trim().to_string();
        if !new_name.is_empty() {
            if let Some(session) = self.sessions.get(self.selected_session) {
                let old_name = session.name.clone();
                let result = Command::new("tmux")
                    .args(["rename-session", "-t", &old_name, &new_name])
                    .output();

                match result {
                    Ok(output) => {
                        if output.status.success() {
                            self.refresh_all();
                        } else {
                            self.last_error = Some(String::from_utf8_lossy(&output.stderr).to_string());
                        }
                    }
                    Err(e) => {
                        self.last_error = Some(format!("Failed to rename session: {}", e));
                    }
                }
            }
        }
        self.close_popup();
    }

    pub fn confirm_kill_session(&mut self) {
        if self.confirm_yes_selected {
            if let Some(session) = self.sessions.get(self.selected_session) {
                let session_name = session.name.clone();
                let result = Command::new("tmux")
                    .args(["kill-session", "-t", &session_name])
                    .output();

                match result {
                    Ok(output) => {
                        if output.status.success() {
                            self.refresh_all();
                            // Adjust selection if needed
                            if !self.sessions.is_empty() {
                                self.selected_session = self.selected_session.min(self.sessions.len().saturating_sub(1));
                                self.session_list_state.select(Some(self.selected_session));
                            }
                        } else {
                            self.last_error = Some(String::from_utf8_lossy(&output.stderr).to_string());
                        }
                    }
                    Err(e) => {
                        self.last_error = Some(format!("Failed to kill session: {}", e));
                    }
                }
            }
        }
        self.close_popup();
    }

    pub fn toggle_confirm_selection(&mut self) {
        self.confirm_yes_selected = !self.confirm_yes_selected;
    }

    // =========================================================================
    // Data Refresh
    // =========================================================================

    pub fn refresh_all(&mut self) {
        self.sessions.clear();

        // Get all sessions
        let sessions_output = Command::new("tmux")
            .args([
                "list-sessions",
                "-F",
                "#{session_name}:#{session_attached}",
            ])
            .output();

        let sessions_str = match sessions_output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).to_string()
            }
            Ok(output) => {
                self.last_error = Some(String::from_utf8_lossy(&output.stderr).to_string());
                return;
            }
            Err(e) => {
                self.last_error = Some(format!("Failed to list sessions: {}", e));
                return;
            }
        };

        for session_line in sessions_str.lines() {
            let parts: Vec<&str> = session_line.split(':').collect();
            if parts.len() >= 2 {
                let session_name = parts[0].to_string();
                let attached = parts[1] == "1";

                let mut session = TmuxSession {
                    name: session_name.clone(),
                    attached,
                    windows: Vec::new(),
                };

                // Get windows for this session
                let windows_output = Command::new("tmux")
                    .args([
                        "list-windows",
                        "-t",
                        &session_name,
                        "-F",
                        "#{window_index}:#{window_name}:#{window_active}",
                    ])
                    .output();

                if let Ok(output) = windows_output {
                    if output.status.success() {
                        let windows_str = String::from_utf8_lossy(&output.stdout);
                        for window_line in windows_str.lines() {
                            let w_parts: Vec<&str> = window_line.split(':').collect();
                            if w_parts.len() >= 3 {
                                let window_index: u32 = w_parts[0].parse().unwrap_or(0);
                                let window_name = w_parts[1].to_string();
                                let window_active = w_parts[2] == "1";

                                let mut window = TmuxWindow {
                                    index: window_index,
                                    name: window_name,
                                    active: window_active,
                                    panes: Vec::new(),
                                    content: String::new(),
                                    pane_width: 80,
                                    pane_height: 24,
                                };

                                // Get panes for this window
                                let panes_output = Command::new("tmux")
                                    .args([
                                        "list-panes",
                                        "-t",
                                        &format!("{}:{}", session_name, window_index),
                                        "-F",
                                        "#{pane_id}:#{pane_index}:#{pane_width}:#{pane_height}:#{pane_active}:#{pane_current_command}",
                                    ])
                                    .output();

                                if let Ok(p_output) = panes_output {
                                    if p_output.status.success() {
                                        let panes_str = String::from_utf8_lossy(&p_output.stdout);
                                        for pane_line in panes_str.lines() {
                                            let p_parts: Vec<&str> = pane_line.split(':').collect();
                                            if p_parts.len() >= 6 {
                                                let pane_id = p_parts[0].to_string();
                                                let pane_index: u32 = p_parts[1].parse().unwrap_or(0);
                                                let width: u32 = p_parts[2].parse().unwrap_or(80);
                                                let height: u32 = p_parts[3].parse().unwrap_or(24);
                                                let pane_active = p_parts[4] == "1";
                                                let current_command = p_parts[5].to_string();

                                                let pane = TmuxPane {
                                                    id: pane_id,
                                                    index: pane_index,
                                                    width,
                                                    height,
                                                    active: pane_active,
                                                    current_command,
                                                };

                                                // Store active pane dimensions
                                                if pane_active {
                                                    window.pane_width = width;
                                                    window.pane_height = height;
                                                }

                                                window.panes.push(pane);
                                            }
                                        }
                                    }
                                }

                                // Capture content of active pane for this window
                                let target = format!("{}:{}", session_name, window_index);
                                if let Ok(output) = Command::new("tmux")
                                    .args(["capture-pane", "-e", "-p", "-J", "-t", &target])
                                    .output()
                                {
                                    if output.status.success() {
                                        window.content = String::from_utf8_lossy(&output.stdout).to_string();
                                    }
                                }

                                session.windows.push(window);
                            }
                        }
                    }
                }

                self.sessions.push(session);
            }
        }

        // Ensure selection is valid
        self.validate_selections();
        self.last_error = None;
    }

    fn validate_selections(&mut self) {
        if !self.sessions.is_empty() {
            self.selected_session = self.selected_session.min(self.sessions.len() - 1);
            self.multi_session = self.multi_session.min(self.sessions.len() - 1);

            if let Some(session) = self.sessions.get(self.selected_session) {
                if !session.windows.is_empty() {
                    self.selected_window = self.selected_window.min(session.windows.len() - 1);
                    if let Some(window) = session.windows.get(self.selected_window) {
                        if !window.panes.is_empty() {
                            self.selected_pane = self.selected_pane.min(window.panes.len() - 1);
                        }
                    }
                }
            }

            if let Some(session) = self.sessions.get(self.multi_session) {
                if !session.windows.is_empty() {
                    self.multi_window = self.multi_window.min(session.windows.len() - 1);
                }
            }
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

    pub fn capture_selected_pane(&mut self) {
        if let Some(target) = self.get_selected_pane_target() {
            let result = Command::new("tmux")
                .args(["capture-pane", "-e", "-p", "-J", "-t", &target])
                .output();

            match result {
                Ok(output) => {
                    if output.status.success() {
                        self.pane_content = String::from_utf8_lossy(&output.stdout).to_string();
                    } else {
                        self.pane_content = format!(
                            "Error capturing pane: {}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                }
                Err(e) => {
                    self.pane_content = format!("Failed to capture pane: {}", e);
                }
            }
        } else {
            self.pane_content = "No pane selected".to_string();
        }
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
                if let Some(session) = self.sessions.get(self.selected_session) {
                    if self.selected_window < session.windows.len().saturating_sub(1) {
                        self.selected_window += 1;
                        self.selected_pane = 0;
                        self.pane_list_state.select(Some(0));
                    }
                }
                self.window_list_state.select(Some(self.selected_window));
            }
            Focus::Panes => {
                if let Some(session) = self.sessions.get(self.selected_session) {
                    if let Some(window) = session.windows.get(self.selected_window) {
                        if self.selected_pane < window.panes.len().saturating_sub(1) {
                            self.selected_pane += 1;
                        }
                    }
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
        if let Some(session) = self.sessions.get(self.multi_session) {
            if self.multi_window < session.windows.len().saturating_sub(1) {
                self.multi_window += 1;
            }
        }
    }

    // =========================================================================
    // Switch to Pane
    // =========================================================================

    /// Switch to the selected pane and return true if successful
    pub fn switch_to_selected_pane(&self) -> bool {
        if let Some(target) = self.get_current_target() {
            if let Ok(output) = Command::new("tmux")
                .args(["switch-client", "-t", &target])
                .output()
            {
                return output.status.success();
            }
        }
        false
    }
}
