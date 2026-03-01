package storage

import (
	"context"
	"database/sql"
	"fmt"
)

// SQLiteNotificationStore implements NotificationStore backed by SQLite.
type SQLiteNotificationStore struct {
	db *sql.DB
}

// NewSQLiteNotificationStore returns a new SQLiteNotificationStore.
func NewSQLiteNotificationStore(db *sql.DB) *SQLiteNotificationStore {
	return &SQLiteNotificationStore{db: db}
}

// LogNotification inserts a notification delivery record into the database.
func (s *SQLiteNotificationStore) LogNotification(ctx context.Context, entry NotificationLogEntry) error {
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO notification_log (event_type, provider, subject, status, error_msg, created_at)
		VALUES (?, ?, ?, ?, ?, ?)`,
		entry.EventType, entry.Provider, entry.Subject,
		entry.Status, entry.ErrorMsg, entry.CreatedAt,
	)
	if err != nil {
		return fmt.Errorf("inserting notification log: %w", err)
	}
	return nil
}

// ListNotifications returns the most recent log entries ordered by created_at descending.
func (s *SQLiteNotificationStore) ListNotifications(ctx context.Context, limit int) ([]NotificationLogEntry, error) {
	if limit <= 0 {
		limit = 50
	}
	rows, err := s.db.QueryContext(ctx, `
		SELECT id, event_type, provider, subject, status, error_msg, created_at
		FROM notification_log
		ORDER BY created_at DESC
		LIMIT ?`, limit)
	if err != nil {
		return nil, fmt.Errorf("querying notification log: %w", err)
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			err = fmt.Errorf("closing rows: %w", cerr)
		}
	}()

	var entries []NotificationLogEntry
	for rows.Next() {
		var e NotificationLogEntry
		if err := rows.Scan(&e.ID, &e.EventType, &e.Provider, &e.Subject,
			&e.Status, &e.ErrorMsg, &e.CreatedAt); err != nil {
			return nil, fmt.Errorf("scanning notification log row: %w", err)
		}
		entries = append(entries, e)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating notification log rows: %w", err)
	}
	return entries, nil
}
