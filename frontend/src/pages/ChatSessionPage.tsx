import { Fragment, useState, useEffect, useRef, useCallback } from 'react'
import { useParams, useNavigate, useLocation } from 'react-router-dom'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { chatsApi, sendMessage, provideInput, permissionResponse } from '@/lib/api'
import type {
  ChatDetail,
  ChatMessage,
  MessageBlock,
  SDKContentBlock,
  AskUserQuestionItem,
  SDKUserEvent,
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

  const [input, setInput] = useState('')
  const [streaming, setStreaming] = useState(false)
  const [streamingText, setStreamingText] = useState('')
  const [thinkingText, setThinkingText] = useState('')
  const [showThinking, setShowThinking] = useState(false)
  const [toolCalls, setToolCalls] = useState<SDKContentBlock[]>([])
  const [systemStatus, setSystemStatus] = useState<string | null>(null)
  // Tool results keyed by tool_use_id — populated from SDK "user" events during streaming.
  const [streamingToolResults, setStreamingToolResults] = useState<
    Record<string, Record<string, unknown>>
  >({})
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
  }, [messages, streamingText, scrollToBottom])

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
      setStreamingText('')
      setThinkingText('')
      setShowThinking(false)
      setToolCalls([])
      setSystemStatus(null)
      setAwaitingInput(false)
      setPermissionRequest(null)
      setStreamingToolResults({})
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
              const toolUseBlocks = event.message.content.filter(
                b => b.type === 'tool_use' && b.name,
              )
              if (toolUseBlocks.length > 0) {
                const newBlocks: MessageBlock[] = toolUseBlocks.map(b => ({
                  type: 'tool_use' as const,
                  id: b.id,
                  name: b.name!,
                  input: b.input,
                }))
                blocks = [...blocks, ...newBlocks]
                setToolCalls(prev => [...prev, ...toolUseBlocks])
              }
            },
            onStreamEvent: event => {
              const delta = event.event.delta
              if (!delta) return
              if (delta.type === 'thinking_delta' && delta.thinking) {
                // Append to the last thinking block or start a new one.
                const last = blocks[blocks.length - 1]
                if (last?.type === 'thinking') {
                  last.text += delta.thinking
                } else {
                  blocks.push({ type: 'thinking', text: delta.thinking })
                }
                setThinkingText(prev => prev + delta.thinking)
                setShowThinking(true)
              } else if (delta.type === 'text_delta' && delta.text) {
                accumulated += delta.text
                // Append to the last text block or start a new one.
                const last = blocks[blocks.length - 1]
                if (last?.type === 'text') {
                  last.text += delta.text
                } else {
                  blocks.push({ type: 'text', text: delta.text })
                }
                setStreamingText(accumulated)
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
            onResult: event => {
              if (event.is_error) {
                const errMsg =
                  event.errors && event.errors.length > 0
                    ? event.errors.join('; ')
                    : (event.result ?? 'Unknown error')
                setError(errMsg)
                setStreamingText('')
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
              setStreamingText('')
              setThinkingText('')
              setShowThinking(false)
              setToolCalls([])
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
        setStreamingText('')
        setThinkingText('')
        setShowThinking(false)
        setToolCalls([])
        setSystemStatus(null)
        setAwaitingInput(false)
        setPermissionRequest(null)
        setStreamingToolResults({})
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
          <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100 truncate">
            {detail?.session.title ?? 'Chat'}
          </h2>
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
            if (msg.role === 'assistant' && msg.blocks && msg.blocks.length > 0) {
              // Render blocks in the exact order they arrived in the stream.
              return (
                <Fragment key={i}>
                  {msg.blocks.map((block, j) => {
                    if (block.type === 'thinking') {
                      return <ThinkingBlock key={j} text={block.text} />
                    }
                    if (block.type === 'tool_use') {
                      // Interactive when: (a) not streaming (historical card can doSend),
                      // or (b) streaming AND awaiting user input via provideInput.
                      const canInteract = isLastMsg && (awaitingInput || !streaming)
                      return (
                        <ToolCallCard
                          key={j}
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
                    return <MessageBubble key={j} message={{ ...msg, content: block.text }} />
                  })}
                </Fragment>
              )
            }
            // Fallback: messages loaded from DB (no blocks) — render text only.
            return <MessageBubble key={i} message={msg} />
          })}

          {/* Streaming: thinking */}
          {streaming && showThinking && thinkingText && <ThinkingBlock text={thinkingText} />}

          {/* Streaming: tool call cards. AskUserQuestion becomes interactive once
               the backend signals user_input_required (awaitingInput=true). */}
          {streaming &&
            toolCalls.map((call, i) => (
              <ToolCallCard
                key={`stream-tool-${call.id ?? call.name}-${i}`}
                block={{ type: 'tool_use', id: call.id, name: call.name, input: call.input }}
                isInteractive={awaitingInput && call.name === 'AskUserQuestion'}
                toolResult={call.id ? streamingToolResults[call.id] : undefined}
                onSubmit={
                  awaitingInput && call.name === 'AskUserQuestion' && id
                    ? answer => {
                        setAwaitingInput(false)
                        provideInput(id, answer)
                      }
                    : undefined
                }
              />
            ))}

          {/* Streaming: system status (tool execution in progress) */}
          {streaming && systemStatus && (
            <div className="flex items-center gap-2 pl-10 text-xs text-zinc-400 dark:text-zinc-500">
              <Loader2 className="h-3 w-3 animate-spin shrink-0" />
              {systemStatus}
            </div>
          )}

          {/* Streaming: assistant response */}
          {streaming && streamingText && (
            <div className="flex gap-3">
              <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 shrink-0 mt-0.5 text-xs font-bold">
                {agentLabel ? agentLabel[0].toUpperCase() : 'C'}
              </div>
              <div className="bg-zinc-50 dark:bg-zinc-800/60 border border-zinc-100 dark:border-zinc-700 rounded-2xl rounded-tl-sm px-4 py-3 text-sm max-w-[90%] sm:max-w-[82%] overflow-x-auto min-w-0">
                <div className="prose prose-sm max-w-none">
                  <ReactMarkdown remarkPlugins={[remarkGfm]}>{streamingText}</ReactMarkdown>
                </div>
              </div>
            </div>
          )}

          {/* Typing indicator — only when no content has arrived yet */}
          {streaming &&
            !streamingText &&
            !thinkingText &&
            toolCalls.length === 0 &&
            !systemStatus && (
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
              onClick={() => abortRef.current?.abort()}
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

  if (isUser) {
    return (
      <div className="flex justify-end">
        <div className="bg-zinc-900 text-white rounded-2xl rounded-tr-sm px-4 py-2.5 text-sm max-w-[85%] sm:max-w-[75%] whitespace-pre-wrap break-words leading-relaxed">
          {message.content}
        </div>
      </div>
    )
  }

  return (
    <div className="flex gap-3">
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 shrink-0 mt-0.5 text-xs font-bold">
        C
      </div>
      <div className="bg-zinc-50 dark:bg-zinc-800/60 border border-zinc-100 dark:border-zinc-700 rounded-2xl rounded-tl-sm px-4 py-3 text-sm max-w-[90%] sm:max-w-[82%] overflow-x-auto min-w-0">
        <div className="prose prose-sm max-w-none dark:text-zinc-200">
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{message.content}</ReactMarkdown>
        </div>
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
    case 'Bash':
      return {
        Icon: Terminal,
        bg: 'bg-zinc-100 dark:bg-zinc-800',
        color: 'text-zinc-500 dark:text-zinc-400',
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
    default:
      return {
        Icon: Terminal,
        bg: 'bg-zinc-100 dark:bg-zinc-800',
        color: 'text-zinc-500 dark:text-zinc-400',
      }
  }
}

/** Returns a short one-line summary for a tool call based on its input fields. */
function toolCallSummary(name: string, input: Record<string, unknown> | undefined): string {
  if (!input) return ''
  const str = (v: unknown) => (typeof v === 'string' ? v : '')
  switch (name) {
    case 'Bash':
      return str(input.description) || str(input.command)
    case 'Read':
    case 'Write':
    case 'Edit':
      return str(input.file_path)
    case 'Glob':
      return str(input.pattern)
    case 'Grep':
      return [str(input.pattern), str(input.path)].filter(Boolean).join(' in ')
    case 'WebFetch':
      return str(input.url)
    case 'WebSearch':
      return str(input.query)
    case 'Task':
      return str(input.description) || str(input.subagent_type)
    default: {
      // Generic fallback: first string value found
      for (const v of Object.values(input)) {
        const s = str(v)
        if (s) return s
      }
      return ''
    }
  }
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
  if (name === 'Write') {
    const content = typeof input?.content === 'string' ? input.content : null
    if (content !== null) {
      return (
        <pre className="rounded-lg border border-zinc-100 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/60 px-3 py-2 text-xs text-zinc-700 dark:text-zinc-300 font-mono whitespace-pre-wrap leading-relaxed overflow-x-auto max-h-64">
          {content}
        </pre>
      )
    }
  }

  if (name === 'Read') {
    const fileResult = toolResult as { type?: string; file?: { content?: string } } | undefined
    const content = fileResult?.file?.content
    if (content !== undefined) {
      return (
        <pre className="rounded-lg border border-zinc-100 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/60 px-3 py-2 text-xs text-zinc-700 dark:text-zinc-300 font-mono whitespace-pre-wrap leading-relaxed overflow-x-auto max-h-64">
          {content}
        </pre>
      )
    }
    return (
      <div className="rounded-lg border border-zinc-100 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800/60 px-3 py-2 text-xs text-zinc-400 dark:text-zinc-500 italic">
        File content not available
      </div>
    )
  }

  if (name === 'Edit') {
    const editResult = toolResult as { structuredPatch?: Array<{ lines: string[] }> } | undefined
    const patch = editResult?.structuredPatch

    if (patch && patch.length > 0) {
      return (
        <div className="rounded-lg border border-zinc-100 dark:border-zinc-700 overflow-hidden text-xs font-mono max-h-64 overflow-y-auto">
          {patch.flatMap((hunk, hi) =>
            hunk.lines.map((line, li) => (
              <div
                key={`${hi}-${li}`}
                className={cn(
                  'px-3 py-0.5 leading-5 whitespace-pre',
                  line.startsWith('-')
                    ? 'bg-red-50 dark:bg-red-950/40 text-red-700 dark:text-red-400'
                    : line.startsWith('+')
                      ? 'bg-green-50 dark:bg-green-950/40 text-green-700 dark:text-green-400'
                      : 'bg-zinc-50 dark:bg-zinc-800/60 text-zinc-400 dark:text-zinc-500',
                )}
              >
                {line}
              </div>
            )),
          )}
        </div>
      )
    }

    // Fallback: synthesize a simple diff from input old/new strings
    const oldStr = typeof input?.old_string === 'string' ? input.old_string : ''
    const newStr = typeof input?.new_string === 'string' ? input.new_string : ''
    if (oldStr || newStr) {
      return (
        <div className="rounded-lg border border-zinc-100 dark:border-zinc-700 overflow-hidden text-xs font-mono max-h-64 overflow-y-auto">
          {oldStr.split('\n').map((line, i) => (
            <div
              key={`old-${i}`}
              className="px-3 py-0.5 leading-5 whitespace-pre bg-red-50 dark:bg-red-950/40 text-red-700 dark:text-red-400"
            >
              -{line}
            </div>
          ))}
          {newStr.split('\n').map((line, i) => (
            <div
              key={`new-${i}`}
              className="px-3 py-0.5 leading-5 whitespace-pre bg-green-50 dark:bg-green-950/40 text-green-700 dark:text-green-400"
            >
              +{line}
            </div>
          ))}
        </div>
      )
    }
  }

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
}: Readonly<{
  block: Pick<SDKContentBlock, 'type' | 'id' | 'name' | 'input'>
  isInteractive?: boolean
  onSubmit?: (answer: string) => void
  toolResult?: Record<string, unknown>
}>) {
  const [expanded, setExpanded] = useState(false)
  const name = block.name ?? 'unknown'

  // AskUserQuestion gets its own rich interactive UI.
  if (name === 'AskUserQuestion' && block.input) {
    return (
      <AskUserQuestionCard input={block.input} isInteractive={isInteractive} onSubmit={onSubmit} />
    )
  }

  const summary = toolCallSummary(name, block.input)
  const { Icon, bg, color } = getToolConfig(name)
  // For file-based tools show just the basename in the header; tooltip shows full path.
  const isFileTool = name === 'Read' || name === 'Write' || name === 'Edit'
  const displaySummary = isFileTool && summary ? (summary.split('/').pop() ?? summary) : summary

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
              title={summary}
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
                <p className="text-[11px] text-amber-600 dark:text-amber-500 mt-1 font-mono truncate">
                  {inputSummary}
                </p>
              )}
            </div>
          </div>
          {!decided ? (
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
          ) : (
            <p className="text-[11px] text-amber-600 dark:text-amber-500">
              {decision ? 'Allowed' : 'Denied'}
            </p>
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
          return (
            <div
              key={i}
              className="rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/60 p-3"
            >
              {q.header && (
                <div className="text-[10px] font-semibold uppercase tracking-wider text-zinc-400 dark:text-zinc-500 mb-1">
                  {q.header}
                </div>
              )}
              <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 mb-2">
                {q.question}
              </div>
              <div className="flex flex-wrap gap-1.5">
                {q.options.map((opt, j) => {
                  const selected = (selections[i] ?? []).includes(opt.label)
                  return (
                    <button
                      key={j}
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
                <div className="mt-2 text-[10px] text-zinc-400 dark:text-zinc-500">
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
          <div className="text-[10px] text-zinc-400 dark:text-zinc-500">Answers sent</div>
        )}
      </div>
    </div>
  )
}
