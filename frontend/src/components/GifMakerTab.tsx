import { useState, useEffect, useRef, useCallback } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { Image as ImageIcon, Loader2 } from 'lucide-react'
import {
  sliceSprite, groupFramesByRow, applyChromaKey, drawFrameWithOffset,
  type Offset,
} from '../utils/spriteUtils'
import { exportGif } from '../utils/gifExport'
import type { PipelineConfig, PipelineItem, CardStatus } from '../lib/types'
import { loadCharacters, getStore } from '../lib/store'
import { loadPipelines, loadPromptFile, PIPELINE_CHROMA } from '../lib/pipeline'

// Canvas-based frame player
function AnimPreview({ frames, offsets, fps, size = 80 }: {
  frames: HTMLCanvasElement[]; offsets: Offset[]; fps: number; size?: number
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const frameIdx = useRef(0)

  useEffect(() => {
    const c = canvasRef.current
    if (!c || frames.length === 0) return
    c.width = frames[0].width; c.height = frames[0].height
    frameIdx.current = 0
    const draw = () => {
      const idx = frameIdx.current % frames.length
      const ctx = c.getContext('2d')!
      const off = offsets[idx] || { dx: 0, dy: 0 }
      drawFrameWithOffset(ctx, frames[idx], off, c.width, c.height)
    }
    draw()
    const interval = setInterval(() => { frameIdx.current++; draw() }, 1000 / fps)
    return () => clearInterval(interval)
  }, [frames, offsets, fps])

  if (frames.length === 0) return null
  return <canvas ref={canvasRef} style={{ width: size, height: size, imageRendering: 'pixelated', borderRadius: 6, background: 'repeating-conic-gradient(#eee 0% 25%, #fff 0% 50%) 0 0 / 12px 12px' }} />
}

export function GifMakerTab() {
  const [charName, setCharName] = useState('')
  const [pipelineId, setPipelineId] = useState<string | null>(null)
  const [pipelines, setPipelines] = useState<PipelineConfig[]>([])
  const [characters, setCharacters] = useState<string[]>([])
  const [referenceFile, setReferenceFile] = useState<File | null>(null)
  const [referencePreview, setReferencePreview] = useState<string | null>(null)
  const [started, setStarted] = useState(false)
  const [items, setItems] = useState<PipelineItem[]>([])
  const [saving, setSaving] = useState(false)
  const [saveMsg, setSaveMsg] = useState('')
  const refCacheRef = useRef<{ base64: string; mimeType: string } | null>(null)
  const [fps, setFps] = useState(4)
  const [geminiKey, setGeminiKey] = useState('')
  const [geminiUrl, setGeminiUrl] = useState('')
  const [showKey, setShowKey] = useState(false)

  useEffect(() => {
    loadPipelines().then((p) => {
      setPipelines(p)
      const top = p.find((x) => x.id === 'top')
      if (top && !pipelineId) setPipelineId(top.id)
      else if (p.length > 0 && !pipelineId) setPipelineId(p[0].id)
    }).catch(() => {})
    loadCharacters().then((chars) => setCharacters(chars.map((c) => c.name)))
    getStore().then(async (store) => {
      const key = ((await store.get('gemini_api_key')) as string) || import.meta.env.VITE_GEMINI_API_KEY || ''
      const url = ((await store.get('gemini_base_url')) as string) || import.meta.env.VITE_GEMINI_BASE_URL || ''
      setGeminiKey(key)
      setGeminiUrl(url)
    })
  }, [])

  const pipeline = pipelines.find((p) => p.id === pipelineId) ?? null

  // Initialize items when pipeline changes
  useEffect(() => {
    if (!pipeline) { setItems([]); return }
    setItems(pipeline.presets.map((preset) => ({
      preset, status: 'idle' as CardStatus, rawFrames: [], keyedFrames: [],
      rowGroups: [], rowLabels: [], globalOffset: { dx: 0, dy: 0 }, rowOffsets: [],
    })))
    setStarted(false); setSaveMsg('')
  }, [pipelineId])

  const handleFile = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (!file) return
    setReferenceFile(file)
    setReferencePreview(URL.createObjectURL(file))
    setStarted(false); setItems([]); setSaveMsg(''); refCacheRef.current = null
  }, [])

  const updateItem = useCallback((idx: number, patch: Partial<PipelineItem>) => {
    setItems((prev) => prev.map((item, i) => (i === idx ? { ...item, ...patch } : item)))
  }, [])

  const ensureRefBase64 = useCallback(async () => {
    if (!referenceFile) throw new Error('No reference file')
    if (refCacheRef.current) return refCacheRef.current
    const { fileToBase64 } = await import('../utils/nanoBanana')
    const base64 = await fileToBase64(referenceFile)
    refCacheRef.current = { base64, mimeType: referenceFile.type }
    return refCacheRef.current
  }, [referenceFile])

  const processImage = useCallback((idx: number, img: HTMLImageElement, pl: PipelineConfig) => {
    const preset = pl.presets[idx]
    try {
      const allFrames = sliceSprite(img, preset.cols, preset.rows)
      const rawFrames = pl.discardLastFrame ? allFrames.slice(0, -1) : allFrames
      const keyedFrames = rawFrames.map((f) => applyChromaKey(f, PIPELINE_CHROMA))
      let rowGroups: HTMLCanvasElement[][]
      const rowLabels: string[] = []
      if (pl.exportMode === 'by-row') {
        const groups = groupFramesByRow(keyedFrames, preset.cols, preset.rows)
        rowGroups = preset.excludeLastFrameRows
          ? groups.map((row, i) => (preset.excludeLastFrameRows!.includes(i) && row.length > 1 ? row.slice(0, -1) : row))
          : groups
        for (let ri = 0; ri < rowGroups.length; ri++) {
          rowLabels.push(preset.rowLabels?.[ri]?.toLowerCase().replace(/\s+/g, '-') || `row-${ri}`)
        }
      } else {
        rowGroups = [keyedFrames]; rowLabels.push(preset.id)
      }
      updateItem(idx, {
        status: 'ready', rawFrames, keyedFrames, rowGroups, rowLabels,
        globalOffset: { dx: 0, dy: 0 }, rowOffsets: rowGroups.map(() => ({ dx: 0, dy: 0 })),
      })
    } catch (err: any) {
      updateItem(idx, { status: 'error', error: err.message || String(err) })
    }
  }, [updateItem])

  const generateOne = useCallback(async (idx: number, pl: PipelineConfig) => {
    updateItem(idx, { status: 'generating', error: undefined })
    try {
      const { generateImage, base64ToImage } = await import('../utils/nanoBanana')
      const [ref, prompt] = await Promise.all([ensureRefBase64(), loadPromptFile(pl.presets[idx].promptFile)])
      const res = await generateImage({ prompt, imageBase64: ref.base64, imageMimeType: ref.mimeType, aspectRatio: '1:1', apiKey: geminiKey, baseUrl: geminiUrl || undefined })
      const img = await base64ToImage(res.imageBase64, res.mimeType)
      updateItem(idx, { status: 'processing' })
      processImage(idx, img, pl)
    } catch (err: any) {
      updateItem(idx, { status: 'error', error: err.message || String(err) })
    }
  }, [ensureRefBase64, updateItem, processImage, geminiKey, geminiUrl])

  const uploadSpriteSheet = useCallback(async (idx: number, file: File, pl: PipelineConfig) => {
    updateItem(idx, { status: 'processing', error: undefined })
    try {
      const img = await new Promise<HTMLImageElement>((resolve, reject) => {
        const image = new Image()
        image.onload = () => resolve(image)
        image.onerror = reject
        image.src = URL.createObjectURL(file)
      })
      processImage(idx, img, pl)
    } catch (err: any) {
      updateItem(idx, { status: 'error', error: err.message || String(err) })
    }
  }, [updateItem, processImage])

  const startPipeline = useCallback(async () => {
    if (!referenceFile || !pipeline) return
    setStarted(true); setSaveMsg('')
    // Reset items to idle
    setItems(pipeline.presets.map((preset) => ({
      preset, status: 'idle' as CardStatus, rawFrames: [], keyedFrames: [],
      rowGroups: [], rowLabels: [], globalOffset: { dx: 0, dy: 0 }, rowOffsets: [],
    })))
    const BATCH = 3
    for (let i = 0; i < pipeline.presets.length; i += BATCH) {
      const batch = pipeline.presets.slice(i, i + BATCH).map((_, j) => i + j).filter((idx) => idx < pipeline.presets.length)
      await Promise.allSettled(batch.map((idx) => generateOne(idx, pipeline)))
    }
  }, [referenceFile, pipeline, generateOne])

  const exportItemGifs = useCallback(async (item: PipelineItem): Promise<{ blob: Blob; label: string }[]> => {
    const results: { blob: Blob; label: string }[] = []
    for (let ri = 0; ri < item.rowGroups.length; ri++) {
      const frames = item.rowGroups[ri]
      if (!frames || frames.length === 0) continue
      if (item.rowLabels[ri] === 'unused') continue
      const rowOff = item.rowOffsets[ri] || { dx: 0, dy: 0 }
      const combined: Offset = { dx: item.globalOffset.dx + rowOff.dx, dy: item.globalOffset.dy + rowOff.dy }
      const offsets = frames.map(() => combined)
      const blob = await exportGif({ frames, frameOrder: frames.map((_, i) => i), offsets, fps, useOffsets: true })
      results.push({ blob, label: item.rowLabels[ri] || `row-${ri}` })
    }
    return results
  }, [fps])

  const getSubfolder = useCallback((item: PipelineItem) => {
    if (!pipeline) return ''
    if (pipeline.id === 'pet') {
      return `pet/${item.preset.id === 'work' ? 'work' : item.preset.id === 'crawl' ? 'crawl' : 'rest'}`
    }
    if (pipeline.id === 'top') return 'mini/top'
    return `mini/${item.preset.id.replace('mini-', '')}`
  }, [pipeline])

  const saveGifBlob = useCallback(async (blob: Blob, label: string, subfolder: string) => {
    const reader = new FileReader()
    const dataUrl: string = await new Promise((resolve) => {
      reader.onloadend = () => resolve(reader.result as string)
      reader.readAsDataURL(blob)
    })
    await invoke('save_character_gif', { charName: charName.trim(), fileName: `${label}.gif`, subfolder, dataUrl })
  }, [charName])

  const refreshChars = useCallback(async () => {
    await invoke('scan_characters')
    const chars = await loadCharacters()
    setCharacters(chars.map((c) => c.name))
  }, [])

  const handleSaveAll = useCallback(async () => {
    if (!charName.trim()) { setSaveMsg('Please enter character name'); return }
    if (!pipeline) return
    setSaving(true); setSaveMsg('')
    let totalSaved = 0
    try {
      for (const item of items) {
        if (item.status !== 'ready') continue
        const gifs = await exportItemGifs(item)
        const subfolder = getSubfolder(item)
        for (const { blob, label } of gifs) {
          await saveGifBlob(blob, label, subfolder)
          totalSaved++
        }
      }
      await refreshChars()
      setSaveMsg(`Done! Saved ${totalSaved} GIFs to ${charName.trim()}/`)
    } catch (e: any) { setSaveMsg(`Save failed: ${e}`) }
    setSaving(false)
  }, [charName, pipeline, items, exportItemGifs, getSubfolder, saveGifBlob, refreshChars])

  const handleSaveItem = useCallback(async (idx: number) => {
    const item = items[idx]
    if (!item || item.status !== 'ready' || !charName.trim()) { setSaveMsg('请输入角色名称'); return }
    setSaving(true); setSaveMsg('')
    try {
      const gifs = await exportItemGifs(item)
      const subfolder = getSubfolder(item)
      for (const { blob, label } of gifs) {
        await saveGifBlob(blob, label, subfolder)
      }
      await refreshChars()
      setSaveMsg(`Done! Saved ${gifs.length} GIF(s) for ${item.preset.name}`)
    } catch (e: any) { setSaveMsg(`Save failed: ${e}`) }
    setSaving(false)
  }, [items, charName, exportItemGifs, getSubfolder, saveGifBlob, refreshChars])

  const handleSaveRow = useCallback(async (idx: number, ri: number) => {
    const item = items[idx]
    if (!item || item.status !== 'ready' || !charName.trim()) { setSaveMsg('请输入角色名称'); return }
    const frames = item.rowGroups[ri]
    if (!frames?.length) return
    setSaving(true); setSaveMsg('')
    try {
      const rowOff = item.rowOffsets[ri] || { dx: 0, dy: 0 }
      const combined: Offset = { dx: item.globalOffset.dx + rowOff.dx, dy: item.globalOffset.dy + rowOff.dy }
      const offsets = frames.map(() => combined)
      const blob = await exportGif({ frames, frameOrder: frames.map((_, i) => i), offsets, fps, useOffsets: true })
      let label = item.rowLabels[ri] || `row-${ri}`
      // For top pipeline: use row label directly (sleep, work, eat, look)
      if (pipeline?.id === 'top' && label !== 'unused') {
        // label is already the correct name from rowLabels
      }
      const subfolder = getSubfolder(item)
      await saveGifBlob(blob, label, subfolder)
      await refreshChars()
      setSaveMsg(`Done! Saved ${label}.gif`)
    } catch (e: any) { setSaveMsg(`Save failed: ${e}`) }
    setSaving(false)
  }, [items, charName, fps, getSubfolder, saveGifBlob, refreshChars])

  const handleReset = () => {
    setReferenceFile(null)
    if (referencePreview) URL.revokeObjectURL(referencePreview)
    setReferencePreview(null)
    setItems([]); setStarted(false); setSaveMsg(''); refCacheRef.current = null
  }

  const readyCount = items.filter((it) => it.status === 'ready').length
  const fileInputRef = useRef<HTMLInputElement>(null)

  return (
    <div className="flex-1 overflow-auto p-8 bg-gray-50">
      <div className="max-w-5xl mx-auto space-y-6">
        {/* Header */}

        {/* Gemini API Key */}
        <div className="bg-white rounded-xl border border-gray-200 shadow-sm p-4">
          <div className="flex items-center gap-4">
            <div className="flex-1 min-w-0">
              <label className="block text-xs font-medium text-gray-500 mb-1">Gemini API Key</label>
              <div className="flex gap-2">
                <input
                  type={showKey ? 'text' : 'password'}
                  value={geminiKey}
                  onChange={async (e) => { setGeminiKey(e.target.value); const store = await getStore(); await store.set('gemini_api_key', e.target.value); await store.save() }}
                  placeholder="填入你的 Gemini API Key"
                  className="flex-1 px-3 py-1.5 text-sm border border-gray-200 rounded-lg focus:outline-none focus:ring-2 focus:ring-orange-500/20 focus:border-orange-400 font-mono"
                />
                <button onClick={() => setShowKey(!showKey)} className="px-2 py-1.5 text-xs text-gray-500 hover:text-gray-700 border border-gray-200 rounded-lg">
                  {showKey ? '隐藏' : '显示'}
                </button>
              </div>
            </div>
            <div className="min-w-0" style={{ width: 220 }}>
              <label className="block text-xs font-medium text-gray-500 mb-1">Base URL <span className="text-gray-400">(可选)</span></label>
              <input
                value={geminiUrl}
                onChange={async (e) => { setGeminiUrl(e.target.value); const store = await getStore(); await store.set('gemini_base_url', e.target.value); await store.save() }}
                placeholder="默认 Google 官方"
                className="w-full px-3 py-1.5 text-sm border border-gray-200 rounded-lg focus:outline-none focus:ring-2 focus:ring-orange-500/20 focus:border-orange-400 font-mono"
              />
            </div>
          </div>
        </div>

        {/* Top Controls */}
        <div className="bg-white rounded-xl border border-gray-200 shadow-sm p-6">
          <div className="grid grid-cols-1 md:grid-cols-3 gap-6">
            <div>
              <label className="block text-sm font-medium text-gray-700 mb-2">角色名称</label>
              <input
                value={charName}
                onChange={(e) => setCharName(e.target.value)}
                placeholder="例如: hutao"
                list="char-name-list-gm"
                className="w-full bg-gray-50 border border-gray-200 text-gray-900 text-sm rounded-lg focus:ring-2 focus:ring-gray-900/10 focus:border-gray-900 block p-2.5 outline-none transition-all placeholder:text-gray-400"
              />
              <datalist id="char-name-list-gm">{characters.map((n) => <option key={n} value={n} />)}</datalist>
            </div>
            <div>
              <label className="block text-sm font-medium text-gray-700 mb-2">参考图</label>
              <div className="flex items-center gap-3">
                <div className="w-10 h-10 rounded-lg bg-gray-100 border border-gray-200 flex items-center justify-center overflow-hidden">
                  {referencePreview ? (
                    <img src={referencePreview} alt="ref" className="w-8 h-8 object-contain" />
                  ) : (
                    <ImageIcon size={20} className="text-gray-400" />
                  )}
                </div>
                <input ref={fileInputRef} type="file" accept="image/*" className="hidden" onChange={handleFile} />
                <button
                  onClick={() => fileInputRef.current?.click()}
                  className="bg-white border border-gray-200 hover:bg-gray-50 text-gray-700 text-sm font-medium py-2 px-4 rounded-lg transition-colors shadow-sm"
                >
                  {referencePreview ? '重新选择' : '选择文件'}
                </button>
              </div>
            </div>
            <div>
              <label className="block text-sm font-medium text-gray-700 mb-2">FPS (1-30) <span className="text-xs text-gray-400 font-normal">数值越高，动作越快</span></label>
              <div className="flex items-center gap-4">
                <input
                  type="range" min="1" max="30" value={fps}
                  onChange={(e) => setFps(+e.target.value)}
                  className="w-full h-2 bg-gray-200 rounded-lg appearance-none cursor-pointer accent-gray-900"
                />
                <span className="text-sm font-medium text-gray-900 w-8 text-right">{fps}</span>
              </div>
            </div>
          </div>
        </div>

        {/* Action bar */}
        {pipeline && (referenceFile || readyCount > 0 || items.some(it => it.status !== 'idle')) && (
          <div className="bg-white rounded-xl border border-gray-200 shadow-sm p-4 flex items-center gap-3">
            {referenceFile && (
              <button
                onClick={startPipeline}
                disabled={!charName.trim() || started}
                className="bg-gray-900 hover:bg-gray-800 text-white text-sm font-medium py-2 px-6 rounded-lg transition-colors shadow-sm disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {started ? 'AI 生成中...' : `AI 生成 (${pipeline.presets.length} presets)`}
              </button>
            )}
            {items.some(it => it.status !== 'idle') && (
              <button onClick={handleReset} className="bg-white border border-gray-200 hover:bg-gray-50 text-gray-700 text-xs font-medium py-1.5 px-3 rounded-lg transition-colors shadow-sm">
                Reset
              </button>
            )}
            {readyCount > 0 && (
              <button
                onClick={handleSaveAll}
                disabled={saving}
                className="bg-gray-900 hover:bg-gray-800 text-white text-sm font-medium py-2 px-6 rounded-lg transition-colors shadow-sm disabled:opacity-50"
              >
                {saving ? 'Saving...' : `Save All to ${charName || '...'}/`}
              </button>
            )}
            {saveMsg && <span className={`text-sm ${saveMsg.startsWith('Done') ? 'text-emerald-600' : 'text-red-500'}`}>{saveMsg}</span>}
          </div>
        )}

        {/* Pipeline cards */}
        <div className="space-y-6">
          {items.map((item, idx) => (
            <PipelineCard
              key={item.preset.id}
              item={item}
              idx={idx}
              pipeline={pipeline}
              fps={fps}
              onUpdate={updateItem}
              onRetry={(i) => pipeline && generateOne(i, pipeline)}
              onUpload={(i, file) => pipeline && uploadSpriteSheet(i, file, pipeline)}
              onSaveItem={handleSaveItem}
              onSaveRow={handleSaveRow}
            />
          ))}
        </div>
      </div>
    </div>
  )
}

// ─── Pipeline Card ───

function PipelineCard({ item, idx, pipeline, fps, onUpdate, onRetry, onUpload, onSaveItem, onSaveRow }: {
  item: PipelineItem; idx: number; pipeline: PipelineConfig | null
  fps: number
  onUpdate: (idx: number, patch: Partial<PipelineItem>) => void; onRetry: (idx: number) => void
  onUpload: (idx: number, file: File) => void
  onSaveItem: (idx: number) => void; onSaveRow: (idx: number, ri: number) => void
}) {
  const uploadRef = useRef<HTMLInputElement>(null)
  const isWholeMode = pipeline?.exportMode !== 'by-row'

  const nudge = (field: 'globalOffset', axis: 'dx' | 'dy', delta: number) => {
    const cur = item[field]
    onUpdate(idx, { [field]: { ...cur, [axis]: cur[axis] + delta } })
  }
  const nudgeRow = (ri: number, axis: 'dx' | 'dy', delta: number) => {
    const newOffsets = [...item.rowOffsets]
    newOffsets[ri] = { ...newOffsets[ri], [axis]: newOffsets[ri][axis] + delta }
    onUpdate(idx, { rowOffsets: newOffsets })
  }
  const resetOffsets = () => {
    onUpdate(idx, { globalOffset: { dx: 0, dy: 0 }, rowOffsets: item.rowGroups.map(() => ({ dx: 0, dy: 0 })) })
  }

  const getRowOffsets = (ri: number): Offset[] => {
    const g = item.globalOffset
    const r = item.rowOffsets[ri] || { dx: 0, dy: 0 }
    const combined: Offset = { dx: g.dx + r.dx, dy: g.dy + r.dy }
    return (item.rowGroups[ri] || []).map(() => combined)
  }

  const handleKeyDown = (e: React.KeyboardEvent) => {
    const step = e.shiftKey ? 5 : 1
    if (e.key === 'ArrowLeft') { e.preventDefault(); nudge('globalOffset', 'dx', -step) }
    if (e.key === 'ArrowRight') { e.preventDefault(); nudge('globalOffset', 'dx', step) }
    if (e.key === 'ArrowUp') { e.preventDefault(); nudge('globalOffset', 'dy', -step) }
    if (e.key === 'ArrowDown') { e.preventDefault(); nudge('globalOffset', 'dy', step) }
  }

  const [showPrompt, setShowPrompt] = useState(false)
  const [promptText, setPromptText] = useState<string | null>(null)

  const togglePrompt = async () => {
    if (showPrompt) { setShowPrompt(false); return }
    if (promptText === null) {
      try {
        const text = await loadPromptFile(item.preset.promptFile)
        setPromptText(text)
      } catch { setPromptText('Failed to load prompt') }
    }
    setShowPrompt(true)
  }

  const statusClass = item.status === 'ready' ? 'bg-emerald-50 text-emerald-600'
    : item.status === 'error' ? 'bg-red-50 text-red-500'
    : item.status === 'idle' ? 'bg-gray-100 text-gray-500'
    : 'bg-blue-50 text-blue-500'

  const statusLabel = item.status === 'idle' ? 'Waiting'
    : item.status === 'generating' ? 'AI Generating...'
    : item.status === 'processing' ? 'Processing...'
    : item.status === 'ready' ? 'Ready' : 'Error'

  return (
    <div className="bg-white rounded-xl border border-gray-200 shadow-sm overflow-hidden outline-none" tabIndex={0} onKeyDown={handleKeyDown}>
      {/* Header */}
      <div className="px-6 py-4 border-b border-gray-100 bg-gray-50/50 flex items-center justify-between">
        <div>
          <span className="text-sm font-semibold text-gray-900">{item.preset.name}</span>
          <span className="text-[11px] text-gray-500 ml-2">{item.preset.description}</span>
        </div>
        <div className="flex gap-2 items-center">
          {item.status === 'ready' && (
            <button onClick={() => onSaveItem(idx)} className="bg-gray-900 hover:bg-gray-800 text-white text-xs font-medium py-1.5 px-3 rounded-lg transition-colors shadow-sm">
              保存
            </button>
          )}
          <input ref={uploadRef} type="file" accept="image/*" className="hidden" onChange={(e) => { const f = e.target.files?.[0]; if (f) onUpload(idx, f); e.target.value = '' }} />
          <button onClick={() => uploadRef.current?.click()} className="bg-white border border-gray-200 hover:bg-gray-50 text-gray-700 text-xs font-medium py-1.5 px-3 rounded-lg transition-colors shadow-sm" title="复制提示词到 ChatGPT/Gemini 等网站生成图片，再上传到这里">
            上传图片
          </button>
          <button onClick={togglePrompt} className={`bg-white border border-gray-200 hover:bg-gray-50 text-gray-700 text-xs font-medium py-1.5 px-3 rounded-lg transition-colors shadow-sm ${showPrompt ? 'ring-1 ring-gray-900/10' : ''}`}>
            提示词
          </button>
          {(item.status === 'ready' || item.status === 'error') && (
            <button onClick={() => onRetry(idx)} className="bg-white border border-gray-200 hover:bg-gray-50 text-gray-700 text-xs font-medium py-1.5 px-3 rounded-lg transition-colors shadow-sm">
              重试
            </button>
          )}
          <span className={`text-[10px] px-2 py-0.5 rounded-full font-medium ${statusClass}`}>{statusLabel}</span>
        </div>
      </div>

      {/* Prompt */}
      {showPrompt && promptText && (
        <div className="mx-6 mt-4 bg-gray-50 border border-gray-200 rounded-lg p-4 max-h-60 overflow-y-auto scrollbar-thin">
          <pre className="text-[11px] text-gray-600 whitespace-pre-wrap font-mono leading-relaxed">{promptText}</pre>
        </div>
      )}

      {/* Idle hint */}
      {item.status === 'idle' && (
        <div className="px-6 py-3 text-[11px] text-gray-400">
          点击「提示词」查看生成要求，复制到 ChatGPT / Gemini / Midjourney 等平台生成图片后上传
        </div>
      )}

      {/* Error */}
      {item.status === 'error' && item.error && (
        <div className="mx-6 mt-4 text-[11px] text-red-500 bg-red-50 p-2.5 rounded-lg">{item.error}</div>
      )}

      {/* Loading */}
      {(item.status === 'generating' || item.status === 'processing') && (
        <div className="p-6 flex items-center gap-2 text-blue-500 text-sm">
          <Loader2 className="w-4 h-4 animate-spin" />
          {item.status === 'generating' ? 'AI Generating...' : 'Processing frames...'}
        </div>
      )}

      {/* Ready: previews + offset controls */}
      {item.status === 'ready' && item.rowGroups.length > 0 && (
        <div className="p-6">
          {/* Global offset controls */}
          <div className="flex items-center gap-2.5 mb-4 text-[11px]">
            <span className="text-gray-500">Global Offset:</span>
            <span className="text-gray-500 font-medium">X</span>
            <input type="number" value={item.globalOffset.dx} onChange={(e) => onUpdate(idx, { globalOffset: { ...item.globalOffset, dx: +e.target.value } })}
              className="w-16 bg-white border border-gray-200 text-gray-700 text-sm rounded focus:ring-1 focus:ring-gray-900/20 focus:border-gray-900 block p-1.5 outline-none transition-all text-center shadow-sm" />
            <span className="text-gray-500 font-medium">Y</span>
            <input type="number" value={item.globalOffset.dy} onChange={(e) => onUpdate(idx, { globalOffset: { ...item.globalOffset, dy: +e.target.value } })}
              className="w-16 bg-white border border-gray-200 text-gray-700 text-sm rounded focus:ring-1 focus:ring-gray-900/20 focus:border-gray-900 block p-1.5 outline-none transition-all text-center shadow-sm" />
            {(item.globalOffset.dx !== 0 || item.globalOffset.dy !== 0) && (
              <button onClick={resetOffsets} className="text-red-500 text-[11px] hover:underline">Reset</button>
            )}
            <span className="text-gray-400 text-[10px] ml-2">Arrow keys, Shift x5</span>
          </div>

          {/* Whole mode */}
          {isWholeMode && (
            <div className="flex gap-3 items-center">
              <AnimPreview frames={item.rowGroups[0]} offsets={getRowOffsets(0)} fps={fps} size={120} />
              <span className="text-[10px] text-gray-500">{item.rowGroups[0].length} frames</span>
              <button onClick={() => onSaveRow(idx, 0)} className="bg-gray-900 hover:bg-gray-800 text-white text-[10px] font-medium py-1 px-2.5 rounded-md transition-colors shadow-sm ml-auto">
                保存
              </button>
            </div>
          )}

          {/* By-row mode */}
          {!isWholeMode && (
            <div className="flex flex-col gap-3">
              {item.rowGroups.map((rowFrames, ri) => {
                const ro = item.rowOffsets[ri] || { dx: 0, dy: 0 }
                return (
                  <div key={ri} className="flex items-center gap-4 p-3 rounded-lg border border-gray-100 bg-gray-50/50 transition-colors hover:bg-gray-50">
                    <div className="w-16 h-16 rounded-md border border-gray-200 bg-white flex items-center justify-center overflow-hidden shrink-0">
                      <AnimPreview frames={rowFrames} offsets={getRowOffsets(ri)} fps={fps} size={56} />
                    </div>
                    <div className="flex-1 flex flex-wrap items-center gap-x-6 gap-y-2">
                      <div className="min-w-[90px]">
                        <div className="text-sm font-medium text-gray-700">{item.rowLabels[ri]}</div>
                        <div className="text-[9px] text-gray-400">{rowFrames.length} frames</div>
                      </div>
                      <div className="flex items-center gap-2">
                        <span className="text-xs text-gray-500 font-medium">X</span>
                        <input type="number" value={ro.dx} onChange={(e) => nudgeRow(ri, 'dx', +e.target.value - ro.dx)}
                          className="w-16 bg-white border border-gray-200 text-gray-700 text-sm rounded focus:ring-1 focus:ring-gray-900/20 focus:border-gray-900 block p-1.5 outline-none transition-all text-center shadow-sm" />
                      </div>
                      <div className="flex items-center gap-2">
                        <span className="text-xs text-gray-500 font-medium">Y</span>
                        <input type="number" value={ro.dy} onChange={(e) => nudgeRow(ri, 'dy', +e.target.value - ro.dy)}
                          className="w-16 bg-white border border-gray-200 text-gray-700 text-sm rounded focus:ring-1 focus:ring-gray-900/20 focus:border-gray-900 block p-1.5 outline-none transition-all text-center shadow-sm" />
                      </div>
                      <button onClick={() => onSaveRow(idx, ri)} className="bg-gray-900 hover:bg-gray-800 text-white text-[10px] font-medium py-1 px-2.5 rounded-md transition-colors shadow-sm ml-auto shrink-0">
                        {pipeline?.id === 'top'
                          ? `保存为 ${item.rowLabels[ri] || `row-${ri}`}.gif`
                          : '保存'}
                      </button>
                    </div>
                  </div>
                )
              })}
            </div>
          )}
        </div>
      )}
    </div>
  )
}

