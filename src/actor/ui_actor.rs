use std::io;
use std::time::Duration;

use color_eyre::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::{mpsc, oneshot};

use crate::actor::messages::{RefreshControl, TmuxCommand, TmuxResponse, UIEvent};
use crate::app::{
    Focus, GroupChoice, InputMode, PopupMode, SESSION_NAME_MAX_LEN, UIState, ViewMode,
};
use crate::config::Action;
use crate::ui::render_ui;

// =============================================================================
// Key Event Poller (runs in dedicated blocking thread)
// =============================================================================

fn spawn_key_event_poller(key_tx: mpsc::Sender<Event>) {
    std::thread::spawn(move || {
        loop {
            // Poll with moderate timeout for balance between responsiveness and CPU usage
            if event::poll(Duration::from_millis(50)).unwrap_or(false)
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
    /// High-priority channel: user-initiated commands.
    tmux_cmd_tx: mpsc::Sender<TmuxCommand>,
    /// Low-priority channel: periodic capture-pane.
    tmux_capture_tx: mpsc::Sender<TmuxCommand>,
    tmux_res_rx: mpsc::Receiver<TmuxResponse>,
    ui_event_rx: mpsc::Receiver<UIEvent>,
    key_rx: mpsc::Receiver<Event>,
    refresh_control: RefreshControl,
    /// Results of background `claude -p` summary jobs: (session id, Ok(text)/Err).
    agent_summary_tx: mpsc::Sender<(String, Result<String, String>)>,
    agent_summary_rx: mpsc::Receiver<(String, Result<String, String>)>,
}

impl UIActor {
    pub fn new(
        terminal: Terminal<CrosstermBackend<io::Stdout>>,
        state: UIState,
        tmux_cmd_tx: mpsc::Sender<TmuxCommand>,
        tmux_capture_tx: mpsc::Sender<TmuxCommand>,
        tmux_res_rx: mpsc::Receiver<TmuxResponse>,
        ui_event_rx: mpsc::Receiver<UIEvent>,
        refresh_control: RefreshControl,
    ) -> Self {
        // Spawn dedicated key event poller thread
        let (key_tx, key_rx) = mpsc::channel::<Event>(64);
        spawn_key_event_poller(key_tx);

        let (agent_summary_tx, agent_summary_rx) = mpsc::channel(8);

        Self {
            terminal,
            state,
            tmux_cmd_tx,
            tmux_capture_tx,
            tmux_res_rx,
            ui_event_rx,
            key_rx,
            refresh_control,
            agent_summary_tx,
            agent_summary_rx,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        // Request initial data
        let _ = self.tmux_cmd_tx.send(TmuxCommand::RefreshAll).await;

        // Initial render before entering event loop
        self.terminal.draw(|frame| {
            render_ui(frame, &mut self.state);
        })?;

        // Drives the Claude "Working" dots spinner. Ticks frequently, but only
        // forces a redraw while something is actually animating, so an idle TUI
        // stays event-driven.
        let mut anim = tokio::time::interval(Duration::from_millis(80));

        loop {
            // Default to redrawing; the animation tick decides for itself.
            let mut redraw = true;

            // Use select to handle multiple event sources
            // biased; ensures key events are checked first (top-to-bottom priority)
            tokio::select! {
                biased;

                // Key events from dedicated poller thread (highest priority)
                Some(event) = self.key_rx.recv() => {
                    if self.handle_key_event(event).await? {
                        break; // Exit requested
                    }
                }

                // Completed background summary jobs.
                Some((id, result)) = self.agent_summary_rx.recv() => {
                    self.state.set_summary_result(id, result);
                }

                // TmuxActor responses
                Some(response) = self.tmux_res_rx.recv() => {
                    self.handle_tmux_response(response);
                }

                // RefreshActor events
                Some(event) = self.ui_event_rx.recv() => {
                    match event {
                        UIEvent::Tick => {
                            // Cheap, local: fold the latest Claude hook states
                            // into the tree so markers stay live between full
                            // tmux refreshes.
                            self.state.refresh_claude_states();

                            match self.state.view_mode {
                                // TreeView captures the selected pane for its preview.
                                ViewMode::TreeView => {
                                    if let Some((target, start, end)) =
                                        self.state.get_selected_pane_target_with_capture_range()
                                    {
                                        let _ = self
                                            .tmux_capture_tx
                                            .send(TmuxCommand::CapturePane { target, start, end })
                                            .await;
                                    }
                                }
                                // The agent view reloads background sessions from disk.
                                ViewMode::Dashboard => self.state.refresh_agents(),
                                ViewMode::MultiPreview => {}
                            }
                        }
                        UIEvent::Shutdown => {
                            break;
                        }
                        _ => (),
                    }
                }

                // Spinner animation tick: only redraw if a spinner is active.
                _ = anim.tick() => {
                    redraw = self.state.has_working_claude();
                }
            }

            // An attach request suspends the TUI, hands the terminal to
            // `claude attach <id>`, then restores the TUI when it returns.
            if let Some(id) = self.state.pending_attach.take() {
                self.attach_agent(&id)?;
                continue;
            }

            // Render UI after processing event (event-driven rendering)
            if redraw {
                self.terminal.draw(|frame| {
                    render_ui(frame, &mut self.state);
                })?;
            }
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
            PopupMode::GroupSession => {
                // Selecting an existing group (or "ungroup") is handled entirely
                // tmux-deck-side: no tmux command and no RefreshAll, since
                // grouping does not change anything tmux knows about.
                match key.code {
                    KeyCode::Esc => {
                        self.state.close_popup();
                        self.refresh_control.resume();
                    }
                    KeyCode::Up | KeyCode::Char('k') => self.state.group_choice_up(),
                    KeyCode::Down | KeyCode::Char('j') => self.state.group_choice_down(),
                    KeyCode::Enter => match self.state.selected_group_choice() {
                        GroupChoice::Existing(group) => {
                            self.state.assign_selected_group(Some(group));
                            self.state.close_popup();
                            self.refresh_control.resume();
                        }
                        GroupChoice::Ungrouped => {
                            self.state.assign_selected_group(None);
                            self.state.close_popup();
                            self.refresh_control.resume();
                        }
                        // Switch to text entry; stay in popup so the refresh
                        // control remains paused until the name is confirmed.
                        GroupChoice::New => self.state.begin_new_group_entry(),
                    },
                    _ => {}
                }
            }
            PopupMode::NewSession | PopupMode::RenameSession | PopupMode::NewGroup => {
                match key.code {
                    KeyCode::Esc => {
                        self.state.close_popup();
                        self.refresh_control.resume();
                    }
                    KeyCode::Enter => {
                        // A new group is handled entirely tmux-deck-side: no
                        // tmux command and no RefreshAll, since grouping does
                        // not change anything tmux knows about.
                        if popup_mode == PopupMode::NewGroup {
                            let group = self.state.get_group_session_input();
                            self.state.assign_selected_group(group);
                            self.state.close_popup();
                            self.refresh_control.resume();
                            return Ok(false);
                        }
                        if popup_mode == PopupMode::NewSession {
                            let name = self.state.get_new_session_name();
                            if !name.is_empty() {
                                let _ = self.tmux_cmd_tx.send(TmuxCommand::NewSession { name }).await;
                            }
                        } else if let Some((old_name, new_name)) =
                            self.state.get_rename_session_info()
                        {
                            // Carry the group label across the rename so the
                            // session does not silently fall out of its group.
                            self.state.groups.rename_session(&old_name, &new_name);
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
                    KeyCode::Char(c) => {
                        self.state.input_char_limited(c, SESSION_NAME_MAX_LEN)
                    }
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
                            // Drop the killed session's group assignment so the
                            // store does not keep stale entries around.
                            self.state.groups.forget(&name);
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
        let in_sessions = self.state.view_mode == ViewMode::TreeView
            && self.state.focus == Focus::Sessions;

        // `za` fold chord: a pending `z` followed by `a` toggles the current
        // group's fold. Any other key cancels the chord and is then processed
        // normally below.
        if self.state.pending_z {
            self.state.pending_z = false;
            if !is_ctrl && key.code == KeyCode::Char('a') {
                self.state.toggle_fold_current_group();
                return Ok(false);
            }
        }

        // Fixed (non-remappable) chords handled before config bindings:
        // `z` begins the `za` fold chord, double-`Space` toggles the view.
        if !is_ctrl {
            match key.code {
                KeyCode::Char('z') if in_sessions => {
                    self.state.pending_z = true;
                    return Ok(false);
                }
                KeyCode::Char(' ') if self.state.view_mode != ViewMode::Dashboard => {
                    self.state.handle_space_press();
                    return Ok(false);
                }
                // Agent-view-only keys: `p` toggles the preview panel, `s`
                // generates an execution summary for the selected session.
                KeyCode::Char('p') if self.state.view_mode == ViewMode::Dashboard => {
                    self.state.toggle_agent_preview();
                    return Ok(false);
                }
                KeyCode::Char('s') if self.state.view_mode == ViewMode::Dashboard => {
                    self.state.open_agent_summary();
                    self.request_agent_summary();
                    return Ok(false);
                }
                // Esc closes the summary popup before falling through to quit.
                KeyCode::Esc
                    if self.state.view_mode == ViewMode::Dashboard
                        && self.state.agent_summary_open =>
                {
                    self.state.close_agent_summary();
                    return Ok(false);
                }
                _ => {}
            }
        }

        // Remappable actions, resolved through the user's key bindings.
        if let Some(action) = self.state.keybindings.action_for(&key) {
            match action {
                Action::Quit => return Ok(true),
                Action::Refresh => {
                    let _ = self.tmux_cmd_tx.send(TmuxCommand::RefreshAll).await;
                }
                Action::Sort if in_sessions => self.state.cycle_session_sort(),
                Action::Group if in_sessions => {
                    self.state.open_group_session_popup();
                    self.refresh_control.pause();
                }
                Action::Input => {
                    self.state.enter_input_mode();
                    self.refresh_control.pause();
                }
                Action::NewSession => {
                    self.state.open_new_session_popup();
                    self.refresh_control.pause();
                }
                Action::RenameSession => {
                    self.state.open_rename_session_popup();
                    self.refresh_control.pause();
                }
                Action::KillSession => {
                    self.state.open_kill_session_popup();
                    self.refresh_control.pause();
                }
                Action::Enter if self.state.view_mode == ViewMode::Dashboard => {
                    // Attach to the selected background session. The UI loop
                    // consumes `pending_attach` to run `claude attach <id>`.
                    self.state.pending_attach = self.state.selected_agent_id();
                }
                Action::Enter => {
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
                        // Optionally keep the deck open after switching.
                        if self.state.behavior.exit_on_switch {
                            return Ok(true);
                        }
                    }
                }
                Action::Dashboard => self.state.toggle_dashboard(),
                // Context-gated actions whose gate is not satisfied fall through
                // to navigation so the key is not swallowed.
                Action::Sort | Action::Group => {
                    if !is_ctrl {
                        self.handle_navigation_key(key.code);
                    }
                }
            }
            return Ok(false);
        }

        // Unbound keys: view-specific navigation (only without Ctrl).
        if !is_ctrl {
            self.handle_navigation_key(key.code);
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

    /// Suspend the TUI, run `claude attach <id>` with the terminal handed over,
    /// then restore the TUI. Mirrors the agent view's attach/detach: when the
    /// user detaches (or the session ends) we come back to the list.
    fn attach_agent(&mut self, id: &str) -> Result<()> {
        // Tear down our TUI so claude owns a clean terminal.
        self.refresh_control.pause();
        disable_raw_mode()?;
        io::stdout().execute(LeaveAlternateScreen)?;

        let status = std::process::Command::new("claude")
            .arg("attach")
            .arg(id)
            .status();

        // Restore the TUI regardless of how claude exited.
        enable_raw_mode()?;
        io::stdout().execute(EnterAlternateScreen)?;
        self.terminal.clear()?;
        self.refresh_control.resume();

        if let Err(e) = status {
            self.state.set_error(format!("claude attach failed: {e}"));
        }
        self.state.refresh_agents();
        Ok(())
    }

    /// Kick off an execution summary for the selected background session by
    /// running `claude -p` (stateless, against a transcript digest) in a
    /// background task. The result is delivered over `agent_summary_rx`.
    fn request_agent_summary(&mut self) {
        let Some(session) = self.state.selected_agent() else {
            return;
        };
        // Don't double-dispatch while one is already running.
        if matches!(
            self.state.summary_status(&session.id),
            Some(crate::app::SummaryStatus::Pending)
        ) {
            return;
        }
        let Some(path) = session.transcript_path.clone() else {
            self.state.set_summary_result(
                session.id.clone(),
                Err("no transcript for this session".into()),
            );
            return;
        };
        let id = session.id.clone();
        let model = self.state.agents_config.summary_model.clone();
        let tx = self.agent_summary_tx.clone();
        self.state.set_summary_pending(id.clone());

        tokio::spawn(async move {
            let result = generate_summary(&path, &model).await;
            let _ = tx.send((id, result)).await;
        });
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
            ViewMode::Dashboard => match code {
                KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => {
                    self.state.agent_select_next()
                }
                KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                    self.state.agent_select_prev()
                }
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

/// Run `claude -p` against a transcript digest to summarise what a background
/// session did. Stateless (no `--resume`), so it never touches the live
/// session or its transcript. Returns the summary text or an error message.
async fn generate_summary(transcript_path: &str, model: &str) -> Result<String, String> {
    let digest = crate::agents::transcript_digest(transcript_path, 6000);
    if digest.trim().is_empty() {
        return Err("transcript is empty".into());
    }
    let prompt = format!(
        "以下は Claude Code セッションの会話抜粋です。このセッションがこれまでに\
         何をしたかを、日本語で3〜5個の簡潔な箇条書きに要約してください。前置きや\
         結びの文は不要です。\n\n---\n{digest}"
    );

    let output = tokio::process::Command::new("claude")
        .arg("-p")
        .arg(&prompt)
        .arg("--model")
        .arg(model)
        .output()
        .await
        .map_err(|e| format!("failed to run claude: {e}"))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let msg = err.lines().next().unwrap_or("claude exited with error");
        return Err(msg.to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        Err("empty summary".into())
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_handle_key_event() {}
}
