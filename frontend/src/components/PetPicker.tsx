import { useCallback, useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import {
  ChevronDown,
  ChevronRight,
  ChevronUp,
  ExternalLink,
  FolderOpen,
  FolderPlus,
  Loader2,
  Plus,
  X as XIcon,
} from 'lucide-react'
import { SpritePet } from './SpritePet'
import {
  clearCodexPetCache,
  loadCodexPets,
  loadCustomCodexPets,
  type CodexPet,
} from '../lib/codexPet'

// Non-codex entries shown at the top of the 看板娘 list (e.g. the legacy
// WebM 香企鹅 character that doesn't have a sprite atlas). These render
// with a custom avatar instead of the sprite renderer and are forwarded
// to the parent via `onSelectSpecial`.
export interface SpecialPet {
  id: string
  displayName: string
  description?: string
  avatar: React.ReactNode
}

interface PetPickerProps {
  selectedId: string | null
  onSelect: (pet: CodexPet) => Promise<void> | void
  onSelectSpecial?: (pet: SpecialPet) => Promise<void> | void
  specialPets?: SpecialPet[]
  // Agent rotation queue. When provided, the picker renders an extra
  // "AGENT 队列" section above 自定义宠物 with reorder + remove + add UI.
  queueIds?: string[]
  onChangeQueue?: (next: string[]) => Promise<void> | void
  // Bracketed around invocations of native pickers (osascript/PowerShell
  // folder dialog) so the parent can suppress the window-blur handler
  // that would otherwise close the settings panel when the dialog steals
  // focus. Optional — picker still works without these.
  onNativeDialogStart?: () => void
  onNativeDialogEnd?: () => void
}

const PETDEX_URL = 'https://codex-pets.net/'
const CODEX_PETS_PATH_HINT = '~/.codex/pets'

// Top-level pet management UI. Replaces the legacy
// "Mascot + AGENT CHARACTER QUEUE" sections in the pairing tab. Built-in
// pets ship with the app under `assets/builtin/`. Custom pets live in the
// codex CLI's `~/.codex/pets/` directory and are scanned on demand by the
// Rust side (`list_custom_codex_pets`).
export function PetPicker({
  selectedId,
  onSelect,
  onSelectSpecial,
  specialPets,
  queueIds,
  onChangeQueue,
  onNativeDialogStart,
  onNativeDialogEnd,
}: PetPickerProps) {
  const [builtins, setBuiltins] = useState<CodexPet[]>([])
  const [customs, setCustoms] = useState<CodexPet[]>([])
  const [petsOpen, setPetsOpen] = useState(true)
  const [createOpen, setCreateOpen] = useState(false)
  const [queueAddOpen, setQueueAddOpen] = useState(false)
  // Anchors the bottom 创建 section so the header button can scroll it
  // into view when expanded from the top.
  const createSectionRef = useRef<HTMLDivElement | null>(null)
  const [importing, setImporting] = useState(false)
  const [importError, setImportError] = useState<string | null>(null)

  const loadAll = useCallback(async () => {
    clearCodexPetCache()
    const [b, c] = await Promise.all([loadCodexPets(), loadCustomCodexPets()])
    setBuiltins(b)
    // Dedupe: a custom pet with the same id as a builtin would render
    // twice (once as builtin, once as custom). The builtin assets ship
    // with the app and always load, so prefer them and drop the dupes.
    const builtinIds = new Set(b.map((p) => p.id))
    setCustoms(c.filter((p) => !builtinIds.has(p.id)))
  }, [])

  const toggleCreate = useCallback(() => {
    setCreateOpen((prev) => {
      const next = !prev
      if (next) {
        requestAnimationFrame(() => {
          createSectionRef.current?.scrollIntoView({ behavior: 'smooth', block: 'nearest' })
        })
      }
      return next
    })
  }, [])

  useEffect(() => {
    void loadAll()
  }, [loadAll])

  const handlePickFolder = useCallback(async () => {
    if (importing) return
    setImportError(null)
    onNativeDialogStart?.()
    try {
      const picked = (await invoke('pick_codex_pet_folder')) as string | null
      if (!picked) return
      setImporting(true)
      try {
        await invoke('import_codex_pet', { srcPath: picked })
        await loadAll()
      } catch (e: unknown) {
        console.warn('[PetPicker] import failed:', e)
        const msg =
          typeof e === 'string'
            ? e
            : e instanceof Error
              ? e.message
              : 'import failed'
        setImportError(msg)
      } finally {
        setImporting(false)
      }
    } catch (e) {
      console.warn('[PetPicker] picker failed:', e)
    } finally {
      // Hold the suppression for ~600ms after the dialog closes so the
      // brief focus-bounce macOS does (dialog → webview → another app
      // briefly → webview) cannot trigger a stray blur that would close
      // the settings panel.
      setTimeout(() => onNativeDialogEnd?.(), 600)
    }
  }, [importing, loadAll, onNativeDialogStart, onNativeDialogEnd])

  const selectedPet =
    builtins.find((p) => p.id === selectedId) ||
    customs.find((p) => p.id === selectedId) ||
    null
  const selectedSpecial = specialPets?.find((p) => p.id === selectedId) ?? null
  const selectedDisplayName = selectedPet?.displayName ?? selectedSpecial?.displayName ?? null

  const allPets = [...builtins, ...customs]
  const findPet = (id: string) => allPets.find((p) => p.id === id) ?? null

  const showQueue = queueIds !== undefined && onChangeQueue !== undefined

  const moveQueueItem = useCallback(
    (idx: number, delta: -1 | 1) => {
      if (!queueIds || !onChangeQueue) return
      const target = idx + delta
      if (target < 0 || target >= queueIds.length) return
      const next = [...queueIds]
      ;[next[idx], next[target]] = [next[target], next[idx]]
      void onChangeQueue(next)
    },
    [queueIds, onChangeQueue],
  )

  const removeQueueItem = useCallback(
    (idx: number) => {
      if (!queueIds || !onChangeQueue) return
      if (queueIds.length <= 1) return
      void onChangeQueue(queueIds.filter((_, i) => i !== idx))
    },
    [queueIds, onChangeQueue],
  )

  const addToQueue = useCallback(
    (id: string) => {
      if (!queueIds || !onChangeQueue) return
      void onChangeQueue([...queueIds, id])
      setQueueAddOpen(false)
    },
    [queueIds, onChangeQueue],
  )

  return (
    <div className="space-y-4">
      <PetSection
        title="看板娘"
        subtitle={selectedDisplayName ? `已选择 ${selectedDisplayName}` : null}
        open={petsOpen}
        onToggle={() => setPetsOpen((v) => !v)}
        actions={
          <>
            <button
              data-no-drag
              onClick={(e) => {
                e.stopPropagation()
                invoke('open_codex_pets_dir').catch(() => {})
              }}
              className="flex items-center gap-1.5 px-2 py-1 rounded-md text-[11px] text-white/35 hover:text-white/70 hover:bg-white/[0.04] transition-colors"
              title={CODEX_PETS_PATH_HINT}
            >
              <FolderOpen className="w-3 h-3" strokeWidth={2} />
              打开文件夹
            </button>
            <button
              data-no-drag
              onClick={(e) => {
                e.stopPropagation()
                toggleCreate()
              }}
              className="flex items-center gap-1.5 px-3 py-1 rounded-md bg-white/[0.05] hover:bg-white/10 text-xs text-white/70"
            >
              <Plus className="w-3 h-3" strokeWidth={2.5} />
              创建
            </button>
          </>
        }
      >
        {(specialPets ?? []).map((pet) => (
          <SpecialPetRow
            key={pet.id}
            pet={pet}
            selected={pet.id === selectedId}
            onSelect={() => onSelectSpecial?.(pet)}
          />
        ))}
        {builtins.length === 0 && customs.length === 0 && (specialPets?.length ?? 0) === 0 ? (
          <div className="text-xs text-white/30 px-4 py-6 text-center">没有看板娘</div>
        ) : (
          <>
            {builtins.map((pet) => (
              <PetRow
                key={pet.id}
                pet={pet}
                selected={pet.id === selectedId}
                onSelect={() => onSelect(pet)}
              />
            ))}
            {customs.map((pet) => (
              <PetRow
                key={pet.id}
                pet={pet}
                selected={pet.id === selectedId}
                onSelect={() => onSelect(pet)}
              />
            ))}
          </>
        )}
      </PetSection>

      {showQueue && (
        <div className="bg-[#0f0f0f] rounded-2xl border border-white/5 overflow-hidden">
          <div className="px-5 py-4 border-b border-white/5">
            <div className="flex items-center justify-between gap-3">
              <span className="text-xs font-bold text-white/30 uppercase tracking-widest">
                Agent 队列
              </span>
              <span className="text-[11px] text-white/30">每个 session 按顺序轮换</span>
            </div>
          </div>
          <div className="p-3 space-y-2">
            {queueIds!.map((id, qi) => {
              const meta = findPet(id)
              return (
                <div
                  key={`${id}-${qi}`}
                  className="flex items-center gap-3 p-2.5 rounded-xl bg-white/[0.04] border border-white/5"
                >
                  <span className="text-[11px] text-white/30 w-5 text-center shrink-0">
                    {qi + 1}
                  </span>
                  <div
                    className="shrink-0 rounded-md bg-black/40 border border-white/10 overflow-hidden flex items-center justify-center"
                    style={{ width: 32, height: 32 }}
                  >
                    {meta ? (
                      <SpritePet pet={meta} state="idle" size={32} />
                    ) : (
                      <span className="text-[10px] text-white/30">?</span>
                    )}
                  </div>
                  <span className="text-sm text-white/80 truncate flex-1">
                    {meta?.displayName ?? id}
                  </span>
                  <div className="flex items-center gap-1 shrink-0">
                    <button
                      data-no-drag
                      onClick={() => moveQueueItem(qi, -1)}
                      disabled={qi === 0}
                      className="w-6 h-6 flex items-center justify-center rounded text-white/40 hover:text-white hover:bg-white/10 transition-colors disabled:opacity-20 disabled:cursor-not-allowed"
                    >
                      <ChevronUp className="w-3.5 h-3.5" />
                    </button>
                    <button
                      data-no-drag
                      onClick={() => moveQueueItem(qi, 1)}
                      disabled={qi === queueIds!.length - 1}
                      className="w-6 h-6 flex items-center justify-center rounded text-white/40 hover:text-white hover:bg-white/10 transition-colors disabled:opacity-20 disabled:cursor-not-allowed"
                    >
                      <ChevronDown className="w-3.5 h-3.5" />
                    </button>
                    <button
                      data-no-drag
                      onClick={() => removeQueueItem(qi)}
                      disabled={queueIds!.length <= 1}
                      className="w-6 h-6 flex items-center justify-center rounded text-white/40 hover:text-rose-400 hover:bg-rose-500/10 transition-colors disabled:opacity-20 disabled:cursor-not-allowed ml-1"
                    >
                      <XIcon className="w-3.5 h-3.5" />
                    </button>
                  </div>
                </div>
              )
            })}
            <button
              data-no-drag
              onClick={() => setQueueAddOpen((v) => !v)}
              className="flex items-center gap-2 w-full p-2.5 rounded-xl border border-dashed border-white/10 text-white/40 hover:text-white/70 hover:border-white/20 hover:bg-white/[0.02] transition-colors"
            >
              <Plus className="w-4 h-4" />
              <span className="text-sm">添加宠物</span>
            </button>
            {queueAddOpen && (
              <div className="rounded-xl border border-white/10 bg-black/30 p-3 max-h-[260px] overflow-y-auto scrollbar-hidden">
                <div className="text-[11px] text-white/30 mb-2 px-1">点击宠物加入队列</div>
                <div className="grid grid-cols-2 gap-2">
                  {allPets.map((pet) => (
                    <button
                      data-no-drag
                      key={pet.id}
                      onClick={() => addToQueue(pet.id)}
                      className="flex items-center gap-2 p-2 rounded-lg bg-white/[0.04] hover:bg-white/[0.08] border border-transparent hover:border-white/10 transition-colors"
                    >
                      <div
                        className="shrink-0 rounded-md bg-black/40 border border-white/10 overflow-hidden flex items-center justify-center"
                        style={{ width: 28, height: 28 }}
                      >
                        <SpritePet pet={pet} state="idle" size={28} />
                      </div>
                      <span className="text-xs text-white/75 truncate flex-1 text-left">
                        {pet.displayName}
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            )}
          </div>
        </div>
      )}

      <div ref={createSectionRef} className="bg-[#0f0f0f] rounded-2xl border border-white/5 overflow-hidden">
        <button
          data-no-drag
          onClick={() => setCreateOpen((v) => !v)}
          className="w-full flex items-center justify-between px-5 py-4 hover:bg-white/[0.03] transition-colors"
        >
          <div className="flex items-center gap-2 text-sm font-medium text-white/80">
            {createOpen ? <ChevronDown className="w-4 h-4" /> : <ChevronRight className="w-4 h-4" />}
            创建
          </div>
          <span className="text-xs text-white/30">下载或选择本地 codex 宠物</span>
        </button>
        {createOpen && (
          <div className="px-5 pb-5 space-y-3">
            <button
              data-no-drag
              onClick={() => invoke('open_url', { url: PETDEX_URL }).catch(() => {})}
              className="w-full flex items-center gap-3 px-4 py-3 rounded-xl bg-white/[0.04] hover:bg-white/[0.07] transition-colors"
            >
              <CreateStepBadge n={1} />
              <div className="flex flex-col items-start gap-0.5 flex-1 min-w-0">
                <span className="text-sm text-white/85 font-medium">前往 Codex Pets 下载</span>
                <span className="text-[11px] text-white/40 truncate">{PETDEX_URL}</span>
              </div>
              <ExternalLink className="w-4 h-4 text-white/40 shrink-0" strokeWidth={2.5} />
            </button>
            <button
              data-no-drag
              onClick={handlePickFolder}
              disabled={importing}
              className="w-full flex items-center gap-3 px-4 py-3 rounded-xl bg-white/[0.04] hover:bg-white/[0.07] disabled:opacity-60 disabled:cursor-not-allowed transition-colors"
            >
              <CreateStepBadge n={2} />
              <div className="flex flex-col items-start gap-0.5 flex-1 min-w-0">
                <span className="text-sm text-white/85 font-medium">
                  {importing ? '正在导入…' : '选择本地文件夹导入'}
                </span>
                <span className="text-[11px] text-white/40">
                  会复制到 {CODEX_PETS_PATH_HINT}/&lt;id&gt;
                </span>
              </div>
              {importing ? (
                <Loader2 className="w-4 h-4 text-white/40 animate-spin shrink-0" strokeWidth={2.5} />
              ) : (
                <FolderPlus className="w-4 h-4 text-white/40 shrink-0" strokeWidth={2.5} />
              )}
            </button>
            {importError && (
              <div className="text-[11px] text-rose-400 px-1">{importError}</div>
            )}
          </div>
        )}
      </div>
    </div>
  )
}

interface PetSectionProps {
  title: string
  subtitle?: string | null
  open: boolean
  onToggle: () => void
  actions?: React.ReactNode
  children: React.ReactNode
}

function PetSection({ title, subtitle, open, onToggle, actions, children }: PetSectionProps) {
  return (
    <div className="bg-[#0f0f0f] rounded-2xl border border-white/5 overflow-hidden">
      <button
        data-no-drag
        onClick={onToggle}
        className="w-full flex items-start justify-between gap-3 px-5 py-4 hover:bg-white/[0.03] transition-colors"
      >
        <div className="flex flex-col items-start gap-0.5 min-w-0">
          <span className="text-sm font-medium text-white/85 flex items-center gap-2">
            {title}
            {open ? <ChevronDown className="w-4 h-4 text-white/40" /> : <ChevronRight className="w-4 h-4 text-white/40" />}
          </span>
          {subtitle && (
            <span className="text-[11px] text-white/40 truncate font-mono">{subtitle}</span>
          )}
        </div>
        {actions && (
          <div
            className="flex items-center gap-2 shrink-0"
            onClick={(e) => e.stopPropagation()}
          >
            {actions}
          </div>
        )}
      </button>
      {open && <div className="border-t border-white/5">{children}</div>}
    </div>
  )
}

interface PetRowProps {
  pet: CodexPet
  selected: boolean
  onSelect: () => void
}

function PetRow({ pet, selected, onSelect }: PetRowProps) {
  return (
    <div className="flex items-center gap-3 px-5 py-3 border-b border-white/5 last:border-b-0 hover:bg-white/[0.02] transition-colors">
      <div
        className="shrink-0 rounded-lg bg-black/40 border border-white/10 overflow-hidden flex items-center justify-center"
        style={{ width: 40, height: 40 }}
      >
        <SpritePet pet={pet} state="idle" size={40} />
      </div>
      <div className="min-w-0 flex-1">
        <div className="text-sm text-white/85 font-medium truncate">{pet.displayName}</div>
        {pet.description && (
          <div className="text-[11px] text-white/40 truncate">{pet.description}</div>
        )}
      </div>
      <button
        data-no-drag
        onClick={onSelect}
        disabled={selected}
        className={`shrink-0 px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
          selected
            ? 'bg-white/[0.04] text-white/40 cursor-default'
            : 'bg-white/[0.06] hover:bg-white/[0.12] text-white/80'
        }`}
      >
        {selected ? '已选' : '选择'}
      </button>
    </div>
  )
}

// Numbered badge shown next to each step in the 创建 section so the
// download → import flow reads as ordered (1 first, 2 second).
function CreateStepBadge({ n }: { n: number }) {
  return (
    <span
      className="shrink-0 inline-flex items-center justify-center rounded-full bg-white/[0.08] text-white/70 text-[11px] font-semibold"
      style={{ width: 20, height: 20 }}
    >
      {n}
    </span>
  )
}

interface SpecialPetRowProps {
  pet: SpecialPet
  selected: boolean
  onSelect: () => void
}

function SpecialPetRow({ pet, selected, onSelect }: SpecialPetRowProps) {
  return (
    <div className="flex items-center gap-3 px-5 py-3 border-b border-white/5 last:border-b-0 hover:bg-white/[0.02] transition-colors">
      <div
        className="shrink-0 rounded-lg bg-black/40 border border-white/10 overflow-hidden flex items-center justify-center"
        style={{ width: 40, height: 40 }}
      >
        {pet.avatar}
      </div>
      <div className="min-w-0 flex-1">
        <div className="text-sm text-white/85 font-medium truncate">{pet.displayName}</div>
        {pet.description && (
          <div className="text-[11px] text-white/40 truncate">{pet.description}</div>
        )}
      </div>
      <button
        data-no-drag
        onClick={onSelect}
        disabled={selected}
        className={`shrink-0 px-3 py-1.5 rounded-md text-xs font-medium transition-colors ${
          selected
            ? 'bg-white/[0.04] text-white/40 cursor-default'
            : 'bg-white/[0.06] hover:bg-white/[0.12] text-white/80'
        }`}
      >
        {selected ? '已选' : '选择'}
      </button>
    </div>
  )
}
