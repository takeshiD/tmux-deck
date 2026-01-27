use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::actor::messages::{RefreshControl, TmuxCommand, UIEvent};

// =============================================================================
// RefreshActor
// =============================================================================

pub struct RefreshActor {
    tmux_tx: mpsc::Sender<TmuxCommand>,
    ui_event_tx: mpsc::Sender<UIEvent>,
    refresh_control: RefreshControl,
    interval: Duration,
}

impl RefreshActor {
    pub fn new(
        tmux_tx: mpsc::Sender<TmuxCommand>,
        ui_event_tx: mpsc::Sender<UIEvent>,
        refresh_control: RefreshControl,
        interval: Duration,
    ) -> Self {
        Self {
            tmux_tx,
            ui_event_tx,
            refresh_control,
            interval,
        }
    }

    pub async fn run(self) {
        let mut ticker = interval(self.interval);

        loop {
            ticker.tick().await;

            // Check if refresh is paused (input mode or popup active)
            if self.refresh_control.is_paused() {
                continue;
            }

            // Send RefreshAll command to TmuxActor
            if self.tmux_tx.send(TmuxCommand::RefreshAll).await.is_err() {
                // TmuxActor has been dropped, exit
                break;
            }

            // Notify UIActor about the tick (for capture request if needed)
            if self.ui_event_tx.send(UIEvent::Tick).await.is_err() {
                // UIActor has been dropped, exit
                break;
            }
        }
    }
}
