package config

import (
	"fmt"
	"log/slog"
	"os"
	"path/filepath"

	"github.com/kelseyhightower/envconfig"
)

// AppConfig holds all application-level configuration loaded from environment variables.
type AppConfig struct {
	// AnthropicAPIKey is forwarded to the claude CLI when set.
	// Optional — the claude CLI uses its own stored credentials if not provided.
	AnthropicAPIKey string `envconfig:"ANTHROPIC_API_KEY"`

	// Port is the HTTP server port. Defaults to 8990.
	Port int `envconfig:"PORT" default:"8990"`

	// DataDir is the root data directory. Defaults to ~/.agento.
	DataDir string `envconfig:"AGENTO_DATA_DIR"`

	// LogLevel sets the minimum log level (debug, info, warn, error). Defaults to info.
	LogLevel string `envconfig:"LOG_LEVEL" default:"info"`

	// DefaultModel is the Claude model used for no-agent (direct) chat sessions.
	// Priority: AGENTO_DEFAULT_MODEL > ANTHROPIC_DEFAULT_SONNET_MODEL > built-in default.
	DefaultModel string `envconfig:"AGENTO_DEFAULT_MODEL"`

	// AnthropicDefaultSonnetModel is the Anthropic-standard env var for a preferred Sonnet model.
	// Used as a soft default when AGENTO_DEFAULT_MODEL is not set (not locked).
	AnthropicDefaultSonnetModel string `envconfig:"ANTHROPIC_DEFAULT_SONNET_MODEL"`

	// WorkingDir is the default working directory for chat sessions.
	// Can be overridden with the AGENTO_WORKING_DIR environment variable.
	WorkingDir string `envconfig:"AGENTO_WORKING_DIR"`
}

// Load reads AppConfig from environment variables using envconfig.
// DataDir defaults to ~/.agento if not set.
func Load() (*AppConfig, error) {
	var c AppConfig
	if err := envconfig.Process("", &c); err != nil {
		return nil, fmt.Errorf("loading config: %w", err)
	}
	if c.DataDir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return nil, fmt.Errorf("resolving home directory: %w", err)
		}
		c.DataDir = filepath.Join(home, ".agento")
	}

	// Resolve the effective default model:
	//   1. AGENTO_DEFAULT_MODEL — highest priority, locks the field
	//   2. ANTHROPIC_DEFAULT_SONNET_MODEL — soft default, user can still override from UI
	//   3. Built-in hardcoded default
	if c.DefaultModel == "" {
		if c.AnthropicDefaultSonnetModel != "" {
			c.DefaultModel = c.AnthropicDefaultSonnetModel
		} else {
			c.DefaultModel = "sonnet"
		}
	}

	return &c, nil
}

// SlogLevel converts the LogLevel string to a slog.Level.
// Unknown values default to slog.LevelInfo.
func (c *AppConfig) SlogLevel() slog.Level {
	switch c.LogLevel {
	case "debug":
		return slog.LevelDebug
	case "warn":
		return slog.LevelWarn
	case "error":
		return slog.LevelError
	default:
		return slog.LevelInfo
	}
}

// LogDir returns the path to the log directory (~/.agento/logs).
func (c *AppConfig) LogDir() string {
	return filepath.Join(c.DataDir, "logs")
}

// AgentsDir returns the path to the agents storage directory.
func (c *AppConfig) AgentsDir() string {
	return filepath.Join(c.DataDir, "agents")
}

// ChatsDir returns the path to the chats storage directory.
func (c *AppConfig) ChatsDir() string {
	return filepath.Join(c.DataDir, "chats")
}

// MCPsFile returns the path to the MCP registry YAML file.
func (c *AppConfig) MCPsFile() string {
	return filepath.Join(c.DataDir, "mcps.yaml")
}

// IntegrationsDir returns the path to the integrations storage directory.
func (c *AppConfig) IntegrationsDir() string {
	return filepath.Join(c.DataDir, "integrations")
}
