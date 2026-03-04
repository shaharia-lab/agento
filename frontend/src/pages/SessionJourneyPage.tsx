import { useState, useEffect, useCallback } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { claudeSessionsApi } from '@/lib/api'
import type { SessionJourney, JourneyTurn, JourneyStep } from '@/types'
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
  Terminal,
  FileText,
  Globe,
  Wrench,
  XCircle,
  CheckCircle,
  Bot,
  Plug,
} from 'lucide-react'

// ── Helpers ───────────────────────────────────────────────────────────────────

function formatTokens(n: number): string {
  if (!n) return '0'
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return String(n)
}

function shortPath(path: string): string {
  return path.replace(/^\/home\/[^/]+\//, '~/')
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`
  const min = Math.floor(ms / 60_000)
  const sec = Math.round((ms % 60_000) / 1000)
  if (min < 60) return `${min}m ${sec}s`
  const hr = Math.floor(min / 60)
  const remMin = min % 60
  return `${hr}h ${remMin}m`
}

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
      const toolName = (data?.tool_name as string) || ''
      return getToolCallStyle(toolName)
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
    case 'bash_output':
      return {
        icon: <Terminal className="h-3.5 w-3.5" />,
        label: 'Bash Output',
        color: 'text-orange-600 dark:text-orange-400',
        bg: 'bg-orange-50 dark:bg-orange-950/30 border-orange-200 dark:border-orange-900/50',
      }
    case 'sub_agent':
      return {
        icon: <Bot className="h-3.5 w-3.5" />,
        label: 'Sub-Agent',
        color: 'text-indigo-600 dark:text-indigo-400',
        bg: 'bg-indigo-50 dark:bg-indigo-950/30 border-indigo-200 dark:border-indigo-900/50',
      }
    case 'skill':
      return {
        icon: <Zap className="h-3.5 w-3.5" />,
        label: 'Skill',
        color: 'text-violet-600 dark:text-violet-400',
        bg: 'bg-violet-50 dark:bg-violet-950/30 border-violet-200 dark:border-violet-900/50',
      }
    case 'mcp_tool':
      return {
        icon: <Plug className="h-3.5 w-3.5" />,
        label: 'MCP Tool',
        color: 'text-slate-600 dark:text-slate-400',
        bg: 'bg-slate-50 dark:bg-slate-950/30 border-slate-200 dark:border-slate-800/50',
      }
    case 'thinking_duration':
      return {
        icon: <Clock className="h-3.5 w-3.5" />,
        label: 'Turn Duration',
        color: 'text-zinc-500 dark:text-zinc-400',
        bg: 'bg-zinc-50 dark:bg-zinc-800/50 border-zinc-200 dark:border-zinc-700/50',
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

function getToolCallStyle(toolName: string): StepStyle {
  if (toolName === 'Bash') {
    return {
      icon: <Terminal className="h-3.5 w-3.5" />,
      label: 'Bash',
      color: 'text-orange-600 dark:text-orange-400',
      bg: 'bg-orange-50 dark:bg-orange-950/30 border-orange-200 dark:border-orange-900/50',
    }
  }
  if (['Read', 'Write', 'Edit', 'Glob', 'Grep', 'NotebookEdit'].includes(toolName)) {
    return {
      icon: <FileText className="h-3.5 w-3.5" />,
      label: toolName,
      color: 'text-yellow-600 dark:text-yellow-400',
      bg: 'bg-yellow-50 dark:bg-yellow-950/30 border-yellow-200 dark:border-yellow-900/50',
    }
  }
  if (['WebFetch', 'WebSearch'].includes(toolName)) {
    return {
      icon: <Globe className="h-3.5 w-3.5" />,
      label: toolName,
      color: 'text-teal-600 dark:text-teal-400',
      bg: 'bg-teal-50 dark:bg-teal-950/30 border-teal-200 dark:border-teal-900/50',
    }
  }
  return {
    icon: <Wrench className="h-3.5 w-3.5" />,
    label: toolName || 'Tool',
    color: 'text-zinc-500 dark:text-zinc-400',
    bg: 'bg-zinc-50 dark:bg-zinc-800/50 border-zinc-200 dark:border-zinc-700/50',
  }
}

// ── Step content components ──────────────────────────────────────────────────

function ExpandableCode({
  label,
  content,
  errorStyle,
}: Readonly<{ label: string; content: string; errorStyle?: boolean }>) {
  const [expanded, setExpanded] = useState(false)
  if (!content) return null
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
          {content}
        </pre>
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

function ToolCallContent({ data }: Readonly<{ data: Record<string, unknown> }>) {
  const toolName = (data?.tool_name as string) || 'unknown'
  const input = data?.input ? JSON.stringify(data.input, null, 2) : ''
  return (
    <div>
      <span className="text-xs font-medium text-zinc-600 dark:text-zinc-300">{toolName}</span>
      {input && <ExpandableCode label="input" content={input} />}
    </div>
  )
}

function MCPToolContent({ data }: Readonly<{ data: Record<string, unknown> }>) {
  const server = (data?.server_name as string) || ''
  const tool = (data?.tool_name as string) || ''
  const status = (data?.status as string) || ''
  return (
    <p className="text-xs text-zinc-500 dark:text-zinc-400">
      {server && <span className="font-mono">{server}</span>}
      {server && tool && <span className="mx-1">/</span>}
      {tool && <span className="font-mono font-medium">{tool}</span>}
      {status && <span className="ml-2 text-zinc-400">({status})</span>}
    </p>
  )
}

function StepContent({ step }: Readonly<{ step: JourneyStep }>) {
  const data = step.data

  switch (step.type) {
    case 'user_input':
    case 'text_response':
      return (
        <p className="text-sm text-zinc-700 dark:text-zinc-300 whitespace-pre-wrap break-words leading-relaxed">
          {data?.content as string}
        </p>
      )
    case 'thinking':
      return <ThinkingContent data={data} />
    case 'tool_call':
      return <ToolCallContent data={data} />
    case 'tool_result':
      return (
        <ExpandableCode
          label={`output${data?.is_error ? ' (error)' : ''}`}
          content={(data?.content as string) || ''}
          errorStyle={!!data?.is_error}
        />
      )
    case 'bash_output':
      return <ExpandableCode label="output" content={(data?.output as string) || ''} />
    case 'sub_agent':
      return (
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {(data?.prompt as string) || 'Sub-agent invocation'}
        </p>
      )
    case 'skill':
      return (
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {(data?.prompt as string) || 'Skill invocation'}
        </p>
      )
    case 'mcp_tool':
      return <MCPToolContent data={data} />
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

function StepRow({ step }: Readonly<{ step: JourneyStep }>) {
  const style = getStepStyle(step)

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

function TurnCard({ turn }: Readonly<{ turn: JourneyTurn }>) {
  const [open, setOpen] = useState(turn.number <= 3) // auto-expand first 3 turns

  return (
    <div className="border border-zinc-200 dark:border-zinc-700/50 rounded-lg overflow-hidden">
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
            <StepRow key={`${step.type}-${idx}`} step={step} />
          ))}
        </div>
      )}
    </div>
  )
}

// ── Token usage bar ─────────────────────────────────────────────────────────

function TokenUsageBar({ journey }: Readonly<{ journey: SessionJourney }>) {
  const u = journey.usage
  const total = u.input_tokens + u.output_tokens + u.cache_creation_tokens + u.cache_read_tokens
  if (total === 0) return null

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
              <span className="flex items-center gap-1 text-xs text-zinc-500 dark:text-zinc-400">
                <Clock className="h-3 w-3" />
                {formatDuration(journey.total_duration_ms)}
              </span>
              <span className="text-xs text-zinc-500 dark:text-zinc-400">
                {journey.total_turns} turn{journey.total_turns === 1 ? '' : 's'}
              </span>
              <span className="flex items-center gap-0.5 text-xs text-zinc-500 dark:text-zinc-400">
                <Zap className="h-3 w-3" />
                {formatTokens(journey.usage.input_tokens)} in /{' '}
                {formatTokens(journey.usage.output_tokens)} out
              </span>
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
            {journey.turns.map(turn => (
              <TurnCard key={turn.number} turn={turn} />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
