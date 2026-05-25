use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::actor::messages::{TmuxCommand, TmuxResponse};
use crate::app::{TmuxPane, TmuxSession, TmuxWindow};

// =============================================================================
// TmuxActor — control-mode based, with fork+exec fallback
// =============================================================================
//
// A single persistent `tmux -C attach` process owns stdin/stdout. Each tmux
// operation writes one command line and consumes one `%begin .. %end`
// (or `%error`) block. Asynchronous notifications between blocks are skipped
// (Step 5 will route them as events).
//
// If the control-mode process is missing or dies, the actor falls back to
// per-operation fork+exec and retries connecting before subsequent calls.

pub struct TmuxActor {
    command_rx: mpsc::Receiver<TmuxCommand>,
    capture_rx: mpsc::Receiver<TmuxCommand>,
    response_tx: mpsc::Sender<TmuxResponse>,
    ctrl: Option<ControlMode>,
}

struct ControlMode {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl TmuxActor {
    pub fn new(
        command_rx: mpsc::Receiver<TmuxCommand>,
        capture_rx: mpsc::Receiver<TmuxCommand>,
        response_tx: mpsc::Sender<TmuxResponse>,
    ) -> Self {
        Self {
            command_rx,
            capture_rx,
            response_tx,
            ctrl: None,
        }
    }

    pub async fn run(mut self) {
        // Try to connect control mode eagerly so the first refresh is fast.
        self.ctrl = Self::try_connect_control().await;

        loop {
            let cmd = tokio::select! {
                biased;
                Some(c) = self.command_rx.recv() => c,
                Some(c) = self.capture_rx.recv() => c,
                else => break,
            };
            let response = self.handle_command(cmd).await;
            if self.response_tx.send(response).await.is_err() {
                break;
            }
        }

        // Best-effort shutdown of the control-mode child.
        if let Some(mut ctrl) = self.ctrl.take() {
            let _ = ctrl.child.kill().await;
        }
    }

    async fn handle_command(&mut self, cmd: TmuxCommand) -> TmuxResponse {
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
                debug!("kill-session");
                self.kill_session(&name).await
            }
            TmuxCommand::SendKeys {
                target,
                keys,
                reply,
            } => {
                debug!("send-keys");
                let response = self.send_keys(&target, &keys).await;
                if let Some(tx) = reply {
                    let _ = tx.send(response.clone());
                }
                response
            }
            TmuxCommand::SwitchClient { target, reply } => {
                debug!("switch-client");
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

    async fn refresh_all(&mut self) -> TmuxResponse {
        // Three commands; outputs prefixed so they can be concatenated.
        let s_args: &[&str] = &[
            "list-sessions",
            "-F",
            "SESS\t#{session_name}\t#{session_attached}\t#{session_activity}",
        ];
        let w_args: &[&str] = &[
            "list-windows",
            "-a",
            "-F",
            "WIN\t#{session_name}\t#{window_index}\t#{window_name}\t#{window_active}\t#{window_activity}",
        ];
        let p_args: &[&str] = &[
            "list-panes",
            "-a",
            "-F",
            "PANE\t#{session_name}\t#{window_index}\t#{pane_id}\t#{pane_index}\t#{pane_width}\t#{pane_height}\t#{pane_active}\t#{pane_last}\t#{pane_current_command}",
        ];

        // If control mode is up, send 3 commands as 3 blocks; otherwise one
        // fork+exec with `;` chaining.
        let stdout = if self.ctrl.is_some() {
            let mut buf = String::new();
            for args in [s_args, w_args, p_args] {
                match self.exec_args(args).await {
                    Ok(out) => {
                        buf.push_str(&out);
                        if !out.ends_with('\n') {
                            buf.push('\n');
                        }
                    }
                    Err(e) => {
                        return TmuxResponse::Error { message: e };
                    }
                }
            }
            buf
        } else {
            // Single fork+exec with `;` chaining
            let mut chained: Vec<&str> = Vec::with_capacity(s_args.len() + w_args.len() + p_args.len() + 2);
            chained.extend_from_slice(s_args);
            chained.push(";");
            chained.extend_from_slice(w_args);
            chained.push(";");
            chained.extend_from_slice(p_args);
            match Self::fork_exec(&chained).await {
                Ok(out) => out,
                Err(e) => return TmuxResponse::Error { message: e },
            }
        };

        TmuxResponse::SessionsRefreshed {
            sessions: build_sessions(&stdout),
        }
    }

    // =========================================================================
    // Capture Pane
    // =========================================================================

    async fn capture_pane(&mut self, target: &str, start: i32, end: i32) -> TmuxResponse {
        let start = start.to_string();
        let end = end.to_string();
        let args: &[&str] = &[
            "capture-pane", "-e", "-p", "-J", "-S", &start, "-E", &end, "-t", target,
        ];
        match self.exec_args(args).await {
            Ok(out) => TmuxResponse::PaneCaptured {
                target: target.to_string(),
                content: out,
            },
            Err(e) => TmuxResponse::Error { message: e },
        }
    }

    // =========================================================================
    // Session Operations
    // =========================================================================

    async fn new_session(&mut self, name: &str) -> TmuxResponse {
        let args: &[&str] = &["new-session", "-d", "-s", name];
        match self.exec_args(args).await {
            Ok(_) => TmuxResponse::SessionCreated {
                name: name.to_string(),
                success: true,
                error: None,
            },
            Err(e) => TmuxResponse::SessionCreated {
                name: name.to_string(),
                success: false,
                error: Some(e),
            },
        }
    }

    async fn rename_session(&mut self, old_name: &str, new_name: &str) -> TmuxResponse {
        let args: &[&str] = &["rename-session", "-t", old_name, new_name];
        match self.exec_args(args).await {
            Ok(_) => TmuxResponse::SessionRenamed {
                success: true,
                error: None,
            },
            Err(e) => TmuxResponse::SessionRenamed {
                success: false,
                error: Some(e),
            },
        }
    }

    async fn kill_session(&mut self, name: &str) -> TmuxResponse {
        let args: &[&str] = &["kill-session", "-t", name];
        match self.exec_args(args).await {
            Ok(_) => TmuxResponse::SessionKilled {
                success: true,
                error: None,
            },
            Err(e) => TmuxResponse::SessionKilled {
                success: false,
                error: Some(e),
            },
        }
    }

    // =========================================================================
    // Pane Operations
    // =========================================================================

    async fn send_keys(&mut self, target: &str, keys: &str) -> TmuxResponse {
        let args: &[&str] = &["send-keys", "-t", target, keys, "Enter"];
        match self.exec_args(args).await {
            Ok(_) => TmuxResponse::KeysSent {
                success: true,
                error: None,
            },
            Err(e) => TmuxResponse::KeysSent {
                success: false,
                error: Some(e),
            },
        }
    }

    async fn switch_client(&mut self, target: &str) -> TmuxResponse {
        let log_path = "/tmp/tmux-deck.log";
        // switch-client must always be issued from the user's interactive
        // tmux client process — not from the control-mode client (which is
        // attached to a session of its own). Always use fork+exec here.
        let args: &[&str] = &["switch-client", "-t", target];
        match Self::fork_exec(args).await {
            Ok(_) => {
                append_switch_log(log_path, target, true, None);
                TmuxResponse::ClientSwitched {
                    target: target.to_string(),
                    success: true,
                    error: None,
                }
            }
            Err(e) => {
                append_switch_log(log_path, target, false, Some(&e));
                TmuxResponse::ClientSwitched {
                    target: target.to_string(),
                    success: false,
                    error: Some(e),
                }
            }
        }
    }

    // =========================================================================
    // Backend dispatch: control mode preferred, fork+exec fallback
    // =========================================================================

    async fn exec_args(&mut self, args: &[&str]) -> Result<String, String> {
        // Ensure we have a connected control mode (lazy reconnect).
        if self.ctrl.is_none() {
            self.ctrl = Self::try_connect_control().await;
        }

        if self.ctrl.is_some() {
            let cmd = args_to_control_command(args);
            match self.exec_via_ctrl(&cmd).await {
                Ok(out) => return Ok(out),
                Err(ControlExecError::Protocol(msg)) => {
                    return Err(msg);
                }
                Err(ControlExecError::Io(_)) => {
                    // Pipe broke. Drop and fall through to fork+exec.
                    if let Some(mut c) = self.ctrl.take() {
                        let _ = c.child.kill().await;
                    }
                }
            }
        }

        Self::fork_exec(args).await
    }

    async fn exec_via_ctrl(&mut self, cmd: &str) -> Result<String, ControlExecError> {
        let ctrl = self
            .ctrl
            .as_mut()
            .ok_or_else(|| ControlExecError::Io("not connected".to_string()))?;

        // Empty command would detach the client — refuse defensively.
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return Err(ControlExecError::Protocol("empty command".to_string()));
        }

        // Write command + newline.
        ctrl.stdin
            .write_all(cmd.as_bytes())
            .await
            .map_err(|e| ControlExecError::Io(e.to_string()))?;
        ctrl.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| ControlExecError::Io(e.to_string()))?;
        ctrl.stdin
            .flush()
            .await
            .map_err(|e| ControlExecError::Io(e.to_string()))?;

        // Read until %begin, then collect lines until %end / %error.
        let mut buf = String::new();
        let mut in_block = false;

        loop {
            let line = ctrl
                .stdout
                .next_line()
                .await
                .map_err(|e| ControlExecError::Io(e.to_string()))?;
            let line = match line {
                Some(l) => l,
                None => return Err(ControlExecError::Io("stdout closed".to_string())),
            };

            if !in_block {
                if line.starts_with("%begin ") {
                    in_block = true;
                } else if line.starts_with('%') {
                    // Notification outside block — skip for now (Step 5 will route).
                    debug!("ctrl notify: {}", line);
                }
                // Any other content before a %begin is unexpected; ignore.
            } else if line.starts_with("%end ") {
                return Ok(buf);
            } else if line.starts_with("%error ") {
                return Err(ControlExecError::Protocol(buf));
            } else {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
    }

    async fn try_connect_control() -> Option<ControlMode> {
        // Pick any existing session to attach control mode to. Without a
        // session, `tmux -C attach` errors and exits immediately.
        let session = match Self::first_session_name().await {
            Some(s) => s,
            None => {
                debug!("no tmux sessions; control mode disabled");
                return None;
            }
        };

        let mut child = match Command::new("tmux")
            .args(["-C", "attach", "-t", &session])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                warn!("spawn tmux -C: {e}");
                return None;
            }
        };

        let stdin = child.stdin.take()?;
        let stdout = BufReader::new(child.stdout.take()?).lines();

        // Drain the initial attach %begin/%end and notifications until the
        // pipe quiesces. A short timeout keeps startup snappy.
        let mut ctrl = ControlMode { child, stdin, stdout };
        if drain_initial(&mut ctrl).await.is_err() {
            let _ = ctrl.child.kill().await;
            return None;
        }
        debug!("control mode connected to ${}", session);
        Some(ctrl)
    }

    async fn first_session_name() -> Option<String> {
        let output = Command::new("tmux")
            .args(["list-sessions", "-F", "#{session_name}"])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&output.stdout);
        s.lines().next().map(|l| l.to_string())
    }

    async fn fork_exec(args: &[&str]) -> Result<String, String> {
        let output = Command::new("tmux")
            .args(args)
            .output()
            .await
            .map_err(|e| format!("tmux: {e}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    }
}

#[derive(Debug)]
enum ControlExecError {
    /// Pipe broken / IO failure — connection should be dropped.
    /// (Wrapped message is logged but the variant alone drives the decision.)
    Io(#[allow(dead_code)] String),
    /// tmux returned %error or other protocol-level failure for this command.
    Protocol(String),
}

/// Read any preamble lines (greeting/notifications) from a freshly-spawned
/// control mode process until the pipe quiesces for ~50 ms.
async fn drain_initial(ctrl: &mut ControlMode) -> Result<(), std::io::Error> {
    loop {
        let next = tokio::time::timeout(Duration::from_millis(50), ctrl.stdout.next_line()).await;
        match next {
            Ok(Ok(Some(line))) => {
                debug!("ctrl preamble: {}", line);
            }
            Ok(Ok(None)) => {
                return Err(std::io::Error::other("stdout closed during preamble"));
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => return Ok(()), // timeout = quiesced
        }
    }
}

/// Render argv into a single tmux command line for control-mode stdin.
fn args_to_control_command(args: &[&str]) -> String {
    args.iter()
        .map(|a| quote_for_control(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_for_control(arg: &str) -> String {
    let needs_quote = arg.is_empty()
        || arg.chars().any(|c| {
            c.is_whitespace() || matches!(c, '\'' | '"' | '\\' | ';' | '#' | '$' | '`')
        });
    if !needs_quote {
        return arg.to_string();
    }
    // POSIX-style single-quote escape: ' -> '\''
    let mut s = String::with_capacity(arg.len() + 2);
    s.push('\'');
    for c in arg.chars() {
        if c == '\'' {
            s.push_str("'\\''");
        } else {
            s.push(c);
        }
    }
    s.push('\'');
    s
}

// =============================================================================
// Refresh output parser (shared by both backends)
// =============================================================================

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
