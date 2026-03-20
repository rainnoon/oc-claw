import { useState, useEffect, useCallback, useRef } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { load } from '@tauri-apps/plugin-store'
import { listen } from '@tauri-apps/api/event'
import { GifMakerTab } from './components/GifMakerTab'
import { SettingsTab } from './components/SettingsTab'
import { getStore, DEFAULT_CHAR } from './lib/store'
import { formatTokens, formatTime, formatDuration, saveAgentCharMap } from './lib/agents'
import type { AgentMetrics } from './lib/types'

interface CharacterMeta {
  name: string
  workGifs: string[]
  restGifs: string[]
  miniActions?: Record<string, string[]>
}

interface AgentInfo {
  id: string
  identityName?: string
  identityEmoji?: string
}

interface AgentHealth {
  agentId: string
  active: boolean
}

interface MiniSessionInfo {
  key: string
  agentId: string
  sessionId: string
  label: string
  channel?: string
  updatedAt: number
  active: boolean
  lastUserMsg?: string
  lastAssistantMsg?: string
}

interface SessionSlot {
  agentId: string
  sessionIdx: number
  agent: AgentInfo
  char?: CharacterMeta
  isWorking: boolean
}

const MAX_SLOTS = 10

function getMiniGif(char: CharacterMeta | undefined, isWorking: boolean, useTop = false): string | undefined {
  const c = (char?.miniActions && Object.values(char.miniActions).flat().length > 0) ? char : DEFAULT_CHAR
  if (!c?.miniActions) return undefined
  if (useTop && c.miniActions['top']?.length) {
    const topGifs = c.miniActions['top']
    if (isWorking) {
      const work = topGifs.find((g) => g.includes('work'))
      if (work) return work
    }
    const sleep = topGifs.find((g) => g.includes('sleep') || g.includes('idle') || g.includes('rest'))
    if (sleep) return sleep
    return topGifs[0]
  }
  const allGifs = Object.values(c.miniActions).flat()
  if (allGifs.length === 0) return undefined
  const idleGifs = allGifs.filter((g) => g.includes('idle'))
  const actionGifs = allGifs.filter((g) => !g.includes('idle'))
  if (isWorking && actionGifs.length > 0) return actionGifs[0]
  return idleGifs[0] || allGifs[0]
}

type ChartMode = 'calls' | 'tokens'

function MiniDailyChart({ extraInfo }: { extraInfo: { daily_counts: { date: string; count: number; tokens: number }[] } }) {
  const [mode, setMode] = useState<ChartMode>('calls')
  const counts = extraInfo.daily_counts
  const todayEntry = counts[counts.length - 1]
  const isCalls = mode === 'calls'
  const values = counts.map((d) => (isCalls ? d.count : d.tokens))
  const maxVal = Math.max(...values, 1)
  const chartH = 80

  const scale = !isCalls && maxVal >= 1_000_000 ? 1_000_000 : !isCalls && maxVal >= 1_000 ? 1_000 : 1
  const unitLabel = isCalls ? '次数' : scale === 1_000_000 ? 'M tokens' : scale === 1_000 ? 'K tokens' : 'tokens'
  const fmtTick = (v: number) => {
    if (isCalls) return String(v)
    const n = v / scale
    return n % 1 === 0 ? String(n) : n.toFixed(1)
  }
  const ticks = [maxVal, Math.round(maxVal / 2), 0]

  return (
    <div style={{ marginBottom: 10 }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 6 }}>
        <div style={{ color: 'rgba(255,255,255,0.4)', fontSize: 9, fontWeight: 700, textTransform: 'uppercase', letterSpacing: 0.5 }}>
          {isCalls ? '每日调用' : '每日 Token'} (近14天)
        </div>
        <div style={{ display: 'flex', background: 'rgba(255,255,255,0.08)', borderRadius: 4, padding: 1 }}>
          {(['calls', 'tokens'] as const).map((m) => (
            <button key={m} onClick={() => setMode(m)} style={{
              background: mode === m ? 'rgba(255,255,255,0.15)' : 'none',
              border: 'none', color: mode === m ? '#fff' : 'rgba(255,255,255,0.4)',
              fontSize: 8, padding: '2px 6px', borderRadius: 3, cursor: 'pointer', fontWeight: mode === m ? 600 : 400,
            }}>
              {m === 'calls' ? '调用' : 'Token'}
            </button>
          ))}
        </div>
      </div>

      <div style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: 4 }}>
        <span style={{ fontSize: 10, color: '#5dade2', fontWeight: 600 }}>
          今天 {isCalls ? `${todayEntry?.count ?? 0} 次` : formatTokens(todayEntry?.tokens ?? 0)}
        </span>
      </div>

      <div style={{ background: 'rgba(255,255,255,0.04)', borderRadius: 6, padding: '8px 8px 4px' }}>
        <div style={{ color: 'rgba(255,255,255,0.3)', fontSize: 8, marginBottom: 2 }}>{unitLabel}</div>
        <div style={{ display: 'flex' }}>
          <div style={{ display: 'flex', flexDirection: 'column', justifyContent: 'space-between', width: 28, paddingRight: 4, height: chartH }}>
            {ticks.map((t, i) => (
              <span key={i} style={{ fontSize: 8, color: 'rgba(255,255,255,0.3)', textAlign: 'right', lineHeight: 1 }}>{fmtTick(t)}</span>
            ))}
          </div>
          <div style={{ flex: 1, display: 'flex', alignItems: 'flex-end', gap: 1, height: chartH, borderLeft: '1px solid rgba(255,255,255,0.1)', borderBottom: '1px solid rgba(255,255,255,0.1)', paddingLeft: 1 }}>
            {counts.map((d, idx) => {
              const v = values[idx]
              const h = Math.max(2, Math.round((v / maxVal) * (chartH - 6)))
              const isToday = d.date === new Date().toISOString().slice(0, 10)
              const tip = isCalls ? `${d.date}: ${d.count} 次` : `${d.date}: ${formatTokens(d.tokens)}`
              return (
                <div key={d.date} style={{ flex: 1, display: 'flex', flexDirection: 'column', alignItems: 'center' }} title={tip}>
                  <div style={{ width: '100%', borderRadius: '2px 2px 0 0', height: h, background: isToday ? '#3b82f6' : v > 0 ? 'rgba(93,173,226,0.5)' : 'rgba(255,255,255,0.06)', transition: 'height 0.3s' }} />
                </div>
              )
            })}
          </div>
        </div>
        <div style={{ display: 'flex', marginTop: 3, paddingLeft: 32 }}>
          <div style={{ flex: 1, display: 'flex', justifyContent: 'space-between' }}>
            <span style={{ color: 'rgba(255,255,255,0.3)', fontSize: 8 }}>{counts[0]?.date.slice(5)}</span>
            <span style={{ color: 'rgba(255,255,255,0.3)', fontSize: 8 }}>{counts[Math.floor(counts.length / 2)]?.date.slice(5)}</span>
            <span style={{ color: 'rgba(255,255,255,0.3)', fontSize: 8 }}>{counts[counts.length - 1]?.date.slice(5)}</span>
          </div>
        </div>
      </div>
    </div>
  )
}

function MiniCharPairing({ characters, currentChar, onSelect }: {
  characters: CharacterMeta[]
  currentChar: string
  onSelect: (charName: string) => void
}) {
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', onClick)
    return () => document.removeEventListener('mousedown', onClick)
  }, [])

  const charsWithMini = characters.filter((c) => c.miniActions && Object.keys(c.miniActions).length > 0)
  const charMeta = characters.find((c) => c.name === currentChar)
  const gif = charMeta ? getMiniGif(charMeta, false, true) : undefined

  return (
    <div className="bg-white rounded-xl border border-gray-200 shadow-sm p-4">
      <div className="flex items-center gap-3">
        <div className="w-11 h-11 rounded-lg bg-gray-100 border border-gray-200 flex items-center justify-center overflow-hidden shrink-0">
          {gif ? (
            <img src={gif} alt="" className="w-9 h-9 object-contain" style={{ imageRendering: 'pixelated' }} draggable={false} />
          ) : (
            <span className="text-gray-400 text-lg">?</span>
          )}
        </div>
        <div className="flex-1 min-w-0">
          <div className="text-sm font-semibold text-gray-900">Mini 角色</div>
          <div className="text-xs text-gray-400">Notch 上显示的角色</div>
        </div>
        <div ref={ref} className="relative shrink-0" style={{ minWidth: 140 }}>
          <button
            onClick={() => setOpen(!open)}
            className="w-full flex items-center justify-between bg-gray-50 border border-gray-200 rounded-lg px-3 py-2 text-sm text-left cursor-pointer hover:bg-gray-100 transition-colors focus:outline-none focus:ring-2 focus:ring-gray-900/10 focus:border-gray-900"
          >
            <span className={currentChar ? 'text-gray-900 font-medium truncate' : 'text-gray-400 truncate'}>
              {currentChar || '自动选择'}
            </span>
            <svg className={`w-4 h-4 text-gray-400 shrink-0 ml-2 transition-transform ${open ? 'rotate-180' : ''}`} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m6 9 6 6 6-6"/></svg>
          </button>

          {open && (
            <div className="absolute top-full right-0 mt-1.5 bg-white border border-gray-200 rounded-xl shadow-lg z-30 py-1.5 max-h-48 overflow-y-auto" style={{ minWidth: 180 }}>
              <button
                onClick={() => { onSelect(''); setOpen(false) }}
                className={`w-full flex items-center gap-2.5 px-3 py-2 text-left text-sm transition-colors ${
                  !currentChar ? 'bg-gray-50 text-gray-900' : 'text-gray-500 hover:bg-gray-50'
                }`}
              >
                <div className="w-7 h-7 rounded-md bg-gray-100 flex items-center justify-center shrink-0">
                  <span className="text-gray-300 text-xs">--</span>
                </div>
                <span className={!currentChar ? 'font-medium' : ''}>自动选择</span>
              </button>
              {charsWithMini.map((c) => {
                const isSelected = c.name === currentChar
                const preview = getMiniGif(c, false, true)
                return (
                  <button
                    key={c.name}
                    onClick={() => { onSelect(c.name); setOpen(false) }}
                    className={`w-full flex items-center gap-2.5 px-3 py-2 text-left text-sm transition-colors ${
                      isSelected ? 'bg-gray-50 text-gray-900' : 'text-gray-700 hover:bg-gray-50'
                    }`}
                  >
                    <div className="w-7 h-7 rounded-md bg-gray-100 border border-gray-200 flex items-center justify-center overflow-hidden shrink-0">
                      {preview ? (
                        <img src={preview} alt="" className="w-6 h-6 object-contain" style={{ imageRendering: 'pixelated' }} draggable={false} />
                      ) : (
                        <span className="text-gray-300 text-xs">?</span>
                      )}
                    </div>
                    <span className={isSelected ? 'font-medium' : ''}>{c.name}</span>
                    {isSelected && (
                      <svg className="w-3.5 h-3.5 text-emerald-500 shrink-0 ml-auto" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round"><path d="M20 6 9 17l-5-5"/></svg>
                    )}
                  </button>
                )
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

function PairingRow({ agent, characters, currentChar, onSelect }: {
  agent: AgentInfo
  characters: CharacterMeta[]
  currentChar: string
  onSelect: (charName: string) => void
}) {
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', onClick)
    return () => document.removeEventListener('mousedown', onClick)
  }, [])

  const charsWithMini = characters.filter((c) => c.miniActions && Object.keys(c.miniActions).length > 0)
  const charMeta = characters.find((c) => c.name === currentChar)
  const gif = charMeta ? getMiniGif(charMeta, false) : undefined

  return (
    <div className="bg-white rounded-xl border border-gray-200 shadow-sm p-4">
      <div className="flex items-center gap-3">
        <div className="w-11 h-11 rounded-lg bg-gray-100 border border-gray-200 flex items-center justify-center overflow-hidden shrink-0">
          {gif ? (
            <img src={gif} alt="" className="w-9 h-9 object-contain" style={{ imageRendering: 'pixelated' }} draggable={false} />
          ) : (
            <span className="text-gray-400 text-lg">{agent.identityEmoji || '?'}</span>
          )}
        </div>
        <div className="flex-1 min-w-0">
          <div className="text-sm font-semibold text-gray-900 truncate">
            {agent.identityEmoji || ''} {agent.identityName || agent.id}
          </div>
          <div className="text-xs text-gray-400 truncate font-mono">{agent.id}</div>
        </div>
        <div ref={ref} className="relative shrink-0" style={{ minWidth: 140 }}>
          <button
            onClick={() => setOpen(!open)}
            className="w-full flex items-center justify-between bg-gray-50 border border-gray-200 rounded-lg px-3 py-2 text-sm text-left cursor-pointer hover:bg-gray-100 transition-colors focus:outline-none focus:ring-2 focus:ring-gray-900/10 focus:border-gray-900"
          >
            <span className={currentChar ? 'text-gray-900 font-medium truncate' : 'text-gray-400 truncate'}>
              {currentChar || '未分配'}
            </span>
            <svg className={`w-4 h-4 text-gray-400 shrink-0 ml-2 transition-transform ${open ? 'rotate-180' : ''}`} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="m6 9 6 6 6-6"/></svg>
          </button>

          {open && (
            <div className="absolute top-full right-0 mt-1.5 bg-white border border-gray-200 rounded-xl shadow-lg z-30 py-1.5 max-h-48 overflow-y-auto" style={{ minWidth: 180 }}>
              <button
                onClick={() => { onSelect(''); setOpen(false) }}
                className={`w-full flex items-center gap-2.5 px-3 py-2 text-left text-sm transition-colors ${
                  !currentChar ? 'bg-gray-50 text-gray-900' : 'text-gray-500 hover:bg-gray-50'
                }`}
              >
                <div className="w-7 h-7 rounded-md bg-gray-100 flex items-center justify-center shrink-0">
                  <span className="text-gray-300 text-xs">--</span>
                </div>
                <span className={!currentChar ? 'font-medium' : ''}>未分配</span>
              </button>
              {charsWithMini.map((c) => {
                const isSelected = c.name === currentChar
                const preview = getMiniGif(c, false)
                return (
                  <button
                    key={c.name}
                    onClick={() => { onSelect(c.name); setOpen(false) }}
                    className={`w-full flex items-center gap-2.5 px-3 py-2 text-left text-sm transition-colors ${
                      isSelected ? 'bg-gray-50 text-gray-900' : 'text-gray-700 hover:bg-gray-50'
                    }`}
                  >
                    <div className="w-7 h-7 rounded-md bg-gray-100 border border-gray-200 flex items-center justify-center overflow-hidden shrink-0">
                      {preview ? (
                        <img src={preview} alt="" className="w-6 h-6 object-contain" style={{ imageRendering: 'pixelated' }} draggable={false} />
                      ) : (
                        <span className="text-gray-300 text-xs">?</span>
                      )}
                    </div>
                    <span className={isSelected ? 'font-medium' : ''}>{c.name}</span>
                    {isSelected && (
                      <svg className="w-3.5 h-3.5 text-emerald-500 shrink-0 ml-auto" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round"><path d="M20 6 9 17l-5-5"/></svg>
                    )}
                  </button>
                )
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

async function getOcParams(): Promise<{ mode?: string; url?: string; token?: string }> {
  const store = await load('settings.json', { defaults: {}, autoSave: true })
  const mode = ((await store.get('oc_mode')) as string) || 'local'
  if (mode !== 'remote') return {}
  const url = ((await store.get('gateway_url')) as string) || ''
  const token = ((await store.get('gateway_token')) as string) || ''
  return { mode, url, token }
}

export default function Mini() {
  const [expanded, setExpanded] = useState(false)
  const [showPanel, setShowPanel] = useState(false)
  const [agents, setAgents] = useState<AgentInfo[]>([])
  const [healthMap, setHealthMap] = useState<Record<string, boolean>>({})
  const [characters, setCharacters] = useState<CharacterMeta[]>([])
  const [agentCharMap, setAgentCharMap] = useState<Record<string, string>>({})
  const [miniChar, setMiniChar] = useState<CharacterMeta | null>(null)
  const [bobPhase, setBobPhase] = useState(0)
  const [allSessions, setAllSessions] = useState<MiniSessionInfo[]>([])
  const [anySessionActive, setAnySessionActive] = useState(false)
  const dismissedSessionsRef = useRef<Map<string, number>>(new Map())

  // Agent detail
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null)
  const [metrics, setMetrics] = useState<AgentMetrics | null>(null)
  const [extraInfo, setExtraInfo] = useState<any>(null)

  // OpenClaw session chat
  const [selectedSessionKey, setSelectedSessionKey] = useState<{ agentId: string, key: string } | null>(null)
  const [sessionMessages, setSessionMessages] = useState<any[]>([])

  // Claude Code
  const [claudeSessions, setClaudeSessions] = useState<any[]>([])
  const [claudeCharName, setClaudeCharName] = useState('')
  const [selectedClaudeSession, setSelectedClaudeSession] = useState<string | null>(null)
  const [claudeConversation, setClaudeConversation] = useState<any[]>([])

  // Feature toggles
  const [enableOpenClaw, setEnableOpenClaw] = useState(true)
  const [enableClaudeCode, setEnableClaudeCode] = useState(false)
  const [soundEnabled, setSoundEnabled] = useState(true)

  // Settings mode: panel becomes wider, shows settings content
  const [settingsMode, setSettingsMode] = useState(false)
  const settingsModeRef = useRef(false)
  const filePickerOpenRef = useRef(false)
  const [settingsNav, setSettingsNav] = useState<'create' | 'pairing' | 'settings'>('create')
  const [showWorkDetail, setShowWorkDetail] = useState(false)
  const [hiding, setHiding] = useState(false)

  // Bob animation (only when collapsed, avoid 60fps re-renders in settings mode)
  useEffect(() => {
    if (expanded) return
    let frame: number
    const animate = () => {
      setBobPhase(Date.now())
      frame = requestAnimationFrame(animate)
    }
    frame = requestAnimationFrame(animate)
    return () => cancelAnimationFrame(frame)
  }, [expanded])

  const bobY = Math.sin(bobPhase / 600) * 3

  // Load mini character from store
  const loadMiniChar = useCallback(async () => {
    const store = await load('settings.json', { defaults: {}, autoSave: true })
    await store.reload()
    const miniCharName = ((await store.get('mini_character')) as string) || ''
    const chars = (await store.get('characters')) as CharacterMeta[] | null
    if (miniCharName && chars) {
      const found = chars.find((c) => c.name === miniCharName)
      if (found) { setMiniChar(found); return }
    }
    if (chars) {
      const fallback = chars.find((c) => c.miniActions && Object.keys(c.miniActions).length > 0)
      if (fallback) setMiniChar(fallback)
    }
  }, [])

  useEffect(() => {
    loadMiniChar()
    const unlisten = listen('character-changed', () => loadMiniChar())
    return () => { unlisten.then((fn) => fn()) }
  }, [loadMiniChar])

  const fetchAgents = useCallback(async () => {
    try {
      const store = await load('settings.json', { defaults: {}, autoSave: true })
      const oc = await getOcParams()
      const [agentList, charMap] = await Promise.all([
        invoke('get_agents', oc) as Promise<AgentInfo[]>,
        store.get('agent_char_map') as Promise<Record<string, string> | null>,
      ])
      const chars = (await store.get('characters')) as CharacterMeta[] | null
      setAgents(agentList)
      setCharacters(chars || [])
      setAgentCharMap(charMap || {})
    } catch { /* ignore */ }
  }, [])

  const pollHealth = useCallback(async () => {
    try {
      const oc = await getOcParams()
      const health = (await invoke('get_health', oc)) as { agents: AgentHealth[] }
      const hMap: Record<string, boolean> = {}
      health.agents.forEach((a) => { hMap[a.agentId] = a.active })
      setHealthMap(hMap)
    } catch { /* ignore */ }
  }, [])

  const fetchAllSessions = useCallback(async () => {
    if (agents.length === 0) { setAllSessions([]); return }
    const oc = await getOcParams()
    const results: MiniSessionInfo[] = []
    await Promise.all(
      agents.map(async (agent) => {
        try {
          const s = (await invoke('get_agent_sessions', { agentId: agent.id, ...oc })) as MiniSessionInfo[]
          results.push(...s)
        } catch { /* ignore */ }
      })
    )
    const filtered = results.filter(s => {
      const key = `${s.agentId}:${s.key}`
      const dismissedAt = dismissedSessionsRef.current.get(key)
      if (dismissedAt !== undefined && s.updatedAt > dismissedAt) {
        dismissedSessionsRef.current.delete(key)
      }
      return !dismissedSessionsRef.current.has(key)
    })
    filtered.sort((a, b) => (b.active ? 1 : 0) - (a.active ? 1 : 0) || b.updatedAt - a.updatedAt)
    setAllSessions(filtered)
  }, [agents])

  useEffect(() => {
    fetchAgents()
    pollHealth()
    const a = setInterval(fetchAgents, 5000)
    const h = setInterval(pollHealth, 1000)
    return () => { clearInterval(a); clearInterval(h) }
  }, [fetchAgents, pollHealth])

  const pollActiveStatus = useCallback(async () => {
    try {
      const oc = await getOcParams()
      const activeKeys = (await invoke('get_active_sessions', oc)) as string[]
      setAnySessionActive(activeKeys.length > 0)
      const activeSet = new Set(activeKeys)
      setAllSessions(prev => {
        let changed = false
        const updated = prev.map(s => {
          const key = `${s.agentId}:${s.key}`
          const isActive = activeSet.has(key)
          if (s.active !== isActive) { changed = true; return { ...s, active: isActive } }
          return s
        })
        if (!changed) return prev
        updated.sort((a, b) => (b.active ? 1 : 0) - (a.active ? 1 : 0) || b.updatedAt - a.updatedAt)
        return updated
      })
    } catch { /* ignore */ }
  }, [])

  useEffect(() => {
    pollActiveStatus()
    const t = setInterval(pollActiveStatus, 1000)
    return () => clearInterval(t)
  }, [pollActiveStatus])

  useEffect(() => {
    if (!expanded) return
    fetchAllSessions()
    const t1 = setInterval(fetchAllSessions, 5000)
    return () => clearInterval(t1)
  }, [expanded, fetchAllSessions])

  // Load feature toggles
  useEffect(() => {
    (async () => {
      const store = await load('settings.json', { defaults: {}, autoSave: true })
      const oc = await store.get('enable_openclaw')
      if (typeof oc === 'boolean') setEnableOpenClaw(oc)
      const cc = await store.get('enable_claudecode')
      if (typeof cc === 'boolean') setEnableClaudeCode(cc)
      const snd = await store.get('sound_enabled')
      if (typeof snd === 'boolean') setSoundEnabled(snd)
      const ccChar = ((await store.get('claude_char')) as string) || ''
      setClaudeCharName(ccChar)
    })()
  }, [])

  // Poll Claude Code sessions
  useEffect(() => {
    if (!enableClaudeCode) { setClaudeSessions([]); return }
    const poll = async () => {
      try {
        const sessions = await invoke('get_claude_sessions') as any[]
        setClaudeSessions(sessions)
      } catch { /* ignore */ }
    }
    poll()
    const t = setInterval(poll, 2000)
    return () => clearInterval(t)
  }, [enableClaudeCode])

  // Listen for Claude task completion → play sound
  const soundEnabledRef = useRef(soundEnabled)
  soundEnabledRef.current = soundEnabled
  useEffect(() => {
    if (!enableClaudeCode) return
    const unlisten = listen('claude-task-complete', () => {
      if (soundEnabledRef.current) {
        invoke('play_sound', { name: 'Purr' }).catch(() => {})
      }
    })
    return () => { unlisten.then((fn) => fn()) }
  }, [enableClaudeCode])

  // Fetch OpenClaw session messages when selected
  useEffect(() => {
    if (!selectedSessionKey) { setSessionMessages([]); return }
    let cancelled = false
    const fetchMsgs = async () => {
      try {
        const msgs = await invoke('get_session_messages', { agentId: selectedSessionKey.agentId, sessionKey: selectedSessionKey.key }) as any[]
        if (!cancelled) setSessionMessages(msgs)
      } catch { if (!cancelled) setSessionMessages([]) }
    }
    fetchMsgs()
    const t = setInterval(fetchMsgs, 3000)
    return () => { cancelled = true; clearInterval(t) }
  }, [selectedSessionKey])

  // Fetch Claude conversation when selected
  useEffect(() => {
    if (!selectedClaudeSession) { setClaudeConversation([]); return }
    let cancelled = false
    const fetch = async () => {
      try {
        const msgs = await invoke('get_claude_conversation', { sessionId: selectedClaudeSession }) as any[]
        if (!cancelled) setClaudeConversation(msgs)
      } catch { if (!cancelled) setClaudeConversation([]) }
    }
    fetch()
    const t = setInterval(fetch, 3000)
    return () => { cancelled = true; clearInterval(t) }
  }, [selectedClaudeSession])

  // Fetch agent metrics when selected
  useEffect(() => {
    if (!selectedAgentId) { setMetrics(null); setExtraInfo(null); return }
    let cancelled = false
    const fetchMetrics = async () => {
      try {
        const oc = await getOcParams()
        const m = (await invoke('get_agent_metrics', { agentId: selectedAgentId, ...oc })) as AgentMetrics
        if (!cancelled) setMetrics(m)
      } catch { if (!cancelled) setMetrics(null) }
    }
    const fetchExtra = async () => {
      try {
        const e = (await invoke('get_agent_extra_info', { agentId: selectedAgentId })) as any
        if (!cancelled) setExtraInfo(e)
      } catch { if (!cancelled) setExtraInfo(null) }
    }
    fetchMetrics()
    fetchExtra()
    const i1 = setInterval(fetchMetrics, 2000)
    const i2 = setInterval(fetchExtra, 10000)
    return () => { cancelled = true; clearInterval(i1); clearInterval(i2) }
  }, [selectedAgentId])

  useEffect(() => {
    getStore().then((s) => s.get('show_work_detail')).then((v) => {
      if (typeof v === 'boolean') setShowWorkDetail(v)
    }).catch(() => {})
  }, [])

  // Build character slots (OpenClaw + Claude Code)
  const ocSlots: SessionSlot[] = allSessions.slice(0, MAX_SLOTS).map((s, i) => {
    const agent = agents.find(a => a.id === s.agentId) || { id: s.agentId }
    const charName = agentCharMap[s.agentId]
    const char = characters.find((c) => c.name === charName) || DEFAULT_CHAR
    return { agentId: s.agentId, sessionIdx: i, agent, char, isWorking: s.active }
  })
  const claudeSlots: SessionSlot[] = claudeSessions.map((cs, i) => {
    const isActive = cs.status === 'processing' || cs.status === 'tool_running'
    const char = characters.find((c) => c.name === claudeCharName) || DEFAULT_CHAR
    return { agentId: `claude:${cs.sessionId}`, sessionIdx: ocSlots.length + i, agent: { id: `claude:${cs.sessionId}`, identityName: 'Claude', identityEmoji: '🤖' }, char, isWorking: isActive }
  })
  const sessionSlots = [...ocSlots, ...claudeSlots].slice(0, MAX_SLOTS)

  const expand = useCallback(async () => {
    setHiding(true)
    // Wait for the browser to paint the hidden state before resizing the window
    await new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())))
    await invoke('set_mini_expanded', { expanded: true })
    setHiding(false)
    setExpanded(true)
    requestAnimationFrame(() => {
      requestAnimationFrame(() => setShowPanel(true))
    })
  }, [])

  const collapse = useCallback(async () => {
    setShowPanel(false)
    setSelectedAgentId(null)
    setSelectedClaudeSession(null)
    setSelectedSessionKey(null)
    const wasSettings = settingsModeRef.current
    setTimeout(async () => {
      settingsModeRef.current = false
      setSettingsMode(false)
      setExpanded(false)
      if (wasSettings) {
        await invoke('set_mini_size', { restore: true })
      } else {
        await invoke('set_mini_expanded', { expanded: false })
      }
    }, 480)
  }, [])

  const enterSettings = useCallback(async () => {
    // 1. Collapse current panel
    setShowPanel(false)
    setSelectedAgentId(null)
    setSelectedClaudeSession(null)
    // 2. After collapse, switch to settings mode + resize window + re-expand
    setTimeout(async () => {
      settingsModeRef.current = true
      setSettingsMode(true)
      try { await invoke('set_mini_size', { restore: false }) } catch {}
      requestAnimationFrame(() => {
        requestAnimationFrame(() => setShowPanel(true))
      })
    }, 480)
  }, [])

  const exitSettings = useCallback(async () => {
    // 1. Collapse panel
    setShowPanel(false)
    // 2. After collapse, switch back to normal mode + resize + re-expand
    setTimeout(async () => {
      settingsModeRef.current = false
      setSettingsMode(false)
      setSettingsNav('create')
      try { await invoke('set_mini_expanded', { expanded: true }) } catch {}
      requestAnimationFrame(() => {
        requestAnimationFrame(() => setShowPanel(true))
      })
    }, 480)
  }, [])

  const toggleWorkDetail = async (val: boolean) => {
    setShowWorkDetail(val)
    const s = await getStore()
    await s.set('show_work_detail', val)
    await s.save()
  }

  // Click outside to collapse
  useEffect(() => {
    if (!expanded) return
    const onClick = (e: MouseEvent) => {
      if (!(e.target as HTMLElement).closest('#mini-panel')) collapse()
    }
    window.addEventListener('mousedown', onClick)
    return () => window.removeEventListener('mousedown', onClick)
  }, [expanded, collapse])

  // Window blur: collapse when user clicks outside the app (settings mode)
  // Skip blur when a file picker dialog is open
  useEffect(() => {
    if (!settingsMode) return
    // Intercept file input clicks to set the flag
    const onClickCapture = (e: MouseEvent) => {
      const el = e.target as HTMLElement
      if (el instanceof HTMLInputElement && el.type === 'file') {
        filePickerOpenRef.current = true
      }
    }
    const onFocus = () => { filePickerOpenRef.current = false }
    const onBlur = () => {
      if (filePickerOpenRef.current) return
      collapse()
    }
    window.addEventListener('click', onClickCapture, true)
    window.addEventListener('blur', onBlur)
    window.addEventListener('focus', onFocus)
    return () => {
      window.removeEventListener('click', onClickCapture, true)
      window.removeEventListener('blur', onBlur)
      window.removeEventListener('focus', onFocus)
    }
  }, [settingsMode, collapse])

  const handleMouseDown = async (e: React.MouseEvent) => {
    if (e.detail === 1 && !(e.target as HTMLElement).closest('[data-no-drag]')) {
      await getCurrentWindow().startDragging()
    }
  }

  const claudeWorking = claudeSessions.some(cs => cs.status === 'processing' || cs.status === 'tool_running')
  const hasWorking = anySessionActive || Object.values(healthMap).some(Boolean) || claudeWorking
  const miniGif = getMiniGif(miniChar ?? undefined, hasWorking, true)
  const inAgentDetail = selectedAgentId !== null
  const selectedAgent = agents.find(a => a.id === selectedAgentId)

  // Panel dimensions depend on settingsMode
  const panelW = settingsMode ? '100vw' : 380
  const panelH = settingsMode ? '100vh' : 560

  return (
    <div style={{
      width: '100vw', height: '100vh',
      background: 'transparent', overflow: 'hidden', userSelect: 'none',
    }}>
      {/* Collapsed */}
      {!expanded && !hiding && (
        <div
          id="mini-panel"
          onClick={() => expand()}
          onMouseDown={handleMouseDown}
          style={{
            width: '100%', height: '100%',
            display: 'flex', alignItems: 'flex-start', justifyContent: 'center',
            cursor: 'grab',
            pointerEvents: 'auto',
          }}
        >
          <div style={{
            transform: `translateY(${bobY}px)`,
            transition: 'transform 0.1s ease',
            position: 'relative',
          }}>
            {miniGif ? (
              <img src={miniGif} alt="mini"
                style={{ width: 40, height: 40, objectFit: 'contain' }}
                draggable={false} />
            ) : (
              <div style={{
                width: 40, height: 40, borderRadius: 10,
                background: 'rgba(0,0,0,0.3)',
                display: 'flex', alignItems: 'center', justifyContent: 'center',
                color: '#999', fontSize: 16,
              }}>?</div>
            )}
            <div style={{
              position: 'absolute', bottom: 0, right: 0,
              width: 8, height: 8, borderRadius: '50%',
              background: hasWorking ? '#2ecc71' : '#777',
              border: '1.5px solid rgba(0,0,0,0.3)',
            }} />
          </div>
        </div>
      )}

      {/* Expanded panel */}
      {expanded && (
        <div id="mini-panel" style={{
          position: 'absolute', top: 0,
          left: '50%',
          transform: 'translateX(-50%)',
          width: showPanel ? panelW : 200,
          maxHeight: showPanel ? panelH : 32,
          overflow: 'hidden',
          background: '#1a1a1a',
          borderRadius: showPanel ? (settingsMode ? '0 0 16px 16px' : '0 0 24px 24px') : '0 0 14px 14px',
          boxShadow: showPanel
            ? '0 8px 32px rgba(0,0,0,0.7)'
            : '0 2px 8px rgba(0,0,0,0.3)',
          transition: showPanel
            ? 'width 0.42s cubic-bezier(0.34, 1.3, 0.64, 1), max-height 0.42s cubic-bezier(0.34, 1.3, 0.64, 1), border-radius 0.42s cubic-bezier(0.34, 1.3, 0.64, 1), box-shadow 0.35s ease'
            : 'width 0.45s cubic-bezier(0.4, 0, 0.2, 1), max-height 0.45s cubic-bezier(0.4, 0, 0.2, 1), border-radius 0.45s cubic-bezier(0.4, 0, 0.2, 1), box-shadow 0.2s ease',
        }}>
          <div style={{
            opacity: showPanel ? 1 : 0,
            transform: showPanel ? 'scale(1) translateY(0)' : 'scale(0.8) translateY(-8px)',
            transformOrigin: 'top center',
            transition: showPanel
              ? 'opacity 0.35s ease, transform 0.35s ease'
              : 'opacity 0.15s ease-out, transform 0.15s ease-out',
            height: settingsMode ? '100vh' : 'auto',
            display: settingsMode ? 'flex' : 'block',
            flexDirection: 'column',
          }}>
            {/* Header */}
            <div style={{
              display: 'flex', alignItems: 'center', justifyContent: 'space-between',
              height: 36, padding: '0 14px', flexShrink: 0,
              borderBottom: '1px solid rgba(255,255,255,0.06)',
            }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 6, minWidth: 0, flex: 1 }}>
                {settingsMode ? (
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                    <button data-no-drag
                      onClick={(e) => { e.stopPropagation(); exitSettings() }}
                      style={{
                        background: 'rgba(255,255,255,0.06)', border: 'none',
                        color: 'rgba(255,255,255,0.6)', fontSize: 11,
                        cursor: 'pointer', padding: '3px 8px',
                        borderRadius: 6, display: 'flex', alignItems: 'center', gap: 4,
                      }}>
                      <span style={{ fontSize: 13 }}>&lsaquo;</span> 返回
                    </button>
                    {(['create', 'pairing', 'settings'] as const).map((nav) => (
                      <button key={nav} data-no-drag
                        onClick={(e) => { e.stopPropagation(); setSettingsNav(nav) }}
                        style={{
                          background: settingsNav === nav ? 'rgba(255,255,255,0.12)' : 'none',
                          border: 'none',
                          color: settingsNav === nav ? '#fff' : 'rgba(255,255,255,0.4)',
                          fontSize: 11, cursor: 'pointer', padding: '3px 10px',
                          borderRadius: 6, fontWeight: settingsNav === nav ? 600 : 400,
                        }}>
                        {nav === 'create' ? '角色创建' : nav === 'pairing' ? '配对' : '设置'}
                      </button>
                    ))}
                  </div>
                ) : (inAgentDetail || selectedClaudeSession || selectedSessionKey) ? (
                  <button data-no-drag
                    onClick={(e) => { e.stopPropagation(); setSelectedAgentId(null); setSelectedClaudeSession(null); setSelectedSessionKey(null) }}
                    style={{
                      background: 'rgba(255,255,255,0.06)', border: 'none',
                      color: 'rgba(255,255,255,0.6)', fontSize: 11,
                      cursor: 'pointer', padding: '3px 8px',
                      borderRadius: 6, display: 'flex', alignItems: 'center', gap: 4,
                    }}>
                    <span style={{ fontSize: 13 }}>&lsaquo;</span> Back
                  </button>
                ) : (
                  <div style={{ color: 'rgba(255,255,255,0.45)', fontSize: 10 }}>
                    {allSessions.length + claudeSessions.length} session{(allSessions.length + claudeSessions.length) !== 1 ? 's' : ''}
                  </div>
                )}
              </div>
              <div style={{ display: 'flex', alignItems: 'center', gap: 2 }}>
                {!settingsMode && (
                  <button data-no-drag
                    onClick={async (e) => {
                      e.stopPropagation()
                      const next = !soundEnabled
                      setSoundEnabled(next)
                      const store = await load('settings.json', { defaults: {}, autoSave: true })
                      await store.set('sound_enabled', next)
                      await store.save()
                      if (next) invoke('play_sound', { name: 'Purr' }).catch(() => {})
                    }}
                    style={{
                      background: 'none', border: 'none',
                      color: soundEnabled ? 'rgba(255,255,255,0.35)' : 'rgba(255,255,255,0.15)', fontSize: 14,
                      cursor: 'pointer', padding: '4px 6px',
                      position: 'relative',
                    }}
                    title={soundEnabled ? '提示音: 开' : '提示音: 关'}
                  >
                    <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9"/>
                      <path d="M13.73 21a2 2 0 0 1-3.46 0"/>
                      {!soundEnabled && <line x1="1" y1="1" x2="23" y2="23" strokeWidth="3"/>}
                    </svg>
                  </button>
                )}
                {!settingsMode && (
                  <button data-no-drag
                    onClick={(e) => { e.stopPropagation(); enterSettings() }}
                    style={{
                      background: 'none', border: 'none',
                      color: 'rgba(255,255,255,0.35)', fontSize: 14,
                      cursor: 'pointer', padding: '4px 6px',
                    }}
                    title="设置"
                  >&#9881;</button>
                )}
                <button data-no-drag
                  onClick={(e) => { e.stopPropagation(); collapse() }}
                  style={{
                    background: 'none', border: 'none',
                    color: 'rgba(255,255,255,0.35)', fontSize: 13,
                    cursor: 'pointer', padding: '4px 6px',
                  }}>x</button>
              </div>
            </div>

            {/* ===== Settings content ===== */}
            {settingsMode ? (
              <div data-no-drag className="scrollbar-thin" style={{ flex: 1, overflow: 'hidden', margin: 8, marginTop: 0, borderRadius: 12, minHeight: 0, display: 'flex', flexDirection: 'column' }}>
                <div className="bg-[#fafafa] text-gray-800 font-sans antialiased scrollbar-thin" style={{ borderRadius: '12px 12px 0 0', overflow: 'auto', flex: 1, minHeight: 0 }}>
                  {settingsNav === 'create' && <GifMakerTab />}
                  {settingsNav === 'pairing' && (
                    <div className="h-full overflow-y-auto bg-slate-50 p-6 scrollbar-thin">
                      <div className="max-w-2xl mx-auto space-y-4">
                        <p className="text-sm text-gray-500">将 Agent 与角色配对，配对后 Mini 中会显示对应角色的 GIF 动画。</p>

                        {/* OpenClaw agents */}
                        {enableOpenClaw && (
                          <>
                            <div className="text-xs font-semibold text-gray-400 uppercase tracking-wider">OpenClaw Agents</div>
                            {agents.length === 0 ? (
                              <div className="text-center text-gray-400 py-4 text-sm">等待 agent 上线...</div>
                            ) : (
                              <div className="space-y-3">
                                {agents.map((agent) => (
                                  <PairingRow
                                    key={agent.id}
                                    agent={agent}
                                    characters={characters}
                                    currentChar={agentCharMap[agent.id] || ''}
                                    onSelect={async (charName) => {
                                      const updated = { ...agentCharMap, [agent.id]: charName }
                                      setAgentCharMap(updated)
                                      await saveAgentCharMap(updated)
                                    }}
                                  />
                                ))}
                              </div>
                            )}
                          </>
                        )}

                        {/* Claude Code agent */}
                        {enableClaudeCode && (
                          <>
                            <div className="text-xs font-semibold text-gray-400 uppercase tracking-wider pt-2">Claude Code</div>
                            <PairingRow
                              agent={{ id: 'claude-code', identityName: 'Claude Code', identityEmoji: '🤖' }}
                              characters={characters}
                              currentChar={claudeCharName}
                              onSelect={async (charName) => {
                                setClaudeCharName(charName)
                                const store = await load('settings.json', { defaults: {}, autoSave: true })
                                await store.set('claude_char', charName)
                                await store.save()
                              }}
                            />
                          </>
                        )}

                        {/* Mini 角色 (notch 上显示的角色) */}
                        <div className="pt-4 border-t border-gray-200">
                          <MiniCharPairing
                            characters={characters}
                            currentChar={miniChar?.name || ''}
                            onSelect={async (name) => {
                              const store = await load('settings.json', { defaults: {}, autoSave: true })
                              await store.set('mini_character', name)
                              await store.save()
                              loadMiniChar()
                            }}
                          />
                        </div>

                        {agents.length > 0 && characters.filter((c) => c.miniActions && Object.keys(c.miniActions).length > 0).length < agents.length && (
                          <div className="text-xs text-amber-600 bg-amber-50 border border-amber-200 rounded-lg p-3">
                            角色数量不足，请先在「角色创建」中创建更多带 Mini 动画的角色。
                          </div>
                        )}
                      </div>
                    </div>
                  )}
                  {settingsNav === 'settings' && (
                    <div className="h-full overflow-y-auto bg-slate-50">
                      <SettingsTab showWorkDetail={showWorkDetail} onToggleWorkDetail={toggleWorkDetail} />
                    </div>
                  )}
                </div>
                <div style={{
                  background: '#1a1a1a', padding: '8px 14px',
                  borderRadius: '0 0 12px 12px', flexShrink: 0,
                  display: 'flex', alignItems: 'center', justifyContent: 'center',
                }}>
                  <span
                    onClick={() => invoke('open_url', { url: 'https://github.com/rainnoon/oc-claw' })}
                    style={{ color: 'rgba(255,255,255,0.5)', fontSize: 11, cursor: 'pointer' }}
                    onMouseEnter={(e) => (e.currentTarget.style.color = 'rgba(255,255,255,0.8)')}
                    onMouseLeave={(e) => (e.currentTarget.style.color = 'rgba(255,255,255,0.5)')}
                  >
                    GitHub · 项目地址
                  </span>
                </div>
              </div>
            ) : (!inAgentDetail && !selectedClaudeSession && !selectedSessionKey) ? (
              /* ===== Normal: character island + sessions ===== */
              <div className="mini-fade-in">
                {/* Character island */}
                <div style={{
                  position: 'relative', height: 100,
                  backgroundImage: 'url(/assets/grass-island.png)',
                  backgroundSize: '80px 100%',
                  backgroundRepeat: 'repeat-x',
                  overflow: 'hidden',
                }}>
                  {sessionSlots.length === 0 && (
                    <div style={{
                      position: 'absolute', inset: 0,
                      display: 'flex', alignItems: 'center', justifyContent: 'center',
                      color: 'rgba(255,255,255,0.3)', fontSize: 11, zIndex: 2,
                    }}>
                      waiting for agents...
                    </div>
                  )}

                  {sessionSlots.map((slot, idx) => {
                    const gif = getMiniGif(slot.char, slot.isWorking, true)
                    const row = idx < 6 ? 0 : 1
                    const col = row === 0 ? idx : idx - 6
                    const cols = row === 0 ? Math.min(sessionSlots.length, 6) : Math.min(sessionSlots.length - 6, 4)
                    const slotW = 380 / Math.max(cols, 1)
                    const xBase = slotW * col + slotW / 2 - 22 + (row === 1 ? slotW * 0.4 : 0)
                    const yBase = row === 0 ? 4 : 52
                    const seed = (slot.agentId + slot.sessionIdx).split('').reduce((a, c) => a + c.charCodeAt(0), 0)
                    const jx = ((seed * 7) % 13) - 6
                    const jy = ((seed * 11) % 9) - 4
                    const x = Math.max(2, Math.min(332, xBase + jx))
                    const y = yBase + jy
                    return (
                      <div
                        key={`${slot.agentId}-${slot.sessionIdx}`}
                        data-no-drag
                        onClick={() => {
                          if (slot.agentId.startsWith('claude:')) {
                            setSelectedAgentId(null); setSelectedSessionKey(null)
                            setSelectedClaudeSession(slot.agentId.replace('claude:', ''))
                          } else {
                            setSelectedClaudeSession(null); setSelectedSessionKey(null)
                            setSelectedAgentId(slot.agentId)
                          }
                        }}
                        style={{
                          position: 'absolute', left: x, top: y,
                          display: 'flex', flexDirection: 'column', alignItems: 'center',
                          cursor: 'pointer', zIndex: 2,
                          transform: `translateY(${Math.sin((bobPhase + idx * 400) / 800) * 2}px)`,
                        }}
                      >
                        <div style={{ position: 'relative' }}>
                          {gif ? (
                            <img src={gif} alt={slot.char?.name}
                              style={{ width: 44, height: 44, objectFit: 'contain' }}
                              draggable={false} />
                          ) : (
                            <div style={{
                              width: 44, height: 44, borderRadius: 8,
                              background: 'rgba(255,255,255,0.1)',
                              display: 'flex', alignItems: 'center', justifyContent: 'center',
                              color: '#555', fontSize: 13,
                            }}>?</div>
                          )}
                        </div>
                      </div>
                    )
                  })}
                </div>

                {/* Session bars */}
                <div style={{ borderTop: '1px solid rgba(255,255,255,0.06)' }}>
                  {allSessions.length === 0 && claudeSessions.length === 0 && (
                    <div style={{
                      color: 'rgba(255,255,255,0.2)', fontSize: 10,
                      textAlign: 'center', padding: '24px 0',
                    }}>no sessions</div>
                  )}

                  <div className="scrollbar-thin" style={{ maxHeight: 4 * 56, overflowY: 'auto' }}>
                    {(() => {
                      // Merge OpenClaw + Claude sessions into unified list, sort by active first
                      const unified: { type: 'oc', data: MiniSessionInfo, active: boolean, updatedAt: number }[] = allSessions.map(s => ({ type: 'oc' as const, data: s, active: s.active, updatedAt: s.updatedAt }))
                      const claudeUnified = claudeSessions.map(cs => ({ type: 'claude' as const, data: cs, active: cs.status === 'processing' || cs.status === 'tool_running', updatedAt: cs.updatedAt || 0 }))
                      const merged = [...unified, ...claudeUnified].sort((a, b) => (b.active ? 1 : 0) - (a.active ? 1 : 0) || b.updatedAt - a.updatedAt)

                      const agentSeqCount: Record<string, number> = {}
                      return merged.map((item) => {
                        if (item.type === 'oc') {
                          const s = item.data
                          const agent = agents.find(a => a.id === s.agentId)
                          const seq = (agentSeqCount[s.agentId] = (agentSeqCount[s.agentId] || 0) + 1)
                          const agentName = `${agent?.identityEmoji || ''} ${agent?.identityName || s.agentId}`.trim()
                          return (
                            <div
                              key={`oc-${s.agentId}-${s.key}`}
                              data-no-drag
                              onClick={() => { setSelectedClaudeSession(null); setSelectedAgentId(null); setSelectedSessionKey({ agentId: s.agentId, key: s.key }) }}
                              style={{
                                padding: '8px 12px 8px 16px',
                                cursor: 'pointer',
                                borderBottom: '1px solid rgba(255,255,255,0.04)',
                                transition: 'background 0.12s',
                                display: 'flex', alignItems: 'center', gap: 10,
                              }}
                              onMouseEnter={(e) => e.currentTarget.style.background = 'rgba(255,255,255,0.04)'}
                              onMouseLeave={(e) => e.currentTarget.style.background = 'transparent'}
                            >
                              <div style={{
                                width: 8, height: 8, borderRadius: '50%', flexShrink: 0,
                                background: s.active ? '#2ecc71' : 'rgba(255,255,255,0.15)',
                                boxShadow: s.active ? '0 0 6px rgba(46,204,113,0.6)' : 'none',
                                animation: s.active ? 'miniPulse 1.5s ease-in-out infinite' : 'none',
                              }} />
                              <div style={{ flex: 1, minWidth: 0 }}>
                                <div style={{
                                  color: '#e0e0e0', fontSize: 11,
                                  overflow: 'hidden', textOverflow: 'ellipsis',
                                  whiteSpace: 'nowrap', lineHeight: 1.5,
                                }}>
                                  <span style={{ fontWeight: 600 }}>{agentName}</span>
                                  <span style={{ marginLeft: 4, fontSize: 10 }}>#{seq}</span>
                                  {s.lastUserMsg && (
                                    <span style={{ marginLeft: 8 }}>{s.lastUserMsg}</span>
                                  )}
                                </div>
                                <div style={{
                                  color: 'rgba(255,255,255,0.35)', fontSize: 10,
                                  overflow: 'hidden', textOverflow: 'ellipsis',
                                  whiteSpace: 'nowrap', lineHeight: 1.5,
                                  marginTop: 1,
                                }}>
                                  {s.lastAssistantMsg || '\u00A0'}
                                </div>
                              </div>
                              <button
                                data-no-drag
                                onClick={(e) => { e.stopPropagation(); dismissedSessionsRef.current.set(`${s.agentId}:${s.key}`, s.updatedAt); setAllSessions(prev => prev.filter(ss => !(ss.agentId === s.agentId && ss.key === s.key))) }}
                                style={{
                                  background: 'none', border: 'none', color: 'rgba(255,255,255,0.15)',
                                  fontSize: 14, cursor: 'pointer', padding: '2px 4px', flexShrink: 0,
                                  lineHeight: 1,
                                }}
                                onMouseEnter={(e) => e.currentTarget.style.color = 'rgba(255,255,255,0.5)'}
                                onMouseLeave={(e) => e.currentTarget.style.color = 'rgba(255,255,255,0.15)'}
                                title="移除"
                              >×</button>
                            </div>
                          )
                        } else {
                          const cs = item.data
                          const projectName = cs.cwd ? cs.cwd.split('/').pop() : 'unknown'
                          const isActive = item.active
                          return (
                            <div
                              key={`claude-${cs.sessionId}`}
                              data-no-drag
                              onClick={() => { setSelectedAgentId(null); setSelectedSessionKey(null); setSelectedClaudeSession(cs.sessionId) }}
                              style={{
                                padding: '8px 12px 8px 16px',
                                cursor: 'pointer',
                                borderBottom: '1px solid rgba(255,255,255,0.04)',
                                transition: 'background 0.12s',
                                display: 'flex', alignItems: 'center', gap: 10,
                              }}
                              onMouseEnter={(e) => e.currentTarget.style.background = 'rgba(255,255,255,0.04)'}
                              onMouseLeave={(e) => e.currentTarget.style.background = 'transparent'}
                            >
                              <div style={{
                                width: 8, height: 8, borderRadius: '50%', flexShrink: 0,
                                background: isActive ? '#3b82f6' : 'rgba(255,255,255,0.15)',
                                boxShadow: isActive ? '0 0 6px rgba(59,130,246,0.6)' : 'none',
                                animation: isActive ? 'miniPulse 1.5s ease-in-out infinite' : 'none',
                              }} />
                              <div style={{ flex: 1, minWidth: 0 }}>
                                <div style={{
                                  color: '#e0e0e0', fontSize: 11,
                                  overflow: 'hidden', textOverflow: 'ellipsis',
                                  whiteSpace: 'nowrap', lineHeight: 1.5,
                                }}>
                                  <span style={{ fontWeight: 600 }}>🤖 Claude</span>
                                  <span style={{ marginLeft: 6, color: 'rgba(255,255,255,0.4)', fontSize: 10 }}>{projectName}</span>
                                </div>
                                <div style={{
                                  color: 'rgba(255,255,255,0.35)', fontSize: 10,
                                  overflow: 'hidden', textOverflow: 'ellipsis',
                                  whiteSpace: 'nowrap', lineHeight: 1.5,
                                  marginTop: 1,
                                }}>
                                  {cs.tool ? `🔧 ${cs.tool}` : cs.status === 'stopped' ? 'idle' : cs.status === 'processing' ? 'thinking...' : cs.status === 'tool_running' ? 'working...' : cs.status}
                                  {cs.userPrompt ? ` · ${cs.userPrompt}` : ''}
                                </div>
                              </div>
                              <button
                                data-no-drag
                                onClick={(e) => { e.stopPropagation(); invoke('remove_claude_session', { sessionId: cs.sessionId }).catch(() => {}); setClaudeSessions(prev => prev.filter(s => s.sessionId !== cs.sessionId)) }}
                                style={{
                                  background: 'none', border: 'none', color: 'rgba(255,255,255,0.15)',
                                  fontSize: 14, cursor: 'pointer', padding: '2px 4px', flexShrink: 0,
                                  lineHeight: 1,
                                }}
                                onMouseEnter={(e) => e.currentTarget.style.color = 'rgba(255,255,255,0.5)'}
                                onMouseLeave={(e) => e.currentTarget.style.color = 'rgba(255,255,255,0.15)'}
                                title="移除"
                              >×</button>
                            </div>
                          )
                        }
                      })
                    })()}
                  </div>
                </div>
              </div>
            ) : selectedSessionKey ? (
              /* ===== OpenClaw session chat ===== */
              <div className="scrollbar-thin" style={{ maxHeight: 524, overflowY: 'auto', padding: '12px 14px' }}>
                {sessionMessages.length === 0 ? (
                  <div style={{ color: 'rgba(255,255,255,0.3)', fontSize: 11, textAlign: 'center', padding: '30px 0' }}>
                    loading...
                  </div>
                ) : (
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
                    {sessionMessages.slice(-30).map((msg, i) => (
                      msg.role === 'user' ? (
                        <div key={i} style={{ display: 'flex', justifyContent: 'flex-end' }}>
                          <div style={{
                            background: '#2ecc71', borderRadius: 18,
                            padding: '8px 14px', maxWidth: '80%',
                            color: '#fff', fontSize: 13, lineHeight: 1.5,
                            wordBreak: 'break-word', whiteSpace: 'pre-wrap',
                          }}>
                            {msg.text.length > 300 ? msg.text.slice(0, 300) + '...' : msg.text}
                          </div>
                        </div>
                      ) : (
                        <div key={i} style={{ display: 'flex', alignItems: 'flex-start', gap: 8 }}>
                          <div style={{ width: 5, height: 5, borderRadius: '50%', background: '#2ecc71', marginTop: 6, flexShrink: 0 }} />
                          <div style={{
                            color: '#ddd', fontSize: 13, lineHeight: 1.5,
                            wordBreak: 'break-word', whiteSpace: 'pre-wrap',
                            maxWidth: '90%',
                          }}>
                            {msg.text.length > 500 ? msg.text.slice(0, 500) + '...' : msg.text}
                          </div>
                        </div>
                      )
                    ))}
                  </div>
                )}
              </div>
            ) : selectedClaudeSession ? (
              /* ===== Claude session chat ===== */
              <div className="scrollbar-thin" style={{ maxHeight: 524, overflowY: 'auto', padding: '12px 14px' }}>
                {claudeConversation.length === 0 ? (
                  <div style={{ color: 'rgba(255,255,255,0.3)', fontSize: 11, textAlign: 'center', padding: '30px 0' }}>
                    loading...
                  </div>
                ) : (
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
                    {claudeConversation.slice(-20).map((msg, i) => (
                      msg.role === 'user' ? (
                        <div key={i} style={{ display: 'flex', justifyContent: 'flex-end' }}>
                          <div style={{
                            background: '#007AFF', borderRadius: 18,
                            padding: '8px 14px', maxWidth: '80%',
                            color: '#fff', fontSize: 13, lineHeight: 1.5,
                            wordBreak: 'break-word', whiteSpace: 'pre-wrap',
                          }}>
                            {msg.text.length > 300 ? msg.text.slice(0, 300) + '...' : msg.text}
                          </div>
                        </div>
                      ) : (
                        <div key={i} style={{ display: 'flex', alignItems: 'flex-start', gap: 8 }}>
                          <div style={{ width: 5, height: 5, borderRadius: '50%', background: '#007AFF', marginTop: 6, flexShrink: 0 }} />
                          <div style={{
                            color: '#ddd', fontSize: 13, lineHeight: 1.5,
                            wordBreak: 'break-word', whiteSpace: 'pre-wrap',
                            maxWidth: '90%',
                          }}>
                            {msg.text.length > 500 ? msg.text.slice(0, 500) + '...' : msg.text}
                          </div>
                        </div>
                      )
                    ))}
                  </div>
                )}
              </div>
            ) : (
              /* ===== Agent detail panel (dark) ===== */
              <div className="scrollbar-thin" style={{ maxHeight: 524, overflowY: 'auto', padding: '12px 14px' }}>
                {!metrics ? (
                  <div style={{ color: 'rgba(255,255,255,0.3)', fontSize: 11, textAlign: 'center', padding: '30px 0' }}>
                    loading...
                  </div>
                ) : (
                  <div>
                    <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 10 }}>
                      <div style={{ color: '#fff', fontSize: 14, fontWeight: 700 }}>
                        {selectedAgent?.identityEmoji || ''} {selectedAgent?.identityName || selectedAgentId}
                      </div>
                      <div style={{
                        background: metrics.active ? 'rgba(46,204,113,0.25)' : 'rgba(255,255,255,0.08)',
                        color: metrics.active ? '#2ecc71' : '#999',
                        fontSize: 10, padding: '2px 8px', borderRadius: 4, fontWeight: 600,
                      }}>
                        {metrics.active ? '工作中' : '空闲'}
                      </div>
                    </div>

                    {metrics.channel && (
                      <div style={{ color: 'rgba(255,255,255,0.35)', fontSize: 10, marginBottom: 8 }}>via {metrics.channel}</div>
                    )}

                    {metrics.currentTask && (
                      <div style={{ marginBottom: 10 }}>
                        <div style={{ color: 'rgba(255,255,255,0.4)', fontSize: 9, fontWeight: 700, textTransform: 'uppercase', letterSpacing: 0.5, marginBottom: 3 }}>当前任务</div>
                        <div style={{ color: '#ddd', fontSize: 11, lineHeight: 1.5, wordBreak: 'break-word', background: 'rgba(255,255,255,0.04)', padding: '6px 8px', borderRadius: 6 }}>
                          {metrics.currentTask}
                        </div>
                      </div>
                    )}

                    {metrics.currentTool && (
                      <div style={{ marginBottom: 10 }}>
                        <div style={{ color: 'rgba(255,255,255,0.4)', fontSize: 9, fontWeight: 700, textTransform: 'uppercase', letterSpacing: 0.5, marginBottom: 3 }}>最近工具</div>
                        <div style={{ color: '#3498db', fontSize: 11, fontFamily: 'monospace', background: 'rgba(52,152,219,0.1)', padding: '6px 8px', borderRadius: 6 }}>
                          {metrics.currentTool}
                        </div>
                      </div>
                    )}

                    <div style={{ marginBottom: 10 }}>
                      <div style={{ color: 'rgba(255,255,255,0.4)', fontSize: 9, fontWeight: 700, textTransform: 'uppercase', letterSpacing: 0.5, marginBottom: 3 }}>模型 & Token</div>
                      <div style={{ background: 'rgba(255,255,255,0.04)', padding: '8px', borderRadius: 6 }}>
                        <div style={{ color: '#ddd', fontSize: 11, marginBottom: 6 }}>
                          {metrics.currentModel || '未知'}
                          {metrics.thinkingLevel && <span style={{ color: 'rgba(255,255,255,0.35)', marginLeft: 6 }}>({metrics.thinkingLevel})</span>}
                        </div>
                        <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '3px 10px', fontSize: 10 }}>
                          {([['输入', metrics.inputTokens], ['输出', metrics.outputTokens], ['缓存读', metrics.cacheReadTokens], ['缓存写', metrics.cacheWriteTokens]] as [string, number][]).map(([label, val]) => (
                            <div key={label} style={{ display: 'flex', justifyContent: 'space-between' }}>
                              <span style={{ color: 'rgba(255,255,255,0.45)' }}>{label}</span>
                              <span style={{ color: '#ddd' }}>{formatTokens(val)}</span>
                            </div>
                          ))}
                        </div>
                        <div style={{ marginTop: 6, paddingTop: 6, borderTop: '1px solid rgba(255,255,255,0.06)' }}>
                          <span style={{ color: '#f1c40f', fontSize: 12, fontWeight: 600 }}>{formatTokens(metrics.totalTokens)} tokens</span>
                          {metrics.totalCost > 0 && <span style={{ color: 'rgba(255,255,255,0.4)', fontSize: 10, marginLeft: 6 }}>${metrics.totalCost.toFixed(4)}</span>}
                          {metrics.sessionStart && <span style={{ color: 'rgba(255,255,255,0.3)', fontSize: 9, marginLeft: 6 }}>{formatDuration(metrics.sessionStart)}</span>}
                        </div>
                      </div>
                    </div>

                    <div style={{ marginBottom: 10 }}>
                      <div style={{ color: 'rgba(255,255,255,0.4)', fontSize: 9, fontWeight: 700, textTransform: 'uppercase', letterSpacing: 0.5, marginBottom: 3 }}>Session</div>
                      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 4 }}>
                        {([
                          { label: '活跃', value: String(metrics.activeSessionCount) },
                          { label: '消息', value: String(metrics.messageCount) },
                          { label: '错误', value: String(metrics.errorCount), color: metrics.errorCount > 0 ? '#e74c3c' : undefined },
                        ] as { label: string; value: string; color?: string }[]).map((item) => (
                          <div key={item.label} style={{ background: 'rgba(255,255,255,0.04)', padding: '5px 8px', borderRadius: 6 }}>
                            <div style={{ color: 'rgba(255,255,255,0.4)', fontSize: 9 }}>{item.label}</div>
                            <div style={{ color: item.color || '#ddd', fontSize: 13, fontWeight: 600 }}>{item.value}</div>
                          </div>
                        ))}
                        {metrics.sessionStart && (
                          <div style={{ background: 'rgba(255,255,255,0.04)', padding: '5px 8px', borderRadius: 6 }}>
                            <div style={{ color: 'rgba(255,255,255,0.4)', fontSize: 9 }}>开始</div>
                            <div style={{ color: '#ddd', fontSize: 11, fontWeight: 600 }}>{formatTime(metrics.sessionStart)}</div>
                          </div>
                        )}
                      </div>
                    </div>

                    {metrics.toolCalls && metrics.toolCalls.length > 0 && (
                      <div style={{ marginBottom: 10 }}>
                        <div style={{ color: 'rgba(255,255,255,0.4)', fontSize: 9, fontWeight: 700, textTransform: 'uppercase', letterSpacing: 0.5, marginBottom: 3 }}>工具分布</div>
                        {metrics.toolCalls.slice(0, 6).map((tc) => (
                          <div key={tc.name} style={{ display: 'flex', alignItems: 'center', gap: 4, marginBottom: 2 }}>
                            <div style={{ flex: 1, background: 'rgba(52,152,219,0.15)', borderRadius: 3, overflow: 'hidden', height: 14, position: 'relative' }}>
                              <div style={{ height: '100%', width: `${Math.max(8, (tc.count / metrics.toolCalls[0].count) * 100)}%`, background: 'rgba(52,152,219,0.4)', borderRadius: 3 }} />
                              <span style={{ position: 'absolute', left: 4, top: '50%', transform: 'translateY(-50%)', color: 'rgba(255,255,255,0.8)', fontSize: 8, fontFamily: 'monospace', whiteSpace: 'nowrap' }}>{tc.name}</span>
                            </div>
                            <span style={{ color: 'rgba(255,255,255,0.5)', fontSize: 9, minWidth: 16, textAlign: 'right' }}>{tc.count}</span>
                          </div>
                        ))}
                      </div>
                    )}

                    {metrics.recentActions && metrics.recentActions.length > 0 && (
                      <div style={{ marginBottom: 10 }}>
                        <div style={{ color: 'rgba(255,255,255,0.4)', fontSize: 9, fontWeight: 700, textTransform: 'uppercase', letterSpacing: 0.5, marginBottom: 3 }}>最近动态</div>
                        {metrics.recentActions.slice(0, 5).map((action, i) => (
                          <div key={i} style={{
                            display: 'flex', alignItems: 'flex-start', gap: 6, padding: '4px 0',
                            borderBottom: i < (metrics.recentActions?.length ?? 0) - 1 ? '1px solid rgba(255,255,255,0.03)' : 'none',
                          }}>
                            <span style={{
                              fontSize: 8, padding: '1px 4px', borderRadius: 3, flexShrink: 0, marginTop: 1,
                              background: action.type === 'tool' ? 'rgba(52,152,219,0.2)' : 'rgba(46,204,113,0.2)',
                              color: action.type === 'tool' ? '#5dade2' : '#58d68d',
                            }}>{action.type === 'tool' ? '工具' : '输出'}</span>
                            <div style={{ flex: 1, minWidth: 0 }}>
                              <div style={{ color: 'rgba(255,255,255,0.7)', fontSize: 10, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{action.summary}</div>
                            </div>
                            {action.timestamp && <span style={{ color: 'rgba(255,255,255,0.2)', fontSize: 8, flexShrink: 0 }}>{formatTime(action.timestamp)}</span>}
                          </div>
                        ))}
                      </div>
                    )}

                    {extraInfo?.skills?.length > 0 && (
                      <div style={{ marginBottom: 10 }}>
                        <div style={{ color: 'rgba(255,255,255,0.4)', fontSize: 9, fontWeight: 700, textTransform: 'uppercase', letterSpacing: 0.5, marginBottom: 3 }}>Skills</div>
                        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 4 }}>
                          {extraInfo.skills.map((s: string) => (
                            <span key={s} style={{ fontSize: 9, padding: '2px 6px', borderRadius: 4, background: 'rgba(155,89,182,0.15)', color: '#bb8fce' }}>{s}</span>
                          ))}
                        </div>
                      </div>
                    )}

                    {extraInfo?.daily_counts?.length > 0 && (
                      <MiniDailyChart extraInfo={extraInfo} />
                    )}
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  )
}
