package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"gopkg.in/yaml.v3"
)

// AgentConfig holds the full configuration for a single agent, as parsed from YAML.
type AgentConfig struct {
	Name         string            `yaml:"name"          json:"name"`
	Slug         string            `yaml:"slug"          json:"slug"`
	Description  string            `yaml:"description"   json:"description"`
	Model        string            `yaml:"model"         json:"model"`
	Thinking     string            `yaml:"thinking"      json:"thinking"` // "adaptive", "disabled", or "enabled"
	SystemPrompt string            `yaml:"system_prompt" json:"system_prompt"`
	Capabilities AgentCapabilities `yaml:"capabilities"  json:"capabilities"`
}

// AgentCapabilities defines what tools an agent can use.
type AgentCapabilities struct {
	BuiltIn []string          `yaml:"built_in" json:"built_in"`
	Local   []string          `yaml:"local"    json:"local"`
	MCP     map[string]MCPCap `yaml:"mcp"      json:"mcp"`
}

// MCPCap specifies which tools from an MCP server an agent may use.
type MCPCap struct {
	Tools []string `yaml:"tools" json:"tools"`
}

// AgentRegistry is an in-memory lookup of agents by slug.
type AgentRegistry struct {
	agents map[string]*AgentConfig
}

// Get returns the agent with the given slug, or nil if not found.
func (r *AgentRegistry) Get(slug string) *AgentConfig {
	return r.agents[slug]
}

// List returns all registered agents.
func (r *AgentRegistry) List() []*AgentConfig {
	list := make([]*AgentConfig, 0, len(r.agents))
	for _, a := range r.agents {
		list = append(list, a)
	}
	return list
}

// LoadAgents reads all *.yaml files from dir and returns a populated AgentRegistry.
// If mcpRegistry is non-nil, it validates that any MCP servers referenced by agents
// are present in the registry.
func LoadAgents(dir string, mcpRegistry *MCPRegistry) (*AgentRegistry, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, fmt.Errorf("reading agents directory %q: %w", dir, err)
	}

	registry := &AgentRegistry{agents: make(map[string]*AgentConfig)}

	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".yaml") {
			continue
		}

		data, err := os.ReadFile(filepath.Join(dir, entry.Name())) //nolint:gosec // path is constructed from admin-configured data dir
		if err != nil {
			return nil, fmt.Errorf("reading agent file %q: %w", entry.Name(), err)
		}

		var cfg AgentConfig
		if err := yaml.Unmarshal(data, &cfg); err != nil {
			return nil, fmt.Errorf("parsing agent file %q: %w", entry.Name(), err)
		}

		if cfg.Slug == "" {
			cfg.Slug = strings.TrimSuffix(entry.Name(), ".yaml")
		}

		if cfg.Model == "" {
			cfg.Model = "claude-sonnet-4-6"
		}

		if cfg.Thinking == "" {
			cfg.Thinking = "adaptive"
		}

		if err := validateAgent(&cfg); err != nil {
			return nil, fmt.Errorf("invalid agent %q: %w", entry.Name(), err)
		}

		if mcpRegistry != nil {
			for serverName := range cfg.Capabilities.MCP {
				if !mcpRegistry.Has(serverName) {
					return nil, fmt.Errorf("agent %q: references unknown MCP server %q (not in MCP registry)", cfg.Slug, serverName)
				}
			}
		}

		registry.agents[cfg.Slug] = &cfg
	}

	return registry, nil
}

func validateAgent(cfg *AgentConfig) error {
	if cfg.Name == "" {
		return fmt.Errorf("missing required field: name")
	}

	switch cfg.Thinking {
	case "adaptive", "disabled", "enabled":
	default:
		return fmt.Errorf("invalid thinking value %q: must be adaptive, disabled, or enabled", cfg.Thinking)
	}

	return nil
}
