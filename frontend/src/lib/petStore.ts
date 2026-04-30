import { load } from '@tauri-apps/plugin-store'

// ─── Types ───

export type AppMode = 'coding' | 'pet'

export type PetAction =
  | 'idle'     // default loop
  | 'sleep'    // idle timeout / night
  | 'work'     // pomodoro active (earns coins)
  | 'study'    // pomodoro with timer overlay
  | 'watch'    // user-triggered activity
  | 'music'    // user-triggered activity
  | 'walk'     // requires hunger >= 30
  | 'dance'    // random during music
  | 'eat'      // feeding animation
  | 'hungry'   // hunger < 30
  | 'headpat'  // user clicks head
  | 'farewell' // app closing
  | 'grasp'    // being dragged
  | 'angry'    // low affection click
  | 'spin'     // high affection click (replaces shy)
  | 'milktea'  // random idle animation
  | 'rest'     // idle when no user activity for 5min
  | 'peek'     // peeking from screen edge
  | 'walkout'  // walking out from screen edge after peek

export interface FoodItem {
  id: string
  name: string
  icon: string
  iconWin?: string
  hunger: number
  affection: number
  price: number
}

export interface PetData {
  hunger: number        // 0-100, default 100
  affection: number     // 0-100, default 50
  coins: number         // >= 0
  lastTickAt: number    // timestamp for decay calc
  lastDailyGift: string // YYYY-MM-DD of last daily gift
  headpatToday: number  // headpat count today
  headpatDate: string   // YYYY-MM-DD
  pomodoroCoins: number // coins earned in current pomodoro
}

export interface PomodoroState {
  active: boolean
  duration: number      // total seconds
  remaining: number     // seconds left
  startedAt: number     // timestamp
}

// ─── Constants ───

export const HUNGER_MAX = 100
export const HUNGER_INIT = 100
export const HUNGER_DECAY_PER_HOUR = 2
export const HUNGER_DECAY_SLEEP_PER_HOUR = 1
export const HUNGER_OFFLINE_FLOOR = 10
export const AFFECTION_MAX = 100
export const AFFECTION_INIT = 100 // TODO: restore to 50 after testing
export const AFFECTION_DECAY_PER_DAY = 5
export const AFFECTION_HUNGRY_DECAY_PER_HOUR = 2
export const AFFECTION_OFFLINE_FLOOR = 10
export const AFFECTION_HEADPAT = 2
export const AFFECTION_HEADPAT_DAILY_LIMIT = 5
export const AFFECTION_ACTIVITY_PER_10MIN = 1
export const AFFECTION_FEED_HUNGRY = 5
export const HUNGER_ACTIVITY_PER_HOUR = 3

export const DAILY_GIFT_MIN = 20
export const DAILY_GIFT_MAX = 60
export const POMODORO_COINS_PER_MIN = 1

export const FOODS: FoodItem[] = [
  { id: 'meat', name: '肉', icon: '🍖', iconWin: '🥩', hunger: 15, affection: 0, price: 8 },
  { id: 'boba', name: '奶茶', icon: '🧋', iconWin: '🥛', hunger: 8, affection: 3, price: 6 },
]

export const POMODORO_PRESETS = [15, 25, 45, 60] // minutes

// ─── Affection tiers ───

export type AffectionTier = 'angry' | 'cold' | 'happy' | 'shy'

export function getAffectionTier(affection: number): AffectionTier {
  if (affection >= 80) return 'shy'
  if (affection >= 50) return 'happy'
  if (affection >= 20) return 'cold'
  return 'angry'
}

// ─── Store helpers ───

const STORE_NAME = 'pet-data.json'

let storePromise: ReturnType<typeof load> | null = null

export function getPetStore() {
  if (!storePromise) {
    storePromise = load(STORE_NAME, { defaults: {}, autoSave: true })
  }
  return storePromise
}

export function defaultPetData(): PetData {
  return {
    hunger: HUNGER_INIT,
    affection: AFFECTION_INIT,
    coins: 0,
    lastTickAt: Date.now(),
    lastDailyGift: '',
    headpatToday: 0,
    headpatDate: '',
    pomodoroCoins: 0,
  }
}

export async function loadPetData(): Promise<PetData> {
  const store = await getPetStore()
  const raw = await store.get('pet') as PetData | null
  if (!raw) {
    const d = defaultPetData()
    await store.set('pet', d)
    await store.save()
    return d
  }
  const d = { ...defaultPetData(), ...raw }
  d.affection = 100 // TODO: remove after testing
  return d
}

export async function savePetData(data: PetData): Promise<void> {
  const store = await getPetStore()
  await store.set('pet', data)
  await store.save()
}

export async function loadAppMode(): Promise<AppMode | null> {
  const store = await getPetStore()
  return (await store.get('app_mode')) as AppMode | null
}

export async function saveAppMode(mode: AppMode): Promise<void> {
  const store = await getPetStore()
  await store.set('app_mode', mode)
  await store.save()
}

// ─── App-mode onboarding version gate ───
//
// Bump this whenever we want to force existing users through the mode-pick
// onboarding again (e.g. when adding a new mode option or changing the
// default behavior of an existing one). On launch, if the stored version is
// missing or strictly below this value, the saved app_mode is ignored and
// the onboarding modal is shown again. After the user picks a mode we save
// this constant so subsequent launches pass the check.
export const APP_MODE_ONBOARDING_VERSION = '1.8.1'

export async function loadAppModeVersion(): Promise<string | null> {
  const store = await getPetStore()
  return (await store.get('app_mode_version')) as string | null
}

export async function saveAppModeVersion(version: string): Promise<void> {
  const store = await getPetStore()
  await store.set('app_mode_version', version)
  await store.save()
}

// Compare two dotted version strings (e.g. "1.8.1" vs "1.8.0").
// Missing segments are treated as 0; non-numeric segments are treated as 0
// so we degrade gracefully on malformed input. Returns -1/0/1.
export function compareVersion(a: string, b: string): number {
  const parse = (v: string) => v.split('.').map((s) => {
    const n = parseInt(s, 10)
    return Number.isFinite(n) ? n : 0
  })
  const pa = parse(a)
  const pb = parse(b)
  const len = Math.max(pa.length, pb.length)
  for (let i = 0; i < len; i++) {
    const ai = pa[i] ?? 0
    const bi = pb[i] ?? 0
    if (ai > bi) return 1
    if (ai < bi) return -1
  }
  return 0
}

// True when the stored onboarding version is missing or older than the
// required version, meaning the user should be re-onboarded.
export function isAppModeOnboardingStale(stored: string | null | undefined): boolean {
  if (!stored) return true
  return compareVersion(stored, APP_MODE_ONBOARDING_VERSION) < 0
}

// ─── Tick logic: apply time-based decay ───

function isSleepHour(hour: number): boolean {
  return hour >= 0 && hour < 8
}

export function tickPetData(data: PetData): PetData {
  const now = Date.now()
  const elapsed = now - data.lastTickAt
  if (elapsed < 60_000) return data // skip if < 1 min

  const hours = elapsed / 3_600_000
  const d = { ...data, lastTickAt: now }

  // Hunger decay
  const currentHour = new Date().getHours()
  const decayRate = isSleepHour(currentHour) ? HUNGER_DECAY_SLEEP_PER_HOUR : HUNGER_DECAY_PER_HOUR
  d.hunger = Math.max(HUNGER_OFFLINE_FLOOR, d.hunger - decayRate * hours)

  // Affection decay (daily)
  const dayFraction = hours / 24
  let affectionLoss = AFFECTION_DECAY_PER_DAY * dayFraction
  if (d.hunger < 30) {
    affectionLoss += AFFECTION_HUNGRY_DECAY_PER_HOUR * hours
  }
  d.affection = Math.max(AFFECTION_OFFLINE_FLOOR, d.affection - affectionLoss)

  // Reset headpat counter for new day
  const today = new Date().toISOString().slice(0, 10)
  if (d.headpatDate !== today) {
    d.headpatToday = 0
    d.headpatDate = today
  }

  return d
}

// ─── Daily gift ───

function localDateKey(d = new Date()): string {
  const y = d.getFullYear()
  const m = String(d.getMonth() + 1).padStart(2, '0')
  const day = String(d.getDate()).padStart(2, '0')
  return `${y}-${m}-${day}`
}

export function canClaimDailyGift(data: PetData): boolean {
  return data.lastDailyGift !== localDateKey()
}

export function claimDailyGift(data: PetData): { data: PetData; amount: number } {
  if (!canClaimDailyGift(data)) return { data, amount: 0 }
  const amount = DAILY_GIFT_MIN + Math.floor(Math.random() * (DAILY_GIFT_MAX - DAILY_GIFT_MIN + 1))
  return {
    data: {
      ...data,
      coins: data.coins + amount,
      lastDailyGift: localDateKey(),
    },
    amount,
  }
}

// ─── Action helpers ───

export function canWalk(data: PetData): boolean {
  return data.hunger >= 30
}

export function canHeadpat(data: PetData): boolean {
  return data.headpatToday < AFFECTION_HEADPAT_DAILY_LIMIT
}

export function applyHeadpat(data: PetData): PetData {
  if (!canHeadpat(data)) return data
  return {
    ...data,
    affection: Math.min(AFFECTION_MAX, data.affection + AFFECTION_HEADPAT),
    headpatToday: data.headpatToday + 1,
  }
}

export function applyFeed(data: PetData, food: FoodItem): PetData {
  if (data.coins < food.price) return data
  const wasHungry = data.hunger < 30
  const d = {
    ...data,
    coins: data.coins - food.price,
    hunger: Math.min(HUNGER_MAX, data.hunger + food.hunger),
    affection: Math.min(AFFECTION_MAX, data.affection + food.affection),
  }
  if (wasHungry) {
    d.affection = Math.min(AFFECTION_MAX, d.affection + AFFECTION_FEED_HUNGRY)
  }
  return d
}
