package service

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/notification"
	"github.com/shaharia-lab/agento/internal/storage"
)

const maskedPassword = "***"

// NotificationService manages notification settings and log access.
type NotificationService interface {
	// GetSettings returns the current notification settings. The SMTP password is masked.
	GetSettings() (*notification.NotificationSettings, error)
	// UpdateSettings persists new notification settings.
	// If the password field is the mask sentinel, the existing password is preserved.
	UpdateSettings(settings *notification.NotificationSettings) error
	// TestNotification sends a test notification using the current settings.
	TestNotification(ctx context.Context) error
	// ListLog returns the most recent notification log entries.
	ListLog(ctx context.Context, limit int) ([]storage.NotificationLogEntry, error)
}

// notificationServiceImpl implements NotificationService.
type notificationServiceImpl struct {
	settingsMgr *config.SettingsManager
	store       storage.NotificationStore
}

// NewNotificationService creates a new NotificationService.
func NewNotificationService(
	settingsMgr *config.SettingsManager,
	store storage.NotificationStore,
) NotificationService {
	return &notificationServiceImpl{
		settingsMgr: settingsMgr,
		store:       store,
	}
}

// loadNotificationSettings unmarshals the JSON-encoded notification settings from
// the user settings row, returning a pointer to the parsed NotificationSettings.
func (s *notificationServiceImpl) loadNotificationSettings() (*notification.NotificationSettings, error) {
	us := s.settingsMgr.Get()
	raw := us.NotificationSettings
	if raw == "" || raw == "{}" {
		return &notification.NotificationSettings{}, nil
	}

	var ns notification.NotificationSettings
	if err := json.Unmarshal([]byte(raw), &ns); err != nil {
		return nil, fmt.Errorf("parsing notification settings: %w", err)
	}
	return &ns, nil
}

// GetSettings returns the current notification settings with the SMTP password masked.
func (s *notificationServiceImpl) GetSettings() (*notification.NotificationSettings, error) {
	ns, err := s.loadNotificationSettings()
	if err != nil {
		return nil, err
	}
	// Mask the password before returning.
	if ns.Provider.Password != "" {
		ns.Provider.Password = maskedPassword
	}
	return ns, nil
}

// UpdateSettings saves the notification settings. If the incoming password is the
// mask sentinel, the previously stored password is preserved.
func (s *notificationServiceImpl) UpdateSettings(incoming *notification.NotificationSettings) error {
	// If password is masked, reload the existing password.
	if incoming.Provider.Password == maskedPassword {
		existing, err := s.loadNotificationSettings()
		if err != nil {
			return fmt.Errorf("loading existing settings: %w", err)
		}
		incoming.Provider.Password = existing.Provider.Password
	}

	raw, err := json.Marshal(incoming)
	if err != nil {
		return fmt.Errorf("encoding notification settings: %w", err)
	}

	us := s.settingsMgr.Get()
	us.NotificationSettings = string(raw)
	if err := s.settingsMgr.Update(us); err != nil {
		return fmt.Errorf("saving settings: %w", err)
	}
	return nil
}

// TestNotification sends a test email using the current SMTP config regardless
// of whether notifications are enabled. This lets users verify credentials
// before committing to enabling notifications.
func (s *notificationServiceImpl) TestNotification(ctx context.Context) error {
	ns, err := s.loadNotificationSettings()
	if err != nil {
		return err
	}

	provider := notification.NewSMTPProvider(ns.Provider)
	return provider.Send(ctx, notification.Message{
		Subject: notification.SubjectPrefix + "Test Notification",
		Body:    "This is a test notification from Agento.\n\nYour SMTP configuration is working correctly.",
	})
}

// ListLog returns the most recent notification log entries.
func (s *notificationServiceImpl) ListLog(ctx context.Context, limit int) ([]storage.NotificationLogEntry, error) {
	return s.store.ListNotifications(ctx, limit)
}
