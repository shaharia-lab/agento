package storage

import "time"

// ConversationMapping links a platform channel to an internal chat session.
type ConversationMapping struct {
	ID           string    `json:"id"`
	SessionID    string    `json:"session_id"`
	PlatformType string    `json:"platform_type"`
	PlatformID   string    `json:"platform_id"`
	ChannelID    string    `json:"channel_id"`
	AgentSlug    string    `json:"agent_slug"`
	CreatedAt    time.Time `json:"created_at"`
	UpdatedAt    time.Time `json:"updated_at"`
}

// ConversationMappingStore persists the link between external platform
// conversations and internal chat sessions.
type ConversationMappingStore interface {
	// GetByPlatformChannel returns the mapping for a given platform + channel
	// combination, or nil if no mapping exists.
	GetByPlatformChannel(platformType, platformID, channelID string) (*ConversationMapping, error)

	// GetBySessionID returns the mapping for a given internal chat session, or nil.
	GetBySessionID(sessionID string) (*ConversationMapping, error)

	// Create inserts a new conversation mapping.
	Create(mapping *ConversationMapping) error

	// UpdateAgentSlug changes the agent associated with a mapping.
	UpdateAgentSlug(id, agentSlug string) error

	// Delete removes a mapping by its ID.
	Delete(id string) error

	// DeleteBySessionID removes the mapping for a given session.
	DeleteBySessionID(sessionID string) error

	// ListByPlatform returns all mappings for a given platform instance.
	ListByPlatform(platformType, platformID string) ([]*ConversationMapping, error)
}
