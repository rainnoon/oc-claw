<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh.md">中文</a> | <b>日本語</b> | <a href="./README.ko.md">한국어</a> | <a href="./README.es.md">Español</a> | <a href="./README.fr.md">Français</a>
</p>
<p align="center">
  AI コーディングエージェントをリアルタイムで監視する macOS メニューバー常駐デスクトップペット。
</p>

## 機能

- **ノッチペット** — MacBook のノッチ横にキャラクターが住み着き、エージェント稼働中はアニメーション、アイドル時は居眠り
- **OpenClaw モニタリング** — ローカルの OpenClaw エージェントを自動検出し、セッション一覧・チャット履歴・呼び出し数/トークン統計を表示
- **Claude Code モニタリング** — Hook 経由でローカル Claude Code セッションをリッスンし、リアルタイム会話を表示
- **リモートモード** — リモートサーバー上の OpenClaw インスタンスに接続
- **キャラクターシステム** — カスタム GIF アニメーション、エージェントごとに異なるキャラクターを割り当て
- **メニューバー専用** — Dock アイコンなし、ステータスバーのトレイアプリとして動作

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

## 技術スタック

- **Tauri v2** + **React** + **TypeScript**
- **Rust** バックエンドでシステム連携と API 通信を処理
- macOS ネイティブ API によるノッチ位置決めとウィンドウ管理

## 開発

```bash
cd frontend
npm install
npx tauri dev
```

## ライセンス

MIT

---

<p align="center">
  <img src="assets/powered-by-kaon.png" height="32" />
</p>
<p align="center">
  <sub>KAON Hackathon にて誕生</sub>
</p>
