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
x ERROR  Claude  tmux-deck/fix-cache       cargo test failed      8m
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
- MultiView writes the selected Presentation Mode to
  `$XDG_STATE_HOME/tmux-deck/ui-state.json` on change and loads it
  best-effort. State-file failure must never prevent startup.

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
: Live Grid, Hybrid, or Summary List. It is derived from available terminal
  area and agent count within Overview and does not change the stored
  Presentation Mode.

**Background Agent**
: A non-pane Claude session discovered from Claude's jobs data and currently
  shown by the Dashboard. It is outside the first MultiView scope.

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

## Open questions

- What layout and content does Attention show when no agent is actionable?
- Does `m` always return to Sessions when pressed inside Agent Monitor, and how
  does it behave when pressed inside Background Agents?
- Is thirty a tested design target or a hard visibility cap? Hiding additional
  detected agents would weaken the monitoring contract.
- Which state colors and symbols are the semantic defaults?
- Does `Esc` also leave focused preview, or only `f`?
- What split ratio should Attention use at wide and narrow sizes?
- Which configuration section owns completion retention and any future Agent
  Monitor settings?
- Should an empty MultiView explain hook installation, or only report that no
  coding agents are detected?
