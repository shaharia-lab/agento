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
func (s *SQLiteAgentStore) List() ([]*config.AgentConfig, error) {
	ctx := context.Background()
	rows, err := s.db.QueryContext(ctx, `
		SELECT slug, name, description, model, thinking, permission_mode,
		       system_prompt, capabilities
		FROM agents
		ORDER BY name ASC`)
	if err != nil {
		return nil, fmt.Errorf("listing agents: %w", err)
	}
	defer rows.Close() //nolint:errcheck

	agents := make([]*config.AgentConfig, 0)
	for rows.Next() {
		a, err := scanAgent(rows)
		if err != nil {
			return nil, err
		}
		agents = append(agents, a)
	}
	return agents, rows.Err()
}

// Get returns the agent config for the given slug, or nil if not found.
func (s *SQLiteAgentStore) Get(slug string) (*config.AgentConfig, error) {
	ctx := context.Background()
	row := s.db.QueryRowContext(ctx, `
		SELECT slug, name, description, model, thinking, permission_mode,
		       system_prompt, capabilities
		FROM agents WHERE slug = ?`, slug)

	a := &config.AgentConfig{}
	var capsJSON string
	err := row.Scan(
		&a.Slug, &a.Name, &a.Description, &a.Model, &a.Thinking,
		&a.PermissionMode, &a.SystemPrompt, &capsJSON,
	)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("getting agent %q: %w", slug, err)
	}
	if err := json.Unmarshal([]byte(capsJSON), &a.Capabilities); err != nil {
		return nil, fmt.Errorf("parsing capabilities for agent %q: %w", slug, err)
	}
	return a, nil
}

// Save persists the agent config (upsert).
func (s *SQLiteAgentStore) Save(agent *config.AgentConfig) error {
	if err := validateAgentForSave(agent); err != nil {
		return err
	}

	capsJSON, err := json.Marshal(agent.Capabilities)
	if err != nil {
		return fmt.Errorf("marshaling capabilities for agent %q: %w", agent.Slug, err)
	}

	now := time.Now().UTC()
	ctx := context.Background()
	_, err = s.db.ExecContext(ctx, `
		INSERT INTO agents (slug, name, description, model, thinking, permission_mode,
		                    system_prompt, capabilities, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(slug) DO UPDATE SET
			name = excluded.name,
			description = excluded.description,
			model = excluded.model,
			thinking = excluded.thinking,
			permission_mode = excluded.permission_mode,
			system_prompt = excluded.system_prompt,
			capabilities = excluded.capabilities,
			updated_at = excluded.updated_at`,
		agent.Slug, agent.Name, agent.Description, agent.Model,
		agent.Thinking, agent.PermissionMode, agent.SystemPrompt,
		string(capsJSON), now, now,
	)
	if err != nil {
		return fmt.Errorf("saving agent %q: %w", agent.Slug, err)
	}
	return nil
}

// Delete removes the agent with the given slug.
func (s *SQLiteAgentStore) Delete(slug string) error {
	ctx := context.Background()
	res, err := s.db.ExecContext(ctx, "DELETE FROM agents WHERE slug = ?", slug)
	if err != nil {
		return fmt.Errorf("deleting agent %q: %w", slug, err)
	}
	n, _ := res.RowsAffected()
	if n == 0 {
		return fmt.Errorf("agent %q not found", slug)
	}
	return nil
}

func scanAgent(rows *sql.Rows) (*config.AgentConfig, error) {
	a := &config.AgentConfig{}
	var capsJSON string
	err := rows.Scan(
		&a.Slug, &a.Name, &a.Description, &a.Model, &a.Thinking,
		&a.PermissionMode, &a.SystemPrompt, &capsJSON,
	)
	if err != nil {
		return nil, fmt.Errorf("scanning agent: %w", err)
	}
	if err := json.Unmarshal([]byte(capsJSON), &a.Capabilities); err != nil {
		return nil, fmt.Errorf("parsing capabilities: %w", err)
	}
	return a, nil
}
