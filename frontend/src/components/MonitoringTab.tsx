import { useState, useEffect, useCallback } from 'react'
import { Lock, Plus, Trash2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { monitoringApi } from '@/lib/api'
import type { MonitoringConfig, MonitoringResponse } from '@/types'

const defaultConfig: MonitoringConfig = {
  enabled: false,
  metrics_exporter: 'none',
  logs_exporter: 'none',
  otlp_endpoint: '',
  otlp_headers: {},
  otlp_insecure: false,
  metric_export_interval_ms: 60000,
}

interface HeaderEntry {
  key: string
  value: string
}

function headersToEntries(headers: Record<string, string>): HeaderEntry[] {
  return Object.entries(headers).map(([key, value]) => ({ key, value }))
}

function entriesToHeaders(entries: HeaderEntry[]): Record<string, string> {
  const result: Record<string, string> = {}
  for (const { key, value } of entries) {
    if (key.trim() !== '') {
      result[key.trim()] = value
    }
  }
  return result
}

interface ToggleProps {
  readonly checked: boolean
  readonly onChange: (v: boolean) => void
  readonly disabled?: boolean
}

function Toggle({ checked, onChange, disabled }: ToggleProps) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => !disabled && onChange(!checked)}
      className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors
        focus:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-zinc-500
        ${disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer'}
        ${checked ? 'bg-zinc-800 dark:bg-zinc-300' : 'bg-zinc-300 dark:bg-zinc-600'}`}
    >
      <span
        className={`inline-block h-4 w-4 rounded-full bg-white dark:bg-zinc-900 shadow transition-transform ${
          checked ? 'translate-x-6' : 'translate-x-1'
        }`}
      />
    </button>
  )
}

export default function MonitoringTab() {
  const [resp, setResp] = useState<MonitoringResponse | null>(null)
  const [cfg, setCfg] = useState<MonitoringConfig>(defaultConfig)
  const [headerEntries, setHeaderEntries] = useState<HeaderEntry[]>([])
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
      const data = await monitoringApi.get()
      setResp(data)
      setCfg(data.settings)
      setHeaderEntries(headersToEntries(data.settings.otlp_headers ?? {}))
    } catch {
      setError('Failed to load monitoring settings')
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
      const payload: MonitoringConfig = {
        ...cfg,
        otlp_headers: entriesToHeaders(headerEntries),
      }
      const updated = await monitoringApi.update(payload)
      setResp(updated)
      setCfg(updated.settings)
      setHeaderEntries(headersToEntries(updated.settings.otlp_headers ?? {}))
      showToast('Monitoring settings saved')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save settings')
    } finally {
      setSaving(false)
    }
  }

  const addHeader = () => {
    setHeaderEntries(prev => [...prev, { key: '', value: '' }])
  }

  const removeHeader = (index: number) => {
    setHeaderEntries(prev => prev.filter((_, i) => i !== index))
  }

  const updateHeader = (index: number, field: 'key' | 'value', val: string) => {
    setHeaderEntries(prev =>
      prev.map((entry, i) => (i === index ? { ...entry, [field]: val } : entry)),
    )
  }

  const envLocked = resp?.env_locked ?? false
  const locked = resp?.locked ?? {}

  const showOtlpFields = cfg.metrics_exporter === 'otlp' || cfg.logs_exporter === 'otlp'

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="text-sm text-zinc-400">Loading…</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
        Monitoring &amp; Observability
      </h2>

      {/* Env-locked banner */}
      {envLocked && (
        <div
          className="flex items-start gap-3 rounded-md border border-amber-200 bg-amber-50
          dark:border-amber-800/50 dark:bg-amber-900/20 px-4 py-3"
        >
          <Lock className="h-4 w-4 text-amber-600 dark:text-amber-400 mt-0.5 shrink-0" />
          <div className="flex flex-col gap-1">
            <span className="text-sm font-medium text-amber-800 dark:text-amber-300">
              Configuration is managed via environment variables
            </span>
            <span className="text-xs text-amber-700 dark:text-amber-400">
              The following env vars are set and override UI changes:
            </span>
            <ul className="mt-1 flex flex-col gap-0.5">
              {Object.entries(locked).map(([field, envVar]) => (
                <li key={field} className="text-xs font-mono text-amber-700 dark:text-amber-400">
                  <span className="text-amber-500 dark:text-amber-500">{field}</span>
                  {' ← '}
                  <code className="font-semibold">{envVar}</code>
                </li>
              ))}
            </ul>
            <span className="text-xs text-amber-600 dark:text-amber-500 mt-1">
              Unset these environment variables to configure monitoring from the UI.
            </span>
          </div>
        </div>
      )}

      <div className="grid grid-cols-1 gap-6 lg:grid-cols-2 lg:items-start">
        {/* Left column: enable toggle + exporters */}
        <div className="flex flex-col gap-5">
          {/* Enable toggle */}
          <div className="flex flex-col gap-1.5">
            <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
              Enable Telemetry
            </Label>
            <div className="flex items-center gap-3">
              <Toggle
                checked={cfg.enabled}
                onChange={v => setCfg(prev => ({ ...prev, enabled: v }))}
                disabled={envLocked}
              />
              <span className="text-sm text-zinc-600 dark:text-zinc-400">
                {cfg.enabled ? 'Enabled' : 'Disabled'}
              </span>
              {'enabled' in locked && <LockedBadge envVar={locked['enabled']} />}
            </div>
          </div>

          {/* Metrics Exporter */}
          <div className="flex flex-col gap-1.5">
            <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
              Metrics Exporter
            </Label>
            <Select
              value={cfg.metrics_exporter}
              onValueChange={v =>
                setCfg(prev => ({
                  ...prev,
                  metrics_exporter: v as MonitoringConfig['metrics_exporter'],
                }))
              }
              disabled={envLocked}
            >
              <SelectTrigger className="w-full">
                <SelectValue placeholder="Select exporter" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">None (disabled)</SelectItem>
                <SelectItem value="otlp">OTLP (gRPC)</SelectItem>
                <SelectItem value="prometheus">Prometheus (pull)</SelectItem>
              </SelectContent>
            </Select>
            {cfg.metrics_exporter === 'prometheus' && (
              <p className="text-xs text-zinc-500 dark:text-zinc-400">
                Metrics are available at <code className="font-mono">/metrics</code>
              </p>
            )}
            {'metrics_exporter' in locked && <LockedBadge envVar={locked['metrics_exporter']} />}
          </div>

          {/* Logs Exporter */}
          <div className="flex flex-col gap-1.5">
            <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
              Logs Exporter
            </Label>
            <Select
              value={cfg.logs_exporter}
              onValueChange={v =>
                setCfg(prev => ({
                  ...prev,
                  logs_exporter: v as MonitoringConfig['logs_exporter'],
                }))
              }
              disabled={envLocked}
            >
              <SelectTrigger className="w-full">
                <SelectValue placeholder="Select exporter" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">None (disabled)</SelectItem>
                <SelectItem value="otlp">OTLP (gRPC)</SelectItem>
              </SelectContent>
            </Select>
            {'logs_exporter' in locked && <LockedBadge envVar={locked['logs_exporter']} />}
          </div>

          {/* Metric Export Interval */}
          <div className="flex flex-col gap-1.5">
            <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
              Metric Export Interval (ms)
            </Label>
            <Input
              type="number"
              min={1000}
              value={cfg.metric_export_interval_ms}
              onChange={e =>
                setCfg(prev => ({
                  ...prev,
                  metric_export_interval_ms: Number(e.target.value),
                }))
              }
              disabled={envLocked}
              className="font-mono text-sm"
              placeholder="60000"
            />
            <p className="text-xs text-zinc-400">
              How often metrics are pushed to the OTLP collector. Default: 60000 ms (1 minute).
            </p>
            {'metric_export_interval' in locked && (
              <LockedBadge envVar={locked['metric_export_interval']} />
            )}
          </div>
        </div>

        {/* Right column: OTLP connection details */}
        {showOtlpFields && (
          <fieldset className="flex flex-col gap-4 rounded-md border border-zinc-200 dark:border-zinc-700 p-4">
            <legend className="px-1 text-xs font-medium text-zinc-500 dark:text-zinc-400">
              OTLP Collector
            </legend>

            {/* Endpoint */}
            <div className="flex flex-col gap-1.5">
              <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
                Endpoint
              </Label>
              <Input
                value={cfg.otlp_endpoint}
                onChange={e => setCfg(prev => ({ ...prev, otlp_endpoint: e.target.value }))}
                disabled={envLocked}
                placeholder="localhost:4317"
                className="font-mono text-sm"
              />
              <p className="text-xs text-zinc-400">
                gRPC endpoint of the OTLP collector (host:port).
              </p>
              {'otlp_endpoint' in locked && <LockedBadge envVar={locked['otlp_endpoint']} />}
            </div>

            {/* Insecure toggle */}
            <div className="flex flex-col gap-1.5">
              <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
                Disable TLS
              </Label>
              <div className="flex items-center gap-3">
                <Toggle
                  checked={cfg.otlp_insecure}
                  onChange={v => setCfg(prev => ({ ...prev, otlp_insecure: v }))}
                  disabled={envLocked}
                />
                <span className="text-sm text-zinc-600 dark:text-zinc-400">
                  {cfg.otlp_insecure ? 'TLS disabled (insecure)' : 'TLS enabled'}
                </span>
              </div>
              {'otlp_insecure' in locked && <LockedBadge envVar={locked['otlp_insecure']} />}
            </div>

            {/* Headers */}
            <div className="flex flex-col gap-2">
              <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
                Headers
              </Label>
              <p className="text-xs text-zinc-400">
                Key-value pairs sent with every OTLP request (e.g. authentication tokens).
              </p>
              {headerEntries.map((entry, i) => (
                <div key={i} className="flex gap-2 items-center">
                  <Input
                    value={entry.key}
                    onChange={e => updateHeader(i, 'key', e.target.value)}
                    disabled={envLocked}
                    placeholder="Header name"
                    className="flex-1 font-mono text-sm"
                  />
                  <Input
                    value={entry.value}
                    onChange={e => updateHeader(i, 'value', e.target.value)}
                    disabled={envLocked}
                    placeholder="Value"
                    className="flex-1 font-mono text-sm"
                  />
                  {!envLocked && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => removeHeader(i)}
                      className="shrink-0 text-zinc-400 hover:text-red-500"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  )}
                </div>
              ))}
              {!envLocked && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={addHeader}
                  className="self-start gap-1.5 text-xs"
                >
                  <Plus className="h-3 w-3" />
                  Add Header
                </Button>
              )}
              {'otlp_headers' in locked && <LockedBadge envVar={locked['otlp_headers']} />}
            </div>
          </fieldset>
        )}
      </div>

      {error && (
        <div
          className="rounded-md border border-red-200 bg-red-50 dark:border-red-800
          dark:bg-red-900/20 px-3 py-2 text-sm text-red-700 dark:text-red-400"
        >
          {error}
        </div>
      )}

      {!envLocked && (
        <Button
          className="bg-zinc-900 hover:bg-zinc-800 text-white dark:bg-zinc-100
            dark:hover:bg-zinc-200 dark:text-zinc-900 self-start"
          onClick={handleSave}
          disabled={saving}
        >
          {saving ? 'Saving…' : 'Save Settings'}
        </Button>
      )}

      {toast && (
        <div
          className="fixed bottom-4 right-4 z-50 rounded-md bg-zinc-900 dark:bg-zinc-100
          text-white dark:text-zinc-900 px-4 py-2 text-sm shadow-lg"
        >
          {toast}
        </div>
      )}
    </div>
  )
}

function LockedBadge({ envVar }: Readonly<{ envVar: string }>) {
  return (
    <span className="flex items-center gap-1 text-xs text-zinc-400 mt-0.5">
      <Lock className="h-3 w-3" />
      Set via <code className="font-mono">{envVar}</code>
    </span>
  )
}
