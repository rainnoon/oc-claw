import { useState } from 'react'
import type { AgentMetrics } from '../lib/types'
import { formatTokens, formatTime, formatDuration } from '../lib/agents'

interface AgentDetailViewProps {
  agent: { id: string; identityName?: string; identityEmoji?: string } | undefined
  metrics: AgentMetrics | null
  extraInfo: any
}

type ChartMode = 'calls' | 'tokens'

function DailyChart({ extraInfo }: { extraInfo: { daily_counts: { date: string; count: number; tokens: number }[] } }) {
  const [mode, setMode] = useState<ChartMode>('calls')
  const counts = extraInfo.daily_counts
  const todayEntry = counts[counts.length - 1]
  const isCalls = mode === 'calls'
  const values = counts.map((d: any) => (isCalls ? d.count : d.tokens))
  const maxVal = Math.max(...values, 1)

  return (
    <div className="flex flex-col gap-4 bg-white/[0.03] border border-white/5 rounded-2xl p-4">
      <div className="flex items-center justify-between">
        <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">
          {isCalls ? '每日调用' : '每日 Token'} (近14天)
        </span>
        <div className="flex bg-white/[0.08] rounded p-0.5">
          {(['calls', 'tokens'] as const).map((m) => (
            <button
              key={m}
              onClick={() => setMode(m)}
              className={`text-[10px] px-2 py-0.5 rounded transition-colors ${mode === m ? 'bg-white/15 text-white font-semibold' : 'text-white/40'}`}
            >
              {m === 'calls' ? '调用' : 'Token'}
            </button>
          ))}
        </div>
      </div>
      <div className="flex items-center justify-end">
        <span className="text-xs font-medium text-blue-400 bg-blue-400/10 px-2 py-0.5 rounded-md">
          今天 {isCalls ? `${todayEntry?.count ?? 0} 次` : formatTokens(todayEntry?.tokens ?? 0)}
        </span>
      </div>
      <div className="h-24 flex items-end gap-1.5 pt-4">
        {counts.map((d: any, i: number) => {
          const v = values[i]
          const pct = Math.max(4, Math.round((v / maxVal) * 100))
          const isToday = d.date === new Date().toISOString().slice(0, 10)
          const tip = isCalls ? `${d.date}: ${d.count} 次` : `${d.date}: ${formatTokens(d.tokens)}`
          return (
            <div key={d.date} className="flex-1 flex flex-col justify-end h-full group relative" title={tip}>
              <div
                className={`w-full rounded-sm transition-all duration-300 ${isToday ? 'bg-blue-500' : v > 0 ? 'bg-white/10 group-hover:bg-white/20' : 'bg-white/[0.04]'}`}
                style={{ height: `${pct}%` }}
              />
            </div>
          )
        })}
      </div>
      <div className="flex justify-between text-[10px] text-white/30 font-mono">
        <span>{counts[0]?.date.slice(5)}</span>
        <span>{counts[Math.floor(counts.length / 2)]?.date.slice(5)}</span>
        <span>{counts[counts.length - 1]?.date.slice(5)}</span>
      </div>
    </div>
  )
}

export function AgentDetailView({ agent, metrics, extraInfo }: AgentDetailViewProps) {
  if (!metrics) {
    return (
      <div className="flex items-center justify-center py-20 text-white/30 text-sm">
        loading...
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
            <div className="flex items-center gap-3">
              <div className="w-10 h-10 rounded-full bg-gradient-to-br from-blue-500 to-purple-600 flex items-center justify-center text-lg shadow-inner border border-white/10">
                {agent?.identityEmoji || '🚀'}
              </div>
              <div className="flex flex-col">
                <h1 className="text-lg font-semibold text-white tracking-tight">{agent?.identityName || agent?.id || '未知'}</h1>
                {metrics.channel && <span className="text-xs text-white/40">via {metrics.channel}</span>}
              </div>
            </div>
            <span className={`px-2.5 py-1 rounded-full text-xs font-medium border flex items-center gap-1.5 ${
              metrics.active
                ? 'bg-emerald-500/10 text-emerald-400 border-emerald-500/20'
                : 'bg-white/5 text-white/50 border-white/10'
            }`}>
              <span className={`w-1.5 h-1.5 rounded-full ${metrics.active ? 'bg-emerald-400 animate-pulse' : 'bg-white/30'}`} />
              {metrics.active ? '工作中' : '空闲'}
            </span>
          </div>

          {metrics.currentTask && (
            <div className="bg-white/[0.03] border border-white/5 rounded-2xl p-4 flex flex-col gap-2 relative overflow-hidden group">
              <div className="absolute inset-0 bg-gradient-to-r from-blue-500/5 to-purple-500/5 opacity-0 group-hover:opacity-100 transition-opacity" />
              <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider relative z-10">当前任务</span>
              <p className="text-sm text-white/90 leading-relaxed relative z-10">{metrics.currentTask}</p>
            </div>
          )}
        </div>

        {/* Recent Activity */}
        {metrics.recentActions && metrics.recentActions.length > 0 && (
          <div className="flex flex-col gap-3">
            <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider px-1">最近动态</span>
            <div className="bg-[#0a0a0a] border border-white/5 rounded-2xl p-4 flex flex-col gap-4 font-mono text-xs">
              {metrics.recentActions.slice(0, 5).map((action, i) => (
                <div key={i} className="flex items-start gap-3 opacity-80 hover:opacity-100 transition-opacity">
                  <span className={`mt-0.5 text-[10px] ${action.type === 'tool' ? 'text-blue-400' : 'text-emerald-400'}`}>●</span>
                  <span className="text-white/70 flex-1 leading-relaxed truncate">{action.summary}</span>
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
            <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">消耗 (Tokens)</span>
            <div className="flex items-baseline gap-1.5 mt-1">
              <span className="text-2xl font-semibold text-white tracking-tight">{totalTokensStr}</span>
            </div>
            {durationStr && <span className="text-xs text-white/40 mt-1">{durationStr}</span>}
          </div>

          {/* Session */}
          <div className="bg-white/[0.03] border border-white/5 rounded-2xl p-4 flex flex-col gap-1 hover:bg-white/[0.05] transition-colors">
            <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">会话状态</span>
            <div className="flex items-baseline gap-2 mt-1">
              <span className="text-2xl font-semibold text-white tracking-tight">{metrics.messageCount}</span>
              <span className="text-xs text-white/40">消息</span>
            </div>
            <div className="flex items-center gap-3 mt-1 text-xs text-white/40">
              <span className="flex items-center gap-1"><span className="w-1.5 h-1.5 rounded-full bg-blue-400" /> {metrics.activeSessionCount} 活跃</span>
              <span className="flex items-center gap-1"><span className="w-1.5 h-1.5 rounded-full bg-red-400" /> {metrics.errorCount} 错误</span>
            </div>
          </div>
        </div>

        {/* Model Details */}
        <div className="flex flex-col gap-3 bg-white/[0.03] border border-white/5 rounded-2xl p-4 hover:bg-white/[0.05] transition-colors">
          <div className="flex items-center justify-between">
            <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider">模型</span>
            <span className="text-xs font-medium text-white/70">{metrics.currentModel || '未知'}</span>
          </div>
          <div className="grid grid-cols-4 gap-2 text-xs mt-1">
            {([
              ['输入', metrics.inputTokens],
              ['输出', metrics.outputTokens],
              ['缓存读', metrics.cacheReadTokens],
              ['缓存写', metrics.cacheWriteTokens],
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
            <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider px-1">工具使用分布</span>
            <div className="flex flex-col gap-2.5 bg-white/[0.02] border border-white/5 rounded-2xl p-4">
              {metrics.toolCalls.slice(0, 8).map(tool => {
                const maxCount = metrics.toolCalls[0].count
                const pct = Math.max(5, Math.round((tool.count / maxCount) * 100))
                return (
                  <div key={tool.name} className="flex items-center gap-3 text-sm group">
                    <span className="w-20 text-white/60 text-xs font-mono group-hover:text-white/90 transition-colors">{tool.name}</span>
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
            <span className="text-[10px] font-medium text-white/40 uppercase tracking-wider px-1">已加载技能 (Skills)</span>
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
