//! A minimal ANSI/VT screen emulator.
//!
//! Background sessions have no tmux pane to `capture-pane`, so the "screen"
//! preview mode feeds the raw PTY byte stream from `claude logs <id>` through
//! this emulator to reconstruct the on-screen grid, then renders it with
//! colours — approximating what the session's terminal looks like.
//!
//! This is intentionally a *subset* of a real terminal: it handles the
//! sequences `claude logs` actually emits (SGR colours, absolute/relative
//! cursor motion, erase, CR/LF/BS/TAB, and printable text with wide-char
//! awareness) and ignores the rest (mode toggles, OSC titles, …). Faithful
//! enough for a preview, not a real terminal.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use unicode_width::UnicodeWidthChar;

#[derive(Clone)]
struct Cell {
    ch: char,
    style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: Style::default(),
        }
    }
}

struct Screen {
    grid: Vec<Vec<Cell>>,
    w: usize,
    h: usize,
    row: usize,
    col: usize,
    style: Style,
}

impl Screen {
    fn new(w: usize, h: usize) -> Self {
        Self {
            grid: vec![vec![Cell::default(); w]; h],
            w,
            h,
            row: 0,
            col: 0,
            style: Style::default(),
        }
    }

    fn scroll_up(&mut self) {
        self.grid.remove(0);
        self.grid.push(vec![Cell::default(); self.w]);
    }

    fn newline(&mut self) {
        self.row += 1;
        if self.row >= self.h {
            self.scroll_up();
            self.row = self.h - 1;
        }
    }

    fn put(&mut self, c: char) {
        let cw = c.width().unwrap_or(0);
        if cw == 0 {
            return; // control / zero-width
        }
        if self.col >= self.w {
            self.col = 0;
            self.newline();
        }
        if self.row < self.h && self.col < self.w {
            self.grid[self.row][self.col] = Cell {
                ch: c,
                style: self.style,
            };
        }
        self.col += cw;
    }

    fn clamp(&mut self) {
        if self.row >= self.h {
            self.row = self.h - 1;
        }
        if self.col >= self.w {
            self.col = self.w - 1;
        }
    }

    fn erase_line(&mut self, mode: u16) {
        if self.row >= self.h {
            return;
        }
        let (from, to) = match mode {
            1 => (0, self.col.min(self.w.saturating_sub(1)) + 1),
            2 => (0, self.w),
            _ => (self.col, self.w), // 0: cursor to end
        };
        for c in from..to.min(self.w) {
            self.grid[self.row][c] = Cell::default();
        }
    }

    fn erase_display(&mut self, mode: u16) {
        match mode {
            2 | 3 => {
                for r in 0..self.h {
                    self.grid[r] = vec![Cell::default(); self.w];
                }
            }
            1 => {
                for r in 0..self.row.min(self.h) {
                    self.grid[r] = vec![Cell::default(); self.w];
                }
                self.erase_line(1);
            }
            _ => {
                // 0: cursor to end of screen
                self.erase_line(0);
                for r in (self.row + 1)..self.h {
                    self.grid[r] = vec![Cell::default(); self.w];
                }
            }
        }
    }

    /// Apply an SGR (`ESC [ ... m`) parameter list to the current style.
    fn sgr(&mut self, params: &[u16]) {
        let mut i = 0;
        if params.is_empty() {
            self.style = Style::default();
            return;
        }
        while i < params.len() {
            match params[i] {
                0 => self.style = Style::default(),
                1 => self.style = self.style.add_modifier(Modifier::BOLD),
                2 => self.style = self.style.add_modifier(Modifier::DIM),
                3 => self.style = self.style.add_modifier(Modifier::ITALIC),
                4 => self.style = self.style.add_modifier(Modifier::UNDERLINED),
                7 => self.style = self.style.add_modifier(Modifier::REVERSED),
                22 => self.style = self.style.remove_modifier(Modifier::BOLD | Modifier::DIM),
                23 => self.style = self.style.remove_modifier(Modifier::ITALIC),
                24 => self.style = self.style.remove_modifier(Modifier::UNDERLINED),
                27 => self.style = self.style.remove_modifier(Modifier::REVERSED),
                30..=37 => self.style = self.style.fg(basic_color(params[i] - 30)),
                39 => self.style = self.style.fg(Color::Reset),
                40..=47 => self.style = self.style.bg(basic_color(params[i] - 40)),
                49 => self.style = self.style.bg(Color::Reset),
                90..=97 => self.style = self.style.fg(basic_color(params[i] - 90 + 8)),
                100..=107 => self.style = self.style.bg(basic_color(params[i] - 100 + 8)),
                38 | 48 => {
                    let is_fg = params[i] == 38;
                    // 38;5;n  or  38;2;r;g;b
                    if let Some(&kind) = params.get(i + 1) {
                        if kind == 5 {
                            if let Some(&n) = params.get(i + 2) {
                                let col = Color::Indexed(n as u8);
                                self.style = if is_fg {
                                    self.style.fg(col)
                                } else {
                                    self.style.bg(col)
                                };
                            }
                            i += 2;
                        } else if kind == 2 {
                            if let (Some(&r), Some(&g), Some(&b)) =
                                (params.get(i + 2), params.get(i + 3), params.get(i + 4))
                            {
                                let col = Color::Rgb(r as u8, g as u8, b as u8);
                                self.style = if is_fg {
                                    self.style.fg(col)
                                } else {
                                    self.style.bg(col)
                                };
                            }
                            i += 4;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn into_text(self) -> Text<'static> {
        let lines = self
            .grid
            .into_iter()
            .map(|row| {
                // Coalesce adjacent cells with the same style into spans.
                let mut spans: Vec<Span> = Vec::new();
                let mut cur = String::new();
                let mut cur_style: Option<Style> = None;
                for cell in row {
                    match cur_style {
                        Some(s) if s == cell.style => cur.push(cell.ch),
                        _ => {
                            if let Some(s) = cur_style {
                                spans.push(Span::styled(std::mem::take(&mut cur), s));
                            }
                            cur.push(cell.ch);
                            cur_style = Some(cell.style);
                        }
                    }
                }
                if let Some(s) = cur_style {
                    spans.push(Span::styled(cur, s));
                }
                Line::from(spans)
            })
            .collect::<Vec<_>>();
        Text::from(lines)
    }
}

fn basic_color(n: u16) -> Color {
    Color::Indexed(n as u8)
}

/// Parse `input` (a raw PTY byte stream) into a `width`×`height` screen and
/// render it as styled ratatui text.
pub fn render_screen(input: &[u8], width: u16, height: u16) -> Text<'static> {
    let w = (width as usize).max(1);
    let h = (height as usize).max(1);
    let mut s = Screen::new(w, h);

    let text = String::from_utf8_lossy(input);
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    parse_csi(&mut chars, &mut s);
                }
                Some(']') => {
                    chars.next();
                    // OSC: skip until BEL or ESC \
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' {
                            chars.next(); // consume the following '\'
                            break;
                        }
                    }
                }
                // Other ESC sequences (e.g. ESC =, ESC >): consume one byte.
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\r' => s.col = 0,
            '\n' => s.newline(),
            '\x08' => s.col = s.col.saturating_sub(1),
            '\t' => {
                s.col = ((s.col / 8) + 1) * 8;
                if s.col >= w {
                    s.col = w - 1;
                }
            }
            c => s.put(c),
        }
    }
    s.into_text()
}

/// Parse a CSI body (after `ESC [`) up to and including its final byte, and
/// apply it to the screen.
fn parse_csi(chars: &mut std::iter::Peekable<std::str::Chars>, s: &mut Screen) {
    let mut private = false;
    let mut buf = String::new();
    let mut final_byte = '\0';
    while let Some(&c) = chars.peek() {
        chars.next();
        match c {
            '?' | '>' | '!' => private = true,
            '0'..='9' | ';' | ':' => buf.push(c),
            ' '..='/' => { /* intermediate bytes, ignore */ }
            '@'..='~' => {
                final_byte = c;
                break;
            }
            _ => break,
        }
    }
    let params: Vec<u16> = buf
        .split(';')
        .map(|p| p.split(':').next().unwrap_or("").parse().unwrap_or(0))
        .collect();
    let p0 = *params.first().unwrap_or(&0);
    let n = p0.max(1) as usize;

    if private {
        // Private modes (?25h/l, ?2026h/l, ?1049h/l, …): nothing to render.
        // Treat entering the alternate screen as a clear so stale content goes.
        if final_byte == 'h' && params.first() == Some(&1049) {
            s.erase_display(2);
            s.row = 0;
            s.col = 0;
        }
        return;
    }

    match final_byte {
        'm' => s.sgr(&params),
        'H' | 'f' => {
            s.row = (p0.max(1) as usize) - 1;
            s.col = (*params.get(1).unwrap_or(&1)).max(1) as usize - 1;
            s.clamp();
        }
        'A' => s.row = s.row.saturating_sub(n),
        'B' => {
            s.row += n;
            s.clamp();
        }
        'C' => {
            s.col += n;
            s.clamp();
        }
        'D' => s.col = s.col.saturating_sub(n),
        'G' => {
            s.col = (p0.max(1) as usize) - 1;
            s.clamp();
        }
        'd' => {
            s.row = (p0.max(1) as usize) - 1;
            s.clamp();
        }
        'J' => s.erase_display(p0),
        'K' => s.erase_line(p0),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &Text) -> String {
        text.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string()
    }

    #[test]
    fn plain_text_and_newlines() {
        let t = render_screen(b"hello\r\nworld", 20, 4);
        assert_eq!(plain(&t), "hello\nworld");
    }

    #[test]
    fn absolute_cursor_positioning() {
        // Move to row 2 col 3 (1-based) and write.
        let t = render_screen(b"\x1b[2;3HX", 10, 3);
        assert_eq!(plain(&t), "\n  X");
    }

    #[test]
    fn erase_line_and_overwrite() {
        // Write "aaaa", carriage-return, erase line, write "b".
        let t = render_screen(b"aaaa\r\x1b[2Kb", 6, 1);
        assert_eq!(plain(&t), "b");
    }

    #[test]
    fn sgr_colour_sets_style() {
        let t = render_screen(b"\x1b[31mR\x1b[0mN", 6, 1);
        let line = &t.lines[0];
        // First span is the red "R".
        assert_eq!(line.spans[0].content.as_ref(), "R");
        assert_eq!(line.spans[0].style.fg, Some(Color::Indexed(1)));
    }
}

