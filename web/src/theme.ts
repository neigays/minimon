import { ref } from 'vue'

const KEY = 'minimon-theme'
export type Theme = 'dark' | 'light'
/** 当前生效的主题（含跟随系统）。 */
export const theme = ref<Theme>('dark')

function stored(): Theme | null {
  try {
    const v = localStorage.getItem(KEY)
    return v === 'dark' || v === 'light' ? v : null
  } catch {
    return null
  }
}

function systemTheme(): Theme {
  try {
    return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark'
  } catch {
    return 'dark'
  }
}

export function initTheme(): void {
  const s = stored()
  if (s) document.documentElement.dataset.theme = s
  theme.value = s ?? systemTheme()
}

export function toggleTheme(): void {
  const next: Theme = theme.value === 'dark' ? 'light' : 'dark'
  theme.value = next
  document.documentElement.dataset.theme = next
  try {
    localStorage.setItem(KEY, next)
  } catch {
    // 无法持久化时仅本次生效
  }
}
