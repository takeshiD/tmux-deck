//! Claude Code and Codex hook integration.
//!
//! Two halves live here:
//!
//! * The **reporter** (`tmux-deck hook report`) is wired into Claude Code's
//!   `settings.json` or Codex's `hooks.json`. The agent runs it on each hook
//!   event, passing the hook JSON on stdin. It records the *calling pane's*
//!   state to a small file keyed by `$TMUX_PANE`.
//! * The **reader** ([`apply_states`]) is used by the TUI to fold those files
//!   back into the session tree so each pane/window/session can show a marker
//!   reflecting what the agent is doing.
//!
//! The two sides are linked purely by `$TMUX_PANE`: the reporter inherits it
//! from the pane the agent runs in, and tmux exposes the same id as
//! `#{pane_id}`. Claude and Codex use separate state directories.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde_json::{Value, json};

use crate::app::{HookState, TmuxSession};

/// Claude hook events we install and listen for. `SessionEnd` is included so a
/// pane's marker is cleared when Claude exits.
const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "SubagentStop",
    "SessionEnd",
];

/// Codex lifecycle events used to derive the interactive pane state.
const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PreCompact",
    "PostCompact",
    "Stop",
    "Interrupt",
    "SessionEnd",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }

    fn dir_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn events(self) -> &'static [&'static str] {
        match self {
            Self::Claude => CLAUDE_EVENTS,
            Self::Codex => CODEX_EVENTS,
        }
    }
}

/// Drop state files older than this. A pane that closes without firing
/// `SessionEnd` (e.g. killed) would otherwise leave a stale marker forever.
const STALE_SECS: i64 = 6 * 60 * 60;

/// Substring that identifies a hook command as ours, for idempotent install.
const COMMAND_MARKER: &str = "hook report";
const EXECUTABLE_MARKER: &str = "tmux-deck";

// =============================================================================
// Paths / time helpers
// =============================================================================

/// Directory holding per-pane agent state files.
///
/// Resolves to `$XDG_STATE_HOME/tmux-deck/<agent>` (the `directories` crate
/// honours `XDG_STATE_HOME` on Linux), falling back to `~/.local/state/...`
/// on platforms where a state dir is not otherwise defined.
fn state_dir(agent: AgentKind) -> Option<PathBuf> {
    let base = ProjectDirs::from("dev", "tkcd", "tmux-deck")
        .and_then(|p| p.state_dir().map(|s| s.to_path_buf()))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/tmux-deck"))
        })?;
    Some(base.join(agent.dir_name()))
}

/// Current Unix time in seconds. Public so the UI can compute how long a pane
/// has been in its current hook state (`now - state_since`).
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Make a filesystem-safe file stem from a tmux pane id like `%3`.
fn pane_file_stem(pane: &str) -> String {
    let stem: String = pane
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    stem
}

fn valid_pane_id(pane: &str) -> bool {
    pane.strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Replace a file through a temporary sibling so readers never observe
/// partially-written JSON. Existing permissions are retained when possible.
fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tmux-deck");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = parent.join(format!(".{file_name}.tmp-{}-{nonce}", std::process::id()));

    let result = (|| {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        output.write_all(contents)?;
        if let Ok(metadata) = std::fs::metadata(path) {
            std::fs::set_permissions(&temp, metadata.permissions())?;
        }
        drop(output);
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn settings_write_path(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::canonicalize(path),
        Ok(_) => Ok(path.to_path_buf()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(error) => Err(error),
    }
}

// =============================================================================
// Reporter: `tmux-deck hook report`
// =============================================================================

/// Max length of the one-line activity digest stored in a state file. The full
/// `tool_input` is never persisted — only a short, single-line summary.
const ACTIVITY_MAX: usize = 80;
const CWD_MAX: usize = 4096;

fn capped(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Collapse a string into a single trimmed line, capped at [`ACTIVITY_MAX`]
/// chars (with an ellipsis when truncated).
fn one_line(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > ACTIVITY_MAX {
        let head: String = collapsed.chars().take(ACTIVITY_MAX - 1).collect();
        format!("{head}…")
    } else {
        collapsed
    }
}

/// Build a short, human-readable description of what the agent is doing, derived
/// from the hook event and a *digest* of its `tool_input`. Returns `None` for
/// events that carry no useful activity (e.g. `Stop`). The raw `tool_input` is
/// intentionally never stored — only this one-line summary.
fn summarize_activity(event: &str, value: &Value) -> Option<String> {
    match event {
        "PreToolUse" | "PostToolUse" => {
            let tool = value.get("tool_name").and_then(|t| t.as_str())?;
            let detail = value.get("tool_input").and_then(|i| match tool {
                "Bash" => i.get("command").and_then(|c| c.as_str()).map(one_line),
                "Edit" | "Write" | "Read" | "NotebookEdit" => {
                    i.get("file_path").and_then(|f| f.as_str()).map(one_line)
                }
                "Grep" | "Glob" => i.get("pattern").and_then(|p| p.as_str()).map(one_line),
                _ => None,
            });
            Some(match detail {
                Some(d) => format!("{tool}: {d}"),
                None => tool.to_string(),
            })
        }
        "Notification" => value.get("message").and_then(|m| m.as_str()).map(one_line),
        "UserPromptSubmit" => Some("prompt submitted".to_string()),
        _ => None,
    }
}

/// Entry point for `tmux-deck hook report`.
///
/// Always exits quietly (the calling agent should never be disrupted by a
/// hook), so every failure path is a silent early return.
pub fn run_report(codex: bool) {
    let agent = if codex {
        AgentKind::Codex
    } else {
        AgentKind::Claude
    };
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);

    // Without a pane id we cannot attribute the event to anything.
    let pane = match std::env::var("TMUX_PANE") {
        Ok(p) if valid_pane_id(&p) => p,
        _ => return,
    };

    // Parse the whole payload once: besides the event name we now mine it for
    // the activity detail (tool, cwd, message, ...) shown in the dashboard.
    let value = match serde_json::from_str::<Value>(&input) {
        Ok(v) => v,
        Err(_) => return,
    };
    let event = match value.get("hook_event_name").and_then(|e| e.as_str()) {
        Some(e) => e.to_string(),
        None => return,
    };

    let dir = match state_dir(agent) {
        Some(d) => d,
        None => return,
    };
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("{}.json", pane_file_stem(&pane)));

    match HookState::from_hook_event(&event) {
        Some(state) => {
            let now = now_secs();

            // `state_since` marks when the *current* state began, so the UI can
            // show how long a pane has been working/waiting. Carry it forward
            // while the state is unchanged; reset it on any transition.
            let prev = std::fs::read_to_string(&file)
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let same_state = prev
                .as_ref()
                .and_then(|p| p.get("state").and_then(|s| s.as_str()))
                == Some(state.as_token());
            let state_since = if same_state {
                prev.as_ref()
                    .and_then(|p| p.get("state_since").and_then(|t| t.as_i64()))
                    .unwrap_or(now)
            } else {
                now
            };

            let mut record = serde_json::Map::new();
            record.insert("pane".into(), json!(pane));
            record.insert("state".into(), json!(state.as_token()));
            record.insert("event".into(), json!(event));
            record.insert("ts".into(), json!(now));
            record.insert("state_since".into(), json!(state_since));
            // Optional context, only stored when present. `tool_input` itself is
            // never persisted — only the one-line `activity` digest below.
            if let Some(c) = value.get("cwd").and_then(|c| c.as_str()) {
                record.insert("cwd".into(), json!(capped(c, CWD_MAX)));
            }
            if let Some(a) = summarize_activity(&event, &value) {
                record.insert("activity".into(), json!(a));
            }
            let _ = write_atomic(&file, Value::Object(record).to_string().as_bytes());
        }
        None => {
            // Clear at both ends of a session: SessionStart prevents state
            // left by a prior process in the same tmux pane from leaking into
            // the new process, and SessionEnd removes the final marker.
            if matches!(event.as_str(), "SessionStart" | "SessionEnd") {
                let _ = std::fs::remove_file(&file);
            }
        }
    }
}

// =============================================================================
// Reader: fold state files into the session tree
// =============================================================================

/// The per-pane agent context read back from a state file. `state` is the only
/// required field; the rest enrich the dashboard and may be absent (e.g. for
/// state files written by older versions, or events that carry no activity).
#[derive(Debug, Clone)]
pub struct HookInfo {
    pub state: HookState,
    pub activity: Option<String>,
    pub state_since: Option<i64>,
    pub cwd: Option<String>,
}

/// Load the current per-pane agent info, keyed by tmux pane id (`%N`).
/// Stale files are removed as a side effect.
fn load_states(agent: AgentKind) -> HashMap<String, HookInfo> {
    let mut map = HashMap::new();
    let dir = match state_dir(agent) {
        Some(d) => d,
        None => return map,
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return map,
    };
    let now = now_secs();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let value: Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let ts = value.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
        if now - ts > STALE_SECS {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let pane = match value.get("pane").and_then(|p| p.as_str()) {
            Some(p) => p.to_string(),
            None => continue,
        };
        if let Some(info) = info_from_value(&value, ts) {
            map.insert(pane, info);
        }
    }
    map
}

/// Parse a [`HookInfo`] out of a state-file JSON object. `ts` is the file's
/// own timestamp, used as a fallback for `state_since` so older state files
/// (written before that field existed) still yield a meaningful elapsed time.
/// Returns `None` when the required `state` token is missing/unrecognised.
fn info_from_value(value: &Value, ts: i64) -> Option<HookInfo> {
    let state = value
        .get("state")
        .and_then(|s| s.as_str())
        .and_then(HookState::from_token)?;
    Some(HookInfo {
        state,
        activity: value.get("activity").and_then(|a| a.as_str()).map(one_line),
        state_since: value
            .get("state_since")
            .and_then(|t| t.as_i64())
            .or(Some(ts)),
        cwd: value
            .get("cwd")
            .and_then(|c| c.as_str())
            .map(|cwd| capped(cwd, CWD_MAX)),
    })
}

/// Apply the current hook states to a session tree, recomputing the
/// per-pane / per-window / per-session markers. Always recomputes from the
/// files on disk, so a marker that has gone away is cleared too.
pub fn apply_states(sessions: &mut [TmuxSession]) {
    let claude = load_states(AgentKind::Claude);
    let codex = load_states(AgentKind::Codex);
    apply_state_maps(sessions, &claude, &codex);
}

fn apply_state_maps(
    sessions: &mut [TmuxSession],
    claude: &HashMap<String, HookInfo>,
    codex: &HashMap<String, HookInfo>,
) {
    for session in sessions.iter_mut() {
        let mut claude_session_state = None;
        let mut codex_session_state = None;
        for window in session.windows.iter_mut() {
            let mut claude_window_state = None;
            let mut codex_window_state = None;
            for pane in window.panes.iter_mut() {
                match claude.get(&pane.id) {
                    Some(info) => {
                        pane.claude_state = Some(info.state);
                        pane.claude_activity = info.activity.clone();
                        pane.claude_state_since = info.state_since;
                        pane.claude_cwd = info.cwd.clone();
                    }
                    None => {
                        pane.claude_state = None;
                        pane.claude_activity = None;
                        pane.claude_state_since = None;
                        pane.claude_cwd = None;
                    }
                }
                match codex.get(&pane.id) {
                    Some(info) => {
                        pane.codex_state = Some(info.state);
                        pane.codex_activity = info.activity.clone();
                        pane.codex_state_since = info.state_since;
                        pane.codex_cwd = info.cwd.clone();
                    }
                    None => {
                        pane.codex_state = None;
                        pane.codex_activity = None;
                        pane.codex_state_since = None;
                        pane.codex_cwd = None;
                    }
                }
                claude_window_state = HookState::merge(claude_window_state, pane.claude_state);
                codex_window_state = HookState::merge(codex_window_state, pane.codex_state);
            }
            window.claude_state = claude_window_state;
            window.codex_state = codex_window_state;
            claude_session_state = HookState::merge(claude_session_state, claude_window_state);
            codex_session_state = HookState::merge(codex_session_state, codex_window_state);
        }
        session.claude_state = claude_session_state;
        session.codex_state = codex_session_state;
    }
}

// =============================================================================
// Installer: `tmux-deck hook install`
// =============================================================================

/// Entry point for `tmux-deck hook install [--project] [--codex]`.
pub fn run_install(project: bool, codex: bool) -> color_eyre::Result<()> {
    let agent = if codex {
        AgentKind::Codex
    } else {
        AgentKind::Claude
    };
    let path = settings_path(project, agent)?;
    let command = report_command(agent);

    let existing = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str::<Value>(&s)?,
        Ok(_) => json!({}),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => return Err(error.into()),
    };
    let merged = merge_hooks(existing, &command, agent.events())
        .map_err(|message| color_eyre::eyre::eyre!(message))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let write_path = settings_write_path(&path)?;
    let mut out = serde_json::to_string_pretty(&merged)?;
    out.push('\n');
    write_atomic(&write_path, out.as_bytes())?;

    println!(
        "Installed tmux-deck {} hooks into {}",
        agent.name(),
        path.display()
    );
    println!("Events: {}", agent.events().join(", "));
    if agent == AgentKind::Codex {
        println!("Review and trust the hooks with /hooks in Codex before first use.");
    }
    Ok(())
}

/// The command the agent should run for each event. Uses the absolute path to the
/// current executable so it works regardless of `$PATH`.
fn report_command(agent: AgentKind) -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "tmux-deck".to_string());
    let suffix = if agent == AgentKind::Codex {
        " --codex"
    } else {
        ""
    };
    format!("{} hook report{}", shell_quote(&exe), suffix)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn settings_path(project: bool, agent: AgentKind) -> color_eyre::Result<PathBuf> {
    let (dir, file) = match agent {
        AgentKind::Claude => (".claude", "settings.json"),
        AgentKind::Codex => (".codex", "hooks.json"),
    };
    if project {
        Ok(PathBuf::from(dir).join(file))
    } else {
        let user_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| color_eyre::eyre::eyre!("HOME is not set"))?;
        let agent_home_override = match agent {
            AgentKind::Claude => std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
            AgentKind::Codex => std::env::var_os("CODEX_HOME").map(PathBuf::from),
        };
        Ok(user_settings_path(
            agent,
            &user_home,
            agent_home_override.as_deref(),
        ))
    }
}

fn user_settings_path(
    agent: AgentKind,
    user_home: &Path,
    agent_home_override: Option<&Path>,
) -> PathBuf {
    let (default_dir, file) = match agent {
        AgentKind::Claude => (".claude", "settings.json"),
        AgentKind::Codex => (".codex", "hooks.json"),
    };
    agent_home_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| user_home.join(default_dir))
        .join(file)
}

/// Merge our managed hooks into an existing settings document, idempotently.
///
/// Any previously-installed tmux-deck report hook is removed first, so running
/// install repeatedly never duplicates entries and always refreshes the path.
fn merge_hooks(mut root: Value, command: &str, events: &[&str]) -> Result<Value, String> {
    let obj = root
        .as_object_mut()
        .ok_or_else(|| "settings root must be a JSON object".to_string())?;

    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "settings `hooks` must be a JSON object".to_string())?;

    for event in events {
        let entry = hooks
            .entry((*event).to_string())
            .or_insert_with(|| json!([]));
        let groups = entry
            .as_array_mut()
            .ok_or_else(|| format!("settings `hooks.{event}` must be a JSON array"))?;
        groups.retain_mut(remove_our_hooks);
        groups.push(json!({
            "hooks": [ { "type": "command", "command": command } ]
        }));
    }
    Ok(root)
}

/// Whether a hook group was installed by us (contains a `hook report` command).
#[cfg(test)]
fn group_is_ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hooks| hooks.iter().any(hook_is_ours))
        .unwrap_or(false)
}

fn hook_is_ours(hook: &Value) -> bool {
    hook.get("command")
        .and_then(|command| command.as_str())
        .is_some_and(|command| {
            command.contains(EXECUTABLE_MARKER) && command.contains(COMMAND_MARKER)
        })
}

/// Remove only tmux-deck handlers from a matcher group. A group can contain
/// handlers owned by several integrations, so deleting the whole group would
/// silently discard unrelated hooks.
fn remove_our_hooks(group: &mut Value) -> bool {
    let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
        return true;
    };
    let previous_len = hooks.len();
    hooks.retain(|hook| !hook_is_ours(hook));
    previous_len == hooks.len() || !hooks.is_empty()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_hook_events_to_states() {
        assert_eq!(
            HookState::from_hook_event("UserPromptSubmit"),
            Some(HookState::Working)
        );
        assert_eq!(
            HookState::from_hook_event("Notification"),
            Some(HookState::Waiting)
        );
        assert_eq!(HookState::from_hook_event("Stop"), Some(HookState::Done));
        assert_eq!(HookState::from_hook_event("SessionEnd"), None);
        assert_eq!(HookState::from_hook_event("Whatever"), None);
        assert_eq!(
            HookState::from_hook_event("PermissionRequest"),
            Some(HookState::Waiting)
        );
        assert_eq!(
            HookState::from_hook_event("PostCompact"),
            Some(HookState::Working)
        );
        assert_eq!(
            HookState::from_hook_event("Interrupt"),
            Some(HookState::Done)
        );
    }

    #[test]
    fn token_roundtrips() {
        for s in [
            HookState::Working,
            HookState::Waiting,
            HookState::Done,
            HookState::Error,
        ] {
            assert_eq!(HookState::from_token(s.as_token()), Some(s));
        }
    }

    #[test]
    fn merge_keeps_higher_priority() {
        // Waiting (3) beats Working (1); Done (0) loses to everything.
        assert_eq!(
            HookState::merge(Some(HookState::Working), Some(HookState::Waiting)),
            Some(HookState::Waiting)
        );
        assert_eq!(
            HookState::merge(Some(HookState::Done), Some(HookState::Working)),
            Some(HookState::Working)
        );
        assert_eq!(
            HookState::merge(None, Some(HookState::Done)),
            Some(HookState::Done)
        );
        assert_eq!(HookState::merge(None, None), None);
    }

    #[test]
    fn pane_file_stem_is_safe() {
        assert_eq!(pane_file_stem("%3"), "_3");
        assert_eq!(pane_file_stem("%12"), "_12");
        assert!(valid_pane_id("%3"));
        assert!(!valid_pane_id("3"));
        assert!(!valid_pane_id("%3/other"));
        assert!(!valid_pane_id("%"));
    }

    #[test]
    fn one_line_collapses_and_caps() {
        // Newlines and runs of whitespace collapse to single spaces.
        assert_eq!(one_line("cargo   test\n--all"), "cargo test --all");
        // Long input is truncated with an ellipsis and never exceeds the cap.
        let long = "x".repeat(200);
        let out = one_line(&long);
        assert!(out.chars().count() <= ACTIVITY_MAX);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn atomic_writer_replaces_complete_files_without_temp_leaks() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "tmux-deck-hook-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("hooks.json");

        write_atomic(&path, br#"{"version":1}"#).unwrap();
        write_atomic(&path, br#"{"version":2}"#).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"version":2}"#);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn settings_update_preserves_existing_symlinks() {
        use std::os::unix::fs::symlink;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "tmux-deck-hook-link-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let target = dir.join("shared.json");
        let link = dir.join("hooks.json");
        std::fs::write(&target, b"old").unwrap();
        symlink("shared.json", &link).unwrap();

        let resolved = settings_write_path(&link).unwrap();
        write_atomic(&resolved, b"new").unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "new");
        std::fs::remove_file(link).unwrap();
        std::fs::remove_file(target).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn info_from_value_reads_enriched_fields() {
        let v = json!({
            "pane": "%3", "state": "working", "ts": 100, "state_since": 90,
            "activity": "Edit: src/app.rs", "cwd": "/repo"
        });
        let info = info_from_value(&v, 100).unwrap();
        assert_eq!(info.state, HookState::Working);
        assert_eq!(info.activity.as_deref(), Some("Edit: src/app.rs"));
        assert_eq!(info.state_since, Some(90));
        assert_eq!(info.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn info_from_value_is_backward_compatible() {
        // An old-format file (only pane/state/ts) still parses; `state_since`
        // falls back to the file timestamp and the rest are absent.
        let v = json!({ "pane": "%3", "state": "waiting", "ts": 42 });
        let info = info_from_value(&v, 42).unwrap();
        assert_eq!(info.state, HookState::Waiting);
        assert_eq!(info.state_since, Some(42));
        assert!(info.activity.is_none());
        assert!(info.cwd.is_none());

        // A missing/garbage state yields nothing.
        assert!(info_from_value(&json!({ "pane": "%3" }), 0).is_none());
    }

    #[test]
    fn state_file_context_is_bounded_when_read() {
        let value = json!({
            "state": "working",
            "activity": "x".repeat(ACTIVITY_MAX + 50),
            "cwd": "y".repeat(CWD_MAX + 50)
        });
        let info = info_from_value(&value, 1).unwrap();
        assert_eq!(info.activity.unwrap().chars().count(), ACTIVITY_MAX);
        assert_eq!(info.cwd.unwrap().chars().count(), CWD_MAX);
    }

    #[test]
    fn applies_claude_and_codex_states_independently_and_rolls_them_up() {
        let pane = crate::app::TmuxPane {
            id: "%3".into(),
            index: 0,
            width: 80,
            height: 24,
            active: true,
            current_command: "codex".into(),
            pid: 1,
            has_claude: false,
            claude_state: Some(HookState::Error),
            claude_activity: Some("stale".into()),
            claude_state_since: Some(1),
            claude_cwd: Some("/stale".into()),
            has_codex: true,
            codex_state: None,
            codex_activity: None,
            codex_state_since: None,
            codex_cwd: None,
        };
        let mut sessions = vec![TmuxSession {
            name: "work".into(),
            windows: vec![crate::app::TmuxWindow {
                index: 0,
                name: "editor".into(),
                panes: vec![pane],
                has_claude: false,
                claude_state: Some(HookState::Error),
                has_codex: true,
                codex_state: None,
            }],
            has_claude: false,
            claude_state: Some(HookState::Error),
            has_codex: true,
            codex_state: None,
            last_attached: 0,
            activity: 0,
            group: None,
        }];
        let codex = HashMap::from([(
            "%3".into(),
            HookInfo {
                state: HookState::Waiting,
                activity: Some("permission".into()),
                state_since: Some(10),
                cwd: Some("/repo".into()),
            },
        )]);

        apply_state_maps(&mut sessions, &HashMap::new(), &codex);

        let session = &sessions[0];
        let window = &session.windows[0];
        let pane = &window.panes[0];
        assert_eq!(pane.claude_state, None);
        assert_eq!(pane.claude_activity, None);
        assert_eq!(session.claude_state, None);
        assert_eq!(pane.codex_state, Some(HookState::Waiting));
        assert_eq!(pane.codex_activity.as_deref(), Some("permission"));
        assert_eq!(window.codex_state, Some(HookState::Waiting));
        assert_eq!(session.codex_state, Some(HookState::Waiting));
    }

    #[test]
    fn summarize_activity_digests_tool_input() {
        // Tool calls become "<tool>: <digest>"; the raw input is never echoed.
        let edit = json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": "src/app.rs", "new_string": "SECRET" }
        });
        let s = summarize_activity("PreToolUse", &edit).unwrap();
        assert_eq!(s, "Edit: src/app.rs");
        assert!(!s.contains("SECRET"));

        // Notifications surface their message; Stop carries no activity.
        let note = json!({ "message": "needs your permission" });
        assert_eq!(
            summarize_activity("Notification", &note).as_deref(),
            Some("needs your permission")
        );
        assert_eq!(summarize_activity("Stop", &json!({})), None);
    }

    #[test]
    fn merge_hooks_adds_all_events() {
        let merged = merge_hooks(json!({}), "tmux-deck hook report", CLAUDE_EVENTS).unwrap();
        let hooks = merged.get("hooks").unwrap().as_object().unwrap();
        for event in CLAUDE_EVENTS {
            let groups = hooks.get(*event).unwrap().as_array().unwrap();
            assert_eq!(groups.len(), 1, "event {event} should have one group");
            assert!(group_is_ours(&groups[0]));
        }
    }

    #[test]
    fn merge_hooks_adds_codex_events() {
        let merged = merge_hooks(
            json!({ "description": "keep me" }),
            "tmux-deck hook report --codex",
            CODEX_EVENTS,
        )
        .unwrap();
        assert_eq!(merged["description"], "keep me");
        let hooks = merged["hooks"].as_object().unwrap();
        for event in CODEX_EVENTS {
            assert_eq!(hooks[*event].as_array().unwrap().len(), 1);
        }
        assert_eq!(
            hooks["PermissionRequest"][0]["hooks"][0]["command"],
            "tmux-deck hook report --codex"
        );
    }

    #[test]
    fn settings_paths_follow_each_agents_native_layout() {
        assert_eq!(
            settings_path(true, AgentKind::Claude).unwrap(),
            PathBuf::from(".claude/settings.json")
        );
        assert_eq!(
            settings_path(true, AgentKind::Codex).unwrap(),
            PathBuf::from(".codex/hooks.json")
        );
        assert!(state_dir(AgentKind::Claude).unwrap().ends_with("claude"));
        assert!(state_dir(AgentKind::Codex).unwrap().ends_with("codex"));
        assert_eq!(
            user_settings_path(AgentKind::Claude, Path::new("/home/example"), None),
            PathBuf::from("/home/example/.claude/settings.json")
        );
        assert_eq!(
            user_settings_path(
                AgentKind::Codex,
                Path::new("/home/example"),
                Some(Path::new("/var/codex"))
            ),
            PathBuf::from("/var/codex/hooks.json")
        );
    }

    #[test]
    fn merge_hooks_is_idempotent() {
        let once = merge_hooks(json!({}), "tmux-deck hook report", CLAUDE_EVENTS).unwrap();
        let twice = merge_hooks(once.clone(), "tmux-deck hook report", CLAUDE_EVENTS).unwrap();
        assert_eq!(once, twice, "installing twice must not duplicate hooks");
    }

    #[test]
    fn merge_hooks_preserves_foreign_entries() {
        let existing = json!({
            "hooks": {
                "Stop": [
                    { "hooks": [ { "type": "command", "command": "echo other" } ] }
                ]
            },
            "permissions": { "allow": ["Bash"] }
        });
        let merged = merge_hooks(existing, "tmux-deck hook report", CLAUDE_EVENTS).unwrap();

        // Foreign top-level keys survive.
        assert!(merged.get("permissions").is_some());
        // Foreign Stop hook is kept alongside ours.
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert!(stop.iter().any(|g| !group_is_ours(g)));
        assert!(stop.iter().any(group_is_ours));
    }

    #[test]
    fn merge_hooks_preserves_foreign_handler_in_our_group() {
        let existing = json!({
            "hooks": {
                "Stop": [{
                    "matcher": "keep matcher",
                    "hooks": [
                        { "type": "command", "command": "tmux-deck hook report --old" },
                        { "type": "command", "command": "echo keep me" }
                    ]
                }]
            }
        });

        let merged = merge_hooks(existing, "tmux-deck hook report", CLAUDE_EVENTS).unwrap();
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert!(stop.iter().any(|group| {
            group["matcher"] == "keep matcher"
                && group["hooks"]
                    .as_array()
                    .is_some_and(|hooks| hooks.iter().any(|hook| hook["command"] == "echo keep me"))
        }));
    }

    #[test]
    fn merge_hooks_rejects_incompatible_existing_shapes() {
        assert!(merge_hooks(json!([]), "tmux-deck hook report", CLAUDE_EVENTS).is_err());
        assert!(
            merge_hooks(
                json!({ "hooks": [] }),
                "tmux-deck hook report",
                CLAUDE_EVENTS
            )
            .is_err()
        );
        assert!(
            merge_hooks(
                json!({ "hooks": { "Stop": {} } }),
                "tmux-deck hook report",
                CLAUDE_EVENTS,
            )
            .is_err()
        );
    }

    #[test]
    fn hook_identity_does_not_claim_unrelated_report_commands() {
        assert!(!hook_is_ours(&json!({ "command": "echo hook report" })));
        assert!(hook_is_ours(
            &json!({ "command": "/usr/bin/tmux-deck hook report" })
        ));
    }

    #[test]
    fn report_command_quotes_executable_paths_for_the_shell() {
        assert_eq!(
            shell_quote("/tmp/tmux deck's/bin"),
            "'/tmp/tmux deck'\\''s/bin'"
        );
    }
}
