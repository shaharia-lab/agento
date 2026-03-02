package storage

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"github.com/google/uuid"
)

// SQLiteConversationMappingStore implements ConversationMappingStore backed by SQLite.
type SQLiteConversationMappingStore struct {
	db *sql.DB
}

// NewSQLiteConversationMappingStore returns a new store instance.
func NewSQLiteConversationMappingStore(db *sql.DB) *SQLiteConversationMappingStore {
	return &SQLiteConversationMappingStore{db: db}
}

// GetByPlatformChannel returns the mapping for a given platform + channel, or nil.
func (s *SQLiteConversationMappingStore) GetByPlatformChannel(
	platformType, platformID, channelID string,
) (*ConversationMapping, error) {
	ctx := context.Background()
	row := s.db.QueryRowContext(ctx, `
		SELECT id, session_id, platform_type, platform_id, channel_id, agent_slug, created_at, updated_at
		FROM conversation_mappings
		WHERE platform_type = ? AND platform_id = ? AND channel_id = ?`,
		platformType, platformID, channelID,
	)

	m := &ConversationMapping{}
	err := row.Scan(
		&m.ID, &m.SessionID, &m.PlatformType, &m.PlatformID,
		&m.ChannelID, &m.AgentSlug, &m.CreatedAt, &m.UpdatedAt,
	)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("getting conversation mapping: %w", err)
	}
	return m, nil
}

// GetBySessionID returns the mapping for a given session, or nil.
func (s *SQLiteConversationMappingStore) GetBySessionID(sessionID string) (*ConversationMapping, error) {
	ctx := context.Background()
	row := s.db.QueryRowContext(ctx, `
		SELECT id, session_id, platform_type, platform_id, channel_id, agent_slug, created_at, updated_at
		FROM conversation_mappings
		WHERE session_id = ?`, sessionID,
	)

	m := &ConversationMapping{}
	err := row.Scan(
		&m.ID, &m.SessionID, &m.PlatformType, &m.PlatformID,
		&m.ChannelID, &m.AgentSlug, &m.CreatedAt, &m.UpdatedAt,
	)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("getting conversation mapping by session: %w", err)
	}
	return m, nil
}

// Create inserts a new conversation mapping.
func (s *SQLiteConversationMappingStore) Create(mapping *ConversationMapping) error {
	if mapping.ID == "" {
		mapping.ID = uuid.New().String()
	}
	now := time.Now().UTC()
	if mapping.CreatedAt.IsZero() {
		mapping.CreatedAt = now
	}
	mapping.UpdatedAt = now

	ctx := context.Background()
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO conversation_mappings
			(id, session_id, platform_type, platform_id, channel_id, agent_slug, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
		mapping.ID, mapping.SessionID, mapping.PlatformType, mapping.PlatformID,
		mapping.ChannelID, mapping.AgentSlug, mapping.CreatedAt, mapping.UpdatedAt,
	)
	if err != nil {
		return fmt.Errorf("creating conversation mapping: %w", err)
	}
	return nil
}

// UpdateAgentSlug changes the agent associated with a mapping.
func (s *SQLiteConversationMappingStore) UpdateAgentSlug(id, agentSlug string) error {
	ctx := context.Background()
	res, err := s.db.ExecContext(ctx, `
		UPDATE conversation_mappings SET agent_slug = ?, updated_at = ? WHERE id = ?`,
		agentSlug, time.Now().UTC(), id,
	)
	if err != nil {
		return fmt.Errorf("updating agent slug for mapping %q: %w", id, err)
	}
	n, _ := res.RowsAffected()
	if n == 0 {
		return fmt.Errorf("conversation mapping %q not found", id)
	}
	return nil
}

// Delete removes a mapping by its ID.
func (s *SQLiteConversationMappingStore) Delete(id string) error {
	ctx := context.Background()
	_, err := s.db.ExecContext(ctx, "DELETE FROM conversation_mappings WHERE id = ?", id)
	if err != nil {
		return fmt.Errorf("deleting conversation mapping %q: %w", id, err)
	}
	return nil
}

// DeleteBySessionID removes the mapping for a given session.
func (s *SQLiteConversationMappingStore) DeleteBySessionID(sessionID string) error {
	ctx := context.Background()
	_, err := s.db.ExecContext(ctx, "DELETE FROM conversation_mappings WHERE session_id = ?", sessionID)
	if err != nil {
		return fmt.Errorf("deleting conversation mapping for session %q: %w", sessionID, err)
	}
	return nil
}

// ListByPlatform returns all mappings for a given platform instance.
func (s *SQLiteConversationMappingStore) ListByPlatform(
	platformType, platformID string,
) ([]*ConversationMapping, error) {
	ctx := context.Background()
	rows, err := s.db.QueryContext(ctx, `
		SELECT id, session_id, platform_type, platform_id, channel_id, agent_slug, created_at, updated_at
		FROM conversation_mappings
		WHERE platform_type = ? AND platform_id = ?
		ORDER BY updated_at DESC`, platformType, platformID,
	)
	if err != nil {
		return nil, fmt.Errorf("listing conversation mappings: %w", err)
	}
	defer rows.Close() //nolint:errcheck

	var result []*ConversationMapping
	for rows.Next() {
		m := &ConversationMapping{}
		if scanErr := rows.Scan(
			&m.ID, &m.SessionID, &m.PlatformType, &m.PlatformID,
			&m.ChannelID, &m.AgentSlug, &m.CreatedAt, &m.UpdatedAt,
		); scanErr != nil {
			return nil, fmt.Errorf("scanning conversation mapping: %w", scanErr)
		}
		result = append(result, m)
	}
	return result, rows.Err()
}
