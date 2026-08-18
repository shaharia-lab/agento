package claudesessions

import (
	"context"
	"encoding/json"
	"strings"
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
// v7: user events whose string content opens with a Claude Code injection
// wrapper no longer start a turn (#197), so turn_count and everything derived
// from it — steps_per_turn_avg, autonomy_score, tokens_per_turn_avg,
// longest_autonomous_chain and the response-time averages — are lower and more
// accurate than every row written before v7.
// v8: cache_hit_rate is the read share of every input-side token (fresh input +
// cache writes + cache reads) rather than of cache traffic alone, so it matches
// the analytics dashboard and a model that never caches now scores 0 instead of
// being excluded from its own denominator. Every row written before v8 holds
// the old, higher figure.
// v9: the two injected user-event classes that arrive as *array* content — the
// skill-invocation preamble and the "[Request interrupted by user]" notice —
// no longer start a turn either (#226). v7 covered only string content, so
// every row written before v9 counts those ~963 machine-written events (on the
// reference corpus) as human turns, inflating turn_count and everything derived
// from it.
// v10: active_duration_ms is computed (inter-event gaps capped at
// IdleGapThreshold — rows written before v10 hold 0 there), thinking_time_ms
// becomes claude_working_time_ms measured from gap attribution rather than
// guessed from character counts, and the response-time averages exclude gaps
// above IdleGapThreshold, so a resume-after-days no longer counts as a
// days-long reply.
const CurrentProcessorVersion = 10

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
	SessionID string `json:"session_id"`
	// ProjectPath is the second half of the row key, matching
	// claude_session_cache. A session id under two project paths is two
	// transcripts and therefore two insights (#362).
	ProjectPath      string    `json:"project_path"`
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

	// TimeProfileProcessor. TotalDurationMs is the raw first-to-last span;
	// ActiveDurationMs caps every inter-event gap at IdleGapThreshold and is
	// what aggregate reporting averages — a resumed session's span contains
	// every idle day between sittings, and one resumed-after-28-days session
	// carried 82% of the dashboard's average on the reference corpus.
	// ClaudeWorkingTimeMs is the subset of active time spent producing
	// assistant output.
	TotalDurationMs     int64 `json:"total_duration_ms"`
	ActiveDurationMs    int64 `json:"active_duration_ms"`
	ClaudeWorkingTimeMs int64 `json:"claude_working_time_ms"`

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
	// AvgActiveDurationMs is the mean of per-session active durations — idle
	// gaps above IdleGapThreshold excluded — and is what the dashboard's Avg
	// Duration card shows. AvgTotalDurationMs (the raw span mean) is kept for
	// callers that genuinely want first-seen-to-last-touched.
	AvgActiveDurationMs float64
	SessionsWithErrors  int
	// TotalToolErrors is the summed error count, so an errors-per-100-calls
	// rate is expressible. SessionsWithErrors counts sessions and cannot be one.
	TotalToolErrors int
	TopToolTotals   map[string]int
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
	// GetSummary returns aggregated statistics over exactly the given sessions.
	// The set is complete, not a hint: an empty set yields a zero summary, not
	// every session. Callers window with FilterSessions and pass the resulting
	// IDs, so this and the analytics report cannot disagree about which sessions
	// a date range contains. Scalar stats are computed in SQL to avoid loading
	// all rows into memory.
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
	// ProjectPath identifies which of a duplicated id's transcripts this is.
	// The insight row is keyed on it, so the worker cannot write without it.
	ProjectPath string
	FilePath    string
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

// injectedTurnMarkers are the wrappers Claude Code writes into the transcript
// as user-role events that nobody typed: slash-command expansions, the output
// of a local command, sub-agent completion notices, and injected reminders.
//
// The list is empirical — it was read off the local corpus, and a new Claude
// Code release can add a form that is not here. That failure mode is benign:
// a missed marker leaves the count where it already is rather than making it
// worse, so re-sampling is periodic maintenance, not a correctness dependency.
var injectedTurnMarkers = []string{
	"<task-notification>",
	"<command-message>",
	"<command-name>",
	"<local-command-caveat>",
	"<local-command-stdout>",
	"<system-reminder>",
}

// injectedArrayTurnMarkers are the injected user events that arrive as *array*
// content — a single text block, no tool_result — rather than as a bare JSON
// string (#226). They are kept in their own table because the two populations
// do not overlap: injectedTurnMarkers are XML-ish wrappers that only ever
// appear as strings, and these only ever appear as array text.
//
// Both entries are required — neither is a prefix of the other, since the
// shorter one closes its bracket where the longer one continues (" for tool
// use]"), so a prefix test on one does not cover the other.
//
// The skill-invocation preamble is the third member of this class and is
// matched by skillPreamblePattern (scanner.go) instead of by a bare prefix, so
// the colon must be followed by a path token. That regexp is shared rather than
// duplicated: it is the same shape fallbackPreviewLabel already recognizes.
//
// Like injectedTurnMarkers this list is empirical — it was read off the local
// corpus (1,598 transcripts: 843 skill preambles, 120 interruption notices,
// among non-sidechain user events that were still counting as turns) — and a
// new Claude Code release can add a form that is not here. That failure mode is
// benign in the same way: a missed marker leaves the count where it already is.
var injectedArrayTurnMarkers = []string{
	"[Request interrupted by user]",
	"[Request interrupted by user for tool use]",
}

// isInjectedUserContent reports whether content is one of Claude Code's own
// injections rather than something a person typed. It handles both shapes the
// harness writes:
//
//   - bare JSON string content, matched against injectedTurnMarkers; and
//   - array content holding exactly one text block, matched against
//     injectedArrayTurnMarkers plus the skill preamble. Any other array shape —
//     several blocks, or a block that is not text — is genuine, because the
//     injected forms are always emitted alone.
//
// The match is anchored to the start, after trimming leading whitespace, and
// that is deliberate: a person can legitimately write about these markers, and
// a substring test would silently stop counting their message as a turn. The
// corpus already contains such a prompt — a genuine human instruction that
// quotes "system-reminder" mid-sentence — so this is a real case, not a
// hypothetical one. Only content that *opens* with a marker is rejected.
func isInjectedUserContent(content json.RawMessage) bool {
	if len(content) == 0 {
		return false
	}
	switch content[0] {
	case '"':
		var s string
		if err := json.Unmarshal(content, &s); err != nil {
			return false
		}
		return hasInjectedPrefix(strings.TrimSpace(s), injectedTurnMarkers)
	case '[':
		blocks := parseContentBlocks(content)
		if len(blocks) != 1 || blocks[0].Type != "text" {
			return false
		}
		text := strings.TrimSpace(blocks[0].Text)
		// skillPreamblePattern is anchored and requires a path token after the
		// colon, so prose that merely opens with the same words stays a turn.
		return hasInjectedPrefix(text, injectedArrayTurnMarkers) ||
			skillPreamblePattern.MatchString(text)
	default:
		return false
	}
}

// hasInjectedPrefix reports whether s opens with any of the given markers. The
// caller trims first; the prefix anchor is the whole point (see above).
func hasInjectedPrefix(s string, markers []string) bool {
	for _, marker := range markers {
		if strings.HasPrefix(s, marker) {
			return true
		}
	}
	return false
}

// isUserTurnContent reports whether a user message's content is genuine human
// input. Two classes of user event are not:
//
//   - carriers for tool_result blocks, which exist only to hand a tool's output
//     back to the model — the overwhelming majority of user events; and
//   - content that opens with one of Claude Code's own injections, whether it
//     arrives as a string (injectedTurnMarkers) or as a lone text block
//     (injectedArrayTurnMarkers and the skill preamble) — the harness wrote it,
//     not the user.
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
	return !isInjectedUserContent(content)
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
