package mocks

import (
	"context"

	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/service"
)

// MockIntegrationService is a mock implementation of service.IntegrationService.
type MockIntegrationService struct {
	mock.Mock
}

//nolint:revive
func (m *MockIntegrationService) List(ctx context.Context) ([]*config.IntegrationConfig, error) {
	args := m.Called(ctx)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*config.IntegrationConfig), args.Error(1)
}

//nolint:revive
func (m *MockIntegrationService) Get(ctx context.Context, id string) (*config.IntegrationConfig, error) {
	args := m.Called(ctx, id)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*config.IntegrationConfig), args.Error(1)
}

//nolint:revive
func (m *MockIntegrationService) Create(ctx context.Context, cfg *config.IntegrationConfig) (*config.IntegrationConfig, error) {
	args := m.Called(ctx, cfg)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*config.IntegrationConfig), args.Error(1)
}

//nolint:revive
func (m *MockIntegrationService) Update(ctx context.Context, id string, cfg *config.IntegrationConfig) (*config.IntegrationConfig, error) {
	args := m.Called(ctx, id, cfg)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*config.IntegrationConfig), args.Error(1)
}

//nolint:revive
func (m *MockIntegrationService) Delete(ctx context.Context, id string) error {
	args := m.Called(ctx, id)
	return args.Error(0)
}

//nolint:revive
func (m *MockIntegrationService) StartOAuth(ctx context.Context, id string) (string, error) {
	args := m.Called(ctx, id)
	return args.String(0), args.Error(1)
}

//nolint:revive
func (m *MockIntegrationService) GetAuthStatus(ctx context.Context, id string) (bool, error) {
	args := m.Called(ctx, id)
	return args.Bool(0), args.Error(1)
}

//nolint:revive
func (m *MockIntegrationService) AvailableTools(ctx context.Context) ([]service.AvailableTool, error) {
	args := m.Called(ctx)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]service.AvailableTool), args.Error(1)
}
