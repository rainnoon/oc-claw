import type { ChromaKeyOptions } from '../utils/spriteUtils'
import type { PipelineConfig } from './types'

export const SUBFOLDER_OPTIONS = [
  { value: 'mini/top', label: 'Top 动画 (mini/top)' },
  { value: 'pet/work', label: '工作动画 (pet/work)' },
  { value: 'pet/rest', label: '休息动画 (pet/rest)' },
  { value: 'pet/crawl', label: '爬行动画 (pet/crawl)' },
  { value: 'mini/walk', label: 'Mini 行走 (mini/walk)' },
  { value: 'mini/fish', label: 'Mini 摸鱼 (mini/fish)' },
  { value: 'mini/sport', label: 'Mini 运动 (mini/sport)' },
]

export const PIPELINE_CHROMA: ChromaKeyOptions = {
  keyColor: { r: 5, g: 249, b: 3 }, tolerance: 38, smoothness: 38, spill: 85, erosion: 65,
}

let _pipelinesCache: PipelineConfig[] | null = null

export async function loadPipelines(): Promise<PipelineConfig[]> {
  if (_pipelinesCache) return _pipelinesCache
  const resp = await fetch('/prompt/pipelines.json')
  if (!resp.ok) throw new Error(`Failed to load pipelines.json (${resp.status})`)
  _pipelinesCache = await resp.json()
  return _pipelinesCache!
}

const _promptCache = new Map<string, string>()

export async function loadPromptFile(path: string): Promise<string> {
  if (_promptCache.has(path)) return _promptCache.get(path)!
  const resp = await fetch(path)
  if (!resp.ok) throw new Error(`Failed to load prompt: ${path}`)
  const text = (await resp.text()).trim()
  _promptCache.set(path, text)
  return text
}
