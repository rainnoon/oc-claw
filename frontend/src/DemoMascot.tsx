import { useCallback, useEffect, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import { LogicalPosition } from '@tauri-apps/api/dpi'
import { MiniPetMascot } from './components/MiniPetMascot'
import { loadCodexPetById, loadDefaultCodexPet, type CodexPet, type CodexPetState } from './lib/codexPet'

// Lightweight mascot-only window used by the dev "演示模式" toggle.
// Spawned by the `spawn_demo_mascot` Tauri command with `?demo=1&pet=<id>`
// in the URL. Each window picks up a single codex pet, listens to the
// same Claude/Codex/Cursor task events the main mini window does, and
// shows the corresponding running/idle/jumping animation. State naturally
// stays in sync because every demo window subscribes to the same events.
const MASCOT_BASE_SIZE = 43
const MASCOT_DISPLAY_MULTIPLIER = 2

export function DemoMascot() {
  const params = new URLSearchParams(window.location.hash.split('?')[1] ?? '')
  const petIdFromUrl = params.get('pet') ?? ''
  const [pet, setPet] = useState<CodexPet | null>(null)
  const [working, setWorking] = useState(false)
  const [waiting, setWaiting] = useState(false)
  const [walkDir, setWalkDir] = useState<-1 | 0 | 1>(0)
  const dragActiveRef = useRef(false)

  useEffect(() => {
    let cancelled = false
    ;(async () => {
      const found = (petIdFromUrl ? await loadCodexPetById(petIdFromUrl) : null) ?? (await loadDefaultCodexPet())
      if (!cancelled) setPet(found)
    })()
    return () => {
      cancelled = true
    }
  }, [petIdFromUrl])

  // Mirror the main mini window's resolved mascot state. The main
  // window owns the claude/codex/cursor session polling and emits
  // `mini-pet-state` on every change (and every 2s as a heartbeat),
  // so listening here keeps every demo window perfectly in sync with
  // the real mascot's working / waiting / idle without duplicating
  // any poll loops on our side.
  useEffect(() => {
    const unlisten = listen<{ state?: string }>('mini-pet-state', (ev) => {
      const s = ev.payload?.state
      if (s === 'waiting') {
        setWaiting(true)
        setWorking(false)
      } else if (s === 'working' || s === 'compacting') {
        setWaiting(false)
        setWorking(true)
      } else {
        setWaiting(false)
        setWorking(false)
      }
    })
    return () => {
      unlisten.then((fn) => fn())
    }
  }, [])

  // Direct drag using the webview's setPosition. `core:window:allow-*`
  // permissions in capabilities/default.json open this up for non-mini
  // windows. macOS' acceptsFirstMouse swizzle ensures the first click
  // delivers immediately even on a non-key floating window.
  const handlePointerDown = useCallback((e: React.PointerEvent) => {
    if (e.button !== 0 || e.ctrlKey) return
    e.preventDefault()
    dragActiveRef.current = true
    const win = getCurrentWebviewWindow()
    const startX = e.screenX
    const startY = e.screenY
    let lastX = e.screenX
    let lastY = e.screenY
    let dragging = false
    const pid = e.pointerId

    const onMove = async (ev: PointerEvent) => {
      if (ev.pointerId !== pid) return
      if (!dragging) {
        if (Math.abs(ev.screenX - startX) + Math.abs(ev.screenY - startY) >= 3) {
          dragging = true
        } else {
          return
        }
      }
      const dx = ev.screenX - lastX
      const dy = ev.screenY - lastY
      lastX = ev.screenX
      lastY = ev.screenY
      if (dx !== 0 || dy !== 0) {
        try {
          const scale = await win.scaleFactor()
          const pos = await win.outerPosition()
          await win.setPosition(
            new LogicalPosition(pos.x / scale + dx, pos.y / scale + dy),
          )
        } catch {
          /* permissions or focus loss; just drop the frame */
        }
        if (dx !== 0) setWalkDir(dx > 0 ? 1 : -1)
      }
    }

    const cleanup = () => {
      dragActiveRef.current = false
      setWalkDir(0)
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
    }

    window.addEventListener('pointermove', onMove)
    window.addEventListener('pointerup', onUp, { once: true })
    window.addEventListener('pointercancel', onCancel, { once: true })
  }, [])

  const baseState: CodexPetState = walkDir === 1
    ? 'run-right'
    : walkDir === -1
      ? 'run-left'
      : waiting
        ? 'waiting'
        : working
          ? 'running'
          : 'idle'

  if (!pet) return null

  const size = MASCOT_BASE_SIZE * MASCOT_DISPLAY_MULTIPLIER
  return (
    <div
      onPointerDown={handlePointerDown}
      style={{
        width: '100%',
        height: '100%',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        background: 'transparent',
        cursor: 'grab',
      }}
    >
      <MiniPetMascot
        pet={pet}
        baseState={baseState}
        size={size}
        enableHoverJump
      />
    </div>
  )
}
