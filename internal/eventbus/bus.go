// Package eventbus provides an in-memory, asynchronous event bus for the application.
// Events are dispatched through a buffered channel and processed by a worker pool.
package eventbus

import (
	"log"
	"sync"
	"time"
)

const (
	defaultWorkers    = 3
	defaultBufferSize = 100
)

// EventBus is the interface for publishing events and managing subscribers.
type EventBus interface {
	// Publish enqueues an event with the given type and payload.
	// It never blocks: if the buffer is full, the event is dropped and a warning is logged.
	Publish(eventType string, payload map[string]string)

	// Subscribe registers a listener that will be called for every published event.
	// All listeners are invoked for each event (broadcast). Subscribe must be called
	// before the first Publish; behavior is undefined if called after Close.
	Subscribe(listener Listener)

	// Close stops accepting new events and waits for all pending events to be processed.
	Close()
}

// inMemoryBus is the default EventBus implementation.
type inMemoryBus struct {
	ch        chan Event
	listeners []Listener
	mu        sync.RWMutex
	wg        sync.WaitGroup
	workers   int
}

// New creates a new in-memory EventBus with the specified number of worker goroutines.
// If workers is <= 0, defaultWorkers (3) is used.
func New(workers int) EventBus {
	if workers <= 0 {
		workers = defaultWorkers
	}
	b := &inMemoryBus{
		ch:      make(chan Event, defaultBufferSize),
		workers: workers,
	}
	b.startWorkers()
	return b
}

// startWorkers launches the worker goroutines that process events from the channel.
func (b *inMemoryBus) startWorkers() {
	for i := 0; i < b.workers; i++ {
		b.wg.Add(1)
		go func() {
			defer b.wg.Done()
			for e := range b.ch {
				b.dispatch(e)
			}
		}()
	}
}

// dispatch calls all registered listeners for the given event.
// Each listener is invoked with panic recovery to prevent one bad listener
// from affecting others.
func (b *inMemoryBus) dispatch(e Event) {
	b.mu.RLock()
	listeners := make([]Listener, len(b.listeners))
	copy(listeners, b.listeners)
	b.mu.RUnlock()

	for _, l := range listeners {
		func() {
			defer func() {
				if r := recover(); r != nil {
					log.Printf("eventbus: listener panicked for event %q: %v", e.Type, r)
				}
			}()
			l(e)
		}()
	}
}

// Publish enqueues an event. If the buffer is full the event is dropped.
func (b *inMemoryBus) Publish(eventType string, payload map[string]string) {
	e := Event{
		Type:      eventType,
		Timestamp: time.Now(),
		Payload:   payload,
	}

	select {
	case b.ch <- e:
		// enqueued successfully
	default:
		log.Printf("eventbus: buffer full, dropping event %q", eventType)
	}
}

// Subscribe adds a listener to receive all future events.
func (b *inMemoryBus) Subscribe(listener Listener) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.listeners = append(b.listeners, listener)
}

// Close drains and closes the event channel, then waits for all workers to finish.
func (b *inMemoryBus) Close() {
	close(b.ch)
	b.wg.Wait()
}
