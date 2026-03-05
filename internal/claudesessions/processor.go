package claudesessions

import (
	"context"
	"encoding/json"
	"time"
)

// CurrentProcessorVersion is bumped whenever any processor logic changes.
// Sessions whose insight row has a lower processor_version are re-scanned automatically.
const CurrentProcessorVersion = 1

// ProcessableEvent is a single decoded line from a Claude Code session JSONL file,
// passed to each SessionProcessor in chronological order.
type ProcessableEvent struct {
	Type        string        `json:"type"`
	Timestamp   time.Time     `json:"timestamp"`
	IsSidechain bool          `json:"isSidechain"`
	Message     *EventMessage `json:"message,omitempty"`
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
}

// InsightStorer persists and retrieves per-session insight records.
type InsightStorer interface {
	Upsert(ctx context.Context, insight *SessionInsight) error
	Get(ctx context.Context, sessionID string) (*SessionInsight, error)
	GetMany(ctx context.Context, sessionIDs []string) ([]*SessionInsight, error)
	// GetSummary returns aggregated statistics across the given sessions.
	// If sessionIDs is empty, all sessions are included. Scalar stats are
	// computed in SQL to avoid loading all rows into memory.
	GetSummary(ctx context.Context, sessionIDs []string) (*InsightAggregateSummary, error)
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

// isTurnStart returns true when ev represents genuine user input — i.e. the
// event is a non-sidechain user message whose content is not a tool_result.
func isTurnStart(ev ProcessableEvent) bool {
	if ev.Type != "user" || ev.IsSidechain || ev.Message == nil {
		return false
	}
	for _, b := range parseContentBlocks(ev.Message.Content) {
		if b.Type == "tool_result" {
			return false
		}
	}
	return true
}
