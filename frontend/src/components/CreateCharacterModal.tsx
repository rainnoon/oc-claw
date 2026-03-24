import { useState, useEffect, useRef, useCallback } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { X, Upload, ExternalLink, Loader2 } from 'lucide-react'
import {
  sliceSprite, groupFramesByRow, applyChromaKey, drawFrameWithOffset,
  type Offset,
} from '../utils/spriteUtils'
import { exportGif } from '../utils/gifExport'
import type { PipelineConfig } from '../lib/types'
import { loadCharacters } from '../lib/store'
import { loadPipelines, PIPELINE_CHROMA } from '../lib/pipeline'

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
  return <canvas ref={canvasRef} style={{ width: size, height: size, imageRendering: 'pixelated', borderRadius: 8, background: 'repeating-conic-gradient(rgba(255,255,255,0.05) 0% 25%, transparent 0% 50%) 0 0 / 12px 12px' }} />
}

interface Props {
  isOpen: boolean
  onClose: () => void
  onSaved: () => void
}

interface RowData {
  frames: HTMLCanvasElement[]
  label: string
  offset: Offset
}

export function CreateCharacterModal({ isOpen, onClose, onSaved }: Props) {
  const [step, setStep] = useState<'upload' | 'processing' | 'tuning'>('upload')
  const [name, setName] = useState('')
  const [rows, setRows] = useState<RowData[]>([])
  const [pipeline, setPipeline] = useState<PipelineConfig | null>(null)
  const [fps] = useState(1)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState('')
  const [existingNames, setExistingNames] = useState<string[]>([])

  useEffect(() => {
    if (isOpen) {
      setStep('upload')
      setName('')
      setRows([])
      setError('')
      setSaving(false)
      loadPipelines().then((p) => {
        const top = p.find((x) => x.id === 'top')
        setPipeline(top || p[0] || null)
      }).catch(() => {})
      loadCharacters().then((chars) => setExistingNames(chars.map((c) => c.name)))
    }
  }, [isOpen])

  const handleFileChange = useCallback(async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (!file || !pipeline) return
    setStep('processing')
    setError('')

    try {
      const img = await new Promise<HTMLImageElement>((resolve, reject) => {
        const image = new Image()
        image.onload = () => resolve(image)
        image.onerror = reject
        image.src = URL.createObjectURL(file)
      })

      const preset = pipeline.presets[0]
      if (!preset) throw new Error('No preset found')

      const allFrames = sliceSprite(img, preset.cols, preset.rows)
      const rawFrames = pipeline.discardLastFrame ? allFrames.slice(0, -1) : allFrames
      const keyedFrames = rawFrames.map((f) => applyChromaKey(f, PIPELINE_CHROMA))

      let rowGroups: HTMLCanvasElement[][]
      const rowLabels: string[] = []

      if (pipeline.exportMode === 'by-row') {
        const groups = groupFramesByRow(keyedFrames, preset.cols, preset.rows)
        rowGroups = preset.excludeLastFrameRows
          ? groups.map((row, i) => (preset.excludeLastFrameRows!.includes(i) && row.length > 1 ? row.slice(0, -1) : row))
          : groups
        for (let ri = 0; ri < rowGroups.length; ri++) {
          rowLabels.push(preset.rowLabels?.[ri]?.toLowerCase().replace(/\s+/g, '-') || `row-${ri}`)
        }
      } else {
        rowGroups = [keyedFrames]
        rowLabels.push(preset.id)
      }

      setRows(rowGroups.map((frames, i) => ({
        frames,
        label: rowLabels[i] || `row-${i}`,
        offset: { dx: 0, dy: 0 },
      })))
      setStep('tuning')
    } catch (err: any) {
      setError(err.message || String(err))
      setStep('upload')
    }
  }, [pipeline])

  const updateOffset = (rowIdx: number, axis: 'dx' | 'dy', delta: number) => {
    setRows((prev) => prev.map((r, i) => i === rowIdx ? { ...r, offset: { ...r.offset, [axis]: r.offset[axis] + delta } } : r))
  }

  const handleSave = useCallback(async () => {
    if (!name.trim() || !pipeline) return
    if (existingNames.includes(name.trim())) {
      setError('角色名已存在，请换一个名字')
      return
    }
    setSaving(true)
    setError('')
    try {
      for (const row of rows) {
        if (row.frames.length === 0) continue
        const offsets = row.frames.map(() => row.offset)
        const blob = await exportGif({
          frames: row.frames,
          frameOrder: row.frames.map((_, i) => i),
          offsets,
          fps,
          useOffsets: true,
        })
        const reader = new FileReader()
        const dataUrl: string = await new Promise((resolve) => {
          reader.onloadend = () => resolve(reader.result as string)
          reader.readAsDataURL(blob)
        })
        let label = row.label
        if (label === 'unused') continue
        const subfolder = pipeline.id === 'pet'
          ? `pet/${row.label === 'work' ? 'work' : row.label === 'crawl' ? 'crawl' : 'rest'}`
          : pipeline.id === 'top' ? 'mini/top'
          : `mini/${row.label.replace('mini-', '')}`
        await invoke('save_character_gif', { charName: name.trim(), fileName: `${label}.gif`, subfolder, dataUrl })
      }
      await invoke('scan_characters')
      onSaved()
      onClose()
    } catch (err: any) {
      setError(err.message || String(err))
    }
    setSaving(false)
  }, [name, rows, pipeline, fps, onSaved, onClose, existingNames])

  if (!isOpen) return null

  return (
    <div className="fixed inset-0 z-50 bg-black/80 backdrop-blur-sm flex items-center justify-center p-4" onMouseDown={(e) => { if (e.target === e.currentTarget) onClose() }}>
      <div className="bg-[#141414] border border-white/10 rounded-2xl w-full max-w-xl overflow-hidden shadow-2xl flex flex-col max-h-[90vh]" onMouseDown={(e) => e.stopPropagation()}>
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-white/5 shrink-0">
          <h3 className="text-lg font-medium text-white">创建新角色</h3>
          <button onClick={onClose} className="text-white/50 hover:text-white transition-colors">
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="p-6 flex-1 overflow-y-auto scrollbar-thin">
          {step === 'upload' && (
            <div className="flex flex-col gap-6">
              {/* Step 1 */}
              <div className="flex flex-col gap-2">
                <div className="flex items-center gap-2 text-white/90 font-medium text-sm">
                  <span className="flex items-center justify-center w-5 h-5 rounded-full bg-white/10 text-xs font-mono">1</span>
                  前往 Gemini 生成角色精灵图
                </div>
                <p className="text-xs text-white/50 pl-7">
                  使用 Google Gemini 生成角色精灵图。
                </p>
                <button
                  onClick={() => invoke('open_url', { url: 'https://gemini.google.com/gem/f30e7b9be50d' })}
                  className="ml-7 inline-flex items-center gap-1.5 text-xs bg-white/10 hover:bg-white/20 text-white/80 w-fit px-3 py-1.5 rounded-lg transition-colors mt-1 border border-white/5"
                >
                  打开 Gemini <ExternalLink className="w-3 h-3" />
                </button>
              </div>

              {/* Step 2 */}
              <div className="flex flex-col gap-2">
                <div className="flex items-center gap-2 text-white/90 font-medium text-sm">
                  <span className="flex items-center justify-center w-5 h-5 rounded-full bg-white/10 text-xs font-mono">2</span>
                  上传图片进行自动切分
                </div>
                <label className="mt-2 ml-7 flex flex-col items-center justify-center h-40 border-2 border-dashed border-white/10 rounded-xl hover:border-white/30 hover:bg-white/5 transition-all cursor-pointer group">
                  <Upload className="w-8 h-8 text-white/30 group-hover:text-white/60 mb-3 transition-colors" />
                  <span className="text-sm text-white/50 group-hover:text-white/80 transition-colors">点击或拖拽上传图片</span>
                  <span className="text-xs text-white/30 mt-1">支持 PNG, JPG, WEBP</span>
                  <input type="file" accept="image/*" className="hidden" onChange={handleFileChange} />
                </label>
              </div>

              {error && (
                <div className="text-sm text-red-400 bg-red-500/10 border border-red-500/20 rounded-lg p-3">{error}</div>
              )}
            </div>
          )}

          {step === 'processing' && (
            <div className="flex flex-col items-center justify-center h-64 gap-4">
              <Loader2 className="w-10 h-10 text-white/80 animate-spin" />
              <div className="flex flex-col items-center gap-1">
                <span className="text-white/80 font-medium">正在处理图像...</span>
                <span className="text-xs text-white/40">识别动作帧并生成 GIF 中</span>
              </div>
            </div>
          )}

          {step === 'tuning' && (
            <div className="flex flex-col gap-6">
              {/* Name */}
              <div className="flex flex-col gap-2">
                <label className="text-sm text-white/80 font-medium">角色名称</label>
                <input
                  type="text"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  placeholder="输入角色名称..."
                  list="char-name-list-modal"
                  className="bg-black/50 border border-white/10 rounded-lg px-4 py-2.5 text-sm text-white placeholder:text-white/30 focus:outline-none focus:border-white/30 transition-colors"
                />
                <datalist id="char-name-list-modal">{existingNames.map((n) => <option key={n} value={n} />)}</datalist>
              </div>

              {/* Row previews with offset controls */}
              <div className="flex flex-col gap-3">
                <div className="flex items-center justify-between">
                  <label className="text-sm text-white/80 font-medium">微调动作帧</label>
                  <span className="text-xs text-white/40">如果自动切分有偏移，请调整 X/Y 坐标</span>
                </div>

                <div className={`grid gap-4`} style={{ gridTemplateColumns: `repeat(${Math.min(rows.filter(r => r.label !== 'unused').length, 3)}, 1fr)` }}>
                  {rows.filter(r => r.label !== 'unused').map((row) => { const ri = rows.indexOf(row); return (
                    <div key={ri} className="flex flex-col items-center gap-3 p-3 bg-white/5 rounded-xl border border-white/5">
                      <span className="text-xs text-white/50 uppercase font-medium tracking-wider">
                        {row.label}
                      </span>
                      <div className="w-20 h-20 bg-black/50 rounded-lg overflow-hidden border border-white/10 flex items-center justify-center">
                        <AnimPreview
                          frames={row.frames}
                          offsets={row.frames.map(() => row.offset)}
                          fps={fps}
                          size={72}
                        />
                      </div>
                      <div className="flex flex-col gap-2 w-full">
                        <div className="flex items-center justify-between bg-black/30 rounded px-2 py-1">
                          <span className="text-[10px] text-white/40 font-mono">X</span>
                          <div className="flex items-center gap-1">
                            <button onClick={() => updateOffset(ri, 'dx', -5)} className="w-5 h-5 flex items-center justify-center text-white/40 hover:text-white hover:bg-white/10 rounded">-</button>
                            <span className="text-xs text-white w-6 text-center font-mono">{row.offset.dx}</span>
                            <button onClick={() => updateOffset(ri, 'dx', 5)} className="w-5 h-5 flex items-center justify-center text-white/40 hover:text-white hover:bg-white/10 rounded">+</button>
                          </div>
                        </div>
                        <div className="flex items-center justify-between bg-black/30 rounded px-2 py-1">
                          <span className="text-[10px] text-white/40 font-mono">Y</span>
                          <div className="flex items-center gap-1">
                            <button onClick={() => updateOffset(ri, 'dy', -5)} className="w-5 h-5 flex items-center justify-center text-white/40 hover:text-white hover:bg-white/10 rounded">-</button>
                            <span className="text-xs text-white w-6 text-center font-mono">{row.offset.dy}</span>
                            <button onClick={() => updateOffset(ri, 'dy', 5)} className="w-5 h-5 flex items-center justify-center text-white/40 hover:text-white hover:bg-white/10 rounded">+</button>
                          </div>
                        </div>
                      </div>
                    </div>
                  )})}
                </div>
              </div>

              {error && (
                <div className="text-sm text-red-400 bg-red-500/10 border border-red-500/20 rounded-lg p-3">{error}</div>
              )}
            </div>
          )}
        </div>

        {/* Footer */}
        {step === 'tuning' && (
          <div className="p-4 border-t border-white/5 flex justify-end gap-3 bg-black/20 shrink-0">
            <button
              onClick={() => { setStep('upload'); setRows([]); setError('') }}
              className="px-4 py-2 rounded-lg text-sm font-medium text-white/60 hover:text-white hover:bg-white/5 transition-colors"
            >
              重新上传
            </button>
            <button
              onClick={onClose}
              className="px-4 py-2 rounded-lg text-sm font-medium text-white/60 hover:text-white hover:bg-white/5 transition-colors"
            >
              取消
            </button>
            <button
              onClick={handleSave}
              disabled={!name.trim() || saving}
              className="px-6 py-2 rounded-lg text-sm font-medium bg-white text-black hover:bg-white/90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
            >
              {saving ? '保存中...' : '保存角色'}
            </button>
          </div>
        )}
      </div>
    </div>
  )
}
