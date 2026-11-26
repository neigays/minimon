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

/** 顶栏颜色（--surface），用于浏览器 / 独立 App 的状态栏与标题栏。 */
const SURFACE: Record<Theme, string> = { light: '#ffffff', dark: '#1b2336' }

/** 两个 theme-color meta（按系统浅 / 深色）应取的值：手动选过主题时两者都跟随手动选择。 */
export function themeColors(override: Theme | null): { light: string; dark: string } {
  return override ? { light: SURFACE[override], dark: SURFACE[override] } : { light: SURFACE.light, dark: SURFACE.dark }
}

function applyThemeColor(override: Theme | null): void {
  const c = themeColors(override)
  document.querySelectorAll<HTMLMetaElement>('meta[name="theme-color"]').forEach((m) => {
    m.content = m.media.includes('dark') ? c.dark : c.light
  })
}

export function initTheme(): void {
  const s = stored()
  if (s) document.documentElement.dataset.theme = s
  theme.value = s ?? systemTheme()
  applyThemeColor(s)
}

export function toggleTheme(): void {
  const next: Theme = theme.value === 'dark' ? 'light' : 'dark'
  theme.value = next
  document.documentElement.dataset.theme = next
  applyThemeColor(next)
  try {
    localStorage.setItem(KEY, next)
  } catch {
    // 无法持久化时仅本次生效
  }
}
