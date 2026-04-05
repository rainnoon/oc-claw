export const languages = ['en', 'zh', 'ja', 'ko', 'es', 'fr'] as const
export type Lang = (typeof languages)[number]
export const defaultLang: Lang = 'en'

/** Display labels for the language switcher (native names) */
export const langLabels: Record<Lang, string> = {
  en: 'EN',
  zh: '中文',
  ja: '日本語',
  ko: '한국어',
  es: 'ES',
  fr: 'FR',
}

export const ui = {
  en: {
    'site.title': 'OC-Claw — Desktop Pet for AI Coding Agents',
    'site.description': 'A desktop pet that monitors your AI coding agents — OpenClaw and Claude Code — in real time. Supports macOS and Windows.',
    'hero.tagline': 'A desktop pet that monitors your AI coding agents — OpenClaw and Claude Code — in real time. Supports macOS and Windows.',
    'hero.download.mac': 'Download for Mac (v1.6.2)',
    'hero.download.win': 'Download for Windows (v1.6.1)',
    'hero.badge': 'OPEN SOURCE',
    'hero.feature1.title': 'Notch Pet',
    'hero.feature1.desc': 'A character lives on your desktop, animating when agents work and sleeping when idle.',
    'hero.feature2.title': 'Agent Monitoring',
    'hero.feature2.desc': 'Auto-discovers OpenClaw agents and Claude Code sessions. View session lists, chat history, and token stats.',
    'hero.feature3.title': 'Character System',
    'hero.feature3.desc': 'Create custom GIF characters and pair them with different agents. Built-in GIF maker included.',
    'nav.github': 'GitHub',
    'schema.os': 'macOS, Windows',
  },
  zh: {
    'site.title': 'OC-Claw — AI 编程 Agent 桌面宠物',
    'site.description': '桌面宠物应用，实时监控你的 AI 编程 agent 工作状态。支持 macOS 和 Windows。',
    'hero.tagline': '桌面宠物应用，实时监控你的 AI 编程 agent（OpenClaw 和 Claude Code）工作状态。支持 macOS 和 Windows。',
    'hero.download.mac': '下载 Mac 版 (v1.6.2)',
    'hero.download.win': '下载 Windows 版 (v1.6.1)',
    'hero.badge': '开源项目',
    'hero.feature1.title': '桌面宠物',
    'hero.feature1.desc': '角色住在你的桌面上，Agent 工作时播放动画，空闲时打盹休息。',
    'hero.feature2.title': 'Agent 监控',
    'hero.feature2.desc': '自动发现 OpenClaw Agent 和 Claude Code 会话，查看会话列表、聊天记录和 Token 统计。',
    'hero.feature3.title': '角色系统',
    'hero.feature3.desc': '创建自定义 GIF 角色并与不同 Agent 配对，内置 GIF 制作工具。',
    'nav.github': 'GitHub',
    'schema.os': 'macOS, Windows',
  },
  ja: {
    'site.title': 'OC-Claw — AIコーディングエージェント用デスクトップペット',
    'site.description': 'AIコーディングエージェント（OpenClawとClaude Code）をリアルタイムで監視するデスクトップペット。macOSとWindowsに対応。',
    'hero.tagline': 'AIコーディングエージェント（OpenClawとClaude Code）をリアルタイムで監視するデスクトップペット。macOSとWindowsに対応。',
    'hero.download.mac': 'Mac版をダウンロード (v1.6.2)',
    'hero.download.win': 'Windows版をダウンロード (v1.6.1)',
    'hero.badge': 'オープンソース',
    'hero.feature1.title': 'ノッチペット',
    'hero.feature1.desc': 'キャラクターがデスクトップに住み、エージェントが動作中はアニメーション、アイドル時は休憩します。',
    'hero.feature2.title': 'エージェント監視',
    'hero.feature2.desc': 'OpenClawエージェントとClaude Codeセッションを自動検出。セッション一覧、チャット履歴、トークン統計を表示。',
    'hero.feature3.title': 'キャラクターシステム',
    'hero.feature3.desc': 'カスタムGIFキャラクターを作成し、異なるエージェントとペアリング。GIFメーカー内蔵。',
    'nav.github': 'GitHub',
    'schema.os': 'macOS, Windows',
  },
  ko: {
    'site.title': 'OC-Claw — AI 코딩 에이전트 데스크톱 펫',
    'site.description': 'AI 코딩 에이전트(OpenClaw 및 Claude Code)를 실시간으로 모니터링하는 데스크톱 펫. macOS와 Windows 지원.',
    'hero.tagline': 'AI 코딩 에이전트(OpenClaw 및 Claude Code)를 실시간으로 모니터링하는 데스크톱 펫. macOS와 Windows 지원.',
    'hero.download.mac': 'Mac 다운로드 (v1.6.2)',
    'hero.download.win': 'Windows 다운로드 (v1.6.1)',
    'hero.badge': '오픈 소스',
    'hero.feature1.title': '노치 펫',
    'hero.feature1.desc': '캐릭터가 데스크톱에 살며, 에이전트가 작업 중일 때 애니메이션을 재생하고 유휴 시 쉽니다.',
    'hero.feature2.title': '에이전트 모니터링',
    'hero.feature2.desc': 'OpenClaw 에이전트와 Claude Code 세션을 자동 감지. 세션 목록, 채팅 기록, 토큰 통계를 확인.',
    'hero.feature3.title': '캐릭터 시스템',
    'hero.feature3.desc': '커스텀 GIF 캐릭터를 만들고 다양한 에이전트와 페어링. GIF 메이커 내장.',
    'nav.github': 'GitHub',
    'schema.os': 'macOS, Windows',
  },
  es: {
    'site.title': 'OC-Claw — Mascota de Escritorio para Agentes de Código IA',
    'site.description': 'Una mascota de escritorio que monitorea tus agentes de código IA — OpenClaw y Claude Code — en tiempo real. Compatible con macOS y Windows.',
    'hero.tagline': 'Una mascota de escritorio que monitorea tus agentes de código IA — OpenClaw y Claude Code — en tiempo real. Compatible con macOS y Windows.',
    'hero.download.mac': 'Descargar para Mac (v1.6.2)',
    'hero.download.win': 'Descargar para Windows (v1.6.1)',
    'hero.badge': 'CÓDIGO ABIERTO',
    'hero.feature1.title': 'Mascota de Escritorio',
    'hero.feature1.desc': 'Un personaje vive en tu escritorio, se anima cuando los agentes trabajan y duerme cuando están inactivos.',
    'hero.feature2.title': 'Monitoreo de Agentes',
    'hero.feature2.desc': 'Detecta automáticamente agentes OpenClaw y sesiones de Claude Code. Ve listas de sesiones, historial de chat y estadísticas de tokens.',
    'hero.feature3.title': 'Sistema de Personajes',
    'hero.feature3.desc': 'Crea personajes GIF personalizados y empárejalos con diferentes agentes. Incluye creador de GIF.',
    'nav.github': 'GitHub',
    'schema.os': 'macOS, Windows',
  },
  fr: {
    'site.title': 'OC-Claw — Animal de Bureau pour Agents de Code IA',
    'site.description': 'Un animal de bureau qui surveille vos agents de code IA — OpenClaw et Claude Code — en temps réel. Compatible macOS et Windows.',
    'hero.tagline': 'Un animal de bureau qui surveille vos agents de code IA — OpenClaw et Claude Code — en temps réel. Compatible macOS et Windows.',
    'hero.download.mac': 'Télécharger pour Mac (v1.6.2)',
    'hero.download.win': 'Télécharger pour Windows (v1.6.1)',
    'hero.badge': 'OPEN SOURCE',
    'hero.feature1.title': 'Animal de Bureau',
    'hero.feature1.desc': 'Un personnage vit sur votre bureau, s\'anime quand les agents travaillent et dort quand ils sont inactifs.',
    'hero.feature2.title': 'Surveillance des Agents',
    'hero.feature2.desc': 'Détecte automatiquement les agents OpenClaw et les sessions Claude Code. Consultez les listes de sessions, l\'historique de chat et les statistiques de tokens.',
    'hero.feature3.title': 'Système de Personnages',
    'hero.feature3.desc': 'Créez des personnages GIF personnalisés et associez-les à différents agents. Créateur de GIF intégré.',
    'nav.github': 'GitHub',
    'schema.os': 'macOS, Windows',
  },
} as const

export function t(lang: Lang, key: keyof (typeof ui)['en']): string {
  return ui[lang][key] ?? ui.en[key] ?? key
}

export function getLangFromUrl(url: URL): Lang {
  const seg = url.pathname.split('/')[1]
  if (languages.includes(seg as Lang)) return seg as Lang
  return 'en'
}
