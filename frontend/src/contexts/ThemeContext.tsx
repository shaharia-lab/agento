import { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react'
import type { ReactNode } from 'react'

export interface AppearanceSettings {
  darkMode: boolean
  fontSize: number
  fontFamily: string
}

const STORAGE_KEY = 'agento-appearance'

const DEFAULTS: AppearanceSettings = {
  darkMode: false,
  fontSize: 15,
  fontFamily: 'system',
}

export const FONT_OPTIONS = [
  {
    value: 'system',
    label: 'System Default',
    css: "-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif",
  },
  { value: 'inter', label: 'Inter', css: "'Inter', sans-serif" },
  { value: 'roboto', label: 'Roboto', css: "'Roboto', sans-serif" },
  { value: 'open-sans', label: 'Open Sans', css: "'Open Sans', sans-serif" },
  { value: 'lato', label: 'Lato', css: "'Lato', sans-serif" },
  { value: 'merriweather', label: 'Merriweather', css: "'Merriweather', serif" },
  { value: 'jetbrains-mono', label: 'JetBrains Mono', css: "'JetBrains Mono', monospace" },
]

function loadFromStorage(): AppearanceSettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (raw) {
      const p = JSON.parse(raw) as Partial<AppearanceSettings>
      return {
        darkMode: typeof p.darkMode === 'boolean' ? p.darkMode : DEFAULTS.darkMode,
        fontSize:
          p.fontSize && p.fontSize >= 12 && p.fontSize <= 24 ? p.fontSize : DEFAULTS.fontSize,
        fontFamily:
          p.fontFamily && FONT_OPTIONS.some(f => f.value === p.fontFamily)
            ? p.fontFamily
            : DEFAULTS.fontFamily,
      }
    }
  } catch {
    // ignore
  }
  return { ...DEFAULTS }
}

function saveToStorage(settings: AppearanceSettings) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(settings))
  } catch {
    // ignore
  }
}

function applyToDom(settings: AppearanceSettings) {
  const root = document.documentElement
  if (settings.darkMode) {
    root.classList.add('dark')
  } else {
    root.classList.remove('dark')
  }
  // Set font-size on html so rem-based Tailwind classes (text-sm, text-xs, etc.) scale with it
  root.style.fontSize = `${settings.fontSize}px`
  const fontOption = FONT_OPTIONS.find(f => f.value === settings.fontFamily)
  root.style.fontFamily = fontOption?.css ?? FONT_OPTIONS[0].css
}

interface ThemeContextValue {
  appearance: AppearanceSettings
  setAppearance: (settings: AppearanceSettings) => void
}

const ThemeContext = createContext<ThemeContextValue | null>(null)

interface AppearanceProviderProps {
  readonly children: ReactNode
  readonly serverSettings?: Partial<AppearanceSettings>
}

export function AppearanceProvider({ children, serverSettings }: AppearanceProviderProps) {
  const [appearance, setAppearance] = useState<AppearanceSettings>(() => {
    const local = loadFromStorage()
    return local
  })

  // On mount, apply DOM immediately from localStorage
  useEffect(() => {
    applyToDom(appearance)
  }, []) // eslint-disable-line react-hooks/exhaustive-deps

  // When server settings arrive, merge them (server wins, then update localStorage)
  useEffect(() => {
    if (!serverSettings || Object.keys(serverSettings).length === 0) return
    const merged = { ...appearance, ...serverSettings }
    saveToStorage(merged)
    applyToDom(merged)
    setAppearance(merged)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [serverSettings?.darkMode, serverSettings?.fontSize, serverSettings?.fontFamily])

  const updateAppearance = useCallback((settings: AppearanceSettings) => {
    applyToDom(settings)
    saveToStorage(settings)
    setAppearance(settings)
  }, [])

  const contextValue = useMemo(
    () => ({ appearance, setAppearance: updateAppearance }),
    [appearance, updateAppearance],
  )

  return <ThemeContext.Provider value={contextValue}>{children}</ThemeContext.Provider>
}

export function useAppearance(): ThemeContextValue {
  const ctx = useContext(ThemeContext)
  if (!ctx) throw new Error('useAppearance must be used inside AppearanceProvider')
  return ctx
}
