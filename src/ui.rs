use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::agents::{self, AgentSession, AgentState};
use crate::app::{
    ClaudeState, Focus, InputMode, PopupMode, SessionRow, TmuxPane, TmuxWindow, UIState,
    UNGROUPED_LABEL, ViewMode,
};
use crate::config::{Action, MarkerSet, Theme};

/// Braille "dots" spinner frames (cli-spinners `dots`). Rendered for a marker
/// configured as `"spinner"` (the default `Working` Claude state) so it
/// visibly animates.
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
/// whether a claude process was detected. The glyph and colour come from the
/// user-configurable [`MarkerSet`]; a marker configured as `"spinner"` animates.
/// Hook state wins, otherwise we fall back to the plain "claude is running"
/// marker so behaviour is unchanged when hooks are not installed. Returns
/// `None` when there is nothing to show.
fn claude_marker(
    markers: &MarkerSet,
    state: Option<ClaudeState>,
    has_claude: bool,
) -> Option<(String, Color)> {
    let marker = match state {
        Some(ClaudeState::Working) => &markers.working,
        Some(ClaudeState::Waiting) => &markers.waiting,
        Some(ClaudeState::Done) => &markers.done,
        Some(ClaudeState::Error) => &markers.error,
        None if has_claude => &markers.running,
        None => return None,
    };
    let glyph = if marker.animated {
        spinner_frame().to_string()
    } else {
        marker.glyph.clone()
    };
    Some((glyph, marker.color))
}

/// Border accent colour for a node that is running claude (any state). The
/// border only signals presence; it reuses the marker's colour for that state.
fn claude_border_color(
    markers: &MarkerSet,
    state: Option<ClaudeState>,
    has_claude: bool,
) -> Option<Color> {
    claude_marker(markers, state, has_claude).map(|(_, color)| color)
}

// =============================================================================
// Main UI Rendering
// =============================================================================

pub fn render_ui(frame: &mut Frame, state: &mut UIState) {
    match state.view_mode {
        ViewMode::TreeView => render_tree_view(frame, state),
        ViewMode::MultiPreview => render_multi_preview(frame, state),
        ViewMode::Dashboard => render_dashboard(frame, state),
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
    let left_width = state.layout.session_panel_width.min(100);
    let main_chunks = Layout::horizontal([
        Constraint::Percentage(left_width),
        Constraint::Percentage(100 - left_width),
    ])
    .split(area);

    let left_panel = main_chunks[0];
    let right_panel = main_chunks[1];

    // Left panel: Sessions | Windows | Panes (vertical stack)
    let [s, w, p] = state.layout.tree_split;
    let left_chunks = Layout::vertical([
        Constraint::Percentage(s),
        Constraint::Percentage(w),
        Constraint::Percentage(p),
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
    let theme = state.theme;
    let is_focused = state.focus == Focus::Sessions;
    let border_style = if is_focused {
        Style::default().fg(theme.focus_border).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.unfocus_border)
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
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD);
                if is_selected {
                    style = style.bg(theme.selection_bg).fg(theme.selection_fg);
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
                    Style::default().bg(theme.selection_bg).fg(theme.selection_fg)
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
                    claude_marker(&state.hooks.claude, session.claude_state, session.has_claude)
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
    let theme = state.theme;
    let is_focused = state.focus == Focus::Windows;
    let border_style = if is_focused {
        Style::default().fg(theme.focus_border).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.unfocus_border)
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
                Style::default().bg(theme.selection_bg).fg(theme.selection_fg)
            } else {
                Style::default()
            };
            let mut spans = vec![Span::raw(format!("{}:{}", window.index, window.name))];
            if let Some((sym, color)) =
                claude_marker(&state.hooks.claude, window.claude_state, window.has_claude)
            {
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
    let theme = state.theme;
    let is_focused = state.focus == Focus::Panes;
    let border_style = if is_focused {
        Style::default().fg(theme.focus_border).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.unfocus_border)
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
                Style::default().bg(theme.selection_bg).fg(theme.selection_fg)
            } else {
                Style::default()
            };
            let mut spans = vec![Span::raw(format!(
                "{}:{} [{}]",
                pane.index, pane.id, pane.current_command
            ))];
            if let Some((sym, color)) =
                claude_marker(&state.hooks.claude, pane.claude_state, pane.has_claude)
            {
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
        .border_style(Style::default().fg(state.theme.accent))
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
    let theme = state.theme;
    let status_text = if let Some(ref err) = state.last_error {
        Line::from(vec![Span::styled(
            format!(" Error: {} ", err),
            Style::default().fg(theme.error),
        )])
    } else {
        let kb = &state.keybindings;
        // `j/k`, `Tab`, `za` and `Space×2` are fixed (not remappable); the rest
        // reflect the user's key bindings so the hint bar always stays accurate.
        Line::from(vec![
            Span::styled("j/k", Style::default().fg(theme.focus_border)),
            Span::raw(":move "),
            Span::styled("Tab", Style::default().fg(theme.focus_border)),
            Span::raw(":focus "),
            Span::styled(kb.label(Action::Sort), Style::default().fg(theme.focus_border)),
            Span::raw(":sort "),
            Span::styled(kb.label(Action::Group), Style::default().fg(theme.focus_border)),
            Span::raw(":group "),
            Span::styled("za", Style::default().fg(theme.focus_border)),
            Span::raw(":fold "),
            Span::styled("Space×2", Style::default().fg(theme.highlight)),
            Span::raw(":multi "),
            Span::styled(kb.label(Action::Dashboard), Style::default().fg(theme.focus_border)),
            Span::raw(":fleet "),
            Span::styled(kb.label(Action::NewSession), Style::default().fg(theme.success)),
            Span::raw(":new "),
            Span::styled(kb.label(Action::RenameSession), Style::default().fg(theme.success)),
            Span::raw(":rename "),
            Span::styled(kb.label(Action::KillSession), Style::default().fg(theme.error)),
            Span::raw(":kill "),
            Span::styled(kb.label(Action::Quit), Style::default().fg(theme.focus_border)),
            Span::raw(":quit"),
        ])
    };

    frame.render_widget(
        Paragraph::new(status_text).style(Style::default().bg(theme.status_bar_bg)),
        area,
    );
}

// =============================================================================
// Fleet Dashboard Rendering
// =============================================================================

/// Human-friendly elapsed time: `12s`, `3m`, `2h`.
fn format_elapsed(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

/// Marker glyph + colour for a background-session state, reusing the user's
/// configured Claude marker set.
fn agent_marker(markers: &MarkerSet, state: AgentState, theme: &Theme) -> (String, Color) {
    let mapped = match state {
        AgentState::Blocked => Some(ClaudeState::Waiting),
        AgentState::Working => Some(ClaudeState::Working),
        AgentState::Done => Some(ClaudeState::Done),
        AgentState::Failed => Some(ClaudeState::Error),
        AgentState::Idle | AgentState::Stopped | AgentState::Unknown => None,
    };
    let has_claude = matches!(state, AgentState::Idle);
    claude_marker(markers, mapped, has_claude)
        .unwrap_or_else(|| ("∙".to_string(), theme.unfocus_border))
}

/// Render the agent view: Claude Code background sessions grouped by working
/// directory, like `claude agents`. `Enter` attaches to the selected session.
fn render_dashboard(frame: &mut Frame, state: &UIState) {
    let area = frame.area();
    let theme = state.theme;

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);
    let header_area = chunks[0];
    let list_area = chunks[1];
    let status_area = chunks[2];

    // Header: summary counts, like the agent view's "N awaiting input · …".
    let (needs, working, completed) = state.agent_group_counts();
    let header = Line::from(vec![
        Span::styled(" Claude Agents ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{needs} awaiting input"), Style::default().fg(theme.highlight)),
        Span::raw(" · "),
        Span::styled(format!("{working} working"), Style::default().fg(theme.focus_border)),
        Span::raw(" · "),
        Span::styled(format!("{completed} completed"), Style::default().fg(theme.unfocus_border)),
    ]);
    frame.render_widget(Paragraph::new(header), header_area);

    let block = Block::default().borders(Borders::ALL);

    if state.agent_sessions.is_empty() {
        let empty = Paragraph::new(
            "No background sessions.\n\nStart one with `claude --bg \"<task>\"` or `/bg` inside a session.",
        )
        .style(Style::default().fg(theme.unfocus_border))
        .block(block);
        frame.render_widget(empty, list_area);
        render_dashboard_status_bar(frame, state, status_area);
        return;
    }

    let selected = state.agent_selected.min(state.agent_sessions.len() - 1);

    // With the preview open, split the body into list | preview.
    let (list_rect, preview_rect) = if state.agent_preview {
        let cols = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(list_area);
        (cols[0], Some(cols[1]))
    } else {
        (list_area, None)
    };

    // Build rows, inserting a directory header whenever the cwd changes.
    let mut items: Vec<ListItem> = Vec::new();
    let mut last_cwd: Option<&str> = None;
    for (idx, s) in state.agent_sessions.iter().enumerate() {
        if last_cwd != Some(s.cwd.as_str()) {
            items.push(ListItem::new(Line::from(Span::styled(
                agents::abbreviate_path(&s.cwd),
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
            ))));
            last_cwd = Some(s.cwd.as_str());
        }
        items.push(agent_row_item(state, s, idx == selected));
    }

    frame.render_widget(List::new(items).block(block), list_rect);
    if let Some(preview_rect) = preview_rect {
        render_agent_preview(frame, state, &state.agent_sessions[selected], preview_rect);
    }
    render_dashboard_status_bar(frame, state, status_area);

    // The summary popup is independent of the preview panel.
    if state.agent_summary_open {
        render_agent_summary_popup(frame, state, &state.agent_sessions[selected], area);
    }
}

/// Style a reconstructed transcript line by its leading glyph: user prompts,
/// assistant text and tool calls each get a distinct colour.
fn transcript_line_style(line: &str, theme: &Theme) -> Style {
    match line.chars().next() {
        Some('▸') => Style::default().fg(theme.highlight), // user
        Some('⏺') => Style::default().fg(theme.success),   // tool call
        Some('●') => Style::default().fg(theme.focus_border), // assistant
        _ => Style::default(),
    }
}

/// Preview panel for the selected session: a tail of the conversation
/// transcript, coloured by role. (The execution summary is a separate popup.)
fn render_agent_preview(frame: &mut Frame, state: &UIState, session: &AgentSession, area: Rect) {
    let theme = state.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(format!(
            " {} ",
            truncate(&session.name, area.width.saturating_sub(4) as usize)
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let tail = session
        .transcript_path
        .as_ref()
        .map(|p| agents::transcript_tail(p, inner.height as usize))
        .unwrap_or_else(|| vec!["(no transcript)".to_string()]);
    let lines: Vec<Line> = tail
        .into_iter()
        .map(|l| {
            let style = transcript_line_style(&l, &theme);
            Line::from(Span::styled(l, style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Centred popup showing the on-demand execution summary for a session,
/// independent of the preview panel. Opened with `s`, closed with `Esc`.
fn render_agent_summary_popup(
    frame: &mut Frame,
    state: &UIState,
    session: &AgentSession,
    area: Rect,
) {
    let theme = state.theme;
    let popup = centered_rect(70, 50, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.highlight))
        .title(format!(" Summary — {} ", truncate(&session.name, 30)))
        .title_bottom(Line::from(" s:regenerate · Esc:close ").centered());

    let body = match state.summary_status(&session.id) {
        Some(crate::app::SummaryStatus::Pending) => {
            Text::styled("summarizing…", Style::default().fg(theme.unfocus_border))
        }
        Some(crate::app::SummaryStatus::Ready(text)) => Text::raw(text.clone()),
        Some(crate::app::SummaryStatus::Failed(e)) => {
            Text::styled(format!("summary failed: {e}"), Style::default().fg(theme.error))
        }
        None => Text::styled(
            "press s to summarize",
            Style::default().fg(theme.unfocus_border),
        ),
    };
    frame.render_widget(Paragraph::new(body).block(block).wrap(ratatui::widgets::Wrap { trim: false }), popup);
}

/// A rect centred in `area`, sized as a percentage of it.
fn centered_rect(pct_w: u16, pct_h: u16, area: Rect) -> Rect {
    let w = area.width * pct_w / 100;
    let h = area.height * pct_h / 100;
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// One agent-view row: state marker, name, summary, PR labels, elapsed time.
fn agent_row_item<'a>(state: &UIState, session: &AgentSession, selected: bool) -> ListItem<'a> {
    let theme = state.theme;
    let (mut glyph, color) = agent_marker(&state.hooks.claude, session.state, &theme);
    // A finished/exited worker is shown with the agent view's "∙" dot shape.
    if !session.alive {
        glyph = "∙".to_string();
    }

    let cursor = if selected { "▌" } else { " " };
    let mut spans = vec![
        Span::styled(format!("{cursor}{glyph} "), Style::default().fg(color)),
        Span::styled(
            format!("{:<24} ", truncate(&session.name, 24)),
            Style::default().fg(theme.focus_border),
        ),
        Span::raw(format!("{:<46} ", truncate(&session.summary, 46))),
    ];
    if !session.prs.is_empty() {
        let label = if session.prs.len() == 1 {
            format!("PR #{}", session.prs[0].id)
        } else {
            format!("{} PRs", session.prs.len())
        };
        spans.push(Span::styled(format!("{label:<8} "), Style::default().fg(theme.success)));
    }
    spans.push(Span::styled(
        format!("{:>4}", format_elapsed(session.elapsed_secs)),
        Style::default().fg(theme.unfocus_border),
    ));

    let style = if selected {
        Style::default().bg(theme.selection_bg).fg(theme.selection_fg)
    } else {
        Style::default()
    };
    ListItem::new(Line::from(spans)).style(style)
}

/// Truncate to `max` display chars with an ellipsis, padding handled by caller.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max - 1).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

fn render_dashboard_status_bar(frame: &mut Frame, state: &UIState, area: Rect) {
    let theme = state.theme;
    let kb = &state.keybindings;
    let status_text = Line::from(vec![
        Span::styled("j/k", Style::default().fg(theme.focus_border)),
        Span::raw(":move "),
        Span::styled(kb.label(Action::Enter), Style::default().fg(theme.highlight)),
        Span::raw(":attach "),
        Span::styled("p", Style::default().fg(theme.focus_border)),
        Span::raw(":preview "),
        Span::styled("s", Style::default().fg(theme.focus_border)),
        Span::raw(":summary "),
        Span::styled(kb.label(Action::Dashboard), Style::default().fg(theme.focus_border)),
        Span::raw(":back "),
        Span::styled(kb.label(Action::Quit), Style::default().fg(theme.focus_border)),
        Span::raw(":quit"),
    ]);
    frame.render_widget(
        Paragraph::new(status_text).style(Style::default().bg(theme.status_bar_bg)),
        area,
    );
}

// =============================================================================
// MultiPreview Rendering
// =============================================================================

fn render_multi_preview(frame: &mut Frame, state: &UIState) {
    let area = frame.area();
    let theme = state.theme;

    let main_chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);

    let preview_area = main_chunks[0];
    let status_area = main_chunks[1];

    if state.sessions.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" No sessions found ");
        frame.render_widget(block, preview_area);
    } else {
        // Create horizontal layout for sessions: the selected session gets
        // `multi_selected_ratio`%, the rest share what remains.
        let selected_ratio = state.layout.multi_selected_ratio.min(100);
        let session_constraints: Vec<Constraint> = if state.sessions.len() == 1 {
            vec![Constraint::Percentage(100)]
        } else {
            let other_count = state.sessions.len() - 1;
            let other_percentage = (100 - selected_ratio) / other_count as u16;
            state.sessions
                .iter()
                .enumerate()
                .map(|(idx, _)| {
                    if idx == state.multi_session {
                        Constraint::Percentage(selected_ratio)
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
                Style::default().fg(theme.focus_border).add_modifier(Modifier::BOLD)
            } else if let Some(color) =
                claude_border_color(&state.hooks.claude, session.claude_state, session.has_claude)
            {
                Style::default().fg(color)
            } else {
                Style::default().fg(theme.unfocus_border)
            };

            let mut title_spans = vec![Span::raw(format!(" {} ", session.name))];
            if let Some((sym, color)) =
                claude_marker(&state.hooks.claude, session.claude_state, session.has_claude)
            {
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
                    .style(Style::default().fg(theme.unfocus_border));
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

                render_window_preview(
                    frame,
                    &state.theme,
                    &state.hooks.claude,
                    window,
                    *window_area,
                    is_selected_window,
                );
            }
        }
    }

    // Status bar
    let status_text = if let Some(ref err) = state.last_error {
        Line::from(vec![Span::styled(
            format!(" Error: {} ", err),
            Style::default().fg(theme.error),
        )])
    } else {
        let selected_info = state
            .get_multi_selected_target()
            .unwrap_or_else(|| "None".to_string());

        let kb = &state.keybindings;
        Line::from(vec![
            Span::styled("h/l", Style::default().fg(theme.focus_border)),
            Span::raw(":session "),
            Span::styled("j/k", Style::default().fg(theme.focus_border)),
            Span::raw(":window "),
            Span::styled("Space×2", Style::default().fg(theme.highlight)),
            Span::raw(":tree "),
            Span::styled(kb.label(Action::NewSession), Style::default().fg(theme.success)),
            Span::raw(":new "),
            Span::styled(kb.label(Action::RenameSession), Style::default().fg(theme.success)),
            Span::raw(":rename "),
            Span::styled(kb.label(Action::KillSession), Style::default().fg(theme.error)),
            Span::raw(":kill "),
            Span::styled(kb.label(Action::Quit), Style::default().fg(theme.focus_border)),
            Span::raw(":quit "),
            Span::raw("| "),
            Span::styled(
                format!("Sel:{}", selected_info),
                Style::default().fg(theme.accent),
            ),
        ])
    };

    frame.render_widget(
        Paragraph::new(status_text).style(Style::default().bg(theme.status_bar_bg)),
        status_area,
    );
}

fn render_window_preview(
    frame: &mut Frame,
    theme: &Theme,
    markers: &MarkerSet,
    window: &TmuxWindow,
    area: Rect,
    is_selected: bool,
) {
    let border_style = if is_selected {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else if let Some(color) = claude_border_color(markers, window.claude_state, window.has_claude) {
        Style::default().fg(color)
    } else {
        Style::default().fg(theme.unfocus_border)
    };

    let cmd = window
        .get_active_pane()
        .map(|p| p.current_command.as_str())
        .unwrap_or("");

    let mut title_spans = vec![Span::raw(format!(
        " {}:{} [{}] ",
        window.index, window.name, cmd
    ))];
    if let Some((sym, color)) = claude_marker(markers, window.claude_state, window.has_claude) {
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
        .border_style(Style::default().fg(state.theme.accent))
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
        .style(Style::default().fg(Color::White).bg(Color::DarkGray));

    frame.render_widget(input_paragraph, input_area);
}

// =============================================================================
// Session Operation Popups
// =============================================================================

fn render_session_name_popup(frame: &mut Frame, state: &UIState, title: &str, label: &str) {
    let area = frame.area();
    let popup_width = (area.width * 60 / 100).clamp(40, 70);
    // border(1) + label(1) + input(1) + border(1) = 4 rows: the input field is
    // a single line tall.
    let popup_height = 4;

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
        .border_style(Style::default().fg(state.theme.accent))
        .title(format!(" {} ", title))
        .title_bottom(Line::from(" Enter:confirm | Esc:cancel ").centered());

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let input_chunks =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);

    let label_widget = Paragraph::new(label).style(Style::default().fg(Color::White));
    frame.render_widget(label_widget, input_chunks[0]);

    let input_area = input_chunks[1];

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
        .style(Style::default().fg(Color::White).bg(Color::DarkGray));

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
        Style::default().fg(state.theme.unfocus_border),
    ))));
    items.push(ListItem::new(Line::from(Span::styled(
        "+ New group…",
        Style::default().fg(state.theme.success),
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
        .border_style(Style::default().fg(state.theme.accent))
        .title(format!(" Group: {} ", session_name))
        .title_bottom(Line::from(" ↑↓:select | Enter:confirm | Esc:cancel ").centered());

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut list_state = ListState::default();
    list_state.select(Some(state.group_choice_index.min(items.len().saturating_sub(1))));

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(state.theme.accent)
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
        .border_style(Style::default().fg(state.theme.error))
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
        Style::default().fg(Color::Black).bg(state.theme.error).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(state.theme.unfocus_border)
    };

    let no_style = if !state.confirm_yes_selected {
        Style::default().fg(Color::Black).bg(state.theme.success).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(state.theme.unfocus_border)
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

#[cfg(test)]
mod cursor_alignment_tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn format_elapsed_scales_units() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(59), "59s");
        assert_eq!(format_elapsed(60), "1m");
        assert_eq!(format_elapsed(3599), "59m");
        assert_eq!(format_elapsed(3600), "1h");
    }

    /// 白背景（カーソルブロック）のセルの (x, y) を返す。
    fn cursor_cell(buf: &ratatui::buffer::Buffer) -> Option<(u16, u16)> {
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf.cell((x, y)).unwrap().style().bg == Some(Color::White) {
                    return Some((x, y));
                }
            }
        }
        None
    }

    fn render_name_popup_cursor(text: &str) -> Option<(u16, u16)> {
        let mut state = UIState::new(crate::config::Config::default());
        state.popup_mode = Some(PopupMode::NewSession);
        for c in text.chars() {
            state.input_char(c);
        }
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        term.draw(|f| render_session_name_popup(f, &state, "New Session", "Label:"))
            .unwrap();
        cursor_cell(term.backend().buffer())
    }

    #[test]
    fn cursor_row_is_stable_between_empty_and_filled() {
        let empty = render_name_popup_cursor("").expect("cursor visible when empty");
        let filled = render_name_popup_cursor("abc").expect("cursor visible with text");
        // 行が一致していること（以前は空文字時に1行下へずれていた）
        assert_eq!(empty.1, filled.1, "cursor row must not shift");
        // 空文字時はカーソルが先頭列、文字入力後は文字数ぶん右
        assert_eq!(filled.0, empty.0 + 3, "cursor should advance by char count");
    }

    #[test]
    fn cursor_row_is_stable_with_multibyte() {
        let empty = render_name_popup_cursor("").expect("cursor visible when empty");
        let jp = render_name_popup_cursor("あい").expect("cursor visible with text");
        assert_eq!(empty.1, jp.1, "cursor row must not shift with multibyte input");
    }

    #[test]
    fn dashboard_renders_without_panic() {
        // Empty agent view: exercises the empty-state, header and status bar.
        let mut state = UIState::new(crate::config::Config::default());
        state.view_mode = ViewMode::Dashboard;
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| render_ui(f, &mut state)).unwrap();
    }

    #[test]
    fn agent_view_renders_grouped_sessions_without_panic() {
        // Sessions across two directories and several states. Exercises the
        // directory headers, state markers, PR labels and selection highlight.
        fn sess(id: &str, name: &str, state: AgentState, cwd: &str, prs: &[&str]) -> AgentSession {
            AgentSession {
                id: id.to_string(),
                name: name.to_string(),
                state,
                summary: "did some work".to_string(),
                cwd: cwd.to_string(),
                elapsed_secs: 120,
                prs: prs.iter().map(|p| crate::agents::PrRef { id: p.to_string() }).collect(),
                alive: true,
                transcript_path: None,
            }
        }
        let mut state = UIState::new(crate::config::Config::default());
        state.view_mode = ViewMode::Dashboard;
        state.agent_sessions = vec![
            sess("a1", "telemetry fix", AgentState::Blocked, "/home/u/proj-a", &["12"]),
            sess("a2", "refactor", AgentState::Working, "/home/u/proj-a", &[]),
            sess("b1", "docs", AgentState::Done, "/home/u/proj-b", &["3", "4"]),
        ];

        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| render_ui(f, &mut state)).unwrap();

        // With the transcript preview split open.
        state.agent_preview = true;
        term.draw(|f| render_ui(f, &mut state)).unwrap();

        // With the (independent) summary popup open.
        state.agent_summary_open = true;
        state.set_summary_pending("a1".to_string());
        term.draw(|f| render_ui(f, &mut state)).unwrap();
    }
}
