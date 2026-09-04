# Agent Monitor Multiview Design

Status: Draft

## Intent

Refocus MultiPreview from a general tmux session overview into a monitor for
coding agents running in tmux panes. The view must support two complementary
loops:

1. Notice and handle agents that require user action.
2. Observe the overall activity of all running agents without losing spatial
   context.

This document records decisions as they are made. Open questions are not
implementation requirements.

## Current behavior

MultiPreview currently renders one horizontal column per tmux session and one
vertical preview per window. Navigation selects a session with `h/l` and a
window with `j/k`. Selection receives a configurable share of the terminal
width. All sessions participate whether or not they contain a coding agent.

`ViewMode::Dashboard`, exposed to users as the Agent view through `d`, is a
separate screen. It reads Claude background sessions from `~/.claude/jobs`, can
preview their transcript or reconstructed screen, and can attach to one. It
does not represent interactive coding agents running in tmux panes.

## Accepted decisions

- The first version monitors only coding agents running in tmux panes.
- One agent pane is one monitor card. A tmux session containing multiple agent
  panes contributes multiple cards.
- MultiView has two presentation modes:
  - **Attention** makes waiting and failed agents difficult to miss and quick
    to enter.
  - **Overview** keeps all active agents visible for ambient progress
    monitoring.
- Completed agents remain visible for a short retention period and then leave
  the view automatically. The exact default remains open.
- The existing background-agent Dashboard remains for now. MultiView and the
  Dashboard should move toward a common agent model so they can be unified
  later without rewriting their state semantics.
- A progress display must report observed state and recent activity, not invent
  a percentage when the agent provides no measurable completion value.

## Proposed information hierarchy

Every card starts with the same compact identity and state header:

```text
WAIT  Codex  tmux-deck  feature/x  03:12
```

The body shows the latest meaningful activity digest and a tail of the live
pane. The selected card uses shape and border weight in addition to color.
Repository, worktree, and branch labels may collapse progressively when width
is limited.

The global header shows counts for actionable, working, and recently completed
agents. State is always encoded by a word or symbol as well as color.

## Proposed presentation modes

### Attention

Use a stable master-detail layout: an action queue lists agent panes ordered by
attention, and the selected pane receives the largest live preview. Working and
recently completed agents remain discoverable but visually subordinate.

The layout optimizes for noticing an action and entering its pane with one
command. It may reorder when an agent crosses an attention boundary.

### Overview

Use an adaptive grid of equal-size agent cards. Preserve card position while
the view is open; state changes update a card without reshuffling the whole
grid. Page rather than shrinking cards below a useful preview size.

The layout optimizes for spatial memory and ambient progress monitoring.

### Responsive floor

- Wide terminals may use a multi-column grid.
- At 80x24, show at most two useful columns.
- Near 60 columns, collapse to a single list/detail layout.
- Below the minimum size needed for identity, state, and controls, show an
  explicit terminal-too-small message.

## Domain language

**Agent Pane**
: A tmux pane in which a supported coding-agent process is detected. This is
  the identity and selection unit of MultiView.

**Agent Kind**
: The supported agent implementation, initially Claude Code or Codex. Kind is
  identity metadata, not a lifecycle state.

**Observed State**
: The latest lifecycle state derived from hooks or process detection, such as
  waiting, working, done, failed, or running-without-hook-data.

**Actionable Agent**
: An Agent Pane whose Observed State requires human intervention. The exact
  state set and priority order remain open.

**Activity Digest**
: A bounded, non-sensitive description of the latest observed action. It is
  not the full prompt or raw tool input.

**Completion Retention**
: The bounded interval during which a completed Agent Pane remains visible so
  the user can notice completion before it disappears.

**Presentation Mode**
: Either Attention or Overview. It changes ordering and layout, not the
  underlying monitored agents.

**Background Agent**
: A non-pane Claude session discovered from Claude's jobs data and currently
  shown by the Dashboard. It is outside the first MultiView scope.

## Open questions

- Which presentation mode opens by default, and how is it switched?
- Does Attention prioritize waiting over failed, or failed over waiting?
- Which actions are available directly from a card besides entering its pane?
- What is the expected and maximum number of simultaneous Agent Panes?
- Is Overview order grouped by repository, tmux session, or a user-defined
  pinned order?
- What exact Completion Retention default should be used, and is it
  configurable?
- What identity remains visible when repository, worktree, or branch data
  cannot be discovered?
- How should hookless but detected Agent Panes be distinguished from agents
  whose state is known?
