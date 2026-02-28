package config

import (
	"log/slog"
	"testing"

	"github.com/stretchr/testify/assert"
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
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, tt.fn())
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
	assert.Equal(t, "eu.anthropic.claude-sonnet-4-5-20250929-v1:0", cfg.DefaultModel)
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
			expectedModel: "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
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
