import { useCallback, useEffect, useRef, useState } from 'react'
import { emit, listen } from '@tauri-apps/api/event'
import { load } from '@tauri-apps/plugin-store'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import { LogicalPosition, LogicalSize } from '@tauri-apps/api/dpi'
import { MiniPetMascot } from './components/MiniPetMascot'
import { loadCodexPetById, loadDefaultCodexPet, type CodexPet, type CodexPetState } from './lib/codexPet'

const isWindowsPlatform =
  typeof navigator !== 'undefined' && navigator.userAgent.includes('Windows')

// Matches Mini.tsx: the collapsed small mascot's visual size is
// round(MASCOT_BASE_SIZE * mascotScale) * largeMascotScale, driven by the
// "Mascot Size" slider (large_mascot_scale). Mirror that here so extra mascots
// scale together with the primary one.
function computeMascotSize(mascotScale: number, largeMascotScale: number): number {
  return Math.round(MASCOT_BASE_SIZE * mascotScale) * largeMascotScale
}

// Lightweight mascot-only window used by the dev "演示模式" toggle.
// Spawned by the `spawn_demo_mascot` Tauri command with `?demo=1&pet=<id>`
// in the URL. Each window picks up a single codex pet, listens to the
// same Claude/Codex/Cursor task events the main mini window does, and
// shows the corresponding running/idle/jumping animation. State naturally
// stays in sync because every demo window subscribes to the same events.
const MASCOT_BASE_SIZE = 43
// Default before the real scale is loaded from settings, matching Mini's
// defaults (mascot_scale 1 × large_mascot_scale 5).
const DEFAULT_MASCOT_SIZE = computeMascotSize(1, 5)

// `functional` mascots (coding-mode multi-mascot feature) emit
// `extra-mascot-activate` to the main mini window on a click (no drag) so the
// main panel expands — making each extra mascot equivalent to the primary one.
// Demo mascots leave `functional` false and stay decorative.
export function DemoMascot({ functional = false }: { functional?: boolean }) {
  const params = new URLSearchParams(window.location.hash.split('?')[1] ?? '')
  const petIdFromUrl = params.get('pet') ?? ''
  const [pet, setPet] = useState<CodexPet | null>(null)
  const [working, setWorking] = useState(false)
  const [waiting, setWaiting] = useState(false)
  const [walkDir, setWalkDir] = useState<-1 | 0 | 1>(0)
  const [dragging, setDragging] = useState(false)
  const [size, setSize] = useState(DEFAULT_MASCOT_SIZE)
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

  // Match the primary mascot's size. Read the persisted scale on mount and keep
  // in sync with live slider changes broadcast by the main window. The owning
  // webview window is resized to fit so the mascot never clips and the
  // transparent drag area stays tight to the sprite.
  useEffect(() => {
    let cancelled = false
    const applySize = (next: number) => {
      if (cancelled || !Number.isFinite(next) || next <= 0) return
      setSize(next)
      const win = getCurrentWebviewWindow()
      const boxW = Math.ceil(next)
      const boxH = Math.ceil(next * (208 / 192))
      win.setSize(new LogicalSize(boxW, boxH)).catch(() => {})
    }
    ;(async () => {
      try {
        const store = await load('settings.json', { defaults: {}, autoSave: false })
        const ms = (await store.get('mascot_scale')) as number | null
        const lms = (await store.get('large_mascot_scale')) as number | null
        applySize(computeMascotSize(
          typeof ms === 'number' && ms > 0 ? ms : 1,
          typeof lms === 'number' && lms > 0 ? lms : 5,
        ))
      } catch {
        /* fall back to default size */
      }
    })()
    const unlisten = listen<{ size?: number }>('mascot-visual-size', (ev) => {
      const s = ev.payload?.size
      if (typeof s === 'number') applySize(s)
    })
    return () => {
      cancelled = true
      unlisten.then((fn) => fn())
    }
  }, [])

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
          // Force the hover/jump animation off so walkDir → run-left/run-right
          // is visible while dragging (otherwise the pointer stays over the
          // mascot and the jump cycle hides the walk frames).
          setDragging(true)
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
      setDragging(false)
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
      const wasDragging = dragging
      cleanup()
      // A tap (no drag) on a functional extra mascot mirrors the primary
      // mascot's click action: expand the main session panel. On macOS the
      // primary mascot opens the panel via notch hover (a tap is a no-op), so
      // keep extra mascots consistent and skip the click-to-expand there.
      if (functional && !wasDragging && isWindowsPlatform) {
        emit('extra-mascot-activate').catch(() => {})
      }
    }

    window.addEventListener('pointermove', onMove)
    window.addEventListener('pointerup', onUp, { once: true })
    window.addEventListener('pointercancel', onCancel, { once: true })
  }, [functional])

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
        suppressHover={dragging}
      />
    </div>
  )
}
