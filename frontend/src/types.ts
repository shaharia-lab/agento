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
  /** Controls tool permission behaviour. Empty string means "bypass" (default). */
  permission_mode: 'bypass' | 'default' | ''
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
  /** Cumulative token usage across all turns. Zero when not yet populated. */
  total_input_tokens?: number
  total_output_tokens?: number
  total_cache_creation_tokens?: number
  total_cache_read_tokens?: number
}

export interface UserSettings {
  default_working_dir: string
  default_model: string
  onboarding_complete: boolean
  appearance_dark_mode?: boolean
  appearance_font_size?: number
  appearance_font_family?: string
}

export interface SettingsResponse {
  settings: UserSettings
  /** Map of field name → env var name for env-locked settings. */
  locked: Record<string, string>
  /**
   * True when the displayed default model comes from an environment variable
   * (AGENTO_DEFAULT_MODEL or ANTHROPIC_DEFAULT_SONNET_MODEL).
   */
  model_from_env: boolean
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
  | {
      type: 'tool_use'
      id?: string
      name: string
      input?: Record<string, unknown>
      /** Tool execution result, captured from the SDK "user" event. In-memory only. */
      toolResult?: Record<string, unknown>
    }

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
  { value: 'sonnet', label: 'Sonnet' },
  { value: 'opus', label: 'Opus' },
  { value: 'haiku', label: 'Haiku' },
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

/** A single hunk in a structured patch (from Edit tool result). */
export interface SDKPatchHunk {
  oldStart: number
  oldLines: number
  newStart: number
  newLines: number
  lines: string[]
}

/** Tool execution result for the Read tool. */
export interface SDKToolUseResultFile {
  type: 'text'
  file: {
    filePath: string
    content: string
    numLines: number
    startLine: number
    totalLines: number
  }
}

/** Tool execution result for the Edit tool. */
export interface SDKToolUseResultEdit {
  filePath: string
  oldString: string
  newString: string
  originalFile: string
  structuredPatch: SDKPatchHunk[]
  userModified: boolean
  replaceAll: boolean
}

/**
 * Emitted when a tool finishes executing (the SDK "user" event).
 * Contains the raw tool result alongside the tool_use_id that links it to the tool call.
 */
export interface SDKUserEvent {
  type: 'user'
  message: {
    role: 'user'
    content: Array<{
      tool_use_id: string
      type: string
      content: string
    }>
  }
  tool_use_result?: SDKToolUseResultFile | SDKToolUseResultEdit | Record<string, unknown>
  session_id: string
  uuid: string
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

// ── Integrations ──────────────────────────────────────────────────────────────

export interface ServiceConfig {
  enabled: boolean
  tools: string[]
}

export interface Integration {
  id: string
  name: string
  type: 'google'
  enabled: boolean
  authenticated: boolean
  services: Record<string, ServiceConfig>
  created_at: string
  updated_at: string
}

export interface GoogleCredentials {
  client_id: string
  client_secret: string
}

export interface AvailableTool {
  integration_id: string
  integration_name: string
  tool_name: string
  qualified_name: string
  service: string
}

// ── Claude settings profiles ──────────────────────────────────────────────────

export interface ClaudeSettingsProfile {
  id: string
  name: string
  file_path: string
  is_default: boolean
}

export interface ClaudeSettingsProfileDetail extends ClaudeSettingsProfile {
  settings: ClaudeCodeSettings | null
  exists: boolean
}

// ── Claude Code settings (~/.claude/settings.json) ────────────────────────────

/**
 * Represents the contents of $HOME/.claude/settings.json.
 * All fields are optional since the user may only set a subset.
 * The index signature allows forward-compatibility with future schema additions.
 */
export interface ClaudeCodeSettings {
  $schema?: string

  // Model & Language
  model?: string
  language?: string
  effortLevel?: 'low' | 'medium' | 'high'
  autoUpdatesChannel?: 'stable' | 'latest'
  outputStyle?: string
  availableModels?: string[]

  // UI & Display
  fastMode?: boolean
  showTurnDuration?: boolean
  spinnerTipsEnabled?: boolean
  terminalProgressBarEnabled?: boolean
  prefersReducedMotion?: boolean
  alwaysThinkingEnabled?: boolean
  teammateMode?: 'auto' | 'in-process' | 'tmux'
  spinnerVerbs?: Record<string, unknown>
  spinnerTipsOverride?: Record<string, unknown>

  // Behaviour
  cleanupPeriodDays?: number
  respectGitignore?: boolean
  skipWebFetchPreflight?: boolean
  plansDirectory?: string
  disableAllHooks?: boolean

  // Permissions & Security
  enableAllProjectMcpServers?: boolean
  allowManagedHooksOnly?: boolean
  allowManagedPermissionRulesOnly?: boolean
  allowManagedMcpServersOnly?: boolean
  allowManagedDomainsOnly?: boolean
  /** @deprecated Use attribution instead */
  includeCoAuthoredBy?: boolean
  forceLoginMethod?: 'claudeai' | 'console'
  forceLoginOrgUUID?: string

  // MCP
  enabledMcpjsonServers?: string[]
  disabledMcpjsonServers?: string[]
  allowedMcpServers?: string[]
  deniedMcpServers?: string[]

  // Plugins & Marketplaces
  enabledPlugins?: Record<string, unknown>
  pluginConfigs?: Record<string, unknown>
  extraKnownMarketplaces?: Record<string, unknown>
  strictKnownMarketplaces?: string[]
  skippedMarketplaces?: string[]
  skippedPlugins?: string[]
  blockedMarketplaces?: string[]

  // Complex objects (edited as raw JSON in the UI)
  permissions?: {
    allow?: string[]
    deny?: string[]
    ask?: string[]
    defaultMode?: string
    disableBypassPermissionsMode?: string
    additionalDirectories?: string[]
  }
  hooks?: Record<string, unknown>
  env?: Record<string, string>
  sandbox?: Record<string, unknown>
  attribution?: { commit?: string; pr?: string }
  statusLine?: Record<string, unknown>
  fileSuggestion?: Record<string, unknown>

  // Helpers & integrations
  apiKeyHelper?: string
  awsCredentialExport?: string
  awsAuthRefresh?: string
  otelHeadersHelper?: string

  // Misc
  companyAnnouncements?: unknown[]

  // Forward-compatibility: future schema additions pass through unchanged.
  [key: string]: unknown
}

export interface ClaudeSettingsResponse {
  exists: boolean
  /** Undefined when exists is false. */
  settings?: ClaudeCodeSettings
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

// ── Claude Code sessions (~/.claude) ─────────────────────────────────────────

export interface ClaudeTokenUsage {
  input_tokens: number
  output_tokens: number
  cache_creation_tokens: number
  cache_read_tokens: number
}

export interface ClaudeProject {
  encoded_name: string
  decoded_path: string
  session_count: number
}

export interface ClaudeSessionSummary {
  session_id: string
  project_path: string
  preview: string
  start_time: string
  last_activity: string
  message_count: number
  usage: ClaudeTokenUsage
  git_branch?: string
  model?: string
  cwd?: string
}

export interface ClaudeNormalizedBlock {
  type: 'thinking' | 'text' | 'tool_use'
  text?: string
  id?: string
  name?: string
  input?: Record<string, unknown>
}

export interface ClaudeMessage {
  uuid: string
  parent_uuid?: string
  type: 'user' | 'assistant' | 'progress'
  timestamp: string
  role?: string
  content?: string
  blocks?: ClaudeNormalizedBlock[]
  usage?: ClaudeTokenUsage
  git_branch?: string
  is_sidechain?: boolean
  children?: ClaudeMessage[]
}

export interface ClaudeTodo {
  content: string
  status: 'completed' | 'in_progress' | 'pending'
  active_form?: string
}

export interface ClaudeSessionDetail extends ClaudeSessionSummary {
  messages: ClaudeMessage[]
  todos: ClaudeTodo[]
}

// ── Scheduled Tasks ──────────────────────────────────────────────────────────

export type ScheduleType = 'one_off' | 'interval' | 'cron'
export type TaskStatus = 'active' | 'paused'
export type JobStatus = 'running' | 'success' | 'failed'

export interface ScheduleConfig {
  run_at?: string
  every_minutes?: number
  every_hours?: number
  every_days?: number
  at_time?: string
  expression?: string
}

export interface ScheduledTask {
  id: string
  name: string
  description: string
  prompt: string
  agent_slug: string
  working_directory: string
  model: string
  settings_profile_id: string
  timeout_minutes: number
  schedule_type: ScheduleType
  schedule_config: ScheduleConfig
  stop_after_count: number
  stop_after_time?: string
  status: TaskStatus
  run_count: number
  last_run_at?: string
  last_run_status: string
  next_run_at?: string
  created_at: string
  updated_at: string
}

export interface JobHistoryEntry {
  id: string
  task_id: string
  task_name: string
  agent_slug: string
  status: JobStatus
  started_at: string
  finished_at?: string
  duration_ms: number
  chat_session_id: string
  model: string
  prompt_preview: string
  error_message: string
  total_input_tokens: number
  total_output_tokens: number
  total_cache_creation_tokens: number
  total_cache_read_tokens: number
}

// ── Analytics ─────────────────────────────────────────────────────────────────

export interface AnalyticsSummary {
  total_sessions: number
  total_tokens: number
  total_input_tokens: number
  total_output_tokens: number
  total_cache_read_tokens: number
  total_cache_creation_tokens: number
  most_used_model: string
  avg_tokens_per_session: number
  estimated_cost_usd: number
}

export interface TimeSeriesPoint {
  date: string
  input_tokens: number
  output_tokens: number
  cache_read_tokens: number
  cache_creation_tokens: number
  total_tokens: number
  sessions: number
}

export interface CacheEfficiencyPoint {
  date: string
  cache_hit_rate: number
  cached_tokens: number
  total_input_tokens: number
}

export interface ModelStat {
  model: string
  tokens: number
  percentage: number
}

export interface ModelSessionStat {
  model: string
  sessions: number
}

export interface DayActivity {
  date: string
  sessions: number
  tokens: number
}

export interface HeatmapCell {
  day_of_week: number // 0=Sunday … 6=Saturday
  hour: number // 0-23
  sessions: number
  tokens: number
}

export interface HourlyActivity {
  hour: number
  sessions: number
  tokens: number
}

export interface CostPoint {
  date: string
  estimated_cost_usd: number
}

export interface CostSummary {
  input_cost_usd: number
  output_cost_usd: number
  cache_read_cost_usd: number
  cache_write_cost_usd: number
  total_cost_usd: number
}

export interface AnalyticsReport {
  summary: AnalyticsSummary
  time_series: TimeSeriesPoint[]
  cache_efficiency: CacheEfficiencyPoint[]
  model_breakdown: ModelStat[]
  sessions_per_model: ModelSessionStat[]
  most_active_days: DayActivity[]
  heatmap: HeatmapCell[]
  hourly_activity: HourlyActivity[]
  cost_over_time: CostPoint[]
  cost_summary: CostSummary
  projects: string[]
}
