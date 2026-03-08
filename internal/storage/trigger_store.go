package storage

import (
	"context"

	"github.com/shaharia-lab/agento/internal/config"
)

// TriggerStore defines the persistence interface for trigger rules and Telegram update deduplication.
type TriggerStore interface {
	// ListRules returns all trigger rules for the given integration, ordered by creation time.
	ListRules(ctx context.Context, integrationID string) ([]*config.TriggerRule, error)

	// GetRule returns a single trigger rule by ID, or nil if not found.
	GetRule(ctx context.Context, id string) (*config.TriggerRule, error)

	// CreateRule inserts a new trigger rule.
	CreateRule(ctx context.Context, rule *config.TriggerRule) error

	// UpdateRule persists changes to an existing trigger rule.
	UpdateRule(ctx context.Context, rule *config.TriggerRule) error

	// DeleteRule removes a trigger rule by ID.
	DeleteRule(ctx context.Context, id string) error

	// DeleteRulesByIntegration removes all trigger rules for the given integration.
	DeleteRulesByIntegration(ctx context.Context, integrationID string) error

	// IsUpdateProcessed returns true if the given Telegram update_id has already been processed.
	IsUpdateProcessed(ctx context.Context, integrationID string, updateID int64) (bool, error)

	// MarkUpdateProcessed records a Telegram update_id as processed.
	MarkUpdateProcessed(ctx context.Context, integrationID string, updateID int64) error

	// GetWebhookInfo returns the webhook secret, status, and error for an integration.
	GetWebhookInfo(ctx context.Context, integrationID string) (secret, status, webhookErr string, err error)

	// SetWebhookInfo updates the webhook secret, status, and error for an integration.
	SetWebhookInfo(ctx context.Context, integrationID, secret, status, webhookErr string) error
}
