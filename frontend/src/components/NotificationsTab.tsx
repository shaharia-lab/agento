import { useState, useEffect, useCallback } from 'react'
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
import { notificationsApi } from '@/lib/api'
import type { NotificationSettings, NotificationLogEntry } from '@/types'

const defaultSettings: NotificationSettings = {
  enabled: false,
  provider: {
    host: '',
    port: 587,
    username: '',
    password: '',
    from_address: '',
    to_addresses: '',
    encryption: 'starttls',
  },
  preferences: {
    scheduled_tasks: {
      on_finished: undefined,
      on_failed: undefined,
    },
  },
}

// Returns the effective value of an optional preference (nil/undefined → true).
function prefValue(v: boolean | undefined): boolean {
  return v !== false
}

interface ToggleProps {
  checked: boolean
  onChange: (v: boolean) => void
  label: string
  description?: string
}

function PreferenceToggle({ checked, onChange, label, description }: ToggleProps) {
  return (
    <div className="flex items-start justify-between gap-4 py-2">
      <div className="flex flex-col gap-0.5 min-w-0">
        <span className="text-sm font-medium text-zinc-700 dark:text-zinc-300">{label}</span>
        {description && (
          <span className="text-xs text-zinc-400 dark:text-zinc-500">{description}</span>
        )}
      </div>
      <button
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
        className={`relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-zinc-500 ${
          checked ? 'bg-zinc-800 dark:bg-zinc-300' : 'bg-zinc-300 dark:bg-zinc-600'
        }`}
      >
        <span
          className={`inline-block h-3.5 w-3.5 rounded-full bg-white dark:bg-zinc-900 shadow transition-transform ${
            checked ? 'translate-x-5' : 'translate-x-0.5'
          }`}
        />
      </button>
    </div>
  )
}

export default function NotificationsTab() {
  const [settings, setSettings] = useState<NotificationSettings>(defaultSettings)
  const [log, setLog] = useState<NotificationLogEntry[]>([])
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [testing, setTesting] = useState(false)
  const [toast, setToast] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 3000)
  }

  const load = useCallback(async () => {
    try {
      const [ns, entries] = await Promise.all([
        notificationsApi.getSettings(),
        notificationsApi.listLog(50),
      ])
      setSettings(ns)
      setLog(entries ?? [])
    } catch {
      setError('Failed to load notification settings')
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
      const updated = await notificationsApi.updateSettings(settings)
      setSettings(updated)
      showToast('Notification settings saved')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save settings')
    } finally {
      setSaving(false)
    }
  }

  const handleTest = async () => {
    setTesting(true)
    setError(null)
    try {
      await notificationsApi.sendTest()
      showToast('Test email sent successfully')
      const entries = await notificationsApi.listLog(50)
      setLog(entries ?? [])
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to send test email')
    } finally {
      setTesting(false)
    }
  }

  const updateProvider = (
    field: keyof NotificationSettings['provider'],
    value: string | number,
  ) => {
    setSettings(prev => ({
      ...prev,
      provider: { ...prev.provider, [field]: value },
    }))
  }

  const updateScheduledTasksPref = (field: 'on_finished' | 'on_failed', value: boolean) => {
    setSettings(prev => ({
      ...prev,
      preferences: {
        ...prev.preferences,
        scheduled_tasks: {
          ...prev.preferences?.scheduled_tasks,
          [field]: value,
        },
      },
    }))
  }

  const scheduledTasksPrefs = settings.preferences?.scheduled_tasks
  const onFinished = prefValue(scheduledTasksPrefs?.on_finished)
  const onFailed = prefValue(scheduledTasksPrefs?.on_failed)

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="text-sm text-zinc-400">Loading…</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-6">
      <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">Notifications</h2>

      {/* Two-column layout: SMTP config | Preferences */}
      <div className="grid grid-cols-1 gap-6 lg:grid-cols-2 lg:items-start">
        {/* ── Left column: SMTP + enable toggle ── */}
        <div className="flex flex-col gap-4">
          {/* Enable toggle */}
          <div className="flex flex-col gap-1.5">
            <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
              Enable Notifications
            </Label>
            <div className="flex items-center gap-3">
              <button
                role="switch"
                aria-checked={settings.enabled}
                onClick={() => setSettings(prev => ({ ...prev, enabled: !prev.enabled }))}
                className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-zinc-500 ${
                  settings.enabled ? 'bg-zinc-800 dark:bg-zinc-300' : 'bg-zinc-300 dark:bg-zinc-600'
                }`}
              >
                <span
                  className={`inline-block h-4 w-4 rounded-full bg-white dark:bg-zinc-900 shadow transition-transform ${
                    settings.enabled ? 'translate-x-6' : 'translate-x-1'
                  }`}
                />
              </button>
              <span className="text-sm text-zinc-600 dark:text-zinc-400">
                {settings.enabled ? 'Enabled' : 'Disabled'}
              </span>
            </div>
          </div>

          {/* SMTP Configuration */}
          <fieldset className="flex flex-col gap-4 rounded-md border border-zinc-200 dark:border-zinc-700 p-4">
            <legend className="px-1 text-xs font-medium text-zinc-500 dark:text-zinc-400">
              SMTP Configuration
            </legend>

            <div className="grid grid-cols-2 gap-3">
              <div className="flex flex-col gap-1.5">
                <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">Host</Label>
                <Input
                  value={settings.provider.host}
                  onChange={e => updateProvider('host', e.target.value)}
                  placeholder="smtp.example.com"
                  className="font-mono text-sm"
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">Port</Label>
                <Input
                  type="number"
                  value={settings.provider.port}
                  onChange={e => updateProvider('port', Number(e.target.value))}
                  placeholder="587"
                  className="font-mono text-sm"
                />
              </div>
            </div>

            <div className="flex flex-col gap-1.5">
              <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
                Encryption
              </Label>
              <Select
                value={settings.provider.encryption}
                onValueChange={v => updateProvider('encryption', v)}
              >
                <SelectTrigger className="w-full">
                  <SelectValue placeholder="Select encryption" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="none">None</SelectItem>
                  <SelectItem value="starttls">STARTTLS</SelectItem>
                  <SelectItem value="ssl_tls">SSL/TLS</SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div className="flex flex-col gap-1.5">
              <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
                Username
              </Label>
              <Input
                value={settings.provider.username}
                onChange={e => updateProvider('username', e.target.value)}
                placeholder="user@example.com"
                autoComplete="username"
                className="text-sm"
              />
            </div>

            <div className="flex flex-col gap-1.5">
              <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
                Password
              </Label>
              <Input
                type="password"
                value={settings.provider.password}
                onChange={e => updateProvider('password', e.target.value)}
                placeholder="Leave as *** to keep existing"
                autoComplete="current-password"
                className="font-mono text-sm"
              />
              <p className="text-xs text-zinc-400">
                The password is stored securely. Leave as *** to preserve the existing value.
              </p>
            </div>

            <div className="flex flex-col gap-1.5">
              <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
                From Address
              </Label>
              <Input
                value={settings.provider.from_address}
                onChange={e => updateProvider('from_address', e.target.value)}
                placeholder="agento@example.com"
                className="text-sm"
              />
            </div>

            <div className="flex flex-col gap-1.5">
              <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
                To Addresses
              </Label>
              <Input
                value={settings.provider.to_addresses}
                onChange={e => updateProvider('to_addresses', e.target.value)}
                placeholder="you@example.com, team@example.com"
                className="text-sm"
              />
              <p className="text-xs text-zinc-400">
                Comma-separated list of recipient email addresses.
              </p>
            </div>
          </fieldset>

          {error && (
            <div className="rounded-md border border-red-200 bg-red-50 dark:border-red-800 dark:bg-red-900/20 px-3 py-2 text-sm text-red-700 dark:text-red-400">
              {error}
            </div>
          )}

          <div className="flex gap-2">
            <Button
              className="bg-zinc-900 hover:bg-zinc-800 text-white dark:bg-zinc-100 dark:hover:bg-zinc-200 dark:text-zinc-900"
              onClick={handleSave}
              disabled={saving}
            >
              {saving ? 'Saving…' : 'Save Settings'}
            </Button>
            <Button variant="outline" onClick={handleTest} disabled={testing}>
              {testing ? 'Sending…' : 'Send Test Email'}
            </Button>
          </div>
        </div>

        {/* ── Right column: Notification Preferences ── */}
        <div className="flex flex-col gap-4">
          <fieldset className="flex flex-col gap-2 rounded-md border border-zinc-200 dark:border-zinc-700 p-4">
            <legend className="px-1 text-xs font-medium text-zinc-500 dark:text-zinc-400">
              Notification Preferences
            </legend>
            <p className="text-xs text-zinc-400 dark:text-zinc-500 mb-2">
              Choose which events trigger a notification email. Applies only when notifications are
              enabled and SMTP is configured.
            </p>

            {/* Scheduled Tasks section */}
            <div className="flex flex-col gap-0.5">
              <span className="text-xs font-semibold text-zinc-500 dark:text-zinc-400 uppercase tracking-wide mb-1">
                Scheduled Tasks
              </span>
              <div className="divide-y divide-zinc-100 dark:divide-zinc-700/50">
                <PreferenceToggle
                  label="When scheduled task finished"
                  description="Send an email when a scheduled task completes successfully."
                  checked={onFinished}
                  onChange={v => updateScheduledTasksPref('on_finished', v)}
                />
                <PreferenceToggle
                  label="When scheduled task failed"
                  description="Send an email when a scheduled task fails, including the error details."
                  checked={onFailed}
                  onChange={v => updateScheduledTasksPref('on_failed', v)}
                />
              </div>
            </div>
          </fieldset>
        </div>
      </div>

      {/* ── Delivery Log (full width) ── */}
      {log.length > 0 && (
        <div className="flex flex-col gap-2">
          <h3 className="text-xs font-semibold text-zinc-500 dark:text-zinc-400 uppercase tracking-wide">
            Recent Delivery Log
          </h3>
          <div className="rounded-md border border-zinc-200 dark:border-zinc-700 divide-y divide-zinc-100 dark:divide-zinc-700/50">
            {log.map(entry => (
              <div key={entry.id} className="px-3 py-2 flex items-start justify-between gap-3">
                <div className="flex flex-col gap-0.5 min-w-0">
                  <span className="text-xs font-mono text-zinc-700 dark:text-zinc-300 truncate">
                    {entry.event_type}
                  </span>
                  {entry.error_msg && (
                    <span className="text-xs text-red-600 dark:text-red-400 truncate">
                      {entry.error_msg}
                    </span>
                  )}
                </div>
                <div className="flex items-center gap-2 shrink-0">
                  <span
                    className={`text-xs px-1.5 py-0.5 rounded-full font-medium ${
                      entry.status === 'sent'
                        ? 'bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400'
                        : 'bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400'
                    }`}
                  >
                    {entry.status}
                  </span>
                  <span className="text-xs text-zinc-400">
                    {new Date(entry.created_at).toLocaleString()}
                  </span>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {toast && (
        <div className="fixed bottom-4 right-4 z-50 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm shadow-lg">
          {toast}
        </div>
      )}
    </div>
  )
}
