use std::io;
use std::time::Duration;

use color_eyre::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::{mpsc, oneshot};

use crate::actor::messages::{RefreshControl, TmuxCommand, TmuxResponse, UIEvent};
use crate::app::{InputMode, PopupMode, UIState, ViewMode};
use crate::ui::render_ui;

// =============================================================================
// Key Event Poller (runs in dedicated blocking thread)
// =============================================================================

fn spawn_key_event_poller(key_tx: mpsc::Sender<Event>) {
    std::thread::spawn(move || {
        loop {
            // Poll with moderate timeout for balance between responsiveness and CPU usage
            if event::poll(Duration::from_millis(20)).unwrap_or(false)
                && let Ok(evt) = event::read()
                && key_tx.blocking_send(evt).is_err()
            {
                // Receiver dropped, exit thread
                break;
            }
        }
    });
}

// =============================================================================
// UIActor
// =============================================================================

pub struct UIActor {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    state: UIState,
    tmux_cmd_tx: mpsc::Sender<TmuxCommand>,
    tmux_res_rx: mpsc::Receiver<TmuxResponse>,
    ui_event_rx: mpsc::Receiver<UIEvent>,
    key_rx: mpsc::Receiver<Event>,
    refresh_control: RefreshControl,
}

impl UIActor {
    pub fn new(
        terminal: Terminal<CrosstermBackend<io::Stdout>>,
        state: UIState,
        tmux_cmd_tx: mpsc::Sender<TmuxCommand>,
        tmux_res_rx: mpsc::Receiver<TmuxResponse>,
        ui_event_rx: mpsc::Receiver<UIEvent>,
        refresh_control: RefreshControl,
    ) -> Self {
        // Spawn dedicated key event poller thread
        let (key_tx, key_rx) = mpsc::channel::<Event>(64);
        spawn_key_event_poller(key_tx);

        Self {
            terminal,
            state,
            tmux_cmd_tx,
            tmux_res_rx,
            ui_event_rx,
            key_rx,
            refresh_control,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        // Request initial data
        let _ = self.tmux_cmd_tx.send(TmuxCommand::RefreshAll).await;

        // Initial render before entering event loop
        self.terminal.draw(|frame| {
            render_ui(frame, &mut self.state);
        })?;

        loop {
            // Use select to handle multiple event sources
            // biased; ensures key events are checked first (top-to-bottom priority)
            tokio::select! {
                // biased;

                // Key events from dedicated poller thread (highest priority)
                Some(event) = self.key_rx.recv() => {
                    if self.handle_key_event(event).await? {
                        break; // Exit requested
                    }
                }

                // TmuxActor responses
                Some(response) = self.tmux_res_rx.recv() => {
                    self.handle_tmux_response(response);
                }

                // RefreshActor events
                Some(event) = self.ui_event_rx.recv() => {
                    match event {
                        UIEvent::Tick => {
                            // Request pane capture if in TreeView mode
                            if self.state.view_mode == ViewMode::TreeView
                                && let Some(target) = self.state.get_selected_pane_target()
                            {
                                let _ = self.tmux_cmd_tx.send(TmuxCommand::CapturePane { target }).await;
                            }
                        }
                        // UIEvent::RequestCapture => {
                        //     if let Some(target) = self.state.get_selected_pane_target() {
                        //         let _ = self.tmux_tx.send(TmuxCommand::CapturePane { target }).await;
                        //     }
                        // }
                        UIEvent::Shutdown => {
                            break;
                        }
                        _ => (),
                    }
                }
            }

            // Render UI after processing event (event-driven rendering)
            self.terminal.draw(|frame| {
                render_ui(frame, &mut self.state);
            })?;
        }

        Ok(())
    }

    async fn handle_key_event(&mut self, event: Event) -> Result<bool> {
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                return Ok(false);
            }

            // Handle popup mode first
            if let Some(popup_mode) = self.state.popup_mode {
                return self.handle_popup_key(key, popup_mode).await;
            }

            // Handle input mode
            match self.state.input_mode {
                InputMode::Normal => {
                    return self.handle_normal_mode_key(key).await;
                }
                InputMode::Input => {
                    self.handle_input_mode_key(key).await?;
                }
            }
        }
        Ok(false)
    }

    async fn handle_popup_key(
        &mut self,
        key: event::KeyEvent,
        popup_mode: PopupMode,
    ) -> Result<bool> {
        match popup_mode {
            PopupMode::NewSession | PopupMode::RenameSession => {
                match key.code {
                    KeyCode::Esc => {
                        self.state.close_popup();
                        self.refresh_control.resume();
                    }
                    KeyCode::Enter => {
                        if popup_mode == PopupMode::NewSession {
                            let name = self.state.get_new_session_name();
                            if !name.is_empty() {
                                let _ = self.tmux_cmd_tx.send(TmuxCommand::NewSession { name }).await;
                            }
                        } else if let Some((old_name, new_name)) =
                            self.state.get_rename_session_info()
                        {
                            let _ = self
                                .tmux_cmd_tx
                                .send(TmuxCommand::RenameSession { old_name, new_name })
                                .await;
                        }
                        self.state.close_popup();
                        self.refresh_control.resume();
                        // Refresh after operation
                        let _ = self.tmux_cmd_tx.send(TmuxCommand::RefreshAll).await;
                    }
                    KeyCode::Backspace => self.state.input_backspace(),
                    KeyCode::Delete => self.state.input_delete(),
                    KeyCode::Left => self.state.input_move_left(),
                    KeyCode::Right => self.state.input_move_right(),
                    KeyCode::Home => self.state.input_move_home(),
                    KeyCode::End => self.state.input_move_end(),
                    KeyCode::Char(c) => self.state.input_char(c),
                    _ => {}
                }
            }
            PopupMode::ConfirmKill => {
                match key.code {
                    KeyCode::Esc => {
                        self.state.close_popup();
                        self.refresh_control.resume();
                    }
                    KeyCode::Enter => {
                        if let Some(name) = self.state.get_kill_session_name() {
                            let _ = self.tmux_cmd_tx.send(TmuxCommand::KillSession { name }).await;
                            // Refresh after operation
                            let _ = self.tmux_cmd_tx.send(TmuxCommand::RefreshAll).await;
                        }
                        self.state.close_popup();
                        self.refresh_control.resume();
                    }
                    KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Tab
                    | KeyCode::Char('h')
                    | KeyCode::Char('l') => {
                        self.state.toggle_confirm_selection();
                    }
                    KeyCode::Char('y') => {
                        self.state.confirm_yes_selected = true;
                    }
                    KeyCode::Char('n') => {
                        self.state.confirm_yes_selected = false;
                    }
                    _ => {}
                }
            }
        }
        Ok(false)
    }

    async fn handle_normal_mode_key(&mut self, key: event::KeyEvent) -> Result<bool> {
        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if is_ctrl {
            match key.code {
                KeyCode::Char('n') => {
                    self.state.open_new_session_popup();
                    self.refresh_control.pause();
                }
                KeyCode::Char('r') => {
                    self.state.open_rename_session_popup();
                    self.refresh_control.pause();
                }
                KeyCode::Char('x') => {
                    self.state.open_kill_session_popup();
                    self.refresh_control.pause();
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(true), // Exit
                KeyCode::Char('r') => {
                    let _ = self.tmux_cmd_tx.send(TmuxCommand::RefreshAll).await;
                }
                KeyCode::Char(' ') => {
                    self.state.handle_space_press();
                }
                KeyCode::Char('i') => {
                    self.state.enter_input_mode();
                    self.refresh_control.pause();
                }
                KeyCode::Enter => {
                    if let Some(target) = self.state.get_enter_target() {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        let _ = self
                            .tmux_cmd_tx
                            .send(TmuxCommand::SwitchClient {
                                target,
                                reply: Some(reply_tx),
                            })
                            .await;
                        let _ = reply_rx.await;
                        return Ok(true); // Exit after switch
                    }
                }
                _ => {
                    // View-specific key handling
                    self.handle_navigation_key(key.code);
                }
            }
        }
        Ok(false)
    }

    async fn handle_input_mode_key(&mut self, key: event::KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.state.exit_input_mode();
                self.refresh_control.resume();
            }
            KeyCode::Enter => {
                if let Some(target) = self.state.get_current_target() {
                    let keys = self.state.input_buffer.clone();
                    let (reply_tx, reply_rx) = oneshot::channel();
                    let _ = self
                        .tmux_cmd_tx
                        .send(TmuxCommand::SendKeys {
                            target,
                            keys,
                            reply: Some(reply_tx),
                        })
                        .await;
                    let _ = reply_rx.await;
                }
                self.state.exit_input_mode();
                self.refresh_control.resume();
            }
            KeyCode::Backspace => self.state.input_backspace(),
            KeyCode::Delete => self.state.input_delete(),
            KeyCode::Left => self.state.input_move_left(),
            KeyCode::Right => self.state.input_move_right(),
            KeyCode::Home => self.state.input_move_home(),
            KeyCode::End => self.state.input_move_end(),
            KeyCode::Char(c) => self.state.input_char(c),
            _ => {}
        }
        Ok(())
    }

    fn handle_navigation_key(&mut self, code: KeyCode) {
        match self.state.view_mode {
            ViewMode::TreeView => match code {
                KeyCode::Up | KeyCode::Char('k') => self.state.tree_move_up(),
                KeyCode::Down | KeyCode::Char('j') => self.state.tree_move_down(),
                KeyCode::Tab => self.state.tree_next_focus(),
                KeyCode::BackTab => self.state.tree_prev_focus(),
                KeyCode::Left | KeyCode::Char('h') => self.state.tree_prev_focus(),
                KeyCode::Right | KeyCode::Char('l') => self.state.tree_next_focus(),
                _ => {}
            },
            ViewMode::MultiPreview => match code {
                KeyCode::Up | KeyCode::Char('k') => self.state.multi_move_up(),
                KeyCode::Down | KeyCode::Char('j') => self.state.multi_move_down(),
                KeyCode::Left | KeyCode::Char('h') => self.state.multi_move_left(),
                KeyCode::Right | KeyCode::Char('l') => self.state.multi_move_right(),
                _ => {}
            },
        }
    }

    fn handle_tmux_response(&mut self, response: TmuxResponse) {
        match response {
            TmuxResponse::SessionsRefreshed { sessions } => {
                self.state.update_sessions(sessions);
            }
            TmuxResponse::PaneCaptured { target: _, content } => {
                self.state.update_pane_content(content);
            }
            TmuxResponse::SessionCreated {
                name,
                success,
                error,
            } => {
                if success {
                    // Select the new session
                    if let Some(idx) = self.state.sessions.iter().position(|s| s.name == name) {
                        self.state.selected_session = idx;
                        self.state.session_list_state.select(Some(idx));
                    }
                } else if let Some(err) = error {
                    self.state.set_error(err);
                }
            }
            TmuxResponse::SessionRenamed { success, error } => {
                if !success && let Some(err) = error {
                    self.state.set_error(err);
                }
            }
            TmuxResponse::SessionKilled { success, error } => {
                if success {
                    // Adjust selection if needed
                    if !self.state.sessions.is_empty() {
                        self.state.selected_session = self
                            .state
                            .selected_session
                            .min(self.state.sessions.len().saturating_sub(1));
                        self.state
                            .session_list_state
                            .select(Some(self.state.selected_session));
                    }
                } else if let Some(err) = error {
                    self.state.set_error(err);
                }
            }
            TmuxResponse::KeysSent { success: _, error } => {
                if let Some(err) = error {
                    self.state.set_error(err);
                }
            }
            TmuxResponse::ClientSwitched {
                target,
                success,
                error,
            } => {
                if !success {
                    let message = match error {
                        Some(err) if !err.trim().is_empty() => {
                            format!("Failed to switch to {}: {}", target, err)
                        }
                        _ => format!("Failed to switch to {}", target),
                    };
                    self.state.set_error(message);
                }
            }
            TmuxResponse::Error { message } => {
                self.state.set_error(message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_handle_key_event() {}
}
