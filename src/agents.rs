//! Claude Code and Codex background-session discovery.
//!
//! Claude Code hosts background sessions through a per-user supervisor and
//! persists each one's state under `$CLAUDE_CONFIG_DIR/jobs/<id>/state.json`
//! (default `~/.claude/jobs`). This module reads those files — the same source
//! `claude agents` renders — so tmux-deck can show and manage the very sessions
//! that appear in the agent view, grouped by working directory.
//!
//! This is independent of the tmux/hook integration in [`crate::hook`]: those
//! track interactive Claude running in tmux panes; the sessions here are
//! provider-managed sessions that need no terminal attached. Codex discovery
//! uses its documented app-server `thread/list` method instead of depending on
//! the layout of rollout files on disk.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentProvider {
    Claude,
    Codex,
}

impl AgentProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }

    pub fn resume_program(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn resume_subcommand(self) -> &'static str {
        match self {
            Self::Claude => "attach",
            Self::Codex => "resume",
        }
    }
}

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
    pub provider: AgentProvider,
    /// Provider-native id used by `claude attach` or `codex resume`.
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
    /// Path to the conversation transcript JSONL, if known (for preview/summary).
    pub transcript_path: Option<String>,
}

impl AgentSession {
    pub fn cache_key(&self) -> String {
        format!("{}:{}", self.provider.label().to_ascii_lowercase(), self.id)
    }
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

        let transcript_path = v
            .get("linkScanPath")
            .and_then(|s| s.as_str())
            .map(String::from);

        sessions.push(AgentSession {
            provider: AgentProvider::Claude,
            alive: alive.contains(&id),
            id,
            name,
            state,
            summary: one_line(summary_raw, 80),
            cwd,
            elapsed_secs,
            prs,
            transcript_path,
        });
    }

    sort_for_display(&mut sessions);
    sessions
}

/// Load Codex interactive threads through the documented app-server protocol.
/// The process is short-lived and bounded; unsupported or unavailable Codex
/// installations simply contribute no sessions.
pub async fn load_codex_sessions() -> Vec<AgentSession> {
    let mut child = match Command::new("codex")
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Vec::new(),
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill().await;
        return Vec::new();
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill().await;
        return Vec::new();
    };
    let requests = [
        serde_json::json!({
            "method": "initialize",
            "id": 0,
            "params": { "clientInfo": {
                "name": "tmux_deck",
                "title": "tmux-deck",
                "version": env!("CARGO_PKG_VERSION")
            }}
        }),
        serde_json::json!({ "method": "initialized", "params": {} }),
        serde_json::json!({
            "method": "thread/list",
            "id": 1,
            "params": {
                "limit": 100,
                "useStateDbOnly": true,
                "sortKey": "updated_at",
                "sortDirection": "desc",
                "sourceKinds": [
                    "cli", "vscode", "subAgent", "subAgentReview", "subAgentCompact",
                    "subAgentThreadSpawn", "subAgentOther"
                ]
            }
        }),
    ]
    .into_iter()
    .map(|request| request.to_string())
    .collect::<Vec<_>>()
    .join("\n")
        + "\n";
    if stdin.write_all(requests.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
        let _ = child.kill().await;
        return Vec::new();
    }

    let mut lines = BufReader::new(stdout).lines();
    let response = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value.get("id").and_then(Value::as_i64) == Some(1) {
                return Some(value);
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    let _ = child.kill().await;
    response
        .map(|value| parse_codex_thread_list(&value, now_secs()))
        .unwrap_or_default()
}

fn parse_codex_thread_list(response: &Value, now: i64) -> Vec<AgentSession> {
    let mut sessions = Vec::new();
    let Some(threads) = response
        .get("result")
        .and_then(|result| result.get("data"))
        .and_then(Value::as_array)
    else {
        return sessions;
    };
    for thread in threads {
        let Some(id) = thread.get("id").and_then(Value::as_str) else {
            continue;
        };
        let preview = thread
            .get("preview")
            .and_then(Value::as_str)
            .unwrap_or("");
        let name = thread
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or(if preview.is_empty() { id } else { preview });
        let status = thread
            .get("status")
            .and_then(|status| status.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("notLoaded");
        let waiting = thread
            .get("status")
            .and_then(|status| status.get("activeFlags"))
            .and_then(Value::as_array)
            .is_some_and(|flags| {
                flags
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|flag| flag == "waitingOnApproval" || flag == "waitingOnUserInput")
            });
        let state = match (status, waiting) {
            ("active", true) => AgentState::Blocked,
            ("active", false) => AgentState::Working,
            ("idle", _) => AgentState::Idle,
            ("systemError", _) => AgentState::Failed,
            _ => AgentState::Done,
        };
        let updated_at = thread
            .get("updatedAt")
            .or_else(|| thread.get("createdAt"))
            .and_then(Value::as_i64)
            .unwrap_or(now);
        sessions.push(AgentSession {
            provider: AgentProvider::Codex,
            id: id.to_string(),
            name: one_line(name, 80),
            state,
            summary: one_line(preview, 80),
            cwd: thread
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("Codex")
                .to_string(),
            elapsed_secs: now.saturating_sub(updated_at).max(0),
            prs: Vec::new(),
            alive: matches!(status, "active" | "idle"),
            transcript_path: None,
        });
    }
    sessions
}

pub async fn load_background_sessions() -> Vec<AgentSession> {
    let claude = tokio::task::spawn_blocking(load_agent_sessions);
    let codex = load_codex_sessions();
    let (claude, codex) = tokio::join!(claude, codex);
    let mut sessions = claude.unwrap_or_default();
    sessions.extend(codex);
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

/// Extract human-readable lines from one transcript JSONL record. Returns
/// labelled lines for user prompts, assistant text and tool calls; `thinking`
/// blocks, snapshots and other internal records yield nothing.
fn transcript_record_lines(v: &Value) -> Vec<String> {
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if ty != "user" && ty != "assistant" {
        return Vec::new();
    }
    let role = v
        .get("message")
        .and_then(|m| m.get("role"))
        .and_then(|r| r.as_str())
        .unwrap_or(ty);
    let content = match v.get("message").and_then(|m| m.get("content")) {
        Some(c) => c,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    // Two spaces after the marker: the geometric/emoji glyphs are East Asian
    // *Ambiguous* width, so some terminals render them 2 cells wide while
    // unicode-width counts 1. The extra space keeps text from overlapping the
    // icon in those terminals.
    let mut push = |prefix: &str, text: String| {
        let t = one_line(&text, 200);
        if !t.is_empty() {
            out.push(format!("{prefix}  {t}"));
        }
    };

    match content {
        // User prompts are often a bare string.
        Value::String(s) => push("▸", s.clone()),
        Value::Array(blocks) => {
            for b in blocks {
                match b.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                            let prefix = if role == "user" { "▸" } else { "●" };
                            push(prefix, t.to_string());
                        }
                    }
                    Some("tool_use") => {
                        let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                        push("⏺", name.to_string());
                    }
                    // Skip thinking, tool_result, images, etc.
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

/// Read the tail of a transcript as readable lines (latest last), for the
/// preview panel. Reads the whole file (transcripts are local and modest);
/// returns at most `max_lines` formatted lines.
pub fn transcript_tail(path: &str, max_lines: usize) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return vec!["(transcript unavailable)".to_string()];
    };
    let mut lines: Vec<String> = Vec::new();
    for raw in content.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(raw) {
            lines.extend(transcript_record_lines(&v));
        }
    }
    if lines.len() > max_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }
    lines
}

/// Build a compact text digest of a transcript for the summary prompt, capped
/// at roughly `max_chars` characters (keeping the most recent content).
pub fn transcript_digest(path: &str, max_chars: usize) -> String {
    let mut lines = transcript_tail(path, 400);
    let mut digest = lines.join("\n");
    while digest.chars().count() > max_chars && !lines.is_empty() {
        lines.remove(0);
        digest = lines.join("\n");
    }
    digest
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
            provider: AgentProvider::Claude,
            id: format!("{cwd}-{elapsed}"),
            name: "n".into(),
            state,
            summary: String::new(),
            cwd: cwd.into(),
            elapsed_secs: elapsed,
            prs: Vec::new(),
            alive: true,
            transcript_path: None,
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

    #[test]
    fn parses_codex_threads_and_runtime_status() {
        let response = serde_json::json!({
            "id": 1,
            "result": { "data": [
                {
                    "id": "thr_wait",
                    "preview": "permission needed",
                    "cwd": "/work/repo",
                    "updatedAt": 90,
                    "status": { "type": "active", "activeFlags": ["waitingOnApproval"] }
                },
                {
                    "id": "thr_done",
                    "name": "finished task",
                    "createdAt": 80,
                    "status": { "type": "notLoaded" }
                }
            ] }
        });
        let sessions = parse_codex_thread_list(&response, 100);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].provider, AgentProvider::Codex);
        assert_eq!(sessions[0].state, AgentState::Blocked);
        assert!(sessions[0].alive);
        assert_eq!(sessions[0].elapsed_secs, 10);
        assert_eq!(sessions[1].state, AgentState::Done);
        assert!(!sessions[1].alive);
        assert_eq!(AgentProvider::Claude.resume_subcommand(), "attach");
        assert_eq!(AgentProvider::Codex.resume_subcommand(), "resume");
    }
}
