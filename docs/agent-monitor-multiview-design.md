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
- MultiView restores the last presentation mode selected by the user. `Tab`
  switches between Attention and Overview; state changes never switch modes
  automatically.
- Attention orders cards by `Waiting > Error > Working > Done`.
- Completed agents remain visible for ten minutes by default and then leave the
  view automatically. Completion retention is configurable.
- Overview groups cards by repository, then worktree, then pane. Within a
  group, card positions remain stable while the view is open.
- Overview adapts its card content to both agent count and available terminal
  area. It may show all live previews, a selected live preview with summarized
  peers, or summary-only cards with an on-demand focused preview.
- Design for four simultaneous Agent Panes as the normal case and thirty as the
  supported maximum.
- The first version exposes only two card actions: `Enter` switches to the
  pane, and `f` temporarily focuses its live preview.
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

Use an adaptive grid of agent cards. Preserve card position while the view is
open; state changes update a card without reshuffling the whole grid. Page
rather than shrinking cards below a useful summary size.

The layout optimizes for spatial memory and ambient progress monitoring.

Overview selects one of three density levels from agent count and available
terminal area:

1. **Live Grid** renders a live terminal tail in every card when every visible
   card can retain a useful minimum width and height.
2. **Hybrid** gives the selected card a live preview and renders the remaining
   agents as compact activity summaries.
3. **Summary Grid** renders identity, state, elapsed time, and Activity Digest
   for all visible cards. `f` temporarily replaces it with the selected live
   preview.

The decision is based on fit, not count alone. Initial target capacities for a
120x40 terminal are one to four agents for Live Grid, five to twelve for
Hybrid, and thirteen to thirty for Summary Grid. These are design targets, not
hard-coded thresholds: the layout computes how many cards meet the minimum
dimensions in the current frame.

Automatic density changes must preserve selection and reading order. To avoid
layout oscillation, density changes only after crossing a fit boundary, not in
response to changing agent state.

## Interaction contract

- `Tab`: switch Attention / Overview and persist the choice.
- `h/j/k/l` and arrow keys: move through agents using visual grid order.
- `Enter`: switch the tmux client to the selected Agent Pane, following the
  existing exit-on-switch behavior.
- `f`: enter or leave a temporary focused live preview without changing the
  stored Presentation Mode.
- State transitions may reorder the Attention queue, but must not steal
  selection from the user. Overview never reorders solely because state
  changed.
- The contextual footer shows only actions available in the current mode.

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
: An Agent Pane whose Observed State requires human intervention. Waiting and
  error states are actionable; waiting has the higher display priority because
  user input can immediately resume blocked work.

**Activity Digest**
: A bounded, non-sensitive description of the latest observed action. It is
  not the full prompt or raw tool input.

**Completion Retention**
: The bounded interval during which a completed Agent Pane remains visible so
  the user can notice completion before it disappears. It defaults to ten
  minutes and is configurable.

**Presentation Mode**
: Either Attention or Overview. It changes ordering and layout, not the
  underlying monitored agents.

**Density Level**
: Live Grid, Hybrid, or Summary Grid. It is derived from available terminal
  area and agent count within Overview and does not change the stored
  Presentation Mode.

**Background Agent**
: A non-pane Claude session discovered from Claude's jobs data and currently
  shown by the Dashboard. It is outside the first MultiView scope.

## Open questions

- What minimum card dimensions make a live preview and a summary useful?
- How should pages be ordered and navigated when all Agent Panes do not fit?
- Where is the last Presentation Mode persisted, and should it be a config
  default, runtime state, or both?
- What identity remains visible when repository, worktree, or branch data
  cannot be discovered?
- How should hookless but detected Agent Panes be distinguished from agents
  whose state is known?
- Should a newly waiting Agent Pane produce only a visual notification, a
  terminal bell, or an optional external notification?
