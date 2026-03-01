package api

import (
	"encoding/json"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/storage"
)

// ─── Agent request/response types ─────────────────────────────────────────────

// AgentRequest is the request body for creating or updating an agent.
type AgentRequest struct {
	Name         string                   `json:"name"`
	Slug         string                   `json:"slug"`
	Description  string                   `json:"description"`
	Model        string                   `json:"model"`
	Thinking     string                   `json:"thinking"`
	SystemPrompt string                   `json:"system_prompt"`
	Capabilities config.AgentCapabilities `json:"capabilities"`
}

// ─── Task request types ────────────────────────────────────────────────────────

// CreateTaskRequest is the request body for creating a new scheduled task.
// It mirrors the storage fields but keeps the public API decoupled from storage internals.
// A separate type from UpdateTaskRequest is kept intentionally: future API versions
// may require certain fields only at creation time (e.g. immutable schedule type).
type CreateTaskRequest struct {
	Name           string                 `json:"name"`
	Description    string                 `json:"description"`
	AgentSlug      string                 `json:"agent_slug"`
	Prompt         string                 `json:"prompt"`
	ScheduleType   storage.ScheduleType   `json:"schedule_type"`
	ScheduleConfig storage.ScheduleConfig `json:"schedule_config"`
	Status         storage.TaskStatus     `json:"status"`
	TimeoutMinutes int                    `json:"timeout_minutes"`
	SaveOutput     bool                   `json:"save_output"`
}

// UpdateTaskRequest is the request body for updating an existing scheduled task.
// Kept separate from CreateTaskRequest to allow future divergence (e.g. immutable
// fields that cannot be changed after creation).
type UpdateTaskRequest struct {
	Name           string                 `json:"name"`
	Description    string                 `json:"description"`
	AgentSlug      string                 `json:"agent_slug"`
	Prompt         string                 `json:"prompt"`
	ScheduleType   storage.ScheduleType   `json:"schedule_type"`
	ScheduleConfig storage.ScheduleConfig `json:"schedule_config"`
	Status         storage.TaskStatus     `json:"status"`
	TimeoutMinutes int                    `json:"timeout_minutes"`
	SaveOutput     bool                   `json:"save_output"`
}

// ─── Integration request types ────────────────────────────────────────────────

// CreateIntegrationRequest is the request body for creating a new integration.
// Kept separate from UpdateIntegrationRequest to allow future divergence (e.g.
// the integration type is immutable after creation).
type CreateIntegrationRequest struct {
	Name        string                          `json:"name"`
	Type        string                          `json:"type"`
	Enabled     bool                            `json:"enabled"`
	Credentials json.RawMessage                 `json:"credentials"`
	Services    map[string]config.ServiceConfig `json:"services"`
}

// UpdateIntegrationRequest is the request body for updating an existing integration.
type UpdateIntegrationRequest struct {
	Name        string                          `json:"name"`
	Type        string                          `json:"type"`
	Enabled     bool                            `json:"enabled"`
	Credentials json.RawMessage                 `json:"credentials"`
	Services    map[string]config.ServiceConfig `json:"services"`
}

// ─── Profile request types ────────────────────────────────────────────────────

// CreateProfileRequest is the request body for creating a new Claude settings profile.
type CreateProfileRequest struct {
	Name string `json:"name"`
}

// BulkDeleteRequest is the request body for bulk-delete endpoints.
type BulkDeleteRequest struct {
	IDs []string `json:"ids"`
}

// UpdateProfileRequest is the request body for updating a Claude settings profile.
type UpdateProfileRequest struct {
	Name     *string         `json:"name"`
	Settings json.RawMessage `json:"settings"`
}
