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

// --- preference tests ---

func boolPtr(b bool) *bool { return &b }

func TestHandle_ScheduledTaskFinished_DefaultEnabled(t *testing.T) {
	// OnFinished nil → default true → should attempt to send (will fail on dial, but log entry recorded)
	store := &stubStore{}
	loader := func() (*notification.NotificationSettings, error) {
		return &notification.NotificationSettings{
			Enabled: true,
			Provider: notification.SMTPConfig{
				Host:     "localhost",
				Port:     9999,
				FromAddr: "from@example.com",
				ToAddrs:  "to@example.com",
			},
			// Preferences.ScheduledTasks.OnFinished is nil → default enabled
		}, nil
	}
	h := notification.NewNotificationHandler(loader, store)
	h.Handle("tasks_scheduler.task_execution.finished", map[string]string{
		"Task Name": "My Task",
	})
	// A log entry should be recorded (even though SMTP dial fails, the attempt is logged).
	require.Len(t, store.entries, 1)
	assert.Equal(t, "tasks_scheduler.task_execution.finished", store.entries[0].EventType)
}

func TestHandle_ScheduledTaskFinished_ExplicitlyDisabled(t *testing.T) {
	store := &stubStore{}
	loader := func() (*notification.NotificationSettings, error) {
		return &notification.NotificationSettings{
			Enabled: true,
			Provider: notification.SMTPConfig{
				Host: "localhost", Port: 9999,
				FromAddr: "from@example.com", ToAddrs: "to@example.com",
			},
			Preferences: notification.NotificationPreferences{
				ScheduledTasks: notification.ScheduledTasksPreferences{
					OnFinished: boolPtr(false), // explicitly disabled
				},
			},
		}, nil
	}
	h := notification.NewNotificationHandler(loader, store)
	h.Handle("tasks_scheduler.task_execution.finished", map[string]string{
		"Task Name": "My Task",
	})
	// No log entry: preference disabled for this event type.
	assert.Empty(t, store.entries)
}

func TestHandle_ScheduledTaskFailed_DefaultEnabled(t *testing.T) {
	store := &stubStore{}
	loader := func() (*notification.NotificationSettings, error) {
		return &notification.NotificationSettings{
			Enabled: true,
			Provider: notification.SMTPConfig{
				Host:     "localhost",
				Port:     9999,
				FromAddr: "from@example.com",
				ToAddrs:  "to@example.com",
			},
		}, nil
	}
	h := notification.NewNotificationHandler(loader, store)
	h.Handle("tasks_scheduler.task_execution.failed", map[string]string{
		"Task Name": "My Task",
		"Error":     "timeout exceeded",
	})
	require.Len(t, store.entries, 1)
	assert.Equal(t, "tasks_scheduler.task_execution.failed", store.entries[0].EventType)
}

func TestHandle_ScheduledTaskFailed_ExplicitlyDisabled(t *testing.T) {
	store := &stubStore{}
	loader := func() (*notification.NotificationSettings, error) {
		return &notification.NotificationSettings{
			Enabled: true,
			Provider: notification.SMTPConfig{
				Host: "localhost", Port: 9999,
				FromAddr: "from@example.com", ToAddrs: "to@example.com",
			},
			Preferences: notification.NotificationPreferences{
				ScheduledTasks: notification.ScheduledTasksPreferences{
					OnFailed: boolPtr(false),
				},
			},
		}, nil
	}
	h := notification.NewNotificationHandler(loader, store)
	h.Handle("tasks_scheduler.task_execution.failed", map[string]string{
		"Task Name": "My Task",
	})
	assert.Empty(t, store.entries)
}

func TestHandle_ScheduledTaskFinished_ExplicitlyEnabled(t *testing.T) {
	store := &stubStore{}
	loader := func() (*notification.NotificationSettings, error) {
		return &notification.NotificationSettings{
			Enabled: true,
			Provider: notification.SMTPConfig{
				Host: "localhost", Port: 9999,
				FromAddr: "from@example.com", ToAddrs: "to@example.com",
			},
			Preferences: notification.NotificationPreferences{
				ScheduledTasks: notification.ScheduledTasksPreferences{
					OnFinished: boolPtr(true), // explicitly true
				},
			},
		}, nil
	}
	h := notification.NewNotificationHandler(loader, store)
	h.Handle("tasks_scheduler.task_execution.finished", map[string]string{"k": "v"})
	require.Len(t, store.entries, 1)
}

func TestHandle_UnknownEventType_AlwaysSends(t *testing.T) {
	// Unknown event types should not be filtered by preferences.
	store := &stubStore{}
	loader := func() (*notification.NotificationSettings, error) {
		return &notification.NotificationSettings{
			Enabled: true,
			Provider: notification.SMTPConfig{
				Host: "localhost", Port: 9999,
				FromAddr: "from@example.com", ToAddrs: "to@example.com",
			},
		}, nil
	}
	h := notification.NewNotificationHandler(loader, store)
	h.Handle("some.other.event", map[string]string{"foo": "bar"})
	require.Len(t, store.entries, 1)
}

// --- preference helper tests ---

func TestScheduledTasksPreferences_Defaults(t *testing.T) {
	p := notification.ScheduledTasksPreferences{}
	assert.True(t, p.IsOnFinishedEnabled(), "nil OnFinished should default to enabled")
	assert.True(t, p.IsOnFailedEnabled(), "nil OnFailed should default to enabled")
}

func TestScheduledTasksPreferences_ExplicitFalse(t *testing.T) {
	p := notification.ScheduledTasksPreferences{
		OnFinished: boolPtr(false),
		OnFailed:   boolPtr(false),
	}
	assert.False(t, p.IsOnFinishedEnabled())
	assert.False(t, p.IsOnFailedEnabled())
}

func TestScheduledTasksPreferences_ExplicitTrue(t *testing.T) {
	p := notification.ScheduledTasksPreferences{
		OnFinished: boolPtr(true),
		OnFailed:   boolPtr(true),
	}
	assert.True(t, p.IsOnFinishedEnabled())
	assert.True(t, p.IsOnFailedEnabled())
}
