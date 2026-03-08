package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"

	"github.com/shaharia-lab/agento/internal/config"
)

// SQLiteTriggerStore implements TriggerStore backed by a SQLite database.
type SQLiteTriggerStore struct {
	db *sql.DB
}

// NewSQLiteTriggerStore returns a new SQLiteTriggerStore.
func NewSQLiteTriggerStore(db *sql.DB) *SQLiteTriggerStore {
	return &SQLiteTriggerStore{db: db}
}

// ListRules returns all trigger rules for the given integration, ordered by creation time.
func (s *SQLiteTriggerStore) ListRules(ctx context.Context, integrationID string) ([]*config.TriggerRule, error) {
	rows, err := s.db.QueryContext(ctx, `
		SELECT id, integration_id, name, agent_slug, enabled,
		       filter_prefix, filter_keywords, filter_chat_ids,
		       created_at, updated_at
		FROM trigger_rules
		WHERE integration_id = ?
		ORDER BY created_at ASC`, integrationID)
	if err != nil {
		return nil, fmt.Errorf("listing trigger rules: %w", err)
	}
	defer rows.Close() //nolint:errcheck

	rules := make([]*config.TriggerRule, 0)
	for rows.Next() {
		r, scanErr := scanTriggerRule(rows)
		if scanErr != nil {
			return nil, scanErr
		}
		rules = append(rules, r)
	}
	return rules, rows.Err()
}

// GetRule returns a single trigger rule by ID, or nil if not found.
func (s *SQLiteTriggerStore) GetRule(ctx context.Context, id string) (*config.TriggerRule, error) {
	row := s.db.QueryRowContext(ctx, `
		SELECT id, integration_id, name, agent_slug, enabled,
		       filter_prefix, filter_keywords, filter_chat_ids,
		       created_at, updated_at
		FROM trigger_rules WHERE id = ?`, id)

	var r config.TriggerRule
	var enabled int
	var keywordsJSON, chatIDsJSON string

	err := row.Scan(
		&r.ID, &r.IntegrationID, &r.Name, &r.AgentSlug, &enabled,
		&r.FilterPrefix, &keywordsJSON, &chatIDsJSON,
		&r.CreatedAt, &r.UpdatedAt,
	)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("getting trigger rule %q: %w", id, err)
	}

	r.Enabled = enabled != 0
	if err := json.Unmarshal([]byte(keywordsJSON), &r.FilterKeywords); err != nil {
		r.FilterKeywords = nil
	}
	if err := json.Unmarshal([]byte(chatIDsJSON), &r.FilterChatIDs); err != nil {
		r.FilterChatIDs = nil
	}
	return &r, nil
}

// CreateRule inserts a new trigger rule.
func (s *SQLiteTriggerStore) CreateRule(ctx context.Context, rule *config.TriggerRule) error {
	if rule.ID == "" {
		rule.ID = uuid.New().String()
	}
	now := time.Now().UTC()
	rule.CreatedAt = now
	rule.UpdatedAt = now

	keywordsJSON, err := json.Marshal(rule.FilterKeywords)
	if err != nil {
		return fmt.Errorf("marshaling filter_keywords: %w", err)
	}
	chatIDsJSON, err := json.Marshal(rule.FilterChatIDs)
	if err != nil {
		return fmt.Errorf("marshaling filter_chat_ids: %w", err)
	}

	enabled := 0
	if rule.Enabled {
		enabled = 1
	}

	_, err = s.db.ExecContext(ctx, `
		INSERT INTO trigger_rules
			(id, integration_id, name, agent_slug, enabled,
			 filter_prefix, filter_keywords, filter_chat_ids,
			 created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		rule.ID, rule.IntegrationID, rule.Name, rule.AgentSlug, enabled,
		rule.FilterPrefix, string(keywordsJSON), string(chatIDsJSON),
		rule.CreatedAt, rule.UpdatedAt,
	)
	if err != nil {
		return fmt.Errorf("creating trigger rule: %w", err)
	}
	return nil
}

// UpdateRule persists changes to an existing trigger rule.
func (s *SQLiteTriggerStore) UpdateRule(ctx context.Context, rule *config.TriggerRule) error {
	rule.UpdatedAt = time.Now().UTC()

	keywordsJSON, err := json.Marshal(rule.FilterKeywords)
	if err != nil {
		return fmt.Errorf("marshaling filter_keywords: %w", err)
	}
	chatIDsJSON, err := json.Marshal(rule.FilterChatIDs)
	if err != nil {
		return fmt.Errorf("marshaling filter_chat_ids: %w", err)
	}

	enabled := 0
	if rule.Enabled {
		enabled = 1
	}

	_, err = s.db.ExecContext(ctx, `
		UPDATE trigger_rules SET
			name = ?, agent_slug = ?, enabled = ?,
			filter_prefix = ?, filter_keywords = ?, filter_chat_ids = ?,
			updated_at = ?
		WHERE id = ?`,
		rule.Name, rule.AgentSlug, enabled,
		rule.FilterPrefix, string(keywordsJSON), string(chatIDsJSON),
		rule.UpdatedAt, rule.ID,
	)
	if err != nil {
		return fmt.Errorf("updating trigger rule: %w", err)
	}
	return nil
}

// DeleteRule removes a trigger rule by ID.
func (s *SQLiteTriggerStore) DeleteRule(ctx context.Context, id string) error {
	_, err := s.db.ExecContext(ctx, `DELETE FROM trigger_rules WHERE id = ?`, id)
	if err != nil {
		return fmt.Errorf("deleting trigger rule %q: %w", id, err)
	}
	return nil
}

// DeleteRulesByIntegration removes all trigger rules for the given integration.
func (s *SQLiteTriggerStore) DeleteRulesByIntegration(ctx context.Context, integrationID string) error {
	_, err := s.db.ExecContext(ctx, `DELETE FROM trigger_rules WHERE integration_id = ?`, integrationID)
	if err != nil {
		return fmt.Errorf("deleting trigger rules for integration %q: %w", integrationID, err)
	}
	return nil
}

// IsUpdateProcessed returns true if the given Telegram update_id has already been processed.
func (s *SQLiteTriggerStore) IsUpdateProcessed(
	ctx context.Context, integrationID string, updateID int64,
) (bool, error) {
	var count int
	err := s.db.QueryRowContext(ctx, `
		SELECT COUNT(*) FROM telegram_processed_updates
		WHERE integration_id = ? AND update_id = ?`,
		integrationID, updateID,
	).Scan(&count)
	if err != nil {
		return false, fmt.Errorf("checking processed update: %w", err)
	}
	return count > 0, nil
}

// MarkUpdateProcessed records a Telegram update_id as processed.
func (s *SQLiteTriggerStore) MarkUpdateProcessed(ctx context.Context, integrationID string, updateID int64) error {
	_, err := s.db.ExecContext(ctx, `
		INSERT OR IGNORE INTO telegram_processed_updates
			(integration_id, update_id, processed_at)
		VALUES (?, ?, ?)`,
		integrationID, updateID, time.Now().UTC(),
	)
	if err != nil {
		return fmt.Errorf("marking update as processed: %w", err)
	}
	return nil
}

// GetWebhookInfo returns the webhook secret, status, and error for an integration.
func (s *SQLiteTriggerStore) GetWebhookInfo(
	ctx context.Context, integrationID string,
) (secret, status, webhookErr string, err error) {
	err = s.db.QueryRowContext(ctx, `
		SELECT webhook_secret, webhook_status, webhook_error
		FROM integrations WHERE id = ?`, integrationID,
	).Scan(&secret, &status, &webhookErr)
	if err == sql.ErrNoRows {
		return "", "", "", nil
	}
	if err != nil {
		return "", "", "", fmt.Errorf("getting webhook info for %q: %w", integrationID, err)
	}
	return secret, status, webhookErr, nil
}

// SetWebhookInfo updates the webhook secret, status, and error for an integration.
func (s *SQLiteTriggerStore) SetWebhookInfo(
	ctx context.Context, integrationID, secret, status, webhookErr string,
) error {
	_, err := s.db.ExecContext(ctx, `
		UPDATE integrations SET webhook_secret = ?, webhook_status = ?, webhook_error = ?
		WHERE id = ?`,
		secret, status, webhookErr, integrationID,
	)
	if err != nil {
		return fmt.Errorf("setting webhook info for %q: %w", integrationID, err)
	}
	return nil
}

// scanTriggerRule scans a row into a TriggerRule.
type triggerRowScanner interface {
	Scan(dest ...any) error
}

func scanTriggerRule(rows triggerRowScanner) (*config.TriggerRule, error) {
	var r config.TriggerRule
	var enabled int
	var keywordsJSON, chatIDsJSON string

	err := rows.Scan(
		&r.ID, &r.IntegrationID, &r.Name, &r.AgentSlug, &enabled,
		&r.FilterPrefix, &keywordsJSON, &chatIDsJSON,
		&r.CreatedAt, &r.UpdatedAt,
	)
	if err != nil {
		return nil, fmt.Errorf("scanning trigger rule: %w", err)
	}

	r.Enabled = enabled != 0
	if err := json.Unmarshal([]byte(keywordsJSON), &r.FilterKeywords); err != nil {
		r.FilterKeywords = nil
	}
	if err := json.Unmarshal([]byte(chatIDsJSON), &r.FilterChatIDs); err != nil {
		r.FilterChatIDs = nil
	}
	return &r, nil
}
