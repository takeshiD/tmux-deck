mod app;
mod cli;
mod ui;

use std::io;

use color_eyre::Result;
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;

use app::{App, InputMode, PopupMode, ViewMode};
use cli::Cli;
use ui::render_ui;

// =============================================================================
// Main
// =============================================================================

fn main() -> Result<()> {
    color_eyre::install()?;
    let cmd = Cli::parse_with_color()?;

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let mut app = App::new(cmd.interval);
    app.refresh_all();
    let result = run_app(&mut terminal, &mut app);

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    loop {
        // Capture selected pane content for TreeView
        if app.view_mode == ViewMode::TreeView {
            app.capture_selected_pane();
        }

        terminal.draw(|frame| {
            render_ui(frame, app);
        })?;

        if event::poll(app.interval)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    // Handle popup mode first
                    if let Some(popup_mode) = app.popup_mode {
                        match popup_mode {
                            PopupMode::NewSession | PopupMode::RenameSession => {
                                match key.code {
                                    KeyCode::Esc => app.close_popup(),
                                    KeyCode::Enter => {
                                        if popup_mode == PopupMode::NewSession {
                                            app.confirm_new_session();
                                        } else {
                                            app.confirm_rename_session();
                                        }
                                    }
                                    KeyCode::Backspace => app.input_backspace(),
                                    KeyCode::Delete => app.input_delete(),
                                    KeyCode::Left => app.input_move_left(),
                                    KeyCode::Right => app.input_move_right(),
                                    KeyCode::Home => app.input_move_home(),
                                    KeyCode::End => app.input_move_end(),
                                    KeyCode::Char(c) => app.input_char(c),
                                    _ => {}
                                }
                            }
                            PopupMode::ConfirmKill => {
                                match key.code {
                                    KeyCode::Esc => app.close_popup(),
                                    KeyCode::Enter => app.confirm_kill_session(),
                                    KeyCode::Left | KeyCode::Right | KeyCode::Tab |
                                    KeyCode::Char('h') | KeyCode::Char('l') |
                                    KeyCode::Char('y') | KeyCode::Char('n') => {
                                        if matches!(key.code, KeyCode::Char('y')) {
                                            app.confirm_yes_selected = true;
                                        } else if matches!(key.code, KeyCode::Char('n')) {
                                            app.confirm_yes_selected = false;
                                        } else {
                                            app.toggle_confirm_selection();
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        continue;
                    }

                    match app.input_mode {
                        InputMode::Normal => {
                            // Check for Ctrl key modifiers
                            let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                            if is_ctrl {
                                match key.code {
                                    KeyCode::Char('n') => app.open_new_session_popup(),
                                    KeyCode::Char('r') => app.open_rename_session_popup(),
                                    KeyCode::Char('x') => app.open_kill_session_popup(),
                                    _ => {}
                                }
                            } else {
                                match key.code {
                                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                                    KeyCode::Char('r') => app.refresh_all(),
                                    KeyCode::Char(' ') => {
                                        app.handle_space_press();
                                    }
                                    KeyCode::Char('i') => app.enter_input_mode(),
                                    KeyCode::Enter => {
                                        if app.switch_to_selected_pane() {
                                            return Ok(());
                                        }
                                    }
                                    _ => {
                                        // View-specific key handling
                                        match app.view_mode {
                                            ViewMode::TreeView => match key.code {
                                                KeyCode::Up | KeyCode::Char('k') => app.tree_move_up(),
                                                KeyCode::Down | KeyCode::Char('j') => app.tree_move_down(),
                                                KeyCode::Tab => app.tree_next_focus(),
                                                KeyCode::BackTab => app.tree_prev_focus(),
                                                KeyCode::Left | KeyCode::Char('h') => app.tree_prev_focus(),
                                                KeyCode::Right | KeyCode::Char('l') => app.tree_next_focus(),
                                                _ => {}
                                            },
                                            ViewMode::MultiPreview => match key.code {
                                                KeyCode::Up | KeyCode::Char('k') => app.multi_move_up(),
                                                KeyCode::Down | KeyCode::Char('j') => app.multi_move_down(),
                                                KeyCode::Left | KeyCode::Char('h') => app.multi_move_left(),
                                                KeyCode::Right | KeyCode::Char('l') => app.multi_move_right(),
                                                _ => {}
                                            },
                                        }
                                    }
                                }
                            }
                        }
                        InputMode::Input => match key.code {
                            KeyCode::Esc => app.exit_input_mode(),
                            KeyCode::Enter => app.send_input_to_pane(),
                            KeyCode::Backspace => app.input_backspace(),
                            KeyCode::Delete => app.input_delete(),
                            KeyCode::Left => app.input_move_left(),
                            KeyCode::Right => app.input_move_right(),
                            KeyCode::Home => app.input_move_home(),
                            KeyCode::End => app.input_move_end(),
                            KeyCode::Char(c) => app.input_char(c),
                            _ => {}
                        },
                    }
                }
            }
        } else {
            // Periodic refresh
            if app.input_mode == InputMode::Normal && app.popup_mode.is_none() {
                app.refresh_all();
            }
        }
    }
}
