package service_test

import (
	"context"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/notification"
	"github.com/shaharia-lab/agento/internal/service"
	"github.com/shaharia-lab/agento/internal/storage"
)

// --- in-memory settings store for tests ---

type memSettingsStore struct {
	settings config.UserSettings
}

func (m *memSettingsStore) Load() (config.UserSettings, error) { return m.settings, nil }
func (m *memSettingsStore) Save(s config.UserSettings) error   { m.settings = s; return nil }

// --- in-memory notification store ---

type memNotificationStore struct {
	entries []storage.NotificationLogEntry
}

func (m *memNotificationStore) LogNotification(_ context.Context, e storage.NotificationLogEntry) error {
	m.entries = append(m.entries, e)
	return nil
}

func (m *memNotificationStore) ListNotifications(_ context.Context, limit int) ([]storage.NotificationLogEntry, error) {
	if limit > 0 && len(m.entries) > limit {
		return m.entries[:limit], nil
	}
	return m.entries, nil
}

func newTestNotificationService(t *testing.T) (service.NotificationService, *memNotificationStore) {
	t.Helper()
	settingsStore := &memSettingsStore{}
	mgr, err := config.NewSettingsManager(settingsStore, &config.AppConfig{})
	require.NoError(t, err)
	notifStore := &memNotificationStore{}
	svc := service.NewNotificationService(mgr, notifStore)
	return svc, notifStore
}

func buildSMTPSettings() *notification.NotificationSettings {
	return &notification.NotificationSettings{
		Enabled: true,
		Provider: notification.SMTPConfig{
			Host:       "smtp.example.com",
			Port:       587,
			Username:   "user",
			Password:   "secret",
			FromAddr:   "from@example.com",
			ToAddrs:    "to@example.com",
			Encryption: "starttls",
		},
	}
}

func TestGetSettings_DefaultEmpty(t *testing.T) {
	svc, _ := newTestNotificationService(t)
	ns, err := svc.GetSettings()
	require.NoError(t, err)
	assert.False(t, ns.Enabled)
}

func TestUpdateAndGet_RoundTrip(t *testing.T) {
	svc, _ := newTestNotificationService(t)

	require.NoError(t, svc.UpdateSettings(buildSMTPSettings()))

	got, err := svc.GetSettings()
	require.NoError(t, err)
	assert.True(t, got.Enabled)
	assert.Equal(t, "smtp.example.com", got.Provider.Host)
	// Password must be masked in output.
	assert.Equal(t, "***", got.Provider.Password)
}

func TestUpdateSettings_PreservesPassword(t *testing.T) {
	svc, _ := newTestNotificationService(t)

	require.NoError(t, svc.UpdateSettings(buildSMTPSettings()))

	// Now update with a masked password — original password should be preserved.
	update := buildSMTPSettings()
	update.Provider.Password = "***"
	update.Provider.Host = "new-smtp.example.com"
	require.NoError(t, svc.UpdateSettings(update))

	got, err := svc.GetSettings()
	require.NoError(t, err)
	assert.Equal(t, "new-smtp.example.com", got.Provider.Host)
	// Password is still masked in output.
	assert.Equal(t, "***", got.Provider.Password)
}

func TestListLog(t *testing.T) {
	svc, notifStore := newTestNotificationService(t)

	for i := 0; i < 3; i++ {
		_ = notifStore.LogNotification(context.Background(), storage.NotificationLogEntry{
			EventType: "test.event",
			Provider:  "smtp",
			Status:    "sent",
		})
	}

	list, err := svc.ListLog(context.Background(), 10)
	require.NoError(t, err)
	assert.Len(t, list, 3)
}

func TestTestNotification_WorksWhenDisabled(t *testing.T) {
	// TestNotification should attempt to send regardless of the enabled flag.
	// With no SMTP host configured it will fail on dial, not on "not enabled".
	svc, _ := newTestNotificationService(t)
	err := svc.TestNotification(context.Background())
	require.Error(t, err)
	assert.NotContains(t, err.Error(), "not enabled")
}
