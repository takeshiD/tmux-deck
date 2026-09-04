# Repository instructions

This file is the source of truth for repository-wide instructions followed by
coding agents.

## Instruction layout

- Keep repository-owned guardrails under `.agents/rules/` and reusable project
  workflows under `.agents/skills/`.
- Keep shared user-level skills and tool configuration outside this repository.
- Keep `CLAUDE.md` as a symbolic link to `AGENTS.md`; do not duplicate these
  instructions in tool-specific files.
- Resolve every repository-owned symbolic link within the repository and
  verify that it remains valid from a fresh checkout.

## Required reading

Before planning or changing product code, configuration, CLI behavior, the
terminal interface, persistence, integrations, build configuration, or release
automation, read and follow [`.agents/rules/architecture.md`](.agents/rules/architecture.md).

For implementation, bug fixes, editable reviews, testing, or parallel agent
work, also read and follow
[`.agents/skills/tmux-deck-workflow/SKILL.md`](.agents/skills/tmux-deck-workflow/SKILL.md).

## Repository map

- `src/main.rs`: process bootstrap, subcommand dispatch, terminal lifecycle,
  and actor wiring.
- `src/actor/`: asynchronous ownership and messages for UI, tmux, and refresh
  work.
- `src/app.rs`: domain types, selection, interaction state, and state
  transitions.
- `src/ui.rs`: Ratatui layout and rendering.
- `src/config.rs`: configuration schema, defaults, key bindings, and themes.
- `src/hook.rs`: Claude Code hook installation and per-pane state files.
- `src/agents.rs`: read-only discovery of Claude background sessions.
- `src/group.rs`: tmux-deck's persisted session-group assignments.
- `src/termscreen.rs`: terminal-screen reconstruction used by previews.
- `docs/config.example.toml`: complete user-facing configuration reference.
- `demo/` and `assets/`: reproducible VHS demo inputs and generated media.

## Change workflow

- Keep the primary checkout clean. Make tracked-file changes in one dedicated
  Git worktree per task; read-only investigation may use the primary checkout.
- Put worktrees under `.worktrees/<task-slug>` at the repository root. The
  directory is ignored by Git.
- Give each concurrent agent a separate worktree and non-overlapping ownership.
- Preserve unrelated user changes. Never stash, reset, restore, relocate, or
  delete them without explicit authorization.
- Keep commits focused. Do not mix unrelated cleanup, formatting, generated
  media, or dependency updates into a feature or fix.
- When a GitHub issue defines the task, read its full body and comments before
  deciding behavior. Keep the pull request scoped to that issue.

## Documentation and compatibility

- Treat the CLI, configuration keys and defaults, key bindings, persisted file
  formats, and visible TUI behavior as user-facing compatibility surfaces.
- Update `README.md` when installation, commands, keys, or visible behavior
  changes.
- Update `docs/config.example.toml` whenever configuration behavior changes.
- Regenerate demo media only when the change intentionally affects it; review
  generated binary changes before committing them.
- Do not change the package version or release workflow as part of unrelated
  work.

## Required checks

For Rust changes, run focused tests while iterating, then:

```bash
cargo test --all --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
git diff --check
```

Format changed Rust code, but do not reformat unrelated files. For Nix or
packaging changes, also run `nix build .#default`. For demo changes, run
`nix run .#demo` and inspect the resulting assets. Documentation-only changes
need `git diff --check` plus validation of links and referenced paths.

Before handoff, review the complete diff against the task's base commit and
report the checks run, checks omitted with a reason, and any remaining risk.
