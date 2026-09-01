#!/usr/bin/env bash
set -euo pipefail

repo_root="$PWD"
if [[ ! -f "$repo_root/demo/tmux-deck.tape" ]]; then
  echo "Run this command from the tmux-deck repository root." >&2
  exit 1
fi

demo_tmp="$(mktemp -d "${TMPDIR:-/tmp}/tmux-deck-demo.XXXXXX")"
export TMUX_TMPDIR="$demo_tmp/tmux"
export XDG_CONFIG_HOME="$demo_tmp/config"
export XDG_STATE_HOME="$demo_tmp/state"
mkdir -p "$TMUX_TMPDIR" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
unset TMUX

demo_socket=""

cleanup() {
  if [[ -n "$demo_socket" ]]; then
    tmux -S "$demo_socket" kill-server >/dev/null 2>&1 || true
  fi
  rm -r -- "$demo_tmp"
}
trap cleanup EXIT

deck_binary="$(command -v tmux-deck)"
bash "$repo_root/demo/setup.sh" "$repo_root" "$deck_binary"

demo_socket="$(tmux display-message -p '#{socket_path}')"
case "$demo_socket" in
  "$TMUX_TMPDIR"/*) ;;
  *)
    echo "Refusing to record against a tmux socket outside $TMUX_TMPDIR" >&2
    exit 1
    ;;
esac

vhs "$repo_root/demo/tmux-deck.tape"
vhs "$repo_root/demo/popup.tape"

printf 'Generated assets/tmux-deck-demo.gif and two screenshots.\n'
