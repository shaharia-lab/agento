import type { Agent, ChatSession, ChatDetail } from '../types'

const BASE = '/api'

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    headers: { 'Content-Type': 'application/json', ...options?.headers },
    ...options,
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({ error: res.statusText }))
    throw new Error(body.error || `HTTP ${res.status}`)
  }
  return res.json() as Promise<T>
}

// ── Agents ────────────────────────────────────────────────────────────────────

export const agentsApi = {
  list: () => request<Agent[]>('/agents'),

  get: (slug: string) => request<Agent>(`/agents/${slug}`),

  create: (data: Partial<Agent>) =>
    request<Agent>('/agents', { method: 'POST', body: JSON.stringify(data) }),

  update: (slug: string, data: Partial<Agent>) =>
    request<Agent>(`/agents/${slug}`, { method: 'PUT', body: JSON.stringify(data) }),

  delete: (slug: string) =>
    fetch(`${BASE}/agents/${slug}`, { method: 'DELETE' }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),
}

// ── Chats ─────────────────────────────────────────────────────────────────────

export const chatsApi = {
  list: () => request<ChatSession[]>('/chats'),

  get: (id: string) => request<ChatDetail>(`/chats/${id}`),

  create: (agentSlug: string) =>
    request<ChatSession>('/chats', {
      method: 'POST',
      body: JSON.stringify({ agent_slug: agentSlug }),
    }),

  delete: (id: string) =>
    fetch(`${BASE}/chats/${id}`, { method: 'DELETE' }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),
}

// ── Streaming message ─────────────────────────────────────────────────────────

export interface StreamCallbacks {
  onThinking?: (text: string) => void
  onText?: (delta: string) => void
  onDone?: (data: { sdk_session_id: string; cost_usd: number }) => void
  onError?: (msg: string) => void
}

export async function sendMessage(
  chatId: string,
  content: string,
  callbacks: StreamCallbacks,
  signal?: AbortSignal,
): Promise<void> {
  const res = await fetch(`${BASE}/chats/${chatId}/messages`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ content }),
    signal,
  })

  if (!res.ok) {
    const body = await res.json().catch(() => ({ error: res.statusText }))
    throw new Error(body.error || `HTTP ${res.status}`)
  }

  const reader = res.body!.getReader()
  const decoder = new TextDecoder()
  let buffer = ''
  let currentEvent = ''

  while (true) {
    const { done, value } = await reader.read()
    if (done) break

    buffer += decoder.decode(value, { stream: true })
    const lines = buffer.split('\n')
    buffer = lines.pop() ?? ''

    for (const line of lines) {
      if (line.startsWith('event: ')) {
        currentEvent = line.slice(7).trim()
      } else if (line.startsWith('data: ')) {
        try {
          const data = JSON.parse(line.slice(6))
          switch (currentEvent) {
            case 'thinking':
              callbacks.onThinking?.(data.text)
              break
            case 'text':
              callbacks.onText?.(data.delta)
              break
            case 'done':
              callbacks.onDone?.(data)
              break
            case 'error':
              callbacks.onError?.(data.error)
              break
          }
        } catch {
          // ignore parse errors
        }
        currentEvent = ''
      }
    }
  }
}
