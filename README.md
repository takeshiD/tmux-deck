# Tmux Deck
tmux-deck is a tmux session manager. 
Monitoring multi session Realtime preview.


# Features
- 🗂️ Tmux Session Management(New, Rename, Kill)
- 👀 Realtime Preview
- ⚙️ Easy Configure

# Quick Start

```bash
tmux-deck
```

![session manager](assets/tmux-deck_session_manager.png)


## Using in tmux popup
Add following key-bind in your `.tmux.conf`, `tmux-deck` would start up on tmux popup.

```bash
bind SPACE run-shell "tmux popup -w80% -h80% -E tmux-deck"
```

![popup](assets/tmux-deck_popup.png)

# Installation
## `cargo`
```
cargo install tmux-deck
```

## `nix`(Planed)
```
nix run github:takeshid/tmux-deck
```

## `npm`(Planed)
```
npm install -g markdown-peek
```

## download prebuild-binary
```
curl -SL https://github.com/takeshid/markdown-peek
```

# Status
- [ ] Session Management(New, Rename, Kill)
- [ ] Realtime Preview
- [ ] Multi Preview

# License
MIT License.
See [LICENSE](LICENSE).
