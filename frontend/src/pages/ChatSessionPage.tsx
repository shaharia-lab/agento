import { Fragment, useState, useEffect, useRef, useCallback } from 'react'
import { useParams, useNavigate, useLocation } from 'react-router-dom'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { chatsApi, sendMessage, provideInput, permissionResponse, stopSession } from '@/lib/api'
import { applyThinkingDelta, applyTextDelta, applyToolUseBlocks } from '@/lib/streamingBlocks'
import type {
  ChatDetail,
  ChatMessage,
  MessageBlock,
  AskUserQuestionItem,
  SDKUserEvent,
  SDKToolProgressEvent,
  SDKToolUseSummaryEvent,
  SDKTaskStartedEvent,
  SDKTaskProgressEvent,
  SDKTaskNotificationEvent,
} from '@/types'
import { Textarea } from '@/components/ui/textarea'
import {
  ArrowLeft,
  Send,
  Loader2,
  ChevronDown,
  ChevronRight,
  Folder,
  Terminal,
  MessageSquare,
  Square,
  FilePen,
  FileText,
  FileEdit,
  Search,
  Globe,
  Bot,
  ShieldQuestion,
  Copy,
  Check,
  Pencil,
  type LucideIcon,
} from 'lucide-react'
import { cn } from '@/lib/utils'

export default function ChatSessionPage() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()
  const location = useLocation()
  const [detail, setDetail] = useState<ChatDetail | null>(null)
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [editingTitle, setEditingTitle] = useState(false)
  const [titleDraft, setTitleDraft] = useState('')
  const titleInputRef = useRef<HTMLInputElement>(null)

  const [input, setInput] = useState('')
  const [streaming, setStreaming] = useState(false)
  // Unified ordered list of all content blocks (thinking, text, tool_use)
  // during streaming — preserves the exact chronological order they arrive.
  const [streamingBlocks, setStreamingBlocks] = useState<MessageBlock[]>([])
  const [systemStatus, setSystemStatus] = useState<string | null>(null)
  // Tool results keyed by tool_use_id — populated from SDK "user" events during streaming.
  const [streamingToolResults, setStreamingToolResults] = useState<
    Record<string, Record<string, unknown>>
  >({})
  // Tool progress keyed by tool_use_id — populated from SDK "tool_progress" events.
  const [toolProgress, setToolProgress] = useState<
    Record<string, { progress?: number; message?: string }>
  >({})
  // Tool summaries keyed by tool_use_id — populated from SDK "tool_use_summary" events.
  const [toolSummaries, setToolSummaries] = useState<Record<string, string>>({})
  // Background task events accumulated during streaming.
  const [taskEvents, setTaskEvents] = useState<
    Array<{ type: string; taskId?: string; status?: string; message?: string }>
  >([])
  // awaitingInput is set when the backend sends user_input_required — the SSE
  // stream stays open and the AskUserQuestion card becomes interactive.
  const [awaitingInput, setAwaitingInput] = useState(false)
  // permissionRequest is set when the backend sends permission_request — the SSE
  // stream pauses until the user allows or denies the tool call.
  const [permissionRequest, setPermissionRequest] = useState<{
    toolName: string
    input: unknown
  } | null>(null)

  const bottomRef = useRef<HTMLDivElement>(null)
  const abortRef = useRef<AbortController | null>(null)
  const pendingSent = useRef(false)

  const scrollToBottom = useCallback(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [])

  useEffect(() => {
    if (!id) return
    chatsApi
      .get(id)
      .then(d => {
        setDetail(d)
        setMessages(d.messages)
      })
      .catch(err => setError(err instanceof Error ? err.message : 'Failed to load chat'))
      .finally(() => setLoading(false))
  }, [id])

  useEffect(() => {
    scrollToBottom()
  }, [messages, streamingBlocks, scrollToBottom])

  const doSend = useCallback(
    async (content: string) => {
      if (!content.trim() || streaming || !id) return

      const userMsg: ChatMessage = {
        role: 'user',
        content,
        timestamp: new Date().toISOString(),
      }
      setMessages(prev => [...prev, userMsg])
      setStreaming(true)
      setStreamingBlocks([])
      setSystemStatus(null)
      setAwaitingInput(false)
      setPermissionRequest(null)
      setStreamingToolResults({})
      setToolProgress({})
      setToolSummaries({})
      setTaskEvents([])
      setError(null)

      abortRef.current = new AbortController()

      try {
        // Local accumulators — avoids stale-closure issues with React state.
        // `blocks` preserves the exact order blocks arrived in the stream so
        // the stored message renders correctly (thinking → text → tool_use or
        // tool_use → text, depending on what the agent did first).
        let accumulated = ''
        let blocks: MessageBlock[] = []
        // Tool results by tool_use_id, captured from SDK "user" events.
        let toolResults: Record<string, Record<string, unknown>> = {}

        await sendMessage(
          id,
          content,
          {
            onSystem: event => {
              if (event.subtype === 'status' && event.message) {
                setSystemStatus(event.message)
              }
            },
            onAssistant: event => {
              // Collect completed tool_use blocks in stream order.
              // applyToolUseBlocks returns the same reference when no tool
              // blocks are found — skip the state update to avoid a
              // needless re-render on every assistant event.
              const updated = applyToolUseBlocks(blocks, event.message.content)
              if (updated !== blocks) {
                blocks = updated
                setStreamingBlocks(blocks)
              }
            },
            onStreamEvent: event => {
              const delta = event.event.delta
              if (!delta) return
              if (delta.type === 'thinking_delta' && delta.thinking) {
                blocks = applyThinkingDelta(blocks, delta.thinking)
                setStreamingBlocks([...blocks])
              } else if (delta.type === 'text_delta' && delta.text) {
                accumulated += delta.text
                blocks = applyTextDelta(blocks, delta.text)
                setStreamingBlocks([...blocks])
              }
            },
            onUserInputRequired: () => {
              // Backend is paused waiting for us to POST /api/chats/{id}/input.
              // Make the streaming AskUserQuestion card interactive.
              setAwaitingInput(true)
            },
            onPermissionRequest: data => {
              // Backend permission handler is paused waiting for user decision.
              setPermissionRequest({ toolName: data.tool_name, input: data.input })
            },
            onToolResult: (event: SDKUserEvent) => {
              const toolUseId = event.message.content[0]?.tool_use_id
              if (toolUseId && event.tool_use_result) {
                const result = event.tool_use_result as Record<string, unknown>
                toolResults[toolUseId] = result
                setStreamingToolResults(prev => ({ ...prev, [toolUseId]: result }))
              }
            },
            onToolProgress: (event: SDKToolProgressEvent) => {
              if (event.tool_use_id) {
                setToolProgress(prev => ({
                  ...prev,
                  [event.tool_use_id]: { progress: event.progress, message: event.message },
                }))
              }
            },
            onToolUseSummary: (event: SDKToolUseSummaryEvent) => {
              if (event.tool_use_id && event.summary) {
                setToolSummaries(prev => ({ ...prev, [event.tool_use_id!]: event.summary! }))
              }
            },
            onTaskStarted: (event: SDKTaskStartedEvent) => {
              setTaskEvents(prev => {
                const next = [
                  ...prev,
                  {
                    type: 'started',
                    taskId: event.task_id,
                    status: event.status,
                    message: event.message,
                  },
                ]
                return next.length > 100 ? next.slice(-100) : next
              })
            },
            onTaskProgress: (event: SDKTaskProgressEvent) => {
              setTaskEvents(prev => {
                const next = [
                  ...prev,
                  {
                    type: 'progress',
                    taskId: event.task_id,
                    status: event.status,
                    message: event.message,
                  },
                ]
                return next.length > 100 ? next.slice(-100) : next
              })
            },
            onTaskNotification: (event: SDKTaskNotificationEvent) => {
              setTaskEvents(prev => {
                const next = [
                  ...prev,
                  {
                    type: 'notification',
                    taskId: event.task_id,
                    status: event.status,
                    message: event.message,
                  },
                ]
                return next.length > 100 ? next.slice(-100) : next
              })
            },
            onResult: event => {
              if (event.is_error) {
                const errMsg =
                  event.errors && event.errors.length > 0
                    ? event.errors.join('; ')
                    : (event.result ?? 'Unknown error')
                setError(errMsg)
                setStreamingBlocks([])
                return
              }
              // Build a rich message with ordered blocks so the render
              // reflects the exact flow: thinking → text → tool_use (or any
              // other ordering the agent chose).
              // Attach any captured tool results to their matching tool_use blocks.
              const finalBlocks: MessageBlock[] = blocks.map(b => {
                if (b.type === 'tool_use' && b.id && toolResults[b.id]) {
                  return { ...b, toolResult: toolResults[b.id] }
                }
                return b
              })
              const assistantMsg: ChatMessage = {
                role: 'assistant',
                // `accumulated` holds the concatenated text deltas for the
                // plain-text content field. Falls back to event.result when
                // no text deltas arrived (e.g. tool-only turns).
                content: accumulated || event.result,
                timestamp: new Date().toISOString(),
                blocks: finalBlocks.length > 0 ? finalBlocks : undefined,
              }
              setMessages(prev => [...prev, assistantMsg])
              // Reset per-turn local accumulators so a follow-up turn
              // (e.g. after AskUserQuestion is answered) starts clean.
              accumulated = ''
              blocks = []
              toolResults = {}
              // Clear streaming UI state — the message now owns the content.
              setStreamingBlocks([])
              setSystemStatus(null)
              // Do NOT clear awaitingInput here — if user_input_required follows
              // this result event, onUserInputRequired will set it to true.
              // The finally block handles final cleanup.

              if (detail) {
                chatsApi
                  .get(id)
                  .then(d => setDetail(d))
                  .catch(() => undefined)
              }
            },
          },
          abortRef.current.signal,
        )
      } catch (err) {
        if ((err as Error).name !== 'AbortError') {
          setError(err instanceof Error ? err.message : 'Failed to send message')
        }
      } finally {
        setStreaming(false)
        setStreamingBlocks([])
        setSystemStatus(null)
        setAwaitingInput(false)
        setPermissionRequest(null)
        setStreamingToolResults({})
        setToolProgress({})
        setToolSummaries({})
        setTaskEvents([])
      }
    },
    [id, streaming, detail],
  )

  // Auto-send first message passed from ChatsPage via navigation state.
  useEffect(() => {
    const pending = (location.state as { pendingMessage?: string } | null)?.pendingMessage
    if (pending && !loading && !pendingSent.current) {
      pendingSent.current = true
      // Clear the navigation state so a page refresh doesn't resend.
      globalThis.history.replaceState({}, '')
      doSend(pending)
    }
  }, [loading, location.state, doSend])

  const handleSend = () => {
    if (!input.trim()) return
    const content = input.trim()
    setInput('')
    doSend(content)
  }

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      handleSend()
    }
  }

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <Loader2 className="h-5 w-5 animate-spin text-zinc-400" />
      </div>
    )
  }

  if (error && !detail) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-red-600">{error}</div>
      </div>
    )
  }

  const agentLabel = detail?.session.agent_slug || null

  const startEditingTitle = () => {
    setTitleDraft(detail?.session.title ?? '')
    setEditingTitle(true)
    setTimeout(() => titleInputRef.current?.select(), 0)
  }

  const saveTitle = async () => {
    const trimmed = titleDraft.trim()
    if (!trimmed || !id || trimmed === detail?.session.title) {
      setEditingTitle(false)
      return
    }
    try {
      await chatsApi.updateTitle(id, trimmed)
      setDetail(prev => (prev ? { ...prev, session: { ...prev.session, title: trimmed } } : prev))
    } catch {
      // silently revert
    }
    setEditingTitle(false)
  }

  const cancelEditingTitle = () => setEditingTitle(false)

  return (
    <div className="flex flex-col h-full min-w-0 overflow-hidden">
      {/* Header */}
      <div className="flex items-center gap-3 border-b border-zinc-100 dark:border-zinc-700/50 px-3 sm:px-4 py-3 shrink-0">
        <button
          onClick={() => navigate('/chats')}
          className="h-7 w-7 flex items-center justify-center rounded-md text-zinc-400 dark:text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
        >
          <ArrowLeft className="h-4 w-4" />
        </button>
        <div className="flex-1 min-w-0">
          {editingTitle ? (
            <input
              ref={titleInputRef}
              value={titleDraft}
              onChange={e => setTitleDraft(e.target.value)}
              onBlur={saveTitle}
              onKeyDown={e => {
                if (e.key === 'Enter') saveTitle()
                if (e.key === 'Escape') cancelEditingTitle()
              }}
              className="w-full text-sm font-semibold text-zinc-900 dark:text-zinc-100 bg-zinc-100 dark:bg-zinc-800 rounded px-2 py-0.5 outline-none focus:ring-1 focus:ring-zinc-400 dark:focus:ring-zinc-500"
            />
          ) : (
            <button
              onClick={startEditingTitle}
              className="group flex items-center gap-1.5 max-w-full text-left cursor-pointer"
              title="Click to edit title"
            >
              <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100 truncate">
                {detail?.session.title ?? 'Chat'}
              </h2>
              <Pencil className="h-3 w-3 text-zinc-300 dark:text-zinc-600 group-hover:text-zinc-500 dark:group-hover:text-zinc-400 shrink-0 transition-colors" />
            </button>
          )}
        </div>
        <span className="text-xs text-zinc-400 dark:text-zinc-500 shrink-0 font-mono">
          {agentLabel ?? 'Direct chat'}
        </span>
      </div>

      {/* Messages */}
      <div className="flex-1 overflow-y-auto overflow-x-hidden">
        <div className="flex flex-col gap-5 px-3 py-4 sm:px-6 sm:py-6 w-full max-w-4xl mx-auto">
          {messages.length === 0 && !streaming && (
            <div className="flex flex-col items-center justify-center py-16 text-center">
              <div className="flex h-10 w-10 items-center justify-center rounded-full bg-zinc-900 text-white text-sm font-bold mb-4">
                {agentLabel ? agentLabel[0].toUpperCase() : 'C'}
              </div>
              <p className="text-sm text-zinc-400">Send a message to start the conversation.</p>
            </div>
          )}

          {messages.map((msg, i) => {
            const isLastMsg = i === messages.length - 1
            const msgKey = `${msg.role}-${msg.timestamp}-${i}`
            if (msg.role === 'assistant' && msg.blocks && msg.blocks.length > 0) {
              // Render blocks in the exact order they arrived in the stream.
              return (
                <Fragment key={msgKey}>
                  {msg.blocks.map((block, j) => {
                    const blockKey = `${block.type}-${j}`
                    if (block.type === 'thinking') {
                      return <ThinkingBlock key={`thinking-${blockKey}`} text={block.text} />
                    }
                    if (block.type === 'tool_use') {
                      // Interactive when: (a) not streaming (historical card can doSend),
                      // or (b) streaming AND awaiting user input via provideInput.
                      const canInteract = isLastMsg && (awaitingInput || !streaming)
                      return (
                        <ToolCallCard
                          key={`tool-${blockKey}`}
                          block={block}
                          isInteractive={canInteract}
                          toolResult={block.toolResult}
                          onSubmit={
                            canInteract && id
                              ? answer => {
                                  if (awaitingInput) {
                                    setAwaitingInput(false)
                                    provideInput(id, answer)
                                  } else {
                                    doSend(answer)
                                  }
                                }
                              : undefined
                          }
                        />
                      )
                    }
                    // text block
                    return (
                      <MessageBubble
                        key={`text-${blockKey}`}
                        message={{ ...msg, content: block.text }}
                      />
                    )
                  })}
                </Fragment>
              )
            }
            // Fallback: messages loaded from DB (no blocks) — render text only.
            return <MessageBubble key={msgKey} message={msg} />
          })}

          {/* Streaming: render all blocks in arrival order so thinking, text,
               and tool calls are interleaved correctly. */}
          {streaming &&
            streamingBlocks.map((block, i) => {
              if (block.type === 'thinking') {
                const thinkKey = block._key ?? String(i)
                return <ThinkingBlock key={`stream-thinking-${thinkKey}`} text={block.text} />
              }
              if (block.type === 'tool_use') {
                const toolKey = block.id ?? `${block.name}-${i}`
                return (
                  <ToolCallCard
                    key={`stream-tool-${toolKey}`}
                    block={block}
                    isInteractive={awaitingInput && block.name === 'AskUserQuestion'}
                    toolResult={block.id ? streamingToolResults[block.id] : undefined}
                    progress={block.id ? toolProgress[block.id] : undefined}
                    summary={block.id ? toolSummaries[block.id] : undefined}
                    onSubmit={
                      awaitingInput && block.name === 'AskUserQuestion' && id
                        ? answer => {
                            setAwaitingInput(false)
                            provideInput(id, answer)
                          }
                        : undefined
                    }
                  />
                )
              }
              // text block
              const textKey = block._key ?? String(i)
              return (
                <div key={`stream-text-${textKey}`} className="flex gap-3">
                  <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 shrink-0 mt-0.5 text-xs font-bold">
                    {agentLabel ? agentLabel[0].toUpperCase() : 'C'}
                  </div>
                  <div className="bg-zinc-50 dark:bg-zinc-800/60 border border-zinc-100 dark:border-zinc-700 rounded-2xl rounded-tl-sm px-4 py-3 text-sm max-w-[90%] sm:max-w-[82%] overflow-x-auto min-w-0">
                    <div className="prose prose-sm max-w-none">
                      <ReactMarkdown remarkPlugins={[remarkGfm]}>{block.text}</ReactMarkdown>
                    </div>
                  </div>
                </div>
              )
            })}

          {/* Streaming: background task events */}
          {streaming && taskEvents.length > 0 && (
            <div className="ml-10 space-y-1">
              {taskEvents.map((te, i) => {
                const dotColors: Record<string, string> = {
                  started: 'bg-blue-400',
                  progress: 'bg-amber-400',
                }
                const dotColorClass = dotColors[te.type] ?? 'bg-emerald-400'
                const taskEventKey = `task-event-${te.taskId ?? ''}-${te.type}-${te.status ?? ''}-${i}`
                return (
                  <div
                    key={taskEventKey}
                    className="flex items-center gap-2 text-xs text-zinc-400 dark:text-zinc-500"
                  >
                    <span className={cn('h-1.5 w-1.5 rounded-full shrink-0', dotColorClass)} />
                    <span className="font-mono">
                      {te.taskId ? `[${te.taskId.slice(0, 8)}]` : '[task]'}
                    </span>
                    {te.status && <span className="font-semibold">{te.status}</span>}
                    {te.message && <span className="truncate">{te.message}</span>}
                  </div>
                )
              })}
            </div>
          )}

          {/* Streaming: system status (tool execution in progress) */}
          {streaming && systemStatus && (
            <div className="flex items-center gap-2 pl-10 text-xs text-zinc-400 dark:text-zinc-500">
              <Loader2 className="h-3 w-3 animate-spin shrink-0" />
              {systemStatus}
            </div>
          )}

          {/* Typing indicator — only when no content has arrived yet */}
          {streaming && streamingBlocks.length === 0 && !systemStatus && (
            <div className="flex gap-3 items-center">
              <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-900 shrink-0">
                <Loader2 className="h-3.5 w-3.5 animate-spin text-white" />
              </div>
              <div className="flex items-center gap-1">
                <span className="h-1.5 w-1.5 rounded-full bg-zinc-300 animate-bounce [animation-delay:0ms]" />
                <span className="h-1.5 w-1.5 rounded-full bg-zinc-300 animate-bounce [animation-delay:150ms]" />
                <span className="h-1.5 w-1.5 rounded-full bg-zinc-300 animate-bounce [animation-delay:300ms]" />
              </div>
            </div>
          )}

          {/* Permission request dialog — shown when agent needs approval to use a tool */}
          {streaming && permissionRequest && id && (
            <PermissionRequestCard
              toolName={permissionRequest.toolName}
              input={permissionRequest.input}
              onDecide={allow => {
                setPermissionRequest(null)
                permissionResponse(id, allow)
              }}
            />
          )}

          {error && (
            <div className="rounded-md border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
              {error}
            </div>
          )}

          <div ref={bottomRef} />
        </div>
      </div>

      {/* Input */}
      <div className="border-t border-zinc-100 dark:border-zinc-700/50 px-3 py-3 sm:px-6 shrink-0 bg-white dark:bg-zinc-950">
        <div className="flex gap-2 max-w-4xl mx-auto">
          <Textarea
            value={input}
            onChange={e => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Message… (Enter to send, Shift+Enter for new line)"
            className="min-h-[72px] max-h-[240px] resize-none text-sm border-zinc-200 focus:border-zinc-900 focus:ring-zinc-900"
            disabled={streaming}
            rows={3}
          />
          {streaming ? (
            <button
              onClick={() => {
                // Gracefully stop the agent session, then abort the SSE connection.
                if (id) {
                  stopSession(id)
                    .catch(() => {
                      // Fallback: if the stop request fails, abort the connection directly.
                    })
                    .finally(() => abortRef.current?.abort())
                } else {
                  abortRef.current?.abort()
                }
              }}
              title="Stop generation"
              className="flex h-9 w-9 items-center justify-center rounded-md shrink-0 self-end transition-colors bg-zinc-900 text-white hover:bg-zinc-700"
            >
              <Square className="h-4 w-4 fill-current" />
            </button>
          ) : (
            <button
              onClick={handleSend}
              disabled={!input.trim()}
              className={cn(
                'flex h-9 w-9 items-center justify-center rounded-md shrink-0 self-end transition-colors',
                input.trim()
                  ? 'bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 hover:bg-zinc-700 dark:hover:bg-zinc-300'
                  : 'bg-zinc-100 dark:bg-zinc-800 text-zinc-400 dark:text-zinc-600 cursor-not-allowed',
              )}
            >
              <Send className="h-4 w-4" />
            </button>
          )}
        </div>
        {/* Session info pills */}
        {detail && (detail.session.working_directory || detail.session.model) && (
          <div className="flex items-center gap-3 max-w-4xl mx-auto mt-1.5">
            {detail.session.working_directory && (
              <span
                className="flex items-center gap-1 text-xs text-zinc-400 truncate max-w-[200px]"
                title={detail.session.working_directory}
              >
                <Folder className="h-3 w-3 shrink-0" />
                {detail.session.working_directory}
              </span>
            )}
            {detail.session.working_directory && detail.session.model && (
              <span className="text-zinc-200">•</span>
            )}
            {detail.session.model && (
              <span
                className="text-xs text-zinc-400 font-mono truncate max-w-[180px]"
                title={detail.session.model}
              >
                {detail.session.model}
              </span>
            )}
          </div>
        )}
      </div>
    </div>
  )
}

function MessageBubble({ message }: Readonly<{ message: ChatMessage }>) {
  const isUser = message.role === 'user'
  const [copied, setCopied] = useState(false)

  const handleCopy = () => {
    navigator.clipboard
      .writeText(message.content)
      .then(() => {
        setCopied(true)
        setTimeout(() => setCopied(false), 2000)
      })
      .catch(() => {
        // Clipboard write failed (e.g. no permission, document not focused)
      })
  }

  if (isUser) {
    return (
      <div className="flex justify-end group">
        <div className="relative max-w-[85%] sm:max-w-[75%]">
          <div className="bg-zinc-900 text-white rounded-2xl rounded-tr-sm px-4 py-2.5 text-sm whitespace-pre-wrap break-words leading-relaxed">
            {message.content}
          </div>
          <button
            onClick={handleCopy}
            title="Copy message"
            className="absolute -left-8 top-2 opacity-0 group-hover:opacity-100 focus:opacity-100 transition-opacity p-1 rounded text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-300"
          >
            {copied ? (
              <Check className="h-3.5 w-3.5 text-emerald-500" />
            ) : (
              <Copy className="h-3.5 w-3.5" />
            )}
          </button>
        </div>
      </div>
    )
  }

  return (
    <div className="flex gap-3 group">
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 shrink-0 mt-0.5 text-xs font-bold">
        C
      </div>
      <div className="relative bg-zinc-50 dark:bg-zinc-800/60 border border-zinc-100 dark:border-zinc-700 rounded-2xl rounded-tl-sm px-4 py-3 text-sm max-w-[90%] sm:max-w-[82%] overflow-x-auto min-w-0">
        <div className="prose prose-sm max-w-none dark:text-zinc-200">
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{message.content}</ReactMarkdown>
        </div>
        <button
          onClick={handleCopy}
          title="Copy message"
          className="absolute top-2 right-2 opacity-0 group-hover:opacity-100 focus:opacity-100 transition-opacity p-1 rounded text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-300"
        >
          {copied ? (
            <Check className="h-3.5 w-3.5 text-emerald-500" />
          ) : (
            <Copy className="h-3.5 w-3.5" />
          )}
        </button>
      </div>
    </div>
  )
}

function ThinkingBlock({ text }: Readonly<{ text: string }>) {
  const [expanded, setExpanded] = useState(false)

  return (
    <div className="flex gap-3">
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-100 dark:bg-zinc-800 text-zinc-500 dark:text-zinc-400 shrink-0 mt-0.5 text-xs">
        ✦
      </div>
      <div className="flex-1 max-w-[82%]">
        <button
          onClick={() => setExpanded(e => !e)}
          className="flex items-center gap-1.5 text-xs text-zinc-400 dark:text-zinc-500 hover:text-zinc-600 dark:hover:text-zinc-300 transition-colors mb-1 cursor-pointer"
        >
          {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
          Thinking
        </button>
        {expanded && (
          <div className="rounded-lg border border-zinc-100 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/60 px-3 py-2 text-xs text-zinc-500 dark:text-zinc-400 font-mono whitespace-pre-wrap leading-relaxed">
            {text}
          </div>
        )}
      </div>
    </div>
  )
}

/** Returns icon component, icon container background, and icon colour for a given tool name. */
function getToolConfig(name: string): { Icon: LucideIcon; bg: string; color: string } {
  switch (name) {
    case 'Write':
      return {
        Icon: FilePen,
        bg: 'bg-emerald-50 dark:bg-emerald-950/50',
        color: 'text-emerald-600 dark:text-emerald-400',
      }
    case 'Read':
      return {
        Icon: FileText,
        bg: 'bg-blue-50 dark:bg-blue-950/50',
        color: 'text-blue-600 dark:text-blue-400',
      }
    case 'Edit':
      return {
        Icon: FileEdit,
        bg: 'bg-orange-50 dark:bg-orange-950/50',
        color: 'text-orange-600 dark:text-orange-400',
      }
    case 'Glob':
    case 'Grep':
      return {
        Icon: Search,
        bg: 'bg-violet-50 dark:bg-violet-950/50',
        color: 'text-violet-500 dark:text-violet-400',
      }
    case 'WebFetch':
    case 'WebSearch':
      return {
        Icon: Globe,
        bg: 'bg-sky-50 dark:bg-sky-950/50',
        color: 'text-sky-600 dark:text-sky-400',
      }
    case 'Task':
    case 'TaskOutput':
    case 'TaskStop':
      return {
        Icon: Bot,
        bg: 'bg-amber-50 dark:bg-amber-950/50',
        color: 'text-amber-600 dark:text-amber-400',
      }
    case 'Bash':
    default:
      return {
        Icon: Terminal,
        bg: 'bg-zinc-100 dark:bg-zinc-800',
        color: 'text-zinc-500 dark:text-zinc-400',
      }
  }
}

function asStr(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

/** Map of tool name to a function that extracts a summary from the input fields. */
const TOOL_SUMMARY_EXTRACTORS: Record<string, (input: Record<string, unknown>) => string> = {
  Bash: input => asStr(input.description) || asStr(input.command),
  Read: input => asStr(input.file_path),
  Write: input => asStr(input.file_path),
  Edit: input => asStr(input.file_path),
  Glob: input => asStr(input.pattern),
  Grep: input => [asStr(input.pattern), asStr(input.path)].filter(Boolean).join(' in '),
  WebFetch: input => asStr(input.url),
  WebSearch: input => asStr(input.query),
  Task: input => asStr(input.description) || asStr(input.subagent_type),
}

function firstStringValue(input: Record<string, unknown>): string {
  for (const v of Object.values(input)) {
    const s = asStr(v)
    if (s) return s
  }
  return ''
}

/** Returns a short one-line summary for a tool call based on its input fields. */
function toolCallSummary(name: string, input: Record<string, unknown> | undefined): string {
  if (!input) return ''
  const extractor = TOOL_SUMMARY_EXTRACTORS[name]
  return extractor ? extractor(input) : firstStringValue(input)
}

const CODE_PRE_CLS =
  'rounded-lg border border-zinc-100 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/60 px-3 py-2 text-xs text-zinc-700 dark:text-zinc-300 font-mono whitespace-pre-wrap leading-relaxed overflow-x-auto max-h-64'

function renderWriteDetail(input: Record<string, unknown> | undefined): React.ReactNode {
  const content = typeof input?.content === 'string' ? input.content : null
  if (content === null) return null
  return <pre className={CODE_PRE_CLS}>{content}</pre>
}

function renderReadDetail(toolResult: Record<string, unknown> | undefined): React.ReactNode {
  const fileResult = toolResult as { type?: string; file?: { content?: string } } | undefined
  const content = fileResult?.file?.content
  if (content !== undefined) return <pre className={CODE_PRE_CLS}>{content}</pre>
  return (
    <div className="rounded-lg border border-zinc-100 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/60 px-3 py-2 text-xs text-zinc-400 dark:text-zinc-500 italic">
      File content not available
    </div>
  )
}

function patchLineColor(line: string): string {
  if (line.startsWith('-')) return 'bg-red-50 dark:bg-red-950/40 text-red-700 dark:text-red-400'
  if (line.startsWith('+'))
    return 'bg-green-50 dark:bg-green-950/40 text-green-700 dark:text-green-400'
  return 'bg-zinc-50 dark:bg-zinc-800/60 text-zinc-400 dark:text-zinc-500'
}

const DIFF_CLS =
  'rounded-lg border border-zinc-100 dark:border-zinc-700 overflow-hidden text-xs font-mono max-h-64 overflow-y-auto'

function renderStructuredPatch(patch: Array<{ lines: string[] }>): React.ReactNode {
  return (
    <div className={DIFF_CLS}>
      {patch.flatMap((hunk, hi) =>
        hunk.lines.map((line, li) => (
          <div
            key={`${hi}-${li}`}
            className={cn('px-3 py-0.5 leading-5 whitespace-pre', patchLineColor(line))}
          >
            {line}
          </div>
        )),
      )}
    </div>
  )
}

function renderSimpleDiff(oldStr: string, newStr: string): React.ReactNode {
  return (
    <div className={DIFF_CLS}>
      {oldStr.split('\n').map((line, i) => (
        <div
          key={`old-${i}-${line.slice(0, 40)}`}
          className="px-3 py-0.5 leading-5 whitespace-pre bg-red-50 dark:bg-red-950/40 text-red-700 dark:text-red-400"
        >
          -{line}
        </div>
      ))}
      {newStr.split('\n').map((line, i) => (
        <div
          key={`new-${i}-${line.slice(0, 40)}`}
          className="px-3 py-0.5 leading-5 whitespace-pre bg-green-50 dark:bg-green-950/40 text-green-700 dark:text-green-400"
        >
          +{line}
        </div>
      ))}
    </div>
  )
}

function renderEditDetail(
  input: Record<string, unknown> | undefined,
  toolResult: Record<string, unknown> | undefined,
): React.ReactNode {
  const editResult = toolResult as { structuredPatch?: Array<{ lines: string[] }> } | undefined
  const patch = editResult?.structuredPatch

  if (patch && patch.length > 0) return renderStructuredPatch(patch)

  const oldStr = typeof input?.old_string === 'string' ? input.old_string : ''
  const newStr = typeof input?.new_string === 'string' ? input.new_string : ''
  if (oldStr || newStr) return renderSimpleDiff(oldStr, newStr)

  return null
}

/** Renders the expanded detail panel for a tool call based on the tool name. */
function ToolCallDetail({
  name,
  input,
  toolResult,
}: Readonly<{
  name: string
  input: Record<string, unknown> | undefined
  toolResult: Record<string, unknown> | undefined
}>) {
  if (name === 'Read') return renderReadDetail(toolResult)

  // Write and Edit have their own renderers but may return null (e.g. missing
  // content/patch), in which case fall through to the raw JSON default.
  let toolDetail = null
  if (name === 'Write') {
    toolDetail = renderWriteDetail(input)
  } else if (name === 'Edit') {
    toolDetail = renderEditDetail(input, toolResult)
  }
  if (toolDetail !== null) return toolDetail

  // Default: raw JSON
  if (input !== undefined) {
    return (
      <div className="rounded-lg border border-zinc-100 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/60 px-3 py-2 text-xs text-zinc-500 dark:text-zinc-400 font-mono whitespace-pre-wrap leading-relaxed overflow-x-auto max-h-48">
        {JSON.stringify(input, null, 2)}
      </div>
    )
  }

  return null
}

function ToolCallCard({
  block,
  isInteractive,
  onSubmit,
  toolResult,
  progress,
  summary: toolSummary,
}: Readonly<{
  block: { type: string; id?: string; name?: string; input?: Record<string, unknown> }
  isInteractive?: boolean
  onSubmit?: (answer: string) => void
  toolResult?: Record<string, unknown>
  progress?: { progress?: number; message?: string }
  summary?: string
}>) {
  const [expanded, setExpanded] = useState(false)
  const name = block.name ?? 'unknown'

  // AskUserQuestion gets its own rich interactive UI.
  if (name === 'AskUserQuestion' && block.input) {
    return (
      <AskUserQuestionCard input={block.input} isInteractive={isInteractive} onSubmit={onSubmit} />
    )
  }

  // Use SDK-provided summary when available, fall back to manual summary.
  const manualSummary = toolCallSummary(name, block.input)
  const displayText = toolSummary || manualSummary
  const { Icon, bg, color } = getToolConfig(name)
  // For file-based tools show just the basename in the header; tooltip shows full path.
  const isFileTool = name === 'Read' || name === 'Write' || name === 'Edit'
  const displaySummary =
    isFileTool && !toolSummary && displayText
      ? (displayText.split('/').pop() ?? displayText)
      : displayText

  return (
    <div className="flex gap-3">
      <div
        className={cn('flex h-7 w-7 items-center justify-center rounded-full shrink-0 mt-0.5', bg)}
      >
        <Icon className={cn('h-3.5 w-3.5', color)} />
      </div>
      <div className="flex-1 min-w-0 max-w-[82%]">
        <button
          onClick={() => setExpanded(e => !e)}
          className="flex w-full items-center gap-1.5 text-xs text-zinc-400 dark:text-zinc-500 hover:text-zinc-600 dark:hover:text-zinc-300 transition-colors mb-1 min-w-0 cursor-pointer"
        >
          {expanded ? (
            <ChevronDown className="h-3 w-3 shrink-0" />
          ) : (
            <ChevronRight className="h-3 w-3 shrink-0" />
          )}
          <span className={cn('font-mono font-semibold shrink-0', color)}>{name}</span>
          {displaySummary && (
            <span
              className="font-mono text-zinc-400 dark:text-zinc-500 truncate min-w-0"
              title={displayText}
            >
              {displaySummary}
            </span>
          )}
          {toolResult && (
            <span
              className="ml-auto shrink-0 h-1.5 w-1.5 rounded-full bg-emerald-400"
              title="Completed"
            />
          )}
        </button>
        {/* Tool progress bar */}
        {progress && !toolResult && (
          <div className="mb-1 space-y-0.5">
            {progress.message && (
              <div className="text-[12px] text-zinc-400 dark:text-zinc-500 truncate">
                {progress.message}
              </div>
            )}
            {progress.progress != null && progress.progress > 0 && (
              <div className="h-1 w-full rounded-full bg-zinc-100 dark:bg-zinc-800 overflow-hidden">
                <div
                  className="h-full rounded-full bg-blue-400 dark:bg-blue-500 transition-all duration-300"
                  style={{ width: `${Math.min(progress.progress * 100, 100)}%` }}
                />
              </div>
            )}
          </div>
        )}
        {expanded && <ToolCallDetail name={name} input={block.input} toolResult={toolResult} />}
      </div>
    </div>
  )
}

function PermissionRequestCard({
  toolName,
  input,
  onDecide,
}: Readonly<{
  toolName: string
  input: unknown
  onDecide: (allow: boolean) => void
}>) {
  const [decided, setDecided] = useState(false)
  const [decision, setDecision] = useState<boolean | null>(null)

  const handleDecide = (allow: boolean) => {
    if (decided) return
    setDecided(true)
    setDecision(allow)
    onDecide(allow)
  }

  // Format a short summary of the input for display.
  const inputSummary = (() => {
    if (!input || typeof input !== 'object') return null
    const entries = Object.entries(input as Record<string, unknown>)
    if (entries.length === 0) return null
    return entries
      .slice(0, 3)
      .map(([k, v]) => `${k}: ${typeof v === 'string' ? v : JSON.stringify(v)}`)
      .join(', ')
  })()

  return (
    <div className="flex gap-3">
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-amber-50 dark:bg-amber-950/50 shrink-0 mt-0.5">
        <ShieldQuestion className="h-3.5 w-3.5 text-amber-600 dark:text-amber-400" />
      </div>
      <div className="flex-1 min-w-0 max-w-[82%]">
        <div className="rounded-lg border border-amber-200 dark:border-amber-800/60 bg-amber-50 dark:bg-amber-950/30 p-3 space-y-2">
          <div className="flex items-start gap-2">
            <div className="flex-1 min-w-0">
              <p className="text-xs font-semibold text-amber-800 dark:text-amber-300">
                Permission request
              </p>
              <p className="text-xs text-amber-700 dark:text-amber-400 mt-0.5">
                The agent wants to use <code className="font-mono font-semibold">{toolName}</code>
              </p>
              {inputSummary && (
                <p className="text-[12px] text-amber-600 dark:text-amber-500 mt-1 font-mono truncate">
                  {inputSummary}
                </p>
              )}
            </div>
          </div>
          {decided ? (
            <p className="text-[12px] text-amber-600 dark:text-amber-500">
              {decision ? 'Allowed' : 'Denied'}
            </p>
          ) : (
            <div className="flex items-center gap-2">
              <button
                onClick={() => handleDecide(true)}
                className="rounded-md px-3 py-1.5 text-xs font-medium bg-emerald-600 dark:bg-emerald-700 text-white hover:bg-emerald-700 dark:hover:bg-emerald-600 transition-colors"
              >
                Allow
              </button>
              <button
                onClick={() => handleDecide(false)}
                className="rounded-md px-3 py-1.5 text-xs font-medium bg-zinc-200 dark:bg-zinc-700 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-300 dark:hover:bg-zinc-600 transition-colors"
              >
                Deny
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

const OTHER_LABEL = 'Other'

function AskUserQuestionCard({
  input,
  isInteractive,
  onSubmit,
}: Readonly<{
  input: Record<string, unknown>
  isInteractive?: boolean
  onSubmit?: (answer: string) => void
}>) {
  const questions = (input.questions as AskUserQuestionItem[] | undefined) ?? []
  // selections[questionIndex] = array of selected option labels (may include OTHER_LABEL)
  const [selections, setSelections] = useState<Record<number, string[]>>({})
  // otherTexts[questionIndex] = free-text typed when "Other" is selected
  const [otherTexts, setOtherTexts] = useState<Record<number, string>>({})
  const [submitted, setSubmitted] = useState(false)

  if (questions.length === 0) return null

  const toggle = (qIdx: number, label: string, multiSelect: boolean) => {
    if (!isInteractive || submitted) return
    setSelections(prev => {
      const current = prev[qIdx] ?? []
      if (multiSelect) {
        const next = current.includes(label)
          ? current.filter(l => l !== label)
          : [...current, label]
        return { ...prev, [qIdx]: next }
      } else {
        return { ...prev, [qIdx]: [label] }
      }
    })
  }

  const hasSelections = questions.every((_, i) => {
    const sel = selections[i] ?? []
    if (sel.length === 0) return false
    // If "Other" is selected, require non-empty text
    if (sel.includes(OTHER_LABEL)) return (otherTexts[i] ?? '').trim().length > 0
    return true
  })

  const handleSubmit = () => {
    if (!onSubmit || submitted) return
    const lines = questions.map((q, i) => {
      const chosen = (selections[i] ?? [])
        .map(label => (label === OTHER_LABEL ? (otherTexts[i] ?? '').trim() : label))
        .filter(Boolean)
        .join(', ')
      const header = q.header ?? `Q${i + 1}`
      return `${header}: ${chosen}`
    })
    onSubmit(lines.join('\n'))
    setSubmitted(true)
  }

  return (
    <div className="flex gap-3">
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-100 dark:bg-zinc-800 text-zinc-500 dark:text-zinc-400 shrink-0 mt-0.5">
        <MessageSquare className="h-3.5 w-3.5" />
      </div>
      <div className="flex-1 min-w-0 max-w-[82%] space-y-3">
        {questions.map((q, i) => {
          const otherSelected = (selections[i] ?? []).includes(OTHER_LABEL)
          const questionKey = q.header ?? `question-${q.question.slice(0, 40)}-${i}`
          return (
            <div
              key={questionKey}
              className="rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/60 p-3"
            >
              {q.header && (
                <div className="text-[12px] font-semibold uppercase tracking-wider text-zinc-400 dark:text-zinc-500 mb-1">
                  {q.header}
                </div>
              )}
              <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 mb-2">
                {q.question}
              </div>
              <div className="flex flex-wrap gap-1.5">
                {q.options.map(opt => {
                  const selected = (selections[i] ?? []).includes(opt.label)
                  return (
                    <button
                      key={`opt-${opt.label}`}
                      disabled={!isInteractive || submitted}
                      onClick={() => toggle(i, opt.label, !!q.multiSelect)}
                      className={cn(
                        'rounded-md border px-2.5 py-1 text-xs text-left transition-colors',
                        isInteractive && !submitted
                          ? 'cursor-pointer hover:border-zinc-400'
                          : 'cursor-default',
                        selected
                          ? 'border-zinc-900 dark:border-zinc-100 bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900'
                          : 'border-zinc-200 dark:border-zinc-600 bg-zinc-50 dark:bg-zinc-700/50 text-zinc-700 dark:text-zinc-300',
                      )}
                    >
                      <span className="font-medium">{opt.label}</span>
                      {opt.description && (
                        <span className={cn('ml-1', selected ? 'text-zinc-300' : 'text-zinc-400')}>
                          {' '}
                          — {opt.description}
                        </span>
                      )}
                    </button>
                  )
                })}
                {/* Always add an "Other" chip for free-form input */}
                <button
                  disabled={!isInteractive || submitted}
                  onClick={() => toggle(i, OTHER_LABEL, !!q.multiSelect)}
                  className={cn(
                    'rounded-md border px-2.5 py-1 text-xs text-left transition-colors',
                    isInteractive && !submitted
                      ? 'cursor-pointer hover:border-zinc-400'
                      : 'cursor-default',
                    otherSelected
                      ? 'border-zinc-900 dark:border-zinc-100 bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900'
                      : 'border-zinc-200 dark:border-zinc-600 bg-zinc-50 dark:bg-zinc-700/50 text-zinc-700 dark:text-zinc-300',
                  )}
                >
                  <span className="font-medium">Other</span>
                </button>
              </div>
              {/* Free-text input shown when "Other" is selected */}
              {otherSelected && isInteractive && !submitted && (
                <input
                  type="text"
                  autoFocus
                  value={otherTexts[i] ?? ''}
                  onChange={e => setOtherTexts(prev => ({ ...prev, [i]: e.target.value }))}
                  onKeyDown={e => {
                    if (e.key === 'Enter' && hasSelections) handleSubmit()
                  }}
                  placeholder="Type your answer…"
                  className="mt-2 w-full rounded-md border border-zinc-200 dark:border-zinc-600 bg-zinc-50 dark:bg-zinc-700/50 px-2.5 py-1.5 text-xs text-zinc-800 dark:text-zinc-200 placeholder:text-zinc-400 dark:placeholder:text-zinc-500 focus:border-zinc-900 dark:focus:border-zinc-400 focus:outline-none"
                />
              )}
              {q.multiSelect && !submitted && (
                <div className="mt-2 text-[12px] text-zinc-400 dark:text-zinc-500">
                  Multiple selections allowed
                </div>
              )}
            </div>
          )
        })}

        {isInteractive && !submitted && (
          <button
            disabled={!hasSelections}
            onClick={handleSubmit}
            className={cn(
              'rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
              hasSelections
                ? 'bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 hover:bg-zinc-700 dark:hover:bg-zinc-300'
                : 'bg-zinc-100 dark:bg-zinc-800 text-zinc-400 dark:text-zinc-600 cursor-not-allowed',
            )}
          >
            Send answers
          </button>
        )}
        {submitted && (
          <div className="text-[12px] text-zinc-400 dark:text-zinc-500">Answers sent</div>
        )}
      </div>
    </div>
  )
}
