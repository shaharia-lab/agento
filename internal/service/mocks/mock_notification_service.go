package mocks

import (
	"context"

	"github.com/stretchr/testify/mock"

	"github.com/shaharia-lab/agento/internal/notification"
	"github.com/shaharia-lab/agento/internal/storage"
)

// MockNotificationService is a mock implementation of service.NotificationService.
type MockNotificationService struct {
	mock.Mock
}

//nolint:revive
func (m *MockNotificationService) GetSettings() (*notification.NotificationSettings, error) {
	args := m.Called()
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).(*notification.NotificationSettings), args.Error(1)
}

//nolint:revive
func (m *MockNotificationService) UpdateSettings(settings *notification.NotificationSettings) error {
	args := m.Called(settings)
	return args.Error(0)
}

//nolint:revive
func (m *MockNotificationService) TestNotification(ctx context.Context) error {
	args := m.Called(ctx)
	return args.Error(0)
}

//nolint:revive
func (m *MockNotificationService) ListLog(ctx context.Context, limit int) ([]storage.NotificationLogEntry, error) {
	args := m.Called(ctx, limit)
	if args.Get(0) == nil {
		return nil, args.Error(1)
	}
	return args.Get(0).([]storage.NotificationLogEntry), args.Error(1)
}
