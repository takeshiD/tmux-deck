mod cli;

use cli::Cli;
use std::io;
use std::process::Command;
use std::time::Duration;

use ansi_to_tui::IntoText;
use color_eyre::Result;
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

/// A character with its associated style
#[derive(Clone, Default)]
struct StyledChar {
    ch: char,
    style: Style,
}

/// Represents a tmux pane with its content
#[derive(Debug, Clone)]
struct PaneInfo {
    session_name: String,
    window_index: u32,
    window_name: String,
    pane_index: u32,
    pane_id: String,
    width: u32,
    height: u32,
    active: bool,
    current_command: String,
    content: String,
}

impl PaneInfo {
    fn target(&self) -> String {
        format!(
            "{}:{}.{}",
            self.session_name, self.window_index, self.pane_index
        )
    }

    fn title(&self) -> String {
        format!(
            "{}:{} [{}] {}",
            self.session_name, self.window_name, self.pane_index, self.current_command
        )
    }
}

/// Application mode
#[derive(Debug, Clone, Copy, PartialEq)]
enum AppMode {
    Normal,
    Input,
}

/// Application state
struct App {
    panes: Vec<PaneInfo>,
    selected: usize,
    interval: Duration,
    last_error: Option<String>,
    columns: usize,
    mode: AppMode,
    input_buffer: String,
    input_cursor: usize,
}

impl App {
    fn new(interval_ms: u64) -> Self {
        Self {
            panes: Vec::new(),
            selected: 0,
            interval: Duration::from_millis(interval_ms),
            last_error: None,
            columns: 2, // Default to 2 columns
            mode: AppMode::Normal,
            input_buffer: String::new(),
            input_cursor: 0,
        }
    }

    fn enter_input_mode(&mut self) {
        self.mode = AppMode::Input;
        self.input_buffer.clear();
        self.input_cursor = 0;
    }

    fn exit_input_mode(&mut self) {
        self.mode = AppMode::Normal;
        self.input_buffer.clear();
        self.input_cursor = 0;
    }

    fn send_input_to_pane(&mut self) {
        if let Some(pane) = self.panes.get(self.selected) {
            let target = pane.target();
            let message = self.input_buffer.clone();

            // Send the message to the pane using tmux send-keys
            let result = Command::new("tmux")
                .args(["send-keys", "-t", &target, &message, "Enter"])
                .output();

            if let Err(e) = result {
                self.last_error = Some(format!("Failed to send keys: {}", e));
            }
        }
        self.exit_input_mode();
    }

    fn input_char(&mut self, c: char) {
        self.input_buffer.insert(self.input_cursor, c);
        self.input_cursor += 1;
    }

    fn input_backspace(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
            self.input_buffer.remove(self.input_cursor);
        }
    }

    fn input_delete(&mut self) {
        if self.input_cursor < self.input_buffer.len() {
            self.input_buffer.remove(self.input_cursor);
        }
    }

    fn input_move_left(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
        }
    }

    fn input_move_right(&mut self) {
        if self.input_cursor < self.input_buffer.len() {
            self.input_cursor += 1;
        }
    }

    fn input_move_home(&mut self) {
        self.input_cursor = 0;
    }

    fn input_move_end(&mut self) {
        self.input_cursor = self.input_buffer.len();
    }

    /// Refresh all pane information and content
    fn refresh_all(&mut self) {
        self.panes.clear();

        // Get all sessions
        let sessions_output = Command::new("tmux")
            .args(["list-sessions", "-F", "#{session_name}"])
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

        for session_name in sessions_str.lines() {
            let session_name = session_name.trim();
            if session_name.is_empty() {
                continue;
            }

            // Get windows for this session
            let windows_output = Command::new("tmux")
                .args([
                    "list-windows",
                    "-t",
                    session_name,
                    "-F",
                    "#{window_index}:#{window_name}",
                ])
                .output();

            if let Ok(output) = windows_output
                && output.status.success()
            {
                let windows_str = String::from_utf8_lossy(&output.stdout);
                for window_line in windows_str.lines() {
                    let w_parts: Vec<&str> = window_line.split(':').collect();
                    if w_parts.len() >= 2 {
                        let window_index: u32 = w_parts[0].parse().unwrap_or(0);
                        let window_name = w_parts[1].to_string();

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
                        if let Ok(p_output) = panes_output
                            && p_output.status.success()
                        {
                            let panes_str = String::from_utf8_lossy(&p_output.stdout);
                            for pane_line in panes_str.lines() {
                                let p_parts: Vec<&str> = pane_line.split(':').collect();
                                if p_parts.len() >= 6 {
                                    let pane = PaneInfo {
                                        session_name: session_name.to_string(),
                                        window_index,
                                        window_name: window_name.clone(),
                                        pane_index: p_parts[1].parse().unwrap_or(0),
                                        pane_id: p_parts[0].to_string(),
                                        width: p_parts[2].parse().unwrap_or(80),
                                        height: p_parts[3].parse().unwrap_or(24),
                                        active: p_parts[4] == "1",
                                        current_command: p_parts[5].to_string(),
                                        content: String::new(),
                                    };
                                    self.panes.push(pane);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Capture content for all panes
        for pane in &mut self.panes {
            let result = Command::new("tmux")
                .args(["capture-pane", "-e", "-p", "-J", "-t", &pane.target()])
                .output();

            if let Ok(output) = result
                && output.status.success()
            {
                pane.content = String::from_utf8_lossy(&output.stdout).to_string();
            }
        }

        // Ensure selection is valid
        if !self.panes.is_empty() {
            self.selected = self.selected.min(self.panes.len() - 1);
        }

        self.last_error = None;
    }

    fn move_selection(&mut self, delta: isize) {
        if self.panes.is_empty() {
            return;
        }
        let len = self.panes.len() as isize;
        let new_idx = (self.selected as isize + delta).rem_euclid(len);
        self.selected = new_idx as usize;
    }

    fn move_up(&mut self) {
        self.move_selection(-(self.columns as isize));
    }

    fn move_down(&mut self) {
        self.move_selection(self.columns as isize);
    }

    fn move_left(&mut self) {
        self.move_selection(-1);
    }

    fn move_right(&mut self) {
        self.move_selection(1);
    }

    fn increase_columns(&mut self) {
        if self.columns < 6 {
            self.columns += 1;
        }
    }

    fn decrease_columns(&mut self) {
        if self.columns > 1 {
            self.columns -= 1;
        }
    }
}

/// Convert ANSI content to a 2D grid of styled characters
fn ansi_to_styled_grid(content: &str) -> Vec<Vec<StyledChar>> {
    // Parse ANSI to ratatui Text
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

/// Shrink styled content to fit within the given dimensions
/// This samples lines and characters to create a thumbnail view while preserving colors
/// Shows content from the bottom (most recent output) first
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

    // Calculate column sampling ratio
    let col_ratio = if source_width > target_width {
        source_width as f64 / target_width as f64
    } else {
        1.0
    };

    // Determine which source rows to display (from bottom)
    // If we have fewer lines than target, show all from top
    // If we have more lines, show the bottom portion with sampling
    let (start_row, row_ratio) = if actual_lines <= target_height {
        // Show all lines, starting from the beginning
        (0, 1.0)
    } else {
        // Sample from bottom portion
        // Calculate how many source rows we need to cover
        let rows_to_show = actual_lines.min(target_height * 2); // Show more context
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

            // If style changes, push current span and start new one
            if styled_char.style != current_style && !current_text.is_empty() {
                spans.push(Span::styled(current_text.clone(), current_style));
                current_text.clear();
            }

            current_style = styled_char.style;
            current_text.push(styled_char.ch);
        }

        // Push remaining text
        if !current_text.is_empty() {
            // Trim trailing spaces from last span
            let trimmed = current_text.trim_end();
            if !trimmed.is_empty() {
                spans.push(Span::styled(trimmed.to_string(), current_style));
            }
        }

        lines.push(Line::from(spans));
    }

    Text::from(lines)
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cmd = Cli::parse_with_color()?;

    // Initialize terminal
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let mut app = App::new(cmd.interval);
    app.refresh_all();
    let result = run_app(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    loop {
        // Draw UI
        terminal.draw(|frame| {
            render_ui(frame, app);
        })?;

        // Handle input with timeout for refresh
        if event::poll(app.interval)? {
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                match app.mode {
                    AppMode::Normal => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('r') => app.refresh_all(),
                        KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                        KeyCode::Left | KeyCode::Char('h') => app.move_left(),
                        KeyCode::Right | KeyCode::Char('l') => app.move_right(),
                        KeyCode::Char('+') | KeyCode::Char('=') => app.increase_columns(),
                        KeyCode::Char('-') | KeyCode::Char('_') => app.decrease_columns(),
                        KeyCode::Char('i') => app.enter_input_mode(),
                        KeyCode::Enter => {
                            // Switch to selected pane
                            if let Some(pane) = app.panes.get(app.selected) {
                                let _ = Command::new("tmux")
                                    .args(["switch-client", "-t", &pane.target()])
                                    .output();
                            }
                        }
                        _ => {}
                    },
                    AppMode::Input => match key.code {
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
        } else {
            // Periodic refresh (only in normal mode)
            if app.mode == AppMode::Normal {
                app.refresh_all();
            }
        }
    }
}

fn render_ui(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Create main layout with status bar
    let main_chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);

    let preview_area = main_chunks[0];
    let status_area = main_chunks[1];

    // Calculate grid layout
    let columns = app.columns;
    let var_name = app.panes.len() + columns - 1;
    let rows = var_name / columns;

    if rows == 0 || app.panes.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" No panes found ");
        frame.render_widget(block, preview_area);
    } else {
        // Create row constraints
        let row_constraints: Vec<Constraint> = (0..rows)
            .map(|_| Constraint::Ratio(1, rows as u32))
            .collect();

        let row_chunks = Layout::vertical(row_constraints).split(preview_area);

        // Create column constraints
        let col_constraints: Vec<Constraint> = (0..columns)
            .map(|_| Constraint::Ratio(1, columns as u32))
            .collect();

        for (row_idx, row_area) in row_chunks.iter().enumerate() {
            let col_chunks = Layout::horizontal(col_constraints.clone()).split(*row_area);

            for (col_idx, cell_area) in col_chunks.iter().enumerate() {
                let pane_idx = row_idx * columns + col_idx;

                if pane_idx < app.panes.len() {
                    let pane = &app.panes[pane_idx];
                    render_pane_preview(frame, pane, *cell_area, pane_idx == app.selected);
                }
            }
        }
    }

    // Render status bar
    let status_text = if let Some(ref err) = app.last_error {
        Line::from(vec![Span::styled(
            format!(" Error: {} ", err),
            Style::default().fg(Color::Red),
        )])
    } else {
        let selected_info = app
            .panes
            .get(app.selected)
            .map(|p| p.target().to_string())
            .unwrap_or_else(|| "None".to_string());

        Line::from(vec![
            Span::styled(" ←→↑↓", Style::default().fg(Color::Yellow)),
            Span::raw(":select "),
            Span::styled("+/-", Style::default().fg(Color::Yellow)),
            Span::raw(":columns "),
            Span::styled("i", Style::default().fg(Color::Yellow)),
            Span::raw(":input "),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(":switch "),
            Span::styled("r", Style::default().fg(Color::Yellow)),
            Span::raw(":refresh "),
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(":quit "),
            Span::raw("| "),
            Span::styled(
                format!(
                    "Panes:{} Cols:{} Selected:{}",
                    app.panes.len(),
                    app.columns,
                    selected_info
                ),
                Style::default().fg(Color::Cyan),
            ),
        ])
    };

    frame.render_widget(
        Paragraph::new(status_text).style(Style::default().bg(Color::DarkGray)),
        status_area,
    );

    // Render input popup if in input mode
    if app.mode == AppMode::Input {
        render_input_popup(frame, app, area);
    }
}

fn render_input_popup(frame: &mut Frame, app: &App, area: Rect) {
    // Calculate popup size and position (centered)
    // let popup_width = (area.width * 70 / 100).min(80).max(40);
    let popup_width = (area.width * 70 / 100).clamp(80, 40);
    let popup_height = 7;

    let popup_x = (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = (area.height.saturating_sub(popup_height)) / 2;

    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    // Get target pane info
    let target_info = app
        .panes
        .get(app.selected)
        .map(|p| p.target())
        .unwrap_or_else(|| "None".to_string());

    // Clear the popup area
    frame.render_widget(Clear, popup_area);

    // Create the popup block
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(" Send to: {} ", target_info))
        .title_bottom(Line::from(" Enter:send | Esc:cancel ").centered());

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Create input layout
    let input_chunks = Layout::vertical([
        Constraint::Length(1), // Label
        Constraint::Length(1), // Spacing
        Constraint::Min(1),    // Input field
    ])
    .split(inner);

    // Render label
    let label = Paragraph::new("Enter message:").style(Style::default().fg(Color::White));
    frame.render_widget(label, input_chunks[0]);

    // Render input field with cursor
    let input_area = input_chunks[2];

    // Build the input text with cursor
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

fn render_pane_preview(frame: &mut Frame, pane: &PaneInfo, area: Rect, is_selected: bool) {
    let border_style = if is_selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if pane.active {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = if area.width > 30 {
        format!(" {} ", pane.title())
    } else if area.width > 15 {
        format!(
            " {}:{}.{} ",
            pane.session_name, pane.window_index, pane.pane_index
        )
    } else {
        format!(" {}.{} ", pane.window_index, pane.pane_index)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner = block.inner(area);

    // Convert ANSI to styled grid and shrink with color preservation
    let styled_grid = ansi_to_styled_grid(&pane.content);
    let shrunk_text = shrink_styled_content(
        &styled_grid,
        inner.width as usize,
        inner.height as usize,
        pane.width,
        pane.height,
    );

    let paragraph = Paragraph::new(shrunk_text).block(block);

    frame.render_widget(paragraph, area);
}
