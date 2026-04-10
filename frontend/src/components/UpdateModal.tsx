import { Check, X } from 'lucide-react'
import { useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import appLogo from '../../src-tauri/icons/128x128.png'

export type UpdateModalPhase = 'available' | 'downloading' | 'ready_to_restart'

export type UpdateModalInfo = {
  current: string
  latest: string
  hasUpdate: boolean
  url: string
  notes: string
}

export type UpdateModalProps = {
  open: boolean
  phase: UpdateModalPhase
  info: UpdateModalInfo | null
  progress: number | null
  progressStage: string
  onLater: () => void
  onSkipVersion: () => void
  onUpdateNow: () => void
  onRestartNow: () => void
}

export function UpdateModal({
  open,
  phase,
  info,
  progress,
  progressStage,
  onLater,
  onSkipVersion,
  onUpdateNow,
  onRestartNow,
}: UpdateModalProps) {
  const { t } = useTranslation()
  const noteLines = useMemo(() => {
    const source = info?.notes || ''
    return source
      .split('\n')
      .map((line) => line.trim())
      .filter(Boolean)
  }, [info?.notes])

  if (!open) return null

  const progressValue = typeof progress === 'number' ? Math.max(0, Math.min(100, progress)) : null
  const progressText = progressValue === null ? '' : `${progressValue}%`

  return (
    <div className="fixed inset-0 z-[80] flex items-center justify-center p-6">
      <div className="w-[420px] max-w-full rounded-2xl border border-white/10 bg-[#1a1a20] shadow-[0_16px_48px_rgba(0,0,0,0.6)] overflow-hidden">
        <div className="px-6 pt-5 pb-5">
          <div className="flex items-start justify-between">
            <div className="flex items-center gap-3">
              <div className="relative w-11 h-11 rounded-xl overflow-hidden border border-white/10 shadow-[0_6px_20px_rgba(0,0,0,0.35)] shrink-0">
                <img src={appLogo} alt="OC-CLAW logo" className="w-full h-full object-cover" draggable={false} />
                {phase === 'ready_to_restart' && (
                  <span className="absolute -right-1 -bottom-1 w-5 h-5 rounded-full bg-emerald-400 text-black flex items-center justify-center border border-black/25">
                    <Check className="w-3.5 h-3.5" strokeWidth={3} />
                  </span>
                )}
              </div>
              <div>
                <h3 className="text-lg font-bold text-white leading-tight">{t('updateModal.title')}</h3>
                <p className="text-sm font-bold text-emerald-400 mt-0.5">v{info?.latest || ''}</p>
              </div>
            </div>
            {phase === 'available' && (
              <button onClick={onLater} className="text-slate-500 hover:text-white transition-colors mt-1">
                <X className="w-4.5 h-4.5" />
              </button>
            )}
          </div>

          {phase === 'available' && (
            <>
              <p className="mt-4 text-[13px] text-white/60">{t('updateModal.availableSubtitle')}</p>
              <p className="mt-0.5 text-[13px] font-medium text-white/80">{t('updateModal.whatsNew')}</p>
              <div className="mt-2.5 rounded-xl border border-white/8 bg-black/25 px-4 py-3 max-h-[160px] overflow-auto">
                {noteLines.length > 0 ? (
                  <div className="space-y-2">
                    {noteLines.map((line, index) => (
                      <div key={`${index}-${line}`} className="text-[13px] leading-snug text-white/90 flex items-start gap-2">
                        <span className="text-yellow-300 mt-px">•</span>
                        <span>{line}</span>
                      </div>
                    ))}
                  </div>
                ) : (
                  <div className="text-[13px] text-white/35">{t('updateModal.noNotes')}</div>
                )}
              </div>
              <div className="mt-4 flex gap-2">
                <button
                  onClick={onLater}
                  className="flex-1 h-9 rounded-lg bg-white/5 hover:bg-white/10 text-white/80 text-[13px] font-semibold transition-colors"
                >
                  {t('updateModal.later')}
                </button>
                <button
                  onClick={onSkipVersion}
                  className="flex-1 h-9 rounded-lg bg-white/5 hover:bg-white/10 text-white/80 text-[13px] font-semibold transition-colors"
                >
                  {t('updateModal.skipVersion')}
                </button>
                <button
                  onClick={onUpdateNow}
                  className="flex-1 h-9 rounded-lg bg-yellow-400 hover:bg-yellow-300 text-black text-[13px] font-bold transition-colors"
                >
                  {t('updateModal.updateNow')}
                </button>
              </div>
            </>
          )}

          {phase === 'downloading' && (
            <>
              <div className="mt-5 flex items-center justify-between">
                <span className="text-sm font-bold text-white">{t(`updateModal.progress.${progressStage || 'downloading'}`)}</span>
                <span className="text-sm font-bold text-yellow-300 tabular-nums">{progressText}</span>
              </div>
              <div className="mt-3 h-2.5 rounded-full bg-black/50 border border-white/5 overflow-hidden">
                <div
                  className="h-full rounded-full bg-yellow-400 transition-all duration-200"
                  style={{ width: `${Math.max(progressValue ?? 0, 2)}%` }}
                />
              </div>
              <p className="mt-4 text-center text-xs text-slate-500">{t('updateModal.pleaseDontClose')}</p>
            </>
          )}

          {phase === 'ready_to_restart' && (
            <>
              <div className="mt-6 flex flex-col items-center gap-2">
                <div className="w-10 h-10 rounded-full bg-emerald-400/20 flex items-center justify-center">
                  <Check className="w-5 h-5 text-emerald-400" strokeWidth={2.5} />
                </div>
                <p className="text-base font-bold text-white">{t('updateModal.completeTitle')}</p>
                <p className="text-xs text-slate-400">{t('updateModal.completeDesc')}</p>
              </div>
              <button
                onClick={onRestartNow}
                className="mt-5 w-full h-9 rounded-lg bg-emerald-400 hover:bg-emerald-300 text-black text-[13px] font-bold transition-colors"
              >
                {t('updateModal.restartNow')}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  )
}
