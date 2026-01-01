use ansi_to_tui::IntoText;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

use crate::app::{App, Focus, InputMode, PopupMode, TmuxPane, TmuxWindow, ViewMode};

// =============================================================================
// ANSI Content Processing
// =============================================================================

/// A character with its associated style (for ANSI rendering)
#[derive(Clone, Default)]
struct StyledChar {
    ch: char,
    style: Style,
}

fn ansi_to_styled_grid(content: &str) -> Vec<Vec<StyledChar>> {
    let text = match content.as_bytes().into_text() {
        Ok(t) => t,
        Err(_) => {
            return content
                .lines()
                .map(|line| {
                    line.chars()
                        .map(|ch| StyledChar {
                            ch,
                            style: Style::default(),
                        })
                        .collect()
                })
                .collect();
        }
    };

    let mut grid: Vec<Vec<StyledChar>> = Vec::new();

    for line in text.lines {
        let mut row: Vec<StyledChar> = Vec::new();
        for span in line.spans {
            for ch in span.content.chars() {
                row.push(StyledChar {
                    ch,
                    style: span.style,
                });
            }
        }
        grid.push(row);
    }

    grid
}

fn shrink_styled_content<'a>(
    grid: &[Vec<StyledChar>],
    target_width: usize,
    target_height: usize,
    source_width: u32,
    _source_height: u32,
) -> Text<'a> {
    if grid.is_empty() || target_width == 0 || target_height == 0 {
        return Text::default();
    }

    let actual_lines = grid.len();
    let source_width = source_width as usize;

    let col_ratio = if source_width > target_width {
        source_width as f64 / target_width as f64
    } else {
        1.0
    };

    let (start_row, row_ratio) = if actual_lines <= target_height {
        (0, 1.0)
    } else {
        let rows_to_show = actual_lines.min(target_height * 2);
        let start = actual_lines.saturating_sub(rows_to_show);
        let ratio = rows_to_show as f64 / target_height as f64;
        (start, ratio)
    };

    let mut lines: Vec<Line> = Vec::new();

    for target_row in 0..target_height {
        let source_row = start_row + (target_row as f64 * row_ratio) as usize;

        if source_row >= grid.len() {
            lines.push(Line::default());
            continue;
        }

        let row = &grid[source_row];
        let mut spans: Vec<Span> = Vec::new();
        let mut current_style = Style::default();
        let mut current_text = String::new();

        for target_col in 0..target_width {
            let source_col = (target_col as f64 * col_ratio) as usize;

            let styled_char = if source_col < row.len() {
                &row[source_col]
            } else {
                &StyledChar {
                    ch: ' ',
                    style: Style::default(),
                }
            };

            if styled_char.style != current_style && !current_text.is_empty() {
                spans.push(Span::styled(current_text.clone(), current_style));
                current_text.clear();
            }

            current_style = styled_char.style;
            current_text.push(styled_char.ch);
        }

        if !current_text.is_empty() {
            let trimmed = current_text.trim_end();
            if !trimmed.is_empty() {
                spans.push(Span::styled(trimmed.to_string(), current_style));
            }
        }

        lines.push(Line::from(spans));
    }

    Text::from(lines)
}

// =============================================================================
// Main UI Rendering
// =============================================================================

pub fn render_ui(frame: &mut Frame, app: &mut App) {
    match app.view_mode {
        ViewMode::TreeView => render_tree_view(frame, app),
        ViewMode::MultiPreview => render_multi_preview(frame, app),
    }

    // Render input popup if in input mode
    if app.input_mode == InputMode::Input {
        render_input_popup(frame, app, frame.area());
    }

    // Render session operation popups
    if let Some(popup_mode) = app.popup_mode {
        match popup_mode {
            PopupMode::NewSession => render_session_name_popup(frame, app, "New Session", "Enter session name:"),
            PopupMode::RenameSession => render_session_name_popup(frame, app, "Rename Session", "Enter new name:"),
            PopupMode::ConfirmKill => render_confirm_kill_popup(frame, app),
        }
    }
}

// =============================================================================
// TreeView Rendering
// =============================================================================

fn render_tree_view(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Main layout: left panel (lists) | right panel (preview)
    let main_chunks =
        Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)]).split(area);

    let left_panel = main_chunks[0];
    let right_panel = main_chunks[1];

    // Left panel: Sessions | Windows | Panes (vertical stack)
    let left_chunks = Layout::vertical([
        Constraint::Percentage(30),
        Constraint::Percentage(35),
        Constraint::Percentage(35),
    ])
    .split(left_panel);

    render_sessions_list(frame, app, left_chunks[0]);
    render_windows_list(frame, app, left_chunks[1]);
    render_panes_list(frame, app, left_chunks[2]);

    // Right panel: Preview with status bar
    let right_chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(right_panel);
    render_pane_preview_tree(frame, app, right_chunks[0]);
    render_tree_status_bar(frame, app, right_chunks[1]);
}

fn render_sessions_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let is_focused = app.focus == Focus::Sessions;
    let border_style = if is_focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, session)| {
            let attached_marker = if session.attached { " ●" } else { "" };
            let style = if i == app.selected_session {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };
            ListItem::new(format!("{}{}", session.name, attached_marker)).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(format!(" Sessions ({}) ", app.sessions.len())),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol(if is_focused { "▶ " } else { "  " });

    frame.render_stateful_widget(list, area, &mut app.session_list_state);
}

fn render_windows_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let is_focused = app.focus == Focus::Windows;
    let border_style = if is_focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let empty_windows: Vec<TmuxWindow> = Vec::new();
    let windows = app
        .sessions
        .get(app.selected_session)
        .map(|s| &s.windows)
        .unwrap_or(&empty_windows);

    let items: Vec<ListItem> = windows
        .iter()
        .enumerate()
        .map(|(i, window)| {
            let active_marker = if window.active { " *" } else { "" };
            let style = if i == app.selected_window {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };
            ListItem::new(format!("{}:{}{}", window.index, window.name, active_marker)).style(style)
        })
        .collect();

    let title = app
        .sessions
        .get(app.selected_session)
        .map(|s| format!(" Windows [{}] ({}) ", s.name, windows.len()))
        .unwrap_or_else(|| " Windows ".to_string());

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol(if is_focused { "▶ " } else { "  " });

    frame.render_stateful_widget(list, area, &mut app.window_list_state);
}

fn render_panes_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let is_focused = app.focus == Focus::Panes;
    let border_style = if is_focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let empty_panes: Vec<TmuxPane> = Vec::new();
    let panes = app
        .sessions
        .get(app.selected_session)
        .and_then(|s| s.windows.get(app.selected_window))
        .map(|w| &w.panes)
        .unwrap_or(&empty_panes);

    let items: Vec<ListItem> = panes
        .iter()
        .enumerate()
        .map(|(i, pane)| {
            let active_marker = if pane.active { " *" } else { "" };
            let style = if i == app.selected_pane {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };
            ListItem::new(format!(
                "{}:{}{} [{}]",
                pane.index, pane.id, active_marker, pane.current_command
            ))
            .style(style)
        })
        .collect();

    let title = app
        .sessions
        .get(app.selected_session)
        .and_then(|s| s.windows.get(app.selected_window))
        .map(|w| format!(" Panes [{}] ({}) ", w.name, panes.len()))
        .unwrap_or_else(|| " Panes ".to_string());

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol(if is_focused { "▶ " } else { "  " });

    frame.render_stateful_widget(list, area, &mut app.pane_list_state);
}

fn render_pane_preview_tree(frame: &mut Frame, app: &App, area: Rect) {
    let title = app
        .get_selected_pane_target()
        .map(|t| format!(" Preview: {} ", t))
        .unwrap_or_else(|| " Preview ".to_string());

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);

    let text = match app.pane_content.as_bytes().into_text() {
        Ok(text) => text,
        Err(_) => Text::raw(&app.pane_content),
    };

    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);
}

fn render_tree_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let status_text = if let Some(ref err) = app.last_error {
        Line::from(vec![Span::styled(
            format!(" Error: {} ", err),
            Style::default().fg(Color::Red),
        )])
    } else {
        Line::from(vec![
            Span::styled("j/k", Style::default().fg(Color::Yellow)),
            Span::raw(":move "),
            Span::styled("Tab", Style::default().fg(Color::Yellow)),
            Span::raw(":focus "),
            Span::styled("Space×2", Style::default().fg(Color::Magenta)),
            Span::raw(":multi "),
            Span::styled("C-n", Style::default().fg(Color::Green)),
            Span::raw(":new "),
            Span::styled("C-r", Style::default().fg(Color::Green)),
            Span::raw(":rename "),
            Span::styled("C-x", Style::default().fg(Color::Red)),
            Span::raw(":kill "),
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(":quit"),
        ])
    };

    frame.render_widget(
        Paragraph::new(status_text).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

// =============================================================================
// MultiPreview Rendering
// =============================================================================

fn render_multi_preview(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let main_chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);

    let preview_area = main_chunks[0];
    let status_area = main_chunks[1];

    if app.sessions.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" No sessions found ");
        frame.render_widget(block, preview_area);
    } else {
        // Create horizontal layout for sessions
        let session_constraints: Vec<Constraint> = app
            .sessions
            .iter()
            .map(|_| Constraint::Ratio(1, app.sessions.len() as u32))
            .collect();

        let session_chunks = Layout::horizontal(session_constraints).split(preview_area);

        for (session_idx, (session, session_area)) in
            app.sessions.iter().zip(session_chunks.iter()).enumerate()
        {
            let is_selected_session = session_idx == app.multi_session;

            // Session block style
            let session_border_style = if is_selected_session {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else if session.attached {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let session_title = if session.attached {
                format!(" {} ● ", session.name)
            } else {
                format!(" {} ", session.name)
            };

            let session_block = Block::default()
                .borders(Borders::ALL)
                .border_style(session_border_style)
                .title(session_title);

            let inner_area = session_block.inner(*session_area);
            frame.render_widget(session_block, *session_area);

            if session.windows.is_empty() {
                let no_windows = Paragraph::new("No windows")
                    .style(Style::default().fg(Color::DarkGray));
                frame.render_widget(no_windows, inner_area);
                continue;
            }

            // Create vertical layout for windows within this session
            let window_constraints: Vec<Constraint> = session
                .windows
                .iter()
                .map(|_| Constraint::Ratio(1, session.windows.len() as u32))
                .collect();

            let window_chunks = Layout::vertical(window_constraints).split(inner_area);

            for (window_idx, (window, window_area)) in
                session.windows.iter().zip(window_chunks.iter()).enumerate()
            {
                let is_selected_window =
                    is_selected_session && window_idx == app.multi_window;

                render_window_preview(frame, window, *window_area, is_selected_window);
            }
        }
    }

    // Status bar
    let status_text = if let Some(ref err) = app.last_error {
        Line::from(vec![Span::styled(
            format!(" Error: {} ", err),
            Style::default().fg(Color::Red),
        )])
    } else {
        let selected_info = app
            .get_multi_selected_target()
            .unwrap_or_else(|| "None".to_string());

        Line::from(vec![
            Span::styled("h/l", Style::default().fg(Color::Yellow)),
            Span::raw(":session "),
            Span::styled("j/k", Style::default().fg(Color::Yellow)),
            Span::raw(":window "),
            Span::styled("Space×2", Style::default().fg(Color::Magenta)),
            Span::raw(":tree "),
            Span::styled("C-n", Style::default().fg(Color::Green)),
            Span::raw(":new "),
            Span::styled("C-r", Style::default().fg(Color::Green)),
            Span::raw(":rename "),
            Span::styled("C-x", Style::default().fg(Color::Red)),
            Span::raw(":kill "),
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(":quit "),
            Span::raw("| "),
            Span::styled(
                format!("Sel:{}", selected_info),
                Style::default().fg(Color::Cyan),
            ),
        ])
    };

    frame.render_widget(
        Paragraph::new(status_text).style(Style::default().bg(Color::DarkGray)),
        status_area,
    );
}

fn render_window_preview(frame: &mut Frame, window: &TmuxWindow, area: Rect, is_selected: bool) {
    let border_style = if is_selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if window.active {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let active_marker = if window.active { " *" } else { "" };
    let cmd = window
        .get_active_pane()
        .map(|p| p.current_command.as_str())
        .unwrap_or("");

    let title = format!(" {}:{}{} [{}] ", window.index, window.name, active_marker, cmd);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner = block.inner(area);

    let styled_grid = ansi_to_styled_grid(&window.content);
    let shrunk_text = shrink_styled_content(
        &styled_grid,
        inner.width as usize,
        inner.height as usize,
        window.pane_width,
        window.pane_height,
    );

    let paragraph = Paragraph::new(shrunk_text).block(block);

    frame.render_widget(paragraph, area);
}

// =============================================================================
// Input Popup
// =============================================================================

fn render_input_popup(frame: &mut Frame, app: &App, area: Rect) {
    let popup_width = (area.width * 70 / 100).clamp(40, 80);
    let popup_height = 7;

    let popup_x = (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = (area.height.saturating_sub(popup_height)) / 2;

    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    let target_info = app
        .get_current_target()
        .unwrap_or_else(|| "None".to_string());

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" Send to: {} ", target_info))
        .title_bottom(Line::from(" Enter:send | Esc:cancel ").centered());

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let input_chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);

    let label = Paragraph::new("Enter message:").style(Style::default().fg(Color::White));
    frame.render_widget(label, input_chunks[0]);

    let input_area = input_chunks[2];

    let before_cursor = &app.input_buffer[..app.input_cursor];
    let cursor_char = app
        .input_buffer
        .chars()
        .nth(app.input_cursor)
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".to_string());
    let after_cursor = if app.input_cursor < app.input_buffer.len() {
        &app.input_buffer[app.input_cursor + cursor_char.len()..]
    } else {
        ""
    };

    let input_text = Line::from(vec![
        Span::raw(before_cursor),
        Span::styled(
            cursor_char,
            Style::default().bg(Color::White).fg(Color::Black),
        ),
        Span::raw(after_cursor),
    ]);

    let input_paragraph = Paragraph::new(input_text)
        .style(Style::default().fg(Color::White).bg(Color::DarkGray))
        .wrap(Wrap { trim: false });

    frame.render_widget(input_paragraph, input_area);
}

// =============================================================================
// Session Operation Popups
// =============================================================================

fn render_session_name_popup(frame: &mut Frame, app: &App, title: &str, label: &str) {
    let area = frame.area();
    let popup_width = (area.width * 60 / 100).clamp(40, 70);
    let popup_height = 7;

    let popup_x = (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = (area.height.saturating_sub(popup_height)) / 2;

    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" {} ", title))
        .title_bottom(Line::from(" Enter:confirm | Esc:cancel ").centered());

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let input_chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);

    let label_widget = Paragraph::new(label).style(Style::default().fg(Color::White));
    frame.render_widget(label_widget, input_chunks[0]);

    let input_area = input_chunks[2];

    // Render input with cursor
    let before_cursor = &app.input_buffer[..app.input_cursor];
    let cursor_char = app
        .input_buffer
        .chars()
        .nth(app.input_cursor)
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".to_string());
    let after_cursor = if app.input_cursor < app.input_buffer.len() {
        &app.input_buffer[app.input_cursor + cursor_char.len()..]
    } else {
        ""
    };

    let input_text = Line::from(vec![
        Span::raw(before_cursor),
        Span::styled(
            cursor_char,
            Style::default().bg(Color::White).fg(Color::Black),
        ),
        Span::raw(after_cursor),
    ]);

    let input_paragraph = Paragraph::new(input_text)
        .style(Style::default().fg(Color::White).bg(Color::DarkGray))
        .wrap(Wrap { trim: false });

    frame.render_widget(input_paragraph, input_area);
}

fn render_confirm_kill_popup(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let popup_width = (area.width * 50 / 100).clamp(40, 60);
    let popup_height = 7;

    let popup_x = (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = (area.height.saturating_sub(popup_height)) / 2;

    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let session_name = app
        .sessions
        .get(app.selected_session)
        .map(|s| s.name.as_str())
        .unwrap_or("?");

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" Kill Session ")
        .title_bottom(Line::from(" Enter:confirm | Esc:cancel ").centered());

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let content_chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);

    // Question text
    let question = Paragraph::new(format!("Kill session '{}'?", session_name))
        .style(Style::default().fg(Color::White))
        .alignment(Alignment::Center);
    frame.render_widget(question, content_chunks[0]);

    // Yes/No buttons
    let button_area = content_chunks[2];
    let button_chunks = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .split(button_area);

    let yes_style = if app.confirm_yes_selected {
        Style::default().fg(Color::Black).bg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let no_style = if !app.confirm_yes_selected {
        Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let yes_button = Paragraph::new(" [Y]es ")
        .style(yes_style)
        .alignment(Alignment::Center);
    let no_button = Paragraph::new(" [N]o ")
        .style(no_style)
        .alignment(Alignment::Center);

    frame.render_widget(yes_button, button_chunks[0]);
    frame.render_widget(no_button, button_chunks[1]);
}
