package storage_test

import (
	"context"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/storage"
)

func TestSQLiteNotificationStore(t *testing.T) {
	db, _, err := storage.NewSQLiteDB(":memory:")
	require.NoError(t, err)
	defer db.Close()

	store := storage.NewSQLiteNotificationStore(db)
	ctx := context.Background()

	t.Run("log and list", func(t *testing.T) {
		entry := storage.NotificationLogEntry{
			EventType: "chat.completed",
			Provider:  "smtp",
			Subject:   "Agento Event: chat.completed",
			Status:    "sent",
			ErrorMsg:  "",
			CreatedAt: time.Now().UTC().Truncate(time.Second),
		}
		require.NoError(t, store.LogNotification(ctx, entry))

		list, err := store.ListNotifications(ctx, 10)
		require.NoError(t, err)
		require.Len(t, list, 1)

		got := list[0]
		assert.Equal(t, entry.EventType, got.EventType)
		assert.Equal(t, entry.Provider, got.Provider)
		assert.Equal(t, entry.Subject, got.Subject)
		assert.Equal(t, entry.Status, got.Status)
		assert.Equal(t, entry.ErrorMsg, got.ErrorMsg)
	})

	t.Run("failed status", func(t *testing.T) {
		entry := storage.NotificationLogEntry{
			EventType: "chat.started",
			Provider:  "smtp",
			Subject:   "Agento Event: chat.started",
			Status:    "failed",
			ErrorMsg:  "connection refused",
			CreatedAt: time.Now().UTC(),
		}
		require.NoError(t, store.LogNotification(ctx, entry))

		list, err := store.ListNotifications(ctx, 10)
		require.NoError(t, err)
		// Latest entry is first.
		assert.Equal(t, "failed", list[0].Status)
		assert.Equal(t, "connection refused", list[0].ErrorMsg)
	})

	t.Run("default limit", func(t *testing.T) {
		list, err := store.ListNotifications(ctx, 0)
		require.NoError(t, err)
		// Should apply default limit without error.
		assert.NotNil(t, list)
	})
}
