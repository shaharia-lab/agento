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
  permission_mode: 'bypass' | 'default' | 'plan' | 'dontAsk' | ''
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
  is_favorite?: boolean
}

export interface UserSettings {
  default_working_dir: string
  default_model: string
  onboarding_complete: boolean
  appearance_dark_mode?: boolean
  appearance_font_size?: number
  appearance_font_family?: string
  notification_settings?: string
  event_bus_worker_pool_size?: number
  public_url?: string
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
  | { type: 'thinking'; text: string; _key?: string }
  | { type: 'text'; text: string; _key?: string }
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
  web_search_requests?: number
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

/** Per-model token and cost breakdown. */
export interface SDKModelUsage {
  input_tokens: number
  output_tokens: number
  cache_read_input_tokens: number
  cache_creation_input_tokens: number
  cost_usd: number
  context_window?: number
  max_output_tokens?: number
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
  /** Per-model token and cost breakdowns keyed by model ID. */
  model_usages?: Record<string, SDKModelUsage>
  /** Tool calls that were denied during the run. */
  permission_denials?: string[]
  /** Parsed structured output when OutputFormat was requested. */
  structured_output?: unknown
}

// ── SDK v0.3.0 event types ────────────────────────────────────────────────────

/** Emitted during tool execution with incremental progress updates. */
export interface SDKToolProgressEvent {
  type: 'tool_progress'
  tool_use_id: string
  progress?: number
  message?: string
}

/** Emitted when a tool finishes, carrying a summary of what the tool did. */
export interface SDKToolUseSummaryEvent {
  type: 'tool_use_summary'
  tool_use_id?: string
  summary?: string
}

/** Emitted when a background task starts. */
export interface SDKTaskStartedEvent {
  type: 'task_started'
  task_id?: string
  status?: string
  message?: string
}

/** Emitted during background task execution with progress updates. */
export interface SDKTaskProgressEvent {
  type: 'task_progress'
  task_id?: string
  status?: string
  message?: string
}

/** Emitted for task-related notifications. */
export interface SDKTaskNotificationEvent {
  type: 'task_notification'
  task_id?: string
  status?: string
  message?: string
}

// ── Integrations ──────────────────────────────────────────────────────────────

export interface ServiceConfig {
  enabled: boolean
  tools: string[]
}

export interface Integration {
  id: string
  name: string
  type: 'google' | 'telegram' | 'jira' | 'confluence' | 'slack' | 'github' | 'whatsapp'
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

export interface TelegramCredentials {
  bot_token: string
}

export interface AtlassianCredentials {
  site_url: string
  email: string
  api_token: string
}

export interface SlackCredentials {
  auth_mode: 'bot_token' | 'oauth'
  bot_token?: string
  client_id?: string
  client_secret?: string
}

export interface GitHubCredentials {
  auth_mode: 'pat' | 'oauth' | 'app'
  personal_access_token?: string
  client_id?: string
  client_secret?: string
  app_id?: string
  private_key?: string
  installation_id?: string
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
  /** Total cache writes; the 5m/1h fields below split it by cache TTL. */
  cache_creation_tokens: number
  cache_creation_5m_tokens: number
  cache_creation_1h_tokens: number
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
  custom_title?: string
  /** Claude Code's own `/rename`, refreshed on every scan. */
  native_title?: string
  /** Claude Code's auto-generated title, refreshed on every scan. */
  ai_title?: string
  /** Resolved label: custom_title || native_title || ai_title || preview. */
  display_title?: string
  is_favorite?: boolean
  start_time: string
  last_activity: string
  /** Main-thread active time: inter-event gaps capped at 10 min. Excludes delegated work. */
  active_duration_ms: number
  /** Summed active time of the session's sub-agent transcripts, mirroring subagent_usage. */
  subagent_active_duration_ms: number
  /** Conversational turns: real user input plus assistant replies containing text. */
  message_count: number
  /** Raw top-level user + assistant events, i.e. API round-trips. */
  event_count: number
  /** Main-thread usage only — delegated work is in `subagent_usage`. */
  usage: ClaudeTokenUsage
  git_branch?: string
  model?: string
  cwd?: string
  /** Number of sub-agent transcripts under `<session-id>/subagents/`. */
  subagent_count: number
  /** Summed usage of those transcripts, reported additively to `usage`. */
  subagent_usage: ClaudeTokenUsage
  /**
   * `subagent_usage` keyed by the model each sub-agent actually ran. Summing
   * its values reproduces `subagent_usage`; it exists so model attribution can
   * credit delegated tokens to the model that spent them. Omitted when the
   * session delegated nothing.
   */
  subagent_usage_by_model?: Record<string, ClaudeTokenUsage>
  /** Sub-agent label from an `agent-name` event; last occurrence wins. */
  agent_name?: string
  /** e.g. `bypassPermissions`; last occurrence wins. */
  permission_mode?: string
  mode?: string
  relocated_cwd?: string
  worktree_name?: string
  worktree_branch?: string
  /** The branch the worktree was created from. */
  original_branch?: string
  /** How many times the conversation was compacted. */
  compaction_count: number
  /** Tokens discarded by compaction across the session. */
  dropped_tokens: number
  /** Pull requests this session was linked to, deduplicated by URL. */
  prs?: ClaudeSessionPR[]
  /**
   * Main-thread cost, priced per assistant message during the scan. Like
   * `usage` it excludes delegated work — that is in `subagent_cost`.
   */
  cost: ClaudeSessionCost
  /** Summed cost of this session's sub-agent transcripts. */
  subagent_cost: ClaudeSessionCost
  /**
   * Models used here that have no known rate. Non-empty means `cost` is a
   * floor, not a total, and the UI must say so.
   */
  unpriced_models?: string[]
  /** Input+output tokens those unpriced models accounted for. */
  unpriced_tokens?: number
}

/** A session's cost broken down by token category, in USD. */
export interface ClaudeSessionCost {
  input_usd: number
  output_usd: number
  cache_read_usd: number
  cache_write_usd: number
  total_usd: number
}

/** A pull request a session produced, from a `pr-link` event. */
/** Cache freshness for the Claude sessions list (#208). */
export interface ClaudeSessionStatus {
  /**
   * The served costs were computed under an older pricing catalog and a
   * re-cost is pending. They are not wrong for the rates they were computed
   * under, so the UI labels them rather than hiding them.
   */
  costs_stale: boolean
  /** A background scan is running right now. */
  scan_in_progress: boolean
  /** RFC3339; empty when the cache has never been scanned. */
  last_scanned_at: string
}

export interface ClaudeSessionPR {
  pr_number: number
  pr_url: string
  pr_repository?: string
  first_seen_at: string
}

/** A sub-agent run delegated from a parent session via the Task/Agent tool. */
export interface ClaudeSubagent {
  agent_id: string
  agent_type?: string
  description?: string
  tool_use_id?: string
  start_time: string
  last_activity: string
  message_count: number
  /** Raw top-level event total — what message_count meant before the turn/event split. */
  event_count: number
  usage: ClaudeTokenUsage
  model?: string
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
  type: 'user' | 'assistant'
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
  subagents: ClaudeSubagent[]
}

// ── Session Journey ──────────────────────────────────────────────────────────

export interface SessionJourney {
  session_id: string
  model?: string
  cwd?: string
  git_branch?: string
  start_time: string
  end_time: string
  /** Raw start-to-end span — includes idle time between sittings of a resumed session. */
  total_duration_ms: number
  /** Inter-event gaps capped at 10 min — the time the session was actually being worked. */
  active_duration_ms: number
  total_turns: number
  /** Main-thread only, like ClaudeSessionSummary.usage. */
  usage: ClaudeTokenUsage
  /** Delegated work — reported separately so the header can label it, not hide it. */
  subagent_usage: ClaudeTokenUsage
  subagent_count: number
  summary?: string
  turns: JourneyTurn[]
}

export interface JourneyTurn {
  number: number
  start_time: string
  end_time: string
  duration_ms: number
  usage?: ClaudeTokenUsage
  tool_calls: number
  steps: JourneyStep[]
}

export interface JourneyStep {
  type: JourneyStepType
  timestamp: string
  duration_ms: number
  data: Record<string, unknown>
  /**
   * Nested steps for a step that spawned a sub-agent (a Task tool_call whose id
   * matches a sub-agent transcript). One level deep; empty for all other steps.
   */
  steps?: JourneyStep[]
}

export type JourneyStepType =
  | 'user_input'
  | 'thinking'
  | 'text_response'
  | 'tool_call'
  | 'tool_result'
  | 'thinking_duration'
  | 'sub_agent'
  | 'compaction'

// ── Notifications ─────────────────────────────────────────────────────────────

export interface SMTPConfig {
  host: string
  port: number
  username: string
  password: string
  from_address: string
  to_addresses: string
  encryption: 'none' | 'starttls' | 'ssl_tls'
}

export interface ScheduledTasksPreferences {
  on_finished?: boolean // undefined/null → default enabled (true)
  on_failed?: boolean // undefined/null → default enabled (true)
}

export interface NotificationPreferences {
  scheduled_tasks?: ScheduledTasksPreferences
}

export interface NotificationSettings {
  enabled: boolean
  provider: SMTPConfig
  preferences?: NotificationPreferences
}

export interface NotificationLogEntry {
  id: number
  event_type: string
  provider: string
  subject: string
  status: 'sent' | 'failed'
  error_msg: string
  created_at: string
}

// ── Scheduled Tasks ──────────────────────────────────────────────────────────

export type ScheduleType = 'run_immediately' | 'one_off' | 'interval' | 'cron'
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
  save_output: boolean
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
  response_text: string
}

// ── Version / update check ────────────────────────────────────────────────────

export interface UpdateCheckResponse {
  current_version: string
  update_available: boolean
  latest_version: string
  release_url: string
}

// ── Model Pricing ─────────────────────────────────────────────────────────────

/** One effective-dated rate row from the pricing catalog. All prices are USD per million tokens. */
export interface PricingRate {
  id: number
  provider: string
  model_pattern: string
  match_type: 'exact' | 'prefix'
  display_name: string
  input_per_mtok: number
  output_per_mtok: number
  cache_write_5m_per_mtok: number
  cache_write_1h_per_mtok: number
  cache_read_per_mtok: number
  /** RFC3339. The rate governs usage from this instant until the next rate starts. */
  effective_from: string
  /** Where the rate came from — provider page, endpoint, and when it was checked. */
  source: string
  /** Shipped with Agento rather than entered by the user. */
  is_builtin: boolean
  /** Edited by the user; a startup re-seed leaves these rows alone. */
  user_modified: boolean
  /** False marks a deliberate zero (synthetic messages, embeddings), not an unfilled row. */
  billable: boolean
  /** A best-effort rate rather than a published one, e.g. the bare family aliases. */
  estimated: boolean
  /**
   * Context-length bands, ascending by `max_input_tokens`. Absent or empty
   * means the rate is flat, which is every Anthropic model — only providers
   * that price by input length (Alibaba) carry bands. The rate's own five
   * price columns are its lowest band.
   */
  tiers?: PricingTier[]
}

/**
 * One context-length band. `max_input_tokens` is an inclusive upper bound on a
 * request's input tokens; the highest band also covers everything above it.
 * All of a request's tokens bill at the selected band — bands are chosen, not
 * accumulated across.
 */
export interface PricingTier {
  max_input_tokens: number
  input_per_mtok: number
  output_per_mtok: number
  cache_write_5m_per_mtok: number
  cache_write_1h_per_mtok: number
  cache_read_per_mtok: number
}

/** A model with the rate in force now and every rate it has ever had. */
export interface PricedModel {
  model_pattern: string
  provider: string
  display_name: string
  match_type: 'exact' | 'prefix'
  /** Null only when every rate for this model is future-dated. */
  current: PricingRate | null
  /** Full history, newest first. */
  rates: PricingRate[]
}

/** The Model Pricing tab's payload. */
export interface PricingCatalog {
  models: PricedModel[]
  /** Model IDs seen in real sessions that match no rate — the tab's to-do list. */
  unpriced_models: string[]
  /** Catalog fingerprint the stored session costs were computed under. */
  revision: number
}

/** Body for creating or correcting a rate. `effective_from` accepts YYYY-MM-DD or RFC3339. */
export interface PricingRateInput {
  provider: string
  model_pattern: string
  match_type: 'exact' | 'prefix'
  display_name: string
  input_per_mtok: number
  output_per_mtok: number
  cache_write_5m_per_mtok: number
  cache_write_1h_per_mtok: number
  cache_read_per_mtok: number
  effective_from: string
  source: string
  billable: boolean
  estimated: boolean
}

// ── Monitoring / OTel ─────────────────────────────────────────────────────────

export interface MonitoringConfig {
  enabled: boolean
  metrics_exporter: 'otlp' | 'prometheus' | 'none'
  logs_exporter: 'otlp' | 'none'
  otlp_endpoint: string
  otlp_headers: Record<string, string>
  otlp_insecure: boolean
  metric_export_interval_ms: number
}

export interface MonitoringResponse {
  settings: MonitoringConfig
  locked: Record<string, string>
  env_locked: boolean
}

export interface MonitoringTestResult {
  ok: boolean
  error?: string
}

// ── Session Insights ─────────────────────────────────────────────────────────

export interface SessionInsight {
  session_id: string
  processor_version: number
  scanned_at: string
  turn_count: number
  steps_per_turn_avg: number
  autonomy_score: number
  tool_calls_total: number
  tool_breakdown: Record<string, number>
  /**
   * Attribution breakdowns. All count tool calls, so
   * `sum(skill_breakdown) + unattributed_calls === tool_calls_total`.
   */
  skill_breakdown: Record<string, number>
  plugin_breakdown: Record<string, number>
  /** Parsed from the `mcp__<server>__<tool>` tool name. */
  mcp_server_breakdown: Record<string, number>
  mcp_tool_breakdown: Record<string, number>
  effort_breakdown: Record<string, number>
  /** Sub-agent type that made the call; empty for main-thread work. */
  agent_breakdown: Record<string, number>
  /** Tool calls made with no skill in context — built-in tool use. */
  unattributed_calls: number
  /** Raw start-to-end span — includes idle time between sittings of a resumed session. */
  total_duration_ms: number
  /** Inter-event gaps capped at 10 min — the time the session was actually being worked. */
  active_duration_ms: number
  /** Subset of active time spent producing assistant output, measured from event timing. */
  claude_working_time_ms: number
  cache_hit_rate: number
  tokens_per_turn_avg: number
  cost_estimate_usd: number
  tool_error_rate: number
  tool_error_count: number
  has_errors: boolean
  max_consecutive_tool_calls: number
  longest_autonomous_chain: number
  avg_user_response_time_ms: number
  avg_claude_response_time_ms: number
  session_type: string
}

export interface ToolUsageStat {
  tool: string
  count: number
}

export interface InsightSummary {
  total_sessions: number
  avg_autonomy_score: number
  avg_turn_count: number
  avg_tool_calls_total: number
  avg_cost_estimate_usd: number
  total_cost_estimate_usd: number
  avg_cache_hit_rate: number
  sessions_with_errors: number
  /** Summed tool errors, the numerator for an errors-per-100-calls rate. */
  total_tool_errors: number
  /** Mean raw span — kept for reference; idle time between sittings inflates it. */
  avg_total_duration_ms: number
  /** Mean active duration (idle gaps over 10 min excluded) — what the dashboard shows. */
  avg_active_duration_ms: number
  top_tools: ToolUsageStat[]
  /** Total tool calls across the period — the denominator for the breakdowns. */
  total_tool_calls: number
  /** Of those, how many were made with no skill in context. */
  unattributed_calls: number
  /** Tool calls grouped by the skill whose instructions were in context. */
  top_skills: ToolUsageStat[]
  /** Tool calls grouped by the plugin that shipped the skill. */
  top_plugins: ToolUsageStat[]
  /** Tool calls grouped by MCP server, parsed from `mcp__<server>__<tool>`. */
  top_mcp_servers: ToolUsageStat[]
  /** The MCP-server breakdown one level deeper — the specific tools called. */
  top_mcp_tools: ToolUsageStat[]
  /** Tool calls grouped by the reasoning-effort tier the turn ran at. */
  top_efforts: ToolUsageStat[]
  /** Tool calls grouped by sub-agent type; empty for main-thread-only work. */
  top_agents: ToolUsageStat[]
}

// ── Analytics ─────────────────────────────────────────────────────────────────

export interface AnalyticsSummary {
  total_sessions: number
  /**
   * Projects the *filtered* sessions belong to. `AnalyticsReport.projects` is
   * built before filtering because it populates the project picker, so its
   * length is the whole corpus's project count regardless of the window.
   */
  unique_projects: number
  total_tokens: number
  total_input_tokens: number
  total_output_tokens: number
  total_cache_read_tokens: number
  total_cache_creation_tokens: number
  most_used_model: string
  avg_tokens_per_session: number
  estimated_cost_usd: number
  /** Tokens on models with no published rates; excluded from estimated_cost_usd. */
  unknown_pricing_tokens: number
  /** Those model identifiers, sorted. */
  unknown_pricing_models: string[]
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

/**
 * One model's share of spend. Unlike ModelStat this is money, attributed to the
 * model that spent it including for delegated work — the two answer different
 * questions and, on a corpus mixing a caching backend with a non-caching one,
 * give nearly opposite pictures.
 */
export interface ModelCostStat {
  model: string
  /** Display grouping derived from the model id: Anthropic, Moonshot, Z.ai, … */
  provider: string
  cost: ClaudeSessionCost
  percentage: number
  /** How many sessions this model spent money in. */
  sessions: number
}

/**
 * One actionable fact about the window, computed from stored data.
 *
 * The backend supplies the numbers and the frontend does the phrasing, so the
 * arithmetic is testable in Go and the copy stays in the UI. Fields not
 * relevant to a `kind` are absent.
 */
export interface InsightCard {
  kind: 'cache_savings' | 'model_low_cache' | 'delegation_mix' | 'expensive_sessions'
  amount_usd?: number
  /** A share, 0–100. */
  percent?: number
  count?: number
  model?: string
  tokens?: number
  avg_duration_ms?: number
  /** What amount_usd should be read against — for savings, the actual bill. */
  comparison_usd?: number
  /** Derived from list rates rather than a stored total — say "about". */
  estimated?: boolean
}

/** One project's activity over the window. */
export interface ProjectStat {
  project: string
  sessions: number
  /** Conversation tokens (input+output). */
  tokens: number
  /** Every token including cache traffic. */
  total_tokens: number
  cost: ClaudeSessionCost
  /** Share of the window's cost. */
  percentage: number
  last_activity: string
}

/** One project's activity on one local day, for the project×day strip. */
export interface ProjectDayActivity {
  project: string
  date: string
  sessions: number
  cost_usd: number
}

/** One leaderboard row; session_id deep-links to the session. */
export interface SessionRanking {
  session_id: string
  title: string
  project: string
  model: string
  cost_usd: number
  duration_ms: number
  tokens: number
  subagent_count: number
  last_activity: string
}

/** The same sessions ranked three ways — they pick out different sessions. */
export interface TopSessions {
  by_cost: SessionRanking[]
  by_duration: SessionRanking[]
  by_tokens: SessionRanking[]
}

/** One time bucket's cost split by model; the values sum to the plain series. */
export interface StackedCostPoint {
  date: string
  cost_by_model: Record<string, number>
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
  cost_by_model: ModelCostStat[]
  insight_cards: InsightCard[]
  cost_over_time_by_model: StackedCostPoint[]
  project_breakdown: ProjectStat[]
  project_activity: ProjectDayActivity[]
  top_sessions: TopSessions
  most_active_days: DayActivity[]
  heatmap: HeatmapCell[]
  hourly_activity: HourlyActivity[]
  cost_over_time: CostPoint[]
  cost_summary: CostSummary
  projects: string[]
}

// ── Inbound Triggers ──────────────────────────────────────────────────────────

export interface TriggerRule {
  id: string
  integration_id: string
  name: string
  agent_slug: string
  enabled: boolean
  filter_prefix: string
  filter_keywords: string[]
  filter_chat_ids: string[]
  created_at: string
  updated_at: string
}

export interface WebhookStatus {
  status: 'active' | 'inactive' | 'error'
  url: string
  has_secret: boolean
  error: string
}
