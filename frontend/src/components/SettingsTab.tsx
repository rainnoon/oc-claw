import { useState, useEffect, useRef, useCallback } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { Loader2, Check, ChevronDown, Copy, Plus, Trash2 } from 'lucide-react'
import { AnimatePresence, motion } from 'motion/react'
import { useTranslation } from 'react-i18next'
import { getStore, loadOcConnections, saveOcConnections } from '../lib/store'
import type { OcConnection } from '../lib/types'

type UpdateProgressPayload = {
  stage: string
  progress?: number | null
  downloadedBytes?: number
  totalBytes?: number | null
  message?: string
}

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

function ConnectionRow({ conn, onUpdate, onDelete, disableLocal }: { conn: OcConnection; onUpdate: (c: OcConnection) => void; onDelete: () => void; disableLocal?: boolean }) {
  const { t } = useTranslation()
  const [testing, setTesting] = useState(false)
  const [testResult, setTestResult] = useState<'success' | 'error' | null>(null)
  const [testMsg, setTestMsg] = useState('')
  const [showGuide, setShowGuide] = useState(false)
  // Used to discard results from a cancelled test — the Tauri invoke can't
  // be aborted, but we stop the UI from acting on stale results.
  const cancelledRef = useRef(false)

  const testConnection = async () => {
    cancelledRef.current = false
    setTesting(true)
    setTestResult(null)
    setTestMsg('')
    try {
      if (conn.type === 'remote') {
        // Reset backoff and stale socket so manual test always retries immediately
        await invoke('reset_ssh', { sshHost: conn.host, sshUser: conn.user }).catch(() => {})
        const result: any = await invoke('get_agents', { mode: 'remote', sshHost: conn.host, sshUser: conn.user })
        if (cancelledRef.current) return
        // Query which SSH key was used for this connection
        let keyInfo = ''
        try {
          const key = await invoke('get_ssh_key_info', { sshHost: conn.host, sshUser: conn.user }) as string | null
          if (key) keyInfo = ` · ${t('settings.key')} ${key}`
        } catch {}
        setTestMsg(`${result.length} ${t('settings.agents')}${keyInfo}`)
      } else {
        const store = await getStore()
        const agentId = ((await store.get('tracked_agent')) as string) || 'main'
        const result: any = await invoke('get_status', { gatewayUrl: 'http://localhost:4446', token: '', agentId })
        if (cancelledRef.current) return
        setTestMsg(`${result.sessions.length} ${t('settings.sessions')}`)
      }
      setTestResult('success')
      setTimeout(() => setTestResult(null), 3000)
    } catch (e: any) {
      if (cancelledRef.current) return
      setTestResult('error')
      setTestMsg(String(e))
    }
    setTesting(false)
  }

  const cancelTest = () => {
    cancelledRef.current = true
    setTesting(false)
    setTestResult(null)
    setTestMsg('')
    // Kill the SSH connection so it doesn't hang in the background
    if (conn.type === 'remote' && conn.host && conn.user) {
      invoke('close_ssh', { sshHost: conn.host, sshUser: conn.user }).catch(() => {})
    }
  }

  return (
    <div className="p-4 flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <div className="flex bg-black/50 p-0.5 rounded-lg border border-white/5">
            {(['local', 'remote'] as const).map((typ) => {
              // Only one local connection allowed across all connections
              const disabled = typ === 'local' && disableLocal && conn.type !== 'local'
              return (
                <button
                  key={typ}
                  onClick={() => !disabled && onUpdate({ ...conn, type: typ })}
                  className={`px-3 py-1 text-xs font-medium rounded-md transition-colors ${conn.type === typ ? 'bg-white/10 text-white' : disabled ? 'text-white/15 cursor-not-allowed' : 'text-white/40 hover:text-white/60'}`}
                >
                  {typ === 'local' ? t('settings.local') : t('settings.remote')}
                </button>
              )
            })}
          </div>
          <span className="text-xs text-white/30">
            {conn.type === 'local' ? '~/.openclaw' : conn.host ? `${conn.user || 'root'}@${conn.host}` : t('settings.notConfigured')}
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
                placeholder={t('settings.username')}
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                className="w-24 bg-black/50 border border-white/10 rounded-lg px-3 py-2 text-sm text-white placeholder:text-white/30 focus:outline-none focus:border-white/30 transition-colors"
              />
              <span className="self-center text-white/30 text-sm">@</span>
              <input
                type="text"
                value={conn.host || ''}
                onChange={(e) => onUpdate({ ...conn, host: e.target.value })}
                placeholder={t('settings.serverAddress')}
                className="flex-1 bg-black/50 border border-white/10 rounded-lg px-3 py-2 text-sm text-white placeholder:text-white/30 focus:outline-none focus:border-white/30 transition-colors"
              />
            </div>
            <button
              onClick={() => setShowGuide(!showGuide)}
              className="flex items-center gap-1 text-xs text-white/40 hover:text-white/60 transition-colors w-fit"
            >
              <ChevronDown className={`w-3 h-3 transition-transform ${showGuide ? 'rotate-0' : '-rotate-90'}`} />
              {t('settings.howToConnect')}
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
                    <p className="text-white/70 font-medium">{t('settings.prerequisites')}</p>
                    <p>{t('settings.prerequisitesDesc')}</p>
                    <p className="text-white/70 font-medium pt-1">{t('settings.steps')}</p>
                    <p>{t('settings.step1')}</p>
                    <CopyCode text="ssh-keygen -t ed25519" />
                    <p>{t('settings.step2')}</p>
                    <CopyCode text="ssh-copy-id -i ~/.ssh/id_ed25519.pub 用户名@xx.xx.xx.xx" />
                    <p>{t('settings.step3')}</p>
                    <CopyCode text={`ssh 用户名@xx.xx.xx.xx "echo ok"`} />
                    <p>{t('settings.step4')}</p>
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
          {t('common.test')}
        </button>
        {testing && (
          <button
            onClick={cancelTest}
            className="px-3 py-1.5 bg-white/5 hover:bg-red-500/20 border border-white/10 rounded-lg text-xs font-medium text-white/50 hover:text-red-400 transition-colors"
          >
            {t('common.cancel')}
          </button>
        )}
        {testResult === 'success' && (
          <span className="text-xs text-emerald-400 flex items-center gap-1">
            <Check className="w-3 h-3" /> {t('common.success')} {testMsg && `· ${testMsg}`}
          </span>
        )}
        {testResult === 'error' && (
          <div className="text-xs text-red-400 w-full">
            <span>{t('common.failed')}</span>
            <pre className="mt-1 p-2 bg-red-500/10 border border-red-500/20 rounded-lg whitespace-pre-wrap break-all max-h-[120px] overflow-y-auto font-mono text-[11px] leading-relaxed select-text">
              {testMsg}
            </pre>
          </div>
        )}
      </div>
    </div>
  )
}

export function SettingsTab({ disableSleepAnim, onToggleSleepAnim, notifySound, onChangeNotifySound, waitingSound, onToggleWaitingSound, soundEnabled, onToggleSoundEnabled, codexSoundEnabled, onToggleCodexSoundEnabled, cursorSoundEnabled, onToggleCursorSoundEnabled, autoCloseCompletion, onToggleAutoCloseCompletion, mascotPosition, onChangeMascotPosition, islandBg, onChangeIslandBg, bgPos, onChangeBgPos, panelMaxHeight, onChangePanelMaxHeight, hoverDelay, onChangeHoverDelay }: { disableSleepAnim: boolean; onToggleSleepAnim: (v: boolean) => void; notifySound: 'default' | 'manbo'; onChangeNotifySound: (v: 'default' | 'manbo') => void; waitingSound: boolean; onToggleWaitingSound: (v: boolean) => void; soundEnabled: boolean; onToggleSoundEnabled: (v: boolean) => void; codexSoundEnabled: boolean; onToggleCodexSoundEnabled: (v: boolean) => void; cursorSoundEnabled: boolean; onToggleCursorSoundEnabled: (v: boolean) => void; autoCloseCompletion: boolean; onToggleAutoCloseCompletion: (v: boolean) => void; mascotPosition: 'left' | 'right'; onChangeMascotPosition: (v: 'left' | 'right') => void; islandBg: string; onChangeIslandBg: (v: string) => void; bgPos: { x: number; y: number }; onChangeBgPos: (v: { x: number; y: number }) => void; panelMaxHeight: number; onChangePanelMaxHeight: (v: number) => void; hoverDelay: number; onChangeHoverDelay: (v: number) => void }) {
  const { t, i18n } = useTranslation()
  const [connections, setConnections] = useState<OcConnection[]>([])
  const [enableClaudeCode, setEnableClaudeCode] = useState(true)
  const [hookStatus, setHookStatus] = useState('')
  const [enableCodex, setEnableCodex] = useState(true)
  const [codexHookStatus, setCodexHookStatus] = useState('')
  const [enableCursor, setEnableCursor] = useState(true)
  const [cursorHookStatus, setCursorHookStatus] = useState('')
  const [updateInfo, setUpdateInfo] = useState<{ current: string; latest: string; hasUpdate: boolean; url: string } | null>(null)
  const [updateChecking, setUpdateChecking] = useState(false)
  const [updateCheckResult, setUpdateCheckResult] = useState<'success' | 'error' | null>(null)
  const [updateCheckMsg, setUpdateCheckMsg] = useState('')
  const [updating, setUpdating] = useState(false)
  const [updateProgress, setUpdateProgress] = useState<number | null>(null)
  const [updateProgressMsg, setUpdateProgressMsg] = useState('')
  const [updateRunResult, setUpdateRunResult] = useState<'success' | 'error' | null>(null)
  const [updateRunMsg, setUpdateRunMsg] = useState('')
  const [backgrounds, setBackgrounds] = useState<string[]>([])
  const [bgPreviewUrl, setBgPreviewUrl] = useState<string | null>(null)
  const [bgNaturalSize, setBgNaturalSize] = useState<{ w: number; h: number } | null>(null)
  const cropContainerRef = useRef<HTMLDivElement>(null)
  const draggingRef = useRef(false)
  const showIslandBackgroundSettings = false
  const resolveUpdateProgressText = useCallback((stage?: string, fallbackMessage?: string) => {
    if (stage) {
      const key = `updateModal.progress.${stage}`
      const localized = t(key)
      if (localized !== key) return localized
    }
    return fallbackMessage || ''
  }, [t])

  const checkForUpdate = useCallback(async (showFeedback = false) => {
    setUpdateChecking(true)
    if (showFeedback) {
      setUpdateCheckResult(null)
      setUpdateCheckMsg('')
    }
    try {
      const info = await invoke('check_for_update', { lang: i18n.language }) as { current: string; latest: string; hasUpdate: boolean; url: string; notes?: string }
      setUpdateInfo(info)
      if (showFeedback) {
        setUpdateCheckResult('success')
        setUpdateCheckMsg(info.hasUpdate ? `${t('settings.newVersionFound')} v${info.latest}` : t('settings.alreadyLatest'))
      }
    } catch (e: any) {
      if (showFeedback) {
        setUpdateCheckResult('error')
        setUpdateCheckMsg(`${t('settings.checkFailed')}${String(e)}`)
      }
    } finally {
      setUpdateChecking(false)
    }
  }, [i18n.language, t])

  useEffect(() => {
    ;(async () => {
      const conns = await loadOcConnections()
      setConnections(conns)
      const store = await getStore()
      const cc = await store.get('enable_claudecode')
      if (typeof cc === 'boolean') setEnableClaudeCode(cc)
      const cod = await store.get('enable_codex')
      if (typeof cod === 'boolean') setEnableCodex(cod)
      const cur = await store.get('enable_cursor')
      if (typeof cur === 'boolean') setEnableCursor(cur)
    })()
    void checkForUpdate()
    if (showIslandBackgroundSettings) {
      invoke('list_backgrounds').then((list: any) => setBackgrounds(list as string[])).catch(() => {})
    }
  }, [checkForUpdate])

  useEffect(() => {
    const unlisten = listen<UpdateProgressPayload>('update-progress', (event) => {
      const payload = event.payload
      setUpdateProgress(typeof payload.progress === 'number' ? payload.progress : null)
      setUpdateProgressMsg(resolveUpdateProgressText(payload.stage, payload.message))
    })
    return () => { unlisten.then((fn) => fn()) }
  }, [resolveUpdateProgressText])

  // Load preview image for current background
  useEffect(() => {
    if (!showIslandBackgroundSettings || !islandBg) return
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
    // Default to remote if a local connection already exists (only one local allowed)
    const hasLocal = connections.some(c => c.type === 'local')
    const updated = [...connections, { id: crypto.randomUUID(), type: (hasLocal ? 'remote' : 'local') as OcConnection['type'] }]
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
        setHookStatus(t('settings.hookInstalled'))
      } catch (e: any) {
        setHookStatus(`${t('settings.hookFailed')} ${String(e)}`)
      }
    }
  }

  const toggleCursor = async (val: boolean) => {
    setEnableCursor(val)
    const store = await getStore()
    await store.set('enable_cursor', val)
    await store.save()
    if (val) {
      try {
        await invoke('install_cursor_hooks')
        setCursorHookStatus(t('settings.hookInstalled'))
      } catch (e: any) {
        setCursorHookStatus(`${t('settings.hookFailed')} ${String(e)}`)
      }
    }
  }

  const toggleCodex = async (val: boolean) => {
    setEnableCodex(val)
    const store = await getStore()
    await store.set('enable_codex', val)
    await store.save()
    if (val) {
      try {
        await invoke('install_claude_hooks')
        setCodexHookStatus(t('settings.hookInstalled'))
      } catch (e: any) {
        setCodexHookStatus(`${t('settings.hookFailed')} ${String(e)}`)
      }
    }
  }

  return (
    <div className="max-w-2xl mx-auto pt-10 pb-20 px-6 flex flex-col gap-10">
      {/* OpenClaw 连接 */}
      <section className="flex flex-col gap-4">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-medium text-white">{t('settings.ocConnections')}</h2>
          <button
            onClick={addConnection}
            className="flex items-center gap-1 px-3 py-1.5 bg-white/5 hover:bg-white/10 border border-white/10 rounded-lg text-xs font-medium text-white transition-colors"
          >
            <Plus className="w-3 h-3" /> {t('common.add')}
          </button>
        </div>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden divide-y divide-white/5">
          {connections.length === 0 ? (
            <div className="text-center text-white/30 py-8 text-sm">
              {t('settings.noConnections')}
            </div>
          ) : (
            connections.map((conn, idx) => (
              <ConnectionRow
                key={conn.id}
                conn={conn}
                onUpdate={(c) => updateConnection(idx, c)}
                onDelete={() => deleteConnection(idx)}
                disableLocal={connections.some((c, i) => i !== idx && c.type === 'local')}
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
              <span className="text-sm font-medium text-white/90">{t('settings.enableClaudeCode')}</span>
              <span className="text-xs text-white/40">{t('settings.enableClaudeCodeDesc')}</span>
              {hookStatus && <span className="text-xs text-white/30 mt-1">{hookStatus}</span>}
            </div>
            <Toggle checked={enableClaudeCode} onChange={toggleClaudeCode} />
          </div>
        </div>
      </section>

      {/* Cursor */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">{t('settings.codex', 'Codex')}</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.enableCodex', 'Enable Codex')}</span>
              <span className="text-xs text-white/40">{t('settings.enableCodexDesc', 'Monitor local Codex sessions via Hooks')}</span>
              {codexHookStatus && <span className="text-xs text-white/30 mt-1">{codexHookStatus}</span>}
            </div>
            <Toggle checked={enableCodex} onChange={toggleCodex} />
          </div>
        </div>
      </section>

      {/* Cursor */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">Cursor</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.enableCursor', 'Enable Cursor')}</span>
              <span className="text-xs text-white/40">{t('settings.enableCursorDesc', 'Monitor local Cursor agent sessions via Hooks')}</span>
              {cursorHookStatus && <span className="text-xs text-white/30 mt-1">{cursorHookStatus}</span>}
            </div>
            <Toggle checked={enableCursor} onChange={toggleCursor} />
          </div>
        </div>
      </section>

      {/* 显示设置 */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">{t('settings.display')}</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4 border-b border-white/5">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.mascotPosition')}</span>
              <span className="text-xs text-white/40">{t('settings.mascotPositionDesc')}</span>
            </div>
            <div className="flex bg-black/50 p-0.5 rounded-lg border border-white/5">
              {(['left', 'right'] as const).map((s) => (
                <button
                  key={s}
                  onClick={() => onChangeMascotPosition(s)}
                  className={`px-3 py-1 text-xs font-medium rounded-md transition-colors ${mascotPosition === s ? 'bg-white/10 text-white' : 'text-white/40 hover:text-white/60'}`}
                >
                  {s === 'left' ? t('settings.left') : t('settings.right')}
                </button>
              ))}
            </div>
          </div>
          <div className="flex items-center justify-between p-4 border-b border-white/5">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.disableSleepAnim')}</span>
              <span className="text-xs text-white/40">{t('settings.disableSleepAnimDesc')}</span>
            </div>
            <Toggle checked={disableSleepAnim} onChange={onToggleSleepAnim} />
          </div>
          <div className="p-4 border-b border-white/5">
            <div className="flex items-center justify-between mb-2">
              <div className="flex flex-col gap-1">
                <span className="text-sm font-medium text-white/90">{t('settings.panelMaxHeight', 'Panel Height')}</span>
                <span className="text-xs text-white/40">{t('settings.panelMaxHeightDesc', 'Maximum height of the expanded panel')}</span>
              </div>
              <span className="text-sm text-white/60 tabular-nums">{panelMaxHeight}px</span>
            </div>
            <input
              type="range"
              min={200}
              max={500}
              step={10}
              value={panelMaxHeight}
              onChange={(e) => onChangePanelMaxHeight(Number(e.target.value))}
              className="w-full accent-white/60 h-1"
            />
          </div>
          <div className={`p-4 ${showIslandBackgroundSettings ? 'border-b border-white/5' : ''}`}>
            <div className="flex items-center justify-between mb-2">
              <div className="flex flex-col gap-1">
                <span className="text-sm font-medium text-white/90">{t('settings.hoverDelay', 'Hover Trigger Delay')}</span>
                <span className="text-xs text-white/40">{t('settings.hoverDelayDesc', 'Delay before expanding panel on hover')}</span>
              </div>
              <span className="text-sm text-white/60 tabular-nums">{hoverDelay.toFixed(1)}s</span>
            </div>
            <input
              type="range"
              min={0}
              max={2}
              step={0.1}
              value={hoverDelay}
              onChange={(e) => onChangeHoverDelay(Number(e.target.value))}
              className="w-full accent-white/60 h-1"
            />
          </div>
          {showIslandBackgroundSettings && (
            <div className="p-4">
              <div className="flex flex-col gap-1 mb-3">
                <span className="text-sm font-medium text-white/90">{t('settings.islandBg')}</span>
                <span className="text-xs text-white/40">{t('settings.islandBgDesc')}</span>
              </div>

              <div className="flex gap-2 flex-wrap mb-3">
                <button
                  onClick={() => onChangeIslandBg('__anime__')}
                  className={`relative w-14 h-9 rounded-lg overflow-hidden border-2 transition-all ${islandBg === '__anime__' ? 'border-blue-500 shadow-lg shadow-blue-500/20' : 'border-white/10 hover:border-white/30'}`}
                >
                  <div style={{ width: '100%', height: '100%', background: '#F0D140' }}>
                    <div style={{ width: '100%', height: '100%', backgroundImage: 'linear-gradient(to right, #00000015 1px, transparent 1px), linear-gradient(to bottom, #00000015 1px, transparent 1px)', backgroundSize: '8px 8px' }} />
                  </div>
                </button>
                {backgrounds.map((bg) => (
                  <button
                    key={bg}
                    onClick={() => onChangeIslandBg(bg)}
                    className={`relative w-14 h-9 rounded-lg overflow-hidden border-2 transition-all ${islandBg === bg ? 'border-blue-500 shadow-lg shadow-blue-500/20' : 'border-white/10 hover:border-white/30'}`}
                  >
                    <div style={{ width: '100%', height: '100%', backgroundImage: `url(/assets/backgrounds/${bg})`, backgroundSize: 'cover' }} />
                  </button>
                ))}
                <label className="relative w-14 h-9 rounded-lg overflow-hidden border-2 border-dashed border-white/20 hover:border-white/40 transition-all cursor-pointer flex items-center justify-center">
                  <Plus className="w-4 h-4 text-white/40" />
                  <input type="file" accept="image/*" onChange={handleBgUpload} className="hidden" />
                </label>
              </div>

              {islandBg !== '__anime__' && bgPreviewUrl && bgNaturalSize && (
                <div className="flex flex-col items-center gap-2">
                  <div
                    ref={cropContainerRef}
                    className="relative rounded-lg overflow-hidden cursor-crosshair select-none"
                    style={{ width: '100%', maxWidth: 360, aspectRatio: `${bgNaturalSize.w} / ${bgNaturalSize.h}` }}
                    onMouseDown={handleCropDrag}
                    onTouchStart={handleCropDrag}
                  >
                    <img src={bgPreviewUrl} alt="" draggable={false} style={{ width: '100%', height: '100%', objectFit: 'cover', opacity: 0.4 }} />
                    {(() => {
                      const cropAspect = 7
                      const imgAspect = bgNaturalSize.w / bgNaturalSize.h
                      let cropW: number, cropH: number
                      if (imgAspect > cropAspect) {
                        cropH = 100
                        cropW = (cropAspect / imgAspect) * 100
                      } else {
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
          )}
        </div>
      </section>

      {/* 提示音 */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">{t('settings.sound')}</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4 border-b border-white/5">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.completionSound')}</span>
              <span className="text-xs text-white/40">{t('settings.completionSoundDesc')}</span>
            </div>
            <div className="flex bg-black/50 p-0.5 rounded-lg border border-white/5">
              {(['default', 'manbo'] as const).map((s) => (
                <button
                  key={s}
                  onClick={() => onChangeNotifySound(s)}
                  className={`px-3 py-1 text-xs font-medium rounded-md transition-colors ${notifySound === s ? 'bg-white/10 text-white' : 'text-white/40 hover:text-white/60'}`}
                >
                  {s === 'default' ? t('settings.defaultSound') : t('settings.manboSound')}
                </button>
              ))}
            </div>
          </div>
          <div className="flex items-center justify-between p-4 border-b border-white/5">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.ccSound', 'Claude Code Completion Sound')}</span>
              <span className="text-xs text-white/40">{t('settings.ccSoundDesc', 'Play sound when Claude Code finishes a task')}</span>
            </div>
            <Toggle checked={soundEnabled} onChange={onToggleSoundEnabled} />
          </div>
          <div className="flex items-center justify-between p-4 border-b border-white/5">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.codexSound', 'Codex Completion Sound')}</span>
              <span className="text-xs text-white/40">{t('settings.codexSoundDesc', 'Play sound when Codex finishes a task')}</span>
            </div>
            <Toggle checked={codexSoundEnabled} onChange={onToggleCodexSoundEnabled} />
          </div>
          <div className="flex items-center justify-between p-4 border-b border-white/5">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.cursorSound', 'Cursor Completion Sound')}</span>
              <span className="text-xs text-white/40">{t('settings.cursorSoundDesc', 'Play sound when Cursor finishes a task')}</span>
            </div>
            <Toggle checked={cursorSoundEnabled} onChange={onToggleCursorSoundEnabled} />
          </div>
          <div className="flex items-center justify-between p-4 border-b border-white/5">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.waitingSound')}</span>
              <span className="text-xs text-white/40">{t('settings.waitingSoundDesc')}</span>
            </div>
            <Toggle checked={waitingSound} onChange={onToggleWaitingSound} />
          </div>
          <div className="flex items-center justify-between p-4">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.autoCloseCompletion', 'Auto-close Completion Popup')}</span>
              <span className="text-xs text-white/40">{t('settings.autoCloseCompletionDesc', 'Automatically close the completion popup after 5 seconds')}</span>
            </div>
            <Toggle checked={autoCloseCompletion} onChange={onToggleAutoCloseCompletion} />
          </div>
        </div>
      </section>

      {/* 关于 */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">{t('settings.about')}</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.currentVersion')}</span>
              <span className="text-xs text-white/40">
                {updateInfo ? `v${updateInfo.current}` : '...'}
                {updateInfo && !updateInfo.hasUpdate && ` (${t('settings.latest')})`}
                {updateInfo?.hasUpdate && (
                  <span className="ml-2 text-emerald-400">v{updateInfo.latest} {t('settings.available')}</span>
                )}
              </span>
              {updateCheckResult === 'success' && updateCheckMsg && (
                <span className="text-xs text-emerald-400">{updateCheckMsg}</span>
              )}
              {updateCheckResult === 'error' && updateCheckMsg && (
                <span className="text-xs text-red-400 break-all">{updateCheckMsg}</span>
              )}
              {updateRunResult === 'success' && updateRunMsg && (
                <span className="text-xs text-emerald-400">{updateRunMsg}</span>
              )}
              {updateRunResult === 'error' && updateRunMsg && (
                <span className="text-xs text-red-400 break-all">{updateRunMsg}</span>
              )}
              {(updating || updateProgressMsg) && (
                <div className="flex flex-col gap-1 pt-1">
                  <span className="text-xs text-white/50">
                    {updateProgressMsg}
                    {typeof updateProgress === 'number' && updateProgress < 100 && ` · ${updateProgress}%`}
                  </span>
                  {typeof updateProgress === 'number' && (
                    <div className="w-full h-1.5 bg-white/5 rounded-full overflow-hidden">
                      <div
                        className="h-full bg-blue-500 transition-all duration-200"
                        style={{ width: `${Math.max(updateProgress, 2)}%` }}
                      />
                    </div>
                  )}
                </div>
              )}
            </div>
            <div className="flex items-center gap-2">
              {updateInfo?.hasUpdate && (
                <button
                  onClick={async () => {
                    setUpdating(true)
                    setUpdateProgress(0)
                    setUpdateProgressMsg(resolveUpdateProgressText('preparing', t('settings.preparingDownload')))
                    setUpdateRunResult(null)
                    setUpdateRunMsg('')
                    try {
                      await invoke('run_update', { dmgUrl: updateInfo?.url || '' })
                      setUpdateRunResult('success')
                      setUpdateRunMsg(t('settings.downloadComplete'))
                      window.setTimeout(() => {
                        void invoke('exit_app').catch((e: any) => {
                          setUpdating(false)
                          setUpdateRunResult('error')
                          setUpdateRunMsg(`${t('settings.exitFailed')}${String(e)}`)
                        })
                      }, 600)
                    } catch (e: any) {
                      setUpdateProgress(null)
                      setUpdateProgressMsg('')
                      setUpdateRunResult('error')
                      setUpdateRunMsg(`${t('settings.updateFailed')}${String(e)}`)
                      setUpdating(false)
                    }
                  }}
                  disabled={updating}
                  className="px-4 py-2 bg-blue-500 hover:bg-blue-600 text-white rounded-lg text-sm font-medium transition-colors disabled:opacity-50"
                >
                  {updating ? t('settings.updating') : t('settings.updateNow')}
                </button>
              )}
              <button
                onClick={() => { void checkForUpdate(true) }}
                disabled={updateChecking}
                className="px-4 py-2 bg-white/5 hover:bg-white/10 border border-white/10 rounded-lg text-sm font-medium text-white transition-colors disabled:opacity-50"
              >
                {updateChecking ? t('settings.checking') : t('settings.checkUpdate')}
              </button>
            </div>
          </div>
        </div>
      </section>

      {/* Language selector */}
      <section className="flex flex-col gap-4">
        <h2 className="text-lg font-medium text-white">{t('settings.language')}</h2>
        <div className="bg-[#0f0f0f] border border-white/5 rounded-2xl overflow-hidden">
          <div className="flex items-center justify-between p-4">
            <div className="flex flex-col gap-1">
              <span className="text-sm font-medium text-white/90">{t('settings.language')}</span>
              <span className="text-xs text-white/40">{t('settings.languageDesc')}</span>
            </div>
            <div className="flex flex-wrap bg-black/50 p-0.5 rounded-lg border border-white/5 gap-0.5">
              {(['zh', 'en', 'ja', 'ko', 'es', 'fr'] as const).map((lng) => (
                <button
                  key={lng}
                  onClick={async () => { i18n.changeLanguage(lng); localStorage.setItem('oc-claw-lang', lng); const store = await getStore(); await store.set('oc-claw-lang', lng); await store.save(); invoke('update_tray_language', { lang: lng }).catch(() => {}) }}
                  className={`px-2 py-1 text-xs font-medium rounded-md transition-colors ${i18n.language === lng ? 'bg-white/10 text-white' : 'text-white/40 hover:text-white/60'}`}
                >
                  {t(`settings.lang${lng.charAt(0).toUpperCase() + lng.slice(1)}`)}
                </button>
              ))}
            </div>
          </div>
        </div>
      </section>

      {/* Exit app */}
      <section className="pt-4">
        <button
          onClick={() => invoke('exit_app').catch(() => {})}
          className="w-full py-3 bg-red-500/10 hover:bg-red-500/20 border border-red-500/20 text-red-400 rounded-xl text-sm font-medium transition-colors"
        >
          {t('settings.exitApp')}
        </button>
      </section>
    </div>
  )
}
