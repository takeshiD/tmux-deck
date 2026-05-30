use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::{
    ClaudeState, Focus, InputMode, PopupMode, SessionRow, TmuxPane, TmuxWindow, UIState,
    UNGROUPED_LABEL, ViewMode,
};

/// Single colour used for every Claude marker. States are distinguished by
/// glyph *shape*, not colour, so the markers stay legible regardless of the
/// terminal palette or colour-blindness. Color::Indexed(208) is the standard
/// 256-color "Orange1" slot, which renders consistently across terminals that
/// do not honour truecolor.
const CLAUDE_MARKER_COLOR: Color = Color::Indexed(208);
/// Marker shown for a claude process when no hook state is known.
const CLAUDE_MARKER: &str = "●";

/// Braille "dots" spinner frames (cli-spinners `dots`). Rendered for the
/// `Working` Claude state so the marker visibly animates.
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Milliseconds each spinner frame is shown.
const SPINNER_FRAME_MS: u128 = 80;

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// The current spinner glyph, chosen from wall-clock time so it animates at a
/// constant rate regardless of how often we happen to redraw.
fn spinner_frame() -> &'static str {
    let idx = (now_millis() / SPINNER_FRAME_MS) as usize % SPINNER_FRAMES.len();
    SPINNER_FRAMES[idx]
}

/// The marker glyph + colour to show for a node, given its hook state and
/// whether a claude process was detected. States are distinguished by shape
/// (the colour is always the same); hook state wins, otherwise we fall back to
/// the plain "claude is running" dot so behaviour is unchanged when hooks are
/// not installed. Returns `None` when there is nothing to show.
fn claude_marker(state: Option<ClaudeState>, has_claude: bool) -> Option<(&'static str, Color)> {
    let sym = match state {
        Some(ClaudeState::Working) => spinner_frame(),
        Some(ClaudeState::Waiting) => "◆",
        Some(ClaudeState::Done) => "✓",
        Some(ClaudeState::Error) => "✗",
        None if has_claude => CLAUDE_MARKER,
        None => return None,
    };
    Some((sym, CLAUDE_MARKER_COLOR))
}

/// Border accent colour for a node that is running claude (any state). The
/// border only signals presence — state is conveyed by the marker shape — so a
/// single colour is used throughout.
fn claude_border_color(state: Option<ClaudeState>, has_claude: bool) -> Option<Color> {
    if state.is_some() || has_claude {
        Some(CLAUDE_MARKER_COLOR)
    } else {
        None
    }
}

// =============================================================================
// Main UI Rendering
// =============================================================================

pub fn render_ui(frame: &mut Frame, state: &mut UIState) {
    match state.view_mode {
        ViewMode::TreeView => render_tree_view(frame, state),
        ViewMode::MultiPreview => render_multi_preview(frame, state),
    }

    // Render input popup if in input mode
    if state.input_mode == InputMode::Input {
        render_input_popup(frame, state, frame.area());
    }

    // Render session operation popups
    if let Some(popup_mode) = state.popup_mode {
        match popup_mode {
            PopupMode::NewSession => render_session_name_popup(frame, state, "New Session", "Enter session name:"),
            PopupMode::RenameSession => render_session_name_popup(frame, state, "Rename Session", "Enter new name:"),
            PopupMode::GroupSession => render_group_select_popup(frame, state),
            PopupMode::NewGroup => {
                render_session_name_popup(frame, state, "New Group", "New group name:")
            }
            PopupMode::ConfirmKill => render_confirm_kill_popup(frame, state),
        }
    }
}

// =============================================================================
// TreeView Rendering
// =============================================================================

fn render_tree_view(frame: &mut Frame, state: &mut UIState) {
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

    render_sessions_list(frame, state, left_chunks[0]);
    render_windows_list(frame, state, left_chunks[1]);
    render_panes_list(frame, state, left_chunks[2]);

    // Right panel: Preview with status bar
    let right_chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(right_panel);
    render_pane_preview_tree(frame, state, right_chunks[0]);
    render_tree_status_bar(frame, state, right_chunks[1]);
}

fn render_sessions_list(frame: &mut Frame, state: &mut UIState, area: Rect) {
    let is_focused = state.focus == Focus::Sessions;
    let border_style = if is_focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    // Grouping turns the flat session list into a list of rows that may also
    // contain non-selectable group headers. When nothing is grouped, the rows
    // are all sessions and this renders identically to the ungrouped list.
    let rows = state.session_rows();
    let indented = rows
        .iter()
        .any(|r| matches!(r, SessionRow::Header { .. }));

    let mut items: Vec<ListItem> = Vec::with_capacity(rows.len());
    let mut selected_row: Option<usize> = None;
    // When the selection sits on a folded group, the cursor lands on that
    // group's header instead of a (hidden) member session.
    let selected_group = if state.selection_on_folded_header() {
        state
            .sessions
            .get(state.selected_session)
            .map(|s| s.group.clone())
    } else {
        None
    };
    for (row_idx, row) in rows.iter().enumerate() {
        match row {
            SessionRow::Header {
                group,
                count,
                collapsed,
            } => {
                let label = group.as_deref().unwrap_or(UNGROUPED_LABEL);
                let arrow = if *collapsed { '▸' } else { '▾' };
                let is_selected = selected_group.as_ref() == Some(group);
                if is_selected {
                    selected_row = Some(row_idx);
                }
                let mut style = Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD);
                if is_selected {
                    style = style.bg(Color::DarkGray).fg(Color::White);
                }
                items.push(ListItem::new(Line::from(vec![Span::styled(
                    format!("{} {} ({})", arrow, label, count),
                    style,
                )])));
            }
            SessionRow::Session { index } => {
                let session = &state.sessions[*index];
                if *index == state.selected_session {
                    selected_row = Some(row_idx);
                }
                let style = if *index == state.selected_session {
                    Style::default().bg(Color::DarkGray).fg(Color::White)
                } else {
                    Style::default()
                };
                // Indent sessions under their header so the hierarchy reads.
                let mut spans = vec![Span::raw(if indented {
                    format!("  {}", session.name)
                } else {
                    session.name.clone()
                })];
                if let Some((sym, color)) =
                    claude_marker(session.claude_state, session.has_claude)
                {
                    spans.push(Span::styled(
                        format!(" {}", sym),
                        Style::default().fg(color),
                    ));
                }
                items.push(ListItem::new(Line::from(spans)).style(style));
            }
        }
    }

    // The highlight tracks rendered rows, not session indices, so map the
    // selected session onto its row before handing the state to ratatui.
    state.session_list_state.select(selected_row);

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(format!(
                    " Sessions ({}) [{}] ",
                    state.sessions.len(),
                    state.session_sort.label()
                )),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol(if is_focused { "▶ " } else { "  " });

    frame.render_stateful_widget(list, area, &mut state.session_list_state);
}

fn render_windows_list(frame: &mut Frame, state: &mut UIState, area: Rect) {
    let is_focused = state.focus == Focus::Windows;
    let border_style = if is_focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let empty_windows: Vec<TmuxWindow> = Vec::new();
    let windows = state
        .sessions
        .get(state.selected_session)
        .map(|s| &s.windows)
        .unwrap_or(&empty_windows);

    let items: Vec<ListItem> = windows
        .iter()
        .enumerate()
        .map(|(i, window)| {
            let style = if i == state.selected_window {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };
            let mut spans = vec![Span::raw(format!("{}:{}", window.index, window.name))];
            if let Some((sym, color)) = claude_marker(window.claude_state, window.has_claude) {
                spans.push(Span::styled(
                    format!(" {}", sym),
                    Style::default().fg(color),
                ));
            }
            ListItem::new(Line::from(spans)).style(style)
        })
        .collect();

    let title = state
        .sessions
        .get(state.selected_session)
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

    frame.render_stateful_widget(list, area, &mut state.window_list_state);
}

fn render_panes_list(frame: &mut Frame, state: &mut UIState, area: Rect) {
    let is_focused = state.focus == Focus::Panes;
    let border_style = if is_focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let empty_panes: Vec<TmuxPane> = Vec::new();
    let panes = state
        .sessions
        .get(state.selected_session)
        .and_then(|s| s.windows.get(state.selected_window))
        .map(|w| &w.panes)
        .unwrap_or(&empty_panes);

    let items: Vec<ListItem> = panes
        .iter()
        .enumerate()
        .map(|(i, pane)| {
            let style = if i == state.selected_pane {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };
            let mut spans = vec![Span::raw(format!(
                "{}:{} [{}]",
                pane.index, pane.id, pane.current_command
            ))];
            if let Some((sym, color)) = claude_marker(pane.claude_state, pane.has_claude) {
                spans.push(Span::styled(
                    format!(" {}", sym),
                    Style::default().fg(color),
                ));
            }
            ListItem::new(Line::from(spans)).style(style)
        })
        .collect();

    let title = state
        .sessions
        .get(state.selected_session)
        .and_then(|s| s.windows.get(state.selected_window))
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

    frame.render_stateful_widget(list, area, &mut state.pane_list_state);
}

fn render_pane_preview_tree(frame: &mut Frame, state: &UIState, area: Rect) {
    let title = state
        .get_selected_pane_target()
        .map(|t| format!(" Preview: {} ", t))
        .unwrap_or_else(|| " Preview ".to_string());

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);

    let inner = block.inner(area);
    let max_lines = inner.height as usize;

    // Use cached parsed Text (rebuilt only when pane_content changes).
    let text = if let Some(parsed) = state.pane_content_parsed.as_ref() {
        if parsed.lines.len() > max_lines {
            let start = parsed.lines.len().saturating_sub(max_lines);
            Text::from(parsed.lines[start..].to_vec())
        } else {
            parsed.clone()
        }
    } else {
        let mut raw: Vec<&str> = state.pane_content.lines().collect();
        if raw.len() > max_lines {
            raw = raw[raw.len().saturating_sub(max_lines)..].to_vec();
        }
        Text::raw(raw.join("\n"))
    };

    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);
}

fn render_tree_status_bar(frame: &mut Frame, state: &UIState, area: Rect) {
    let status_text = if let Some(ref err) = state.last_error {
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
            Span::styled("s", Style::default().fg(Color::Yellow)),
            Span::raw(":sort "),
            Span::styled("g", Style::default().fg(Color::Yellow)),
            Span::raw(":group "),
            Span::styled("za", Style::default().fg(Color::Yellow)),
            Span::raw(":fold "),
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

fn render_multi_preview(frame: &mut Frame, state: &UIState) {
    let area = frame.area();

    let main_chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);

    let preview_area = main_chunks[0];
    let status_area = main_chunks[1];

    if state.sessions.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" No sessions found ");
        frame.render_widget(block, preview_area);
    } else {
        // Create horizontal layout for sessions
        // Selected session gets 80%, others share 20%
        let session_constraints: Vec<Constraint> = if state.sessions.len() == 1 {
            vec![Constraint::Percentage(100)]
        } else {
            let other_count = state.sessions.len() - 1;
            // Each non-selected session gets an equal share of 20%
            let other_percentage = 30 / other_count as u16;
            state.sessions
                .iter()
                .enumerate()
                .map(|(idx, _)| {
                    if idx == state.multi_session {
                        Constraint::Percentage(70)
                    } else {
                        Constraint::Percentage(other_percentage.max(1))
                    }
                })
                .collect()
        };

        let session_chunks = Layout::horizontal(session_constraints).split(preview_area);

        for (session_idx, (session, session_area)) in
            state.sessions.iter().zip(session_chunks.iter()).enumerate()
        {
            let is_selected_session = session_idx == state.multi_session;

            // Session block style. Sessions running Claude are accented with
            // their Claude state colour unless they are the currently selected
            // session (selection colour wins so focus is never lost).
            let session_border_style = if is_selected_session {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else if let Some(color) =
                claude_border_color(session.claude_state, session.has_claude)
            {
                Style::default().fg(color)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let mut title_spans = vec![Span::raw(format!(" {} ", session.name))];
            if let Some((sym, color)) = claude_marker(session.claude_state, session.has_claude) {
                title_spans.push(Span::styled(
                    format!("{} ", sym),
                    Style::default().fg(color),
                ));
            }

            let session_block = Block::default()
                .borders(Borders::ALL)
                .border_style(session_border_style)
                .title(Line::from(title_spans));

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
                    is_selected_session && window_idx == state.multi_window;

                render_window_preview(frame, window, *window_area, is_selected_window);
            }
        }
    }

    // Status bar
    let status_text = if let Some(ref err) = state.last_error {
        Line::from(vec![Span::styled(
            format!(" Error: {} ", err),
            Style::default().fg(Color::Red),
        )])
    } else {
        let selected_info = state
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
    } else if let Some(color) = claude_border_color(window.claude_state, window.has_claude) {
        Style::default().fg(color)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let cmd = window
        .get_active_pane()
        .map(|p| p.current_command.as_str())
        .unwrap_or("");

    let mut title_spans = vec![Span::raw(format!(
        " {}:{} [{}] ",
        window.index, window.name, cmd
    ))];
    if let Some((sym, color)) = claude_marker(window.claude_state, window.has_claude) {
        title_spans.push(Span::styled(
            format!("{} ", sym),
            Style::default().fg(color),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(title_spans));

    frame.render_widget(block, area);
}

// =============================================================================
// Input Popup
// =============================================================================

fn render_input_popup(frame: &mut Frame, state: &UIState, area: Rect) {
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

    let target_info = state
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

    // input_cursor は char 単位なので、char 単位で前後を分割する
    let before_cursor: String = state.input_buffer.chars().take(state.input_cursor).collect();
    let cursor_char = state
        .input_buffer
        .chars()
        .nth(state.input_cursor)
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".to_string());
    let after_cursor: String = state
        .input_buffer
        .chars()
        .skip(state.input_cursor + 1)
        .collect();

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

fn render_session_name_popup(frame: &mut Frame, state: &UIState, title: &str, label: &str) {
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
    // input_cursor は char 単位なので、char 単位で前後を分割する
    let before_cursor: String = state.input_buffer.chars().take(state.input_cursor).collect();
    let cursor_char = state
        .input_buffer
        .chars()
        .nth(state.input_cursor)
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".to_string());
    let after_cursor: String = state
        .input_buffer
        .chars()
        .skip(state.input_cursor + 1)
        .collect();

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

/// Render the group selection list: every existing group, then an "Ungrouped"
/// entry that clears the assignment and a "New group" entry that switches to
/// text entry. The highlighted row tracks [`UIState::group_choice_index`].
fn render_group_select_popup(frame: &mut Frame, state: &UIState) {
    let area = frame.area();

    let session_name = state
        .sessions
        .get(state.selected_session)
        .map(|s| s.name.as_str())
        .unwrap_or("");

    // Build the rows in the same order the selection index walks them.
    let mut items: Vec<ListItem> = Vec::new();
    for group in &state.group_choices {
        items.push(ListItem::new(Line::from(group.clone())));
    }
    let ungrouped_label = "(Ungrouped)";
    items.push(ListItem::new(Line::from(Span::styled(
        ungrouped_label,
        Style::default().fg(Color::DarkGray),
    ))));
    items.push(ListItem::new(Line::from(Span::styled(
        "+ New group…",
        Style::default().fg(Color::Green),
    ))));

    // Size the popup to the content, clamped so it always fits on screen.
    let list_len = items.len() as u16;
    let popup_width = (area.width * 60 / 100).clamp(40, 70);
    let max_height = area.height.saturating_sub(2).max(5);
    let popup_height = (list_len + 4).min(max_height);

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
        .title(format!(" Group: {} ", session_name))
        .title_bottom(Line::from(" ↑↓:select | Enter:confirm | Esc:cancel ").centered());

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut list_state = ListState::default();
    list_state.select(Some(state.group_choice_index.min(items.len().saturating_sub(1))));

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    );

    frame.render_stateful_widget(list, inner, &mut list_state);
}

fn render_confirm_kill_popup(frame: &mut Frame, state: &UIState) {
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

    let session_name = state
        .sessions
        .get(state.selected_session)
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

    let yes_style = if state.confirm_yes_selected {
        Style::default().fg(Color::Black).bg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let no_style = if !state.confirm_yes_selected {
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
