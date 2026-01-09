use tokio::process::Command;
use tokio::sync::mpsc;

use crate::actor::messages::{TmuxCommand, TmuxResponse};
use crate::app::{TmuxPane, TmuxSession, TmuxWindow};

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
            TmuxCommand::RefreshAll => self.refresh_all().await,
            TmuxCommand::CapturePane { target } => self.capture_pane(&target).await,
            TmuxCommand::NewSession { name } => self.new_session(&name).await,
            TmuxCommand::RenameSession { old_name, new_name } => {
                self.rename_session(&old_name, &new_name).await
            }
            TmuxCommand::KillSession { name } => self.kill_session(&name).await,
            TmuxCommand::SendKeys { target, keys } => self.send_keys(&target, &keys).await,
            TmuxCommand::SwitchClient { target } => self.switch_client(&target).await,
        }
    }

    // =========================================================================
    // Refresh All Sessions
    // =========================================================================

    async fn refresh_all(&self) -> TmuxResponse {
        let output = Command::new("tmux")
            .args([
                "list-sessions",
                "-F",
                "#{session_name}:#{session_attached}",
            ])
            .output()
            .await;

        let sessions_str = match output {
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
                    message: format!("Failed to list sessions: {}", e),
                };
            }
        };

        let mut sessions = Vec::new();

        for session_line in sessions_str.lines() {
            let parts: Vec<&str> = session_line.split(':').collect();
            if parts.len() >= 2 {
                let session_name = parts[0].to_string();
                let attached = parts[1] == "1";

                let windows = self.get_windows(&session_name).await;

                sessions.push(TmuxSession {
                    name: session_name,
                    attached,
                    windows,
                });
            }
        }

        TmuxResponse::SessionsRefreshed { sessions }
    }

    async fn get_windows(&self, session_name: &str) -> Vec<TmuxWindow> {
        let output = Command::new("tmux")
            .args([
                "list-windows",
                "-t",
                session_name,
                "-F",
                "#{window_index}:#{window_name}:#{window_active}",
            ])
            .output()
            .await;

        let windows_str = match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).to_string()
            }
            _ => return Vec::new(),
        };

        let mut windows = Vec::new();

        for window_line in windows_str.lines() {
            let w_parts: Vec<&str> = window_line.split(':').collect();
            if w_parts.len() >= 3 {
                let window_index: u32 = w_parts[0].parse().unwrap_or(0);
                let window_name = w_parts[1].to_string();
                let window_active = w_parts[2] == "1";

                let (panes, pane_width, pane_height) =
                    self.get_panes(session_name, window_index).await;

                let content = self
                    .capture_window_content(session_name, window_index)
                    .await;

                windows.push(TmuxWindow {
                    index: window_index,
                    name: window_name,
                    active: window_active,
                    panes,
                    content,
                    pane_width,
                    pane_height,
                });
            }
        }

        windows
    }

    async fn get_panes(&self, session_name: &str, window_index: u32) -> (Vec<TmuxPane>, u32, u32) {
        let target = format!("{}:{}", session_name, window_index);
        let output = Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                &target,
                "-F",
                "#{pane_id}:#{pane_index}:#{pane_width}:#{pane_height}:#{pane_active}:#{pane_current_command}",
            ])
            .output()
            .await;

        let panes_str = match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).to_string()
            }
            _ => return (Vec::new(), 80, 24),
        };

        let mut panes = Vec::new();
        let mut active_width = 80u32;
        let mut active_height = 24u32;

        for pane_line in panes_str.lines() {
            let p_parts: Vec<&str> = pane_line.split(':').collect();
            if p_parts.len() >= 6 {
                let pane_id = p_parts[0].to_string();
                let pane_index: u32 = p_parts[1].parse().unwrap_or(0);
                let width: u32 = p_parts[2].parse().unwrap_or(80);
                let height: u32 = p_parts[3].parse().unwrap_or(24);
                let pane_active = p_parts[4] == "1";
                let current_command = p_parts[5].to_string();

                if pane_active {
                    active_width = width;
                    active_height = height;
                }

                panes.push(TmuxPane {
                    id: pane_id,
                    index: pane_index,
                    width,
                    height,
                    active: pane_active,
                    current_command,
                });
            }
        }

        (panes, active_width, active_height)
    }

    async fn capture_window_content(&self, session_name: &str, window_index: u32) -> String {
        let target = format!("{}:{}", session_name, window_index);
        let output = Command::new("tmux")
            .args(["capture-pane", "-e", "-p", "-J", "-t", &target])
            .output()
            .await;

        match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).to_string()
            }
            _ => String::new(),
        }
    }

    // =========================================================================
    // Capture Pane
    // =========================================================================

    async fn capture_pane(&self, target: &str) -> TmuxResponse {
        let output = Command::new("tmux")
            .args(["capture-pane", "-e", "-p", "-J", "-t", target])
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
        let output = Command::new("tmux")
            .args(["switch-client", "-t", target])
            .output()
            .await;

        match output {
            Ok(output) => TmuxResponse::ClientSwitched {
                success: output.status.success(),
            },
            Err(_) => TmuxResponse::ClientSwitched { success: false },
        }
    }
}
