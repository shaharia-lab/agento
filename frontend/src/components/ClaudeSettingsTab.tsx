import { useState, useEffect, useCallback, useRef } from 'react'
import {
  ChevronDown,
  ChevronRight,
  Copy,
  Check,
  AlertCircle,
  FilePlus,
  Plus,
  Star,
  Trash2,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { claudeSettingsApi, claudeSettingsProfilesApi } from '@/lib/api'
import type { ClaudeCodeSettings, ClaudeSettingsProfile } from '@/types'

// ─── Minimal toggle component ─────────────────────────────────────────────────

function Toggle({
  checked,
  onChange,
  disabled,
}: Readonly<{
  checked: boolean
  onChange: (val: boolean) => void
  disabled?: boolean
}>) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => !disabled && onChange(!checked)}
      className={`
        relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full
        transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-zinc-950
        disabled:cursor-not-allowed disabled:opacity-50
        ${checked ? 'bg-zinc-900' : 'bg-zinc-200'}
      `}
    >
      <span
        className={`
          pointer-events-none inline-block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform
          ${checked ? 'translate-x-4' : 'translate-x-0.5'}
        `}
      />
    </button>
  )
}

// ─── Field row: label + control side by side ───────────────────────────────────

function FieldRow({
  label,
  description,
  children,
}: Readonly<{
  label: string
  description?: string
  children: React.ReactNode
}>) {
  return (
    <div className="flex items-start justify-between gap-4 py-3 border-b border-zinc-100 last:border-b-0">
      <div className="flex flex-col gap-0.5 min-w-0 flex-1">
        <Label className="text-sm font-medium text-zinc-800">{label}</Label>
        {description && <p className="text-xs text-zinc-400">{description}</p>}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  )
}

// ─── Collapsible section for complex JSON fields ───────────────────────────────

function CollapsibleSection({
  title,
  description,
  value,
  onChange,
  error,
}: Readonly<{
  title: string
  description: string
  value: string
  onChange: (v: string) => void
  error?: string
}>) {
  const [open, setOpen] = useState(false)

  return (
    <div className="border border-zinc-200 rounded-md overflow-hidden">
      <button
        type="button"
        className="flex w-full items-center justify-between px-4 py-3 text-sm font-medium text-zinc-800 bg-zinc-50 hover:bg-zinc-100 transition-colors"
        onClick={() => setOpen(o => !o)}
      >
        <span className="flex flex-col items-start gap-0.5 text-left">
          <span>{title}</span>
          <span className="text-xs font-normal text-zinc-400">{description}</span>
        </span>
        {open ? (
          <ChevronDown className="h-4 w-4 text-zinc-500 shrink-0" />
        ) : (
          <ChevronRight className="h-4 w-4 text-zinc-500 shrink-0" />
        )}
      </button>
      {open && (
        <div className="p-3 flex flex-col gap-1.5">
          <Textarea
            value={value}
            onChange={e => onChange(e.target.value)}
            className="font-mono text-xs min-h-[160px] resize-y"
            placeholder="{}"
            spellCheck={false}
          />
          {error && (
            <p className="text-xs text-red-600 flex items-center gap-1">
              <AlertCircle className="h-3 w-3 shrink-0" />
              {error}
            </p>
          )}
        </div>
      )}
    </div>
  )
}

// ─── JSON preview with copy button ────────────────────────────────────────────

function JsonPreview({ json }: Readonly<{ json: string }>) {
  const [copied, setCopied] = useState(false)

  const copy = async () => {
    await navigator.clipboard.writeText(json)
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }

  return (
    <div className="border border-zinc-200 rounded-md overflow-hidden">
      <div className="flex items-center justify-between px-4 py-2 bg-zinc-50 border-b border-zinc-200">
        <span className="text-xs font-medium text-zinc-600">
          JSON Preview (~/.claude/settings.json)
        </span>
        <button
          type="button"
          onClick={() => copy()}
          className="flex items-center gap-1.5 text-xs text-zinc-500 hover:text-zinc-900 transition-colors"
        >
          {copied ? (
            <>
              <Check className="h-3.5 w-3.5" />
              Copied
            </>
          ) : (
            <>
              <Copy className="h-3.5 w-3.5" />
              Copy
            </>
          )}
        </button>
      </div>
      <pre className="px-4 py-3 text-xs font-mono text-zinc-700 overflow-x-auto max-h-64 overflow-y-auto bg-white">
        {json}
      </pre>
    </div>
  )
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

function prettyJson(v: unknown): string {
  if (v === undefined || v === null) return '{}'
  try {
    return JSON.stringify(v, null, 2)
  } catch {
    return '{}'
  }
}

function parseJsonField(raw: string): { value: unknown; error?: string } {
  const trimmed = raw.trim()
  if (!trimmed || trimmed === '{}') return { value: undefined }
  try {
    return { value: JSON.parse(trimmed) }
  } catch {
    return { value: undefined, error: 'Invalid JSON' }
  }
}

// ─── Empty value helpers ──────────────────────────────────────────────────────

type SelectNone = ''

function strVal(v: string | undefined): string {
  return v ?? ''
}

// ─── Main component ───────────────────────────────────────────────────────────

export default function ClaudeSettingsTab() {
  const [loading, setLoading] = useState(true)
  const [exists, setExists] = useState(false)
  const [saving, setSaving] = useState(false)
  const [toast, setToast] = useState<string | null>(null)
  const [globalError, setGlobalError] = useState<string | null>(null)

  // ── Profile state ────────────────────────────────────────────────────────────
  const [profiles, setProfiles] = useState<ClaudeSettingsProfile[]>([])
  const [activeProfileId, setActiveProfileId] = useState<string>('')
  const [profileLoading, setProfileLoading] = useState(false)

  // ── Simple form fields ───────────────────────────────────────────────────────
  const [model, setModel] = useState('')
  const [language, setLanguage] = useState('')
  const [effortLevel, setEffortLevel] = useState<'low' | 'medium' | 'high' | SelectNone>('')
  const [autoUpdatesChannel, setAutoUpdatesChannel] = useState<'stable' | 'latest' | SelectNone>('')
  const [outputStyle, setOutputStyle] = useState('')
  const [cleanupPeriodDays, setCleanupPeriodDays] = useState('')
  const [plansDirectory, setPlansDirectory] = useState('')
  const [apiKeyHelper, setApiKeyHelper] = useState('')

  // Toggles — undefined means "not present in the file"; only written when explicitly set.
  // This preserves explicit false values and keeps the file clean when nothing was changed.
  const [fastMode, setFastMode] = useState<boolean | undefined>(undefined)
  const [showTurnDuration, setShowTurnDuration] = useState<boolean | undefined>(undefined)
  const [spinnerTipsEnabled, setSpinnerTipsEnabled] = useState<boolean | undefined>(undefined)
  const [terminalProgressBarEnabled, setTerminalProgressBarEnabled] = useState<boolean | undefined>(
    undefined,
  )
  const [prefersReducedMotion, setPrefersReducedMotion] = useState<boolean | undefined>(undefined)
  const [alwaysThinkingEnabled, setAlwaysThinkingEnabled] = useState<boolean | undefined>(undefined)
  // respectGitignore defaults to true in Claude Code; undefined renders as checked (on).
  const [respectGitignore, setRespectGitignore] = useState<boolean | undefined>(undefined)
  const [skipWebFetchPreflight, setSkipWebFetchPreflight] = useState<boolean | undefined>(undefined)
  const [disableAllHooks, setDisableAllHooks] = useState<boolean | undefined>(undefined)
  const [enableAllProjectMcpServers, setEnableAllProjectMcpServers] = useState<boolean | undefined>(
    undefined,
  )
  const [allowManagedHooksOnly, setAllowManagedHooksOnly] = useState<boolean | undefined>(undefined)
  const [allowManagedPermissionRulesOnly, setAllowManagedPermissionRulesOnly] = useState<
    boolean | undefined
  >(undefined)
  const [allowManagedMcpServersOnly, setAllowManagedMcpServersOnly] = useState<boolean | undefined>(
    undefined,
  )

  const [teammateMode, setTeammateMode] = useState<'auto' | 'in-process' | 'tmux' | SelectNone>('')

  // ── Complex JSON fields ──────────────────────────────────────────────────────
  const [permissionsJson, setPermissionsJson] = useState('')
  const [hooksJson, setHooksJson] = useState('')
  const [envJson, setEnvJson] = useState('')
  const [sandboxJson, setSandboxJson] = useState('')
  const [attributionJson, setAttributionJson] = useState('')

  // Per-field JSON errors
  const [jsonErrors, setJsonErrors] = useState<Record<string, string>>({})

  // ── Track any "extra" keys from the original file so we don't lose them ──────
  const extraKeysRef = useRef<Record<string, unknown>>({})

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 3000)
  }

  // ─── Populate form from loaded settings ─────────────────────────────────────

  const applySettings = useCallback((s: ClaudeCodeSettings) => {
    setModel(strVal(s.model))
    setLanguage(strVal(s.language))
    setEffortLevel((s.effortLevel ?? '') as 'low' | 'medium' | 'high' | SelectNone)
    setAutoUpdatesChannel((s.autoUpdatesChannel ?? '') as 'stable' | 'latest' | SelectNone)
    setOutputStyle(strVal(s.outputStyle))
    setCleanupPeriodDays(s.cleanupPeriodDays !== undefined ? String(s.cleanupPeriodDays) : '')
    setPlansDirectory(strVal(s.plansDirectory))
    setApiKeyHelper(strVal(s.apiKeyHelper))

    // Load the raw value from the file. undefined means the field was absent — it will
    // not be written back on save, keeping the file clean.
    setFastMode(s.fastMode)
    setShowTurnDuration(s.showTurnDuration)
    setSpinnerTipsEnabled(s.spinnerTipsEnabled)
    setTerminalProgressBarEnabled(s.terminalProgressBarEnabled)
    setPrefersReducedMotion(s.prefersReducedMotion)
    setAlwaysThinkingEnabled(s.alwaysThinkingEnabled)
    setRespectGitignore(s.respectGitignore)
    setSkipWebFetchPreflight(s.skipWebFetchPreflight)
    setDisableAllHooks(s.disableAllHooks)
    setEnableAllProjectMcpServers(s.enableAllProjectMcpServers)
    setAllowManagedHooksOnly(s.allowManagedHooksOnly)
    setAllowManagedPermissionRulesOnly(s.allowManagedPermissionRulesOnly)
    setAllowManagedMcpServersOnly(s.allowManagedMcpServersOnly)
    setTeammateMode((s.teammateMode ?? '') as 'auto' | 'in-process' | 'tmux' | SelectNone)

    setPermissionsJson(s.permissions ? prettyJson(s.permissions) : '')
    setHooksJson(s.hooks ? prettyJson(s.hooks) : '')
    setEnvJson(s.env ? prettyJson(s.env) : '')
    setSandboxJson(s.sandbox ? prettyJson(s.sandbox) : '')
    setAttributionJson(s.attribution ? prettyJson(s.attribution) : '')

    // Preserve any keys we don't explicitly handle so they survive a round-trip.
    const knownKeys = new Set([
      '$schema',
      'model',
      'language',
      'effortLevel',
      'autoUpdatesChannel',
      'outputStyle',
      'cleanupPeriodDays',
      'plansDirectory',
      'apiKeyHelper',
      'fastMode',
      'showTurnDuration',
      'spinnerTipsEnabled',
      'terminalProgressBarEnabled',
      'prefersReducedMotion',
      'alwaysThinkingEnabled',
      'respectGitignore',
      'skipWebFetchPreflight',
      'disableAllHooks',
      'enableAllProjectMcpServers',
      'allowManagedHooksOnly',
      'allowManagedPermissionRulesOnly',
      'allowManagedMcpServersOnly',
      'teammateMode',
      'permissions',
      'hooks',
      'env',
      'sandbox',
      'attribution',
    ])
    const extra: Record<string, unknown> = {}
    for (const [k, v] of Object.entries(s)) {
      if (!knownKeys.has(k)) extra[k] = v
    }
    extraKeysRef.current = extra
  }, [])

  // ─── Load ────────────────────────────────────────────────────────────────────

  const loadProfile = useCallback(
    async (profileId: string) => {
      setProfileLoading(true)
      try {
        const detail = await claudeSettingsProfilesApi.get(profileId)
        setExists(detail.exists)
        if (detail.exists && detail.settings) {
          applySettings(detail.settings as ClaudeCodeSettings)
        }
      } catch {
        setGlobalError('Failed to load profile settings')
      } finally {
        setProfileLoading(false)
      }
    },
    [applySettings],
  )

  const load = useCallback(async () => {
    try {
      const profileList = await claudeSettingsProfilesApi.list()
      setProfiles(profileList)
      const defaultProfile = profileList.find(p => p.is_default) ?? profileList[0]
      if (defaultProfile) {
        setActiveProfileId(defaultProfile.id)
        const detail = await claudeSettingsProfilesApi.get(defaultProfile.id)
        setExists(detail.exists)
        if (detail.exists && detail.settings) {
          applySettings(detail.settings as ClaudeCodeSettings)
        }
      } else {
        // No profiles yet — fall back to reading settings.json directly
        const resp = await claudeSettingsApi.get()
        setExists(resp.exists)
        if (resp.exists && resp.settings) {
          applySettings(resp.settings)
        }
      }
    } catch {
      setGlobalError('Failed to load Claude settings')
    } finally {
      setLoading(false)
    }
  }, [applySettings])

  useEffect(() => {
    load()
  }, [load])

  // ─── Build the full settings object from current form state ─────────────────

  const buildSettings = (): { settings: ClaudeCodeSettings; errors: Record<string, string> } => {
    const errors: Record<string, string> = {}

    const { value: permissions, error: permErr } = parseJsonField(permissionsJson)
    if (permErr) errors.permissions = permErr

    const { value: hooks, error: hooksErr } = parseJsonField(hooksJson)
    if (hooksErr) errors.hooks = hooksErr

    const { value: env, error: envErr } = parseJsonField(envJson)
    if (envErr) errors.env = envErr

    const { value: sandbox, error: sandboxErr } = parseJsonField(sandboxJson)
    if (sandboxErr) errors.sandbox = sandboxErr

    const { value: attribution, error: attrErr } = parseJsonField(attributionJson)
    if (attrErr) errors.attribution = attrErr

    const settings: ClaudeCodeSettings = {
      ...extraKeysRef.current,
    }

    // Only include fields that have a non-empty/non-default value.
    if (model) settings.model = model
    if (language) settings.language = language
    if (effortLevel) settings.effortLevel = effortLevel as 'low' | 'medium' | 'high'
    if (autoUpdatesChannel) settings.autoUpdatesChannel = autoUpdatesChannel as 'stable' | 'latest'
    if (outputStyle) settings.outputStyle = outputStyle
    if (cleanupPeriodDays !== '') {
      const n = Number.parseInt(cleanupPeriodDays, 10)
      if (!Number.isNaN(n)) settings.cleanupPeriodDays = n
    }
    if (plansDirectory) settings.plansDirectory = plansDirectory
    if (apiKeyHelper) settings.apiKeyHelper = apiKeyHelper

    // Only write booleans that were explicitly set (not undefined).
    // This preserves round-trip fidelity: explicit false values survive, and
    // untouched fields are never injected into the file.
    if (fastMode !== undefined) settings.fastMode = fastMode
    if (showTurnDuration !== undefined) settings.showTurnDuration = showTurnDuration
    if (spinnerTipsEnabled !== undefined) settings.spinnerTipsEnabled = spinnerTipsEnabled
    if (terminalProgressBarEnabled !== undefined)
      settings.terminalProgressBarEnabled = terminalProgressBarEnabled
    if (prefersReducedMotion !== undefined) settings.prefersReducedMotion = prefersReducedMotion
    if (alwaysThinkingEnabled !== undefined) settings.alwaysThinkingEnabled = alwaysThinkingEnabled
    if (respectGitignore !== undefined) settings.respectGitignore = respectGitignore
    if (skipWebFetchPreflight !== undefined) settings.skipWebFetchPreflight = skipWebFetchPreflight
    if (disableAllHooks !== undefined) settings.disableAllHooks = disableAllHooks
    if (enableAllProjectMcpServers !== undefined)
      settings.enableAllProjectMcpServers = enableAllProjectMcpServers
    if (allowManagedHooksOnly !== undefined) settings.allowManagedHooksOnly = allowManagedHooksOnly
    if (allowManagedPermissionRulesOnly !== undefined)
      settings.allowManagedPermissionRulesOnly = allowManagedPermissionRulesOnly
    if (allowManagedMcpServersOnly !== undefined)
      settings.allowManagedMcpServersOnly = allowManagedMcpServersOnly
    if (teammateMode) settings.teammateMode = teammateMode as 'auto' | 'in-process' | 'tmux'

    if (permissions !== undefined)
      settings.permissions = permissions as ClaudeCodeSettings['permissions']
    if (hooks !== undefined) settings.hooks = hooks as Record<string, unknown>
    if (env !== undefined) settings.env = env as Record<string, string>
    if (sandbox !== undefined) settings.sandbox = sandbox as Record<string, unknown>
    if (attribution !== undefined)
      settings.attribution = attribution as ClaudeCodeSettings['attribution']

    return { settings, errors }
  }

  // ─── Live JSON preview ───────────────────────────────────────────────────────

  const { settings: previewSettings } = buildSettings()
  const previewJson = prettyJson(previewSettings)

  // ─── Save ────────────────────────────────────────────────────────────────────

  const handleSave = async () => {
    const { settings, errors } = buildSettings()
    setJsonErrors(errors)
    if (Object.keys(errors).length > 0) return

    setSaving(true)
    setGlobalError(null)
    try {
      if (activeProfileId) {
        const detail = await claudeSettingsProfilesApi.update(activeProfileId, { settings })
        setExists(detail.exists)
        if (detail.settings) applySettings(detail.settings as ClaudeCodeSettings)
      } else {
        const resp = await claudeSettingsApi.update(settings)
        setExists(resp.exists)
        if (resp.settings) applySettings(resp.settings)
      }
      showToast('Claude settings saved')
    } catch (err) {
      setGlobalError(err instanceof Error ? err.message : 'Failed to save Claude settings')
    } finally {
      setSaving(false)
    }
  }

  // ─── Create file ─────────────────────────────────────────────────────────────

  const handleCreate = async () => {
    setSaving(true)
    setGlobalError(null)
    try {
      const resp = await claudeSettingsApi.update({})
      setExists(resp.exists)
      // Reload profiles after creating the file so the default profile picks it up.
      const profileList = await claudeSettingsProfilesApi.list()
      setProfiles(profileList)
      showToast('Created ~/.claude/settings.json')
    } catch (err) {
      setGlobalError(err instanceof Error ? err.message : 'Failed to create settings file')
    } finally {
      setSaving(false)
    }
  }

  // ─── Profile actions ──────────────────────────────────────────────────────────

  const handleProfileSwitch = async (profileId: string) => {
    setActiveProfileId(profileId)
    await loadProfile(profileId)
  }

  const handleNewProfile = async () => {
    const name = globalThis.prompt('Profile name:')
    if (!name?.trim()) return
    setProfileLoading(true)
    try {
      const newProfile = await claudeSettingsProfilesApi.create(name.trim())
      const profileList = await claudeSettingsProfilesApi.list()
      setProfiles(profileList)
      setActiveProfileId(newProfile.id)
      await loadProfile(newProfile.id)
      showToast(`Profile "${newProfile.name}" created`)
    } catch (err) {
      setGlobalError(err instanceof Error ? err.message : 'Failed to create profile')
    } finally {
      setProfileLoading(false)
    }
  }

  const handleDuplicateProfile = async () => {
    if (!activeProfileId) return
    setProfileLoading(true)
    try {
      const copy = await claudeSettingsProfilesApi.duplicate(activeProfileId)
      const profileList = await claudeSettingsProfilesApi.list()
      setProfiles(profileList)
      setActiveProfileId(copy.id)
      await loadProfile(copy.id)
      showToast(`Profile "${copy.name}" created`)
    } catch (err) {
      setGlobalError(err instanceof Error ? err.message : 'Failed to duplicate profile')
    } finally {
      setProfileLoading(false)
    }
  }

  const handleSetDefault = async () => {
    if (!activeProfileId) return
    setProfileLoading(true)
    try {
      await claudeSettingsProfilesApi.setDefault(activeProfileId)
      const profileList = await claudeSettingsProfilesApi.list()
      setProfiles(profileList)
      showToast('Default profile updated')
    } catch (err) {
      setGlobalError(err instanceof Error ? err.message : 'Failed to set default profile')
    } finally {
      setProfileLoading(false)
    }
  }

  const handleDeleteProfile = async () => {
    if (!activeProfileId) return
    const profile = profiles.find(p => p.id === activeProfileId)
    if (profile?.is_default) return
    if (!globalThis.confirm(`Delete profile "${profile?.name}"? This cannot be undone.`)) return
    setProfileLoading(true)
    try {
      await claudeSettingsProfilesApi.delete(activeProfileId)
      const profileList = await claudeSettingsProfilesApi.list()
      setProfiles(profileList)
      const defaultProfile = profileList.find(p => p.is_default) ?? profileList[0]
      if (defaultProfile) {
        setActiveProfileId(defaultProfile.id)
        await loadProfile(defaultProfile.id)
      }
      showToast('Profile deleted')
    } catch (err) {
      setGlobalError(err instanceof Error ? err.message : 'Failed to delete profile')
    } finally {
      setProfileLoading(false)
    }
  }

  const activeProfile = profiles.find(p => p.id === activeProfileId)

  // ─── Render ──────────────────────────────────────────────────────────────────

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="text-sm text-zinc-400">Loading…</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-5">
      {/* Header */}
      <div>
        <h2 className="text-sm font-semibold text-zinc-900">Claude Code Settings</h2>
        <p className="text-xs text-zinc-500 mt-1">
          Manages <code className="font-mono">~/.claude/settings.json</code> — the global Claude
          Code configuration file.
        </p>
      </div>

      {/* Profile selector bar */}
      {profiles.length > 0 && (
        <div className="flex flex-wrap items-center gap-2">
          <Select
            value={activeProfileId}
            onValueChange={id => handleProfileSwitch(id)}
            disabled={profileLoading}
          >
            <SelectTrigger className="h-8 text-xs w-52">
              <SelectValue placeholder="Select profile" />
            </SelectTrigger>
            <SelectContent>
              {profiles.map(p => (
                <SelectItem key={p.id} value={p.id} className="text-xs">
                  {p.name}
                  {p.is_default ? ' (default)' : ''}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>

          <Button
            variant="outline"
            size="sm"
            className="h-8 text-xs gap-1"
            onClick={() => handleNewProfile()}
            disabled={profileLoading}
          >
            <Plus className="h-3 w-3" />
            New
          </Button>

          <Button
            variant="outline"
            size="sm"
            className="h-8 text-xs gap-1"
            onClick={() => handleDuplicateProfile()}
            disabled={profileLoading || !activeProfileId}
          >
            <Copy className="h-3 w-3" />
            Duplicate
          </Button>

          {activeProfile && !activeProfile.is_default && (
            <Button
              variant="outline"
              size="sm"
              className="h-8 text-xs gap-1"
              onClick={() => handleSetDefault()}
              disabled={profileLoading}
            >
              <Star className="h-3 w-3" />
              Set as Default
            </Button>
          )}

          <Button
            variant="outline"
            size="sm"
            className="h-8 text-xs gap-1 text-red-600 hover:text-red-700 hover:border-red-300"
            onClick={() => handleDeleteProfile()}
            disabled={profileLoading || !activeProfileId || activeProfile?.is_default}
            title={
              activeProfile?.is_default ? 'Cannot delete the default profile' : 'Delete profile'
            }
          >
            <Trash2 className="h-3 w-3" />
            Delete
          </Button>
        </div>
      )}

      {/* File-not-found notice */}
      {!exists && (
        <div className="flex flex-col gap-3 rounded-md border border-amber-200 bg-amber-50 px-4 py-4">
          <div className="flex items-start gap-2">
            <AlertCircle className="h-4 w-4 text-amber-600 mt-0.5 shrink-0" />
            <div className="flex flex-col gap-0.5">
              <p className="text-sm font-medium text-amber-800">Settings file not found</p>
              <p className="text-xs text-amber-700">
                <code className="font-mono">~/.claude/settings.json</code> does not exist yet.
                Create it to start configuring Claude Code settings.
              </p>
            </div>
          </div>
          <Button
            variant="outline"
            size="sm"
            className="self-start gap-1.5 border-amber-300 bg-white text-amber-800 hover:bg-amber-50"
            onClick={() => handleCreate()}
            disabled={saving}
          >
            <FilePlus className="h-3.5 w-3.5" />
            {saving ? 'Creating…' : 'Create settings file'}
          </Button>
        </div>
      )}

      {exists && (
        <>
          {/* ── Two-column grid (single column on small screens) ────────── */}
          <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
            {/* ── Left column ─────────────────────────────────────────── */}
            <div className="flex flex-col gap-4">
              {/* Model & Language */}
              <section className="flex flex-col gap-0">
                <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide mb-2">
                  Model &amp; Language
                </h3>
                <div className="border border-zinc-200 rounded-md px-4 divide-y divide-zinc-100">
                  <FieldRow label="Model" description="Override the default Claude model">
                    <Input
                      value={model}
                      onChange={e => setModel(e.target.value)}
                      className="w-44 font-mono text-sm"
                      placeholder="claude-sonnet-4-6"
                    />
                  </FieldRow>
                  <FieldRow label="Language" description="Preferred response language">
                    <Input
                      value={language}
                      onChange={e => setLanguage(e.target.value)}
                      className="w-32 text-sm"
                      placeholder="en"
                    />
                  </FieldRow>
                  <FieldRow label="Effort Level" description="Opus 4.6 reasoning effort">
                    <Select
                      value={effortLevel}
                      onValueChange={v =>
                        setEffortLevel(v as 'low' | 'medium' | 'high' | SelectNone)
                      }
                    >
                      <SelectTrigger className="w-32">
                        <SelectValue placeholder="Default" />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="low">Low</SelectItem>
                        <SelectItem value="medium">Medium</SelectItem>
                        <SelectItem value="high">High</SelectItem>
                      </SelectContent>
                    </Select>
                  </FieldRow>
                  <FieldRow label="Auto-Updates" description="Release channel for updates">
                    <Select
                      value={autoUpdatesChannel}
                      onValueChange={v =>
                        setAutoUpdatesChannel(v as 'stable' | 'latest' | SelectNone)
                      }
                    >
                      <SelectTrigger className="w-32">
                        <SelectValue placeholder="Default" />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="stable">Stable</SelectItem>
                        <SelectItem value="latest">Latest</SelectItem>
                      </SelectContent>
                    </Select>
                  </FieldRow>
                  <FieldRow label="Output Style" description="Response output style">
                    <Input
                      value={outputStyle}
                      onChange={e => setOutputStyle(e.target.value)}
                      className="w-32 text-sm"
                    />
                  </FieldRow>
                  <FieldRow label="API Key Helper" description="Path to auth helper script">
                    <Input
                      value={apiKeyHelper}
                      onChange={e => setApiKeyHelper(e.target.value)}
                      className="w-44 font-mono text-sm"
                      placeholder="/path/to/script"
                    />
                  </FieldRow>
                </div>
              </section>

              {/* Behaviour */}
              <section className="flex flex-col gap-0">
                <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide mb-2">
                  Behaviour
                </h3>
                <div className="border border-zinc-200 rounded-md px-4 divide-y divide-zinc-100">
                  <FieldRow
                    label="Cleanup Period (days)"
                    description="Days to retain chat transcripts"
                  >
                    <Input
                      type="number"
                      value={cleanupPeriodDays}
                      onChange={e => setCleanupPeriodDays(e.target.value)}
                      className="w-20 text-sm"
                      placeholder="30"
                      min={1}
                    />
                  </FieldRow>
                  <FieldRow label="Plans Directory" description="Custom plan file storage">
                    <Input
                      value={plansDirectory}
                      onChange={e => setPlansDirectory(e.target.value)}
                      className="w-44 font-mono text-sm"
                      placeholder="/path/to/plans"
                    />
                  </FieldRow>
                  <FieldRow
                    label="Respect .gitignore"
                    description="@ file picker respects .gitignore"
                  >
                    {/* Claude Code default is true, so undefined renders as checked */}
                    <Toggle checked={respectGitignore ?? true} onChange={setRespectGitignore} />
                  </FieldRow>
                  <FieldRow
                    label="Skip WebFetch Preflight"
                    description="Skip WebFetch blocklist check"
                  >
                    <Toggle
                      checked={skipWebFetchPreflight ?? false}
                      onChange={setSkipWebFetchPreflight}
                    />
                  </FieldRow>
                  <FieldRow label="Disable All Hooks" description="Disable all hooks execution">
                    <Toggle checked={disableAllHooks ?? false} onChange={setDisableAllHooks} />
                  </FieldRow>
                </div>
              </section>
            </div>

            {/* ── Right column ────────────────────────────────────────── */}
            <div className="flex flex-col gap-4">
              {/* UI & Display */}
              <section className="flex flex-col gap-0">
                <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide mb-2">
                  UI &amp; Display
                </h3>
                <div className="border border-zinc-200 rounded-md px-4 divide-y divide-zinc-100">
                  <FieldRow label="Fast Mode" description="Enable Opus 4.6 fast mode">
                    <Toggle checked={fastMode ?? false} onChange={setFastMode} />
                  </FieldRow>
                  <FieldRow label="Show Turn Duration" description="Display response duration">
                    <Toggle checked={showTurnDuration ?? false} onChange={setShowTurnDuration} />
                  </FieldRow>
                  <FieldRow label="Spinner Tips" description="Show tips during work">
                    <Toggle
                      checked={spinnerTipsEnabled ?? false}
                      onChange={setSpinnerTipsEnabled}
                    />
                  </FieldRow>
                  <FieldRow label="Terminal Progress Bar" description="Enable progress bar">
                    <Toggle
                      checked={terminalProgressBarEnabled ?? false}
                      onChange={setTerminalProgressBarEnabled}
                    />
                  </FieldRow>
                  <FieldRow label="Reduced Motion" description="Reduce UI animations">
                    <Toggle
                      checked={prefersReducedMotion ?? false}
                      onChange={setPrefersReducedMotion}
                    />
                  </FieldRow>
                  <FieldRow label="Always Thinking" description="Extended thinking by default">
                    <Toggle
                      checked={alwaysThinkingEnabled ?? false}
                      onChange={setAlwaysThinkingEnabled}
                    />
                  </FieldRow>
                  <FieldRow label="Teammate Mode" description="Agent team display mode">
                    <Select
                      value={teammateMode}
                      onValueChange={v =>
                        setTeammateMode(v as 'auto' | 'in-process' | 'tmux' | SelectNone)
                      }
                    >
                      <SelectTrigger className="w-32">
                        <SelectValue placeholder="Default" />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="auto">Auto</SelectItem>
                        <SelectItem value="in-process">In-Process</SelectItem>
                        <SelectItem value="tmux">Tmux</SelectItem>
                      </SelectContent>
                    </Select>
                  </FieldRow>
                </div>
              </section>

              {/* Permissions & MCP */}
              <section className="flex flex-col gap-0">
                <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide mb-2">
                  Permissions &amp; MCP
                </h3>
                <div className="border border-zinc-200 rounded-md px-4 divide-y divide-zinc-100">
                  <FieldRow
                    label="Enable All Project MCPs"
                    description="Auto-approve all project MCP servers"
                  >
                    <Toggle
                      checked={enableAllProjectMcpServers ?? false}
                      onChange={setEnableAllProjectMcpServers}
                    />
                  </FieldRow>
                  <FieldRow label="Managed Hooks Only" description="Only load managed/SDK hooks">
                    <Toggle
                      checked={allowManagedHooksOnly ?? false}
                      onChange={setAllowManagedHooksOnly}
                    />
                  </FieldRow>
                  <FieldRow
                    label="Managed Permission Rules"
                    description="Only managed permission rules apply"
                  >
                    <Toggle
                      checked={allowManagedPermissionRulesOnly ?? false}
                      onChange={setAllowManagedPermissionRulesOnly}
                    />
                  </FieldRow>
                  <FieldRow
                    label="Managed MCP Servers Only"
                    description="Only managed MCP servers respected"
                  >
                    <Toggle
                      checked={allowManagedMcpServersOnly ?? false}
                      onChange={setAllowManagedMcpServersOnly}
                    />
                  </FieldRow>
                </div>
              </section>
            </div>
          </div>

          {/* ── Advanced — full width ─────────────────────────────────────── */}
          <section className="flex flex-col gap-2">
            <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide">
              Advanced
            </h3>
            <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-2">
              <CollapsibleSection
                title="Permissions"
                description="Allow / deny / ask rules"
                value={permissionsJson}
                onChange={setPermissionsJson}
                error={jsonErrors.permissions}
              />
              <CollapsibleSection
                title="Hooks"
                description="Tool execution event commands"
                value={hooksJson}
                onChange={setHooksJson}
                error={jsonErrors.hooks}
              />
              <CollapsibleSection
                title="Environment (env)"
                description="Extra environment variables"
                value={envJson}
                onChange={setEnvJson}
                error={jsonErrors.env}
              />
              <CollapsibleSection
                title="Sandbox"
                description="Sandboxed bash configuration"
                value={sandboxJson}
                onChange={setSandboxJson}
                error={jsonErrors.sandbox}
              />
              <CollapsibleSection
                title="Attribution"
                description="Git commit / PR attribution"
                value={attributionJson}
                onChange={setAttributionJson}
                error={jsonErrors.attribution}
              />
            </div>
          </section>

          {/* ── JSON Preview — full width ─────────────────────────────────── */}
          <JsonPreview json={previewJson} />

          {globalError && (
            <div className="rounded-md border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
              {globalError}
            </div>
          )}

          <Button
            className="bg-zinc-900 hover:bg-zinc-800 text-white w-full sm:w-auto self-start"
            onClick={() => handleSave()}
            disabled={saving || profileLoading}
          >
            {saving
              ? 'Saving…'
              : activeProfile
                ? `Save "${activeProfile.name}"`
                : 'Save Claude Settings'}
          </Button>
        </>
      )}

      {/* Toast */}
      {toast && (
        <div className="fixed bottom-4 right-4 z-50 rounded-md bg-zinc-900 text-white px-4 py-2 text-sm shadow-lg">
          {toast}
        </div>
      )}
    </div>
  )
}
