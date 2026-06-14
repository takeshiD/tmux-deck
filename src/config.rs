//! User configuration loaded from a TOML file.
//!
//! tmux-deck is zero-config by default: when no config file exists the built-in
//! defaults reproduce the previous hard-coded behaviour exactly. A config file
//! lets the user tune the preview interval, the colour theme, key bindings, the
//! per-state Claude hook markers (and, in future, Codex markers), the panel
//! layout and a handful of behavioural toggles.
//!
//! Loading is best-effort, mirroring [`crate::group::GroupStore`]: a missing
//! file yields defaults, and an unreadable / malformed file logs a warning and
//! falls back to defaults so a broken config can never stop the app starting.
//!
//! Resolution order for the file path:
//!   1. `--config <path>` on the CLI (`~` is expanded)
//!   2. `$XDG_CONFIG_HOME/tmux-deck/config.toml` (via the `directories` crate)
//!   3. built-in defaults (no file)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use directories::ProjectDirs;
use ratatui::style::Color;
use serde::Deserialize;
use serde::de::{self, Deserializer};
use tracing::{debug, warn};

use crate::app::{SessionSort, SessionSortKey, SortDirection, ViewMode};

// =============================================================================
// Top-level config
// =============================================================================

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub preview: PreviewConfig,
    pub theme: ThemeConfig,
    pub keybindings: KeyBindings,
    pub hooks: HooksConfig,
    pub layout: LayoutConfig,
    pub behavior: BehaviorConfig,
    pub agents: AgentsConfig,
}

impl Config {
    /// Load the config, preferring an explicit `--config` path, then the XDG
    /// config dir, then built-in defaults. Never fails: any error degrades to
    /// the default config with a warning.
    pub fn load(cli_path: Option<&Path>) -> Self {
        let path = cli_path
            .map(expand_tilde)
            .or_else(Self::default_path);
        let Some(path) = path else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str::<Config>(&contents) {
                Ok(cfg) => {
                    debug!("loaded config from {}", path.display());
                    cfg
                }
                Err(e) => {
                    warn!("failed to parse config {}: {e}; using defaults", path.display());
                    Self::default()
                }
            },
            // Missing file is the common zero-config case: silently use defaults.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                warn!("failed to read config {}: {e}; using defaults", path.display());
                Self::default()
            }
        }
    }

    fn default_path() -> Option<PathBuf> {
        let dirs = ProjectDirs::from("dev", "tkcd", "tmux-deck")?;
        Some(dirs.config_dir().join("config.toml"))
    }
}

/// Expand a leading `~` to the user's home directory.
fn expand_tilde(p: &Path) -> PathBuf {
    if let Ok(stripped) = p.strip_prefix("~")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(stripped);
    }
    p.to_path_buf()
}

// =============================================================================
// [preview]
// =============================================================================

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PreviewConfig {
    /// Preview refresh interval in milliseconds. `None` lets the CLI flag / the
    /// built-in default (300ms) win, so the precedence is CLI > config > 300.
    pub interval: Option<u64>,
}

// =============================================================================
// [agents]
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    /// Model passed to `claude -p` when generating an execution summary for the
    /// selected background session (agent view, `s`). Accepts a `claude`
    /// `--model` value such as an alias (`haiku`, `sonnet`, `opus`) or a full id.
    pub summary_model: String,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            summary_model: "haiku".to_string(),
        }
    }
}

// =============================================================================
// [behavior]
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BehaviorConfig {
    /// View shown on startup: `tree` or `multi`.
    pub default_view: String,
    /// Initial session sort: `recent`, `recent_asc`, `abc`, `abc_asc`.
    pub default_sort: String,
    /// Window (ms) within which a second Space press toggles the view mode.
    pub double_space_ms: u64,
    /// Whether selecting a session/window (Enter) exits tmux-deck after the
    /// tmux client switch. When false, the deck stays open.
    pub exit_on_switch: bool,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            default_view: "tree".to_string(),
            default_sort: "recent".to_string(),
            double_space_ms: 300,
            exit_on_switch: true,
        }
    }
}

impl BehaviorConfig {
    pub fn view_mode(&self) -> ViewMode {
        match self.default_view.to_ascii_lowercase().as_str() {
            "multi" | "multipreview" => ViewMode::MultiPreview,
            _ => ViewMode::TreeView,
        }
    }

    pub fn session_sort(&self) -> SessionSort {
        match self.default_sort.to_ascii_lowercase().as_str() {
            "recent_asc" | "oldest" => SessionSort {
                key: SessionSortKey::LastAttached,
                direction: SortDirection::Asc,
            },
            "abc" | "alphabet" => SessionSort {
                key: SessionSortKey::Alphabet,
                direction: SortDirection::Desc,
            },
            "abc_asc" | "alphabet_asc" => SessionSort {
                key: SessionSortKey::Alphabet,
                direction: SortDirection::Asc,
            },
            // "recent" / unknown -> the historical default (most recent first).
            _ => SessionSort {
                key: SessionSortKey::LastAttached,
                direction: SortDirection::Desc,
            },
        }
    }
}

// =============================================================================
// [layout]
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    /// Width of the left (lists) panel as a percentage of the screen; the
    /// preview takes the rest. TreeView only.
    pub session_panel_width: u16,
    /// Vertical split of the left panel into Sessions / Windows / Panes, as
    /// three percentages.
    pub tree_split: [u16; 3],
    /// In MultiPreview, the width percentage given to the selected session; the
    /// remaining sessions share what's left.
    pub multi_selected_ratio: u16,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            session_panel_width: 30,
            tree_split: [30, 35, 35],
            multi_selected_ratio: 70,
        }
    }
}

// =============================================================================
// [theme]
// =============================================================================

/// Raw theme config as written in TOML: a preset name plus optional per-role
/// colour overrides. Resolved into a concrete [`Theme`] via [`Self::resolve`].
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub preset: String,
    pub colors: HashMap<String, String>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            preset: "default".to_string(),
            colors: HashMap::new(),
        }
    }
}

impl ThemeConfig {
    /// Resolve the preset and apply any per-role overrides. Unknown colour
    /// strings / role names are ignored with a warning so a typo never breaks
    /// rendering.
    pub fn resolve(&self) -> Theme {
        let mut theme = Theme::preset(&self.preset);
        for (role, value) in &self.colors {
            match parse_color(value) {
                Some(color) if theme.set(role, color) => {}
                Some(_) => warn!("unknown theme colour role '{role}', ignoring"),
                None => warn!("invalid colour '{value}' for role '{role}', ignoring"),
            }
        }
        theme
    }
}

/// A resolved set of semantic UI colours. Roles are intentionally coarse so the
/// presets stay easy to define; the renderer maps each widget onto one role.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Border of the focused list.
    pub focus_border: Color,
    /// Border of unfocused lists.
    pub unfocus_border: Color,
    /// General accent: preview/popup borders, headers, info text.
    pub accent: Color,
    /// Background of the selected row.
    pub selection_bg: Color,
    /// Foreground of the selected row.
    pub selection_fg: Color,
    /// Background of the status bar.
    pub status_bar_bg: Color,
    /// Errors and destructive actions (e.g. kill).
    pub error: Color,
    /// Success / creation accents (e.g. new / rename hints, "No" button).
    pub success: Color,
    /// Attention accent used sparingly (e.g. the multi-preview hint).
    pub highlight: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::preset("default")
    }
}

impl Theme {
    /// Apply an override to the named role. Returns false for unknown roles.
    fn set(&mut self, role: &str, color: Color) -> bool {
        match role {
            "focus_border" => self.focus_border = color,
            "unfocus_border" => self.unfocus_border = color,
            "accent" => self.accent = color,
            "selection_bg" => self.selection_bg = color,
            "selection_fg" => self.selection_fg = color,
            "status_bar_bg" => self.status_bar_bg = color,
            "error" => self.error = color,
            "success" => self.success = color,
            "highlight" => self.highlight = color,
            _ => return false,
        }
        true
    }

    /// Look up a named preset, falling back to `default` for unknown names.
    pub fn preset(name: &str) -> Self {
        let rgb = Color::Rgb;
        match name.to_ascii_lowercase().as_str() {
            // The historical hard-coded palette, kept byte-for-byte so an
            // unconfigured deck looks exactly as before.
            "default" => Self {
                focus_border: Color::Yellow,
                unfocus_border: Color::DarkGray,
                accent: Color::Cyan,
                selection_bg: Color::DarkGray,
                selection_fg: Color::White,
                status_bar_bg: Color::DarkGray,
                error: Color::Red,
                success: Color::Green,
                highlight: Color::Magenta,
            },
            // Distinguished by brightness, not hue (colour-blind friendly).
            "monochrome" => Self {
                focus_border: rgb(0xff, 0xff, 0xff),
                unfocus_border: rgb(0x5c, 0x63, 0x70),
                accent: rgb(0xab, 0xb2, 0xbf),
                selection_bg: rgb(0x3e, 0x44, 0x51),
                selection_fg: rgb(0xff, 0xff, 0xff),
                status_bar_bg: rgb(0x3e, 0x44, 0x51),
                error: rgb(0xff, 0xff, 0xff),
                success: rgb(0xab, 0xb2, 0xbf),
                highlight: rgb(0xff, 0xff, 0xff),
            },
            "dracula" => Self {
                focus_border: rgb(0xf1, 0xfa, 0x8c),  // yellow
                unfocus_border: rgb(0x62, 0x72, 0xa4), // comment
                accent: rgb(0x8b, 0xe9, 0xfd),         // cyan
                selection_bg: rgb(0x44, 0x47, 0x5a),   // current line
                selection_fg: rgb(0xf8, 0xf8, 0xf2),   // foreground
                status_bar_bg: rgb(0x44, 0x47, 0x5a),
                error: rgb(0xff, 0x55, 0x55),          // red
                success: rgb(0x50, 0xfa, 0x7b),        // green
                highlight: rgb(0xbd, 0x93, 0xf9),      // purple
            },
            "nord" => Self {
                focus_border: rgb(0xeb, 0xcb, 0x8b),
                unfocus_border: rgb(0x4c, 0x56, 0x6a),
                accent: rgb(0x88, 0xc0, 0xd0),
                selection_bg: rgb(0x43, 0x4c, 0x5e),
                selection_fg: rgb(0xec, 0xef, 0xf4),
                status_bar_bg: rgb(0x3b, 0x42, 0x52),
                error: rgb(0xbf, 0x61, 0x6a),
                success: rgb(0xa3, 0xbe, 0x8c),
                highlight: rgb(0xb4, 0x8e, 0xad),
            },
            "gruvbox" => Self {
                focus_border: rgb(0xfa, 0xbd, 0x2f),
                unfocus_border: rgb(0x92, 0x83, 0x74),
                accent: rgb(0x8e, 0xc0, 0x7c),
                selection_bg: rgb(0x3c, 0x38, 0x36),
                selection_fg: rgb(0xeb, 0xdb, 0xb2),
                status_bar_bg: rgb(0x3c, 0x38, 0x36),
                error: rgb(0xfb, 0x49, 0x34),
                success: rgb(0xb8, 0xbb, 0x26),
                highlight: rgb(0xd3, 0x86, 0x9b),
            },
            "tokyonight" => Self {
                focus_border: rgb(0xe0, 0xaf, 0x68),
                unfocus_border: rgb(0x56, 0x5f, 0x89),
                accent: rgb(0x7d, 0xcf, 0xff),
                selection_bg: rgb(0x28, 0x2e, 0x44),
                selection_fg: rgb(0xc0, 0xca, 0xf5),
                status_bar_bg: rgb(0x24, 0x28, 0x3b),
                error: rgb(0xf7, 0x76, 0x8e),
                success: rgb(0x9e, 0xce, 0x6a),
                highlight: rgb(0xbb, 0x9a, 0xf7),
            },
            "catppuccin" => Self {
                focus_border: rgb(0xf9, 0xe2, 0xaf),
                unfocus_border: rgb(0x6c, 0x70, 0x86),
                accent: rgb(0x89, 0xdc, 0xeb),
                selection_bg: rgb(0x31, 0x32, 0x44),
                selection_fg: rgb(0xcd, 0xd6, 0xf4),
                status_bar_bg: rgb(0x31, 0x32, 0x44),
                error: rgb(0xf3, 0x8b, 0xa8),
                success: rgb(0xa6, 0xe3, 0xa1),
                highlight: rgb(0xcb, 0xa6, 0xf7),
            },
            "solarized" => Self {
                focus_border: rgb(0xb5, 0x89, 0x00),
                unfocus_border: rgb(0x58, 0x6e, 0x75),
                accent: rgb(0x2a, 0xa1, 0x98),
                selection_bg: rgb(0x07, 0x36, 0x42),
                selection_fg: rgb(0x93, 0xa1, 0xa1),
                status_bar_bg: rgb(0x07, 0x36, 0x42),
                error: rgb(0xdc, 0x32, 0x2f),
                success: rgb(0x85, 0x99, 0x00),
                highlight: rgb(0x6c, 0x71, 0xc4),
            },
            "cyberdream" => Self {
                focus_border: rgb(0xf1, 0xff, 0x5e),
                unfocus_border: rgb(0x7b, 0x84, 0x96),
                accent: rgb(0x5e, 0xf1, 0xff),
                selection_bg: rgb(0x3c, 0x40, 0x48),
                selection_fg: rgb(0xff, 0xff, 0xff),
                status_bar_bg: rgb(0x3c, 0x40, 0x48),
                error: rgb(0xff, 0x6e, 0x5e),
                success: rgb(0x5e, 0xff, 0x6c),
                highlight: rgb(0xbd, 0x5e, 0xff),
            },
            "carbonfox" => Self {
                focus_border: rgb(0x08, 0xbd, 0xba),
                unfocus_border: rgb(0x6f, 0x6f, 0x6f),
                accent: rgb(0x33, 0xb1, 0xff),
                selection_bg: rgb(0x28, 0x28, 0x28),
                selection_fg: rgb(0xf2, 0xf4, 0xf8),
                status_bar_bg: rgb(0x28, 0x28, 0x28),
                error: rgb(0xee, 0x53, 0x96),
                success: rgb(0x25, 0xbe, 0x6a),
                highlight: rgb(0xbe, 0x95, 0xff),
            },
            other => {
                warn!("unknown theme preset '{other}', using default");
                Self::preset("default")
            }
        }
    }
}

/// Parse a colour string: a name (`red`, `darkgray`, `lightblue`…), a 256-colour
/// index (`"208"`), or a truecolor hex (`"#rrggbb"`). Returns `None` on anything
/// unrecognised.
pub fn parse_color(s: &str) -> Option<Color> {
    let t = s.trim();
    if t.starts_with('#') {
        return parse_hex_color(t);
    }
    if let Ok(idx) = t.parse::<u8>() {
        return Some(Color::Indexed(idx));
    }
    let color = match t.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "white" => Color::White,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "reset" => Color::Reset,
        _ => return None,
    };
    Some(color)
}

/// Parse a truecolor hex code like `#ff8700` into [`Color::Rgb`]. Returns `None`
/// for anything that is not a 6-digit `#rrggbb` string.
pub fn parse_hex_color(s: &str) -> Option<Color> {
    let hex = s.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

// =============================================================================
// [hooks.claude] / [hooks.codex]
// =============================================================================

/// Default marker colour as a truecolor code (`#ff8700`, the orange of the
/// classic xterm-256 slot 208). Marker colours are specified as hex colour
/// codes in the config.
const DEFAULT_MARKER_COLOR: Color = Color::Rgb(0xff, 0x87, 0x00);

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    pub claude: MarkerSet,
    pub codex: MarkerSet,
}

/// The five markers shown for a hook-driven agent's states. `running` is shown
/// when the process is detected but no hook state is known yet.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MarkerSet {
    pub working: Marker,
    pub waiting: Marker,
    pub done: Marker,
    pub error: Marker,
    pub running: Marker,
}

impl Default for MarkerSet {
    fn default() -> Self {
        let color = DEFAULT_MARKER_COLOR;
        Self {
            working: Marker::spinner(color),
            waiting: Marker::glyph("◆", color),
            done: Marker::glyph("✓", color),
            error: Marker::glyph("✗", color),
            running: Marker::glyph("●", color),
        }
    }
}

/// A single marker: its glyph and colour. The special glyph `"spinner"` renders
/// the animated braille spinner instead of a static character.
#[derive(Debug, Clone)]
pub struct Marker {
    pub glyph: String,
    pub color: Color,
    /// True when the glyph was the literal `"spinner"` sentinel.
    pub animated: bool,
}

impl Marker {
    fn glyph(g: &str, color: Color) -> Self {
        Self {
            glyph: g.to_string(),
            color,
            animated: false,
        }
    }

    fn spinner(color: Color) -> Self {
        Self {
            glyph: "spinner".to_string(),
            color,
            animated: true,
        }
    }
}

impl<'de> Deserialize<'de> for Marker {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            glyph: String,
            #[serde(default)]
            color: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        // Marker colours are given as a hex colour code, e.g. `color = "#ff8700"`.
        let color = match raw.color.as_deref() {
            Some(c) => parse_hex_color(c).ok_or_else(|| {
                de::Error::custom(format!("invalid marker colour {c:?}, expected a hex code like \"#ff8700\""))
            })?,
            None => DEFAULT_MARKER_COLOR,
        };
        Ok(Marker {
            animated: raw.glyph == "spinner",
            glyph: raw.glyph,
            color,
        })
    }
}

// =============================================================================
// [keybindings]
// =============================================================================

/// A remappable user action. Navigation (j/k/h/l/arrows/Tab) and chords
/// (`za` fold, double-Space) are intentionally not remappable yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Refresh,
    Sort,
    Group,
    Input,
    Enter,
    NewSession,
    RenameSession,
    KillSession,
    /// Toggle the fleet dashboard (all Claude panes, sorted by attention).
    Dashboard,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct KeyBindings {
    #[serde(deserialize_with = "de_keys")]
    pub quit: Vec<KeySpec>,
    #[serde(deserialize_with = "de_keys")]
    pub refresh: Vec<KeySpec>,
    #[serde(deserialize_with = "de_keys")]
    pub sort: Vec<KeySpec>,
    #[serde(deserialize_with = "de_keys")]
    pub group: Vec<KeySpec>,
    #[serde(deserialize_with = "de_keys")]
    pub input: Vec<KeySpec>,
    #[serde(deserialize_with = "de_keys")]
    pub enter: Vec<KeySpec>,
    #[serde(deserialize_with = "de_keys")]
    pub new_session: Vec<KeySpec>,
    #[serde(deserialize_with = "de_keys")]
    pub rename_session: Vec<KeySpec>,
    #[serde(deserialize_with = "de_keys")]
    pub kill_session: Vec<KeySpec>,
    #[serde(deserialize_with = "de_keys")]
    pub dashboard: Vec<KeySpec>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        // Mirrors the historical hard-coded bindings.
        Self {
            quit: vec![key('q'), named(KeyCode::Esc)],
            refresh: vec![key('r')],
            sort: vec![key('s')],
            group: vec![key('g')],
            input: vec![key('i')],
            enter: vec![named(KeyCode::Enter)],
            new_session: vec![ctrl('n')],
            rename_session: vec![ctrl('r')],
            kill_session: vec![ctrl('x')],
            dashboard: vec![key('d')],
        }
    }
}

impl KeyBindings {
    /// Pairs of (action, bindings) in match priority order. Modifier-bearing
    /// bindings (e.g. `C-r`) are listed so they win over the plain `r` refresh.
    fn entries(&self) -> [(Action, &Vec<KeySpec>); 10] {
        [
            (Action::NewSession, &self.new_session),
            (Action::RenameSession, &self.rename_session),
            (Action::KillSession, &self.kill_session),
            (Action::Quit, &self.quit),
            (Action::Refresh, &self.refresh),
            (Action::Sort, &self.sort),
            (Action::Group, &self.group),
            (Action::Input, &self.input),
            (Action::Enter, &self.enter),
            (Action::Dashboard, &self.dashboard),
        ]
    }

    /// The action a key event maps to, if any.
    pub fn action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.entries()
            .into_iter()
            .find(|(_, specs)| specs.iter().any(|s| s.matches(key)))
            .map(|(action, _)| action)
    }

    /// Human-readable label for an action's primary binding, e.g. `C-n` or `q`,
    /// used to keep the on-screen hint bar in sync with the user's remaps.
    /// Returns an empty string if the action has no binding.
    pub fn label(&self, action: Action) -> String {
        self.entries()
            .into_iter()
            .find(|(a, _)| *a == action)
            .and_then(|(_, specs)| specs.first())
            .map(KeySpec::label)
            .unwrap_or_default()
    }
}

/// A parsed key chord: a base key plus modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeySpec {
    /// Whether this spec matches a crossterm key event. Only the C/S/A
    /// modifiers are considered (other state flags are masked out).
    pub fn matches(&self, key: &KeyEvent) -> bool {
        let relevant = KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::ALT;
        self.code == key.code && (key.modifiers & relevant) == self.mods
    }

    /// Render the chord back to a short label like `C-n`, `S-Tab`, `Space`, `q`.
    /// Roughly the inverse of [`parse_key`].
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            s.push_str("C-");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            s.push_str("A-");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            s.push_str("S-");
        }
        let base = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Esc => "Esc".to_string(),
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::BackTab => "BackTab".to_string(),
            KeyCode::Up => "Up".to_string(),
            KeyCode::Down => "Down".to_string(),
            KeyCode::Left => "Left".to_string(),
            KeyCode::Right => "Right".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            KeyCode::Backspace => "Backspace".to_string(),
            KeyCode::Delete => "Delete".to_string(),
            other => format!("{other:?}"),
        };
        s.push_str(&base);
        s
    }
}

fn key(c: char) -> KeySpec {
    KeySpec {
        code: KeyCode::Char(c),
        mods: KeyModifiers::NONE,
    }
}

fn ctrl(c: char) -> KeySpec {
    KeySpec {
        code: KeyCode::Char(c),
        mods: KeyModifiers::CONTROL,
    }
}

fn named(code: KeyCode) -> KeySpec {
    KeySpec {
        code,
        mods: KeyModifiers::NONE,
    }
}

/// Parse a key string like `q`, `Esc`, `C-n`, `S-Tab`, `Up`, `Space`.
pub fn parse_key(s: &str) -> Option<KeySpec> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split('-').collect();
    let (mod_parts, key_part) = parts.split_at(parts.len() - 1);

    let mut mods = KeyModifiers::NONE;
    for m in mod_parts {
        match m.to_ascii_uppercase().as_str() {
            "C" | "CTRL" | "CONTROL" => mods |= KeyModifiers::CONTROL,
            "S" | "SHIFT" => mods |= KeyModifiers::SHIFT,
            "A" | "M" | "ALT" | "META" => mods |= KeyModifiers::ALT,
            _ => return None,
        }
    }

    let token = key_part[0];
    let code = match token.to_ascii_lowercase().as_str() {
        "esc" | "escape" => KeyCode::Esc,
        "enter" | "return" | "cr" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "space" => KeyCode::Char(' '),
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        // A single character keeps its original case.
        _ if token.chars().count() == 1 => KeyCode::Char(token.chars().next().unwrap()),
        _ => return None,
    };
    Some(KeySpec { code, mods })
}

impl<'de> Deserialize<'de> for KeySpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_key(&s).ok_or_else(|| de::Error::custom(format!("invalid key binding: {s}")))
    }
}

/// Deserialize a single key string or a list of key strings into `Vec<KeySpec>`.
fn de_keys<'de, D>(deserializer: D) -> Result<Vec<KeySpec>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(KeySpec),
        Many(Vec<KeySpec>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(k) => vec![k],
        OneOrMany::Many(v) => v,
    })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_color_forms() {
        assert_eq!(parse_color("red"), Some(Color::Red));
        assert_eq!(parse_color("DarkGray"), Some(Color::DarkGray));
        assert_eq!(parse_color("208"), Some(Color::Indexed(208)));
        assert_eq!(parse_color("#ff8800"), Some(Color::Rgb(0xff, 0x88, 0x00)));
        assert_eq!(parse_color("notacolor"), None);
        assert_eq!(parse_color("#xyz"), None);
        // Hex-only parser used for marker colours.
        assert_eq!(parse_hex_color("#ff8700"), Some(Color::Rgb(0xff, 0x87, 0x00)));
        assert_eq!(parse_hex_color("red"), None);
        assert_eq!(parse_hex_color("208"), None);
    }

    #[test]
    fn marker_color_must_be_hex() {
        // A colour name is rejected for marker colours (hex codes only).
        let err = toml::from_str::<Config>(
            "[hooks.claude]\nworking = { glyph = \"x\", color = \"red\" }\n",
        );
        assert!(err.is_err(), "non-hex marker colour should be rejected");
    }

    #[test]
    fn parses_key_forms() {
        assert_eq!(parse_key("q"), Some(key('q')));
        assert_eq!(parse_key("C-n"), Some(ctrl('n')));
        assert_eq!(parse_key("Esc"), Some(named(KeyCode::Esc)));
        assert_eq!(parse_key("Space"), Some(named(KeyCode::Char(' '))));
        assert_eq!(
            parse_key("C-S-x"),
            Some(KeySpec {
                code: KeyCode::Char('x'),
                mods: KeyModifiers::CONTROL | KeyModifiers::SHIFT
            })
        );
        assert_eq!(
            parse_key("BackTab"),
            Some(named(KeyCode::BackTab))
        );
        assert_eq!(parse_key(""), None);
        assert_eq!(parse_key("C-"), None);
    }

    #[test]
    fn empty_config_is_default() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.preview.interval, None);
        assert_eq!(cfg.behavior.double_space_ms, 300);
        assert!(cfg.behavior.exit_on_switch);
        assert_eq!(cfg.layout.session_panel_width, 30);
        // Default markers match the historical glyphs.
        assert_eq!(cfg.hooks.claude.done.glyph, "✓");
        assert!(cfg.hooks.claude.working.animated);
    }

    #[test]
    fn partial_config_merges_with_defaults() {
        let cfg: Config = toml::from_str(
            r##"
            [preview]
            interval = 500

            [keybindings]
            quit = "x"

            [hooks.claude]
            done = { glyph = "DONE", color = "#00ff00" }
        "##,
        )
        .unwrap();
        assert_eq!(cfg.preview.interval, Some(500));
        // Overridden binding takes effect...
        assert_eq!(cfg.keybindings.quit, vec![key('x')]);
        // ...while untouched bindings keep their defaults.
        assert_eq!(cfg.keybindings.refresh, vec![key('r')]);
        // Overridden marker (hex colour), and a default sibling marker.
        assert_eq!(cfg.hooks.claude.done.glyph, "DONE");
        assert_eq!(cfg.hooks.claude.done.color, Color::Rgb(0x00, 0xff, 0x00));
        assert_eq!(cfg.hooks.claude.waiting.glyph, "◆");
    }

    #[test]
    fn keys_accept_single_or_list() {
        let cfg: Config = toml::from_str(
            r#"
            [keybindings]
            quit = ["q", "Esc", "C-c"]
        "#,
        )
        .unwrap();
        assert_eq!(cfg.keybindings.quit.len(), 3);
    }

    #[test]
    fn theme_preset_and_overrides_resolve() {
        let cfg: Config = toml::from_str(
            r##"
            [theme]
            preset = "dracula"
            [theme.colors]
            accent = "#123456"
            bogus_role = "red"
        "##,
        )
        .unwrap();
        let theme = cfg.theme.resolve();
        // Override applied.
        assert_eq!(theme.accent, Color::Rgb(0x12, 0x34, 0x56));
        // Preset value retained for a non-overridden role.
        assert_eq!(theme.success, Color::Rgb(0x50, 0xfa, 0x7b));
    }

    #[test]
    fn unknown_preset_falls_back_to_default() {
        let theme = Theme::preset("does-not-exist");
        // Falls back to the `default` preset's palette.
        assert_eq!(theme.accent, Color::Cyan);
    }

    #[test]
    fn action_lookup_respects_modifiers() {
        let kb = KeyBindings::default();
        let plain_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(kb.action_for(&plain_r), Some(Action::Refresh));
        assert_eq!(kb.action_for(&ctrl_r), Some(Action::RenameSession));
        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(kb.action_for(&j), None);
    }

    #[test]
    fn shipped_example_config_parses() {
        // The example we ship must always parse against the current schema.
        let example = include_str!("../docs/config.example.toml");
        let cfg: Config = toml::from_str(example).expect("example config must parse");
        // Spot-check representative values without pinning the exact interval
        // (the example may tweak it).
        assert!(cfg.preview.interval.is_some());
        assert!(cfg.hooks.claude.working.animated);
        // Marker colours in the example are hex codes.
        assert_eq!(cfg.hooks.claude.waiting.color, Color::Rgb(0xff, 0x87, 0x00));
    }

    #[test]
    fn behavior_maps_view_and_sort() {
        let b = BehaviorConfig {
            default_view: "multi".to_string(),
            default_sort: "abc".to_string(),
            ..BehaviorConfig::default()
        };
        assert_eq!(b.view_mode(), ViewMode::MultiPreview);
        assert_eq!(b.session_sort().key, SessionSortKey::Alphabet);
        assert_eq!(b.session_sort().direction, SortDirection::Desc);
    }
}
