import { useState, useEffect, useCallback, useMemo, type ReactNode } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { claudeSessionsApi } from '@/lib/api'
import type { ClaudeSessionSummary, ClaudeProject } from '@/types'
import { formatRelativeTime } from '@/lib/utils'
import { Badge } from '@/components/ui/badge'
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
  ExternalLink,
  Zap,
  Star,
  Activity,
  Clock,
  X,
} from 'lucide-react'
import { Tooltip } from '@/components/ui/tooltip'
import { CopyableId } from '@/components/CopyableId'
import { formatTokens, shortPath } from '@/lib/format'
import { overlapsRange, resolvePresetRange, type TimePreset } from '@/lib/timefilter'
import { decodeWindows, overlapsAnyWindow } from '@/lib/drilldown'

const TIME_PRESET_LABELS: Record<TimePreset, string> = {
  all: 'All time',
  '1h': 'Last hour',
  '24h': 'Last 24 hours',
  '7d': 'Last 7 days',
  '30d': 'Last 30 days',
  custom: 'Custom range…',
}

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

  // Keep the project filter in sync when arriving via a new drill-down URL
  // while the page is already mounted.
  const projectParam = searchParams.get('project')
  useEffect(() => {
    if (projectParam) setFilterProject(projectParam)
  }, [projectParam])
  const [filterFavorites, setFilterFavorites] = useState(false)
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

  const hasFavorites = sessions.some(s => s.is_favorite)

  const timeFilterActive = drilldownActive || timePreset !== 'all'

  const clearDrilldown = () => {
    // Drop only the time drill-down; a project carried over from analytics
    // stays applied (and visible in the project dropdown).
    const next = new URLSearchParams(searchParams)
    next.delete('windows')
    next.delete('label')
    setSearchParams(next, { replace: true })
  }

  const filtered = useMemo(() => {
    const { from, to } = resolvePresetRange(timePreset, customFrom, customTo)
    const result = sessions.filter(s => {
      const matchesProject = filterProject === 'all' || s.project_path === filterProject
      const q = search.toLowerCase()
      const matchesSearch =
        !q ||
        s.session_id.toLowerCase().includes(q) ||
        (s.display_title ?? '').toLowerCase().includes(q) ||
        s.preview.toLowerCase().includes(q) ||
        s.project_path.toLowerCase().includes(q)
      const matchesFavorites = !filterFavorites || !!s.is_favorite
      const matchesTime = drilldownActive
        ? overlapsAnyWindow(s.start_time, s.last_activity, drilldownWindows)
        : overlapsRange(s.start_time, s.last_activity, from, to)
      return matchesProject && matchesSearch && matchesFavorites && matchesTime
    })
    return result
  }, [
    sessions,
    search,
    filterProject,
    filterFavorites,
    timePreset,
    customFrom,
    customTo,
    drilldownActive,
    drilldownWindows,
  ])

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400">Scanning Claude sessions…</div>
      </div>
    )
  }

  let sessionListContent: ReactNode
  if (sessions.length === 0) {
    sessionListContent = (
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
    sessionListContent = (
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
    sessionListContent = (
      <div
        key={`${filterFavorites}-${filterProject}-${search}-${timePreset}-${customFrom}-${customTo}`}
        className="divide-y divide-zinc-100 dark:divide-zinc-700/50"
      >
        {filtered.map(session => (
          <SessionRow
            key={session.session_id}
            session={session}
            onClick={() => navigate(`/claude-sessions/${session.session_id}`)}
            onJourney={() => navigate(`/claude-sessions/${session.session_id}/journey`)}
            onToggleFavorite={() => handleToggleFavorite(session.session_id, !!session.is_favorite)}
          />
        ))}
      </div>
    )
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

      {/* Filters */}
      {sessions.length > 0 && (
        <div className="flex flex-col sm:flex-row items-stretch sm:items-center gap-2 sm:gap-3 px-4 sm:px-6 py-3 border-b border-zinc-100 dark:border-zinc-700/50 shrink-0">
          <div className="relative flex-1 sm:max-w-xs">
            <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-zinc-400 dark:text-zinc-500" />
            <input
              value={search}
              onChange={e => setSearch(e.target.value)}
              placeholder="Search by ID or message…"
              className="w-full rounded-md border border-zinc-200 dark:border-zinc-600 bg-white dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 pl-8 pr-3 py-1.5 text-sm placeholder:text-zinc-400 dark:placeholder:text-zinc-500 focus:outline-none focus:ring-1 focus:ring-zinc-900 dark:focus:ring-zinc-400 focus:border-zinc-900 dark:focus:border-zinc-400"
            />
          </div>
          {projects.length > 1 && (
            <Select value={filterProject} onValueChange={setFilterProject}>
              <SelectTrigger className="w-full sm:w-56 h-8 text-xs">
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
            <SelectTrigger className="w-full sm:w-44 h-8 text-xs">
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
                className="rounded-md border border-zinc-200 dark:border-zinc-600 bg-white dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 px-2 py-1 h-8 text-xs focus:outline-none focus:ring-1 focus:ring-zinc-900 dark:focus:ring-zinc-400"
              />
              <span className="text-xs text-zinc-400 dark:text-zinc-500">–</span>
              <input
                type="datetime-local"
                value={customTo}
                onChange={e => setCustomTo(e.target.value)}
                aria-label="Active to"
                className="rounded-md border border-zinc-200 dark:border-zinc-600 bg-white dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 px-2 py-1 h-8 text-xs focus:outline-none focus:ring-1 focus:ring-zinc-900 dark:focus:ring-zinc-400"
              />
            </div>
          )}
          {hasFavorites && (
            <button
              onClick={() => setFilterFavorites(f => !f)}
              className={`flex items-center gap-1.5 rounded-md border h-8 px-3 text-xs transition-colors shrink-0 ${
                filterFavorites
                  ? 'border-amber-400 bg-amber-50 dark:bg-amber-950/30 text-amber-600 dark:text-amber-400'
                  : 'border-zinc-200 dark:border-zinc-600 bg-white dark:bg-zinc-800 text-zinc-500 dark:text-zinc-400 hover:border-amber-300 hover:text-amber-500'
              }`}
              title={filterFavorites ? 'Show all' : 'Show favorites only'}
            >
              <Star
                className={`h-3.5 w-3.5 ${filterFavorites ? 'fill-amber-400 text-amber-400' : ''}`}
              />
              Favorites
            </button>
          )}
        </div>
      )}

      {error && (
        <div className="mx-6 mt-3 rounded-md border border-red-200 bg-red-50 px-4 py-2.5 text-sm text-red-700">
          {error}
        </div>
      )}

      {/* Session list */}
      <div className="flex-1 overflow-y-auto">{sessionListContent}</div>
    </div>
  )
}

function SessionRow({
  session,
  onClick,
  onToggleFavorite,
  onJourney,
}: Readonly<{
  session: ClaudeSessionSummary
  onClick: () => void
  onToggleFavorite: () => void
  onJourney: () => void
}>) {
  const totalTokens = (session.usage?.input_tokens ?? 0) + (session.usage?.output_tokens ?? 0)
  const hasTokens = totalTokens > 0

  return (
    <div className="px-4 sm:px-6 py-3.5 hover:bg-zinc-50 dark:hover:bg-zinc-800/50 cursor-pointer group transition-colors relative">
      <div className="flex items-start gap-3">
        <button
          type="button"
          className="flex items-start gap-3 flex-1 min-w-0 text-left appearance-none bg-transparent border-0 p-0"
          onClick={onClick}
        >
          <div className="flex h-8 w-8 items-center justify-center rounded-full bg-zinc-100 dark:bg-zinc-800 text-zinc-500 dark:text-zinc-400 shrink-0 mt-0.5">
            <History className="h-3.5 w-3.5" />
          </div>
          <div className="flex-1 min-w-0">
            {/* Resolved title: Agento rename › native rename › AI title › preview */}
            <p className="text-sm font-medium text-zinc-900 dark:text-zinc-100 truncate leading-snug">
              {session.display_title || session.preview || (
                <span className="italic text-zinc-400">No message content</span>
              )}
            </p>
            {/* Meta row */}
            <div className="flex items-center gap-2 mt-1 flex-wrap">
              <Badge
                variant="secondary"
                className="text-xs py-0 h-4 bg-zinc-100 dark:bg-zinc-700 text-zinc-600 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-700 border-0 font-mono font-normal"
              >
                {shortPath(session.project_path)}
              </Badge>
              {session.git_branch && (
                <span className="text-xs text-zinc-400 dark:text-zinc-500 font-mono">
                  {session.git_branch}
                </span>
              )}
              <span className="text-xs text-zinc-400 dark:text-zinc-500">
                {formatRelativeTime(session.last_activity)}
              </span>
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
                <span className="text-xs text-zinc-400 dark:text-zinc-500 cursor-default">
                  {session.message_count} msg{session.message_count === 1 ? '' : 's'}
                </span>
              </Tooltip>
              {hasTokens && (
                <Tooltip
                  side="top"
                  content={
                    <div className="space-y-1">
                      <div className="flex justify-between gap-4">
                        <span className="text-zinc-400">Input tokens</span>
                        <span>{session.usage.input_tokens.toLocaleString()}</span>
                      </div>
                      <div className="flex justify-between gap-4">
                        <span className="text-zinc-400">Output tokens</span>
                        <span>{session.usage.output_tokens.toLocaleString()}</span>
                      </div>
                      {session.usage.cache_read_tokens > 0 && (
                        <div className="flex justify-between gap-4">
                          <span className="text-zinc-400">Cache read</span>
                          <span>{session.usage.cache_read_tokens.toLocaleString()}</span>
                        </div>
                      )}
                      {session.usage.cache_creation_tokens > 0 && (
                        <div className="flex justify-between gap-4">
                          <span className="text-zinc-400">Cache write</span>
                          <span>{session.usage.cache_creation_tokens.toLocaleString()}</span>
                        </div>
                      )}
                    </div>
                  }
                >
                  <span className="flex items-center gap-0.5 text-xs text-zinc-400 dark:text-zinc-500 cursor-default">
                    <Zap className="h-2.5 w-2.5" />
                    {formatTokens(session.usage.input_tokens)}↑&nbsp;
                    {formatTokens(session.usage.output_tokens)}↓
                  </span>
                </Tooltip>
              )}
            </div>
          </div>
        </button>
        <button
          type="button"
          className={`h-7 w-7 flex items-center justify-center rounded-md transition-all shrink-0 mt-0.5 ${
            session.is_favorite
              ? 'text-amber-400'
              : 'opacity-0 group-hover:opacity-100 text-zinc-300 dark:text-zinc-600 hover:text-amber-400'
          }`}
          onClick={e => {
            e.stopPropagation()
            onToggleFavorite()
          }}
          title={session.is_favorite ? 'Remove from favorites' : 'Add to favorites'}
        >
          <Star className={`h-3.5 w-3.5 ${session.is_favorite ? 'fill-amber-400' : ''}`} />
        </button>
        <button
          type="button"
          className="h-7 w-7 flex items-center justify-center rounded-md opacity-0 group-hover:opacity-100 text-zinc-300 dark:text-zinc-600 hover:text-zinc-500 dark:hover:text-zinc-400 transition-all shrink-0 mt-0.5 cursor-pointer"
          onClick={e => {
            e.stopPropagation()
            onJourney()
          }}
          title="View session journey"
        >
          <Activity className="h-3.5 w-3.5" />
        </button>
        <ExternalLink className="h-3.5 w-3.5 text-zinc-300 dark:text-zinc-600 group-hover:text-zinc-400 dark:group-hover:text-zinc-400 shrink-0 mt-1.5 transition-colors" />
      </div>
      {/* Session ID — copies to clipboard on click, does not navigate */}
      <div className="pl-11 mt-0.5">
        <CopyableId value={session.session_id} label="Copy session ID" />
      </div>
    </div>
  )
}
