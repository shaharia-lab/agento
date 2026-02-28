package storage

import (
	"fmt"

	"github.com/shaharia-lab/agento/internal/config"
)

// AgentStore defines the interface for agent persistence.
type AgentStore interface {
	List() ([]*config.AgentConfig, error)
	Get(slug string) (*config.AgentConfig, error)
	Save(agent *config.AgentConfig) error
	Delete(slug string) error
}

func validateAgentForSave(cfg *config.AgentConfig) error {
	if cfg.Name == "" {
		return fmt.Errorf("agent name is required")
	}
	if cfg.Slug == "" {
		return fmt.Errorf("agent slug is required")
	}
	switch cfg.Thinking {
	case "", "adaptive", "disabled", "enabled":
	default:
		return fmt.Errorf("invalid thinking value %q: must be adaptive, disabled, or enabled", cfg.Thinking)
	}
	return nil
}
