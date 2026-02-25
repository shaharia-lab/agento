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

	// Port is the HTTP server port. Defaults to 8080.
	Port int `envconfig:"PORT" default:"8080"`

	// DataDir is the root data directory. Defaults to ~/.agento.
	DataDir string `envconfig:"AGENTO_DATA_DIR"`

	// LogLevel sets the minimum log level (debug, info, warn, error). Defaults to info.
	LogLevel string `envconfig:"LOG_LEVEL" default:"info"`
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
