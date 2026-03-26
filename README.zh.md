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
  一个 macOS 菜单栏桌宠应用，实时监控你的 AI 编程 agent 工作状态。
</p>

## 功能

- **刘海桌宠** — 角色栖息在 MacBook 刘海旁边，工作时播放动画，休息时打盹
- **OpenClaw 监控** — 自动发现本地 OpenClaw agent，显示 session 列表、聊天记录、调用量/token 统计图表
- **Claude Code 监控** — 通过 Hook 监听本地 Claude Code 会话，查看实时对话
- **远程模式** — 连接远程服务器上的 OpenClaw 实例
- **角色系统** — 自定义角色 GIF 动画，将不同 agent 配对不同角色
- **纯状态栏应用** — 不占用程序坞，仅在菜单栏显示托盘图标

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

## 技术栈

- **Tauri v2** + **React** + **TypeScript**
- **Rust** 后端处理系统交互与 API 通信
- macOS 原生 API 实现刘海定位与窗口管理

## 开发

```bash
cd frontend
npm install
npx tauri dev
```

## 许可证

MIT

---

<p align="center">
  <sub>最初诞生于 KAON Hackathon</sub>
</p>
