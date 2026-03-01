import { useState, useEffect, useCallback } from 'react'
import { FolderOpen, Lock } from 'lucide-react'
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
import { Tooltip } from '@/components/ui/tooltip'
import FilesystemBrowserModal from '@/components/FilesystemBrowserModal'
import ClaudeSettingsTab from '@/components/ClaudeSettingsTab'
import AppearanceTab from '@/components/AppearanceTab'
import NotificationsTab from '@/components/NotificationsTab'
import AdvancedTab from '@/components/AdvancedTab'
import { settingsApi } from '@/lib/api'
import type { SettingsResponse } from '@/types'
import { MODELS } from '@/types'

type Tab = 'general' | 'claude' | 'appearance' | 'notifications' | 'advanced'

export default function SettingsPage() {
  const [activeTab, setActiveTab] = useState<Tab>('general')

  const [resp, setResp] = useState<SettingsResponse | null>(null)
  const [loading, setLoading] = useState(true)
  const [workingDir, setWorkingDir] = useState('')
  const [model, setModel] = useState('')
  const [browserOpen, setBrowserOpen] = useState(false)
  const [saving, setSaving] = useState(false)
  const [toast, setToast] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      const data = await settingsApi.get()
      setResp(data)
      setWorkingDir(data.settings.default_working_dir)
      setModel(data.settings.default_model)
    } catch {
      setError('Failed to load settings')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    load()
  }, [load])

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 3000)
  }

  const handleSave = async () => {
    setSaving(true)
    setError(null)
    try {
      const updated = await settingsApi.update({
        default_working_dir: workingDir,
        default_model: model,
        onboarding_complete: resp?.settings.onboarding_complete ?? true,
      })
      setResp(updated)
      showToast('Settings saved')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save settings')
    } finally {
      setSaving(false)
    }
  }

  const locked = resp?.locked ?? {}
  const wdirLocked = 'default_working_dir' in locked
  const modelLocked = 'default_model' in locked

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400">Loading…</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="border-b border-zinc-100 dark:border-zinc-700/50 px-4 sm:px-6 py-4 shrink-0">
        <h1 className="text-base font-semibold text-zinc-900 dark:text-zinc-100">Settings</h1>
      </div>

      <div className="flex flex-1 overflow-hidden">
        {/* Left tab sidebar */}
        <nav className="w-44 shrink-0 border-r border-zinc-100 dark:border-zinc-700/50 py-3 px-2 flex flex-col gap-1">
          <button
            className={`flex w-full items-center gap-2 rounded-md px-3 py-2 text-sm transition-colors ${
              activeTab === 'general'
                ? 'bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900'
                : 'text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-900 dark:hover:text-zinc-100'
            }`}
            onClick={() => setActiveTab('general')}
          >
            General
          </button>
          <button
            className={`flex w-full items-center gap-2 rounded-md px-3 py-2 text-sm transition-colors ${
              activeTab === 'claude'
                ? 'bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900'
                : 'text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-900 dark:hover:text-zinc-100'
            }`}
            onClick={() => setActiveTab('claude')}
          >
            Claude Settings
          </button>
          <button
            className={`flex w-full items-center gap-2 rounded-md px-3 py-2 text-sm transition-colors ${
              activeTab === 'appearance'
                ? 'bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900'
                : 'text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-900 dark:hover:text-zinc-100'
            }`}
            onClick={() => setActiveTab('appearance')}
          >
            Appearance
          </button>
          <button
            className={`flex w-full items-center gap-2 rounded-md px-3 py-2 text-sm transition-colors ${
              activeTab === 'notifications'
                ? 'bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900'
                : 'text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-900 dark:hover:text-zinc-100'
            }`}
            onClick={() => setActiveTab('notifications')}
          >
            Notifications
          </button>
          <button
            className={`flex w-full items-center gap-2 rounded-md px-3 py-2 text-sm transition-colors ${
              activeTab === 'advanced'
                ? 'bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900'
                : 'text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-900 dark:hover:text-zinc-100'
            }`}
            onClick={() => setActiveTab('advanced')}
          >
            Advanced
          </button>
        </nav>

        {/* Content */}
        <div className="flex-1 overflow-y-auto px-6 py-6">
          {activeTab === 'general' && (
            <>
              <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100 mb-6">
                General Settings
              </h2>

              <div className="max-w-md flex flex-col gap-6">
                {/* Working Directory */}
                <div className="flex flex-col gap-1.5">
                  <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
                    Working Directory
                  </Label>
                  <div className="flex gap-2">
                    <Input
                      value={workingDir}
                      onChange={e => setWorkingDir(e.target.value)}
                      disabled={wdirLocked}
                      className="flex-1 font-mono text-sm"
                      placeholder="Default working directory"
                    />
                    {!wdirLocked && (
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => setBrowserOpen(true)}
                        className="shrink-0 gap-1.5"
                      >
                        <FolderOpen className="h-3.5 w-3.5" />
                        Browse
                      </Button>
                    )}
                  </div>
                  {wdirLocked && <LockedNote envVar={locked['default_working_dir']} />}
                </div>

                {/* Default Model */}
                <div className="flex flex-col gap-1.5">
                  <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
                    Default Model
                  </Label>
                  {modelLocked ? (
                    <div className="flex items-center gap-2">
                      <Input value={model} disabled className="flex-1 font-mono text-sm" />
                      <LockedNote envVar={locked['default_model']} inline />
                    </div>
                  ) : (
                    <Select value={model} onValueChange={setModel}>
                      <SelectTrigger className="w-full">
                        <SelectValue placeholder="Select a model" />
                      </SelectTrigger>
                      <SelectContent>
                        {MODELS.map(m => (
                          <SelectItem key={m.value} value={m.value}>
                            {m.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  )}
                  {modelLocked && <LockedNote envVar={locked['default_model']} />}
                </div>

                {error && (
                  <div className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
                    {error}
                  </div>
                )}

                <Button
                  className="bg-zinc-900 hover:bg-zinc-800 text-white w-full sm:w-auto"
                  onClick={() => handleSave()}
                  disabled={saving}
                >
                  {saving ? 'Saving…' : 'Save Settings'}
                </Button>
              </div>
            </>
          )}

          {activeTab === 'claude' && <ClaudeSettingsTab />}

          {activeTab === 'appearance' && <AppearanceTab />}

          {activeTab === 'notifications' && <NotificationsTab />}

          {activeTab === 'advanced' && <AdvancedTab />}
        </div>
      </div>

      {/* Toast */}
      {toast && (
        <div className="fixed bottom-4 right-4 z-50 rounded-md bg-zinc-900 text-white px-4 py-2 text-sm shadow-lg">
          {toast}
        </div>
      )}

      <FilesystemBrowserModal
        open={browserOpen}
        onOpenChange={setBrowserOpen}
        initialPath={workingDir}
        onSelect={path => setWorkingDir(path)}
      />
    </div>
  )
}

function LockedNote({ envVar, inline = false }: Readonly<{ envVar: string; inline?: boolean }>) {
  const content = (
    <span className={`flex items-center gap-1 text-xs text-zinc-400 ${inline ? '' : 'mt-0.5'}`}>
      <Lock className="h-3 w-3" />
      Set via <code className="font-mono">{envVar}</code>
    </span>
  )

  return (
    <Tooltip content={`Remove the ${envVar} environment variable to edit this field.`}>
      {content}
    </Tooltip>
  )
}
