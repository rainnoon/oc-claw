import { motion, AnimatePresence } from 'motion/react'
import type { AppMode } from '../lib/petStore'

interface OnboardingModalProps {
  open: boolean
  onSelect: (mode: AppMode) => void
}

export function OnboardingModal({ open, onSelect }: OnboardingModalProps) {
  return (
    <AnimatePresence>
      {open && (
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.3 }}
          style={{
            position: 'fixed',
            inset: 0,
            zIndex: 9999,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            background: 'transparent',
          }}
        >
          <motion.div
            initial={{ opacity: 0, scale: 0.9, y: 20 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.9, y: 20 }}
            transition={{ duration: 0.35, ease: [0.16, 1, 0.3, 1] }}
            style={{
              display: 'flex',
              gap: 20,
              padding: 32,
              borderRadius: 24,
              background: 'linear-gradient(135deg, #1a1a1a 0%, #111 100%)',
              border: '1px solid rgba(255,255,255,0.08)',
              boxShadow: '0 24px 80px rgba(0,0,0,0.8), 0 0 0 1px rgba(255,255,255,0.05)',
              maxWidth: 520,
              width: '90vw',
              flexDirection: 'column',
              alignItems: 'center',
            }}
          >
            <h2 style={{ color: '#fff', fontSize: 18, fontWeight: 600, margin: 0, letterSpacing: '-0.01em' }}>
              Choose Your Mode
            </h2>
            <p style={{ color: 'rgba(255,255,255,0.4)', fontSize: 13, margin: 0, textAlign: 'center', lineHeight: 1.5 }}>
              You can switch between modes at any time in Settings.
            </p>

            <div style={{ display: 'flex', gap: 16, width: '100%', marginTop: 8 }}>
              <ModeCard
                title="Coding Mode"
                icon="💻"
                description="Monitor your AI agents. Hover the mascot to see agent status, sessions, and activity."
                accent="#3b82f6"
                onClick={() => onSelect('coding')}
              />
              <ModeCard
                title="Pet Mode"
                icon="🐾"
                description="Raise your desktop pet! Feed, play, and build affection with rich animations."
                accent="#f59e0b"
                onClick={() => onSelect('pet')}
              />
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  )
}

function ModeCard({ title, icon, description, accent, onClick }: {
  title: string
  icon: string
  description: string
  accent: string
  onClick: () => void
}) {
  return (
    <motion.button
      whileHover={{ scale: 1.03, borderColor: accent }}
      whileTap={{ scale: 0.97 }}
      onClick={onClick}
      style={{
        flex: 1,
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        gap: 12,
        padding: '28px 20px',
        borderRadius: 16,
        background: 'rgba(255,255,255,0.03)',
        border: '1px solid rgba(255,255,255,0.08)',
        cursor: 'pointer',
        transition: 'border-color 0.2s, background 0.2s',
        color: '#fff',
        textAlign: 'center',
      }}
      onMouseEnter={(e) => {
        ;(e.currentTarget as HTMLElement).style.background = 'rgba(255,255,255,0.06)'
      }}
      onMouseLeave={(e) => {
        ;(e.currentTarget as HTMLElement).style.background = 'rgba(255,255,255,0.03)'
      }}
    >
      <span style={{ fontSize: 36 }}>{icon}</span>
      <span style={{ fontSize: 15, fontWeight: 600, letterSpacing: '-0.01em' }}>{title}</span>
      <span style={{ fontSize: 12, color: 'rgba(255,255,255,0.45)', lineHeight: 1.5 }}>{description}</span>
    </motion.button>
  )
}
