import { useState, useEffect, useCallback, useMemo } from 'react'
import { Eye, EyeOff, Search } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Checkbox } from '@/components/ui/checkbox'
import { settingsApi, claudeSessionsApi } from '@/lib/api'
import type { SettingsResponse, ClaudeProject } from '@/types'
import { DEFAULT_IDLE_GAP_MINUTES, MIN_IDLE_GAP_MINUTES, MAX_IDLE_GAP_MINUTES } from '@/types'

/**
 * Data & Analytics settings: which projects Agento reports on, and what counts
 * as continuous work when it measures how long a session ran.
 *
 * Both settings change what every number on the dashboard means, so each one
 * says in a line what it does — and the idle threshold additionally says that
 * saving it recomputes stored figures, because that takes time and the user
 * would otherwise see durations shift with no explanation.
 */
export default function DataAnalyticsTab() {
  const [resp, setResp] = useState<SettingsResponse | null>(null)
  const [projects, setProjects] = useState<ClaudeProject[]>([])
  const [hidden, setHidden] = useState<Set<string>>(new Set())
  const [idleGap, setIdleGap] = useState(DEFAULT_IDLE_GAP_MINUTES)
  const [filter, setFilter] = useState('')
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
      // include_hidden: the list must offer every project, or an already
      // hidden one could never be unhidden from here.
      const [settings, allProjects] = await Promise.all([
        settingsApi.get(),
        claudeSessionsApi.projects(true),
      ])
      setResp(settings)
      setProjects(allProjects)
      setHidden(new Set(settings.settings.hidden_projects ?? []))
      setIdleGap(settings.settings.idle_gap_threshold_minutes || DEFAULT_IDLE_GAP_MINUTES)
    } catch {
      setError('Failed to load data settings')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    load()
  }, [load])

  const toggleProject = (path: string) => {
    setHidden(prev => {
      const next = new Set(prev)
      if (next.has(path)) {
        next.delete(path)
      } else {
        next.add(path)
      }
      return next
    })
  }

  const filtered = useMemo(() => {
    const q = filter.trim().toLowerCase()
    if (!q) return projects
    return projects.filter(p => p.decoded_path.toLowerCase().includes(q))
  }, [projects, filter])

  // Bulk actions apply to what is on screen, not to everything: with a filter
  // active, "Hide all" that also swept away projects the user cannot see would
  // be a change they did not ask for and cannot review.
  const setAllVisible = (visible: boolean) => {
    setHidden(prev => {
      const next = new Set(prev)
      for (const p of filtered) {
        if (visible) {
          next.delete(p.decoded_path)
        } else {
          next.add(p.decoded_path)
        }
      }
      return next
    })
  }

  const idleGapChanged =
    (resp?.settings.idle_gap_threshold_minutes || DEFAULT_IDLE_GAP_MINUTES) !== idleGap

  const idleGapInvalid = idleGap < MIN_IDLE_GAP_MINUTES || idleGap > MAX_IDLE_GAP_MINUTES

  const handleSave = async () => {
    if (idleGapInvalid) return
    setSaving(true)
    setError(null)
    try {
      const updated = await settingsApi.update({
        ...resp?.settings,
        hidden_projects: [...hidden],
        idle_gap_threshold_minutes: idleGap,
      })
      setResp(updated)
      showToast(
        idleGapChanged
          ? 'Saved — recomputing session durations in the background'
          : 'Data settings saved',
      )
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
    <div className="max-w-2xl flex flex-col gap-8">
      <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
        Data &amp; Analytics
      </h2>

      {/* Idle threshold */}
      <div className="flex flex-col gap-1.5">
        <Label
          htmlFor="idle-gap-minutes"
          className="text-sm font-medium text-zinc-700 dark:text-zinc-300"
        >
          Idle Threshold
        </Label>
        <div className="flex items-center gap-2">
          <Input
            id="idle-gap-minutes"
            type="number"
            min={MIN_IDLE_GAP_MINUTES}
            max={MAX_IDLE_GAP_MINUTES}
            value={idleGap}
            onChange={e => setIdleGap(Number(e.target.value))}
            className="w-28 font-mono text-sm"
          />
          <span className="text-sm text-zinc-500 dark:text-zinc-400">minutes</span>
        </div>
        <p className="text-xs text-zinc-400">
          A pause longer than this ends a working session, so idle time is left out of every
          duration Agento reports. Sessions are resumable, so without it a session picked up a week
          later would count that whole week as time spent. Default: {DEFAULT_IDLE_GAP_MINUTES}{' '}
          minutes.
        </p>
        {idleGapInvalid && (
          <p className="text-xs text-red-600 dark:text-red-400">
            Enter a value between {MIN_IDLE_GAP_MINUTES} and {MAX_IDLE_GAP_MINUTES} minutes.
          </p>
        )}
        {idleGapChanged && !idleGapInvalid && (
          <p className="text-xs text-amber-600 dark:text-amber-500">
            Saving re-reads every transcript to recompute stored durations. This runs in the
            background and can take a minute on a large history.
          </p>
        )}
      </div>

      {/* Hidden projects */}
      <div className="flex flex-col gap-1.5">
        <Label className="text-sm font-medium text-zinc-700 dark:text-zinc-300">
          Visible Projects
        </Label>
        <p className="text-xs text-zinc-400">
          Unchecked projects are excluded everywhere: their sessions disappear from the list, and
          their tokens, costs and metrics are left out of every chart and total. Nothing is deleted
          — re-checking a project brings its data straight back.
        </p>

        {projects.length === 0 ? (
          <p className="mt-2 text-sm text-zinc-500 dark:text-zinc-400">
            No Claude Code projects found in ~/.claude/projects.
          </p>
        ) : (
          <>
            <div className="mt-2 flex items-center gap-2">
              <div className="relative flex-1">
                <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-zinc-400 pointer-events-none" />
                <Input
                  value={filter}
                  onChange={e => setFilter(e.target.value)}
                  placeholder="Filter projects"
                  aria-label="Filter projects"
                  className="pl-8 text-sm"
                />
              </div>
              <Button variant="outline" size="sm" onClick={() => setAllVisible(true)}>
                <Eye className="h-3.5 w-3.5 mr-1.5" />
                Show all
              </Button>
              <Button variant="outline" size="sm" onClick={() => setAllVisible(false)}>
                <EyeOff className="h-3.5 w-3.5 mr-1.5" />
                Hide all
              </Button>
            </div>

            <div className="mt-2 max-h-80 overflow-y-auto rounded-md border border-zinc-200 dark:border-zinc-700">
              {filtered.map(project => {
                const isHidden = hidden.has(project.decoded_path)
                return (
                  <label
                    key={project.decoded_path}
                    className="flex cursor-pointer items-center gap-3 border-b border-zinc-100 px-3 py-2 last:border-b-0 hover:bg-zinc-50 dark:border-zinc-800 dark:hover:bg-zinc-800/50"
                  >
                    <Checkbox
                      checked={!isHidden}
                      onCheckedChange={() => toggleProject(project.decoded_path)}
                      aria-label={`Include ${project.decoded_path} in analytics`}
                    />
                    <span
                      className={`flex-1 truncate font-mono text-xs ${
                        isHidden
                          ? 'text-zinc-400 line-through dark:text-zinc-600'
                          : 'text-zinc-700 dark:text-zinc-300'
                      }`}
                      title={project.decoded_path}
                    >
                      {project.decoded_path}
                    </span>
                    <span className="shrink-0 text-xs text-zinc-400">
                      {project.session_count} session{project.session_count === 1 ? '' : 's'}
                    </span>
                  </label>
                )
              })}
              {filtered.length === 0 && (
                <p className="px-3 py-4 text-sm text-zinc-500 dark:text-zinc-400">
                  No project matches “{filter}”.
                </p>
              )}
            </div>
            <p className="mt-1 text-xs text-zinc-400">
              {hidden.size === 0
                ? `All ${projects.length} projects included.`
                : `${hidden.size} of ${projects.length} projects excluded.`}
            </p>
          </>
        )}
      </div>

      {error && (
        <div className="rounded-md border border-red-200 bg-red-50 dark:border-red-800 dark:bg-red-900/20 px-3 py-2 text-sm text-red-700 dark:text-red-400">
          {error}
        </div>
      )}

      <Button
        className="bg-zinc-900 hover:bg-zinc-800 text-white dark:bg-zinc-100 dark:hover:bg-zinc-200 dark:text-zinc-900 w-full sm:w-auto"
        onClick={handleSave}
        disabled={saving || idleGapInvalid}
      >
        {saving ? 'Saving…' : 'Save Data Settings'}
      </Button>

      {toast && (
        <div className="fixed bottom-4 right-4 z-50 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm shadow-lg">
          {toast}
        </div>
      )}
    </div>
  )
}
