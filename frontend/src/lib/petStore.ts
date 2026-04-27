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
  | 'shy'      // high affection click

export interface FoodItem {
  id: string
  name: string
  icon: string
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
  { id: 'meat', name: '肉', icon: '🍖', hunger: 15, affection: 0, price: 8 },
  { id: 'boba', name: '奶茶', icon: '🧋', hunger: 8, affection: 3, price: 6 },
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

export function canClaimDailyGift(_data: PetData): boolean {
  // TODO: restore daily check after testing
  // const today = new Date().toISOString().slice(0, 10)
  // return data.lastDailyGift !== today
  return true
}

export function claimDailyGift(data: PetData): { data: PetData; amount: number } {
  if (!canClaimDailyGift(data)) return { data, amount: 0 }
  const amount = DAILY_GIFT_MIN + Math.floor(Math.random() * (DAILY_GIFT_MAX - DAILY_GIFT_MIN + 1))
  return {
    data: {
      ...data,
      coins: data.coins + amount,
      lastDailyGift: new Date().toISOString().slice(0, 10),
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
