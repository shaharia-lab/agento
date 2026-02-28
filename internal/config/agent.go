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
	Name        string `yaml:"name"            json:"name"`
	Slug        string `yaml:"slug"            json:"slug"`
	Description string `yaml:"description"     json:"description"`
	Model       string `yaml:"model"           json:"model"`
	// Thinking mode: "adaptive", "disabled", or "enabled".
	Thinking string `yaml:"thinking"        json:"thinking"`
	// PermissionMode: "bypass" (default) or "default".
	PermissionMode string            `yaml:"permission_mode" json:"permission_mode"`
	SystemPrompt   string            `yaml:"system_prompt"   json:"system_prompt"`
	Capabilities   AgentCapabilities `yaml:"capabilities"    json:"capabilities"`
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

		cfg, err := loadAgentFile(dir, entry.Name())
		if err != nil {
			return nil, err
		}

		if err := validateAgentMCP(cfg, mcpRegistry); err != nil {
			return nil, err
		}

		registry.agents[cfg.Slug] = cfg
	}

	return registry, nil
}

// loadAgentFile reads and parses a single agent YAML file, applying defaults.
func loadAgentFile(dir, fileName string) (*AgentConfig, error) {
	//nolint:gosec // path is constructed from admin-configured data dir
	data, err := os.ReadFile(filepath.Join(dir, fileName))
	if err != nil {
		return nil, fmt.Errorf("reading agent file %q: %w", fileName, err)
	}

	var cfg AgentConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parsing agent file %q: %w", fileName, err)
	}

	applyAgentDefaults(&cfg, fileName)

	if err := validateAgent(&cfg); err != nil {
		return nil, fmt.Errorf("invalid agent %q: %w", fileName, err)
	}

	return &cfg, nil
}

// applyAgentDefaults fills in missing fields with sensible defaults.
func applyAgentDefaults(cfg *AgentConfig, fileName string) {
	if cfg.Slug == "" {
		cfg.Slug = strings.TrimSuffix(fileName, ".yaml")
	}
	if cfg.Model == "" {
		cfg.Model = "claude-sonnet-4-6"
	}
	if cfg.Thinking == "" {
		cfg.Thinking = "adaptive"
	}
}

// validateAgentMCP checks that all MCP servers referenced by the agent exist in the registry.
func validateAgentMCP(cfg *AgentConfig, mcpRegistry *MCPRegistry) error {
	if mcpRegistry == nil {
		return nil
	}
	for serverName := range cfg.Capabilities.MCP {
		if !mcpRegistry.Has(serverName) {
			return fmt.Errorf("agent %q: references unknown MCP server %q (not in MCP registry)", cfg.Slug, serverName)
		}
	}
	return nil
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

	switch cfg.PermissionMode {
	case "", "bypass", "default":
		// valid
	default:
		return fmt.Errorf("invalid permission_mode %q: must be bypass or default", cfg.PermissionMode)
	}

	return nil
}
