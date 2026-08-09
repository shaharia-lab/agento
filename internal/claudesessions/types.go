package claudesessions

import (
	"encoding/json"
	"sort"
	"time"

	"github.com/shaharia-lab/agento/internal/pricing"
)

// TokenUsage represents API token consumption for a session or message turn.
type TokenUsage struct {
	InputTokens  int `json:"input_tokens"`
	OutputTokens int `json:"output_tokens"`
	// CacheCreationTokens is the authoritative total of cache writes. The 5m/1h
	// fields below split it by cache TTL, which bill at different multiples of
	// the input rate (1.25× and 2×); they always sum to this value.
	CacheCreationTokens   int `json:"cache_creation_tokens"`
	CacheCreation5mTokens int `json:"cache_creation_5m_tokens"`
	CacheCreation1hTokens int `json:"cache_creation_1h_tokens"`
	CacheReadTokens       int `json:"cache_read_tokens"`
}

// eventUsage converts a usage record to the pricing package's per-message
// shape (same fields, no cache-total — pricing works off the TTL split).
func (u TokenUsage) eventUsage() pricing.Usage {
	return pricing.Usage{
		InputTokens:           u.InputTokens,
		OutputTokens:          u.OutputTokens,
		CacheCreation5mTokens: u.CacheCreation5mTokens,
		CacheCreation1hTokens: u.CacheCreation1hTokens,
		CacheReadTokens:       u.CacheReadTokens,
	}
}

// costAccumulator prices a transcript message-by-message. Each assistant
// message is resolved against the catalog at its own timestamp with its own
// model, which is what makes a session that spans a price change — or mixes
// models — cost correctly. Messages on models with no known rate are tracked
// by count so the reported total can state what it left out; cost stays at
// zero when nothing was priced (indistinguishable from "no known rate", which
// is exactly the semantic #180 established).
type costAccumulator struct {
	resolver       *pricing.Resolver
	cost           pricing.Cost
	pricedMessages int
	unknownModels  map[string]int
}

func newCostAccumulator(resolver *pricing.Resolver) *costAccumulator {
	return &costAccumulator{resolver: resolver}
}

// addAssistantMessage prices one assistant message. When resolver is nil the
// accumulator is inert, so test fixtures need no pricing setup.
func (a *costAccumulator) addAssistantMessage(model string, u TokenUsage, at time.Time) {
	if a.resolver == nil || u.InputTokens+u.OutputTokens+u.CacheCreationTokens+u.CacheReadTokens == 0 {
		return
	}
	// The synthetic placeholder and embedding models resolve to non-billable
	// catalog rows, so they price at $0.00 without being mistaken for a gap in
	// the catalog — no special case is needed here for them.
	res, ok := a.resolver.Resolve(model, at)
	if !ok {
		if model != "" {
			if a.unknownModels == nil {
				a.unknownModels = map[string]int{}
			}
			a.unknownModels[model] += u.InputTokens + u.OutputTokens
		}
		return
	}
	a.cost.Add(res.Rate.Price(u.eventUsage()))
	a.pricedMessages++
}

// UnknownPricingTokens returns the input+output tokens seen on models with no
// known rate, so an aggregate can state what its cost total left out.
func (a *costAccumulator) UnknownPricingTokens() int {
	total := 0
	for _, n := range a.unknownModels {
		total += n
	}
	return total
}

// UnpricedModels returns the distinct models this session used that carry no
// known rate, sorted so the persisted list is deterministic and a re-scan that
// changes nothing does not rewrite the row.
func (a *costAccumulator) UnpricedModels() []string {
	if a == nil || len(a.unknownModels) == 0 {
		return nil
	}
	out := make([]string, 0, len(a.unknownModels))
	for m := range a.unknownModels {
		out = append(out, m)
	}
	sort.Strings(out)
	return out
}

// SessionCost is a session's cost broken down by token category. It is stored
// on the session cache rather than derived per request, so the list, the detail
// page and the analytics totals cannot disagree about what a session cost.
//
// The breakdown is accumulated per assistant message at that message's own
// model and timestamp, which is what lets a session that mixes models — or
// spans a rate change — cost correctly.
type SessionCost struct {
	InputUSD      float64 `json:"input_usd"`
	OutputUSD     float64 `json:"output_usd"`
	CacheReadUSD  float64 `json:"cache_read_usd"`
	CacheWriteUSD float64 `json:"cache_write_usd"`
	TotalUSD      float64 `json:"total_usd"`
}

// sessionCostFromPricing converts the accumulator's running total. A nil
// accumulator (a transcript that yielded no timestamps, or a process with no
// pricing wired) yields a zero cost rather than a panic.
func sessionCostFromPricing(a *costAccumulator) SessionCost {
	if a == nil {
		return SessionCost{}
	}
	return SessionCost{
		InputUSD:      a.cost.InputCostUSD,
		OutputUSD:     a.cost.OutputCostUSD,
		CacheReadUSD:  a.cost.CacheReadCostUSD,
		CacheWriteUSD: a.cost.CacheWriteCostUSD,
		TotalUSD:      a.cost.TotalCostUSD,
	}
}

// Add accumulates another breakdown into c.
func (c *SessionCost) Add(o SessionCost) {
	c.InputUSD += o.InputUSD
	c.OutputUSD += o.OutputUSD
	c.CacheReadUSD += o.CacheReadUSD
	c.CacheWriteUSD += o.CacheWriteUSD
	c.TotalUSD += o.TotalUSD
}

// ClaudeProject represents a project directory containing Claude Code sessions.
type ClaudeProject struct {
	EncodedName  string `json:"encoded_name"`
	DecodedPath  string `json:"decoded_path"`
	SessionCount int    `json:"session_count"`
}

// ClaudeSessionSummary contains lightweight metadata for list views.
type ClaudeSessionSummary struct {
	SessionID   string `json:"session_id"`
	ProjectPath string `json:"project_path"`
	Preview     string `json:"preview"` // first user message text, truncated
	// previewIsFallback marks a Preview taken from an injected wrapper because
	// no genuine prompt had been seen yet. Scan-local only (never stored or
	// serialized): it lets a later real prompt replace the placeholder.
	previewIsFallback bool
	CustomTitle       string `json:"custom_title,omitempty"` // user-defined label, preserved across rescans
	IsFavorite        bool   `json:"is_favorite,omitempty"`  // user-starred, preserved across rescans
	// NativeTitle and AITitle come from Claude Code's own `custom-title` and
	// `ai-title` transcript events. Unlike CustomTitle they are refreshed on
	// every rescan, so an Agento rename and a native rename never fight.
	NativeTitle string `json:"native_title,omitempty"`
	AITitle     string `json:"ai_title,omitempty"`
	// DisplayTitle is the resolved label the UI should render — see
	// ResolveDisplayTitle for the precedence.
	DisplayTitle string    `json:"display_title"`
	StartTime    time.Time `json:"start_time"`
	LastActivity time.Time `json:"last_activity"`
	// MessageCount counts conversational turns, not JSONL events: user events
	// that carry genuine human input (not tool_result carriers) plus assistant
	// events containing at least one text block. It is the number a person
	// would arrive at reading the transcript. For raw event volume see
	// EventCount.
	MessageCount int `json:"message_count"`
	// EventCount is the raw number of top-level user and assistant events —
	// effectively API round-trips. This is what MessageCount used to hold.
	EventCount int        `json:"event_count"`
	Usage      TokenUsage `json:"usage"` // main thread only; see SubagentUsage
	GitBranch  string     `json:"git_branch,omitempty"`
	Model      string     `json:"model,omitempty"`
	CWD        string     `json:"cwd,omitempty"`

	// SubagentCount is the number of sub-agent transcripts found under
	// <session-id>/subagents/ for this session.
	SubagentCount int `json:"subagent_count"`
	// SubagentUsage is the summed token usage of all those transcripts. It is
	// reported separately from Usage so the existing per-session numbers keep
	// meaning "main thread" rather than silently changing definition.
	SubagentUsage TokenUsage `json:"subagent_usage"`

	// The fields below come from metadata events Claude Code re-appends on every
	// session resume. Like the title events they carry no timestamp, so the last
	// occurrence in the file wins and none of them bounds the session's time
	// range — see boundsSessionTimeRange.
	AgentName      string `json:"agent_name,omitempty"`
	PermissionMode string `json:"permission_mode,omitempty"`
	Mode           string `json:"mode,omitempty"`
	RelocatedCWD   string `json:"relocated_cwd,omitempty"`
	WorktreeName   string `json:"worktree_name,omitempty"`
	WorktreeBranch string `json:"worktree_branch,omitempty"`
	OriginalBranch string `json:"original_branch,omitempty"`

	// CompactionCount is how many times the conversation was compacted, from
	// `system` events with subtype compact_boundary.
	CompactionCount int `json:"compaction_count"`
	// DroppedTokens is how many tokens compaction discarded. Claude Code
	// normally reports cumulativeDroppedTokens, a running total, so the maximum
	// seen is the session's figure rather than the sum. Older releases omit it
	// and only report preTokens/postTokens; those boundaries contribute their
	// own difference instead, accumulated on top.
	DroppedTokens int `json:"dropped_tokens"`

	// PRs are the pull requests this session was linked to, deduplicated by URL.
	// A session can produce several.
	PRs []ClaudeSessionPR `json:"prs,omitempty"`

	// Cost is the main-thread cost, accumulated per assistant message at that
	// message's own model and timestamp during the scan. Like Usage it excludes
	// delegated work; SubagentCost holds that, and TotalCost() sums them.
	Cost SessionCost `json:"cost"`
	// SubagentCost is the summed cost of this session's sub-agent transcripts,
	// mirroring SubagentUsage.
	SubagentCost SessionCost `json:"subagent_cost"`
	// UnpricedModels names the models this session used that have no known rate.
	// Non-empty means Cost is a floor rather than a total, and the UI must say
	// so — an understated number presented as complete is the failure mode this
	// field exists to prevent.
	UnpricedModels []string `json:"unpriced_models,omitempty"`
	// UnpricedTokens is how many input+output tokens those models accounted for,
	// so an aggregate can state the size of what its total left out.
	UnpricedTokens int `json:"unpriced_tokens,omitempty"`
}

// ClaudeSessionPR is one pull request a session was linked to, from a `pr-link`
// event. Claude Code re-emits the event on every resume, so rows are keyed by
// URL and the first sighting's timestamp is kept.
type ClaudeSessionPR struct {
	PRNumber     int       `json:"pr_number"`
	PRURL        string    `json:"pr_url"`
	PRRepository string    `json:"pr_repository,omitempty"`
	FirstSeenAt  time.Time `json:"first_seen_at"`
}

// ResolveDisplayTitle picks the label to show for a session, most specific
// first: an explicit Agento rename, then Claude Code's own `/rename`, then its
// auto-generated title, and only then the first-message preview — which is
// frequently an injected system prompt and so nearly useless as a label.
func (s *ClaudeSessionSummary) ResolveDisplayTitle() string {
	for _, candidate := range []string{s.CustomTitle, s.NativeTitle, s.AITitle, s.Preview} {
		if candidate != "" {
			return candidate
		}
	}
	return ""
}

// TotalCost returns the session's main-thread cost plus the cost of every
// sub-agent it delegated to — the counterpart of TotalUsage, and what aggregate
// cost reporting must use so delegated spend is not silently dropped.
func (s ClaudeSessionSummary) TotalCost() SessionCost {
	total := s.Cost
	total.Add(s.SubagentCost)
	return total
}

// TotalUsage returns the session's main-thread usage plus the usage of every
// sub-agent it delegated to. Aggregate reporting (analytics, cost) should use
// this rather than Usage, which deliberately excludes delegated work.
func (s ClaudeSessionSummary) TotalUsage() TokenUsage {
	return TokenUsage{
		InputTokens:           s.Usage.InputTokens + s.SubagentUsage.InputTokens,
		OutputTokens:          s.Usage.OutputTokens + s.SubagentUsage.OutputTokens,
		CacheCreationTokens:   s.Usage.CacheCreationTokens + s.SubagentUsage.CacheCreationTokens,
		CacheCreation5mTokens: s.Usage.CacheCreation5mTokens + s.SubagentUsage.CacheCreation5mTokens,
		CacheCreation1hTokens: s.Usage.CacheCreation1hTokens + s.SubagentUsage.CacheCreation1hTokens,
		CacheReadTokens:       s.Usage.CacheReadTokens + s.SubagentUsage.CacheReadTokens,
	}
}

// ClaudeSubagent is a single sub-agent transcript delegated from a parent
// session, read from <session-id>/subagents/agent-<id>.jsonl plus its adjacent
// agent-<id>.meta.json sidecar.
type ClaudeSubagent struct {
	AgentID      string     `json:"agent_id"`
	AgentType    string     `json:"agent_type,omitempty"`
	Description  string     `json:"description,omitempty"`
	ToolUseID    string     `json:"tool_use_id,omitempty"`
	StartTime    time.Time  `json:"start_time"`
	LastActivity time.Time  `json:"last_activity"`
	MessageCount int        `json:"message_count"`
	Usage        TokenUsage `json:"usage"`
	Model        string     `json:"model,omitempty"`
}

// ClaudeSessionDetail extends the summary with full message history and todos.
type ClaudeSessionDetail struct {
	ClaudeSessionSummary
	Messages  []ClaudeMessage  `json:"messages"`
	Todos     []ClaudeTodo     `json:"todos"`
	Subagents []ClaudeSubagent `json:"subagents"`
}

// ClaudeMessage represents a single conversation turn (user or assistant).
type ClaudeMessage struct {
	UUID        string            `json:"uuid"`
	ParentUUID  string            `json:"parent_uuid,omitempty"`
	Type        string            `json:"type"` // "user" | "assistant"
	Timestamp   time.Time         `json:"timestamp"`
	Role        string            `json:"role,omitempty"`
	Content     string            `json:"content,omitempty"` // plain text for user messages
	Blocks      []NormalizedBlock `json:"blocks,omitempty"`  // for assistant messages
	Usage       *TokenUsage       `json:"usage,omitempty"`
	GitBranch   string            `json:"git_branch,omitempty"`
	IsSidechain bool              `json:"is_sidechain,omitempty"`
	// Children is reserved for events nested under this message. The progress
	// events that once populated it no longer exist, so it is currently unused;
	// sub-agent work is nested in the journey view instead (see journey.go).
	Children []ClaudeMessage `json:"children,omitempty"`
}

// NormalizedBlock is a content block normalized to Agento's rendering format.
// Thinking blocks use the "text" field (matching Agento's stored format).
type NormalizedBlock struct {
	Type  string          `json:"type"`            // "thinking" | "text" | "tool_use"
	Text  string          `json:"text,omitempty"`  // for "thinking" and "text"
	ID    string          `json:"id,omitempty"`    // for "tool_use"
	Name  string          `json:"name,omitempty"`  // for "tool_use"
	Input json.RawMessage `json:"input,omitempty"` // for "tool_use"
}

// ClaudeTodo represents a task item from the session's todo list.
type ClaudeTodo struct {
	Content    string `json:"content"`
	Status     string `json:"status"`                // "completed" | "in_progress" | "pending"
	ActiveForm string `json:"active_form,omitempty"` // present-continuous description
}
