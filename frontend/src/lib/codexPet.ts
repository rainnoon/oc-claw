// Codex-compatible pet asset model.
// Each pet is a single 8-column x 9-row atlas of 192x208 cells (1536x1872),
// matching the openai/skills hatch-pet output format. Row layout is by
// convention; pet.json itself does not declare it.

export type CodexPetState =
  | 'idle'
  | 'run-right'
  | 'run-left'
  | 'waving'
  | 'jumping'
  | 'failed'
  | 'waiting'
  | 'running'
  | 'review'

export interface CodexPet {
  id: string
  displayName: string
  description: string
  // Resolved absolute URL ready to use as a CSS background-image source.
  spritesheetUrl: string
}

export const ATLAS = {
  cellW: 192,
  cellH: 208,
  cols: 8,
  rows: 9,
} as const

export interface AnimationRow {
  row: number
  frames: number
}

// Standard hatch-pet row layout. Frame counts come from the canonical
// references/animation-rows.md contract used by the curated skill.
export const ANIMATION_ROWS: Record<CodexPetState, AnimationRow> = {
  'idle':      { row: 0, frames: 6 },
  'run-right': { row: 1, frames: 8 },
  'run-left':  { row: 2, frames: 8 },
  'waving':    { row: 3, frames: 4 },
  'jumping':   { row: 4, frames: 5 },
  'failed':    { row: 5, frames: 8 },
  'waiting':   { row: 6, frames: 6 },
  'running':   { row: 7, frames: 6 },
  'review':    { row: 8, frames: 6 },
}

export const SPRITE_FPS = 12

// Per-state fps overrides. States not listed fall back to SPRITE_FPS.
// Idle is intentionally slow (subtle breathing-style loop). Jumping plays
// slower than the default 12fps so the 5-frame animation reads clearly
// during the brief one-shot.
export const STATE_FPS: Partial<Record<CodexPetState, number>> = {
  idle: 2,
  jumping: 6,
}

export function fpsFor(state: CodexPetState): number {
  return STATE_FPS[state] ?? SPRITE_FPS
}

export const DEFAULT_PET_ID = 'homie'

// Maps Mini.tsx's `PetState` (idle/working/compacting/waiting) to a codex
// sprite state. Walking direction and hover are layered on top of this by
// the wrapper component, not by this function.
export type MiniPetSourceState = 'idle' | 'working' | 'compacting' | 'waiting'

export function petStateToCodexState(state: MiniPetSourceState): CodexPetState {
  switch (state) {
    case 'idle':
      return 'idle'
    case 'working':
    case 'compacting':
      return 'running'
    case 'waiting':
      return 'waiting'
    default:
      return 'idle'
  }
}

const BUILTIN_BASE = '/assets/builtin'
const MANIFEST_URL = `${BUILTIN_BASE}/pets-manifest.json`

interface RawPetMeta {
  id: string
  displayName: string
  description: string
  spritesheetPath: string
}

interface PetsManifest {
  pets: string[]
}

let cachedPets: Promise<CodexPet[]> | null = null

export function loadCodexPets(): Promise<CodexPet[]> {
  if (!cachedPets) {
    cachedPets = (async () => {
      const manifestRes = await fetch(MANIFEST_URL)
      if (!manifestRes.ok) {
        throw new Error(`pets-manifest.json fetch failed: ${manifestRes.status}`)
      }
      const manifest = (await manifestRes.json()) as PetsManifest
      const ids = Array.isArray(manifest.pets) ? manifest.pets : []

      const results = await Promise.all(
        ids.map(async (id): Promise<CodexPet | null> => {
          try {
            const res = await fetch(`${BUILTIN_BASE}/${id}/pet.json`)
            if (!res.ok) return null
            const meta = (await res.json()) as RawPetMeta
            return {
              id: meta.id || id,
              displayName: meta.displayName || id,
              description: meta.description || '',
              spritesheetUrl: `${BUILTIN_BASE}/${id}/${meta.spritesheetPath}`,
            }
          } catch {
            return null
          }
        }),
      )
      return results.filter((p): p is CodexPet => p !== null)
    })()
  }
  return cachedPets
}

export async function loadCodexPetById(id: string): Promise<CodexPet | null> {
  const pets = await loadCodexPets()
  return pets.find((p) => p.id === id) ?? null
}

export async function loadDefaultCodexPet(): Promise<CodexPet | null> {
  const pets = await loadCodexPets()
  if (pets.length === 0) return null
  return pets.find((p) => p.id === DEFAULT_PET_ID) ?? pets[0]
}
