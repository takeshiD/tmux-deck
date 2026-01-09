# Tmux-Deck アクターモデル再構築計画

## 概要

同期的な実装をUIActor、TmuxActor、RefreshActorの3つのアクターモデルに再構築する。

## アーキテクチャ

```
┌─────────────────────────────────────────────────────────────┐
│                      Main (tokio runtime)                    │
└─────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ▼                     ▼                     ▼
┌───────────────┐     ┌───────────────┐     ┌───────────────┐
│   UIActor     │     │  TmuxActor    │     │ RefreshActor  │
│               │     │               │     │               │
│ - キーイベント │────▶│ - tmux操作    │◀────│ - 定期更新    │
│ - UI描画     │◀────│ - データ取得  │     │ - タイマー    │
│ - 状態管理   │     │ - コマンド実行│     │               │
└───────────────┘     └───────────────┘     └───────────────┘
```

## モジュール構造

```
src/
├── main.rs              # tokioランタイム起動、アクター生成
├── app.rs               # App → UIState（tmux操作削除）
├── ui.rs                # 変更なし（型エイリアスで互換性維持）
├── cli.rs               # 変更なし
└── actor/               # 新規ディレクトリ
    ├── mod.rs           # モジュール公開
    ├── messages.rs      # TmuxCommand, TmuxResponse, UIEvent
    ├── tmux_actor.rs    # TmuxActor実装
    ├── ui_actor.rs      # UIActor実装
    └── refresh_actor.rs # RefreshActor実装
```

## 実装進捗

| タスク | ステータス | 備考 |
|--------|-----------|------|
| actor/messages.rs | ✅ 完了 | TmuxCommand, TmuxResponse, UIEvent, RefreshControl |
| actor/mod.rs | ✅ 完了 | モジュール公開 |
| actor/tmux_actor.rs | ✅ 完了 | tokio::process::Commandで非同期化 |
| actor/refresh_actor.rs | ✅ 完了 | tokio::time::intervalで定期更新 |
| app.rs | ✅ 完了 | App→UIState、tmux操作削除、型エイリアス追加 |
| actor/ui_actor.rs | ✅ 完了 | tokio::select!、専用キーポーラースレッド |
| main.rs | ✅ 完了 | #[tokio::main]、3アクター起動 |
| ui.rs | ✅ 完了 | 変更不要（型エイリアスで互換） |
| ビルド・テスト | ✅ 完了 | 警告なし |

## 修正履歴

### 1. キー入力の反応が悪い問題
**原因**: `tokio::select!`がデフォルトでランダムにブランチを選択
**修正**: `biased;`を追加してキーイベントを最優先に

### 2. 連続キー入力で遅延する問題
**原因**: 毎回`spawn_blocking`でキーポーリング → オーバーヘッド大
**修正**: 専用スレッドでキーイベントをポーリングし、チャネル経由で送信

```rust
// Before: 毎ループspawn_blocking
key = spawn_blocking(|| poll(16ms))

// After: 専用スレッド + チャネル
std::thread::spawn(|| loop { poll(10ms) → tx.send() })
key_rx.recv()  // チャネルから受信するだけ
```

## チャネル構成

```
┌────────────────────────────────────────┐
│           TmuxActor                    │
│  rx: Receiver<TmuxCommand>             │
│  tx: Sender<TmuxResponse>              │
└────────────────────────────────────────┘
       ▲                    │
       │ TmuxCommand        │ TmuxResponse
       │                    ▼
┌──────┴────────────┐   ┌─────────────────────────────┐
│   RefreshActor    │   │         UIActor             │
│  tmux_tx          │   │  tmux_tx: Sender<TmuxCmd>   │
│  ui_tx            │   │  tmux_rx: Receiver<TmuxResp>│
└───────────────────┘   │  event_rx: Receiver<UIEvent>│
       │                │  key_rx: Receiver<Event>    │ ← 専用スレッド
       │ UIEvent        │  refresh_control            │
       └────────────────┴─────────────────────────────┘
```

## メッセージ型

### TmuxCommand（UIActor/RefreshActor → TmuxActor）
- `RefreshAll` - 全セッション/ウィンドウ/ペイン取得
- `CapturePane { target }` - ペインコンテンツ取得
- `NewSession { name }` - セッション作成
- `RenameSession { old_name, new_name }` - セッション名変更
- `KillSession { name }` - セッション削除
- `SendKeys { target, keys }` - キー送信
- `SwitchClient { target }` - クライアント切り替え

### TmuxResponse（TmuxActor → UIActor）
- `SessionsRefreshed { sessions }` - セッションデータ更新
- `PaneCaptured { target, content }` - ペインコンテンツ
- `SessionCreated/Renamed/Killed { success, error }` - 操作結果
- `Error { message }` - エラー通知

### UIEvent（RefreshActor → UIActor）
- `Tick` - 定期更新トリガー
- `RequestCapture` - ペインキャプチャ要求
- `Shutdown` - 終了シグナル
