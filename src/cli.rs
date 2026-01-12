use clap::{
    CommandFactory, FromArgMatches, Parser,
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
