use clap::{
    CommandFactory, FromArgMatches, Parser, Subcommand,
    builder::{Styles, styling::AnsiColor},
};
use color_eyre::Result;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(author, name="tmux-deck", about="a tmux session manager and monitoring multi sessions.", version, long_about=None)]
pub struct Cli {
    /// Config file (defaults to $XDG_CONFIG_HOME/tmux-deck/config.toml)
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    /// Target pane (e.g., "session:window.pane" or "%123")
    #[arg(short, long)]
    pub target: Option<String>,
    /// Preview refresh interval in milliseconds (overrides the config file)
    #[arg(short, long)]
    pub interval: Option<u64>,
    /// Subcommand (omit to launch the interactive TUI)
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Drive tree-view markers from Claude Code or Codex lifecycle hooks.
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum HookAction {
    /// Report an agent hook event (reads the hook JSON on stdin).
    ///
    /// This is meant to be wired into Claude Code or Codex as a command hook.
    /// It records the calling pane's agent state so tmux-deck can render a
    /// per-pane marker.
    Report {
        /// Store Codex state instead of Claude Code state.
        #[arg(long)]
        codex: bool,
    },
    /// Install hooks for Claude Code (default) or Codex.
    Install {
        /// Use project-local .claude/settings.json or .codex/hooks.json.
        #[arg(long)]
        project: bool,
        /// Target Codex hooks.json instead of Claude Code settings.json.
        #[arg(long)]
        codex: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn help_for(args: &[&str]) -> String {
        Cli::command()
            .try_get_matches_from(args)
            .expect_err("--help should stop argument parsing")
            .to_string()
    }

    #[test]
    fn install_help_identifies_both_targets() {
        let help = help_for(&["tmux-deck", "hook", "install", "--help"]);
        assert!(help.contains("Claude Code (default) or Codex"));
        assert!(help.contains(".codex/hooks.json"));
        assert!(help.contains(".claude/settings.json"));
    }

    #[test]
    fn report_help_explains_codex_state_separation() {
        let help = help_for(&["tmux-deck", "hook", "report", "--help"]);
        assert!(help.contains("Store Codex state instead of Claude Code state"));
    }

    #[test]
    fn existing_claude_commands_remain_the_default() {
        let report = Cli::try_parse_from(["tmux-deck", "hook", "report"]).unwrap();
        assert!(matches!(
            report.command,
            Some(Command::Hook {
                action: HookAction::Report { codex: false }
            })
        ));

        let install = Cli::try_parse_from(["tmux-deck", "hook", "install", "--project"]).unwrap();
        assert!(matches!(
            install.command,
            Some(Command::Hook {
                action: HookAction::Install {
                    project: true,
                    codex: false
                }
            })
        ));
    }
}
