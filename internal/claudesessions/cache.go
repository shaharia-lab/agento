package claudesessions

import (
	"context"
	"database/sql"
	"log/slog"
	"sync"
	"time"
)

// CacheTTL is the duration after which the SQLite-backed cache is considered
// stale and an incremental rescan is triggered.
const CacheTTL = 1 * time.Hour

// Cache is a SQLite-backed cache of Claude Code session summaries with
// TTL-based invalidation and incremental scanning. It is safe for concurrent use.
type Cache struct {
	mu     sync.Mutex
	db     *sql.DB
	logger *slog.Logger
}

// NewCache creates a new Cache backed by the given SQLite database.
func NewCache(db *sql.DB, logger *slog.Logger) *Cache {
	return &Cache{
		db:     db,
		logger: logger,
	}
}

// StartBackgroundScan runs an incremental scan in a background goroutine so
// the server starts immediately while the cache is being populated.
func (c *Cache) StartBackgroundScan() {
	go func() {
		c.logger.Info("claude sessions: starting background scan")
		c.mu.Lock()
		defer c.mu.Unlock()

		if _, err := IncrementalScan(c.db, c.logger); err != nil {
			c.logger.Warn("claude sessions: background scan failed", "error", err)
			return
		}
		c.logger.Info("claude sessions: background scan complete")
	}()
}

// List returns all cached session summaries. If the cache has expired,
// an incremental rescan is performed before returning.
func (c *Cache) List() []ClaudeSessionSummary {
	c.mu.Lock()
	defer c.mu.Unlock()

	if c.isFresh() {
		sessions, err := c.loadAll()
		if err != nil {
			c.logger.Warn("claude sessions: failed to load from cache", "error", err)
			return []ClaudeSessionSummary{}
		}
		return sessions
	}

	sessions, err := IncrementalScan(c.db, c.logger)
	if err != nil {
		c.logger.Warn("claude sessions: refresh scan failed", "error", err)
		// Try returning stale data.
		stale, loadErr := c.loadAll()
		if loadErr == nil && len(stale) > 0 {
			return stale
		}
		return []ClaudeSessionSummary{}
	}
	return sessions
}

// Invalidate resets the cache metadata so the next List() call triggers a rescan.
func (c *Cache) Invalidate() {
	c.mu.Lock()
	defer c.mu.Unlock()

	ctx := context.Background()
	_, err := c.db.ExecContext(ctx,
		`INSERT INTO claude_cache_metadata (id, last_scanned_at) VALUES (1, ?)
		 ON CONFLICT(id) DO UPDATE SET last_scanned_at = excluded.last_scanned_at`,
		time.Time{},
	)
	if err != nil {
		c.logger.Warn("claude sessions: failed to invalidate cache", "error", err)
	}
}

// isFresh returns true if the cache was scanned within CacheTTL.
func (c *Cache) isFresh() bool {
	ctx := context.Background()
	var lastScanned time.Time
	err := c.db.QueryRowContext(ctx, "SELECT last_scanned_at FROM claude_cache_metadata WHERE id = 1").Scan(&lastScanned)
	if err != nil {
		return false
	}
	return time.Since(lastScanned) < CacheTTL
}

// loadAll queries all rows from claude_session_cache ordered by last_activity desc.
func (c *Cache) loadAll() ([]ClaudeSessionSummary, error) {
	ctx := context.Background()
	rows, err := c.db.QueryContext(ctx, `
		SELECT session_id, project_path, preview, start_time, last_activity,
		       message_count, input_tokens, output_tokens, cache_creation_tokens,
		       cache_read_tokens, git_branch, model, cwd
		FROM claude_session_cache
		ORDER BY last_activity DESC`)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			c.logger.Error("failed to close rows", "error", cerr)
		}
	}()

	var sessions []ClaudeSessionSummary
	for rows.Next() {
		var s ClaudeSessionSummary
		if err := rows.Scan(
			&s.SessionID, &s.ProjectPath, &s.Preview,
			&s.StartTime, &s.LastActivity, &s.MessageCount,
			&s.Usage.InputTokens, &s.Usage.OutputTokens,
			&s.Usage.CacheCreationTokens, &s.Usage.CacheReadTokens,
			&s.GitBranch, &s.Model, &s.CWD,
		); err != nil {
			return nil, err
		}
		sessions = append(sessions, s)
	}
	if sessions == nil {
		sessions = []ClaudeSessionSummary{}
	}
	return sessions, rows.Err()
}
