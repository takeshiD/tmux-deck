//! Claude Code background-session ("agents view") integration.
//!
//! Claude Code hosts background sessions through a per-user supervisor and
//! persists each one's state under `$CLAUDE_CONFIG_DIR/jobs/<id>/state.json`
//! (default `~/.claude/jobs`). This module reads those files — the same source
//! `claude agents` renders — so tmux-deck can show and manage the very sessions
//! that appear in the agent view, grouped by working directory.
//!
//! This is independent of the tmux/hook integration in [`crate::hook`]: those
//! track interactive Claude running in tmux panes; the sessions here are
//! supervisor-hosted background sessions that need no terminal attached.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Lifecycle state of a background session, mirroring the agent view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Waiting on the user (a question or permission decision).
    Blocked,
    /// Actively generating or running tools.
    Working,
    /// Nothing to do, ready for the next prompt.
    Idle,
    /// Task finished successfully.
    Done,
    /// Ended with an error.
    Failed,
    /// Stopped by the user.
    Stopped,
    /// Anything not recognised.
    Unknown,
}

impl AgentState {
    fn parse(s: &str) -> Self {
        match s {
            "blocked" => Self::Blocked,
            "working" => Self::Working,
            "idle" => Self::Idle,
            "done" | "completed" => Self::Done,
            "failed" | "error" => Self::Failed,
            "stopped" => Self::Stopped,
            _ => Self::Unknown,
        }
    }

    /// Attention group the session is shown under, like the agent view's
    /// "Needs input / Working / Completed" sections.
    pub fn group(self) -> AgentGroup {
        match self {
            Self::Blocked => AgentGroup::NeedsInput,
            Self::Working | Self::Idle | Self::Unknown => AgentGroup::Working,
            Self::Done | Self::Failed | Self::Stopped => AgentGroup::Completed,
        }
    }

}

/// Attention grouping for the list (highest attention first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentGroup {
    NeedsInput,
    Working,
    Completed,
}

/// A pull request the session opened.
#[derive(Debug, Clone)]
pub struct PrRef {
    pub id: String,
}

/// One background session as shown in the agent view.
#[derive(Debug, Clone)]
pub struct AgentSession {
    /// Short id (the `jobs/<id>` directory name) used by `claude attach <id>`.
    pub id: String,
    pub name: String,
    pub state: AgentState,
    /// One-line summary: the pending question when blocked, else the latest detail.
    pub summary: String,
    /// Working directory the session runs in (used to group the list).
    pub cwd: String,
    /// Seconds since the session state last changed (state.json mtime).
    pub elapsed_secs: i64,
    pub prs: Vec<PrRef>,
    /// True while the supervisor has a live worker process for this session.
    pub alive: bool,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Root Claude config dir, honouring `CLAUDE_CONFIG_DIR` like Claude Code does.
fn claude_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude"))
}

/// Ids of sessions the supervisor currently has a live worker for.
fn alive_ids(config_dir: &std::path::Path) -> std::collections::HashSet<String> {
    let path = config_dir.join("daemon").join("roster.json");
    let mut set = std::collections::HashSet::new();
    if let Ok(content) = std::fs::read_to_string(&path)
        && let Ok(v) = serde_json::from_str::<Value>(&content)
        && let Some(workers) = v.get("workers").and_then(|w| w.as_object())
    {
        for id in workers.keys() {
            set.insert(id.clone());
        }
    }
    set
}

fn one_line(s: &str, max: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > max {
        let head: String = collapsed.chars().take(max - 1).collect();
        format!("{head}…")
    } else {
        collapsed
    }
}

/// Load all background sessions from `~/.claude/jobs/*/state.json`, sorted for
/// display: grouped by working directory (the directory with the most-pressing
/// / most-recent session first), and within a directory by attention group
/// then recency. Returns an empty list when no sessions exist or the directory
/// is unreadable — never errors, so it can't break the TUI.
pub fn load_agent_sessions() -> Vec<AgentSession> {
    let Some(config_dir) = claude_config_dir() else {
        return Vec::new();
    };
    let jobs_dir = config_dir.join("jobs");
    let alive = alive_ids(&config_dir);
    let now = now_secs();

    let mut sessions = Vec::new();
    let Ok(entries) = std::fs::read_dir(&jobs_dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().to_string();
        let state_path = entry.path().join("state.json");
        let Ok(content) = std::fs::read_to_string(&state_path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&content) else {
            continue;
        };

        let state = AgentState::parse(v.get("state").and_then(|s| s.as_str()).unwrap_or(""));
        let name = v
            .get("name")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&id)
            .to_string();
        // Prefer the pending question, fall back to the latest detail.
        let summary_raw = v
            .get("needs")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| v.get("detail").and_then(|s| s.as_str()))
            .unwrap_or("");
        let cwd = v
            .get("cwd")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let prs = v
            .get("children")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|c| c.get("kind").and_then(|k| k.as_str()) == Some("pr"))
                    .filter_map(|c| c.get("id").and_then(|i| i.as_str()))
                    .map(|id| PrRef { id: id.to_string() })
                    .collect()
            })
            .unwrap_or_default();
        // Use the state file's mtime as "last changed", avoiding date parsing.
        let elapsed_secs = std::fs::metadata(&state_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| (now - d.as_secs() as i64).max(0))
            .unwrap_or(0);

        sessions.push(AgentSession {
            alive: alive.contains(&id),
            id,
            name,
            state,
            summary: one_line(summary_raw, 80),
            cwd,
            elapsed_secs,
            prs,
        });
    }

    sort_for_display(&mut sessions);
    sessions
}

/// Attention rank within a directory: needy first, then recency.
fn attention_key(s: &AgentSession) -> (u8, i64) {
    let group = match s.state.group() {
        AgentGroup::NeedsInput => 0,
        AgentGroup::Working => 1,
        AgentGroup::Completed => 2,
    };
    (group, s.elapsed_secs)
}

/// Sort sessions so they render grouped by directory. Directories are ordered
/// by their most-pressing session; within a directory, by attention then
/// recency. This keeps the flat `Vec` order identical to the rendered order so
/// a selection index maps directly onto it.
fn sort_for_display(sessions: &mut [AgentSession]) {
    use std::collections::HashMap;
    // Rank each directory by its best (smallest) attention key.
    let mut dir_rank: HashMap<String, (u8, i64)> = HashMap::new();
    for s in sessions.iter() {
        let key = attention_key(s);
        dir_rank
            .entry(s.cwd.clone())
            .and_modify(|best| {
                if key < *best {
                    *best = key;
                }
            })
            .or_insert(key);
    }
    sessions.sort_by(|a, b| {
        let ra = dir_rank.get(&a.cwd).copied().unwrap_or((u8::MAX, i64::MAX));
        let rb = dir_rank.get(&b.cwd).copied().unwrap_or((u8::MAX, i64::MAX));
        ra.cmp(&rb)
            .then_with(|| a.cwd.cmp(&b.cwd))
            .then_with(|| attention_key(a).cmp(&attention_key(b)))
    });
}

/// Abbreviate a path for a group header, replacing `$HOME` with `~`.
pub fn abbreviate_path(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(cwd: &str, state: AgentState, elapsed: i64) -> AgentSession {
        AgentSession {
            id: format!("{cwd}-{elapsed}"),
            name: "n".into(),
            state,
            summary: String::new(),
            cwd: cwd.into(),
            elapsed_secs: elapsed,
            prs: Vec::new(),
            alive: true,
        }
    }

    #[test]
    fn state_parse_and_grouping() {
        assert_eq!(AgentState::parse("blocked"), AgentState::Blocked);
        assert_eq!(AgentState::parse("done").group(), AgentGroup::Completed);
        assert_eq!(AgentState::parse("working").group(), AgentGroup::Working);
        assert_eq!(AgentState::parse("???"), AgentState::Unknown);
    }

    #[test]
    fn sort_groups_by_directory_with_needy_dir_first() {
        // dir "b" has a blocked session (needs input) so it should come before
        // dir "a" whose sessions are only working/done. Within a dir, the
        // needier/most-recent session leads.
        let mut v = vec![
            sess("a", AgentState::Done, 10),
            sess("b", AgentState::Working, 5),
            sess("a", AgentState::Working, 20),
            sess("b", AgentState::Blocked, 99),
        ];
        sort_for_display(&mut v);
        let order: Vec<(&str, AgentState)> = v.iter().map(|s| (s.cwd.as_str(), s.state)).collect();
        assert_eq!(
            order,
            vec![
                ("b", AgentState::Blocked),
                ("b", AgentState::Working),
                ("a", AgentState::Working),
                ("a", AgentState::Done),
            ]
        );
    }

    #[test]
    fn abbreviate_path_replaces_home() {
        // SAFETY: single-threaded test process.
        unsafe { std::env::set_var("HOME", "/home/u") };
        assert_eq!(abbreviate_path("/home/u/proj"), "~/proj");
        assert_eq!(abbreviate_path("/etc/x"), "/etc/x");
    }
}
