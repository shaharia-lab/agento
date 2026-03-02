// Package messaging provides a generic abstraction for bidirectional messaging
// integrations (Telegram, Slack, etc.). It decouples the messaging platform
// specifics from the agent conversation lifecycle so new platforms can be added
// by implementing the Platform interface.
package messaging

import (
	"context"
	"net/http"
	"time"
)

// Platform represents a bidirectional messaging integration that can receive
// inbound messages from users and send outbound responses. Each concrete
// adapter (Telegram, Slack, …) implements this interface.
type Platform interface {
	// ID returns the unique identifier of this platform instance (typically
	// the integration ID from the database).
	ID() string

	// Type returns the platform type (e.g. "telegram", "slack").
	Type() string

	// Start initialises the platform adapter (e.g. sets up webhook or
	// long-polling). The provided context controls the adapter's lifetime.
	Start(ctx context.Context) error

	// Stop gracefully shuts down the adapter.
	Stop() error

	// SendMessage sends a text response to the specified channel on the platform.
	SendMessage(ctx context.Context, msg OutboundMessage) error

	// SendTypingIndicator shows a "typing…" status in the given channel.
	// Implementations may no-op if the platform does not support this.
	SendTypingIndicator(ctx context.Context, channelID string) error

	// HandleWebhook processes an inbound HTTP webhook from the platform.
	// The adapter is responsible for parsing the request, extracting the
	// message, and forwarding it via the MessageHandler.
	HandleWebhook(w http.ResponseWriter, r *http.Request)
}

// MessageHandler is the callback that Platform adapters invoke when an inbound
// message arrives. Typically implemented by the Dispatcher.
type MessageHandler interface {
	HandleMessage(ctx context.Context, msg InboundMessage) error
}

// MessageHandlerFunc is a convenience adapter that turns a plain function into
// a MessageHandler.
type MessageHandlerFunc func(ctx context.Context, msg InboundMessage) error

// HandleMessage implements MessageHandler.
func (f MessageHandlerFunc) HandleMessage(ctx context.Context, msg InboundMessage) error {
	return f(ctx, msg)
}

// InboundMessage is a platform-agnostic representation of a message received
// from an external messaging service.
type InboundMessage struct {
	// PlatformType identifies the source platform ("telegram", "slack", …).
	PlatformType string

	// PlatformID is the integration instance ID that received the message.
	PlatformID string

	// ChannelID is the platform-specific conversation identifier
	// (e.g. Telegram chat_id, Slack channel ID).
	ChannelID string

	// MessageID is the platform-specific unique message identifier.
	MessageID string

	// UserID is the platform-specific sender identifier.
	UserID string

	// UserDisplayName is the human-readable name of the sender.
	UserDisplayName string

	// Content is the text body of the message.
	Content string

	// Timestamp is when the message was sent on the platform.
	Timestamp time.Time

	// Metadata carries platform-specific extra data that does not fit in
	// the standard fields (e.g. Telegram update_id, Slack thread_ts).
	Metadata map[string]string
}

// OutboundMessage is a platform-agnostic representation of a message to be
// sent to an external messaging service.
type OutboundMessage struct {
	// ChannelID is the platform-specific conversation identifier.
	ChannelID string

	// Content is the text body to send. Platforms may apply their own
	// formatting rules (e.g. Markdown for Telegram, mrkdwn for Slack).
	Content string

	// ReplyToMessageID optionally references the platform-specific message
	// ID that this message is a reply to.
	ReplyToMessageID string

	// Metadata carries platform-specific options (e.g. parse_mode for Telegram).
	Metadata map[string]string
}

// PlatformFactory creates a Platform adapter from an integration config.
// The messageHandler is wired by the Manager so the adapter can forward
// inbound messages to the Dispatcher.
type PlatformFactory func(integrationID string, credentials map[string]string, handler MessageHandler) (Platform, error)
