package config

import (
	"log/slog"
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAppConfig_SlogLevel(t *testing.T) {
	tests := []struct {
		name     string
		logLevel string
		want     slog.Level
	}{
		{"debug", "debug", slog.LevelDebug},
		{"info", "info", slog.LevelInfo},
		{"warn", "warn", slog.LevelWarn},
		{"error", "error", slog.LevelError},
		{"unknown defaults to info", "unknown", slog.LevelInfo},
		{"empty defaults to info", "", slog.LevelInfo},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := &AppConfig{LogLevel: tt.logLevel}
			assert.Equal(t, tt.want, c.SlogLevel())
		})
	}
}

func TestAppConfig_DirectoryPaths(t *testing.T) {
	c := &AppConfig{DataDir: "/data"}

	tests := []struct {
		name string
		fn   func() string
		want string
	}{
		{"LogDir", c.LogDir, "/data/logs"},
		{"AgentsDir", c.AgentsDir, "/data/agents"},
		{"ChatsDir", c.ChatsDir, "/data/chats"},
		{"MCPsFile", c.MCPsFile, "/data/mcps.yaml"},
		{"IntegrationsDir", c.IntegrationsDir, "/data/integrations"},
		{"DatabasePath", c.DatabasePath, "/data/agento.db"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, tt.fn())
		})
	}
}

func TestLoad_DefaultDataDir(t *testing.T) {
	t.Setenv("AGENTO_DATA_DIR", "")
	t.Setenv("PORT", "8990")
	t.Setenv("AGENTO_DEFAULT_MODEL", "")
	t.Setenv("ANTHROPIC_DEFAULT_SONNET_MODEL", "")
	t.Setenv("ANTHROPIC_API_KEY", "")
	t.Setenv("AGENTO_WORKING_DIR", "")

	cfg, err := Load()
	require.NoError(t, err)

	home, err := os.UserHomeDir()
	require.NoError(t, err)
	assert.Equal(t, filepath.Join(home, ".agento"), cfg.DataDir)
}

func TestLoad_TildeExpansion(t *testing.T) {
	t.Setenv("AGENTO_DATA_DIR", "~/.agento-dev")
	t.Setenv("PORT", "8990")
	t.Setenv("AGENTO_DEFAULT_MODEL", "")
	t.Setenv("ANTHROPIC_DEFAULT_SONNET_MODEL", "")
	t.Setenv("ANTHROPIC_API_KEY", "")
	t.Setenv("AGENTO_WORKING_DIR", "")

	cfg, err := Load()
	require.NoError(t, err)

	home, err := os.UserHomeDir()
	require.NoError(t, err)
	assert.Equal(t, filepath.Join(home, ".agento-dev"), cfg.DataDir)
	// Tilde must not be present in the resolved path.
	assert.NotContains(t, cfg.DataDir, "~")
}

func TestResolveDataDir(t *testing.T) {
	home, err := os.UserHomeDir()
	require.NoError(t, err)

	tests := []struct {
		name  string
		input string
		want  string
	}{
		{"empty defaults to ~/.agento", "", filepath.Join(home, ".agento")},
		{"bare tilde resolves to home", "~", home},
		{"tilde prefix is expanded", "~/.agento-dev", filepath.Join(home, ".agento-dev")},
		{"absolute path is unchanged", "/custom/data", "/custom/data"},
		{"relative path is unchanged", "relative/path", "relative/path"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := resolveDataDir(tt.input)
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestLoad(t *testing.T) {
	t.Setenv("PORT", "9090")
	t.Setenv("AGENTO_DATA_DIR", "/tmp/test-agento")
	t.Setenv("LOG_LEVEL", "debug")
	t.Setenv("AGENTO_DEFAULT_MODEL", "")
	t.Setenv("ANTHROPIC_DEFAULT_SONNET_MODEL", "")
	t.Setenv("ANTHROPIC_API_KEY", "")
	t.Setenv("AGENTO_WORKING_DIR", "")

	cfg, err := Load()
	assert.NoError(t, err)
	assert.Equal(t, "/tmp/test-agento", cfg.DataDir)
	assert.Equal(t, "debug", cfg.LogLevel)
	assert.Equal(t, 9090, cfg.Port)
	// DefaultModel should be the built-in default
	assert.Equal(t, "sonnet", cfg.DefaultModel)
}

func TestLoad_DefaultModel_Priority(t *testing.T) {
	tests := []struct {
		name          string
		defaultModel  string
		sonnetModel   string
		expectedModel string
	}{
		{
			name:          "AGENTO_DEFAULT_MODEL takes priority",
			defaultModel:  "custom-model",
			sonnetModel:   "sonnet-model",
			expectedModel: "custom-model",
		},
		{
			name:          "ANTHROPIC_DEFAULT_SONNET_MODEL used when no AGENTO_DEFAULT_MODEL",
			defaultModel:  "",
			sonnetModel:   "sonnet-model",
			expectedModel: "sonnet-model",
		},
		{
			name:          "built-in default when neither env var set",
			defaultModel:  "",
			sonnetModel:   "",
			expectedModel: "sonnet",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Setenv("PORT", "8990")
			t.Setenv("AGENTO_DATA_DIR", "/tmp/test")
			t.Setenv("AGENTO_DEFAULT_MODEL", tt.defaultModel)
			t.Setenv("ANTHROPIC_DEFAULT_SONNET_MODEL", tt.sonnetModel)

			cfg, err := Load()
			assert.NoError(t, err)
			assert.Equal(t, tt.expectedModel, cfg.DefaultModel)
		})
	}
}
