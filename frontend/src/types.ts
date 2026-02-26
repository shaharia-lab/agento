export interface AgentCapabilities {
  built_in?: string[]
  local?: string[]
  mcp?: Record<string, { tools: string[] }>
}

export interface Agent {
  name: string
  slug: string
  description: string
  model: string
  thinking: 'adaptive' | 'enabled' | 'disabled'
  system_prompt: string
  capabilities: AgentCapabilities
}

export interface ChatSession {
  id: string
  title: string
  /** Empty string when no agent is selected (direct chat). */
  agent_slug: string
  sdk_session_id: string
  working_directory: string
  model: string
  created_at: string
  updated_at: string
}

export interface UserSettings {
  default_working_dir: string
  default_model: string
  onboarding_complete: boolean
}

export interface SettingsResponse {
  settings: UserSettings
  /** Map of field name → env var name for env-locked settings. */
  locked: Record<string, string>
}

export interface FSEntry {
  name: string
  is_dir: boolean
  path: string
}

export interface FSListResponse {
  path: string
  parent: string
  entries: FSEntry[]
}

/**
 * An ordered content block inside an assistant message.
 * Stored in-memory only — not persisted to the database.
 * The ordering of blocks in the array reflects the order they arrived in the stream,
 * so thinking → text → tool_use or tool_use → text are both represented correctly.
 */
export type MessageBlock =
  | { type: 'thinking'; text: string }
  | { type: 'text'; text: string }
  | { type: 'tool_use'; id?: string; name: string; input?: Record<string, unknown> }

export interface ChatMessage {
  role: 'user' | 'assistant'
  content: string
  timestamp: string
  /**
   * Ordered content blocks for assistant messages (in-memory only).
   * When present, the UI renders from blocks instead of content.
   * Falls back to content-only for messages loaded from the database.
   */
  blocks?: MessageBlock[]
}

// ── AskUserQuestion tool types ─────────────────────────────────────────────

export interface AskUserQuestionOption {
  label: string
  description?: string
}

export interface AskUserQuestionItem {
  question: string
  header?: string
  multiSelect?: boolean
  options: AskUserQuestionOption[]
}

export interface ChatDetail {
  session: ChatSession
  messages: ChatMessage[]
}

export const MODELS = [
  { value: 'eu.anthropic.claude-sonnet-4-5-20250929-v1:0', label: 'Claude Sonnet 4.5 (EU)' },
  { value: 'claude-opus-4-6', label: 'Claude Opus 4.6' },
  { value: 'claude-sonnet-4-6', label: 'Claude Sonnet 4.6' },
  { value: 'claude-haiku-4-5', label: 'Claude Haiku 4.5' },
  { value: 'claude-haiku-4-5-20251001', label: 'Claude Haiku 4.5 (2025-10)' },
]

// ── Raw SDK streaming event types ─────────────────────────────────────────────

/** Emitted at session start (subtype "init") and as tool-execution status updates (subtype "status"). */
export interface SDKSystemEvent {
  type: 'system'
  subtype: string
  status?: string
  message?: string
  session_id?: string
  cwd?: string
  model?: string
  tools?: string[]
  /** camelCase in the JSON protocol */
  permissionMode?: string
  claude_code_version?: string
  /** camelCase in the JSON protocol */
  apiKeySource?: string
}

/** A single content block inside an assistant message. */
export interface SDKContentBlock {
  type: string
  /** Populated when type is "text" */
  text?: string
  /** Populated when type is "thinking" */
  thinking?: string
  /** Populated when type is "tool_use" */
  id?: string
  name?: string
  input?: Record<string, unknown>
}

/** Emitted when the LLM completes a turn (may contain tool_use and/or text blocks). */
export interface SDKAssistantEvent {
  type: 'assistant'
  message: {
    role: 'assistant'
    content: SDKContentBlock[]
  }
  session_id: string
  uuid: string
  parent_tool_use_id?: string | null
}

/** The incremental delta payload inside a stream_event. */
export interface SDKStreamDelta {
  /** "thinking_delta" | "text_delta" | "input_json_delta" | … */
  type: string
  text?: string
  thinking?: string
  partial_json?: string
}

/** The inner Anthropic API streaming event (content_block_delta, content_block_start, …). */
export interface SDKInnerStreamEvent {
  type: string
  delta?: SDKStreamDelta
  index?: number
}

/** Emitted during LLM output streaming (wraps Anthropic API stream events). */
export interface SDKStreamEventMessage {
  type: 'stream_event'
  event: SDKInnerStreamEvent
  session_id: string
  uuid: string
  parent_tool_use_id?: string | null
}

export interface SDKUsage {
  input_tokens: number
  output_tokens: number
  cache_read_input_tokens: number
  cache_creation_input_tokens: number
}

/** Terminal event emitted when the agent finishes (success or error). */
export interface SDKResultEvent {
  type: 'result'
  subtype: string
  result: string
  is_error: boolean
  duration_ms: number
  duration_api_ms: number
  num_turns: number
  total_cost_usd: number
  usage: SDKUsage
  session_id: string
  uuid: string
  errors?: string[]
  stop_reason?: string | null
}

export const BUILT_IN_TOOLS = [
  'Read',
  'Write',
  'Edit',
  'Bash',
  'Glob',
  'Grep',
  'WebFetch',
  'WebSearch',
  'Task',
  'current_time',
]
