package storage

import (
	"context"
	"encoding/json"
	"time"
)

// ChatSession represents a chat session's metadata.
type ChatSession struct {
	ID                string `json:"id"`
	Title             string `json:"title"`
	AgentSlug         string `json:"agent_slug"`
	SDKSession        string `json:"sdk_session_id"`
	WorkingDir        string `json:"working_directory"`
	Model             string `json:"model"`
	SettingsProfileID string `json:"settings_profile_id,omitempty"`
	// PermissionMode overrides the agent's own mode for this conversation:
	// "bypass", "default", "plan" or "dontAsk". Empty means no choice was
	// recorded, which is not a fifth mode — the run falls back to the agent's
	// configured mode, and to "default" for a chat with no agent. See
	// migration 30. `omitempty` keeps it off the wire for every row written
	// before it existed.
	PermissionMode string    `json:"permission_mode,omitempty"`
	CreatedAt      time.Time `json:"created_at"`
	UpdatedAt      time.Time `json:"updated_at"`
	// Cumulative token usage across all turns in this session.
	TotalInputTokens         int  `json:"total_input_tokens,omitempty"`
	TotalOutputTokens        int  `json:"total_output_tokens,omitempty"`
	TotalCacheCreationTokens int  `json:"total_cache_creation_tokens,omitempty"`
	TotalCacheReadTokens     int  `json:"total_cache_read_tokens,omitempty"`
	IsFavorite               bool `json:"is_favorite,omitempty"`
}

// MessageBlock represents a single ordered content block within an assistant message.
// Blocks are stored alongside the message so the UI can reconstruct the full
// thinking → text → tool_use rendering after a page reload.
type MessageBlock struct {
	Type  string          `json:"type"`            // "thinking" | "text" | "tool_use"
	Text  string          `json:"text,omitempty"`  // for "thinking" and "text"
	ID    string          `json:"id,omitempty"`    // for "tool_use"
	Name  string          `json:"name,omitempty"`  // for "tool_use"
	Input json.RawMessage `json:"input,omitempty"` // for "tool_use"
}

// ChatMessage represents a single message in a chat session.
type ChatMessage struct {
	Role      string         `json:"role"`
	Content   string         `json:"content"`
	Timestamp time.Time      `json:"timestamp"`
	Blocks    []MessageBlock `json:"blocks,omitempty"`
}

// NewSessionParams is everything CreateSession writes onto a fresh row.
//
// A struct rather than positional arguments because every field is a string:
// `CreateSession(ctx, slug, dir, model, profile, mode)` has no call site where
// transposing two of them fails to compile, and there are five such pairs.
type NewSessionParams struct {
	AgentSlug         string
	WorkingDir        string
	Model             string
	SettingsProfileID string
	// PermissionMode is empty for "no choice recorded" — see the field's note
	// on ChatSession and migration 30.
	PermissionMode string
}

// ChatStore defines the interface for chat session persistence.
type ChatStore interface {
	ListSessions(ctx context.Context) ([]*ChatSession, error)
	GetSession(ctx context.Context, id string) (*ChatSession, error)
	GetSessionWithMessages(ctx context.Context, id string) (*ChatSession, []ChatMessage, error)
	CreateSession(ctx context.Context, p NewSessionParams) (*ChatSession, error)
	AppendMessage(ctx context.Context, sessionID string, msg ChatMessage) error
	UpdateSession(ctx context.Context, session *ChatSession) error
	DeleteSession(ctx context.Context, id string) error
	BulkDeleteSessions(ctx context.Context, ids []string) error
}
