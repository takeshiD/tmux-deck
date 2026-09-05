# Agent Monitor Multiview Design

Status: Accepted for implementation

## Intent

Refocus MultiPreview from a general tmux session overview into a monitor for
coding agents running in tmux panes. The view must support two complementary
loops:

1. Notice and handle agents that require user action.
2. Observe the overall activity of all running agents without losing spatial
   context.

This document is the implementation contract for the first Agent Monitor
release.

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
- Agent Monitor has two presentation modes:
  - **Attention** makes waiting and failed agents difficult to miss and quick
    to enter.
  - **Overview** keeps all active agents visible for ambient progress
    monitoring.
- Agent Monitor restores the last presentation mode selected by the user. `Tab`
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
- Design for four simultaneous Agent Panes as the normal case and thirty as a
  required verification target, not a visibility cap.
- The first version exposes only two card actions: `Enter` switches to the
  pane, and `f` temporarily focuses its live preview.
- Summary uses a borderless virtual list rather than a grid of bordered cards.
  Working rows retain a one-cell animated spinner; waiting, error, done, and
  hookless states use stable symbols and labels.
- Live previews require at least 44x10 cells. A Hybrid selected preview requires
  at least 60x12 cells; otherwise Overview uses Summary List.
- Summary List uses virtual scrolling rather than explicit pages. `j/k` moves
  one row, `PageUp/PageDown` moves one viewport, and `Home/End` moves to the
  first or last agent. The footer reports the visible range and total.
- Overview does not reorder when an off-screen agent becomes actionable. Its
  global header highlights the actionable count and directs the user to switch
  to Attention with `Tab`.
- Persist the last Presentation Mode as best-effort runtime state under
  `$XDG_STATE_HOME/tmux-deck/ui-state.json`, not by rewriting user config.
- Attention uses visual notification only. It must not emit a terminal bell.
- A detected agent without hook data appears as `RUN` / `state unavailable` in
  Overview and does not enter the Attention queue.
- When repository or worktree identity cannot be resolved, display the stable
  tmux `session:window.pane` identity.
- Within the same actionable state, Attention shows the agent that has waited
  longest first.
- A newly discovered agent is appended to its repository/worktree group in
  Overview; existing cards do not move.
- If the selected agent disappears, select the next agent in the same worktree,
  then the nearest adjacent group. Close focused preview and show a transient
  status message when its target disappears.
- `/` supports free-text matching across identity and activity plus structured
  `state:`, `agent:`, and `repo:` filters.
- Prefer `repository / worktree-or-branch` for display identity. Add a parent
  path only to disambiguate duplicate repository names.
- User-facing view names are **Sessions**, **Agent Monitor**, and
  **Background Agents**. Rename the internal `MultiPreview` variant to
  `AgentMonitor`; continue accepting the existing `multi` configuration value
  as a compatibility alias.
- A configurable `m` action opens Agent Monitor and replaces the double-Space
  gesture. Do not retain double-Space as a second binding.
- Pressing `m` toggles Sessions and Agent Monitor. From Background Agents, `m`
  opens Agent Monitor. The existing `d` action continues to open Background
  Agents.
- Thirty agents is a verification target, not a visibility cap. Continue to
  expose every detected agent through the virtual Summary List.
- Default state symbols and colors are `! WAIT` yellow, `× ERROR` red,
  Braille-spinner `WORK` cyan, `✓ DONE` green, and `● RUN` neutral/dim. Agent
  kind remains a text label; color communicates state only.
- `Esc` dismisses the innermost transient state first: filter input, then
  focused preview, then Agent Monitor itself.
- At 100 columns or wider, Attention uses a 34/66 horizontal queue/preview
  split. At 60-99 columns it uses a 35/65 vertical split. Below 60 columns it
  shows the queue alone and opens preview on demand with `f`.
- Agent Monitor settings live under `[agent_monitor]`. The first public setting
  is `completed_retention_secs = 600`; density minimums remain internal layout
  rules.
- With no detected agents, explain how to start an agent inside tmux and how to
  install hooks for detailed state. A detected hookless agent uses the `RUN`
  row instead of the empty state.
- The existing Background Agents view remains for now. Agent Monitor and
  Background Agents should move toward a common agent model so they can be
  unified later without rewriting their state semantics.
- A progress display must report observed state and recent activity, not invent
  a percentage when the agent provides no measurable completion value.

## Information hierarchy

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

## Presentation modes

### Attention

Use a stable master-detail layout: an action queue lists agent panes ordered by
attention, and the selected pane receives the largest live preview. Working and
recently completed agents remain discoverable but visually subordinate.

The layout optimizes for noticing an action and entering its pane with one
command. It may reorder when an agent crosses an attention boundary.

When no agent is actionable, Attention remains active and shows an `All clear`
message above the working and recently completed agents. It never switches to
Overview automatically.

At 100 columns or wider, the queue occupies 34% and the selected live preview
66% of the width. At 60-99 columns, the queue occupies the upper 35% and the
preview the lower 65%. Below 60 columns, render only the queue; `f` temporarily
opens the selected preview full-screen.

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
3. **Summary List** renders identity, state, elapsed time, and Activity Digest
   as a borderless virtual list. `f` temporarily replaces it with the selected
   live preview.

The decision is based on fit, not count alone. Initial target capacities for a
120x40 terminal are one to four agents for Live Grid, five to twelve for
Hybrid, and thirteen to thirty for Summary List. These are design targets, not
hard-coded thresholds: the layout computes how many cards meet the minimum
dimensions in the current frame.

Live preview cells require at least 44x10 terminal cells. Hybrid requires at
least 60x12 for the selected preview. Summary List is the fallback whenever
those minimums cannot be met. It uses virtual scrolling and reports a range
such as `1-22/30` rather than dividing agents into explicit pages.

In Summary List, Working is rendered with a one-cell Braille spinner and a
`WORK` label. All rows use a text label or stable symbol as well as color:

```text
! WAIT   Codex   tmux-deck/feature-auth    permission required   3m
× ERROR  Claude  tmux-deck/fix-cache       cargo test failed      8m
⠋ WORK   Codex   tmux-deck/feature-layout  editing ui.rs         42s
✓ DONE   Claude  tmux-deck/fix-config       completed              4m
● RUN    Codex   session:window.%pane       state unavailable       -
```

The spinner uses the existing shared animation tick; each row does not own a
timer. Redraw at animation cadence only while at least one visible agent is
Working.

Automatic density changes must preserve selection and reading order. To avoid
layout oscillation, density changes only after crossing a fit boundary, not in
response to changing agent state.

## Interaction contract

- `m`: enter or leave Agent Monitor. This is a normal configurable action and
  replaces the fixed double-Space gesture.
- `Tab`: switch Attention / Overview and persist the choice.
- `h/j/k/l` and arrow keys: move through agents using visual grid order.
- `PageUp/PageDown` and `Home/End`: navigate Summary List by viewport or
  boundary.
- `Enter`: switch the tmux client to the selected Agent Pane, following the
  existing exit-on-switch behavior.
- `f`: enter or leave a temporary focused live preview without changing the
  stored Presentation Mode.
- `/`: filter by free text or `state:`, `agent:`, and `repo:` tokens. Filtering
  changes the visible set, not the underlying stable Overview order.
- State transitions may reorder the Attention queue, but must not steal
  selection from the user. Overview never reorders solely because state
  changed.
- The contextual footer shows only actions available in the current mode.
- Agent Monitor writes the selected Presentation Mode to
  `$XDG_STATE_HOME/tmux-deck/ui-state.json` on change and loads it
  best-effort. State-file failure must never prevent startup.
- Escape handling is layered: cancel filter input first, then leave focused
  preview, then return from Agent Monitor to Sessions.
- From Sessions, `m` opens Agent Monitor; from Agent Monitor, `m` returns to
  Sessions; from Background Agents, `m` opens Agent Monitor directly.

## Capture and refresh budget

- Live Grid captures every visible Agent Pane because each card renders a live
  tail.
- Hybrid captures only the selected Agent Pane; peer summaries use hook state
  and Activity Digest.
- Summary List performs no pane capture unless focused preview is active.
- Focused preview captures only the selected Agent Pane.
- Agent discovery and repository/worktree metadata are resolved outside
  rendering and cached by stable pane identity. Rendering never launches git,
  tmux, or agent commands.

### Responsive floor

- Wide terminals may use a multi-column grid.
- At 80x24, show at most two useful columns.
- Near 60 columns, collapse to a single list/detail layout.
- Below the minimum size needed for identity, state, and controls, show an
  explicit terminal-too-small message.

## Empty state

When no Agent Pane is detected, render concise recovery guidance:

```text
No coding agents detected in tmux.

Start Claude Code or Codex inside a tmux pane.
Install hooks for detailed working/waiting status:
  tmux-deck hook install
```

Do not show this state when a supported process is detected without hooks;
render that pane as `RUN` / `state unavailable` instead.

## Configuration and runtime state

The public configuration surface begins with:

```toml
[agent_monitor]
completed_retention_secs = 600
```

Keep density minimums internal until real usage shows a need to configure them.
Persist only the last Attention/Overview choice in
`$XDG_STATE_HOME/tmux-deck/ui-state.json`. Filters, selection, scroll position,
and focused preview are session-transient.

## Domain language

**Agent Pane**
: A tmux pane in which a supported coding-agent process is detected. This is
  the identity and selection unit of Agent Monitor.

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
: Live Grid, Hybrid, or Summary List. It is derived from available terminal
  area and agent count within Overview and does not change the stored
  Presentation Mode.

**Background Agent**
: A non-pane Claude session discovered from Claude's jobs data and currently
  shown by Background Agents. It is outside the first Agent Monitor scope.

## Implementation sequence

1. Complete and merge the existing `codex-hooks` work as an independent change.
   It establishes shared Claude/Codex state, activity, and working-directory
   data without coupling that protocol work to the new layout.
2. Rebase Agent Monitor implementation on the resulting main branch.
3. Introduce the shared pane-agent projection and state tests before replacing
   the current MultiPreview renderer and navigation.
4. Implement Attention, then Overview density levels, then persistence and
   filtering. Verify each layer at fixed terminal sizes before adding the next.
5. Keep Background Agents separate, but adapt it to the common state vocabulary
   only where that does not expand the first release.

## Acceptance criteria

- Agent Monitor contains only supported coding-agent panes and represents each
  pane independently, including multiple agents within one tmux session.
- `m` performs the documented view transitions and is configurable. The old
  double-Space gesture does not switch views.
- Attention and Overview switch with `Tab`; the last choice survives restart
  without mutating the user's configuration file.
- Attention orders states and same-state age as documented, keeps user
  selection stable, and clearly exposes an actionable agent from Overview even
  when its row is off-screen.
- Overview selects Live Grid, Hybrid, or Summary List from available cell area
  while preserving selection and stable repository/worktree ordering.
- Summary List remains navigable with more than thirty agents and never hides
  a detected agent because a design target was exceeded.
- Working agents animate with one shared spinner tick. Non-working states do
  not animate, and no state transition emits a terminal bell.
- Pane capture follows the density budget and never originates in rendering.
- Completed agents disappear after the configured retention and reappear when
  a new working transition is observed.
- Hookless and incomplete identity data use the documented fallbacks without
  entering Attention or crashing.
- `Enter`, `f`, `/`, navigation, and layered `Esc` behavior match the contextual
  footer in every density and responsive layout.
- Sessions, Agent Monitor, and Background Agents are the user-facing names;
  the existing `multi` configuration value continues to load.

## Verification matrix

- Unit-test pane-to-agent projection, attention ordering, stable insertion,
  selection fallback, retention, filtering, persisted mode, and density choice.
- Render-test Attention with waiting, error, working, done, all-clear, and empty
  states.
- Render-test Overview with 1, 4, 5, 12, 13, 30, and 31 agents at 120x40; also
  test 80x24, a 60-column split, and the terminal-too-small boundary.
- Test duplicate repository names, missing Git identity, hookless processes,
  wide Unicode text, long Activity Digests, and disappearing panes.
- Verify capture requests for Live Grid, Hybrid, Summary List, and focused
  preview independently from rendered output.
- Verify mode persistence failure degrades safely and terminal cleanup remains
  correct on normal exit and errors.
