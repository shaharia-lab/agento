import { useState, useEffect, useRef, useCallback } from 'react'
import { useParams, useNavigate, useLocation } from 'react-router-dom'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { chatsApi, sendMessage } from '@/lib/api'
import type { ChatDetail, ChatMessage } from '@/types'
import { Textarea } from '@/components/ui/textarea'
import { ArrowLeft, Send, Loader2, ChevronDown, ChevronRight, Folder } from 'lucide-react'
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
      setError(null)

      abortRef.current = new AbortController()

      try {
        let accumulated = ''
        await sendMessage(
          id,
          content,
          {
            onThinking: text => {
              setThinkingText(prev => prev + text)
              setShowThinking(true)
            },
            onText: delta => {
              accumulated += delta
              setStreamingText(accumulated)
            },
            onDone: () => {
              const assistantMsg: ChatMessage = {
                role: 'assistant',
                content: accumulated,
                timestamp: new Date().toISOString(),
              }
              setMessages(prev => [...prev, assistantMsg])
              setStreamingText('')
              setThinkingText('')
              setShowThinking(false)

              if (detail) {
                chatsApi
                  .get(id)
                  .then(d => setDetail(d))
                  .catch(() => undefined)
              }
            },
            onError: msg => {
              setError(msg)
              setStreamingText('')
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

          {messages.map((msg, i) => (
            <MessageBubble key={i} message={msg} />
          ))}

          {/* Streaming: thinking */}
          {streaming && showThinking && thinkingText && <ThinkingBlock text={thinkingText} />}

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

          {/* Typing indicator */}
          {streaming && !streamingText && !thinkingText && (
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
