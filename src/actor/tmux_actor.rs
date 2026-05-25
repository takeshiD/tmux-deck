use std::fs::OpenOptions;
use std::io::Write;

use tokio::process::Command;
use tokio::sync::mpsc;

use crate::actor::messages::{TmuxCommand, TmuxResponse};
use crate::app::{TmuxPane, TmuxSession, TmuxWindow};
use tracing::debug;

// =============================================================================
// TmuxActor
// =============================================================================

pub struct TmuxActor {
    command_rx: mpsc::Receiver<TmuxCommand>,
    response_tx: mpsc::Sender<TmuxResponse>,
}

impl TmuxActor {
    pub fn new(
        command_rx: mpsc::Receiver<TmuxCommand>,
        response_tx: mpsc::Sender<TmuxResponse>,
    ) -> Self {
        Self {
            command_rx,
            response_tx,
        }
    }

    pub async fn run(mut self) {
        while let Some(cmd) = self.command_rx.recv().await {
            let response = self.handle_command(cmd).await;
            if self.response_tx.send(response).await.is_err() {
                // UIActor has been dropped, exit
                break;
            }
        }
    }

    async fn handle_command(&self, cmd: TmuxCommand) -> TmuxResponse {
        match cmd {
            TmuxCommand::RefreshAll => {
                debug!("refresh all");
                self.refresh_all().await
            }
            TmuxCommand::CapturePane { target, start, end } => {
                debug!("capture-pane: target={target} range({start}, {end})");
                self.capture_pane(&target, start, end).await
            }
            TmuxCommand::NewSession { name } => {
                debug!("new-session");
                self.new_session(&name).await
            }
            TmuxCommand::RenameSession { old_name, new_name } => {
                debug!("rename-session");
                self.rename_session(&old_name, &new_name).await
            }
            TmuxCommand::KillSession { name } => {
                debug!("kile-session");
                self.kill_session(&name).await
            }
            TmuxCommand::SendKeys {
                target,
                keys,
                reply,
            } => {
                debug!("send keys");
                let response = self.send_keys(&target, &keys).await;
                if let Some(tx) = reply {
                    let _ = tx.send(response.clone());
                }
                response
            }
            TmuxCommand::SwitchClient { target, reply } => {
                debug!("sqitch client");
                let response = self.switch_client(&target).await;
                if let Some(tx) = reply {
                    let _ = tx.send(response.clone());
                }
                response
            }
        }
    }

    // =========================================================================
    // Refresh All Sessions
    // =========================================================================
    //
    // Single fork+exec via `\;`-chained tmux commands and `-a` flag.
    // Each output line is tagged with SESS/WIN/PANE prefix for dispatch.

    async fn refresh_all(&self) -> TmuxResponse {
        let output = Command::new("tmux")
            .args([
                "list-sessions",
                "-F",
                "SESS\t#{session_name}\t#{session_attached}\t#{session_activity}",
                ";",
                "list-windows",
                "-a",
                "-F",
                "WIN\t#{session_name}\t#{window_index}\t#{window_name}\t#{window_active}\t#{window_activity}",
                ";",
                "list-panes",
                "-a",
                "-F",
                "PANE\t#{session_name}\t#{window_index}\t#{pane_id}\t#{pane_index}\t#{pane_width}\t#{pane_height}\t#{pane_active}\t#{pane_last}\t#{pane_current_command}",
            ])
            .output()
            .await;

        let stdout = match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).to_string()
            }
            Ok(output) => {
                return TmuxResponse::Error {
                    message: String::from_utf8_lossy(&output.stderr).to_string(),
                };
            }
            Err(e) => {
                return TmuxResponse::Error {
                    message: format!("Failed to refresh: {}", e),
                };
            }
        };

        TmuxResponse::SessionsRefreshed {
            sessions: build_sessions(&stdout),
        }
    }

    // =========================================================================
    // Capture Pane
    // =========================================================================

    async fn capture_pane(&self, target: &str, start: i32, end: i32) -> TmuxResponse {
        debug!("capture-pane: target={target}, range({start}, {end})");
        let start = start.to_string();
        let end = end.to_string();
        let output = Command::new("tmux")
            .args([
                "capture-pane",
                "-e",
                "-p",
                "-J",
                "-S",
                start.as_str(),
                "-E",
                end.as_str(),
                "-t",
                target,
            ])
            .output()
            .await;

        match output {
            Ok(output) if output.status.success() => TmuxResponse::PaneCaptured {
                target: target.to_string(),
                content: String::from_utf8_lossy(&output.stdout).to_string(),
            },
            Ok(output) => TmuxResponse::Error {
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            },
            Err(e) => TmuxResponse::Error {
                message: format!("Failed to capture pane: {}", e),
            },
        }
    }

    // =========================================================================
    // Session Operations
    // =========================================================================

    async fn new_session(&self, name: &str) -> TmuxResponse {
        let output = Command::new("tmux")
            .args(["new-session", "-d", "-s", name])
            .output()
            .await;

        match output {
            Ok(output) if output.status.success() => TmuxResponse::SessionCreated {
                name: name.to_string(),
                success: true,
                error: None,
            },
            Ok(output) => TmuxResponse::SessionCreated {
                name: name.to_string(),
                success: false,
                error: Some(String::from_utf8_lossy(&output.stderr).to_string()),
            },
            Err(e) => TmuxResponse::SessionCreated {
                name: name.to_string(),
                success: false,
                error: Some(format!("Failed to create session: {}", e)),
            },
        }
    }

    async fn rename_session(&self, old_name: &str, new_name: &str) -> TmuxResponse {
        let output = Command::new("tmux")
            .args(["rename-session", "-t", old_name, new_name])
            .output()
            .await;

        match output {
            Ok(output) if output.status.success() => TmuxResponse::SessionRenamed {
                success: true,
                error: None,
            },
            Ok(output) => TmuxResponse::SessionRenamed {
                success: false,
                error: Some(String::from_utf8_lossy(&output.stderr).to_string()),
            },
            Err(e) => TmuxResponse::SessionRenamed {
                success: false,
                error: Some(format!("Failed to rename session: {}", e)),
            },
        }
    }

    async fn kill_session(&self, name: &str) -> TmuxResponse {
        let output = Command::new("tmux")
            .args(["kill-session", "-t", name])
            .output()
            .await;

        match output {
            Ok(output) if output.status.success() => TmuxResponse::SessionKilled {
                success: true,
                error: None,
            },
            Ok(output) => TmuxResponse::SessionKilled {
                success: false,
                error: Some(String::from_utf8_lossy(&output.stderr).to_string()),
            },
            Err(e) => TmuxResponse::SessionKilled {
                success: false,
                error: Some(format!("Failed to kill session: {}", e)),
            },
        }
    }

    // =========================================================================
    // Pane Operations
    // =========================================================================

    async fn send_keys(&self, target: &str, keys: &str) -> TmuxResponse {
        let output = Command::new("tmux")
            .args(["send-keys", "-t", target, keys, "Enter"])
            .output()
            .await;

        match output {
            Ok(output) if output.status.success() => TmuxResponse::KeysSent {
                success: true,
                error: None,
            },
            Ok(output) => TmuxResponse::KeysSent {
                success: false,
                error: Some(String::from_utf8_lossy(&output.stderr).to_string()),
            },
            Err(e) => TmuxResponse::KeysSent {
                success: false,
                error: Some(format!("Failed to send keys: {}", e)),
            },
        }
    }

    async fn switch_client(&self, target: &str) -> TmuxResponse {
        let log_path = "/tmp/tmux-deck.log";
        let output = Command::new("tmux")
            .args(["switch-client", "-t", target])
            .output()
            .await;
        match output {
            Ok(output) if output.status.success() => {
                append_switch_log(log_path, target, true, None);
                TmuxResponse::ClientSwitched {
                    target: target.to_string(),
                    success: true,
                    error: None,
                }
            }
            Ok(output) => {
                let err = String::from_utf8_lossy(&output.stderr).to_string();
                append_switch_log(log_path, target, false, Some(&err));
                TmuxResponse::ClientSwitched {
                    target: target.to_string(),
                    success: false,
                    error: Some(err),
                }
            }
            Err(e) => {
                let err = format!("Failed to switch client: {}", e);
                append_switch_log(log_path, target, false, Some(&err));
                TmuxResponse::ClientSwitched {
                    target: target.to_string(),
                    success: false,
                    error: Some(err),
                }
            }
        }
    }
}

struct SessionAccum {
    activity: i64,
    attached: bool,
    windows: Vec<WindowAccum>,
}

struct WindowAccum {
    activity: i64,
    active: bool,
    index: u32,
    name: String,
    /// (active, last, index, pane) — sorted then unwrapped
    panes_raw: Vec<(bool, bool, u32, TmuxPane)>,
    pane_width: u32,
    pane_height: u32,
}

fn build_sessions(stdout: &str) -> Vec<TmuxSession> {
    use std::collections::HashMap;

    let mut sessions: HashMap<String, SessionAccum> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for line in stdout.lines() {
        let mut it = line.split('\t');
        let tag = match it.next() {
            Some(t) => t,
            None => continue,
        };

        match tag {
            "SESS" => {
                let name = it.next().unwrap_or("").to_string();
                let attached = it.next() == Some("1");
                let activity = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                if name.is_empty() {
                    continue;
                }
                if !sessions.contains_key(&name) {
                    order.push(name.clone());
                }
                sessions.insert(
                    name,
                    SessionAccum {
                        activity,
                        attached,
                        windows: Vec::new(),
                    },
                );
            }
            "WIN" => {
                let session = it.next().unwrap_or("");
                let index: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let name = it.next().unwrap_or("").to_string();
                let active = it.next() == Some("1");
                let activity = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                if let Some(s) = sessions.get_mut(session) {
                    s.windows.push(WindowAccum {
                        activity,
                        active,
                        index,
                        name,
                        panes_raw: Vec::new(),
                        pane_width: 80,
                        pane_height: 24,
                    });
                }
            }
            "PANE" => {
                let session = it.next().unwrap_or("");
                let window_index: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let pane_id = it.next().unwrap_or("").to_string();
                let pane_index: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let width: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(80);
                let height: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(24);
                let active = it.next() == Some("1");
                let last = it.next() == Some("1");
                let current_command = it.next().unwrap_or("").to_string();

                if let Some(s) = sessions.get_mut(session)
                    && let Some(w) = s.windows.iter_mut().find(|w| w.index == window_index)
                {
                    if active {
                        w.pane_width = width;
                        w.pane_height = height;
                    }
                    w.panes_raw.push((
                        active,
                        last,
                        pane_index,
                        TmuxPane {
                            id: pane_id,
                            index: pane_index,
                            width,
                            height,
                            active,
                            current_command,
                        },
                    ));
                }
            }
            _ => {}
        }
    }

    for name in &order {
        if let Some(s) = sessions.get_mut(name) {
            for w in &mut s.windows {
                // (active desc, last desc, index asc)
                w.panes_raw.sort_by(|a, b| {
                    b.0.cmp(&a.0)
                        .then_with(|| b.1.cmp(&a.1))
                        .then_with(|| a.2.cmp(&b.2))
                });
            }
            s.windows.sort_by(|a, b| {
                b.activity
                    .cmp(&a.activity)
                    .then_with(|| b.active.cmp(&a.active))
                    .then_with(|| a.index.cmp(&b.index))
            });
        }
    }

    // Sort sessions by (activity desc, attached desc, name asc) — match prior behavior
    let mut keys: Vec<String> = order.into_iter().filter(|k| sessions.contains_key(k)).collect();
    keys.sort_by(|a, b| {
        let sa = &sessions[a];
        let sb = &sessions[b];
        sb.activity
            .cmp(&sa.activity)
            .then_with(|| sb.attached.cmp(&sa.attached))
            .then_with(|| a.cmp(b))
    });

    keys.into_iter()
        .filter_map(|name| {
            let s = sessions.remove(&name)?;
            let windows: Vec<TmuxWindow> = s
                .windows
                .into_iter()
                .map(|w| TmuxWindow {
                    index: w.index,
                    name: w.name,
                    active: w.active,
                    panes: w.panes_raw.into_iter().map(|(_, _, _, p)| p).collect(),
                    pane_width: w.pane_width,
                    pane_height: w.pane_height,
                })
                .collect();
            Some(TmuxSession {
                name,
                attached: s.attached,
                windows,
            })
        })
        .collect()
}

fn append_switch_log(path: &str, target: &str, success: bool, error: Option<&str>) {
    let mut file = match OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => file,
        Err(_) => return,
    };
    let error = error.unwrap_or("");
    let _ = writeln!(
        file,
        "switch-client target=\"{}\" success={} error=\"{}\"",
        target, success, error
    );
}
