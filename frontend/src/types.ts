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
  agent_slug: string
  sdk_session_id: string
  created_at: string
  updated_at: string
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
  { value: 'claude-opus-4-6', label: 'Claude Opus 4.6' },
  { value: 'claude-sonnet-4-6', label: 'Claude Sonnet 4.6' },
  { value: 'claude-haiku-4-5', label: 'Claude Haiku 4.5' },
  { value: 'claude-haiku-4-5-20251001', label: 'Claude Haiku 4.5 (2025-10)' },
]

export const BUILT_IN_TOOLS = [
  'Read', 'Write', 'Edit', 'Bash', 'Glob', 'Grep',
  'WebFetch', 'WebSearch', 'Task', 'current_time',
]
