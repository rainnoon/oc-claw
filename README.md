<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <b>English</b> | <a href="./README.zh.md">中文</a> | <a href="./README.ja.md">日本語</a> | <a href="./README.ko.md">한국어</a> | <a href="./README.es.md">Español</a> | <a href="./README.fr.md">Français</a>
</p>
<p align="center">
  A macOS menu bar desktop pet that monitors your AI coding agents in real time.
</p>

## Features

- **Notch Pet** — A character lives beside the MacBook notch, animating when agents are working and sleeping when idle
- **OpenClaw Monitoring** — Auto-discovers local OpenClaw agents, displays session lists, chat history, and daily calls/tokens charts
- **Claude Code Monitoring** — Listens to local Claude Code sessions via hooks, view live conversations
- **Remote Mode** — Connect to OpenClaw instances running on remote servers
- **Character System** — Custom GIF animations, pair different agents with different characters
- **Menu Bar Only** — No dock icon, runs as a status bar tray app

## Install

> [!TIP]
> **Tell your AI agent (OpenClaw / Claude Code):**
>
> ```
> Install oc-claw: curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```

> [!NOTE]
> **Or run it yourself in Terminal:**
>
> ```bash
> curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```
>
> You can also download the DMG from [Releases](https://github.com/rainnoon/oc-claw/releases). After installing, run `xattr -cr /Applications/oc-claw.app` to bypass macOS Gatekeeper.

## Tech Stack

- **Tauri v2** + **React** + **TypeScript**
- **Rust** backend for system interaction and API communication
- macOS native APIs for notch positioning and window management

## Development

```bash
cd frontend
npm install
npx tauri dev
```

## License

MIT

---

<p align="center">
  <img src="assets/powered-by-kaon.png" height="32" />
</p>
<p align="center">
  <sub>Originally created at KAON Hackathon</sub>
</p>
