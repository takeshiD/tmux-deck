use clap::{
    CommandFactory, FromArgMatches, Parser, Subcommand,
    builder::{Styles, styling::AnsiColor},
};
use color_eyre::Result;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(author, name="tmux-deck", about="a tmux session manager and monitoring multi sessions.", version, long_about=None)]
pub struct Cli {
    /// Config file
    #[arg(short, long, default_value = "~/.config/tmux_deck/config.toml")]
    pub config: Option<PathBuf>,
    /// Target pane (e.g., "session:window.pane" or "%123")
    #[arg(short, long)]
    pub target: Option<String>,
    /// Preview refresh interval in milliseconds
    #[arg(short, long, default_value = "300")]
    pub interval: u64,
    /// Subcommand (omit to launch the interactive TUI)
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Claude Code hook integration: drive treeview markers from Claude's state.
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum HookAction {
    /// Report a Claude hook event (reads the hook JSON on stdin).
    ///
    /// This is meant to be wired into Claude Code's `settings.json` as a
    /// `command` hook. It records the calling pane's Claude state so the
    /// tmux-deck TUI can render a per-pane marker.
    Report,
    /// Install the tmux-deck hooks into Claude Code's settings.json.
    Install {
        /// Install into the project-local `.claude/settings.json` instead of
        /// the user-global `~/.claude/settings.json`.
        #[arg(long)]
        project: bool,
    },
}

impl Cli {
    pub fn parse_with_color() -> Result<Self, clap::Error> {
        const STYLES: Styles = Styles::styled()
            .header(AnsiColor::Green.on_default().bold())
            .usage(AnsiColor::Green.on_default().bold())
            .literal(AnsiColor::Blue.on_default())
            .placeholder(AnsiColor::Cyan.on_default().bold());
        let cmd = Self::command().styles(STYLES);
        Self::from_arg_matches(&cmd.get_matches())
    }
}
