# 計画: Claude hook によるツリービューマーカーの動的変化

## 目的

tmux-deck のツリービュー（sessions / windows / panes の3リスト）に表示する
Claude マーカーを、Claude Code の **hook イベント**が報告する状態
（作業中・入力待ち・完了・エラー等）に応じて、記号と色で出し分ける。

現状は「Claude プロセスが動いているか / いないか」の 2 状態のみ
（オレンジの `●`）。これを多状態化して、ユーザーがどのセッションが
注目を必要としているか（許可待ち・完了）を一目で把握できるようにする。

## 現状の実装（参照）

- `src/actor/tmux_actor.rs:844` `annotate_claude_panes()`
  `ps -eo pid=,ppid=,args=` でプロセスツリーを走査し、pane の子孫に
  `claude` プロセスがあれば `has_claude = true` をセット。
- `src/app.rs:13-60` `TmuxPane / TmuxWindow / TmuxSession` が
  `has_claude: bool` を保持。
- `src/ui.rs:8-13` 定数 `CLAUDE_MARKER = "●"`,
  `CLAUDE_MARKER_COLOR = Color::Indexed(208)`（オレンジ）。
  `render_sessions_list` / `render_windows_list` / `render_panes_list` が
  `has_claude` のとき `●` を付与。

## Claude Code の hook 概要

### 利用するイベント（要：実装前に公式ドキュメントで最終確定）

公式: https://code.claude.com/docs/en/hooks.md

| イベント | 発火タイミング | 遷移先の状態 |
|---|---|---|
| `UserPromptSubmit` | プロンプト送信時（処理開始前） | Working |
| `PreToolUse` / `PostToolUse` | ツール実行前後（matcher 絞り込み可） | Working（継続） |
| `Stop` | Claude が応答を完了したとき | Idle/Done |
| `Notification` | 許可待ち / アイドル待ちの通知時 | Waiting（要注目） |
| `SubagentStop` | サブエージェント終了時 | 補助 |
| `SessionStart` / `SessionEnd` | セッション開始 / 終了 | 状態の初期化 / クリア |

> 注: 調査では `StopFailure` などの追加イベントも候補に挙がったが
> 確実性が低いため、実装着手時に公式ドキュメントで存在と入力スキーマを
> 確定してから採用する。確定までは上記の安定イベントで設計する。

### hook が受け取る入力（stdin の JSON）

共通フィールド: `session_id`, `cwd`, `transcript_path`,
`hook_event_name`, `permission_mode`。

- `Stop`: `stop_reason`（`end_turn` 等）
- `Notification`: `notification_type`（`permission_prompt` / `idle_prompt` 等）

### インストール方法

hook は `settings.json` の `hooks` キーで定義。優先順位（高い順）:

1. `.claude/settings.local.json`（プロジェクト・gitignore）
2. `.claude/settings.json`（プロジェクト・コミット可）
3. `~/.claude/settings.json`（ユーザーグローバル）

設定例:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "tmux-deck hook report" }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "tmux-deck hook report" }] }
    ],
    "Notification": [
      { "hooks": [{ "type": "command", "command": "tmux-deck hook report" }] }
    ],
    "SubagentStop": [
      { "hooks": [{ "type": "command", "command": "tmux-deck hook report" }] }
    ],
    "SessionEnd": [
      { "hooks": [{ "type": "command", "command": "tmux-deck hook report" }] }
    ]
  }
}
```

hook コマンドは終了コード 0 で正常。stdin から JSON を受け取る。

## 設計

### 連携の鍵: `$TMUX_PANE`

hook スクリプト（= `tmux-deck hook report`）は Claude のサブプロセスとして
その pane の環境で実行されるため、環境変数 **`$TMUX_PANE`**（例 `%3`）を
読める。Claude の `session_id` は tmux pane と無関係なので、`$TMUX_PANE` を
キーにして「どの hook イベントがどの tmux pane に属するか」を結びつける。
`$TMUX_PANE` が無い（tmux 外で起動）場合は何もせず正常終了する。

### 状態ファイル（決定: `$XDG_STATE_HOME`）

- 保存先: `${XDG_STATE_HOME:-~/.local/state}/tmux-deck/claude/<pane_id>.json`
  - `<pane_id>` は `$TMUX_PANE` の `%` を除いたもの等、ファイル名に安全な形へ正規化。
- 内容例:

  ```json
  { "state": "waiting", "ts": 1730000000, "reason": "idle_prompt", "session_id": "..." }
  ```

- `ts` は更新時刻（epoch 秒）。一定時間（例: 6 時間）更新が無いもの、
  および対応する pane が存在しないものは tmux-deck 側でクリーンアップ。

### データフロー

```
Claude の状態変化
  → hook 発火 → `tmux-deck hook report` 実行
      → stdin の JSON をパース + $TMUX_PANE 取得
      → 状態ファイル書き込み（pane 単位）

tmux-deck 本体
  → refresh 時に状態ディレクトリを走査
  → pane.id でマッチング → ClaudeState を pane に付与
  → window / session は子の「最も注目すべき」状態へ集約
  → ui.rs が状態 → (記号, 色) を出し分けて描画
```

### 状態定義 `ClaudeState`

| 状態 | 由来 | マーク | 色（案） |
|---|---|---|---|
| `Working` | `UserPromptSubmit` / `PreToolUse` | `●` | オレンジ (208) |
| `Waiting` | `Notification`(permission/idle) | `◆` | 黄 |
| `Idle` / `Done` | `Stop` (end_turn) | `●` | 緑 |
| `Error` | （要確認）`StopFailure` 等 | `✗` | 赤 |
| `Unknown` | プロセス検出のみ（hook 未設定） | `●` | グレー / オレンジ |

- hook 状態を優先し、無ければ既存のプロセス検出（`has_claude`）に
  フォールバックする。これにより hook 未設定環境でも従来どおり動作。
- window / session への集約は注目度順
  （Waiting > Error > Working > Done > Unknown など）で最大値を採用。

## 実装ステップ（次フェーズ）

1. **CLI サブコマンド追加** (`src/cli.rs`)
   - `tmux-deck hook report` … hook 本体（stdin を受けて状態ファイル書込）
   - `tmux-deck hook install [--user|--project]` … settings.json へ自動追記
     （デフォルト: `--user` = `~/.claude/settings.json`）
2. **`hook report` 実装**
   - stdin JSON パース + `$TMUX_PANE` 取得 + 状態ファイル書込。
   - pane 不明時は no-op。依存最小・高速。
3. **状態ストア読込** (`src/actor/tmux_actor.rs`)
   - refresh 時に状態ディレクトリを読み、`pane.id` にマッピング。
   - `annotate_claude_panes` を拡張し `claude_state` をセット＋古いファイルの掃除。
4. **データ構造拡張** (`src/app.rs`)
   - `TmuxPane / TmuxWindow / TmuxSession` に `claude_state` を追加
     （window/session は集約値）。
5. **UI 描画** (`src/ui.rs`)
   - `ClaudeState -> (記号, Color)` のマッピング関数を追加。
   - 3 リストの描画を更新。既存の `CLAUDE_MARKER*` 定数を整理。
6. **`hook install` 実装**
   - 既存 settings.json を読み、`hooks` をマージして書き戻す
     （既存設定を壊さない・冪等）。
7. **テスト**
   - JSON パース、状態集約、settings.json マージの単体テスト。
   - `cargo test` / `cargo clippy`。
8. **ドキュメント** (`README.md`)
   - `tmux-deck hook install` のセットアップ手順とマーカーの意味を追記。
   - README の `LLM Integration`（現状未チェック）を進捗反映。

## 決定事項（このフェーズで確定）

- 状態ファイル保存先: **`$XDG_STATE_HOME`**（未設定時 `~/.local/state`）。
- hook 自動インストール先デフォルト: **ユーザーグローバル `~/.claude/settings.json`**。
- 今フェーズのスコープ: **本計画ドキュメントの確定まで**。実装は次フェーズ。

## 未確定・実装時に確定する事項

- 採用する hook イベントの最終セット（`StopFailure` 等の存在確認）。
- 各状態の最終的な記号・配色（点滅相当の表現可否含む）。
- 状態ファイルのクリーンアップ間隔・保持時間の具体値。
