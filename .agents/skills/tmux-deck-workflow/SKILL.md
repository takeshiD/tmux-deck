---
name: tmux-deck-workflow
description: Coordinate implementation, issue work, testing, and parallel coding-agent tasks in the tmux-deck repository using isolated Git worktrees and focused integration.
---

# tmux-deck workflow

Use an isolated worktree for every task that changes repository files. A task
already running inside its assigned worktree should continue there rather than
creating another nested worktree.

## Create or select the worktree

1. Inspect `git status --short --branch` and `git worktree list` before edits.
2. Derive a short lowercase task slug, using the issue number when one exists
   (for example, `issue-15-preview-scroll` or `codex-hooks`).
3. Use `<repository-root>/.agents/worktrees/<task-slug>` as the default
   worktree path and `agent/<task-slug>` as its branch. Resolve the repository
   root first; do not assume the current directory is the root. A
   user-specified path or branch wins.
4. Create the worktree from the intended integration commit, normally the
   current `HEAD`:

   ```bash
   git worktree add -b agent/<task-slug> <repository-root>/.agents/worktrees/<task-slug> HEAD
   ```

If the primary checkout is dirty, stop before moving its changes. Continue
read-only, work from a clean base, or ask the user for explicit permission to
stash or relocate those changes. Never assume another worktree's modifications
belong to the current task.

For parallel work, give each agent a distinct worktree and a non-overlapping
task boundary. Tell the agent which branch it owns and whether it should commit.

## Implement and verify

- When a request references a GitHub issue, read that issue before deciding its
  behavior. Preserve its configurable defaults and acceptance criteria.
- Follow existing `src/app.rs`, actor, configuration, and rendering boundaries.
  Add state transitions and config parsing tests where behavior changes.
- Run focused tests while iterating, then run:

  ```bash
  cargo test --all --locked
  cargo clippy --all-targets --all-features --locked -- -D warnings
  git diff --check
  ```

- Review `git diff --stat` and the complete diff against the task's base commit.
  Remove unrelated formatting, generated artifacts, and opportunistic cleanup.
- Commit the focused result on the worktree branch and report the full commit
  hash, checks run, and any remaining constraint.

## Integrate parallel results

The coordinating agent should integrate completed commits one at a time into
its integration worktree or branch. Resolve conflicts semantically; do not take
an entire side when both tasks intentionally changed the same state, config, or
UI path. Re-run the full test and Clippy commands after the final integration.

Keep source worktrees and branches until their commits have been integrated and
verified. Cleanup is a separate operation; do not remove worktrees or branches
unless the user asks for it or the active workflow explicitly owns cleanup.
