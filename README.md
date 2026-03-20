<img src="icon.png" width="80" />

# OC-Claw

**English** | [中文](./README_CN.md)

A macOS menu bar desktop pet that monitors your AI coding agents in real time.

## Features

- **Notch Pet** — A character lives beside the MacBook notch, animating when agents are working and sleeping when idle
- **OpenClaw Monitoring** — Auto-discovers local OpenClaw agents, displays session lists, chat history, and daily calls/tokens charts
- **Claude Code Monitoring** — Listens to local Claude Code sessions via hooks, view live conversations
- **Remote Mode** — Connect to OpenClaw instances running on remote servers
- **Character System** — Custom GIF animations, pair different agents with different characters
- **Menu Bar Only** — No dock icon, runs as a status bar tray app

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
