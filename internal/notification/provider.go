// Package notification provides an abstraction for sending notifications
// (currently email via SMTP) and a handler that dispatches events to providers.
package notification

import "context"

// Message is the content to be delivered by a Provider.
type Message struct {
	Subject string
	Body    string
	To      []string
}

// Provider is the interface for notification delivery backends.
type Provider interface {
	// Name returns the provider identifier (e.g. "smtp").
	Name() string
	// Send delivers the message using the provider's transport.
	Send(ctx context.Context, msg Message) error
}
