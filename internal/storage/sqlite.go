package storage

import (
	"context"
	"database/sql"
	"fmt"
	"log/slog"
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
	{
		version: 3,
		sql: `
CREATE TABLE scheduled_tasks (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    description         TEXT NOT NULL DEFAULT '',
    prompt              TEXT NOT NULL,
    agent_slug          TEXT NOT NULL DEFAULT '',
    working_directory   TEXT NOT NULL DEFAULT '',
    model               TEXT NOT NULL DEFAULT '',
    settings_profile_id TEXT NOT NULL DEFAULT '',
    timeout_minutes     INTEGER NOT NULL DEFAULT 30,
    schedule_type       TEXT NOT NULL DEFAULT 'one_off',
    schedule_config     TEXT NOT NULL DEFAULT '{}',
    stop_after_count    INTEGER NOT NULL DEFAULT 0,
    stop_after_time     DATETIME,
    status              TEXT NOT NULL DEFAULT 'active',
    run_count           INTEGER NOT NULL DEFAULT 0,
    last_run_at         DATETIME,
    last_run_status     TEXT NOT NULL DEFAULT '',
    next_run_at         DATETIME,
    created_at          DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_scheduled_tasks_status ON scheduled_tasks(status);
CREATE INDEX idx_scheduled_tasks_next_run ON scheduled_tasks(next_run_at);

CREATE TABLE job_history (
    id                          TEXT PRIMARY KEY,
    task_id                     TEXT NOT NULL REFERENCES scheduled_tasks(id) ON DELETE CASCADE,
    task_name                   TEXT NOT NULL,
    agent_slug                  TEXT NOT NULL DEFAULT '',
    status                      TEXT NOT NULL DEFAULT 'running',
    started_at                  DATETIME NOT NULL,
    finished_at                 DATETIME,
    duration_ms                 INTEGER NOT NULL DEFAULT 0,
    chat_session_id             TEXT NOT NULL DEFAULT '',
    model                       TEXT NOT NULL DEFAULT '',
    prompt_preview              TEXT NOT NULL DEFAULT '',
    error_message               TEXT NOT NULL DEFAULT '',
    total_input_tokens          INTEGER NOT NULL DEFAULT 0,
    total_output_tokens         INTEGER NOT NULL DEFAULT 0,
    total_cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    total_cache_read_tokens     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_job_history_task ON job_history(task_id, started_at DESC);
CREATE INDEX idx_job_history_started ON job_history(started_at DESC);
`,
	},
	{
		version: 4,
		sql: `
ALTER TABLE user_settings ADD COLUMN notification_settings TEXT NOT NULL DEFAULT '{}';
ALTER TABLE user_settings ADD COLUMN event_bus_worker_pool_size INTEGER NOT NULL DEFAULT 3;

CREATE TABLE notification_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    event_type TEXT    NOT NULL,
    provider   TEXT    NOT NULL,
    subject    TEXT    NOT NULL DEFAULT '',
    status     TEXT    NOT NULL DEFAULT 'sent',
    error_msg  TEXT    NOT NULL DEFAULT '',
    created_at DATETIME NOT NULL
);

CREATE INDEX idx_notification_log_created ON notification_log(created_at DESC);
`,
	},
	{
		version: 5,
		sql:     `ALTER TABLE job_history ADD COLUMN response_text TEXT NOT NULL DEFAULT '';`,
	},
	{
		version: 6,
		sql:     `ALTER TABLE scheduled_tasks ADD COLUMN save_output INTEGER NOT NULL DEFAULT 0;`,
	},
	{
		version: 7,
		sql:     `ALTER TABLE claude_session_cache ADD COLUMN custom_title TEXT NOT NULL DEFAULT '';`,
	},
	{
		version: 8,
		sql: `ALTER TABLE chat_sessions ADD COLUMN is_favorite INTEGER NOT NULL DEFAULT 0;
ALTER TABLE claude_session_cache ADD COLUMN is_favorite INTEGER NOT NULL DEFAULT 0;`,
	},
	{
		version: 9,
		sql: `
CREATE TABLE IF NOT EXISTS session_insights (
    session_id                  TEXT PRIMARY KEY,
    processor_version           INTEGER NOT NULL DEFAULT 0,
    scanned_at                  DATETIME NOT NULL,

    turn_count                  INTEGER NOT NULL DEFAULT 0,
    steps_per_turn_avg          REAL    NOT NULL DEFAULT 0,

    autonomy_score              REAL    NOT NULL DEFAULT 0,

    tool_calls_total            INTEGER NOT NULL DEFAULT 0,
    tool_breakdown              TEXT    NOT NULL DEFAULT '{}',
    tool_error_rate             REAL    NOT NULL DEFAULT 0,

    total_duration_ms           INTEGER NOT NULL DEFAULT 0,
    thinking_time_ms            INTEGER NOT NULL DEFAULT 0,

    cache_hit_rate              REAL    NOT NULL DEFAULT 0,
    tokens_per_turn_avg         REAL    NOT NULL DEFAULT 0,
    cost_estimate_usd           REAL    NOT NULL DEFAULT 0,

    tool_error_count            INTEGER NOT NULL DEFAULT 0,
    has_errors                  INTEGER NOT NULL DEFAULT 0,

    max_consecutive_tool_calls  INTEGER NOT NULL DEFAULT 0,
    longest_autonomous_chain    INTEGER NOT NULL DEFAULT 0,

    avg_user_response_time_ms   REAL    NOT NULL DEFAULT 0,
    avg_claude_response_time_ms REAL    NOT NULL DEFAULT 0,

    session_type                TEXT    NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_session_insights_version ON session_insights(processor_version);
`,
	},
	{
		version: 10,
		sql: `
ALTER TABLE user_settings ADD COLUMN public_url TEXT NOT NULL DEFAULT '';

ALTER TABLE integrations ADD COLUMN webhook_secret TEXT NOT NULL DEFAULT '';
ALTER TABLE integrations ADD COLUMN webhook_status TEXT NOT NULL DEFAULT '';
ALTER TABLE integrations ADD COLUMN webhook_error TEXT NOT NULL DEFAULT '';

CREATE TABLE trigger_rules (
    id              TEXT PRIMARY KEY,
    integration_id  TEXT NOT NULL REFERENCES integrations(id) ON DELETE CASCADE,
    name            TEXT NOT NULL DEFAULT '',
    agent_slug      TEXT NOT NULL,
    enabled         INTEGER NOT NULL DEFAULT 1,
    filter_prefix   TEXT NOT NULL DEFAULT '',
    filter_keywords TEXT NOT NULL DEFAULT '[]',
    filter_chat_ids TEXT NOT NULL DEFAULT '[]',
    created_at      DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX idx_trigger_rules_integration ON trigger_rules(integration_id);

CREATE TABLE telegram_processed_updates (
    integration_id  TEXT NOT NULL,
    update_id       INTEGER NOT NULL,
    processed_at    DATETIME NOT NULL,
    PRIMARY KEY (integration_id, update_id)
);
`,
	},
	{
		version: 11,
		sql: `
CREATE TABLE claude_subagent_cache (
    parent_session_id     TEXT NOT NULL,
    agent_id              TEXT NOT NULL,
    file_path             TEXT NOT NULL,
    file_mtime            DATETIME NOT NULL,
    agent_type            TEXT NOT NULL DEFAULT '',
    description           TEXT NOT NULL DEFAULT '',
    tool_use_id           TEXT NOT NULL DEFAULT '',
    start_time            DATETIME,
    last_activity         DATETIME,
    message_count         INTEGER NOT NULL DEFAULT 0,
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    model                 TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (parent_session_id, agent_id)
);
CREATE INDEX idx_subagent_parent ON claude_subagent_cache(parent_session_id);
CREATE INDEX idx_subagent_file_path ON claude_subagent_cache(file_path);
`,
	},
	{
		version: 12,
		sql: `
ALTER TABLE claude_session_cache ADD COLUMN cache_creation_5m_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE claude_session_cache ADD COLUMN cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0;

ALTER TABLE claude_subagent_cache ADD COLUMN cache_creation_5m_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE claude_subagent_cache ADD COLUMN cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0;

-- scanner_version records the reader version the cached rows were produced by.
-- When the code's CurrentScannerVersion moves ahead of it, the next scan
-- re-reads every transcript even though no file mtime changed. Existing rows
-- predate the cache-TTL split, so version 0 forces exactly one re-read.
ALTER TABLE claude_cache_metadata ADD COLUMN scanner_version INTEGER NOT NULL DEFAULT 0;
`,
	},
	{
		version: 13,
		sql: `
-- Titles Claude Code records in the transcript itself. Unlike custom_title
-- (set only through Agento's UI) these are refreshed on every rescan, so the
-- two can never overwrite each other.
ALTER TABLE claude_session_cache ADD COLUMN native_title TEXT NOT NULL DEFAULT '';
ALTER TABLE claude_session_cache ADD COLUMN ai_title TEXT NOT NULL DEFAULT '';
`,
	},
	{
		version: 14,
		sql: `
-- message_count now holds conversational turns rather than raw JSONL events;
-- event_count preserves the old meaning. Existing rows are recomputed by the
-- CurrentScannerVersion 2 -> 3 bump, which forces one full re-read.
ALTER TABLE claude_session_cache ADD COLUMN event_count INTEGER NOT NULL DEFAULT 0;
`,
	},
	{
		version: 15,
		sql: `
-- Session metadata Claude Code records in the transcript. All are derived from
-- the JSONL, so unlike custom_title they refresh on every rescan. Populated by
-- the CurrentScannerVersion 3 -> 4 bump, which forces one full re-read.
ALTER TABLE claude_session_cache ADD COLUMN agent_name       TEXT NOT NULL DEFAULT '';
ALTER TABLE claude_session_cache ADD COLUMN permission_mode  TEXT NOT NULL DEFAULT '';
ALTER TABLE claude_session_cache ADD COLUMN mode             TEXT NOT NULL DEFAULT '';
ALTER TABLE claude_session_cache ADD COLUMN relocated_cwd    TEXT NOT NULL DEFAULT '';
ALTER TABLE claude_session_cache ADD COLUMN worktree_name    TEXT NOT NULL DEFAULT '';
ALTER TABLE claude_session_cache ADD COLUMN worktree_branch  TEXT NOT NULL DEFAULT '';
ALTER TABLE claude_session_cache ADD COLUMN original_branch  TEXT NOT NULL DEFAULT '';
ALTER TABLE claude_session_cache ADD COLUMN compaction_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE claude_session_cache ADD COLUMN dropped_tokens   INTEGER NOT NULL DEFAULT 0;

-- Pull requests a session produced. One session can link several, and the same
-- pr-link event is re-emitted on every resume, so the URL is part of the key.
CREATE TABLE claude_session_pr (
    session_id    TEXT NOT NULL,
    pr_url        TEXT NOT NULL,
    pr_number     INTEGER NOT NULL DEFAULT 0,
    pr_repository TEXT NOT NULL DEFAULT '',
    first_seen_at DATETIME,
    PRIMARY KEY (session_id, pr_url)
);
CREATE INDEX idx_session_pr_session ON claude_session_pr(session_id);
`,
	},
	{
		version: 16,
		sql: `
-- Attribution of tool calls to the skill, plugin and MCP server that made them.
-- Stored as JSON maps like tool_breakdown. Populated by the
-- CurrentProcessorVersion 3 -> 4 bump, which makes NeedsProcessing return every
-- existing session.
ALTER TABLE session_insights ADD COLUMN skill_breakdown      TEXT    NOT NULL DEFAULT '{}';
ALTER TABLE session_insights ADD COLUMN plugin_breakdown     TEXT    NOT NULL DEFAULT '{}';
ALTER TABLE session_insights ADD COLUMN mcp_server_breakdown TEXT    NOT NULL DEFAULT '{}';
ALTER TABLE session_insights ADD COLUMN mcp_tool_breakdown   TEXT    NOT NULL DEFAULT '{}';
ALTER TABLE session_insights ADD COLUMN effort_breakdown     TEXT    NOT NULL DEFAULT '{}';
ALTER TABLE session_insights ADD COLUMN unattributed_calls   INTEGER NOT NULL DEFAULT 0;
`,
	},
	{
		version: 17,
		sql: `
-- The time-versioned pricing catalog (#186). Rates are effective-dated, so the
-- uniqueness key is (model_pattern, effective_from): a price change is a new
-- row, never an edit of the row history was priced against. Seeded from the
-- built-in catalog on startup; user_modified rows are never re-seeded.
CREATE TABLE model_pricing (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider TEXT NOT NULL DEFAULT '',
    model_pattern TEXT NOT NULL,
    match_type TEXT NOT NULL DEFAULT 'exact',
    display_name TEXT NOT NULL DEFAULT '',
    input_per_mtok REAL NOT NULL DEFAULT 0,
    output_per_mtok REAL NOT NULL DEFAULT 0,
    cache_write_5m_per_mtok REAL NOT NULL DEFAULT 0,
    cache_write_1h_per_mtok REAL NOT NULL DEFAULT 0,
    cache_read_per_mtok REAL NOT NULL DEFAULT 0,
    effective_from DATETIME NOT NULL,
    source TEXT NOT NULL DEFAULT '',
    is_builtin INTEGER NOT NULL DEFAULT 0,
    user_modified INTEGER NOT NULL DEFAULT 0,
    created_at DATETIME NOT NULL,
    updated_at DATETIME NOT NULL,
    UNIQUE(model_pattern, effective_from)
);
`,
	},
	{
		version: 18,
		sql: `
-- Non-Anthropic model pricing (#187). billable separates a model that
-- deliberately costs nothing (Claude Code's <synthetic> placeholder, embedding
-- models) from one whose rates were never filled in: both price at $0.00, but
-- only the latter belongs in the unknown-pricing bucket. estimated marks a rate
-- that is a best effort rather than a published price, such as the bare family
-- aliases ("opus") that name no concrete model. Existing rows are real,
-- published Anthropic rates, so the defaults are correct for them.
ALTER TABLE model_pricing ADD COLUMN billable INTEGER NOT NULL DEFAULT 1;
ALTER TABLE model_pricing ADD COLUMN estimated INTEGER NOT NULL DEFAULT 0;
`,
	},
	{
		version: 19,
		sql: `
-- Per-session cost, stored rather than derived (#188). Cost was previously
-- recomputed on every read, which is why a rate edit needed no re-scan. Storing
-- it makes the session list, the detail page and the analytics totals read one
-- number instead of three derivations -- but it also means a rate change no
-- longer reaches cached rows on its own, which is what pricing_rev below is for.
--
-- Zero defaults are correct: a row that predates this migration has no cost yet,
-- and the scanner-version bump re-reads every transcript to populate it.
ALTER TABLE claude_session_cache ADD COLUMN input_cost_usd REAL NOT NULL DEFAULT 0;
ALTER TABLE claude_session_cache ADD COLUMN output_cost_usd REAL NOT NULL DEFAULT 0;
ALTER TABLE claude_session_cache ADD COLUMN cache_read_cost_usd REAL NOT NULL DEFAULT 0;
ALTER TABLE claude_session_cache ADD COLUMN cache_write_cost_usd REAL NOT NULL DEFAULT 0;
ALTER TABLE claude_session_cache ADD COLUMN total_cost_usd REAL NOT NULL DEFAULT 0;

-- Newline-separated; empty means fully priced. A non-empty list makes the
-- stored total a floor, which the UI has to disclose rather than round off.
ALTER TABLE claude_session_cache ADD COLUMN unpriced_models TEXT NOT NULL DEFAULT '';
ALTER TABLE claude_session_cache ADD COLUMN unpriced_tokens INTEGER NOT NULL DEFAULT 0;

-- Delegated work is costed separately for the same reason its tokens are:
-- claude_session_cache.* stays main-thread, and the sub-agent roll-up is summed
-- back in at query time.
ALTER TABLE claude_subagent_cache ADD COLUMN input_cost_usd REAL NOT NULL DEFAULT 0;
ALTER TABLE claude_subagent_cache ADD COLUMN output_cost_usd REAL NOT NULL DEFAULT 0;
ALTER TABLE claude_subagent_cache ADD COLUMN cache_read_cost_usd REAL NOT NULL DEFAULT 0;
ALTER TABLE claude_subagent_cache ADD COLUMN cache_write_cost_usd REAL NOT NULL DEFAULT 0;
ALTER TABLE claude_subagent_cache ADD COLUMN total_cost_usd REAL NOT NULL DEFAULT 0;
ALTER TABLE claude_subagent_cache ADD COLUMN unpriced_models TEXT NOT NULL DEFAULT '';
ALTER TABLE claude_subagent_cache ADD COLUMN unpriced_tokens INTEGER NOT NULL DEFAULT 0;

-- The catalog fingerprint the cached costs were computed under. When it drifts
-- from the live catalog the stored costs are stale, and the only way to redo
-- them is to re-read the transcripts -- per-message model and timestamp are not
-- retained on the row. Mirrors how scanner_version already forces a re-read.
ALTER TABLE claude_cache_metadata ADD COLUMN pricing_rev INTEGER NOT NULL DEFAULT 0;
`,
	},
	{
		version: 20,
		sql: `
-- Attribution of tool calls to the sub-agent that made them (#202). Stored as a
-- JSON map like the other breakdowns. Populated by the CurrentProcessorVersion
-- 5 -> 6 bump, which makes NeedsProcessing return every existing session, so
-- the '{}' default is what a pre-v6 row correctly reads as until it is
-- reprocessed.
ALTER TABLE session_insights ADD COLUMN agent_breakdown TEXT NOT NULL DEFAULT '{}';
`,
	},
	{
		version: 21,
		sql: `
-- Context-length rate bands (#218). Alibaba prices by the number of input
-- tokens in a request, so one model at one effective_from has several prices
-- and the flat five columns on model_pricing cannot express it.
--
-- A child table rather than more model_pricing rows on purpose: model_pricing's
-- UNIQUE(model_pattern, effective_from) is what makes AddRate's collision
-- detection and CorrectRate's history-preserving semantics work, and a per-band
-- row would collide on that key. An untiered rate simply has zero rows here, so
-- every existing row, constraint and query is untouched.
--
-- ON DELETE CASCADE ties bands to their rate: deleting a rate through the
-- settings UI must not strand its bands, which would then re-attach to whatever
-- row later reused the id.
CREATE TABLE model_pricing_tier (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    rate_id INTEGER NOT NULL REFERENCES model_pricing(id) ON DELETE CASCADE,
    max_input_tokens INTEGER NOT NULL,
    input_per_mtok REAL NOT NULL,
    output_per_mtok REAL NOT NULL,
    cache_write_5m_per_mtok REAL NOT NULL,
    cache_write_1h_per_mtok REAL NOT NULL,
    cache_read_per_mtok REAL NOT NULL,
    UNIQUE(rate_id, max_input_tokens)
);
CREATE INDEX idx_model_pricing_tier_rate ON model_pricing_tier(rate_id);
`,
	},
}

// NewSQLiteDB opens (or creates) a SQLite database at dbPath, configures
// pragmas for WAL mode and foreign keys, and runs any pending schema
// migrations. Returns true as the second value if the database was newly
// created (i.e. no tables existed before this call).
func NewSQLiteDB(dbPath string, logger *slog.Logger) (*sql.DB, bool, error) {
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
				logger.Warn("failed to close database after pragma error", "error", cerr)
			}
			return nil, false, fmt.Errorf("setting pragma %q: %w", p, pragmaErr)
		}
	}

	freshDB, err := runMigrations(ctx, db, logger)
	if err != nil {
		if cerr := db.Close(); cerr != nil {
			logger.Warn("failed to close database after migration error", "error", cerr)
		}
		return nil, false, fmt.Errorf("running migrations: %w", err)
	}

	return db, freshDB, nil
}

// runMigrations ensures the schema_migrations table exists and applies any
// pending migrations. Returns true if migration version 1 was applied during
// this call (indicating a fresh database).
func runMigrations(ctx context.Context, db *sql.DB, logger *slog.Logger) (bool, error) {
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
		if err := applyMigration(ctx, db, m, logger); err != nil {
			return false, err
		}
	}

	return freshDB, nil
}

// applyMigration runs a single schema migration inside a transaction.
func applyMigration(ctx context.Context, db *sql.DB, m migration, logger *slog.Logger) error {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin migration %d: %w", m.version, err)
	}

	if _, err := tx.ExecContext(ctx, m.sql); err != nil {
		if rbErr := tx.Rollback(); rbErr != nil {
			logger.Warn("failed to rollback migration", "version", m.version, "error", rbErr)
		}
		return fmt.Errorf("migration %d: %w", m.version, err)
	}

	if _, err := tx.ExecContext(ctx,
		"INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)",
		m.version, time.Now().UTC(),
	); err != nil {
		if rbErr := tx.Rollback(); rbErr != nil {
			logger.Warn("failed to rollback migration", "version", m.version, "error", rbErr)
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
