import { invoke } from '@tauri-apps/api/core'
import { emit } from '@tauri-apps/api/event'
import { load } from '@tauri-apps/plugin-store'
import type { CharacterMeta } from './types'

export async function getStore() {
  return load('settings.json', { defaults: {}, autoSave: true })
}

const ASSET_PREFIX = import.meta.env.DEV ? '/assets/builtin' : 'localasset://localhost'
export const CUSTOM_ASSET_PREFIX = import.meta.env.DEV ? '/assets/custom' : 'customasset://localhost'

export const DEFAULT_CHAR_NAME = '诗歌剧'

export const DEFAULT_CHAR: CharacterMeta = {
  name: DEFAULT_CHAR_NAME,
  workGifs: [],
  restGifs: [],
  miniActions: {
    top: [
      `${ASSET_PREFIX}/${DEFAULT_CHAR_NAME}/mini/top/sleeping.gif`,
      `${ASSET_PREFIX}/${DEFAULT_CHAR_NAME}/mini/top/working.gif`,
    ],
  },
}

export const MINI_CATEGORIES = ['top', 'walk', 'fish', 'sport']

export async function loadCharacters(): Promise<CharacterMeta[]> {
  const store = await getStore()

  let scanned: CharacterMeta[] = []
  let configDefaults: Record<string, string> | null = null
  try {
    const result = (await invoke('scan_characters')) as { characters: CharacterMeta[]; defaults?: Record<string, string> }
    scanned = result.characters
    configDefaults = result.defaults || null
  } catch (e) {
    console.warn('[loadCharacters] scan_characters failed:', e)
  }

  const scannedDefault = scanned.find((sc) => sc.name === DEFAULT_CHAR_NAME)
  const merged: CharacterMeta[] = [scannedDefault ? { ...DEFAULT_CHAR, ...scannedDefault } : DEFAULT_CHAR]
  for (const sc of scanned) {
    if (sc.name === DEFAULT_CHAR_NAME) continue
    merged.push(sc)
  }

  await store.set('characters', merged)

  // Clean up stale pairings that reference removed characters
  const validNames = new Set(merged.map((c) => c.name))
  const charMap = ((await store.get('agent_char_map')) as Record<string, string>) || {}
  let mapDirty = false
  for (const [k, v] of Object.entries(charMap)) {
    if (v && !validNames.has(v)) { charMap[k] = DEFAULT_CHAR_NAME; mapDirty = true }
  }
  if (mapDirty) await store.set('agent_char_map', charMap)

  const claudeChar = (await store.get('claude_char')) as string
  if (claudeChar && !validNames.has(claudeChar)) await store.set('claude_char', DEFAULT_CHAR_NAME)

  const miniChar = (await store.get('mini_character')) as string
  if (miniChar && !validNames.has(miniChar)) await store.set('mini_character', DEFAULT_CHAR_NAME)

  // Apply defaults from characters.json for unset values
  if (configDefaults) {
    if (!miniChar && configDefaults.mini_character && validNames.has(configDefaults.mini_character)) {
      await store.set('mini_character', configDefaults.mini_character)
    }
    if (!claudeChar && configDefaults.claude_char && validNames.has(configDefaults.claude_char)) {
      await store.set('claude_char', configDefaults.claude_char)
    }
  }

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
  return ((await store.get('active_character')) as string) || DEFAULT_CHAR_NAME
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

