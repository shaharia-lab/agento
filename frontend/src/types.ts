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

export interface ChatMessage {
  role: 'user' | 'assistant'
  content: string
  timestamp: string
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
