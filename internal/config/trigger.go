package config

import "time"

// TriggerRule defines a rule that matches incoming messages to an agent.
type TriggerRule struct {
	ID             string    `json:"id"`
	IntegrationID  string    `json:"integration_id"`
	Name           string    `json:"name"`
	AgentSlug      string    `json:"agent_slug"`
	Enabled        bool      `json:"enabled"`
	FilterPrefix   string    `json:"filter_prefix"`
	FilterKeywords []string  `json:"filter_keywords"`
	FilterChatIDs  []string  `json:"filter_chat_ids"`
	CreatedAt      time.Time `json:"created_at"`
	UpdatedAt      time.Time `json:"updated_at"`
}
