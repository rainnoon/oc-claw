import { useState, useEffect, useCallback, useRef } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { load } from '@tauri-apps/plugin-store'
import { listen } from '@tauri-apps/api/event'
import { ChevronDown, ChevronUp, Check, Loader2, Pen, Plus, X, Pin, Bell, BellOff, Move, Settings, Asterisk, Trash2, Cloud, PanelLeft, Rows } from 'lucide-react'
import { AnimatePresence, motion } from 'motion/react'
import ReactMarkdown from 'react-markdown'
import { useTranslation } from 'react-i18next'
import { SettingsTab } from './components/SettingsTab'
import { AgentDetailView } from './components/AgentDetailView'
import { CreateCharacterModal } from './components/CreateCharacterModal'
import { ClaudeStatsView } from './components/ClaudeStatsView'
import { getStore, DEFAULT_CHAR, DEFAULT_CHAR_NAME, loadCharacters, loadOcConnections, saveOcConnections } from './lib/store'
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
    if (el)
      requestAnimationFrame(() => {
        el.scrollTop = el.scrollHeight
      })
  }, [messages.length])

  const toggle = (i: number) =>
    setExpandedSet((prev) => {
      const s = new Set(prev)
      if (s.has(i)) s.delete(i)
      else s.add(i)
      return s
    })

  return (
    <div ref={containerRef} className="scrollbar-thin selectable-text" style={{ flex: 1, minHeight: 0, overflowY: 'auto', padding: '12px 14px' }}>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
        {messages.map((msg, i) => (
          <div key={i}>
            {msg.role === 'user'
              ? (() => {
                  const limit = 300
                  const truncated = !expandedSet.has(i) && msg.text.length > limit
                  return (
                    <div style={{ display: 'flex', justifyContent: 'flex-end' }}>
                      <div
                        style={{
                          background: accentColor,
                          borderRadius: 18,
                          padding: '8px 14px',
                          maxWidth: '80%',
                          color: '#fff',
                          fontSize: 13,
                          lineHeight: 1.5,
                          wordBreak: 'break-word',
                          whiteSpace: 'pre-wrap',
                        }}
                      >
                        {truncated ? msg.text.slice(0, limit) + '...' : msg.text}
                        {(truncated || (expandedSet.has(i) && msg.text.length > limit)) && (
                          <button
                            onClick={() => toggle(i)}
                            style={{
                              display: 'flex',
                              alignItems: 'center',
                              justifyContent: 'center',
                              gap: 2,
                              width: '100%',
                              marginTop: 4,
                              padding: '2px 0',
                              background: 'none',
                              border: 'none',
                              color: 'rgba(255,255,255,0.5)',
                              fontSize: 11,
                              cursor: 'pointer',
                            }}
                          >
                            <ChevronDown style={{ width: 12, height: 12, transition: 'transform 0.2s', transform: expandedSet.has(i) ? 'rotate(180deg)' : 'none' }} />
                          </button>
                        )}
                      </div>
                    </div>
                  )
                })()
              : (() => {
                  const limit = 500
                  const truncated = !expandedSet.has(i) && msg.text.length > limit
                  return (
                    <div style={{ display: 'flex', alignItems: 'flex-start', gap: 8 }}>
                      <div style={{ width: 5, height: 5, borderRadius: '50%', background: accentColor, marginTop: 6, flexShrink: 0 }} />
                      <div
                        className="markdown-content"
                        style={{
                          color: '#ddd',
                          fontSize: 13,
                          lineHeight: 1.5,
                          wordBreak: 'break-word',
                          maxWidth: '90%',
                        }}
                      >
                        <ReactMarkdown>{truncated ? msg.text.slice(0, limit) + '...' : msg.text}</ReactMarkdown>
                        {(truncated || (expandedSet.has(i) && msg.text.length > limit)) && (
                          <button
                            onClick={() => toggle(i)}
                            style={{
                              display: 'flex',
                              alignItems: 'center',
                              justifyContent: 'center',
                              gap: 2,
                              width: '100%',
                              marginTop: 2,
                              padding: '2px 0',
                              background: 'none',
                              border: 'none',
                              color: 'rgba(255,255,255,0.3)',
                              fontSize: 11,
                              cursor: 'pointer',
                            }}
                          >
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
        requestAnimationFrame(() => {
          setGifSrc(src!)
          setPlaying(true)
        })
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
  return <canvas ref={canvasRef} style={style} {...(props as any)} />
}

function getMiniGif(char: CharacterMeta | undefined, petState: PetState | boolean, useTop = false): string | undefined {
  // backward compat: boolean → PetState
  const state: PetState = typeof petState === 'boolean' ? (petState ? 'working' : 'idle') : petState
  const c = char?.miniActions && Object.values(char.miniActions).flat().length > 0 ? char : DEFAULT_CHAR
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
  const idleGifs = allGifs.filter((g) => /idle|sleep|rest/.test(g))
  const workGifs = allGifs.filter((g) => g.includes('work'))
  const actionGifs = allGifs.filter((g) => !/idle|sleep|rest/.test(g))
  if ((state === 'working' || state === 'compacting' || state === 'waiting') && actionGifs.length > 0) {
    return workGifs[0] || actionGifs[0]
  }
  return idleGifs[0] || allGifs[0]
}

function AgentAccordionItem({
  agent,
  characters,
  currentChar,
  onSelect,
  isOpen,
  onToggle,
  onOpenCreate,
  onDeleteChar,
  sourceLabel,
}: {
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
  const { t } = useTranslation()
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
      <div className="relative flex items-center justify-between p-4 hover:bg-white/[0.02] transition-colors cursor-pointer" onClick={onToggle}>
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
          <span>{currentChar ? (characters.find((c) => c.name === currentChar)?.builtin ? t(`charNames.${currentChar}`, currentChar) : currentChar) : t('mini.unassigned')}</span>
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
                    title={t('mini.createChar')}
                  >
                    <Plus className="w-4 h-4" />
                  </button>
                )}
                <button
                  onClick={() => setIsEditing(!isEditing)}
                  className={`flex items-center justify-center w-7 h-7 rounded-md transition-colors ${
                    isEditing ? 'bg-red-500/20 text-red-400 border border-red-500/20' : 'bg-white/5 text-white/60 hover:text-white hover:bg-white/10 border border-transparent'
                  }`}
                  title={isEditing ? t('common.done') : t('common.edit')}
                >
                  {isEditing ? <Check className="w-4 h-4" /> : <Pen className="w-3.5 h-3.5" />}
                </button>
              </div>

              {/* Character Grid grouped by IP */}
              <div className="max-h-[260px] overflow-y-auto pr-2 pt-2 scrollbar-white">
                {(() => {
                  const groups: { ip: string; chars: typeof charsWithMini }[] = []
                  const ipOrder: string[] = []
                  for (const c of charsWithMini) {
                    const ip = c.ip || '自定义'
                    if (!ipOrder.includes(ip)) ipOrder.push(ip)
                  }
                  // 自定义 always first, 其他 always last
                  const customIdx = ipOrder.indexOf('自定义')
                  if (customIdx > 0) {
                    ipOrder.splice(customIdx, 1)
                    ipOrder.unshift('自定义')
                  }
                  const otherIdx = ipOrder.indexOf('其他')
                  if (otherIdx >= 0 && otherIdx < ipOrder.length - 1) {
                    ipOrder.splice(otherIdx, 1)
                    ipOrder.push('其他')
                  }
                  for (const ip of ipOrder) {
                    groups.push({ ip, chars: charsWithMini.filter((c) => (c.ip || '自定义') === ip) })
                  }
                  return groups.map(({ ip, chars }) => (
                    <div key={ip} className="mb-3 last:mb-0">
                      <div className="text-[10px] font-medium text-white/25 uppercase tracking-wider mb-2 px-1">
                        {ip === '自定义' ? t('mini.custom') : ip === '其他' ? t('mini.other') : ip === '原神' ? t('mini.ipGenshin') : ip === '赛马娘' ? t('mini.ipUmaMusume') : ip}
                      </div>
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
                              <span className="text-sm text-white/80 truncate flex-1">{c.builtin ? t(`charNames.${c.name}`, c.name) : c.name}</span>
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

// Returns null for incomplete remote connections (missing host/user)
// so callers can skip them instead of accidentally treating them as local.
function connToOcParams(conn: OcConnection): OcParams | null {
  if (conn.type === 'remote') {
    if (conn.host && conn.user) return { mode: 'remote', sshHost: conn.host, sshUser: conn.user }
    return null // incomplete remote — skip
  }
  return {} // local
}

export default function Mini() {
  const [expanded, setExpanded] = useState(false)
  const [showPanel, setShowPanel] = useState(false)
  const [agents, setAgents] = useState<AgentInfo[]>([])
  const [healthMap, setHealthMap] = useState<Record<string, boolean>>({})
  const [characters, setCharacters] = useState<CharacterMeta[]>([])
  const [agentCharMap, setAgentCharMap] = useState<Record<string, string>>({})
  const [miniChar, setMiniChar] = useState<CharacterMeta | null>(null)

  const [allSessions, setAllSessions] = useState<MiniSessionInfo[]>([])
  const [anySessionActive, setAnySessionActive] = useState(false)
  const [refreshingAgents, setRefreshingAgents] = useState(false)
  // Snapshot of connection config to detect changes across settings edits
  const lastConnSnapshotRef = useRef<string>('')
  const refreshTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const dismissedSessionsRef = useRef<Map<string, number>>(new Map())

  // Agent detail
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null)
  const [metrics, setMetrics] = useState<AgentMetrics | null>(null)
  const [extraInfo, setExtraInfo] = useState<any>(null)

  // OpenClaw session chat
  const [selectedSessionKey, setSelectedSessionKey] = useState<{ agentId: string; key: string } | null>(null)
  const [sessionMessages, setSessionMessages] = useState<any[]>([])

  // Claude Code & Cursor
  const [claudeSessions, setClaudeSessions] = useState<any[]>([])
  const claudeSessionsRef = useRef<any[]>([])
  claudeSessionsRef.current = claudeSessions
  const [charQueue, setCharQueue] = useState<string[]>([DEFAULT_CHAR_NAME])
  const [selectedClaudeSession, setSelectedClaudeSession] = useState<string | null>(null)
  const [claudeConversation, setClaudeConversation] = useState<any[]>([])
  const [showClaudeStats, setShowClaudeStats] = useState(false)
  const [sessionNicknames, setSessionNicknames] = useState<Record<string, string>>({})
  const [editingSessionTitle, setEditingSessionTitle] = useState<string | null>(null)
  const editingTitleValueRef = useRef('')
  const editingTitleDefaultRef = useRef('')
  const saveSessionNickname = useCallback(async (sessionId: string, val: string, defaultName: string) => {
    const trimmed = val.trim()
    setSessionNicknames((prev) => {
      const next = { ...prev }
      if (trimmed && trimmed !== defaultName) {
        next[sessionId] = trimmed
      } else {
        delete next[sessionId]
      }
      load('settings.json', { defaults: {}, autoSave: true }).then(async (store) => {
        await store.set('session_nicknames', next)
        await store.save()
      })
      return next
    })
  }, [])
  useEffect(() => {
    if (!showPanel && editingSessionTitle) {
      saveSessionNickname(editingSessionTitle, editingTitleValueRef.current, editingTitleDefaultRef.current)
      setEditingSessionTitle(null)
    }
  }, [showPanel, editingSessionTitle, saveSessionNickname])

  // OC multi-connection: qualifiedId → connection params, qualifiedId → real agent ID, qualifiedId → source label
  const agentConnMapRef = useRef<Map<string, OcParams>>(new Map())
  const agentRealIdMapRef = useRef<Map<string, string>>(new Map())
  const [agentSourceLabels, setAgentSourceLabels] = useState<Record<string, string>>({})

  // Feature toggles
  const [enableClaudeCode, setEnableClaudeCode] = useState(true)
  const [enableCodex, setEnableCodex] = useState(true)
  const [enableCursor, setEnableCursor] = useState(true)
  const [soundEnabled, setSoundEnabled] = useState(true)
  const [cursorSoundEnabled, setCursorSoundEnabled] = useState(false)
  const [notifySound, setNotifySound] = useState<'default' | 'manbo'>('default')
  const [waitingSound, setWaitingSound] = useState(false)
  const [autoCloseCompletion, setAutoCloseCompletion] = useState(false)
  const [disableSleepAnim, setDisableSleepAnim] = useState(true)
  const [panelMaxHeight, setPanelMaxHeight] = useState(300)
  const panelMaxHeightRef = useRef(300)
  panelMaxHeightRef.current = panelMaxHeight
  const [mascotPosition, setMascotPosition] = useState<'left' | 'right'>('right')
  const mascotPositionRef = useRef<'left' | 'right'>('right')
  const [islandBg, setIslandBg] = useState('__anime__')
  const [uiScale, setUiScale] = useState(1.0)
  const [bgPos, setBgPos] = useState({ x: 50, y: 50 })

  // Settings mode: native window grows, then a separate settings card animates in.
  const [settingsMode, setSettingsMode] = useState(false)
  const settingsModeRef = useRef(false)
  const [showSettingsOverlay, setShowSettingsOverlay] = useState(false)
  const [settingsTransitioning, setSettingsTransitioning] = useState(false)
  const settingsTransitioningRef = useRef(false)
  const filePickerOpenRef = useRef(false)
  const [settingsNav, setSettingsNav] = useState<'pairing' | 'settings'>('pairing')
  const [openAccordionId, setOpenAccordionId] = useState<string | null>(null)
  const [isCreateModalOpen, _setIsCreateModalOpen] = useState(false)
  const isCreateModalOpenRef = useRef(false)
  const setIsCreateModalOpen = (v: boolean) => {
    isCreateModalOpenRef.current = v
    _setIsCreateModalOpen(v)
  }
  const [hiding, setHiding] = useState(false)
  const [pinned, setPinned] = useState(false)
  const pinnedRef = useRef(false)
  const [viewMode, _setViewMode] = useState<'island' | 'efficiency'>('efficiency')
  const viewModeRef = useRef<'island' | 'efficiency'>('efficiency')
  const expandedRef = useRef(false)
  const setViewMode = useCallback(async (v: 'island' | 'efficiency' | ((prev: 'island' | 'efficiency') => 'island' | 'efficiency')) => {
    _setViewMode((prev) => {
      const next = typeof v === 'function' ? v(prev) : v
      viewModeRef.current = next
      load('settings.json', { defaults: {}, autoSave: true }).then((store) => {
        store.set('view_mode', next)
        store.save()
      })
      return next
    })
  }, [])
  // showIdleSessions removed — all sessions visible, important ones sorted to top
  const collapsingRef = useRef(false)
  const customPosRef = useRef<{ x: number; y: number } | null>(null)
  const [moveMode, _setMoveMode] = useState(false)
  const moveModeRef = useRef(false)
  const setMoveMode = (v: boolean) => {
    moveModeRef.current = v
    _setMoveMode(v)
  }

  const { t } = useTranslation()

  // Load mini character from store
  const loadMiniChar = useCallback(async () => {
    const store = await load('settings.json', { defaults: {}, autoSave: true })
    await store.reload()
    const miniCharName = ((await store.get('mini_character')) as string) || ''
    const chars = (await store.get('characters')) as CharacterMeta[] | null
    if (miniCharName && chars) {
      const found = chars.find((c) => c.name === miniCharName)
      if (found) {
        setMiniChar(found)
        return
      }
    }
    if (chars) {
      const fallback = chars.find((c) => c.miniActions && Object.keys(c.miniActions).length > 0)
      if (fallback) setMiniChar(fallback)
    }
  }, [])

  useEffect(() => {
    loadMiniChar()
    load('settings.json', { defaults: {}, autoSave: true }).then(async (store) => {
      const nicks = (await store.get('session_nicknames')) as Record<string, string> | null
      if (nicks) setSessionNicknames(nicks)
    })
    const unlisten = listen('character-changed', () => loadMiniChar())
    return () => {
      unlisten.then((fn) => fn())
    }
  }, [loadMiniChar])

  useEffect(() => {
    load('settings.json', { defaults: {}, autoSave: true }).then(async (store) => {
      _setViewMode('efficiency')
      viewModeRef.current = 'efficiency'
      invoke('set_mini_expanded', { expanded: false, position: 'right', efficiency: true }).catch(() => {})
      await store.set('view_mode', 'efficiency')
      // Restore saved mascot position from a previous drag.
      const pos = (await store.get('mini_custom_pos')) as { x: number; y: number } | null
      if (pos) {
        customPosRef.current = pos
        invoke('set_mini_origin', pos).catch(() => {})
      }
    })
  }, [])

  const fetchAgents = useCallback(async () => {
    // Skip polling while settings page is open — snapshot comparison would
    // detect the config change prematurely, consuming it before the user exits
    // settings, which means exitSettings' call wouldn't show the loading overlay.
    if (settingsModeRef.current) return

    try {
      const chars = await loadCharacters()
      setCharacters(chars)
      const store0 = await load('settings.json', { defaults: {}, autoSave: true })
      const q = (await store0.get('char_queue')) as string[] | null
      if (q && q.length) setCharQueue(q)
    } catch (e) {
      console.warn('[fetchAgents] loadCharacters failed:', e)
    }
    try {
      const store = await load('settings.json', { defaults: {}, autoSave: true })
      const connections = await loadOcConnections()

      // Detect connection config changes — show loading overlay if changed
      const snapshot = JSON.stringify(connections.map((c) => ({ id: c.id, type: c.type, host: c.host, user: c.user })))
      const configChanged = lastConnSnapshotRef.current !== '' && snapshot !== lastConnSnapshotRef.current
      lastConnSnapshotRef.current = snapshot
      if (configChanged) {
        setAgents([])
        setAllSessions([])
        setRefreshingAgents(true)
        if (refreshTimeoutRef.current) clearTimeout(refreshTimeoutRef.current)
        refreshTimeoutRef.current = setTimeout(() => setRefreshingAgents(false), 45000)
      }

      const newConnMap = new Map<string, OcParams>()
      const newRealIdMap = new Map<string, string>()
      const newSourceLabels: Record<string, string> = {}
      const allAgents: AgentInfo[] = []
      const multi = connections.length > 1
      await Promise.all(
        connections.map(async (conn) => {
          try {
            const oc = connToOcParams(conn)
            if (!oc) return // skip incomplete remote connections
            const agents = (await invoke('get_agents', oc)) as AgentInfo[]
            const prefix = multi ? `${conn.id.slice(0, 8)}:` : ''
            const label = conn.type === 'local' ? t('mini.local') : conn.host || t('mini.remote')
            for (const a of agents) {
              const qualifiedId = prefix + a.id
              newConnMap.set(qualifiedId, oc)
              newRealIdMap.set(qualifiedId, a.id)
              if (multi) newSourceLabels[qualifiedId] = label
              allAgents.push({ ...a, id: qualifiedId })
            }
          } catch (e) {
            console.warn('[fetchAgents] connection failed:', conn.id, e)
          }
        }),
      )

      agentConnMapRef.current = newConnMap
      agentRealIdMapRef.current = newRealIdMap
      setAgentSourceLabels(newSourceLabels)
      const charMap = (await store.get('agent_char_map')) as Record<string, string> | null
      setAgents(allAgents)
      setAgentCharMap(charMap || {})
      // Clear loading overlay — data is now fresh
      setRefreshingAgents(false)
      if (refreshTimeoutRef.current) {
        clearTimeout(refreshTimeoutRef.current)
        refreshTimeoutRef.current = null
      }
    } catch (e) {
      console.warn('[fetchAgents] get_agents failed:', e)
      setRefreshingAgents(false)
      if (refreshTimeoutRef.current) {
        clearTimeout(refreshTimeoutRef.current)
        refreshTimeoutRef.current = null
      }
    }
  }, [])

  const playDefaultSound = useCallback(() => {
    if (navigator.userAgent.includes('Windows')) {
      new Audio('/audio/glass.mp3').play().catch(() => {})
    } else {
      invoke('play_sound', { name: 'Purr' }).catch(() => {})
    }
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
      playDefaultSound()
    }
  }, [])

  const prevHealthRef = useRef<Record<string, boolean>>({})
  const prevSessionHealthRef = useRef<Record<string, boolean>>({})
  // Prevent concurrent pollHealth calls — if a remote SSH call takes > 1s,
  // the 1s interval would stack requests, overwhelming the SSH socket and
  // causing repeated "stale socket" failures.
  const pollHealthBusyRef = useRef(false)
  const pollHealth = useCallback(async () => {
    if (pollHealthBusyRef.current) return
    pollHealthBusyRef.current = true
    try {
      const connections = await loadOcConnections()
      // Start with previous data — only overwrite for connections that succeed
      const hMap: Record<string, boolean> = { ...prevHealthRef.current }
      const sMap: Record<string, boolean> = { ...prevSessionHealthRef.current }
      const freshKeys = new Set<string>() // session keys that got fresh data this round
      await Promise.all(
        connections.map(async (conn) => {
          const prefix = connections.length > 1 ? `${conn.id.slice(0, 8)}:` : ''
          try {
            const oc = connToOcParams(conn)
            if (!oc) {
              // Incomplete remote connection — still clear stale health data for
              // this prefix so the mascot/status doesn't stay "busy" from the
              // previous (now-removed) connection's data.
              for (const k of Object.keys(hMap)) {
                if (prefix === '' || k.startsWith(prefix)) delete hMap[k]
              }
              for (const k of Object.keys(sMap)) {
                if (prefix === '' || k.startsWith(prefix)) delete sMap[k]
              }
              return
            }
            const health = (await invoke('get_health', oc)) as { agents: AgentHealth[]; gatewayAlive?: boolean }
            // Gateway dead (local OpenClaw process not running) — remove this
            // connection from settings so the character cleanly goes idle instead
            // of flickering between stale "working" and "idle" states.
            if (health.gatewayAlive === false) {
              console.warn('[pollHealth] gateway dead, removing connection:', conn.id)
              const remaining = connections.filter((c) => c.id !== conn.id)
              saveOcConnections(remaining)
              for (const k of Object.keys(hMap)) {
                if (prefix === '' || k.startsWith(prefix)) delete hMap[k]
              }
              for (const k of Object.keys(sMap)) {
                if (prefix === '' || k.startsWith(prefix)) delete sMap[k]
              }
              return
            }
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
          } catch {
            /* SSH/invoke failed — previous data preserved */
          }
        }),
      )

      // Detect session active→inactive transitions (only for fresh data)
      // Skip sub-agent sessions — their key contains ":subagent:" (from OpenClaw session key format)
      const prev = prevSessionHealthRef.current
      if (freshKeys.size > 0) {
        const anyBecameInactive = Array.from(freshKeys).some((k) => prev[k] === true && sMap[k] === false && !k.includes(':subagent:'))
        if (anyBecameInactive) {
          console.log('[pollHealth] session became inactive, prev:', prev, 'curr:', sMap)
          playOcCompletionSound('pollHealth')
        }
      }
      prevSessionHealthRef.current = sMap

      const anyActive = Object.values(sMap).some((v) => v)
      setAnySessionActive(anyActive)

      prevHealthRef.current = hMap
      setHealthMap(hMap)
    } catch {
      /* ignore */
    }
    pollHealthBusyRef.current = false
  }, [playOcCompletionSound])

  const previewCacheRef = useRef<Map<string, { active: boolean; lastUserMsg?: string; lastAssistantMsg?: string; fetchedAt: number }>>(new Map())
  const previewQueueRef = useRef<string[]>([])
  const previewTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const sessionFileMapRef = useRef<Map<string, string>>(new Map())
  const sessionAgentMapRef = useRef<Map<string, string>>(new Map()) // sessionCompositeKey → qualifiedAgentId
  const fetchingSessionsRef = useRef(false)

  const fetchAllSessions = useCallback(async () => {
    if (agents.length === 0) {
      setAllSessions([])
      return
    }
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
          results.push(...s.map((ss) => ({ ...ss, agentId: agent.id })))
        } catch {
          /* ignore */
        }
      }),
    )
    console.log('[fetchAllSessions] raw results:', results.length, results)
    // Keep previous data if all fetches failed (SSH backoff etc.)
    if (results.length === 0 && previewCacheRef.current.size > 0) {
      console.log('[fetchAllSessions] empty results, keeping cache')
      fetchingSessionsRef.current = false
      return
    }
    const seen = new Set<string>()
    const deduped = results.filter((s) => {
      const k = `${s.agentId}:${s.key}`
      if (seen.has(k)) return false
      seen.add(k)
      return true
    })
    const filtered = deduped.filter((s) => {
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
    const merged = top
      .map((s) => {
        const k = `${s.agentId}:${s.key}`
        const cached = previewCacheRef.current.get(k)
        if (cached) {
          return { ...s, active: cached.active, lastUserMsg: cached.lastUserMsg, lastAssistantMsg: cached.lastAssistantMsg }
        }
        return s
      })
      // Filter out OpenClaw sub-agent sessions (key contains ":subagent:")
      .filter((s) => !s.key.includes(':subagent:'))
    merged.sort((a, b) => (b.active ? 1 : 0) - (a.active ? 1 : 0) || b.updatedAt - a.updatedAt)
    console.log('[fetchAllSessions] final merged:', merged.length, merged)
    setAllSessions(merged)

    // Build session file lookup and queue preview fetches
    const queue: string[] = []
    for (const s of top) {
      const k = `${s.agentId}:${s.key}`
      if (s.sessionFile) sessionFileMapRef.current.set(k, s.sessionFile)
      sessionAgentMapRef.current.set(k, s.agentId)
      const cached = previewCacheRef.current.get(k)
      const staleTime = cached?.active ? 8000 : 15000 // poll active sessions faster
      const stale = !cached || Date.now() - cached.fetchedAt > staleTime
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
    return () => {
      clearInterval(a)
      clearInterval(h)
    }
  }, [fetchAgents, pollHealth])

  // Update allSessions active states from pollHealth session data
  const syncSessionActiveStates = useCallback(() => {
    const sMap = prevSessionHealthRef.current
    if (Object.keys(sMap).length === 0) return
    setAllSessions((prev) => {
      let changed = false
      const updated = prev.map((s) => {
        const key = `${s.agentId}:${s.key}`
        const isActive = !!sMap[key]
        if (s.active !== isActive) {
          changed = true
          return { ...s, active: isActive }
        }
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
    if (previewTimerRef.current) {
      clearTimeout(previewTimerRef.current)
      previewTimerRef.current = null
    }
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
          setAllSessions((prev) =>
            prev.map((s) => {
              if (`${s.agentId}:${s.key}` === k) {
                return { ...s, active: p.active, lastUserMsg: p.lastUserMsg, lastAssistantMsg: p.lastAssistantMsg }
              }
              return s
            }),
          )
        })
        .catch(() => {
          /* ignore */
        })
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
    const t1 = setInterval(() => {
      fetchAllSessions().then(() => drainPreviewQueue())
    }, 5000)
    return () => {
      clearInterval(t1)
      if (previewTimerRef.current) {
        clearTimeout(previewTimerRef.current)
        previewTimerRef.current = null
      }
    }
  }, [expanded, fetchAllSessions, drainPreviewQueue])

  // Load feature toggles
  useEffect(() => {
    ;(async () => {
      const store = await load('settings.json', { defaults: {}, autoSave: true })
      const cc = await store.get('enable_claudecode')
      if (typeof cc === 'boolean') setEnableClaudeCode(cc)
      const cod = await store.get('enable_codex')
      setEnableCodex(cod !== false)
      if (cc !== false || cod !== false) invoke('install_claude_hooks').catch(() => {})
      const cur = await store.get('enable_cursor')
      setEnableCursor(cur !== false)
      if (cur !== false) invoke('install_cursor_hooks').catch(() => {})
      const snd = await store.get('sound_enabled')
      if (typeof snd === 'boolean') setSoundEnabled(snd)
      const csnd = await store.get('cursor_sound_enabled')
      if (typeof csnd === 'boolean') setCursorSoundEnabled(csnd)
      const ns = (await store.get('notify_sound')) as string
      if (ns === 'default' || ns === 'manbo') setNotifySound(ns)
      const ws = await store.get('waiting_sound')
      if (typeof ws === 'boolean') setWaitingSound(ws)
      const acc = await store.get('auto_close_completion')
      if (typeof acc === 'boolean') setAutoCloseCompletion(acc)
      const dsa = await store.get('disable_sleep_anim')
      if (typeof dsa === 'boolean') setDisableSleepAnim(dsa)
      const pmh = await store.get('panel_max_height')
      if (typeof pmh === 'number' && pmh >= 200 && pmh <= 500) setPanelMaxHeight(pmh)
      const mp = (await store.get('mascot_position')) as string
      if (mp === 'left' || mp === 'right') {
        setMascotPosition(mp)
        mascotPositionRef.current = mp
      }
      const bg = (await store.get('island_bg')) as string
      if (bg) setIslandBg(bg)
      const bp = (await store.get('island_bg_pos')) as { x: number; y: number }
      if (bp) setBgPos(bp)
      const queue = (await store.get('char_queue')) as string[] | null
      if (queue && queue.length) setCharQueue(queue)
      invoke('get_ui_scale')
        .then((s) => {
          if (typeof s === 'number' && s > 0) setUiScale(s)
        })
        .catch(() => {})
    })()
  }, [])

  // Poll Claude/Codex/Cursor sessions
  useEffect(() => {
    if (!(enableClaudeCode || enableCodex || enableCursor)) {
      setClaudeSessions([])
      return
    }
    // Track which sessions already had lastResponse so we only auto-expand once.
    const seenCompletions = new Set<string>()
    const poll = async () => {
      try {
        const sessions = (await invoke('get_claude_sessions')) as any[]
        // In efficiency mode, auto-expand panel when a session just completed
        // with an AI response (lastResponse appeared for the first time).
        // Mark all newly completed sessions as seen, but only auto-expand
        // if the session's terminal tab is not currently active.
        for (const s of sessions) {
          if (s.lastResponse && s.status === 'stopped' && !seenCompletions.has(s.sessionId)) {
            seenCompletions.add(s.sessionId)
            // Only auto-expand if tab not active and panel is collapsed
            if (!s.isActiveTab && viewModeRef.current === 'efficiency' && !expandedRef.current && !expandingRef.current && !collapsingRef.current) {
              hoverExpandedRef.current = true
              setCompletionSessionId(s.sessionId)
              expandFnRef.current?.()
            }
          }
        }
        // Keep seenCompletions in sync: remove sessions that no longer have lastResponse
        for (const sid of seenCompletions) {
          if (!sessions.find((s: any) => s.sessionId === sid && s.lastResponse)) {
            seenCompletions.delete(sid)
          }
        }
        setClaudeSessions(sessions)
      } catch {
        /* ignore */
      }
    }
    poll()
    const t = setInterval(poll, 2000)
    return () => clearInterval(t)
  }, [enableClaudeCode, enableCodex, enableCursor])

  // Listen for Claude/Codex/Cursor task completion → play sound
  const soundEnabledRef = useRef(soundEnabled)
  soundEnabledRef.current = soundEnabled
  const cursorSoundEnabledRef = useRef(cursorSoundEnabled)
  cursorSoundEnabledRef.current = cursorSoundEnabled
  const notifySoundRef = useRef(notifySound)
  notifySoundRef.current = notifySound
  const waitingSoundRef = useRef(waitingSound)
  waitingSoundRef.current = waitingSound
  const autoCloseCompletionRef = useRef(autoCloseCompletion)
  autoCloseCompletionRef.current = autoCloseCompletion
  const autoCloseTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => {
    if (!(enableClaudeCode || enableCodex || enableCursor)) return
    const unlisten = listen('claude-task-complete', (ev: any) => {
      if (ev.payload?.waiting && viewModeRef.current === 'efficiency') {
        setEffListCollapsed(true)
        if (!expandedRef.current && expandFnRef.current) {
          expandFnRef.current()
        }
      }
      const currentSession = claudeSessionsRef.current.find((s) => s.sessionId === ev.payload?.sessionId)
      const isCursor = ev.payload?.source === 'cursor' || currentSession?.source === 'cursor'
      const shouldSound = isCursor ? cursorSoundEnabledRef.current : soundEnabledRef.current
      if (!shouldSound) return
      if (ev.payload?.waiting && !waitingSoundRef.current) return
      if (notifySoundRef.current === 'manbo') {
        new Audio('/audio/manbo.m4a').play().catch(() => {})
      } else {
        playDefaultSound()
      }
    })
    return () => {
      unlisten.then((fn) => fn())
    }
  }, [enableClaudeCode, enableCodex, enableCursor])

  // Fetch OpenClaw session messages when selected
  useEffect(() => {
    if (!selectedSessionKey) {
      setSessionMessages([])
      return
    }
    let cancelled = false
    const fetchMsgs = async () => {
      try {
        const oc = agentConnMapRef.current.get(selectedSessionKey.agentId) || {}
        const realId = agentRealIdMapRef.current.get(selectedSessionKey.agentId) || selectedSessionKey.agentId
        const msgs = (await invoke('get_session_messages', { agentId: realId, sessionKey: selectedSessionKey.key, ...oc })) as any[]
        if (!cancelled) setSessionMessages(msgs)
      } catch {
        if (!cancelled) setSessionMessages([])
      }
    }
    fetchMsgs()
    const t = setInterval(fetchMsgs, 3000)
    return () => {
      cancelled = true
      clearInterval(t)
    }
  }, [selectedSessionKey])

  // Fetch Claude conversation when selected
  useEffect(() => {
    if (!selectedClaudeSession) {
      setClaudeConversation([])
      return
    }
    let cancelled = false
    const fetch = async () => {
      try {
        const msgs = (await invoke('get_claude_conversation', { sessionId: selectedClaudeSession })) as any[]
        if (!cancelled) setClaudeConversation(msgs)
      } catch {
        if (!cancelled) setClaudeConversation([])
      }
    }
    fetch()
    const t = setInterval(fetch, 3000)
    return () => {
      cancelled = true
      clearInterval(t)
    }
  }, [selectedClaudeSession])

  // Fetch agent metrics when selected
  useEffect(() => {
    if (!selectedAgentId) {
      setMetrics(null)
      setExtraInfo(null)
      return
    }
    let cancelled = false
    const realId = agentRealIdMapRef.current.get(selectedAgentId) || selectedAgentId
    const fetchMetrics = async () => {
      try {
        const oc = agentConnMapRef.current.get(selectedAgentId) || {}
        const m = (await invoke('get_agent_metrics', { agentId: realId, ...oc })) as AgentMetrics
        if (!cancelled) setMetrics(m)
      } catch {
        if (!cancelled) setMetrics(null)
      }
    }
    const fetchExtra = async () => {
      const oc = agentConnMapRef.current.get(selectedAgentId) || {}
      try {
        const e = (await invoke('get_agent_extra_info', { agentId: realId, ...oc })) as any
        if (!cancelled) setExtraInfo(e)
      } catch {
        if (!cancelled) setExtraInfo(null)
      }
    }
    fetchMetrics()
    fetchExtra()
    const i1 = setInterval(fetchMetrics, 2000)
    const i2 = setInterval(fetchExtra, 10000)
    return () => {
      cancelled = true
      clearInterval(i1)
      clearInterval(i2)
    }
  }, [selectedAgentId])

  // Build character slots (OpenClaw + Claude Code)
  const ocSlots: SessionSlot[] = allSessions.slice(0, MAX_SLOTS).map((s, i) => {
    const agent = agents.find((a) => a.id === s.agentId) || { id: s.agentId }
    const charName = agentCharMap[s.agentId]
    const char = characters.find((c) => c.name === charName) || DEFAULT_CHAR
    return { agentId: s.agentId, sessionIdx: i, agent, char, isWorking: s.active }
  })
  const claudeSlots: SessionSlot[] = claudeSessions.map((cs, i) => {
    const isWaiting = cs.status === 'waiting'
    const isCompacting = cs.status === 'compacting'
    const isActive = cs.status === 'processing' || cs.status === 'tool_running'
    const qName = charQueue[i % charQueue.length]
    const char = characters.find((c) => c.name === qName) || DEFAULT_CHAR
    const petState: PetState = isWaiting ? 'waiting' : isCompacting ? 'compacting' : isActive ? 'working' : 'idle'
    return {
      agentId: `claude:${cs.sessionId}`,
      sessionIdx: ocSlots.length + i,
      agent: { id: `claude:${cs.sessionId}`, identityName: 'Claude', identityEmoji: '🤖' },
      char,
      isWorking: isActive || isCompacting || isWaiting,
      petState,
    }
  })
  const sessionSlots = [...ocSlots, ...claudeSlots].slice(0, MAX_SLOTS)

  const expandingRef = useRef(false)
  const expandFnRef = useRef<(() => void) | null>(null)
  const hoverExpandedRef = useRef(false)
  // Track which session triggered auto-expand on completion, so we can
  // show only that session's completion popup and collapse the rest.
  // State drives re-render; ref allows reads from async callbacks.
  const [completionSessionId, _setCompletionSessionId] = useState<string | null>(null)
  const completionSessionIdRef = useRef<string | null>(null)
  const [effListCollapsed, setEffListCollapsed] = useState(false)
  const setCompletionSessionId = useCallback((id: string | null) => {
    completionSessionIdRef.current = id
    _setCompletionSessionId(id)
    if (id) setEffListCollapsed(true)
    if (autoCloseTimerRef.current) {
      clearTimeout(autoCloseTimerRef.current)
      autoCloseTimerRef.current = null
    }
    if (id && autoCloseCompletionRef.current) {
      autoCloseTimerRef.current = setTimeout(() => {
        completionSessionIdRef.current = null
        _setCompletionSessionId(null)
        autoCloseTimerRef.current = null
      }, 5000)
    }
  }, [])
  const hoverCloseTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const expand = useCallback(async () => {
    if (collapsingRef.current || expandingRef.current) return
    expandingRef.current = true
    setHiding(true)
    await new Promise<void>((r) => setTimeout(r, 50))
    await invoke('set_mini_expanded', {
      expanded: true,
      position: mascotPositionRef.current,
      efficiency: viewModeRef.current === 'efficiency',
      maxHeight: panelMaxHeightRef.current,
    })
    setHiding(false)
    setExpanded(true)
    expandedRef.current = true
    requestAnimationFrame(() => {
      requestAnimationFrame(() => setShowPanel(true))
    })
    expandingRef.current = false
  }, [])
  expandFnRef.current = expand

  const handleMascotPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (e.button !== 0 || collapsingRef.current) return

      // Normal mode: click to expand
      if (!moveMode) {
        hoverExpandedRef.current = false
        setCompletionSessionId(null)
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
        try {
          el.releasePointerCapture(pid)
        } catch {}
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
          requestAnimationFrame(() => {
            document.body.style.cursor = ''
          })
        } else {
          // Save dragged position (macOS native coordinates) to memory
          // and persist to settings so it survives restarts.
          invoke('get_mini_origin').then(async (pos) => {
            const [x, y] = pos as [number, number]
            customPosRef.current = { x, y }
            const store = await load('settings.json', { defaults: {}, autoSave: true })
            await store.set('mini_custom_pos', { x, y })
            await store.save()
          })
        }
      }

      el.addEventListener('pointermove', onMove)
      el.addEventListener('pointerup', onUp)
      el.addEventListener('lostpointercapture', cleanup)
    },
    [expand, moveMode],
  )

  const enterMoveMode = useCallback(async () => {
    setShowPanel(false)
    setTimeout(async () => {
      setHiding(true)
      setExpanded(false)
      expandedRef.current = false
      try {
        await new Promise<void>((r) => setTimeout(r, 50))
        await invoke('set_mini_expanded', { expanded: false, position: mascotPositionRef.current, efficiency: viewModeRef.current === 'efficiency' })
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
    hoverExpandedRef.current = false
    setCompletionSessionId(null)
    if (hoverCloseTimerRef.current) {
      clearTimeout(hoverCloseTimerRef.current)
      hoverCloseTimerRef.current = null
    }
    setIsCreateModalOpen(false)
    setShowPanel(false)
    setSelectedAgentId(null)
    setSelectedClaudeSession(null)
    setSelectedSessionKey(null)
    const wasSettings = settingsModeRef.current
    if (wasSettings) {
      setShowSettingsOverlay(false)
      setSettingsTransitioning(true)
    }
    const delay = wasSettings ? 280 : 350
    setTimeout(async () => {
      settingsModeRef.current = false
      setSettingsMode(false)
      setShowSettingsOverlay(false)
      if (wasSettings) {
        setSettingsNav('pairing')
        // Keep outside-click close behavior consistent with clicking "X":
        // re-sync feature toggles from store immediately.
        try {
          const store = await load('settings.json', { defaults: {}, autoSave: true })
          const cc = await store.get('enable_claudecode')
          setEnableClaudeCode(cc !== false)
          const cod = await store.get('enable_codex')
          setEnableCodex(cod !== false)
          const cur = await store.get('enable_cursor')
          setEnableCursor(cur !== false)
        } catch {}
        // Trigger immediate refresh so config changes are reflected right away.
        fetchAgents()
      }
      // Hide mascot first to avoid flicker at old position
      setHiding(true)
      // Use setTimeout instead of rAF (rAF may not fire when window is blurred)
      try {
        await new Promise<void>((r) => setTimeout(r, 50))
        if (wasSettings) {
          await invoke('set_mini_size', { restore: true, position: mascotPositionRef.current })
        } else {
          await invoke('set_mini_expanded', { expanded: false, position: mascotPositionRef.current, efficiency: viewModeRef.current === 'efficiency' })
        }
        setExpanded(false)
        expandedRef.current = false
        await new Promise<void>((r) => setTimeout(r, 50))
        if (customPosRef.current) {
          await invoke('set_mini_origin', customPosRef.current)
        }
      } catch {
        /* ensure hiding is always cleared */
      }
      setHiding(false)
      setSettingsTransitioning(false)
      // Brief cooldown to prevent focus event from immediately re-expanding
      setTimeout(() => {
        collapsingRef.current = false
        settingsTransitioningRef.current = false
      }, 300)
    }, delay)
  }, [fetchAgents])

  // ── Efficiency-mode notch hover tracking (native cursor polling) ──
  // On macOS the mini window sits in the menu-bar / notch area where the
  // system intercepts mouse events, so web-level onMouseEnter never fires.
  // A Rust-side 50ms poll of NSEvent.mouseLocation emits "efficiency-hover"
  // events which we handle here to open / close the panel on hover.
  useEffect(() => {
    if (viewMode === 'efficiency' && !moveMode) {
      invoke('set_efficiency_hover_tracking', { active: true }).catch(() => {})
    } else {
      invoke('set_efficiency_hover_tracking', { active: false }).catch(() => {})
    }
    return () => {
      invoke('set_efficiency_hover_tracking', { active: false }).catch(() => {})
    }
  }, [viewMode, moveMode])

  useEffect(() => {
    if (viewMode !== 'efficiency') return
    const unlisten = listen<boolean>('efficiency-hover', (event) => {
      if (event.payload) {
        // Cursor entered the notch / panel region.
        // Cancel any pending auto-close timer first.
        if (hoverCloseTimerRef.current) {
          clearTimeout(hoverCloseTimerRef.current)
          hoverCloseTimerRef.current = null
        }
        // If collapsed, not in a transition, and not in drag-move mode, expand via hover.
        if (!expandedRef.current && !collapsingRef.current && !expandingRef.current && !moveModeRef.current) {
          hoverExpandedRef.current = true
          expandFnRef.current?.()
        }
      } else {
        // Cursor left the region.  If the panel was hover-opened (and not pinned),
        // schedule auto-close after a short grace period.
        if (expandedRef.current && hoverExpandedRef.current && !pinnedRef.current) {
          hoverCloseTimerRef.current = setTimeout(() => {
            hoverExpandedRef.current = false
            hoverCloseTimerRef.current = null
            collapse()
          }, 300)
        }
      }
    })
    return () => {
      unlisten.then((fn) => fn())
    }
  }, [viewMode, collapse])

  const enterSettings = useCallback(async () => {
    if (settingsModeRef.current || settingsTransitioningRef.current) return
    // Entering settings is an intentional action — disable hover auto-close
    // so the panel stays open when the mouse leaves the window.
    hoverExpandedRef.current = false
    settingsTransitioningRef.current = true
    setSelectedAgentId(null)
    setSelectedClaudeSession(null)
    setSelectedSessionKey(null)
    setShowClaudeStats(false)
    setShowSettingsOverlay(false)
    setSettingsTransitioning(true)
    await new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())))
    try {
      await invoke('set_mini_size', { restore: false, position: mascotPositionRef.current })
    } catch {}
    settingsModeRef.current = true
    setSettingsMode(true)
    setShowSettingsOverlay(true)
    setSettingsTransitioning(false)
    settingsTransitioningRef.current = false
  }, [])

  const exitSettings = useCallback(async () => {
    if (!settingsModeRef.current || settingsTransitioningRef.current) return
    settingsTransitioningRef.current = true
    setShowSettingsOverlay(false)
    await new Promise<void>((r) => setTimeout(r, 220))
    setSettingsTransitioning(true)
    settingsModeRef.current = false
    setSettingsMode(false)
    setSettingsNav('pairing')
    // Re-sync feature toggles from store
    const store = await load('settings.json', { defaults: {}, autoSave: true })
    const cc = await store.get('enable_claudecode')
    setEnableClaudeCode(cc !== false)
    const cod = await store.get('enable_codex')
    setEnableCodex(cod !== false)
    const cur = await store.get('enable_cursor')
    setEnableCursor(cur !== false)
    fetchAgents()
    try {
      await invoke('set_mini_expanded', {
        expanded: true,
        position: mascotPositionRef.current,
        efficiency: viewModeRef.current === 'efficiency',
        maxHeight: panelMaxHeightRef.current,
      })
    } catch {}
    setSettingsTransitioning(false)
    settingsTransitioningRef.current = false
  }, [fetchAgents])

  // Click outside to collapse (only when not pinned)
  useEffect(() => {
    if (!expanded || pinned || settingsMode || settingsTransitioning) return
    const onClick = (e: MouseEvent) => {
      if (isCreateModalOpenRef.current) return
      if (!(e.target as HTMLElement).closest('#mini-panel')) collapse()
    }
    window.addEventListener('mousedown', onClick)
    return () => window.removeEventListener('mousedown', onClick)
  }, [expanded, pinned, settingsMode, settingsTransitioning, collapse])

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
    const onFocus = () => {
      filePickerOpenRef.current = false
    }
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

  const claudeWaiting = claudeSessions.some((cs) => cs.status === 'waiting')
  const claudeCompacting = claudeSessions.some((cs) => cs.status === 'compacting')
  const claudeWorking = claudeSessions.some((cs) => cs.status === 'processing' || cs.status === 'tool_running')
  const hasWorking = anySessionActive || Object.values(healthMap).some(Boolean) || claudeWorking || claudeCompacting || claudeWaiting
  // Priority: waiting > compacting > working > idle
  const mainPetState: PetState = claudeWaiting ? 'waiting' : claudeCompacting ? 'compacting' : hasWorking ? 'working' : 'idle'
  const miniGif = getMiniGif(miniChar ?? undefined, mainPetState, true)
  const handleDeleteChar = useCallback(async (name: string) => {
    try {
      await invoke('delete_character_assets', { name })
      const chars = await loadCharacters()
      setCharacters(chars)
    } catch (e) {
      console.warn('delete char failed:', e)
    }
  }, [])
  const saveCharQueue = useCallback(async (queue: string[]) => {
    setCharQueue(queue)
    const store = await load('settings.json', { defaults: {}, autoSave: true })
    await store.set('char_queue', queue)
    await store.save()
  }, [])
  const [queuePickerOpen, setQueuePickerOpen] = useState(false)

  const inAgentDetail = selectedAgentId !== null
  const selectedAgent = agents.find((a) => a.id === selectedAgentId)

  // Panel dimensions — CSS uses fixed base sizes; on Windows high-DPI screens
  // the panel root applies `zoom: uiScale` so all content scales uniformly.
  const panelW = viewMode === 'efficiency' ? 575 : 475
  const panelRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!expanded || settingsMode || settingsTransitioning || !showPanel) return
    const el = panelRef.current
    if (!el) return
    const ro = new ResizeObserver((entries) => {
      const h = entries[0]?.contentRect.height
      if (h && h > 0) invoke('resize_mini_height', { height: Math.min(h * uiScale, panelMaxHeight * uiScale), maxHeight: panelMaxHeight }).catch(() => {})
    })
    ro.observe(el)
    return () => ro.disconnect()
  }, [expanded, settingsMode, settingsTransitioning, showPanel, uiScale, panelMaxHeight])

  return (
    <div
      style={{
        width: '100vw',
        height: '100vh',
        background: 'transparent',
        overflow: 'hidden',
        userSelect: 'none',
      }}
    >
      {/* Collapsed */}
      {!expanded && !hiding && (
        <div
          id="mini-panel"
          onMouseEnter={() => {
            // Hover expand disabled — efficiency mode only opens on click.
          }}
          style={{
            width: '100%',
            height: '100%',
            display: 'flex',
            alignItems: 'flex-start',
            justifyContent: 'center',
            background: viewMode === 'efficiency' ? 'rgba(0,0,0,0.01)' : undefined,
            pointerEvents: 'auto',
          }}
        >
          <div
            onPointerDown={handleMascotPointerDown}
            style={{
              position: 'relative',
              cursor: moveMode ? 'grab' : 'pointer',
              animation: moveMode ? 'movePulse 1.2s ease-in-out infinite' : 'none',
              ...(moveMode
                ? {
                    borderRadius: 12,
                    outline: '2px solid rgba(59,130,246,0.6)',
                    outlineOffset: -2,
                  }
                : {}),
            }}
          >
            {miniGif ? (
              disableSleepAnim && mainPetState === 'idle' ? (
                <FrozenImg src={miniGif} style={{ width: 43, height: 43, objectFit: 'contain' }} draggable={false} />
              ) : mainPetState === 'compacting' ? (
                <IntervalGif src={miniGif} style={{ width: 43, height: 43, objectFit: 'contain' }} />
              ) : (
                <img src={miniGif} alt="mini" style={{ width: 43, height: 43, objectFit: 'contain' }} draggable={false} />
              )
            ) : (
              <div
                style={{
                  width: 43,
                  height: 43,
                  borderRadius: 10,
                  background: 'rgba(0,0,0,0.3)',
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'center',
                  color: '#999',
                  fontSize: 16,
                }}
              >
                ?
              </div>
            )}
            <div
              style={{
                position: 'absolute',
                bottom: 0,
                right: 0,
                width: 8,
                height: 8,
                borderRadius: '50%',
                background: mainPetState === 'waiting' ? '#f59e0b' : hasWorking ? '#2ecc71' : '#777',
                border: '1.5px solid rgba(0,0,0,0.3)',
              }}
            />
          </div>
        </div>
      )}

      {/* Expanded panel */}
      {expanded && !settingsMode && !settingsTransitioning && (
        <div
          id="mini-panel"
          ref={panelRef}
          className="scrollbar-hidden"
          onMouseEnter={() => {
            if (hoverCloseTimerRef.current) {
              clearTimeout(hoverCloseTimerRef.current)
              hoverCloseTimerRef.current = null
            }
          }}
          onMouseLeave={() => {
            if (hoverExpandedRef.current && !pinnedRef.current) {
              hoverCloseTimerRef.current = setTimeout(() => {
                hoverExpandedRef.current = false
                hoverCloseTimerRef.current = null
                collapse()
              }, 300)
            }
          }}
          style={{
            position: 'absolute',
            top: 0,
            left: '50%',
            transform: 'translateX(-50%)',
            transformOrigin: 'top center',
            zoom: uiScale !== 1 ? uiScale : undefined,
            width: panelW,
            height: 'auto',
            maxHeight: panelMaxHeight,
            overflowY: 'hidden',
            overflowX: 'hidden',
            display: 'flex',
            flexDirection: 'column',
            background: '#18181c',
            clipPath: showPanel ? 'inset(0 0 0 0 round 0 0 24px 24px)' : 'inset(0 calc(50% - 30px) calc(100% + 200px) calc(50% - 30px) round 0 0 8px 8px)',
            boxShadow: showPanel ? '0 8px 32px rgba(0,0,0,0.8)' : '0 2px 8px rgba(0,0,0,0.3)',
            transition: showPanel ? 'clip-path 0.45s cubic-bezier(0.22, 1.2, 0.36, 1), box-shadow 0.3s ease' : 'clip-path 0.25s cubic-bezier(0.4, 0, 0, 1), box-shadow 0.15s ease',
          }}
        >
          {/* Top Control Bar — outside the transform wrapper so sticky works correctly */}
          <div
            className="flex items-center justify-between px-4 py-2.5 shrink-0 sticky top-0 z-20 bg-black text-white"
            style={{
              opacity: showPanel ? 1 : 0,
              transition: showPanel ? 'opacity 0.25s cubic-bezier(0.2, 1, 0.3, 1) 0.05s' : 'opacity 0.08s ease-out',
            }}
          >
            <div className="flex items-center gap-4 min-w-0 flex-1">
              {inAgentDetail || selectedClaudeSession || selectedSessionKey || showClaudeStats ? (
                <button
                  data-no-drag
                  onClick={(e) => {
                    e.stopPropagation()
                    setSelectedAgentId(null)
                    setSelectedClaudeSession(null)
                    setSelectedSessionKey(null)
                    setShowClaudeStats(false)
                  }}
                  className="text-slate-400 hover:text-slate-200 transition-colors"
                >
                  <span style={{ fontSize: 13 }}>&lsaquo;</span> {t('common.back')}
                </button>
              ) : (
                <button
                  data-no-drag
                  onClick={(e) => {
                    e.stopPropagation()
                    const next = !pinned
                    setPinned(next)
                    pinnedRef.current = next
                    // If pinning while hover-opened, upgrade to intentional open
                    // so mouse-leave won't auto-close.
                    if (next) hoverExpandedRef.current = false
                  }}
                  className={`transition-colors ${pinned ? 'text-[#F0D140]' : 'text-slate-400 hover:text-slate-200'}`}
                  title={pinned ? t('mini.unpin') : t('mini.pin')}
                >
                  <Pin className="w-4 h-4" strokeWidth={2.5} />
                </button>
              )}
              <button
                data-no-drag
                onClick={(e) => {
                  e.stopPropagation()
                  setViewMode((v) => (v === 'island' ? 'efficiency' : 'island'))
                }}
                className="text-slate-400 hover:text-[#F0D140] transition-colors"
                title={viewMode === 'island' ? t('mini.efficiencyMode') || 'Efficiency Mode' : t('mini.islandMode') || 'Island Mode'}
              >
                {viewMode === 'island' ? <Rows className="w-4 h-4" strokeWidth={2.5} /> : <PanelLeft className="w-4 h-4" strokeWidth={2.5} />}
              </button>
              <button
                data-no-drag
                onClick={async (e) => {
                  e.stopPropagation()
                  const allOn = soundEnabled || cursorSoundEnabled
                  const next = !allOn
                  setSoundEnabled(next)
                  setCursorSoundEnabled(next)
                  const store = await load('settings.json', { defaults: {}, autoSave: true })
                  await store.set('sound_enabled', next)
                  await store.set('cursor_sound_enabled', next)
                  await store.save()
                  if (next) {
                    if (notifySound === 'manbo') new Audio('/audio/manbo.m4a').play().catch(() => {})
                    else playDefaultSound()
                  }
                }}
                className={`transition-colors ${soundEnabled || cursorSoundEnabled ? 'text-slate-400 hover:text-[#F0D140]' : 'text-slate-600 hover:text-[#F0D140]'}`}
                title={soundEnabled || cursorSoundEnabled ? t('mini.soundOn') : t('mini.soundOff')}
              >
                {soundEnabled || cursorSoundEnabled ? <Bell className="w-4 h-4" strokeWidth={2.5} /> : <BellOff className="w-4 h-4" strokeWidth={2.5} />}
              </button>
            </div>
            <div className="flex items-center gap-4">
              <button
                data-no-drag
                onClick={(e) => {
                  e.stopPropagation()
                  enterMoveMode()
                }}
                className="text-slate-400 hover:text-[#F0D140] transition-colors"
                title={t('mini.moveMascot')}
              >
                <Move className="w-4 h-4" strokeWidth={2.5} />
              </button>
              <button
                data-no-drag
                onClick={(e) => {
                  e.stopPropagation()
                  enterSettings()
                }}
                className="text-slate-400 hover:text-[#F0D140] transition-colors"
                title={t('mini.settings')}
              >
                <Settings className="w-4 h-4" strokeWidth={2.5} />
              </button>
              <button
                data-no-drag
                onClick={(e) => {
                  e.stopPropagation()
                  window.blur()
                  collapse()
                }}
                className="text-slate-400 hover:text-rose-500 transition-colors ml-1"
              >
                <X className="w-4.5 h-4.5" strokeWidth={2.5} />
              </button>
            </div>
          </div>

          <div
            style={{
              position: 'relative',
              zIndex: 1,
              opacity: showPanel ? 1 : 0,
              transform: showPanel ? 'scale(1) translateY(0)' : 'scale(0.9) translateY(-20px)',
              filter: showPanel ? 'blur(0px)' : 'blur(8px)',
              transformOrigin: 'top center',
              transition: showPanel
                ? 'opacity 0.25s cubic-bezier(0.2, 1, 0.3, 1) 0.05s, transform 0.4s cubic-bezier(0.2, 1, 0.3, 1), filter 0.25s ease 0.05s'
                : 'opacity 0.08s ease-out, transform 0.08s ease-out, filter 0.08s ease-out',
              flex: 1,
              minHeight: 0,
              display: 'flex',
              flexDirection: 'column',
            }}
          >
            {/* ===== Normal content (always rendered when expanded) ===== */}
            <AnimatePresence mode="wait">
              {!inAgentDetail && !selectedClaudeSession && !selectedSessionKey && !showClaudeStats ? (
                viewMode === 'efficiency' ? (
                  /* ===== Efficiency Mode ===== */
                  <motion.div
                    key="efficiency-view"
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                    exit={{ opacity: 0 }}
                    transition={{ duration: 0.15 }}
                    style={{ position: 'relative', display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}
                  >
                    <div className="flex flex-col bg-black" style={{ flex: 1, minHeight: 0 }}>
                      <div className="overflow-y-auto scrollbar-hidden" style={{ maxHeight: panelMaxHeight - 60 }}>
                        <AnimatePresence mode="popLayout">
                          {(() => {
                            const unified: { type: 'oc'; data: MiniSessionInfo; active: boolean; updatedAt: number }[] = allSessions.map((s) => ({
                              type: 'oc' as const,
                              data: s,
                              active: s.active,
                              updatedAt: s.updatedAt,
                            }))
                            const filteredClaude = claudeSessions.filter((cs) => {
                              if (cs.source === 'cursor' && !enableCursor) return false
                              if (cs.source === 'codex' && !enableCodex) return false
                              if (cs.source !== 'cursor' && cs.source !== 'codex' && !enableClaudeCode) return false
                              return true
                            })
                            const claudeUnified = filteredClaude.map((cs, ci) => ({
                              type: 'claude' as const,
                              data: cs,
                              claudeIdx: ci,
                              active: cs.status === 'processing' || cs.status === 'tool_running',
                              updatedAt: cs.updatedAt || 0,
                            }))
                            // Sort: waiting first, then everything else by recency.
                            const getPriority = (item: (typeof unified)[0] | (typeof claudeUnified)[0]) => {
                              if (item.type === 'claude') {
                                const cs = item.data as any
                                if (cs.status === 'waiting') return 0
                              }
                              return 1
                            }
                            const allItems = [...unified, ...claudeUnified].sort((a, b) => {
                              const pa = getPriority(a),
                                pb = getPriority(b)
                              if (pa !== pb) return pa - pb
                              return b.updatedAt - a.updatedAt
                            })

                            if (allItems.length === 0) {
                              const trackingTargets = [
                                ...(agents.length > 0 ? ['OpenClaw'] : []),
                                ...(enableClaudeCode ? ['Claude Code'] : []),
                                ...(enableCodex ? ['Codex'] : []),
                                ...(enableCursor ? ['Cursor'] : []),
                              ]
                              return (
                                <motion.div initial={{ opacity: 0 }} animate={{ opacity: 1 }} className="text-center py-10 px-4 flex flex-col items-center gap-2.5">
                                  {trackingTargets.length > 0 && (
                                    <p className="text-slate-500 text-sm font-medium">
                                      {t('mini.startTracking', { targets: trackingTargets.join(' / ') })}
                                    </p>
                                  )}
                                </motion.div>
                              )
                            }

                            const agentSeqCount: Record<string, number> = {}
                            const formatTimeAgo = (ts: number) => {
                              if (!ts) return ''
                              const diff = Date.now() - ts
                              const mins = Math.floor(diff / 60000)
                              if (mins < 1) return '<1m'
                              if (mins < 60) return `${mins}m`
                              const hrs = Math.floor(mins / 60)
                              if (hrs < 24) return `${hrs}h`
                              return `${Math.floor(hrs / 24)}d`
                            }
                            const hasImportant = allItems.some((item) => {
                              if (item.type !== 'claude') return false
                              const cs = item.data as any
                              if (cs.status === 'waiting' && cs.source !== 'cursor') return true
                              if (!cs.status || cs.status === 'stopped') {
                                if (cs.lastResponse && completionSessionId === cs.sessionId) return true
                              }
                              return false
                            })
                            const isImportant = (item: (typeof allItems)[0]) => {
                              if (item.type !== 'claude') return false
                              const cs = item.data as any
                              if (cs.status === 'waiting' && cs.source !== 'cursor') return true
                              if (cs.lastResponse && completionSessionId === cs.sessionId) return true
                              return false
                            }
                            const visibleItems = (effListCollapsed && hasImportant)
                              ? allItems.filter((item) => isImportant(item))
                              : allItems
                            const hiddenCount = allItems.length - visibleItems.length
                            const elements: React.ReactNode[] = visibleItems.map((item, index) => {
                              if (item.type === 'oc') {
                                const s = item.data
                                const agent = agents.find((a) => a.id === s.agentId)
                                const seq = (agentSeqCount[s.agentId] = (agentSeqCount[s.agentId] || 0) + 1)
                                const agentName = `${agent?.identityEmoji || ''} ${agent?.identityName || s.agentId}`.trim()
                                const charName = agentCharMap[s.agentId]
                                const charMeta = characters.find((c) => c.name === charName)
                                const gif = charMeta ? getMiniGif(charMeta, s.active ? 'working' : 'idle') : undefined
                                const title = `${agentName} #${seq}`
                                const subtitle = s.lastUserMsg || ''
                                const timeAgo = formatTimeAgo(s.updatedAt)
                                const isWorking = s.active
                                return (
                                  <motion.div
                                    key={`list-oc-${s.agentId}-${s.key}`}
                                    layout
                                    initial={{ opacity: 0, x: -10 }}
                                    animate={{ opacity: 1, x: 0 }}
                                    exit={{ opacity: 0, filter: 'blur(4px)' }}
                                    transition={{ duration: 0.2, delay: index * 0.05 }}
                                    data-no-drag
                                    onClick={() => {
                                      const ch = (s.channel || '').toLowerCase()
                                      const appName =
                                        ch.includes('feishu') || ch.includes('lark')
                                          ? 'Lark'
                                          : ch.includes('telegram')
                                            ? 'Telegram'
                                            : ch.includes('discord')
                                              ? 'Discord'
                                              : ch.includes('slack')
                                                ? 'Slack'
                                                : ch.includes('wechat') || ch.includes('weixin')
                                                  ? 'WeChat'
                                                  : null
                                      if (appName) {
                                        invoke('activate_app', { appName }).catch((err: unknown) => console.warn('activate failed:', err))
                                      }
                                    }}
                                    className="group flex items-center gap-3 px-4 hover:bg-white/[0.04] transition-colors cursor-pointer"
                                    style={{ padding: '10px 16px' }}
                                  >
                                    {isWorking && (
                                      <div className="relative shrink-0 w-10 h-10 flex items-center justify-center">
                                        {gif ? (
                                          <img src={gif} alt="" className="w-10 h-10 object-contain" style={{ imageRendering: 'pixelated' }} draggable={false} />
                                        ) : (
                                          <span className="text-white/40 text-lg">{agent?.identityEmoji || '?'}</span>
                                        )}
                                        <div
                                          style={{
                                            position: 'absolute',
                                            bottom: 0,
                                            right: 0,
                                            width: 8,
                                            height: 8,
                                            borderRadius: '50%',
                                            background: '#2ecc71',
                                            border: '1.5px solid rgba(0,0,0,0.3)',
                                          }}
                                        />
                                      </div>
                                    )}
                                    {!isWorking && (
                                      <div className="shrink-0 flex items-center justify-center w-10">
                                        <span className="w-1 h-1 rounded-full bg-slate-600" />
                                      </div>
                                    )}
                                    <div className="flex min-w-0 flex-1 items-center gap-1.5">
                                      <span className={`text-[13px] font-bold shrink-0 ${isWorking ? 'text-white' : 'text-slate-300'}`}>{title}</span>
                                      {subtitle && <span className="text-[13px] font-normal text-slate-500 shrink-0">· {subtitle}</span>}
                                      {s.lastAssistantMsg && <span className="text-[11px] text-white/40 truncate">· {s.lastAssistantMsg}</span>}
                                    </div>
                                    <div className="flex items-center gap-2 shrink-0">
                                      {s.channel && <span className="text-[11px] px-2 py-0.5 rounded-md font-normal bg-[#27272a] text-slate-300">{s.channel}</span>}
                                      <div className="w-8 flex items-center justify-center">
                                        <span className="text-[11px] text-slate-500 font-normal group-hover:hidden">{timeAgo}</span>
                                        <button
                                          data-no-drag
                                          onClick={(e) => {
                                            e.stopPropagation()
                                            dismissedSessionsRef.current.set(`${s.agentId}:${s.key}`, s.updatedAt)
                                            setAllSessions((prev) => prev.filter((ss) => !(ss.agentId === s.agentId && ss.key === s.key)))
                                          }}
                                          className="hidden group-hover:flex items-center justify-center text-slate-600 hover:text-rose-500 transition-colors outline-none"
                                          title={t('mini.remove')}
                                        >
                                          <Trash2 className="w-4 h-4" strokeWidth={2} />
                                        </button>
                                      </div>
                                    </div>
                                  </motion.div>
                                )
                              } else {
                                const cs = item.data
                                const defaultProjectName = cs.cwd ? cs.cwd.split('/').pop() : 'unknown'
                                const projectName = sessionNicknames[cs.sessionId] || defaultProjectName
                                const isActive = item.active
                                const isWaiting = cs.status === 'waiting'
                                const isCompacting = cs.status === 'compacting'
                                const isWorking = isActive || isWaiting || isCompacting
                                const recentlyDone = !isWorking && cs.status === 'stopped' && cs.updatedAt && (Date.now() - cs.updatedAt < 5 * 60 * 1000)
                                const showCharGif = isWorking || recentlyDone
                                const ci = 'claudeIdx' in item ? (item as { claudeIdx: number }).claudeIdx : 0
                                const charMeta = characters.find((c) => c.name === charQueue[ci % charQueue.length])
                                const petState = isWaiting ? 'waiting' : isCompacting ? 'compacting' : isActive ? 'working' : 'idle'
                                const gif = charMeta ? getMiniGif(charMeta, petState) : undefined
                                const subtitle = cs.userPrompt || ''
                                const timeAgo = formatTimeAgo(cs.updatedAt || 0)
                                const isCursorSource = cs.source === 'cursor'
                                const isCodexSource = cs.source === 'codex'
                                const sourceLabel = isCursorSource ? 'Cursor' : isCodexSource ? 'Codex' : 'Claude'
                                const sourceBadgeClass = isCursorSource
                                  ? 'bg-[#1a2f3f] text-[#5eb5f7]'
                                  : isCodexSource
                                    ? 'bg-[#1d2f26] text-[#6dd29c]'
                                    : 'bg-[#3f211d] text-[#e87a65]'
                                return (
                                  <motion.div
                                    key={`list-claude-${cs.sessionId}`}
                                    layout
                                    initial={{ opacity: 0, x: -10 }}
                                    animate={{ opacity: 1, x: 0 }}
                                    exit={{ opacity: 0, filter: 'blur(4px)' }}
                                    transition={{ duration: 0.2, delay: index * 0.05 }}
                                    data-no-drag
                                    onClick={() => {
                                      if (!isWaiting) {
                                        if (cs.source === 'cursor') {
                                          invoke('focus_cursor_terminal', { sessionId: cs.sessionId }).catch((err: unknown) => console.warn('focus cursor failed:', err))
                                        } else {
                                          invoke('jump_to_claude_terminal', { sessionId: cs.sessionId }).catch((err: unknown) => console.warn('jump failed:', err))
                                        }
                                      }
                                    }}
                                    className={`group hover:bg-white/[0.04] transition-colors ${isWaiting ? '' : 'cursor-pointer'}`}
                                    style={{ padding: '10px 16px' }}
                                  >
                                    <div className="flex items-center gap-3">
                                      {showCharGif && (
                                        <div className="relative shrink-0 w-10 h-10 flex items-center justify-center">
                                          {gif ? (
                                            <img src={gif} alt="" className="w-10 h-10 object-contain" style={{ imageRendering: 'pixelated' }} draggable={false} />
                                          ) : (
                                            <span className="text-white/40 text-lg">🤖</span>
                                          )}
                                          <div
                                            style={{
                                              position: 'absolute',
                                              bottom: 0,
                                              right: 0,
                                              width: 8,
                                              height: 8,
                                              borderRadius: '50%',
                                              background: isWaiting ? '#f59e0b' : recentlyDone ? '#94a3b8' : '#2ecc71',
                                              border: '1.5px solid rgba(0,0,0,0.3)',
                                            }}
                                          />
                                        </div>
                                      )}
                                      {!showCharGif && (
                                        <div className="shrink-0 flex items-center justify-center w-10">
                                          <span className="w-1 h-1 rounded-full bg-slate-600" />
                                        </div>
                                      )}
                                      <div
                                        className="flex min-w-0 flex-1 items-center gap-1.5"
                                        data-no-drag
                                      >
                                        {editingSessionTitle === cs.sessionId ? (
                                          <input
                                            autoFocus
                                            data-no-drag
                                            className="text-[13px] font-bold bg-transparent border-b border-slate-500 outline-none text-white w-24"
                                            defaultValue={projectName}
                                            ref={(el) => {
                                              if (el) {
                                                editingTitleValueRef.current = el.value
                                                editingTitleDefaultRef.current = defaultProjectName
                                              }
                                            }}
                                            onChange={(e) => { editingTitleValueRef.current = e.target.value }}
                                            onBlur={() => {
                                              saveSessionNickname(cs.sessionId, editingTitleValueRef.current, defaultProjectName)
                                              setEditingSessionTitle(null)
                                            }}
                                            onKeyDown={(e) => {
                                              if (e.key === 'Enter') (e.target as HTMLInputElement).blur()
                                              if (e.key === 'Escape') setEditingSessionTitle(null)
                                            }}
                                            onClick={(e) => e.stopPropagation()}
                                          />
                                        ) : (
                                          <span
                                            className={`text-[13px] font-bold shrink-0 cursor-text ${isWorking ? 'text-white' : 'text-slate-300'}`}
                                            onClick={(e) => {
                                              // Keep title area reserved for rename interaction.
                                              // Clicking title should not trigger jump.
                                              e.stopPropagation()
                                            }}
                                            onDoubleClick={(e) => {
                                              e.stopPropagation()
                                              setEditingSessionTitle(cs.sessionId)
                                            }}
                                          >
                                            {projectName}
                                          </span>
                                        )}
                                        {subtitle && <span className="text-[13px] font-normal text-slate-500 truncate">· {subtitle}</span>}
                                      </div>
                                      <div className="flex items-center gap-2 shrink-0">
                                        <span className={`text-[11px] px-2 py-0.5 rounded-md font-normal ${sourceBadgeClass}`}>
                                          {sourceLabel}
                                        </span>
                                        <div className="w-8 flex items-center justify-center">
                                          <span className="text-[11px] text-slate-500 font-normal group-hover:hidden">{timeAgo}</span>
                                          <button
                                            data-no-drag
                                            onClick={(e) => {
                                              e.stopPropagation()
                                              invoke('remove_claude_session', { sessionId: cs.sessionId }).catch(() => {})
                                              setClaudeSessions((prev) => prev.filter((s) => s.sessionId !== cs.sessionId))
                                            }}
                                            className="hidden group-hover:flex items-center justify-center text-slate-600 hover:text-rose-500 transition-colors outline-none"
                                            title={t('mini.remove')}
                                          >
                                            <Trash2 className="w-4 h-4" strokeWidth={2} />
                                          </button>
                                        </div>
                                      </div>
                                    </div>
                                    {/* ── 提醒弹窗 (Reminder Popup) ──
                                       在效率模式下，当 CC session 需要用户批准
                                       (PermissionRequest → isWaiting) 且该 session
                                       对应的终端 tab 不在当前激活状态时，自动弹出
                                       此面板。包含四个操作按钮：拒绝、允许一次、
                                       全部允许、自动批准。
                                       用途：让用户无需切换到终端即可快速处理权限请求。 */}
                                    {isWaiting && cs.source !== 'cursor' && (
                                      <div className="mt-2">
                                        {cs.tool && (
                                          <div className="flex items-center gap-1.5 mb-2">
                                            <span className="text-amber-400 text-[12px]">⚠</span>
                                            <span className="text-amber-400 text-[12px] font-bold">{cs.tool}</span>
                                          </div>
                                        )}
                                        {cs.toolInput &&
                                          (() => {
                                            try {
                                              const input = JSON.parse(cs.toolInput)
                                              // Write/Edit: show file name + numbered code lines
                                              if ((cs.tool === 'Write' || cs.tool === 'Edit') && (input.file_path || input.content)) {
                                                const fileName = input.file_path ? input.file_path.split('/').pop() : ''
                                                const isNew = cs.tool === 'Write'
                                                const content = input.content || input.new_string || input.old_string || ''
                                                const lines = content.split('\n')
                                                return (
                                                  <div className="mb-2 rounded-lg bg-[#1a1a1e] border border-[#2a2a2e] overflow-hidden">
                                                    {fileName && (
                                                      <div className="flex items-center gap-2 px-3 py-1.5 border-b border-[#2a2a2e] sticky top-0 bg-[#1a1a1e] z-10">
                                                        <span className="text-[12px] text-slate-300 font-mono">{fileName}</span>
                                                        {isNew && <span className="text-[10px] px-1.5 py-0.5 rounded bg-emerald-900/50 text-emerald-400">{t('mini.newFile', '新文件')}</span>}
                                                      </div>
                                                    )}
                                                    <div className="px-3 py-2 max-h-[120px] overflow-y-auto scrollbar-thin">
                                                      {lines.map((line: string, i: number) => (
                                                        <div key={i} className="flex gap-3 leading-[1.6]">
                                                          <span className="text-[11px] text-slate-600 font-mono select-none w-5 text-right shrink-0">{i + 1}</span>
                                                          <pre className="text-[11px] text-slate-300 font-mono whitespace-pre-wrap break-all">{line || ' '}</pre>
                                                        </div>
                                                      ))}
                                                    </div>
                                                  </div>
                                                )
                                              }
                                              if (typeof input.justification === 'string' && input.justification.trim()) {
                                                return (
                                                  <div className="mb-2 p-2.5 rounded-lg bg-[#1a1a1e] border border-[#2a2a2e] max-h-[120px] overflow-auto">
                                                    <pre className="text-[11px] text-amber-300 font-mono whitespace-pre-wrap break-all leading-tight">{input.justification}</pre>
                                                  </div>
                                                )
                                              }
                                              // Bash: show command
                                              if (cs.tool === 'Bash' && input.command) {
                                                return (
                                                  <div className="mb-2 p-2.5 rounded-lg bg-[#1a1a1e] border border-[#2a2a2e] max-h-[120px] overflow-auto">
                                                    <pre className="text-[11px] text-slate-300 font-mono whitespace-pre-wrap break-all leading-tight">{input.command}</pre>
                                                  </div>
                                                )
                                              }
                                              // Fallback: show parsed fields
                                              const preview = input.command || input.file_path || input.content?.slice(0, 150) || cs.toolInput.slice(0, 150)
                                              return (
                                                <div className="mb-2 p-2.5 rounded-lg bg-[#1a1a1e] border border-[#2a2a2e] max-h-[120px] overflow-auto">
                                                  <pre className="text-[11px] text-slate-400 font-mono whitespace-pre-wrap break-all leading-tight">{preview}</pre>
                                                </div>
                                              )
                                            } catch {
                                              return (
                                                <div className="mb-2 p-2.5 rounded-lg bg-[#1a1a1e] border border-[#2a2a2e] max-h-[120px] overflow-auto">
                                                  <pre className="text-[11px] text-slate-400 font-mono whitespace-pre-wrap break-all leading-tight">{cs.toolInput.slice(0, 150)}</pre>
                                                </div>
                                              )
                                            }
                                          })()}
                                        <div className="flex gap-2">
                                          {(() => {
                                            // Immediately clear the waiting state locally so
                                            // the permission popup closes without waiting for
                                            // the next 2s poll cycle.
                                            const resolvePermission = (decision: string) => {
                                              invoke('resolve_claude_permission', { sessionId: cs.sessionId, decision }).catch(() => {})
                                              // Clear waiting state locally so popup disappears instantly
                                              setClaudeSessions((prev) => prev.map((s) => (s.sessionId === cs.sessionId ? { ...s, status: 'processing', tool: undefined, toolInput: undefined } : s)))
                                              // Collapse the panel
                                              hoverExpandedRef.current = false
                                              collapse()
                                            }
                                            if (cs.source === 'codex') {
                                              return (
                                                <>
                                                  <button
                                                    data-no-drag
                                                    onClick={(e) => {
                                                      e.stopPropagation()
                                                      invoke('jump_to_claude_terminal', { sessionId: cs.sessionId }).catch(() => {})
                                                      hoverExpandedRef.current = false
                                                      collapse()
                                                    }}
                                                    className="flex-1 py-1.5 rounded-md text-[12px] font-normal bg-[#27272a] text-slate-300 hover:bg-[#303033] transition-colors"
                                                  >
                                                    {t('mini.viewInCodex', '前往 Codex')}
                                                  </button>
                                                  <button
                                                    data-no-drag
                                                    onClick={(e) => {
                                                      e.stopPropagation()
                                                      hoverExpandedRef.current = false
                                                      collapse()
                                                    }}
                                                    className="flex-1 py-1.5 rounded-md text-[12px] font-normal bg-[#27272a] text-slate-300 hover:bg-[#303033] transition-colors"
                                                  >
                                                    {t('mini.later', '稍后处理')}
                                                  </button>
                                                </>
                                              )
                                            }
                                            return (
                                              <>
                                                <button
                                                  data-no-drag
                                                  onClick={(e) => {
                                                    e.stopPropagation()
                                                    resolvePermission('deny')
                                                  }}
                                                  className="flex-1 py-1.5 rounded-md text-[12px] font-normal bg-[#27272a] text-slate-300 hover:bg-[#303033] transition-colors"
                                                >
                                                  {t('mini.deny', '拒绝')}
                                                </button>
                                                <button
                                                  data-no-drag
                                                  onClick={(e) => {
                                                    e.stopPropagation()
                                                    resolvePermission('allow_once')
                                                  }}
                                                  className="flex-1 py-1.5 rounded-md text-[12px] font-normal bg-[#27272a] text-slate-300 hover:bg-[#303033] transition-colors"
                                                >
                                                  {t('mini.allowOnce', '允许一次')}
                                                </button>
                                                <button
                                                  data-no-drag
                                                  onClick={(e) => {
                                                    e.stopPropagation()
                                                    resolvePermission('allow_all')
                                                  }}
                                                  className="flex-1 py-1.5 rounded-md text-[12px] font-normal bg-emerald-900/50 text-emerald-300 hover:bg-emerald-800/50 transition-colors"
                                                >
                                                  {t('mini.allowAll', '全部允许')}
                                                </button>
                                                <button
                                                  data-no-drag
                                                  onClick={(e) => {
                                                    e.stopPropagation()
                                                    resolvePermission('auto_approve')
                                                  }}
                                                  className="flex-1 py-1.5 rounded-md text-[12px] font-normal bg-rose-900/50 text-rose-300 hover:bg-rose-800/50 transition-colors"
                                                >
                                                  {t('mini.autoApprove', '自动批准')}
                                                </button>
                                              </>
                                            )
                                          })()}
                                        </div>
                                      </div>
                                    )}
                                    {/* ── 完成提醒弹窗 (Completion Reminder) ──
                                       任务完成且终端未激活时，显示用户问题和 AI 回复预览，
                                       点击跳转到对应终端。
                                       只有刚完成的 session 才展开弹窗，其余已完成的只显示标题行。 */}
                                    {!isWaiting && !isWorking && cs.lastResponse && completionSessionId === cs.sessionId && (
                                      <div
                                        data-no-drag
                                        className="mt-2 rounded-lg bg-[#1a1a1e] border border-[#2a2a2e] overflow-hidden"
                                      >
                                        <div
                                          className="flex items-center justify-between px-3 py-2 border-b border-[#2a2a2e] cursor-pointer hover:bg-[#222226] transition-colors"
                                          onClick={(e) => {
                                            e.stopPropagation()
                                            setCompletionSessionId(null)
                                            if (cs.source === 'cursor') {
                                              invoke('focus_cursor_terminal', { sessionId: cs.sessionId }).catch((err: unknown) => console.warn('focus cursor failed:', err))
                                            } else {
                                              invoke('jump_to_claude_terminal', { sessionId: cs.sessionId }).catch(() => {})
                                            }
                                          }}
                                        >
                                          <span className="text-[12px] text-slate-300 truncate">
                                            {cs.userPrompt ? (
                                              <><span className="text-slate-500">{t('mini.you', '你')}：</span>{cs.userPrompt}</>
                                            ) : (
                                              <span className="text-slate-500">{t('mini.taskCompleted', 'Task completed')}</span>
                                            )}
                                          </span>
                                          <span className="text-[11px] px-1.5 py-0.5 rounded bg-emerald-900/50 text-emerald-400 shrink-0 ml-2">{t('mini.done', '完成')}</span>
                                        </div>
                                        <div className="px-3 py-2 max-h-[160px] overflow-y-auto scrollbar-thin text-[12px] text-slate-400 leading-[1.6] markdown-content">
                                          {(cs.source === 'cursor' || cs.source === 'codex') && cs.lastResponse === '✓' ? (
                                            <p>
                                              {cs.source === 'codex'
                                                ? t('mini.codeDone', 'Code has finished working. Click to view.')
                                                : t('mini.cursorDone', 'Cursor has finished working. Click to view.')}
                                            </p>
                                          ) : (
                                            <ReactMarkdown>{cs.lastResponse}</ReactMarkdown>
                                          )}
                                        </div>
                                      </div>
                                    )}
                                  </motion.div>
                                )
                              }
                            })
                            if (hiddenCount > 0) {
                              elements.push(
                                <motion.div
                                  key="expand-list-btn"
                                  layout
                                  initial={{ opacity: 0 }}
                                  animate={{ opacity: 1 }}
                                  exit={{ opacity: 0 }}
                                  className="flex justify-center py-2"
                                >
                                  <button
                                    data-no-drag
                                    onClick={(e) => {
                                      e.stopPropagation()
                                      setEffListCollapsed(false)
                                    }}
                                    className="text-[11px] text-slate-500 hover:text-slate-300 transition-colors"
                                  >
                                    {t('mini.showMore', 'Show {{count}} more', { count: hiddenCount })}
                                  </button>
                                </motion.div>
                              )
                            } else if (!effListCollapsed && hasImportant && allItems.length > 1) {
                              elements.push(
                                <motion.div
                                  key="collapse-list-btn"
                                  layout
                                  initial={{ opacity: 0 }}
                                  animate={{ opacity: 1 }}
                                  exit={{ opacity: 0 }}
                                  className="flex justify-center py-2"
                                >
                                  <button
                                    data-no-drag
                                    onClick={(e) => {
                                      e.stopPropagation()
                                      setEffListCollapsed(true)
                                    }}
                                    className="text-[11px] text-slate-500 hover:text-slate-300 transition-colors"
                                  >
                                    {t('mini.collapse', 'Collapse')}
                                  </button>
                                </motion.div>
                              )
                            }
                            return elements
                          })()}
                        </AnimatePresence>
                      </div>
                      {/* Footer */}
                      <div className="mt-auto py-0.5 flex justify-center items-center select-none opacity-30 hover:opacity-60 transition-opacity">
                        <span
                          data-no-drag
                          onClick={() => invoke('open_url', { url: 'https://github.com/rainnoon/oc-claw' })}
                          className="text-[8px] font-bold tracking-[0.2em] text-slate-500 uppercase cursor-pointer"
                        >
                          oc–claw
                        </span>
                      </div>
                    </div>
                  </motion.div>
                ) : (
                  /* ===== Normal: character island + sessions ===== */
                  <motion.div
                    key="main"
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                    exit={{ opacity: 0 }}
                    transition={{ duration: 0.15 }}
                    style={{ position: 'relative', display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}
                  >
                    {/* Loading overlay while refreshing connections */}
                    <AnimatePresence>
                      {refreshingAgents && (
                        <motion.div
                          initial={{ opacity: 0 }}
                          animate={{ opacity: 1 }}
                          exit={{ opacity: 0 }}
                          transition={{ duration: 0.15 }}
                          style={{
                            position: 'absolute',
                            inset: 0,
                            zIndex: 10,
                            background: 'rgba(15,15,19,0.9)',
                            display: 'flex',
                            flexDirection: 'column',
                            alignItems: 'center',
                            justifyContent: 'center',
                            gap: 8,
                          }}
                        >
                          <Loader2 className="w-5 h-5 animate-spin" style={{ color: 'rgba(255,255,255,0.4)' }} />
                          <span style={{ color: 'rgba(255,255,255,0.35)', fontSize: 11 }}>{t('mini.connecting')}</span>
                        </motion.div>
                      )}
                    </AnimatePresence>
                    {/* Banner Area */}
                    <div
                      className="border-b-[3px] border-black relative overflow-hidden select-none"
                      style={{
                        height: 125,
                        flexShrink: 0,
                        ...(islandBg === '__anime__'
                          ? {
                              background: '#F0D140',
                            }
                          : {
                              backgroundImage: `url(/assets/backgrounds/${islandBg})`,
                              backgroundSize: 'cover',
                              backgroundPosition: `${bgPos.x}% ${bgPos.y}%`,
                            }),
                      }}
                    >
                      {islandBg === '__anime__' && (
                        <>
                          <div className="absolute inset-0 bg-[linear-gradient(to_right,#00000015_2px,transparent_2px),linear-gradient(to_bottom,#00000015_2px,transparent_2px)] bg-[size:16px_16px]" />
                          <motion.div
                            animate={{ x: [-80, panelW + 80] }}
                            transition={{ repeat: Infinity, duration: 18, ease: 'linear' }}
                            className="absolute top-1 left-0 text-black p-4 -m-4"
                            style={{ filter: 'drop-shadow(2px 2px 0px #000)' }}
                          >
                            <Cloud className="w-12 h-12 fill-white" strokeWidth={2} style={{ overflow: 'visible' }} />
                          </motion.div>
                          <motion.div
                            animate={{ x: [-60, panelW + 60] }}
                            transition={{ repeat: Infinity, duration: 25, ease: 'linear', delay: 4 }}
                            className="absolute top-10 left-0 text-black p-4 -m-4"
                            style={{ filter: 'drop-shadow(2px 2px 0px #000)' }}
                          >
                            <Cloud className="w-8 h-8 fill-white" strokeWidth={2} style={{ overflow: 'visible' }} />
                          </motion.div>
                        </>
                      )}

                      {sessionSlots.length === 0 &&
                        (() => {
                          const emptyGif = getMiniGif(miniChar ?? undefined, 'idle', true)
                          return (
                            <div
                              style={{
                                position: 'absolute',
                                inset: 0,
                                display: 'flex',
                                alignItems: 'center',
                                justifyContent: 'center',
                                zIndex: 2,
                              }}
                            >
                              {emptyGif ? (
                                <img
                                  src={emptyGif}
                                  style={{
                                    width: 68,
                                    height: 68,
                                    objectFit: 'contain',
                                    animation: 'bob 2s ease-in-out infinite',
                                    opacity: 0.8,
                                  }}
                                  draggable={false}
                                />
                              ) : (
                                <span style={{ color: 'rgba(255,255,255,0.3)', fontSize: 11 }}>{t('mini.waitingForAgents')}</span>
                              )}
                            </div>
                          )
                        })()}

                      {(() => {
                        const shuffled = sessionSlots
                          .map((slot, idx) => {
                            const seed = (slot.agentId + slot.sessionIdx).split('').reduce((a: number, c: string) => a + c.charCodeAt(0), 0)
                            return { slot, idx, seed }
                          })
                          .sort((a, b) => ((a.seed * 7 + 13) % 97) - ((b.seed * 7 + 13) % 97))

                        return shuffled.map(({ slot, seed }, sortedIdx) => {
                          const gif = getMiniGif(slot.char, slot.petState ?? (slot.isWorking ? 'working' : 'idle'), true)
                          const singleRow = sessionSlots.length <= 6
                          const row = sortedIdx < 6 ? 0 : 1
                          const col = row === 0 ? sortedIdx : sortedIdx - 6
                          const cols = row === 0 ? Math.min(sessionSlots.length, 6) : Math.min(sessionSlots.length - 6, 4)
                          const slotW = 475 / Math.max(cols, 1)
                          const xBase = slotW * col + slotW / 2 - 28 + (row === 1 ? slotW * 0.4 : 0)
                          const yBase = row === 0 ? (singleRow ? 16 : 10) : 64
                          const jx = ((seed * 7) % 17) - 8
                          const jy = singleRow ? ((seed * 11) % 45) - 22 : ((seed * 11) % 11) - 5
                          const x = Math.max(2, Math.min(415, xBase + jx))
                          const y = yBase + jy
                          return (
                            <div
                              key={`${slot.agentId}-${slot.sessionIdx}`}
                              data-no-drag
                              onClick={() => {
                                if (slot.agentId.startsWith('claude:')) {
                                  setSelectedAgentId(null)
                                  setSelectedSessionKey(null)
                                  setSelectedClaudeSession(null)
                                  setShowClaudeStats(true)
                                } else {
                                  setSelectedClaudeSession(null)
                                  setSelectedSessionKey(null)
                                  setSelectedAgentId(slot.agentId)
                                }
                              }}
                              style={{
                                position: 'absolute',
                                left: x,
                                top: y,
                                display: 'flex',
                                flexDirection: 'column',
                                alignItems: 'center',
                                cursor: 'pointer',
                                zIndex: 2,
                                animation: 'bob 1.6s ease-in-out infinite',
                                animationDelay: `${sortedIdx * -0.3}s`,
                              }}
                            >
                              <div style={{ position: 'relative' }}>
                                {gif ? (
                                  <img src={gif} alt={slot.char?.name} style={{ width: 56, height: 56, objectFit: 'contain' }} draggable={false} />
                                ) : (
                                  <div
                                    style={{
                                      width: 56,
                                      height: 56,
                                      borderRadius: 8,
                                      background: 'rgba(255,255,255,0.1)',
                                      display: 'flex',
                                      alignItems: 'center',
                                      justifyContent: 'center',
                                      color: '#555',
                                      fontSize: 13,
                                    }}
                                  >
                                    ?
                                  </div>
                                )}
                              </div>
                            </div>
                          )
                        })
                      })()}
                    </div>

                    {/* Task List */}
                    <div className="p-2 bg-[#0f0f13] flex flex-col gap-0.5" style={{ flex: 1, minHeight: 0, overflow: 'hidden' }}>
                      {allSessions.length === 0 && claudeSessions.length === 0 && !refreshingAgents && (
                        <motion.div initial={{ opacity: 0 }} animate={{ opacity: 1 }} className="text-center py-10 px-4 flex flex-col items-center gap-2.5">
                          {(() => {
                            const targets = [
                              ...(agents.length > 0 ? ['OpenClaw'] : []),
                              ...(enableClaudeCode ? ['Claude Code'] : []),
                              ...(enableCodex ? ['Codex'] : []),
                              ...(enableCursor ? ['Cursor'] : []),
                            ]
                            return targets.length > 0 ? (
                              <p className="text-slate-500 text-sm font-medium">
                                {t('mini.startTracking', { targets: targets.join(' / ') })}
                              </p>
                            ) : null
                          })()}
                          <button
                            data-no-drag
                            onClick={(e) => {
                              e.stopPropagation()
                              enterSettings()
                            }}
                            className="text-slate-400 text-sm font-medium underline decoration-slate-500 underline-offset-4 hover:text-slate-200 transition-colors"
                          >
                            {t('mini.goToSettings')}
                          </button>
                        </motion.div>
                      )}

                      <div className="scrollbar-hidden" style={{ flex: 1, minHeight: 0, overflowY: 'auto' }}>
                        <AnimatePresence mode="popLayout">
                          {(() => {
                            const unified: { type: 'oc'; data: MiniSessionInfo; active: boolean; updatedAt: number }[] = allSessions.map((s) => ({
                              type: 'oc' as const,
                              data: s,
                              active: s.active,
                              updatedAt: s.updatedAt,
                            }))
                            const filteredClaude = claudeSessions.filter((cs) => {
                              if (cs.source === 'cursor' && !enableCursor) return false
                              if (cs.source === 'codex' && !enableCodex) return false
                              if (cs.source !== 'cursor' && cs.source !== 'codex' && !enableClaudeCode) return false
                              return true
                            })
                            const claudeUnified = filteredClaude.map((cs, ci) => ({
                              type: 'claude' as const,
                              data: cs,
                              claudeIdx: ci,
                              active: cs.status === 'processing' || cs.status === 'tool_running',
                              updatedAt: cs.updatedAt || 0,
                            }))
                            const merged = [...unified, ...claudeUnified].sort((a, b) => (b.active ? 1 : 0) - (a.active ? 1 : 0) || b.updatedAt - a.updatedAt)

                            const agentSeqCount: Record<string, number> = {}
                            return merged.map((item, index) => {
                              if (item.type === 'oc') {
                                const s = item.data
                                const agent = agents.find((a) => a.id === s.agentId)
                                const seq = (agentSeqCount[s.agentId] = (agentSeqCount[s.agentId] || 0) + 1)
                                const agentName = `${agent?.identityEmoji || ''} ${agent?.identityName || s.agentId}`.trim()
                                const label = `${agentName} #${seq}${s.lastUserMsg ? ` - ${s.lastUserMsg}` : ''}`
                                return (
                                  <motion.div
                                    key={`oc-${s.agentId}-${s.key}`}
                                    layout
                                    initial={{ opacity: 0, x: -10 }}
                                    animate={{ opacity: 1, x: 0 }}
                                    exit={{ opacity: 0, filter: 'blur(4px)' }}
                                    transition={{ duration: 0.2, delay: index * 0.05 }}
                                    data-no-drag
                                    onClick={() => {
                                      setSelectedClaudeSession(null)
                                      setSelectedAgentId(null)
                                      setSelectedSessionKey({ agentId: s.agentId, key: s.key })
                                    }}
                                    className="group flex items-center gap-3 px-3 py-2.5 rounded-lg hover:bg-white/[0.04] transition-colors cursor-pointer"
                                  >
                                    <div className="shrink-0 flex items-center justify-center w-4 h-4">
                                      {s.active ? (
                                        <Asterisk className="w-4 h-4 text-emerald-400 animate-[spin_4s_linear_infinite]" strokeWidth={2.5} />
                                      ) : (
                                        <span className="w-1 h-1 rounded-full bg-slate-600" />
                                      )}
                                    </div>
                                    <div className="flex items-baseline gap-2 min-w-0 flex-1">
                                      <span className={`text-sm font-bold tracking-wide truncate ${s.active ? 'text-slate-200' : 'text-slate-400'}`}>{label}</span>
                                    </div>
                                    <button
                                      data-no-drag
                                      onClick={(e) => {
                                        e.stopPropagation()
                                        dismissedSessionsRef.current.set(`${s.agentId}:${s.key}`, s.updatedAt)
                                        setAllSessions((prev) => prev.filter((ss) => !(ss.agentId === s.agentId && ss.key === s.key)))
                                      }}
                                      className="shrink-0 text-slate-600 hover:text-rose-500 transition-colors outline-none"
                                      title={t('mini.remove')}
                                    >
                                      <Trash2 className="w-4 h-4" strokeWidth={2} />
                                    </button>
                                  </motion.div>
                                )
                              } else {
                                const cs = item.data
                                const projectName = cs.cwd ? cs.cwd.split('/').pop() : 'unknown'
                                const isActive = item.active
                                const isWaiting = cs.status === 'waiting'
                                const statusText = cs.tool
                                  ? `🔧 ${cs.tool}`
                                  : cs.status === 'stopped'
                                    ? t('mini.idle')
                                    : cs.status === 'waiting'
                                      ? '⏳ ' + t('mini.waiting')
                                      : cs.status === 'processing'
                                        ? t('mini.thinking')
                                        : cs.status === 'tool_running'
                                          ? t('mini.working')
                                          : cs.status === 'compacting'
                                            ? t('mini.compacting')
                                            : cs.status
                                const label = `${projectName}${cs.userPrompt ? ` - ${cs.userPrompt}` : ` - ${statusText}`}`
                                return (
                                  <motion.div
                                    key={`claude-${cs.sessionId}`}
                                    layout
                                    initial={{ opacity: 0, x: -10 }}
                                    animate={{ opacity: 1, x: 0 }}
                                    exit={{ opacity: 0, filter: 'blur(4px)' }}
                                    transition={{ duration: 0.2, delay: index * 0.05 }}
                                    data-no-drag
                                    onClick={() => {
                                      setSelectedAgentId(null)
                                      setSelectedSessionKey(null)
                                      setSelectedClaudeSession(cs.sessionId)
                                    }}
                                    className="group flex items-center gap-3 px-3 py-2.5 rounded-lg hover:bg-white/[0.04] transition-colors cursor-pointer"
                                  >
                                    <div className="shrink-0 flex items-center justify-center w-4 h-4">
                                      {isActive || isWaiting ? (
                                        <Asterisk className={`w-4 h-4 animate-[spin_4s_linear_infinite] ${isWaiting ? 'text-amber-400' : 'text-emerald-400'}`} strokeWidth={2.5} />
                                      ) : (
                                        <span className="w-1 h-1 rounded-full bg-slate-600" />
                                      )}
                                    </div>
                                    <div className="flex items-baseline gap-2 min-w-0 flex-1">
                                      <span className={`text-sm font-bold tracking-wide truncate ${isActive || isWaiting ? 'text-slate-200' : 'text-slate-400'}`}>{label}</span>
                                    </div>
                                    <button
                                      data-no-drag
                                      onClick={(e) => {
                                        e.stopPropagation()
                                        invoke('remove_claude_session', { sessionId: cs.sessionId }).catch(() => {})
                                        setClaudeSessions((prev) => prev.filter((s) => s.sessionId !== cs.sessionId))
                                      }}
                                      className="shrink-0 text-slate-600 hover:text-rose-500 transition-colors outline-none"
                                      title={t('mini.remove')}
                                    >
                                      <Trash2 className="w-4 h-4" strokeWidth={2} />
                                    </button>
                                  </motion.div>
                                )
                              }
                            })
                          })()}
                        </AnimatePresence>
                      </div>

                      {/* Trademark / Footer */}
                      <div className="mt-auto pt-1.5 pb-1 flex justify-center items-center select-none">
                        <span
                          data-no-drag
                          onClick={() => invoke('open_url', { url: 'https://github.com/rainnoon/oc-claw' })}
                          className="text-[10px] font-black tracking-[0.25em] text-slate-500 uppercase cursor-pointer hover:text-slate-300 transition-colors"
                        >
                          oc–claw.ai
                        </span>
                      </div>
                    </div>
                  </motion.div>
                )
              ) : selectedSessionKey ? (
                /* ===== OpenClaw session chat ===== */
                <motion.div
                  key="oc-chat"
                  style={{ background: '#1a1a1a', display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}
                  initial={{ opacity: 0, x: 30 }}
                  animate={{ opacity: 1, x: 0 }}
                  exit={{ opacity: 0, x: -30 }}
                  transition={{ duration: 0.2, ease: [0.25, 0.1, 0.25, 1] }}
                >
                  {sessionMessages.length === 0 ? (
                    <div style={{ color: 'rgba(255,255,255,0.3)', fontSize: 11, textAlign: 'center', padding: '30px 0' }}>{t('common.loading')}</div>
                  ) : (
                    <ChatList messages={sessionMessages} accentColor="#2ecc71" />
                  )}
                </motion.div>
              ) : selectedClaudeSession ? (
                /* ===== Claude session chat ===== */
                <motion.div
                  key="claude-chat"
                  style={{ background: '#1a1a1a', display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}
                  initial={{ opacity: 0, x: 30 }}
                  animate={{ opacity: 1, x: 0 }}
                  exit={{ opacity: 0, x: -30 }}
                  transition={{ duration: 0.2, ease: [0.25, 0.1, 0.25, 1] }}
                >
                  {claudeConversation.length === 0 ? (
                    <div style={{ color: 'rgba(255,255,255,0.3)', fontSize: 11, textAlign: 'center', padding: '30px 0' }}>{t('common.loading')}</div>
                  ) : (
                    <ChatList messages={claudeConversation} accentColor="#007AFF" />
                  )}
                </motion.div>
              ) : showClaudeStats ? (
                /* ===== Claude Code stats ===== */
                <motion.div
                  key="claude-stats"
                  style={{ background: '#1a1a1a' }}
                  initial={{ opacity: 0, x: 30 }}
                  animate={{ opacity: 1, x: 0 }}
                  exit={{ opacity: 0, x: -30 }}
                  transition={{ duration: 0.2, ease: [0.25, 0.1, 0.25, 1] }}
                >
                  <ClaudeStatsView />
                </motion.div>
              ) : (
                /* ===== Agent detail panel (ui-2 style) ===== */
                <motion.div
                  key="agent-detail"
                  style={{ background: '#1a1a1a' }}
                  initial={{ opacity: 0, x: 30 }}
                  animate={{ opacity: 1, x: 0 }}
                  exit={{ opacity: 0, x: -30 }}
                  transition={{ duration: 0.2, ease: [0.25, 0.1, 0.25, 1] }}
                >
                  <AgentDetailView agent={selectedAgent} metrics={metrics} extraInfo={extraInfo} />
                </motion.div>
              )}
            </AnimatePresence>
          </div>
        </div>
      )}

      {/* ===== Settings overlay (independent fixed layer) ===== */}
      <AnimatePresence>
        {showSettingsOverlay && (
          <>
            <div
              data-no-drag
              onMouseDown={(e) => {
                if (e.target === e.currentTarget) exitSettings()
              }}
              style={{
                position: 'fixed',
                inset: 0,
                zIndex: 40,
              }}
            />
            <motion.div
              key="settings-overlay"
              data-no-drag
              className="scrollbar-hidden"
              variants={{
                hidden: { opacity: 0, scale: 0.96, y: -24, filter: 'blur(10px)' },
                visible: { opacity: 1, scale: 1, y: 0, filter: 'blur(0px)', transition: { type: 'spring', damping: 22, stiffness: 150, mass: 1.2 } },
                exit: { opacity: 0, scale: 0.96, y: -24, filter: 'blur(10px)', transition: { type: 'spring', damping: 25, stiffness: 300 } },
              }}
              initial="hidden"
              animate="visible"
              exit="exit"
              style={{
                position: 'fixed',
                top: 4,
                left: 12,
                right: 12,
                bottom: 12,
                zIndex: 50,
                background: '#0f0f13',
                border: '1px solid rgba(255,255,255,0.08)',
                borderRadius: 24,
                display: 'flex',
                flexDirection: 'column',
                transformOrigin: 'top center',
                overflow: 'hidden',
              }}
            >
              {/* Settings header */}
              <div id="settings-overlay" className="flex items-center justify-between px-4 py-2.5 shrink-0 bg-[#18181c] border-b border-white/[0.06]">
                <div className="flex items-center gap-6 min-w-0 flex-1">
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                    <button
                      data-no-drag
                      onClick={(e) => {
                        e.stopPropagation()
                        exitSettings()
                      }}
                      style={{
                        background: 'rgba(255,255,255,0.06)',
                        border: 'none',
                        color: 'rgba(255,255,255,0.6)',
                        fontSize: 11,
                        cursor: 'pointer',
                        padding: '3px 8px',
                        borderRadius: 6,
                        display: 'flex',
                        alignItems: 'center',
                        gap: 4,
                      }}
                    >
                      <span style={{ fontSize: 13 }}>&lsaquo;</span> {t('common.back')}
                    </button>
                    {(['pairing', 'settings'] as const).map((nav) => (
                      <button
                        key={nav}
                        data-no-drag
                        onClick={(e) => {
                          e.stopPropagation()
                          setSettingsNav(nav)
                        }}
                        style={{
                          background: settingsNav === nav ? 'rgba(255,255,255,0.12)' : 'none',
                          border: 'none',
                          color: settingsNav === nav ? '#fff' : 'rgba(255,255,255,0.4)',
                          fontSize: 11,
                          cursor: 'pointer',
                          padding: '3px 10px',
                          borderRadius: 6,
                          fontWeight: settingsNav === nav ? 600 : 400,
                        }}
                      >
                        {nav === 'pairing' ? t('mini.pairing') : t('mini.settings')}
                      </button>
                    ))}
                  </div>
                </div>
                <button
                  data-no-drag
                  onClick={(e) => {
                    e.stopPropagation()
                    exitSettings()
                  }}
                  className="text-slate-400 hover:text-rose-500 transition-colors ml-1"
                >
                  <X className="w-4.5 h-4.5" strokeWidth={2.5} />
                </button>
              </div>
              <div style={{ flex: 1, overflow: 'hidden', margin: 8, marginTop: 0, borderRadius: 12, minHeight: 0, display: 'flex', flexDirection: 'column' }}>
                <div className="bg-[#151515] text-white font-sans antialiased scrollbar-hidden" style={{ borderRadius: '12px 12px 0 0', overflow: 'auto', flex: 1, minHeight: 0 }}>
                  {settingsNav === 'pairing' && (
                    <div className="h-full overflow-y-auto bg-[#151515] pt-6 px-6 pb-10 scrollbar-hidden">
                      <div className="max-w-3xl mx-auto">
                        <p className="text-sm text-white/50 mb-10">{t('mini.pairingDesc')}</p>
                        <div className="mb-8">
                          <h2 className="text-xs font-bold text-white/30 uppercase tracking-widest mb-3 px-4">{t('mini.mascot')}</h2>
                          <div className="bg-[#0f0f0f] rounded-2xl border border-white/5 shadow-2xl overflow-hidden">
                            <AgentAccordionItem
                              agent={{ id: '__mini__', identityName: t('mini.mascot') }}
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
                        <div className="mb-8">
                          <h2 className="text-xs font-bold text-white/30 uppercase tracking-widest mb-3 px-4">{t('mini.charQueue', 'Agent Character Queue')}</h2>
                          <div className="bg-[#0f0f0f] rounded-2xl border border-white/5 shadow-2xl overflow-hidden">
                            <div className="p-4 space-y-2">
                              {charQueue.map((name, qi) => {
                                const charMeta = characters.find((c) => c.name === name)
                                const preview = charMeta ? getMiniGif(charMeta, false) : undefined
                                return (
                                  <div key={`${name}-${qi}`} className="flex items-center gap-3 p-2.5 rounded-xl bg-white/5 border border-white/10">
                                    <span className="text-[11px] text-white/30 w-5 text-center shrink-0">{qi + 1}</span>
                                    <div className="w-9 h-9 shrink-0 rounded-lg overflow-hidden bg-black/50 border border-white/10">
                                      {preview ? (
                                        <img src={preview} alt={name} className="w-full h-full object-contain opacity-90" style={{ imageRendering: 'pixelated' }} draggable={false} />
                                      ) : (
                                        <div className="w-full h-full flex items-center justify-center text-white/30 text-xs">?</div>
                                      )}
                                    </div>
                                    <span className="text-sm text-white/80 truncate flex-1">{charMeta?.builtin ? t(`charNames.${name}`, name) : name}</span>
                                    <div className="flex items-center gap-1 shrink-0">
                                      <button
                                        onClick={() => {
                                          if (qi === 0) return
                                          const q = [...charQueue]
                                          ;[q[qi - 1], q[qi]] = [q[qi], q[qi - 1]]
                                          saveCharQueue(q)
                                        }}
                                        disabled={qi === 0}
                                        className="w-6 h-6 flex items-center justify-center rounded text-white/40 hover:text-white hover:bg-white/10 transition-colors disabled:opacity-20 disabled:cursor-not-allowed"
                                      >
                                        <ChevronUp className="w-3.5 h-3.5" />
                                      </button>
                                      <button
                                        onClick={() => {
                                          if (qi === charQueue.length - 1) return
                                          const q = [...charQueue]
                                          ;[q[qi], q[qi + 1]] = [q[qi + 1], q[qi]]
                                          saveCharQueue(q)
                                        }}
                                        disabled={qi === charQueue.length - 1}
                                        className="w-6 h-6 flex items-center justify-center rounded text-white/40 hover:text-white hover:bg-white/10 transition-colors disabled:opacity-20 disabled:cursor-not-allowed"
                                      >
                                        <ChevronDown className="w-3.5 h-3.5" />
                                      </button>
                                      <button
                                        onClick={() => {
                                          if (charQueue.length <= 1) return
                                          saveCharQueue(charQueue.filter((_, j) => j !== qi))
                                        }}
                                        disabled={charQueue.length <= 1}
                                        className="w-6 h-6 flex items-center justify-center rounded text-white/40 hover:text-red-400 hover:bg-red-500/10 transition-colors disabled:opacity-20 disabled:cursor-not-allowed ml-1"
                                      >
                                        <X className="w-3.5 h-3.5" />
                                      </button>
                                    </div>
                                  </div>
                                )
                              })}
                              <button
                                onClick={() => setQueuePickerOpen(!queuePickerOpen)}
                                className="flex items-center gap-2 w-full p-2.5 rounded-xl border border-dashed border-white/10 text-white/40 hover:text-white/70 hover:border-white/20 hover:bg-white/[0.02] transition-colors"
                              >
                                <Plus className="w-4 h-4" />
                                <span className="text-sm">{t('mini.addChar', 'Add Character')}</span>
                              </button>
                              <AnimatePresence>
                                {queuePickerOpen && (
                                  <motion.div
                                    initial={{ height: 0, opacity: 0 }}
                                    animate={{ height: 'auto', opacity: 1 }}
                                    exit={{ height: 0, opacity: 0 }}
                                    transition={{ duration: 0.3, ease: [0.16, 1, 0.3, 1] }}
                                    className="overflow-hidden"
                                  >
                                    <div className="pt-2 border-t border-white/5">
                                      <div className="flex items-center justify-between mb-2">
                                        <span className="text-[11px] text-white/30">{t('mini.selectToAdd', 'Select a character to add')}</span>
                                        <button onClick={() => setIsCreateModalOpen(true)} className="flex items-center justify-center w-6 h-6 rounded text-white/40 hover:text-white hover:bg-white/10 transition-colors" title={t('mini.createChar')}>
                                          <Plus className="w-3.5 h-3.5" />
                                        </button>
                                      </div>
                                      <div className="max-h-[220px] overflow-y-auto pr-1 scrollbar-white">
                                        {(() => {
                                          const charsWithMini = characters.filter((c) => c.miniActions && Object.keys(c.miniActions).length > 0)
                                          const groups: { ip: string; chars: typeof charsWithMini }[] = []
                                          const ipOrder: string[] = []
                                          for (const c of charsWithMini) {
                                            const ip = c.ip || '自定义'
                                            if (!ipOrder.includes(ip)) ipOrder.push(ip)
                                          }
                                          const customIdx = ipOrder.indexOf('自定义')
                                          if (customIdx > 0) { ipOrder.splice(customIdx, 1); ipOrder.unshift('自定义') }
                                          const otherIdx = ipOrder.indexOf('其他')
                                          if (otherIdx >= 0 && otherIdx < ipOrder.length - 1) { ipOrder.splice(otherIdx, 1); ipOrder.push('其他') }
                                          for (const ip of ipOrder) { groups.push({ ip, chars: charsWithMini.filter((c) => (c.ip || '自定义') === ip) }) }
                                          return groups.map(({ ip, chars }) => (
                                            <div key={ip} className="mb-3 last:mb-0">
                                              <div className="text-[10px] font-medium text-white/25 uppercase tracking-wider mb-2 px-1">
                                                {ip === '自定义' ? t('mini.custom') : ip === '其他' ? t('mini.other') : ip === '原神' ? t('mini.ipGenshin') : ip === '赛马娘' ? t('mini.ipUmaMusume') : ip}
                                              </div>
                                              <div className="grid grid-cols-3 gap-2">
                                                {chars.map((c) => {
                                                  const cPreview = getMiniGif(c, false)
                                                  return (
                                                    <div
                                                      key={c.name}
                                                      onClick={() => {
                                                        saveCharQueue([...charQueue, c.name])
                                                        setQueuePickerOpen(false)
                                                      }}
                                                      className="flex items-center gap-2 p-2 rounded-lg bg-white/5 border border-transparent hover:bg-white/10 hover:border-white/10 cursor-pointer transition-all"
                                                    >
                                                      <div className="w-8 h-8 shrink-0 rounded-md overflow-hidden bg-black/50 border border-white/10">
                                                        {cPreview ? (
                                                          <img src={cPreview} alt={c.name} className="w-full h-full object-contain opacity-90" style={{ imageRendering: 'pixelated' }} draggable={false} />
                                                        ) : (
                                                          <div className="w-full h-full flex items-center justify-center text-white/30 text-xs">?</div>
                                                        )}
                                                      </div>
                                                      <span className="text-xs text-white/70 truncate">{c.builtin ? t(`charNames.${c.name}`, c.name) : c.name}</span>
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
                              <p className="text-[11px] text-white/20 px-1 pt-1">{t('mini.queueHint', 'Characters rotate across sessions in queue order.')}</p>
                            </div>
                          </div>
                        </div>
                        {agents.length > 0 && characters.filter((c) => c.miniActions && Object.keys(c.miniActions).length > 0).length < agents.length && (
                          <div className="text-xs text-amber-400 bg-amber-500/10 border border-amber-500/20 rounded-lg p-3">{t('mini.notEnoughChars')}</div>
                        )}
                      </div>
                    </div>
                  )}
                  {settingsNav === 'settings' && (
                    <div className="h-full overflow-y-auto bg-[#151515] scrollbar-hidden">
                      <SettingsTab
                        disableSleepAnim={disableSleepAnim}
                        onToggleSleepAnim={async (v) => {
                          setDisableSleepAnim(v)
                          const store = await getStore()
                          await store.set('disable_sleep_anim', v)
                          await store.save()
                        }}
                        notifySound={notifySound}
                        onChangeNotifySound={async (v) => {
                          setNotifySound(v)
                          const store = await getStore()
                          await store.set('notify_sound', v)
                          await store.save()
                        }}
                        soundEnabled={soundEnabled}
                        onToggleSoundEnabled={async (v) => {
                          setSoundEnabled(v)
                          const store = await getStore()
                          await store.set('sound_enabled', v)
                          await store.save()
                        }}
                        cursorSoundEnabled={cursorSoundEnabled}
                        onToggleCursorSoundEnabled={async (v) => {
                          setCursorSoundEnabled(v)
                          const store = await getStore()
                          await store.set('cursor_sound_enabled', v)
                          await store.save()
                        }}
                        waitingSound={waitingSound}
                        onToggleWaitingSound={async (v) => {
                          setWaitingSound(v)
                          const store = await getStore()
                          await store.set('waiting_sound', v)
                          await store.save()
                        }}
                        autoCloseCompletion={autoCloseCompletion}
                        onToggleAutoCloseCompletion={async (v) => {
                          setAutoCloseCompletion(v)
                          const store = await getStore()
                          await store.set('auto_close_completion', v)
                          await store.save()
                        }}
                        mascotPosition={mascotPosition}
                        onChangeMascotPosition={async (v) => {
                          setMascotPosition(v)
                          mascotPositionRef.current = v
                          const store = await getStore()
                          await store.set('mascot_position', v)
                          await store.save()
                        }}
                        islandBg={islandBg}
                        onChangeIslandBg={async (v) => {
                          setIslandBg(v)
                          const store = await getStore()
                          await store.set('island_bg', v)
                          await store.save()
                        }}
                        bgPos={bgPos}
                        onChangeBgPos={async (v) => {
                          setBgPos(v)
                          const store = await getStore()
                          await store.set('island_bg_pos', v)
                          await store.save()
                        }}
                        panelMaxHeight={panelMaxHeight}
                        onChangePanelMaxHeight={async (v) => {
                          setPanelMaxHeight(v)
                          const store = await getStore()
                          await store.set('panel_max_height', v)
                          await store.save()
                        }}
                      />
                    </div>
                  )}
                </div>
                <div
                  style={{
                    background: '#1a1a1a',
                    padding: '10px 14px',
                    borderRadius: '0 0 12px 12px',
                    flexShrink: 0,
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                  }}
                >
                  <span
                    onClick={() => invoke('open_url', { url: 'https://github.com/rainnoon/oc-claw' })}
                    style={{
                      color: 'rgba(255,255,255,0.35)',
                      fontSize: 11,
                      cursor: 'pointer',
                      transition: 'color 0.25s, transform 0.25s, letter-spacing 0.25s',
                      display: 'inline-flex',
                      alignItems: 'center',
                      gap: 4,
                    }}
                    onMouseEnter={(e) => {
                      e.currentTarget.style.color = '#f5c542'
                      e.currentTarget.style.transform = 'scale(1.04)'
                      e.currentTarget.style.letterSpacing = '0.3px'
                    }}
                    onMouseLeave={(e) => {
                      e.currentTarget.style.color = 'rgba(255,255,255,0.35)'
                      e.currentTarget.style.transform = 'scale(1)'
                      e.currentTarget.style.letterSpacing = '0px'
                    }}
                  >
                    {t('mini.starPrompt')} <span style={{ fontSize: 13, lineHeight: 1 }}>⭐</span> {t('mini.starPromptSuffix')}
                  </span>
                </div>
              </div>
            </motion.div>
          </>
        )}
      </AnimatePresence>

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
