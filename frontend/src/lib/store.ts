import { invoke } from '@tauri-apps/api/core'
import { emit } from '@tauri-apps/api/event'
import { load } from '@tauri-apps/plugin-store'
import type { CharacterMeta, OcConnection } from './types'

export async function getStore() {
  return load('settings.json', { defaults: {}, autoSave: true })
}

// On Windows, WebView2 maps custom URI schemes to http://<scheme>.localhost/
// instead of <scheme>://localhost/. Detect platform to use the correct prefix.
const isWindows = typeof navigator !== 'undefined' && navigator.userAgent.includes('Windows')
const ASSET_PREFIX = import.meta.env.DEV
  ? '/assets/builtin'
  : isWindows ? 'http://localasset.localhost' : 'localasset://localhost'
export const CUSTOM_ASSET_PREFIX = import.meta.env.DEV
  ? '/assets/custom'
  : isWindows ? 'http://customasset.localhost' : 'customasset://localhost'

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
  let configDefaults: Record<string, unknown> | null = null
  try {
    const result = (await invoke('scan_characters')) as { characters: CharacterMeta[]; defaults?: Record<string, unknown> }
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

  // Clean up char_queue: remove entries referencing deleted characters
  let existingQueue = (await store.get('char_queue')) as string[] | null
  if (existingQueue) {
    const cleaned = existingQueue.filter((n) => validNames.has(n))
    if (cleaned.length !== existingQueue.length) {
      existingQueue = cleaned.length ? cleaned : null
      await store.set('char_queue', existingQueue ?? [DEFAULT_CHAR_NAME])
    }
  }

  // Apply defaults from characters.json for unset values
  if (configDefaults) {
    if (!miniChar && typeof configDefaults.mini_character === 'string' && validNames.has(configDefaults.mini_character)) {
      await store.set('mini_character', configDefaults.mini_character)
    }
    if (!claudeChar && typeof configDefaults.claude_char === 'string' && validNames.has(configDefaults.claude_char)) {
      await store.set('claude_char', configDefaults.claude_char)
    }
    if (Array.isArray(configDefaults.char_queue)) {
      const defaultQueue = (configDefaults.char_queue as string[]).filter((n) => validNames.has(n))
      if (defaultQueue.length && (!existingQueue || existingQueue.length < defaultQueue.length)) {
        // Merge: keep user's existing order, append missing defaults
        const merged = [...(existingQueue || [])]
        for (const name of defaultQueue) {
          if (!merged.includes(name)) merged.push(name)
        }
        await store.set('char_queue', merged)
      }
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

/** Load OC connections, migrating from old single-connection format if needed. */
export async function loadOcConnections(): Promise<OcConnection[]> {
  const store = await getStore()
  const existing = await store.get('oc_connections') as OcConnection[] | null
  if (existing) return existing

  // Migrate from old format
  const mode = ((await store.get('oc_mode')) as string) || 'local'
  const host = ((await store.get('ssh_host')) as string) || ''
  const user = ((await store.get('ssh_user')) as string) || ''
  const connections: OcConnection[] = []
  if (mode === 'remote' && host && user) {
    connections.push({ id: crypto.randomUUID(), type: 'remote', host, user })
  } else {
    connections.push({ id: crypto.randomUUID(), type: 'local' })
  }
  await store.set('oc_connections', connections)
  await store.save()
  return connections
}

export async function saveOcConnections(connections: OcConnection[]) {
  const store = await getStore()
  await store.set('oc_connections', connections)
  await store.save()
}

export function fileToDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve(reader.result as string)
    reader.onerror = reject
    reader.readAsDataURL(file)
  })
}

