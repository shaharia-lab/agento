package storage

import (
	"context"
	"time"
)

// NotificationLogEntry records a single notification delivery attempt.
type NotificationLogEntry struct {
	ID        int64     `json:"id"`
	EventType string    `json:"event_type"`
	Provider  string    `json:"provider"`
	Subject   string    `json:"subject"`
	Status    string    `json:"status"`
	ErrorMsg  string    `json:"error_msg"`
	CreatedAt time.Time `json:"created_at"`
}

// NotificationStore defines the interface for persisting notification delivery logs.
type NotificationStore interface {
	// LogNotification records a notification delivery attempt.
	LogNotification(ctx context.Context, entry NotificationLogEntry) error
	// ListNotifications returns the most recent notification log entries, up to limit.
	ListNotifications(ctx context.Context, limit int) ([]NotificationLogEntry, error)
}
