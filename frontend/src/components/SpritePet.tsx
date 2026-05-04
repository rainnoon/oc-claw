import { useEffect, useRef, useState } from 'react'
import {
  ATLAS,
  ANIMATION_ROWS,
  fpsFor,
  type CodexPet,
  type CodexPetState,
} from '../lib/codexPet'

interface SpritePetProps {
  pet: CodexPet
  state: CodexPetState
  // Visual width in CSS pixels. Height is derived from the 192:208 cell ratio.
  size: number
  // Fired once jumping reaches its last frame so the parent can flip back
  // to its previous resting state. No-op for looping states.
  onOneShotEnd?: () => void
  // When true, treat one-shot states (jumping) as looping. Used by hover
  // interactions where the parent wants the animation to keep playing as
  // long as the cursor is over the mascot.
  loop?: boolean
  className?: string
  style?: React.CSSProperties
}

// Render a single 192x208 cell from a hatch-pet style 8x9 atlas, advancing
// frames at SPRITE_FPS via requestAnimationFrame. Looping states cycle
// indefinitely; one-shot states (currently `jumping`) hold the last frame
// and notify the parent via onOneShotEnd.
const ONE_SHOT_STATES: ReadonlySet<CodexPetState> = new Set(['jumping'])

export function SpritePet({ pet, state, size, onOneShotEnd, loop, className, style }: SpritePetProps) {
  const [frameIndex, setFrameIndex] = useState(0)
  const stateRef = useRef(state)
  const loopRef = useRef(loop ?? false)
  const onOneShotEndRef = useRef(onOneShotEnd)
  const oneShotFiredRef = useRef(false)

  useEffect(() => {
    stateRef.current = state
    oneShotFiredRef.current = false
    setFrameIndex(0)
  }, [state])

  useEffect(() => {
    loopRef.current = loop ?? false
  }, [loop])

  useEffect(() => {
    onOneShotEndRef.current = onOneShotEnd
  }, [onOneShotEnd])

  useEffect(() => {
    let raf = 0
    let acc = 0
    let last = performance.now()
    let cancelled = false

    const tick = (now: number) => {
      if (cancelled) return
      const dt = now - last
      last = now
      acc += dt
      // Re-read the per-frame interval each iteration so per-state fps
      // overrides (e.g. a slower idle loop) take effect immediately when
      // the state changes mid-tick.
      const frameMs = 1000 / fpsFor(stateRef.current)
      while (acc >= frameMs) {
        acc -= frameMs
        const cur = stateRef.current
        const row = ANIMATION_ROWS[cur]
        if (!row) continue
        setFrameIndex((prev) => {
          const next = prev + 1
          if (ONE_SHOT_STATES.has(cur) && !loopRef.current) {
            if (next >= row.frames) {
              if (!oneShotFiredRef.current) {
                oneShotFiredRef.current = true
                const cb = onOneShotEndRef.current
                if (cb) queueMicrotask(cb)
              }
              return row.frames - 1
            }
            return next
          }
          return next % row.frames
        })
      }
      raf = requestAnimationFrame(tick)
    }
    raf = requestAnimationFrame(tick)
    return () => {
      cancelled = true
      cancelAnimationFrame(raf)
    }
  }, [])

  const row = ANIMATION_ROWS[state]
  const frame = Math.min(frameIndex, row.frames - 1)
  const aspect = ATLAS.cellH / ATLAS.cellW
  const renderW = size
  const renderH = size * aspect
  const scale = renderW / ATLAS.cellW
  const totalW = ATLAS.cellW * ATLAS.cols * scale
  const totalH = ATLAS.cellH * ATLAS.rows * scale
  const bgX = -frame * ATLAS.cellW * scale
  const bgY = -row.row * ATLAS.cellH * scale

  return (
    <div
      className={className}
      style={{
        width: renderW,
        height: renderH,
        backgroundImage: `url("${pet.spritesheetUrl}")`,
        backgroundRepeat: 'no-repeat',
        backgroundSize: `${totalW}px ${totalH}px`,
        backgroundPosition: `${bgX}px ${bgY}px`,
        imageRendering: 'pixelated',
        ...style,
      }}
    />
  )
}
