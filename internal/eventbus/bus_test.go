package eventbus_test

import (
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/eventbus"
)

func TestPublishAndReceive(t *testing.T) {
	bus := eventbus.New(2)
	defer bus.Close()

	var received []eventbus.Event
	var mu sync.Mutex

	bus.Subscribe(func(e eventbus.Event) {
		mu.Lock()
		received = append(received, e)
		mu.Unlock()
	})

	bus.Publish("test.event", map[string]string{"key": "value"})

	// Give workers time to process
	time.Sleep(50 * time.Millisecond)

	mu.Lock()
	defer mu.Unlock()
	require.Len(t, received, 1)
	assert.Equal(t, "test.event", received[0].Type)
	assert.Equal(t, "value", received[0].Payload["key"])
	assert.False(t, received[0].Timestamp.IsZero())
}

func TestMultipleListeners(t *testing.T) {
	bus := eventbus.New(2)
	defer bus.Close()

	var count int32

	for i := 0; i < 3; i++ {
		bus.Subscribe(func(_ eventbus.Event) {
			atomic.AddInt32(&count, 1)
		})
	}

	bus.Publish("multi", nil)
	time.Sleep(50 * time.Millisecond)

	assert.EqualValues(t, 3, atomic.LoadInt32(&count))
}

func TestListenerPanicDoesNotCrash(t *testing.T) {
	bus := eventbus.New(1)
	defer bus.Close()

	var goodCalled int32

	bus.Subscribe(func(_ eventbus.Event) {
		panic("intentional panic in listener")
	})
	bus.Subscribe(func(_ eventbus.Event) {
		atomic.AddInt32(&goodCalled, 1)
	})

	bus.Publish("panic.event", nil)
	time.Sleep(50 * time.Millisecond)

	// The second listener should still have been called.
	assert.EqualValues(t, 1, atomic.LoadInt32(&goodCalled))
}

func TestClose(t *testing.T) {
	bus := eventbus.New(2)

	var count int32
	bus.Subscribe(func(_ eventbus.Event) {
		atomic.AddInt32(&count, 1)
	})

	for i := 0; i < 5; i++ {
		bus.Publish("evt", nil)
	}

	// Close waits for all workers to finish processing.
	bus.Close()

	assert.EqualValues(t, 5, atomic.LoadInt32(&count))
}

func TestDefaultWorkers(t *testing.T) {
	// workers <= 0 should use default without panicking.
	bus := eventbus.New(0)
	require.NotNil(t, bus)
	bus.Close()
}
