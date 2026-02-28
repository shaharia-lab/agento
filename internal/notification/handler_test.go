package notification_test

import (
	"context"
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/notification"
	"github.com/shaharia-lab/agento/internal/storage"
)

// --- stub store ---

type stubStore struct {
	entries []storage.NotificationLogEntry
	err     error
}

func (s *stubStore) LogNotification(_ context.Context, entry storage.NotificationLogEntry) error {
	if s.err != nil {
		return s.err
	}
	s.entries = append(s.entries, entry)
	return nil
}

func (s *stubStore) ListNotifications(_ context.Context, _ int) ([]storage.NotificationLogEntry, error) {
	return s.entries, nil
}

// --- tests ---

func TestHandle_NotificationsDisabled(t *testing.T) {
	store := &stubStore{}
	loader := func() (*notification.NotificationSettings, error) {
		return &notification.NotificationSettings{Enabled: false}, nil
	}
	h := notification.NewNotificationHandler(loader, store)
	h.Handle("test.event", map[string]string{"foo": "bar"})

	// Nothing should be logged when notifications are disabled.
	assert.Empty(t, store.entries)
}

func TestHandle_LoaderError(t *testing.T) {
	store := &stubStore{}
	loader := func() (*notification.NotificationSettings, error) {
		return nil, errors.New("load failure")
	}
	h := notification.NewNotificationHandler(loader, store)
	// Should not panic; just log.
	h.Handle("test.event", map[string]string{})
	assert.Empty(t, store.entries)
}

func TestHandle_LogStoreError(t *testing.T) {
	// Even if the store fails to log, the handler should not panic.
	store := &stubStore{err: errors.New("db error")}
	loader := func() (*notification.NotificationSettings, error) {
		// Use an empty SMTP config — Send will fail, but that path is tested separately.
		return &notification.NotificationSettings{
			Enabled: true,
			Provider: notification.SMTPConfig{
				Host:     "localhost",
				Port:     9999, // will fail to connect
				FromAddr: "from@example.com",
				ToAddrs:  "to@example.com",
			},
		}, nil
	}
	h := notification.NewNotificationHandler(loader, store)
	// Should not panic even though both Send and LogNotification fail.
	h.Handle("test.event", map[string]string{"key": "val"})
}

func TestNotificationLogEntry_Fields(t *testing.T) {
	entry := storage.NotificationLogEntry{
		EventType: "chat.completed",
		Provider:  "smtp",
		Status:    "sent",
	}
	require.Equal(t, "chat.completed", entry.EventType)
	require.Equal(t, "smtp", entry.Provider)
	require.Equal(t, "sent", entry.Status)
}
