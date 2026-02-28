import { useState, useEffect, useCallback } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { claudeSessionsApi } from '@/lib/api'
import type { ClaudeSessionDetail, ClaudeMessage, ClaudeNormalizedBlock, ClaudeTodo } from '@/types'
import { formatRelativeTime } from '@/lib/utils'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  ArrowLeft,
  ChevronDown,
  ChevronRight,
  GitBranch,
  Folder,
  Cpu,
  Zap,
  CheckCircle2,
  Circle,
  Clock,
  Play,
  Loader2,
} from 'lucide-react'

// ── Helpers ───────────────────────────────────────────────────────────────────

function formatTokens(n: number): string {
  if (!n) return '—'
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return String(n)
}

function shortPath(path: string): string {
  return path.replace(/^\/home\/[^/]+\//, '~/')
}

function todoIcon(status: string) {
  if (status === 'completed')
    return <CheckCircle2 className="h-3.5 w-3.5 text-green-500 shrink-0" />
  if (status === 'in_progress')
    return <Clock className="h-3.5 w-3.5 text-yellow-500 shrink-0 animate-pulse" />
  return <Circle className="h-3.5 w-3.5 text-zinc-400 shrink-0" />
}

// ── Block renderer ────────────────────────────────────────────────────────────

function ThinkingBlock({ text }: Readonly<{ text: string }>) {
  const [open, setOpen] = useState(false)
  return (
    <div className="rounded-md border border-purple-200 dark:border-purple-900/50 bg-purple-50 dark:bg-purple-950/20 text-xs overflow-hidden">
      <button
        className="flex w-full items-center gap-1.5 px-3 py-1.5 text-purple-700 dark:text-purple-400 hover:bg-purple-100 dark:hover:bg-purple-900/30 transition-colors text-left"
        onClick={() => setOpen(o => !o)}
      >
        {open ? (
          <ChevronDown className="h-3 w-3 shrink-0" />
        ) : (
          <ChevronRight className="h-3 w-3 shrink-0" />
        )}
        <Cpu className="h-3 w-3 shrink-0" />
        <span>Thinking</span>
      </button>
      {open && (
        <pre className="px-3 pb-2 text-purple-800 dark:text-purple-300 whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed border-t border-purple-200 dark:border-purple-900/50">
          {text}
        </pre>
      )}
    </div>
  )
}

function ToolUseBlock({ block }: Readonly<{ block: ClaudeNormalizedBlock }>) {
  const [open, setOpen] = useState(false)
  const toolName = block.name ?? 'unknown'
  const inputStr = block.input ? JSON.stringify(block.input, null, 2) : ''

  // Extract a short summary from the input for common tools.
  let summary = ''
  if (block.input) {
    const inp = block.input
    if (toolName === 'Read' || toolName === 'Write' || toolName === 'Edit') {
      summary = String(inp.file_path ?? inp.filePath ?? '')
    } else if (toolName === 'Bash') {
      summary = String(inp.command ?? '').slice(0, 60)
    } else if (toolName === 'Glob' || toolName === 'Grep') {
      summary = String(inp.pattern ?? inp.query ?? '')
    } else if (toolName === 'WebFetch' || toolName === 'WebSearch') {
      summary = String(inp.url ?? inp.query ?? '')
    } else if (toolName === 'Task') {
      summary = String(inp.description ?? '').slice(0, 60)
    }
  }

  return (
    <div className="rounded-md border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/50 text-xs overflow-hidden">
      <button
        className="flex w-full items-center gap-1.5 px-3 py-1.5 text-zinc-600 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-700/50 transition-colors text-left"
        onClick={() => setOpen(o => !o)}
      >
        {open ? (
          <ChevronDown className="h-3 w-3 shrink-0" />
        ) : (
          <ChevronRight className="h-3 w-3 shrink-0" />
        )}
        <Badge
          variant="secondary"
          className="text-[10px] py-0 h-3.5 bg-zinc-200 dark:bg-zinc-700 text-zinc-600 dark:text-zinc-300 border-0 font-mono px-1"
        >
          {toolName}
        </Badge>
        {summary && (
          <span className="text-zinc-500 dark:text-zinc-400 font-mono truncate">{summary}</span>
        )}
      </button>
      {open && inputStr && (
        <pre className="px-3 pb-2 text-zinc-600 dark:text-zinc-400 whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed border-t border-zinc-200 dark:border-zinc-700">
          {inputStr}
        </pre>
      )}
    </div>
  )
}

function MessageBlocks({ blocks }: Readonly<{ blocks: ClaudeNormalizedBlock[] }>) {
  return (
    <div className="flex flex-col gap-2">
      {blocks.map((b, i) => {
        const blockKey = b.id ?? `${b.type}-${i}`
        if (b.type === 'thinking') {
          return <ThinkingBlock key={`thinking-${blockKey}`} text={b.text ?? ''} />
        }
        if (b.type === 'text') {
          return (
            <p
              key={`text-${blockKey}`}
              className="text-sm text-zinc-800 dark:text-zinc-200 whitespace-pre-wrap leading-relaxed"
            >
              {b.text}
            </p>
          )
        }
        if (b.type === 'tool_use') {
          return <ToolUseBlock key={`tool-${blockKey}`} block={b} />
        }
        return null
      })}
    </div>
  )
}

// ── Progress children ─────────────────────────────────────────────────────────

function ProgressChildren({ messages }: Readonly<{ messages: ClaudeMessage[] }>) {
  const [open, setOpen] = useState(false)
  if (!messages || messages.length === 0) return null
  return (
    <div className="mt-2">
      <button
        className="flex items-center gap-1 text-xs text-zinc-400 dark:text-zinc-500 hover:text-zinc-600 dark:hover:text-zinc-400 transition-colors"
        onClick={() => setOpen(o => !o)}
      >
        {open ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
        {messages.length} sub-agent event{messages.length === 1 ? '' : 's'}
      </button>
      {open && (
        <div className="mt-1 pl-3 border-l border-zinc-200 dark:border-zinc-700 flex flex-col gap-0.5">
          {messages.map(c => (
            <div key={c.uuid} className="text-xs text-zinc-400 dark:text-zinc-500 font-mono">
              {new Date(c.timestamp).toLocaleTimeString()}
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

// ── Message components ────────────────────────────────────────────────────────

function UserMessage({ msg }: Readonly<{ msg: ClaudeMessage }>) {
  return (
    <div className="flex gap-3">
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-200 dark:bg-zinc-700 text-zinc-600 dark:text-zinc-300 shrink-0 text-xs font-semibold mt-0.5">
        U
      </div>
      <div className="flex-1 min-w-0">
        <p className="text-sm text-zinc-800 dark:text-zinc-200 whitespace-pre-wrap leading-relaxed">
          {msg.content}
        </p>
        <span className="text-xs text-zinc-400 dark:text-zinc-500 mt-0.5 block">
          {formatRelativeTime(msg.timestamp)}
        </span>
      </div>
    </div>
  )
}

function AssistantMessage({ msg }: Readonly<{ msg: ClaudeMessage }>) {
  const hasBlocks = (msg.blocks ?? []).length > 0
  const hasChildren = (msg.children ?? []).length > 0

  return (
    <div className="flex gap-3">
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 shrink-0 text-xs font-semibold mt-0.5">
        A
      </div>
      <div className="flex-1 min-w-0">
        {hasBlocks ? (
          <MessageBlocks blocks={msg.blocks!} />
        ) : (
          <p className="text-sm text-zinc-400 dark:text-zinc-500 italic">No text content</p>
        )}
        {hasChildren && <ProgressChildren messages={msg.children!} />}
        <div className="flex items-center gap-3 mt-1">
          <span className="text-xs text-zinc-400 dark:text-zinc-500">
            {formatRelativeTime(msg.timestamp)}
          </span>
          {msg.usage && (
            <span className="flex items-center gap-0.5 text-xs text-zinc-400 dark:text-zinc-500">
              <Zap className="h-2.5 w-2.5" />
              {formatTokens(msg.usage.input_tokens)}↑&nbsp;{formatTokens(msg.usage.output_tokens)}↓
            </span>
          )}
        </div>
      </div>
    </div>
  )
}

// ── Todos ─────────────────────────────────────────────────────────────────────

function TodosSection({ todos }: Readonly<{ todos: ClaudeTodo[] }>) {
  const [open, setOpen] = useState(false)
  if (!todos || todos.length === 0) return null

  const completed = todos.filter(t => t.status === 'completed').length

  return (
    <div className="rounded-md border border-zinc-200 dark:border-zinc-700 overflow-hidden">
      <button
        className="flex w-full items-center gap-2 px-4 py-2.5 bg-zinc-50 dark:bg-zinc-800/50 hover:bg-zinc-100 dark:hover:bg-zinc-700/50 transition-colors text-left"
        onClick={() => setOpen(o => !o)}
      >
        {open ? (
          <ChevronDown className="h-3.5 w-3.5 text-zinc-400" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 text-zinc-400" />
        )}
        <span className="text-xs font-medium text-zinc-700 dark:text-zinc-300">Todos</span>
        <span className="text-xs text-zinc-400 dark:text-zinc-500">
          {completed}/{todos.length} completed
        </span>
      </button>
      {open && (
        <div className="divide-y divide-zinc-100 dark:divide-zinc-700/50">
          {todos.map((todo, i) => (
            <div
              key={`todo-${todo.content.slice(0, 50)}-${i}`}
              className="flex items-start gap-2 px-4 py-2"
            >
              {todoIcon(todo.status)}
              <span
                className={`text-xs leading-relaxed ${
                  todo.status === 'completed'
                    ? 'text-zinc-400 dark:text-zinc-500 line-through'
                    : 'text-zinc-700 dark:text-zinc-300'
                }`}
              >
                {todo.content}
              </span>
            </div>
          ))}
        </div>
      )}
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
            Back to Sessions
          </button>
        </div>
        <div className="flex flex-col items-center justify-center flex-1 text-center">
          <p className="text-sm text-zinc-500">{error ?? 'Session not found.'}</p>
        </div>
      </div>
    )
  }

  const totalTokens = (detail.usage?.input_tokens ?? 0) + (detail.usage?.output_tokens ?? 0)

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="border-b border-zinc-100 dark:border-zinc-700/50 px-4 sm:px-6 py-4 shrink-0">
        <button
          onClick={() => navigate('/claude-sessions')}
          className="flex items-center gap-1.5 text-xs text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-300 transition-colors mb-3"
        >
          <ArrowLeft className="h-3.5 w-3.5" />
          Back to Sessions
        </button>
        <div className="flex items-start justify-between gap-3">
          <div className="flex-1 min-w-0">
            <p className="text-base font-semibold text-zinc-900 dark:text-zinc-100 truncate">
              {detail.preview || 'Session ' + (id ?? '').slice(0, 8)}
            </p>
            {/* Session meta */}
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1 mt-1.5">
              {detail.cwd && (
                <span className="flex items-center gap-1 text-xs text-zinc-500 dark:text-zinc-400">
                  <Folder className="h-3 w-3" />
                  <span className="font-mono">{shortPath(detail.cwd)}</span>
                </span>
              )}
              {detail.git_branch && (
                <span className="flex items-center gap-1 text-xs text-zinc-500 dark:text-zinc-400">
                  <GitBranch className="h-3 w-3" />
                  <span className="font-mono">{detail.git_branch}</span>
                </span>
              )}
              {detail.model && (
                <Badge
                  variant="secondary"
                  className="text-xs py-0 h-4 bg-zinc-100 dark:bg-zinc-700 text-zinc-600 dark:text-zinc-300 border-0 font-mono font-normal"
                >
                  {detail.model}
                </Badge>
              )}
              <span className="text-xs text-zinc-400 dark:text-zinc-500 font-mono">
                {(id ?? '').slice(0, 8)}…
              </span>
            </div>
          </div>
          <Button
            size="sm"
            className="gap-1.5 bg-zinc-900 hover:bg-zinc-800 text-white text-xs h-8 shrink-0"
            onClick={() => handleContinue()}
            disabled={continuing}
          >
            {continuing ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Play className="h-3.5 w-3.5" />
            )}
            {continuing ? 'Opening…' : 'Continue'}
          </Button>
        </div>
      </div>

      {/* Token usage banner */}
      {totalTokens > 0 && (
        <div className="flex items-center gap-4 px-4 sm:px-6 py-2 border-b border-zinc-100 dark:border-zinc-700/50 bg-zinc-50 dark:bg-zinc-900/50 shrink-0">
          <Zap className="h-3.5 w-3.5 text-zinc-400 shrink-0" />
          <div className="flex items-center gap-4 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              <span className="font-medium text-zinc-700 dark:text-zinc-300">
                {formatTokens(detail.usage.input_tokens)}
              </span>{' '}
              input
            </span>
            <span>
              <span className="font-medium text-zinc-700 dark:text-zinc-300">
                {formatTokens(detail.usage.output_tokens)}
              </span>{' '}
              output
            </span>
            {detail.usage.cache_creation_tokens > 0 && (
              <span>
                <span className="font-medium text-zinc-700 dark:text-zinc-300">
                  {formatTokens(detail.usage.cache_creation_tokens)}
                </span>{' '}
                cache write
              </span>
            )}
            {detail.usage.cache_read_tokens > 0 && (
              <span>
                <span className="font-medium text-zinc-700 dark:text-zinc-300">
                  {formatTokens(detail.usage.cache_read_tokens)}
                </span>{' '}
                cache read
              </span>
            )}
          </div>
        </div>
      )}

      {/* Body */}
      <div className="flex-1 overflow-y-auto px-4 sm:px-6 py-4 flex flex-col gap-4">
        {/* Todos */}
        {detail.todos && detail.todos.length > 0 && <TodosSection todos={detail.todos} />}

        {/* Conversation */}
        {detail.messages.length === 0 ? (
          <p className="text-sm text-zinc-400 text-center py-8">No messages in this session.</p>
        ) : (
          <div className="flex flex-col gap-5">
            {detail.messages.map(msg => {
              if (msg.role === 'user') {
                return <UserMessage key={msg.uuid} msg={msg} />
              }
              if (msg.role === 'assistant') {
                return <AssistantMessage key={msg.uuid} msg={msg} />
              }
              return null
            })}
          </div>
        )}
      </div>
    </div>
  )
}
