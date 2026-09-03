# tmux-deck agent guidance

These instructions apply to the whole repository.

## Shared workflow

- For implementation, bug fixes, reviews that may edit files, or parallel agent
  work, read and follow `.agents/skills/tmux-deck-workflow/SKILL.md`.
- Keep the primary checkout clean. Do implementation in a dedicated Git
  worktree; read-only investigation may use the primary checkout.
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

`AGENTS.md` is the canonical cross-agent instruction file. `CLAUDE.md` is a
Claude Code adapter and must point back here instead of duplicating these
rules. Put reusable operational detail in the repository skill, not in
tool-specific instruction files.
