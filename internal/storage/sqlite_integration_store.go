package storage

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"

	"golang.org/x/oauth2"

	"github.com/shaharia-lab/agento/internal/config"
)

// SQLiteIntegrationStore implements IntegrationStore backed by a SQLite database.
type SQLiteIntegrationStore struct {
	db *sql.DB
}

// NewSQLiteIntegrationStore returns a new SQLiteIntegrationStore.
func NewSQLiteIntegrationStore(db *sql.DB) *SQLiteIntegrationStore {
	return &SQLiteIntegrationStore{db: db}
}

// List returns all integration configs.
func (s *SQLiteIntegrationStore) List() ([]*config.IntegrationConfig, error) {
	ctx := context.Background()
	rows, err := s.db.QueryContext(ctx, `
		SELECT id, name, type, enabled, credentials, auth, services, created_at, updated_at
		FROM integrations
		ORDER BY name ASC`)
	if err != nil {
		return nil, fmt.Errorf("listing integrations: %w", err)
	}
	defer rows.Close() //nolint:errcheck

	integrations := make([]*config.IntegrationConfig, 0)
	for rows.Next() {
		cfg, err := scanIntegration(rows)
		if err != nil {
			return nil, err
		}
		integrations = append(integrations, cfg)
	}
	return integrations, rows.Err()
}

// Get returns the integration config for the given id, or nil if not found.
func (s *SQLiteIntegrationStore) Get(id string) (*config.IntegrationConfig, error) {
	ctx := context.Background()
	row := s.db.QueryRowContext(ctx, `
		SELECT id, name, type, enabled, credentials, auth, services, created_at, updated_at
		FROM integrations WHERE id = ?`, id)

	cfg := &config.IntegrationConfig{}
	var credJSON, servJSON string
	var authJSON sql.NullString
	var enabled int
	err := row.Scan(
		&cfg.ID, &cfg.Name, &cfg.Type, &enabled,
		&credJSON, &authJSON, &servJSON,
		&cfg.CreatedAt, &cfg.UpdatedAt,
	)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("getting integration %q: %w", id, err)
	}

	cfg.Enabled = enabled != 0
	if err := json.Unmarshal([]byte(credJSON), &cfg.Credentials); err != nil {
		return nil, fmt.Errorf("parsing credentials for integration %q: %w", id, err)
	}
	if authJSON.Valid && authJSON.String != "" {
		var token oauth2.Token
		if err := json.Unmarshal([]byte(authJSON.String), &token); err != nil {
			return nil, fmt.Errorf("parsing auth for integration %q: %w", id, err)
		}
		cfg.Auth = &token
	}
	if err := json.Unmarshal([]byte(servJSON), &cfg.Services); err != nil {
		return nil, fmt.Errorf("parsing services for integration %q: %w", id, err)
	}
	return cfg, nil
}

// Save persists the integration config (upsert).
func (s *SQLiteIntegrationStore) Save(cfg *config.IntegrationConfig) error {
	if cfg.ID == "" {
		return fmt.Errorf("integration id is required")
	}

	credJSON, err := json.Marshal(cfg.Credentials)
	if err != nil {
		return fmt.Errorf("marshaling credentials: %w", err)
	}

	var authJSON *string
	if cfg.Auth != nil {
		b, err := json.Marshal(cfg.Auth)
		if err != nil {
			return fmt.Errorf("marshaling auth token: %w", err)
		}
		s := string(b)
		authJSON = &s
	}

	servJSON, err := json.Marshal(cfg.Services)
	if err != nil {
		return fmt.Errorf("marshaling services: %w", err)
	}

	enabled := 0
	if cfg.Enabled {
		enabled = 1
	}

	ctx := context.Background()
	_, err = s.db.ExecContext(ctx, `
		INSERT INTO integrations (id, name, type, enabled, credentials, auth, services, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(id) DO UPDATE SET
			name = excluded.name,
			type = excluded.type,
			enabled = excluded.enabled,
			credentials = excluded.credentials,
			auth = excluded.auth,
			services = excluded.services,
			updated_at = excluded.updated_at`,
		cfg.ID, cfg.Name, cfg.Type, enabled,
		string(credJSON), authJSON, string(servJSON),
		cfg.CreatedAt, cfg.UpdatedAt,
	)
	if err != nil {
		return fmt.Errorf("saving integration %q: %w", cfg.ID, err)
	}
	return nil
}

// Delete removes the integration with the given id.
func (s *SQLiteIntegrationStore) Delete(id string) error {
	ctx := context.Background()
	res, err := s.db.ExecContext(ctx, "DELETE FROM integrations WHERE id = ?", id)
	if err != nil {
		return fmt.Errorf("deleting integration %q: %w", id, err)
	}
	n, _ := res.RowsAffected()
	if n == 0 {
		return fmt.Errorf("integration %q not found", id)
	}
	return nil
}

func scanIntegration(rows *sql.Rows) (*config.IntegrationConfig, error) {
	cfg := &config.IntegrationConfig{}
	var credJSON, servJSON string
	var authJSON sql.NullString
	var enabled int
	err := rows.Scan(
		&cfg.ID, &cfg.Name, &cfg.Type, &enabled,
		&credJSON, &authJSON, &servJSON,
		&cfg.CreatedAt, &cfg.UpdatedAt,
	)
	if err != nil {
		return nil, fmt.Errorf("scanning integration: %w", err)
	}

	cfg.Enabled = enabled != 0
	if err := json.Unmarshal([]byte(credJSON), &cfg.Credentials); err != nil {
		return nil, fmt.Errorf("parsing credentials: %w", err)
	}
	if authJSON.Valid && authJSON.String != "" {
		var token oauth2.Token
		if err := json.Unmarshal([]byte(authJSON.String), &token); err != nil {
			return nil, fmt.Errorf("parsing auth: %w", err)
		}
		cfg.Auth = &token
	}
	if err := json.Unmarshal([]byte(servJSON), &cfg.Services); err != nil {
		return nil, fmt.Errorf("parsing services: %w", err)
	}
	return cfg, nil
}
