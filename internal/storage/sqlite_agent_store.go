package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"time"

	"github.com/shaharia-lab/agento/internal/config"
)

// SQLiteAgentStore implements AgentStore backed by a SQLite database.
type SQLiteAgentStore struct {
	db *sql.DB
}

// NewSQLiteAgentStore returns a new SQLiteAgentStore.
func NewSQLiteAgentStore(db *sql.DB) *SQLiteAgentStore {
	return &SQLiteAgentStore{db: db}
}

// List returns all agent configs.
func (s *SQLiteAgentStore) List(ctx context.Context) (agents []*config.AgentConfig, err error) {
	ctx, end := withStorageSpan(ctx, "list", "agent")
	defer func() { end(err) }()

	var rows *sql.Rows
	rows, err = s.db.QueryContext(ctx, `
		SELECT slug, name, description, model, thinking, permission_mode,
		       system_prompt, capabilities, claude_config_dir
		FROM agents
		ORDER BY name ASC`)
	if err != nil {
		return nil, fmt.Errorf("listing agents: %w", err)
	}
	defer rows.Close() //nolint:errcheck

	agents = make([]*config.AgentConfig, 0)
	for rows.Next() {
		a, scanErr := scanAgent(rows)
		if scanErr != nil {
			return nil, scanErr
		}
		agents = append(agents, a)
	}
	err = rows.Err()
	return agents, err
}

// Get returns the agent config for the given slug, or nil if not found.
func (s *SQLiteAgentStore) Get(ctx context.Context, slug string) (a *config.AgentConfig, err error) {
	ctx, end := withStorageSpan(ctx, "get", "agent")
	defer func() { end(err) }()

	row := s.db.QueryRowContext(ctx, `
		SELECT slug, name, description, model, thinking, permission_mode,
		       system_prompt, capabilities, claude_config_dir
		FROM agents WHERE slug = ?`, slug)

	a = &config.AgentConfig{}
	var capsJSON string
	err = row.Scan(
		&a.Slug, &a.Name, &a.Description, &a.Model, &a.Thinking,
		&a.PermissionMode, &a.SystemPrompt, &capsJSON, &a.ClaudeConfigDir,
	)
	if err == sql.ErrNoRows {
		err = nil
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("getting agent %q: %w", slug, err)
	}
	if unmarshalErr := json.Unmarshal([]byte(capsJSON), &a.Capabilities); unmarshalErr != nil {
		err = fmt.Errorf("parsing capabilities for agent %q: %w", slug, unmarshalErr)
		return nil, err
	}
	return a, nil
}

// Save persists the agent config (upsert).
func (s *SQLiteAgentStore) Save(ctx context.Context, agent *config.AgentConfig) (err error) {
	ctx, end := withStorageSpan(ctx, "save", "agent")
	defer func() { end(err) }()

	if valErr := validateAgentForSave(agent); valErr != nil {
		err = valErr
		return err
	}

	capsJSON, err := json.Marshal(agent.Capabilities)
	if err != nil {
		return fmt.Errorf("marshaling capabilities for agent %q: %w", agent.Slug, err)
	}

	now := time.Now().UTC()
	_, err = s.db.ExecContext(ctx, `
		INSERT INTO agents (slug, name, description, model, thinking, permission_mode,
		                    system_prompt, capabilities, claude_config_dir,
		                    created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(slug) DO UPDATE SET
			name = excluded.name,
			description = excluded.description,
			model = excluded.model,
			thinking = excluded.thinking,
			permission_mode = excluded.permission_mode,
			system_prompt = excluded.system_prompt,
			capabilities = excluded.capabilities,
			claude_config_dir = excluded.claude_config_dir,
			updated_at = excluded.updated_at`,
		agent.Slug, agent.Name, agent.Description, agent.Model,
		agent.Thinking, agent.PermissionMode, agent.SystemPrompt,
		string(capsJSON), agent.ClaudeConfigDir, now, now,
	)
	if err != nil {
		return fmt.Errorf("saving agent %q: %w", agent.Slug, err)
	}
	return nil
}

// Delete removes the agent with the given slug.
func (s *SQLiteAgentStore) Delete(ctx context.Context, slug string) (err error) {
	ctx, end := withStorageSpan(ctx, "delete", "agent")
	defer func() { end(err) }()

	var res sql.Result
	res, err = s.db.ExecContext(ctx, "DELETE FROM agents WHERE slug = ?", slug)
	if err != nil {
		return fmt.Errorf("deleting agent %q: %w", slug, err)
	}
	n, raErr := res.RowsAffected()
	if raErr != nil {
		err = fmt.Errorf("checking rows affected for agent %q: %w", slug, raErr)
		return err
	}
	if n == 0 {
		err = fmt.Errorf("agent %q not found", slug)
		return err
	}
	return nil
}

func scanAgent(rows *sql.Rows) (*config.AgentConfig, error) {
	a := &config.AgentConfig{}
	var capsJSON string
	err := rows.Scan(
		&a.Slug, &a.Name, &a.Description, &a.Model, &a.Thinking,
		&a.PermissionMode, &a.SystemPrompt, &capsJSON, &a.ClaudeConfigDir,
	)
	if err != nil {
		return nil, fmt.Errorf("scanning agent: %w", err)
	}
	if err := json.Unmarshal([]byte(capsJSON), &a.Capabilities); err != nil {
		return nil, fmt.Errorf("parsing capabilities: %w", err)
	}
	return a, nil
}
