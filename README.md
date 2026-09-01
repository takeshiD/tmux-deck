# tmux-deck

[![crates.io](https://img.shields.io/crates/v/tmux-deck.svg)](https://crates.io/crates/tmux-deck)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

See every tmux session before you switch.

`tmux-deck` is an interactive session manager with live pane previews. Browse
sessions, windows, and panes in one keyboard-driven view, or open a dashboard
that monitors several sessions at once.

![tmux-deck demo](assets/tmux-deck-demo.gif)

## Why tmux-deck?

- **Live previews** — inspect the current contents of a pane without switching
  away from your work.
- **Multi-session view** — monitor windows across several sessions on one
  screen.
- **Interactive management** — create, rename, group, sort, and kill sessions
  from the TUI.
- **Zero configuration** — sensible defaults work immediately; themes, layout,
  refresh rate, and key bindings remain configurable.
- **Optional Claude Code awareness** — show working, waiting, done, and error
  markers, plus a dedicated background-agent view.

`tmux-deck` is distributed as a Rust binary and requires a working `tmux`
installation.

## Install

### Cargo

```bash
cargo install tmux-deck
tmux-deck
```

### Nix

Try the latest revision without installing it:

```bash
nix run github:takeshiD/tmux-deck
```

## Use it as a tmux popup

Add this binding to `~/.tmux.conf`, then press `prefix + Space`:

```tmux
bind-key Space display-popup -w 80% -h 80% -E tmux-deck
```

If you prefer the Nix command:

```tmux
bind-key Space display-popup -w 80% -h 80% -E \
  'nix run github:takeshiD/tmux-deck'
```

| Full session manager | tmux popup |
| --- | --- |
| <img src="assets/tmux-deck-session-manager.png" alt="tmux-deck session manager with a live API pane preview" width="600"> | <img src="assets/tmux-deck-popup.png" alt="tmux-deck running inside a tmux popup" width="600"> |

## Essential keys

The status bar always reflects your configured bindings.

| Key | Action | Key | Action |
| --- | --- | --- | --- |
| `j` / `k` or arrows | Move | `Tab` / `Shift+Tab` | Change panel |
| `Enter` | Switch to selection | `Space` twice | Toggle tree/multi preview |
| `s` | Cycle sort order | `g` | Assign a session group |
| `za` | Fold/unfold a group | `i` | Send input to a pane |
| `Ctrl+n` | New session | `Ctrl+r` | Rename session |
| `Ctrl+x` | Kill session | `q` / `Esc` | Quit |
| `d` | Toggle agent view | `r` | Refresh |

## Configuration

Configuration is optional. Put a TOML file at:

```text
$XDG_CONFIG_HOME/tmux-deck/config.toml
```

This is usually `~/.config/tmux-deck/config.toml`. You can also pass a file
with `tmux-deck --config <path>`. A missing or malformed file falls back to the
defaults.

```toml
[preview]
interval = 300

[theme]
preset = "tokyonight"

[layout]
session_panel_width = 30
tree_split = [30, 35, 35]
multi_selected_ratio = 70

[behavior]
default_view = "tree"       # "tree" or "multi"
default_sort = "recent"     # "recent", "recent_asc", "abc", "abc_asc"
exit_on_switch = true

[keybindings]
quit = ["q", "Esc"]
new_session = "C-n"
```

See the fully commented [configuration reference](docs/config.example.toml)
for every setting and semantic colour role.

### Themes

The built-in presets are `default`, `monochrome`, `dracula`, `nord`,
`gruvbox`, `tokyonight`, `catppuccin`, `solarized`, `cyberdream`, and
`carbonfox`. Individual semantic colours can be overridden with named colours,
256-colour indexes, or `#rrggbb` values.

## Advanced: Claude Code integration

tmux-deck can identify panes running
[Claude Code](https://code.claude.com) and display a `●` marker. Installing the
optional hooks makes the marker reflect the pane's current state:

| Marker | State | Meaning |
| --- | --- | --- |
| `⠋⠙⠹…` | Working | A prompt or tool is running |
| `◆` | Waiting | Claude is waiting for input |
| `✓` | Done | The turn completed |
| `✗` | Error | The turn ended with an error |
| `●` | Running | Claude is detected without hook state |

Install the hooks in your user settings:

```bash
tmux-deck hook install
```

Use `tmux-deck hook install --project` for project-local Claude settings. The
installer is idempotent and preserves existing settings.

### Agent view

Press `d` to open a full-screen view of Claude Code background sessions. It
reads the sessions managed by `claude agents` and groups them by working
directory.

| Key | Action |
| --- | --- |
| `j` / `k` | Select an agent session |
| `Enter` | Attach with `claude attach` |
| `p` | Toggle the preview panel |
| `v` | Toggle transcript/screen preview |
| `s` | Generate an execution summary |
| `d` | Return to the tmux tree |

The agent view needs a Claude Code version that provides background sessions.
The tmux pane markers and the agent view are independent features.

## How it differs

tmux-deck focuses on interactively inspecting and managing sessions that are
running now. Tools such as tmuxinator and tmuxp focus on declaratively defining,
sharing, and restoring session layouts. Use tmux-deck for live visibility; use
a declarative manager when reproducible session setup is the primary goal.

## Development

Run the Rust checks:

```bash
cargo test --all --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
```

Regenerate the README demo and screenshots:

```bash
nix run .#demo
```

The demo command starts an isolated tmux server with synthetic sessions. It
does not read from or modify your current tmux server. The VHS tapes and fixture
live in [`demo/`](demo/).

Contributor documentation will be maintained separately from this user-facing
README.

## License

[MIT](LICENSE)
