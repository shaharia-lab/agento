import { useState, useEffect, useCallback, useMemo, useRef, type ReactNode } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { claudeSessionsApi } from '@/lib/api'
import type {
  ClaudeSessionDetail,
  ClaudeMessage,
  ClaudeNormalizedBlock,
  ClaudeTodo,
  ClaudeSubagent,
} from '@/types'
import { formatRelativeTime, formatDateTime } from '@/lib/utils'
import {
  ArrowLeft,
  ChevronRight,
  CheckCircle2,
  Circle,
  Clock,
  Play,
  Loader2,
  Star,
  Activity,
  Search,
  Copy,
  Check,
  GitPullRequest,
} from 'lucide-react'
import { formatCost, formatTokens, formatDuration, shortPath } from '@/lib/format'
import {
  filesTouched,
  outline,
  tailPath,
  toolSummary,
  toolUsage,
  type OutlineEntry,
  type ToolUsage,
  type FileTouch,
} from '@/lib/transcript'
import {
  useTranscriptWindow,
  type TranscriptFilter,
  type TranscriptJump,
} from '@/lib/useTranscriptWindow'
import { SessionInsightsCard } from './SessionInsightsCard'

/**
 * How recently the transcript must have been written to for the header to call
 * the session active. Claude Code appends as it works, so a gap this long means
 * nothing is running.
 */
const ACTIVE_WINDOW_MS = 10 * 60 * 1000

const FILTER_LABELS: Record<TranscriptFilter, string> = {
  all: 'All',
  messages: 'Messages',
  tools: 'Tools',
}

// ── Small presentational pieces ───────────────────────────────────────────────

/** A metadata chip in the header: filled for identity, outlined for attributes. */
function MetaChip({
  children,
  variant = 'outline',
  title,
}: Readonly<{ children: ReactNode; variant?: 'filled' | 'outline'; title?: string }>) {
  const base = 'font-mono text-[11.5px] rounded-md whitespace-nowrap'
  return (
    <span
      title={title}
      className={
        variant === 'filled'
          ? `${base} px-1.5 py-[3px] bg-zinc-100 dark:bg-zinc-800 text-zinc-800 dark:text-zinc-100`
          : `${base} px-1.5 py-[2px] border border-zinc-200 dark:border-zinc-700 text-zinc-500 dark:text-zinc-400`
      }
    >
      {children}
    </span>
  )
}

function MetaDivider() {
  return <span className="w-px h-3.5 bg-zinc-200 dark:bg-zinc-700 mx-1" />
}

/** One cell of the six-across summary strip under the header. */
function StatCell({ label, value }: Readonly<{ label: string; value: string }>) {
  return (
    <div className="bg-white dark:bg-zinc-900 px-3 py-2 flex flex-col gap-0.5 min-w-0">
      <span className="text-[10.5px] uppercase tracking-[0.06em] font-semibold text-zinc-500 dark:text-zinc-400">
        {label}
      </span>
      <span className="text-[14.5px] font-semibold tabular-nums text-zinc-900 dark:text-zinc-100 truncate">
        {value}
      </span>
    </div>
  )
}

function SidebarCard({ title, children }: Readonly<{ title: string; children: ReactNode }>) {
  return (
    <div className="rounded-xl border border-zinc-200 dark:border-zinc-700/60 bg-white dark:bg-zinc-900 p-3.5">
      <div className="text-[11px] uppercase tracking-[0.06em] font-semibold text-zinc-500 dark:text-zinc-400 mb-2.5">
        {title}
      </div>
      {children}
    </div>
  )
}

/** Copies the session ID, echoing the confirmation on the chip like the design. */
function CopyIdChip({ value }: Readonly<{ value: string }>) {
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
      className="inline-flex items-center gap-1.5 font-mono text-[11.5px] text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-100 transition-colors"
    >
      {value}
      <span className="inline-flex items-center gap-1 border border-zinc-200 dark:border-zinc-700 rounded-[5px] px-1.5 py-px text-[10.5px]">
        {copied ? (
          <Check className="h-2.5 w-2.5 text-emerald-500" />
        ) : (
          <Copy className="h-2.5 w-2.5" />
        )}
        {copied ? 'Copied' : 'Copy'}
      </span>
    </button>
  )
}

// ── Transcript blocks ─────────────────────────────────────────────────────────

/**
 * A collapsible tool call or thinking block.
 *
 * Openness is `explicit ?? defaultOpen` so the toolbar's expand/collapse-all
 * moves every block that has not been touched individually, without the page
 * having to hold one entry per block up front.
 */
function CollapsibleBlock({
  label,
  summary,
  body,
  open,
  onToggle,
}: Readonly<{
  label: string
  summary?: string
  body: string
  open: boolean
  onToggle: () => void
}>) {
  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/50 overflow-hidden">
      <button
        type="button"
        onClick={onToggle}
        className="flex w-full items-center gap-2 px-2.5 py-[7px] text-[12.5px] text-left hover:bg-zinc-100 dark:hover:bg-zinc-700/50 transition-colors"
      >
        <ChevronRight
          className={`h-3 w-3 shrink-0 text-zinc-400 dark:text-zinc-500 transition-transform duration-150 ${
            open ? 'rotate-90' : ''
          }`}
        />
        <span className="font-mono font-semibold text-zinc-800 dark:text-zinc-100 shrink-0">
          {label}
        </span>
        {summary && (
          <span className="font-mono text-[11.5px] text-zinc-500 dark:text-zinc-400 truncate">
            {summary}
          </span>
        )}
      </button>
      {open && body && (
        <pre className="m-0 px-3 py-2.5 border-t border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 font-mono text-[11.5px] leading-[1.55] text-zinc-700 dark:text-zinc-300 overflow-x-auto whitespace-pre-wrap break-words">
          {body}
        </pre>
      )}
    </div>
  )
}

// ── Transcript row ────────────────────────────────────────────────────────────

function EventRow({
  msg,
  filter,
  isBlockOpen,
  onToggleBlock,
}: Readonly<{
  msg: ClaudeMessage
  filter: TranscriptFilter
  isBlockOpen: (key: string) => boolean
  onToggleBlock: (key: string) => void
}>) {
  const isUser = (msg.role ?? msg.type) === 'user'
  const blocks = msg.blocks ?? []
  const showProse = filter !== 'tools'
  const showTools = filter !== 'messages'

  const rendered: ReactNode[] = []
  if (isUser && showProse && (msg.content ?? '').trim()) {
    rendered.push(
      <div key="content" className="text-[13.5px] leading-[1.6] whitespace-pre-wrap text-pretty">
        {msg.content}
      </div>,
    )
  }
  blocks.forEach((b: ClaudeNormalizedBlock, i) => {
    const key = `${msg.uuid}-${i}`
    if (b.type === 'text' && showProse) {
      rendered.push(
        <div key={key} className="text-[13.5px] leading-[1.6] whitespace-pre-wrap text-pretty">
          {b.text}
        </div>,
      )
    } else if (b.type === 'thinking' && showProse) {
      rendered.push(
        <CollapsibleBlock
          key={key}
          label="Thinking"
          body={b.text ?? ''}
          open={isBlockOpen(key)}
          onToggle={() => onToggleBlock(key)}
        />,
      )
    } else if (b.type === 'tool_use' && showTools) {
      rendered.push(
        <CollapsibleBlock
          key={key}
          label={b.name ?? 'unknown'}
          summary={toolSummary(b)}
          body={b.input ? JSON.stringify(b.input, null, 2) : ''}
          open={isBlockOpen(key)}
          onToggle={() => onToggleBlock(key)}
        />,
      )
    }
  })

  if (rendered.length === 0) return null

  const inTokens = msg.usage?.input_tokens ?? 0
  const outTokens = msg.usage?.output_tokens ?? 0

  return (
    <div
      id={`event-${msg.uuid}`}
      className={`grid gap-3 px-4 py-3.5 border-b border-zinc-100 dark:border-zinc-700/50 scroll-mt-4 ${
        isUser ? 'bg-zinc-50 dark:bg-zinc-800/40' : ''
      }`}
      style={{ gridTemplateColumns: '26px minmax(0,1fr) 96px' }}
    >
      <div
        className={`h-6 w-6 rounded-md flex items-center justify-center text-[10.5px] font-bold border border-zinc-200 dark:border-zinc-700 ${
          isUser
            ? 'bg-white dark:bg-zinc-900 text-zinc-500 dark:text-zinc-400'
            : 'bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900'
        }`}
      >
        {isUser ? 'U' : 'A'}
      </div>

      <div className="min-w-0 flex flex-col gap-2">{rendered}</div>

      <div className="flex flex-col gap-[3px] items-end text-right tabular-nums">
        <span className="text-[11.5px] text-zinc-400 dark:text-zinc-500">
          {new Date(msg.timestamp).toLocaleTimeString('en-GB')}
        </span>
        {(inTokens > 0 || outTokens > 0) && (
          <span className="text-[11px] font-mono text-zinc-400 dark:text-zinc-500">
            {formatTokens(inTokens)}↑ {formatTokens(outTokens)}↓
          </span>
        )}
      </div>
    </div>
  )
}

// ── Sidebar sections ──────────────────────────────────────────────────────────

function todoIcon(status: string) {
  if (status === 'completed')
    return <CheckCircle2 className="h-3.5 w-3.5 text-emerald-500 shrink-0 mt-px" />
  if (status === 'in_progress')
    return <Clock className="h-3.5 w-3.5 text-amber-500 shrink-0 mt-px animate-pulse" />
  return <Circle className="h-3.5 w-3.5 text-zinc-400 shrink-0 mt-px" />
}

function TodosCard({ todos }: Readonly<{ todos: ClaudeTodo[] }>) {
  if (!todos?.length) return null
  const done = todos.filter(t => t.status === 'completed').length
  return (
    <SidebarCard title={`Todos · ${done}/${todos.length}`}>
      <div className="flex flex-col gap-1.5">
        {todos.map((t, i) => (
          <div key={`${t.content.slice(0, 40)}-${i}`} className="flex items-start gap-2 text-xs">
            {todoIcon(t.status)}
            <span
              className={
                t.status === 'completed'
                  ? 'text-zinc-400 dark:text-zinc-500 line-through'
                  : 'text-zinc-700 dark:text-zinc-300'
              }
            >
              {t.content}
            </span>
          </div>
        ))}
      </div>
    </SidebarCard>
  )
}

function SubagentsCard({ subagents }: Readonly<{ subagents: ClaudeSubagent[] }>) {
  if (!subagents?.length) return null
  return (
    <SidebarCard title={`Sub-agents · ${subagents.length}`}>
      <div className="flex flex-col gap-2.5">
        {subagents.map(sa => (
          <div key={sa.agent_id} className="text-xs min-w-0">
            <div className="font-mono text-[11.5px] text-zinc-800 dark:text-zinc-100 truncate">
              {sa.agent_type || 'sub-agent'}
            </div>
            {sa.description && (
              <div className="text-[11.5px] text-zinc-500 dark:text-zinc-400 line-clamp-2">
                {sa.description}
              </div>
            )}
            <div className="text-[11px] tabular-nums text-zinc-400 dark:text-zinc-500 mt-0.5">
              {sa.message_count} msgs ·{' '}
              {formatTokens(sa.usage.input_tokens + sa.usage.output_tokens)} tokens
            </div>
          </div>
        ))}
      </div>
    </SidebarCard>
  )
}

/**
 * The end of the rendered window: extends it as it scrolls into view.
 */
function TranscriptSentinel({
  shown,
  total,
  onMore,
}: Readonly<{ shown: number; total: number; onMore: () => void }>) {
  const ref = useRef<HTMLDivElement | null>(null)
  useEffect(() => {
    const el = ref.current
    if (!el) return
    const observer = new IntersectionObserver(
      entries => {
        if (entries.some(e => e.isIntersecting)) onMore()
      },
      { rootMargin: '600px' },
    )
    observer.observe(el)
    return () => observer.disconnect()
  }, [onMore])

  return (
    <div
      ref={ref}
      className="flex items-center justify-center gap-2 py-3.5 text-[12.5px] text-zinc-400 dark:text-zinc-500"
    >
      <button
        type="button"
        onClick={onMore}
        className="tabular-nums hover:text-zinc-800 dark:hover:text-zinc-100 transition-colors"
      >
        Show more ({shown} of {total} events)
      </button>
    </div>
  )
}

/**
 * The transcript itself: search, the All/Messages/Tools filter, expand-all, and
 * the event rows. It owns that state because nothing outside it reads the
 * filter — the sidebar reaches rows through their DOM ids, not through React.
 */
function TranscriptPanel({
  messages,
  isActive,
  jump,
}: Readonly<{
  messages: ClaudeMessage[]
  isActive: boolean
  /**
   * The event the sidebar asked to scroll to. The nonce is what makes clicking
   * the same entry twice scroll twice: the uuid alone would not change.
   */
  jump: TranscriptJump | null
}>) {
  const [defaultOpen, setDefaultOpen] = useState(false)
  const [overrides, setOverrides] = useState<ReadonlyMap<string, boolean>>(() => new Map())
  const { search, setSearch, filter, setFilter, visible, shown, showMore } = useTranscriptWindow(
    messages,
    jump,
  )

  const isBlockOpen = useCallback(
    (key: string) => overrides.get(key) ?? defaultOpen,
    [overrides, defaultOpen],
  )
  const toggleBlock = useCallback(
    (key: string) =>
      setOverrides(prev => {
        const next = new Map(prev)
        next.set(key, !(prev.get(key) ?? defaultOpen))
        return next
      }),
    [defaultOpen],
  )
  const toggleAll = () => {
    setDefaultOpen(v => !v)
    setOverrides(new Map())
  }

  return (
    <div className="flex flex-col gap-3 min-w-0">
      <div className="flex items-center gap-2 flex-wrap">
        <div className="relative w-full sm:w-[280px]">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-zinc-400 dark:text-zinc-500" />
          <input
            value={search}
            onChange={e => setSearch(e.target.value)}
            placeholder="Search this transcript…"
            className="w-full h-8 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 pl-8 pr-3 text-[12.5px] placeholder:text-zinc-500 dark:placeholder:text-zinc-400 focus:outline-none focus:ring-1 focus:ring-zinc-900 dark:focus:ring-zinc-400"
          />
        </div>
        <div className="flex rounded-lg border border-zinc-200 dark:border-zinc-700 overflow-hidden bg-white dark:bg-zinc-900">
          {(Object.keys(FILTER_LABELS) as TranscriptFilter[]).map((f, i) => (
            <button
              key={f}
              onClick={() => setFilter(f)}
              className={`px-3 py-1.5 text-[12.5px] transition-colors ${
                i > 0 ? 'border-l border-zinc-200 dark:border-zinc-700' : ''
              } ${
                filter === f
                  ? 'bg-zinc-100 dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 font-semibold'
                  : 'text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-100'
              }`}
            >
              {FILTER_LABELS[f]}
            </button>
          ))}
        </div>
        <div className="flex-1" />
        <button
          onClick={toggleAll}
          className="text-[12.5px] text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-100 transition-colors"
        >
          {defaultOpen ? 'Collapse all tools' : 'Expand all tools'}
        </button>
      </div>

      <div className="rounded-[14px] border border-zinc-200 dark:border-zinc-700/60 bg-white dark:bg-zinc-900 overflow-hidden">
        {visible.length === 0 ? (
          <p className="text-sm text-zinc-400 text-center py-12">
            {messages.length === 0
              ? 'No messages in this session.'
              : 'No events match the current filter.'}
          </p>
        ) : (
          <>
            {shown.map(msg => (
              <EventRow
                key={msg.uuid}
                msg={msg}
                filter={filter}
                isBlockOpen={isBlockOpen}
                onToggleBlock={toggleBlock}
              />
            ))}
            {shown.length < visible.length ? (
              <TranscriptSentinel shown={shown.length} total={visible.length} onMore={showMore} />
            ) : (
              <div className="flex items-center justify-center gap-2 py-3.5 text-[12.5px] text-zinc-400 dark:text-zinc-500">
                <span
                  className={`w-1.5 h-1.5 rounded-full ${
                    isActive ? 'bg-emerald-500' : 'bg-zinc-300 dark:bg-zinc-600'
                  }`}
                />
                {isActive
                  ? 'Session is live. Reload to pick up new events'
                  : `End of transcript · ${visible.length} of ${messages.length} events`}
              </div>
            )}
          </>
        )}
      </div>
    </div>
  )
}

/** Timeline outline, tool histogram, files touched, todos and sub-agents. */
function SessionSidebar({
  timeline,
  tools,
  files,
  todos,
  subagents,
  sessionId,
  onJump,
}: Readonly<{
  timeline: OutlineEntry[]
  tools: ToolUsage[]
  files: FileTouch[]
  todos: ClaudeTodo[]
  subagents: ClaudeSubagent[]
  sessionId?: string
  /** Asks the transcript to reveal and scroll to an event. */
  onJump: (uuid: string) => void
}>) {
  const maxToolCount = tools[0]?.count ?? 0
  return (
    <div className="flex flex-col gap-4">
      {/* First card in the sidebar: these are the measurements of the run, and
          the endpoint behind them had no caller at all until now. */}
      {sessionId && <SessionInsightsCard sessionId={sessionId} />}
      {timeline.length > 0 && (
        <SidebarCard title="Timeline">
          <div className="flex flex-col">
            {timeline.map(entry => (
              <button
                key={entry.uuid}
                onClick={() => onJump(entry.uuid)}
                className="grid gap-2.5 items-baseline py-[5px] text-[12.5px] text-left text-zinc-700 dark:text-zinc-300 hover:text-zinc-900 dark:hover:text-zinc-100 transition-colors"
                style={{ gridTemplateColumns: '52px 1fr' }}
              >
                <span className="tabular-nums text-[11.5px] text-zinc-400 dark:text-zinc-500">
                  {new Date(entry.timestamp).toLocaleTimeString('en-GB', {
                    hour: '2-digit',
                    minute: '2-digit',
                  })}
                </span>
                <span className="text-pretty line-clamp-2">{entry.label}</span>
              </button>
            ))}
          </div>
        </SidebarCard>
      )}

      {tools.length > 0 && (
        <SidebarCard title="Tool usage">
          <div className="flex flex-col gap-2">
            {tools.map(t => (
              <div
                key={t.name}
                className="grid gap-2 items-center text-[12.5px]"
                style={{ gridTemplateColumns: '1fr 34px' }}
              >
                <div className="flex flex-col gap-1 min-w-0">
                  <span className="font-mono text-[11.5px] truncate text-zinc-700 dark:text-zinc-300">
                    {t.name}
                  </span>
                  <span className="h-[5px] rounded-[3px] bg-zinc-100 dark:bg-zinc-700/70 overflow-hidden block">
                    <span
                      className="block h-full bg-zinc-900/80 dark:bg-zinc-100/80"
                      style={{ width: `${maxToolCount ? (t.count / maxToolCount) * 100 : 0}%` }}
                    />
                  </span>
                </div>
                <span className="text-right tabular-nums text-[11.5px] text-zinc-400 dark:text-zinc-500">
                  {t.count}
                </span>
              </div>
            ))}
          </div>
        </SidebarCard>
      )}

      {files.length > 0 && (
        <SidebarCard title="Files touched">
          <div className="flex flex-col gap-1.5">
            {files.map(f => (
              <div key={f.path} className="flex items-center gap-2 text-xs">
                <span
                  title={shortPath(f.path)}
                  className="font-mono text-[11.5px] truncate flex-1 text-zinc-700 dark:text-zinc-300"
                >
                  {tailPath(f.path)}
                </span>
                <span className="tabular-nums text-[11px] text-zinc-400 dark:text-zinc-500">
                  ×{f.count}
                </span>
              </div>
            ))}
          </div>
        </SidebarCard>
      )}

      <TodosCard todos={todos} />
      <SubagentsCard subagents={subagents} />
    </div>
  )
}

/** Everything the transcript records about the run, as one wrapping chip row. */
function SessionMetaRow({
  detail,
  sessionId,
}: Readonly<{ detail: ClaudeSessionDetail; sessionId?: string }>) {
  const worktreeLabel = detail.worktree_name || detail.worktree_branch
  return (
    <div className="flex flex-wrap items-center gap-1.5 text-xs text-zinc-500 dark:text-zinc-400">
      <MetaChip variant="filled">{shortPath(detail.cwd || detail.project_path)}</MetaChip>
      {detail.git_branch && <MetaChip>⑂ {detail.git_branch}</MetaChip>}
      {detail.worktree_branch && (
        <MetaChip
          title={
            detail.original_branch
              ? `Worktree ${worktreeLabel}, branched from ${detail.original_branch}`
              : `Worktree ${worktreeLabel}`
          }
        >
          🌳 {detail.worktree_branch}
        </MetaChip>
      )}
      {detail.permission_mode && <MetaChip>{detail.permission_mode}</MetaChip>}
      {detail.model && <MetaChip variant="filled">{detail.model}</MetaChip>}
      {detail.compaction_count > 0 && (
        <MetaChip title={`${detail.dropped_tokens.toLocaleString()} tokens dropped`}>
          {detail.compaction_count} compaction{detail.compaction_count === 1 ? '' : 's'}
        </MetaChip>
      )}
      {detail.prs?.map(pr => (
        <a
          key={pr.pr_url}
          href={pr.pr_url}
          target="_blank"
          rel="noopener noreferrer"
          title={pr.pr_repository ? `${pr.pr_repository}#${pr.pr_number}` : pr.pr_url}
          className="inline-flex items-center gap-1 font-mono text-[11.5px] rounded-md px-1.5 py-[2px] border border-zinc-200 dark:border-zinc-700 text-emerald-600 dark:text-emerald-400 hover:border-emerald-400"
        >
          <GitPullRequest className="h-3 w-3" />#{pr.pr_number}
        </a>
      ))}
      <MetaDivider />
      <span className="tabular-nums" title={formatDateTime(detail.start_time, true)}>
        Started {formatDateTime(detail.start_time)} · {formatRelativeTime(detail.last_activity)}
      </span>
      <MetaDivider />
      {sessionId && <CopyIdChip value={sessionId} />}
    </div>
  )
}

// ── Main page ─────────────────────────────────────────────────────────────────

export default function ClaudeSessionDetailPage() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()
  const [detail, setDetail] = useState<ClaudeSessionDetail | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [continuing, setContinuing] = useState(false)
  const [editingTitle, setEditingTitle] = useState(false)
  const [titleDraft, setTitleDraft] = useState('')
  const titleInputRef = useRef<HTMLInputElement>(null)

  const load = useCallback(async () => {
    if (!id) return
    try {
      const d = await claudeSessionsApi.get(id)
      setDetail(d)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load session')
    } finally {
      setLoading(false)
    }
  }, [id])

  useEffect(() => {
    load()
  }, [load])

  const startEditingTitle = () => {
    if (!detail) return
    // Seed from custom_title alone: seeding from display_title would silently
    // promote Claude Code's own title into an Agento override on first save.
    setTitleDraft(detail.custom_title ?? '')
    setEditingTitle(true)
    setTimeout(() => titleInputRef.current?.select(), 0)
  }

  const saveTitle = async () => {
    if (!id || !detail) return
    const trimmed = titleDraft.trim()
    setEditingTitle(false)
    if (trimmed === (detail.custom_title ?? '')) return
    try {
      await claudeSessionsApi.updateTitle(id, trimmed)
      // The heading renders display_title, so re-resolve it here (same
      // precedence as the backend) — otherwise it keeps the old label until
      // the next reload, including when the override is cleared.
      setDetail(prev =>
        prev
          ? {
              ...prev,
              custom_title: trimmed,
              display_title: trimmed || prev.native_title || prev.ai_title || prev.preview,
            }
          : prev,
      )
    } catch {
      // silently ignore — title stays as-is
    }
  }

  const handleContinue = async () => {
    if (!id || continuing) return
    setContinuing(true)
    try {
      const { chat_id } = await claudeSessionsApi.continue(id)
      navigate(`/chats/${chat_id}`)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to continue session')
      setContinuing(false)
    }
  }

  const messages = useMemo(() => detail?.messages ?? [], [detail])

  const tools = useMemo(() => toolUsage(messages), [messages])
  const files = useMemo(() => filesTouched(messages), [messages])
  const timeline = useMemo(() => outline(messages), [messages])
  // The sidebar's timeline scrolls the transcript to an event. It used to reach
  // the node through the DOM directly; with the transcript windowed the node
  // may not exist yet, so the request goes through state and the panel widens
  // its window to include the target before the scroll runs.
  const [jump, setJump] = useState<TranscriptJump | null>(null)
  const requestJump = useCallback(
    (uuid: string) => setJump(prev => ({ uuid, nonce: (prev?.nonce ?? 0) + 1 })),
    [],
  )

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400">Loading session…</div>
      </div>
    )
  }

  if (error || !detail) {
    return (
      <div className="flex flex-col h-full">
        <div className="px-4 sm:px-6 py-4 border-b border-zinc-100 dark:border-zinc-700/50">
          <button
            onClick={() => navigate('/claude-sessions')}
            className="flex items-center gap-1.5 text-xs text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300 transition-colors"
          >
            <ArrowLeft className="h-3.5 w-3.5" />
            Back to sessions
          </button>
        </div>
        <div className="flex flex-col items-center justify-center flex-1 text-center">
          <p className="text-sm text-zinc-500">{error ?? 'Session not found.'}</p>
        </div>
      </div>
    )
  }

  // Totals mirror the sessions list: main thread plus delegated sub-agents.
  const usage = detail.usage
  const sub = detail.subagent_usage
  const inTokens = (usage?.input_tokens ?? 0) + (sub?.input_tokens ?? 0)
  const outTokens = (usage?.output_tokens ?? 0) + (sub?.output_tokens ?? 0)
  const cacheWrite = (usage?.cache_creation_tokens ?? 0) + (sub?.cache_creation_tokens ?? 0)
  const cacheRead = (usage?.cache_read_tokens ?? 0) + (sub?.cache_read_tokens ?? 0)
  const totalCost = (detail.cost?.total_usd ?? 0) + (detail.subagent_cost?.total_usd ?? 0)
  const unpricedModels = detail.unpriced_models ?? []
  const partiallyPriced = unpricedModels.length > 0
  const delegatedTokens =
    (sub?.input_tokens ?? 0) +
    (sub?.output_tokens ?? 0) +
    (sub?.cache_creation_tokens ?? 0) +
    (sub?.cache_read_tokens ?? 0)

  const durationMs =
    new Date(detail.last_activity).getTime() - new Date(detail.start_time).getTime()
  const isActive = Date.now() - new Date(detail.last_activity).getTime() < ACTIVE_WINDOW_MS
  const title = detail.display_title || detail.preview || `Session ${(id ?? '').slice(0, 8)}`

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="shrink-0 border-b border-zinc-200 dark:border-zinc-700/60 px-4 sm:px-7 pt-3.5 pb-3 flex flex-col gap-2.5">
        <button
          onClick={() => navigate('/claude-sessions')}
          className="flex items-center gap-1.5 text-[12.5px] text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-100 transition-colors w-fit"
        >
          <ArrowLeft className="h-3.5 w-3.5" />
          Back to sessions
        </button>

        <div className="flex items-start gap-4">
          <div className="flex flex-col gap-2 min-w-0 flex-1">
            <div className="flex items-center gap-2.5 min-w-0">
              {editingTitle ? (
                <input
                  ref={titleInputRef}
                  value={titleDraft}
                  onChange={e => setTitleDraft(e.target.value)}
                  onBlur={saveTitle}
                  onKeyDown={e => {
                    if (e.key === 'Enter') e.currentTarget.blur()
                    if (e.key === 'Escape') setEditingTitle(false)
                  }}
                  className="flex-1 min-w-0 text-[21px] font-bold tracking-[-0.02em] text-zinc-900 dark:text-zinc-100 bg-transparent border-b border-zinc-400 dark:border-zinc-500 outline-none"
                  autoFocus
                />
              ) : (
                <>
                  <h1 className="text-[21px] font-bold tracking-[-0.02em] text-zinc-900 dark:text-zinc-100 truncate">
                    {title}
                  </h1>
                  <button
                    type="button"
                    onClick={startEditingTitle}
                    className="text-xs text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-100 shrink-0 transition-colors"
                  >
                    Rename
                  </button>
                </>
              )}
              {isActive && (
                <span
                  title="Transcript was written to in the last 10 minutes"
                  className="inline-flex items-center h-5 rounded-full px-2.5 text-[11px] bg-blue-500/10 text-blue-600 dark:text-blue-400 shrink-0"
                >
                  Active
                </span>
              )}
            </div>

            <SessionMetaRow detail={detail} sessionId={id} />
          </div>

          <div className="flex gap-2 shrink-0 pt-0.5">
            <button
              onClick={() => {
                if (!id) return
                const next = !detail.is_favorite
                setDetail(prev => (prev ? { ...prev, is_favorite: next } : prev))
                claudeSessionsApi.toggleFavorite(id, next).catch(() => {
                  setDetail(prev => (prev ? { ...prev, is_favorite: !next } : prev))
                })
              }}
              title={detail.is_favorite ? 'Remove from favorites' : 'Add to favorites'}
              className={`h-[34px] w-[34px] flex items-center justify-center rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 transition-colors ${
                detail.is_favorite
                  ? 'text-amber-400'
                  : 'text-zinc-400 dark:text-zinc-500 hover:text-amber-400'
              }`}
            >
              <Star className={`h-4 w-4 ${detail.is_favorite ? 'fill-amber-400' : ''}`} />
            </button>
            <button
              onClick={() => navigate(`/claude-sessions/${id ?? ''}/journey`)}
              className="flex items-center gap-1.5 h-[34px] px-3 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-[13px] text-zinc-700 dark:text-zinc-200 hover:bg-zinc-50 dark:hover:bg-zinc-800 transition-colors"
            >
              <Activity className="h-3.5 w-3.5" />
              Journey
            </button>
            <button
              onClick={() => handleContinue()}
              disabled={continuing}
              className="flex items-center gap-1.5 h-[34px] px-4 rounded-lg bg-zinc-900 dark:bg-zinc-100 text-[13px] font-medium text-white dark:text-zinc-900 hover:bg-zinc-800 dark:hover:bg-zinc-200 disabled:opacity-50 transition-colors"
            >
              {continuing ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Play className="h-3.5 w-3.5" />
              )}
              {continuing ? 'Opening…' : 'Resume'}
            </button>
          </div>
        </div>

        {/* Summary strip. The 1px grid gap is the divider — the background shows
            through between the cells. */}
        <div className="grid grid-cols-3 sm:grid-cols-6 gap-px bg-zinc-200 dark:bg-zinc-700/60 border border-zinc-200 dark:border-zinc-700/60 rounded-[10px] overflow-hidden mt-0.5">
          <StatCell label="Input" value={formatTokens(inTokens)} />
          <StatCell label="Output" value={formatTokens(outTokens)} />
          <StatCell label="Cache write" value={formatTokens(cacheWrite)} />
          <StatCell label="Cache read" value={formatTokens(cacheRead)} />
          <StatCell label="Duration" value={durationMs > 0 ? formatDuration(durationMs) : '—'} />
          <StatCell label="Cost" value={`${partiallyPriced ? '~' : ''}${formatCost(totalCost)}`} />
        </div>
        {partiallyPriced && (
          // The total is a floor, so say which models it left out rather than
          // letting an understated figure read as complete.
          <p className="text-[11.5px] text-amber-600 dark:text-amber-400">
            Cost excludes {unpricedModels.join(', ')} (no published rate).
          </p>
        )}
        {/* The strip's figures include delegated work and survive compaction,
            but nothing said so: a session with 35 sub-agents looked exactly
            like one that ran everything on the main thread. */}
        {(delegatedTokens > 0 || (detail.compaction_count ?? 0) > 0) && (
          <p className="text-[11.5px] text-zinc-500 dark:text-zinc-400">
            {delegatedTokens > 0 && (
              <>
                Includes {formatTokens(delegatedTokens)} tokens and{' '}
                {formatCost(detail.subagent_cost?.total_usd ?? 0)} from {detail.subagent_count}{' '}
                delegated sub-agent
                {detail.subagent_count === 1 ? '' : 's'}.
              </>
            )}
            {delegatedTokens > 0 && (detail.compaction_count ?? 0) > 0 && ' '}
            {(detail.compaction_count ?? 0) > 0 && (
              <>
                Compacted {detail.compaction_count}×
                {(detail.dropped_tokens ?? 0) > 0 &&
                  `, dropping ${formatTokens(detail.dropped_tokens ?? 0)} tokens`}
                .
              </>
            )}
          </p>
        )}
      </div>

      {/* Body. Side by side the two columns are independent scroll panes rather
          than one page scroll with a sticky sidebar: a sidebar taller than the
          viewport pins at the top and its lower cards become unreachable, which
          a long tool list or file list reaches easily. Stacked (below xl) the
          whole body scrolls as one, so nothing is trapped there either. */}
      <div className="flex-1 min-h-0 overflow-y-auto xl:overflow-hidden xl:flex xl:gap-7 px-4 sm:px-7 pt-5 pb-14 xl:pb-5 max-w-[1600px]">
        <div className="min-w-0 xl:flex-1 xl:h-full xl:overflow-y-auto">
          <TranscriptPanel messages={messages} isActive={isActive} jump={jump} />
        </div>

        <div className="mt-6 xl:mt-0 xl:w-[300px] xl:shrink-0 xl:h-full xl:overflow-y-auto">
          <SessionSidebar
            timeline={timeline}
            tools={tools}
            files={files}
            todos={detail.todos}
            subagents={detail.subagents}
            sessionId={id}
            onJump={requestJump}
          />
        </div>
      </div>
    </div>
  )
}
