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
type ClaudeStatsSource = 'cc' | 'codex' | 'cursor'

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

export function ClaudeStatsView({ source = 'cc' }: { source?: ClaudeStatsSource }) {
  const { t } = useTranslation()
  const [stats, setStats] = useState<ClaudeStats | null>(null)

  useEffect(() => {
    setStats(null)
    invoke('get_claude_stats', { source }).then((s: any) => setStats(s)).catch(() => {})
  }, [source])

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
      : 'claudeStats.title'

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
