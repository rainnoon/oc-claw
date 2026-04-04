<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <img src="assets/powered-by-kaon.png" height="28" />
</p>
<p align="center">
  <b>English</b> | <a href="./README.zh.md">中文</a> | <a href="./README.ja.md">日本語</a> | <a href="./README.ko.md">한국어</a> | <a href="./README.es.md">Español</a> | <a href="./README.fr.md">Français</a>
</p>
<p align="center">
  A desktop pet that monitors your AI coding agents in real time. Supports macOS and Windows.
</p>

<p align="center">
  <img src="https://github.com/user-attachments/assets/71b1518d-d611-4b86-ba06-d78c719995db" width="600" />
</p>

## Install

Download the latest version from the official website: **[oc-claw.ai](https://www.oc-claw.ai)**

## What it does

- Reacts to OpenClaw / Claude Code agent activity in real time (working, idle, waiting)
- Desktop pet character animates when agents work and sleeps when idle (macOS notch or Windows taskbar)
- Auto-discovers local OpenClaw agents with session lists, chat history, and daily calls/tokens charts
- Listens to local Claude Code sessions via hooks, view live conversations
- Connect to remote OpenClaw instances running on servers via SSH
- Custom character animations, pair different agents with different characters
- Customizable island backgrounds with crop tool
- Completion & waiting sound effects

## Requirements

- macOS or Windows
- [OpenClaw](https://github.com/nicepkg/openclaw) and/or [Claude Code](https://docs.anthropic.com/en/docs/claude-code) installed

## How it works

```
OpenClaw Agents ──→ JSONL session files ──→ Health polling ──→ Activity state
Claude Code     ──→ Hooks (SessionStart/Stop) ──→ Event parser ──→ Activity state
                                                                        ↓
                                        Animated sprites ← State machine ← Sound effects
```

OC-Claw polls OpenClaw session files to detect agent activity, and listens to Claude Code via installed hooks. Activity states drive character animations on the notch island, with an expandable panel for session details, chat history, and metrics.

## Tech Stack

- **Tauri v2** + **React** + **TypeScript** — frontend
- **Rust** — backend for system interaction, SSH tunneling, and API communication
- macOS / Windows native APIs for window management and positioning

## Development

```bash
cd frontend
npm install
npx tauri dev
```

## Contributing

Bug reports, feature suggestions, and pull requests are welcome.

## Credits

- [Notchi](https://github.com/sk-ruban/notchi) — design inspiration for notch companion concept and grass island

## License

MIT

---

<p align="center">
  <sub>Originally created at KAON Hackathon</sub>
</p>
