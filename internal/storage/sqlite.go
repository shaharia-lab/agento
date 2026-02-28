package storage

import (
	"context"
	"database/sql"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"time"

	_ "modernc.org/sqlite" // Pure Go SQLite driver.
)

// migration represents a single schema migration step.
type migration struct {
	version int
	sql     string
}

// migrations holds all schema migrations in order. Each migration is applied
// exactly once, tracked by the schema_migrations table.
var migrations = []migration{
	{
		version: 1,
		sql: `
CREATE TABLE agents (
    slug            TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    description     TEXT NOT NULL DEFAULT '',
    model           TEXT NOT NULL DEFAULT 'claude-sonnet-4-6',
    thinking        TEXT NOT NULL DEFAULT 'adaptive',
    permission_mode TEXT NOT NULL DEFAULT 'bypass',
    system_prompt   TEXT NOT NULL DEFAULT '',
    capabilities    TEXT NOT NULL DEFAULT '{}',
    created_at      DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE chat_sessions (
    id                          TEXT PRIMARY KEY,
    title                       TEXT NOT NULL DEFAULT '',
    agent_slug                  TEXT NOT NULL,
    sdk_session_id              TEXT NOT NULL DEFAULT '',
    working_directory           TEXT NOT NULL DEFAULT '',
    model                       TEXT NOT NULL DEFAULT '',
    settings_profile_id         TEXT NOT NULL DEFAULT '',
    total_input_tokens          INTEGER NOT NULL DEFAULT 0,
    total_output_tokens         INTEGER NOT NULL DEFAULT 0,
    total_cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    total_cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    created_at                  DATETIME NOT NULL,
    updated_at                  DATETIME NOT NULL
);

CREATE TABLE chat_messages (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
    role       TEXT NOT NULL,
    content    TEXT NOT NULL DEFAULT '',
    blocks     TEXT NOT NULL DEFAULT '[]',
    timestamp  DATETIME NOT NULL
);
CREATE INDEX idx_chat_messages_session ON chat_messages(session_id, id);

CREATE TABLE integrations (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    type        TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 0,
    credentials TEXT NOT NULL DEFAULT '{}',
    auth        TEXT,
    services    TEXT NOT NULL DEFAULT '{}',
    created_at  DATETIME NOT NULL,
    updated_at  DATETIME NOT NULL
);

CREATE TABLE user_settings (
    id                     INTEGER PRIMARY KEY CHECK (id = 1),
    default_working_dir    TEXT NOT NULL DEFAULT '',
    default_model          TEXT NOT NULL DEFAULT '',
    onboarding_complete    INTEGER NOT NULL DEFAULT 0,
    appearance_dark_mode   INTEGER NOT NULL DEFAULT 0,
    appearance_font_size   INTEGER NOT NULL DEFAULT 0,
    appearance_font_family TEXT NOT NULL DEFAULT ''
);

`,
	},
	{
		version: 2,
		sql: `
CREATE TABLE claude_session_cache (
    session_id    TEXT NOT NULL,
    project_path  TEXT NOT NULL,
    file_path     TEXT NOT NULL,
    file_mtime    DATETIME NOT NULL,
    preview       TEXT NOT NULL DEFAULT '',
    start_time    DATETIME NOT NULL,
    last_activity DATETIME NOT NULL,
    message_count INTEGER NOT NULL DEFAULT 0,
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    git_branch    TEXT NOT NULL DEFAULT '',
    model         TEXT NOT NULL DEFAULT '',
    cwd           TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (session_id, project_path)
);

CREATE TABLE claude_cache_metadata (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    last_scanned_at DATETIME NOT NULL
);
`,
	},
}

// NewSQLiteDB opens (or creates) a SQLite database at dbPath, configures
// pragmas for WAL mode and foreign keys, and runs any pending schema
// migrations. Returns true as the second value if the database was newly
// created (i.e. no tables existed before this call).
func NewSQLiteDB(dbPath string) (*sql.DB, bool, error) {
	if err := os.MkdirAll(filepath.Dir(dbPath), 0750); err != nil {
		return nil, false, fmt.Errorf("creating database directory: %w", err)
	}

	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		return nil, false, fmt.Errorf("opening database: %w", err)
	}

	// SQLite is single-writer; serialize all access through one connection
	// to avoid SQLITE_BUSY errors from concurrent goroutines.
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	db.SetConnMaxLifetime(0)

	ctx := context.Background()

	// Configure SQLite pragmas.
	pragmas := []string{
		"PRAGMA journal_mode=WAL",
		"PRAGMA busy_timeout=5000",
		"PRAGMA foreign_keys=ON",
		"PRAGMA synchronous=NORMAL",
	}
	for _, p := range pragmas {
		if _, pragmaErr := db.ExecContext(ctx, p); pragmaErr != nil {
			if cerr := db.Close(); cerr != nil {
				log.Printf("failed to close database after pragma error: %v", cerr)
			}
			return nil, false, fmt.Errorf("setting pragma %q: %w", p, pragmaErr)
		}
	}

	freshDB, err := runMigrations(ctx, db)
	if err != nil {
		if cerr := db.Close(); cerr != nil {
			log.Printf("failed to close database after migration error: %v", cerr)
		}
		return nil, false, fmt.Errorf("running migrations: %w", err)
	}

	return db, freshDB, nil
}

// runMigrations ensures the schema_migrations table exists and applies any
// pending migrations. Returns true if migration version 1 was applied during
// this call (indicating a fresh database).
func runMigrations(ctx context.Context, db *sql.DB) (bool, error) {
	// Ensure the migrations tracking table exists.
	_, err := db.ExecContext(ctx, `CREATE TABLE IF NOT EXISTS schema_migrations (
		version    INTEGER PRIMARY KEY,
		applied_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
	)`)
	if err != nil {
		return false, fmt.Errorf("creating schema_migrations table: %w", err)
	}

	current, err := currentVersion(ctx, db)
	if err != nil {
		return false, err
	}

	freshDB := false
	for _, m := range migrations {
		if m.version <= current {
			continue
		}
		if m.version == 1 {
			freshDB = true
		}
		if err := applyMigration(ctx, db, m); err != nil {
			return false, err
		}
	}

	return freshDB, nil
}

// applyMigration runs a single schema migration inside a transaction.
func applyMigration(ctx context.Context, db *sql.DB, m migration) error {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin migration %d: %w", m.version, err)
	}

	if _, err := tx.ExecContext(ctx, m.sql); err != nil {
		if rbErr := tx.Rollback(); rbErr != nil {
			log.Printf("failed to rollback migration %d: %v", m.version, rbErr)
		}
		return fmt.Errorf("migration %d: %w", m.version, err)
	}

	if _, err := tx.ExecContext(ctx,
		"INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)",
		m.version, time.Now().UTC(),
	); err != nil {
		if rbErr := tx.Rollback(); rbErr != nil {
			log.Printf("failed to rollback migration %d: %v", m.version, rbErr)
		}
		return fmt.Errorf("recording migration %d: %w", m.version, err)
	}

	if err := tx.Commit(); err != nil {
		return fmt.Errorf("commit migration %d: %w", m.version, err)
	}
	return nil
}

func currentVersion(ctx context.Context, db *sql.DB) (int, error) {
	var v int
	err := db.QueryRowContext(ctx, "SELECT COALESCE(MAX(version), 0) FROM schema_migrations").Scan(&v)
	if err != nil {
		return 0, fmt.Errorf("querying current schema version: %w", err)
	}
	return v, nil
}
