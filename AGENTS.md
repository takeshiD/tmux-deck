# tmux-deck agent guidance

These instructions apply to the whole repository.

## Shared workflow

- Before changing product code, configuration, CLI behavior, or the TUI, read
  and follow `.agents/rules/architecture.md`.
- For implementation, bug fixes, reviews that may edit files, or parallel agent
  work, read and follow `.agents/skills/tmux-deck-workflow/SKILL.md`.
- Keep the primary checkout clean. Do implementation in a dedicated Git
  worktree; read-only investigation may use the primary checkout.
- Create worktrees under `.worktrees/<task-slug>` at the repository
  root. This directory is ignored by Git.
- One agent owns one worktree. Never let multiple agents edit the same
  worktree concurrently.
- Preserve unrelated user changes. Do not stash, reset, restore, or relocate a
  dirty checkout unless the user explicitly asks for that operation.
- Keep commits focused. Do not mix mechanical formatting or unrelated cleanup
  into a feature or fix.

## Project checks

Use the repository's Rust checks:

```bash
cargo test --all --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
```

The CI does not run `cargo fmt`. Format changed code, but do not reformat
unrelated files merely to make a repository-wide format check pass.

## Instruction ownership

`AGENTS.md` is the canonical cross-agent instruction file. Keep the root
`CLAUDE.md` as a symbolic link to this file so agents share one source of
truth. Put architecture constraints under `.agents/rules/` and reusable
project workflows under `.agents/skills/`.
