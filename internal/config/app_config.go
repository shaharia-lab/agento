package config

import (
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"

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

	// PublicURL is the externally reachable URL of this Agento instance.
	// Required for webhook registration (e.g. Telegram inbound triggers).
	// When set via env var, it takes precedence over the settings-stored value.
	PublicURL string `envconfig:"AGENTO_PUBLIC_URL"`
}

// Load reads AppConfig from environment variables using envconfig.
// DataDir defaults to ~/.agento if not set.
func Load() (*AppConfig, error) {
	var c AppConfig
	if err := envconfig.Process("", &c); err != nil {
		return nil, fmt.Errorf("loading config: %w", err)
	}
	resolvedDataDir, err := resolveDataDir(c.DataDir)
	if err != nil {
		return nil, err
	}
	c.DataDir = resolvedDataDir

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

// DatabasePath returns the path to the SQLite database file.
func (c *AppConfig) DatabasePath() string {
	return filepath.Join(c.DataDir, "agento.db")
}

// TmpUploadsDir returns the path to the temporary uploads directory.
// Files here are cleaned up at startup (files older than 24 hours are removed).
func (c *AppConfig) TmpUploadsDir() string {
	return filepath.Join(c.DataDir, "tmp-uploads")
}

// resolveDataDir returns the resolved data directory path.
// If dir is empty it defaults to ~/.agento.
// A leading ~ is expanded to the user's home directory so that values like
// AGENTO_DATA_DIR=~/.agento-dev work correctly without relying on shell expansion.
// Both forward-slash (Unix) and backslash (Windows) separators after ~ are supported.
func resolveDataDir(dir string) (string, error) {
	if dir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", fmt.Errorf("resolving home directory: %w", err)
		}
		return filepath.Join(home, ".agento"), nil
	}
	if dir == "~" || strings.HasPrefix(dir, "~/") || strings.HasPrefix(dir, `~\`) {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", fmt.Errorf("resolving home directory: %w", err)
		}
		if dir == "~" {
			return home, nil
		}
		return filepath.Join(home, dir[2:]), nil
	}
	return dir, nil
}
