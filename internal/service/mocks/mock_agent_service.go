package mocks

import (
	"context"

	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/config"
)

// MockAgentService is a mock implementation of service.AgentService.
type MockAgentService struct {
	mock.Mock
}

//nolint:revive
func (m *MockAgentService) List(ctx context.Context) ([]*config.AgentConfig, error) {
	args := m.Called(ctx)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*config.AgentConfig), args.Error(1)
}

//nolint:revive
func (m *MockAgentService) Get(ctx context.Context, slug string) (*config.AgentConfig, error) {
	args := m.Called(ctx, slug)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*config.AgentConfig), args.Error(1)
}

//nolint:revive
func (m *MockAgentService) Create(ctx context.Context, agent *config.AgentConfig) (*config.AgentConfig, error) {
	args := m.Called(ctx, agent)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*config.AgentConfig), args.Error(1)
}

//nolint:revive
func (m *MockAgentService) Update(ctx context.Context, slug string, agent *config.AgentConfig) (*config.AgentConfig, error) {
	args := m.Called(ctx, slug, agent)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*config.AgentConfig), args.Error(1)
}

//nolint:revive
func (m *MockAgentService) Delete(ctx context.Context, slug string) error {
	args := m.Called(ctx, slug)
	return args.Error(0)
}
