import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import ReactMarkdown from 'react-markdown'
import type { AgentMetrics } from '../lib/types'
import { formatTokens, formatTime, formatDuration } from '../lib/agents'

interface AgentDetailViewProps {
  agent: { id: string; identityName?: string; identityEmoji?: string } | undefined
  metrics: AgentMetrics | null
  extraInfo: any
}

type ChartMode = 'calls' | 'tokens'

function DailyChart({ extraInfo }: { extraInfo: { daily_counts: { date: string; count: number; tokens: number }[] } }) {
  const { t } = useTranslation()
  const [mode, setMode] = useState<ChartMode>('calls')
  const counts = extraInfo.daily_counts
  const todayEntry = counts[counts.length - 1]
  const isCalls = mode === 'calls'
  const values = counts.map((d: any) => (isCalls ? d.count : d.tokens))
  const maxVal = Math.max(...values, 1)
  const chartH = 80

  const scale = !isCalls && maxVal >= 1_000_000 ? 1_000_000 : !isCalls && maxVal >= 1_000 ? 1_000 : 1
  const unitLabel = isCalls ? t('agentDetail.callsUnit') : scale === 1_000_000 ? 'M tokens' : scale === 1_000 ? 'K tokens' : 'tokens'
  const fmtTick = (v: number) => {
    if (isCalls) return String(v)
    const n = v / scale
    return n % 1 === 0 ? String(n) : n.toFixed(1)
  }
  const ticks = [maxVal, Math.round(maxVal / 2), 0]

  return (
    <div className="flex flex-col gap-4 bg-white/[0.03] border border-white/5 rounded-2xl p-4">
      <div className="flex items-center justify-between">
        <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">
          {isCalls ? t('agentDetail.dailyCalls') : t('agentDetail.dailyTokens')} ({t('agentDetail.last14Days')})
        </span>
        <div className="flex bg-white/[0.08] rounded p-0.5">
          {(['calls', 'tokens'] as const).map((m) => (
            <button
              key={m}
              onClick={() => setMode(m)}
              className={`text-[10px] px-2 py-0.5 rounded transition-colors ${mode === m ? 'bg-white/15 text-white font-semibold' : 'text-white/40'}`}
            >
              {m === 'calls' ? t('agentDetail.calls') : t('agentDetail.token')}
            </button>
          ))}
        </div>
      </div>
      <div className="flex items-center justify-end">
        <span className="text-xs font-medium text-blue-400 bg-blue-400/10 px-2 py-0.5 rounded-md">
          {t('agentDetail.today')} {isCalls ? `${todayEntry?.count ?? 0} ${t('agentDetail.times')}` : formatTokens(todayEntry?.tokens ?? 0)}
        </span>
      </div>
      <div className="bg-white/[0.04] rounded-lg p-2 pt-1">
        <div className="text-[8px] text-white/30 mb-0.5">{unitLabel}</div>
        <div className="flex">
          {/* Y-axis ticks */}
          <div className="flex flex-col justify-between pr-1 font-mono" style={{ width: 28, height: chartH }}>
            {ticks.map((t, i) => (
              <span key={i} className="text-[8px] text-white/30 text-right leading-none">{fmtTick(t)}</span>
            ))}
          </div>
          {/* Chart bars */}
          <div className="flex-1 flex items-end gap-px" style={{ height: chartH, borderLeft: '1px solid rgba(255,255,255,0.1)', borderBottom: '1px solid rgba(255,255,255,0.1)', paddingLeft: 1 }}>
            {counts.map((d: any, i: number) => {
              const v = values[i]
              const h = Math.max(2, Math.round((v / maxVal) * (chartH - 6)))
              const isToday = d.date === new Date().toISOString().slice(0, 10)
              const tip = isCalls ? `${d.date}: ${d.count} ${t('agentDetail.times')}` : `${d.date}: ${formatTokens(d.tokens)}`
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
        {/* X-axis dates */}
        <div className="flex mt-1" style={{ paddingLeft: 32 }}>
          <div className="flex-1 flex justify-between text-[8px] text-white/30 font-mono">
            <span>{counts[0]?.date.slice(5)}</span>
            <span>{counts[Math.floor(counts.length / 2)]?.date.slice(5)}</span>
            <span>{counts[counts.length - 1]?.date.slice(5)}</span>
          </div>
        </div>
      </div>
    </div>
  )
}

export function AgentDetailView({ agent, metrics, extraInfo }: AgentDetailViewProps) {
  const { t } = useTranslation()
  if (!metrics) {
    return (
      <div className="flex items-center justify-center py-20 text-white/30 text-sm">
        {t('common.loading')}
      </div>
    )
  }

  const totalTokensStr = formatTokens(metrics.totalTokens)
  const durationStr = metrics.sessionStart ? formatDuration(metrics.sessionStart) : ''

  return (
    <div className="px-5 py-5 flex flex-col gap-6 overflow-y-auto scrollbar-thin" style={{ maxHeight: 524 }}>

        {/* Hero Profile */}
        <div className="flex flex-col gap-4">
          <div className="flex items-center justify-between">
            <div className="flex flex-col">
              <h1 className="text-lg font-semibold text-white tracking-tight">{agent?.identityName || agent?.id || t('common.unknown')}</h1>
              {metrics.channel && <span className="text-xs text-white/40">via {metrics.channel}</span>}
            </div>
            <span className={`px-2.5 py-1 rounded-full text-xs font-medium border flex items-center gap-1.5 ${
              metrics.active
                ? 'bg-emerald-500/10 text-emerald-400 border-emerald-500/20'
                : 'bg-white/5 text-white/50 border-white/10'
            }`}>
              <span className={`w-1.5 h-1.5 rounded-full ${metrics.active ? 'bg-emerald-400 animate-pulse' : 'bg-white/30'}`} />
              {metrics.active ? t('agentDetail.working') : t('agentDetail.idleStatus')}
            </span>
          </div>

          {metrics.currentTask && (
            <div className="bg-white/[0.03] border border-white/5 rounded-2xl p-4 flex flex-col gap-2 relative overflow-hidden group">
              <div className="absolute inset-0 bg-gradient-to-r from-blue-500/5 to-purple-500/5 opacity-0 group-hover:opacity-100 transition-opacity" />
              <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider relative z-10">{t('agentDetail.currentTask')}</span>
              <div className="text-sm text-white/90 leading-relaxed relative z-10 markdown-content">
                <ReactMarkdown>{metrics.currentTask}</ReactMarkdown>
              </div>
            </div>
          )}
        </div>

        {/* Recent Activity */}
        {metrics.recentActions && metrics.recentActions.length > 0 && (
          <div className="flex flex-col gap-3">
            <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider px-1">{t('agentDetail.recentActivity')}</span>
            <div className="bg-[#0a0a0a] border border-white/5 rounded-2xl p-4 flex flex-col gap-4 font-mono text-xs">
              {metrics.recentActions.slice(0, 5).map((action, i) => (
                <div key={i} className="flex items-start gap-3 opacity-80 hover:opacity-100 transition-opacity">
                  <span className={`mt-0.5 text-[10px] ${action.type === 'tool' ? 'text-blue-400' : 'text-emerald-400'}`}>●</span>
                  <span className="text-white/70 flex-1 leading-relaxed truncate markdown-content markdown-inline">
                    <ReactMarkdown>{action.summary}</ReactMarkdown>
                  </span>
                  {action.timestamp && <span className="text-white/30">{formatTime(action.timestamp).split(' ')[1]}</span>}
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
              <span className="text-2xl font-semibold text-white tracking-tight">{totalTokensStr}</span>
            </div>
            {durationStr && <span className="text-xs text-white/40 mt-1">{durationStr}</span>}
          </div>

          {/* Session */}
          <div className="bg-white/[0.03] border border-white/5 rounded-2xl p-4 flex flex-col gap-1 hover:bg-white/[0.05] transition-colors">
            <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">{t('agentDetail.sessionStatus')}</span>
            <div className="flex items-baseline gap-2 mt-1">
              <span className="text-2xl font-semibold text-white tracking-tight">{metrics.messageCount}</span>
              <span className="text-xs text-white/40">{t('agentDetail.messages')}</span>
            </div>
            <div className="flex items-center gap-3 mt-1 text-xs text-white/40">
              <span className="flex items-center gap-1"><span className="w-1.5 h-1.5 rounded-full bg-blue-400" /> {metrics.activeSessionCount} {t('agentDetail.active')}</span>
              <span className="flex items-center gap-1"><span className="w-1.5 h-1.5 rounded-full bg-red-400" /> {metrics.errorCount} {t('agentDetail.error')}</span>
            </div>
          </div>
        </div>

        {/* Model Details */}
        <div className="flex flex-col gap-3 bg-white/[0.03] border border-white/5 rounded-2xl p-4 hover:bg-white/[0.05] transition-colors">
          <div className="flex items-center justify-between">
            <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">{t('agentDetail.model')}</span>
            <span className="text-xs font-medium text-white/70">{metrics.currentModel || t('common.unknown')}</span>
          </div>
          <div className="grid grid-cols-4 gap-2 text-xs mt-1">
            {([
              [t('agentDetail.input'), metrics.inputTokens],
              [t('agentDetail.output'), metrics.outputTokens],
              [t('agentDetail.cacheRead'), metrics.cacheReadTokens],
              [t('agentDetail.cacheWrite'), metrics.cacheWriteTokens],
            ] as [string, number][]).map(([label, val]) => (
              <div key={label} className="flex flex-col gap-1">
                <span className="text-white/40 text-[10px]">{label}</span>
                <span className="text-white/90 font-mono">{formatTokens(val)}</span>
              </div>
            ))}
          </div>
        </div>

        {/* Tools Distribution */}
        {metrics.toolCalls && metrics.toolCalls.length > 0 && (
          <div className="flex flex-col gap-3">
            <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider px-1">{t('agentDetail.toolDistribution')}</span>
            <div className="flex flex-col gap-2.5 bg-white/[0.02] border border-white/5 rounded-2xl p-4">
              {metrics.toolCalls.slice(0, 8).map(tool => {
                const maxCount = metrics.toolCalls[0].count
                const pct = Math.max(5, Math.round((tool.count / maxCount) * 100))
                return (
                  <div key={tool.name} className="flex items-center gap-3 text-sm group relative">
                    <span className="w-20 text-white/60 text-xs font-mono group-hover:text-white/90 transition-colors truncate shrink-0">{tool.name}</span>
                    <div className="absolute left-0 bottom-full mb-1 hidden group-hover:block z-10 bg-[#1a1a1a] border border-white/10 rounded-lg px-3 py-2 text-xs text-white/90 font-mono whitespace-pre-wrap max-w-[200px] break-all shadow-lg">
                      {tool.name}
                    </div>
                    <div className="flex-1 h-1.5 bg-white/5 rounded-full overflow-hidden">
                      <div className="h-full bg-blue-500/80 rounded-full" style={{ width: `${pct}%` }} />
                    </div>
                    <span className="w-6 text-right text-white/40 text-xs font-mono group-hover:text-white/90 transition-colors">{tool.count}</span>
                  </div>
                )
              })}
            </div>
          </div>
        )}

        {/* Skills */}
        {extraInfo?.skills?.length > 0 && (
          <div className="flex flex-col gap-3">
            <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider px-1">{t('agentDetail.loadedSkills')}</span>
            <div className="flex flex-wrap gap-1.5">
              {extraInfo.skills.map((skill: string) => (
                <span key={skill} className="bg-white/5 hover:bg-white/10 transition-colors text-white/60 px-2.5 py-1 rounded-md text-[11px] font-mono border border-white/5">
                  {skill}
                </span>
              ))}
            </div>
          </div>
        )}

        {/* Chart */}
        {extraInfo?.daily_counts?.length > 0 && (
          <DailyChart extraInfo={extraInfo} />
        )}
    </div>
  )
}
