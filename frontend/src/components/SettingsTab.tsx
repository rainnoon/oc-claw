import { useState, useEffect, useRef, useCallback } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { Loader2, Check, ChevronDown, Copy, Plus, Trash2 } from 'lucide-react'
import { AnimatePresence, motion } from 'motion/react'
import { getStore, loadOcConnections, saveOcConnections } from '../lib/store'
import type { OcConnection } from '../lib/types'

function Toggle({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <button
      onClick={() => onChange(!checked)}
      className={`relative inline-flex h-6 w-11 shrink-0 cursor-pointer rounded-full border-2 border-transparent transition-colors duration-200 ease-in-out focus:outline-none ${checked ? 'bg-blue-500' : 'bg-white/10'}`}
      role="switch"
      aria-checked={checked}
    >
      <span
        className={`pointer-events-none inline-block h-5 w-5 transform rounded-full bg-white shadow ring-0 transition duration-200 ease-in-out ${checked ? 'translate-x-5' : 'translate-x-0'}`}
      />
    </button>
  )
}

function CopyCode({ text }: { text: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="flex items-center gap-1 bg-black/40 rounded overflow-hidden">
      <code className="flex-1 px-2 py-1 text-[11px] text-white/60 font-mono select-all">{text}</code>
      <button
        onClick={() => { navigator.clipboard.writeText(text); setCopied(true); setTimeout(() => setCopied(false), 1500) }}
        className="px-1.5 py-1 text-white/30 hover:text-white/60 transition-colors shrink-0"
      >
        {copied ? <Check className="w-3 h-3 text-emerald-400" /> : <Copy className="w-3 h-3" />}
      </button>
    </div>
  )
}

function ConnectionRow({ conn, onUpdate, onDelete }: { conn: OcConnection; onUpdate: (c: OcConnection) => void; onDelete: () => void }) {
  const [testing, setTesting] = useState(false)
  const [testResult, setTestResult] = useState<'success' | 'error' | null>(null)
  const [testMsg, setTestMsg] = useState('')
  const [showGuide, setShowGuide] = useState(false)

  const testConnection = async () => {
    setTesting(true)
    setTestResult(null)
    setTestMsg('')
    try {
      if (conn.type === 'remote') {
        const result: any = await invoke('get_agents', { mode: 'remote', sshHost: conn.host, sshUser: conn.user })
        setTestMsg(`${result.length} 个 agent`)
      } else {
        const store = await getStore()
        const agentId = ((await store.get('tracked_agent')) as string) || 'main'
        const result: any = await invoke('get_status', { gatewayUrl: 'http://localhost:4446', token: '', agentId })
        setTestMsg(`${result.sessions.length} 个 session`)
      }
      setTestResult('success')
      setTimeout(() => setTestResult(null), 3000)
    } catch (e: any) {
      setTestResult('error')
      setTestMsg(String(e))
    }
    setTesting(false)
  }

  return (
    <div className="p-4 flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <div className="flex bg-black/50 p-0.5 rounded-lg border border-white/5">
            {(['local', 'remote'] as const).map((t) => (
              <button
                key={t}
                onClick={() => onUpdate({ ...conn, type: t })}
                className={`px-3 py-1 text-xs font-medium rounded-md transition-colors ${conn.type === t ? 'bg-white/10 text-white' : 'text-white/40 hover:text-white/60'}`}
              >
                {t === 'local' ? '本地' : '远程'}
              </button>
            ))}
          </div>
          <span className="text-xs text-white/30">
            {conn.type === 'local' ? '~/.openclaw' : conn.host ? `${conn.user || 'root'}@${conn.host}` : '未配置'}
          </span>
        </div>
        <button onClick={onDelete} className="p-1.5 text-white/20 hover:text-red-400 transition-colors rounded-lg hover:bg-red-500/10">
          <Trash2 className="w-3.5 h-3.5" />
        </button>
      </div>

      <AnimatePresence>
        {conn.type === 'remote' && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: 'auto', opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            className="flex flex-col gap-3 overflow-hidden"
          >
            <div className="flex gap-2">
              <input
                type="text"
                value={conn.user || ''}
                onChange={(e) => onUpdate({ ...conn, user: e.target.value })}
                placeholder="用户名"
                className="w-24 bg-black/50 border border-white/10 rounded-lg px-3 py-2 text-sm text-white placeholder:text-white/30 focus:outline-none focus:border-white/30 transition-colors"
              />
              <span className="self-center text-white/30 text-sm">@</span>
              <input
                type="text"
                value={conn.host || ''}
                onChange={(e) => onUpdate({ ...conn, host: e.target.value })}
                placeholder="服务器地址"
                className="flex-1 bg-black/50 border border-white/10 rounded-lg px-3 py-2 text-sm text-white placeholder:text-white/30 focus:outline-none focus:border-white/30 transition-colors"
              />
            </div>
            <button
              onClick={() => setShowGuide(!showGuide)}
              className="flex items-center gap-1 text-xs text-white/40 hover:text-white/60 transition-colors w-fit"
            >
              <ChevronDown className={`w-3 h-3 transition-transform ${showGuide ? 'rotate-0' : '-rotate-90'}`} />
              如何连接远程服务器？
            </button>
            <AnimatePresence>
              {showGuide && (
                <motion.div
                  initial={{ height: 0, opacity: 0 }}
                  animate={{ height: 'auto', opacity: 1 }}
                  exit={{ height: 0, opacity: 0 }}
                  className="overflow-hidden"
                >
                  <div className="bg-white/[0.03] border border-white/5 rounded-lg p-3 flex flex-col gap-2 text-xs text-white/50 leading-relaxed">
                    <p className="text-white/70 font-medium">前置条件</p>
                    <p>远程服务器需安装 OpenClaw 并运行 Gateway</p>
                    <p className="text-white/70 font-medium pt-1">步骤</p>
                    <p>1. 生成本地 SSH 密钥（如果没有）</p>
                    <CopyCode text="ssh-keygen -t ed25519" />
                    <p>2. 将公钥复制到远程服务器</p>
                    <CopyCode text="ssh-copy-id -i ~/.ssh/id_ed25519.pub 用户名@xx.xx.xx.xx" />
                    <p>3. 验证免密登录</p>
                    <CopyCode text={`ssh 用户名@xx.xx.xx.xx "echo ok"`} />
                    <p>4. 填入用户名和服务器地址，点击「测试」</p>
                  </div>
                </motion.div>
              )}
            </AnimatePresence>
          </motion.div>
        )}
      </AnimatePresence>

      <div className="flex items-center gap-2">
        <button
          onClick={testConnection}
          disabled={testing || (conn.type === 'remote' && (!conn.host || !conn.user))}
          className="px-3 py-1.5 bg-white/5 hover:bg-white/10 border border-white/10 rounded-lg text-xs font-medium text-white transition-colors flex items-center gap-1.5 disabled:opacity-50"
        >
          {testing && <Loader2 className="w-3 h-3 animate-spin" />}
          测试
        </button>
        {testResult === 'success' && (
          <span className="text-xs text-emerald-400 flex items-center gap-1">
            <Check className="w-3 h-3" /> 成功 {testMsg && `· ${testMsg}`}
          </span>
        )}
        {testResult === 'error' && (
          <span className="text-xs text-red-400 truncate max-w-[200px]" title={testMsg}>
            失败: {testMsg}
          </span>
        )}
      </div>
    </div>
  )
}

export function SettingsTab({ disableSleepAnim, onToggleSleepAnim, notifySound, onChangeNotifySound, waitingSound, onToggleWaitingSound, mascotPosition, onChangeMascotPosition, islandBg, onChangeIslandBg, bgPos, onChangeBgPos }: { disableSleepAnim: boolean; onToggleSleepAnim: (v: boolean) => void; notifySound: 'default' | 'manbo'; onChangeNotifySound: (v: 'default' | 'manbo') => void; waitingSound: boolean; onToggleWaitingSound: (v: boolean) => void; mascotPosition: 'left' | 'right'; onChangeMascotPosition: (v: 'left' | 'right') => void; islandBg: string; onChangeIslandBg: (v: string) => void; bgPos: { x: number; y: number }; onChangeBgPos: (v: { x: number; y: number }) => void }) {
  const [connections, setConnections] = useState<OcConnection[]>([])
  const [enableClaudeCode, setEnableClaudeCode] = useState(true)
  const [hookStatus, setHookStatus] = useState('')
  const [updateInfo, setUpdateInfo] = useState<{ current: string; latest: string; hasUpdate: boolean; url: string } | null>(null)
  const [updateChecking, setUpdateChecking] = useState(false)
  const [updating, setUpdating] = useState(false)
  const [backgrounds, setBackgrounds] = useState<string[]>([])
  const [bgPreviewUrl, setBgPreviewUrl] = useState<string | null>(null)
  const [bgNaturalSize, setBgNaturalSize] = useState<{ w: number; h: number } | null>(null)
  const cropContainerRef = useRef<HTMLDivElement>(null)
  const draggingRef = useRef(false)

  useEffect(() => {
    ;(async () => {
      const conns = await loadOcConnections()
      setConnections(conns)
      const store = await getStore()
      const cc = await store.get('enable_claudecode')
      if (typeof cc === 'boolean') setEnableClaudeCode(cc)
    })()
    invoke('check_for_update').then((info: any) => setUpdateInfo(info)).catch(() => {})
    invoke('list_backgrounds').then((list: any) => setBackgrounds(list as string[])).catch(() => {})
  }, [])

  // Load preview image for current background
  useEffect(() => {
    if (!islandBg) return
    // Try public path first (bundled), fallback to Rust command (custom)
    const img = new Image()
    img.onload = () => { setBgPreviewUrl(img.src); setBgNaturalSize({ w: img.naturalWidth, h: img.naturalHeight }) }
    img.onerror = () => {
      invoke('get_background_data', { fileName: islandBg }).then((dataUrl: any) => {
        const img2 = new Image()
        img2.onload = () => { setBgPreviewUrl(dataUrl as string); setBgNaturalSize({ w: img2.naturalWidth, h: img2.naturalHeight }) }
        img2.src = dataUrl as string
      }).catch(() => {})
    }
    img.src = `/assets/backgrounds/${islandBg}`
  }, [islandBg])

  // Handle file upload for custom background
  const handleBgUpload = useCallback(async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (!file) return
    const reader = new FileReader()
    reader.onload = async () => {
      const dataUrl = reader.result as string
      try {
        const saved = await invoke('save_background', { fileName: file.name, dataUrl }) as string
        // Refresh list and select
        const list = await invoke('list_backgrounds') as string[]
        setBackgrounds(list)
        onChangeIslandBg(saved)
      } catch (e: any) { console.error('save bg:', e) }
    }
    reader.readAsDataURL(file)
    e.target.value = ''
  }, [onChangeIslandBg])

  // Drag handler for crop rectangle
  const handleCropDrag = useCallback((e: React.MouseEvent | React.TouchEvent) => {
    e.preventDefault()
    const container = cropContainerRef.current
    if (!container || !bgNaturalSize) return
    draggingRef.current = true
    const rect = container.getBoundingClientRect()
    const update = (clientX: number, clientY: number) => {
      // The crop rect aspect ratio is ~7:1 (island width:height)
      // Container shows full image, crop rect shows visible portion
      const x = Math.max(0, Math.min(100, ((clientX - rect.left) / rect.width) * 100))
      const y = Math.max(0, Math.min(100, ((clientY - rect.top) / rect.height) * 100))
      onChangeBgPos({ x: Math.round(x), y: Math.round(y) })
    }
    const isTouch = 'touches' in e
    if (isTouch) {
      const t = (e as React.TouchEvent).touches[0]
      update(t.clientX, t.clientY)
    } else {
      update((e as React.MouseEvent).clientX, (e as React.MouseEvent).clientY)
    }
    const onMove = (ev: MouseEvent | TouchEvent) => {
      if (!draggingRef.current) return
      const p = 'touches' in ev ? (ev as TouchEvent).touches[0] : (ev as MouseEvent)
      update(p.clientX, p.clientY)
    }
    const onUp = () => { draggingRef.current = false; window.removeEventListener('mousemove', onMove); window.removeEventListener('mouseup', onUp); window.removeEventListener('touchmove', onMove); window.removeEventListener('touchend', onUp) }
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)
    window.addEventListener('touchmove', onMove)
    window.addEventListener('touchend', onUp)
  }, [bgNaturalSize, onChangeBgPos])

  const updateConnection = (idx: number, conn: OcConnection) => {
    const updated = [...connections]
    updated[idx] = conn
    setConnections(updated)
    saveOcConnections(updated)
  }

  const deleteConnection = (idx: number) => {
    const conn = connections[idx]
    if (conn.type === 'remote' && conn.host && conn.user) {
      invoke('close_ssh', { sshHost: conn.host, sshUser: conn.user }).catch(() => {})
    }
    const updated = connections.filter((_, i) => i !== idx)
    setConnections(updated)
    saveOcConnections(updated)
  }

  const addConnection = () => {
    const updated = [...connections, { id: crypto.randomUUID(), type: 'local' as const }]
    setConnections(updated)
    saveOcConnections(updated)
  }

  const toggleClaudeCode = async (val: boolean) => {
    setEnableClaudeCode(val)
    const store = await getStore()
    await store.set('enable_claudecode', val)
    await store.save()
    if (val) {
      try {
        await invoke('install_claude_hooks')
        setHookStatus('Hook 已安装')
      } catch (e: any) {
        setHookStatus(`安装失败: ${String(e)}`)
      }
    }
  }

  return (
    <div className="max-w-2xl mx-auto pt-10 pb-20 px-6 flex flex-col gap-10">
      {/* OpenClaw 连接 */}
      <section className="flex flex-col gap-4">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-medium text-white">OpenClaw 连接</h2>
          <button
            onClick={addConnection}
            className="flex items-center gap-1 px-3 py-1.5 bg-white/5 hover:bg-white/10 border border-white/10 rounded-lg text-xs font-medium text-white transition-colors"
          >
            <Plus className="w-3 h-3" /> 添加
          </button>
        </div>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden divide-y divide-white/5">
          {connections.length === 0 ? (
            <div className="text-center text-white/30 py-8 text-sm">
              暂无连接，点击「添加」来连接 OpenClaw 实例
            </div>
          ) : (
            connections.map((conn, idx) => (
              <ConnectionRow
                key={conn.id}
                conn={conn}
                onUpdate={(c) => updateConnection(idx, c)}
                onDelete={() => deleteConnection(idx)}
              />
            ))
          )}
        </div>
      </section>

      {/* Claude Code */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">Claude Code</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">启用 Claude Code</span>
              <span className="text-xs text-white/40">通过 Hook 监听本地 Claude Code 会话</span>
              {hookStatus && <span className="text-xs text-white/30 mt-1">{hookStatus}</span>}
            </div>
            <Toggle checked={enableClaudeCode} onChange={toggleClaudeCode} />
          </div>
        </div>
      </section>

      {/* 显示设置 */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">显示</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4 border-b border-white/5">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">看板娘位置</span>
              <span className="text-xs text-white/40">看板娘在刘海的哪一侧</span>
            </div>
            <div className="flex bg-black/50 p-0.5 rounded-lg border border-white/5">
              {(['left', 'right'] as const).map((s) => (
                <button
                  key={s}
                  onClick={() => onChangeMascotPosition(s)}
                  className={`px-3 py-1 text-xs font-medium rounded-md transition-colors ${mascotPosition === s ? 'bg-white/10 text-white' : 'text-white/40 hover:text-white/60'}`}
                >
                  {s === 'left' ? '居左' : '居右'}
                </button>
              ))}
            </div>
          </div>
          <div className="flex items-center justify-between p-4 border-b border-white/5">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">关闭睡眠动画</span>
              <span className="text-xs text-white/40">看板娘空闲时显示静态画面</span>
            </div>
            <Toggle checked={disableSleepAnim} onChange={onToggleSleepAnim} />
          </div>
          {/* Background picker with crop preview */}
          <div className="p-4">
            <div className="flex flex-col gap-1 mb-3">
              <span className="text-sm font-medium text-white/90">岛屿背景</span>
              <span className="text-xs text-white/40">选择或上传背景图片，拖动选择裁剪区域</span>
            </div>

            {/* Background thumbnails */}
            <div className="flex gap-2 flex-wrap mb-3">
              {backgrounds.map((bg) => (
                <button
                  key={bg}
                  onClick={() => onChangeIslandBg(bg)}
                  className={`relative w-14 h-9 rounded-lg overflow-hidden border-2 transition-all ${islandBg === bg ? 'border-blue-500 shadow-lg shadow-blue-500/20' : 'border-white/10 hover:border-white/30'}`}
                >
                  <div style={{ width: '100%', height: '100%', backgroundImage: `url(/assets/backgrounds/${bg})`, backgroundSize: 'cover' }} />
                </button>
              ))}
              {/* Upload button */}
              <label className="relative w-14 h-9 rounded-lg overflow-hidden border-2 border-dashed border-white/20 hover:border-white/40 transition-all cursor-pointer flex items-center justify-center">
                <Plus className="w-4 h-4 text-white/40" />
                <input type="file" accept="image/*" onChange={handleBgUpload} className="hidden" />
              </label>
            </div>

            {/* Crop preview */}
            {bgPreviewUrl && bgNaturalSize && (
              <div className="flex flex-col items-center gap-2">
                {/* Full image with crop overlay */}
                <div
                  ref={cropContainerRef}
                  className="relative rounded-lg overflow-hidden cursor-crosshair select-none"
                  style={{ width: '100%', maxWidth: 360, aspectRatio: `${bgNaturalSize.w} / ${bgNaturalSize.h}` }}
                  onMouseDown={handleCropDrag}
                  onTouchStart={handleCropDrag}
                >
                  {/* Full image dimmed */}
                  <img src={bgPreviewUrl} alt="" draggable={false} style={{ width: '100%', height: '100%', objectFit: 'cover', opacity: 0.4 }} />
                  {/* Crop rectangle — aspect ratio ~7:1 matching the island strip */}
                  {(() => {
                    // The crop rect represents what background-position + cover would show
                    // Island area is roughly 7:1 aspect ratio
                    const cropAspect = 7
                    const imgAspect = bgNaturalSize.w / bgNaturalSize.h
                    // With cover, the image fills the island. Determine crop rect size relative to full image.
                    let cropW: number, cropH: number
                    if (imgAspect > cropAspect) {
                      // Image is wider than crop — full height used, crop width = subset
                      cropH = 100
                      cropW = (cropAspect / imgAspect) * 100
                    } else {
                      // Image is taller — full width used, crop height = subset
                      cropW = 100
                      cropH = (imgAspect / cropAspect) * 100
                    }
                    const maxX = 100 - cropW
                    const maxY = 100 - cropH
                    const left = (bgPos.x / 100) * maxX
                    const top = (bgPos.y / 100) * maxY
                    return (
                      <div
                        style={{
                          position: 'absolute',
                          left: `${left}%`, top: `${top}%`,
                          width: `${cropW}%`, height: `${cropH}%`,
                          border: '2px solid white',
                          borderRadius: 4,
                          boxShadow: '0 0 0 9999px rgba(0,0,0,0.5)',
                          pointerEvents: 'none',
                        }}
                      />
                    )
                  })()}
                </div>
                {/* Live preview strip */}
                <div className="rounded-lg overflow-hidden border border-white/10" style={{ width: '100%', maxWidth: 360, height: 50 }}>
                  <div style={{
                    width: '100%', height: '100%',
                    backgroundImage: `url(${bgPreviewUrl})`,
                    backgroundSize: 'cover',
                    backgroundPosition: `${bgPos.x}% ${bgPos.y}%`,
                  }} />
                </div>
              </div>
            )}
          </div>
        </div>
      </section>

      {/* 提示音 */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">提示音</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4 border-b border-white/5">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">完成提示音</span>
              <span className="text-xs text-white/40">任务完成时播放的提示音</span>
            </div>
            <div className="flex bg-black/50 p-0.5 rounded-lg border border-white/5">
              {(['default', 'manbo'] as const).map((s) => (
                <button
                  key={s}
                  onClick={() => onChangeNotifySound(s)}
                  className={`px-3 py-1 text-xs font-medium rounded-md transition-colors ${notifySound === s ? 'bg-white/10 text-white' : 'text-white/40 hover:text-white/60'}`}
                >
                  {s === 'default' ? '默认' : '曼波'}
                </button>
              ))}
            </div>
          </div>
          <div className="flex items-center justify-between p-4">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">等待时提示音</span>
              <span className="text-xs text-white/40">Claude Code 等待用户确认时播放提示音</span>
            </div>
            <Toggle checked={waitingSound} onChange={onToggleWaitingSound} />
          </div>
        </div>
      </section>

      {/* 关于 */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">关于</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">当前版本</span>
              <span className="text-xs text-white/40">
                {updateInfo ? `v${updateInfo.current}` : '...'}
                {updateInfo && !updateInfo.hasUpdate && ' (Latest)'}
                {updateInfo?.hasUpdate && (
                  <span className="ml-2 text-emerald-400">v{updateInfo.latest} 可用</span>
                )}
              </span>
            </div>
            <div className="flex items-center gap-2">
              {updateInfo?.hasUpdate && (
                <button
                  onClick={async () => {
                    setUpdating(true)
                    try { await invoke('run_update') } catch (e: any) { setUpdating(false) }
                  }}
                  disabled={updating}
                  className="px-4 py-2 bg-blue-500 hover:bg-blue-600 text-white rounded-lg text-sm font-medium transition-colors disabled:opacity-50"
                >
                  {updating ? '更新中...' : '立即更新'}
                </button>
              )}
              <button
                onClick={async () => {
                  setUpdateChecking(true)
                  try {
                    const info = await invoke('check_for_update') as any
                    setUpdateInfo(info)
                  } catch { setUpdateInfo(null) }
                  setUpdateChecking(false)
                }}
                disabled={updateChecking}
                className="px-4 py-2 bg-white/5 hover:bg-white/10 border border-white/10 rounded-lg text-sm font-medium text-white transition-colors disabled:opacity-50"
              >
                {updateChecking ? '检查中...' : '检查更新'}
              </button>
            </div>
          </div>
        </div>
      </section>

      {/* 退出 */}
      <section className="pt-4">
        <button
          onClick={() => invoke('exit_app').catch(() => {})}
          className="w-full py-3 bg-red-500/10 hover:bg-red-500/20 border border-red-500/20 text-red-400 rounded-xl text-sm font-medium transition-colors"
        >
          退出应用
        </button>
      </section>
    </div>
  )
}
