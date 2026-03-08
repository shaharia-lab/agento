// Package whatsapp provides WhatsApp integration via the whatsmeow library.
// It implements the linked-device protocol (multi-device) for sending/receiving
// messages through a personal WhatsApp account.
package whatsapp

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"

	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/store/sqlstore"
	waLog "go.mau.fi/whatsmeow/util/log"

	_ "modernc.org/sqlite" // pure-Go SQLite driver for whatsmeow store
)

// Client wraps a whatsmeow.Client with lifecycle management.
// whatsmeow.Client is documented as goroutine-safe, so no mutex is needed.
type Client struct {
	wm     *whatsmeow.Client
	store  *sqlstore.Container
	logger *slog.Logger
}

// NewClient creates a new WhatsApp client backed by a SQLite device store
// at dataDir/whatsapp_<integrationID>.db.
func NewClient(ctx context.Context, dataDir, integrationID string, logger *slog.Logger) (*Client, error) {
	dbPath := filepath.Join(dataDir, fmt.Sprintf("whatsapp_%s.db", integrationID))

	// Ensure the data directory exists.
	if err := os.MkdirAll(dataDir, 0750); err != nil {
		return nil, fmt.Errorf("creating data directory: %w", err)
	}

	dsn := fmt.Sprintf("file:%s?_pragma=journal_mode(WAL)&_pragma=foreign_keys(ON)", dbPath)
	container, err := sqlstore.New(ctx, "sqlite", dsn, waLog.Noop)
	if err != nil {
		return nil, fmt.Errorf("creating whatsmeow store at %s: %w", dbPath, err)
	}

	deviceStore, err := container.GetFirstDevice(ctx)
	if err != nil {
		return nil, fmt.Errorf("getting device store: %w", err)
	}

	wm := whatsmeow.NewClient(deviceStore, waLog.Noop)

	return &Client{
		wm:     wm,
		store:  container,
		logger: logger,
	}, nil
}

// Connect establishes the WebSocket connection to WhatsApp servers.
// If the device is already paired, it reconnects automatically.
func (c *Client) Connect() error {
	return c.wm.Connect()
}

// Disconnect gracefully closes the WhatsApp connection and the underlying
// SQLite device store so that no database connections are leaked.
func (c *Client) Disconnect() {
	c.wm.Disconnect()
	if c.store != nil {
		if err := c.store.Close(); err != nil {
			c.logger.Error("failed to close whatsmeow store", "error", err)
		}
	}
}

// IsConnected returns true if the client has an active connection.
func (c *Client) IsConnected() bool {
	return c.wm.IsConnected()
}

// IsLoggedIn returns true if the device store has a valid session.
func (c *Client) IsLoggedIn() bool {
	return c.wm.IsLoggedIn()
}

// WM returns the underlying whatsmeow client for direct access by tools and pairing.
func (c *Client) WM() *whatsmeow.Client {
	return c.wm
}
