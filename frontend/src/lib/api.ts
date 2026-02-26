import type {
  Agent,
  ChatSession,
  ChatDetail,
  SettingsResponse,
  UserSettings,
  FSListResponse,
  SDKSystemEvent,
  SDKAssistantEvent,
  SDKStreamEventMessage,
  SDKResultEvent,
  ClaudeSettingsResponse,
  ClaudeCodeSettings,
} from '../types'

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

  /**
   * Creates a new chat session.
   * @param agentSlug - optional agent slug. Pass empty string or omit for no-agent chat.
   * @param workingDirectory - optional working directory for the session.
   * @param model - optional model override for the session.
   */
  create: (agentSlug?: string, workingDirectory?: string, model?: string) =>
    request<ChatSession>('/chats', {
      method: 'POST',
      body: JSON.stringify({
        agent_slug: agentSlug ?? '',
        working_directory: workingDirectory ?? '',
        model: model ?? '',
      }),
    }),

  delete: (id: string) =>
    fetch(`${BASE}/chats/${id}`, { method: 'DELETE' }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),
}

// ── Settings ──────────────────────────────────────────────────────────────────

export const settingsApi = {
  get: () => request<SettingsResponse>('/settings'),

  update: (data: Partial<UserSettings>) =>
    request<SettingsResponse>('/settings', {
      method: 'PUT',
      body: JSON.stringify(data),
    }),
}

// ── Claude Code settings (~/.claude/settings.json) ────────────────────────────

export const claudeSettingsApi = {
  get: () => request<ClaudeSettingsResponse>('/claude-settings'),

  update: (data: ClaudeCodeSettings) =>
    request<ClaudeSettingsResponse>('/claude-settings', {
      method: 'PUT',
      body: JSON.stringify(data),
    }),
}

// ── Filesystem ────────────────────────────────────────────────────────────────

export const filesystemApi = {
  list: (path?: string) => {
    const params = new URLSearchParams()
    if (path) params.set('path', path)
    return request<FSListResponse>(`/fs?${params.toString()}`)
  },

  mkdir: (path: string) =>
    request<{ path: string }>('/fs/mkdir', {
      method: 'POST',
      body: JSON.stringify({ path }),
    }),
}

// ── Streaming message ─────────────────────────────────────────────────────────

/**
 * Typed callbacks for the raw SDK event stream.
 * Each callback corresponds to one SSE event type emitted by the backend.
 */
export interface StreamCallbacks {
  /** Emitted at session start (subtype "init") and for tool-execution status (subtype "status"). */
  onSystem?: (event: SDKSystemEvent) => void
  /** Emitted when the LLM completes a turn — may contain tool_use and/or text content blocks. */
  onAssistant?: (event: SDKAssistantEvent) => void
  /** Emitted for every LLM output delta (text, thinking, tool-input streaming). */
  onStreamEvent?: (event: SDKStreamEventMessage) => void
  /** Terminal event — either a successful result or an error. Check event.is_error. */
  onResult?: (event: SDKResultEvent) => void
  /**
   * Emitted when the agent called AskUserQuestion and is waiting for the user's answer.
   * The SSE connection stays open. Call provideInput() with the answer to continue.
   */
  onUserInputRequired?: (data: { input: Record<string, unknown> }) => void
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
            case 'system':
              callbacks.onSystem?.(data as SDKSystemEvent)
              break
            case 'assistant':
              callbacks.onAssistant?.(data as SDKAssistantEvent)
              break
            case 'stream_event':
              callbacks.onStreamEvent?.(data as SDKStreamEventMessage)
              break
            case 'result':
              callbacks.onResult?.(data as SDKResultEvent)
              break
            case 'user_input_required':
              callbacks.onUserInputRequired?.(data as { input: Record<string, unknown> })
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

/**
 * Sends the user's answer to an AskUserQuestion prompt back to the agent.
 * The SSE stream for the chat stays open; the agent will continue after this call.
 */
export async function provideInput(chatId: string, answer: string): Promise<void> {
  const res = await fetch(`${BASE}/chats/${chatId}/input`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ answer }),
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({ error: res.statusText }))
    throw new Error(body.error || `HTTP ${res.status}`)
  }
}
