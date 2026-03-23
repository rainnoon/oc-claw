import { invoke } from '@tauri-apps/api/core'
import { emit } from '@tauri-apps/api/event'
import { load } from '@tauri-apps/plugin-store'
import type { CharacterMeta } from './types'

export async function getStore() {
  return load('settings.json', { defaults: {}, autoSave: true })
}

const ASSET_PREFIX = import.meta.env.DEV ? '/assets' : 'localasset://localhost'
export const CUSTOM_ASSET_PREFIX = import.meta.env.DEV ? '/custom_assets' : 'customasset://localhost'

export const DEFAULT_CHAR: CharacterMeta = {
  name: 'default',
  workGifs: [],
  restGifs: [],
  miniActions: {
    top: [
      `${ASSET_PREFIX}/default/mini/top/sleep.gif`,
      `${ASSET_PREFIX}/default/mini/top/work.gif`,
    ],
  },
}

export const MINI_CATEGORIES = ['top', 'walk', 'fish', 'sport']

export async function loadCharacters(): Promise<CharacterMeta[]> {
  const store = await getStore()

  let scanned: CharacterMeta[] = []
  try {
    scanned = (await invoke('scan_characters')) as CharacterMeta[]
  } catch (e) {
    console.warn('[loadCharacters] scan_characters failed:', e)
  }

  const scannedDefault = scanned.find((sc) => sc.name === 'default')
  const merged: CharacterMeta[] = [scannedDefault ? { ...DEFAULT_CHAR, ...scannedDefault } : DEFAULT_CHAR]
  for (const sc of scanned) {
    if (sc.name === 'default') continue
    merged.push(sc)
  }

  await store.set('characters', merged)

  // Clean up stale pairings that reference removed characters
  const validNames = new Set(merged.map((c) => c.name))
  const charMap = ((await store.get('agent_char_map')) as Record<string, string>) || {}
  let mapDirty = false
  for (const [k, v] of Object.entries(charMap)) {
    if (v && !validNames.has(v)) { charMap[k] = 'default'; mapDirty = true }
  }
  if (mapDirty) await store.set('agent_char_map', charMap)

  const claudeChar = (await store.get('claude_char')) as string
  if (claudeChar && !validNames.has(claudeChar)) await store.set('claude_char', 'default')

  const miniChar = (await store.get('mini_character')) as string
  if (miniChar && !validNames.has(miniChar)) await store.set('mini_character', 'default')

  await store.save()

  return merged
}

export async function saveCharacters(chars: CharacterMeta[]) {
  const store = await getStore()
  await store.set('characters', chars)
  await store.save()
  await emit('character-changed')
}

export async function getActiveCharacter(): Promise<string> {
  const store = await getStore()
  return ((await store.get('active_character')) as string) || 'default'
}

export async function setActiveCharacter(name: string) {
  const store = await getStore()
  await store.set('active_character', name)
  await store.save()
  await emit('character-changed')
}

export function fileToDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve(reader.result as string)
    reader.onerror = reject
    reader.readAsDataURL(file)
  })
}

