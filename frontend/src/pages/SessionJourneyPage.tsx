import { useState, useEffect, useCallback } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { claudeSessionsApi } from '@/lib/api'
import type { SessionJourney, JourneyTurn, JourneyStep, ClaudeTokenUsage } from '@/types'
import { Badge } from '@/components/ui/badge'
import {
  ArrowLeft,
  ChevronDown,
  ChevronRight,
  GitBranch,
  Folder,
  Clock,
  Zap,
  MessageSquare,
  Brain,
  MessageCircle,
  Wrench,
  XCircle,
  CheckCircle,
  Bot,
  Scissors,
} from 'lucide-react'
import { formatTokens, shortPath, formatDuration } from '@/lib/format'

// ── Helpers ───────────────────────────────────────────────────────────────────

function formatTime(ts: string): string {
  return new Date(ts).toLocaleTimeString([], {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

// ── Step icon/color mapping ─────────────────────────────────────────────────

interface StepStyle {
  icon: React.ReactNode
  label: string
  color: string // tailwind text color class
  bg: string // tailwind bg color class
}

function getStepStyle(step: JourneyStep): StepStyle {
  const data = step.data
  switch (step.type) {
    case 'user_input':
      return {
        icon: <MessageSquare className="h-3.5 w-3.5" />,
        label: 'User Input',
        color: 'text-blue-600 dark:text-blue-400',
        bg: 'bg-blue-50 dark:bg-blue-950/30 border-blue-200 dark:border-blue-900/50',
      }
    case 'thinking':
      return {
        icon: <Brain className="h-3.5 w-3.5" />,
        label: 'Thinking',
        color: 'text-purple-600 dark:text-purple-400',
        bg: 'bg-purple-50 dark:bg-purple-950/30 border-purple-200 dark:border-purple-900/50',
      }
    case 'text_response':
      return {
        icon: <MessageCircle className="h-3.5 w-3.5" />,
        label: 'Response',
        color: 'text-green-600 dark:text-green-400',
        bg: 'bg-green-50 dark:bg-green-950/30 border-green-200 dark:border-green-900/50',
      }
    case 'tool_call': {
      const hasAgent = !!(data?.agent_type || data?.description)
      return {
        icon: hasAgent ? <Bot className="h-3.5 w-3.5" /> : <Wrench className="h-3.5 w-3.5" />,
        label: hasAgent ? 'Sub-Agent Task' : 'Tool Call',
        color: 'text-indigo-600 dark:text-indigo-400',
        bg: 'bg-indigo-50 dark:bg-indigo-950/30 border-indigo-200 dark:border-indigo-900/50',
      }
    }
    case 'tool_result': {
      const isErr = !!data?.is_error
      return {
        icon: isErr ? <XCircle className="h-3.5 w-3.5" /> : <CheckCircle className="h-3.5 w-3.5" />,
        label: isErr ? 'Tool Error' : 'Tool Result',
        color: isErr ? 'text-red-600 dark:text-red-400' : 'text-green-600 dark:text-green-400',
        bg: isErr
          ? 'bg-red-50 dark:bg-red-950/30 border-red-200 dark:border-red-900/50'
          : 'bg-green-50 dark:bg-green-950/30 border-green-200 dark:border-green-900/50',
      }
    }
    case 'sub_agent':
      return {
        icon: <Bot className="h-3.5 w-3.5" />,
        label: 'Sub-Agent',
        color: 'text-indigo-600 dark:text-indigo-400',
        bg: 'bg-indigo-50 dark:bg-indigo-950/30 border-indigo-200 dark:border-indigo-900/50',
      }
    case 'thinking_duration':
      return {
        icon: <Clock className="h-3.5 w-3.5" />,
        label: 'Turn Duration',
        color: 'text-zinc-500 dark:text-zinc-400',
        bg: 'bg-zinc-50 dark:bg-zinc-800/50 border-zinc-200 dark:border-zinc-700/50',
      }
    case 'compaction':
      return {
        icon: <Scissors className="h-3.5 w-3.5" />,
        label: 'Compaction',
        color: 'text-amber-600 dark:text-amber-400',
        bg: 'bg-amber-50 dark:bg-amber-950/30 border-amber-200 dark:border-amber-900/50',
      }
    default:
      return {
        icon: <Wrench className="h-3.5 w-3.5" />,
        label: step.type,
        color: 'text-zinc-500 dark:text-zinc-400',
        bg: 'bg-zinc-50 dark:bg-zinc-800/50 border-zinc-200 dark:border-zinc-700/50',
      }
  }
}

// ── Step content components ──────────────────────────────────────────────────

/**
 * Content that is expensive to produce is passed as a thunk, so the cost is
 * paid on expand rather than on every render of a collapsed block.
 *
 * A tool call's input used to be JSON.stringify'd eagerly for every step in
 * every open turn, whether or not anyone looked at it. On a session with
 * hundreds of tool calls carrying large inputs that is the page's single
 * biggest render cost, and it bought nothing.
 */
type LazyContent = string | (() => string)

function resolveContent(content: LazyContent): string {
  return typeof content === 'function' ? content() : content
}

function ExpandableCode({
  label,
  content,
  hasContent = true,
  errorStyle,
}: Readonly<{
  label: string
  content: LazyContent
  /** Whether there is anything to show, checked without resolving the thunk. */
  hasContent?: boolean
  errorStyle?: boolean
}>) {
  const [expanded, setExpanded] = useState(false)
  if (!hasContent) return null
  if (typeof content === 'string' && !content) return null
  return (
    <div>
      <button
        className="text-xs text-zinc-400 hover:underline"
        onClick={() => setExpanded(e => !e)}
      >
        {expanded ? 'Hide' : 'Show'} {label}
      </button>
      {expanded && (
        <pre
          className={`mt-1 text-xs font-mono whitespace-pre-wrap break-all max-h-60 overflow-y-auto rounded p-2 ${
            errorStyle
              ? 'text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-950/20'
              : 'text-zinc-500 dark:text-zinc-400 bg-zinc-100 dark:bg-zinc-800'
          }`}
        >
          {resolveContent(content)}
        </pre>
      )}
    </div>
  )
}

/**
 * How much of a message body renders before it is cut off behind a link.
 *
 * A pasted file or a long tool-driven answer can run to tens of thousands of
 * characters, and a single turn can hold dozens of them; rendering all of it as
 * one text node is what made the largest sessions — the expensive ones users
 * most want to inspect — hang the tab for 30 seconds at a time.
 */
const MAX_INLINE_CHARS = 2_000

function TruncatedText({ content }: Readonly<{ content: string }>) {
  const [expanded, setExpanded] = useState(false)
  const isLong = content.length > MAX_INLINE_CHARS

  return (
    <div>
      <p className="text-sm text-zinc-700 dark:text-zinc-300 whitespace-pre-wrap break-words leading-relaxed">
        {expanded || !isLong ? content : `${content.slice(0, MAX_INLINE_CHARS)}…`}
      </p>
      {isLong && (
        <button
          className="mt-1 text-xs text-zinc-400 hover:underline"
          onClick={() => setExpanded(e => !e)}
        >
          {expanded ? 'Show less' : `Show all ${content.length.toLocaleString()} characters`}
        </button>
      )}
    </div>
  )
}

function ThinkingContent({ data }: Readonly<{ data: Record<string, unknown> }>) {
  const [expanded, setExpanded] = useState(false)
  const preview = (data?.preview as string) || ''
  const full = (data?.full as string) || ''
  return (
    <div>
      <button
        className="text-xs text-purple-600 dark:text-purple-400 hover:underline"
        onClick={() => setExpanded(e => !e)}
      >
        {expanded ? 'Collapse' : 'Expand'} thinking
      </button>
      <pre className="mt-1 text-xs text-zinc-600 dark:text-zinc-400 whitespace-pre-wrap break-words font-mono leading-relaxed max-h-96 overflow-y-auto">
        {expanded ? full : preview}
      </pre>
    </div>
  )
}

// AgentUsageBadge renders a sub-agent's token cost in the "in+out" form the
// turn header already uses.
function AgentUsageBadge({ usage }: Readonly<{ usage: Record<string, unknown> }>) {
  const inTokens = (usage.input_tokens as number) || 0
  const outTokens = (usage.output_tokens as number) || 0
  return (
    <span className="inline-flex items-center gap-0.5 text-[10px] text-indigo-500 dark:text-indigo-400 font-mono">
      <Zap className="h-2.5 w-2.5" />
      {formatTokens(inTokens)}+{formatTokens(outTokens)}
    </span>
  )
}

// AgentStepsSummary renders a nested sub-agent's steps, collapsed by default
// behind an "N steps · M tokens" summary line — the same collapse pattern tool
// calls use.
function AgentStepsSummary({ step }: Readonly<{ step: JourneyStep }>) {
  const [expanded, setExpanded] = useState(false)
  const steps = step.steps ?? []
  if (steps.length === 0) return null
  return (
    <div className="mt-1">
      <button
        className="flex items-center gap-1 text-xs text-indigo-600 dark:text-indigo-400 hover:underline"
        onClick={() => setExpanded(e => !e)}
      >
        {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
        {steps.length} step{steps.length === 1 ? '' : 's'}
      </button>
      {expanded && (
        <div className="mt-1 pl-3 border-l border-indigo-200 dark:border-indigo-800/50 flex flex-col gap-1">
          {steps.map((s, i) => (
            <StepRow key={`${s.type}-${s.timestamp}-${i}`} step={s} depth={1} />
          ))}
        </div>
      )}
    </div>
  )
}

function ToolCallContent({ step }: Readonly<{ step: JourneyStep }>) {
  const data = step.data
  const toolName = (data?.tool_name as string) || 'unknown'
  const agentType = (data?.agent_type as string) || ''
  const description = (data?.description as string) || ''
  const agentUsage = data?.agent_usage as Record<string, unknown> | undefined
  const isAgent = !!agentType || !!description
  return (
    <div>
      <span className="text-xs font-medium text-zinc-600 dark:text-zinc-300">{toolName}</span>
      <ExpandableCode
        label="input"
        hasContent={!!data?.input}
        content={() => JSON.stringify(data.input, null, 2)}
      />
      {isAgent && (
        <div className="mt-0.5 flex flex-wrap items-center gap-2">
          {agentType && (
            <Badge
              variant="secondary"
              className="text-[10px] py-0 h-4 bg-indigo-100 dark:bg-indigo-900/40 text-indigo-600 dark:text-indigo-400 border-0 font-normal"
            >
              {agentType}
            </Badge>
          )}
          {description && (
            <span className="text-xs text-zinc-500 dark:text-zinc-400">{description}</span>
          )}
          {agentUsage && <AgentUsageBadge usage={agentUsage} />}
        </div>
      )}
      <AgentStepsSummary step={step} />
    </div>
  )
}

// SubAgentGroup renders a sub-agent whose originating tool_use is not in the
// rendered transcript (compacted away, or no toolUseId in its sidecar).
function SubAgentGroup({ step }: Readonly<{ step: JourneyStep }>) {
  const data = step.data
  const agentType = (data?.agent_type as string) || ''
  const description = (data?.description as string) || ''
  const usage = data?.usage as Record<string, unknown> | undefined
  return (
    <div>
      <div className="flex flex-wrap items-center gap-2">
        {agentType && (
          <Badge
            variant="secondary"
            className="text-[10px] py-0 h-4 bg-indigo-100 dark:bg-indigo-900/40 text-indigo-600 dark:text-indigo-400 border-0 font-normal"
          >
            {agentType}
          </Badge>
        )}
        <span className="text-xs text-zinc-600 dark:text-zinc-300">
          {description || 'Sub-agent'}
        </span>
        {usage && <AgentUsageBadge usage={usage} />}
      </div>
      <AgentStepsSummary step={step} />
    </div>
  )
}

function StepContent({ step }: Readonly<{ step: JourneyStep }>) {
  const data = step.data

  switch (step.type) {
    case 'user_input':
    case 'text_response':
      return <TruncatedText content={(data?.content as string) ?? ''} />
    case 'thinking':
      return <ThinkingContent data={data} />
    case 'tool_call':
      return <ToolCallContent step={step} />
    case 'tool_result':
      return (
        <ExpandableCode
          label={`output${data?.is_error ? ' (error)' : ''}`}
          content={(data?.content as string) || ''}
          errorStyle={!!data?.is_error}
        />
      )
    case 'sub_agent':
      return <SubAgentGroup step={step} />
    case 'compaction':
      return (
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          Context compacted
          {data?.trigger ? ` (${data.trigger as string})` : ''}
          {typeof data?.pre_tokens === 'number' && typeof data?.post_tokens === 'number'
            ? `: ${(data.pre_tokens as number).toLocaleString()} → ${(data.post_tokens as number).toLocaleString()} tokens`
            : ''}
        </p>
      )
    case 'thinking_duration':
      return (
        <p className="text-xs text-zinc-400">
          Turn completed in {formatDuration(step.duration_ms)}
        </p>
      )
    default:
      return null
  }
}

// ── Step row ──────────────────────────────────────────────────────────────────

function StepRow({ step, depth = 0 }: Readonly<{ step: JourneyStep; depth?: number }>) {
  const style = getStepStyle(step)

  // Nested (sub-agent) steps render compactly, without a timeline dot — the
  // parent's dot and connecting line already anchor the group.
  if (depth > 0) {
    return (
      <div className="flex items-baseline gap-2">
        <span className={`text-[10px] font-medium shrink-0 ${style.color}`}>{style.label}</span>
        <div className="flex-1 min-w-0">
          <StepContent step={step} />
        </div>
      </div>
    )
  }

  return (
    <div className="flex gap-3 group">
      {/* Timeline line + dot */}
      <div className="flex flex-col items-center shrink-0">
        <div
          className={`flex h-7 w-7 items-center justify-center rounded-full border ${style.bg} ${style.color}`}
        >
          {style.icon}
        </div>
        <div className="w-px flex-1 bg-zinc-200 dark:bg-zinc-700 mt-1" />
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0 pb-4">
        <div className="flex items-center gap-2 mb-1">
          <span className={`text-xs font-medium ${style.color}`}>{style.label}</span>
          <span className="text-xs text-zinc-400 dark:text-zinc-500">
            {formatTime(step.timestamp)}
          </span>
          {step.duration_ms > 0 && (
            <Badge
              variant="secondary"
              className="text-[10px] py-0 h-4 bg-zinc-100 dark:bg-zinc-700 text-zinc-500 dark:text-zinc-400 border-0 font-mono font-normal"
            >
              {formatDuration(step.duration_ms)}
            </Badge>
          )}
        </div>
        <StepContent step={step} />
      </div>
    </div>
  )
}

// ── Turn card ─────────────────────────────────────────────────────────────────

/**
 * Above this many turns nothing auto-expands.
 *
 * Below it, auto-expanding the first few turns is a convenience. Above it the
 * same behaviour is a hazard: the sessions with the most turns are also the
 * ones whose turns are largest, and rendering even three of them up front is
 * what users experienced as the page freezing on their most expensive sessions.
 */
const AUTO_EXPAND_TURN_LIMIT = 30

function TurnCard({ turn, defaultOpen }: Readonly<{ turn: JourneyTurn; defaultOpen: boolean }>) {
  const [open, setOpen] = useState(defaultOpen)

  return (
    <div
      className="border border-zinc-200 dark:border-zinc-700/50 rounded-lg overflow-hidden"
      // Lets the browser skip layout and paint for cards scrolled out of view,
      // with a size hint so the scrollbar stays stable. This is what keeps a
      // 100-turn timeline responsive without hand-rolled virtualization, which
      // would break in-page search and anchor links.
      style={{ contentVisibility: 'auto', containIntrinsicSize: 'auto 56px' }}
    >
      {/* Turn header */}
      <button
        className="flex items-center gap-3 w-full px-4 py-3 text-left hover:bg-zinc-50 dark:hover:bg-zinc-800/50 transition-colors"
        onClick={() => setOpen(o => !o)}
      >
        {open ? (
          <ChevronDown className="h-4 w-4 text-zinc-400 shrink-0" />
        ) : (
          <ChevronRight className="h-4 w-4 text-zinc-400 shrink-0" />
        )}
        <span className="text-sm font-medium text-zinc-900 dark:text-zinc-100">
          Turn {turn.number}
        </span>
        <div className="flex items-center gap-2 ml-auto flex-wrap justify-end">
          <span className="text-xs text-zinc-400 dark:text-zinc-500">
            {formatTime(turn.start_time)}
          </span>
          <Badge
            variant="secondary"
            className="text-xs py-0 h-4 bg-zinc-100 dark:bg-zinc-700 text-zinc-500 dark:text-zinc-400 border-0 font-mono font-normal"
          >
            {formatDuration(turn.duration_ms)}
          </Badge>
          {turn.tool_calls > 0 && (
            <span className="flex items-center gap-0.5 text-xs text-zinc-400 dark:text-zinc-500">
              <Wrench className="h-3 w-3" />
              {turn.tool_calls}
            </span>
          )}
          {turn.usage && (
            <span className="flex items-center gap-0.5 text-xs text-zinc-400 dark:text-zinc-500">
              <Zap className="h-3 w-3" />
              {formatTokens(turn.usage.input_tokens)}+{formatTokens(turn.usage.output_tokens)}
            </span>
          )}
        </div>
      </button>

      {/* Turn steps */}
      {open && (
        <div className="px-4 pb-3 pt-1 border-t border-zinc-100 dark:border-zinc-700/50">
          {turn.steps.map((step, idx) => (
            <StepRow key={`${step.type}-${step.timestamp}-${idx}`} step={step} />
          ))}
        </div>
      )}
    </div>
  )
}

// ── Token usage bar ─────────────────────────────────────────────────────────

/** Main thread plus delegated, the figure every other page reports. */
function totalJourneyUsage(journey: SessionJourney): ClaudeTokenUsage {
  const u = journey.usage
  const s = journey.subagent_usage
  return {
    input_tokens: u.input_tokens + s.input_tokens,
    output_tokens: u.output_tokens + s.output_tokens,
    cache_creation_tokens: u.cache_creation_tokens + s.cache_creation_tokens,
    cache_creation_5m_tokens: u.cache_creation_5m_tokens + s.cache_creation_5m_tokens,
    cache_creation_1h_tokens: u.cache_creation_1h_tokens + s.cache_creation_1h_tokens,
    cache_read_tokens: u.cache_read_tokens + s.cache_read_tokens,
  }
}

function TokenUsageBar({ journey }: Readonly<{ journey: SessionJourney }>) {
  const u = totalJourneyUsage(journey)
  const total = u.input_tokens + u.output_tokens + u.cache_creation_tokens + u.cache_read_tokens
  if (total === 0) return null

  const delegated =
    journey.subagent_usage.input_tokens +
    journey.subagent_usage.output_tokens +
    journey.subagent_usage.cache_creation_tokens +
    journey.subagent_usage.cache_read_tokens

  const segments = [
    { label: 'Input', value: u.input_tokens, color: 'bg-blue-500' },
    { label: 'Output', value: u.output_tokens, color: 'bg-green-500' },
    { label: 'Cache Read', value: u.cache_read_tokens, color: 'bg-amber-500' },
    { label: 'Cache Write', value: u.cache_creation_tokens, color: 'bg-purple-500' },
  ].filter(s => s.value > 0)

  return (
    <div className="px-4 sm:px-6 py-3 border-b border-zinc-100 dark:border-zinc-700/50 bg-zinc-50 dark:bg-zinc-900/50">
      {/* Bar */}
      <div className="flex h-2 rounded-full overflow-hidden bg-zinc-200 dark:bg-zinc-700 mb-2">
        {segments.map(seg => (
          <div
            key={seg.label}
            className={`${seg.color} transition-all`}
            style={{ width: `${(seg.value / total) * 100}%` }}
            title={`${seg.label}: ${seg.value.toLocaleString()}`}
          />
        ))}
      </div>
      {/* Legend */}
      <div className="flex flex-wrap gap-x-4 gap-y-1">
        {segments.map(seg => (
          <div
            key={seg.label}
            className="flex items-center gap-1.5 text-xs text-zinc-500 dark:text-zinc-400"
          >
            <div className={`h-2 w-2 rounded-full ${seg.color}`} />
            <span>{seg.label}:</span>
            <span className="font-medium text-zinc-700 dark:text-zinc-300">
              {formatTokens(seg.value)}
            </span>
          </div>
        ))}
        {delegated > 0 && (
          <div className="flex items-center gap-1.5 text-xs text-indigo-600 dark:text-indigo-400">
            <Bot className="h-3 w-3" />
            {/* Spelled out because the header states delegated in+out while
                this bar covers all four token types; two different numbers
                labeled "delegated" a few pixels apart read as a contradiction. */}
            <span>of which delegated (all types):</span>
            <span className="font-medium">{formatTokens(delegated)}</span>
          </div>
        )}
      </div>
    </div>
  )
}

// ── Main page ─────────────────────────────────────────────────────────────────

export default function SessionJourneyPage() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()
  const [journey, setJourney] = useState<SessionJourney | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async () => {
    if (!id) return
    try {
      const j = await claudeSessionsApi.journey(id)
      setJourney(j)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load session journey')
    } finally {
      setLoading(false)
    }
  }, [id])

  useEffect(() => {
    load()
  }, [load])

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400">Loading journey...</div>
      </div>
    )
  }

  if (error || !journey) {
    return (
      <div className="flex flex-col h-full">
        <div className="px-4 sm:px-6 py-4 border-b border-zinc-100 dark:border-zinc-700/50">
          <button
            onClick={() => navigate(`/claude-sessions/${id ?? ''}`)}
            className="flex items-center gap-1.5 text-xs text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300 transition-colors"
          >
            <ArrowLeft className="h-3.5 w-3.5" />
            Back to Session
          </button>
        </div>
        <div className="flex flex-col items-center justify-center flex-1 text-center">
          <p className="text-sm text-zinc-500">{error ?? 'Session not found.'}</p>
        </div>
      </div>
    )
  }

  const totalUsage = totalJourneyUsage(journey)

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="border-b border-zinc-100 dark:border-zinc-700/50 px-4 sm:px-6 py-4 shrink-0">
        <button
          onClick={() => navigate(`/claude-sessions/${id ?? ''}`)}
          className="flex items-center gap-1.5 text-xs text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300 transition-colors mb-3"
        >
          <ArrowLeft className="h-3.5 w-3.5" />
          Back to Session
        </button>
        <div className="flex items-start justify-between gap-3">
          <div className="flex-1 min-w-0">
            <h1 className="text-base font-semibold text-zinc-900 dark:text-zinc-100 truncate">
              Session Journey
            </h1>
            {journey.summary && (
              <p className="text-sm text-zinc-500 dark:text-zinc-400 mt-0.5 truncate">
                {journey.summary}
              </p>
            )}
            {/* Session meta */}
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1 mt-2">
              {journey.cwd && (
                <span className="flex items-center gap-1 text-xs text-zinc-500 dark:text-zinc-400">
                  <Folder className="h-3 w-3" />
                  <span className="font-mono">{shortPath(journey.cwd)}</span>
                </span>
              )}
              {journey.git_branch && (
                <span className="flex items-center gap-1 text-xs text-zinc-500 dark:text-zinc-400">
                  <GitBranch className="h-3 w-3" />
                  <span className="font-mono">{journey.git_branch}</span>
                </span>
              )}
              {journey.model && (
                <Badge
                  variant="secondary"
                  className="text-xs py-0 h-4 bg-zinc-100 dark:bg-zinc-700 text-zinc-600 dark:text-zinc-300 border-0 font-mono font-normal"
                >
                  {journey.model}
                </Badge>
              )}
              {/* Active time, with the raw span in the tooltip: a resumed
                  session's span includes every idle day between sittings, so
                  showing it as "duration" reported a 6-hour session as 678h. */}
              <span
                className="flex items-center gap-1 text-xs text-zinc-500 dark:text-zinc-400"
                title={`active time (idle gaps > 10 min excluded); span ${formatDuration(journey.total_duration_ms)}`}
              >
                <Clock className="h-3 w-3" />
                {formatDuration(journey.active_duration_ms)}
              </span>
              <span className="text-xs text-zinc-500 dark:text-zinc-400">
                {journey.total_turns} turn{journey.total_turns === 1 ? '' : 's'}
              </span>
              {/* The session's real total, matching the sessions list. The
                  header used to show main-thread usage only while the list
                  showed main + delegated, so a heavily delegating session
                  reported two different totals on two pages. Delegated work is
                  named rather than merged, so the split stays legible. */}
              <span className="flex items-center gap-0.5 text-xs text-zinc-500 dark:text-zinc-400">
                <Zap className="h-3 w-3" />
                {formatTokens(totalUsage.input_tokens)} in /{' '}
                {formatTokens(totalUsage.output_tokens)} out
              </span>
              {journey.subagent_count > 0 && (
                <span
                  className="flex items-center gap-1 text-xs text-indigo-600 dark:text-indigo-400"
                  title="Tokens spent by sub-agents this session delegated to, included in the total above"
                >
                  <Bot className="h-3 w-3" />
                  {journey.subagent_count} sub-agent{journey.subagent_count === 1 ? '' : 's'} ·{' '}
                  {formatTokens(
                    journey.subagent_usage.input_tokens + journey.subagent_usage.output_tokens,
                  )}{' '}
                  in+out delegated
                </span>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* Token usage bar */}
      <TokenUsageBar journey={journey} />

      {/* Timeline */}
      <div className="flex-1 overflow-y-auto px-4 sm:px-6 py-4">
        {journey.turns.length === 0 ? (
          <p className="text-sm text-zinc-400 text-center py-8">No turns in this session.</p>
        ) : (
          <div className="flex flex-col gap-3">
            {journey.turns.length > AUTO_EXPAND_TURN_LIMIT && (
              <p className="text-xs text-zinc-400 dark:text-zinc-500">
                {journey.turns.length} turns, all collapsed by default. Open the ones you need; each
                renders its content only when expanded.
              </p>
            )}
            {journey.turns.map(turn => (
              <TurnCard
                key={turn.number}
                turn={turn}
                defaultOpen={journey.turns.length <= AUTO_EXPAND_TURN_LIMIT && turn.number <= 3}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
