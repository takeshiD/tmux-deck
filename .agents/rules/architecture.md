# Architecture guardrails

Apply these rules when planning, implementing, reviewing, or refactoring
product code, configuration, CLI behavior, and the terminal interface.

## Runtime boundaries

- Keep `UIActor` as the owner of the terminal and mutable `UIState`.
- Keep tmux subprocess and control-mode interaction in `TmuxActor`; rendering
  and application state must not invoke tmux directly.
- Keep periodic work in `RefreshActor` and communicate between actors through
  the message types in `src/actor/messages.rs`.
- Do not block the UI actor on tmux, filesystem, transcript, or model work.
  Return asynchronous results through the existing actor channels.
- Subcommands that do not need the TUI must complete before raw mode and the
  alternate screen are entered.

## State, rendering, and configuration

- Keep domain and interaction state in `src/app.rs`; keep `src/ui.rs` focused
  on rendering that state.
- Model user actions in the configuration/action layer instead of scattering
  hard-coded key handling through rendering code.
- Preserve zero-configuration startup. Invalid or missing optional user
  configuration should retain the documented fallback behavior.
- Keep theme values semantic and configurable. Do not make color the only
  signal for a state.
- Measure and truncate terminal content by display-cell width. Test changes
  involving layout or text with narrow terminals and wide Unicode characters.

## tmux and agent integration

- Prefer structured tmux commands and control-mode events over parsing
  presentation-oriented terminal output.
- Treat tmux session, window, and pane identifiers as external data; do not
  construct shell command strings from them without safe argument handling.
- Keep Claude Code hooks optional and preserve useful behavior when hooks or
  agent metadata are unavailable.
- Project-local hook installation must preserve unrelated user settings and be
  idempotent.

## Completion check

- Add tests at the lowest stable boundary: state-transition and parser tests
  first, rendering tests for visible layout behavior, and subprocess tests only
  for integration risk.
- Verify terminal cleanup for any changed exit or error path.
- Run the commands documented in `AGENTS.md` and `git diff --check` before
  handing off a change.
