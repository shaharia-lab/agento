import { useState, useEffect, useCallback, useMemo, type ReactNode } from 'react'
import { useNavigate } from 'react-router-dom'
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
import { History, Search, RefreshCw, ExternalLink, Zap } from 'lucide-react'
import { Tooltip } from '@/components/ui/tooltip'

function formatTokens(n: number): string {
  if (!n) return '—'
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return String(n)
}

function shortPath(path: string): string {
  // Show last two segments for readability: ~/Projects/foo → Projects/foo
  const parts = path.replace(/^\/home\/[^/]+\//, '~/').split('/')
  return parts.slice(-2).join('/')
}

export default function ClaudeSessionsPage() {
  const navigate = useNavigate()
  const [sessions, setSessions] = useState<ClaudeSessionSummary[]>([])
  const [projects, setProjects] = useState<ClaudeProject[]>([])
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [search, setSearch] = useState('')
  const [filterProject, setFilterProject] = useState('all')

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

  const filtered = useMemo(() => {
    return sessions.filter(s => {
      const matchesProject = filterProject === 'all' || s.project_path === filterProject
      const q = search.toLowerCase()
      const matchesSearch =
        !q ||
        s.session_id.toLowerCase().includes(q) ||
        s.preview.toLowerCase().includes(q) ||
        s.project_path.toLowerCase().includes(q)
      return matchesProject && matchesSearch
    })
  }, [sessions, search, filterProject])

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
        <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100 mb-1">
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
        <p className="text-sm text-zinc-400">No sessions match your filters.</p>
      </div>
    )
  } else {
    sessionListContent = (
      <div className="divide-y divide-zinc-100 dark:divide-zinc-700/50">
        {filtered.map(session => (
          <SessionRow
            key={session.session_id}
            session={session}
            onClick={() => navigate(`/claude-sessions/${session.session_id}`)}
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
          <h1 className="text-base font-semibold text-zinc-900 dark:text-zinc-100">
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
}: Readonly<{ session: ClaudeSessionSummary; onClick: () => void }>) {
  const totalTokens = (session.usage?.input_tokens ?? 0) + (session.usage?.output_tokens ?? 0)
  const hasTokens = totalTokens > 0

  return (
    <button
      type="button"
      className="flex items-start gap-3 px-4 sm:px-6 py-3.5 hover:bg-zinc-50 dark:hover:bg-zinc-800/50 cursor-pointer group transition-colors w-full text-left appearance-none bg-transparent border-0"
      onClick={onClick}
    >
      <div className="flex h-8 w-8 items-center justify-center rounded-full bg-zinc-100 dark:bg-zinc-800 text-zinc-500 dark:text-zinc-400 shrink-0 mt-0.5">
        <History className="h-3.5 w-3.5" />
      </div>
      <div className="flex-1 min-w-0">
        {/* Preview / first message */}
        <p className="text-sm font-medium text-zinc-900 dark:text-zinc-100 truncate leading-snug">
          {session.preview || <span className="italic text-zinc-400">No message content</span>}
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
          <span className="text-xs text-zinc-400 dark:text-zinc-500">
            {session.message_count} msg{session.message_count === 1 ? '' : 's'}
          </span>
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
        {/* Session ID */}
        <p className="text-xs text-zinc-300 dark:text-zinc-600 font-mono mt-0.5 truncate">
          {session.session_id}
        </p>
      </div>
      <ExternalLink className="h-3.5 w-3.5 text-zinc-300 dark:text-zinc-600 group-hover:text-zinc-400 dark:group-hover:text-zinc-400 shrink-0 mt-1 transition-colors" />
    </button>
  )
}
