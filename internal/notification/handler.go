package notification

import (
	"context"
	"fmt"
	"log"
	"strings"
	"time"

	"github.com/shaharia-lab/agento/internal/storage"
)

// SettingsLoader is a function that loads the current notification settings.
// It is called on every event so that configuration changes take effect
// without requiring a restart.
type SettingsLoader func() (*NotificationSettings, error)

// NotificationHandler receives application events and delivers notifications
// according to the current notification settings.
// The name is intentional: it provides clarity when referenced as notification.NotificationHandler.
//
//nolint:revive
type NotificationHandler struct {
	settingsLoader SettingsLoader
	store          storage.NotificationStore
}

// NewNotificationHandler creates a new NotificationHandler.
func NewNotificationHandler(loader SettingsLoader, store storage.NotificationStore) *NotificationHandler {
	return &NotificationHandler{settingsLoader: loader, store: store}
}

// Handle processes an event: loads settings, builds the message, calls the
// SMTP provider, and logs the outcome.
func (h *NotificationHandler) Handle(eventType string, payload map[string]string) {
	settings, err := h.settingsLoader()
	if err != nil {
		log.Printf("notification: failed to load settings: %v", err)
		return
	}
	if !settings.Enabled {
		return
	}

	provider := NewSMTPProvider(settings.Provider)
	subject := fmt.Sprintf("Agento Event: %s", eventType)

	bodyParts := make([]string, 0, len(payload))
	for k, v := range payload {
		bodyParts = append(bodyParts, fmt.Sprintf("%s: %s", k, v))
	}
	body := strings.Join(bodyParts, "\n")

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	sendErr := provider.Send(ctx, Message{Subject: subject, Body: body})

	entry := storage.NotificationLogEntry{
		EventType: eventType,
		Provider:  provider.Name(),
		Subject:   subject,
		Status:    "sent",
		CreatedAt: time.Now(),
	}
	if sendErr != nil {
		entry.Status = "failed"
		entry.ErrorMsg = sendErr.Error()
		log.Printf("notification: failed to send for event %q: %v", eventType, sendErr)
	}

	if logErr := h.store.LogNotification(context.Background(), entry); logErr != nil {
		log.Printf("notification: failed to log delivery for event %q: %v", eventType, logErr)
	}
}
