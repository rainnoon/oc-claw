import { useState, useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { Loader2 } from 'lucide-react'
import { motion } from 'motion/react'
import { formatTokens } from '../lib/agents'

interface DailyStats {
  date: string
  input_tokens: number
  output_tokens: number
  cache_read_tokens: number
  cache_write_tokens: number
  messages: number
  sessions: number
}

interface ClaudeStats {
  totalInputTokens: number
  totalOutputTokens: number
  totalCacheReadTokens: number
  totalCacheWriteTokens: number
  totalMessages: number
  totalSessions: number
  dailyStats: DailyStats[]
  model: string
}

type ChartMetric = 'tokens' | 'messages'
type ClaudeStatsSource = 'cc' | 'codex' | 'cursor' | 'gemini' | 'hermes'

function DailyChart({ stats }: { stats: DailyStats[] }) {
  const { t } = useTranslation()
  const [metric, setMetric] = useState<ChartMetric>('tokens')
  const isTokens = metric === 'tokens'
  const values = stats.map(d => isTokens ? d.input_tokens + d.output_tokens + d.cache_read_tokens + d.cache_write_tokens : d.messages)
  const maxVal = Math.max(...values, 1)
  const chartH = 80

  const scale = isTokens && maxVal >= 1_000_000 ? 1_000_000 : isTokens && maxVal >= 1_000 ? 1_000 : 1
  const unitLabel = isTokens ? (scale === 1_000_000 ? 'M tokens' : scale === 1_000 ? 'K tokens' : 'tokens') : t('claudeStats.messageCountUnit')
  const fmtTick = (v: number) => {
    if (!isTokens) return String(v)
    const n = v / scale
    return n % 1 === 0 ? String(n) : n.toFixed(1)
  }
  const ticks = [maxVal, Math.round(maxVal / 2), 0]
  const todayVal = values[values.length - 1] ?? 0

  return (
    <div className="flex flex-col gap-4 bg-white/[0.03] border border-white/5 rounded-2xl p-4">
      <div className="flex items-center justify-between">
        <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">
          {isTokens ? t('claudeStats.dailyTokens') : t('claudeStats.dailyMessages')} ({t('claudeStats.last14Days')})
        </span>
        <div className="flex bg-white/[0.08] rounded p-0.5">
          {(['tokens', 'messages'] as const).map(m => (
            <button
              key={m}
              onClick={() => setMetric(m)}
              className={`text-[10px] px-2 py-0.5 rounded transition-colors ${metric === m ? 'bg-white/15 text-white font-semibold' : 'text-white/40'}`}
            >
              {m === 'tokens' ? t('claudeStats.token') : t('claudeStats.messagesLabel')}
            </button>
          ))}
        </div>
      </div>
      <div className="flex items-center justify-end">
        <span className="text-xs font-medium text-blue-400 bg-blue-400/10 px-2 py-0.5 rounded-md">
          {t('agentDetail.today')} {isTokens ? formatTokens(todayVal) : `${todayVal} ${t('claudeStats.messagesUnit')}`}
        </span>
      </div>
      <div className="bg-white/[0.04] rounded-lg p-2 pt-1">
        <div className="text-[8px] text-white/30 mb-0.5">{unitLabel}</div>
        <div className="flex">
          <div className="flex flex-col justify-between pr-1 font-mono" style={{ width: 28, height: chartH }}>
            {ticks.map((t, i) => (
              <span key={i} className="text-[8px] text-white/30 text-right leading-none">{fmtTick(t)}</span>
            ))}
          </div>
          <div className="flex-1 flex items-end gap-px" style={{ height: chartH, borderLeft: '1px solid rgba(255,255,255,0.1)', borderBottom: '1px solid rgba(255,255,255,0.1)', paddingLeft: 1 }}>
            {stats.map((d, i) => {
              const v = values[i]
              const h = Math.max(2, Math.round((v / maxVal) * (chartH - 6)))
              const isToday = d.date === new Date().toISOString().slice(0, 10)
              const tip = isTokens ? `${d.date}: ${formatTokens(v)}` : `${d.date}: ${v} ${t('claudeStats.messagesUnit')}`
              return (
                <div key={d.date} className="flex-1 flex flex-col items-center group" title={tip}>
                  <div
                    className={`w-full rounded-t-sm transition-all duration-300 ${isToday ? 'bg-blue-500' : v > 0 ? 'bg-blue-400/50 group-hover:bg-blue-400/70' : 'bg-white/[0.06]'}`}
                    style={{ height: h }}
                  />
                </div>
              )
            })}
          </div>
        </div>
        <div className="flex mt-1" style={{ paddingLeft: 32 }}>
          <div className="flex-1 flex justify-between text-[8px] text-white/30 font-mono">
            <span>{stats[0]?.date.slice(5)}</span>
            <span>{stats[Math.floor(stats.length / 2)]?.date.slice(5)}</span>
            <span>{stats[stats.length - 1]?.date.slice(5)}</span>
          </div>
        </div>
      </div>
    </div>
  )
}

interface HermesActivity {
  type: 'user' | 'assistant' | 'tool'
  summary: string
  toolName?: string
  timestamp: number
}

function HermesDetailView({ stats, isActive, channel, sshConn, sessionId }: { stats: ClaudeStats; isActive?: boolean; channel?: string; sshConn?: { host: string; user: string }; sessionId?: string }) {
  const { t } = useTranslation()
  const [activities, setActivities] = useState<HermesActivity[]>([])

  useEffect(() => {
    const fetchActivity = () => {
      const cmd = sshConn
        ? invoke('get_hermes_remote_recent_activity', { sshHost: sshConn.host, sshUser: sshConn.user, sessionId: sessionId || '' })
        : invoke('get_hermes_recent_activity', { sessionId: sessionId || '' })
      cmd.then((items: any) => {
        if (items?.length) setActivities(items.slice(0, 3))
      }).catch(() => {})
    }
    fetchActivity()
    const t = setInterval(fetchActivity, 5000)
    return () => clearInterval(t)
  }, [sshConn?.host, sshConn?.user, sessionId])

  const fmtTime = (ts: number) => {
    const d = new Date(ts * 1000)
    return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
  }

  const totalTokens = stats.totalInputTokens + stats.totalOutputTokens + stats.totalCacheReadTokens + stats.totalCacheWriteTokens
  const active = isActive ?? false

  return (
    <div className="flex-1 min-h-0 px-5 py-5 flex flex-col gap-6 overflow-y-auto scrollbar-thin">

      {/* Hero Profile */}
      <div className="flex flex-col gap-4">
        <div className="flex items-center justify-between">
          <div className="flex flex-col">
            <h1 className="text-lg font-semibold text-white tracking-tight">Hermes</h1>
            {channel && <span className="text-xs text-white/40">via {channel}</span>}
          </div>
          <span className={`px-2.5 py-1 rounded-full text-xs font-medium border flex items-center gap-1.5 ${
            active
              ? 'bg-emerald-500/10 text-emerald-400 border-emerald-500/20'
              : 'bg-white/5 text-white/50 border-white/10'
          }`}>
            <span className={`w-1.5 h-1.5 rounded-full ${active ? 'bg-emerald-400 animate-pulse' : 'bg-white/30'}`} />
            {active ? t('agentDetail.working') : t('agentDetail.idleStatus')}
          </span>
        </div>
      </div>

      {/* Recent Activity */}
      {activities.length > 0 && (
        <div className="flex flex-col gap-3">
          <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider px-1">{t('agentDetail.recentActivity')}</span>
          <div className="bg-[#0a0a0a] border border-white/5 rounded-2xl p-4 flex flex-col gap-4 font-mono text-xs">
            {activities.map((a, i) => (
              <div key={i} className="flex items-start gap-3 opacity-80 hover:opacity-100 transition-opacity">
                <span className={`mt-0.5 text-[10px] ${a.type === 'tool' ? 'text-blue-400' : a.type === 'user' ? 'text-purple-400' : 'text-emerald-400'}`}>●</span>
                <span className="text-white/70 flex-1 leading-relaxed truncate">
                  {a.summary}
                </span>
                <span className="text-white/30 shrink-0">{fmtTime(a.timestamp)}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Bento Grid: Stats */}
      <div className="grid grid-cols-2 gap-3">
        {/* Tokens */}
        <div className="bg-white/[0.03] border border-white/5 rounded-2xl p-4 flex flex-col gap-1 hover:bg-white/[0.05] transition-colors">
          <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">{t('agentDetail.tokensUsed')}</span>
          <div className="flex items-baseline gap-1.5 mt-1">
            <span className="text-2xl font-semibold text-white tracking-tight">{formatTokens(totalTokens)}</span>
          </div>
          <span className="text-xs text-white/40 mt-1">{stats.totalSessions} {t('claudeStats.sessionsCount')}</span>
        </div>

        {/* Session */}
        <div className="bg-white/[0.03] border border-white/5 rounded-2xl p-4 flex flex-col gap-1 hover:bg-white/[0.05] transition-colors">
          <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">{t('agentDetail.sessionStatus')}</span>
          <div className="flex items-baseline gap-2 mt-1">
            <span className="text-2xl font-semibold text-white tracking-tight">{stats.totalMessages}</span>
            <span className="text-xs text-white/40">{t('agentDetail.messages')}</span>
          </div>
        </div>
      </div>

      {/* Model Details */}
      <div className="flex flex-col gap-3 bg-white/[0.03] border border-white/5 rounded-2xl p-4 hover:bg-white/[0.05] transition-colors">
        <div className="flex items-center justify-between">
          <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">{t('agentDetail.model')}</span>
          <span className="text-xs font-medium text-white/70">{stats.model || t('common.unknown')}</span>
        </div>
        <div className="grid grid-cols-4 gap-2 text-xs mt-1">
          {([
            [t('agentDetail.input'), stats.totalInputTokens],
            [t('agentDetail.output'), stats.totalOutputTokens],
            [t('agentDetail.cacheRead'), stats.totalCacheReadTokens],
            [t('agentDetail.cacheWrite'), stats.totalCacheWriteTokens],
          ] as [string, number][]).map(([label, val]) => (
            <div key={label} className="flex flex-col gap-1">
              <span className="text-white/40 text-[10px]">{label}</span>
              <span className="text-white/90 font-mono">{formatTokens(val)}</span>
            </div>
          ))}
        </div>
      </div>

      {/* Daily chart */}
      {stats.dailyStats.length > 0 && (
        <DailyChart stats={stats.dailyStats} />
      )}
    </div>
  )
}

export function ClaudeStatsView({ source = 'cc', isActive, channel, sshConn, hermesSessionId }: { source?: ClaudeStatsSource; isActive?: boolean; channel?: string; sshConn?: { host: string; user: string }; hermesSessionId?: string }) {
  const { t } = useTranslation()
  const [stats, setStats] = useState<ClaudeStats | null>(null)

  useEffect(() => {
    setStats(null)
    const cmd = (source === 'hermes' && sshConn)
      ? invoke('get_hermes_remote_stats', { sshHost: sshConn.host, sshUser: sshConn.user })
      : invoke('get_claude_stats', { source })
    cmd.then((s: any) => setStats(s)).catch(() => {})
  }, [source, sshConn?.host, sshConn?.user])

  if (!stats) {
    return (
      <div className="flex-1 min-h-0 flex flex-col items-center justify-center py-24 gap-3">
        <motion.div
          animate={{ rotate: 360 }}
          transition={{ duration: 1, repeat: Infinity, ease: "linear" }}
        >
          <Loader2 className="w-5 h-5 text-white/30" />
        </motion.div>
        <span className="text-white/30 text-xs font-medium tracking-wide animate-pulse">
          {t('common.loading')}
        </span>
      </div>
    )
  }

  const totalTokens = stats.totalInputTokens + stats.totalOutputTokens + stats.totalCacheReadTokens + stats.totalCacheWriteTokens
  const titleKey = source === 'cursor'
    ? 'claudeStats.titleCursor'
    : source === 'codex'
      ? 'claudeStats.titleCodex'
      : source === 'gemini'
        ? 'claudeStats.titleGemini'
        : source === 'hermes'
          ? 'claudeStats.titleHermes'
          : 'claudeStats.title'

  if (source === 'cursor') {
    const unsupportedTitle = t('claudeStats.cursorUnsupportedTitle', 'Cursor 暂不支持详细统计')
    const unsupportedDesc = t('claudeStats.cursorUnsupportedDesc', 'Cursor 不向第三方工具暴露每次请求的 token 用量，oc-claw 无法在本地准确还原。请在 Cursor 应用内查看用量。')
    return (
      <div className="flex-1 min-h-0 px-5 py-5 flex flex-col gap-6">
        <div className="flex items-center justify-between">
          <h1 className="text-lg font-semibold text-white tracking-tight">{t(titleKey)}</h1>
        </div>
        <div className="flex-1 flex flex-col items-center justify-center gap-2 text-center px-6">
          <span className="text-white/60 text-sm font-medium">
            {unsupportedTitle}
          </span>
          <span className="text-white/40 text-xs leading-relaxed max-w-sm">
            {unsupportedDesc}
          </span>
        </div>
      </div>
    )
  }

  // Hermes: use AgentDetailView-style layout
  if (source === 'hermes') {
    return <HermesDetailView stats={stats} isActive={isActive} channel={channel} sshConn={sshConn} sessionId={hermesSessionId} />
  }

  return (
    <div className="flex-1 min-h-0 px-5 py-5 flex flex-col gap-6 overflow-y-auto scrollbar-thin">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex flex-col">
          <h1 className="text-lg font-semibold text-white tracking-tight">{t(titleKey)}</h1>
        </div>
        <span className="text-xs text-white/40">{t('claudeStats.last14Days')}</span>
      </div>

      {/* Bento: totals */}
      <div className="grid grid-cols-2 gap-3">
        <div className="bg-white/[0.03] border border-white/5 rounded-2xl p-4 flex flex-col gap-1 hover:bg-white/[0.05] transition-colors">
          <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">{t('claudeStats.totalTokens')}</span>
          <span className="text-2xl font-semibold text-white tracking-tight mt-1">{formatTokens(totalTokens)}</span>
          <span className="text-xs text-white/40">{stats.totalSessions} {t('claudeStats.sessionsCount')}</span>
        </div>
        <div className="bg-white/[0.03] border border-white/5 rounded-2xl p-4 flex flex-col gap-1 hover:bg-white/[0.05] transition-colors">
          <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">{t('claudeStats.messageCount')}</span>
          <span className="text-2xl font-semibold text-white tracking-tight mt-1">{stats.totalMessages}</span>
          <span className="text-xs text-white/40">{t('claudeStats.aiReply')}</span>
        </div>
      </div>

      {/* Token breakdown */}
      <div className="flex flex-col gap-3 bg-white/[0.03] border border-white/5 rounded-2xl p-4 hover:bg-white/[0.05] transition-colors">
        <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">{t('claudeStats.tokenDetails')}</span>
        <div className="grid grid-cols-4 gap-2 text-xs mt-1">
          {([
            [t('claudeStats.input'), stats.totalInputTokens],
            [t('claudeStats.output'), stats.totalOutputTokens],
            [t('claudeStats.cacheRead'), stats.totalCacheReadTokens],
            [t('claudeStats.cacheWrite'), stats.totalCacheWriteTokens],
          ] as [string, number][]).map(([label, val]) => (
            <div key={label} className="flex flex-col gap-1">
              <span className="text-white/40 text-[10px]">{label}</span>
              <span className="text-white/90 font-mono">{formatTokens(val)}</span>
            </div>
          ))}
        </div>
      </div>

      {/* Daily chart */}
      {stats.dailyStats.length > 0 && (
        <DailyChart stats={stats.dailyStats} />
      )}
    </div>
  )
}
