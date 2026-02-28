import { useState, useEffect, useCallback } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { settingsApi } from '@/lib/api'
import type { SettingsResponse } from '@/types'

export default function AdvancedTab() {
  const [resp, setResp] = useState<SettingsResponse | null>(null)
  const [workerPoolSize, setWorkerPoolSize] = useState(3)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [toast, setToast] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 3000)
  }

  const load = useCallback(async () => {
    try {
      const data = await settingsApi.get()
      setResp(data)
      setWorkerPoolSize(data.settings.event_bus_worker_pool_size ?? 3)
    } catch {
      setError('Failed to load advanced settings')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    load()
  }, [load])

  const handleSave = async () => {
    setSaving(true)
    setError(null)
    try {
      const updated = await settingsApi.update({
        ...resp?.settings,
        event_bus_worker_pool_size: workerPoolSize,
      })
      setResp(updated)
      showToast('Advanced settings saved')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save settings')
    } finally {
      setSaving(false)
    }
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="text-sm text-zinc-400">Loading…</div>
      </div>
    )
  }

  return (
    <div className="max-w-md flex flex-col gap-6">
      <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">Advanced</h2>

      {/* Event Bus Worker Pool */}
      <div className="flex flex-col gap-1.5">
        <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
          Event Bus Worker Pool Size
        </Label>
        <Input
          type="number"
          min={1}
          max={20}
          value={workerPoolSize}
          onChange={e => setWorkerPoolSize(Math.max(1, Number(e.target.value)))}
          className="w-32 font-mono text-sm"
        />
        <p className="text-xs text-zinc-400">
          Number of goroutines that process background events (e.g. notifications). Default: 3.
          Requires a server restart to take effect.
        </p>
      </div>

      {error && (
        <div className="rounded-md border border-red-200 bg-red-50 dark:border-red-800 dark:bg-red-900/20 px-3 py-2 text-sm text-red-700 dark:text-red-400">
          {error}
        </div>
      )}

      <Button
        className="bg-zinc-900 hover:bg-zinc-800 text-white dark:bg-zinc-100 dark:hover:bg-zinc-200 dark:text-zinc-900 w-full sm:w-auto"
        onClick={handleSave}
        disabled={saving}
      >
        {saving ? 'Saving…' : 'Save Advanced Settings'}
      </Button>

      {toast && (
        <div className="fixed bottom-4 right-4 z-50 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm shadow-lg">
          {toast}
        </div>
      )}
    </div>
  )
}
