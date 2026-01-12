use crate::app::TmuxSession;
use tokio::sync::oneshot;

// =============================================================================
// TmuxActor Commands (UIActor/RefreshActor → TmuxActor)
// =============================================================================

#[derive(Debug)]
pub enum TmuxCommand {
    /// Refresh all sessions, windows, and panes
    RefreshAll,

    /// Capture pane content
    CapturePane { target: String, start: i32, end: i32 },

    /// Create a new session
    NewSession { name: String },

    /// Rename an existing session
    RenameSession { old_name: String, new_name: String },

    /// Kill a session
    KillSession { name: String },

    /// Send keys to a pane
    SendKeys {
        target: String,
        keys: String,
        reply: Option<oneshot::Sender<TmuxResponse>>,
    },

    /// Switch client to a target
    SwitchClient {
        target: String,
        reply: Option<oneshot::Sender<TmuxResponse>>,
    },
}

// =============================================================================
// TmuxActor Responses (TmuxActor → UIActor)
// =============================================================================

#[derive(Debug, Clone)]
pub enum TmuxResponse {
    /// Sessions data refreshed
    SessionsRefreshed { sessions: Vec<TmuxSession> },

    /// Pane content captured
    PaneCaptured {
        #[allow(dead_code)]
        target: String,
        content: String,
    },

    /// Session created result
    SessionCreated {
        name: String,
        success: bool,
        error: Option<String>,
    },

    /// Session renamed result
    SessionRenamed {
        success: bool,
        error: Option<String>,
    },

    /// Session killed result
    SessionKilled {
        success: bool,
        error: Option<String>,
    },

    /// Keys sent result
    KeysSent {
        #[allow(dead_code)]
        success: bool,
        error: Option<String>,
    },

    /// Client switched result
    ClientSwitched {
        #[allow(dead_code)]
        target: String,
        success: bool,
        error: Option<String>,
    },

    /// Error occurred
    Error { message: String },
}

// =============================================================================
// UIActor Events (RefreshActor → UIActor)
// =============================================================================

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum UIEvent {
    /// Request to capture the currently selected pane
    RequestCapture,

    /// Periodic tick for refresh
    Tick,

    /// Shutdown signal
    Shutdown,
}

// =============================================================================
// Shared State for RefreshActor coordination
// =============================================================================

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct RefreshControl {
    /// Whether refresh is paused (during input mode or popup)
    paused: Arc<AtomicBool>,
}

impl RefreshControl {
    pub fn new() -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
}

impl Default for RefreshControl {
    fn default() -> Self {
        Self::new()
    }
}
