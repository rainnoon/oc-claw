import { useState, useCallback, useEffect, useRef, useMemo } from 'react'
import { motion, AnimatePresence } from 'motion/react'
import {
  type PetData, type PetAction, type PomodoroState, type AffectionTier,
  FOODS, POMODORO_PRESETS,
  getAffectionTier, canWalk, canHeadpat,
  canClaimDailyGift, claimDailyGift,
  applyFeed, applyHeadpat,
} from '../lib/petStore'

import { Heart, Drumstick, Coins } from 'lucide-react'
import { useTranslation } from 'react-i18next'

type SubPanel = 'main' | 'actions' | 'shop' | 'pomodoro' | 'dev'

interface PetContextMenuProps {
  open: boolean
  petData: PetData
  currentAction: PetAction
  pomodoro: PomodoroState | null
  mascotSize: number
  side?: 'left' | 'right'
  onClose: () => void
  onUpdatePetData: (data: PetData) => void
  onSetAction: (action: PetAction) => void
  onStartPomodoro: (minutes: number) => void
  onStopPomodoro: () => void
  onOpenSettings: () => void
  onFoodRain?: (emoji: string) => void
  onClaimGift?: (amount: number) => void
  onQuit?: () => void
  onPlayAudio?: (action: PetAction) => void
}

export function PetContextMenu({
  open, petData, currentAction, pomodoro, mascotSize, side = 'left',
  onClose, onUpdatePetData, onSetAction,
  onStartPomodoro, onStopPomodoro, onOpenSettings, onFoodRain, onClaimGift, onQuit, onPlayAudio,
}: PetContextMenuProps) {
  const [subPanel, setSubPanel] = useState<SubPanel>('main')
  const menuRef = useRef<HTMLDivElement>(null)
  const { t } = useTranslation()

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
    onPlayAudio?.('eat')
  }, [petData, onUpdatePetData, onSetAction, onFoodRain, onPlayAudio])

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
      {/* Status bar */}
      <div
        onPointerDown={e => e.stopPropagation()}
        style={{
        position: 'absolute',
        ...(side === 'left' ? { right: 20 } : { left: 20 }),
        top: -70,
        pointerEvents: 'auto',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        gap: 12,
        padding: '4px 16px',
        background: 'rgba(10,10,10,0.85)',
        border: '1.5px solid #facc15',
        boxShadow: '0 0 12px rgba(250, 204, 21, 0.25), inset 0 0 8px rgba(250, 204, 21, 0.15)',
        transform: 'skewX(-15deg)',
        whiteSpace: 'nowrap',
        zIndex: 10,
      }}>
        <div style={{ transform: 'skewX(15deg)', display: 'flex', gap: 12, alignItems: 'center' }}>
          <StatBadge icon={<Heart size={13} strokeWidth={3} />} value={Math.round(petData.affection)} color="#facc15" />
          <StatBadge icon={<Drumstick size={13} strokeWidth={3} />} value={Math.round(petData.hunger)} color="#facc15" />
          <span style={{ fontSize: 13, color: '#facc15', fontWeight: 900, fontVariantNumeric: 'tabular-nums', position: 'relative', display: 'flex', alignItems: 'center', gap: 4, textShadow: '-1px -1px 0 #000, 1px -1px 0 #000, -1px 1px 0 #000, 1px 1px 0 #000, 0 2px 4px rgba(0,0,0,0.8)' }}>
            <Coins size={13} strokeWidth={3} /> {petData.coins}
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
                    fontWeight: 900,
                    color: '#facc15',
                    pointerEvents: 'none',
                    textShadow: '-1px -1px 0 #000, 1px -1px 0 #000, -1px 1px 0 #000, 1px 1px 0 #000, 0 0 8px rgba(250,204,21,0.8)',
                  }}
                >
                  +{coinBonus.amount}
                </motion.span>
              )}
            </AnimatePresence>
          </span>
        </div>
      </div>

      {/* Buttons beside mascot */}
      <style>{`
        .pet-menu-scroll::-webkit-scrollbar { display: none; }
        .pet-menu-scroll { -ms-overflow-style: none; scrollbar-width: none; }
      `}</style>
      <div className="pet-menu-scroll" onPointerDown={e => e.stopPropagation()} style={{
        position: 'absolute',
        ...(side === 'left' ? { right: mascotSize + 14 } : { left: mascotSize + 14 }),
        top: '50%',
        transform: 'translateY(-50%)',
        maxHeight: '90%',
        display: 'flex',
        flexDirection: 'column',
        pointerEvents: 'auto',
        overflowY: 'auto',
        overflowX: 'hidden',
        paddingRight: 24,
        paddingTop: 30,
        paddingBottom: 30,
        paddingLeft: 30,
      }}>
        {/* Inner container to ensure the line spans the full scroll height */}
        <div style={{
          display: 'flex',
          flexDirection: 'column',
          gap: 24,
          position: 'relative',
          minHeight: 'max-content',
        }}>
          {/* Vertical line with black shadow for contrast */}
          <div style={{
            position: 'absolute',
            ...(side === 'left' ? { right: -12 } : { left: -12 }),
            top: 0,
            bottom: 0,
            width: 1.5,
            background: 'linear-gradient(to bottom, transparent, rgba(250,204,21,0.8) 20%, rgba(250,204,21,0.8) 80%, transparent)',
            pointerEvents: 'none',
            zIndex: 1,
            boxShadow: '0 0 2px #000, 0 0 4px #000',
          }} />

          {subPanel === 'main' ? (
            <>
              <SideBtn side={side} label={giftAvailable ? t('pet.dailyGift') : t('pet.claimed')} onClick={handleClaimGift} disabled={!giftAvailable || !!pomodoro?.active} active={giftAvailable && !pomodoro?.active} />
              <SideBtn side={side} label={t('pet.actions')} onClick={() => setSubPanel('actions')} disabled={!!pomodoro?.active} />
              <SideBtn side={side} label={t('pet.shop')} onClick={() => setSubPanel('shop')} disabled={!!pomodoro?.active} />
              {pomodoro?.active ? (
                <SideBtn side={side} label={t('pet.stop')} onClick={onStopPomodoro} />
              ) : (
                <SideBtn side={side} label={t('pet.pomodoro')} onClick={() => setSubPanel('pomodoro')} />
              )}
              <SideBtn side={side} label={t('mini.settings')} onClick={onOpenSettings} />
              {onQuit && <SideBtn side={side} label={t('pet.quit')} onClick={onQuit} dim />}
              {import.meta.env.DEV && <SideBtn side={side} label="Dev" onClick={() => setSubPanel('dev')} dim />}
            </>
          ) : (
            <>
              <SideBtn side={side} label={t('common.back')} onClick={() => setSubPanel('main')} dim />
              {subPanel === 'actions' && (
                <>
                  <SideBtn side={side} label={t('pet.sleep')} onClick={() => handleAction('sleep')} active={currentAction === 'sleep'} />
                  <SideBtn side={side} label={t('pet.watch')} onClick={() => handleAction('watch')} active={currentAction === 'watch'} />
                  <SideBtn side={side} label={t('pet.music')} onClick={() => handleAction('music')} active={currentAction === 'music'} />
                  <SideBtn side={side} label={t('pet.walk')} onClick={() => handleAction('walk')} disabled={!canWalk(petData)} />
                  <SideBtn side={side} label={`${t('pet.pat')} ${petData.headpatToday}/5`} onClick={() => handleAction('headpat')} disabled={!canHeadpat(petData)} />
                </>
              )}
              {subPanel === 'shop' && FOODS.map(food => (
                <SideBtn
                  side={side}
                  key={food.id}
                  label={`${food.name} 🪙${food.price}`}
                  onClick={() => handleBuy(food.id)}
                  disabled={petData.coins < food.price}
                />
              ))}
              {subPanel === 'pomodoro' && (
                <>
                  {POMODORO_PRESETS.map(m => (
                    <SideBtn side={side} key={m} label={`${m} ${t('pet.min')}`} onClick={() => onStartPomodoro(m)} />
                  ))}
                  <CustomTimeInput side={side} onStart={onStartPomodoro} />
                </>
              )}
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
      </div>

    </motion.div>
  )
}

function StatBadge({ icon, value, color }: { icon: React.ReactNode; value: number; color: string }) {
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 4, color, textShadow: '-1px -1px 0 #000, 1px -1px 0 #000, -1px 1px 0 #000, 1px 1px 0 #000, 0 2px 4px rgba(0,0,0,0.8)' }}>
      <span style={{ display: 'flex', alignItems: 'center' }}>{icon}</span>
      <span style={{ fontSize: 13, fontWeight: 900, fontVariantNumeric: 'tabular-nums' }}>{value}</span>
    </div>
  )
}

function SideBtn({ label, onClick, disabled, active, dim, side = 'left' }: {
  label: string; onClick: () => void
  disabled?: boolean; active?: boolean; dim?: boolean; side?: 'left' | 'right'
}) {
  const isRight = side === 'right'
  return (
    <button
      onClick={disabled ? undefined : onClick}
      style={{
        display: 'flex',
        alignItems: 'center',
        justifyContent: isRight ? 'flex-start' : 'flex-end',
        color: '#facc15',
        fontWeight: 900,
        letterSpacing: '1.5px',
        fontSize: 13,
        textTransform: 'uppercase',
        cursor: disabled ? 'not-allowed' : 'pointer',
        opacity: disabled ? 0.3 : dim ? 0.6 : 1,
        whiteSpace: 'nowrap',
        background: 'transparent',
        border: 'none',
        padding: '4px 0',
        position: 'relative',
        transition: 'all 0.2s',
        textShadow: active 
          ? '-1px -1px 0 #000, 1px -1px 0 #000, -1px 1px 0 #000, 1px 1px 0 #000, -2px 0 0 #000, 2px 0 0 #000, 0 -2px 0 #000, 0 2px 0 #000, 0 0 8px rgba(250,204,21,0.8)'
          : '-1px -1px 0 #111, 1px -1px 0 #111, -1px 1px 0 #111, 1px 1px 0 #111, -2px 0 0 #111, 2px 0 0 #111, 0 -2px 0 #111, 0 2px 0 #111, 0 4px 8px rgba(0,0,0,0.8)',
      }}
      onMouseEnter={e => {
        if (!disabled) {
          e.currentTarget.style.textShadow = '-1px -1px 0 #000, 1px -1px 0 #000, -1px 1px 0 #000, 1px 1px 0 #000, -2px 0 0 #000, 2px 0 0 #000, 0 -2px 0 #000, 0 2px 0 #000, 0 0 10px rgba(250,204,21,1)';
          const diamond = e.currentTarget.querySelector('.diamond') as HTMLElement;
          if (diamond) {
            diamond.style.background = '#facc15';
            diamond.style.boxShadow = '0 0 0 1.5px #000, 0 0 8px #facc15';
          }
        }
      }}
      onMouseLeave={e => {
        e.currentTarget.style.textShadow = active 
          ? '-1px -1px 0 #000, 1px -1px 0 #000, -1px 1px 0 #000, 1px 1px 0 #000, -2px 0 0 #000, 2px 0 0 #000, 0 -2px 0 #000, 0 2px 0 #000, 0 0 8px rgba(250,204,21,0.8)'
          : '-1px -1px 0 #111, 1px -1px 0 #111, -1px 1px 0 #111, 1px 1px 0 #111, -2px 0 0 #111, 2px 0 0 #111, 0 -2px 0 #111, 0 2px 0 #111, 0 4px 8px rgba(0,0,0,0.8)';
        const diamond = e.currentTarget.querySelector('.diamond') as HTMLElement;
        if (diamond) {
          diamond.style.background = active ? '#facc15' : '#111';
          diamond.style.boxShadow = active ? '0 0 0 1.5px #000, 0 0 6px #facc15' : '0 0 0 1.5px #facc15';
        }
      }}
    >
      <span style={{ position: 'relative', zIndex: 1 }}>{label}</span>
      <div
        className="diamond"
        style={{
          width: 6,
          height: 6,
          border: active ? '1.5px solid #000' : '1.5px solid #facc15',
          transform: 'rotate(45deg)',
          position: 'absolute',
          ...(isRight ? { left: -15 } : { right: -15 }),
          background: active ? '#facc15' : '#111',
          boxShadow: active ? '0 0 0 1.5px #000, 0 0 6px #facc15' : '0 0 0 1.5px #facc15',
          transition: 'all 0.2s',
          zIndex: 2,
        }}
      />
    </button>
  )
}

function CustomTimeInput({ side, onStart }: { side?: 'left' | 'right'; onStart: (m: number) => void }) {
  const [val, setVal] = useState('')
  const { t } = useTranslation()
  const isRight = side === 'right'
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 6,
      justifyContent: isRight ? 'flex-start' : 'flex-end',
    }}>
      <input
        type="number"
        min={1}
        max={480}
        placeholder={t('pet.min')}
        value={val}
        onChange={e => setVal(e.target.value)}
        onKeyDown={e => {
          if (e.key === 'Enter') {
            const n = parseInt(val, 10)
            if (n > 0 && n <= 480) onStart(n)
          }
        }}
        style={{
          width: 52,
          padding: '3px 6px',
          fontSize: 12,
          fontWeight: 700,
          color: '#facc15',
          background: 'rgba(250,204,21,0.08)',
          border: '1.5px solid rgba(250,204,21,0.4)',
          borderRadius: 4,
          outline: 'none',
          textAlign: 'center',
          fontVariantNumeric: 'tabular-nums',
        }}
      />
      <button
        onClick={() => {
          const n = parseInt(val, 10)
          if (n > 0 && n <= 480) onStart(n)
        }}
        style={{
          fontSize: 11,
          fontWeight: 900,
          color: '#facc15',
          background: 'transparent',
          border: '1.5px solid rgba(250,204,21,0.4)',
          borderRadius: 4,
          padding: '3px 8px',
          cursor: 'pointer',
          letterSpacing: '1px',
          textTransform: 'uppercase',
          textShadow: '-1px -1px 0 #000, 1px -1px 0 #000, -1px 1px 0 #000, 1px 1px 0 #000',
        }}
      >
        {t('pet.go')}
      </button>
    </div>
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
  const { t } = useTranslation()
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
          {t('pet.stop')}
        </button>
      </div>
    </div>
  )
}
