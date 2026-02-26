import { Fragment, useState, useEffect, useRef, useCallback } from 'react'
import { useParams, useNavigate, useLocation } from 'react-router-dom'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { chatsApi, sendMessage, provideInput } from '@/lib/api'
import type {
  ChatDetail,
  ChatMessage,
  MessageBlock,
  SDKContentBlock,
  AskUserQuestionItem,
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
  // awaitingInput is set when the backend sends user_input_required — the SSE
  // stream stays open and the AskUserQuestion card becomes interactive.
  const [awaitingInput, setAwaitingInput] = useState(false)

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
      setError(null)

      abortRef.current = new AbortController()

      try {
        // Local accumulators — avoids stale-closure issues with React state.
        // `blocks` preserves the exact order blocks arrived in the stream so
        // the stored message renders correctly (thinking → text → tool_use or
        // tool_use → text, depending on what the agent did first).
        let accumulated = ''
        let blocks: MessageBlock[] = []

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
              const assistantMsg: ChatMessage = {
                role: 'assistant',
                content: accumulated || event.result,
                timestamp: new Date().toISOString(),
                blocks: blocks.length > 0 ? [...blocks] : undefined,
              }
              setMessages(prev => [...prev, assistantMsg])
              // Reset per-turn local accumulators so a follow-up turn
              // (e.g. after AskUserQuestion is answered) starts clean.
              accumulated = ''
              blocks = []
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
      window.history.replaceState({}, '')
      void doSend(pending)
    }
  }, [loading, location.state, doSend])

  const handleSend = () => {
    if (!input.trim()) return
    const content = input.trim()
    setInput('')
    void doSend(content)
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
      <div className="flex items-center gap-3 border-b border-zinc-100 px-3 sm:px-4 py-3 shrink-0">
        <button
          onClick={() => navigate('/chats')}
          className="h-7 w-7 flex items-center justify-center rounded-md text-zinc-400 hover:text-zinc-700 hover:bg-zinc-100 transition-colors"
        >
          <ArrowLeft className="h-4 w-4" />
        </button>
        <div className="flex-1 min-w-0">
          <h2 className="text-sm font-semibold text-zinc-900 truncate">
            {detail?.session.title ?? 'Chat'}
          </h2>
        </div>
        <span className="text-xs text-zinc-400 shrink-0 font-mono">
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
                          onSubmit={
                            canInteract && id
                              ? answer => {
                                  if (awaitingInput) {
                                    setAwaitingInput(false)
                                    void provideInput(id, answer)
                                  } else {
                                    void doSend(answer)
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
                onSubmit={
                  awaitingInput && call.name === 'AskUserQuestion' && id
                    ? answer => {
                        setAwaitingInput(false)
                        void provideInput(id, answer)
                      }
                    : undefined
                }
              />
            ))}

          {/* Streaming: system status (tool execution in progress) */}
          {streaming && systemStatus && (
            <div className="flex items-center gap-2 pl-10 text-xs text-zinc-400">
              <Loader2 className="h-3 w-3 animate-spin shrink-0" />
              {systemStatus}
            </div>
          )}

          {/* Streaming: assistant response */}
          {streaming && streamingText && (
            <div className="flex gap-3">
              <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-900 text-white shrink-0 mt-0.5 text-xs font-bold">
                {agentLabel ? agentLabel[0].toUpperCase() : 'C'}
              </div>
              <div className="bg-zinc-50 border border-zinc-100 rounded-2xl rounded-tl-sm px-4 py-3 text-sm max-w-[90%] sm:max-w-[82%] overflow-x-auto min-w-0">
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

          {error && (
            <div className="rounded-md border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
              {error}
            </div>
          )}

          <div ref={bottomRef} />
        </div>
      </div>

      {/* Input */}
      <div className="border-t border-zinc-100 px-3 py-3 sm:px-6 shrink-0 bg-white">
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
          <button
            onClick={handleSend}
            disabled={!input.trim() || streaming}
            className={cn(
              'flex h-9 w-9 items-center justify-center rounded-md shrink-0 self-end transition-colors',
              input.trim() && !streaming
                ? 'bg-zinc-900 text-white hover:bg-zinc-700'
                : 'bg-zinc-100 text-zinc-400 cursor-not-allowed',
            )}
          >
            {streaming ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Send className="h-4 w-4" />
            )}
          </button>
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

function MessageBubble({ message }: { message: ChatMessage }) {
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
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-900 text-white shrink-0 mt-0.5 text-xs font-bold">
        C
      </div>
      <div className="bg-zinc-50 border border-zinc-100 rounded-2xl rounded-tl-sm px-4 py-3 text-sm max-w-[90%] sm:max-w-[82%] overflow-x-auto min-w-0">
        <div className="prose prose-sm max-w-none">
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{message.content}</ReactMarkdown>
        </div>
      </div>
    </div>
  )
}

function ThinkingBlock({ text }: { text: string }) {
  const [expanded, setExpanded] = useState(false)

  return (
    <div className="flex gap-3">
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-100 text-zinc-500 shrink-0 mt-0.5 text-xs">
        ✦
      </div>
      <div className="flex-1 max-w-[82%]">
        <button
          onClick={() => setExpanded(e => !e)}
          className="flex items-center gap-1.5 text-xs text-zinc-400 hover:text-zinc-600 transition-colors mb-1"
        >
          {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
          Thinking
        </button>
        {expanded && (
          <div className="rounded-lg border border-zinc-100 bg-zinc-50 px-3 py-2 text-xs text-zinc-500 font-mono whitespace-pre-wrap leading-relaxed">
            {text}
          </div>
        )}
      </div>
    </div>
  )
}

function ToolCallCard({
  block,
  isInteractive,
  onSubmit,
}: {
  block: Pick<SDKContentBlock, 'type' | 'id' | 'name' | 'input'>
  isInteractive?: boolean
  onSubmit?: (answer: string) => void
}) {
  const [expanded, setExpanded] = useState(false)
  const name = block.name ?? 'unknown'

  // AskUserQuestion gets its own rich interactive UI.
  if (name === 'AskUserQuestion' && block.input) {
    return (
      <AskUserQuestionCard input={block.input} isInteractive={isInteractive} onSubmit={onSubmit} />
    )
  }

  return (
    <div className="flex gap-3">
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-100 text-zinc-500 shrink-0 mt-0.5">
        <Terminal className="h-3.5 w-3.5" />
      </div>
      <div className="flex-1 min-w-0 max-w-[82%]">
        <button
          onClick={() => setExpanded(e => !e)}
          className="flex items-center gap-1.5 text-xs text-zinc-400 hover:text-zinc-600 transition-colors mb-1"
        >
          {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
          <span className="font-mono font-medium">{name}</span>
        </button>
        {expanded && block.input !== undefined && (
          <div className="rounded-lg border border-zinc-100 bg-zinc-50 px-3 py-2 text-xs text-zinc-500 font-mono whitespace-pre-wrap leading-relaxed overflow-x-auto max-h-48">
            {JSON.stringify(block.input, null, 2)}
          </div>
        )}
      </div>
    </div>
  )
}

function AskUserQuestionCard({
  input,
  isInteractive,
  onSubmit,
}: {
  input: Record<string, unknown>
  isInteractive?: boolean
  onSubmit?: (answer: string) => void
}) {
  const questions = (input.questions as AskUserQuestionItem[] | undefined) ?? []
  // selections[questionIndex] = array of selected option labels
  const [selections, setSelections] = useState<Record<number, string[]>>({})
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

  const hasSelections = questions.every((_, i) => (selections[i] ?? []).length > 0)

  const handleSubmit = () => {
    if (!onSubmit || submitted) return
    const lines = questions.map((q, i) => {
      const chosen = (selections[i] ?? []).join(', ')
      const header = q.header ?? `Q${i + 1}`
      return `${header}: ${chosen}`
    })
    onSubmit(lines.join('\n'))
    setSubmitted(true)
  }

  return (
    <div className="flex gap-3">
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-zinc-100 text-zinc-500 shrink-0 mt-0.5">
        <MessageSquare className="h-3.5 w-3.5" />
      </div>
      <div className="flex-1 min-w-0 max-w-[82%] space-y-3">
        {questions.map((q, i) => (
          <div key={i} className="rounded-lg border border-zinc-200 bg-white p-3">
            {q.header && (
              <div className="text-[10px] font-semibold uppercase tracking-wider text-zinc-400 mb-1">
                {q.header}
              </div>
            )}
            <div className="text-sm font-medium text-zinc-800 mb-2">{q.question}</div>
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
                        ? 'border-zinc-900 bg-zinc-900 text-white'
                        : 'border-zinc-200 bg-zinc-50 text-zinc-700',
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
            </div>
            {q.multiSelect && !submitted && (
              <div className="mt-2 text-[10px] text-zinc-400">Multiple selections allowed</div>
            )}
          </div>
        ))}

        {isInteractive && !submitted && (
          <button
            disabled={!hasSelections}
            onClick={handleSubmit}
            className={cn(
              'rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
              hasSelections
                ? 'bg-zinc-900 text-white hover:bg-zinc-700'
                : 'bg-zinc-100 text-zinc-400 cursor-not-allowed',
            )}
          >
            Send answers
          </button>
        )}
        {submitted && <div className="text-[10px] text-zinc-400">Answers sent</div>}
      </div>
    </div>
  )
}
