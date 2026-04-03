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
  AI コーディングエージェントを監視するデスクトップペット、macOS と Windows に対応。
</p>

## 機能

- OpenClaw / Claude Code エージェントの活動にリアルタイムで反応（稼働中・アイドル・待機中）
- デスクトップにキャラクターが住み着き（macOS / Windows）、エージェント稼働中はアニメーション、アイドル時は居眠り
- ローカルの OpenClaw エージェントを自動検出し、セッション一覧・チャット履歴・呼び出し数/トークン統計を表示
- Hook 経由でローカル Claude Code セッションをリッスンし、リアルタイム会話を表示
- SSH 経由でリモートサーバー上の OpenClaw インスタンスに接続
- カスタムアニメーション、エージェントごとに異なるキャラクターを割り当て
- 島の背景をカスタマイズ可能、クロップツール対応
- 完了音＆待機音

## 必要条件

- macOS または Windows
- [OpenClaw](https://github.com/nicepkg/openclaw) および/または [Claude Code](https://docs.anthropic.com/en/docs/claude-code) がインストール済み

## インストール

**[oc-claw.ai](https://www.oc-claw.ai)** からダウンロードしてください。

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
- macOS / Windows ネイティブ API によるウィンドウ管理

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
