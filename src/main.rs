mod actor;
mod agents;
mod app;
mod cli;
mod config;
mod group;
mod hook;
mod ui;

use std::io;
use std::time::Duration;

use color_eyre::Result;
use crossterm::{
    ExecutableCommand,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use directories::ProjectDirs;
use ratatui::prelude::*;
use tokio::sync::mpsc;
use tracing_subscriber::{EnvFilter, fmt::time};

use actor::{RefreshActor, RefreshControl, TmuxActor, TmuxCommand, TmuxResponse, UIActor, UIEvent};
use app::UIState;
use cli::{Cli, Command, HookAction};
use config::Config;

/// Fallback preview interval (ms) when neither the CLI flag nor the config sets one.
const DEFAULT_INTERVAL_MS: u64 = 300;

// =============================================================================
// Main
// =============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let cmd = Cli::parse_with_color()?;

    // Subcommands run without the TUI / terminal setup.
    if let Some(command) = &cmd.command {
        return match command {
            Command::Hook { action } => match action {
                HookAction::Report => {
                    hook::run_report();
                    Ok(())
                }
                HookAction::Install { project } => hook::run_install(*project),
            },
        };
    }

    // Load user config (best-effort): CLI --config > XDG config.toml > defaults.
    let config = Config::load(cmd.config.as_deref());
    // CLI --interval wins over the config, which wins over the built-in default.
    let interval_ms = cmd
        .interval
        .or(config.preview.interval)
        .unwrap_or(DEFAULT_INTERVAL_MS);

    let project_dir =
        ProjectDirs::from("dev", "tkcd", "tmux-deck").expect("cannot determine project directory");
    let log_dir = project_dir.state_dir().expect("failed to get log dir");
    std::fs::create_dir_all(log_dir).expect("failed to create log dir");
    let log_file_path = log_dir.join("tmux-deck.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)
        .expect("failed to open log file");
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(log_file)
        .with_ansi(false)
        .with_timer(time::LocalTime::rfc_3339())
        .init();

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let result = run_app(terminal, config, interval_ms).await;

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

async fn run_app(
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    config: Config,
    interval_ms: u64,
) -> Result<()> {
    // Create channels.
    // tmux_cmd_*: high-priority user-initiated commands.
    // tmux_capture_*: low-priority periodic capture-pane requests.
    let (tmux_cmd_tx, tmux_cmd_rx) = mpsc::channel::<TmuxCommand>(32);
    let (tmux_capture_tx, tmux_capture_rx) = mpsc::channel::<TmuxCommand>(32);
    let (tmux_resp_tx, tmux_resp_rx) = mpsc::channel::<TmuxResponse>(32);
    let (ui_event_tx, ui_event_rx) = mpsc::channel::<UIEvent>(32);

    // Create shared refresh control
    let refresh_control = RefreshControl::new();

    // Initialize UIState
    let state = UIState::new(config);
    let interval = Duration::from_millis(interval_ms);

    // Create actors
    let tmux_actor = TmuxActor::new(tmux_cmd_rx, tmux_capture_rx, tmux_resp_tx);
    let refresh_actor = RefreshActor::new(
        tmux_capture_tx.clone(),
        ui_event_tx,
        refresh_control.clone(),
        interval,
    );
    let ui_actor = UIActor::new(
        terminal,
        state,
        tmux_cmd_tx,
        tmux_capture_tx,
        tmux_resp_rx,
        ui_event_rx,
        refresh_control,
    );

    // Spawn TmuxActor
    let tmux_handle = tokio::spawn(async move {
        tmux_actor.run().await;
    });

    // Spawn RefreshActor
    let refresh_handle = tokio::spawn(async move {
        refresh_actor.run().await;
    });

    // Run UIActor on main task (it owns the terminal)
    let result = ui_actor.run().await;

    // Cleanup: abort background actors
    tmux_handle.abort();
    refresh_handle.abort();

    result
}
