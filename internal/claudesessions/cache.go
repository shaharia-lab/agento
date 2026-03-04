package claudesessions

import (
	"context"
	"database/sql"
	"log/slog"
	"sync"
	"time"

	"github.com/shaharia-lab/agento/internal/eventbus"
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
	bus    eventbus.EventBus // optional; publishes session events on scan
}

// NewCache creates a new Cache backed by the given SQLite database.
func NewCache(db *sql.DB, logger *slog.Logger) *Cache {
	return &Cache{
		db:     db,
		logger: logger,
	}
}

// WithEventBus attaches an event bus to the cache so that newly discovered or
// updated sessions trigger EventSessionDiscovered / EventSessionUpdated events.
func (c *Cache) WithEventBus(bus eventbus.EventBus) *Cache {
	c.bus = bus
	return c
}

// notify publishes a session event to the bus if one is configured.
func (c *Cache) notify(sessionID, filePath string) {
	if c.bus == nil {
		return
	}
	c.bus.Publish(eventbus.EventSessionDiscovered, map[string]string{
		eventbus.PayloadKeySessionID: sessionID,
		eventbus.PayloadKeyFilePath:  filePath,
	})
}

// StartBackgroundScan runs an incremental scan in a background goroutine so
// the server starts immediately while the cache is being populated.
func (c *Cache) StartBackgroundScan() {
	go func() {
		c.logger.Info("claude sessions: starting background scan")
		c.mu.Lock()
		defer c.mu.Unlock()

		if _, err := IncrementalScanWithNotify(c.db, c.logger, c.notify); err != nil {
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

	sessions, err := IncrementalScanWithNotify(c.db, c.logger, c.notify)
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
		SELECT session_id, project_path, preview, custom_title, is_favorite, start_time, last_activity,
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
			&s.SessionID, &s.ProjectPath, &s.Preview, &s.CustomTitle, &s.IsFavorite,
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

// UpdateCustomTitle sets a user-defined label for the given session. The title
// is preserved across incremental rescans and removed only when the underlying
// JSONL file is deleted from ~/.claude/projects/.
func (c *Cache) UpdateCustomTitle(sessionID, title string) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	_, err := c.db.ExecContext(context.Background(),
		`UPDATE claude_session_cache SET custom_title = ? WHERE session_id = ?`,
		title, sessionID,
	)
	return err
}

// GetCustomTitle returns the stored custom_title for the given session ID.
// Returns an empty string if the session is not cached or has no custom title.
func (c *Cache) GetCustomTitle(sessionID string) string {
	c.mu.Lock()
	defer c.mu.Unlock()
	var title string
	row := c.db.QueryRowContext(context.Background(),
		`SELECT custom_title FROM claude_session_cache WHERE session_id = ?`,
		sessionID,
	)
	if err := row.Scan(&title); err != nil {
		return ""
	}
	return title
}

// UpdateFavorite sets the is_favorite flag for the given session. The value is
// preserved across incremental rescans (same pattern as custom_title).
func (c *Cache) UpdateFavorite(sessionID string, isFavorite bool) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	_, err := c.db.ExecContext(context.Background(),
		`UPDATE claude_session_cache SET is_favorite = ? WHERE session_id = ?`,
		isFavorite, sessionID,
	)
	return err
}

// GetFavorite returns the stored is_favorite flag for the given session ID.
func (c *Cache) GetFavorite(sessionID string) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	var v bool
	row := c.db.QueryRowContext(context.Background(),
		`SELECT is_favorite FROM claude_session_cache WHERE session_id = ?`,
		sessionID,
	)
	if err := row.Scan(&v); err != nil {
		return false
	}
	return v
}
