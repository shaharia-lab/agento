package mocks

import (
	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/config"
)

// MockAgentStore is a mock implementation of storage.AgentStore.
type MockAgentStore struct {
	mock.Mock
}

//nolint:revive
func (m *MockAgentStore) List() ([]*config.AgentConfig, error) {
	args := m.Called()
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]*config.AgentConfig), args.Error(1)
}

//nolint:revive
func (m *MockAgentStore) Get(slug string) (*config.AgentConfig, error) {
	args := m.Called(slug)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*config.AgentConfig), args.Error(1)
}

//nolint:revive
func (m *MockAgentStore) Save(agent *config.AgentConfig) error {
	args := m.Called(agent)
	return args.Error(0)
}

//nolint:revive
func (m *MockAgentStore) Delete(slug string) error {
	args := m.Called(slug)
	return args.Error(0)
}
