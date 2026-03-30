import i18n from 'i18next'
import { initReactI18next } from 'react-i18next'
import zh from './locales/zh.json'
import en from './locales/en.json'

// Detect saved language from localStorage, fallback to system language, then 'zh'
function detectLanguage(): string {
  const saved = localStorage.getItem('oc-claw-lang')
  if (saved && (saved === 'zh' || saved === 'en')) return saved
  const nav = navigator.language.toLowerCase()
  if (nav.startsWith('zh')) return 'zh'
  return 'en'
}

i18n.use(initReactI18next).init({
  resources: {
    zh: { translation: zh },
    en: { translation: en },
  },
  lng: detectLanguage(),
  fallbackLng: 'zh',
  interpolation: { escapeValue: false },
})

export default i18n

/** Change language and persist to localStorage */
export function setLanguage(lng: string) {
  i18n.changeLanguage(lng)
  localStorage.setItem('oc-claw-lang', lng)
}
