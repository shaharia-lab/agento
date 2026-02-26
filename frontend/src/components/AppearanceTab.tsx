import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useAppearance, FONT_OPTIONS } from '@/contexts/ThemeContext'
import { settingsApi } from '@/lib/api'

export default function AppearanceTab() {
  const { appearance, setAppearance } = useAppearance()
  const [saving, setSaving] = useState(false)
  const [toast, setToast] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 3000)
  }

  const handleDarkMode = (enabled: boolean) => {
    setAppearance({ ...appearance, darkMode: enabled })
  }

  const handleFontSize = (size: number) => {
    setAppearance({ ...appearance, fontSize: size })
  }

  const handleFontFamily = (family: string) => {
    setAppearance({ ...appearance, fontFamily: family })
  }

  const handleSave = async () => {
    setSaving(true)
    setError(null)
    try {
      const current = await settingsApi.get()
      await settingsApi.update({
        ...current.settings,
        appearance_dark_mode: appearance.darkMode,
        appearance_font_size: appearance.fontSize,
        appearance_font_family: appearance.fontFamily,
      })
      showToast('Appearance saved')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save appearance')
    } finally {
      setSaving(false)
    }
  }

  const previewFontCss =
    FONT_OPTIONS.find(f => f.value === appearance.fontFamily)?.css ?? FONT_OPTIONS[0].css

  return (
    <div className="max-w-md flex flex-col gap-6">
      <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100 mb-0">Appearance</h2>

      {/* Dark Mode */}
      <div className="flex flex-col gap-1.5">
        <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">Dark Mode</Label>
        <div className="flex items-center gap-3">
          <button
            role="switch"
            aria-checked={appearance.darkMode}
            onClick={() => handleDarkMode(!appearance.darkMode)}
            className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-zinc-500 ${
              appearance.darkMode ? 'bg-zinc-800' : 'bg-zinc-300'
            }`}
          >
            <span
              className={`inline-block h-4 w-4 rounded-full bg-white shadow transition-transform ${
                appearance.darkMode ? 'translate-x-6' : 'translate-x-1'
              }`}
            />
          </button>
          <span className="text-sm text-zinc-600 dark:text-zinc-400">
            {appearance.darkMode ? 'Enabled' : 'Disabled'}
          </span>
        </div>
      </div>

      {/* Font Size */}
      <div className="flex flex-col gap-1.5">
        <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
          Font Size: {appearance.fontSize}px
        </Label>
        <input
          type="range"
          min={12}
          max={24}
          step={1}
          value={appearance.fontSize}
          onChange={e => handleFontSize(Number(e.target.value))}
          className="w-full h-2 bg-zinc-200 dark:bg-zinc-700 rounded-lg appearance-none cursor-pointer accent-zinc-800 dark:accent-zinc-300"
        />
        <div className="flex justify-between text-xs text-zinc-400">
          <span>12px</span>
          <span>24px</span>
        </div>
      </div>

      {/* Font Family */}
      <div className="flex flex-col gap-1.5">
        <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">Font Family</Label>
        <Select value={appearance.fontFamily} onValueChange={handleFontFamily}>
          <SelectTrigger className="w-full">
            <SelectValue placeholder="Select a font" />
          </SelectTrigger>
          <SelectContent>
            {FONT_OPTIONS.map(f => (
              <SelectItem key={f.value} value={f.value}>
                {f.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {/* Live Preview */}
      <div
        className="rounded-md border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/50 p-4"
        style={{ fontFamily: previewFontCss, fontSize: `${appearance.fontSize}px` }}
      >
        <p className="font-semibold text-zinc-900 dark:text-zinc-100 mb-1">Preview</p>
        <p className="text-zinc-600 dark:text-zinc-400">
          The quick brown fox jumps over the lazy dog. 0123456789
        </p>
        <p className="text-zinc-500 dark:text-zinc-500 text-[0.85em] mt-1">
          AaBbCcDdEeFfGgHhIiJjKkLlMmNnOoPpQqRrSsTtUuVvWwXxYyZz
        </p>
      </div>

      {error && (
        <div className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
          {error}
        </div>
      )}

      <Button
        className="bg-zinc-900 hover:bg-zinc-800 text-white dark:bg-zinc-100 dark:hover:bg-zinc-200 dark:text-zinc-900 w-full sm:w-auto"
        onClick={() => void handleSave()}
        disabled={saving}
      >
        {saving ? 'Saving…' : 'Save Appearance Settings'}
      </Button>

      {toast && (
        <div className="fixed bottom-4 right-4 z-50 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm shadow-lg">
          {toast}
        </div>
      )}
    </div>
  )
}
