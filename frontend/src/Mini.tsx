import { useState, useEffect, useCallback, useRef, useMemo } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { load } from '@tauri-apps/plugin-store'
import { listen } from '@tauri-apps/api/event'
import { ChevronDown, ChevronUp, Check, Loader2, Pen, Plus, X, Pin, Bell, BellOff, Move, Settings, Asterisk, Trash2, Cloud, Maximize, Minimize } from 'lucide-react'
import { AnimatePresence, motion } from 'motion/react'
import ReactMarkdown from 'react-markdown'
import { useTranslation } from 'react-i18next'
import { SettingsTab } from './components/SettingsTab'
import { UpdateModal, type UpdateModalInfo, type UpdateModalPhase } from './components/UpdateModal'
import { AgentDetailView } from './components/AgentDetailView'
import { CreateCharacterModal } from './components/CreateCharacterModal'
import { ClaudeStatsView } from './components/ClaudeStatsView'
import { getStore, DEFAULT_CHAR, DEFAULT_CHAR_NAME, loadCharacters, loadOcConnections, saveOcConnections } from './lib/store'
import { saveAgentCharMap } from './lib/agents'
import type { AgentMetrics, OcConnection } from './lib/types'
import { OnboardingModal } from './components/OnboardingModal'
import { PetContextMenu, PomodoroOverlay } from './components/PetContextMenu'
import {
  type AppMode, type PetData, type PetAction, type PomodoroState,
  loadAppMode, saveAppMode, loadPetData, savePetData, tickPetData,
  defaultPetData, getAffectionTier, canWalk,
  POMODORO_COINS_PER_MIN, AFFECTION_ACTIVITY_PER_10MIN, AFFECTION_MAX,
  HUNGER_ACTIVITY_PER_HOUR, HUNGER_OFFLINE_FLOOR,
  applyHeadpat,
} from './lib/petStore'

interface CharacterMeta {
  name: string
  builtin?: boolean
  ip?: string
  workGifs: string[]
  restGifs: string[]
  miniActions?: Record<string, string[]>
  largeActions?: Record<string, string>
  audioMap?: Record<string, string>
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

interface UpdateProgressPayload {
  stage: string
  progress?: number | null
  downloadedBytes?: number
  totalBytes?: number | null
  message?: string
}

const MAX_SLOTS = 10
const MASCOT_SCALE_MIN = 1
const MASCOT_SCALE_MAX = 3
const MASCOT_BASE_SIZE = 43

type PetState = 'idle' | 'working' | 'compacting' | 'waiting'
type ClaudeStatsSource = 'cc' | 'codex' | 'cursor'
const TRANSIENT_PET_ACTIONS: PetAction[] = ['eat', 'headpat', 'dance', 'farewell', 'angry', 'spin', 'milktea', 'walkout']

// Priority: higher number = harder to interrupt
function petActionPriority(action: PetAction): number {
  switch (action) {
    case 'grasp': return 100
    case 'study': case 'work': return 90
    case 'peek': case 'walkout': return 75
    case 'hungry': return 70
    case 'watch': return 50
    case 'music': return 40
    default: return 0
  }
}
const LARGE_MASCOT_HITBOX_WIDTH_MULTIPLIER = 1.8
const LARGE_MASCOT_HITBOX_HEIGHT_MULTIPLIER = 2.5
const MOVE_DRAG_THRESHOLD = 1

function clampMascotScale(value: number): number {
  if (!Number.isFinite(value)) return 1
  return Math.min(MASCOT_SCALE_MAX, Math.max(MASCOT_SCALE_MIN, value))
}

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

type LargePetAction = 'work' | 'rest' | 'question' | 'grasp' | 'spin' | 'angry'

// Pet mode action → largeActions video key mapping
const PET_ACTION_VIDEO_MAP: Record<PetAction, string> = {
  idle: 'idle',
  sleep: 'rest',
  work: 'work',
  study: 'study',
  watch: 'watch',
  music: 'music',
  walk: 'walk',
  dance: 'dance',
  eat: 'eat',
  hungry: 'hungry',
  headpat: 'headpat',
  farewell: 'farewell',
  grasp: 'grasp',
  angry: 'angry',
  spin: 'spin',
  milktea: 'milktea',
  rest: 'rest',
  peek: 'peek',
  walkout: 'walkout',
}

function getLargeVideoPetMode(char: CharacterMeta | undefined, petAction: PetAction, fallbackLargeActions?: Record<string, string>): string | undefined {
  const la = char?.largeActions && Object.keys(char.largeActions).length > 0
    ? char.largeActions
    : fallbackLargeActions
  if (!la || Object.keys(la).length === 0) return undefined
  const key = PET_ACTION_VIDEO_MAP[petAction] || 'idle'
  return la[key] || la['idle'] || la['rest'] || Object.values(la)[0]
}

function getLargeVideo(char: CharacterMeta | undefined, petState: PetState, overrideAction: string | null, fallbackLargeActions?: Record<string, string>): string | undefined {
  const la = char?.largeActions && Object.keys(char.largeActions).length > 0
    ? char.largeActions
    : fallbackLargeActions
  if (!la || Object.keys(la).length === 0) return undefined
  if (overrideAction && la[overrideAction]) return la[overrideAction]
  if (petState === 'waiting' && la['question']) return la['question']
  if (petState === 'working' || petState === 'compacting') return la['work'] || la['rest']
  return la['rest'] || la['work']
}

function getAlternateLargeVideoUrl(url: string): string | undefined {
  if (url.includes('/large/webm/') && url.endsWith('.webm')) {
    return url.replace('/large/webm/', '/large/mov/').replace(/\.webm$/, '.mov')
  }
  if (url.includes('/large/mov/') && url.endsWith('.mov')) {
    return url.replace('/large/mov/', '/large/webm/').replace(/\.mov$/, '.webm')
  }
  return undefined
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
  const [hasConfiguredOpenClaw, setHasConfiguredOpenClaw] = useState(false)
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
  const [claudeStatsSource, setClaudeStatsSource] = useState<ClaudeStatsSource>('cc')
  const [sessionNicknames, setSessionNicknames] = useState<Record<string, string>>({})
  const [editingSessionTitle, setEditingSessionTitle] = useState<string | null>(null)
  const editingTitleValueRef = useRef('')
  const editingTitleDefaultRef = useRef('')
  const composingRef = useRef(false)
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

  const resolveClaudeStatsSource = useCallback((source?: string): ClaudeStatsSource => {
    if (source === 'cursor') return 'cursor'
    if (source === 'codex') return 'codex'
    return 'cc'
  }, [])
  const resolveClaudeStatsSourceBySession = useCallback(
    (sessionId: string): ClaudeStatsSource => {
      const session = claudeSessionsRef.current.find((s) => s.sessionId === sessionId)
      return resolveClaudeStatsSource(session?.source)
    },
    [resolveClaudeStatsSource],
  )

  const isWindowsPlatform = typeof navigator !== 'undefined' && navigator.userAgent.includes('Windows')

  // Feature toggles
  const [enableClaudeCode, setEnableClaudeCode] = useState(true)
  const [enableCodex, setEnableCodex] = useState(!isWindowsPlatform)
  const [enableCursor, setEnableCursor] = useState(!isWindowsPlatform)
  const [soundEnabled, setSoundEnabled] = useState(true)
  const [codexSoundEnabled, setCodexSoundEnabled] = useState(true)
  const [cursorSoundEnabled, setCursorSoundEnabled] = useState(false)
  const [notifySound, setNotifySound] = useState<'default' | 'manbo'>('default')
  const [waitingSound, setWaitingSound] = useState(false)
  const [autoCloseCompletion, setAutoCloseCompletion] = useState(false)
  const [petSfxEnabled, setPetSfxEnabled] = useState(true)
  const petSfxEnabledRef = useRef(true)
  const [autoExpandOnTask, setAutoExpandOnTask] = useState(true)
  const [disableSleepAnim, setDisableSleepAnim] = useState(true)
  const [largeMascot, setLargeMascot] = useState(false)
  const largeMascotRef = useRef(false)
  largeMascotRef.current = largeMascot
  const [largeMascotScale, setLargeMascotScale] = useState(5)
  const largeMascotScaleRef = useRef(5)
  largeMascotScaleRef.current = largeMascotScale
  const [largePetAction, setLargePetAction] = useState<LargePetAction | null>(null)
  const largePetActionRef = useRef<LargePetAction | null>(null)
  largePetActionRef.current = largePetAction
  const largeActionTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const [panelMaxHeight, setPanelMaxHeight] = useState(300)
  const panelMaxHeightRef = useRef(300)
  panelMaxHeightRef.current = panelMaxHeight
  const [hoverDelay, setHoverDelay] = useState(0.2)
  const hoverDelayRef = useRef(0.2)
  hoverDelayRef.current = hoverDelay
  const [mascotScale, setMascotScale] = useState(1)
  const mascotScaleRef = useRef(1)
  mascotScaleRef.current = mascotScale
  const [, setMascotPosition] = useState<'left' | 'right'>('right')
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
  // ─── Pet / Nurture mode state ───
  const [appMode, setAppMode] = useState<AppMode | null>(null)
  const appModeRef = useRef<AppMode | null>(null)
  const [showOnboarding, setShowOnboarding] = useState(false)
  const [petData, setPetData] = useState<PetData>(defaultPetData())
  const petDataRef = useRef<PetData>(defaultPetData())
  petDataRef.current = petData
  const [currentPetAction, setCurrentPetAction] = useState<PetAction>('idle')
  const currentPetActionRef = useRef<PetAction>('idle')
  currentPetActionRef.current = currentPetAction
  const [walkFlipped, setWalkFlipped] = useState(false)
  const walkTimerRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const walkAutoRef = useRef(false)
  // When true, auto-walk heads straight to a screen edge (no flipping)
  const walkToEdgeRef = useRef(false)
  // 'left' = peeking from left edge (video flipped), 'right' = right edge
  const peekEdgeRef = useRef<'left' | 'right'>('right')
  const [pomodoro, setPomodoro] = useState<PomodoroState | null>(null)
  const pomodoroRef = useRef<PomodoroState | null>(null)
  pomodoroRef.current = pomodoro
  const pomodoroIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const petTickIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const activityTimerRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const [petContextMenuOpen, setPetContextMenuOpen] = useState(false)
  const petContextMenuOpenRef = useRef(false)
  const petContextMenuTransitionRef = useRef(false)
  const [petMenuSide, setPetMenuSide] = useState<'left' | 'right'>('left')
  // Pet-mode window inner width when the right-side menu is NOT open.
  // The mascot anchors to `left: petBaseWinW - mascotW` so its on-screen
  // position stays put even when the window widens for the menu. On
  // high-DPI Windows the actual window is wider than `mascotW + 180`
  // (because of `win_ui_scale`), so we cannot hard-code 180 here.
  const [petBaseWinW, setPetBaseWinW] = useState<number | null>(null)
  const petBaseWinWRef = useRef<number | null>(null)
  useEffect(() => { petBaseWinWRef.current = petBaseWinW }, [petBaseWinW])

  useEffect(() => {
    if (!isWindowsPlatform || appMode !== 'coding' || !largeMascot) return
    // Windows coding mode: disable large mascot to avoid close-time disappear regressions.
    setLargeMascot(false)
    largeMascotRef.current = false
    load('settings.json', { defaults: {}, autoSave: true }).then(async (store) => {
      await store.set('large_mascot', false)
      await store.save()
    }).catch(() => {})
  }, [isWindowsPlatform, appMode, largeMascot])

  // Food rain effect (lives outside context menu so it persists after menu closes)
  interface FoodRainDrop { id: number; emoji: string; x: number; delay: number; duration: number; size: number }
  const [foodRainDrops, setFoodRainDrops] = useState<FoodRainDrop[]>([])
  const foodRainIdRef = useRef(0)

  // Capture the pet-mode window's "no-menu" inner width. The mascot's
  // CSS `left:` is derived from this so it stays put when the right-side
  // context menu temporarily widens the window.
  useEffect(() => {
    if (!(appMode === 'pet' && largeMascot)) {
      setPetBaseWinW(null)
      return
    }
    const update = () => {
      if (petContextMenuOpenRef.current || petContextMenuTransitionRef.current) return
      const w = window.innerWidth || 0
      if (w > 0) setPetBaseWinW(w)
    }
    const id1 = requestAnimationFrame(() => requestAnimationFrame(update))
    const id2 = window.setTimeout(update, 200)
    window.addEventListener('resize', update)
    return () => {
      cancelAnimationFrame(id1)
      window.clearTimeout(id2)
      window.removeEventListener('resize', update)
    }
  }, [appMode, largeMascot])


  const [hiding, setHiding] = useState(false)
  const [pinned, setPinned] = useState(false)
  const pinnedRef = useRef(false)
  const [viewMode, _setViewMode] = useState<'island' | 'efficiency'>('efficiency')
  const viewModeRef = useRef<'island' | 'efficiency'>('efficiency')
  const expandedRef = useRef(false)
  const expandedWindowModeRef = useRef<'island' | 'efficiency' | null>(null)
  // showIdleSessions removed — all sessions visible, important ones sorted to top
  const collapsingRef = useRef(false)
  const customPosRef = useRef<{ x: number; y: number } | null>(null)
  const [moveMode, _setMoveMode] = useState(false)
  const moveModeRef = useRef(false)
  const moveModeActivatedAtRef = useRef(0)
  const mascotDragActiveRef = useRef(false)
  const setMoveMode = (v: boolean) => {
    moveModeRef.current = v
    if (v) moveModeActivatedAtRef.current = Date.now()
    _setMoveMode(v)
  }

  const { t, i18n } = useTranslation()
  const [updateModalOpen, setUpdateModalOpen] = useState(false)
  const updateModalOpenRef = useRef(false)
  const pendingUpdateInfoRef = useRef<UpdateModalInfo | null>(null)
  const updateModalRunOwnedRef = useRef(false)
  const [updateModalPhase, setUpdateModalPhase] = useState<UpdateModalPhase>('available')
  const [updateModalInfo, setUpdateModalInfo] = useState<UpdateModalInfo | null>(null)
  const [updateModalProgress, setUpdateModalProgress] = useState<number | null>(null)
  const [updateModalProgressStage, setUpdateModalProgressStage] = useState('preparing')

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

  // Pet mode data loaded inside main init effect below (no separate effect)

  // React to hunger changes (e.g. from Dev slider) — switch to/from hungry
  useEffect(() => {
    if (appMode !== 'pet') return
    if (petData.hunger < 30 && petActionPriority(currentPetAction) < petActionPriority('hungry')) {
      setCurrentPetAction('hungry')
      currentPetActionRef.current = 'hungry'
    } else if (petData.hunger >= 30 && currentPetAction === 'hungry') {
      setCurrentPetAction('idle')
      currentPetActionRef.current = 'idle'
    }
  }, [appMode, petData.hunger])

  // Pet mode: periodic tick for hunger/affection decay (every 5 min)
  useEffect(() => {
    if (appMode !== 'pet') return
    const tick = async () => {
      const ticked = tickPetData(petDataRef.current)
      setPetData(ticked)
      petDataRef.current = ticked
      await savePetData(ticked)
      // Auto-select hungry animation when hunger drops (overrides lower-priority actions)
      if (ticked.hunger < 30 && petActionPriority(currentPetActionRef.current) < petActionPriority('hungry')) {
        setCurrentPetAction('hungry')
        currentPetActionRef.current = 'hungry'
      }
    }
    petTickIntervalRef.current = setInterval(tick, 5 * 60 * 1000)
    return () => {
      if (petTickIntervalRef.current) clearInterval(petTickIntervalRef.current)
    }
  }, [appMode])

  // Pet mode always forces large mascot
  useEffect(() => {
    if (appMode === 'pet' && !largeMascot) {
      setLargeMascot(true)
      largeMascotRef.current = true
      load('settings.json', { defaults: {}, autoSave: true }).then(async (store) => {
        await store.set('large_mascot', true)
        await store.save()
      })
    }
  }, [appMode, largeMascot])

  // Pet mode: check system idle time → rest (5min no activity & no media) or idle
  const idleCheckRef = useRef<ReturnType<typeof setInterval> | null>(null)
  useEffect(() => {
    if (idleCheckRef.current) { clearInterval(idleCheckRef.current); idleCheckRef.current = null }
    if (appMode !== 'pet') return
    const check = async () => {
      const cur = currentPetActionRef.current
      if (petActionPriority(cur) >= petActionPriority('hungry')) return
      if (TRANSIENT_PET_ACTIONS.includes(cur)) return
      if (cur === 'walk' || cur === 'peek' || cur === 'walkout' || cur === 'grasp') return
      try {
        const [idleSec, media] = await Promise.all([
          invoke<number>('get_system_idle_time'),
          invoke<string>('get_now_playing'),
        ])
        const hasMedia = media === 'music' || media === 'video'
        const userInactive = idleSec >= 300 && !hasMedia
        if (userInactive && cur !== 'rest') {
          setCurrentPetAction('rest')
          currentPetActionRef.current = 'rest'
        } else if (!userInactive && cur === 'rest') {
          setCurrentPetAction('idle')
          currentPetActionRef.current = 'idle'
        }
      } catch {}
    }
    check()
    idleCheckRef.current = setInterval(check, 10_000)
    return () => { if (idleCheckRef.current) clearInterval(idleCheckRef.current) }
  }, [appMode])

  // Pet mode: while idle, randomly trigger weighted actions every ~3 min.
  // Keep spin out of idle random pool; spin is reserved for high-affection click.
  const idleAutoTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => {
    if (idleAutoTimerRef.current) {
      clearTimeout(idleAutoTimerRef.current)
      idleAutoTimerRef.current = null
    }
    if (appMode !== 'pet' || currentPetAction !== 'idle' || petActionPriority(currentPetAction) > 0) return
    idleAutoTimerRef.current = setTimeout(() => {
      if (currentPetActionRef.current !== 'idle' || petActionPriority(currentPetActionRef.current) > 0) return
      const weightedIdleActions: PetAction[] = ['walk', 'milktea', 'dance', 'dance']
      let next = weightedIdleActions[Math.floor(Math.random() * weightedIdleActions.length)]
      if (next === 'walk' && !canWalk(petDataRef.current)) {
        next = 'milktea'
      }
      if (next === 'walk') {
        walkAutoRef.current = true
        walkToEdgeRef.current = Math.random() > 0.5
      }
      setCurrentPetAction(next)
      currentPetActionRef.current = next
    }, 3 * 60 * 1000)
    return () => {
      if (idleAutoTimerRef.current) clearTimeout(idleAutoTimerRef.current)
    }
  }, [appMode, currentPetAction])

  // Pet mode: safety timeout for transient actions (recover from stuck states)
  const transientTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => {
    if (transientTimeoutRef.current) { clearTimeout(transientTimeoutRef.current); transientTimeoutRef.current = null }
    if (appMode !== 'pet' || !TRANSIENT_PET_ACTIONS.includes(currentPetAction)) return
    transientTimeoutRef.current = setTimeout(() => {
      if (appModeRef.current === 'pet' && TRANSIENT_PET_ACTIONS.includes(currentPetActionRef.current)) {
        console.warn('[pet] transient action timed out:', currentPetActionRef.current)
        const d = petDataRef.current
        const next: PetAction = d.hunger < 30 ? 'hungry' : 'idle'
        setCurrentPetAction(next)
        currentPetActionRef.current = next
      }
    }, 15_000)
    return () => { if (transientTimeoutRef.current) clearTimeout(transientTimeoutRef.current) }
  }, [appMode, currentPetAction])

  // Pet mode always uses the builtin mascot character assets.
  const PET_BUILTIN_BASE = '/assets/builtin/香企鹅'
  const preferWebmLarge = typeof navigator !== 'undefined' && navigator.userAgent.includes('Windows')
  const largeFolder = preferWebmLarge ? 'webm' : 'mov'
  const largeExt = preferWebmLarge ? 'webm' : 'mov'
  const petBuiltinLargeActions = useMemo<Record<string, string>>(() => ({
    idle: `${PET_BUILTIN_BASE}/large/${largeFolder}/idle.${largeExt}`,
    hungry: `${PET_BUILTIN_BASE}/large/${largeFolder}/hungry.${largeExt}`,
    eat: `${PET_BUILTIN_BASE}/large/${largeFolder}/eat.${largeExt}`,
    walk: `${PET_BUILTIN_BASE}/large/${largeFolder}/walk.${largeExt}`,
    walkout: `${PET_BUILTIN_BASE}/large/${largeFolder}/walkout.${largeExt}`,
    peek: `${PET_BUILTIN_BASE}/large/${largeFolder}/peek.${largeExt}`,
    headpat: `${PET_BUILTIN_BASE}/large/${largeFolder}/headpat.${largeExt}`,
    grasp: `${PET_BUILTIN_BASE}/large/${largeFolder}/grasp.${largeExt}`,
    angry: `${PET_BUILTIN_BASE}/large/${largeFolder}/angry.${largeExt}`,
    question: `${PET_BUILTIN_BASE}/large/${largeFolder}/question.${largeExt}`,
    farewell: `${PET_BUILTIN_BASE}/large/${largeFolder}/farewell.${largeExt}`,
    watch: `${PET_BUILTIN_BASE}/large/${largeFolder}/watch.${largeExt}`,
    music: `${PET_BUILTIN_BASE}/large/${largeFolder}/music.${largeExt}`,
    dance: `${PET_BUILTIN_BASE}/large/${largeFolder}/dance.${largeExt}`,
    study: `${PET_BUILTIN_BASE}/large/${largeFolder}/study.${largeExt}`,
    work: `${PET_BUILTIN_BASE}/large/${largeFolder}/work.${largeExt}`,
    rest: `${PET_BUILTIN_BASE}/large/${largeFolder}/rest.${largeExt}`,
    milktea: `${PET_BUILTIN_BASE}/large/${largeFolder}/milktea.${largeExt}`,
    spin: `${PET_BUILTIN_BASE}/large/${largeFolder}/spin.${largeExt}`,
  }), [PET_BUILTIN_BASE, largeExt, largeFolder])

  // Pet mode: play audio SFX mapped to pet actions via audio.json
  const petAudioRef = useRef<HTMLAudioElement | null>(null)
  const petAudioMapRef = useRef<Record<string, string> | null>(null)
  useEffect(() => {
    const jsonUrl = appMode === 'pet'
      ? `${PET_BUILTIN_BASE}/audio.json`
      : (() => {
          if (!miniChar?.largeActions) return ''
          const sampleUrl = Object.values(miniChar.largeActions)[0]
          if (!sampleUrl) return ''
          const baseDir = sampleUrl.replace(/\/large\/.*$/, '')
          return `${baseDir}/audio.json`
        })()
    if (!jsonUrl) return
    console.log('[pet-audio] fetching audio.json from:', jsonUrl)
    fetch(jsonUrl)
      .then(r => {
        console.log('[pet-audio] fetch status:', r.status, r.ok)
        return r.ok ? r.json() : null
      })
      .then((map: Record<string, string> | null) => {
        if (!map) { console.warn('[pet-audio] audio.json empty or failed'); return }
        const resolved: Record<string, string> = {}
        const baseDir = jsonUrl.replace(/\/audio\.json$/, '')
        for (const [action, file] of Object.entries(map)) {
          resolved[action] = `${baseDir}/audio/${file}`
        }
        console.log('[pet-audio] loaded audioMap:', resolved)
        petAudioMapRef.current = resolved
      })
      .catch(e => console.error('[pet-audio] fetch error:', e))
  }, [miniChar, appMode])

  const petSfxPlayingRef = useRef(false)
  const playPetAudio = useCallback((action: PetAction) => {
    if (appModeRef.current !== 'pet') return
    if (!petSfxEnabledRef.current) return
    const FALLBACK_AUDIO: Record<string, string> = {
      angry: '/assets/builtin/香企鹅/audio/angry.mp3',
      grasp: '/assets/builtin/香企鹅/audio/angry.mp3',
      headpat: '/assets/builtin/香企鹅/audio/cute.mp3',
      spin: '/assets/builtin/香企鹅/audio/happy.mp3',
      walkout: '/assets/builtin/香企鹅/audio/happy.mp3',
      eat: '/assets/builtin/香企鹅/audio/happy.mp3',
    }
    const map = petAudioMapRef.current || FALLBACK_AUDIO
    const src = map[action]
    if (!src) return
    if (petAudioRef.current) {
      petAudioRef.current.pause()
      petAudioRef.current.currentTime = 0
    }
    const audio = new Audio(src)
    audio.volume = 0.6
    petAudioRef.current = audio
    petSfxPlayingRef.current = true
    audio.addEventListener('ended', () => { petSfxPlayingRef.current = false })
    audio.addEventListener('pause', () => { petSfxPlayingRef.current = false })
    audio.addEventListener('error', () => { petSfxPlayingRef.current = false })
    audio.play().catch(() => { petSfxPlayingRef.current = false })
  }, [])

  // Pet mode: activity affection gain (watch/music: +1 per 10 min)
  useEffect(() => {
    if (appMode !== 'pet') return
    if (currentPetAction !== 'watch' && currentPetAction !== 'music') {
      if (activityTimerRef.current) clearInterval(activityTimerRef.current)
      activityTimerRef.current = null
      return
    }
    activityTimerRef.current = setInterval(async () => {
      const d = { ...petDataRef.current }
      d.affection = Math.min(AFFECTION_MAX, d.affection + AFFECTION_ACTIVITY_PER_10MIN)
      d.hunger = Math.max(HUNGER_OFFLINE_FLOOR, d.hunger - HUNGER_ACTIVITY_PER_HOUR / 6)
      setPetData(d)
      petDataRef.current = d
      await savePetData(d)
    }, 10 * 60 * 1000)
    return () => {
      if (activityTimerRef.current) clearInterval(activityTimerRef.current)
    }
  }, [appMode, currentPetAction])

  // Pet mode: randomly trigger dance during music (avg every ~5 min)
  const musicDanceTimerRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const danceFromMusicRef = useRef(false)
  useEffect(() => {
    if (musicDanceTimerRef.current) {
      clearInterval(musicDanceTimerRef.current)
      musicDanceTimerRef.current = null
    }
    if (appMode !== 'pet' || currentPetAction !== 'music') return
    musicDanceTimerRef.current = setInterval(() => {
      if (currentPetActionRef.current !== 'music') return
      if (Math.random() < 0.1) {
        danceFromMusicRef.current = true
        setCurrentPetAction('dance')
        currentPetActionRef.current = 'dance'
      }
    }, 30_000)
    return () => {
      if (musicDanceTimerRef.current) clearInterval(musicDanceTimerRef.current)
    }
  }, [appMode, currentPetAction])

  // Walk animation: move the window and flip direction every 3 seconds.
  // In "walk to edge" mode, walk straight to the nearest screen edge → peek.
  useEffect(() => {
    if (walkTimerRef.current) {
      clearInterval(walkTimerRef.current)
      walkTimerRef.current = null
    }
    if (currentPetAction !== 'walk') {
      setWalkFlipped(false)
      return
    }
    const WALK_SPEED = 2
    const WALK_INTERVAL = 30
    const FLIP_AFTER = 3000
    const AUTO_WALK_DURATION = 6000
    const isAuto = walkAutoRef.current
    const isToEdge = walkToEdgeRef.current
    walkAutoRef.current = false
    walkToEdgeRef.current = false
    let elapsed = 0
    let totalElapsed = 0
    let direction = -1
    let edgeDirection = 0
    let edgeCheckCounter = 0
    setWalkFlipped(false)

    if (isToEdge) {
      // Determine which edge is closer, then walk toward it
      Promise.all([
        invoke('get_mini_origin'),
        invoke('get_mini_monitor_rect'),
      ]).then(([pos, rect]) => {
        const [x] = pos as [number, number]
        const [monitorX, , monitorW] = rect as [number, number, number, number]
        const monitorLeft = monitorX
        const monitorRight = monitorX + monitorW
        const baseWinW = petBaseWinWRef.current ?? window.innerWidth ?? 300
        const mascotW = largeMascotRef.current
          ? MASCOT_BASE_SIZE * mascotScaleRef.current * largeMascotScaleRef.current
          : MASCOT_BASE_SIZE * mascotScaleRef.current
        const mascotLeft = x + baseWinW - mascotW
        const mascotRight = x + baseWinW
        const distLeft = mascotLeft - monitorLeft
        const distRight = monitorRight - mascotRight
        edgeDirection = distLeft <= distRight ? -1 : 1
        setWalkFlipped(edgeDirection === 1)
      }).catch(() => {})
    }

    walkTimerRef.current = setInterval(() => {
      elapsed += WALK_INTERVAL
      totalElapsed += WALK_INTERVAL

      if (isToEdge) {
        // Walk straight toward the edge without flipping
        const dir = edgeDirection || -1
        invoke('move_mini_by', { dx: dir * WALK_SPEED, dy: 0 }).catch(() => {})
      } else {
        // Normal oscillating walk
        if (isAuto && totalElapsed >= AUTO_WALK_DURATION) {
          if (walkTimerRef.current) clearInterval(walkTimerRef.current)
          walkTimerRef.current = null
          const d = petDataRef.current
          const next: PetAction = d.hunger < 30 ? 'hungry' : 'idle'
          setCurrentPetAction(next)
          currentPetActionRef.current = next
          return
        }
        if (elapsed >= FLIP_AFTER) {
          elapsed = 0
          direction *= -1
          setWalkFlipped(prev => !prev)
        }
        invoke('move_mini_by', { dx: direction * WALK_SPEED, dy: 0 }).catch(() => {})
      }

      // Check screen edge every ~300ms
      edgeCheckCounter += WALK_INTERVAL
      if (edgeCheckCounter >= 300) {
        edgeCheckCounter = 0
        Promise.all([
          invoke('get_mini_origin'),
          invoke('get_mini_monitor_rect'),
        ]).then(([pos, rect]) => {
          const [x] = pos as [number, number]
          const [monitorX, , monitorW] = rect as [number, number, number, number]
          const monitorLeft = monitorX
          const monitorRight = monitorX + monitorW
          const baseWinW = petBaseWinWRef.current ?? window.innerWidth ?? 300
          const mascotW = largeMascotRef.current
            ? MASCOT_BASE_SIZE * mascotScaleRef.current * largeMascotScaleRef.current
            : MASCOT_BASE_SIZE * mascotScaleRef.current
          const mascotRight = x + baseWinW
          const mascotLeft = x + baseWinW - mascotW
          if (mascotLeft <= monitorLeft + EDGE_THRESHOLD) {
            if (walkTimerRef.current) clearInterval(walkTimerRef.current)
            walkTimerRef.current = null
            peekEdgeRef.current = 'left'
            setCurrentPetAction('peek')
            currentPetActionRef.current = 'peek'
            snapToPeekEdge('left', x, monitorLeft, monitorRight)
          } else if (mascotRight >= monitorRight - EDGE_THRESHOLD) {
            if (walkTimerRef.current) clearInterval(walkTimerRef.current)
            walkTimerRef.current = null
            peekEdgeRef.current = 'right'
            setCurrentPetAction('peek')
            currentPetActionRef.current = 'peek'
            snapToPeekEdge('right', x, monitorLeft, monitorRight)
          }
        }).catch(() => {})
      }
    }, WALK_INTERVAL)
    return () => {
      if (walkTimerRef.current) clearInterval(walkTimerRef.current)
    }
  }, [currentPetAction])

  // Pet mode: auto-detect music/video from frontmost app
  useEffect(() => {
    if (appMode !== 'pet') return
    const poll = async () => {
      if (petSfxPlayingRef.current) return
      const cur = currentPetActionRef.current
      const curPri = petActionPriority(cur)
      if (curPri >= petActionPriority('hungry')) return
      try {
        const media = await invoke<string>('get_now_playing')
        if (media === 'video' && cur !== 'watch') {
          setCurrentPetAction('watch')
          currentPetActionRef.current = 'watch'
        } else if (media === 'music' && cur !== 'music') {
          setCurrentPetAction('music')
          currentPetActionRef.current = 'music'
        } else if (media === 'none' && (cur === 'music' || cur === 'watch')) {
          const d = petDataRef.current
          const next: PetAction = d.hunger < 30 ? 'hungry' : 'idle'
          setCurrentPetAction(next)
          currentPetActionRef.current = next
        }
      } catch {}
    }
    poll()
    const id = setInterval(poll, 2000)
    return () => clearInterval(id)
  }, [appMode])

  const handleSelectAppMode = useCallback(async (mode: AppMode) => {
    appModeRef.current = mode
    await saveAppMode(mode)
    if (mode === 'pet') {
      largeMascotRef.current = true
      const store = await load('settings.json', { defaults: {}, autoSave: true })
      await store.set('large_mascot', true)
      await store.save()
      const data = await loadPetData()
      const ticked = tickPetData(data)
      petDataRef.current = ticked
      await savePetData(ticked)
      // When switching mode from inside Settings, keep the settings-sized window.
      // Expanding to pet window size here compresses the settings UI for a few frames.
      if (settingsModeRef.current || settingsTransitioningRef.current) {
        setAppMode(mode)
        setShowOnboarding(false)
        setLargeMascot(true)
        setPetData(ticked)
        try {
          await invoke('set_mini_size', { restore: false, position: mascotPositionRef.current, mascotScale: mascotScaleRef.current, keepOnTop: true })
        } catch {}
        return
      }
      // Hide window content before resizing to avoid flashing the
      // onboarding modal at the wrong window dimensions.
      document.documentElement.style.opacity = '0'
      // Reposition window, then update React state, then fade in
      await invoke('set_mini_expanded', { expanded: false, position: mascotPositionRef.current, efficiency: true, mascotScale: mascotScaleRef.current, largeMascot: true, largeMascotScale: largeMascotScaleRef.current }).catch(() => {})
      await invoke('set_pet_mode_window', { active: true, mascotScale: mascotScaleRef.current, largeMascotScale: largeMascotScaleRef.current }).catch(() => {})
      setAppMode(mode)
      setShowOnboarding(false)
      setLargeMascot(true)
      setPetData(ticked)
      // Wait for React to render the mascot and chroma key canvas to draw, then reveal.
      // Windows needs extra frames for the canvas chroma key loop to process the first frame.
      const reveal = () => { document.documentElement.style.opacity = '1' }
      if (isWindowsPlatform) {
        setTimeout(reveal, 150)
      } else {
        requestAnimationFrame(reveal)
      }
    } else {
      setAppMode(mode)
      setShowOnboarding(false)
      if (isWindowsPlatform && mode === 'coding') {
        setLargeMascot(false)
        largeMascotRef.current = false
        const store = await load('settings.json', { defaults: {}, autoSave: true })
        await store.set('large_mascot', false)
        await store.save()
      }
      // Leaving pet mode: stop the pass-through poll first.
      await invoke('set_pet_mode_window', { active: false, mascotScale: mascotScaleRef.current, largeMascotScale: largeMascotScaleRef.current }).catch(() => {})
      // When switching mode from inside Settings, keep the settings-sized window.
      // Shrinking to collapsed size here compresses the settings UI for a few frames.
      if (settingsModeRef.current || settingsTransitioningRef.current) {
        try {
          await invoke('set_mini_size', { restore: false, position: mascotPositionRef.current, mascotScale: mascotScaleRef.current, keepOnTop: true })
        } catch {}
        return
      }
      // Restore window back to collapsed mascot size
      try {
        await invoke('set_mini_size', { restore: true, position: mascotPositionRef.current, mascotScale: mascotScaleRef.current, largeMascot: largeMascotRef.current, largeMascotScale: largeMascotScaleRef.current })
      } catch {}
    }
  }, [])

  const handleUpdatePetData = useCallback(async (data: PetData) => {
    setPetData(data)
    petDataRef.current = data
    await savePetData(data)
  }, [])

  const handleSetPetAction = useCallback((action: PetAction) => {
    // During pomodoro only allow 'study' or explicit stop (which sets 'idle')
    if (pomodoroRef.current?.active && action !== 'study' && action !== 'idle') return
    if (action === 'walk') {
      walkToEdgeRef.current = true
    }
    setCurrentPetAction(action)
    currentPetActionRef.current = action
  }, [])

  // Check if the mascot is at a screen edge and switch to peek if so.
  // Snaps window to a fixed position so the character protrudes by a
  // consistent amount (PEEK_VISIBLE_FRACTION of the mascot width).
  const PEEK_VISIBLE_FRACTION = 0.95
  const EDGE_THRESHOLD = 30

  const snapToPeekEdge = useCallback((edge: 'left' | 'right', currentX: number, monitorLeft: number, monitorRight: number) => {
    const baseWinW = petBaseWinWRef.current ?? window.innerWidth ?? 300
    const mascotW = MASCOT_BASE_SIZE * mascotScaleRef.current * largeMascotScaleRef.current
    const visibleW = mascotW * PEEK_VISIBLE_FRACTION
    let targetX: number
    if (edge === 'right') {
      targetX = monitorRight - visibleW - baseWinW + mascotW
    } else {
      targetX = monitorLeft + visibleW - baseWinW
    }
    const dx = targetX - currentX
    if (Math.abs(dx) > 1) {
      invoke('move_mini_by', { dx: Math.round(dx), dy: 0 }).catch(() => {})
    }
  }, [])

  const checkEdgeAndSetPeek = useCallback(() => {
    Promise.all([
      invoke('get_mini_origin'),
      invoke('get_mini_monitor_rect'),
    ]).then(([pos, rect]) => {
      const [x] = pos as [number, number]
      const [monitorX, , monitorW] = rect as [number, number, number, number]
      const monitorLeft = monitorX
      const monitorRight = monitorX + monitorW
      const baseWinW = petBaseWinWRef.current ?? window.innerWidth ?? 300
      const mascotW = MASCOT_BASE_SIZE * mascotScaleRef.current * largeMascotScaleRef.current
      const mascotRight = x + baseWinW
      const mascotLeft = x + baseWinW - mascotW
      if (mascotLeft <= monitorLeft + EDGE_THRESHOLD) {
        peekEdgeRef.current = 'left'
        setCurrentPetAction('peek')
        currentPetActionRef.current = 'peek'
        snapToPeekEdge('left', x, monitorLeft, monitorRight)
      } else if (mascotRight >= monitorRight - EDGE_THRESHOLD) {
        peekEdgeRef.current = 'right'
        setCurrentPetAction('peek')
        currentPetActionRef.current = 'peek'
        snapToPeekEdge('right', x, monitorLeft, monitorRight)
      } else {
        const d = petDataRef.current
        const action: PetAction = d.hunger < 30 ? 'hungry' : 'idle'
        setCurrentPetAction(action)
        currentPetActionRef.current = action
      }
    }).catch(() => {
      const d = petDataRef.current
      const action: PetAction = d.hunger < 30 ? 'hungry' : 'idle'
      setCurrentPetAction(action)
      currentPetActionRef.current = action
    })
  }, [snapToPeekEdge])

  const handleStartPomodoro = useCallback(async (minutes: number) => {
    const state: PomodoroState = {
      active: true,
      duration: minutes * 60,
      remaining: minutes * 60,
      startedAt: Date.now(),
    }
    setPomodoro(state)
    pomodoroRef.current = state
    setCurrentPetAction('study')
    currentPetActionRef.current = 'study'

    if (pomodoroIntervalRef.current) clearInterval(pomodoroIntervalRef.current)
    pomodoroIntervalRef.current = setInterval(async () => {
      const p = pomodoroRef.current
      if (!p?.active) return
      const elapsed = (Date.now() - p.startedAt) / 1000
      const left = Math.max(0, p.duration - elapsed)
      const earnedMinutes = Math.floor(elapsed / 60)
      const prevEarned = petDataRef.current.pomodoroCoins
      if (earnedMinutes > prevEarned) {
        const d = { ...petDataRef.current }
        const newCoins = earnedMinutes - prevEarned
        d.coins += newCoins * POMODORO_COINS_PER_MIN
        d.pomodoroCoins = earnedMinutes
        setPetData(d)
        petDataRef.current = d
        await savePetData(d)
      }
      if (left <= 0) {
        // Pomodoro complete
        if (pomodoroIntervalRef.current) clearInterval(pomodoroIntervalRef.current)
        pomodoroIntervalRef.current = null
        // Play a cute completion cue for pet mode pomodoro.
        playPetAudio('headpat')
        setPomodoro(null)
        pomodoroRef.current = null
        setCurrentPetAction('idle')
        currentPetActionRef.current = 'idle'
        const d = { ...petDataRef.current, pomodoroCoins: 0 }
        setPetData(d)
        petDataRef.current = d
        await savePetData(d)
      }
    }, 1000)
  }, [])

  const handleStopPomodoro = useCallback(async () => {
    if (pomodoroIntervalRef.current) clearInterval(pomodoroIntervalRef.current)
    pomodoroIntervalRef.current = null
    setPomodoro(null)
    pomodoroRef.current = null
    setCurrentPetAction('idle')
    currentPetActionRef.current = 'idle'
    const d = { ...petDataRef.current, pomodoroCoins: 0 }
    setPetData(d)
    petDataRef.current = d
    await savePetData(d)
  }, [])

  const closePetContextMenu = useCallback(async () => {
    if (!petContextMenuOpenRef.current || petContextMenuTransitionRef.current) return
    petContextMenuTransitionRef.current = true
    try {
      setPetContextMenuOpen(false)
      petContextMenuOpenRef.current = false
      await invoke('set_pet_context_menu', { open: false }).catch(() => {})
    } finally {
      petContextMenuTransitionRef.current = false
    }
  }, [])

  const triggerFoodRain = useCallback((emoji: string) => {
    const drops: FoodRainDrop[] = Array.from({ length: 12 }, () => {
      foodRainIdRef.current += 1
      return {
        id: foodRainIdRef.current,
        emoji,
        x: Math.random() * 100,
        delay: Math.random() * 0.6,
        duration: 1.2 + Math.random() * 0.8,
        size: 18 + Math.random() * 14,
      }
    })
    setFoodRainDrops(prev => [...prev, ...drops])
    setTimeout(() => {
      setFoodRainDrops(prev => prev.filter(d => !drops.includes(d)))
    }, 2500)
  }, [])


  useEffect(() => {
    load('settings.json', { defaults: {}, autoSave: true }).then(async (store) => {
      _setViewMode('efficiency')
      viewModeRef.current = 'efficiency'
      const storedPosition = (await store.get('mascot_position')) as string | null
      const initialMascotPosition = storedPosition === 'left' || storedPosition === 'right' ? storedPosition : 'right'
      const storedMascotScale = await store.get('mascot_scale')
      const initialMascotScale = typeof storedMascotScale === 'number' ? clampMascotScale(storedMascotScale) : 1
      const storedLargeMascot = await store.get('large_mascot')
      let initialLargeMascot = typeof storedLargeMascot === 'boolean' ? storedLargeMascot : false
      const storedLargeMascotScale = await store.get('large_mascot_scale')
      const initialLargeMascotScale = typeof storedLargeMascotScale === 'number' ? Math.min(6, Math.max(4, storedLargeMascotScale)) : 5
      setMascotPosition(initialMascotPosition)
      setMascotScale(initialMascotScale)
      setLargeMascot(initialLargeMascot)
      setLargeMascotScale(initialLargeMascotScale)
      mascotPositionRef.current = initialMascotPosition
      mascotScaleRef.current = initialMascotScale
      largeMascotRef.current = initialLargeMascot
      largeMascotScaleRef.current = initialLargeMascotScale

      const existingMode = await loadAppMode()
      if (existingMode === 'pet' || existingMode === 'coding') {
        setAppMode(existingMode)
        appModeRef.current = existingMode
        setShowOnboarding(false)
        if (existingMode === 'pet') {
          const data = await loadPetData()
          const ticked = tickPetData(data)
          setPetData(ticked)
          petDataRef.current = ticked
          await savePetData(ticked)
          initialLargeMascot = true
          setLargeMascot(true)
          largeMascotRef.current = true
          await store.set('large_mascot', true)
        } else if (isWindowsPlatform) {
          // Windows coding mode: force small mascot for stability.
          initialLargeMascot = false
          setLargeMascot(false)
          largeMascotRef.current = false
          await store.set('large_mascot', false)
        }
        await invoke('set_mini_expanded', {
          expanded: false,
          position: initialMascotPosition,
          efficiency: true,
          mascotScale: initialMascotScale,
          largeMascot: existingMode === 'pet' ? true : initialLargeMascot,
          largeMascotScale: initialLargeMascotScale,
        }).catch(() => {})
        if (existingMode === 'pet') {
          await invoke('set_pet_mode_window', {
            active: true,
            mascotScale: initialMascotScale,
            largeMascotScale: initialLargeMascotScale,
          }).catch(() => {})
        } else {
          invoke('set_pet_mode_window', {
            active: false,
            mascotScale: initialMascotScale,
            largeMascotScale: initialLargeMascotScale,
          }).catch(() => {})
        }
      } else {
        // First launch (or mode not chosen yet): show mode onboarding.
        setAppMode(null)
        appModeRef.current = null
        try {
          await invoke('set_mini_size', {
            restore: false,
            position: initialMascotPosition,
            keepOnTop: true,
            mascotScale: initialMascotScale,
          })
        } catch {}
        setShowOnboarding(true)
      }

      await store.set('view_mode', 'efficiency')
      // Force-reset mascot custom position to avoid off-screen placement.
      // Keep collapsed default placement controlled by `set_mini_expanded`.
      customPosRef.current = null
      await store.set('mini_custom_pos', null)
      await store.save()
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
      await loadMiniChar()
      const store0 = await load('settings.json', { defaults: {}, autoSave: true })
      const q = (await store0.get('char_queue')) as string[] | null
      if (q && q.length) setCharQueue(q)
    } catch (e) {
      console.warn('[fetchAgents] loadCharacters failed:', e)
    }
    try {
      const store = await load('settings.json', { defaults: {}, autoSave: true })
      const connections = await loadOcConnections()
      setHasConfiguredOpenClaw(connections.some((conn) => connToOcParams(conn) !== null))

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
  }, [loadMiniChar, t])

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
    if (appMode !== 'coding') return
    fetchAgents()
    pollHealth()
    const a = setInterval(fetchAgents, 5000)
    const h = setInterval(pollHealth, 1000)
    return () => {
      clearInterval(a)
      clearInterval(h)
    }
  }, [fetchAgents, pollHealth, appMode])

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
    if (!expanded || appMode !== 'coding') return
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
  }, [expanded, fetchAllSessions, drainPreviewQueue, appMode])

  // Load feature toggles
  useEffect(() => {
    ;(async () => {
      const store = await load('settings.json', { defaults: {}, autoSave: true })
      const cc = await store.get('enable_claudecode')
      if (typeof cc === 'boolean') setEnableClaudeCode(cc)
      const cod = await store.get('enable_codex')
      const codEnabled = isWindowsPlatform ? false : cod !== false
      setEnableCodex(codEnabled)
      if (cc !== false || codEnabled) invoke('install_claude_hooks').catch(() => {})
      const cur = await store.get('enable_cursor')
      const curEnabled = isWindowsPlatform ? false : cur !== false
      setEnableCursor(curEnabled)
      if (curEnabled) invoke('install_cursor_hooks').catch(() => {})
      if (isWindowsPlatform) {
        await store.set('enable_codex', false)
        await store.set('enable_cursor', false)
        await store.save()
      }
      const snd = await store.get('sound_enabled')
      if (typeof snd === 'boolean') setSoundEnabled(snd)
      const codsnd = await store.get('codex_sound_enabled')
      if (typeof codsnd === 'boolean') setCodexSoundEnabled(codsnd)
      const csnd = await store.get('cursor_sound_enabled')
      if (typeof csnd === 'boolean') setCursorSoundEnabled(csnd)
      const ns = (await store.get('notify_sound')) as string
      if (ns === 'default' || ns === 'manbo') setNotifySound(ns)
      const ws = await store.get('waiting_sound')
      if (typeof ws === 'boolean') setWaitingSound(ws)
      const acc = await store.get('auto_close_completion')
      if (typeof acc === 'boolean') setAutoCloseCompletion(acc)
      const psfx = await store.get('pet_sfx_enabled')
      if (typeof psfx === 'boolean') { setPetSfxEnabled(psfx); petSfxEnabledRef.current = psfx }
      const aet = await store.get('auto_expand_on_task')
      if (typeof aet === 'boolean') {
        setAutoExpandOnTask(aet)
        autoExpandOnTaskRef.current = aet
      }
      const dsa = await store.get('disable_sleep_anim')
      if (typeof dsa === 'boolean') setDisableSleepAnim(dsa)
      const lm = await store.get('large_mascot')
      if (typeof lm === 'boolean' && appModeRef.current !== 'pet') {
        const largeEnabled = isWindowsPlatform && appModeRef.current === 'coding' ? false : lm
        setLargeMascot(largeEnabled)
        largeMascotRef.current = largeEnabled
      }
      const lms = await store.get('large_mascot_scale')
      if (typeof lms === 'number') {
        const clamped = Math.min(6, Math.max(4, lms))
        setLargeMascotScale(clamped)
        largeMascotScaleRef.current = clamped
      }
      const pmh = await store.get('panel_max_height')
      if (typeof pmh === 'number' && pmh >= 200 && pmh <= 500) setPanelMaxHeight(pmh)
      const hd = await store.get('hover_delay')
      if (typeof hd === 'number' && hd >= 0 && hd <= 2) {
        setHoverDelay(hd)
        hoverDelayRef.current = hd
      }
      const ms = await store.get('mascot_scale')
      if (typeof ms === 'number') {
        const nextMascotScale = clampMascotScale(ms)
        setMascotScale(nextMascotScale)
        mascotScaleRef.current = nextMascotScale
      }
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
    if (appMode !== 'coding') { setClaudeSessions([]); return }
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
            if (
              autoExpandOnTaskRef.current &&
              !updateModalOpenRef.current &&
              !s.isActiveTab &&
              viewModeRef.current === 'efficiency' &&
              !expandedRef.current &&
              !expandingRef.current &&
              !collapsingRef.current
            ) {
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
  }, [enableClaudeCode, enableCodex, enableCursor, appMode])

  // Listen for Claude/Codex/Cursor task completion → play sound
  const soundEnabledRef = useRef(soundEnabled)
  soundEnabledRef.current = soundEnabled
  const codexSoundEnabledRef = useRef(codexSoundEnabled)
  codexSoundEnabledRef.current = codexSoundEnabled
  const cursorSoundEnabledRef = useRef(cursorSoundEnabled)
  cursorSoundEnabledRef.current = cursorSoundEnabled
  const notifySoundRef = useRef(notifySound)
  notifySoundRef.current = notifySound
  const waitingSoundRef = useRef(waitingSound)
  waitingSoundRef.current = waitingSound
  const autoCloseCompletionRef = useRef(autoCloseCompletion)
  autoCloseCompletionRef.current = autoCloseCompletion
  const autoExpandOnTaskRef = useRef(autoExpandOnTask)
  autoExpandOnTaskRef.current = autoExpandOnTask
  const autoCloseTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => {
    if (appMode !== 'coding') return
    if (!(enableClaudeCode || enableCodex || enableCursor)) return
    const unlisten = listen('claude-task-complete', (ev: any) => {
      if (ev.payload?.waiting && viewModeRef.current === 'efficiency' && autoExpandOnTaskRef.current) {
        setEffListCollapsed(true)
        if (!expandedRef.current && expandFnRef.current) {
          expandFnRef.current()
        }
      }
      const currentSession = claudeSessionsRef.current.find((s) => s.sessionId === ev.payload?.sessionId)
      const isCursor = ev.payload?.source === 'cursor' || currentSession?.source === 'cursor'
      const isCodex = ev.payload?.source === 'codex' || currentSession?.source === 'codex'
      const shouldSound = isCursor ? cursorSoundEnabledRef.current : isCodex ? codexSoundEnabledRef.current : soundEnabledRef.current
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
  }, [enableClaudeCode, enableCodex, enableCursor, appMode])

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
  const hoverOpenTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const syncExpandedWindowLayout = useCallback(async (mode: 'island' | 'efficiency' = viewModeRef.current) => {
    await invoke('set_mini_expanded', {
      expanded: true,
      position: mascotPositionRef.current,
      efficiency: mode === 'efficiency',
      maxHeight: panelMaxHeightRef.current,
      mascotScale: mascotScaleRef.current,
    })
    expandedWindowModeRef.current = mode
  }, [])
  const expand = useCallback(async () => {
    if (collapsingRef.current || expandingRef.current) return
    expandingRef.current = true
    setHiding(true)
    try {
      await new Promise<void>((r) => setTimeout(r, 50))
      await syncExpandedWindowLayout(viewModeRef.current)
      setExpanded(true)
      expandedRef.current = true
      requestAnimationFrame(() => {
        requestAnimationFrame(() => setShowPanel(true))
      })
    } catch (e) {
      console.warn('[mini] expand failed:', e)
      setShowPanel(false)
      setExpanded(false)
      expandedRef.current = false
      expandedWindowModeRef.current = null
    } finally {
      setHiding(false)
      expandingRef.current = false
    }
  }, [syncExpandedWindowLayout])
  expandFnRef.current = expand
  const updateModalWindowAdjustedRef = useRef(false)
  const updateModalPrevExpandedRef = useRef(false)

  const ensureUpdateModalWindow = useCallback(async () => {
    if (settingsModeRef.current) return
    if (updateModalWindowAdjustedRef.current) return
    updateModalWindowAdjustedRef.current = true
    updateModalPrevExpandedRef.current = expandedRef.current
    expandedWindowModeRef.current = null
    try {
      await invoke('set_mini_size', { restore: false, position: mascotPositionRef.current, keepOnTop: true, mascotScale: mascotScaleRef.current })
      await new Promise<void>((r) => setTimeout(r, 80))
    } catch {
      updateModalWindowAdjustedRef.current = false
    }
  }, [])

  const restoreCollapsedMascotPosition = useCallback(async () => {
    const pos = customPosRef.current
    if (!pos) return
    try {
      await invoke('set_mini_origin', { x: pos.x, y: pos.y })
    } catch (e) {
      console.warn('[mini] restore custom mascot position failed:', e)
    }
  }, [])

  const restoreWindowAfterUpdateModal = useCallback(async () => {
    if (!updateModalWindowAdjustedRef.current) return
    const wasExpanded = updateModalPrevExpandedRef.current
    updateModalWindowAdjustedRef.current = false
    if (settingsModeRef.current) return
    try {
      if (wasExpanded) {
        await syncExpandedWindowLayout(viewModeRef.current)
        setExpanded(true)
        expandedRef.current = true
        setShowPanel(true)
      } else {
        await invoke('set_mini_size', { restore: true, position: mascotPositionRef.current, mascotScale: mascotScaleRef.current, largeMascot: largeMascotRef.current, largeMascotScale: largeMascotScaleRef.current })
        await restoreCollapsedMascotPosition()
        setExpanded(false)
        expandedRef.current = false
        expandedWindowModeRef.current = null
        setShowPanel(false)
      }
    } catch {}
  }, [restoreCollapsedMascotPosition, syncExpandedWindowLayout])

  const openAvailableUpdateModal = useCallback(
    async (info: UpdateModalInfo) => {
      if (settingsModeRef.current || settingsTransitioningRef.current || isCreateModalOpenRef.current) {
        pendingUpdateInfoRef.current = info
        return
      }
      setUpdateModalInfo(info)
      setUpdateModalPhase('available')
      setUpdateModalProgress(null)
      setUpdateModalProgressStage('preparing')
      hoverExpandedRef.current = false
      if (hoverCloseTimerRef.current) {
        clearTimeout(hoverCloseTimerRef.current)
        hoverCloseTimerRef.current = null
      }
      setEffListCollapsed(true)
      await ensureUpdateModalWindow()
      updateModalOpenRef.current = true
      setUpdateModalOpen(true)
    },
    [ensureUpdateModalWindow],
  )

  const closeUpdateModal = useCallback(() => {
    updateModalRunOwnedRef.current = false
    updateModalOpenRef.current = false
    setUpdateModalOpen(false)
    void restoreWindowAfterUpdateModal()
  }, [restoreWindowAfterUpdateModal])

  const skipCurrentUpdateVersion = useCallback(async () => {
    if (!updateModalInfo?.latest) return
    const store = await load('settings.json', { defaults: {}, autoSave: true })
    await store.set('skipped_update_version', updateModalInfo.latest)
    await store.save()
    updateModalRunOwnedRef.current = false
    updateModalOpenRef.current = false
    setUpdateModalOpen(false)
    void restoreWindowAfterUpdateModal()
  }, [restoreWindowAfterUpdateModal, updateModalInfo?.latest])

  const runUpdateFromModal = useCallback(async () => {
    if (!updateModalInfo?.url) return
    setUpdateModalPhase('downloading')
    setUpdateModalProgress(0)
    setUpdateModalProgressStage('preparing')
    updateModalRunOwnedRef.current = true
    hoverExpandedRef.current = false
    if (hoverCloseTimerRef.current) {
      clearTimeout(hoverCloseTimerRef.current)
      hoverCloseTimerRef.current = null
    }
    setEffListCollapsed(true)
    await ensureUpdateModalWindow()
    updateModalOpenRef.current = true
    setUpdateModalOpen(true)
    try {
      await invoke('run_update', { dmgUrl: updateModalInfo.url })
    } catch (e) {
      console.warn('[update modal] run_update failed:', e)
      setUpdateModalPhase('available')
    }
  }, [ensureUpdateModalWindow, updateModalInfo?.url])

  const restartFromModal = useCallback(() => {
    invoke('exit_app').catch(() => {})
  }, [])

  useEffect(() => {
    let cancelled = false
    const checkForUpdates = async () => {
      try {
        const store = await load('settings.json', { defaults: {}, autoSave: true })
        const now = Date.now()
        const lastCheckedAt = Number((await store.get('update_last_check_at')) ?? 0)
        if (lastCheckedAt > 0 && now - lastCheckedAt < 86_400_000) return
        await store.set('update_last_check_at', now)
        await store.save()
        const info = (await invoke('check_for_update', { lang: i18n.language })) as UpdateModalInfo
        if (!info?.hasUpdate) return
        const skippedVersion = ((await store.get('skipped_update_version')) as string) || ''
        if (skippedVersion && skippedVersion === info.latest) return
        if (cancelled) return
        openAvailableUpdateModal(info)
      } catch {
        /* ignore */
      }
    }
    void checkForUpdates()
    const timer = setInterval(() => {
      void checkForUpdates()
    }, 86_400_000)
    return () => {
      cancelled = true
      clearInterval(timer)
    }
  }, [i18n.language, openAvailableUpdateModal])

  useEffect(() => {
    const unlisten = listen<UpdateProgressPayload>('update-progress', (event) => {
      if (!updateModalOpenRef.current && !updateModalRunOwnedRef.current) return
      const payload = event.payload
      const stage = payload.stage || 'downloading'
      setUpdateModalProgress(typeof payload.progress === 'number' ? payload.progress : null)
      setUpdateModalProgressStage(stage)
      if (stage === 'ready_to_restart') {
        setUpdateModalPhase('ready_to_restart')
      } else if (stage === 'preparing' || stage === 'downloading' || stage === 'downloaded') {
        setUpdateModalPhase('downloading')
      }
      if (!updateModalOpenRef.current) {
        void (async () => {
          await ensureUpdateModalWindow()
          updateModalOpenRef.current = true
          setUpdateModalOpen(true)
        })()
      }
    })
    return () => {
      unlisten.then((fn) => fn())
    }
  }, [ensureUpdateModalWindow])

  useEffect(() => {
    if (updateModalOpenRef.current) return
    if (settingsMode || settingsTransitioning || isCreateModalOpen) return
    const pending = pendingUpdateInfoRef.current
    if (!pending) return
    pendingUpdateInfoRef.current = null
    void openAvailableUpdateModal(pending)
  }, [isCreateModalOpen, openAvailableUpdateModal, settingsMode, settingsTransitioning])

  useEffect(() => {
    if (!updateModalOpen || updateModalPhase !== 'available' || !updateModalInfo?.hasUpdate) return
    let cancelled = false
    ;(async () => {
      try {
        const latest = (await invoke('check_for_update', { lang: i18n.language })) as UpdateModalInfo
        if (cancelled) return
        if (latest?.hasUpdate && latest.latest === updateModalInfo.latest) {
          setUpdateModalInfo(latest)
        }
      } catch {
        /* ignore */
      }
    })()
    return () => {
      cancelled = true
    }
  }, [i18n.language, updateModalInfo?.hasUpdate, updateModalInfo?.latest, updateModalOpen, updateModalPhase])

  useEffect(() => {
    return () => {
      if (largeActionTimerRef.current) clearTimeout(largeActionTimerRef.current)
    }
  }, [])

  const handleMascotContextMenu = useCallback((e: React.MouseEvent) => {
    e.preventDefault()
  }, [])


  const handleMascotPointerDown = useCallback(
    (e: React.PointerEvent) => {
      // Pet mode: right-click / ctrl+click toggles context menu
      const isRightClick = e.button === 2 || (e.button === 0 && e.ctrlKey)
      if (isRightClick && appModeRef.current === 'pet' && largeMascotRef.current) {
        e.preventDefault()
        e.stopPropagation()
        if (petContextMenuTransitionRef.current) return
        if (!petContextMenuOpenRef.current) {
          petContextMenuTransitionRef.current = true
          const winW = petBaseWinWRef.current ?? window.innerWidth ?? 300
          const mascotW = MASCOT_BASE_SIZE * mascotScaleRef.current * largeMascotScaleRef.current
          Promise.all([
            invoke('get_mini_origin'),
            invoke('get_mini_monitor_rect'),
          ]).then(async ([pos, rect]) => {
            const [x] = pos as [number, number]
            const [monitorX, , monitorW] = rect as [number, number, number, number]
            const monitorMid = monitorX + monitorW / 2
            const mascotLeft = x + winW - mascotW
            const side = mascotLeft < monitorMid ? 'right' : 'left'
            setPetMenuSide(side)
            setPetContextMenuOpen(true)
            petContextMenuOpenRef.current = true
            await invoke('set_pet_context_menu', { open: true, side }).catch(() => {})
          }).catch(() => {}).finally(() => {
            petContextMenuTransitionRef.current = false
          })
        } else {
          void closePetContextMenu()
        }
        return
      }
      // Coding mode: always expand immediately on click, independent of mascot size.
      // This avoids pointerup/drag timing races on Windows.
      if (!moveModeRef.current && appModeRef.current !== 'pet') {
        hoverExpandedRef.current = false
        setCompletionSessionId(null)
        expand()
        return
      }
      if (e.button !== 0 || e.ctrlKey || collapsingRef.current) return
      const isMoveMode = moveModeRef.current
      const target = e.currentTarget as HTMLElement
      // Keep large mascot visual size unchanged, but shrink the effective hitbox.
      // Skip inset check when peeking at edge so the visible slice stays clickable.
      if (!isMoveMode && largeMascotRef.current && currentPetActionRef.current !== 'peek') {
        const visualSize = MASCOT_BASE_SIZE * mascotScaleRef.current * largeMascotScaleRef.current
        const hitWidth = MASCOT_BASE_SIZE * mascotScaleRef.current * (LARGE_MASCOT_HITBOX_WIDTH_MULTIPLIER / 3 * largeMascotScaleRef.current)
        const hitHeight = MASCOT_BASE_SIZE * mascotScaleRef.current * (LARGE_MASCOT_HITBOX_HEIGHT_MULTIPLIER / 3 * largeMascotScaleRef.current)
        const insetX = Math.max(0, (visualSize - hitWidth) / 2)
        const insetY = Math.max(0, (visualSize - hitHeight) / 2)
        const rect = target.getBoundingClientRect()
        const localX = e.clientX - rect.left
        const localY = e.clientY - rect.top
        if (localX < insetX || localX > visualSize - insetX || localY < insetY || localY > visualSize - insetY) return
      }

      // Large mascot normal mode: drag to move window, click for angry/spin
      // Block drag and clicks during pomodoro
      if (!isMoveMode && largeMascotRef.current && pomodoroRef.current?.active) return
      if (!isMoveMode && largeMascotRef.current) {
        e.preventDefault()
        mascotDragActiveRef.current = true
        let lastX = e.screenX
        let lastY = e.screenY
        let dragging = false
        const pid = e.pointerId

        const onMove = (ev: PointerEvent) => {
          if (ev.pointerId !== pid) return
          if (!dragging) {
            if (Math.abs(ev.screenX - lastX) + Math.abs(ev.screenY - lastY) >= 3) {
              dragging = true
              setLargePetAction('grasp')
              largePetActionRef.current = 'grasp'
              playPetAudio('grasp')
              if (appModeRef.current === 'pet') {
                setCurrentPetAction('grasp')
                currentPetActionRef.current = 'grasp'
              }
            } else return
          }
          const dx = ev.screenX - lastX
          const dy = ev.screenY - lastY
          lastX = ev.screenX
          lastY = ev.screenY
          if (dx !== 0 || dy !== 0) invoke('move_mini_by', { dx, dy })
        }

        const cleanup = () => {
          mascotDragActiveRef.current = false
          if (largePetActionRef.current === 'grasp') {
            setLargePetAction(null)
            largePetActionRef.current = null
            if (appModeRef.current === 'pet') {
              checkEdgeAndSetPeek()
            }
          }
          window.removeEventListener('pointermove', onMove)
          window.removeEventListener('pointerup', onUp)
          window.removeEventListener('pointercancel', onCancelLarge)
        }

        const onCancelLarge = (ev: PointerEvent) => {
          if (ev.pointerId !== pid) return
          cleanup()
        }

        const onUp = (ev: PointerEvent) => {
          if (ev.pointerId !== pid) return
          cleanup()
          if (dragging) {
            invoke('get_mini_origin').then(async (pos) => {
              const [x, y] = pos as [number, number]
              customPosRef.current = { x, y }
              const store = await load('settings.json', { defaults: {}, autoSave: true })
              await store.set('mini_custom_pos', { x, y })
              await store.save()
            })
          } else {
            if (appModeRef.current === 'pet') {
              if (currentPetActionRef.current === 'peek') {
                handleSetPetAction('walkout')
                playPetAudio('walkout')
              } else {
                // Pet mode click: headpat based on affection tier
                const tier = getAffectionTier(petDataRef.current.affection)
                if (tier === 'angry') {
                  handleSetPetAction('angry')
                  playPetAudio('angry')
                } else if (tier === 'shy') {
                  const updated = applyHeadpat(petDataRef.current)
                  handleUpdatePetData(updated)
                  handleSetPetAction('spin')
                  playPetAudio('spin')
                } else {
                  const updated = applyHeadpat(petDataRef.current)
                  handleUpdatePetData(updated)
                  handleSetPetAction('headpat')
                  playPetAudio('headpat')
                }
              }
            } else {
              // Coding mode should open panel on click, regardless of large mascot size.
              hoverExpandedRef.current = false
              setCompletionSessionId(null)
              expand()
            }
          }
        }

        window.addEventListener('pointermove', onMove)
        window.addEventListener('pointerup', onUp, { once: true })
        window.addEventListener('pointercancel', onCancelLarge, { once: true })
        return
      }

      // Normal mode (small mascot): click to expand (coding mode only)
      if (!isMoveMode) {
        if (appModeRef.current === 'pet') return // no panel in pet mode
        hoverExpandedRef.current = false
        setCompletionSessionId(null)
        expand()
        return
      }

      // Move mode: drag to reposition, click to exit.
      // Listen on `window` instead of the element so that pointer events
      // keep arriving even when macOS blurs/refocuses the webview during
      // a native window move (which kills pointer capture and fires
      // lostpointercapture, aborting the first drag attempt).
      e.preventDefault()
      mascotDragActiveRef.current = true

      let lastX = e.screenX
      let lastY = e.screenY
      let dragging = false
      const pid = e.pointerId

      const onMove = (ev: PointerEvent) => {
        if (ev.pointerId !== pid) return
        if (!dragging) {
          if (Math.abs(ev.screenX - lastX) + Math.abs(ev.screenY - lastY) >= MOVE_DRAG_THRESHOLD) {
            dragging = true
            if (largeMascotRef.current) {
              setLargePetAction('grasp')
              largePetActionRef.current = 'grasp'
              if (appModeRef.current === 'pet') {
                setCurrentPetAction('grasp')
                currentPetActionRef.current = 'grasp'
              }
            }
          } else return
        }
        const dx = ev.screenX - lastX
        const dy = ev.screenY - lastY
        lastX = ev.screenX
        lastY = ev.screenY
        if (dx !== 0 || dy !== 0) invoke('move_mini_by', { dx, dy })
      }

      const cleanup = () => {
        mascotDragActiveRef.current = false
        if (largeMascotRef.current && largePetActionRef.current === 'grasp') {
          setLargePetAction(null)
          largePetActionRef.current = null
          if (appModeRef.current === 'pet') {
            checkEdgeAndSetPeek()
          }
        }
        window.removeEventListener('pointermove', onMove)
        window.removeEventListener('pointerup', onUp)
        window.removeEventListener('pointercancel', onCancel)
      }

      const onCancel = (ev: PointerEvent) => {
        if (ev.pointerId !== pid) return
        cleanup()
      }

      const onUp = (ev: PointerEvent) => {
        if (ev.pointerId !== pid) return
        cleanup()
        if (!dragging) {
          // Ignore accidental click-up right after entering move mode.
          if (Date.now() - moveModeActivatedAtRef.current < 700) return
          setMoveMode(false)
          document.body.style.cursor = 'pointer'
          requestAnimationFrame(() => {
            document.body.style.cursor = ''
          })
        } else {
          invoke('get_mini_origin').then(async (pos) => {
            const [x, y] = pos as [number, number]
            customPosRef.current = { x, y }
            const store = await load('settings.json', { defaults: {}, autoSave: true })
            await store.set('mini_custom_pos', { x, y })
            await store.save()
          })
        }
      }

      window.addEventListener('pointermove', onMove)
      window.addEventListener('pointerup', onUp)
      window.addEventListener('pointercancel', onCancel)
    },
    [expand],
  )

  const enterMoveMode = useCallback(async () => {
    moveModeRef.current = true
    mascotDragActiveRef.current = true
    // Immediately hide everything, skip the panel close animation so the
    // user doesn't see a 350ms delay followed by a position jump.
    setShowPanel(false)
    setHiding(true)
    setExpanded(false)
    expandedRef.current = false
    expandedWindowModeRef.current = null
    try {
      // Wait one frame so React unmounts the expanded tree before we
      // resize the native window.
      await new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())))
      await invoke('set_mini_expanded', {
        expanded: false,
        position: mascotPositionRef.current,
        efficiency: viewModeRef.current === 'efficiency',
        mascotScale: mascotScaleRef.current,
        largeMascot: largeMascotRef.current,
        largeMascotScale: largeMascotScaleRef.current,
      })
      await restoreCollapsedMascotPosition()
    } catch {}
    setMoveMode(true)
    setHiding(false)
    setTimeout(() => {
      if (moveModeRef.current) mascotDragActiveRef.current = false
    }, 500)
  }, [restoreCollapsedMascotPosition])

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
    setShowClaudeStats(false)
    const wasSettings = settingsModeRef.current
    if (wasSettings) {
      setShowSettingsOverlay(false)
      setSettingsTransitioning(true)
    }
    const delay = isWindowsPlatform ? 150 : wasSettings ? 280 : 480
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
          setEnableCodex(isWindowsPlatform ? false : cod !== false)
          const cur = await store.get('enable_cursor')
          setEnableCursor(isWindowsPlatform ? false : cur !== false)
        } catch {}
        // Trigger immediate refresh so config changes are reflected right away.
        fetchAgents()
      }
      setHiding(true)
      // Unmount the expanded React tree before shrinking/repositioning the
      // native Tauri window. Otherwise the last frames of the expanded panel
      // can render inside the collapsed window geometry and look like a flicker.
      setExpanded(false)
      expandedRef.current = false
      expandedWindowModeRef.current = null
      try {
        await new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())))
        if (wasSettings && appModeRef.current === 'pet' && largeMascotRef.current) {
          await invoke('set_mini_expanded', { expanded: false, position: mascotPositionRef.current, efficiency: true, mascotScale: mascotScaleRef.current, largeMascot: true, largeMascotScale: largeMascotScaleRef.current }).catch(() => {})
          await invoke('set_pet_mode_window', { active: true, mascotScale: mascotScaleRef.current, largeMascotScale: largeMascotScaleRef.current }).catch(() => {})
          if (petOriginBeforeSettingsRef.current) {
            const [x, y] = petOriginBeforeSettingsRef.current
            await invoke('set_mini_origin', { x, y }).catch(() => {})
            petOriginBeforeSettingsRef.current = null
          }
        } else if (wasSettings) {
          await invoke('set_mini_size', { restore: true, position: mascotPositionRef.current, mascotScale: mascotScaleRef.current, largeMascot: largeMascotRef.current, largeMascotScale: largeMascotScaleRef.current })
          await restoreCollapsedMascotPosition()
        } else {
          await invoke('set_mini_expanded', {
            expanded: false,
            position: mascotPositionRef.current,
            efficiency: viewModeRef.current === 'efficiency',
            mascotScale: mascotScaleRef.current,
            largeMascot: largeMascotRef.current,
            largeMascotScale: largeMascotScaleRef.current,
          })
          await restoreCollapsedMascotPosition()
        }
      } catch {
        /* ensure hiding is always cleared */
      }
      // Even if the native resize invoke fails, always clear the hiding shell so
      // the mascot becomes visible again instead of getting stuck transparent.
      setHiding(false)
      setSettingsTransitioning(false)
      // Brief cooldown to prevent focus event from immediately re-expanding
      setTimeout(() => {
        collapsingRef.current = false
        settingsTransitioningRef.current = false
      }, 300)
    }, delay)
  }, [fetchAgents, restoreCollapsedMascotPosition])

  // ── Efficiency-mode notch hover tracking (native cursor polling) ──
  // On macOS the mini window sits in the menu-bar / notch area where the
  // system intercepts mouse events, so web-level onMouseEnter never fires.
  // A Rust-side 50ms poll of NSEvent.mouseLocation emits "efficiency-hover"
  // events which we handle here to open / close the panel on hover.
  useEffect(() => {
    if (appMode === 'pet') {
      invoke('set_efficiency_hover_tracking', { active: false }).catch(() => {})
    } else if (viewMode === 'efficiency' && !moveMode && !updateModalOpen && !settingsMode && !settingsTransitioning) {
      invoke('set_efficiency_hover_tracking', { active: true }).catch(() => {})
    } else {
      invoke('set_efficiency_hover_tracking', { active: false }).catch(() => {})
    }
    return () => {
      invoke('set_efficiency_hover_tracking', { active: false }).catch(() => {})
    }
  }, [viewMode, moveMode, updateModalOpen, settingsMode, settingsTransitioning, appMode])

  useEffect(() => {
    if (viewMode !== 'efficiency' || appMode === 'pet') return
    const unlisten = listen<boolean>('efficiency-hover', (event) => {
      if (settingsModeRef.current || settingsTransitioningRef.current) {
        return
      }
      if (updateModalOpenRef.current) {
        if (hoverCloseTimerRef.current) {
          clearTimeout(hoverCloseTimerRef.current)
          hoverCloseTimerRef.current = null
        }
        return
      }
      if (event.payload) {
        if (hoverCloseTimerRef.current) {
          clearTimeout(hoverCloseTimerRef.current)
          hoverCloseTimerRef.current = null
        }
        if (!expandedRef.current && !collapsingRef.current && !expandingRef.current && !moveModeRef.current && !hoverOpenTimerRef.current) {
          const delayMs = Math.round(hoverDelayRef.current * 1000)
          if (delayMs <= 0) {
            hoverExpandedRef.current = true
            expandFnRef.current?.()
          } else {
            hoverOpenTimerRef.current = setTimeout(() => {
              hoverOpenTimerRef.current = null
              if (!expandedRef.current && !collapsingRef.current && !expandingRef.current && !moveModeRef.current) {
                hoverExpandedRef.current = true
                expandFnRef.current?.()
              }
            }, delayMs)
          }
        }
      } else {
        if (hoverOpenTimerRef.current) {
          clearTimeout(hoverOpenTimerRef.current)
          hoverOpenTimerRef.current = null
        }
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
      if (hoverOpenTimerRef.current) {
        clearTimeout(hoverOpenTimerRef.current)
        hoverOpenTimerRef.current = null
      }
    }
  }, [viewMode, collapse, appMode])

  const petOriginBeforeSettingsRef = useRef<[number, number] | null>(null)

  const enterSettings = useCallback(async () => {
    if (settingsModeRef.current || settingsTransitioningRef.current) return
    hoverExpandedRef.current = false
    settingsTransitioningRef.current = true
    setSelectedAgentId(null)
    setSelectedClaudeSession(null)
    setSelectedSessionKey(null)
    setShowClaudeStats(false)
    setSettingsNav(appModeRef.current === 'pet' ? 'settings' : 'pairing')
    // Save pet mode window origin so we can restore it exactly
    if (appModeRef.current === 'pet') {
      try {
        const pos = await invoke('get_mini_origin') as [number, number]
        petOriginBeforeSettingsRef.current = pos
      } catch {}
    }
    setShowSettingsOverlay(false)
    setSettingsTransitioning(true)
    // Stop pet passthrough poll before resizing to settings mode
    if (appModeRef.current === 'pet') {
      await invoke('set_pet_mode_window', { active: false, mascotScale: mascotScaleRef.current, largeMascotScale: largeMascotScaleRef.current }).catch(() => {})
    }
    await new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())))
    try {
      await invoke('set_mini_size', { restore: false, position: mascotPositionRef.current, mascotScale: mascotScaleRef.current })
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
    try {
      await new Promise<void>((r) => setTimeout(r, 220))
      setSettingsTransitioning(true)
      settingsModeRef.current = false
      setSettingsMode(false)
      setSettingsNav('pairing')
      // Always return to collapsed visual state when leaving settings.
      setShowPanel(false)
      setExpanded(false)
      expandedRef.current = false
      expandedWindowModeRef.current = null
      if (appModeRef.current === 'pet' && largeMascotRef.current) {
        // Pet mode: restore pet-sized window directly, skip syncExpandedWindowLayout
        await invoke('set_mini_expanded', { expanded: false, position: mascotPositionRef.current, efficiency: true, mascotScale: mascotScaleRef.current, largeMascot: true, largeMascotScale: largeMascotScaleRef.current }).catch(() => {})
        await invoke('set_pet_mode_window', { active: true, mascotScale: mascotScaleRef.current, largeMascotScale: largeMascotScaleRef.current }).catch(() => {})
        // Restore exact window position saved before entering settings
        if (petOriginBeforeSettingsRef.current) {
          const [x, y] = petOriginBeforeSettingsRef.current
          await invoke('set_mini_origin', { x, y }).catch(() => {})
          petOriginBeforeSettingsRef.current = null
        }
      } else {
        // Coding mode: re-sync feature toggles and restore layout
        const store = await load('settings.json', { defaults: {}, autoSave: true })
        const cc = await store.get('enable_claudecode')
        setEnableClaudeCode(cc !== false)
        const cod = await store.get('enable_codex')
        setEnableCodex(isWindowsPlatform ? false : cod !== false)
        const cur = await store.get('enable_cursor')
        setEnableCursor(isWindowsPlatform ? false : cur !== false)
        fetchAgents()
        try {
          await syncExpandedWindowLayout(viewModeRef.current)
        } catch {}
      }
    } finally {
      // Always clear transition/hiding guards so mascot can't get stuck invisible.
      setSettingsTransitioning(false)
      settingsTransitioningRef.current = false
      setHiding(false)
    }
  }, [fetchAgents, syncExpandedWindowLayout])

  // Click outside to collapse (only when not pinned)
  useEffect(() => {
    if (!expanded || pinned || settingsMode || settingsTransitioning || updateModalOpen) return
    const onClick = (e: MouseEvent) => {
      if (isCreateModalOpenRef.current) return
      if (!(e.target as HTMLElement).closest('#mini-panel')) collapse()
    }
    window.addEventListener('mousedown', onClick)
    return () => window.removeEventListener('mousedown', onClick)
  }, [expanded, pinned, settingsMode, settingsTransitioning, updateModalOpen, collapse])

  // Window blur: collapse when user clicks outside the app (when not pinned, or in settings mode)
  // Skip blur when a file picker dialog is open
  useEffect(() => {
    if (updateModalOpen) return
    // In pet mode settings, expanded stays false; still need blur handling
    // so clicking desktop can close settings via exitSettings.
    if (!expanded && !settingsMode) return
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
      // When settings is open, use the dedicated close path so pet mode
      // restores window geometry/state consistently.
      if (settingsModeRef.current) {
        exitSettings()
        return
      }
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
  }, [expanded, pinned, settingsMode, updateModalOpen, collapse, exitSettings])

  useEffect(() => {
    if (expanded || moveMode || updateModalOpen) return
    const onFocus = () => {
      if (appModeRef.current === 'pet') return // no auto-expand in pet mode
      if (collapsingRef.current || moveModeRef.current || mascotDragActiveRef.current) return
      // Large mascot uses long-press to expand; auto-expand on focus
      // would race with the pointerdown handler and steal the click.
      if (largeMascotRef.current) return
      expand()
    }
    window.addEventListener('focus', onFocus)
    return () => window.removeEventListener('focus', onFocus)
  }, [expanded, expand, moveMode, updateModalOpen])

  // Exit move mode when clicking outside mascot or when window loses focus.
  // Use a debounced blur so that programmatic window moves (which briefly
  // blur + refocus the webview on macOS) don't cancel move mode.
  useEffect(() => {
    if (!moveMode) return
    let blurTimer: ReturnType<typeof setTimeout> | null = null
    const onBlur = () => {
      if (mascotDragActiveRef.current) return
      blurTimer = setTimeout(() => {
        blurTimer = null
        if (!mascotDragActiveRef.current) setMoveMode(false)
      }, 300)
    }
    const onFocus = () => {
      if (blurTimer) {
        clearTimeout(blurTimer)
        blurTimer = null
      }
    }
    window.addEventListener('blur', onBlur)
    window.addEventListener('focus', onFocus)
    return () => {
      window.removeEventListener('blur', onBlur)
      window.removeEventListener('focus', onFocus)
      if (blurTimer) clearTimeout(blurTimer)
    }
  }, [moveMode])

  const claudeWaiting = claudeSessions.some((cs) => cs.status === 'waiting')
  const claudeCompacting = claudeSessions.some((cs) => cs.status === 'compacting')
  const claudeWorking = claudeSessions.some((cs) => cs.status === 'processing' || cs.status === 'tool_running')
  const hasWorking = anySessionActive || Object.values(healthMap).some(Boolean) || claudeWorking || claudeCompacting || claudeWaiting
  // Priority: waiting > compacting > working > idle
  const mainPetState: PetState = claudeWaiting ? 'waiting' : claudeCompacting ? 'compacting' : hasWorking ? 'working' : 'idle'
  const miniGif = getMiniGif(miniChar ?? undefined, mainPetState, true)
  const fallbackLargeActions = useMemo(() => {
    const c = characters.find((ch) => ch.largeActions && Object.keys(ch.largeActions).length > 0)
    return c?.largeActions
  }, [characters])
  const largeCharForRender = (appMode === 'pet' || appMode === 'coding')
    ? ({ name: '香企鹅', largeActions: petBuiltinLargeActions } as CharacterMeta)
    : miniChar
  const hasAnyLargeActions = !!(largeCharForRender?.largeActions && Object.keys(largeCharForRender.largeActions).length > 0) || !!(fallbackLargeActions && Object.keys(fallbackLargeActions).length > 0)
  const showLargeMascotToggle = !isWindowsPlatform && (appMode === 'coding' || (appMode !== 'pet' && hasAnyLargeActions))
  const largeVideoBaseUrl = largeMascot
    ? appMode === 'pet'
      ? getLargeVideoPetMode(largeCharForRender ?? undefined, currentPetAction, fallbackLargeActions)
      : getLargeVideo(largeCharForRender ?? undefined, mainPetState, largePetAction, fallbackLargeActions)
    : undefined
  const largeVideoUrl = largeVideoBaseUrl ? `${largeVideoBaseUrl}?rev=alpha-fix-2` : undefined
  // Double-buffer video: two stacked <video> elements swap roles on each
  // animation change. The old video stays visible until the new one's first
  // frame is decoded (onLoadedData), eliminating blank-frame flicker.
  // vid.load() clears the frame buffer immediately, so a single-element
  // approach always flashes transparent between animations.
  const largeVideoRefA = useRef<HTMLVideoElement>(null)
  const largeVideoRefB = useRef<HTMLVideoElement>(null)
  const largeVideoCanvasRef = useRef<HTMLCanvasElement>(null)
  // Which buffer (0=A, 1=B) is currently the *front* (visible, playing) video
  const activeBufferRef = useRef<0 | 1>(0)
  const [activeBuffer, setActiveBuffer] = useState<0 | 1>(0)
  const prevLargeVideoUrlRef = useRef<string | undefined>(undefined)
  const useWindowsChromaKey = isWindowsPlatform && !!largeVideoBaseUrl && largeVideoBaseUrl.includes('/large/webm/')

  useEffect(() => {
    if (!largeVideoUrl) {
      // No video to show — reset tracking so we load fresh when a URL
      // appears (e.g. switching back to pet mode restores the same URL).
      prevLargeVideoUrlRef.current = undefined
      return
    }

    const frontIdx = activeBufferRef.current
    const backIdx: 0 | 1 = frontIdx === 0 ? 1 : 0
    const front = frontIdx === 0 ? largeVideoRefA.current : largeVideoRefB.current
    const back = backIdx === 0 ? largeVideoRefA.current : largeVideoRefB.current
    if (!front || !back) {
      // Video elements not yet in the DOM (e.g. collapsed view hidden
      // while panel is expanded for settings). Reset URL tracker so we
      // retry loading when the elements remount.
      prevLargeVideoUrlRef.current = undefined
      return
    }

    if (prevLargeVideoUrlRef.current === largeVideoUrl) return

    const allowAlternateFormatFallback = !(typeof navigator !== 'undefined' && navigator.userAgent.includes('Windows'))

    const isFirstLoad = prevLargeVideoUrlRef.current === undefined
    prevLargeVideoUrlRef.current = largeVideoUrl

    let cancelled = false
    const listeners: Array<() => void> = []
    const addOnce = (el: HTMLVideoElement, event: 'playing' | 'error', fn: () => void) => {
      el.addEventListener(event, fn, { once: true })
      listeners.push(() => el.removeEventListener(event, fn))
    }
    const clearListeners = () => {
      for (const off of listeners) off()
      listeners.length = 0
    }
    const finishSwap = (newFront: 0 | 1) => {
      if (cancelled) return
      activeBufferRef.current = newFront
      setActiveBuffer(newFront)
      // Only pause the old buffer — do NOT clear its src synchronously.
      // setActiveBuffer triggers an async React render that sets visibility:hidden,
      // but removeAttribute('src') + load() would clear the frame buffer
      // *before* React hides the element, causing a blank flash.
      // The stale content is safe: the old buffer is hidden, and loadWithFallback
      // will replace its src before it becomes front again.
      const old = newFront === 0 ? largeVideoRefB.current : largeVideoRefA.current
      if (old) {
        old.pause()
      }
    }
    const loadWithFallback = (
      target: HTMLVideoElement,
      url: string,
      allowFallback: boolean,
      onReady: () => void,
      onFailed: () => void,
    ) => {
      clearListeners()
      const ready = () => {
        clearListeners()
        onReady()
      }
      const failed = () => {
        clearListeners()
        if (cancelled) return
        if (allowFallback) {
          const alt = getAlternateLargeVideoUrl(url)
          if (alt && alt !== url) {
            loadWithFallback(target, alt, false, onReady, onFailed)
            return
          }
        }
        onFailed()
      }
      addOnce(target, 'playing', ready)
      addOnce(target, 'error', failed)
      target.currentTime = 0
      target.src = url
      target.load()
      target.play().catch(() => {})
    }

    if (isFirstLoad) {
      loadWithFallback(front, largeVideoUrl, allowAlternateFormatFallback, () => {}, () => {})
      return () => {
        cancelled = true
        clearListeners()
      }
    }

    // Keep current front visible, preload next on back, swap only after next plays.
    loadWithFallback(back, largeVideoUrl, allowAlternateFormatFallback, () => finishSwap(backIdx), () => {})
    return () => {
      cancelled = true
      clearListeners()
    }
  }, [largeVideoUrl, expanded])

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
  const inDetailPage = inAgentDetail || selectedClaudeSession !== null || selectedSessionKey !== null || showClaudeStats
  const detailPageMaxHeight = typeof window !== 'undefined' ? Math.max(240, Math.floor(((window.screen?.availHeight || 800) * 0.75) / Math.max(uiScale, 0.01))) : 600

  // Panel dimensions — CSS uses fixed base sizes; on Windows high-DPI screens
  // the panel root applies `zoom: uiScale` so all content scales uniformly.
  const panelW = viewMode === 'efficiency' ? 575 : 475
  const closedNotchWidth = 44
  const closedNotchHeight = 10
  const openClipPath = 'inset(0 0 0 0 round 0 0 24px 24px)'
  const closedClipPath = isWindowsPlatform
    ? 'inset(0 0 100% 0)'
    : `inset(0 calc(50% - ${closedNotchWidth / 2}px) calc(100% - ${closedNotchHeight}px) calc(50% - ${closedNotchWidth / 2}px) round 0 0 8px 8px)`
  const panelClipTransition = isWindowsPlatform
    ? 'clip-path 0.12s ease-out, box-shadow 0.12s ease-out'
    : 'clip-path 0.42s cubic-bezier(0.16, 1, 0.3, 1), box-shadow 0.32s cubic-bezier(0.16, 1, 0.3, 1)'
  const panelChromeTransition = isWindowsPlatform
    ? 'opacity 0.1s ease-out'
    : 'opacity 0.28s cubic-bezier(0.16, 1, 0.3, 1)'
  const panelRef = useRef<HTMLDivElement>(null)

  const lastResizeHeightRef = useRef(0)
  const resizeTweenFrameRef = useRef<number | null>(null)
  const resizeTweenTargetRef = useRef(0)
  const stopResizeTween = useCallback(() => {
    if (resizeTweenFrameRef.current !== null) {
      cancelAnimationFrame(resizeTweenFrameRef.current)
      resizeTweenFrameRef.current = null
    }
  }, [])
  const pushMiniHeight = useCallback((height: number, maxHeight: number) => {
    invoke('resize_mini_height', { height, maxHeight, animate: false }).catch(() => {})
  }, [])
  const tweenMiniHeight = useCallback(
    (from: number, to: number, maxHeight: number) => {
      stopResizeTween()
      resizeTweenTargetRef.current = to
      const start = performance.now()
      const duration = 240
      const easeOutCubic = (t: number) => 1 - Math.pow(1 - t, 3)
      const step = (now: number) => {
        const progress = Math.min((now - start) / duration, 1)
        const value = from + (to - from) * easeOutCubic(progress)
        lastResizeHeightRef.current = value
        pushMiniHeight(value, maxHeight)
        if (progress < 1 && resizeTweenTargetRef.current === to) {
          resizeTweenFrameRef.current = requestAnimationFrame(step)
          return
        }
        resizeTweenFrameRef.current = null
        lastResizeHeightRef.current = to
        pushMiniHeight(to, maxHeight)
      }
      resizeTweenFrameRef.current = requestAnimationFrame(step)
    },
    [pushMiniHeight, stopResizeTween],
  )
  useEffect(() => {
    if (!expanded || settingsMode || settingsTransitioning || !showPanel) return
    const el = panelRef.current
    if (!el) return
    let first = true
    const ro = new ResizeObserver((entries) => {
      const h = entries[0]?.contentRect.height
      if (h && h > 0) {
        const limit = inDetailPage ? detailPageMaxHeight : panelMaxHeight
        const clamped = Math.min(h * uiScale, limit * uiScale)
        const prev = lastResizeHeightRef.current || clamped
        const delta = Math.abs(clamped - prev)
        const shouldAnimate = !first && inDetailPage && delta > 10
        first = false
        if (shouldAnimate) {
          tweenMiniHeight(prev, clamped, limit)
          return
        }
        stopResizeTween()
        lastResizeHeightRef.current = clamped
        pushMiniHeight(clamped, limit)
      }
    })
    ro.observe(el)
    return () => {
      ro.disconnect()
      stopResizeTween()
    }
  }, [expanded, settingsMode, settingsTransitioning, showPanel, uiScale, panelMaxHeight, inDetailPage, detailPageMaxHeight, pushMiniHeight, stopResizeTween, tweenMiniHeight])

  useEffect(() => {
    if (!expanded || !showPanel || settingsMode || settingsTransitioning || updateModalOpen) return
    if (expandedWindowModeRef.current === viewMode) return
    syncExpandedWindowLayout(viewMode).catch((e) => {
      console.warn('[mini] sync expanded window layout failed:', e)
    })
  }, [expanded, showPanel, settingsMode, settingsTransitioning, updateModalOpen, viewMode, syncExpandedWindowLayout])

  const collapsedMascotSize = Math.round(MASCOT_BASE_SIZE * mascotScale)
  const collapsedPlaceholderRadius = Math.round(10 * mascotScale)
  const collapsedPlaceholderFontSize = Math.max(16, Math.round(16 * mascotScale))
  const collapsedStatusSize = largeMascot ? 6 : 8
  const collapsedStatusBorder = largeMascot ? 1.1 : 1.5
  const largeMascotVisualSize = collapsedMascotSize * largeMascotScale

  useEffect(() => {
    if (!useWindowsChromaKey || !largeMascot) return
    const canvas = largeVideoCanvasRef.current
    if (!canvas) return
    const ctx = canvas.getContext('2d', { willReadFrequently: true })
    if (!ctx) return
    let rafId = 0
    let retryCount = 0
    const draw = () => {
      const front = activeBufferRef.current === 0 ? largeVideoRefA.current : largeVideoRefB.current
      if (front && front.readyState >= 2 && front.videoWidth > 0 && front.videoHeight > 0) {
        retryCount = 0
        const targetSize = Math.max(1, Math.round(largeMascotVisualSize))
        if (canvas.width !== targetSize || canvas.height !== targetSize) {
          canvas.width = targetSize
          canvas.height = targetSize
        }
        ctx.clearRect(0, 0, canvas.width, canvas.height)
        ctx.drawImage(front, 0, 0, canvas.width, canvas.height)
        const frame = ctx.getImageData(0, 0, canvas.width, canvas.height)
        const data = frame.data
        // Chroma key black-ish pixels to transparent as a Windows fallback
        // when WebView2 drops VP9 alpha during decode.
        for (let i = 0; i < data.length; i += 4) {
          const maxRgb = Math.max(data[i], data[i + 1], data[i + 2])
          if (maxRgb <= 12) {
            data[i + 3] = 0
          } else if (maxRgb < 28) {
            const softAlpha = Math.round(((maxRgb - 12) / 16) * 255)
            if (softAlpha < data[i + 3]) data[i + 3] = softAlpha
          }
        }
        ctx.putImageData(frame, 0, 0)
      } else if (front && front.src && front.paused) {
        // Video has a source but isn't playing — kick-start it.
        // This handles cases where play() was called while the element
        // was hidden (e.g. during settingsTransitioning display:none) and
        // WebView2 silently rejected or stalled decoding.
        // Throttle retries to ~2/sec to avoid spamming play().
        retryCount++
        if (retryCount % 30 === 0) front.play().catch(() => {})
      }
      rafId = requestAnimationFrame(draw)
    }
    rafId = requestAnimationFrame(draw)
    return () => {
      cancelAnimationFrame(rafId)
    }
  // `expanded` is included so the rAF loop restarts when the collapsed view
  // mounts (the canvas element is only in the DOM when !expanded).
  }, [useWindowsChromaKey, largeMascot, largeMascotVisualSize, activeBuffer, largeVideoUrl, expanded])

  return (
    <div
      style={{
        width: '100vw',
        height: '100vh',
        background: 'transparent',
        overflow: (appMode === 'pet' && largeMascot) ? 'visible' : 'hidden',
        userSelect: 'none',
      }}
    >
      {/* Collapsed */}
      {!expanded && !hiding && !updateModalOpen && !showOnboarding && (
        <div
          id="mini-panel"
          onMouseEnter={() => {
            // Hover expand disabled — efficiency mode only opens on click.
          }}
          style={{
            width: '100%',
            height: '100%',
            position: 'relative',
            display: (appMode === 'pet' && largeMascot) ? 'block' : 'flex',
            alignItems: (appMode === 'pet' && largeMascot) ? undefined : 'flex-start',
            justifyContent: (appMode === 'pet' && largeMascot) ? undefined : 'center',
            background: (viewMode === 'efficiency' && appMode !== 'pet') ? 'rgba(0,0,0,0.01)' : undefined,
            pointerEvents: 'auto',
          }}
        >
          <div
            onPointerDown={handleMascotPointerDown}
            onContextMenu={handleMascotContextMenu}
            onMouseMove={undefined}
            onMouseLeave={undefined}
            style={{
              position: (appMode === 'pet' && largeMascot) ? 'absolute' : 'relative',
              bottom: (appMode === 'pet' && largeMascot) ? 0 : undefined,
              // Anchor mascot to a fixed window-x equal to the pet-mode
              // window's "no-menu" inner width minus mascot CSS width.
              // This matches the previous `right: 0` visual position but
              // stays constant when the right-side menu temporarily
              // widens the window, avoiding any race between React CSS
              // commit and native window resize.
              // Fallback to `right: 0` until petBaseWinW has been measured.
              left: (appMode === 'pet' && largeMascot && petBaseWinW != null)
                ? Math.max(0, Math.round(petBaseWinW - largeMascotVisualSize))
                : undefined,
              right: (appMode === 'pet' && largeMascot && petBaseWinW == null)
                ? 0
                : undefined,
              overflow: 'visible',
              cursor: moveMode ? 'grab' : 'pointer',
              animation: moveMode ? 'movePulse 1.2s ease-in-out infinite' : 'none',
              display: (appMode === 'pet' && (showSettingsOverlay || settingsTransitioning)) ? 'none' : undefined,
              ...(moveMode
                ? {
                    borderRadius: 12,
                    outline: '2px solid rgba(59,130,246,0.6)',
                    outlineOffset: -2,
                  }
                : {}),
            }}
          >
            {largeMascot && largeVideoUrl ? (
              <div style={{ position: 'relative', width: largeMascotVisualSize, height: largeMascotVisualSize }}>
              {useWindowsChromaKey && (
                <canvas
                  ref={largeVideoCanvasRef}
                  style={{
                    position: 'absolute',
                    inset: 0,
                    width: '100%',
                    height: '100%',
                    pointerEvents: 'none',
                    transform:
                      (currentPetAction === 'walk' && walkFlipped) ? 'scaleX(-1)'
                      : ((currentPetAction === 'peek' || currentPetAction === 'walkout') && peekEdgeRef.current === 'left') ? 'scaleX(-1)'
                      : undefined,
                  }}
                />
              )}
              {/* Double-buffer: front buffer stays visible while back buffer preloads.
                  Swap only after back buffer is already playing to avoid blank frames. */}
              {[0, 1].map((idx) => {
                const isFront = activeBuffer === idx
                const ref = idx === 0 ? largeVideoRefA : largeVideoRefB
                return (
                  <video
                    key={idx}
                    ref={ref}
                    autoPlay={isFront}
                    loop={!(appMode === 'pet' && TRANSIENT_PET_ACTIONS.includes(currentPetAction))}
                    muted
                    playsInline
                    preload="auto"
                    onError={(e) => {
                      if (!isFront) return
                      console.warn('[large-video] error:', (e.target as HTMLVideoElement).error?.message, 'src:', largeVideoUrl)
                      if (appModeRef.current === 'pet' && TRANSIENT_PET_ACTIONS.includes(currentPetActionRef.current)) {
                        const d = petDataRef.current
                        const next: PetAction = d.hunger < 30 ? 'hungry' : 'idle'
                        setCurrentPetAction(next)
                        currentPetActionRef.current = next
                      }
                    }}
                    onEnded={() => {
                      if (!isFront) return
                      if (appModeRef.current === 'pet' && TRANSIENT_PET_ACTIONS.includes(currentPetActionRef.current)) {
                        if (currentPetActionRef.current === 'farewell') {
                          invoke('exit_app').catch(() => {})
                          return
                        }
                        let next: PetAction
                        if (currentPetActionRef.current === 'dance' && danceFromMusicRef.current) {
                          danceFromMusicRef.current = false
                          next = 'music'
                        } else {
                          const d = petDataRef.current
                          next = d.hunger < 30 ? 'hungry' : 'idle'
                        }
                        setCurrentPetAction(next)
                        currentPetActionRef.current = next
                      }
                    }}
                    style={{
                      position: 'absolute',
                      inset: 0,
                      width: '100%',
                      height: '100%',
                      objectFit: 'contain',
                      pointerEvents: 'none',
                      visibility: isFront ? 'visible' : 'hidden',
                      opacity: useWindowsChromaKey ? 0 : 1,
                      transform:
                        (currentPetAction === 'walk' && walkFlipped) ? 'scaleX(-1)'
                        : ((currentPetAction === 'peek' || currentPetAction === 'walkout') && peekEdgeRef.current === 'left') ? 'scaleX(-1)'
                        : undefined,
                    }}
                    draggable={false}
                  />
                )
              })}
            </div>) : miniGif ? (
              disableSleepAnim && mainPetState === 'idle' ? (
                <FrozenImg src={miniGif} style={{ width: collapsedMascotSize, height: collapsedMascotSize, objectFit: 'contain', pointerEvents: 'none' }} draggable={false} />
              ) : mainPetState === 'compacting' ? (
                <IntervalGif src={miniGif} style={{ width: collapsedMascotSize, height: collapsedMascotSize, objectFit: 'contain', pointerEvents: 'none' }} />
              ) : (
                <img src={miniGif} alt="mini" style={{ width: collapsedMascotSize, height: collapsedMascotSize, objectFit: 'contain', pointerEvents: 'none' }} draggable={false} />
              )
            ) : (
              <div
                style={{
                  width: collapsedMascotSize,
                  height: collapsedMascotSize,
                  borderRadius: collapsedPlaceholderRadius,
                  background: 'rgba(0,0,0,0.3)',
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'center',
                  color: '#999',
                  fontSize: collapsedPlaceholderFontSize,
                }}
              >
                ?
              </div>
            )}
            {appMode !== 'pet' && (
              <div
                style={{
                  position: 'absolute',
                  bottom: 0,
                  right: 0,
                  width: collapsedStatusSize,
                  height: collapsedStatusSize,
                  borderRadius: '50%',
                  background: mainPetState === 'waiting' ? '#f59e0b' : hasWorking ? '#2ecc71' : '#777',
                  border: `${collapsedStatusBorder}px solid rgba(0,0,0,0.3)`,
                }}
              />
            )}
            {/* Pomodoro timer overlay (pet mode, study action) */}
            {appMode === 'pet' && largeMascot && pomodoro?.active && (
              <PomodoroOverlay
                pomodoro={pomodoro}
                mascotSize={largeMascotVisualSize}
                onStop={handleStopPomodoro}
              />
            )}
            {/* Pet mode context menu: status bar above + buttons on left */}
            {appMode === 'pet' && largeMascot && (
              <PetContextMenu
                open={petContextMenuOpen}
                petData={petData}
                currentAction={currentPetAction}
                pomodoro={pomodoro}
                mascotSize={largeMascotVisualSize}
                side={petMenuSide}
                onClose={closePetContextMenu}
                onUpdatePetData={handleUpdatePetData}
                onSetAction={(action) => {
                  handleSetPetAction(action)
                  closePetContextMenu()
                }}
                onStartPomodoro={(m) => {
                  handleStartPomodoro(m)
                  closePetContextMenu()
                }}
                onStopPomodoro={() => {
                  handleStopPomodoro()
                  closePetContextMenu()
                }}
                onOpenSettings={() => {
                  closePetContextMenu()
                  enterSettings()
                }}
                onStar={() => {
                  closePetContextMenu()
                  invoke('open_url', { url: 'https://github.com/rainnoon/oc-claw' }).catch(() => {})
                }}
                onFoodRain={triggerFoodRain}
                onPlayAudio={playPetAudio}
                onQuit={() => {
                  closePetContextMenu()
                  handleSetPetAction('farewell')
                }}
              />
            )}

            {/* Food rain effect — rendered outside context menu so it persists after menu closes */}
            {foodRainDrops.length > 0 && (
              <div style={{ position: 'absolute', inset: 0, pointerEvents: 'none', overflow: 'hidden', zIndex: 9999 }}>
                <AnimatePresence>
                  {foodRainDrops.map(drop => (
                    <motion.span
                      key={drop.id}
                      initial={{ y: -30, opacity: 1 }}
                      animate={{ y: '100vh', opacity: 0 }}
                      transition={{ duration: drop.duration, delay: drop.delay, ease: 'easeIn' }}
                      style={{ position: 'absolute', left: `${drop.x}%`, fontSize: drop.size, willChange: 'transform' }}
                    >
                      {drop.emoji}
                    </motion.span>
                  ))}
                </AnimatePresence>
              </div>
            )}

          </div>
        </div>
      )}

      {/* Expanded panel */}
      {expanded && !settingsMode && !settingsTransitioning && !updateModalOpen && (
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
            maxHeight: inDetailPage ? detailPageMaxHeight : panelMaxHeight,
            overflowY: 'hidden',
            overflowX: 'hidden',
            display: 'flex',
            flexDirection: 'column',
            background: '#010101',
            // Keep a real closed notch rectangle at the top-center. This mirrors
            // ping-island's "always-present header" model and makes collapse
            // feel like shrinking inward to the notch, not vanishing upward.
            clipPath: showPanel ? openClipPath : closedClipPath,
            boxShadow: showPanel ? '0 8px 32px rgba(0,0,0,0.8)' : '0 1px 4px rgba(0,0,0,0.18)',
            transition: panelClipTransition,
          }}
        >
          {/* Closed header: geometric anchor for clip-path collapse target (macOS notch only). */}
          {!showPanel && !isWindowsPlatform && (
            <div
              style={{
                position: 'absolute',
                top: 0,
                left: '50%',
                transform: 'translateX(-50%)',
                width: closedNotchWidth,
                height: closedNotchHeight,
                borderBottomLeftRadius: 8,
                borderBottomRightRadius: 8,
                background: '#010101',
                pointerEvents: 'none',
                zIndex: 30,
              }}
            />
          )}

          {/* Top Control Bar — outside the transform wrapper so sticky works correctly */}
          <div
            className="flex items-center justify-between px-4 py-2.5 shrink-0 sticky top-0 z-20 bg-black text-white"
            style={{
              opacity: showPanel ? 1 : 0,
              transition: panelChromeTransition,
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
                onClick={async (e) => {
                  e.stopPropagation()
                  const allOn = soundEnabled || codexSoundEnabled || cursorSoundEnabled
                  const next = !allOn
                  setSoundEnabled(next)
                  setCodexSoundEnabled(next)
                  setCursorSoundEnabled(next)
                  const store = await load('settings.json', { defaults: {}, autoSave: true })
                  await store.set('sound_enabled', next)
                  await store.set('codex_sound_enabled', next)
                  await store.set('cursor_sound_enabled', next)
                  await store.save()
                  if (next) {
                    if (notifySound === 'manbo') new Audio('/audio/manbo.m4a').play().catch(() => {})
                    else playDefaultSound()
                  }
                }}
                className={`transition-colors ${soundEnabled || codexSoundEnabled || cursorSoundEnabled ? 'text-slate-400 hover:text-[#F0D140]' : 'text-slate-600 hover:text-[#F0D140]'}`}
                title={soundEnabled || codexSoundEnabled || cursorSoundEnabled ? t('mini.soundOn') : t('mini.soundOff')}
              >
                {soundEnabled || codexSoundEnabled || cursorSoundEnabled ? <Bell className="w-4 h-4" strokeWidth={2.5} /> : <BellOff className="w-4 h-4" strokeWidth={2.5} />}
              </button>
              {showLargeMascotToggle && (
                <button
                  data-no-drag
                  onClick={async (e) => {
                    e.stopPropagation()
                    const next = !largeMascot
                    setLargeMascot(next)
                    largeMascotRef.current = next
                    if (largeActionTimerRef.current) clearTimeout(largeActionTimerRef.current)
                    largeActionTimerRef.current = null
                    setLargePetAction(null)
                    largePetActionRef.current = null
                    const store = await load('settings.json', { defaults: {}, autoSave: true })
                    await store.set('large_mascot', next)
                    await store.save()
                  }}
                  className={`transition-colors ${largeMascot ? 'text-[#F0D140]' : 'text-slate-400 hover:text-slate-200'}`}
                  title={largeMascot ? '切换到小看板娘' : '切换到大看板娘'}
                >
                  {largeMascot ? <Minimize className="w-4 h-4" strokeWidth={2.5} /> : <Maximize className="w-4 h-4" strokeWidth={2.5} />}
                </button>
              )}
            </div>
            <div className="flex items-center gap-4">
              {appMode !== 'pet' && (
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
              )}
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
              transformOrigin: 'top center',
              transition: panelChromeTransition,
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
                                ...(hasConfiguredOpenClaw ? ['OpenClaw'] : []),
                                ...(enableClaudeCode ? ['Claude Code'] : []),
                                ...(enableCodex ? ['Codex'] : []),
                                ...(enableCursor ? ['Cursor'] : []),
                              ]
                              return (
                                <motion.div initial={{ opacity: 0 }} animate={{ opacity: 1 }} className="text-center py-10 px-4 flex flex-col items-center gap-2.5">
                                  {trackingTargets.length > 0 && <p className="text-slate-500 text-sm font-medium">{t('mini.startTracking', { targets: trackingTargets.join(' / ') })}</p>}
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
                            const formatChannelLabel = (channel?: string) => {
                              const raw = (channel || '').trim()
                              if (!raw) return ''
                              const lower = raw.toLowerCase()
                              if (lower.includes('feishu')) return 'Feishu'
                              if (lower.includes('lark')) return 'Lark'
                              if (lower.includes('telegram')) return 'Telegram'
                              if (lower.includes('discord')) return 'Discord'
                              if (lower.includes('slack')) return 'Slack'
                              if (lower.includes('wechat') || lower.includes('weixin')) return 'WeChat'
                              return raw.charAt(0).toUpperCase() + raw.slice(1)
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
                            const visibleItems = effListCollapsed && hasImportant ? allItems.filter((item) => isImportant(item)) : allItems
                            const hiddenCount = allItems.length - visibleItems.length
                            const elements: React.ReactNode[] = visibleItems.map((item, index) => {
                              if (item.type === 'oc') {
                                const s = item.data
                                const agent = agents.find((a) => a.id === s.agentId)
                                const seq = (agentSeqCount[s.agentId] = (agentSeqCount[s.agentId] || 0) + 1)
                                const agentName = `${agent?.identityEmoji || ''} ${agent?.identityName || s.agentId}`.trim()
                                const charName = agentCharMap[s.agentId]
                                const charMeta = characters.find((c) => c.name === charName)
                                const recentlyDone = !s.active && s.updatedAt && Date.now() - s.updatedAt < 5 * 60 * 1000
                                const showCharGif = s.active || recentlyDone
                                const petState: PetState = s.active ? 'working' : 'idle'
                                const gif = charMeta ? getMiniGif(charMeta, petState) : undefined
                                const title = `${agentName} #${seq}`
                                const subtitle = s.lastUserMsg || ''
                                const timeAgo = formatTimeAgo(s.updatedAt)
                                const isWorking = s.active
                                const openOcDetail = () => {
                                  setSelectedClaudeSession(null)
                                  setSelectedSessionKey(null)
                                  setShowClaudeStats(false)
                                  setSelectedAgentId(s.agentId)
                                }
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
                                    {showCharGif && (
                                      <div
                                        data-no-drag
                                        onClick={(e) => {
                                          e.stopPropagation()
                                          openOcDetail()
                                        }}
                                        className="relative shrink-0 w-10 h-10 flex items-center justify-center cursor-pointer"
                                      >
                                        <div className="absolute inset-0" style={{ left: -16 }} />
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
                                            background: recentlyDone ? '#94a3b8' : '#2ecc71',
                                            border: '1.5px solid rgba(0,0,0,0.3)',
                                          }}
                                        />
                                      </div>
                                    )}
                                    {!showCharGif && (
                                      <div
                                        data-no-drag
                                        onClick={(e) => {
                                          e.stopPropagation()
                                          openOcDetail()
                                        }}
                                        className="relative shrink-0 w-10 h-10 flex items-center justify-center cursor-pointer"
                                      >
                                        <div className="absolute inset-0" style={{ left: -16 }} />
                                        <span className="w-1 h-1 rounded-full bg-slate-600" />
                                      </div>
                                    )}
                                    <div className="flex min-w-0 flex-1 items-center gap-1.5 overflow-hidden" data-no-drag>
                                      {editingSessionTitle === `oc:${s.agentId}:${s.key}` ? (
                                        <input
                                          autoFocus
                                          data-no-drag
                                          className="text-[13px] font-bold bg-transparent border-b border-slate-500 outline-none text-white w-24"
                                          style={{ WebkitUserSelect: 'text', userSelect: 'text' }}
                                          defaultValue={sessionNicknames[`oc:${s.agentId}:${s.key}`] || title}
                                          ref={(el) => {
                                            if (el) {
                                              editingTitleValueRef.current = el.value
                                              editingTitleDefaultRef.current = title
                                            }
                                          }}
                                          onChange={(e) => {
                                            editingTitleValueRef.current = e.target.value
                                          }}
                                          onCompositionStart={() => {
                                            composingRef.current = true
                                          }}
                                          onCompositionEnd={(e) => {
                                            composingRef.current = false
                                            editingTitleValueRef.current = (e.target as HTMLInputElement).value
                                          }}
                                          onBlur={() => {
                                            saveSessionNickname(`oc:${s.agentId}:${s.key}`, editingTitleValueRef.current, title)
                                            setEditingSessionTitle(null)
                                          }}
                                          onKeyDown={(e) => {
                                            if (composingRef.current) return
                                            if (e.key === 'Enter') (e.target as HTMLInputElement).blur()
                                            if (e.key === 'Escape') setEditingSessionTitle(null)
                                          }}
                                          onClick={(e) => e.stopPropagation()}
                                        />
                                      ) : (
                                        <span
                                          className={`min-w-0 max-w-[40%] truncate text-[13px] font-bold cursor-text ${isWorking ? 'text-white' : 'text-slate-300'}`}
                                          onClick={(e) => e.stopPropagation()}
                                          onDoubleClick={(e) => {
                                            e.stopPropagation()
                                            setEditingSessionTitle(`oc:${s.agentId}:${s.key}`)
                                          }}
                                        >
                                          {sessionNicknames[`oc:${s.agentId}:${s.key}`] || title}
                                        </span>
                                      )}
                                      {subtitle && <span className="min-w-0 max-w-[45%] truncate text-[13px] font-normal text-slate-500">· {subtitle}</span>}
                                      {s.lastAssistantMsg && <span className="min-w-0 flex-1 truncate text-[11px] text-white/40">· {s.lastAssistantMsg}</span>}
                                    </div>
                                    <div className="flex items-center gap-2 shrink-0">
                                      {s.channel && <span className="text-[11px] px-2 py-0.5 rounded-md font-normal bg-[#27272a] text-slate-300">{formatChannelLabel(s.channel)}</span>}
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
                                const recentlyDone = !isWorking && cs.status === 'stopped' && cs.updatedAt && Date.now() - cs.updatedAt < 5 * 60 * 1000
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
                                const sourceBadgeClass = isCursorSource ? 'bg-[#1a2f3f] text-[#5eb5f7]' : isCodexSource ? 'bg-[#1d2f26] text-[#6dd29c]' : 'bg-[#3f211d] text-[#e87a65]'
                                const openClaudeDetail = () => {
                                  setSelectedAgentId(null)
                                  setSelectedSessionKey(null)
                                  setSelectedClaudeSession(null)
                                  setClaudeStatsSource(resolveClaudeStatsSource(cs.source))
                                  setShowClaudeStats(true)
                                }
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
                                    <div className="flex min-w-0 w-full items-center gap-3">
                                      {showCharGif && (
                                        <div
                                          data-no-drag
                                          onClick={(e) => {
                                            e.stopPropagation()
                                            openClaudeDetail()
                                          }}
                                          className="relative shrink-0 w-10 h-10 flex items-center justify-center cursor-pointer"
                                        >
                                          <div className="absolute inset-0" style={{ left: -16 }} />
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
                                        <div
                                          data-no-drag
                                          onClick={(e) => {
                                            e.stopPropagation()
                                            openClaudeDetail()
                                          }}
                                          className="relative shrink-0 w-10 h-10 flex items-center justify-center cursor-pointer"
                                        >
                                          <div className="absolute inset-0" style={{ left: -16 }} />
                                          <span className="w-1 h-1 rounded-full bg-slate-600" />
                                        </div>
                                      )}
                                      <div className="flex min-w-0 flex-1 items-center gap-1.5 overflow-hidden" data-no-drag>
                                        {editingSessionTitle === cs.sessionId ? (
                                          <input
                                            autoFocus
                                            data-no-drag
                                            className="text-[13px] font-bold bg-transparent border-b border-slate-500 outline-none text-white w-24"
                                            style={{ WebkitUserSelect: 'text', userSelect: 'text' }}
                                            defaultValue={projectName}
                                            ref={(el) => {
                                              if (el) {
                                                editingTitleValueRef.current = el.value
                                                editingTitleDefaultRef.current = defaultProjectName
                                              }
                                            }}
                                            onChange={(e) => {
                                              editingTitleValueRef.current = e.target.value
                                            }}
                                            onCompositionStart={() => {
                                              composingRef.current = true
                                            }}
                                            onCompositionEnd={(e) => {
                                              composingRef.current = false
                                              editingTitleValueRef.current = (e.target as HTMLInputElement).value
                                            }}
                                            onBlur={() => {
                                              saveSessionNickname(cs.sessionId, editingTitleValueRef.current, defaultProjectName)
                                              setEditingSessionTitle(null)
                                            }}
                                            onKeyDown={(e) => {
                                              if (composingRef.current) return
                                              if (e.key === 'Enter') (e.target as HTMLInputElement).blur()
                                              if (e.key === 'Escape') setEditingSessionTitle(null)
                                            }}
                                            onClick={(e) => e.stopPropagation()}
                                          />
                                        ) : (
                                          <span
                                            className={`min-w-0 max-w-[42%] truncate text-[13px] font-bold cursor-text ${isWorking ? 'text-white' : 'text-slate-300'}`}
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
                                        {subtitle && <span className="min-w-0 flex-1 truncate text-[13px] font-normal text-slate-500">· {subtitle}</span>}
                                      </div>
                                      <div className="flex items-center gap-2 shrink-0">
                                        <span className={`text-[11px] px-2 py-0.5 rounded-md font-normal ${sourceBadgeClass}`}>{sourceLabel}</span>
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
                                      <div data-no-drag className="mt-2 rounded-lg bg-[#1a1a1e] border border-[#2a2a2e] overflow-hidden">
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
                                              <>
                                                <span className="text-slate-500">{t('mini.you', '你')}：</span>
                                                {cs.userPrompt}
                                              </>
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
                                <motion.div key="expand-list-btn" layout initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} className="flex justify-center py-2">
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
                                </motion.div>,
                              )
                            } else if (!effListCollapsed && hasImportant && allItems.length > 1) {
                              elements.push(
                                <motion.div key="collapse-list-btn" layout initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} className="flex justify-center py-2">
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
                                </motion.div>,
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
                                  const sessionId = slot.agentId.slice('claude:'.length)
                                  setSelectedAgentId(null)
                                  setSelectedSessionKey(null)
                                  setSelectedClaudeSession(null)
                                  setClaudeStatsSource(resolveClaudeStatsSourceBySession(sessionId))
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
                              ...(hasConfiguredOpenClaw ? ['OpenClaw'] : []),
                              ...(enableClaudeCode ? ['Claude Code'] : []),
                              ...(enableCodex ? ['Codex'] : []),
                              ...(enableCursor ? ['Cursor'] : []),
                            ]
                            return targets.length > 0 ? <p className="text-slate-500 text-sm font-medium">{t('mini.startTracking', { targets: targets.join(' / ') })}</p> : null
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
                  initial={{ opacity: 0, filter: 'blur(8px)', y: -20 }}
                  animate={{ opacity: 1, filter: 'blur(0px)', y: 0 }}
                  exit={{ opacity: 0, filter: 'blur(8px)', y: -20 }}
                  transition={{ duration: 0.25, delay: 0.05 }}
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
                  initial={{ opacity: 0, filter: 'blur(8px)', y: -20 }}
                  animate={{ opacity: 1, filter: 'blur(0px)', y: 0 }}
                  exit={{ opacity: 0, filter: 'blur(8px)', y: -20 }}
                  transition={{ duration: 0.25, delay: 0.05 }}
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
                  style={{ background: '#1a1a1a', display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}
                  initial={{ opacity: 0, filter: 'blur(8px)', y: -20 }}
                  animate={{ opacity: 1, filter: 'blur(0px)', y: 0 }}
                  exit={{ opacity: 0, filter: 'blur(8px)', y: -20 }}
                  transition={{ duration: 0.25, delay: 0.05 }}
                >
                  <ClaudeStatsView source={claudeStatsSource} />
                </motion.div>
              ) : (
                /* ===== Agent detail panel (ui-2 style) ===== */
                <motion.div
                  key="agent-detail"
                  style={{ background: '#1a1a1a', display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0 }}
                  initial={{ opacity: 0, filter: 'blur(8px)', y: -20 }}
                  animate={{ opacity: 1, filter: 'blur(0px)', y: 0 }}
                  exit={{ opacity: 0, filter: 'blur(8px)', y: -20 }}
                  transition={{ duration: 0.25, delay: 0.05 }}
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
                if (e.target === e.currentTarget) {
                  e.preventDefault()
                  e.stopPropagation()
                }
              }}
              onClick={(e) => {
                if (e.target === e.currentTarget) {
                  e.preventDefault()
                  e.stopPropagation()
                  exitSettings()
                }
              }}
              style={{
                position: 'fixed',
                inset: 0,
                zIndex: 40,
                background: 'rgba(0,0,0,0.01)',
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
                    {(appMode === 'pet' ? ['settings'] as const : ['pairing', 'settings'] as const).map((nav) => (
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
                {showLargeMascotToggle && (
                  <button
                    data-no-drag
                    onClick={async (e) => {
                      e.stopPropagation()
                      const next = !largeMascot
                      setLargeMascot(next)
                      largeMascotRef.current = next
                      if (largeActionTimerRef.current) clearTimeout(largeActionTimerRef.current)
                      largeActionTimerRef.current = null
                      setLargePetAction(null)
                      largePetActionRef.current = null
                      const store = await load('settings.json', { defaults: {}, autoSave: true })
                      await store.set('large_mascot', next)
                      await store.save()
                    }}
                    style={{
                      background: largeMascot ? 'rgba(240,209,64,0.15)' : 'rgba(255,255,255,0.06)',
                      border: largeMascot ? '1px solid rgba(240,209,64,0.3)' : '1px solid transparent',
                      color: largeMascot ? '#F0D140' : 'rgba(255,255,255,0.5)',
                      fontSize: 11,
                      cursor: 'pointer',
                      padding: '3px 10px',
                      borderRadius: 6,
                      fontWeight: 500,
                      marginRight: 8,
                      transition: 'all 0.2s',
                    }}
                  >
                    {largeMascot ? '小看板娘' : '🐧 大看板娘'}
                  </button>
                )}
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
                                        <button
                                          onClick={() => setIsCreateModalOpen(true)}
                                          className="flex items-center justify-center w-6 h-6 rounded text-white/40 hover:text-white hover:bg-white/10 transition-colors"
                                          title={t('mini.createChar')}
                                        >
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
                                                {ip === '自定义'
                                                  ? t('mini.custom')
                                                  : ip === '其他'
                                                    ? t('mini.other')
                                                    : ip === '原神'
                                                      ? t('mini.ipGenshin')
                                                      : ip === '赛马娘'
                                                        ? t('mini.ipUmaMusume')
                                                        : ip}
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
                                                          <img
                                                            src={cPreview}
                                                            alt={c.name}
                                                            className="w-full h-full object-contain opacity-90"
                                                            style={{ imageRendering: 'pixelated' }}
                                                            draggable={false}
                                                          />
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
                        codexSoundEnabled={codexSoundEnabled}
                        onToggleCodexSoundEnabled={async (v) => {
                          setCodexSoundEnabled(v)
                          const store = await getStore()
                          await store.set('codex_sound_enabled', v)
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
                        autoExpandOnTask={autoExpandOnTask}
                        onToggleAutoExpandOnTask={async (v) => {
                          setAutoExpandOnTask(v)
                          autoExpandOnTaskRef.current = v
                          const store = await getStore()
                          await store.set('auto_expand_on_task', v)
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
                        hoverDelay={hoverDelay}
                        onChangeHoverDelay={async (v) => {
                          setHoverDelay(v)
                          hoverDelayRef.current = v
                          const store = await getStore()
                          await store.set('hover_delay', v)
                          await store.save()
                        }}
                        largeMascotScale={largeMascotScale}
                        onChangeLargeMascotScale={async (v) => {
                          const clamped = Math.min(6, Math.max(4, v))
                          setLargeMascotScale(clamped)
                          largeMascotScaleRef.current = clamped
                          const store = await getStore()
                          await store.set('large_mascot_scale', clamped)
                          await store.save()
                        }}
                        appMode={appMode}
                        onChangeAppMode={handleSelectAppMode}
                        petSfxEnabled={petSfxEnabled}
                        onTogglePetSfxEnabled={async (v) => {
                          setPetSfxEnabled(v)
                          petSfxEnabledRef.current = v
                          const store = await getStore()
                          await store.set('pet_sfx_enabled', v)
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

      <UpdateModal
        open={updateModalOpen}
        phase={updateModalPhase}
        info={updateModalInfo}
        progress={updateModalProgress}
        progressStage={updateModalProgressStage}
        onLater={closeUpdateModal}
        onSkipVersion={skipCurrentUpdateVersion}
        onUpdateNow={runUpdateFromModal}
        onRestartNow={restartFromModal}
      />

      <CreateCharacterModal
        isOpen={isCreateModalOpen}
        onClose={() => setIsCreateModalOpen(false)}
        onSaved={async () => {
          await invoke('scan_characters')
          const chars = await loadCharacters()
          setCharacters(chars)
        }}
      />

      {/* Onboarding modal — first launch only */}
      <OnboardingModal open={showOnboarding} onSelect={handleSelectAppMode} />

      {/* Pet context menu rendered inside mascot wrapper below */}
    </div>
  )
}
