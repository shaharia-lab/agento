import { useState, useEffect, useRef, useCallback } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { chatsApi, sendMessage } from '@/lib/api'
import type { ChatDetail, ChatMessage } from '@/types'
import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import { Badge } from '@/components/ui/badge'
import { ScrollArea } from '@/components/ui/scroll-area'
import { ArrowLeft, Send, Loader2, BrainCircuit } from 'lucide-react'

export default function ChatSessionPage() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()
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

  const handleSend = async () => {
    if (!input.trim() || streaming || !id) return

    const content = input.trim()
    setInput('')

    // Optimistically show user message
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
            // Add assistant message to state
            const assistantMsg: ChatMessage = {
              role: 'assistant',
              content: accumulated,
              timestamp: new Date().toISOString(),
            }
            setMessages(prev => [...prev, assistantMsg])
            setStreamingText('')
            setThinkingText('')
            setShowThinking(false)

            // Refresh session title
            if (detail) {
              chatsApi.get(id).then(d => setDetail(d)).catch(() => undefined)
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
  }

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      void handleSend()
    }
  }

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
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

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center gap-3 border-b border-border px-4 py-3 shrink-0">
        <button
          onClick={() => navigate('/chats')}
          className="h-7 w-7 flex items-center justify-center rounded-md text-muted-foreground hover:text-foreground hover:bg-gray-100 transition-colors"
        >
          <ArrowLeft className="h-4 w-4" />
        </button>
        <div className="flex-1 min-w-0">
          <h2 className="text-sm font-semibold truncate">{detail?.session.title ?? 'Chat'}</h2>
        </div>
        {detail?.session.agent_slug && (
          <Badge variant="secondary" className="text-xs shrink-0">
            {detail.session.agent_slug}
          </Badge>
        )}
      </div>

      {/* Messages */}
      <ScrollArea className="flex-1">
        <div className="flex flex-col gap-4 px-4 py-6 max-w-3xl mx-auto w-full">
          {messages.length === 0 && !streaming && (
            <div className="flex flex-col items-center justify-center py-16 text-center">
              <div className="text-3xl mb-3">💬</div>
              <p className="text-sm text-muted-foreground">
                Send a message to start the conversation.
              </p>
            </div>
          )}

          {messages.map((msg, i) => (
            <MessageBubble key={i} message={msg} />
          ))}

          {/* Streaming: thinking */}
          {streaming && showThinking && thinkingText && (
            <div className="flex gap-3">
              <div className="flex h-7 w-7 items-center justify-center rounded-full bg-purple-100 text-purple-600 shrink-0 mt-0.5">
                <BrainCircuit className="h-3.5 w-3.5" />
              </div>
              <div className="bg-purple-50 border border-purple-100 rounded-lg px-3 py-2 text-xs text-purple-700 max-w-2xl font-mono whitespace-pre-wrap opacity-80">
                {thinkingText}
              </div>
            </div>
          )}

          {/* Streaming: assistant response */}
          {streaming && streamingText && (
            <div className="flex gap-3">
              <div className="flex h-7 w-7 items-center justify-center rounded-full bg-gray-100 text-gray-600 shrink-0 mt-0.5 text-xs font-bold">
                A
              </div>
              <div className="bg-gray-50 rounded-lg px-4 py-3 text-sm max-w-2xl">
                <div className="prose prose-sm max-w-none">
                  <ReactMarkdown remarkPlugins={[remarkGfm]}>{streamingText}</ReactMarkdown>
                </div>
              </div>
            </div>
          )}

          {/* Loading indicator (no text yet) */}
          {streaming && !streamingText && !thinkingText && (
            <div className="flex gap-3 items-center">
              <div className="flex h-7 w-7 items-center justify-center rounded-full bg-gray-100 shrink-0">
                <Loader2 className="h-3.5 w-3.5 animate-spin text-gray-500" />
              </div>
              <div className="text-xs text-muted-foreground animate-pulse">Thinking…</div>
            </div>
          )}

          {/* Error */}
          {error && (
            <div className="rounded-md bg-red-50 border border-red-200 px-4 py-3 text-sm text-red-700">
              {error}
            </div>
          )}

          <div ref={bottomRef} />
        </div>
      </ScrollArea>

      {/* Input */}
      <div className="border-t border-border px-4 py-3 shrink-0">
        <div className="flex gap-2 max-w-3xl mx-auto">
          <Textarea
            value={input}
            onChange={e => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Message… (Enter to send, Shift+Enter for new line)"
            className="min-h-[40px] max-h-[200px] resize-none text-sm"
            disabled={streaming}
            rows={1}
          />
          <Button
            size="icon"
            onClick={() => void handleSend()}
            disabled={!input.trim() || streaming}
            className="h-9 w-9 shrink-0 self-end"
          >
            {streaming ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Send className="h-4 w-4" />
            )}
          </Button>
        </div>
        <p className="text-center text-xs text-muted-foreground mt-1.5">
          AI can make mistakes. Verify important information.
        </p>
      </div>
    </div>
  )
}

function MessageBubble({ message }: { message: ChatMessage }) {
  const isUser = message.role === 'user'

  if (isUser) {
    return (
      <div className="flex justify-end">
        <div className="bg-gray-900 text-white rounded-2xl rounded-tr-sm px-4 py-2.5 text-sm max-w-[75%] whitespace-pre-wrap">
          {message.content}
        </div>
      </div>
    )
  }

  return (
    <div className="flex gap-3">
      <div className="flex h-7 w-7 items-center justify-center rounded-full bg-indigo-100 text-indigo-700 shrink-0 mt-0.5 text-xs font-bold">
        A
      </div>
      <div className="bg-gray-50 rounded-2xl rounded-tl-sm px-4 py-3 text-sm max-w-[80%]">
        <div className="prose prose-sm max-w-none">
          <ReactMarkdown remarkPlugins={[remarkGfm]}>{message.content}</ReactMarkdown>
        </div>
      </div>
    </div>
  )
}
