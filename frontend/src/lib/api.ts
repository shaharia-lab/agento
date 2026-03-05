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
  SDKUserEvent,
  SDKToolProgressEvent,
  SDKToolUseSummaryEvent,
  SDKTaskStartedEvent,
  SDKTaskProgressEvent,
  SDKTaskNotificationEvent,
  ClaudeSettingsResponse,
  ClaudeCodeSettings,
  ClaudeSettingsProfile,
  ClaudeSettingsProfileDetail,
  ClaudeProject,
  ClaudeSessionSummary,
  ClaudeSessionDetail,
  SessionJourney,
  AnalyticsReport,
  Integration,
  AvailableTool,
  NotificationSettings,
  NotificationLogEntry,
  ScheduledTask,
  JobHistoryEntry,
  UpdateCheckResponse,
  MonitoringConfig,
  MonitoringResponse,
  MonitoringTestResult,
  InsightSummary,
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

  updateTitle: (id: string, title: string) =>
    request<ChatSession>(`/chats/${id}`, { method: 'PATCH', body: JSON.stringify({ title }) }),

  /**
   * Creates a new chat session.
   * @param agentSlug - optional agent slug. Pass empty string or omit for no-agent chat.
   * @param workingDirectory - optional working directory for the session.
   * @param model - optional model override for the session.
   * @param settingsProfileId - optional settings profile ID for the session.
   */
  create: (
    agentSlug?: string,
    workingDirectory?: string,
    model?: string,
    settingsProfileId?: string,
  ) =>
    request<ChatSession>('/chats', {
      method: 'POST',
      body: JSON.stringify({
        agent_slug: agentSlug ?? '',
        working_directory: workingDirectory ?? '',
        model: model ?? '',
        settings_profile_id: settingsProfileId ?? '',
      }),
    }),

  delete: (id: string) =>
    fetch(`${BASE}/chats/${id}`, { method: 'DELETE' }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),

  bulkDelete: (ids: string[]) =>
    fetch(`${BASE}/chats`, {
      method: 'DELETE',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ids }),
    }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),

  toggleFavorite: (id: string, isFavorite: boolean) =>
    request<ChatSession>(`/chats/${id}`, {
      method: 'PATCH',
      body: JSON.stringify({ is_favorite: isFavorite }),
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

// ── Claude settings profiles ──────────────────────────────────────────────────

export const claudeSettingsProfilesApi = {
  list: () => request<ClaudeSettingsProfile[]>('/claude-settings/profiles'),

  create: (name: string) =>
    request<ClaudeSettingsProfile>('/claude-settings/profiles', {
      method: 'POST',
      body: JSON.stringify({ name }),
    }),

  get: (id: string) => request<ClaudeSettingsProfileDetail>(`/claude-settings/profiles/${id}`),

  update: (id: string, data: { name?: string; settings?: ClaudeCodeSettings }) =>
    request<ClaudeSettingsProfileDetail>(`/claude-settings/profiles/${id}`, {
      method: 'PUT',
      body: JSON.stringify(data),
    }),

  delete: (id: string) =>
    fetch(`${BASE}/claude-settings/profiles/${id}`, { method: 'DELETE' }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),

  duplicate: (id: string) =>
    request<ClaudeSettingsProfile>(`/claude-settings/profiles/${id}/duplicate`, {
      method: 'POST',
    }),

  setDefault: (id: string) =>
    request<ClaudeSettingsProfile>(`/claude-settings/profiles/${id}/default`, {
      method: 'PUT',
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

// ── Claude Code sessions ──────────────────────────────────────────────────────

export const claudeSessionsApi = {
  /**
   * List all Claude Code sessions, optionally filtered by project path or search query.
   */
  list: (params?: { project?: string; q?: string }) => {
    const qs = new URLSearchParams()
    if (params?.project) qs.set('project', params.project)
    if (params?.q) qs.set('q', params.q)
    const query = qs.toString()
    const suffix = query ? `?${query}` : ''
    return request<ClaudeSessionSummary[]>(`/claude-sessions${suffix}`)
  },

  /** List all projects (decoded paths) found in ~/.claude/projects/. */
  projects: () => request<ClaudeProject[]>('/claude-sessions/projects'),

  /** Get the full detail of a single session including messages and todos. */
  get: (id: string) => request<ClaudeSessionDetail>(`/claude-sessions/${id}`),

  /** Get the structured turn-by-turn journey visualization for a session. */
  journey: (id: string) => request<SessionJourney>(`/claude-sessions/${id}/journey`),

  /** Invalidate the server-side session cache and trigger a background rescan. */
  refresh: () =>
    fetch(`${BASE}/claude-sessions/refresh`, { method: 'POST' }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),

  /**
   * Create a new Agento chat session that inherits the given Claude Code session ID,
   * allowing the conversation to be continued in Agento's chat interface.
   * Returns the new Agento chat ID.
   */
  continue: (sessionId: string) =>
    request<{ chat_id: string }>(`/claude-sessions/${sessionId}/continue`, {
      method: 'POST',
    }),

  /** Set a user-defined title for a session (preserved across cache rescans). */
  updateTitle: (sessionId: string, customTitle: string) =>
    fetch(`${BASE}/claude-sessions/${sessionId}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ custom_title: customTitle }),
    }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),

  /** Toggle the is_favorite flag for a session (preserved across cache rescans). */
  toggleFavorite: (sessionId: string, isFavorite: boolean) =>
    fetch(`${BASE}/claude-sessions/${sessionId}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ is_favorite: isFavorite }),
    }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
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
  /**
   * Emitted when a tool needs user approval before executing.
   * The SSE connection stays open. Call permissionResponse() with allow=true/false.
   */
  onPermissionRequest?: (data: { tool_name: string; input: unknown }) => void
  /**
   * Emitted when a tool finishes executing. Contains the tool result keyed by tool_use_id.
   * Use this to render rich tool output (e.g. file content for Read, diff for Edit).
   */
  onToolResult?: (event: SDKUserEvent) => void
  /** Emitted during tool execution with incremental progress (progress float and message). */
  onToolProgress?: (event: SDKToolProgressEvent) => void
  /** Emitted when a tool finishes with a summary of what it did. */
  onToolUseSummary?: (event: SDKToolUseSummaryEvent) => void
  /** Emitted when a background task starts. */
  onTaskStarted?: (event: SDKTaskStartedEvent) => void
  /** Emitted during background task execution with progress updates. */
  onTaskProgress?: (event: SDKTaskProgressEvent) => void
  /** Emitted for task-related notifications. */
  onTaskNotification?: (event: SDKTaskNotificationEvent) => void
}

function dispatchSseEvent(eventType: string, data: unknown, callbacks: StreamCallbacks): void {
  switch (eventType) {
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
    case 'permission_request':
      callbacks.onPermissionRequest?.(data as { tool_name: string; input: unknown })
      break
    case 'user':
      callbacks.onToolResult?.(data as SDKUserEvent)
      break
    case 'tool_progress':
      callbacks.onToolProgress?.(data as SDKToolProgressEvent)
      break
    case 'tool_use_summary':
      callbacks.onToolUseSummary?.(data as SDKToolUseSummaryEvent)
      break
    case 'task_started':
      callbacks.onTaskStarted?.(data as SDKTaskStartedEvent)
      break
    case 'task_progress':
      callbacks.onTaskProgress?.(data as SDKTaskProgressEvent)
      break
    case 'task_notification':
      callbacks.onTaskNotification?.(data as SDKTaskNotificationEvent)
      break
  }
}

interface SseParserState {
  buffer: string
  currentEvent: string
}

function parseSseLine(line: string, state: SseParserState, callbacks: StreamCallbacks): void {
  if (line.startsWith('event: ')) {
    state.currentEvent = line.slice(7).trim()
    return
  }
  if (!line.startsWith('data: ')) return
  try {
    const data = JSON.parse(line.slice(6))
    dispatchSseEvent(state.currentEvent, data, callbacks)
  } catch {
    // ignore parse errors
  }
  state.currentEvent = ''
}

async function readSseStream(
  reader: ReadableStreamDefaultReader<Uint8Array>,
  callbacks: StreamCallbacks,
): Promise<void> {
  const decoder = new TextDecoder()
  const state: SseParserState = { buffer: '', currentEvent: '' }

  while (true) {
    const { done, value } = await reader.read()
    if (done) break

    state.buffer += decoder.decode(value, { stream: true })
    const lines = state.buffer.split('\n')
    state.buffer = lines.pop() ?? ''

    for (const line of lines) {
      parseSseLine(line, state, callbacks)
    }
  }
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

  await readSseStream(res.body!.getReader(), callbacks)
}

/**
 * Sends the user's allow/deny decision for a pending tool permission request.
 * The SSE stream for the chat stays open; the agent will continue after this call.
 */
export async function permissionResponse(chatId: string, allow: boolean): Promise<void> {
  const res = await fetch(`${BASE}/chats/${chatId}/permission`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ allow }),
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({ error: res.statusText }))
    throw new Error(body.error || `HTTP ${res.status}`)
  }
}

/**
 * Gracefully stops the active agent session for a chat.
 * Sends SIGINT to the subprocess, giving it a chance to finish and write the session.
 * The SSE stream will end shortly after this call.
 */
export async function stopSession(chatId: string): Promise<void> {
  const res = await fetch(`${BASE}/chats/${chatId}/stop`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({ error: res.statusText }))
    throw new Error(body.error || `HTTP ${res.status}`)
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

// ── Integrations ─────────────────────────────────────────────────────────────

export const integrationsApi = {
  list: () => request<Integration[]>('/integrations'),

  get: (id: string) => request<Integration>(`/integrations/${id}`),

  create: (data: Partial<Integration> & { credentials: Record<string, unknown> }) =>
    request<Integration>('/integrations', { method: 'POST', body: JSON.stringify(data) }),

  update: (id: string, data: Partial<Integration> & { credentials?: Record<string, unknown> }) =>
    request<Integration>(`/integrations/${id}`, { method: 'PUT', body: JSON.stringify(data) }),

  delete: (id: string) =>
    fetch(`${BASE}/integrations/${id}`, { method: 'DELETE' }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),

  startOAuth: (id: string) =>
    request<{ auth_url: string }>(`/integrations/${id}/auth/start`, { method: 'POST' }),

  getAuthStatus: (id: string) =>
    request<{ authenticated: boolean }>(`/integrations/${id}/auth/status`),

  validateAuth: (id: string) =>
    request<{ valid: boolean; validated?: boolean; error?: string }>(
      `/integrations/${id}/auth/validate`,
      { method: 'POST' },
    ),

  availableTools: () => request<AvailableTool[]>('/integrations/available-tools'),
}

// ── Notifications ─────────────────────────────────────────────────────────────

export const notificationsApi = {
  getSettings: () => request<NotificationSettings>('/notifications/settings'),

  updateSettings: (data: NotificationSettings) =>
    request<NotificationSettings>('/notifications/settings', {
      method: 'PUT',
      body: JSON.stringify(data),
    }),

  sendTest: () => request<{ status: string }>('/notifications/test', { method: 'POST' }),

  listLog: (limit?: number) => {
    const params = new URLSearchParams()
    if (limit) params.set('limit', String(limit))
    const suffix = params.toString() ? `?${params.toString()}` : ''
    return request<NotificationLogEntry[]>(`/notifications/log${suffix}`)
  },
}

// ── Scheduled Tasks ──────────────────────────────────────────────────────────

export const tasksApi = {
  list: () => request<ScheduledTask[]>('/tasks'),

  get: (id: string) => request<ScheduledTask>(`/tasks/${id}`),

  create: (data: Partial<ScheduledTask>) =>
    request<ScheduledTask>('/tasks', { method: 'POST', body: JSON.stringify(data) }),

  update: (id: string, data: Partial<ScheduledTask>) =>
    request<ScheduledTask>(`/tasks/${id}`, { method: 'PUT', body: JSON.stringify(data) }),

  delete: (id: string) =>
    fetch(`${BASE}/tasks/${id}`, { method: 'DELETE' }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),

  pause: (id: string) => request<ScheduledTask>(`/tasks/${id}/pause`, { method: 'POST' }),

  resume: (id: string) => request<ScheduledTask>(`/tasks/${id}/resume`, { method: 'POST' }),

  jobHistory: (id: string, limit?: number) => {
    const params = new URLSearchParams()
    if (limit) params.set('limit', String(limit))
    const query = params.toString()
    const suffix = query ? `?${query}` : ''
    return request<JobHistoryEntry[]>(`/tasks/${id}/job-history${suffix}`)
  },
}

export const jobHistoryApi = {
  list: (limit?: number, offset?: number) => {
    const params = new URLSearchParams()
    if (limit) params.set('limit', String(limit))
    if (offset) params.set('offset', String(offset))
    const query = params.toString()
    const suffix = query ? `?${query}` : ''
    return request<JobHistoryEntry[]>(`/job-history${suffix}`)
  },

  get: (id: string) => request<JobHistoryEntry>(`/job-history/${id}`),

  delete: (id: string) =>
    fetch(`${BASE}/job-history/${id}`, { method: 'DELETE' }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),

  bulkDelete: (ids: string[]) =>
    fetch(`${BASE}/job-history`, {
      method: 'DELETE',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ ids }),
    }).then(res => {
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
    }),
}

// ── Version / update check ────────────────────────────────────────────────────

export const versionApi = {
  checkUpdate: () => request<UpdateCheckResponse>('/version/update-check'),
}

// ── Monitoring / OTel ─────────────────────────────────────────────────────────

export const monitoringApi = {
  get: (): Promise<MonitoringResponse> => request<MonitoringResponse>('/monitoring'),

  update: (cfg: MonitoringConfig): Promise<MonitoringResponse> =>
    request<MonitoringResponse>('/monitoring', {
      method: 'PUT',
      body: JSON.stringify(cfg),
    }),

  test: (cfg: MonitoringConfig): Promise<MonitoringTestResult> =>
    request<MonitoringTestResult>('/monitoring/test', {
      method: 'POST',
      body: JSON.stringify(cfg),
    }),
}

// ── Analytics ─────────────────────────────────────────────────────────────────

export const analyticsApi = {
  get: (params?: { from?: string; to?: string; project?: string }): Promise<AnalyticsReport> => {
    const qs = new URLSearchParams()
    if (params?.from) qs.set('from', params.from)
    if (params?.to) qs.set('to', params.to)
    if (params?.project) qs.set('project', params.project)
    const query = qs.toString()
    const suffix = query ? `?${query}` : ''
    return request<AnalyticsReport>(`/claude-analytics${suffix}`)
  },
}

// ── Session Insights ─────────────────────────────────────────────────────────

export const insightsApi = {
  /** Aggregate insights across multiple (or all) sessions, optionally filtered by date range. */
  getSummary: (params?: {
    ids?: string[]
    from?: string
    to?: string
  }): Promise<InsightSummary> => {
    const qs = new URLSearchParams()
    if (params?.ids && params.ids.length > 0) qs.set('ids', params.ids.join(','))
    if (params?.from) qs.set('from', params.from)
    if (params?.to) qs.set('to', params.to)
    const suffix = qs.toString() ? `?${qs.toString()}` : ''
    return request<InsightSummary>(`/claude-sessions/insights/summary${suffix}`)
  },
}
