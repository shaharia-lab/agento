package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"
)

// SQLiteChatStore implements ChatStore backed by a SQLite database.
type SQLiteChatStore struct {
	db *sql.DB
}

// NewSQLiteChatStore returns a new SQLiteChatStore.
func NewSQLiteChatStore(db *sql.DB) *SQLiteChatStore {
	return &SQLiteChatStore{db: db}
}

// ListSessions returns all chat sessions ordered by most recently updated.
func (s *SQLiteChatStore) ListSessions() ([]*ChatSession, error) {
	ctx := context.Background()
	rows, err := s.db.QueryContext(ctx, `
		SELECT id, title, agent_slug, sdk_session_id, working_directory, model,
		       settings_profile_id, total_input_tokens, total_output_tokens,
		       total_cache_creation_tokens, total_cache_read_tokens,
		       created_at, updated_at
		FROM chat_sessions
		ORDER BY updated_at DESC`)
	if err != nil {
		return nil, fmt.Errorf("listing sessions: %w", err)
	}
	defer rows.Close() //nolint:errcheck

	sessions := make([]*ChatSession, 0)
	for rows.Next() {
		cs, err := scanChatSession(rows)
		if err != nil {
			return nil, err
		}
		sessions = append(sessions, cs)
	}
	return sessions, rows.Err()
}

// GetSession returns session metadata for the given ID, or nil if not found.
func (s *SQLiteChatStore) GetSession(id string) (*ChatSession, error) {
	ctx := context.Background()
	row := s.db.QueryRowContext(ctx, `
		SELECT id, title, agent_slug, sdk_session_id, working_directory, model,
		       settings_profile_id, total_input_tokens, total_output_tokens,
		       total_cache_creation_tokens, total_cache_read_tokens,
		       created_at, updated_at
		FROM chat_sessions WHERE id = ?`, id)

	cs := &ChatSession{}
	err := row.Scan(
		&cs.ID, &cs.Title, &cs.AgentSlug, &cs.SDKSession, &cs.WorkingDir,
		&cs.Model, &cs.SettingsProfileID,
		&cs.TotalInputTokens, &cs.TotalOutputTokens,
		&cs.TotalCacheCreationTokens, &cs.TotalCacheReadTokens,
		&cs.CreatedAt, &cs.UpdatedAt,
	)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("getting session %q: %w", id, err)
	}
	return cs, nil
}

// GetSessionWithMessages returns the session and its full message history.
func (s *SQLiteChatStore) GetSessionWithMessages(id string) (*ChatSession, []ChatMessage, error) {
	cs, err := s.GetSession(id)
	if err != nil || cs == nil {
		return cs, nil, err
	}

	ctx := context.Background()
	rows, err := s.db.QueryContext(ctx, `
		SELECT role, content, blocks, timestamp
		FROM chat_messages
		WHERE session_id = ?
		ORDER BY id ASC`, id)
	if err != nil {
		return nil, nil, fmt.Errorf("listing messages for session %q: %w", id, err)
	}
	defer rows.Close() //nolint:errcheck

	messages := make([]ChatMessage, 0)
	for rows.Next() {
		var msg ChatMessage
		var blocksJSON string
		var ts time.Time
		if err := rows.Scan(&msg.Role, &msg.Content, &blocksJSON, &ts); err != nil {
			return nil, nil, fmt.Errorf("scanning message: %w", err)
		}
		msg.Timestamp = ts
		if blocksJSON != "" && blocksJSON != "[]" {
			if err := json.Unmarshal([]byte(blocksJSON), &msg.Blocks); err != nil {
				msg.Blocks = nil // non-fatal
			}
		}
		messages = append(messages, msg)
	}
	return cs, messages, rows.Err()
}

// CreateSession creates a new chat session.
func (s *SQLiteChatStore) CreateSession(agentSlug, workingDir, model, settingsProfileID string) (*ChatSession, error) {
	id := newSQLiteUUID()
	now := time.Now().UTC()
	cs := &ChatSession{
		ID:                id,
		Title:             "New Chat",
		AgentSlug:         agentSlug,
		WorkingDir:        workingDir,
		Model:             model,
		SettingsProfileID: settingsProfileID,
		CreatedAt:         now,
		UpdatedAt:         now,
	}

	ctx := context.Background()
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO chat_sessions
			(id, title, agent_slug, sdk_session_id, working_directory, model,
			 settings_profile_id, total_input_tokens, total_output_tokens,
			 total_cache_creation_tokens, total_cache_read_tokens, created_at, updated_at)
		VALUES (?, ?, ?, '', ?, ?, ?, 0, 0, 0, 0, ?, ?)`,
		cs.ID, cs.Title, cs.AgentSlug, cs.WorkingDir, cs.Model,
		cs.SettingsProfileID, cs.CreatedAt, cs.UpdatedAt,
	)
	if err != nil {
		return nil, fmt.Errorf("creating session: %w", err)
	}
	return cs, nil
}

// AppendMessage appends a message to the given session.
func (s *SQLiteChatStore) AppendMessage(sessionID string, msg ChatMessage) error {
	blocksJSON := "[]"
	if len(msg.Blocks) > 0 {
		b, err := json.Marshal(msg.Blocks)
		if err != nil {
			return fmt.Errorf("marshaling blocks: %w", err)
		}
		blocksJSON = string(b)
	}

	ctx := context.Background()
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO chat_messages (session_id, role, content, blocks, timestamp)
		VALUES (?, ?, ?, ?, ?)`,
		sessionID, msg.Role, msg.Content, blocksJSON, msg.Timestamp,
	)
	if err != nil {
		return fmt.Errorf("appending message to session %q: %w", sessionID, err)
	}
	return nil
}

// UpdateSession updates a session's metadata.
func (s *SQLiteChatStore) UpdateSession(session *ChatSession) error {
	ctx := context.Background()
	res, err := s.db.ExecContext(ctx, `
		UPDATE chat_sessions SET
			title = ?, agent_slug = ?, sdk_session_id = ?, working_directory = ?,
			model = ?, settings_profile_id = ?,
			total_input_tokens = ?, total_output_tokens = ?,
			total_cache_creation_tokens = ?, total_cache_read_tokens = ?,
			updated_at = ?
		WHERE id = ?`,
		session.Title, session.AgentSlug, session.SDKSession, session.WorkingDir,
		session.Model, session.SettingsProfileID,
		session.TotalInputTokens, session.TotalOutputTokens,
		session.TotalCacheCreationTokens, session.TotalCacheReadTokens,
		session.UpdatedAt, session.ID,
	)
	if err != nil {
		return fmt.Errorf("updating session %q: %w", session.ID, err)
	}
	n, raErr := res.RowsAffected()
	if raErr != nil {
		return fmt.Errorf("checking rows affected for session %q: %w", session.ID, raErr)
	}
	if n == 0 {
		return fmt.Errorf("session %q not found", session.ID)
	}
	return nil
}

// DeleteSession deletes a session and its messages (via CASCADE).
func (s *SQLiteChatStore) DeleteSession(id string) error {
	ctx := context.Background()
	res, err := s.db.ExecContext(ctx, "DELETE FROM chat_sessions WHERE id = ?", id)
	if err != nil {
		return fmt.Errorf("deleting session %q: %w", id, err)
	}
	n, raErr := res.RowsAffected()
	if raErr != nil {
		return fmt.Errorf("checking rows affected for session %q: %w", id, raErr)
	}
	if n == 0 {
		return fmt.Errorf("session %q not found", id)
	}
	return nil
}

func scanChatSession(rows *sql.Rows) (*ChatSession, error) {
	cs := &ChatSession{}
	err := rows.Scan(
		&cs.ID, &cs.Title, &cs.AgentSlug, &cs.SDKSession, &cs.WorkingDir,
		&cs.Model, &cs.SettingsProfileID,
		&cs.TotalInputTokens, &cs.TotalOutputTokens,
		&cs.TotalCacheCreationTokens, &cs.TotalCacheReadTokens,
		&cs.CreatedAt, &cs.UpdatedAt,
	)
	if err != nil {
		return nil, fmt.Errorf("scanning session: %w", err)
	}
	return cs, nil
}

func newSQLiteUUID() string {
	return uuid.New().String()
}
