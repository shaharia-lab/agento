package mocks

import (
	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/config"
)

// MockIntegrationStore is a mock implementation of storage.IntegrationStore.
type MockIntegrationStore struct {
	mock.Mock
}

//nolint:revive
func (m *MockIntegrationStore) List() ([]*config.IntegrationConfig, error) {
	args := m.Called()
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*config.IntegrationConfig), args.Error(1)
}

//nolint:revive
func (m *MockIntegrationStore) Get(id string) (*config.IntegrationConfig, error) {
	args := m.Called(id)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*config.IntegrationConfig), args.Error(1)
}

//nolint:revive
func (m *MockIntegrationStore) Save(cfg *config.IntegrationConfig) error {
	args := m.Called(cfg)
	return args.Error(0)
}

//nolint:revive
func (m *MockIntegrationStore) Delete(id string) error {
	args := m.Called(id)
	return args.Error(0)
}
