//! Claude Code hook integration.
//!
//! Two halves live here:
//!
//! * The **reporter** (`tmux-deck hook report`) is wired into Claude Code's
//!   `settings.json`. Claude runs it on each hook event, passing the hook JSON
//!   on stdin. It records the *calling pane's* Claude state to a small file
//!   keyed by `$TMUX_PANE`.
//! * The **reader** ([`apply_states`]) is used by the TUI to fold those files
//!   back into the session tree so each pane/window/session can show a marker
//!   reflecting what Claude is doing.
//!
//! The two sides are linked purely by `$TMUX_PANE`: the reporter inherits it
//! from the pane Claude runs in, and tmux exposes the same id as `#{pane_id}`.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde_json::{Value, json};

use crate::app::{ClaudeState, TmuxSession};

/// Hook events we install and listen for. `SessionEnd` is included so a pane's
/// marker is cleared when Claude exits.
const MANAGED_EVENTS: &[&str] = &[
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "SubagentStop",
    "SessionEnd",
];

/// Drop state files older than this. A pane that closes without firing
/// `SessionEnd` (e.g. killed) would otherwise leave a stale marker forever.
const STALE_SECS: i64 = 6 * 60 * 60;

/// Substring that identifies a hook command as ours, for idempotent install.
const COMMAND_MARKER: &str = "hook report";

// =============================================================================
// Paths / time helpers
// =============================================================================

/// Directory holding per-pane Claude state files.
///
/// Resolves to `$XDG_STATE_HOME/tmux-deck/claude` (the `directories` crate
/// honours `XDG_STATE_HOME` on Linux), falling back to `~/.local/state/...`
/// on platforms where a state dir is not otherwise defined.
fn state_dir() -> Option<PathBuf> {
    let base = ProjectDirs::from("dev", "tkcd", "tmux-deck")
        .and_then(|p| p.state_dir().map(|s| s.to_path_buf()))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/tmux-deck"))
        })?;
    Some(base.join("claude"))
}

fn now_secs() -> i64 {
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

// =============================================================================
// Reporter: `tmux-deck hook report`
// =============================================================================

/// Entry point for `tmux-deck hook report`.
///
/// Always exits quietly (the caller — Claude — should never be disrupted by a
/// hook), so every failure path is a silent early return.
pub fn run_report() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);

    // Without a pane id we cannot attribute the event to anything.
    let pane = match std::env::var("TMUX_PANE") {
        Ok(p) if !p.is_empty() => p,
        _ => return,
    };

    let event = serde_json::from_str::<Value>(&input)
        .ok()
        .and_then(|v| {
            v.get("hook_event_name")
                .and_then(|e| e.as_str())
                .map(String::from)
        });
    let event = match event {
        Some(e) => e,
        None => return,
    };

    let dir = match state_dir() {
        Some(d) => d,
        None => return,
    };
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("{}.json", pane_file_stem(&pane)));

    match ClaudeState::from_hook_event(&event) {
        Some(state) => {
            let record = json!({
                "pane": pane,
                "state": state.as_token(),
                "event": event,
                "ts": now_secs(),
            });
            let _ = std::fs::write(&file, record.to_string());
        }
        None => {
            // SessionEnd (and any other terminal/unmapped event) clears the
            // marker so a finished pane stops showing a stale state.
            if event == "SessionEnd" {
                let _ = std::fs::remove_file(&file);
            }
        }
    }
}

// =============================================================================
// Reader: fold state files into the session tree
// =============================================================================

/// Load the current per-pane states, keyed by tmux pane id (`%N`).
/// Stale files are removed as a side effect.
fn load_states() -> HashMap<String, ClaudeState> {
    let mut map = HashMap::new();
    let dir = match state_dir() {
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
        let state = match value
            .get("state")
            .and_then(|s| s.as_str())
            .and_then(ClaudeState::from_token)
        {
            Some(s) => s,
            None => continue,
        };
        map.insert(pane, state);
    }
    map
}

/// Apply the current hook states to a session tree, recomputing the
/// per-pane / per-window / per-session markers. Always recomputes from the
/// files on disk, so a marker that has gone away is cleared too.
pub fn apply_states(sessions: &mut [TmuxSession]) {
    let map = load_states();
    for session in sessions.iter_mut() {
        let mut session_state = None;
        for window in session.windows.iter_mut() {
            let mut window_state = None;
            for pane in window.panes.iter_mut() {
                pane.claude_state = map.get(&pane.id).copied();
                window_state = ClaudeState::merge(window_state, pane.claude_state);
            }
            window.claude_state = window_state;
            session_state = ClaudeState::merge(session_state, window_state);
        }
        session.claude_state = session_state;
    }
}

// =============================================================================
// Installer: `tmux-deck hook install`
// =============================================================================

/// Entry point for `tmux-deck hook install [--project]`.
pub fn run_install(project: bool) -> color_eyre::Result<()> {
    let path = settings_path(project)?;
    let command = report_command();

    let existing = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str::<Value>(&s)?,
        _ => json!({}),
    };
    let merged = merge_hooks(existing, &command);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = serde_json::to_string_pretty(&merged)?;
    out.push('\n');
    std::fs::write(&path, out)?;

    println!("Installed tmux-deck Claude hooks into {}", path.display());
    println!("Events: {}", MANAGED_EVENTS.join(", "));
    Ok(())
}

/// The command Claude should run for each event. Uses the absolute path to the
/// current executable so it works regardless of `$PATH`.
fn report_command() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "tmux-deck".to_string());
    format!("{} hook report", exe)
}

fn settings_path(project: bool) -> color_eyre::Result<PathBuf> {
    if project {
        Ok(PathBuf::from(".claude").join("settings.json"))
    } else {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| color_eyre::eyre::eyre!("HOME is not set"))?;
        Ok(home.join(".claude").join("settings.json"))
    }
}

/// Merge our managed hooks into an existing settings document, idempotently.
///
/// Any previously-installed tmux-deck report hook is removed first, so running
/// install repeatedly never duplicates entries and always refreshes the path.
fn merge_hooks(mut root: Value, command: &str) -> Value {
    if !root.is_object() {
        root = json!({});
    }
    let obj = root.as_object_mut().expect("root is an object");

    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks = hooks.as_object_mut().expect("hooks is an object");

    for event in MANAGED_EVENTS {
        let entry = hooks.entry((*event).to_string()).or_insert_with(|| json!([]));
        if !entry.is_array() {
            *entry = json!([]);
        }
        let groups = entry.as_array_mut().expect("event is an array");
        groups.retain(|group| !group_is_ours(group));
        groups.push(json!({
            "hooks": [ { "type": "command", "command": command } ]
        }));
    }
    root
}

/// Whether a hook group was installed by us (contains a `hook report` command).
fn group_is_ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains(COMMAND_MARKER))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
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
            ClaudeState::from_hook_event("UserPromptSubmit"),
            Some(ClaudeState::Working)
        );
        assert_eq!(
            ClaudeState::from_hook_event("Notification"),
            Some(ClaudeState::Waiting)
        );
        assert_eq!(ClaudeState::from_hook_event("Stop"), Some(ClaudeState::Done));
        assert_eq!(ClaudeState::from_hook_event("SessionEnd"), None);
        assert_eq!(ClaudeState::from_hook_event("Whatever"), None);
    }

    #[test]
    fn token_roundtrips() {
        for s in [
            ClaudeState::Working,
            ClaudeState::Waiting,
            ClaudeState::Done,
            ClaudeState::Error,
        ] {
            assert_eq!(ClaudeState::from_token(s.as_token()), Some(s));
        }
    }

    #[test]
    fn merge_keeps_higher_priority() {
        // Waiting (3) beats Working (1); Done (0) loses to everything.
        assert_eq!(
            ClaudeState::merge(Some(ClaudeState::Working), Some(ClaudeState::Waiting)),
            Some(ClaudeState::Waiting)
        );
        assert_eq!(
            ClaudeState::merge(Some(ClaudeState::Done), Some(ClaudeState::Working)),
            Some(ClaudeState::Working)
        );
        assert_eq!(
            ClaudeState::merge(None, Some(ClaudeState::Done)),
            Some(ClaudeState::Done)
        );
        assert_eq!(ClaudeState::merge(None, None), None);
    }

    #[test]
    fn pane_file_stem_is_safe() {
        assert_eq!(pane_file_stem("%3"), "_3");
        assert_eq!(pane_file_stem("%12"), "_12");
    }

    #[test]
    fn merge_hooks_adds_all_events() {
        let merged = merge_hooks(json!({}), "tmux-deck hook report");
        let hooks = merged.get("hooks").unwrap().as_object().unwrap();
        for event in MANAGED_EVENTS {
            let groups = hooks.get(*event).unwrap().as_array().unwrap();
            assert_eq!(groups.len(), 1, "event {event} should have one group");
            assert!(group_is_ours(&groups[0]));
        }
    }

    #[test]
    fn merge_hooks_is_idempotent() {
        let once = merge_hooks(json!({}), "tmux-deck hook report");
        let twice = merge_hooks(once.clone(), "tmux-deck hook report");
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
        let merged = merge_hooks(existing, "tmux-deck hook report");

        // Foreign top-level keys survive.
        assert!(merged.get("permissions").is_some());
        // Foreign Stop hook is kept alongside ours.
        let stop = merged["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert!(stop.iter().any(|g| !group_is_ours(g)));
        assert!(stop.iter().any(group_is_ours));
    }
}
