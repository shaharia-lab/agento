import { useState, useEffect, useCallback, useMemo, useRef } from 'react'
import { Search, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { settingsApi, claudeSessionsApi } from '@/lib/api'
import type { SettingsResponse, ClaudeProject } from '@/types'
import { DEFAULT_IDLE_GAP_MINUTES, MIN_IDLE_GAP_MINUTES, MAX_IDLE_GAP_MINUTES } from '@/types'

/**
 * How many matches the picker renders at once. A corpus can hold several
 * hundred projects, so the list is a set of suggestions to choose from rather
 * than an inventory to scroll: past a handful, typing one more character beats
 * any amount of scrolling, and the count of what was left out says so.
 */
const MAX_SUGGESTIONS = 8

/**
 * Data & Analytics settings: which projects Agento reports on, and what counts
 * as continuous work when it measures how long a session ran.
 *
 * Both settings change what every number on the dashboard means, so each one
 * says in a line what it does — and the idle threshold additionally says that
 * saving it recomputes stored figures, because that takes time and the user
 * would otherwise see durations shift with no explanation.
 *
 * Exclusions are shown as a list of exceptions with a search box to add to it,
 * not as a checkbox per project: hiding a project is rare and a corpus can hold
 * hundreds, so the useful question is "what am I leaving out" rather than "here
 * is everything you have, find the two you meant".
 */
export default function DataAnalyticsTab() {
  const [resp, setResp] = useState<SettingsResponse | null>(null)
  const [projects, setProjects] = useState<ClaudeProject[]>([])
  const [hidden, setHidden] = useState<string[]>([])
  const [idleGap, setIdleGap] = useState(DEFAULT_IDLE_GAP_MINUTES)
  const [query, setQuery] = useState('')
  const [pickerOpen, setPickerOpen] = useState(false)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [toast, setToast] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const pickerRef = useRef<HTMLDivElement>(null)

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 3000)
  }

  const load = useCallback(async () => {
    try {
      // include_hidden: the picker must know about every project, or one that
      // is already excluded could never be found and restored from here.
      const [settings, allProjects] = await Promise.all([
        settingsApi.get(),
        claudeSessionsApi.projects(true),
      ])
      setResp(settings)
      setProjects(allProjects)
      setHidden(settings.settings.hidden_projects ?? [])
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

  // Clicking away closes the suggestions. Without this the list stays open over
  // the rest of the form after a selection is made elsewhere on the page.
  useEffect(() => {
    if (!pickerOpen) return
    const onPointerDown = (e: MouseEvent) => {
      if (!pickerRef.current?.contains(e.target as Node)) setPickerOpen(false)
    }
    document.addEventListener('mousedown', onPointerDown)
    return () => document.removeEventListener('mousedown', onPointerDown)
  }, [pickerOpen])

  const sessionCounts = useMemo(
    () => new Map(projects.map(p => [p.decoded_path, p.session_count])),
    [projects],
  )

  const matches = useMemo(() => {
    const q = query.trim().toLowerCase()
    const excluded = new Set(hidden)
    const candidates = projects.filter(
      p => !excluded.has(p.decoded_path) && (!q || p.decoded_path.toLowerCase().includes(q)),
    )
    return { shown: candidates.slice(0, MAX_SUGGESTIONS), total: candidates.length }
  }, [projects, hidden, query])

  const exclude = (path: string) => {
    setHidden(prev => (prev.includes(path) ? prev : [...prev, path]))
    setQuery('')
    setPickerOpen(false)
  }

  const include = (path: string) => setHidden(prev => prev.filter(p => p !== path))

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
        hidden_projects: hidden,
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

      {/* Excluded projects */}
      <div className="flex flex-col gap-1.5">
        <Label
          htmlFor="project-search"
          className="text-sm font-medium text-zinc-700 dark:text-zinc-300"
        >
          Excluded Projects
        </Label>
        <p className="text-xs text-zinc-400">
          Sessions from these projects disappear from the list, and their tokens, costs and metrics
          are left out of every chart and total. Nothing is deleted — removing a project from this
          list brings its data straight back.
        </p>

        <div ref={pickerRef} className="relative mt-2">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-zinc-400 pointer-events-none" />
          <Input
            id="project-search"
            value={query}
            onChange={e => {
              setQuery(e.target.value)
              setPickerOpen(true)
            }}
            onFocus={() => setPickerOpen(true)}
            onKeyDown={e => {
              if (e.key === 'Escape') setPickerOpen(false)
              // Enter takes the top match, so excluding a project you can name
              // never requires reaching for the mouse.
              if (e.key === 'Enter' && matches.shown.length > 0) {
                e.preventDefault()
                exclude(matches.shown[0].decoded_path)
              }
            }}
            placeholder={
              projects.length === 0
                ? 'No Claude Code projects found in ~/.claude/projects'
                : 'Search a project to exclude…'
            }
            disabled={projects.length === 0}
            className="pl-8 text-sm"
            role="combobox"
            aria-expanded={pickerOpen}
            aria-controls="project-suggestions"
            autoComplete="off"
          />

          {pickerOpen && projects.length > 0 && (
            <div
              id="project-suggestions"
              role="listbox"
              className="absolute z-20 mt-1 w-full overflow-hidden rounded-md border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-zinc-900"
            >
              {matches.shown.map(project => (
                <button
                  key={project.decoded_path}
                  type="button"
                  role="option"
                  aria-selected={false}
                  onClick={() => exclude(project.decoded_path)}
                  className="flex w-full items-center gap-3 px-3 py-2 text-left hover:bg-zinc-50 dark:hover:bg-zinc-800"
                >
                  <span className="flex-1 truncate font-mono text-xs text-zinc-700 dark:text-zinc-300">
                    {project.decoded_path}
                  </span>
                  <span className="shrink-0 text-xs text-zinc-400">
                    {project.session_count} session{project.session_count === 1 ? '' : 's'}
                  </span>
                </button>
              ))}

              {matches.total === 0 && (
                <p className="px-3 py-2 text-xs text-zinc-500 dark:text-zinc-400">
                  {query.trim()
                    ? `No project matches “${query.trim()}”.`
                    : 'Every project is already excluded.'}
                </p>
              )}

              {matches.total > matches.shown.length && (
                <p className="border-t border-zinc-100 px-3 py-1.5 text-xs text-zinc-400 dark:border-zinc-800">
                  {matches.total - matches.shown.length} more — keep typing to narrow it down.
                </p>
              )}
            </div>
          )}
        </div>

        {hidden.length === 0 ? (
          <p className="mt-2 text-xs text-zinc-500 dark:text-zinc-400">
            Nothing is excluded: every project counts towards the figures Agento reports.
          </p>
        ) : (
          <>
            <ul className="mt-2 divide-y divide-zinc-100 rounded-md border border-zinc-200 dark:divide-zinc-800 dark:border-zinc-700">
              {hidden.map(path => (
                <li key={path} className="flex items-center gap-3 px-3 py-2">
                  <span
                    className="flex-1 truncate font-mono text-xs text-zinc-700 dark:text-zinc-300"
                    title={path}
                  >
                    {path}
                  </span>
                  {/* A path with no count is one that is no longer on disk. It
                      stays listed so it can still be removed. */}
                  <span className="shrink-0 text-xs text-zinc-400">
                    {sessionCounts.has(path)
                      ? `${sessionCounts.get(path)} session${sessionCounts.get(path) === 1 ? '' : 's'}`
                      : 'not found on disk'}
                  </span>
                  <button
                    type="button"
                    onClick={() => include(path)}
                    aria-label={`Stop excluding ${path}`}
                    className="shrink-0 rounded p-1 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-700 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </li>
              ))}
            </ul>
            <button
              type="button"
              onClick={() => setHidden([])}
              className="mt-1 self-start text-xs text-zinc-500 underline-offset-2 hover:underline dark:text-zinc-400"
            >
              Clear all {hidden.length} exclusions
            </button>
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
