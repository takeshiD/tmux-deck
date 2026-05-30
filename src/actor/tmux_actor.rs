use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::actor::messages::{TmuxCommand, TmuxResponse};
use crate::app::{TmuxPane, TmuxSession, TmuxWindow};

// =============================================================================
// TmuxActor — control-mode based, with fork+exec fallback
// =============================================================================
//
// A single persistent `tmux -C attach` process owns stdin/stdout. A background
// reader task partitions lines into command-response events (Begin/Output/End/
// Error) and asynchronous notifications. exec_via_ctrl consumes one block at a
// time; structural notifications (window-add, session-renamed, …) trigger an
// internal RefreshAll without waiting for the next periodic tick.
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
    /// Per-line events from the reader task for the currently-pending command.
    response_rx: mpsc::Receiver<CtrlEvent>,
    /// Asynchronous structural-change notifications (best-effort, coalesced).
    notify_rx: mpsc::Receiver<()>,
    /// Reader task; aborted on drop.
    reader_handle: Option<JoinHandle<()>>,
}

impl Drop for ControlMode {
    fn drop(&mut self) {
        if let Some(h) = self.reader_handle.take() {
            h.abort();
        }
    }
}

#[derive(Debug)]
enum CtrlEvent {
    Begin,
    End,
    Error,
    Line(String),
    Closed,
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
            // tokio::select! requires the future inside notify_rx.recv() to be
            // present; build it as a guarded branch so it's only polled when a
            // connection exists.
            let cmd = {
                let notify_available = self.ctrl.is_some();
                tokio::select! {
                    biased;
                    Some(c) = self.command_rx.recv() => c,
                    Some(c) = self.capture_rx.recv() => c,
                    Some(()) = async {
                        if notify_available {
                            self.ctrl.as_mut().unwrap().notify_rx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        // Coalesce any other pending notifications into a single
                        // refresh — bursts (window-add + layout-change + …) for
                        // one user action would otherwise queue N refreshes.
                        if let Some(ctrl) = self.ctrl.as_mut() {
                            while ctrl.notify_rx.try_recv().is_ok() {}
                        }
                        TmuxCommand::RefreshAll
                    }
                    else => break,
                }
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
            "SESS\t#{session_name}\t#{session_attached}\t#{session_activity}\t#{session_last_attached}",
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

        let mut sessions = build_sessions(&stdout);
        crate::hook::apply_states(&mut sessions);
        TmuxResponse::SessionsRefreshed { sessions }
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
        // Without -c, tmux's default target-client is the most recently
        // active client. Our own control-mode client is constantly active
        // (it services refresh queries), so it wins that heuristic and
        // switch-client silently retargets the control-mode client —
        // leaving the user's interactive session unchanged. Resolve the
        // interactive client tty up-front and pass it explicitly.
        let interactive_tty = self.find_interactive_client_tty().await;
        let mut args: Vec<&str> = Vec::with_capacity(5);
        args.push("switch-client");
        if let Some(tty) = interactive_tty.as_deref() {
            args.push("-c");
            args.push(tty);
        }
        args.push("-t");
        args.push(target);

        // switch-client itself must still go via fork+exec — running it
        // through the control-mode pipe would just switch the control
        // client.
        match Self::fork_exec(&args).await {
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

    /// Resolve the user's interactive tmux client by querying list-clients
    /// and picking the most-recently-active client that is NOT a control
    /// mode client and has a tty. Returns None if no such client exists
    /// (e.g. no one is attached) — the caller then falls back to running
    /// switch-client without -c.
    async fn find_interactive_client_tty(&mut self) -> Option<String> {
        let args: &[&str] = &[
            "list-clients",
            "-F",
            "#{client_tty}\t#{client_control_mode}\t#{client_activity}",
        ];
        let out = self.exec_args(args).await.ok()?;
        let mut best: Option<(i64, String)> = None;
        for line in out.lines() {
            let mut it = line.split('\t');
            let tty = it.next().unwrap_or("");
            let control = it.next().unwrap_or("0");
            let activity: i64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            if control == "1" || tty.is_empty() {
                continue;
            }
            match &best {
                Some((a, _)) if *a >= activity => {}
                _ => best = Some((activity, tty.to_string())),
            }
        }
        best.map(|(_, t)| t)
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

        // Consume events from the reader task until End / Error / Closed.
        let mut buf = String::new();
        let mut in_block = false;
        loop {
            let event = match ctrl.response_rx.recv().await {
                Some(e) => e,
                None => return Err(ControlExecError::Io("reader task gone".to_string())),
            };
            match event {
                CtrlEvent::Begin => in_block = true,
                CtrlEvent::Line(l) if in_block => {
                    buf.push_str(&l);
                    buf.push('\n');
                }
                CtrlEvent::Line(_) => {
                    // Out-of-block content is unexpected; ignore.
                }
                CtrlEvent::End => return Ok(buf),
                CtrlEvent::Error => return Err(ControlExecError::Protocol(buf)),
                CtrlEvent::Closed => return Err(ControlExecError::Io("stdout closed".to_string())),
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
        let stdout = child.stdout.take()?;

        let (response_tx, response_rx) = mpsc::channel::<CtrlEvent>(64);
        let (notify_tx, notify_rx) = mpsc::channel::<()>(16);

        // Spawn the reader task. It partitions stdout into command-block events
        // and (filtered) structural notifications. The initial attach also
        // produces a %begin..%end block; that first block belongs to the
        // implicit attach command and is drained here before we hand control
        // back to the actor.
        let mut reader_stdout = BufReader::new(stdout).lines();
        // Wait for the implicit attach block to complete.
        let mut saw_first_block = false;
        loop {
            match tokio::time::timeout(Duration::from_millis(500), reader_stdout.next_line()).await {
                Ok(Ok(Some(line))) => {
                    if line.starts_with("%begin ") {
                        saw_first_block = true;
                    } else if saw_first_block && (line.starts_with("%end ") || line.starts_with("%error ")) {
                        break;
                    }
                    // Drop notifications during preamble — they relate to our
                    // own attach (%session-changed, etc).
                }
                Ok(Ok(None)) => {
                    let _ = child.kill().await;
                    return None;
                }
                Ok(Err(_)) | Err(_) => {
                    let _ = child.kill().await;
                    return None;
                }
            }
        }

        let reader_handle = tokio::spawn(reader_task(reader_stdout, response_tx, notify_tx));

        debug!("control mode connected to ${}", session);
        Some(ControlMode {
            child,
            stdin,
            response_rx,
            notify_rx,
            reader_handle: Some(reader_handle),
        })
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

/// Long-lived task that owns the control-mode stdout. Lines inside a command
/// block are forwarded as CtrlEvent::{Begin,Line,End,Error}; lines outside a
/// block are classified as notifications and the structural ones produce a
/// best-effort tick on `notify_tx`.
///
/// The task exits when stdout closes or either downstream channel is dropped.
async fn reader_task(
    mut stdout: tokio::io::Lines<BufReader<ChildStdout>>,
    response_tx: mpsc::Sender<CtrlEvent>,
    notify_tx: mpsc::Sender<()>,
) {
    let mut in_block = false;
    loop {
        let line = match stdout.next_line().await {
            Ok(Some(l)) => l,
            _ => {
                let _ = response_tx.send(CtrlEvent::Closed).await;
                return;
            }
        };

        if in_block {
            if line.starts_with("%end ") {
                if response_tx.send(CtrlEvent::End).await.is_err() {
                    return;
                }
                in_block = false;
            } else if line.starts_with("%error ") {
                if response_tx.send(CtrlEvent::Error).await.is_err() {
                    return;
                }
                in_block = false;
            } else if response_tx.send(CtrlEvent::Line(line)).await.is_err() {
                return;
            }
        } else if line.starts_with("%begin ") {
            if response_tx.send(CtrlEvent::Begin).await.is_err() {
                return;
            }
            in_block = true;
        } else if line.starts_with('%') && is_structural_notification(&line) {
            // try_send: if the channel is full the consumer is already going
            // to refresh, so dropping is harmless (coalesced upstream).
            let _ = notify_tx.try_send(());
        }
        // else: non-structural notification (%output, %pane-mode-changed, …)
        // is dropped silently.
    }
}

/// Returns true for notifications whose arrival indicates the cached
/// session/window/pane tree may be stale.
fn is_structural_notification(line: &str) -> bool {
    // Match by leading token followed by space or end-of-line.
    const PREFIXES: &[&str] = &[
        "%sessions-changed",
        "%session-renamed",
        "%session-window-changed",
        "%window-add",
        "%window-close",
        "%window-renamed",
        "%window-pane-changed",
        "%layout-change",
        "%unlinked-window-add",
        "%unlinked-window-close",
        "%unlinked-window-renamed",
    ];
    PREFIXES.iter().any(|p| {
        line.len() == p.len() || line.starts_with(&format!("{} ", p))
    })
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
    last_attached: i64,
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
                let last_attached = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
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
                        last_attached,
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
                            claude_state: None,
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
        sb.last_attached
            .cmp(&sa.last_attached)
            .then_with(|| sb.activity.cmp(&sa.activity))
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
                    claude_state: None,
                })
                .collect();
            let unread = !s.attached && s.activity > s.last_attached;
            Some(TmuxSession {
                name,
                attached: s.attached,
                unread,
                windows,
                claude_state: None,
                last_attached: s.last_attached,
                activity: s.activity,
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
