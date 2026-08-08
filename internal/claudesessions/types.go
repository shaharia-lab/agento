package claudesessions

import (
	"encoding/json"
	"time"
)

// TokenUsage represents API token consumption for a session or message turn.
type TokenUsage struct {
	InputTokens         int `json:"input_tokens"`
	OutputTokens        int `json:"output_tokens"`
	CacheCreationTokens int `json:"cache_creation_tokens"`
	CacheReadTokens     int `json:"cache_read_tokens"`
}

// ClaudeProject represents a project directory containing Claude Code sessions.
type ClaudeProject struct {
	EncodedName  string `json:"encoded_name"`
	DecodedPath  string `json:"decoded_path"`
	SessionCount int    `json:"session_count"`
}

// ClaudeSessionSummary contains lightweight metadata for list views.
type ClaudeSessionSummary struct {
	SessionID    string     `json:"session_id"`
	ProjectPath  string     `json:"project_path"`
	Preview      string     `json:"preview"`                // first user message text, truncated
	CustomTitle  string     `json:"custom_title,omitempty"` // user-defined label, preserved across rescans
	IsFavorite   bool       `json:"is_favorite,omitempty"`  // user-starred, preserved across rescans
	StartTime    time.Time  `json:"start_time"`
	LastActivity time.Time  `json:"last_activity"`
	MessageCount int        `json:"message_count"` // user + assistant top-level messages
	Usage        TokenUsage `json:"usage"`         // main thread only; see SubagentUsage
	GitBranch    string     `json:"git_branch,omitempty"`
	Model        string     `json:"model,omitempty"`
	CWD          string     `json:"cwd,omitempty"`

	// SubagentCount is the number of sub-agent transcripts found under
	// <session-id>/subagents/ for this session.
	SubagentCount int `json:"subagent_count"`
	// SubagentUsage is the summed token usage of all those transcripts. It is
	// reported separately from Usage so the existing per-session numbers keep
	// meaning "main thread" rather than silently changing definition.
	SubagentUsage TokenUsage `json:"subagent_usage"`
}

// TotalUsage returns the session's main-thread usage plus the usage of every
// sub-agent it delegated to. Aggregate reporting (analytics, cost) should use
// this rather than Usage, which deliberately excludes delegated work.
func (s ClaudeSessionSummary) TotalUsage() TokenUsage {
	return TokenUsage{
		InputTokens:         s.Usage.InputTokens + s.SubagentUsage.InputTokens,
		OutputTokens:        s.Usage.OutputTokens + s.SubagentUsage.OutputTokens,
		CacheCreationTokens: s.Usage.CacheCreationTokens + s.SubagentUsage.CacheCreationTokens,
		CacheReadTokens:     s.Usage.CacheReadTokens + s.SubagentUsage.CacheReadTokens,
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
	Type        string            `json:"type"` // "user" | "assistant" | "progress"
	Timestamp   time.Time         `json:"timestamp"`
	Role        string            `json:"role,omitempty"`
	Content     string            `json:"content,omitempty"` // plain text for user messages
	Blocks      []NormalizedBlock `json:"blocks,omitempty"`  // for assistant messages
	Usage       *TokenUsage       `json:"usage,omitempty"`
	GitBranch   string            `json:"git_branch,omitempty"`
	IsSidechain bool              `json:"is_sidechain,omitempty"`
	// Children holds progress/sub-agent events nested under this message.
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
