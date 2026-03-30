export const languages = ['en', 'zh'] as const
export type Lang = (typeof languages)[number]
export const defaultLang: Lang = 'en'

export const ui = {
  en: {
    'site.title': 'OC-Claw — Desktop Pet for AI Coding Agents',
    'site.description': 'A desktop pet that monitors your AI coding agents — OpenClaw and Claude Code — in real time. Supports macOS and Windows.',
    'hero.tagline': 'A desktop pet that monitors your AI coding agents — OpenClaw and Claude Code — in real time. Supports macOS and Windows.',
    'hero.download.mac': 'Download for Mac',
    'hero.download.win': 'Download for Windows',
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
    'hero.download.mac': '下载 Mac 版',
    'hero.download.win': '下载 Windows 版',
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
} as const

export function t(lang: Lang, key: keyof (typeof ui)['en']): string {
  return ui[lang][key] ?? ui.en[key] ?? key
}

export function getLangFromUrl(url: URL): Lang {
  const seg = url.pathname.split('/')[1]
  if (seg === 'zh') return 'zh'
  return 'en'
}
