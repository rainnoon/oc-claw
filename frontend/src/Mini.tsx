import { useState, useEffect, useCallback, useRef } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { load } from '@tauri-apps/plugin-store'
import { listen } from '@tauri-apps/api/event'
import { ChevronDown, Check, Pen, Plus, X } from 'lucide-react'
import { AnimatePresence, motion } from 'motion/react'
import ReactMarkdown from 'react-markdown'
import { SettingsTab } from './components/SettingsTab'
import { AgentDetailView } from './components/AgentDetailView'
import { CreateCharacterModal } from './components/CreateCharacterModal'
import { ClaudeStatsView } from './components/ClaudeStatsView'
import { getStore, DEFAULT_CHAR, DEFAULT_CHAR_NAME, loadCharacters, loadOcConnections } from './lib/store'
import { saveAgentCharMap } from './lib/agents'
import type { AgentMetrics, OcConnection } from './lib/types'

interface CharacterMeta {
  name: string
  builtin?: boolean
  ip?: string
  workGifs: string[]
  restGifs: string[]
  miniActions?: Record<string, string[]>
}

interface AgentInfo {
  id: string
  identityName?: string
  identityEmoji?: string
}

interface SessionHealthInfo {
  key: string
  active: boolean
}

interface AgentHealth {
  agentId: string
  active: boolean
  sessions?: SessionHealthInfo[]
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
  sessionFile?: string
}

interface SessionPreview {
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
  petState?: PetState
}

const MAX_SLOTS = 10

type PetState = 'idle' | 'working' | 'compacting' | 'waiting'

function ChatList({ messages, accentColor }: { messages: { role: string; text: string }[]; accentColor: string }) {
  const containerRef = useRef<HTMLDivElement>(null)
  const [expandedSet, setExpandedSet] = useState<Set<number>>(new Set())

  useEffect(() => {
    const el = containerRef.current
    if (el) requestAnimationFrame(() => { el.scrollTop = el.scrollHeight })
  }, [messages.length])

  const toggle = (i: number) => setExpandedSet(prev => { const s = new Set(prev); if (s.has(i)) s.delete(i); else s.add(i); return s })

  return (
    <div
      ref={containerRef}
      className="scrollbar-thin selectable-text"
      style={{ maxHeight: 524, overflowY: 'auto', padding: '12px 14px' }}
    >
      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
        {messages.map((msg, i) => (
          <div key={i}>
            {msg.role === 'user' ? (() => {
              const limit = 300
              const truncated = !expandedSet.has(i) && msg.text.length > limit
              return (
                <div style={{ display: 'flex', justifyContent: 'flex-end' }}>
                  <div style={{
                    background: accentColor, borderRadius: 18,
                    padding: '8px 14px', maxWidth: '80%',
                    color: '#fff', fontSize: 13, lineHeight: 1.5,
                    wordBreak: 'break-word', whiteSpace: 'pre-wrap',
                  }}>
                    {truncated ? msg.text.slice(0, limit) + '...' : msg.text}
                    {(truncated || (expandedSet.has(i) && msg.text.length > limit)) && (
                      <button onClick={() => toggle(i)}
                        style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 2, width: '100%', marginTop: 4, padding: '2px 0', background: 'none', border: 'none', color: 'rgba(255,255,255,0.5)', fontSize: 11, cursor: 'pointer' }}>
                        <ChevronDown style={{ width: 12, height: 12, transition: 'transform 0.2s', transform: expandedSet.has(i) ? 'rotate(180deg)' : 'none' }} />
                      </button>
                    )}
                  </div>
                </div>
              )
            })() : (() => {
              const limit = 500
              const truncated = !expandedSet.has(i) && msg.text.length > limit
              return (
                <div style={{ display: 'flex', alignItems: 'flex-start', gap: 8 }}>
                  <div style={{ width: 5, height: 5, borderRadius: '50%', background: accentColor, marginTop: 6, flexShrink: 0 }} />
                  <div className="markdown-content" style={{
                    color: '#ddd', fontSize: 13, lineHeight: 1.5,
                    wordBreak: 'break-word', maxWidth: '90%',
                  }}>
                    <ReactMarkdown>{truncated ? msg.text.slice(0, limit) + '...' : msg.text}</ReactMarkdown>
                    {(truncated || (expandedSet.has(i) && msg.text.length > limit)) && (
                      <button onClick={() => toggle(i)}
                        style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 2, width: '100%', marginTop: 2, padding: '2px 0', background: 'none', border: 'none', color: 'rgba(255,255,255,0.3)', fontSize: 11, cursor: 'pointer' }}>
                        <ChevronDown style={{ width: 12, height: 12, transition: 'transform 0.2s', transform: expandedSet.has(i) ? 'rotate(180deg)' : 'none' }} />
                      </button>
                    )}
                  </div>
                </div>
              )
            })()}
          </div>
        ))}
      </div>
    </div>
  )
}

/** Plays a GIF once, then freezes for `pause` ms, then repeats. */
function IntervalGif({ src, playMs = 1300, pauseMs = 4000, style, ...props }: React.ImgHTMLAttributes<HTMLImageElement> & { playMs?: number; pauseMs?: number }) {
  const [playing, setPlaying] = useState(true)
  const [gifSrc, setGifSrc] = useState(src)
  useEffect(() => {
    setPlaying(true)
    setGifSrc(src)
  }, [src])
  useEffect(() => {
    if (playing) {
      const t = setTimeout(() => setPlaying(false), playMs)
      return () => clearTimeout(t)
    } else {
      const t = setTimeout(() => {
        // Force GIF restart by briefly clearing src
        setGifSrc('')
        requestAnimationFrame(() => { setGifSrc(src!); setPlaying(true) })
      }, pauseMs)
      return () => clearTimeout(t)
    }
  }, [playing, src, playMs, pauseMs])
  if (!playing) return <FrozenImg src={src} style={style} {...props} />
  return <img src={gifSrc || undefined} alt="mini" style={style} draggable={false} {...props} />
}

function FrozenImg({ src, style, ...props }: React.ImgHTMLAttributes<HTMLImageElement>) {
  const canvasRef = useRef<HTMLCanvasElement>(null)
  useEffect(() => {
    if (!src) return
    const img = new Image()
    img.onload = () => {
      const c = canvasRef.current
      if (!c) return
      c.width = img.naturalWidth
      c.height = img.naturalHeight
      c.getContext('2d')?.drawImage(img, 0, 0)
    }
    img.src = src
  }, [src])
  return <canvas ref={canvasRef} style={style} {...props as any} />
}

function getMiniGif(char: CharacterMeta | undefined, petState: PetState | boolean, useTop = false): string | undefined {
  // backward compat: boolean → PetState
  const state: PetState = typeof petState === 'boolean' ? (petState ? 'working' : 'idle') : petState
  const c = (char?.miniActions && Object.values(char.miniActions).flat().length > 0) ? char : DEFAULT_CHAR
  if (!c?.miniActions) return undefined
  if (useTop && c.miniActions['top']?.length) {
    const topGifs = c.miniActions['top']
    // Priority: waiting(look) > compacting(eat) > working > idle(sleep)
    // Each state falls through to the next if no matching GIF is found
    if (state === 'waiting') {
      const look = topGifs.find((g) => g.includes('look') || g.includes('wait'))
      if (look) return look
    }
    if (state === 'compacting') {
      const eat = topGifs.find((g) => g.includes('eat') || g.includes('compact') || g.includes('power'))
      if (eat) return eat
    }
    if (state === 'working' || state === 'compacting' || state === 'waiting') {
      const work = topGifs.find((g) => g.includes('work'))
      if (work) return work
    }
    const sleep = topGifs.find((g) => g.includes('sleep') || g.includes('idle') || g.includes('rest'))
    if (sleep) return sleep
    return topGifs[0]
  }
  const allGifs = Object.values(c.miniActions).flat()
  if (allGifs.length === 0) return undefined
  if (state === 'waiting') {
    const lookGifs = allGifs.filter((g) => g.includes('look') || g.includes('wait'))
    if (lookGifs.length > 0) return lookGifs[0]
  }
  if (state === 'compacting') {
    const eatGifs = allGifs.filter((g) => g.includes('eat') || g.includes('compact') || g.includes('power'))
    if (eatGifs.length > 0) return eatGifs[0]
  }
  const idleGifs = allGifs.filter((g) => g.includes('idle'))
  const actionGifs = allGifs.filter((g) => !g.includes('idle'))
  if ((state === 'working' || state === 'compacting' || state === 'waiting') && actionGifs.length > 0) return actionGifs[0]
  return idleGifs[0] || allGifs[0]
}

function AgentAccordionItem({ agent, characters, currentChar, onSelect, isOpen, onToggle, onOpenCreate, onDeleteChar, sourceLabel }: {
  agent: AgentInfo
  characters: CharacterMeta[]
  currentChar: string
  onSelect: (charName: string) => void
  isOpen: boolean
  onToggle: () => void
  onOpenCreate?: () => void
  onDeleteChar?: (name: string) => void
  sourceLabel?: string
}) {
  const [isEditing, setIsEditing] = useState(false)
  const charsWithMini = characters.filter((c) => c.miniActions && Object.keys(c.miniActions).length > 0)
  const charMeta = characters.find((c) => c.name === currentChar)
  const gif = charMeta ? getMiniGif(charMeta, false) : undefined

  useEffect(() => {
    if (!isOpen) setIsEditing(false)
  }, [isOpen])

  return (
    <div className="flex flex-col border-b border-white/5 last:border-b-0 group">
      {/* Main Row */}
      <div
        className="relative flex items-center justify-between p-4 hover:bg-white/[0.02] transition-colors cursor-pointer"
        onClick={onToggle}
      >
        <div className="flex items-center gap-4">
          <div className="relative">
            <div className="w-12 h-12 rounded-xl bg-white/5 border border-white/10 flex items-center justify-center overflow-hidden">
              {gif ? (
                <img key={gif} src={gif} alt="" className="w-full h-full object-contain" style={{ imageRendering: 'pixelated' }} draggable={false} />
              ) : (
                <span className="text-white/40 text-xl">{agent.identityEmoji || '?'}</span>
              )}
            </div>
          </div>
          <div>
            <div className="flex items-center gap-2">
              <span className="text-base font-medium text-white/90">{agent.identityName || agent.id}</span>
              {agent.identityEmoji && <span className="text-sm">{agent.identityEmoji}</span>}
              {sourceLabel && <span className="text-[10px] text-white/30 bg-white/5 px-1.5 py-0.5 rounded">{sourceLabel}</span>}
            </div>
          </div>
        </div>
        <div className="flex items-center gap-2 text-sm text-white/50 group-hover:text-white/80 transition-colors pr-2">
          <span>{currentChar || '未分配'}</span>
          <ChevronDown className={`w-4 h-4 transition-transform duration-300 ${isOpen ? 'rotate-180' : ''}`} />
        </div>
      </div>

      {/* Expanded Selection Area */}
      <AnimatePresence>
        {isOpen && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: 'auto', opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.3, ease: [0.16, 1, 0.3, 1] }}
            className="overflow-hidden bg-[#0a0a0a]"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="p-4 border-t border-white/5">
              {/* Action Buttons row */}
              <div className="flex items-center justify-end gap-2 mb-3">
                {onOpenCreate && (
                  <button
                    onClick={() => onOpenCreate()}
                    className="flex items-center justify-center w-7 h-7 rounded-md transition-colors bg-white/5 text-white/60 hover:text-white hover:bg-white/10 border border-transparent"
                    title="创建角色"
                  >
                    <Plus className="w-4 h-4" />
                  </button>
                )}
                <button
                  onClick={() => setIsEditing(!isEditing)}
                  className={`flex items-center justify-center w-7 h-7 rounded-md transition-colors ${
                    isEditing
                      ? 'bg-red-500/20 text-red-400 border border-red-500/20'
                      : 'bg-white/5 text-white/60 hover:text-white hover:bg-white/10 border border-transparent'
                  }`}
                  title={isEditing ? '完成' : '编辑'}
                >
                  {isEditing ? <Check className="w-4 h-4" /> : <Pen className="w-3.5 h-3.5" />}
                </button>
              </div>

              {/* Character Grid grouped by IP */}
              <div className="max-h-[260px] overflow-y-auto pr-2 pt-2 scrollbar-thin">
                {(() => {
                  const groups: { ip: string; chars: typeof charsWithMini }[] = []
                  const ipOrder: string[] = []
                  for (const c of charsWithMini) {
                    const ip = c.ip || '自定义'
                    if (!ipOrder.includes(ip)) ipOrder.push(ip)
                  }
                  // 自定义 always first, 其他 always last
                  const customIdx = ipOrder.indexOf('自定义')
                  if (customIdx > 0) { ipOrder.splice(customIdx, 1); ipOrder.unshift('自定义') }
                  const otherIdx = ipOrder.indexOf('其他')
                  if (otherIdx >= 0 && otherIdx < ipOrder.length - 1) { ipOrder.splice(otherIdx, 1); ipOrder.push('其他') }
                  for (const ip of ipOrder) {
                    groups.push({ ip, chars: charsWithMini.filter((c) => (c.ip || '自定义') === ip) })
                  }
                  return groups.map(({ ip, chars }) => (
                    <div key={ip} className="mb-3 last:mb-0">
                      <div className="text-[10px] font-medium text-white/25 uppercase tracking-wider mb-2 px-1">{ip}</div>
                      <div className="grid grid-cols-3 gap-3">
                        {chars.map((c) => {
                          const isSelected = c.name === currentChar
                          const preview = getMiniGif(c, false)
                          const isDefault = !!c.builtin
                          return (
                            <div
                              key={c.name}
                              onClick={() => {
                                if (isEditing && !isDefault) {
                                  onDeleteChar?.(c.name)
                                } else if (!isEditing) {
                                  onSelect(c.name)
                                }
                              }}
                              className={`relative flex items-center gap-3 p-2.5 rounded-xl border transition-all ${
                                isEditing && !isDefault
                                  ? 'cursor-pointer hover:bg-red-500/10 border-red-500/30'
                                  : isEditing && isDefault
                                  ? 'opacity-40 cursor-not-allowed border-transparent'
                                  : isSelected
                                  ? 'bg-white/10 border-white/20 cursor-default'
                                  : 'bg-white/5 border-transparent hover:bg-white/10 cursor-pointer'
                              }`}
                            >
                              <div className="relative w-9 h-9 shrink-0 rounded-lg overflow-hidden bg-black/50 border border-white/10">
                                {preview ? (
                                  <img
                                    src={preview}
                                    alt={c.name}
                                    className={`w-full h-full object-contain transition-opacity ${isEditing && !isDefault ? 'opacity-50' : 'opacity-90'}`}
                                    style={{ imageRendering: 'pixelated' }}
                                    draggable={false}
                                  />
                                ) : (
                                  <div className="w-full h-full flex items-center justify-center text-white/30 text-xs">?</div>
                                )}
                              </div>
                              <span className="text-sm text-white/80 truncate flex-1">{c.name}</span>
                              {isEditing && !isDefault && (
                                <div className="absolute -top-1.5 -right-1.5 w-5 h-5 bg-red-500 rounded-full flex items-center justify-center shadow-md z-10 hover:bg-red-600 transition-colors">
                                  <X className="w-3 h-3 text-white" />
                                </div>
                              )}
                              {!isEditing && isSelected && (
                                <div className="absolute top-1/2 -translate-y-1/2 right-3">
                                  <div className="w-2 h-2 rounded-full bg-emerald-500 shadow-[0_0_8px_rgba(16,185,129,0.6)]" />
                                </div>
                              )}
                            </div>
                          )
                        })}
                      </div>
                    </div>
                  ))
                })()}
              </div>
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  )
}

type OcParams = { mode?: string; url?: string; token?: string; sshHost?: string; sshUser?: string }

function connToOcParams(conn: OcConnection): OcParams {
  if (conn.type === 'remote' && conn.host && conn.user) {
    return { mode: 'remote', sshHost: conn.host, sshUser: conn.user }
  }
  return {}
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
  const [claudeCharName, setClaudeCharName] = useState(DEFAULT_CHAR_NAME)
  const [selectedClaudeSession, setSelectedClaudeSession] = useState<string | null>(null)
  const [claudeConversation, setClaudeConversation] = useState<any[]>([])
  const [showClaudeStats, setShowClaudeStats] = useState(false)

  // OC multi-connection: qualifiedId → connection params, qualifiedId → real agent ID, qualifiedId → source label
  const agentConnMapRef = useRef<Map<string, OcParams>>(new Map())
  const agentRealIdMapRef = useRef<Map<string, string>>(new Map())
  const [agentSourceLabels, setAgentSourceLabels] = useState<Record<string, string>>({})

  // Feature toggles
  const [enableClaudeCode, setEnableClaudeCode] = useState(true)
  const [soundEnabled, setSoundEnabled] = useState(true)
  const [notifySound, setNotifySound] = useState<'default' | 'manbo'>('default')
  const [waitingSound, setWaitingSound] = useState(false)
  const [disableSleepAnim, setDisableSleepAnim] = useState(true)
  const [mascotPosition, setMascotPosition] = useState<'left' | 'right'>('right')
  const mascotPositionRef = useRef<'left' | 'right'>('right')

  // Settings mode: panel becomes wider, shows settings content
  const [settingsMode, setSettingsMode] = useState(false)
  const settingsModeRef = useRef(false)
  const filePickerOpenRef = useRef(false)
  const [settingsNav, setSettingsNav] = useState<'pairing' | 'settings'>('pairing')
  const [openAccordionId, setOpenAccordionId] = useState<string | null>(null)
  const [isCreateModalOpen, _setIsCreateModalOpen] = useState(false)
  const isCreateModalOpenRef = useRef(false)
  const setIsCreateModalOpen = (v: boolean) => { isCreateModalOpenRef.current = v; _setIsCreateModalOpen(v) }
  const [hiding, setHiding] = useState(false)
  const [pinned, setPinned] = useState(false)
  const collapsingRef = useRef(false)
  const customPosRef = useRef<{ x: number; y: number } | null>(null)
  const [moveMode, setMoveMode] = useState(false)

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

  const bobYRaw = Math.sin(bobPhase / 600) * 3

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
      const chars = await loadCharacters()
      setCharacters(chars)
    } catch (e) { console.warn('[fetchAgents] loadCharacters failed:', e) }
    try {
      const store = await load('settings.json', { defaults: {}, autoSave: true })
      const connections = await loadOcConnections()
      const newConnMap = new Map<string, OcParams>()
      const newRealIdMap = new Map<string, string>()
      const newSourceLabels: Record<string, string> = {}
      const allAgents: AgentInfo[] = []
      const multi = connections.length > 1
      await Promise.all(connections.map(async (conn) => {
        try {
          const oc = connToOcParams(conn)
          const agents = (await invoke('get_agents', oc)) as AgentInfo[]
          const prefix = multi ? `${conn.id.slice(0, 8)}:` : ''
          const label = conn.type === 'local' ? '本地' : (conn.host || '远程')
          for (const a of agents) {
            const qualifiedId = prefix + a.id
            newConnMap.set(qualifiedId, oc)
            newRealIdMap.set(qualifiedId, a.id)
            if (multi) newSourceLabels[qualifiedId] = label
            allAgents.push({ ...a, id: qualifiedId })
          }
        } catch (e) { console.warn('[fetchAgents] connection failed:', conn.id, e) }
      }))
      agentConnMapRef.current = newConnMap
      agentRealIdMapRef.current = newRealIdMap
      setAgentSourceLabels(newSourceLabels)
      const charMap = (await store.get('agent_char_map')) as Record<string, string> | null
      setAgents(allAgents)
      setAgentCharMap(charMap || {})
    } catch (e) { console.warn('[fetchAgents] get_agents failed:', e) }
  }, [])

  const lastOcSoundRef = useRef(0)
  const playOcCompletionSound = useCallback((source: string) => {
    console.log('[OC-SOUND] triggered from', source, 'soundEnabled:', soundEnabledRef.current)
    if (!soundEnabledRef.current) return
    const now = Date.now()
    if (now - lastOcSoundRef.current < 5000) {
      console.log('[OC-SOUND] deduped, last played', now - lastOcSoundRef.current, 'ms ago')
      return
    }
    lastOcSoundRef.current = now
    console.log('[OC-SOUND] PLAYING sound:', notifySoundRef.current)
    if (notifySoundRef.current === 'manbo') {
      new Audio('/audio/manbo.m4a').play().catch(() => {})
    } else {
      invoke('play_sound', { name: 'Purr' }).catch(() => {})
    }
  }, [])

  const prevHealthRef = useRef<Record<string, boolean>>({})
  const prevSessionHealthRef = useRef<Record<string, boolean>>({})
  const pollHealth = useCallback(async () => {
    try {
      const connections = await loadOcConnections()
      // Start with previous data — only overwrite for connections that succeed
      const hMap: Record<string, boolean> = { ...prevHealthRef.current }
      const sMap: Record<string, boolean> = { ...prevSessionHealthRef.current }
      const freshKeys = new Set<string>() // session keys that got fresh data this round
      await Promise.all(connections.map(async (conn) => {
        const prefix = connections.length > 1 ? `${conn.id.slice(0, 8)}:` : ''
        try {
          const oc = connToOcParams(conn)
          const health = (await invoke('get_health', oc)) as { agents: AgentHealth[] }
          // Clear old entries for this connection, then fill fresh data
          for (const k of Object.keys(hMap)) {
            if (prefix === '' || k.startsWith(prefix)) delete hMap[k]
          }
          for (const k of Object.keys(sMap)) {
            if (prefix === '' || k.startsWith(prefix)) delete sMap[k]
          }
          health.agents.forEach((a) => {
            hMap[prefix + a.agentId] = a.active
            if (a.sessions) {
              a.sessions.forEach((s) => {
                const sk = `${prefix}${a.agentId}:${s.key}`
                sMap[sk] = s.active
                freshKeys.add(sk)
              })
            }
          })
        } catch { /* SSH failed — previous data preserved */ }
      }))

      // Detect session active→inactive transitions (only for fresh data)
      // Skip sub-agent sessions — their key contains ":subagent:" (from OpenClaw session key format)
      const prev = prevSessionHealthRef.current
      if (freshKeys.size > 0) {
        const anyBecameInactive = Array.from(freshKeys).some(k =>
          prev[k] === true && sMap[k] === false && !k.includes(':subagent:')
        )
        if (anyBecameInactive) {
          console.log('[pollHealth] session became inactive, prev:', prev, 'curr:', sMap)
          playOcCompletionSound('pollHealth')
        }
      }
      prevSessionHealthRef.current = sMap

      const anyActive = Object.values(sMap).some(v => v)
      setAnySessionActive(anyActive)

      prevHealthRef.current = hMap
      setHealthMap(hMap)
    } catch { /* ignore */ }
  }, [playOcCompletionSound])

  const previewCacheRef = useRef<Map<string, { active: boolean; lastUserMsg?: string; lastAssistantMsg?: string; fetchedAt: number }>>(new Map())
  const previewQueueRef = useRef<string[]>([])
  const previewTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const sessionFileMapRef = useRef<Map<string, string>>(new Map())
  const sessionAgentMapRef = useRef<Map<string, string>>(new Map()) // sessionCompositeKey → qualifiedAgentId
  const fetchingSessionsRef = useRef(false)

  const fetchAllSessions = useCallback(async () => {
    if (agents.length === 0) { setAllSessions([]); return }
    if (fetchingSessionsRef.current) return
    fetchingSessionsRef.current = true
    const results: MiniSessionInfo[] = []
    await Promise.all(
      agents.map(async (agent) => {
        try {
          const oc = agentConnMapRef.current.get(agent.id) || {}
          const realId = agentRealIdMapRef.current.get(agent.id) || agent.id
          const s = (await invoke('get_agent_sessions', { agentId: realId, ...oc })) as MiniSessionInfo[]
          // Tag sessions with the qualified agent ID
          results.push(...s.map(ss => ({ ...ss, agentId: agent.id })))
        } catch { /* ignore */ }
      })
    )
    // Keep previous data if all fetches failed (SSH backoff etc.)
    if (results.length === 0 && previewCacheRef.current.size > 0) { fetchingSessionsRef.current = false; return }
    const seen = new Set<string>()
    const deduped = results.filter(s => {
      const k = `${s.agentId}:${s.key}`
      if (seen.has(k)) return false
      seen.add(k)
      return true
    })
    const filtered = deduped.filter(s => {
      const key = `${s.agentId}:${s.key}`
      const dismissedAt = dismissedSessionsRef.current.get(key)
      if (dismissedAt !== undefined && s.updatedAt > dismissedAt) {
        dismissedSessionsRef.current.delete(key)
      }
      return !dismissedSessionsRef.current.has(key)
    })
    filtered.sort((a, b) => b.updatedAt - a.updatedAt)
    const top = filtered.slice(0, MAX_SLOTS)

    // Merge cached preview data into sessions
    const merged = top.map(s => {
      const k = `${s.agentId}:${s.key}`
      const cached = previewCacheRef.current.get(k)
      if (cached) {
        return { ...s, active: cached.active, lastUserMsg: cached.lastUserMsg, lastAssistantMsg: cached.lastAssistantMsg }
      }
      return s
    })
    // Filter out OpenClaw sub-agent sessions (key contains ":subagent:")
    .filter(s => !s.key.includes(':subagent:'))
    merged.sort((a, b) => (b.active ? 1 : 0) - (a.active ? 1 : 0) || b.updatedAt - a.updatedAt)
    setAllSessions(merged)

    // Build session file lookup and queue preview fetches
    const queue: string[] = []
    for (const s of top) {
      const k = `${s.agentId}:${s.key}`
      if (s.sessionFile) sessionFileMapRef.current.set(k, s.sessionFile)
      sessionAgentMapRef.current.set(k, s.agentId)
      const cached = previewCacheRef.current.get(k)
      const staleTime = cached?.active ? 8000 : 15000 // poll active sessions faster
      const stale = !cached || (Date.now() - cached.fetchedAt > staleTime)
      if (stale) queue.push(k)
    }
    // Prioritize active sessions first
    queue.sort((a, b) => {
      const ca = previewCacheRef.current.get(a)
      const cb = previewCacheRef.current.get(b)
      return (cb?.active ? 1 : 0) - (ca?.active ? 1 : 0)
    })
    previewQueueRef.current = queue
    fetchingSessionsRef.current = false
  }, [agents])

  useEffect(() => {
    fetchAgents()
    pollHealth()
    const a = setInterval(fetchAgents, 5000)
    const h = setInterval(pollHealth, 1000)
    return () => { clearInterval(a); clearInterval(h) }
  }, [fetchAgents, pollHealth])

  // Update allSessions active states from pollHealth session data
  const syncSessionActiveStates = useCallback(() => {
    const sMap = prevSessionHealthRef.current
    if (Object.keys(sMap).length === 0) return
    setAllSessions(prev => {
      let changed = false
      const updated = prev.map(s => {
        const key = `${s.agentId}:${s.key}`
        const isActive = !!sMap[key]
        if (s.active !== isActive) { changed = true; return { ...s, active: isActive } }
        return s
      })
      if (!changed) return prev
      updated.sort((a, b) => (b.active ? 1 : 0) - (a.active ? 1 : 0) || b.updatedAt - a.updatedAt)
      return updated
    })
  }, [])

  useEffect(() => {
    syncSessionActiveStates()
    const t = setInterval(syncSessionActiveStates, 2000)
    return () => clearInterval(t)
  }, [syncSessionActiveStates])

  const drainPreviewQueue = useCallback(async () => {
    if (previewTimerRef.current) { clearTimeout(previewTimerRef.current); previewTimerRef.current = null }
    const queue = [...previewQueueRef.current]
    if (queue.length === 0) return
    const processNext = (idx: number) => {
      if (idx >= queue.length) return
      const k = queue[idx]
      const sessionFile = sessionFileMapRef.current.get(k)
      if (!sessionFile) {
        console.warn('[drainPreview] no sessionFile for', k, 'map size:', sessionFileMapRef.current.size)
        previewTimerRef.current = setTimeout(() => processNext(idx + 1), 200)
        return
      }
      const agentId = sessionAgentMapRef.current.get(k) || k.split(':')[0]
      const oc = agentConnMapRef.current.get(agentId) || {}
      invoke('get_session_preview', { sessionFile, ...oc })
        .then((preview) => {
          const p = preview as SessionPreview
          previewCacheRef.current.set(k, { ...p, fetchedAt: Date.now() })
          setAllSessions(prev => prev.map(s => {
            if (`${s.agentId}:${s.key}` === k) {
              return { ...s, active: p.active, lastUserMsg: p.lastUserMsg, lastAssistantMsg: p.lastAssistantMsg }
            }
            return s
          }))
        })
        .catch(() => { /* ignore */ })
        .finally(() => {
          if (idx + 1 < queue.length) {
            previewTimerRef.current = setTimeout(() => processNext(idx + 1), 1500)
          }
        })
    }
    processNext(0)
  }, [playOcCompletionSound])

  useEffect(() => {
    if (!expanded) return
    fetchAllSessions().then(() => drainPreviewQueue())
    const t1 = setInterval(() => { fetchAllSessions().then(() => drainPreviewQueue()) }, 5000)
    return () => {
      clearInterval(t1)
      if (previewTimerRef.current) { clearTimeout(previewTimerRef.current); previewTimerRef.current = null }
    }
  }, [expanded, fetchAllSessions, drainPreviewQueue])

  // Load feature toggles
  useEffect(() => {
    (async () => {
      const store = await load('settings.json', { defaults: {}, autoSave: true })
      const cc = await store.get('enable_claudecode')
      if (typeof cc === 'boolean') setEnableClaudeCode(cc)
      if (cc !== false) invoke('install_claude_hooks').catch(() => {})
      const snd = await store.get('sound_enabled')
      if (typeof snd === 'boolean') setSoundEnabled(snd)
      const ns = await store.get('notify_sound') as string
      if (ns === 'default' || ns === 'manbo') setNotifySound(ns)
      const ws = await store.get('waiting_sound')
      if (typeof ws === 'boolean') setWaitingSound(ws)
      const dsa = await store.get('disable_sleep_anim')
      if (typeof dsa === 'boolean') setDisableSleepAnim(dsa)
      const mp = await store.get('mascot_position') as string
      if (mp === 'left' || mp === 'right') { setMascotPosition(mp); mascotPositionRef.current = mp }
      const ccChar = ((await store.get('claude_char')) as string) || DEFAULT_CHAR_NAME
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
  const notifySoundRef = useRef(notifySound)
  notifySoundRef.current = notifySound
  const waitingSoundRef = useRef(waitingSound)
  waitingSoundRef.current = waitingSound
  useEffect(() => {
    if (!enableClaudeCode) return
    const unlisten = listen('claude-task-complete', (ev: any) => {
      if (!soundEnabledRef.current) return
      if (ev.payload?.waiting && !waitingSoundRef.current) return
      if (notifySoundRef.current === 'manbo') {
        new Audio('/audio/manbo.m4a').play().catch(() => {})
      } else {
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
        const oc = agentConnMapRef.current.get(selectedSessionKey.agentId) || {}
        const realId = agentRealIdMapRef.current.get(selectedSessionKey.agentId) || selectedSessionKey.agentId
        const msgs = await invoke('get_session_messages', { agentId: realId, sessionKey: selectedSessionKey.key, ...oc }) as any[]
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
    const realId = agentRealIdMapRef.current.get(selectedAgentId) || selectedAgentId
    const fetchMetrics = async () => {
      try {
        const oc = agentConnMapRef.current.get(selectedAgentId) || {}
        const m = (await invoke('get_agent_metrics', { agentId: realId, ...oc })) as AgentMetrics
        if (!cancelled) setMetrics(m)
      } catch { if (!cancelled) setMetrics(null) }
    }
    const fetchExtra = async () => {
      const oc = agentConnMapRef.current.get(selectedAgentId) || {}
      try {
        const e = (await invoke('get_agent_extra_info', { agentId: realId, ...oc })) as any
        if (!cancelled) setExtraInfo(e)
      } catch { if (!cancelled) setExtraInfo(null) }
    }
    fetchMetrics()
    fetchExtra()
    const i1 = setInterval(fetchMetrics, 2000)
    const i2 = setInterval(fetchExtra, 10000)
    return () => { cancelled = true; clearInterval(i1); clearInterval(i2) }
  }, [selectedAgentId])


  // Build character slots (OpenClaw + Claude Code)
  const ocSlots: SessionSlot[] = allSessions.slice(0, MAX_SLOTS).map((s, i) => {
    const agent = agents.find(a => a.id === s.agentId) || { id: s.agentId }
    const charName = agentCharMap[s.agentId]
    const char = characters.find((c) => c.name === charName) || DEFAULT_CHAR
    return { agentId: s.agentId, sessionIdx: i, agent, char, isWorking: s.active }
  })
  const claudeSlots: SessionSlot[] = claudeSessions.map((cs, i) => {
    const isWaiting = cs.status === 'waiting'
    const isCompacting = cs.status === 'compacting'
    const isActive = cs.status === 'processing' || cs.status === 'tool_running'
    const char = characters.find((c) => c.name === claudeCharName) || DEFAULT_CHAR
    const petState: PetState = isWaiting ? 'waiting' : isCompacting ? 'compacting' : isActive ? 'working' : 'idle'
    return { agentId: `claude:${cs.sessionId}`, sessionIdx: ocSlots.length + i, agent: { id: `claude:${cs.sessionId}`, identityName: 'Claude', identityEmoji: '🤖' }, char, isWorking: isActive || isCompacting || isWaiting, petState }
  })
  const sessionSlots = [...ocSlots, ...claudeSlots].slice(0, MAX_SLOTS)

  const expandingRef = useRef(false)
  const expand = useCallback(async () => {
    if (collapsingRef.current || expandingRef.current) return
    expandingRef.current = true
    setHiding(true)
    await new Promise<void>((r) => setTimeout(r, 50))
    await invoke('set_mini_expanded', { expanded: true, position: mascotPositionRef.current })
    setHiding(false)
    setExpanded(true)
    requestAnimationFrame(() => {
      requestAnimationFrame(() => setShowPanel(true))
    })
    expandingRef.current = false
  }, [])

  const handleMascotPointerDown = useCallback((e: React.PointerEvent) => {
    if (e.button !== 0 || collapsingRef.current) return

    // Normal mode: click to expand
    if (!moveMode) {
      expand()
      return
    }

    // Move mode: drag to reposition, click to exit
    e.preventDefault()
    const el = e.currentTarget as HTMLElement
    el.setPointerCapture(e.pointerId)

    let lastX = e.screenX
    let lastY = e.screenY
    let dragging = false
    const pid = e.pointerId

    const onMove = (ev: PointerEvent) => {
      if (!dragging) {
        if (Math.abs(ev.screenX - lastX) + Math.abs(ev.screenY - lastY) > 3) {
          dragging = true
        } else return
      }
      const dx = ev.screenX - lastX
      const dy = ev.screenY - lastY
      lastX = ev.screenX
      lastY = ev.screenY
      if (dx !== 0 || dy !== 0) invoke('move_mini_by', { dx, dy })
    }

    const cleanup = () => {
      try { el.releasePointerCapture(pid) } catch {}
      el.removeEventListener('pointermove', onMove)
      el.removeEventListener('pointerup', onUp)
      el.removeEventListener('lostpointercapture', cleanup)
    }

    const onUp = () => {
      cleanup()
      if (!dragging) {
        // Click without moving: exit move mode
        setMoveMode(false)
        // Force browser to re-evaluate cursor immediately
        document.body.style.cursor = 'pointer'
        requestAnimationFrame(() => { document.body.style.cursor = '' })
      } else {
        // Save dragged position (macOS native coordinates)
        invoke('get_mini_origin').then((pos) => {
          const [x, y] = pos as [number, number]
          customPosRef.current = { x, y }
        })
      }
    }

    el.addEventListener('pointermove', onMove)
    el.addEventListener('pointerup', onUp)
    el.addEventListener('lostpointercapture', cleanup)
  }, [expand, moveMode])

  const enterMoveMode = useCallback(async () => {
    setShowPanel(false)
    setTimeout(async () => {
      setHiding(true)
      setExpanded(false)
      try {
        await new Promise<void>((r) => setTimeout(r, 50))
        await invoke('set_mini_expanded', { expanded: false, position: mascotPositionRef.current })
        if (customPosRef.current) {
          await invoke('set_mini_origin', customPosRef.current)
        }
        await new Promise<void>((r) => setTimeout(r, 50))
      } catch {}
      setHiding(false)
      setMoveMode(true)
    }, 350)
  }, [])

  const collapse = useCallback(async () => {
    if (collapsingRef.current) return
    collapsingRef.current = true
    setIsCreateModalOpen(false)
    setShowPanel(false)
    setSelectedAgentId(null)
    setSelectedClaudeSession(null)
    setSelectedSessionKey(null)
    const wasSettings = settingsModeRef.current
    const delay = wasSettings ? 280 : 350
    setTimeout(async () => {
      settingsModeRef.current = false
      setSettingsMode(false)
      // Hide mascot first to avoid flicker at old position
      setHiding(true)
      setExpanded(false)
      // Use setTimeout instead of rAF (rAF may not fire when window is blurred)
      try {
        await new Promise<void>((r) => setTimeout(r, 50))
        if (wasSettings) {
          await invoke('set_mini_size', { restore: true, position: mascotPositionRef.current })
        } else {
          await invoke('set_mini_expanded', { expanded: false, position: mascotPositionRef.current })
        }
        await new Promise<void>((r) => setTimeout(r, 50))
        // Restore custom drag position if set
        if (customPosRef.current) {
          await invoke('set_mini_origin', customPosRef.current)
        }
      } catch { /* ensure hiding is always cleared */ }
      setHiding(false)
      // Brief cooldown to prevent focus event from immediately re-expanding
      setTimeout(() => { collapsingRef.current = false }, 300)
    }, delay)
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
      try { await invoke('set_mini_size', { restore: false, position: mascotPositionRef.current }) } catch {}
      requestAnimationFrame(() => {
        requestAnimationFrame(() => setShowPanel(true))
      })
    }, 300)
  }, [])

  const exitSettings = useCallback(async () => {
    // 1. Collapse panel
    setShowPanel(false)
    // 2. After collapse, switch back to normal mode + resize + re-expand
    setTimeout(async () => {
      settingsModeRef.current = false
      setSettingsMode(false)
      setSettingsNav('pairing')
      try { await invoke('set_mini_expanded', { expanded: true, position: mascotPositionRef.current }) } catch {}
      requestAnimationFrame(() => {
        requestAnimationFrame(() => setShowPanel(true))
      })
    }, 300)
  }, [])

  // Click outside to collapse (only when not pinned)
  useEffect(() => {
    if (!expanded || pinned) return
    const onClick = (e: MouseEvent) => {
      if (isCreateModalOpenRef.current) return
      if (!(e.target as HTMLElement).closest('#mini-panel')) collapse()
    }
    window.addEventListener('mousedown', onClick)
    return () => window.removeEventListener('mousedown', onClick)
  }, [expanded, pinned, collapse])

  // Window blur: collapse when user clicks outside the app (when not pinned, or in settings mode)
  // Skip blur when a file picker dialog is open
  useEffect(() => {
    if (!expanded) return
    if (pinned && !settingsMode) return
    const onClickCapture = (e: MouseEvent) => {
      const el = e.target as HTMLElement
      if (el instanceof HTMLInputElement && el.type === 'file') {
        filePickerOpenRef.current = true
      }
      if (el.closest('a[target="_blank"]')) {
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
  }, [expanded, pinned, settingsMode, collapse])



  useEffect(() => {
    if (expanded || moveMode) return
    const onFocus = () => {
      if (collapsingRef.current) return
      expand()
    }
    window.addEventListener('focus', onFocus)
    return () => window.removeEventListener('focus', onFocus)
  }, [expanded, expand, moveMode])

  // Exit move mode when clicking outside mascot or when window loses focus
  useEffect(() => {
    if (!moveMode) return
    const onBlur = () => setMoveMode(false)
    window.addEventListener('blur', onBlur)
    return () => window.removeEventListener('blur', onBlur)
  }, [moveMode])

  const claudeWaiting = claudeSessions.some(cs => cs.status === 'waiting')
  const claudeCompacting = claudeSessions.some(cs => cs.status === 'compacting')
  const claudeWorking = claudeSessions.some(cs => cs.status === 'processing' || cs.status === 'tool_running')
  const hasWorking = anySessionActive || Object.values(healthMap).some(Boolean) || claudeWorking || claudeCompacting || claudeWaiting
  // Priority: waiting > compacting > working > idle
  const mainPetState: PetState = claudeWaiting ? 'waiting' : claudeCompacting ? 'compacting' : hasWorking ? 'working' : 'idle'
  const bobY = (disableSleepAnim && mainPetState === 'idle') ? 0 : bobYRaw
  const miniGif = getMiniGif(miniChar ?? undefined, mainPetState, true)
  const handleDeleteChar = useCallback(async (name: string) => {
    try {
      await invoke('delete_character_assets', { name })
      const chars = await loadCharacters()
      setCharacters(chars)
    } catch (e) { console.warn('delete char failed:', e) }
  }, [])
  const inAgentDetail = selectedAgentId !== null
  const selectedAgent = agents.find(a => a.id === selectedAgentId)

  // Panel dimensions depend on settingsMode
  const panelW = settingsMode ? '100vw' : 380
  const panelH = settingsMode ? '100vh' : 560
  const panelRef = useRef<HTMLDivElement>(null)

  // Dynamically resize Tauri window to match panel height (max 400)
  useEffect(() => {
    if (!expanded || settingsMode) return
    const el = panelRef.current
    if (!el) return
    const ro = new ResizeObserver((entries) => {
      const h = entries[0]?.contentRect.height
      if (h && h > 0) invoke('resize_mini_height', { height: Math.min(h, 400) }).catch(() => {})
    })
    ro.observe(el)
    return () => ro.disconnect()
  }, [expanded, settingsMode])

  return (
    <div style={{
      width: '100vw', height: '100vh',
      background: 'transparent', overflow: 'hidden', userSelect: 'none',
    }}>
      {/* Collapsed */}
      {!expanded && !hiding && (
        <div
          id="mini-panel"
          onPointerDown={handleMascotPointerDown}
          style={{
            width: '100%', height: '100%',
            display: 'flex', alignItems: 'flex-start', justifyContent: 'center',
            cursor: moveMode ? 'grab' : 'pointer',
            pointerEvents: 'auto',
          }}
        >
          <div style={{
            transform: `translateY(${bobY}px)`,
            transition: 'transform 0.1s ease',
            position: 'relative',
            ...(moveMode ? {
              borderRadius: 12,
              outline: '2px solid rgba(59,130,246,0.6)',
              outlineOffset: -2,
              animation: 'movePulse 1.2s ease-in-out infinite',
            } : {}),
          }}>
            {miniGif ? (
              disableSleepAnim && mainPetState === 'idle' ? (
                <FrozenImg src={miniGif}
                  style={{ width: 43, height: 43, objectFit: 'contain' }}
                  draggable={false} />
              ) : mainPetState === 'compacting' ? (
                <IntervalGif src={miniGif}
                  style={{ width: 43, height: 43, objectFit: 'contain' }} />
              ) : (
                <img src={miniGif} alt="mini"
                  style={{ width: 43, height: 43, objectFit: 'contain' }}
                  draggable={false} />
              )
            ) : (
              <div style={{
                width: 43, height: 43, borderRadius: 10,
                background: 'rgba(0,0,0,0.3)',
                display: 'flex', alignItems: 'center', justifyContent: 'center',
                color: '#999', fontSize: 16,
              }}>?</div>
            )}
            <div style={{
              position: 'absolute', bottom: 0, right: 0,
              width: 8, height: 8, borderRadius: '50%',
              background: mainPetState === 'waiting' ? '#f59e0b' : hasWorking ? '#2ecc71' : '#777',
              border: '1.5px solid rgba(0,0,0,0.3)',
            }} />
          </div>
        </div>
      )}

      {/* Expanded panel */}
      {expanded && (
        <div id="mini-panel" ref={panelRef} style={{
          position: 'absolute', top: 0,
          left: '50%',
          transform: 'translateX(-50%)',
          width: panelW,
          height: settingsMode ? panelH : 'auto',
          maxHeight: settingsMode ? undefined : 400,
          overflowY: settingsMode ? 'hidden' : 'auto',
          overflowX: 'hidden',
          background: '#1a1a1a',
          clipPath: showPanel
            ? `inset(0 0 0 0 round 0 0 ${settingsMode ? '16px 16px' : '24px 24px'})`
            : `inset(0 calc(50% - 30px) calc(100% + 200px) calc(50% - 30px) round 0 0 8px 8px)`,
          boxShadow: showPanel
            ? '0 0 12px rgba(0,0,0,0.7)'
            : '0 2px 8px rgba(0,0,0,0.3)',
          transition: showPanel
            ? 'clip-path 0.4s cubic-bezier(0.2, 1, 0.3, 1), box-shadow 0.3s ease'
            : settingsMode
              ? 'clip-path 0.25s cubic-bezier(0.3, 0, 0, 1), box-shadow 0.15s ease'
              : 'clip-path 0.3s cubic-bezier(0.2, 0, 0, 1), box-shadow 0.2s ease',
        }}>
          {/* Grass background layer - only visible during panel collapse animation */}
          {!settingsMode && (
            <div style={{
              position: 'absolute', top: 36, left: 0, right: 0,
              height: 30,
              backgroundImage: 'url(/assets/grass-island.png)',
              backgroundSize: '80px 100%',
              backgroundRepeat: 'repeat-x',
              backgroundPosition: 'bottom',
              pointerEvents: 'none',
              zIndex: 0,
              filter: showPanel ? 'blur(0px)' : 'blur(8px)',
              opacity: showPanel ? 0 : 1,
              transition: showPanel
                ? 'filter 0.2s ease, opacity 0.15s ease'
                : 'filter 0.4s cubic-bezier(0.2, 0, 0, 1), opacity 0.15s ease 0.05s',
            }} />
          )}
          <div style={{
            position: 'relative', zIndex: 1,
            opacity: showPanel ? 1 : 0,
            transform: showPanel ? 'scale(1) translateY(0)' : 'scale(0.92) translateY(-8px)',
            transformOrigin: 'top center',
            transition: showPanel
              ? 'opacity 0.25s cubic-bezier(0.2, 1, 0.3, 1) 0.08s, transform 0.4s cubic-bezier(0.2, 1, 0.3, 1)'
              : 'opacity 0.08s ease-out, transform 0.08s ease-out',
            height: settingsMode ? '100vh' : 'auto',
            display: settingsMode ? 'flex' : 'block',
            flexDirection: 'column',
          }}>
            {/* Header */}
            <div style={{
              display: 'flex', alignItems: 'center', justifyContent: 'space-between',
              height: 36, padding: '0 14px', flexShrink: 0,
              background: '#1a1a1a',
              borderBottom: '1px solid rgba(255,255,255,0.06)',
              position: 'sticky', top: 0, zIndex: 10,
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
                    {(['pairing', 'settings'] as const).map((nav) => (
                      <button key={nav} data-no-drag
                        onClick={(e) => { e.stopPropagation(); setSettingsNav(nav) }}
                        style={{
                          background: settingsNav === nav ? 'rgba(255,255,255,0.12)' : 'none',
                          border: 'none',
                          color: settingsNav === nav ? '#fff' : 'rgba(255,255,255,0.4)',
                          fontSize: 11, cursor: 'pointer', padding: '3px 10px',
                          borderRadius: 6, fontWeight: settingsNav === nav ? 600 : 400,
                        }}>
                        {nav === 'pairing' ? '配对' : '设置'}
                      </button>
                    ))}
                  </div>
                ) : (inAgentDetail || selectedClaudeSession || selectedSessionKey || showClaudeStats) ? (
                  <button data-no-drag
                    onClick={(e) => { e.stopPropagation(); setSelectedAgentId(null); setSelectedClaudeSession(null); setSelectedSessionKey(null); setShowClaudeStats(false) }}
                    style={{
                      background: 'rgba(255,255,255,0.06)', border: 'none',
                      color: 'rgba(255,255,255,0.6)', fontSize: 11,
                      cursor: 'pointer', padding: '3px 8px',
                      borderRadius: 6, display: 'flex', alignItems: 'center', gap: 4,
                    }}>
                    <span style={{ fontSize: 13 }}>&lsaquo;</span> Back
                  </button>
                ) : (
                  <button data-no-drag
                    onClick={(e) => { e.stopPropagation(); setPinned(!pinned) }}
                    style={{
                      background: 'none', border: 'none',
                      color: pinned ? '#fff' : 'rgba(255,255,255,0.25)',
                      fontSize: 12, cursor: 'pointer', padding: '2px 6px',
                      transform: pinned ? 'rotate(0deg)' : 'rotate(45deg)',
                      transition: 'transform 0.2s, color 0.2s',
                    }}
                    title={pinned ? '取消置顶' : '置顶'}
                  >
                    <svg width="12" height="12" viewBox="0 0 24 24" fill={pinned ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M12 17v5"/>
                      <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16h14v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z"/>
                    </svg>
                  </button>
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
                      if (next) {
                        if (notifySound === 'manbo') new Audio('/audio/manbo.m4a').play().catch(() => {})
                        else invoke('play_sound', { name: 'Purr' }).catch(() => {})
                      }
                    }}
                    style={{
                      background: 'none', border: 'none',
                      color: soundEnabled ? 'rgba(255,255,255,0.35)' : 'rgba(255,255,255,0.15)', fontSize: 14,
                      cursor: 'pointer', padding: '4px 6px', paddingTop: '5px',
                      position: 'relative',
                    }}
                    title={soundEnabled ? '提示音: 开' : '提示音: 关'}
                  >
                    <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9"/>
                      <path d="M13.73 21a2 2 0 0 1-3.46 0"/>
                      {!soundEnabled && <line x1="1" y1="1" x2="23" y2="23" strokeWidth="3"/>}
                    </svg>
                  </button>
                )}
                {!settingsMode && (
                  <button data-no-drag
                    onClick={(e) => { e.stopPropagation(); enterMoveMode() }}
                    style={{
                      background: 'none', border: 'none',
                      color: 'rgba(255,255,255,0.35)', fontSize: 14,
                      cursor: 'pointer', padding: '4px 6px',
                    }}
                    title="移动看板娘"
                  >
                    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M5 9l-3 3 3 3"/><path d="M9 5l3-3 3 3"/><path d="M15 19l-3 3-3-3"/><path d="M19 9l3 3-3 3"/><path d="M2 12h20"/><path d="M12 2v20"/>
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
                  onClick={(e) => { e.stopPropagation(); window.blur(); collapse() }}
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
                <div className="bg-[#151515] text-white font-sans antialiased scrollbar-thin" style={{ borderRadius: '12px 12px 0 0', overflow: 'auto', flex: 1, minHeight: 0 }}>
                  {settingsNav === 'pairing' && (
                    <div className="h-full overflow-y-auto bg-[#151515] pt-6 px-6 pb-10 scrollbar-thin">
                      <div className="max-w-3xl mx-auto">
                        <p className="text-sm text-white/50 mb-10">
                          将 Agent 与角色配对，配对后 Mini 中会显示对应角色的 GIF 动画。
                        </p>

                        {/* System (看板娘) */}
                        <div className="mb-8">
                          <h2 className="text-xs font-bold text-white/30 uppercase tracking-widest mb-3 px-4">看板娘</h2>
                          <div className="bg-[#0f0f0f] rounded-2xl border border-white/5 shadow-2xl overflow-hidden">
                            <AgentAccordionItem
                              agent={{ id: '__mini__', identityName: '看板娘' }}
                              characters={characters}
                              currentChar={miniChar?.name || ''}
                              isOpen={openAccordionId === '__mini__'}
                              onToggle={() => setOpenAccordionId(openAccordionId === '__mini__' ? null : '__mini__')}
                              onOpenCreate={() => setIsCreateModalOpen(true)}
                              onDeleteChar={handleDeleteChar}
                              onSelect={async (name) => {
                                const store = await load('settings.json', { defaults: {}, autoSave: true })
                                await store.set('mini_character', name)
                                await store.save()
                                loadMiniChar()
                              }}
                            />
                          </div>
                        </div>

                        {/* OpenClaw Agents */}
                        {agents.length > 0 && (
                          <div className="mb-8">
                            <h2 className="text-xs font-bold text-white/30 uppercase tracking-widest mb-3 px-4">OpenClaw Agents</h2>
                            <div className="bg-[#0f0f0f] rounded-2xl border border-white/5 shadow-2xl overflow-hidden">
                              {agents.map((agent) => (
                                  <AgentAccordionItem
                                    key={agent.id}
                                    agent={agent}
                                    characters={characters}
                                    currentChar={agentCharMap[agent.id] || DEFAULT_CHAR_NAME}
                                    isOpen={openAccordionId === agent.id}
                                    onToggle={() => setOpenAccordionId(openAccordionId === agent.id ? null : agent.id)}
                                    sourceLabel={agentSourceLabels[agent.id]}
                                    onOpenCreate={() => setIsCreateModalOpen(true)}
                                    onDeleteChar={handleDeleteChar}
                                    onSelect={async (charName) => {
                                      const updated = { ...agentCharMap, [agent.id]: charName }
                                      setAgentCharMap(updated)
                                      await saveAgentCharMap(updated)
                                    }}
                                  />
                                ))}
                            </div>
                          </div>
                        )}

                        {/* Claude Code */}
                        {enableClaudeCode && (
                          <div className="mb-8">
                            <h2 className="text-xs font-bold text-white/30 uppercase tracking-widest mb-3 px-4">Claude Code</h2>
                            <div className="bg-[#0f0f0f] rounded-2xl border border-white/5 shadow-2xl overflow-hidden">
                              <AgentAccordionItem
                                agent={{ id: 'claude-code', identityName: 'Claude Code', identityEmoji: '🤖' }}
                                characters={characters}
                                currentChar={claudeCharName}
                                isOpen={openAccordionId === 'claude-code'}
                                onToggle={() => setOpenAccordionId(openAccordionId === 'claude-code' ? null : 'claude-code')}
                                onOpenCreate={() => setIsCreateModalOpen(true)}
                                onDeleteChar={handleDeleteChar}
                                onSelect={async (charName) => {
                                  setClaudeCharName(charName)
                                  const store = await load('settings.json', { defaults: {}, autoSave: true })
                                  await store.set('claude_char', charName)
                                  await store.save()
                                }}
                              />
                            </div>
                          </div>
                        )}

                        {agents.length > 0 && characters.filter((c) => c.miniActions && Object.keys(c.miniActions).length > 0).length < agents.length && (
                          <div className="text-xs text-amber-400 bg-amber-500/10 border border-amber-500/20 rounded-lg p-3">
                            角色数量不足，请先创建更多带 Mini 动画的角色。
                          </div>
                        )}
                      </div>
                    </div>
                  )}
                  {settingsNav === 'settings' && (
                    <div className="h-full overflow-y-auto bg-[#151515] scrollbar-thin">
                      <SettingsTab disableSleepAnim={disableSleepAnim} onToggleSleepAnim={async (v) => { setDisableSleepAnim(v); const store = await getStore(); await store.set('disable_sleep_anim', v); await store.save() }} notifySound={notifySound} onChangeNotifySound={async (v) => { setNotifySound(v); const store = await getStore(); await store.set('notify_sound', v); await store.save() }} waitingSound={waitingSound} onToggleWaitingSound={async (v) => { setWaitingSound(v); const store = await getStore(); await store.set('waiting_sound', v); await store.save() }} mascotPosition={mascotPosition} onChangeMascotPosition={async (v) => { setMascotPosition(v); mascotPositionRef.current = v; const store = await getStore(); await store.set('mascot_position', v); await store.save() }} />
                    </div>
                  )}
                </div>
                <div style={{
                  background: '#1a1a1a', padding: '10px 14px',
                  borderRadius: '0 0 12px 12px', flexShrink: 0,
                  display: 'flex', alignItems: 'center', justifyContent: 'center',
                }}>
                  <span
                    onClick={() => invoke('open_url', { url: 'https://github.com/rainnoon/oc-claw' })}
                    style={{
                      color: 'rgba(255,255,255,0.35)', fontSize: 11, cursor: 'pointer',
                      transition: 'color 0.25s, transform 0.25s, letter-spacing 0.25s',
                      display: 'inline-flex', alignItems: 'center', gap: 4,
                    }}
                    onMouseEnter={(e) => { e.currentTarget.style.color = '#f5c542'; e.currentTarget.style.transform = 'scale(1.04)'; e.currentTarget.style.letterSpacing = '0.3px' }}
                    onMouseLeave={(e) => { e.currentTarget.style.color = 'rgba(255,255,255,0.35)'; e.currentTarget.style.transform = 'scale(1)'; e.currentTarget.style.letterSpacing = '0px' }}
                  >
                    如果觉得有用，给我们一个 <span style={{ fontSize: 13, lineHeight: 1 }}>⭐</span> 吧
                  </span>
                </div>
              </div>
            ) : (
              <AnimatePresence mode="wait">
              {(!inAgentDetail && !selectedClaudeSession && !selectedSessionKey && !showClaudeStats) ? (
              /* ===== Normal: character island + sessions ===== */
              <motion.div key="main"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                transition={{ duration: 0.15 }}
              >
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

                  {(() => {
                    // Shuffle slots by seed for random x positions
                    const shuffled = sessionSlots.map((slot, idx) => {
                      const seed = (slot.agentId + slot.sessionIdx).split('').reduce((a: number, c: string) => a + c.charCodeAt(0), 0)
                      return { slot, idx, seed }
                    }).sort((a, b) => ((a.seed * 7 + 13) % 97) - ((b.seed * 7 + 13) % 97))

                    return shuffled.map(({ slot, seed }, sortedIdx) => {
                    const gif = getMiniGif(slot.char, slot.petState ?? (slot.isWorking ? 'working' : 'idle'), true)
                    const singleRow = sessionSlots.length <= 6
                    const row = sortedIdx < 6 ? 0 : 1
                    const col = row === 0 ? sortedIdx : sortedIdx - 6
                    const cols = row === 0 ? Math.min(sessionSlots.length, 6) : Math.min(sessionSlots.length - 6, 4)
                    const slotW = 380 / Math.max(cols, 1)
                    const xBase = slotW * col + slotW / 2 - 22 + (row === 1 ? slotW * 0.4 : 0)
                    const yBase = row === 0 ? (singleRow ? 20 : 4) : 52
                    const jx = ((seed * 7) % 13) - 6
                    const jy = singleRow ? ((seed * 11) % 41) - 20 : ((seed * 11) % 9) - 4
                    const x = Math.max(2, Math.min(332, xBase + jx))
                    const y = yBase + jy
                    return (
                      <div
                        key={`${slot.agentId}-${slot.sessionIdx}`}
                        data-no-drag
                        onClick={() => {
                          if (slot.agentId.startsWith('claude:')) {
                            setSelectedAgentId(null); setSelectedSessionKey(null); setSelectedClaudeSession(null)
                            setShowClaudeStats(true)
                          } else {
                            setSelectedClaudeSession(null); setSelectedSessionKey(null)
                            setSelectedAgentId(slot.agentId)
                          }
                        }}
                        style={{
                          position: 'absolute', left: x, top: y,
                          display: 'flex', flexDirection: 'column', alignItems: 'center',
                          cursor: 'pointer', zIndex: 2,
                          transform: `translateY(${Math.sin((bobPhase + sortedIdx * 400) / 800) * 2}px)`,
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
                  })
                  })()}
                </div>

                {/* Session bars */}
                <div style={{ borderTop: '1px solid rgba(255,255,255,0.06)', background: '#1a1a1a' }}>
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
                                  fontSize: 14, cursor: 'pointer', padding: '8px 10px', flexShrink: 0,
                                  lineHeight: 1, margin: '-6px -6px -6px 0',
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
                          const isWaiting = cs.status === 'waiting'
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
                                background: isWaiting ? '#f59e0b' : isActive ? '#3b82f6' : 'rgba(255,255,255,0.15)',
                                boxShadow: isWaiting ? '0 0 6px rgba(245,158,11,0.6)' : isActive ? '0 0 6px rgba(59,130,246,0.6)' : 'none',
                                animation: (isActive || isWaiting) ? 'miniPulse 1.5s ease-in-out infinite' : 'none',
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
                                  {cs.tool ? `🔧 ${cs.tool}` : cs.status === 'stopped' ? 'idle' : cs.status === 'waiting' ? '⏳ waiting...' : cs.status === 'processing' ? 'thinking...' : cs.status === 'tool_running' ? 'working...' : cs.status === 'compacting' ? 'compacting...' : cs.status}
                                  {cs.userPrompt ? ` · ${cs.userPrompt}` : ''}
                                </div>
                              </div>
                              <button
                                data-no-drag
                                onClick={(e) => { e.stopPropagation(); invoke('remove_claude_session', { sessionId: cs.sessionId }).catch(() => {}); setClaudeSessions(prev => prev.filter(s => s.sessionId !== cs.sessionId)) }}
                                style={{
                                  background: 'none', border: 'none', color: 'rgba(255,255,255,0.15)',
                                  fontSize: 14, cursor: 'pointer', padding: '8px 10px', flexShrink: 0,
                                  lineHeight: 1, margin: '-6px -6px -6px 0',
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
              </motion.div>
            ) : selectedSessionKey ? (
              /* ===== OpenClaw session chat ===== */
              <motion.div key="oc-chat"
                style={{ background: '#1a1a1a' }}
                initial={{ opacity: 0, x: 30 }}
                animate={{ opacity: 1, x: 0 }}
                exit={{ opacity: 0, x: -30 }}
                transition={{ duration: 0.2, ease: [0.25, 0.1, 0.25, 1] }}>
                {sessionMessages.length === 0 ? (
                  <div style={{ color: 'rgba(255,255,255,0.3)', fontSize: 11, textAlign: 'center', padding: '30px 0' }}>
                    loading...
                  </div>
                ) : (
                  <ChatList messages={sessionMessages} accentColor="#2ecc71" />
                )}
              </motion.div>
            ) : selectedClaudeSession ? (
              /* ===== Claude session chat ===== */
              <motion.div key="claude-chat"
                style={{ background: '#1a1a1a' }}
                initial={{ opacity: 0, x: 30 }}
                animate={{ opacity: 1, x: 0 }}
                exit={{ opacity: 0, x: -30 }}
                transition={{ duration: 0.2, ease: [0.25, 0.1, 0.25, 1] }}>
                {claudeConversation.length === 0 ? (
                  <div style={{ color: 'rgba(255,255,255,0.3)', fontSize: 11, textAlign: 'center', padding: '30px 0' }}>
                    loading...
                  </div>
                ) : (
                  <ChatList messages={claudeConversation} accentColor="#007AFF" />
                )}
              </motion.div>
            ) : showClaudeStats ? (
              /* ===== Claude Code stats ===== */
              <motion.div key="claude-stats"
                style={{ background: '#1a1a1a' }}
                initial={{ opacity: 0, x: 30 }}
                animate={{ opacity: 1, x: 0 }}
                exit={{ opacity: 0, x: -30 }}
                transition={{ duration: 0.2, ease: [0.25, 0.1, 0.25, 1] }}>
                <ClaudeStatsView />
              </motion.div>
            ) : (
              /* ===== Agent detail panel (ui-2 style) ===== */
              <motion.div key="agent-detail"
                style={{ background: '#1a1a1a' }}
                initial={{ opacity: 0, x: 30 }}
                animate={{ opacity: 1, x: 0 }}
                exit={{ opacity: 0, x: -30 }}
                transition={{ duration: 0.2, ease: [0.25, 0.1, 0.25, 1] }}
              >
                <AgentDetailView
                  agent={selectedAgent}
                  metrics={metrics}
                  extraInfo={extraInfo}
                />
              </motion.div>
            )}
              </AnimatePresence>
            )}
          </div>
        </div>
      )}

      <CreateCharacterModal
        isOpen={isCreateModalOpen}
        onClose={() => setIsCreateModalOpen(false)}
        onSaved={async () => {
          await invoke('scan_characters')
          const chars = await loadCharacters()
          setCharacters(chars)
        }}
      />
    </div>
  )
}
