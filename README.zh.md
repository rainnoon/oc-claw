<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <img src="assets/powered-by-kaon.png" height="28" />
</p>
<p align="center">
  <a href="./README.md">English</a> | <b>中文</b> | <a href="./README.ja.md">日本語</a> | <a href="./README.ko.md">한국어</a> | <a href="./README.es.md">Español</a> | <a href="./README.fr.md">Français</a>
</p>
<p align="center">
  一个 macOS 刘海伴侣应用，实时监控你的 AI 编程 agent 工作状态。
</p>

## 功能

- 实时响应 OpenClaw / Claude Code agent 活动状态（工作、空闲、等待）
- 角色栖息在 MacBook 刘海旁边，工作时播放动画，休息时打盹
- 自动发现本地 OpenClaw agent，显示 session 列表、聊天记录、调用量/token 统计图表
- 通过 Hook 监听本地 Claude Code 会话，查看实时对话
- 通过 SSH 连接远程服务器上的 OpenClaw 实例
- 自定义角色 GIF 动画，将不同 agent 配对不同角色
- 可自定义岛屿背景，支持裁剪工具
- 完成提示音 & 等待提示音
- 纯状态栏应用，不占用程序坞

## 前置条件

- macOS（推荐带刘海的 MacBook）
- 已安装 [OpenClaw](https://github.com/nicepkg/openclaw) 和/或 [Claude Code](https://docs.anthropic.com/en/docs/claude-code)

## 安装

> [!TIP]
> **丢给你的 AI agent（OpenClaw / Claude Code）：**
>
> ```
> 帮我安装 oc-claw：curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```

> [!NOTE]
> **或自己在终端运行：**
>
> ```bash
> curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```
>
> 也可以从 [Releases](https://github.com/rainnoon/oc-claw/releases) 手动下载 DMG。安装后需运行 `xattr -cr /Applications/oc-claw.app` 解除 macOS 安全限制。

## 工作原理

```
OpenClaw Agents ──→ JSONL session 文件 ──→ 健康轮询 ──→ 活动状态
Claude Code     ──→ Hooks (SessionStart/Stop) ──→ 事件解析 ──→ 活动状态
                                                                    ↓
                                      角色动画 ← 状态机 ← 提示音效
```

OC-Claw 通过轮询 OpenClaw session 文件检测 agent 活动，并通过安装的 Hook 监听 Claude Code。活动状态驱动刘海岛屿上的角色动画，可展开面板查看 session 详情、聊天记录和统计数据。

## 技术栈

- **Tauri v2** + **React** + **TypeScript** — 前端
- **Rust** — 后端，负责系统交互、SSH 隧道和 API 通信
- macOS 原生 API 实现刘海定位与窗口管理

## 开发

```bash
cd frontend
npm install
npx tauri dev
```

## 贡献

欢迎提交 Bug 报告、功能建议和 Pull Request。

## 致谢

- [Notchi](https://github.com/sk-ruban/notchi) — 刘海伴侣概念和草地岛屿的设计灵感
- [OpenClaw](https://github.com/nicepkg/openclaw) — 本应用监控的 AI agent 平台

## 许可证

MIT

---

<p align="center">
  <sub>最初诞生于 KAON Hackathon</sub>
</p>
