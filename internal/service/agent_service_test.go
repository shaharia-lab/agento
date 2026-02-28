package service

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/storage/mocks"
)

func newTestLogger() *slog.Logger {
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}

func TestToSlug(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		expected string
	}{
		{name: "simple lowercase", input: "hello", expected: "hello"},
		{name: "uppercase to lowercase", input: "Hello World", expected: "hello-world"},
		{name: "special characters replaced", input: "My Agent #1!", expected: "my-agent-1"},
		{name: "multiple spaces collapsed", input: "hello   world", expected: "hello-world"},
		{name: "trailing special chars trimmed", input: "hello--", expected: "hello"},
		{name: "leading special chars skipped", input: "  hello", expected: "hello"},
		{name: "mixed alphanumeric", input: "Agent-v2.0", expected: "agent-v2-0"},
		{name: "all special characters", input: "!!!", expected: ""},
		{name: "digits only", input: "123", expected: "123"},
		{name: "already a slug", input: "my-agent", expected: "my-agent"},
		{name: "unicode replaced with hyphens", input: "café latte", expected: "caf-latte"},
		{name: "trailing hyphens from special chars", input: "test!!!", expected: "test"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := toSlug(tt.input)
			assert.Equal(t, tt.expected, result)
		})
	}
}

func TestAgentService_List(t *testing.T) {
	tests := []struct {
		name        string
		setupMock   func(m *mocks.MockAgentStore)
		wantAgents  []*config.AgentConfig
		wantErr     bool
		errContains string
	}{
		{
			name: "returns agents successfully",
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("List").Return([]*config.AgentConfig{
					{Name: "Agent A", Slug: "agent-a"},
					{Name: "Agent B", Slug: "agent-b"},
				}, nil)
			},
			wantAgents: []*config.AgentConfig{
				{Name: "Agent A", Slug: "agent-a"},
				{Name: "Agent B", Slug: "agent-b"},
			},
		},
		{
			name: "returns empty list",
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("List").Return([]*config.AgentConfig{}, nil)
			},
			wantAgents: []*config.AgentConfig{},
		},
		{
			name: "wraps repo error",
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("List").Return(nil, errors.New("db connection failed"))
			},
			wantErr:     true,
			errContains: "listing agents",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			repo := new(mocks.MockAgentStore)
			tt.setupMock(repo)
			svc := NewAgentService(repo, newTestLogger())

			agents, err := svc.List(context.Background())

			if tt.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tt.errContains)
				assert.Nil(t, agents)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.wantAgents, agents)
			}
			repo.AssertExpectations(t)
		})
	}
}

func TestAgentService_Get(t *testing.T) {
	tests := []struct {
		name        string
		slug        string
		setupMock   func(m *mocks.MockAgentStore)
		wantAgent   *config.AgentConfig
		wantErr     bool
		errContains string
	}{
		{
			name: "returns agent successfully",
			slug: "my-agent",
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "my-agent").Return(&config.AgentConfig{
					Name: "My Agent", Slug: "my-agent", Model: "claude-sonnet-4-6",
				}, nil)
			},
			wantAgent: &config.AgentConfig{
				Name: "My Agent", Slug: "my-agent", Model: "claude-sonnet-4-6",
			},
		},
		{
			name: "returns nil when not found",
			slug: "nonexistent",
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "nonexistent").Return(nil, nil)
			},
			wantAgent: nil,
		},
		{
			name: "wraps repo error",
			slug: "bad-agent",
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "bad-agent").Return(nil, errors.New("read error"))
			},
			wantErr:     true,
			errContains: "getting agent",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			repo := new(mocks.MockAgentStore)
			tt.setupMock(repo)
			svc := NewAgentService(repo, newTestLogger())

			agent, err := svc.Get(context.Background(), tt.slug)

			if tt.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tt.errContains)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.wantAgent, agent)
			}
			repo.AssertExpectations(t)
		})
	}
}

func TestAgentService_Create(t *testing.T) {
	tests := []struct {
		name        string
		input       *config.AgentConfig
		setupMock   func(m *mocks.MockAgentStore)
		wantAgent   *config.AgentConfig
		wantErr     bool
		errType     interface{}
		errContains string
	}{
		{
			name: "creates agent with all defaults",
			input: &config.AgentConfig{
				Name: "My New Agent",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "my-new-agent").Return(nil, nil)
				m.On("Save", mock.AnythingOfType("*config.AgentConfig")).Return(nil)
			},
			wantAgent: &config.AgentConfig{
				Name:     "My New Agent",
				Slug:     "my-new-agent",
				Model:    "claude-sonnet-4-6",
				Thinking: "adaptive",
			},
		},
		{
			name: "creates agent with explicit slug",
			input: &config.AgentConfig{
				Name: "My Agent",
				Slug: "custom-slug",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "custom-slug").Return(nil, nil)
				m.On("Save", mock.AnythingOfType("*config.AgentConfig")).Return(nil)
			},
			wantAgent: &config.AgentConfig{
				Name:     "My Agent",
				Slug:     "custom-slug",
				Model:    "claude-sonnet-4-6",
				Thinking: "adaptive",
			},
		},
		{
			name: "creates agent with explicit model and thinking",
			input: &config.AgentConfig{
				Name:     "Full Agent",
				Model:    "claude-opus-4-6",
				Thinking: "enabled",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "full-agent").Return(nil, nil)
				m.On("Save", mock.AnythingOfType("*config.AgentConfig")).Return(nil)
			},
			wantAgent: &config.AgentConfig{
				Name:     "Full Agent",
				Slug:     "full-agent",
				Model:    "claude-opus-4-6",
				Thinking: "enabled",
			},
		},
		{
			name: "creates agent with bypass permission mode",
			input: &config.AgentConfig{
				Name:           "Bypass Agent",
				PermissionMode: "bypass",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "bypass-agent").Return(nil, nil)
				m.On("Save", mock.AnythingOfType("*config.AgentConfig")).Return(nil)
			},
			wantAgent: &config.AgentConfig{
				Name:           "Bypass Agent",
				Slug:           "bypass-agent",
				Model:          "claude-sonnet-4-6",
				Thinking:       "adaptive",
				PermissionMode: "bypass",
			},
		},
		{
			name: "creates agent with default permission mode",
			input: &config.AgentConfig{
				Name:           "Default Agent",
				PermissionMode: "default",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "default-agent").Return(nil, nil)
				m.On("Save", mock.AnythingOfType("*config.AgentConfig")).Return(nil)
			},
			wantAgent: &config.AgentConfig{
				Name:           "Default Agent",
				Slug:           "default-agent",
				Model:          "claude-sonnet-4-6",
				Thinking:       "adaptive",
				PermissionMode: "default",
			},
		},
		{
			name:  "fails when name is empty",
			input: &config.AgentConfig{},
			setupMock: func(m *mocks.MockAgentStore) {
				// no repo calls expected
			},
			wantErr: true,
			errType: &ValidationError{},
		},
		{
			name: "fails when slug is invalid",
			input: &config.AgentConfig{
				Name: "Agent",
				Slug: "INVALID_SLUG!!",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				// no repo calls expected
			},
			wantErr: true,
			errType: &ValidationError{},
		},
		{
			name: "fails when slug has uppercase",
			input: &config.AgentConfig{
				Name: "Agent",
				Slug: "MyAgent",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				// no repo calls expected
			},
			wantErr: true,
			errType: &ValidationError{},
		},
		{
			name: "fails when slug has trailing hyphen",
			input: &config.AgentConfig{
				Name: "Agent",
				Slug: "my-agent-",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				// no repo calls expected
			},
			wantErr: true,
			errType: &ValidationError{},
		},
		{
			name: "fails when slug has leading hyphen",
			input: &config.AgentConfig{
				Name: "Agent",
				Slug: "-my-agent",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				// no repo calls expected
			},
			wantErr: true,
			errType: &ValidationError{},
		},
		{
			name: "fails when slug has consecutive hyphens",
			input: &config.AgentConfig{
				Name: "Agent",
				Slug: "my--agent",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				// no repo calls expected
			},
			wantErr: true,
			errType: &ValidationError{},
		},
		{
			name: "fails with invalid permission mode",
			input: &config.AgentConfig{
				Name:           "Agent",
				PermissionMode: "invalid",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				// no repo calls expected
			},
			wantErr: true,
			errType: &ValidationError{},
		},
		{
			name: "fails when slug already exists",
			input: &config.AgentConfig{
				Name: "Existing Agent",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "existing-agent").Return(&config.AgentConfig{
					Name: "Existing Agent", Slug: "existing-agent",
				}, nil)
			},
			wantErr: true,
			errType: &ConflictError{},
		},
		{
			name: "fails when repo.Get returns error during uniqueness check",
			input: &config.AgentConfig{
				Name: "Agent",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "agent").Return(nil, errors.New("db error"))
			},
			wantErr:     true,
			errContains: "checking slug uniqueness",
		},
		{
			name: "fails when repo.Save returns error",
			input: &config.AgentConfig{
				Name: "Save Fail Agent",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "save-fail-agent").Return(nil, nil)
				m.On("Save", mock.AnythingOfType("*config.AgentConfig")).Return(errors.New("write error"))
			},
			wantErr:     true,
			errContains: "saving agent",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			repo := new(mocks.MockAgentStore)
			tt.setupMock(repo)
			svc := NewAgentService(repo, newTestLogger())

			result, err := svc.Create(context.Background(), tt.input)

			if tt.wantErr {
				assert.Error(t, err)
				if tt.errType != nil {
					assert.IsType(t, tt.errType, err)
				}
				if tt.errContains != "" {
					assert.Contains(t, err.Error(), tt.errContains)
				}
				assert.Nil(t, result)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.wantAgent, result)
			}
			repo.AssertExpectations(t)
		})
	}
}

func TestAgentService_Update(t *testing.T) {
	tests := []struct {
		name        string
		slug        string
		input       *config.AgentConfig
		setupMock   func(m *mocks.MockAgentStore)
		wantAgent   *config.AgentConfig
		wantErr     bool
		errType     interface{}
		errContains string
	}{
		{
			name: "updates agent successfully with defaults",
			slug: "my-agent",
			input: &config.AgentConfig{
				Name: "Updated Agent",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "my-agent").Return(&config.AgentConfig{
					Name: "My Agent", Slug: "my-agent", Model: "claude-sonnet-4-6",
				}, nil)
				m.On("Save", mock.AnythingOfType("*config.AgentConfig")).Return(nil)
			},
			wantAgent: &config.AgentConfig{
				Name:     "Updated Agent",
				Slug:     "my-agent",
				Model:    "claude-sonnet-4-6",
				Thinking: "adaptive",
			},
		},
		{
			name: "forces slug to path parameter value",
			slug: "original-slug",
			input: &config.AgentConfig{
				Name: "Agent",
				Slug: "different-slug",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "original-slug").Return(&config.AgentConfig{
					Name: "Agent", Slug: "original-slug",
				}, nil)
				m.On("Save", mock.AnythingOfType("*config.AgentConfig")).Return(nil)
			},
			wantAgent: &config.AgentConfig{
				Name:     "Agent",
				Slug:     "original-slug",
				Model:    "claude-sonnet-4-6",
				Thinking: "adaptive",
			},
		},
		{
			name: "preserves explicit model and thinking",
			slug: "my-agent",
			input: &config.AgentConfig{
				Name:     "Agent",
				Model:    "claude-opus-4-6",
				Thinking: "disabled",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "my-agent").Return(&config.AgentConfig{
					Name: "Agent", Slug: "my-agent",
				}, nil)
				m.On("Save", mock.AnythingOfType("*config.AgentConfig")).Return(nil)
			},
			wantAgent: &config.AgentConfig{
				Name:     "Agent",
				Slug:     "my-agent",
				Model:    "claude-opus-4-6",
				Thinking: "disabled",
			},
		},
		{
			name: "updates with bypass permission mode",
			slug: "my-agent",
			input: &config.AgentConfig{
				Name:           "Agent",
				PermissionMode: "bypass",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "my-agent").Return(&config.AgentConfig{
					Name: "Agent", Slug: "my-agent",
				}, nil)
				m.On("Save", mock.AnythingOfType("*config.AgentConfig")).Return(nil)
			},
			wantAgent: &config.AgentConfig{
				Name:           "Agent",
				Slug:           "my-agent",
				Model:          "claude-sonnet-4-6",
				Thinking:       "adaptive",
				PermissionMode: "bypass",
			},
		},
		{
			name: "fails when agent not found",
			slug: "nonexistent",
			input: &config.AgentConfig{
				Name: "Agent",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "nonexistent").Return(nil, nil)
			},
			wantErr: true,
			errType: &NotFoundError{},
		},
		{
			name: "fails when repo.Get returns error",
			slug: "my-agent",
			input: &config.AgentConfig{
				Name: "Agent",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "my-agent").Return(nil, errors.New("db error"))
			},
			wantErr:     true,
			errContains: "looking up agent",
		},
		{
			name: "fails when name is empty",
			slug: "my-agent",
			input: &config.AgentConfig{
				Name: "",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "my-agent").Return(&config.AgentConfig{
					Name: "Agent", Slug: "my-agent",
				}, nil)
			},
			wantErr: true,
			errType: &ValidationError{},
		},
		{
			name: "fails with invalid permission mode",
			slug: "my-agent",
			input: &config.AgentConfig{
				Name:           "Agent",
				PermissionMode: "admin",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "my-agent").Return(&config.AgentConfig{
					Name: "Agent", Slug: "my-agent",
				}, nil)
			},
			wantErr: true,
			errType: &ValidationError{},
		},
		{
			name: "fails when repo.Save returns error",
			slug: "my-agent",
			input: &config.AgentConfig{
				Name: "Agent",
			},
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Get", "my-agent").Return(&config.AgentConfig{
					Name: "Agent", Slug: "my-agent",
				}, nil)
				m.On("Save", mock.AnythingOfType("*config.AgentConfig")).Return(errors.New("write error"))
			},
			wantErr:     true,
			errContains: "saving agent",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			repo := new(mocks.MockAgentStore)
			tt.setupMock(repo)
			svc := NewAgentService(repo, newTestLogger())

			result, err := svc.Update(context.Background(), tt.slug, tt.input)

			if tt.wantErr {
				assert.Error(t, err)
				if tt.errType != nil {
					assert.IsType(t, tt.errType, err)
				}
				if tt.errContains != "" {
					assert.Contains(t, err.Error(), tt.errContains)
				}
				assert.Nil(t, result)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tt.wantAgent, result)
			}
			repo.AssertExpectations(t)
		})
	}
}

func TestAgentService_Delete(t *testing.T) {
	tests := []struct {
		name        string
		slug        string
		setupMock   func(m *mocks.MockAgentStore)
		wantErr     bool
		errContains string
	}{
		{
			name: "deletes successfully",
			slug: "my-agent",
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Delete", "my-agent").Return(nil)
			},
		},
		{
			name: "wraps repo error",
			slug: "bad-agent",
			setupMock: func(m *mocks.MockAgentStore) {
				m.On("Delete", "bad-agent").Return(errors.New("delete failed"))
			},
			wantErr:     true,
			errContains: "deleting agent",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			repo := new(mocks.MockAgentStore)
			tt.setupMock(repo)
			svc := NewAgentService(repo, newTestLogger())

			err := svc.Delete(context.Background(), tt.slug)

			if tt.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tt.errContains)
			} else {
				assert.NoError(t, err)
			}
			repo.AssertExpectations(t)
		})
	}
}
