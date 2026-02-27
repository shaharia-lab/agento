package storage

import (
	"encoding/json"
	"time"
)

// ChatSession represents a chat session's metadata.
type ChatSession struct {
	ID                string    `json:"id"`
	Title             string    `json:"title"`
	AgentSlug         string    `json:"agent_slug"`
	SDKSession        string    `json:"sdk_session_id"`
	WorkingDir        string    `json:"working_directory"`
	Model             string    `json:"model"`
	SettingsProfileID string    `json:"settings_profile_id,omitempty"`
	CreatedAt         time.Time `json:"created_at"`
	UpdatedAt         time.Time `json:"updated_at"`
	// Cumulative token usage across all turns in this session.
	TotalInputTokens         int `json:"total_input_tokens,omitempty"`
	TotalOutputTokens        int `json:"total_output_tokens,omitempty"`
	TotalCacheCreationTokens int `json:"total_cache_creation_tokens,omitempty"`
	TotalCacheReadTokens     int `json:"total_cache_read_tokens,omitempty"`
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

// ChatStore defines the interface for chat session persistence.
type ChatStore interface {
	ListSessions() ([]*ChatSession, error)
	GetSession(id string) (*ChatSession, error)
	GetSessionWithMessages(id string) (*ChatSession, []ChatMessage, error)
	CreateSession(agentSlug, workingDir, model, settingsProfileID string) (*ChatSession, error)
	AppendMessage(sessionID string, msg ChatMessage) error
	UpdateSession(session *ChatSession) error
	DeleteSession(id string) error
}
