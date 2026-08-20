/* ============================================================================
   Wire types — mirror the Go JSON exactly.

   Field names here are the `json:"..."` tags on the Go structs, not idiomatic
   TypeScript. Renaming them at the boundary would only hide drift, so the
   snake_case stays and the UI reads it directly.
   ========================================================================== */

/* --- Agents (internal/config/agent.go) ----------------------------------- */

export interface MCPCap {
  tools: string[];
}

export interface AgentCapabilities {
  built_in: string[] | null;
  local: string[] | null;
  mcp: Record<string, MCPCap> | null;
}

export interface Agent {
  name: string;
  slug: string;
  description: string;
  model: string;
  /** "adaptive" | "disabled" | "enabled" */
  thinking: string;
  /** "bypass" | "default" | "plan" | "dontAsk" */
  permission_mode: string;
  system_prompt: string;
  capabilities: AgentCapabilities;
  claude_config_dir: string;
}

/* --- Chats (internal/storage/chat_store.go) ------------------------------ */

export interface MessageBlock {
  /** "thinking" | "text" | "tool_use" */
  type: string;
  text?: string;
  id?: string;
  name?: string;
  input?: unknown;
}

export interface ChatMessage {
  role: "user" | "assistant";
  content: string;
  timestamp: string;
  blocks?: MessageBlock[];
}

export interface ChatSession {
  id: string;
  title: string;
  agent_slug: string;
  sdk_session_id: string;
  working_directory: string;
  model: string;
  settings_profile_id?: string;
  /**
   * This conversation's own permission mode — "bypass" | "default" | "plan" |
   * "dontAsk". Optional because Go marks it `omitempty`, and absent is not a
   * fifth mode: it means no choice was recorded, so the run falls back to the
   * agent's own mode (and to "default" for a chat with no agent).
   */
  permission_mode?: string;
  created_at: string;
  updated_at: string;
  total_input_tokens?: number;
  total_output_tokens?: number;
  total_cache_creation_tokens?: number;
  total_cache_read_tokens?: number;
  is_favorite?: boolean;
}

/**
 * What `GET /chats/{id}` actually puts on the wire — the session is nested, not
 * flattened. `ChatsView` normalises it to `ChatDetail` before anything else
 * sees it.
 */
export interface ChatDetailResponse {
  session: ChatSession;
  messages: ChatMessage[];
}

/** The flattened shape the UI works with. */
export interface ChatDetail extends ChatSession {
  messages: ChatMessage[];
}

/* --- Integrations (internal/config/integration.go) ----------------------- */

export interface ServiceConfig {
  enabled: boolean;
  tools: string[];
}

/** The scrubbed shape the API returns — credentials are never sent back. */
export interface Integration {
  id: string;
  name: string;
  type: string;
  enabled: boolean;
  authenticated: boolean;
  services: Record<string, ServiceConfig>;
  created_at: string;
  updated_at: string;
}

export interface AvailableTool {
  integration_id: string;
  integration_name: string;
  tool_name: string;
  qualified_name: string;
  service: string;
}

export interface TriggerRule {
  id: string;
  integration_id: string;
  name: string;
  agent_slug: string;
  enabled: boolean;
  filter_prefix: string;
  filter_keywords: string[] | null;
  filter_chat_ids: string[] | null;
  created_at: string;
  updated_at: string;
}

export interface WebhookStatus {
  status: string;
  url: string;
  has_secret: boolean;
  error: string;
}

/* --- Scheduled tasks (internal/storage/task_store.go) -------------------- */

export type ScheduleType =
  | "run_immediately"
  | "one_off"
  | "interval"
  | "cron";

export type TaskStatus = "active" | "paused";
export type JobStatus = "running" | "success" | "failed";

export interface ScheduleConfig {
  run_at?: string;
  every_minutes?: number;
  every_hours?: number;
  every_days?: number;
  /** HH:MM */
  at_time?: string;
  expression?: string;
}

export interface ScheduledTask {
  id: string;
  name: string;
  description: string;
  prompt: string;
  agent_slug: string;
  working_directory: string;
  model: string;
  settings_profile_id: string;
  timeout_minutes: number;
  schedule_type: ScheduleType;
  schedule_config: ScheduleConfig;
  stop_after_count: number;
  stop_after_time?: string | null;
  save_output: boolean;
  status: TaskStatus;
  run_count: number;
  last_run_at?: string | null;
  last_run_status: string;
  next_run_at?: string | null;
  created_at: string;
  updated_at: string;
}

export interface JobHistory {
  id: string;
  task_id: string;
  task_name: string;
  agent_slug: string;
  status: JobStatus;
  started_at: string;
  finished_at?: string | null;
  duration_ms: number;
  chat_session_id: string;
  model: string;
  prompt_preview: string;
  error_message: string;
  total_input_tokens: number;
  total_output_tokens: number;
  total_cache_creation_tokens: number;
  total_cache_read_tokens: number;
  response_text: string;
}

/* --- Settings (internal/config/settings.go) ------------------------------ */

export interface UserSettings {
  default_working_dir: string;
  default_model: string;
  onboarding_complete: boolean;
  appearance_dark_mode: boolean;
  appearance_font_size: number;
  appearance_font_family: string;
  notification_settings: string;
  event_bus_worker_pool_size: number;
  public_url: string;
  hidden_projects: string[] | null;
  idle_gap_threshold_minutes: number;
  claude_config_dir: string;
  claude_config_dirs: string[] | null;
}

/**
 * GET /settings does not return bare settings — it wraps them alongside which
 * fields the environment has pinned. `locked` maps a field name to the *name of
 * the environment variable* that pinned it, so the UI can say which one to
 * unset. A locked field must be read-only: a PUT that changes it is rejected.
 */
export interface SettingsResponse {
  settings: UserSettings;
  locked: Record<string, string>;
  model_from_env: boolean;
}

/* --- Claude sessions (internal/claudesessions/types.go) ------------------ */

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
  cache_creation_tokens: number;
  cache_creation_5m_tokens: number;
  cache_creation_1h_tokens: number;
  cache_read_tokens: number;
}

export interface SessionCost {
  input_usd: number;
  output_usd: number;
  cache_read_usd: number;
  cache_write_usd: number;
  total_usd: number;
}

export interface ClaudeSessionPR {
  pr_number: number;
  pr_url: string;
  pr_repository: string;
  first_seen_at: string;
}

export interface ClaudeSessionSummary {
  session_id: string;
  project_path: string;
  config_dir?: string;
  preview: string;
  custom_title?: string;
  is_favorite?: boolean;
  native_title?: string;
  ai_title?: string;
  display_title: string;
  start_time: string;
  last_activity: string;
  active_duration_ms: number;
  subagent_active_duration_ms: number;
  message_count: number;
  event_count: number;
  usage: TokenUsage;
  git_branch?: string;
  model?: string;
  cwd?: string;
  subagent_count: number;
  subagent_usage: TokenUsage;
  subagent_usage_by_model?: Record<string, TokenUsage>;
  agent_name?: string;
  permission_mode?: string;
  mode?: string;
  relocated_cwd?: string;
  worktree_name?: string;
  worktree_branch?: string;
  original_branch?: string;
  compaction_count: number;
  dropped_tokens: number;
  prs?: ClaudeSessionPR[];
  cost: SessionCost;
  subagent_cost: SessionCost;
  cost_by_model?: Record<string, SessionCost>;
  subagent_cost_by_model?: Record<string, SessionCost>;
  unpriced_models?: string[];
  unpriced_tokens?: number;
}

/** One content block of an assistant turn, normalized by the scanner. */
export interface NormalizedBlock {
  type: string; // "thinking" | "text" | "tool_use"
  text?: string;
  id?: string;
  name?: string;
  input?: unknown;
}

/** One conversation turn from a session transcript. */
export interface ClaudeMessage {
  uuid: string;
  parent_uuid?: string;
  type: string; // "user" | "assistant"
  timestamp: string;
  role?: string;
  content?: string;
  blocks?: NormalizedBlock[] | null;
  usage?: TokenUsage | null;
  git_branch?: string;
  is_sidechain?: boolean;
  children?: ClaudeMessage[] | null;
}

export interface ClaudeTodo {
  content: string;
  status: string; // "completed" | "in_progress" | "pending"
  active_form?: string;
}

export interface ClaudeSubagent {
  agent_id: string;
  agent_type?: string;
  description?: string;
  tool_use_id?: string;
  start_time: string;
  last_activity: string;
  message_count: number;
  event_count: number;
  usage: TokenUsage;
  model?: string;
}

/** GET /api/claude-sessions/{id} — the summary plus the full transcript. */
export interface ClaudeSessionDetail extends ClaudeSessionSummary {
  messages: ClaudeMessage[] | null;
  todos: ClaudeTodo[] | null;
  subagents: ClaudeSubagent[] | null;
}

export interface SessionPage {
  items: ClaudeSessionSummary[];
  next_cursor: string;
  has_more: boolean;
}

export interface SessionFacets {
  total: number;
  total_tokens: number;
  total_cost_usd: number;
  token_p90: number;
  /** Global option sets, deliberately not narrowed by the active filter. */
  models: string[] | null;
  permission_modes: string[] | null;
  /** Omitted from the payload entirely when there is only one config dir. */
  config_dirs?: string[] | null;
  has_favorites: boolean;
  has_prs: boolean;
}

export interface ClaudeProject {
  encoded_name: string;
  decoded_path: string;
  session_count: number;
  hidden: boolean;
}

export interface SessionScanStatus {
  costs_stale: boolean;
  scan_in_progress: boolean;
  files_done: number;
  files_total: number;
  last_scanned_at: string;
}

/* --- Analytics (internal/claudesessions/analytics.go) -------------------- */

export interface AnalyticsSummary {
  total_sessions: number;
  unique_projects: number;
  total_tokens: number;
  total_input_tokens: number;
  total_output_tokens: number;
  total_cache_read_tokens: number;
  total_cache_creation_tokens: number;
  most_used_model: string;
  avg_tokens_per_session: number;
  estimated_cost_usd: number;
  unknown_pricing_tokens: number;
  unknown_pricing_models: string[] | null;
}

export interface TimeSeriesPoint {
  date: string;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  total_tokens: number;
  sessions: number;
}

export interface CacheEfficiencyPoint {
  date: string;
  cache_hit_rate: number;
  cached_tokens: number;
  total_input_tokens: number;
}

export interface ModelBreakdown {
  model: string;
  tokens: number;
  percentage: number;
}

export interface CostByModel {
  model: string;
  provider: string;
  cost: SessionCost;
  percentage: number;
  sessions: number;
}

export interface ProjectBreakdown {
  project: string;
  sessions: number;
  tokens: number;
  total_tokens: number;
  cost: SessionCost;
  percentage: number;
  last_activity: string;
  folded_projects: number;
}

export interface SessionRanking {
  session_id: string;
  title: string;
  project: string;
  model: string;
  cost_usd: number;
  duration_ms: number;
  tokens: number;
  subagent_count: number;
  last_activity: string;
}

export interface CostSummary {
  input_cost_usd: number;
  output_cost_usd: number;
  cache_read_cost_usd: number;
  cache_write_cost_usd: number;
  total_cost_usd: number;
}

export interface InsightCard {
  kind:
    | "cache_savings"
    | "model_low_cache"
    | "delegation_mix"
    | "expensive_sessions";
  amount_usd: number;
  percent: number;
  count: number;
  model: string;
  tokens: number;
  avg_duration_ms: number;
  comparison_usd: number;
  estimated: boolean;
}

export interface AnalyticsReport {
  summary: AnalyticsSummary;
  time_series: TimeSeriesPoint[] | null;
  cache_efficiency: CacheEfficiencyPoint[] | null;
  model_breakdown: ModelBreakdown[] | null;
  sessions_per_model: { model: string; sessions: number }[] | null;
  cost_by_model: CostByModel[] | null;
  insight_cards: InsightCard[] | null;
  project_breakdown: ProjectBreakdown[] | null;
  project_activity:
    | { project: string; date: string; sessions: number; cost_usd: number }[]
    | null;
  top_sessions: {
    by_cost: SessionRanking[] | null;
    by_duration: SessionRanking[] | null;
    by_tokens: SessionRanking[] | null;
  };
  cost_over_time_by_model:
    | { date: string; cost_by_model: Record<string, number> }[]
    | null;
  most_active_days: { date: string; sessions: number; tokens: number }[] | null;
  heatmap:
    | { day_of_week: number; hour: number; sessions: number; tokens: number }[]
    | null;
  hourly_activity: { hour: number; sessions: number; tokens: number }[] | null;
  cost_over_time: { date: string; estimated_cost_usd: number }[] | null;
  cost_summary: CostSummary;
  projects: string[] | null;
  granularity: "hourly" | "daily" | "weekly" | "monthly" | "yearly";
}

/* --- Session insights ---------------------------------------------------- */

export interface SessionInsight {
  session_id: string;
  processor_version: number;
  scanned_at: string;
  turn_count: number;
  steps_per_turn_avg: number;
  autonomy_score: number;
  tool_calls_total: number;
  tool_breakdown: Record<string, number> | null;
  skill_breakdown: Record<string, number> | null;
  plugin_breakdown: Record<string, number> | null;
  mcp_server_breakdown: Record<string, number> | null;
  mcp_tool_breakdown: Record<string, number> | null;
  effort_breakdown: Record<string, number> | null;
  agent_breakdown: Record<string, number> | null;
  unattributed_calls: number;
  total_duration_ms: number;
  active_duration_ms: number;
  claude_working_time_ms: number;
  cache_hit_rate: number;
  tokens_per_turn_avg: number;
  cost_estimate_usd: number;
  tool_error_rate: number;
  tool_error_count: number;
  has_errors: boolean;
  max_consecutive_tool_calls: number;
  longest_autonomous_chain: number;
  avg_user_response_time_ms: number;
  avg_claude_response_time_ms: number;
  session_type: string;
}

/**
 * Every ranked list on the insights endpoint keys its label `tool`, whatever
 * the list is actually of (skills, plugins, agents…). `name` is accepted only
 * defensively.
 */
export interface TopEntry {
  tool?: string;
  name?: string;
  count: number;
}

export interface InsightsSummary {
  total_sessions: number;
  avg_autonomy_score: number;
  avg_turn_count: number;
  avg_tool_calls_total: number;
  avg_cost_estimate_usd: number;
  total_cost_estimate_usd: number;
  avg_cache_hit_rate: number;
  sessions_with_errors: number;
  total_tool_errors: number;
  avg_total_duration_ms: number;
  avg_active_duration_ms: number;
  top_tools: TopEntry[] | null;
  total_tool_calls: number;
  unattributed_calls: number;
  top_skills: TopEntry[] | null;
  top_plugins: TopEntry[] | null;
  top_mcp_servers: TopEntry[] | null;
  top_mcp_tools: TopEntry[] | null;
  top_efforts: TopEntry[] | null;
  top_agents: TopEntry[] | null;
}

/* --- Pricing (internal/pricing/types.go) --------------------------------- */

export interface TierRate {
  max_input_tokens: number;
  input_per_mtok: number;
  output_per_mtok: number;
  cache_write_5m_per_mtok: number;
  cache_write_1h_per_mtok: number;
  cache_read_per_mtok: number;
}

export interface PricingRate {
  id: number;
  provider: string;
  model_pattern: string;
  match_type: "exact" | "prefix";
  display_name: string;
  input_per_mtok: number;
  output_per_mtok: number;
  cache_write_5m_per_mtok: number;
  cache_write_1h_per_mtok: number;
  cache_read_per_mtok: number;
  effective_from: string;
  source: string;
  is_builtin: boolean;
  user_modified: boolean;
  billable: boolean;
  estimated: boolean;
  tiers?: TierRate[];
}

export interface PricingCatalog {
  models: {
    model_pattern: string;
    provider: string;
    display_name: string;
    match_type: "exact" | "prefix";
    current: PricingRate | null;
    rates: PricingRate[];
  }[];
  unpriced_models: string[] | null;
  revision: number;
}

/* --- Notifications ------------------------------------------------------- */

export interface NotificationLogEntry {
  id: number;
  event_type: string;
  provider: string;
  subject: string;
  status: string;
  error_msg: string;
  created_at: string;
}

/* --- Misc ---------------------------------------------------------------- */

export interface VersionInfo {
  version: string;
  commit?: string;
  build_date?: string;
}

export interface FSEntry {
  name: string;
  path: string;
  is_dir: boolean;
}

export interface ClaudeSettingsProfile {
  id: string;
  name: string;
  file_path: string;
  is_default: boolean;
}
