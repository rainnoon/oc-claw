import { useState, useCallback, useEffect, useRef, useMemo } from 'react'
import { motion, AnimatePresence } from 'motion/react'
import {
  type PetData, type PetAction, type PomodoroState, type AffectionTier,
  FOODS, POMODORO_PRESETS,
  getAffectionTier, canWalk, canHeadpat,
  canClaimDailyGift, claimDailyGift,
  applyFeed, applyHeadpat,
} from '../lib/petStore'

type SubPanel = 'main' | 'actions' | 'shop' | 'pomodoro' | 'dev'

interface PetContextMenuProps {
  open: boolean
  petData: PetData
  currentAction: PetAction
  pomodoro: PomodoroState | null
  mascotSize: number
  onClose: () => void
  onUpdatePetData: (data: PetData) => void
  onSetAction: (action: PetAction) => void
  onStartPomodoro: (minutes: number) => void
  onStopPomodoro: () => void
  onOpenSettings: () => void
  onFoodRain?: (emoji: string) => void
  onClaimGift?: (amount: number) => void
}

export function PetContextMenu({
  open, petData, currentAction, pomodoro, mascotSize,
  onClose, onUpdatePetData, onSetAction,
  onStartPomodoro, onStopPomodoro, onOpenSettings, onFoodRain, onClaimGift,
}: PetContextMenuProps) {
  const [subPanel, setSubPanel] = useState<SubPanel>('main')
  const menuRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) {
      setSubPanel('main')
      return
    }
    const onClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        onClose()
      }
    }
    const timer = setTimeout(() => window.addEventListener('mousedown', onClick), 200)
    return () => {
      clearTimeout(timer)
      window.removeEventListener('mousedown', onClick)
    }
  }, [open, onClose])

  const tier: AffectionTier = getAffectionTier(petData.affection)

  const handleAction = useCallback((action: PetAction) => {
    if (action === 'headpat') {
      onUpdatePetData(applyHeadpat(petData))
    }
    onSetAction(action)
  }, [petData, onUpdatePetData, onSetAction])

  const giftAvailable = canClaimDailyGift(petData)

  const [coinBonus, setCoinBonus] = useState<{ amount: number; id: number } | null>(null)
  const coinBonusIdRef = useRef(0)

  const handleClaimGift = useCallback(() => {
    if (!canClaimDailyGift(petData)) return
    const { data: updated, amount } = claimDailyGift(petData)
    onUpdatePetData(updated)
    coinBonusIdRef.current += 1
    setCoinBonus({ amount, id: coinBonusIdRef.current })
    setTimeout(() => setCoinBonus(null), 2000)
    onClaimGift?.(amount)
  }, [petData, onUpdatePetData, onClaimGift])

  const handleBuy = useCallback((foodId: string) => {
    const food = FOODS.find(f => f.id === foodId)
    if (!food) return
    const updated = applyFeed(petData, food)
    if (updated === petData) return
    onUpdatePetData(updated)
    onFoodRain?.(food.icon)
    onSetAction('eat')
  }, [petData, onUpdatePetData, onSetAction, onFoodRain])

  if (!open) return null

  return (
    <motion.div
      ref={menuRef}
      initial={{ opacity: 0, scale: 0.95 }}
      animate={{ opacity: 1, scale: 1 }}
      exit={{ opacity: 0, scale: 0.95 }}
      transition={{ duration: 0.1 }}
      style={{
        position: 'absolute',
        inset: 0,
        zIndex: 9990,
        pointerEvents: 'none',
      }}
    >
      {/* Status bar above mascot */}
      <div style={{
        position: 'absolute',
        left: '50%',
        top: -88,
        transform: 'translateX(-50%)',
        pointerEvents: 'auto',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        gap: 12,
        padding: '8px 16px',
        borderRadius: 10,
        background: 'rgba(8,8,8,0.88)',
        border: '1px solid rgba(255,255,255,0.1)',
        backdropFilter: 'blur(12px)',
        whiteSpace: 'nowrap',
      }}>
        <StatBadge icon="❤️" value={Math.round(petData.affection)} color={tierColor(tier)} />
        <StatBadge icon="🍗" value={Math.round(petData.hunger)} color={petData.hunger < 30 ? '#ef4444' : '#22c55e'} />
        <span style={{ fontSize: 13, color: '#fbbf24', fontWeight: 700, fontVariantNumeric: 'tabular-nums', position: 'relative' }}>
          🪙 {petData.coins}
          <AnimatePresence>
            {coinBonus && (
              <motion.span
                key={coinBonus.id}
                initial={{ opacity: 1, y: 0 }}
                animate={{ opacity: 1, y: -20 }}
                exit={{ opacity: 0, y: -28 }}
                transition={{ duration: 0.8, ease: 'easeOut' }}
                style={{
                  position: 'absolute',
                  left: '50%',
                  top: 0,
                  transform: 'translateX(-50%)',
                  fontSize: 12,
                  fontWeight: 700,
                  color: '#4ade80',
                  pointerEvents: 'none',
                }}
              >
                +{coinBonus.amount}
              </motion.span>
            )}
          </AnimatePresence>
        </span>
      </div>

      {/* Buttons to the left of mascot */}
      <style>{`
        .pet-menu-scroll::-webkit-scrollbar { width: 2px; }
        .pet-menu-scroll::-webkit-scrollbar-track { background: transparent; }
        .pet-menu-scroll::-webkit-scrollbar-thumb { background: rgba(255,255,255,0.25); border-radius: 1px; }
      `}</style>
      <div className="pet-menu-scroll" style={{
        position: 'absolute',
        right: mascotSize + 14,
        top: 10,
        bottom: 10,
        display: 'flex',
        flexDirection: 'column',
        gap: 10,
        pointerEvents: 'auto',
        overflowY: 'auto',
        overflowX: 'hidden',
        paddingRight: 6,
      }}>
        {subPanel === 'main' ? (
          <>
            <SideBtn
              icon="🎁"
              label={giftAvailable ? 'Daily Gift' : 'Claimed'}
              onClick={handleClaimGift}
              disabled={!giftAvailable}
              active={giftAvailable}
            />
            <SideBtn icon="🎬" label="Actions" onClick={() => setSubPanel('actions')} />
            <SideBtn icon="🛒" label="Shop" onClick={() => setSubPanel('shop')} />
            {pomodoro?.active ? (
              <SideBtn icon="⏹️" label="Stop" onClick={onStopPomodoro} />
            ) : (
              <SideBtn icon="🍅" label="Pomodoro" onClick={() => setSubPanel('pomodoro')} />
            )}
            <SideBtn icon="⚙️" label="Settings" onClick={onOpenSettings} />
            {import.meta.env.DEV && <SideBtn icon="🔧" label="Dev" onClick={() => setSubPanel('dev')} dim />}
          </>
        ) : (
          <>
            <SideBtn icon="←" label="Back" onClick={() => setSubPanel('main')} dim />
            {subPanel === 'actions' && (
              <>
                <SideBtn icon="😴" label="Sleep" onClick={() => handleAction('sleep')} active={currentAction === 'sleep'} />
                <SideBtn icon="📺" label="Watch" onClick={() => handleAction('watch')} active={currentAction === 'watch'} />
                <SideBtn icon="🎵" label="Music" onClick={() => handleAction('music')} active={currentAction === 'music'} />
                <SideBtn icon="🚶" label="Walk" onClick={() => handleAction('walk')} disabled={!canWalk(petData)} />
                <SideBtn icon="😊" label={`Pat ${petData.headpatToday}/5`} onClick={() => handleAction('headpat')} disabled={!canHeadpat(petData)} />
              </>
            )}
            {subPanel === 'shop' && FOODS.map(food => (
              <SideBtn
                key={food.id}
                icon={food.icon}
                label={`${food.name} 🪙${food.price}`}
                onClick={() => handleBuy(food.id)}
                disabled={petData.coins < food.price}
              />
            ))}
            {subPanel === 'pomodoro' && POMODORO_PRESETS.map(m => (
              <SideBtn key={m} icon="🍅" label={`${m} min`} onClick={() => onStartPomodoro(m)} />
            ))}
            {subPanel === 'dev' && (
              <div style={{
                display: 'flex', flexDirection: 'column', gap: 8,
                background: 'rgba(8,8,8,0.88)', border: '1px solid rgba(255,255,255,0.1)',
                borderRadius: 10, padding: '10px 12px', backdropFilter: 'blur(12px)',
              }}>
                <DevSlider label="❤️ Affection" value={Math.round(petData.affection)} min={0} max={100}
                  onChange={v => onUpdatePetData({ ...petData, affection: v })} />
                <DevSlider label="🍗 Hunger" value={Math.round(petData.hunger)} min={0} max={100}
                  onChange={v => onUpdatePetData({ ...petData, hunger: v })} />
                <DevSlider label="🪙 Coins" value={petData.coins} min={0} max={9999}
                  onChange={v => onUpdatePetData({ ...petData, coins: v })} />
              </div>
            )}
          </>
        )}
      </div>

    </motion.div>
  )
}

function StatBadge({ icon, value, color }: { icon: string; value: number; color: string }) {
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
      <span style={{ fontSize: 14 }}>{icon}</span>
      <span style={{ fontSize: 14, color, fontWeight: 700, fontVariantNumeric: 'tabular-nums' }}>{value}</span>
    </div>
  )
}

function SideBtn({ icon, label, onClick, disabled, active, dim }: {
  icon: string; label: string; onClick: () => void
  disabled?: boolean; active?: boolean; dim?: boolean
}) {
  return (
    <button
      onClick={disabled ? undefined : onClick}
      style={{
        padding: '8px 12px',
        borderRadius: 10,
        border: active ? '1px solid rgba(245,158,11,0.4)' : '1px solid rgba(255,255,255,0.1)',
        background: active ? 'rgba(245,158,11,0.15)' : 'rgba(8,8,8,0.88)',
        backdropFilter: 'blur(12px)',
        color: '#fff',
        display: 'flex',
        alignItems: 'center',
        gap: 8,
        fontSize: 13,
        fontWeight: 600,
        cursor: disabled ? 'not-allowed' : 'pointer',
        opacity: disabled ? 0.3 : dim ? 0.5 : 1,
        whiteSpace: 'nowrap',
      }}
      onMouseEnter={e => { if (!disabled) e.currentTarget.style.background = active ? 'rgba(245,158,11,0.25)' : 'rgba(20,20,20,0.92)' }}
      onMouseLeave={e => { e.currentTarget.style.background = active ? 'rgba(245,158,11,0.15)' : 'rgba(8,8,8,0.88)' }}
    >
      <span style={{ fontSize: 15 }}>{icon}</span>
      <span>{label}</span>
    </button>
  )
}

function DevSlider({ label, value, min, max, onChange }: {
  label: string; value: number; min: number; max: number; onChange: (v: number) => void
}) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: 12, color: '#ccc' }}>
        <span>{label}</span>
        <span style={{ fontVariantNumeric: 'tabular-nums', fontWeight: 700, color: '#fff' }}>{value}</span>
      </div>
      <input
        type="range" min={min} max={max} value={value}
        onChange={e => onChange(Number(e.target.value))}
        style={{ width: '100%', accentColor: '#f59e0b', cursor: 'pointer' }}
      />
    </div>
  )
}

function tierColor(tier: AffectionTier): string {
  switch (tier) {
    case 'shy': return '#f472b6'
    case 'happy': return '#22c55e'
    case 'cold': return '#60a5fa'
    case 'angry': return '#ef4444'
  }
}

// ─── Pomodoro Timer Overlay ───

export function PomodoroOverlay({ pomodoro, mascotSize, onStop }: {
  pomodoro: PomodoroState | null
  mascotSize: number
  onStop: () => void
}) {
  const [remaining, setRemaining] = useState(pomodoro?.remaining ?? 0)

  useEffect(() => {
    if (!pomodoro?.active) return
    const update = () => {
      const elapsed = (Date.now() - pomodoro.startedAt) / 1000
      const left = Math.max(0, pomodoro.duration - elapsed)
      setRemaining(Math.ceil(left))
    }
    update()
    const interval = setInterval(update, 1000)
    return () => clearInterval(interval)
  }, [pomodoro])

  if (!pomodoro?.active) return null

  const mins = Math.floor(remaining / 60)
  const secs = remaining % 60
  const pct = ((pomodoro.duration - remaining) / pomodoro.duration) * 100

  return (
    <div style={{
      position: 'absolute', bottom: 0, left: '50%',
      transform: 'translateX(-50%)',
      pointerEvents: 'auto',
    }}>
      <div style={{
        background: 'rgba(0,0,0,0.8)', borderRadius: 10,
        padding: '6px 14px', display: 'flex', flexDirection: 'column',
        alignItems: 'center', gap: 4,
        border: '1px solid rgba(255,255,255,0.1)', minWidth: 70,
      }}>
        <span style={{
          fontSize: 16, fontWeight: 700, color: '#fff',
          fontVariantNumeric: 'tabular-nums',
        }}>
          {String(mins).padStart(2, '0')}:{String(secs).padStart(2, '0')}
        </span>
        <div style={{
          width: '100%', height: 3, borderRadius: 2,
          background: 'rgba(255,255,255,0.1)', overflow: 'hidden',
        }}>
          <div style={{
            width: `${pct}%`, height: '100%', borderRadius: 2,
            background: '#f59e0b', transition: 'width 1s linear',
          }} />
        </div>
        <button
          onClick={onStop}
          style={{
            fontSize: 9, color: 'rgba(255,255,255,0.35)',
            background: 'none', border: 'none', cursor: 'pointer', padding: '2px 4px',
          }}
          onMouseEnter={e => { e.currentTarget.style.color = '#ef4444' }}
          onMouseLeave={e => { e.currentTarget.style.color = 'rgba(255,255,255,0.35)' }}
        >
          Stop
        </button>
      </div>
    </div>
  )
}
