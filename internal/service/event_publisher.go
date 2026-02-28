package service

// EventPublisher is the interface for publishing application events.
// Services use this interface to emit events without depending on a concrete
// event bus implementation.
type EventPublisher interface {
	Publish(eventType string, payload map[string]string)
}
