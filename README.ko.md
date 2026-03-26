<p align="center">
  <img src="icon.png" width="80" />
</p>
<h1 align="center">OC-Claw</h1>
<p align="center">
  <img src="assets/powered-by-kaon.png" height="28" />
</p>
<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh.md">中文</a> | <a href="./README.ja.md">日本語</a> | <b>한국어</b> | <a href="./README.es.md">Español</a> | <a href="./README.fr.md">Français</a>
</p>
<p align="center">
  AI 코딩 에이전트를 실시간으로 모니터링하는 macOS 노치 컴패니언.
</p>

## 기능

- OpenClaw / Claude Code 에이전트 활동에 실시간 반응 (작업 중, 유휴, 대기)
- MacBook 노치 옆에 캐릭터가 살며, 에이전트 작업 중에는 애니메이션, 유휴 시에는 졸기
- 로컬 OpenClaw 에이전트를 자동 감지하고, 세션 목록, 채팅 기록, 일일 호출/토큰 통계 표시
- Hook을 통해 로컬 Claude Code 세션을 수신하고 실시간 대화 표시
- SSH를 통해 원격 서버의 OpenClaw 인스턴스에 연결
- 커스텀 GIF 캐릭터 애니메이션, 에이전트별로 다른 캐릭터 페어링
- 섬 배경 커스터마이즈 가능, 크롭 도구 지원
- 완료 알림음 & 대기 알림음
- 메뉴 바 전용 — Dock 아이콘 없음

## 요구 사항

- macOS (노치가 있는 MacBook 권장)
- [OpenClaw](https://github.com/nicepkg/openclaw) 및/또는 [Claude Code](https://docs.anthropic.com/en/docs/claude-code) 설치 필요

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

## 작동 방식

```
OpenClaw Agents ──→ JSONL 세션 파일 ──→ 헬스 폴링 ──→ 활동 상태
Claude Code     ──→ Hooks (SessionStart/Stop) ──→ 이벤트 파서 ──→ 활동 상태
                                                                       ↓
                                       애니메이션 스프라이트 ← 상태 머신 ← 사운드 효과
```

OC-Claw는 OpenClaw 세션 파일을 폴링하여 에이전트 활동을 감지하고, 설치된 Hook을 통해 Claude Code를 수신합니다. 활동 상태가 노치 섬의 캐릭터 애니메이션을 구동하며, 확장 패널에서 세션 상세, 채팅 기록, 메트릭을 확인할 수 있습니다.

## 기술 스택

- **Tauri v2** + **React** + **TypeScript** — 프론트엔드
- **Rust** — 백엔드 (시스템 상호작용, SSH 터널링, API 통신)
- macOS 네이티브 API로 노치 위치 지정 및 윈도우 관리

## 개발

```bash
cd frontend
npm install
npx tauri dev
```

## 기여

버그 리포트, 기능 제안, 풀 리퀘스트를 환영합니다.

## 크레딧

- [Notchi](https://github.com/sk-ruban/notchi) — 노치 컴패니언 컨셉과 잔디 섬의 디자인 영감
- [OpenClaw](https://github.com/nicepkg/openclaw) — 이 앱이 모니터링하는 AI 에이전트 플랫폼

## 라이선스

MIT

---

<p align="center">
  <sub>KAON Hackathon에서 탄생</sub>
</p>
