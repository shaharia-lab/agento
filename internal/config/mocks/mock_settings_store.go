package mocks

import (
	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/config"
)

// MockSettingsStore is a mock implementation of config.SettingsStore.
type MockSettingsStore struct {
	mock.Mock
}

//nolint:revive
func (m *MockSettingsStore) Load() (config.UserSettings, error) {
	args := m.Called()
	return args.Get(0).(config.UserSettings), args.Error(1)
}

//nolint:revive
func (m *MockSettingsStore) Save(settings config.UserSettings) error {
	args := m.Called(settings)
	return args.Error(0)
}
