#!/usr/bin/env bash
set -euo pipefail

scenario="${1:?usage: pane.sh <scenario>}"
tick=1

while true; do
  printf '\033[2J\033[H'
  case "$scenario" in
    api)
      printf '\033[1;36m API SERVER \033[0m  http://127.0.0.1:8080\n\n'
      printf '  \033[32mGET\033[0m  /health       \033[32m200\033[0m   2ms\n'
      printf '  \033[32mGET\033[0m  /sessions     \033[32m200\033[0m  11ms\n'
      printf '  \033[33mPOST\033[0m /preview      \033[32m202\033[0m  18ms\n\n'
      printf '\033[2mrequest stream · synthetic #%03d\033[0m\n' "$tick"
      ;;
    worker)
      printf '\033[1;35m PREVIEW WORKER \033[0m\n\n'
      printf '  queue     \033[32mhealthy\033[0m\n'
      printf '  jobs      %d active\n' "$((tick % 4 + 2))"
      printf '  latency   %dms\n\n' "$((tick % 7 + 8))"
      printf '\033[2mrendering pane frames…\033[0m\n'
      ;;
    tests)
      printf '\033[1;32m TEST SUITE \033[0m\n\n'
      printf '  ✓ session tree\n  ✓ pane capture\n  ✓ key bindings\n  ✓ theme parser\n\n'
      printf '\033[32m4 passed\033[0m · finished in 0.%02ds\n' "$((tick % 30 + 20))"
      ;;
    editor)
      printf '\033[1;34m tmux-deck / src/ui.rs \033[0m\n\n'
      printf '  \033[35mfn\033[0m render_preview(frame: &\033[36mmut\033[0m Frame) {\n'
      printf '      frame.render_widget(deck, area);\n'
      printf '  }\n\n'
      printf '\033[2mNORMAL  main  Rust  Ln 128, Col 9\033[0m\n'
      ;;
    git)
      printf '\033[1;33m WORKTREE \033[0m\n\n'
      printf '  \033[32mM\033[0m README.md\n'
      printf '  \033[32mA\033[0m demo/tmux-deck.tape\n'
      printf '  \033[32mA\033[0m assets/tmux-deck-demo.gif\n\n'
      printf '\033[2mmain · demo fixture\033[0m\n'
      ;;
    ops)
      bars=$((tick % 5 + 4))
      printf '\033[1;36m SYSTEM OVERVIEW \033[0m\n\n'
      printf '  cpu   \033[34m%*s\033[0m%*s  %d%%\n' "$bars" '' "$((12 - bars))" '' "$((bars * 6))"
      printf '  mem   \033[32m██████\033[0m       48%%\n'
      printf '  net   \033[35m███\033[0m          21%%\n\n'
      printf '\033[2mall systems operational · sample %03d\033[0m\n' "$tick"
      ;;
    deploy)
      printf '\033[1;32m RELEASE PIPELINE \033[0m\n\n'
      printf '  ✓ checks\n  ✓ linux build\n  ✓ macOS build\n'
      printf '  ◉ publish preview\n\n'
      printf '\033[2martifacts are synthetic\033[0m\n'
      ;;
    *)
      printf 'unknown demo scenario: %s\n' "$scenario" >&2
      exit 1
      ;;
  esac
  tick=$((tick + 1))
  sleep 0.7
done
