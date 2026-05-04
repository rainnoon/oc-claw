import { useCallback, useEffect, useRef, useState } from 'react'
import { SpritePet } from './SpritePet'
import { ANIMATION_ROWS, fpsFor } from '../lib/codexPet'
import type { CodexPet, CodexPetState } from '../lib/codexPet'

interface MiniPetMascotProps {
  pet: CodexPet
  // Resting state computed by the parent: idle / running (working+compacting) /
  // waiting / run-right / run-left. `jumping` is owned by this wrapper via
  // hover and should not be passed in.
  baseState: CodexPetState
  size: number
  // When true, the wrapper plays a one-shot jump while hovered, then waits
  // before triggering the next jump.
  enableHoverJump?: boolean
  // External hover signal driven by a native cursor poll (used on macOS).
  // When `useExternalHover` is true this is the single source of truth and
  // webview-level mouseenter/leave is ignored, because macOS does not
  // deliver mouseenter to non-key floating windows reliably and would also
  // keep firing during a drag (sprite would stay frozen on `jumping`).
  externalHover?: boolean
  useExternalHover?: boolean
  className?: string
  style?: React.CSSProperties
}

// How long the sprite holds the jump's final frame before replaying the
// next one-shot. While hovered, the cycle is: play jump → freeze on last
// frame for JUMP_REST_MS → replay jump → ...
const JUMP_REST_MS = 400

export function MiniPetMascot({
  pet,
  baseState,
  size,
  enableHoverJump = false,
  externalHover = false,
  useExternalHover = false,
  className,
  style,
}: MiniPetMascotProps) {
  const [internalHover, setInternalHover] = useState(false)
  const [showJump, setShowJump] = useState(false)
  // Bumping this remounts SpritePet (via `key`) and replays the jump
  // animation from frame 0 without leaving the `jumping` state — that way
  // the rest period stays on the last jump frame instead of falling back
  // to baseState (idle/run/etc.) between cycles.
  const [jumpKey, setJumpKey] = useState(0)
  const hoveringRef = useRef(false)
  const restTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  const onEnter = useCallback(() => {
    if (enableHoverJump && !useExternalHover) setInternalHover(true)
  }, [enableHoverJump, useExternalHover])

  const onLeave = useCallback(() => {
    if (enableHoverJump && !useExternalHover) setInternalHover(false)
  }, [enableHoverJump, useExternalHover])

  const hovering =
    enableHoverJump && (useExternalHover ? externalHover : internalHover)
  hoveringRef.current = hovering

  useEffect(() => {
    if (!hovering) {
      if (restTimerRef.current) {
        clearTimeout(restTimerRef.current)
        restTimerRef.current = null
      }
      setShowJump(false)
      return
    }
    setShowJump(true)
    return () => {
      if (restTimerRef.current) {
        clearTimeout(restTimerRef.current)
        restTimerRef.current = null
      }
    }
  }, [hovering])

  const handleJumpEnd = useCallback(() => {
    // SpritePet's one-shot logic naturally holds the last frame here, so
    // we do NOT flip back to baseState. After the rest delay we just
    // bump jumpKey to remount SpritePet and let it play from frame 0.
    if (restTimerRef.current) clearTimeout(restTimerRef.current)
    restTimerRef.current = setTimeout(() => {
      restTimerRef.current = null
      if (hoveringRef.current) setJumpKey((k) => k + 1)
    }, JUMP_REST_MS)
  }, [])

  // Safety net: if SpritePet's onOneShotEnd somehow doesn't fire (e.g.
  // tab throttling), schedule the rest cycle by the animation's nominal
  // duration plus a small buffer.
  useEffect(() => {
    if (!showJump) return
    const row = ANIMATION_ROWS['jumping']
    const fps = fpsFor('jumping')
    const expected = (row.frames / Math.max(fps, 1)) * 1000
    const fallback = setTimeout(() => {
      handleJumpEnd()
    }, expected + 200)
    return () => clearTimeout(fallback)
  }, [showJump, jumpKey, handleJumpEnd])

  const renderState: CodexPetState = showJump ? 'jumping' : baseState

  return (
    <div
      className={className}
      onMouseEnter={enableHoverJump && !useExternalHover ? onEnter : undefined}
      onMouseLeave={enableHoverJump && !useExternalHover ? onLeave : undefined}
      style={{ display: 'inline-block', lineHeight: 0, ...style }}
    >
      <SpritePet
        key={showJump ? `jump-${jumpKey}` : `base-${renderState}`}
        pet={pet}
        state={renderState}
        size={size}
        onOneShotEnd={handleJumpEnd}
      />
    </div>
  )
}
