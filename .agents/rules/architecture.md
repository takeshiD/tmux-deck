# Architecture guardrails

These rules describe the boundaries that keep tmux-deck responsive, safe, and
compatible. Apply them when planning, implementing, reviewing, or refactoring
product code, configuration, integrations, build configuration, or release
automation. If an intentional architecture change conflicts with a rule,
update the design and this file together rather than silently bypassing it.

## Product and compatibility boundary

- Keep tmux-deck focused on interactively inspecting and managing live tmux
  sessions. Declarative session creation and restoration belong to tools such
  as tmuxinator and tmuxp.
- Preserve a useful zero-configuration experience and graceful degradation
  when optional Claude Code metadata or hooks are unavailable.
- Treat commands and flags, configuration keys and defaults, key bindings,
  state-file formats, group persistence, and visible interaction behavior as
  compatibility surfaces. Make breaking changes explicit and documented.
- Keep the executable self-contained at runtime apart from capabilities the
  user invokes explicitly, such as tmux or Claude Code integration.

## Module ownership

- `src/main.rs` owns bootstrap, non-TUI subcommand dispatch, terminal entry and
  restoration, actor construction, and shutdown coordination. Keep business
  logic out of it.
- `src/actor/messages.rs` owns communication contracts between actors.
  `TmuxActor` owns tmux IO, `RefreshActor` owns periodic scheduling, and
  `UIActor` owns the terminal and mutable `UIState`.
- `src/app.rs` owns domain and interaction state. State transitions must remain
  testable without a live terminal or tmux server.
- `src/ui.rs` renders current state. It must not invoke tmux, read persistence,
  or become a second owner of application state.
- `src/config.rs` owns configuration parsing, defaults, key bindings, and theme
  resolution. `src/group.rs`, `src/hook.rs`, and `src/agents.rs` own their
  respective external file formats and discovery behavior.
- Put terminal-emulation logic in `src/termscreen.rs`; do not duplicate ANSI or
  display-cell parsing in widgets.

## Concurrency and responsiveness

- Keep `UIActor` as the sole terminal owner and route background results back
  through channels. Never mutate `UIState` concurrently.
- Do not block the UI task on tmux, filesystem traversal, transcript parsing,
  external commands, or model work. Use the established actor, task, or
  blocking-thread boundary and make completion observable as an event.
- Preserve separate high-priority user commands and low-priority preview
  capture traffic so refresh work cannot starve input.
- Coalesce bursty refresh notifications and avoid unbounded queues, tasks, or
  caches. Periodic ticks should redraw only when data or animation changes.
- Background work must tolerate its consumer disappearing during shutdown;
  dropped channels are a normal termination path.

## tmux integration

- Prefer the persistent tmux control-mode connection for routine operations,
  while retaining the existing per-command fallback when control mode is
  unavailable or dies.
- Keep parsing coupled to explicit tmux format strings owned by the same code.
  Do not scrape human-oriented tmux output.
- Pass session, window, pane, and user-provided values as process arguments or
  validated control-mode fields. Never interpolate them into a shell command.
- Treat tmux output and identifiers as untrusted external data. Malformed or
  partial records must produce a recoverable error rather than panic or corrupt
  the current selection.
- Preserve stable tmux pane IDs as the key for previews and hook state. Keep
  socket/server identity in mind when introducing persisted cross-pane data.
- User-triggered tmux actions must take priority over periodic capture and
  refresh work.

## Terminal UI

- Enter raw mode and the alternate screen only for the interactive TUI. Restore
  raw mode, alternate screen, cursor, and any enabled input modes on every exit
  and error path.
- Keep logs and diagnostics out of the screen owned by Ratatui. Write runtime
  logs to the configured state-directory log file.
- Recompute layout from the current frame size. Every multi-pane layout needs a
  narrow fallback or an honest terminal-too-small state.
- Measure display width in terminal cells, not bytes or Unicode scalar count.
  Test truncation and alignment with CJK text, combining characters, and emoji.
- Keep styles semantic and configurable. Never use color as the only signal,
  and preserve meaningful output under reduced-color terminals.
- Make every action keyboard reachable, keep focus visible, and keep displayed
  key hints synchronized with configurable bindings.
- Rendering must be deterministic from state plus intentional animation time;
  it must not perform IO or launch subprocesses.

## Configuration and persistence

- Preserve precedence as CLI override, then configuration file, then built-in
  default. Missing or malformed optional configuration must keep the app usable
  and report diagnostics through logging.
- Add new configuration fields with serde/default behavior so existing files
  continue to load. Update defaults, parser tests, and
  `docs/config.example.toml` together.
- Keep group assignments a tmux-deck concept distinct from tmux session groups.
  Missing or unreadable group storage should degrade to an empty or in-memory
  store rather than prevent startup.
- Use atomic replacement for user-owned files whenever partial writes could
  damage configuration or persisted state. Preserve unrelated content when
  editing shared files.
- Bound persisted and displayed external strings. Do not store secrets, full
  prompts, transcripts, or raw tool input merely to enrich the UI.

## Claude Code integration

- Keep pane hook state and background-agent discovery independent: either must
  work when the other is unavailable.
- Hook reporting is best-effort and must never block or fail the calling Claude
  session. Reject malformed input quietly and avoid panics.
- Hook installation must be idempotent, preserve unrelated settings, respect
  project-local versus user-global scope, and use an absolute executable path
  when required by the spawned environment.
- Treat Claude state files, transcripts, roster data, and command output as
  untrusted and version-unstable. Ignore unknown fields and degrade cleanly when
  files disappear during a read.
- Persist only the minimum activity digest needed by the UI. Never persist full
  `tool_input` or prompt content.

## Build and release

- Keep `Cargo.lock` committed and use locked dependency resolution in CI and
  normal verification.
- Keep Cargo and Nix package metadata aligned. Changes to dependencies or
  packaged paths must be validated through both Cargo and `nix build .#default`.
- Release tags must follow the format accepted by the release workflow and
  match the package version exactly.
- Keep release permissions minimal and do not expose publication credentials to
  pull-request jobs.
- Generated demo assets are reviewed outputs, not authoritative sources; their
  tapes and scripts under `demo/` remain the reproducible source.

## Completion check

- Add tests at the lowest stable boundary: state transitions and parsers first,
  rendered behavior next, and subprocess or PTY tests only for integration risk.
- For actor changes, verify channel closure, command priority, and shutdown.
- For terminal changes, verify normal exit, error cleanup, resize, narrow width,
  and wide-character behavior.
- For configuration or persistence changes, verify missing, valid, malformed,
  and backward-compatible inputs.
- Run the checks required by `AGENTS.md`, inspect the complete diff, and verify
  every applicable section above before declaring the change complete.
