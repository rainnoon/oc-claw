<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <img src="assets/powered-by-kaon.png" height="28" />
</p>
<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh.md">中文</a> | <b>日本語</b> | <a href="./README.ko.md">한국어</a> | <a href="./README.es.md">Español</a> | <a href="./README.fr.md">Français</a>
</p>
<p align="center">
  AI コーディングエージェントをリアルタイムで監視する macOS ノッチコンパニオン。
</p>

## 機能

- OpenClaw / Claude Code エージェントの活動にリアルタイムで反応（稼働中・アイドル・待機中）
- MacBook のノッチ横にキャラクターが住み着き、エージェント稼働中はアニメーション、アイドル時は居眠り
- ローカルの OpenClaw エージェントを自動検出し、セッション一覧・チャット履歴・呼び出し数/トークン統計を表示
- Hook 経由でローカル Claude Code セッションをリッスンし、リアルタイム会話を表示
- SSH 経由でリモートサーバー上の OpenClaw インスタンスに接続
- カスタム GIF アニメーション、エージェントごとに異なるキャラクターを割り当て
- 島の背景をカスタマイズ可能、クロップツール対応
- 完了音＆待機音
- メニューバー専用 — Dock アイコンなし

## 必要条件

- macOS（ノッチ付き MacBook 推奨）
- [OpenClaw](https://github.com/nicepkg/openclaw) および/または [Claude Code](https://docs.anthropic.com/en/docs/claude-code) がインストール済み

## インストール

> [!TIP]
> **AI エージェント（OpenClaw / Claude Code）に伝えてください：**
>
> ```
> oc-claw をインストール: curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```

> [!NOTE]
> **またはターミナルで直接実行：**
>
> ```bash
> curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```
>
> [Releases](https://github.com/rainnoon/oc-claw/releases) から DMG を直接ダウンロードすることもできます。インストール後、`xattr -cr /Applications/oc-claw.app` を実行して macOS Gatekeeper を回避してください。

## 仕組み

```
OpenClaw Agents ──→ JSONL セッションファイル ──→ ヘルスポーリング ──→ 活動状態
Claude Code     ──→ Hooks (SessionStart/Stop) ──→ イベントパーサー ──→ 活動状態
                                                                          ↓
                                        アニメスプライト ← ステートマシン ← サウンドエフェクト
```

OC-Claw は OpenClaw のセッションファイルをポーリングしてエージェントの活動を検出し、インストールされた Hook を通じて Claude Code をリッスンします。活動状態がノッチ島のキャラクターアニメーションを駆動し、展開パネルでセッション詳細、チャット履歴、メトリクスを表示します。

## 技術スタック

- **Tauri v2** + **React** + **TypeScript** — フロントエンド
- **Rust** — バックエンド（システム連携、SSH トンネリング、API 通信）
- macOS ネイティブ API によるノッチ位置決めとウィンドウ管理

## 開発

```bash
cd frontend
npm install
npx tauri dev
```

## コントリビュート

バグ報告、機能提案、プルリクエストを歓迎します。

## クレジット

- [Notchi](https://github.com/sk-ruban/notchi) — ノッチコンパニオンコンセプトと草地島のデザインインスピレーション

## ライセンス

MIT

---

<p align="center">
  <sub>KAON Hackathon にて誕生</sub>
</p>
