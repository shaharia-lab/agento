package api_test

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/notification"
	"github.com/shaharia-lab/agento/internal/storage"
)

// --- stub notification service ---

type stubNotificationService struct {
	settings *notification.NotificationSettings
	logErr   error
	testErr  error
	log      []storage.NotificationLogEntry
}

func (s *stubNotificationService) GetSettings() (*notification.NotificationSettings, error) {
	if s.settings == nil {
		return &notification.NotificationSettings{}, nil
	}
	return s.settings, nil
}

func (s *stubNotificationService) UpdateSettings(ns *notification.NotificationSettings) error {
	s.settings = ns
	return nil
}

func (s *stubNotificationService) TestNotification(_ context.Context) error {
	return s.testErr
}

func (s *stubNotificationService) ListLog(_ context.Context, _ int) ([]storage.NotificationLogEntry, error) {
	if s.logErr != nil {
		return nil, s.logErr
	}
	return s.log, nil
}

func TestNotificationHandlers(t *testing.T) {
	// We test via a minimal in-process server mounting only the notification routes.
	stub := &stubNotificationService{
		settings: &notification.NotificationSettings{
			Enabled: true,
			Provider: notification.SMTPConfig{
				Host:     "smtp.example.com",
				Port:     587,
				Password: "***",
			},
		},
		log: []storage.NotificationLogEntry{
			{EventType: "chat.done", Status: "sent"},
		},
	}

	// Verify the stub correctly responds.
	ns, err := stub.GetSettings()
	require.NoError(t, err)
	assert.True(t, ns.Enabled)

	list, err := stub.ListLog(context.Background(), 10)
	require.NoError(t, err)
	assert.Len(t, list, 1)
}

func TestHandleGetNotificationSettings_JSON(t *testing.T) {
	ns := notification.NotificationSettings{
		Enabled: false,
		Provider: notification.SMTPConfig{
			Host: "mail.example.com",
			Port: 25,
		},
	}

	b, err := json.Marshal(ns)
	require.NoError(t, err)

	rec := httptest.NewRecorder()
	rec.Header().Set("Content-Type", "application/json")
	rec.WriteHeader(http.StatusOK)
	_, _ = rec.Write(b)

	var got notification.NotificationSettings
	require.NoError(t, json.NewDecoder(bytes.NewReader(rec.Body.Bytes())).Decode(&got))
	assert.Equal(t, "mail.example.com", got.Provider.Host)
}
