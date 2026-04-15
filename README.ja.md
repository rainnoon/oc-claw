<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <a href="https://www.oc-claw.ai"><img src="https://img.shields.io/badge/公式サイトからダウンロード-oc--claw.ai-8A2BE2?style=for-the-badge" alt="Download" /></a>
</p>
<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh.md">中文</a> | <b>日本語</b> | <a href="./README.ko.md">한국어</a> | <a href="./README.es.md">Español</a> | <a href="./README.fr.md">Français</a>
</p>
<p align="center">
  AI コーディングエージェントを監視するデスクトップペット、macOS と Windows に対応。
</p>

<p align="center">
  <b>macOS</b>
</p>
<p align="center">
  <img src="https://github.com/user-attachments/assets/74b8bbf8-ddcf-4149-a91e-d18d5c24fec6" width="600" />
</p>
<p align="center">
  <b>Windows</b>
</p>
<p align="center">
  <img src="https://github.com/user-attachments/assets/71b1518d-d611-4b86-ba06-d78c719995db" width="600" />
</p>

## 機能

- OpenClaw / Claude Code / Codex / Cursor エージェントの活動にリアルタイムで反応（稼働中・アイドル・待機中）
- デスクトップにキャラクターが住み着き（macOS / Windows）、エージェント稼働中はアニメーション、アイドル時は居眠り
- ローカルの OpenClaw エージェントを自動検出し、セッション一覧・チャット履歴・呼び出し数/トークン統計を表示
- Hook 経由でローカル Claude Code、Codex、Cursor セッションをリッスンし、リアルタイム会話を表示
- SSH 経由でリモートサーバー上の OpenClaw インスタンスに接続
- カスタムアニメーション、エージェントごとに異なるキャラクターを割り当て
- 島の背景をカスタマイズ可能、クロップツール対応
- 完了音＆待機音

## 必要条件

- macOS または Windows
- [OpenClaw](https://github.com/nicepkg/openclaw)、[Claude Code](https://docs.anthropic.com/en/docs/claude-code)、[Codex](https://github.com/openai/codex)、[Cursor](https://www.cursor.com) のいずれかがインストール済み

## 仕組み

```
OpenClaw Agents ──→ JSONL セッションファイル ──→ ヘルスポーリング ──→ 活動状態
Claude Code     ──→ Hooks ──→ イベントパーサー ──→ 活動状態
Codex           ──→ Hooks ──→ イベントパーサー ──→ 活動状態
Cursor          ──→ Hooks ──→ イベントパーサー ──→ 活動状態
                                                      ↓
                  アニメスプライト ← ステートマシン ← サウンドエフェクト
```

OC-Claw は OpenClaw のセッションファイルをポーリングしてエージェントの活動を検出し、インストールされた Hook を通じて Claude Code、Codex、Cursor をリッスンします。活動状態がノッチ島のキャラクターアニメーションを駆動し、展開パネルでセッション詳細、チャット履歴、メトリクスを表示します。

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
- [Vibe Island](https://github.com/vibeislandapp/vibe-island) — インタラクションデザインの参考

## ライセンス

MIT

---

<p align="center">
  <img src="assets/powered-by-kaon.png" height="28" />
</p>
<p align="center">
  <sub>KAON Hackathon にて誕生</sub>
</p>
