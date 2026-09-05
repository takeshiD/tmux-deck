use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use ansi_to_tui::IntoText;
use ratatui::text::Text;
use ratatui::widgets::ListState;

use crate::agents::AgentSession;
use crate::config::{
    AgentMonitorConfig, AgentsConfig, BehaviorConfig, Config, HooksConfig, KeyBindings,
    LayoutConfig, Theme,
};
use crate::group::GroupStore;

/// How the agent-view preview panel renders the selected session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewMode {
    /// Reconstructed conversation from the transcript JSONL (fast, plain).
    Transcript,
    /// Reconstructed terminal screen from `claude logs` (faithful, heavier).
    Screen,
}

impl PreviewMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "screen" => Self::Screen,
            _ => Self::Transcript,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Transcript => "transcript",
            Self::Screen => "screen",
        }
    }
}

/// State of an on-demand execution summary for a background session.
#[derive(Debug, Clone)]
pub enum SummaryStatus {
    /// A `claude -p` summary is being generated.
    Pending,
    /// Summary text ready to show.
    Ready(String),
    /// Generation failed (message).
    Failed(String),
}

/// Label shown for the implicit group of sessions that have not been assigned
/// to any user group. Only rendered when at least one session *is* grouped.
pub const UNGROUPED_LABEL: &str = "Ungrouped";

/// Maximum number of characters (not bytes) accepted in the session/group name
/// input popups. Keeps names short enough to render in the narrow list panes.
pub const SESSION_NAME_MAX_LEN: usize = 30;

// =============================================================================
// Data Structures
// =============================================================================

/// State reported by agent lifecycle hooks for a given pane.
///
/// Process detection only tells us whether an agent is running; these states
/// describe what it is doing, sourced from its hook events (see
/// [`crate::hook`]). Variants are ordered loosely by how much
/// they want the user's attention — see [`HookState::priority`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookState {
    /// The agent is actively working (prompt submitted / tool running).
    Working,
    /// The agent is waiting on the user (permission prompt / idle prompt).
    Waiting,
    /// The agent finished its turn.
    Done,
    /// The agent's turn ended with an error.
    Error,
}

impl HookState {
    /// Map a Claude/Codex `hook_event_name` to the state it implies.
    /// Returns `None` for events that carry no marker meaning (the caller may
    /// treat `SessionEnd` specially, clearing any existing marker).
    pub fn from_hook_event(event: &str) -> Option<Self> {
        match event {
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PreCompact" | "PostCompact" => {
                Some(Self::Working)
            }
            "Notification" | "PermissionRequest" => Some(Self::Waiting),
            "Stop" | "SubagentStop" => Some(Self::Done),
            "Interrupt" => Some(Self::Done),
            // StopFailure is not yet confirmed in the public docs; map it
            // defensively so it lights up red if it ever fires.
            "StopFailure" => Some(Self::Error),
            _ => None,
        }
    }

    /// Stable lowercase token used in the on-disk state files.
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Done => "done",
            Self::Error => "error",
        }
    }

    /// Inverse of [`Self::as_token`].
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "working" => Some(Self::Working),
            "waiting" => Some(Self::Waiting),
            "done" => Some(Self::Done),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// How strongly this state wants the user's attention. Used when rolling
    /// pane states up into a single window / session marker — the highest
    /// priority among children wins.
    pub fn priority(self) -> u8 {
        match self {
            Self::Waiting => 3,
            Self::Error => 2,
            Self::Working => 1,
            Self::Done => 0,
        }
    }

    /// Combine two optional states, keeping the higher-priority one.
    pub fn merge(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(x), Some(y)) => Some(if x.priority() >= y.priority() { x } else { y }),
            (Some(x), None) => Some(x),
            (None, b) => b,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObservedState {
    Waiting,
    Error,
    Working,
    Done,
    Running,
}

impl ObservedState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Waiting => "WAIT",
            Self::Error => "ERROR",
            Self::Working => "WORK",
            Self::Done => "DONE",
            Self::Running => "RUN",
        }
    }

    fn attention_priority(self) -> u8 {
        match self {
            Self::Waiting => 0,
            Self::Error => 1,
            Self::Working => 2,
            Self::Done => 3,
            Self::Running => 4,
        }
    }

    pub fn actionable(self) -> bool {
        matches!(self, Self::Waiting | Self::Error)
    }
}

impl From<HookState> for ObservedState {
    fn from(value: HookState) -> Self {
        match value {
            HookState::Working => Self::Working,
            HookState::Waiting => Self::Waiting,
            HookState::Done => Self::Done,
            HookState::Error => Self::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPane {
    pub pane_id: String,
    pub target: String,
    pub tmux_identity: String,
    pub session_name: String,
    pub window_index: u32,
    pub pane_index: u32,
    pub pane_height: u32,
    pub kind: AgentKind,
    pub state: ObservedState,
    pub activity: String,
    pub state_since: Option<i64>,
    pub repository: Option<String>,
    pub worktree: Option<String>,
    pub parent: Option<String>,
}

impl AgentPane {
    pub fn group_key(&self) -> (String, String) {
        (
            self.repository.clone().unwrap_or_default(),
            self.worktree.clone().unwrap_or_default(),
        )
    }

    pub fn identity(&self, duplicate_repository: bool) -> String {
        match &self.repository {
            Some(repository) => {
                let display_repository = if duplicate_repository {
                    self.parent
                        .as_ref()
                        .map(|parent| format!("{parent}/{repository}"))
                        .unwrap_or_else(|| repository.clone())
                } else {
                    repository.clone()
                };
                match &self.worktree {
                    Some(worktree) if worktree != repository => {
                        format!("{display_repository}/{worktree}")
                    }
                    _ => display_repository,
                }
            }
            None => self.tmux_identity.clone(),
        }
    }

    pub fn elapsed_secs(&self, now: i64) -> Option<i64> {
        self.state_since.map(|since| now.saturating_sub(since).max(0))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PresentationMode {
    #[default]
    Attention,
    Overview,
}

impl PresentationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Attention => "attention",
            Self::Overview => "overview",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewDensity {
    LiveGrid,
    Hybrid,
    SummaryList,
}

pub fn overview_density(width: u16, height: u16, count: usize) -> OverviewDensity {
    if count == 0 {
        return OverviewDensity::SummaryList;
    }
    let columns = (count as f64).sqrt().ceil() as u16;
    let rows = u16::try_from(count.div_ceil(usize::from(columns))).unwrap_or(u16::MAX);
    let content_height = height.saturating_sub(2);
    if width / columns.max(1) >= 44 && content_height / rows.max(1) >= 10 {
        return OverviewDensity::LiveGrid;
    }
    let hybrid_capacity = usize::from(content_height / 3);
    if width >= 60 && content_height >= 12 && count <= hybrid_capacity {
        OverviewDensity::Hybrid
    } else {
        OverviewDensity::SummaryList
    }
}

/// Represents a tmux pane
#[derive(Debug, Clone)]
pub struct TmuxPane {
    pub id: String,
    pub index: u32,
    #[allow(dead_code)]
    pub width: u32,
    #[allow(dead_code)]
    pub height: u32,
    #[allow(dead_code)]
    pub active: bool,
    pub current_command: String,
    /// Current pane directory reported by tmux; used as a hookless metadata fallback.
    pub current_path: String,
    pub pid: u32,
    /// True if a claude process is running in this pane (detected via descendant process scan).
    pub has_claude: bool,
    /// Latest state reported by Claude Code hooks for this pane, if any.
    pub claude_state: Option<HookState>,
    // The hook (see [`crate::hook`]) still collects this per-pane context;
    // it is reserved for an inline tree-view indicator and not yet displayed,
    // so these carry `#[allow(dead_code)]`.
    /// One-line summary of what Claude is currently doing (e.g. `Edit: src/app.rs`).
    #[allow(dead_code)]
    pub claude_activity: Option<String>,
    /// Unix timestamp (secs) when the current Claude state began.
    #[allow(dead_code)]
    pub claude_state_since: Option<i64>,
    /// Working directory Claude reported for this pane (repo identification).
    #[allow(dead_code)]
    pub claude_cwd: Option<String>,
    /// True if a Codex process is running in this pane.
    pub has_codex: bool,
    /// Latest state reported by Codex hooks for this pane, if any.
    pub codex_state: Option<HookState>,
    /// One-line summary of what Codex is currently doing.
    #[allow(dead_code)]
    pub codex_activity: Option<String>,
    /// Unix timestamp (secs) when the current Codex state began.
    #[allow(dead_code)]
    pub codex_state_since: Option<i64>,
    /// Working directory Codex reported for this pane.
    #[allow(dead_code)]
    pub codex_cwd: Option<String>,
    /// Git identity resolved asynchronously by TmuxActor and cached by pane id.
    pub agent_repository: Option<String>,
    pub agent_worktree: Option<String>,
    pub agent_repository_parent: Option<String>,
}

impl TmuxPane {
    /// Seconds elapsed since the current Claude state began, if known.
    #[allow(dead_code)]
    pub fn claude_state_elapsed_secs(&self) -> Option<i64> {
        self.claude_state_since
            .map(|since| crate::hook::now_secs().saturating_sub(since).max(0))
    }

    #[allow(dead_code)]
    pub fn codex_state_elapsed_secs(&self) -> Option<i64> {
        self.codex_state_since
            .map(|since| crate::hook::now_secs().saturating_sub(since).max(0))
    }
}

fn repository_identity(cwd: Option<&str>) -> (Option<String>, Option<String>, Option<String>) {
    let Some(cwd) = cwd.filter(|value| !value.trim().is_empty()) else {
        return (None, None, None);
    };
    let path = Path::new(cwd);
    let components: Vec<String> = path
        .components()
        .filter_map(|part| part.as_os_str().to_str().map(ToOwned::to_owned))
        .collect();
    if let Some(marker) = components.iter().position(|part| part == ".worktrees")
        && marker > 0
        && marker + 1 < components.len()
    {
        let repository = components[marker - 1].clone();
        let parent = marker
            .checked_sub(2)
            .and_then(|index| components.get(index).cloned());
        return (
            Some(repository),
            Some(components[marker + 1].clone()),
            parent,
        );
    }
    (None, None, None)
}

fn project_agent_panes(sessions: &[TmuxSession], now: i64, retention_secs: u64) -> Vec<AgentPane> {
    let mut agents = Vec::new();
    for session in sessions {
        for window in &session.windows {
            for pane in &window.panes {
                let mut candidates = Vec::new();
                if pane.has_claude {
                    candidates.push((
                        AgentKind::Claude,
                        pane.claude_state,
                        pane.claude_activity.as_deref(),
                        pane.claude_state_since,
                        pane.claude_cwd.as_deref(),
                    ));
                }
                if pane.has_codex {
                    candidates.push((
                        AgentKind::Codex,
                        pane.codex_state,
                        pane.codex_activity.as_deref(),
                        pane.codex_state_since,
                        pane.codex_cwd.as_deref(),
                    ));
                }
                let Some((kind, hook_state, activity, state_since, cwd)) = candidates
                    .into_iter()
                    .max_by_key(|(kind, state, _, since, _)| {
                        (
                            state.map(HookState::priority).unwrap_or(0),
                            since.unwrap_or(0),
                            matches!(kind, AgentKind::Codex),
                        )
                    })
                else {
                    continue;
                };
                let state = hook_state.map(Into::into).unwrap_or(ObservedState::Running);
                if state == ObservedState::Done
                    && state_since.is_some_and(|since| {
                        now.saturating_sub(since) > i64::try_from(retention_secs).unwrap_or(i64::MAX)
                    })
                {
                    continue;
                }
                let (fallback_repository, fallback_worktree, fallback_parent) =
                    repository_identity(cwd.or(Some(pane.current_path.as_str())));
                agents.push(AgentPane {
                    pane_id: pane.id.clone(),
                    target: pane.id.clone(),
                    tmux_identity: format!("{}:{}.{}", session.name, window.index, pane.id),
                    session_name: session.name.clone(),
                    window_index: window.index,
                    pane_index: pane.index,
                    pane_height: pane.height,
                    kind,
                    state,
                    activity: if hook_state.is_some() {
                        activity.unwrap_or("activity unavailable").to_string()
                    } else {
                        "state unavailable".to_string()
                    },
                    state_since,
                    repository: pane.agent_repository.clone().or(fallback_repository),
                    worktree: pane.agent_worktree.clone().or(fallback_worktree),
                    parent: pane.agent_repository_parent.clone().or(fallback_parent),
                });
            }
        }
    }
    agents.sort_by(|a, b| {
        a.group_key()
            .cmp(&b.group_key())
            .then_with(|| a.session_name.cmp(&b.session_name))
            .then_with(|| a.window_index.cmp(&b.window_index))
            .then_with(|| a.pane_index.cmp(&b.pane_index))
            .then_with(|| a.pane_id.cmp(&b.pane_id))
    });
    agents
}

/// Represents a tmux window with captured content
#[derive(Debug, Clone)]
pub struct TmuxWindow {
    pub index: u32,
    pub name: String,
    pub panes: Vec<TmuxPane>,
    /// True if any pane in this window has claude running.
    pub has_claude: bool,
    /// Highest-priority Claude hook state across this window's panes.
    pub claude_state: Option<HookState>,
    /// True if any pane in this window has Codex running.
    pub has_codex: bool,
    /// Highest-priority Codex hook state across this window's panes.
    pub codex_state: Option<HookState>,
}

impl TmuxWindow {
    #[allow(dead_code)]
    pub fn get_active_pane(&self) -> Option<&TmuxPane> {
        self.panes.iter().find(|p| p.active).or(self.panes.first())
    }
}

/// Represents a tmux session
#[derive(Debug, Clone)]
pub struct TmuxSession {
    pub name: String,
    pub windows: Vec<TmuxWindow>,
    /// True if any window in this session has claude running.
    pub has_claude: bool,
    /// Highest-priority Claude hook state across this session's windows.
    pub claude_state: Option<HookState>,
    /// True if any window in this session has Codex running.
    pub has_codex: bool,
    /// Highest-priority Codex hook state across this session's windows.
    pub codex_state: Option<HookState>,
    /// Epoch seconds — kept on the struct so [`SessionSort`] can reorder
    /// the list without re-querying tmux.
    pub last_attached: i64,
    pub activity: i64,
    /// tmux-deck-side group label this session belongs to, if any. This is a
    /// purely organisational tag managed by the deck (see [`crate::group`]),
    /// independent of tmux's native session groups. `None` means ungrouped.
    pub group: Option<String>,
}

// =============================================================================
// Enums
// =============================================================================

/// Main view mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewMode {
    TreeView,
    AgentMonitor,
    /// Full-screen fleet view of Claude Code background sessions (the
    /// `claude agents` agent view), grouped by working directory.
    Dashboard,
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

/// What attribute to sort sessions by.
///
/// To add a new sort attribute:
///   1. Add a variant here.
///   2. Add a comparator branch in [`SessionSortKey::cmp_ascending`].
///   3. Add a short label in [`SessionSortKey::label`].
///   4. Add `SessionSort` entries (one per direction) to [`SessionSort::ALL`]
///      at the position you want users to land on when cycling with `s`.
///
/// Direction handling, UI display and key wiring are all generic over key —
/// no further code needs to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSortKey {
    /// Most recently attached time (`last_attached`, tie-broken by `activity`).
    LastAttached,
    /// Case-insensitive session name.
    Alphabet,
}

impl SessionSortKey {
    /// Short label fragment shown in the Sessions list title.
    pub fn label(self) -> &'static str {
        match self {
            SessionSortKey::LastAttached => "recent",
            SessionSortKey::Alphabet => "abc",
        }
    }

    /// Compare two sessions by this key, with smaller raw values first.
    /// [`SessionSort`] flips this for the [`SortDirection::Desc`] case so the
    /// key implementer only ever has to think about the natural ordering.
    fn cmp_ascending(self, a: &TmuxSession, b: &TmuxSession) -> std::cmp::Ordering {
        match self {
            SessionSortKey::LastAttached => a
                .last_attached
                .cmp(&b.last_attached)
                .then_with(|| a.activity.cmp(&b.activity)),
            SessionSortKey::Alphabet => a
                .name
                .to_lowercase()
                .cmp(&b.name.to_lowercase()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    /// Largest value first — top of the list has the highest raw key.
    /// e.g. `LastAttached + Desc` = newest first; `Alphabet + Desc` = Z first.
    Desc,
    /// Smallest value first — top of the list has the lowest raw key.
    /// e.g. `LastAttached + Asc` = oldest first; `Alphabet + Asc` = A first.
    Asc,
}

impl SortDirection {
    pub fn arrow(self) -> char {
        match self {
            SortDirection::Desc => '↓',
            SortDirection::Asc => '↑',
        }
    }
}

/// A complete sort spec: which attribute, in which direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSort {
    pub key: SessionSortKey,
    pub direction: SortDirection,
}

impl SessionSort {
    /// All sort modes in the order the `s` key cycles through. Default is the
    /// first entry. To add a new key, expand this list with one entry per
    /// direction (typically `Desc` then `Asc`).
    pub const ALL: &'static [SessionSort] = &[
        SessionSort {
            key: SessionSortKey::LastAttached,
            direction: SortDirection::Desc,
        },
        SessionSort {
            key: SessionSortKey::LastAttached,
            direction: SortDirection::Asc,
        },
        SessionSort {
            key: SessionSortKey::Alphabet,
            direction: SortDirection::Desc,
        },
        SessionSort {
            key: SessionSortKey::Alphabet,
            direction: SortDirection::Asc,
        },
    ];

    /// Label shown in the Sessions list title, e.g. "recent↓" / "abc↑".
    pub fn label(self) -> String {
        format!("{}{}", self.key.label(), self.direction.arrow())
    }

    /// Next mode in [`Self::ALL`], wrapping around.
    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    /// Sort `sessions` in-place.
    pub fn apply(self, sessions: &mut [TmuxSession]) {
        sessions.sort_by(|a, b| {
            let ord = self.key.cmp_ascending(a, b);
            let ord = match self.direction {
                SortDirection::Desc => ord.reverse(),
                SortDirection::Asc => ord,
            };
            // Stable, deterministic tie-break — always by name ascending so
            // the list does not jiggle on refresh when the primary key ties.
            ord.then_with(|| a.name.cmp(&b.name))
        });
    }
}

impl Default for SessionSort {
    fn default() -> Self {
        Self::ALL[0]
    }
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
    /// Choosing a group for the selected session from a list of existing
    /// groups (plus "ungroup" and "create new" entries).
    GroupSession,
    /// Typing the name of a brand-new group, reached from the GroupSession
    /// list via the "New group" entry.
    NewGroup,
}

/// The entry highlighted in the [`PopupMode::GroupSession`] selection list.
/// The list shows every existing group, then an "Ungrouped" entry that clears
/// the assignment, then a "New group" entry that switches to text entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupChoice {
    /// Assign the session to an existing group of this name.
    Existing(String),
    /// Remove the session from any group.
    Ungrouped,
    /// Create a new group (switches the popup to text entry).
    New,
}

/// A single rendered row in the Sessions list. Grouping inserts non-selectable
/// [`SessionRow::Header`] rows between the [`SessionRow::Session`] rows; the
/// session rows still map 1:1 onto indices into [`UIState::sessions`], so all
/// navigation continues to operate on session indices and only rendering needs
/// to be group-aware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRow {
    /// A group heading. `group` is `None` for the implicit ungrouped bucket.
    /// `collapsed` drives the fold indicator and means the member session rows
    /// are hidden.
    Header {
        group: Option<String>,
        count: usize,
        collapsed: bool,
    },
    /// A session, identified by its index into [`UIState::sessions`].
    Session { index: usize },
}

// =============================================================================
// UI State (formerly App)
// =============================================================================

pub struct UIState {
    // View mode
    pub view_mode: ViewMode,

    // TreeView state
    pub sessions: Vec<TmuxSession>,
    pub selected_session: usize,
    pub selected_window: usize,
    pub selected_pane: usize,
    pub focus: Focus,
    pub session_list_state: ListState,
    pub window_list_state: ListState,
    pub pane_list_state: ListState,
    pub session_sort: SessionSort,

    /// Persisted tmux-deck-side session grouping (session name -> group).
    pub groups: GroupStore,
    /// Groups currently folded in the Sessions list. A group key of `None` is
    /// the implicit "Ungrouped" bucket. Fold state is session-runtime only and
    /// is not persisted.
    pub collapsed_groups: HashSet<Option<String>>,
    /// True after `z` is pressed, awaiting the `a` of the `za` fold chord.
    pub pending_z: bool,

    // Agent Monitor state. Pane ids are stable across refreshes and are the
    // selection/order key; the projected records contain no borrowed tmux data.
    pub agent_panes: Vec<AgentPane>,
    pub agent_order: Vec<String>,
    pub agent_pane_selected: Option<String>,
    pub agent_monitor_mode: PresentationMode,
    pub agent_monitor_focused: bool,
    pub agent_monitor_filter_editing: bool,
    pub agent_monitor_filter: String,
    pub agent_monitor_scroll: usize,
    pub agent_monitor_message: Option<(String, i64)>,
    pub agent_pane_contents: HashMap<String, Text<'static>>,
    pub agent_monitor_config: AgentMonitorConfig,

    /// Claude jobs and Codex threads shown in Background Agents. Order matches
    /// the rendered grouped-by-directory order, so `agent_selected` indexes it.
    pub agent_sessions: Vec<AgentSession>,
    pub agent_sessions_loading: bool,
    /// Selected row in the agent view (`ViewMode::Dashboard`).
    pub agent_selected: usize,
    /// Selected provider-native session to resume; consumed by the UI loop.
    pub pending_attach: Option<AgentSession>,
    /// Whether the agent-view preview panel is shown (`p`).
    pub agent_preview: bool,
    /// How the preview renders (transcript vs screen); toggled with `v`.
    pub agent_preview_mode: PreviewMode,
    /// Whether the execution-summary popup is open (`s`); independent of preview.
    pub agent_summary_open: bool,
    /// On-demand execution summaries, keyed by session short id.
    pub agent_summaries: HashMap<String, SummaryStatus>,
    /// Cached `claude logs` output (raw PTY bytes) per session id, for the
    /// screen preview mode.
    pub agent_logs: HashMap<String, Vec<u8>>,
    /// Background Agents config (currently Claude preview/summary settings).
    pub agents_config: AgentsConfig,

    // Shared state
    pub pane_content: String,
    pub pane_content_parsed: Option<Text<'static>>,
    /// Number of lines the TreeView preview is scrolled back from the live tail.
    tree_preview_scroll: usize,
    /// Last rendered height of the TreeView preview's inner viewport.
    tree_preview_height: usize,
    pub last_error: Option<String>,
    #[allow(dead_code)]
    pub interval: Duration,

    // Resolved user configuration.
    /// Semantic UI colour palette.
    pub theme: Theme,
    /// Per-state hook markers (claude / codex).
    pub hooks: HooksConfig,
    /// Remappable key bindings.
    pub keybindings: KeyBindings,
    /// Panel layout ratios.
    pub layout: LayoutConfig,
    /// Behavioural toggles (startup view, exit-on-switch, …).
    pub behavior: BehaviorConfig,

    pub input_mode: InputMode,
    pub input_buffer: String,
    pub input_cursor: usize,

    // Popup state
    pub popup_mode: Option<PopupMode>,
    pub confirm_yes_selected: bool,
    /// Existing group names offered in the GroupSession selection list,
    /// snapshotted when the popup opens so navigation stays stable.
    pub group_choices: Vec<String>,
    /// Index of the highlighted entry in the GroupSession list. Entries are
    /// `group_choices` followed by the "Ungrouped" and "New group" entries.
    pub group_choice_index: usize,
}

impl UIState {
    pub fn new(config: Config) -> Self {
        let interval_ms = config.preview.interval.unwrap_or(300);
        let theme = config.theme.resolve();
        let view_mode = config.behavior.view_mode();
        let session_sort = config.behavior.session_sort();
        let agent_monitor_mode = crate::ui_state::load_agent_monitor_mode();
        let mut state = Self {
            view_mode,

            sessions: Vec::new(),
            selected_session: 0,
            selected_window: 0,
            selected_pane: 0,
            focus: Focus::Sessions,
            session_list_state: ListState::default(),
            window_list_state: ListState::default(),
            pane_list_state: ListState::default(),
            session_sort,

            groups: GroupStore::load(),
            collapsed_groups: HashSet::new(),
            pending_z: false,

            agent_panes: Vec::new(),
            agent_order: Vec::new(),
            agent_pane_selected: None,
            agent_monitor_mode,
            agent_monitor_focused: false,
            agent_monitor_filter_editing: false,
            agent_monitor_filter: String::new(),
            agent_monitor_scroll: 0,
            agent_monitor_message: None,
            agent_pane_contents: HashMap::new(),
            agent_monitor_config: config.agent_monitor,

            agent_sessions: Vec::new(),
            agent_sessions_loading: false,
            agent_selected: 0,
            pending_attach: None,
            agent_preview: false,
            agent_preview_mode: PreviewMode::from_str(&config.agents.preview_mode),
            agent_summary_open: false,
            agent_summaries: HashMap::new(),
            agent_logs: HashMap::new(),
            agents_config: config.agents,

            pane_content: String::new(),
            pane_content_parsed: None,
            tree_preview_scroll: 0,
            tree_preview_height: 0,
            last_error: None,
            interval: Duration::from_millis(interval_ms),

            theme,
            hooks: config.hooks,
            keybindings: config.keybindings,
            layout: config.layout,
            behavior: config.behavior,

            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            input_cursor: 0,

            popup_mode: None,
            group_choices: Vec::new(),
            group_choice_index: 0,
            confirm_yes_selected: false,
        };
        state.session_list_state.select(Some(0));
        state.window_list_state.select(Some(0));
        state.pane_list_state.select(Some(0));
        state
    }

    // =========================================================================
    // View Mode Switching
    // =========================================================================

    /// Re-read Claude and Codex hook state files and patch the session tree.
    ///
    /// Cheap enough to call on every refresh tick: it only reads a small local
    /// state directories. This keeps markers live without a full tmux refresh.
    pub fn refresh_agent_states(&mut self) {
        crate::hook::apply_states(&mut self.sessions);
        let now = crate::hook::now_secs();
        self.rebuild_agent_panes(now);
        if self
            .agent_monitor_message
            .as_ref()
            .is_some_and(|(_, shown_at)| now.saturating_sub(*shown_at) >= 3)
        {
            self.agent_monitor_message = None;
        }
    }

    /// True if any session currently has a `Working` agent marker, used to
    /// decide whether the spinner animation needs frequent redraws.
    pub fn has_visible_working_agent(&self, width: u16, height: u16) -> bool {
        if self.view_mode != ViewMode::AgentMonitor {
            return self.sessions.iter().any(|session| {
                session.claude_state == Some(HookState::Working)
                    || session.codex_state == Some(HookState::Working)
            });
        }
        if self.agent_monitor_focused {
            return self
                .selected_agent_pane()
                .is_some_and(|agent| agent.state == ObservedState::Working);
        }
        let visible = self.visible_agent_panes();
        if self.agent_monitor_mode == PresentationMode::Overview
            && overview_density(width, height, visible.len()) == OverviewDensity::SummaryList
        {
            return visible
                .iter()
                .skip(self.agent_monitor_scroll)
                .take(usize::from(height.saturating_sub(2)))
                .any(|agent| agent.state == ObservedState::Working);
        }
        visible
            .iter()
            .any(|agent| agent.state == ObservedState::Working)
    }

    pub fn toggle_agent_monitor(&mut self) {
        let returning_to_sessions = self.view_mode == ViewMode::AgentMonitor;
        self.view_mode = match self.view_mode {
            ViewMode::TreeView | ViewMode::Dashboard => ViewMode::AgentMonitor,
            ViewMode::AgentMonitor => ViewMode::TreeView,
        };
        if returning_to_sessions {
            self.reset_tree_preview_scroll();
        }
        self.agent_monitor_focused = false;
        self.agent_monitor_filter_editing = false;
    }

    pub fn cycle_agent_monitor_mode(&mut self) {
        self.agent_monitor_mode = match self.agent_monitor_mode {
            PresentationMode::Attention => PresentationMode::Overview,
            PresentationMode::Overview => PresentationMode::Attention,
        };
        self.agent_monitor_scroll = 0;
        self.agent_monitor_focused = false;
        self.ensure_agent_selection_visible();
    }

    fn rebuild_agent_panes(&mut self, now: i64) {
        let projected = project_agent_panes(
            &self.sessions,
            now,
            self.agent_monitor_config.completed_retention_secs,
        );
        let previous_order = self.agent_order.clone();
        let previous_selected = self.agent_pane_selected.clone();
        let previous_group = previous_selected.as_ref().and_then(|id| {
            self.agent_panes
                .iter()
                .find(|agent| &agent.pane_id == id)
                .map(AgentPane::group_key)
        });
        let ids: HashSet<String> = projected
            .iter()
            .map(|agent| agent.pane_id.clone())
            .collect();
        self.agent_order.retain(|id| ids.contains(id));

        for agent in &projected {
            if self.agent_order.contains(&agent.pane_id) {
                continue;
            }
            let group = agent.group_key();
            let insert_at = self
                .agent_order
                .iter()
                .enumerate()
                .filter_map(|(index, id)| {
                    projected
                        .iter()
                        .find(|existing| &existing.pane_id == id)
                        .filter(|existing| existing.group_key() == group)
                        .map(|_| index + 1)
                })
                .next_back()
                .unwrap_or(self.agent_order.len());
            self.agent_order.insert(insert_at, agent.pane_id.clone());
        }

        let by_id: HashMap<String, AgentPane> = projected
            .into_iter()
            .map(|agent| (agent.pane_id.clone(), agent))
            .collect();
        self.agent_panes = self
            .agent_order
            .iter()
            .filter_map(|id| by_id.get(id).cloned())
            .collect();
        let active_targets: HashSet<&str> = self
            .agent_panes
            .iter()
            .map(|agent| agent.target.as_str())
            .collect();
        self.agent_pane_contents
            .retain(|target, _| active_targets.contains(target.as_str()));

        if previous_selected
            .as_ref()
            .is_some_and(|id| !ids.contains(id))
        {
            let old_index = previous_selected
                .as_ref()
                .and_then(|id| previous_order.iter().position(|old| old == id))
                .unwrap_or(0);
            let adjacent_ids = || {
                previous_order[old_index.saturating_add(1).min(previous_order.len())..]
                    .iter()
                    .chain(previous_order[..old_index.min(previous_order.len())].iter().rev())
            };
            let replacement = previous_group
                .and_then(|group| {
                    adjacent_ids().find_map(|id| {
                        self.agent_panes
                            .iter()
                            .find(|agent| &agent.pane_id == id && agent.group_key() == group)
                    })
                })
                .or_else(|| {
                    adjacent_ids()
                        .find_map(|id| self.agent_panes.iter().find(|agent| &agent.pane_id == id))
                })
                .or_else(|| self.agent_panes.first());
            self.agent_pane_selected = replacement.map(|agent| agent.pane_id.clone());
            if self.agent_monitor_focused {
                self.agent_monitor_focused = false;
                self.agent_monitor_message = Some((
                    "Focused agent disappeared".to_string(),
                    crate::hook::now_secs(),
                ));
            }
        } else if self.agent_pane_selected.is_none() {
            self.agent_pane_selected = self.agent_panes.first().map(|agent| agent.pane_id.clone());
        }
        self.ensure_agent_selection_visible();
    }

    pub fn visible_agent_panes(&self) -> Vec<&AgentPane> {
        let mut agents: Vec<&AgentPane> = self
            .agent_panes
            .iter()
            .filter(|agent| self.agent_matches_filter(agent))
            .filter(|agent| {
                self.agent_monitor_mode == PresentationMode::Overview
                    || agent.state != ObservedState::Running
            })
            .collect();
        if self.agent_monitor_mode == PresentationMode::Attention {
            let stable_position: HashMap<&str, usize> = self
                .agent_order
                .iter()
                .enumerate()
                .map(|(index, id)| (id.as_str(), index))
                .collect();
            agents.sort_by_key(|agent| {
                (
                    agent.state.attention_priority(),
                    agent.state_since.unwrap_or(i64::MAX),
                    stable_position
                        .get(agent.pane_id.as_str())
                        .copied()
                        .unwrap_or(usize::MAX),
                )
            });
        }
        agents
    }

    fn agent_matches_filter(&self, agent: &AgentPane) -> bool {
        let query = self.agent_monitor_filter.trim().to_ascii_lowercase();
        if query.is_empty() {
            return true;
        }
        let identity = self.agent_identity(agent).to_ascii_lowercase();
        query.split_whitespace().all(|token| {
            if let Some(value) = token.strip_prefix("state:") {
                let aliases: &[&str] = match agent.state {
                    ObservedState::Waiting => &["wait", "waiting", "blocked"],
                    ObservedState::Error => &["error", "failed"],
                    ObservedState::Working => &["work", "working"],
                    ObservedState::Done => &["done", "completed"],
                    ObservedState::Running => &["run", "running"],
                };
                return aliases.iter().any(|alias| alias.contains(value));
            }
            if let Some(value) = token.strip_prefix("agent:") {
                return agent.kind.label().to_ascii_lowercase().contains(value);
            }
            if let Some(value) = token.strip_prefix("repo:") {
                return identity.contains(value);
            }
            identity.contains(token)
                || agent.activity.to_ascii_lowercase().contains(token)
                || agent.target.to_ascii_lowercase().contains(token)
                || agent.kind.label().to_ascii_lowercase().contains(token)
                || agent.state.label().to_ascii_lowercase().contains(token)
        })
    }

    pub fn agent_identity(&self, agent: &AgentPane) -> String {
        let duplicate = agent.repository.as_ref().is_some_and(|repository| {
            self.agent_panes.iter().any(|other| {
                other.pane_id != agent.pane_id
                    && other.repository.as_ref() == Some(repository)
                    && other.parent != agent.parent
            })
        });
        agent.identity(duplicate)
    }

    pub fn selected_agent_pane(&self) -> Option<&AgentPane> {
        let selected = self.agent_pane_selected.as_ref()?;
        self.visible_agent_panes()
            .into_iter()
            .find(|agent| &agent.pane_id == selected)
    }

    fn ensure_agent_selection_visible(&mut self) {
        let visible_ids: Vec<String> = self
            .visible_agent_panes()
            .iter()
            .map(|agent| agent.pane_id.clone())
            .collect();
        if !self
            .agent_pane_selected
            .as_ref()
            .is_some_and(|selected| visible_ids.contains(selected))
        {
            self.agent_pane_selected = visible_ids.first().cloned();
        }
        self.agent_monitor_scroll = self
            .agent_monitor_scroll
            .min(visible_ids.len().saturating_sub(1));
    }

    pub fn agent_move_by(&mut self, amount: isize) {
        let ids: Vec<String> = self
            .visible_agent_panes()
            .iter()
            .map(|agent| agent.pane_id.clone())
            .collect();
        let current = self
            .agent_pane_selected
            .as_ref()
            .and_then(|selected| ids.iter().position(|id| id == selected))
            .unwrap_or(0);
        let next = current
            .saturating_add_signed(amount)
            .min(ids.len().saturating_sub(1));
        self.agent_pane_selected = ids.get(next).cloned();
    }

    pub fn agent_move_visual(
        &mut self,
        horizontal: isize,
        vertical: isize,
        width: u16,
        height: u16,
    ) {
        let count = self.visible_agent_panes().len();
        let columns = if self.agent_monitor_mode == PresentationMode::Overview
            && overview_density(width, height, count) == OverviewDensity::LiveGrid
        {
            (count as f64).sqrt().ceil().max(1.0) as isize
        } else {
            1
        };
        if columns > 1 && horizontal != 0 {
            let ids: Vec<String> = self
                .visible_agent_panes()
                .iter()
                .map(|agent| agent.pane_id.clone())
                .collect();
            let current = self
                .agent_pane_selected
                .as_ref()
                .and_then(|selected| ids.iter().position(|id| id == selected))
                .unwrap_or(0);
            let column = isize::try_from(current).unwrap_or(isize::MAX) % columns;
            let can_move = (horizontal < 0 && column > 0)
                || (horizontal > 0
                    && column + 1 < columns
                    && current + 1 < ids.len());
            if can_move {
                self.agent_pane_selected = ids
                    .get(current.saturating_add_signed(horizontal))
                    .cloned();
            }
            return;
        }
        let amount = if horizontal != 0 {
            horizontal
        } else {
            vertical.saturating_mul(
                columns.min(isize::try_from(width.max(1)).unwrap_or(isize::MAX)),
            )
        };
        self.agent_move_by(amount);
    }

    pub fn agent_move_home(&mut self) {
        self.agent_pane_selected = self
            .visible_agent_panes()
            .first()
            .map(|agent| agent.pane_id.clone());
    }

    pub fn agent_move_end(&mut self) {
        self.agent_pane_selected = self
            .visible_agent_panes()
            .last()
            .map(|agent| agent.pane_id.clone());
    }

    pub fn agent_move_page(&mut self, viewport: usize, down: bool) {
        let amount = isize::try_from(viewport.max(1)).unwrap_or(isize::MAX);
        self.agent_move_by(if down { amount } else { -amount });
    }

    pub fn toggle_agent_focus(&mut self) {
        if self.selected_agent_pane().is_some() {
            self.agent_monitor_focused = !self.agent_monitor_focused;
        }
    }

    pub fn begin_agent_filter(&mut self) {
        self.agent_monitor_filter_editing = true;
    }

    pub fn clear_agent_filter(&mut self) {
        self.agent_monitor_filter_editing = false;
        self.agent_monitor_filter.clear();
        self.ensure_agent_selection_visible();
    }

    pub fn agent_filter_char(&mut self, character: char) {
        self.agent_monitor_filter.push(character);
        self.ensure_agent_selection_visible();
    }

    pub fn agent_filter_backspace(&mut self) {
        self.agent_monitor_filter.pop();
        self.ensure_agent_selection_visible();
    }

    pub fn agent_counts(&self) -> (usize, usize, usize) {
        let actionable = self
            .agent_panes
            .iter()
            .filter(|agent| agent.state.actionable())
            .count();
        let working = self
            .agent_panes
            .iter()
            .filter(|agent| agent.state == ObservedState::Working)
            .count();
        let done = self
            .agent_panes
            .iter()
            .filter(|agent| agent.state == ObservedState::Done)
            .count();
        (actionable, working, done)
    }

    pub fn agent_capture_targets(&self, width: u16, height: u16) -> Vec<(String, i32, i32)> {
        let selected = || {
            self.selected_agent_pane().map(|agent| {
                (
                    agent.target.clone(),
                    0,
                    i32::try_from(agent.pane_height).unwrap_or(i32::MAX),
                )
            })
        };
        if self.agent_monitor_focused {
            return selected().into_iter().collect();
        }
        match self.agent_monitor_mode {
            PresentationMode::Attention => {
                if width >= 60 {
                    selected().into_iter().collect()
                } else {
                    Vec::new()
                }
            }
            PresentationMode::Overview => match overview_density(
                width,
                height,
                self.visible_agent_panes().len(),
            ) {
                OverviewDensity::LiveGrid => self
                    .visible_agent_panes()
                    .into_iter()
                    .map(|agent| {
                        (
                            agent.target.clone(),
                            0,
                            i32::try_from(agent.pane_height).unwrap_or(i32::MAX),
                        )
                    })
                    .collect(),
                OverviewDensity::Hybrid => selected().into_iter().collect(),
                OverviewDensity::SummaryList => Vec::new(),
            },
        }
    }

    pub fn update_agent_pane_content(&mut self, target: &str, content: String) {
        if let Ok(parsed) = content.as_bytes().into_text() {
            self.agent_pane_contents.insert(target.to_string(), parsed);
        }
    }

    // =========================================================================
    // Agent View (Claude Code background sessions)
    // =========================================================================

    /// Toggle Background Agents on/off. Loading is dispatched asynchronously
    /// by UIActor so filesystem and app-server work never blocks input.
    pub fn toggle_dashboard(&mut self) {
        if self.view_mode == ViewMode::Dashboard {
            self.view_mode = ViewMode::TreeView;
        } else {
            self.agent_selected = 0;
            self.view_mode = ViewMode::Dashboard;
        }
    }

    pub fn update_agent_sessions(&mut self, sessions: Vec<AgentSession>) {
        let selected_key = self.selected_agent().map(AgentSession::cache_key);
        self.agent_sessions = sessions;
        if self.agent_sessions.is_empty() {
            self.agent_selected = 0;
        } else if let Some(key) = selected_key
            && let Some(index) = self
                .agent_sessions
                .iter()
                .position(|session| session.cache_key() == key)
        {
            self.agent_selected = index;
        } else {
            self.agent_selected = self.agent_selected.min(self.agent_sessions.len() - 1);
        }
    }

    pub fn agent_select_prev(&mut self) {
        self.agent_selected = self.agent_selected.saturating_sub(1);
        self.agent_summary_open = false;
    }

    pub fn agent_select_next(&mut self) {
        if !self.agent_sessions.is_empty() {
            self.agent_selected = (self.agent_selected + 1).min(self.agent_sessions.len() - 1);
        }
        self.agent_summary_open = false;
    }

    pub fn selected_agent_attach(&self) -> Option<AgentSession> {
        self.selected_agent().cloned()
    }

    /// The selected background session, if any.
    pub fn selected_agent(&self) -> Option<&AgentSession> {
        self.agent_sessions.get(self.agent_selected)
    }

    /// Toggle the preview panel for the selected session.
    pub fn toggle_agent_preview(&mut self) {
        self.agent_preview = !self.agent_preview;
    }

    /// Switch the preview between transcript and screen rendering (`v`).
    pub fn cycle_preview_mode(&mut self) {
        self.agent_preview_mode = match self.agent_preview_mode {
            PreviewMode::Transcript => PreviewMode::Screen,
            PreviewMode::Screen => PreviewMode::Transcript,
        };
    }

    /// Store freshly fetched `claude logs` output for the screen preview.
    pub fn update_agent_logs(&mut self, id: String, bytes: Vec<u8>) {
        self.agent_logs.insert(id, bytes);
    }

    /// Cached `claude logs` bytes for a session, if fetched.
    pub fn agent_logs_for(&self, id: &str) -> Option<&Vec<u8>> {
        self.agent_logs.get(id)
    }

    /// Open the execution-summary popup (independent of the preview panel).
    pub fn open_agent_summary(&mut self) {
        self.agent_summary_open = true;
    }

    /// Close the execution-summary popup.
    pub fn close_agent_summary(&mut self) {
        self.agent_summary_open = false;
    }

    /// Mark a session's summary as generating.
    pub fn set_summary_pending(&mut self, id: String) {
        self.agent_summaries.insert(id, SummaryStatus::Pending);
    }

    /// Store the outcome of a summary generation.
    pub fn set_summary_result(&mut self, id: String, result: Result<String, String>) {
        let status = match result {
            Ok(text) => SummaryStatus::Ready(text),
            Err(e) => SummaryStatus::Failed(e),
        };
        self.agent_summaries.insert(id, status);
    }

    /// Current summary status for a session, if any.
    pub fn summary_status(&self, id: &str) -> Option<&SummaryStatus> {
        self.agent_summaries.get(id)
    }

    /// Per-attention-group counts, used for the agent-view header summary.
    pub fn agent_group_counts(&self) -> (usize, usize, usize) {
        let mut needs = 0;
        let mut working = 0;
        let mut completed = 0;
        for s in &self.agent_sessions {
            match s.state.group() {
                crate::agents::AgentGroup::NeedsInput => needs += 1,
                crate::agents::AgentGroup::Working => working += 1,
                crate::agents::AgentGroup::Completed => completed += 1,
            }
        }
        (needs, working, completed)
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
            ViewMode::AgentMonitor => None,
            // Agent-view sessions are not tmux panes; they have no send-keys target.
            ViewMode::Dashboard => None,
        }
    }

    pub fn get_enter_target(&self) -> Option<String> {
        match self.view_mode {
            ViewMode::TreeView => match self.focus {
                Focus::Sessions => self
                    .sessions
                    .get(self.selected_session)
                    .map(|s| s.name.clone()),
                Focus::Windows => {
                    let session = self.sessions.get(self.selected_session)?;
                    let window = session.windows.get(self.selected_window)?;
                    Some(format!("{}:{}", session.name, window.index))
                }
                Focus::Panes => self.get_selected_pane_target(),
            },
            ViewMode::AgentMonitor => self
                .selected_agent_pane()
                .map(|agent| agent.target.clone()),
            // The agent view attaches via `claude attach`, not a tmux target.
            ViewMode::Dashboard => None,
        }
    }

    /// `input_cursor`（char 単位）を `input_buffer` 内のバイトオフセットへ変換する。
    fn input_cursor_byte_offset(&self) -> usize {
        self.input_buffer
            .char_indices()
            .nth(self.input_cursor)
            .map(|(byte_idx, _)| byte_idx)
            .unwrap_or(self.input_buffer.len())
    }

    /// `input_buffer` の文字数（char 単位）。
    fn input_char_count(&self) -> usize {
        self.input_buffer.chars().count()
    }

    pub fn input_char(&mut self, c: char) {
        let byte_offset = self.input_cursor_byte_offset();
        self.input_buffer.insert(byte_offset, c);
        self.input_cursor += 1;
    }

    /// Insert a character only while the buffer holds fewer than `max_chars`
    /// characters; otherwise the keystroke is ignored. Used by the session/group
    /// name popups to cap the name length.
    pub fn input_char_limited(&mut self, c: char, max_chars: usize) {
        if self.input_char_count() < max_chars {
            self.input_char(c);
        }
    }

    pub fn input_backspace(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
            let byte_offset = self.input_cursor_byte_offset();
            self.input_buffer.remove(byte_offset);
        }
    }

    pub fn input_delete(&mut self) {
        if self.input_cursor < self.input_char_count() {
            let byte_offset = self.input_cursor_byte_offset();
            self.input_buffer.remove(byte_offset);
        }
    }

    pub fn input_move_left(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
        }
    }

    pub fn input_move_right(&mut self) {
        if self.input_cursor < self.input_char_count() {
            self.input_cursor += 1;
        }
    }

    pub fn input_move_home(&mut self) {
        self.input_cursor = 0;
    }

    pub fn input_move_end(&mut self) {
        self.input_cursor = self.input_char_count();
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
            self.input_cursor = self.input_char_count();
        }
    }

    pub fn open_group_session_popup(&mut self) {
        let Some(session) = self.sessions.get(self.selected_session) else {
            return;
        };
        let current = session.group.clone();
        self.popup_mode = Some(PopupMode::GroupSession);
        self.group_choices = self.groups.group_names();
        // Highlight the session's current group by default, falling back to the
        // "Ungrouped" entry (which sits just past the existing groups) when the
        // session is not grouped yet.
        self.group_choice_index = match current {
            Some(g) => self
                .group_choices
                .iter()
                .position(|name| *name == g)
                .unwrap_or(self.group_choices.len()),
            None => self.group_choices.len(),
        };
        self.input_buffer.clear();
        self.input_cursor = 0;
    }

    /// Total number of entries in the GroupSession list: every existing group,
    /// then the "Ungrouped" and "New group" entries.
    pub fn group_choice_count(&self) -> usize {
        self.group_choices.len() + 2
    }

    /// The entry currently highlighted in the GroupSession list.
    pub fn selected_group_choice(&self) -> GroupChoice {
        let n = self.group_choices.len();
        if self.group_choice_index < n {
            GroupChoice::Existing(self.group_choices[self.group_choice_index].clone())
        } else if self.group_choice_index == n {
            GroupChoice::Ungrouped
        } else {
            GroupChoice::New
        }
    }

    pub fn group_choice_up(&mut self) {
        let n = self.group_choice_count();
        self.group_choice_index = (self.group_choice_index + n - 1) % n;
    }

    pub fn group_choice_down(&mut self) {
        let n = self.group_choice_count();
        self.group_choice_index = (self.group_choice_index + 1) % n;
    }

    /// Switch the open GroupSession popup into text entry for a new group name.
    pub fn begin_new_group_entry(&mut self) {
        self.popup_mode = Some(PopupMode::NewGroup);
        self.input_buffer.clear();
        self.input_cursor = 0;
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
        self.group_choices.clear();
        self.group_choice_index = 0;
    }

    pub fn toggle_confirm_selection(&mut self) {
        self.confirm_yes_selected = !self.confirm_yes_selected;
    }

    /// Get the session name to create (for NewSession popup)
    pub fn get_new_session_name(&self) -> String {
        self.input_buffer.trim().to_string()
    }

    /// Get the current session name and new name (for RenameSession popup)
    pub fn get_rename_session_info(&self) -> Option<(String, String)> {
        let new_name = self.input_buffer.trim().to_string();
        if new_name.is_empty() {
            return None;
        }
        self.sessions
            .get(self.selected_session)
            .map(|s| (s.name.clone(), new_name))
    }

    /// Get the group name typed in the GroupSession popup. An empty/whitespace
    /// entry means "remove from any group" and is returned as `None`.
    pub fn get_group_session_input(&self) -> Option<String> {
        let trimmed = self.input_buffer.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    /// Get the session name to kill (for ConfirmKill popup)
    pub fn get_kill_session_name(&self) -> Option<String> {
        if self.confirm_yes_selected {
            self.sessions
                .get(self.selected_session)
                .map(|s| s.name.clone())
        } else {
            None
        }
    }

    // =========================================================================
    // Data Update (called when TmuxResponse is received)
    // =========================================================================

    pub fn update_sessions(&mut self, sessions: Vec<TmuxSession>) {
        let previous_target = self.get_selected_pane_target();
        // Preserve the user's currently-highlighted session across the refresh:
        // it may move to a new index once the new order is applied (e.g. when
        // sort is Alphabet and a session was renamed).
        let current_name = self
            .sessions
            .get(self.selected_session)
            .map(|s| s.name.clone());

        self.sessions = sessions;
        self.apply_group_labels();
        self.order_sessions();
        self.rebuild_agent_panes(crate::hook::now_secs());

        if let Some(name) = current_name
            && let Some(idx) = self.sessions.iter().position(|s| s.name == name)
        {
            self.selected_session = idx;
        }

        self.validate_selections();
        if self.get_selected_pane_target() != previous_target {
            self.reset_tree_preview_scroll();
        }
        self.last_error = None;
    }

    /// Stamp each session with its persisted group label. Called whenever fresh
    /// session data arrives from tmux, since the tmux layer is group-agnostic.
    fn apply_group_labels(&mut self) {
        for session in &mut self.sessions {
            session.group = self.groups.group_of(&session.name);
        }
    }

    /// Order the session list: first by the active [`SessionSort`], then cluster
    /// sessions of the same group together. Because the clustering pass is a
    /// *stable* sort keyed only on the group, sessions keep their sort order
    /// within each group, and ungrouped sessions fall to the bottom.
    fn order_sessions(&mut self) {
        self.session_sort.apply(&mut self.sessions);
        self.sessions.sort_by(|a, b| match (&a.group, &b.group) {
            (Some(x), Some(y)) => x.to_lowercase().cmp(&y.to_lowercase()),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
    }

    /// Assign the currently-selected session to `group` (or remove it from any
    /// group when `group` is `None`/empty), persist the change, and re-order the
    /// list in place keeping that session highlighted. No tmux round-trip is
    /// needed — grouping is entirely tmux-deck-side.
    pub fn assign_selected_group(&mut self, group: Option<String>) {
        let Some(session) = self.sessions.get(self.selected_session) else {
            return;
        };
        let name = session.name.clone();
        self.groups.set(&name, group.as_deref());
        // Reveal the destination group so the user sees the session land, even
        // if that group was folded.
        self.collapsed_groups.remove(&group);
        self.apply_group_labels();
        self.resort_sessions_preserve_selection();
    }

    /// Whether any session carries a group label. When false there are no
    /// headers and folding is a no-op (there is nothing to organise yet).
    fn any_grouped(&self) -> bool {
        self.sessions.iter().any(|s| s.group.is_some())
    }

    /// Whether `group` is currently folded. Folding only takes effect once
    /// real groups exist, so a stray collapsed entry never hides a flat list.
    fn is_collapsed(&self, group: &Option<String>) -> bool {
        self.any_grouped() && self.collapsed_groups.contains(group)
    }

    /// Whether the session at `index` is the first of its group in the current
    /// ordering — the row a folded group collapses onto.
    fn is_group_head(&self, index: usize) -> bool {
        match self.sessions.get(index) {
            None => false,
            Some(s) => index == 0 || self.sessions[index - 1].group != s.group,
        }
    }

    /// Whether the cursor may rest on the session at `index`. A session is a
    /// stop when it is visible, or when it is the head of a folded group — in
    /// which case the cursor visually sits on that group's (collapsed) header,
    /// so the group can be re-expanded with `za`.
    fn is_cursor_stop(&self, index: usize) -> bool {
        match self.sessions.get(index) {
            None => false,
            Some(s) => !self.is_collapsed(&s.group) || self.is_group_head(index),
        }
    }

    /// Whether the selection currently sits on a folded group's header rather
    /// than a visible session. Used by the renderer to highlight the header.
    pub fn selection_on_folded_header(&self) -> bool {
        self.sessions
            .get(self.selected_session)
            .map(|s| self.is_collapsed(&s.group))
            .unwrap_or(false)
    }

    /// Toggle the fold state of the group containing the selected session.
    /// When folding, the selection collapses onto the group's head so the
    /// cursor stays on the (now folded) header and the group can be reopened
    /// with another `za`.
    pub fn toggle_fold_current_group(&mut self) {
        let previous_target = self.get_selected_pane_target();
        if !self.any_grouped() {
            return;
        }
        let Some(session) = self.sessions.get(self.selected_session) else {
            return;
        };
        let group = session.group.clone();
        if self.collapsed_groups.contains(&group) {
            self.collapsed_groups.remove(&group);
        } else {
            self.collapsed_groups.insert(group.clone());
            // Park the selection on the group's head so it remains a valid
            // cursor stop (the folded header) instead of a hidden member.
            if let Some(head) = self.sessions.iter().position(|s| s.group == group) {
                self.selected_session = head;
                self.selected_window = 0;
                self.selected_pane = 0;
                self.window_list_state.select(Some(0));
                self.pane_list_state.select(Some(0));
            }
        }
        if self.get_selected_pane_target() != previous_target {
            self.reset_tree_preview_scroll();
        }
    }

    fn next_cursor_stop(&self, from: usize) -> Option<usize> {
        ((from + 1)..self.sessions.len()).find(|&i| self.is_cursor_stop(i))
    }

    fn prev_cursor_stop(&self, from: usize) -> Option<usize> {
        (0..from).rev().find(|&i| self.is_cursor_stop(i))
    }

    /// Build the rendered Sessions rows, inserting group headers and dropping
    /// the members of folded groups. When no session is grouped the result is a
    /// flat list of [`SessionRow::Session`] rows (no headers), matching the
    /// pre-grouping behaviour exactly.
    pub fn session_rows(&self) -> Vec<SessionRow> {
        let any_grouped = self.any_grouped();
        let mut rows = Vec::with_capacity(self.sessions.len());
        let mut current: Option<&Option<String>> = None;
        for (index, session) in self.sessions.iter().enumerate() {
            let collapsed = any_grouped && self.collapsed_groups.contains(&session.group);
            if any_grouped && current != Some(&session.group) {
                let count = self
                    .sessions
                    .iter()
                    .filter(|s| s.group == session.group)
                    .count();
                rows.push(SessionRow::Header {
                    group: session.group.clone(),
                    count,
                    collapsed,
                });
                current = Some(&session.group);
            }
            if !collapsed {
                rows.push(SessionRow::Session { index });
            }
        }
        rows
    }

    /// Advance to the next [`SessionSort`] and re-sort the list in place,
    /// keeping the currently-highlighted session highlighted.
    pub fn cycle_session_sort(&mut self) {
        self.session_sort = self.session_sort.next();
        self.resort_sessions_preserve_selection();
    }

    fn resort_sessions_preserve_selection(&mut self) {
        let current_name = self
            .sessions
            .get(self.selected_session)
            .map(|s| s.name.clone());

        self.order_sessions();

        if let Some(name) = current_name
            && let Some(idx) = self.sessions.iter().position(|s| s.name == name)
        {
            self.selected_session = idx;
            self.session_list_state.select(Some(idx));
        }
    }

    pub fn update_pane_content(&mut self, content: String) {
        self.pane_content_parsed = content.as_bytes().into_text().ok();
        self.pane_content = content;
        self.clamp_tree_preview_scroll();
    }

    /// Apply a capture only while its pane is still selected. Preview captures
    /// are asynchronous, so a response for the previous selection may arrive
    /// after the user has moved elsewhere in the tree.
    pub fn update_tree_preview_content(&mut self, target: &str, content: String) -> bool {
        if self.get_selected_pane_target().as_deref() != Some(target) {
            return false;
        }
        self.update_pane_content(content);
        true
    }

    /// Record the current preview viewport height so page-wise key actions use
    /// the dimensions the user actually sees.
    pub fn set_tree_preview_height(&mut self, height: usize) {
        self.tree_preview_height = height;
        self.clamp_tree_preview_scroll();
    }

    pub fn tree_preview_scroll_up_line(&mut self) {
        self.scroll_tree_preview_up(1);
    }

    pub fn tree_preview_scroll_down_line(&mut self) {
        self.tree_preview_scroll = self.tree_preview_scroll.saturating_sub(1);
    }

    pub fn tree_preview_scroll_up_half_page(&mut self) {
        self.scroll_tree_preview_up((self.tree_preview_height / 2).max(1));
    }

    pub fn tree_preview_scroll_down_half_page(&mut self) {
        let amount = (self.tree_preview_height / 2).max(1);
        self.tree_preview_scroll = self.tree_preview_scroll.saturating_sub(amount);
    }

    /// Line range to render, keeping offset zero anchored to the live tail.
    pub fn tree_preview_visible_range(&self, line_count: usize) -> std::ops::Range<usize> {
        let height = self.tree_preview_height;
        if height == 0 {
            return line_count..line_count;
        }
        let offset = self
            .tree_preview_scroll
            .min(line_count.saturating_sub(height));
        let end = line_count.saturating_sub(offset);
        end.saturating_sub(height)..end
    }

    fn scroll_tree_preview_up(&mut self, amount: usize) {
        if self.tree_preview_height == 0 {
            return;
        }
        let max_scroll = self
            .tree_preview_line_count()
            .saturating_sub(self.tree_preview_height);
        self.tree_preview_scroll = self
            .tree_preview_scroll
            .saturating_add(amount)
            .min(max_scroll);
    }

    fn clamp_tree_preview_scroll(&mut self) {
        let max_scroll = self
            .tree_preview_line_count()
            .saturating_sub(self.tree_preview_height);
        self.tree_preview_scroll = self.tree_preview_scroll.min(max_scroll);
    }

    fn tree_preview_line_count(&self) -> usize {
        self.pane_content_parsed
            .as_ref()
            .map(|text| text.lines.len())
            .unwrap_or_else(|| self.pane_content.lines().count())
    }

    fn reset_tree_preview_scroll(&mut self) {
        self.tree_preview_scroll = 0;
    }

    pub fn set_error(&mut self, message: String) {
        self.last_error = Some(message);
    }

    pub fn validate_selections(&mut self) {
        if !self.sessions.is_empty() {
            self.selected_session = self.selected_session.min(self.sessions.len() - 1);
            if let Some(session) = self.sessions.get(self.selected_session)
                && !session.windows.is_empty()
            {
                self.selected_window = self.selected_window.min(session.windows.len() - 1);
                if let Some(window) = session.windows.get(self.selected_window)
                    && !window.panes.is_empty()
                {
                    self.selected_pane = self.selected_pane.min(window.panes.len() - 1);
                }
            }

            self.session_list_state.select(Some(self.selected_session));
            self.window_list_state.select(Some(self.selected_window));
            self.pane_list_state.select(Some(self.selected_pane));
        } else {
            self.session_list_state.select(None);
            self.window_list_state.select(None);
            self.pane_list_state.select(None);
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

    pub fn get_selected_pane_target_with_capture_range(&self) -> Option<(String, i32, i32)> {
        let session = self.sessions.get(self.selected_session)?;
        let window = session.windows.get(self.selected_window)?;
        let pane = window.panes.get(self.selected_pane)?;
        let target = format!("{}:{}.{}", session.name, window.index, pane.index);
        let height = i32::try_from(pane.height).unwrap_or(i32::MAX);
        let start = 0;
        let end = height;
        Some((target, start, end))
    }

    pub fn tree_move_up(&mut self) {
        let previous_target = self.get_selected_pane_target();
        match self.focus {
            Focus::Sessions => {
                if let Some(prev) = self.prev_cursor_stop(self.selected_session) {
                    self.selected_session = prev;
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
        if self.get_selected_pane_target() != previous_target {
            self.reset_tree_preview_scroll();
        }
    }

    pub fn tree_move_down(&mut self) {
        let previous_target = self.get_selected_pane_target();
        match self.focus {
            Focus::Sessions => {
                if let Some(next) = self.next_cursor_stop(self.selected_session) {
                    self.selected_session = next;
                    self.selected_window = 0;
                    self.selected_pane = 0;
                    self.window_list_state.select(Some(0));
                    self.pane_list_state.select(Some(0));
                }
                self.session_list_state.select(Some(self.selected_session));
            }
            Focus::Windows => {
                if let Some(session) = self.sessions.get(self.selected_session)
                    && self.selected_window < session.windows.len().saturating_sub(1)
                {
                    self.selected_window += 1;
                    self.selected_pane = 0;
                    self.pane_list_state.select(Some(0));
                }
                self.window_list_state.select(Some(self.selected_window));
            }
            Focus::Panes => {
                if let Some(session) = self.sessions.get(self.selected_session)
                    && let Some(window) = session.windows.get(self.selected_window)
                    && self.selected_pane < window.panes.len().saturating_sub(1)
                {
                    self.selected_pane += 1;
                }
                self.pane_list_state.select(Some(self.selected_pane));
            }
        }
        if self.get_selected_pane_target() != previous_target {
            self.reset_tree_preview_scroll();
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

}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str) -> TmuxSession {
        TmuxSession {
            name: name.to_string(),
            windows: Vec::new(),
            has_claude: false,
            claude_state: None,
            has_codex: false,
            codex_state: None,
            last_attached: 0,
            activity: 0,
            group: None,
        }
    }

    fn session_with_pane(name: &str, pane_index: u32) -> TmuxSession {
        TmuxSession {
            name: name.to_string(),
            windows: vec![TmuxWindow {
                index: 0,
                name: "window".to_string(),
                panes: vec![TmuxPane {
                    id: format!("%{pane_index}"),
                    index: pane_index,
                    width: 80,
                    height: 24,
                    active: true,
                    current_command: "shell".to_string(),
                    current_path: "/tmp".to_string(),
                    pid: 1,
                    has_claude: false,
                    claude_state: None,
                    claude_activity: None,
                    claude_state_since: None,
                    claude_cwd: None,
                    has_codex: false,
                    codex_state: None,
                    codex_activity: None,
                    codex_state_since: None,
                    codex_cwd: None,
                    agent_repository: None,
                    agent_worktree: None,
                    agent_repository_parent: None,
                }],
                has_claude: false,
                claude_state: None,
                has_codex: false,
                codex_state: None,
            }],
            ..session(name)
        }
    }

    /// Build a UIState with an in-memory (no-disk) group store and the given
    /// assignments, then load `names` as the session list.
    fn state_with(names: &[&str], groups: &[(&str, &str)]) -> UIState {
        let mut state = UIState::new(Config::default());
        state.groups = GroupStore::default();
        for (sess, grp) in groups {
            state.groups.set(sess, Some(grp));
        }
        state.update_sessions(names.iter().map(|n| session(n)).collect());
        state
    }

    fn agent_pane(
        id: &str,
        index: u32,
        kind: AgentKind,
        state: Option<HookState>,
        since: Option<i64>,
        cwd: Option<&str>,
    ) -> TmuxPane {
        TmuxPane {
            id: id.to_string(),
            index,
            width: 120,
            height: 40,
            active: index == 0,
            current_command: kind.label().to_ascii_lowercase(),
            current_path: cwd.unwrap_or("/tmp").to_string(),
            pid: index + 100,
            has_claude: kind == AgentKind::Claude,
            claude_state: (kind == AgentKind::Claude).then_some(state).flatten(),
            claude_activity: (kind == AgentKind::Claude).then(|| "editing ui.rs".to_string()),
            claude_state_since: (kind == AgentKind::Claude).then_some(since).flatten(),
            claude_cwd: (kind == AgentKind::Claude)
                .then(|| cwd.map(ToOwned::to_owned))
                .flatten(),
            has_codex: kind == AgentKind::Codex,
            codex_state: (kind == AgentKind::Codex).then_some(state).flatten(),
            codex_activity: (kind == AgentKind::Codex).then(|| "running tests".to_string()),
            codex_state_since: (kind == AgentKind::Codex).then_some(since).flatten(),
            codex_cwd: (kind == AgentKind::Codex)
                .then(|| cwd.map(ToOwned::to_owned))
                .flatten(),
            agent_repository: None,
            agent_worktree: None,
            agent_repository_parent: None,
        }
    }

    fn monitored_session(name: &str, panes: Vec<TmuxPane>) -> TmuxSession {
        TmuxSession {
            name: name.to_string(),
            windows: vec![TmuxWindow {
                index: 1,
                name: "main".to_string(),
                panes,
                has_claude: false,
                claude_state: None,
                has_codex: false,
                codex_state: None,
            }],
            has_claude: false,
            claude_state: None,
            has_codex: false,
            codex_state: None,
            last_attached: 0,
            activity: 0,
            group: None,
        }
    }

    fn monitor_state() -> UIState {
        let mut state = UIState::new(Config::default());
        state.groups = GroupStore::default();
        state.agent_monitor_mode = PresentationMode::Overview;
        state
    }

    #[test]
    fn pane_projection_keeps_each_supported_pane_and_hookless_fallback() {
        let sessions = vec![monitored_session(
            "dev",
            vec![
                agent_pane(
                    "%1",
                    0,
                    AgentKind::Claude,
                    Some(HookState::Waiting),
                    Some(90),
                    Some("/src/tmux-deck/.worktrees/feature-x"),
                ),
                agent_pane("%2", 1, AgentKind::Codex, None, None, None),
            ],
        )];
        let agents = project_agent_panes(&sessions, 100, 600);
        assert_eq!(agents.len(), 2);
        let hooked = agents.iter().find(|agent| agent.pane_id == "%1").unwrap();
        let hookless = agents.iter().find(|agent| agent.pane_id == "%2").unwrap();
        assert_eq!(hooked.state, ObservedState::Waiting);
        assert_eq!(hooked.repository.as_deref(), Some("tmux-deck"));
        assert_eq!(hooked.worktree.as_deref(), Some("feature-x"));
        assert_eq!(hookless.state, ObservedState::Running);
        assert_eq!(hookless.activity, "state unavailable");
    }

    #[test]
    fn attention_order_is_state_then_longest_wait() {
        let mut state = monitor_state();
        state.sessions = vec![monitored_session(
            "dev",
            vec![
                agent_pane("%work", 3, AgentKind::Codex, Some(HookState::Working), Some(1), None),
                agent_pane("%new", 2, AgentKind::Codex, Some(HookState::Waiting), Some(200), None),
                agent_pane("%old", 1, AgentKind::Claude, Some(HookState::Waiting), Some(100), None),
                agent_pane("%err", 0, AgentKind::Claude, Some(HookState::Error), Some(50), None),
            ],
        )];
        state.rebuild_agent_panes(300);
        state.agent_monitor_mode = PresentationMode::Attention;
        let ids: Vec<&str> = state
            .visible_agent_panes()
            .iter()
            .map(|agent| agent.pane_id.as_str())
            .collect();
        assert_eq!(ids, vec!["%old", "%new", "%err", "%work"]);
    }

    #[test]
    fn stable_insertion_and_selection_fallback_stay_in_worktree() {
        let mut state = monitor_state();
        state.sessions = vec![monitored_session(
            "dev",
            vec![
                agent_pane("%1", 0, AgentKind::Codex, Some(HookState::Working), Some(1), Some("/src/repo/.worktrees/a")),
                agent_pane("%3", 2, AgentKind::Claude, Some(HookState::Working), Some(1), Some("/src/repo/.worktrees/a")),
            ],
        )];
        state.rebuild_agent_panes(10);
        state.agent_pane_selected = Some("%1".to_string());
        state.agent_monitor_focused = true;
        state.sessions[0].windows[0].panes = vec![
            agent_pane("%2", 1, AgentKind::Codex, Some(HookState::Working), Some(1), Some("/src/repo/.worktrees/a")),
            agent_pane("%3", 2, AgentKind::Claude, Some(HookState::Working), Some(1), Some("/src/repo/.worktrees/a")),
        ];
        state.rebuild_agent_panes(11);
        assert_eq!(state.agent_order, vec!["%3", "%2"]);
        assert_eq!(state.agent_pane_selected.as_deref(), Some("%3"));
        assert!(!state.agent_monitor_focused);
        assert!(state.agent_monitor_message.is_some());

        state.sessions[0].windows[0].panes.push(agent_pane(
            "%0",
            3,
            AgentKind::Codex,
            Some(HookState::Working),
            Some(1),
            Some("/src/aaa/.worktrees/x"),
        ));
        state.rebuild_agent_panes(12);
        assert_eq!(state.agent_order, vec!["%3", "%2", "%0"]);
    }

    #[test]
    fn retention_filter_and_new_work_transition() {
        let mut state = monitor_state();
        state.agent_monitor_config.completed_retention_secs = 600;
        state.sessions = vec![monitored_session(
            "dev",
            vec![agent_pane(
                "%1",
                0,
                AgentKind::Codex,
                Some(HookState::Done),
                Some(100),
                Some("/src/repo"),
            )],
        )];
        state.rebuild_agent_panes(700);
        assert_eq!(state.agent_panes.len(), 1);
        state.rebuild_agent_panes(701);
        assert!(state.agent_panes.is_empty());
        state.sessions[0].windows[0].panes[0].codex_state = Some(HookState::Working);
        state.sessions[0].windows[0].panes[0].codex_state_since = Some(702);
        state.rebuild_agent_panes(702);
        assert_eq!(state.agent_panes[0].state, ObservedState::Working);
    }

    #[test]
    fn structured_and_free_text_filters_compose() {
        let mut state = monitor_state();
        state.sessions = vec![monitored_session(
            "dev",
            vec![
                agent_pane("%1", 0, AgentKind::Codex, Some(HookState::Working), Some(1), Some("/src/repo-a/.worktrees/main")),
                agent_pane("%2", 1, AgentKind::Claude, Some(HookState::Waiting), Some(1), Some("/src/repo-b/.worktrees/main")),
            ],
        )];
        state.rebuild_agent_panes(10);
        state.agent_monitor_filter = "state:work agent:codex tests".to_string();
        assert_eq!(state.visible_agent_panes().len(), 1);
        assert_eq!(state.visible_agent_panes()[0].pane_id, "%1");
        state.agent_monitor_filter = "repo:repo-b".to_string();
        assert_eq!(state.visible_agent_panes()[0].pane_id, "%2");
    }

    #[test]
    fn duplicate_repository_names_disambiguate_and_missing_git_uses_tmux_target() {
        let mut state = monitor_state();
        let mut first = agent_pane("%1", 0, AgentKind::Codex, None, None, None);
        first.agent_repository = Some("repo".to_string());
        first.agent_repository_parent = Some("alice".to_string());
        let mut second = agent_pane("%2", 1, AgentKind::Claude, None, None, None);
        second.agent_repository = Some("repo".to_string());
        second.agent_repository_parent = Some("bob".to_string());
        let missing = agent_pane("%3", 2, AgentKind::Codex, None, None, None);
        state.sessions = vec![monitored_session("dev", vec![first, second, missing])];
        state.rebuild_agent_panes(10);
        let identities: Vec<String> = state
            .agent_panes
            .iter()
            .map(|agent| state.agent_identity(agent))
            .collect();
        assert!(identities.contains(&"alice/repo".to_string()));
        assert!(identities.contains(&"bob/repo".to_string()));
        assert!(identities.contains(&"dev:1.%3".to_string()));

        state.agent_monitor_mode = PresentationMode::Attention;
        assert!(state.visible_agent_panes().is_empty());
    }

    #[test]
    fn density_targets_and_capture_budget_follow_design() {
        for count in [1, 4] {
            assert_eq!(overview_density(120, 40, count), OverviewDensity::LiveGrid);
        }
        for count in [5, 12] {
            assert_eq!(overview_density(120, 40, count), OverviewDensity::Hybrid);
        }
        for count in [13, 30, 31] {
            assert_eq!(overview_density(120, 40, count), OverviewDensity::SummaryList);
        }

        let mut state = monitor_state();
        state.sessions = vec![monitored_session(
            "dev",
            (0..5)
                .map(|index| agent_pane(&format!("%{index}"), index, AgentKind::Codex, Some(HookState::Working), Some(1), None))
                .collect(),
        )];
        state.rebuild_agent_panes(10);
        assert_eq!(state.agent_capture_targets(120, 40).len(), 1);
        assert!(state.agent_capture_targets(50, 20).is_empty());
        state.agent_monitor_focused = true;
        assert_eq!(state.agent_capture_targets(50, 20).len(), 1);

        state.agent_monitor_focused = false;
        state.sessions[0].windows[0].panes.truncate(4);
        state.rebuild_agent_panes(11);
        assert_eq!(state.agent_capture_targets(120, 40).len(), 4);
        state.agent_monitor_mode = PresentationMode::Attention;
        assert_eq!(state.agent_capture_targets(120, 40).len(), 1);
        assert!(state.agent_capture_targets(59, 40).is_empty());
    }

    #[test]
    fn monitor_view_transitions_and_large_list_navigation_are_unbounded() {
        let mut state = monitor_state();
        state.view_mode = ViewMode::TreeView;
        state.toggle_agent_monitor();
        assert_eq!(state.view_mode, ViewMode::AgentMonitor);
        state.toggle_agent_monitor();
        assert_eq!(state.view_mode, ViewMode::TreeView);
        state.view_mode = ViewMode::Dashboard;
        state.toggle_agent_monitor();
        assert_eq!(state.view_mode, ViewMode::AgentMonitor);

        state.agent_panes = (0..31)
            .map(|index| AgentPane {
                pane_id: format!("%{index}"),
                target: format!("dev:1.{index}"),
                tmux_identity: format!("dev:1.%{index}"),
                session_name: "dev".to_string(),
                window_index: 1,
                pane_index: index,
                pane_height: 24,
                kind: AgentKind::Codex,
                state: ObservedState::Working,
                activity: "working".to_string(),
                state_since: Some(1),
                repository: None,
                worktree: None,
                parent: None,
            })
            .collect();
        state.agent_order = state
            .agent_panes
            .iter()
            .map(|agent| agent.pane_id.clone())
            .collect();
        state.agent_pane_selected = state.agent_order.first().cloned();
        state.agent_move_end();
        assert_eq!(state.agent_pane_selected.as_deref(), Some("%30"));
        state.agent_move_page(10, false);
        assert_eq!(state.agent_pane_selected.as_deref(), Some("%20"));
        state.agent_move_home();
        assert_eq!(state.agent_pane_selected.as_deref(), Some("%0"));
    }

    #[test]
    fn background_refresh_preserves_provider_scoped_selection() {
        fn background(provider: crate::agents::AgentProvider) -> AgentSession {
            AgentSession {
                provider,
                id: "same-id".to_string(),
                name: provider.label().to_string(),
                state: crate::agents::AgentState::Idle,
                summary: String::new(),
                cwd: "/work".to_string(),
                elapsed_secs: 0,
                prs: Vec::new(),
                alive: true,
                transcript_path: None,
            }
        }

        let mut state = monitor_state();
        state.agent_sessions = vec![
            background(crate::agents::AgentProvider::Claude),
            background(crate::agents::AgentProvider::Codex),
        ];
        state.agent_selected = 1;
        state.update_agent_sessions(vec![
            background(crate::agents::AgentProvider::Codex),
            background(crate::agents::AgentProvider::Claude),
        ]);
        assert_eq!(state.agent_selected, 0);
        assert_eq!(
            state.selected_agent().unwrap().provider,
            crate::agents::AgentProvider::Codex
        );
    }

    /// Build a session with a single window holding the given panes, each
    /// described as `(pane_id, index, claude_state, state_since)`.

    #[test]
    fn ungrouped_sessions_have_no_headers() {
        let state = state_with(&["a", "b", "c"], &[]);
        let rows = state.session_rows();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| matches!(r, SessionRow::Session { .. })));
    }

    #[test]
    fn grouped_sessions_cluster_with_ungrouped_last() {
        // a, c -> "work"; b ungrouped. Names tie-break ascending within a group.
        let state = state_with(&["a", "b", "c"], &[("a", "work"), ("c", "work")]);
        let ordered: Vec<&str> = state.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(ordered, vec!["a", "c", "b"]);
    }

    #[test]
    fn rows_insert_one_header_per_group() {
        let state = state_with(
            &["a", "b", "c"],
            &[("a", "work"), ("c", "work"), ("b", "play")],
        );
        let rows = state.session_rows();
        // play(b) sorts before work(a,c) alphabetically; ungrouped bucket absent.
        let labels: Vec<String> = rows
            .iter()
            .filter_map(|r| match r {
                SessionRow::Header { group, count, .. } => {
                    Some(format!("{}:{}", group.as_deref().unwrap_or("none"), count))
                }
                SessionRow::Session { .. } => None,
            })
            .collect();
        assert_eq!(labels, vec!["play:1".to_string(), "work:2".to_string()]);
    }

    #[test]
    fn ungrouped_bucket_gets_a_header_when_mixed() {
        let state = state_with(&["a", "b"], &[("a", "work")]);
        let rows = state.session_rows();
        let has_ungrouped_header = rows.iter().any(|r| {
            matches!(r, SessionRow::Header { group: None, count, .. } if *count == 1)
        });
        assert!(has_ungrouped_header);
    }

    #[test]
    fn folding_hides_members_but_keeps_header() {
        // work: a, c ; play: b. Select "a" (in work) and fold its group.
        let mut state = state_with(
            &["a", "b", "c"],
            &[("a", "work"), ("c", "work"), ("b", "play")],
        );
        let work_idx = state.sessions.iter().position(|s| s.name == "a").unwrap();
        state.selected_session = work_idx;
        state.toggle_fold_current_group();

        let rows = state.session_rows();
        // No "work" member sessions remain visible, but its header stays
        // (now marked collapsed); play's member is still shown.
        let work_collapsed = rows.iter().any(|r| matches!(
            r,
            SessionRow::Header { group: Some(g), collapsed: true, .. } if g == "work"
        ));
        assert!(work_collapsed);
        let visible_sessions: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                SessionRow::Session { index } => Some(state.sessions[*index].name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(visible_sessions, vec!["b"]);
        // Selection parks on the folded group's head, so the cursor sits on the
        // (collapsed) "work" header and can re-open it.
        assert!(state.selection_on_folded_header());
        assert_eq!(state.sessions[state.selected_session].group.as_deref(), Some("work"));

        // Toggling again from the folded header re-expands the group — this is
        // the regression that previously had no way to recover.
        state.toggle_fold_current_group();
        let rows = state.session_rows();
        let names: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                SessionRow::Session { index } => Some(state.sessions[*index].name.as_str()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"a") && names.contains(&"c"));
        assert!(!state.selection_on_folded_header());
    }

    #[test]
    fn navigation_lands_on_folded_group_then_reopens() {
        let mut state = state_with(
            &["a", "b", "c"],
            &[("a", "work"), ("c", "work"), ("b", "play")],
        );
        // Order is play(b), work(a,c). Fold work (selection parks on work head).
        state.selected_session = state.sessions.iter().position(|s| s.name == "a").unwrap();
        state.toggle_fold_current_group();
        // From the visible "b", moving down stops on the folded work header
        // rather than skipping it entirely.
        state.selected_session = state.sessions.iter().position(|s| s.name == "b").unwrap();
        state.tree_move_down();
        assert!(state.selection_on_folded_header());
        assert_eq!(state.sessions[state.selected_session].group.as_deref(), Some("work"));
        // And `za` there expands it back.
        state.toggle_fold_current_group();
        assert!(!state.selection_on_folded_header());
    }

    #[test]
    fn fold_is_noop_without_groups() {
        let mut state = state_with(&["a", "b"], &[]);
        state.toggle_fold_current_group();
        let rows = state.session_rows();
        // No headers, all sessions still visible.
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| matches!(r, SessionRow::Session { .. })));
    }

    #[test]
    fn assigning_group_updates_store_and_order() {
        let mut state = state_with(&["a", "b"], &[]);
        state.selected_session = 1; // "b"
        state.assign_selected_group(Some("work".to_string()));
        assert_eq!(state.groups.group_of("b"), Some("work".to_string()));
        // "b" is now grouped and clusters above the ungrouped "a".
        let ordered: Vec<&str> = state.sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(ordered, vec!["b", "a"]);
        // Selection still tracks "b" after the reorder.
        assert_eq!(state.sessions[state.selected_session].name, "b");
    }

    #[test]
    fn group_popup_lists_existing_groups_and_highlights_current() {
        let state = state_with(
            &["a", "b", "c"],
            &[("a", "work"), ("b", "personal"), ("c", "work")],
        );
        // Sorted, deduplicated existing groups.
        assert_eq!(state.groups.group_names(), vec!["personal", "work"]);

        let mut state = state;
        state.selected_session = state.sessions.iter().position(|s| s.name == "b").unwrap();
        state.open_group_session_popup();
        assert_eq!(state.popup_mode, Some(PopupMode::GroupSession));
        // "b" is in "personal", so that entry starts highlighted.
        assert_eq!(
            state.selected_group_choice(),
            GroupChoice::Existing("personal".to_string())
        );
        // Entries: 2 groups + Ungrouped + New.
        assert_eq!(state.group_choice_count(), 4);
    }

    #[test]
    fn group_popup_defaults_to_ungrouped_for_ungrouped_session() {
        let mut state = state_with(&["a", "b"], &[("b", "work")]);
        state.selected_session = state.sessions.iter().position(|s| s.name == "a").unwrap();
        state.open_group_session_popup();
        // Index sits on the "Ungrouped" entry, just past the single group.
        assert_eq!(state.selected_group_choice(), GroupChoice::Ungrouped);
    }

    #[test]
    fn group_choice_navigation_wraps_and_reaches_new() {
        let mut state = state_with(&["a"], &[("a", "work")]);
        state.open_group_session_popup();
        // Entries: ["work", Ungrouped, New]. Starts on "work".
        assert_eq!(
            state.selected_group_choice(),
            GroupChoice::Existing("work".to_string())
        );
        state.group_choice_up(); // wraps to last entry
        assert_eq!(state.selected_group_choice(), GroupChoice::New);
        state.group_choice_down(); // wraps back to first
        assert_eq!(
            state.selected_group_choice(),
            GroupChoice::Existing("work".to_string())
        );
        state.group_choice_down();
        assert_eq!(state.selected_group_choice(), GroupChoice::Ungrouped);
    }

    #[test]
    fn input_handles_multibyte_chars_without_panic() {
        let mut state = UIState::new(Config::default());
        // 日本語を複数文字入力（旧実装ではバイト境界パニックしていた）
        state.input_char('あ');
        state.input_char('い');
        state.input_char('う');
        assert_eq!(state.input_buffer, "あいう");
        assert_eq!(state.input_cursor, 3);
    }

    #[test]
    fn input_cursor_movement_and_editing_with_multibyte() {
        let mut state = UIState::new(Config::default());
        for c in "あいう".chars() {
            state.input_char(c);
        }
        // 左へ2つ移動 → カーソルは「い」の前
        state.input_move_left();
        state.input_move_left();
        assert_eq!(state.input_cursor, 1);
        // カーソル位置に「ん」を挿入
        state.input_char('ん');
        assert_eq!(state.input_buffer, "あんいう");
        assert_eq!(state.input_cursor, 2);
        // backspace で「ん」を削除
        state.input_backspace();
        assert_eq!(state.input_buffer, "あいう");
        assert_eq!(state.input_cursor, 1);
        // delete でカーソル位置の「い」を削除
        state.input_delete();
        assert_eq!(state.input_buffer, "あう");
        assert_eq!(state.input_cursor, 1);
    }

    #[test]
    fn input_move_end_uses_char_count() {
        let mut state = UIState::new(Config::default());
        for c in "あい".chars() {
            state.input_char(c);
        }
        state.input_move_home();
        assert_eq!(state.input_cursor, 0);
        state.input_move_end();
        assert_eq!(state.input_cursor, 2);
    }

    #[test]
    fn input_char_limited_caps_char_count() {
        let mut state = UIState::new(Config::default());
        for _ in 0..40 {
            state.input_char_limited('a', SESSION_NAME_MAX_LEN);
        }
        assert_eq!(state.input_buffer.chars().count(), SESSION_NAME_MAX_LEN);
    }

    #[test]
    fn input_char_limited_counts_chars_not_bytes() {
        let mut state = UIState::new(Config::default());
        // マルチバイト文字でもバイト長ではなく文字数で制限される
        for _ in 0..40 {
            state.input_char_limited('あ', SESSION_NAME_MAX_LEN);
        }
        assert_eq!(state.input_buffer.chars().count(), SESSION_NAME_MAX_LEN);
    }

    #[test]
    fn tree_preview_scrolls_by_line_and_clamps_at_both_ends() {
        let mut state = UIState::new(Config::default());
        state.update_pane_content(
            (0..10)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        state.tree_preview_scroll_up_line();
        assert_eq!(state.tree_preview_scroll, 0);
        state.set_tree_preview_height(4);

        assert_eq!(state.tree_preview_visible_range(10), 6..10);
        state.tree_preview_scroll_up_line();
        assert_eq!(state.tree_preview_scroll, 1);
        assert_eq!(state.tree_preview_visible_range(10), 5..9);

        for _ in 0..20 {
            state.tree_preview_scroll_up_line();
        }
        assert_eq!(state.tree_preview_scroll, 6);
        assert_eq!(state.tree_preview_visible_range(10), 0..4);

        for _ in 0..20 {
            state.tree_preview_scroll_down_line();
        }
        assert_eq!(state.tree_preview_scroll, 0);
        assert_eq!(state.tree_preview_visible_range(10), 6..10);
    }

    #[test]
    fn tree_preview_half_page_uses_rendered_viewport_height() {
        let mut state = UIState::new(Config::default());
        state.update_pane_content(
            (0..20)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        state.set_tree_preview_height(7);

        state.tree_preview_scroll_up_half_page();
        assert_eq!(state.tree_preview_scroll, 3);
        state.tree_preview_scroll_up_half_page();
        assert_eq!(state.tree_preview_scroll, 6);
        state.tree_preview_scroll_down_half_page();
        assert_eq!(state.tree_preview_scroll, 3);
    }

    #[test]
    fn tree_preview_resets_when_refresh_changes_the_selected_pane() {
        let mut state = UIState::new(Config::default());
        state.groups = GroupStore::default();
        state.update_sessions(vec![session_with_pane("session", 0)]);
        state.update_pane_content(
            (0..10)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        state.set_tree_preview_height(4);
        state.tree_preview_scroll_up_half_page();
        assert_eq!(state.tree_preview_scroll, 2);

        state.update_sessions(vec![session_with_pane("session", 1)]);

        assert_eq!(state.tree_preview_scroll, 0);
        assert_eq!(
            state.get_selected_pane_target().as_deref(),
            Some("session:0.1")
        );
    }

    #[test]
    fn tree_preview_ignores_capture_for_a_previous_selection() {
        let mut state = UIState::new(Config::default());
        state.groups = GroupStore::default();
        state.update_sessions(vec![
            session_with_pane("first", 0),
            session_with_pane("second", 1),
        ]);

        assert!(state.update_tree_preview_content("first:0.0", "first pane".to_string()));
        state.set_tree_preview_height(1);
        state.update_pane_content("first line\nsecond line".to_string());
        state.tree_preview_scroll_up_line();
        assert_eq!(state.tree_preview_scroll, 1);
        state.tree_move_down();
        assert_eq!(
            state.get_selected_pane_target().as_deref(),
            Some("second:0.1")
        );
        assert_eq!(state.tree_preview_scroll, 0);
        assert!(!state.update_tree_preview_content("first:0.0", "stale".to_string()));
        assert_eq!(state.pane_content, "first line\nsecond line");
        assert!(state.update_tree_preview_content("second:0.1", "second pane".to_string()));
        assert_eq!(state.pane_content, "second pane");
    }
}
