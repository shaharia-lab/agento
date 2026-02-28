package eventbus

import "time"

// Event represents an application event published to the bus.
type Event struct {
	Type      string            `json:"type"`
	Timestamp time.Time         `json:"timestamp"`
	Payload   map[string]string `json:"payload"`
}

// Listener is a function that handles an event.
type Listener func(Event)
