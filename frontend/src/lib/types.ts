import type { ChromaKeyOptions, Offset } from '../utils/spriteUtils'

export type { ChromaKeyOptions, Offset }

export interface SessionInfo {
  id: string
  label?: string
  status: string
  model?: string
  channel?: string
}

export interface CharacterMeta {
  name: string
  builtin?: boolean
  ip?: string
  workGifs: string[]
  restGifs: string[]
  crawlGifs?: string[]
  angryGifs?: string[]
  shyGifs?: string[]
  miniActions?: Record<string, string[]>
}

export interface AgentInfo {
  id: string
  identityName?: string
  identityEmoji?: string
}

export interface AgentHealth {
  agentId: string
  active: boolean
}

export interface ToolCallStat {
  name: string
  count: number
}

export interface RecentAction {
  type: 'tool' | 'text'
  summary: string
  detail?: string
  timestamp?: string
}

export interface AgentMetrics {
  agentId: string
  active: boolean
  currentModel?: string
  thinkingLevel?: string
  activeSessionCount: number
  currentTask?: string
  currentTool?: string
  totalTokens: number
  inputTokens: number
  outputTokens: number
  cacheReadTokens: number
  cacheWriteTokens: number
  totalCost: number
  toolCalls: ToolCallStat[]
  recentActions: RecentAction[]
  errorCount: number
  messageCount: number
  sessionStart?: string
  lastActivity?: string
  channel?: string
}

export interface PipelinePreset {
  id: string; name: string; description: string; promptFile: string
  cols: number; rows: number; needsRefImage: boolean
  rowLabels?: string[]; excludeLastFrameRows?: number[]
}

export interface PipelineConfig {
  id: string; name: string; description: string
  presets: PipelinePreset[]; exportMode: 'whole' | 'by-row'; discardLastFrame: boolean
}

export type CardStatus = 'idle' | 'generating' | 'processing' | 'ready' | 'error'

export interface PipelineItem {
  preset: PipelinePreset; status: CardStatus; error?: string
  rawFrames: HTMLCanvasElement[]
  keyedFrames: HTMLCanvasElement[]
  rowGroups: HTMLCanvasElement[][]
  rowLabels: string[]
  globalOffset: Offset
  rowOffsets: Offset[]
}
