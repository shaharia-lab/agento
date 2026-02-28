package storage

import (
	"testing"

	"github.com/stretchr/testify/assert"

	"github.com/shaharia-lab/agento/internal/config"
)

func TestValidateAgentForSave(t *testing.T) {
	tests := []struct {
		name    string
		cfg     *config.AgentConfig
		wantErr string
	}{
		{
			name:    "valid agent",
			cfg:     &config.AgentConfig{Name: "Test", Slug: "test", Thinking: "adaptive"},
			wantErr: "",
		},
		{
			name:    "missing name",
			cfg:     &config.AgentConfig{Slug: "test"},
			wantErr: "agent name is required",
		},
		{
			name:    "missing slug",
			cfg:     &config.AgentConfig{Name: "Test"},
			wantErr: "agent slug is required",
		},
		{
			name:    "empty thinking is valid",
			cfg:     &config.AgentConfig{Name: "Test", Slug: "test", Thinking: ""},
			wantErr: "",
		},
		{
			name:    "adaptive thinking",
			cfg:     &config.AgentConfig{Name: "Test", Slug: "test", Thinking: "adaptive"},
			wantErr: "",
		},
		{
			name:    "disabled thinking",
			cfg:     &config.AgentConfig{Name: "Test", Slug: "test", Thinking: "disabled"},
			wantErr: "",
		},
		{
			name:    "enabled thinking",
			cfg:     &config.AgentConfig{Name: "Test", Slug: "test", Thinking: "enabled"},
			wantErr: "",
		},
		{
			name:    "invalid thinking",
			cfg:     &config.AgentConfig{Name: "Test", Slug: "test", Thinking: "invalid"},
			wantErr: "invalid thinking value",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := validateAgentForSave(tt.cfg)
			if tt.wantErr == "" {
				assert.NoError(t, err)
			} else {
				assert.ErrorContains(t, err, tt.wantErr)
			}
		})
	}
}
