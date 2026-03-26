<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh.md">中文</a> | <a href="./README.ja.md">日本語</a> | <b>한국어</b> | <a href="./README.es.md">Español</a> | <a href="./README.fr.md">Français</a>
</p>
<p align="center">
  AI 코딩 에이전트를 실시간으로 모니터링하는 macOS 메뉴 바 데스크톱 펫.
</p>

## 기능

- **노치 펫** — MacBook 노치 옆에 캐릭터가 살며, 에이전트 작업 중에는 애니메이션, 유휴 시에는 졸기
- **OpenClaw 모니터링** — 로컬 OpenClaw 에이전트를 자동 감지하고, 세션 목록, 채팅 기록, 일일 호출/토큰 통계 표시
- **Claude Code 모니터링** — Hook을 통해 로컬 Claude Code 세션을 수신하고 실시간 대화 표시
- **원격 모드** — 원격 서버의 OpenClaw 인스턴스에 연결
- **캐릭터 시스템** — 커스텀 GIF 애니메이션, 에이전트별로 다른 캐릭터 페어링
- **메뉴 바 전용** — Dock 아이콘 없이 상태 바 트레이 앱으로 실행

## 설치

> [!TIP]
> **AI 에이전트(OpenClaw / Claude Code)에게 전달하세요:**
>
> ```
> oc-claw 설치: curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```

> [!NOTE]
> **또는 터미널에서 직접 실행:**
>
> ```bash
> curl -fsSL https://raw.githubusercontent.com/rainnoon/oc-claw/main/install.sh | bash
> ```
>
> [Releases](https://github.com/rainnoon/oc-claw/releases)에서 DMG를 직접 다운로드할 수도 있습니다. 설치 후 `xattr -cr /Applications/oc-claw.app`을 실행하여 macOS Gatekeeper를 우회하세요.

## 기술 스택

- **Tauri v2** + **React** + **TypeScript**
- **Rust** 백엔드로 시스템 상호작용 및 API 통신 처리
- macOS 네이티브 API로 노치 위치 지정 및 윈도우 관리

## 개발

```bash
cd frontend
npm install
npx tauri dev
```

## 라이선스

MIT

---

<p align="center">
  <img src="assets/powered-by-kaon.png" height="32" />
</p>
<p align="center">
  <sub>KAON Hackathon에서 탄생</sub>
</p>
