#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:?usage: setup.sh <repo-root>}"
deck_binary="${2:?usage: setup.sh <repo-root> <tmux-deck-binary>}"
config_file="$repo_root/demo/config.toml"
pane_script="$repo_root/demo/pane.sh"

new_session() {
  local session="$1"
  local window="$2"
  local scenario="$3"
  local command
  printf -v command '%q %q' "$pane_script" "$scenario"
  tmux new-session -d -s "$session" -n "$window" "$command"
}

new_window() {
  local target="$1"
  local window="$2"
  local scenario="$3"
  local command
  printf -v command '%q %q' "$pane_script" "$scenario"
  tmux new-window -d -t "$target" -n "$window" "$command"
}

split_pane() {
  local target="$1"
  local scenario="$2"
  local command
  printf -v command '%q %q' "$pane_script" "$scenario"
  tmux split-window -d -h -t "$target" "$command"
}

new_session api server api
split_pane api:server worker
new_window api tests tests
tmux select-window -t api:server

new_session editor code editor
new_window editor git git
tmux select-window -t editor:code

new_session ops metrics ops
new_window ops release deploy
tmux select-window -t ops:metrics

tmux set-option -g status-style 'bg=#1a1b26,fg=#a9b1d6'
tmux set-option -g status-left '#[fg=#7aa2f7,bold] tmux-deck demo '
tmux set-option -g status-right '#[fg=#9ece6a] synthetic data '
tmux set-option -g status-left-length 24
tmux set-option -g status-right-length 24

printf -v popup_command "'%s' --config '%s' --interval 200" "$deck_binary" "$config_file"
tmux bind-key -n C-g display-popup -w 92% -h 88% -E "$popup_command"
