package api

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestLiveSessionStore(t *testing.T) {
	store := newLiveSessionStore()

	t.Run("put and get", func(t *testing.T) {
		ls := &liveSession{
			inputCh:          make(chan string, 1),
			permissionRespCh: make(chan bool, 1),
		}
		store.put("sess-1", ls)

		got, ok := store.get("sess-1")
		assert.True(t, ok)
		assert.Equal(t, ls, got)
	})

	t.Run("get missing", func(t *testing.T) {
		_, ok := store.get("missing")
		assert.False(t, ok)
	})

	t.Run("delete", func(t *testing.T) {
		store.put("sess-2", &liveSession{})
		store.delete("sess-2")

		_, ok := store.get("sess-2")
		assert.False(t, ok)
	})

	t.Run("delete nonexistent is no-op", func(t *testing.T) {
		store.delete("nonexistent") // should not panic
	})
}
