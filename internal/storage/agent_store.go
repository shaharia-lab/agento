package storage

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"gopkg.in/yaml.v3"

	"github.com/shaharia-lab/agents-platform-cc-go/internal/config"
)

// AgentStore defines the interface for agent persistence.
type AgentStore interface {
	List() ([]*config.AgentConfig, error)
	Get(slug string) (*config.AgentConfig, error)
	Save(agent *config.AgentConfig) error
	Delete(slug string) error
}

// FSAgentStore implements AgentStore on the local filesystem.
// Each agent is stored as a YAML file: <dir>/<slug>.yaml
type FSAgentStore struct {
	dir string
}

// NewFSAgentStore creates an FSAgentStore rooted at dir.
func NewFSAgentStore(dir string) *FSAgentStore {
	return &FSAgentStore{dir: dir}
}

func (s *FSAgentStore) List() ([]*config.AgentConfig, error) {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		if os.IsNotExist(err) {
			return []*config.AgentConfig{}, nil
		}
		return nil, fmt.Errorf("reading agents dir: %w", err)
	}

	var agents []*config.AgentConfig
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".yaml") {
			continue
		}
		slug := strings.TrimSuffix(entry.Name(), ".yaml")
		agent, err := s.Get(slug)
		if err != nil {
			return nil, err
		}
		if agent != nil {
			agents = append(agents, agent)
		}
	}
	if agents == nil {
		agents = []*config.AgentConfig{}
	}
	return agents, nil
}

func (s *FSAgentStore) Get(slug string) (*config.AgentConfig, error) {
	data, err := os.ReadFile(filepath.Join(s.dir, slug+".yaml"))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("reading agent %q: %w", slug, err)
	}

	var cfg config.AgentConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parsing agent %q: %w", slug, err)
	}
	if cfg.Slug == "" {
		cfg.Slug = slug
	}
	if cfg.Model == "" {
		cfg.Model = "claude-sonnet-4-6"
	}
	if cfg.Thinking == "" {
		cfg.Thinking = "adaptive"
	}
	return &cfg, nil
}

func (s *FSAgentStore) Save(agent *config.AgentConfig) error {
	if err := validateAgentForSave(agent); err != nil {
		return err
	}
	data, err := yaml.Marshal(agent)
	if err != nil {
		return fmt.Errorf("marshaling agent %q: %w", agent.Slug, err)
	}
	path := filepath.Join(s.dir, agent.Slug+".yaml")
	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("writing agent %q: %w", agent.Slug, err)
	}
	return nil
}

func (s *FSAgentStore) Delete(slug string) error {
	path := filepath.Join(s.dir, slug+".yaml")
	if err := os.Remove(path); err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("agent %q not found", slug)
		}
		return fmt.Errorf("deleting agent %q: %w", slug, err)
	}
	return nil
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
