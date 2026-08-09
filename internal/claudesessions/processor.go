package claudesessions

import (
	"context"
	"encoding/json"
	"time"
)

// CurrentProcessorVersion is bumped whenever any processor logic changes.
// Sessions whose insight row has a lower processor_version are re-scanned automatically.
//
// v2: sub-agent transcripts under <session-id>/subagents/ are fed through the
// pipeline alongside the parent, so tool counts, cost and error rates include
// delegated work.
// v3: cost estimates price cache writes by cache TTL and no longer bill unknown
// models at Sonnet rates, so every stored cost_estimate_usd is out of date.
// v4: tool calls are attributed to the skill, plugin and MCP server that made
// them — every row written before v4 has empty attribution breakdowns.
// v5: cost is accumulated per assistant message against the pricing catalog
// (#186) — rows written before v5 were priced whole-session at the first-seen
// model's flat rate, so every stored cost_estimate_usd is out of date.
// v6: tool calls are also attributed to the sub-agent that made them (#202) —
// every row written before v6 has an empty agent_breakdown.
const CurrentProcessorVersion = 6

// ProcessableEvent is a single decoded line from a Claude Code session JSONL file,
// passed to each SessionProcessor in chronological order.
type ProcessableEvent struct {
	Type        string        `json:"type"`
	Timestamp   time.Time     `json:"timestamp"`
	IsSidechain bool          `json:"isSidechain"`
	Message     *EventMessage `json:"message,omitempty"`

	// Attribution fields are stamped by Claude Code at the top level of
	// assistant events — never inside message, and never on user events. They
	// describe which skill's instructions were in context when the turn ran, so
	// on a Skill tool call they name the *caller*, not the skill being invoked.
	//
	// Claude Code emits one event per content block, all sharing a message id
	// and therefore identical attribution, so counting these per event
	// over-counts. See AttributionProcessor, which counts per tool_use block.
	AttributionSkill  string `json:"attributionSkill,omitempty"`
	AttributionPlugin string `json:"attributionPlugin,omitempty"`
	AttributionAgent  string `json:"attributionAgent,omitempty"`
	// AttributionMcpServer and AttributionMcpTool are recorded but deliberately
	// NOT counted: they hold the last MCP tool touched and persist onto later,
	// unrelated turns. MCP attribution is parsed from the tool_use block name
	// (mcp__<server>__<tool>) instead, which is authoritative.
	AttributionMcpServer string `json:"attributionMcpServer,omitempty"`
	AttributionMcpTool   string `json:"attributionMcpTool,omitempty"`
	// Effort is the reasoning-effort tier the turn ran at.
	Effort string `json:"effort,omitempty"`

	// Raw holds the original JSON bytes so processors can extract fields not
	// present in the decoded struct (e.g. system event subtypes).
	Raw json.RawMessage `json:"-"`
}

// EventMessage is the decoded message payload of a user or assistant event.
type EventMessage struct {
	Role    string          `json:"role"`
	Model   string          `json:"model,omitempty"`
	Content json.RawMessage `json:"content"`
	Usage   *EventUsage     `json:"usage,omitempty"`
}

// EventUsage holds token usage counters attached to an assistant message.
type EventUsage struct {
	InputTokens              int `json:"input_tokens"`
	OutputTokens             int `json:"output_tokens"`
	CacheCreationInputTokens int `json:"cache_creation_input_tokens"`
	CacheReadInputTokens     int `json:"cache_read_input_tokens"`
	// CacheCreation splits the cache-creation total by cache TTL. Absent on
	// transcripts written before Claude Code emitted it — see EventCacheCreation.
	CacheCreation *EventCacheCreation `json:"cache_creation,omitempty"`
}

// EventCacheCreation is the nested cache-TTL split of CacheCreationInputTokens.
// The two tiers bill at different multiples of the input rate (1.25× and 2×).
type EventCacheCreation struct {
	Ephemeral5mInputTokens int `json:"ephemeral_5m_input_tokens"`
	Ephemeral1hInputTokens int `json:"ephemeral_1h_input_tokens"`
}

// Split attributes the cache-creation total across the 5m and 1h buckets. It
// shares splitCacheTiers with the scanner so the two decoders cannot drift.
func (u *EventUsage) Split() (fiveMin, oneHour int) {
	if u.CacheCreation == nil {
		return splitCacheTiers(u.CacheCreationInputTokens, 0)
	}
	return splitCacheTiers(u.CacheCreationInputTokens, u.CacheCreation.Ephemeral1hInputTokens)
}

// SessionInsight holds all computed static-analysis metrics for a single
// Claude Code session JSONL file.
type SessionInsight struct {
	SessionID        string    `json:"session_id"`
	ProcessorVersion int       `json:"processor_version"`
	ScannedAt        time.Time `json:"scanned_at"`

	// TurnCountProcessor
	TurnCount       int     `json:"turn_count"`
	StepsPerTurnAvg float64 `json:"steps_per_turn_avg"`

	// AutonomyScoreProcessor
	AutonomyScore float64 `json:"autonomy_score"`

	// ToolUsageProcessor
	ToolCallsTotal int            `json:"tool_calls_total"`
	ToolBreakdown  map[string]int `json:"tool_breakdown"`

	// AttributionProcessor. Every breakdown counts tool_use blocks, so they
	// share ToolCallsTotal as their denominator:
	// sum(SkillBreakdown) + UnattributedCalls == ToolCallsTotal.
	SkillBreakdown     map[string]int `json:"skill_breakdown"`
	PluginBreakdown    map[string]int `json:"plugin_breakdown"`
	McpServerBreakdown map[string]int `json:"mcp_server_breakdown"`
	McpToolBreakdown   map[string]int `json:"mcp_tool_breakdown"`
	EffortBreakdown    map[string]int `json:"effort_breakdown"`
	// AgentBreakdown counts calls by the sub-agent type that made them, which
	// is what makes delegation visible now that sub-agent transcripts run
	// through the same registry.
	AgentBreakdown map[string]int `json:"agent_breakdown"`
	// UnattributedCalls is tool calls made with no skill in context — built-in
	// tool use. Kept out of SkillBreakdown so the sum above reconciles.
	UnattributedCalls int `json:"unattributed_calls"`

	// TimeProfileProcessor
	TotalDurationMs int64 `json:"total_duration_ms"`
	ThinkingTimeMs  int64 `json:"thinking_time_ms"`

	// TokenProfileProcessor
	CacheHitRate     float64 `json:"cache_hit_rate"`
	TokensPerTurnAvg float64 `json:"tokens_per_turn_avg"`
	CostEstimateUSD  float64 `json:"cost_estimate_usd"`

	// ErrorRateProcessor
	ToolErrorRate  float64 `json:"tool_error_rate"`
	ToolErrorCount int     `json:"tool_error_count"`
	HasErrors      bool    `json:"has_errors"`

	// ConversationDepthProcessor
	MaxConsecutiveToolCalls int `json:"max_consecutive_tool_calls"`
	LongestAutonomousChain  int `json:"longest_autonomous_chain"`

	// SessionRhythmProcessor
	AvgUserResponseTimeMs   float64 `json:"avg_user_response_time_ms"`
	AvgClaudeResponseTimeMs float64 `json:"avg_claude_response_time_ms"`

	// Reserved for future AI-based classifier (Issue #101).
	SessionType string `json:"session_type"`
}

// SessionProcessor is implemented by each static-analysis pass over a session.
// Processors maintain internal state across Process calls, written to a
// SessionInsight only when Finalize is called.
type SessionProcessor interface {
	// Name returns the unique identifier for this processor.
	Name() string
	// Process handles a single event in chronological order.
	Process(ev ProcessableEvent)
	// Finalize writes accumulated metrics into the provided SessionInsight.
	// It is called after all events have been processed.
	Finalize(insight *SessionInsight)
	// Reset clears all internal state so the processor can be reused for a new session.
	Reset()
}

// InsightAggregateSummary holds SQL-computed aggregate statistics across sessions.
// Scalar fields are computed via SQL aggregation; TopToolTotals is the merged
// tool_breakdown across all included sessions.
type InsightAggregateSummary struct {
	TotalSessions        int
	AvgAutonomyScore     float64
	AvgTurnCount         float64
	AvgToolCallsTotal    float64
	TotalCostEstimateUSD float64
	AvgCacheHitRate      float64
	AvgTotalDurationMs   float64
	SessionsWithErrors   int
	TopToolTotals        map[string]int
	// Attribution totals, merged across sessions the same way TopToolTotals is.
	// All of them count tool calls, so they are directly comparable with it.
	TopSkillTotals     map[string]int
	TopPluginTotals    map[string]int
	TopMcpServerTotals map[string]int
	// TopMcpToolTotals breaks TopMcpServerTotals down one level further, so a
	// busy server can be read as the specific tools driving it.
	TopMcpToolTotals map[string]int
	// TopEffortTotals is the reasoning-effort tier mix; TopAgentTotals is the
	// delegation mix by sub-agent type, empty for main-thread-only work.
	TopEffortTotals map[string]int
	TopAgentTotals  map[string]int
	// TotalToolCalls and UnattributedCalls are the denominator for those
	// totals: roughly half of all tool calls are made with no skill in
	// context, and a breakdown without that share is misleading.
	TotalToolCalls    int
	UnattributedCalls int
}

// InsightStorer persists and retrieves per-session insight records.
type InsightStorer interface {
	Upsert(ctx context.Context, insight *SessionInsight) error
	Get(ctx context.Context, sessionID string) (*SessionInsight, error)
	GetMany(ctx context.Context, sessionIDs []string) ([]*SessionInsight, error)
	// GetSummary returns aggregated statistics across the given sessions,
	// optionally filtered to sessions whose start_time falls within [from, to].
	// If sessionIDs is empty, all sessions are included. Scalar stats are
	// computed in SQL to avoid loading all rows into memory.
	// from and to are inclusive date boundaries; nil means unbounded.
	GetSummary(ctx context.Context, sessionIDs []string, from, to *time.Time) (*InsightAggregateSummary, error)
	// NeedsProcessing returns sessions present in the scanner cache that have
	// no insight row or whose insight has processor_version < version.
	// The FilePath is included so callers avoid a separate filesystem walk.
	NeedsProcessing(ctx context.Context, version int) ([]SessionToProcess, error)
}

// SessionToProcess pairs a session ID with its JSONL file path.
// Returned by InsightStorer.NeedsProcessing so callers do not need a separate
// filesystem walk to locate the JSONL file.
type SessionToProcess struct {
	SessionID string
	FilePath  string
}

// contentBlock is the decoded form of a single block within a message's content array.
type contentBlock struct {
	Type      string          `json:"type"`
	Text      string          `json:"text,omitempty"`
	Thinking  string          `json:"thinking,omitempty"`
	ID        string          `json:"id,omitempty"`
	Name      string          `json:"name,omitempty"`
	Input     json.RawMessage `json:"input,omitempty"`
	ToolUseID string          `json:"tool_use_id,omitempty"`
	IsError   bool            `json:"is_error,omitempty"`
}

// parseContentBlocks decodes the content field of an EventMessage into a slice
// of contentBlock values. Returns nil for string content or decode errors.
func parseContentBlocks(raw json.RawMessage) []contentBlock {
	if len(raw) == 0 || raw[0] != '[' {
		return nil
	}
	var blocks []contentBlock
	if err := json.Unmarshal(raw, &blocks); err != nil {
		return nil
	}
	return blocks
}

// isUserTurnContent reports whether a user message's content is genuine human
// input rather than a carrier for tool_result blocks. The overwhelming majority
// of user events in a transcript are the latter — they exist only to hand a
// tool's output back to the model, and nobody typed them.
//
// The isSidechain check is deliberately left to the caller: the flag means
// "delegated work, already counted elsewhere" in a parent transcript but is set
// on every event of a sub-agent transcript, where it carries no such meaning.
func isUserTurnContent(content json.RawMessage) bool {
	for _, b := range parseContentBlocks(content) {
		if b.Type == "tool_result" {
			return false
		}
	}
	return true
}

// isAssistantReply reports whether an assistant message contains something the
// user actually saw — at least one text block. Turns that only issue tool calls
// are round-trips, not messages.
func isAssistantReply(content json.RawMessage) bool {
	blocks := parseContentBlocks(content)
	if blocks == nil {
		// No decodable blocks: absent, null, or non-array content. Only a
		// non-empty bare JSON string carries text the user saw.
		return len(content) > 2 && content[0] == '"'
	}
	for _, b := range blocks {
		if b.Type == "text" {
			return true
		}
	}
	return false
}

// isTurnStart returns true when ev represents genuine user input — i.e. the
// event is a non-sidechain user message whose content is not a tool_result.
func isTurnStart(ev ProcessableEvent) bool {
	if ev.Type != "user" || ev.IsSidechain || ev.Message == nil {
		return false
	}
	return isUserTurnContent(ev.Message.Content)
}
