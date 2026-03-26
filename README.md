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
  A macOS notch companion that monitors your AI coding agents in real time.
</p>

## What it does

- Reacts to OpenClaw / Claude Code agent activity in real time (working, idle, waiting)
- Character lives beside the MacBook notch, animating when agents work and sleeping when idle
- Auto-discovers local OpenClaw agents with session lists, chat history, and daily calls/tokens charts
- Listens to local Claude Code sessions via hooks, view live conversations
- Connect to remote OpenClaw instances running on servers via SSH
- Custom GIF character animations, pair different agents with different characters
- Customizable island backgrounds with crop tool
- Completion & waiting sound effects
- Menu bar only — no dock icon

## Requirements

- macOS (MacBook with notch recommended)
- [OpenClaw](https://github.com/nicepkg/openclaw) and/or [Claude Code](https://docs.anthropic.com/en/docs/claude-code) installed

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
- macOS native APIs for notch positioning and window management

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
- [OpenClaw](https://github.com/nicepkg/openclaw) — the AI agent platform this app monitors

## License

MIT

---

<p align="center">
  <sub>Originally created at KAON Hackathon</sub>
</p>
