package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"strings"
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
func (s *SQLiteChatStore) ListSessions(ctx context.Context) (sessions []*ChatSession, err error) {
	ctx, end := withStorageSpan(ctx, "list", "chat_session")
	defer func() { end(err) }()

	var rows *sql.Rows
	rows, err = s.db.QueryContext(ctx, `
		SELECT id, title, agent_slug, sdk_session_id, working_directory, model,
		       settings_profile_id, total_input_tokens, total_output_tokens,
		       total_cache_creation_tokens, total_cache_read_tokens,
		       created_at, updated_at, is_favorite
		FROM chat_sessions
		ORDER BY updated_at DESC`)
	if err != nil {
		return nil, fmt.Errorf("listing sessions: %w", err)
	}
	defer rows.Close() //nolint:errcheck

	sessions = make([]*ChatSession, 0)
	for rows.Next() {
		cs, scanErr := scanChatSession(rows)
		if scanErr != nil {
			return nil, scanErr
		}
		sessions = append(sessions, cs)
	}
	err = rows.Err()
	return sessions, err
}

// GetSession returns session metadata for the given ID, or nil if not found.
func (s *SQLiteChatStore) GetSession(ctx context.Context, id string) (cs *ChatSession, err error) {
	ctx, end := withStorageSpan(ctx, "get", "chat_session")
	defer func() { end(err) }()

	row := s.db.QueryRowContext(ctx, `
		SELECT id, title, agent_slug, sdk_session_id, working_directory, model,
		       settings_profile_id, total_input_tokens, total_output_tokens,
		       total_cache_creation_tokens, total_cache_read_tokens,
		       created_at, updated_at, is_favorite
		FROM chat_sessions WHERE id = ?`, id)

	cs = &ChatSession{}
	err = row.Scan(
		&cs.ID, &cs.Title, &cs.AgentSlug, &cs.SDKSession, &cs.WorkingDir,
		&cs.Model, &cs.SettingsProfileID,
		&cs.TotalInputTokens, &cs.TotalOutputTokens,
		&cs.TotalCacheCreationTokens, &cs.TotalCacheReadTokens,
		&cs.CreatedAt, &cs.UpdatedAt, &cs.IsFavorite,
	)
	if err == sql.ErrNoRows {
		err = nil
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("getting session %q: %w", id, err)
	}
	return cs, nil
}

// GetSessionWithMessages returns the session and its full message history.
func (s *SQLiteChatStore) GetSessionWithMessages(ctx context.Context, id string) (*ChatSession, []ChatMessage, error) {
	cs, err := s.GetSession(ctx, id)
	if err != nil || cs == nil {
		return cs, nil, err
	}

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
			if json.Unmarshal([]byte(blocksJSON), &msg.Blocks) != nil {
				msg.Blocks = nil // non-fatal
			}
		}
		messages = append(messages, msg)
	}
	return cs, messages, rows.Err()
}

// CreateSession creates a new chat session.
func (s *SQLiteChatStore) CreateSession(
	ctx context.Context, agentSlug, workingDir, model, settingsProfileID string,
) (cs *ChatSession, err error) {
	ctx, end := withStorageSpan(ctx, "create", "chat_session")
	defer func() { end(err) }()

	id := newSQLiteUUID()
	now := time.Now().UTC()
	cs = &ChatSession{
		ID:                id,
		Title:             "New Chat",
		AgentSlug:         agentSlug,
		WorkingDir:        workingDir,
		Model:             model,
		SettingsProfileID: settingsProfileID,
		CreatedAt:         now,
		UpdatedAt:         now,
	}

	_, err = s.db.ExecContext(ctx, `
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
func (s *SQLiteChatStore) AppendMessage(ctx context.Context, sessionID string, msg ChatMessage) (err error) {
	ctx, end := withStorageSpan(ctx, "append", "chat_message")
	defer func() { end(err) }()

	blocksJSON := "[]"
	if len(msg.Blocks) > 0 {
		var b []byte
		b, err = json.Marshal(msg.Blocks)
		if err != nil {
			return fmt.Errorf("marshaling blocks: %w", err)
		}
		blocksJSON = string(b)
	}

	_, err = s.db.ExecContext(ctx, `
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
func (s *SQLiteChatStore) UpdateSession(ctx context.Context, session *ChatSession) (err error) {
	ctx, end := withStorageSpan(ctx, "update", "chat_session")
	defer func() { end(err) }()

	var res sql.Result
	res, err = s.db.ExecContext(ctx, `
		UPDATE chat_sessions SET
			title = ?, agent_slug = ?, sdk_session_id = ?, working_directory = ?,
			model = ?, settings_profile_id = ?,
			total_input_tokens = ?, total_output_tokens = ?,
			total_cache_creation_tokens = ?, total_cache_read_tokens = ?,
			is_favorite = ?,
			updated_at = ?
		WHERE id = ?`,
		session.Title, session.AgentSlug, session.SDKSession, session.WorkingDir,
		session.Model, session.SettingsProfileID,
		session.TotalInputTokens, session.TotalOutputTokens,
		session.TotalCacheCreationTokens, session.TotalCacheReadTokens,
		session.IsFavorite,
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
func (s *SQLiteChatStore) DeleteSession(ctx context.Context, id string) (err error) {
	ctx, end := withStorageSpan(ctx, "delete", "chat_session")
	defer func() { end(err) }()

	var res sql.Result
	res, err = s.db.ExecContext(ctx, "DELETE FROM chat_sessions WHERE id = ?", id)
	if err != nil {
		return fmt.Errorf("deleting session %q: %w", id, err)
	}
	n, raErr := res.RowsAffected()
	if raErr != nil {
		err = fmt.Errorf("checking rows affected for session %q: %w", id, raErr)
		return err
	}
	if n == 0 {
		err = fmt.Errorf("session %q not found", id)
		return err
	}
	return nil
}

// BulkDeleteSessions deletes multiple chat sessions (and their messages via CASCADE) by ID.
func (s *SQLiteChatStore) BulkDeleteSessions(ctx context.Context, ids []string) (err error) {
	if len(ids) == 0 {
		return nil
	}
	ctx, end := withStorageSpan(ctx, "bulk_delete", "chat_session")
	defer func() { end(err) }()
	placeholders := make([]string, len(ids))
	args := make([]any, len(ids))
	for i, id := range ids {
		placeholders[i] = "?"
		args[i] = id
	}
	//nolint:gosec // placeholders are "?" repeated, not user input
	query := "DELETE FROM chat_sessions WHERE id IN (" + strings.Join(placeholders, ",") + ")"
	_, err = s.db.ExecContext(ctx, query, args...)
	if err != nil {
		return fmt.Errorf("bulk deleting sessions: %w", err)
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
		&cs.CreatedAt, &cs.UpdatedAt, &cs.IsFavorite,
	)
	if err != nil {
		return nil, fmt.Errorf("scanning session: %w", err)
	}
	return cs, nil
}

func newSQLiteUUID() string {
	return uuid.New().String()
}
