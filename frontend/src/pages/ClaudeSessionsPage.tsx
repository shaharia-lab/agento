import {
  useState,
  useEffect,
  useCallback,
  useMemo,
  useRef,
  type KeyboardEvent,
  type ReactNode,
} from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { claudeSessionsApi } from '@/lib/api'
import type {
  ClaudeSessionSummary,
  ClaudeSessionCost,
  ClaudeProject,
  ClaudeSessionStatus,
} from '@/types'
import { formatDateTime, formatRelativeTime } from '@/lib/utils'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  History,
  Search,
  RefreshCw,
  Star,
  Clock,
  X,
  Shield,
  ChevronRight,
  ChevronDown,
  SlidersHorizontal,
  Cpu,
  Copy,
  Check,
} from 'lucide-react'
import { Tooltip } from '@/components/ui/tooltip'
import { formatCost, formatTokens, formatDuration, shortPath } from '@/lib/format'
import { resolvePresetRange, type TimePreset } from '@/lib/timefilter'
import { decodeWindows } from '@/lib/drilldown'
import {
  matchesFilters,
  permissionModesOf,
  modelsOf,
  hasPRs as sessionsHavePRs,
  hasFavorites as hasFavoriteSessions,
  isBounded,
  UNBOUNDED,
  type LinkFilter,
  type NumericRange,
} from '@/lib/sessionFilters'
import { groupSessionsByDay, tokenBarReference } from '@/lib/sessionGroups'
import { sessionCost, sessionDurationMs } from '@/lib/sessionMetrics'

// How often to re-check whether the background re-cost finished. Slow enough
// to be free, fast enough that a ~18s corpus rescan is noticed promptly.
const STATUS_POLL_MS = 3000

const TIME_PRESET_LABELS: Record<TimePreset, string> = {
  all: 'All time',
  '1h': 'Last hour',
  '24h': 'Last 24 hours',
  '7d': 'Last 7 days',
  '30d': 'Last 30 days',
  custom: 'Custom range…',
}

/**
 * The row grid. Every cell in the header, the rows and the day headers shares
 * this template — a column that drifts here misaligns the whole table, so it is
 * declared exactly once.
 */
const ROW_GRID = '28px minmax(200px,2fr) minmax(170px,1fr) 92px 108px 56px 146px 82px 66px'
/**
 * Width at which the two flexible columns hit their minimum. Below it the table
 * scrolls sideways rather than squashing titles and branches into nothing.
 */
const ROW_MIN_WIDTH = 1076

/**
 * Presentation for the permission mode a session ran under. This is the one
 * genuine per-session state Claude Code records — there is no "succeeded /
 * failed" in a transcript — so it fills the design's status column rather than
 * a derived label that would be guesswork.
 */
const MODE_PRESENTATION: Record<string, { label: string; tone: string }> = {
  bypassPermissions: {
    label: 'Bypass',
    tone: 'bg-amber-500/15 text-amber-600 dark:text-amber-400',
  },
  acceptEdits: {
    label: 'Accept',
    tone: 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400',
  },
  plan: { label: 'Plan', tone: 'bg-blue-500/10 text-blue-600 dark:text-blue-400' },
  dontAsk: { label: "Don't ask", tone: 'bg-amber-500/15 text-amber-600 dark:text-amber-400' },
  default: { label: 'Default', tone: 'bg-zinc-500/10 text-zinc-500 dark:text-zinc-400' },
}

const NEUTRAL_TONE = 'bg-zinc-500/10 text-zinc-500 dark:text-zinc-400'

/**
 * The filters behind the "Advanced" disclosure: everything that is either
 * rarely needed or too wide for the toolbar. They live in one object so the
 * active-count badge and "Clear" can treat them as a unit — a filter left set
 * behind a collapsed panel is otherwise invisible.
 */
interface AdvancedFilters {
  permissionMode: string
  model: string
  links: LinkFilter
  messages: NumericRange
  durationMinutes: NumericRange
  tokensIn: NumericRange
  tokensOut: NumericRange
  cost: NumericRange
}

const NO_ADVANCED: AdvancedFilters = {
  permissionMode: 'all',
  model: 'all',
  links: 'all',
  messages: UNBOUNDED,
  durationMinutes: UNBOUNDED,
  tokensIn: UNBOUNDED,
  tokensOut: UNBOUNDED,
  cost: UNBOUNDED,
}

const ADVANCED_RANGES = [
  'messages',
  'durationMinutes',
  'tokensIn',
  'tokensOut',
  'cost',
] as const satisfies readonly (keyof AdvancedFilters)[]

/** How many advanced filters are narrowing the list, for the button's badge. */
function countActive(a: AdvancedFilters): number {
  return (
    (a.permissionMode === 'all' ? 0 : 1) +
    (a.model === 'all' ? 0 : 1) +
    (a.links === 'all' ? 0 : 1) +
    ADVANCED_RANGES.filter(k => isBounded(a[k])).length
  )
}

const LINK_OPTIONS: { value: LinkFilter; label: string }[] = [
  { value: 'all', label: 'Any' },
  { value: 'with', label: 'With PR' },
  { value: 'without', label: 'No PR' },
]

export default function ClaudeSessionsPage() {
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  // Drill-down from the analytics page: explicit hour windows + a label.
  const drilldownWindows = useMemo(() => decodeWindows(searchParams.get('windows')), [searchParams])
  const drilldownLabel = searchParams.get('label')
  const drilldownActive = drilldownWindows.length > 0
  const [sessions, setSessions] = useState<ClaudeSessionSummary[]>([])
  const [projects, setProjects] = useState<ClaudeProject[]>([])
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [search, setSearch] = useState('')
  const [filterProject, setFilterProject] = useState(searchParams.get('project') ?? 'all')
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set())

  // Keep the project filter in sync when arriving via a new drill-down URL
  // while the page is already mounted.
  const projectParam = searchParams.get('project')
  useEffect(() => {
    if (projectParam) setFilterProject(projectParam)
  }, [projectParam])
  const [filterFavorites, setFilterFavorites] = useState(false)
  const [advancedOpen, setAdvancedOpen] = useState(false)
  // `advanced` is what the list is filtered by; `advancedDraft` is what the
  // panel is showing. They diverge only while the user is mid-edit.
  const [advanced, setAdvanced] = useState<AdvancedFilters>(NO_ADVANCED)
  const [advancedDraft, setAdvancedDraft] = useState<AdvancedFilters>(NO_ADVANCED)
  const toggleAdvanced = () => {
    // Re-seed on every open so a draft abandoned last time cannot reappear as
    // if it were applied.
    setAdvancedDraft(advanced)
    setAdvancedOpen(o => !o)
  }
  const resetAdvanced = () => {
    setAdvancedDraft(NO_ADVANCED)
    setAdvanced(NO_ADVANCED)
  }
  const [timePreset, setTimePreset] = useState<TimePreset>('all')
  const [customFrom, setCustomFrom] = useState('')
  const [customTo, setCustomTo] = useState('')

  const load = useCallback(async () => {
    try {
      const [s, p] = await Promise.all([claudeSessionsApi.list(), claudeSessionsApi.projects()])
      setSessions(s)
      setProjects(p)
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load sessions')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    load()
  }, [load])

  // Since #208 the list is served from cache even when the pricing catalog
  // moved, so the costs on screen may predate it while a rescan runs. Poll the
  // cheap status endpoint until that clears, then reload once so the figures
  // update without the user having to do anything.
  const [recosting, setRecosting] = useState(false)
  const wasPending = useRef(false)
  useEffect(() => {
    let cancelled = false
    let timer: ReturnType<typeof setTimeout>

    const poll = async () => {
      let status: ClaudeSessionStatus
      try {
        status = await claudeSessionsApi.status()
      } catch {
        // Status is an affordance, not the feature — a failure here must never
        // break the list, so stop polling and leave the figures unlabelled.
        return
      }
      if (cancelled) return

      const pending = status.costs_stale || status.scan_in_progress
      // Tracked in a ref, not read out of the state updater: an updater must
      // stay pure, and React may invoke it twice in development.
      if (wasPending.current && !pending) load()
      wasPending.current = pending
      setRecosting(pending)
      if (pending) timer = setTimeout(poll, STATUS_POLL_MS)
    }

    poll()
    return () => {
      cancelled = true
      clearTimeout(timer)
    }
  }, [load])

  const handleRefresh = async () => {
    setRefreshing(true)
    try {
      await claudeSessionsApi.refresh()
      // Brief pause so the background rescan has time to start.
      await new Promise(r => setTimeout(r, 800))
      await load()
    } catch {
      // Ignore refresh errors — load() will surface them if needed.
    } finally {
      setRefreshing(false)
    }
  }

  const applyFavorite = (sessionId: string, value: boolean) => (prev: ClaudeSessionSummary[]) =>
    prev.map(s => (s.session_id === sessionId ? { ...s, is_favorite: value } : s))

  const handleToggleFavorite = (sessionId: string, isFavorite: boolean) => {
    const next = !isFavorite
    setSessions(applyFavorite(sessionId, next))
    claudeSessionsApi
      .toggleFavorite(sessionId, next)
      .catch(() => setSessions(applyFavorite(sessionId, !next)))
  }

  const handleToggleExpanded = useCallback((sessionId: string) => {
    setExpanded(prev => {
      const next = new Set(prev)
      if (!next.delete(sessionId)) next.add(sessionId)
      return next
    })
  }, [])

  const hasFavorites = hasFavoriteSessions(sessions)
  const hasPRs = sessionsHavePRs(sessions)
  // Only offer the permission-mode filter once more than one mode is present —
  // a single-value dropdown filters nothing.
  const permissionModes = useMemo(() => permissionModesOf(sessions), [sessions])
  const models = useMemo(() => modelsOf(sessions), [sessions])
  const activeAdvanced = countActive(advanced)

  const timeFilterActive = drilldownActive || timePreset !== 'all'

  const clearDrilldown = () => {
    // Drop only the time drill-down; a project carried over from analytics
    // stays applied (and visible in the project dropdown).
    const next = new URLSearchParams(searchParams)
    next.delete('windows')
    next.delete('label')
    setSearchParams(next, { replace: true })
  }

  // The filters outside the advanced panel, built once rather than per session.
  const baseFilters = useMemo(() => {
    const { from, to } = resolvePresetRange(timePreset, customFrom, customTo)
    return {
      project: filterProject,
      search,
      favorites: filterFavorites,
      from,
      to,
      drilldownActive,
      drilldownWindows,
    }
  }, [
    search,
    filterProject,
    filterFavorites,
    timePreset,
    customFrom,
    customTo,
    drilldownActive,
    drilldownWindows,
  ])

  const filtered = useMemo(
    () => sessions.filter(s => matchesFilters(s, { ...baseFilters, ...advanced })),
    [sessions, baseFilters, advanced],
  )

  // What the panel's unapplied draft would leave, so "Apply" is never a leap in
  // the dark. Only counted while the panel is open.
  const draftMatchCount = useMemo(() => {
    if (!advancedOpen) return 0
    return sessions.filter(s => matchesFilters(s, { ...baseFilters, ...advancedDraft })).length
  }, [advancedOpen, sessions, baseFilters, advancedDraft])

  const groups = useMemo(() => groupSessionsByDay(filtered), [filtered])

  const totals = useMemo(
    () =>
      groups.reduce((acc, g) => ({ tokens: acc.tokens + g.tokens, cost: acc.cost + g.cost }), {
        tokens: 0,
        cost: 0,
      }),
    [groups],
  )

  // The token bar is a comparison within what is on screen, scaled so a single
  // outlier session cannot flatten every other bar to nothing.
  const tokenRef = useMemo(() => tokenBarReference(filtered), [filtered])

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400">Scanning Claude sessions…</div>
      </div>
    )
  }

  let listContent: ReactNode
  if (sessions.length === 0) {
    listContent = (
      <div className="flex flex-col items-center justify-center py-20 text-center">
        <div className="flex h-12 w-12 items-center justify-center rounded-full bg-zinc-100 dark:bg-zinc-800 mb-4">
          <History className="h-5 w-5 text-zinc-400" />
        </div>
        <h2 className="text-lg font-semibold text-zinc-900 dark:text-zinc-100 mb-1">
          No Claude sessions found
        </h2>
        <p className="text-xs text-zinc-500 mb-4 max-w-xs">
          Sessions will appear here once you run Claude Code on this machine.
        </p>
      </div>
    )
  } else if (filtered.length === 0) {
    listContent = (
      <div className="flex flex-col items-center justify-center py-16 text-center">
        <p className="text-sm text-zinc-400">
          {timeFilterActive
            ? 'No sessions active in the selected time range.'
            : 'No sessions match your filters.'}
        </p>
        {drilldownActive && (
          <button
            onClick={clearDrilldown}
            className="mt-3 text-xs text-indigo-500 hover:text-indigo-600 dark:hover:text-indigo-400"
          >
            Clear time filter
          </button>
        )}
      </div>
    )
  } else {
    listContent = groups.map(group => (
      <div key={group.key}>
        <div className="flex items-center gap-3 px-4 py-[7px] bg-zinc-50 dark:bg-zinc-800/60 border-b border-zinc-200 dark:border-zinc-700/60">
          <span className="text-xs font-bold uppercase tracking-[0.04em] text-zinc-900 dark:text-zinc-100">
            {group.label}
          </span>
          <span className="text-xs text-zinc-500 dark:text-zinc-400 tabular-nums">
            {group.sessions.length} session{group.sessions.length === 1 ? '' : 's'} ·{' '}
            {group.messageCount} msgs · {formatTokens(group.tokens)} tokens
          </span>
          <div className="flex-1" />
          <span className="text-xs font-bold tabular-nums text-zinc-900 dark:text-zinc-100">
            {formatCost(group.cost)}
          </span>
        </div>
        {group.sessions.map(session => (
          <SessionRow
            key={session.session_id}
            session={session}
            tokenRef={tokenRef}
            open={expanded.has(session.session_id)}
            onToggle={() => handleToggleExpanded(session.session_id)}
            onOpen={() => navigate(`/claude-sessions/${session.session_id}`)}
            onJourney={() => navigate(`/claude-sessions/${session.session_id}/journey`)}
            onToggleFavorite={() => handleToggleFavorite(session.session_id, !!session.is_favorite)}
          />
        ))}
      </div>
    ))
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-zinc-100 dark:border-zinc-700/50 px-4 sm:px-6 py-4 shrink-0">
        <div>
          <h1 className="text-xl font-semibold text-zinc-900 dark:text-zinc-100">
            Claude Sessions
          </h1>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {sessions.length} session{sessions.length === 1 ? '' : 's'} from{' '}
            <span className="font-mono">~/.claude</span>
          </p>
        </div>
        <button
          onClick={() => handleRefresh()}
          disabled={refreshing}
          className="flex items-center gap-1.5 rounded-md border border-zinc-200 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-1.5 text-xs text-zinc-600 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 disabled:opacity-50 transition-colors"
        >
          <RefreshCw className={`h-3.5 w-3.5 ${refreshing ? 'animate-spin' : ''}`} />
          Refresh
        </button>
      </div>

      {/* Re-cost pending. Non-blocking by design: the figures below are correct
          for the rates they were priced at, so they are labelled rather than
          hidden or waited on. */}
      {recosting && (
        <div className="flex items-center gap-2 px-4 sm:px-6 py-2 border-b border-amber-100 dark:border-amber-900/50 bg-amber-50/60 dark:bg-amber-950/30 shrink-0">
          <RefreshCw className="h-3.5 w-3.5 shrink-0 animate-spin text-amber-600 dark:text-amber-400" />
          <p className="text-xs text-amber-700 dark:text-amber-300">
            Cost figures are being recalculated against updated pricing — the values shown are from
            the previous rates and will refresh automatically.
          </p>
        </div>
      )}

      {/* Drill-down banner (from analytics charts) */}
      {drilldownActive && (
        <div className="flex items-center justify-between gap-3 px-4 sm:px-6 py-2 border-b border-indigo-100 dark:border-indigo-900/50 bg-indigo-50/60 dark:bg-indigo-950/30 shrink-0">
          <p className="text-xs text-indigo-700 dark:text-indigo-300 truncate">
            Showing sessions active {drilldownLabel ?? 'in the selected hours'} ·{' '}
            <span className="text-indigo-500 dark:text-indigo-400">
              {filtered.length} match{filtered.length === 1 ? '' : 'es'}
            </span>
          </p>
          <button
            onClick={clearDrilldown}
            className="flex items-center gap-1 text-xs text-indigo-600 dark:text-indigo-400 hover:text-indigo-800 dark:hover:text-indigo-200 shrink-0"
          >
            <X className="h-3 w-3" />
            Clear
          </button>
        </div>
      )}

      {error && (
        <div className="mx-4 sm:mx-6 mt-3 rounded-md border border-red-200 bg-red-50 px-4 py-2.5 text-sm text-red-700 shrink-0">
          {error}
        </div>
      )}

      {/* The table card */}
      <div className="flex-1 min-h-0 flex flex-col px-4 sm:px-6 py-4">
        {/* Capped and centred: the design is a bounded card, and past ~1800px the
            two flexible columns turn into whitespace rather than useful room. */}
        <div className="flex-1 min-h-0 w-full max-w-[1800px] mx-auto flex flex-col overflow-hidden rounded-[14px] border border-zinc-200 dark:border-zinc-700/60 bg-white dark:bg-zinc-900">
          {/* Toolbar */}
          <div className="flex flex-wrap items-center gap-2.5 px-4 py-3 border-b border-zinc-200 dark:border-zinc-700/60 shrink-0">
            <div className="relative w-full sm:w-[300px]">
              <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-zinc-400 dark:text-zinc-500" />
              <input
                value={search}
                onChange={e => setSearch(e.target.value)}
                placeholder="Search by ID or message…"
                className="w-full h-[34px] rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 pl-8 pr-3 text-[13px] placeholder:text-zinc-500 dark:placeholder:text-zinc-400 focus:outline-none focus:ring-1 focus:ring-zinc-900 dark:focus:ring-zinc-400 focus:border-zinc-900 dark:focus:border-zinc-400"
              />
            </div>
            {projects.length > 1 && (
              <Select value={filterProject} onValueChange={setFilterProject}>
                <SelectTrigger className="w-full sm:w-52 h-[34px] text-xs">
                  <SelectValue placeholder="All projects" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All projects</SelectItem>
                  {projects.map(p => (
                    <SelectItem key={p.encoded_name} value={p.decoded_path} className="text-xs">
                      <span className="font-mono">{shortPath(p.decoded_path)}</span>
                      <span className="ml-1.5 text-zinc-400">({p.session_count})</span>
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}
            <Select
              value={timePreset}
              onValueChange={v => setTimePreset(v as TimePreset)}
              disabled={drilldownActive}
            >
              <SelectTrigger className="w-full sm:w-40 h-[34px] text-xs">
                <Clock className="h-3.5 w-3.5 text-zinc-400 dark:text-zinc-500 mr-1.5 shrink-0" />
                <SelectValue placeholder="All time" />
              </SelectTrigger>
              <SelectContent>
                {(Object.keys(TIME_PRESET_LABELS) as TimePreset[]).map(p => (
                  <SelectItem key={p} value={p} className="text-xs">
                    {TIME_PRESET_LABELS[p]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {timePreset === 'custom' && !drilldownActive && (
              <div className="flex items-center gap-1.5 shrink-0">
                <input
                  type="datetime-local"
                  value={customFrom}
                  onChange={e => setCustomFrom(e.target.value)}
                  aria-label="Active from"
                  className="rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 px-2 h-[34px] text-xs focus:outline-none focus:ring-1 focus:ring-zinc-900 dark:focus:ring-zinc-400"
                />
                <span className="text-xs text-zinc-400 dark:text-zinc-500">–</span>
                <input
                  type="datetime-local"
                  value={customTo}
                  onChange={e => setCustomTo(e.target.value)}
                  aria-label="Active to"
                  className="rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 px-2 h-[34px] text-xs focus:outline-none focus:ring-1 focus:ring-zinc-900 dark:focus:ring-zinc-400"
                />
              </div>
            )}
            <ToolbarToggle
              active={advancedOpen || activeAdvanced > 0}
              onClick={toggleAdvanced}
              title="Filter by mode, links, messages, tokens or cost"
              activeClass="border-zinc-900 dark:border-zinc-300 bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900"
            >
              <SlidersHorizontal className="h-3.5 w-3.5" />
              Advanced
              {activeAdvanced > 0 && (
                <span
                  className={`ml-0.5 inline-flex items-center justify-center min-w-4 h-4 px-1 rounded-full text-[10px] font-semibold tabular-nums ${
                    advancedOpen || activeAdvanced > 0
                      ? 'bg-white/25 dark:bg-zinc-900/20'
                      : 'bg-zinc-200 dark:bg-zinc-700'
                  }`}
                >
                  {activeAdvanced}
                </span>
              )}
              <ChevronDown
                className={`h-3 w-3 transition-transform duration-150 ${advancedOpen ? 'rotate-180' : ''}`}
              />
            </ToolbarToggle>
            {hasFavorites && (
              <ToolbarToggle
                active={filterFavorites}
                onClick={() => setFilterFavorites(f => !f)}
                title={filterFavorites ? 'Show all' : 'Show favorites only'}
                activeClass="border-amber-400 bg-amber-50 dark:bg-amber-950/30 text-amber-600 dark:text-amber-400"
              >
                <Star
                  className={`h-3.5 w-3.5 ${filterFavorites ? 'fill-amber-400 text-amber-400' : ''}`}
                />
                Favorites
              </ToolbarToggle>
            )}
            <div className="flex-1" />
            <div className="text-xs text-zinc-500 dark:text-zinc-400 tabular-nums shrink-0">
              {filtered.length} session{filtered.length === 1 ? '' : 's'} ·{' '}
              {formatTokens(totals.tokens)} tokens · {formatCost(totals.cost)}
            </div>
          </div>

          {advancedOpen && (
            <AdvancedFilterPanel
              draft={advancedDraft}
              applied={advanced}
              onChange={setAdvancedDraft}
              onApply={() => setAdvanced(advancedDraft)}
              onReset={resetAdvanced}
              permissionModes={permissionModes}
              models={models}
              showLinks={hasPRs}
              matchCount={draftMatchCount}
            />
          )}

          {/* Column headers and rows share one horizontal scroll box, sized by a
              single min-width wrapper so every track lines up when it scrolls. */}
          <div className="flex-1 min-h-0 overflow-auto">
            {filtered.length > 0 ? (
              <div style={{ minWidth: ROW_MIN_WIDTH }}>
                <div
                  className="sticky top-0 z-10 grid items-center gap-3 px-4 py-2 border-b border-zinc-200 dark:border-zinc-700/60 bg-zinc-50 dark:bg-zinc-800/80 text-[11px] font-semibold uppercase tracking-[0.06em] text-zinc-500 dark:text-zinc-400"
                  style={{ gridTemplateColumns: ROW_GRID }}
                >
                  <div />
                  <div>Session</div>
                  <div>Project · branch</div>
                  <div>Mode</div>
                  <div>Links</div>
                  <div className="text-right">Msgs</div>
                  <div>Tokens in / out</div>
                  <div className="text-right">Cost</div>
                  <div className="text-right">Last</div>
                </div>
                {listContent}
              </div>
            ) : (
              listContent
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

/** Empty means "no bound", not zero — and a half-typed "-" must not become NaN. */
function parseBound(raw: string): number | null {
  if (raw.trim() === '') return null
  const n = Number(raw)
  return Number.isFinite(n) ? n : null
}

const boundValue = (n: number | null) => (n === null ? '' : String(n))

const sameRange = (a: NumericRange, b: NumericRange) => a.min === b.min && a.max === b.max

/** Whether two advanced-filter sets are equivalent, gating the Apply button. */
function sameAdvanced(a: AdvancedFilters, b: AdvancedFilters): boolean {
  return (
    a.permissionMode === b.permissionMode &&
    a.model === b.model &&
    a.links === b.links &&
    ADVANCED_RANGES.every(k => sameRange(a[k], b[k]))
  )
}

// A number input only ever holds 3-6 characters here, so it is sized to that
// rather than stretched to the column. The spinner arrows are removed because
// at this width they cover the value they are meant to help edit.
const NUMBER_INPUT =
  'w-[74px] h-8 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 ' +
  'text-zinc-900 dark:text-zinc-100 px-2 text-xs tabular-nums placeholder:text-zinc-400 ' +
  'dark:placeholder:text-zinc-500 focus:outline-none focus:ring-1 focus:ring-zinc-900 ' +
  'dark:focus:ring-zinc-400 [appearance:textfield] ' +
  '[&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none'

const FIELD_LABEL =
  'text-[11px] uppercase tracking-[0.06em] font-semibold text-zinc-500 dark:text-zinc-400'

/** Label above any advanced field, so every control lines up on one baseline. */
function Field({
  label,
  hint,
  children,
}: Readonly<{ label: string; hint?: string; children: ReactNode }>) {
  return (
    <div className="flex flex-col gap-1.5">
      <span className={FIELD_LABEL}>
        {label}
        {hint && <span className="ml-1 normal-case tracking-normal font-normal">{hint}</span>}
      </span>
      {children}
    </div>
  )
}

/**
 * One min–max pair. Two bounds rather than an operator dropdown plus a value:
 * min alone reads as "at least", max alone as "at most", both as "between" —
 * every comparison asked for, with one fewer control per field.
 */
function RangeField({
  label,
  hint,
  range,
  onChange,
  step,
}: Readonly<{
  label: string
  hint?: string
  range: NumericRange
  onChange: (next: NumericRange) => void
  step?: string
}>) {
  return (
    <Field label={label} hint={hint}>
      <div className="flex items-center gap-1.5">
        <input
          type="number"
          min="0"
          step={step}
          value={boundValue(range.min)}
          onChange={e => onChange({ ...range, min: parseBound(e.target.value) })}
          placeholder="Min"
          aria-label={`${label} minimum`}
          className={NUMBER_INPUT}
        />
        <span className="text-xs text-zinc-400 dark:text-zinc-500">–</span>
        <input
          type="number"
          min="0"
          step={step}
          value={boundValue(range.max)}
          onChange={e => onChange({ ...range, max: parseBound(e.target.value) })}
          placeholder="Max"
          aria-label={`${label} maximum`}
          className={NUMBER_INPUT}
        />
      </div>
    </Field>
  )
}

/**
 * The advanced-filter area below the toolbar.
 *
 * Edits go to a draft and take effect on Apply rather than on every keystroke:
 * typing "50" into a minimum would otherwise filter on "5" first, throwing the
 * list away and back mid-keystroke. The draft is measured against the list
 * before it is committed, so the match count answers "will this leave me
 * anything?" before the user gives up their current view.
 */
function AdvancedFilterPanel({
  draft,
  applied,
  onChange,
  onApply,
  onReset,
  permissionModes,
  models,
  showLinks,
  matchCount,
}: Readonly<{
  draft: AdvancedFilters
  applied: AdvancedFilters
  onChange: (next: AdvancedFilters) => void
  onApply: () => void
  onReset: () => void
  permissionModes: string[]
  models: string[]
  showLinks: boolean
  matchCount: number
}>) {
  const dirty = !sameAdvanced(draft, applied)
  const anySet = countActive(draft) > 0 || countActive(applied) > 0
  const patch = (p: Partial<AdvancedFilters>) => onChange({ ...draft, ...p })

  return (
    <form
      onSubmit={e => {
        e.preventDefault()
        onApply()
      }}
      className="border-b border-zinc-200 dark:border-zinc-700/60 bg-zinc-50/60 dark:bg-zinc-800/30 px-4 py-3.5 shrink-0"
    >
      <div className="flex flex-wrap items-end gap-x-5 gap-y-3">
        {/* One recorded mode is still worth filtering on here, unlike in the
            toolbar: it separates those sessions from the ones Claude Code
            recorded no mode for at all. */}
        {permissionModes.length > 0 && (
          <Field label="Mode">
            <Select value={draft.permissionMode} onValueChange={v => patch({ permissionMode: v })}>
              <SelectTrigger className="w-40 h-8 text-xs">
                <Shield className="h-3.5 w-3.5 text-zinc-400 dark:text-zinc-500 mr-1.5 shrink-0" />
                <SelectValue placeholder="All modes" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All modes</SelectItem>
                {permissionModes.map(m => (
                  <SelectItem key={m} value={m} className="text-xs">
                    <span className="font-mono">{m}</span>
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
        )}

        {models.length > 0 && (
          <Field label="Model">
            <Select value={draft.model} onValueChange={v => patch({ model: v })}>
              <SelectTrigger className="w-44 h-8 text-xs">
                <Cpu className="h-3.5 w-3.5 text-zinc-400 dark:text-zinc-500 mr-1.5 shrink-0" />
                <SelectValue placeholder="All models" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All models</SelectItem>
                {models.map(m => (
                  <SelectItem key={m} value={m} className="text-xs">
                    <span className="font-mono">{m}</span>
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
        )}

        {showLinks && (
          <Field label="Links">
            <div className="flex h-8 rounded-lg border border-zinc-200 dark:border-zinc-700 overflow-hidden bg-white dark:bg-zinc-900">
              {LINK_OPTIONS.map((opt, i) => (
                <button
                  key={opt.value}
                  type="button"
                  onClick={() => patch({ links: opt.value })}
                  className={`px-3 text-xs whitespace-nowrap transition-colors ${
                    i > 0 ? 'border-l border-zinc-200 dark:border-zinc-700' : ''
                  } ${
                    draft.links === opt.value
                      ? 'bg-zinc-100 dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 font-semibold'
                      : 'text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-100'
                  }`}
                >
                  {opt.label}
                </button>
              ))}
            </div>
          </Field>
        )}

        <RangeField
          label="Messages"
          range={draft.messages}
          onChange={r => patch({ messages: r })}
        />
        <RangeField
          label="Duration"
          hint="(min)"
          range={draft.durationMinutes}
          onChange={r => patch({ durationMinutes: r })}
        />
        <RangeField
          label="Tokens in"
          range={draft.tokensIn}
          onChange={r => patch({ tokensIn: r })}
        />
        <RangeField
          label="Tokens out"
          range={draft.tokensOut}
          onChange={r => patch({ tokensOut: r })}
        />
        <RangeField
          label="Cost"
          hint="(USD)"
          step="0.01"
          range={draft.cost}
          onChange={r => patch({ cost: r })}
        />
      </div>

      <div className="flex flex-wrap items-center gap-3 mt-3.5 pt-3 border-t border-zinc-200 dark:border-zinc-700/60">
        <p className="text-[11px] text-zinc-400 dark:text-zinc-500">
          Leave a side empty for an open bound — min only is “at least”, max only “at most”.
        </p>
        <div className="flex-1" />
        {dirty && (
          <span className="text-xs text-zinc-500 dark:text-zinc-400 tabular-nums">
            {matchCount} session{matchCount === 1 ? '' : 's'} match
          </span>
        )}
        <button
          type="button"
          onClick={onReset}
          disabled={!anySet}
          className="h-8 px-3 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-xs text-zinc-700 dark:text-zinc-200 hover:bg-zinc-50 dark:hover:bg-zinc-800 disabled:opacity-40 disabled:pointer-events-none transition-colors"
        >
          Reset
        </button>
        <button
          type="submit"
          disabled={!dirty}
          className="h-8 px-4 rounded-lg bg-zinc-900 dark:bg-zinc-100 text-xs font-medium text-white dark:text-zinc-900 hover:bg-zinc-800 dark:hover:bg-zinc-200 disabled:opacity-40 disabled:pointer-events-none transition-colors"
        >
          Apply filters
        </button>
      </div>
    </form>
  )
}

/** Filter pill in the toolbar — same shape as the outline buttons in the design. */
function ToolbarToggle({
  active,
  onClick,
  title,
  activeClass,
  children,
}: Readonly<{
  active: boolean
  onClick: () => void
  title: string
  activeClass: string
  children: ReactNode
}>) {
  return (
    <button
      onClick={onClick}
      title={title}
      className={`flex items-center gap-1.5 rounded-lg border h-[34px] px-3 text-[13px] transition-colors shrink-0 ${
        active
          ? activeClass
          : 'border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-zinc-600 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-800'
      }`}
    >
      {children}
    </button>
  )
}

/** One labelled figure inside the cost tooltip. */
function CostLine({ label, usd }: Readonly<{ label: string; usd: number }>) {
  if (!usd) return null
  return (
    <div className="flex justify-between gap-4">
      <span className="text-zinc-400">{label}</span>
      <span>{formatCost(usd)}</span>
    </div>
  )
}

/**
 * Cost cell for a session row.
 *
 * A session that used a model with no published rate is marked with `~` and
 * names the offending models: its total is a floor, and presenting an
 * understated figure as complete is the failure this disclosure prevents.
 */
function SessionCostCell({ session }: Readonly<{ session: ClaudeSessionSummary }>) {
  const main = session.cost
  const sub = session.subagent_cost
  const total = sessionCost(session)
  const unpriced = session.unpriced_models ?? []
  const partial = unpriced.length > 0

  const sum = (pick: (c: ClaudeSessionCost) => number) =>
    (main ? pick(main) : 0) + (sub ? pick(sub) : 0)

  return (
    <Tooltip
      side="top"
      content={
        <div className="space-y-1">
          <CostLine label="Input" usd={sum(c => c.input_usd)} />
          <CostLine label="Output" usd={sum(c => c.output_usd)} />
          <CostLine label="Cache read" usd={sum(c => c.cache_read_usd)} />
          <CostLine label="Cache write" usd={sum(c => c.cache_write_usd)} />
          <CostLine label="Sub-agents" usd={sub?.total_usd ?? 0} />
          {partial && (
            <div className="text-amber-300 pt-1">
              Excludes {unpriced.join(', ')} — no published rate
            </div>
          )}
        </div>
      }
    >
      <span
        className={`block w-full text-right tabular-nums cursor-default ${
          partial ? 'text-amber-600 dark:text-amber-400' : 'text-zinc-900 dark:text-zinc-100'
        }`}
      >
        {partial && '~'}
        {formatCost(total)}
      </span>
    </Tooltip>
  )
}

/** Copies the session ID, echoing the confirmation on the label like the design. */
function CopyIdButton({ value }: Readonly<{ value: string }>) {
  const [copied, setCopied] = useState(false)
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => () => clearTimeout(timeoutRef.current ?? undefined), [])

  const handleCopy = async () => {
    if (!navigator.clipboard) return
    try {
      await navigator.clipboard.writeText(value)
    } catch {
      return
    }
    setCopied(true)
    clearTimeout(timeoutRef.current ?? undefined)
    timeoutRef.current = setTimeout(() => setCopied(false), 1400)
  }

  return (
    <button
      type="button"
      onClick={handleCopy}
      className="flex items-center gap-1.5 h-8 px-3 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-[13px] text-zinc-700 dark:text-zinc-200 hover:bg-zinc-50 dark:hover:bg-zinc-800 transition-colors"
    >
      {copied ? (
        <Check className="h-3.5 w-3.5 text-emerald-500" />
      ) : (
        <Copy className="h-3.5 w-3.5" />
      )}
      {copied ? 'Copied' : 'Copy ID'}
    </button>
  )
}

/** One label/value pair in the expanded detail grid. */
function DetailField({ label, value }: Readonly<{ label: string; value: string }>) {
  return (
    <>
      <span className="text-zinc-500 dark:text-zinc-400">{label}</span>
      <span className="tabular-nums text-zinc-900 dark:text-zinc-100 truncate">{value}</span>
    </>
  )
}

function SessionRow({
  session,
  tokenRef,
  open,
  onToggle,
  onOpen,
  onJourney,
  onToggleFavorite,
}: Readonly<{
  session: ClaudeSessionSummary
  tokenRef: number
  open: boolean
  onToggle: () => void
  onOpen: () => void
  onJourney: () => void
  onToggleFavorite: () => void
}>) {
  const inTokens = (session.usage?.input_tokens ?? 0) + (session.subagent_usage?.input_tokens ?? 0)
  const outTokens =
    (session.usage?.output_tokens ?? 0) + (session.subagent_usage?.output_tokens ?? 0)
  const total = inTokens + outTokens
  // Bars are proportional to the largest session on screen, then split in/out.
  const scale = tokenRef > 0 ? Math.min(1, total / tokenRef) : 0
  const inPct = total > 0 ? (scale * 100 * inTokens) / total : 0
  const outPct = total > 0 ? (scale * 100 * outTokens) / total : 0

  const mode = session.permission_mode
  const presentation = mode
    ? (MODE_PRESENTATION[mode] ?? { label: mode, tone: NEUTRAL_TONE })
    : null

  const cacheRead =
    (session.usage?.cache_read_tokens ?? 0) + (session.subagent_usage?.cache_read_tokens ?? 0)
  // Shared with the duration filter so the figure shown and the bound that
  // hides the row are computed the same way.
  const durationMs = sessionDurationMs(session)

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault()
      onToggle()
    }
  }

  return (
    <div className="border-b border-zinc-100 dark:border-zinc-700/50">
      <div
        role="button"
        tabIndex={0}
        aria-expanded={open}
        onClick={onToggle}
        onKeyDown={handleKeyDown}
        className="group/row grid items-center gap-3 px-4 h-11 text-[13px] cursor-pointer hover:bg-zinc-50 dark:hover:bg-zinc-800/50 transition-colors"
        style={{ gridTemplateColumns: ROW_GRID }}
      >
        <ChevronRight
          className={`h-3.5 w-3.5 text-zinc-400 dark:text-zinc-500 transition-transform duration-150 ${
            open ? 'rotate-90' : ''
          }`}
        />

        {/* Session title, with the favourite toggle inline so every title still
            starts at the same x-position whether or not it is starred. */}
        <div className="flex items-center gap-1.5 min-w-0">
          <button
            type="button"
            onClick={e => {
              e.stopPropagation()
              onToggleFavorite()
            }}
            title={session.is_favorite ? 'Remove from favorites' : 'Add to favorites'}
            className={`shrink-0 transition-opacity ${
              session.is_favorite
                ? 'text-amber-400'
                : 'opacity-0 group-hover/row:opacity-100 focus-visible:opacity-100 text-zinc-300 dark:text-zinc-600 hover:text-amber-400'
            }`}
          >
            <Star className={`h-3.5 w-3.5 ${session.is_favorite ? 'fill-amber-400' : ''}`} />
          </button>
          <span className="truncate text-zinc-900 dark:text-zinc-100">
            {session.display_title || session.preview || (
              <span className="italic font-normal text-zinc-400">No message content</span>
            )}
          </span>
        </div>

        <div className="flex items-center gap-1.5 min-w-0">
          <span className="font-mono text-[11.5px] bg-zinc-100 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-300 rounded-md px-1.5 py-0.5 truncate">
            {shortPath(session.project_path)}
          </span>
          {session.git_branch && (
            <span className="font-mono text-[11.5px] text-zinc-400 dark:text-zinc-500 truncate">
              {session.git_branch}
            </span>
          )}
        </div>

        <div className="min-w-0">
          {presentation && (
            <span
              className={`inline-flex items-center h-5 rounded-full px-2.5 text-[11px] whitespace-nowrap ${presentation.tone}`}
            >
              {presentation.label}
            </span>
          )}
        </div>

        <div className="flex items-center gap-1 min-w-0 overflow-hidden">
          {session.prs?.map(pr => (
            <a
              key={pr.pr_url}
              href={pr.pr_url}
              target="_blank"
              rel="noopener noreferrer"
              onClick={e => e.stopPropagation()}
              title={pr.pr_repository ? `${pr.pr_repository}#${pr.pr_number}` : pr.pr_url}
              className="font-mono text-[11.5px] text-zinc-500 dark:text-zinc-400 border border-zinc-200 dark:border-zinc-700 rounded-[5px] px-[5px] py-px hover:text-emerald-600 dark:hover:text-emerald-400 hover:border-emerald-400"
            >
              #{pr.pr_number}
            </a>
          ))}
        </div>

        <Tooltip
          side="top"
          content={
            <div className="space-y-1">
              <div className="flex justify-between gap-4">
                <span className="text-zinc-400">Messages</span>
                <span>{session.message_count.toLocaleString()}</span>
              </div>
              <div className="flex justify-between gap-4">
                <span className="text-zinc-400">Raw events</span>
                <span>{session.event_count.toLocaleString()}</span>
              </div>
            </div>
          }
        >
          <span className="block w-full text-right tabular-nums text-zinc-500 dark:text-zinc-400 cursor-default">
            {session.message_count}
          </span>
        </Tooltip>

        <Tooltip
          side="top"
          content={
            <div className="space-y-1">
              <div className="flex justify-between gap-4">
                <span className="text-zinc-400">Input tokens</span>
                <span>{inTokens.toLocaleString()}</span>
              </div>
              <div className="flex justify-between gap-4">
                <span className="text-zinc-400">Output tokens</span>
                <span>{outTokens.toLocaleString()}</span>
              </div>
              {session.subagent_count > 0 && (
                <div className="flex justify-between gap-4">
                  <span className="text-zinc-400">Sub-agents</span>
                  <span>{session.subagent_count.toLocaleString()}</span>
                </div>
              )}
            </div>
          }
        >
          <span className="flex w-full items-center gap-2 cursor-default">
            <span className="flex-1 h-1.5 rounded-[3px] bg-zinc-100 dark:bg-zinc-700/70 overflow-hidden flex">
              <span
                className="h-full bg-zinc-900/85 dark:bg-zinc-100/85"
                style={{ width: `${inPct}%` }}
              />
              <span
                className="h-full bg-zinc-900/30 dark:bg-zinc-100/30"
                style={{ width: `${outPct}%` }}
              />
            </span>
            <span className="tabular-nums text-[11.5px] text-zinc-500 dark:text-zinc-400 whitespace-nowrap">
              {formatTokens(inTokens)} / {formatTokens(outTokens)}
            </span>
          </span>
        </Tooltip>

        <SessionCostCell session={session} />

        <span className="text-right tabular-nums text-xs text-zinc-400 dark:text-zinc-500 truncate">
          {formatRelativeTime(session.last_activity)}
        </span>
      </div>

      {open && (
        <div
          className="grid gap-7 pl-14 pr-4 pt-3.5 pb-4 bg-zinc-50 dark:bg-zinc-800/50 border-t border-zinc-200 dark:border-zinc-700/60"
          style={{ gridTemplateColumns: '1fr 260px 200px' }}
        >
          <div className="flex flex-col gap-2.5 min-w-0">
            <div className="text-[12.5px] leading-[1.55] text-zinc-500 dark:text-zinc-400 text-pretty line-clamp-3">
              {session.preview || 'No message content.'}
            </div>
            <div className="flex items-center gap-2 flex-wrap">
              <span className="font-mono text-[11.5px] bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-700 rounded-md px-2 py-1 text-zinc-600 dark:text-zinc-300">
                {session.session_id}
              </span>
              <CopyIdButton value={session.session_id} />
            </div>
          </div>

          <div className="grid grid-cols-[auto_1fr] gap-y-1 gap-x-3.5 text-xs content-start min-w-0">
            <DetailField label="Model" value={session.model || '—'} />
            <DetailField label="Started" value={formatDateTime(session.start_time) || '—'} />
            <DetailField
              label="Duration"
              value={durationMs > 0 ? formatDuration(durationMs) : '—'}
            />
            <DetailField label="Cache read" value={cacheRead > 0 ? formatTokens(cacheRead) : '—'} />
            {session.compaction_count > 0 && (
              <DetailField
                label="Compactions"
                value={`${session.compaction_count} · ${formatTokens(session.dropped_tokens)} dropped`}
              />
            )}
            {session.subagent_count > 0 && (
              <DetailField label="Sub-agents" value={String(session.subagent_count)} />
            )}
          </div>

          <div className="flex gap-2 items-start justify-end">
            <button
              type="button"
              onClick={onJourney}
              className="h-8 px-3 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-[13px] text-zinc-700 dark:text-zinc-200 hover:bg-zinc-50 dark:hover:bg-zinc-800 transition-colors"
            >
              Journey
            </button>
            <button
              type="button"
              onClick={onOpen}
              className="h-8 px-4 rounded-lg bg-zinc-900 dark:bg-zinc-100 text-[13px] font-medium text-white dark:text-zinc-900 hover:bg-zinc-800 dark:hover:bg-zinc-200 transition-colors"
            >
              Open
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
