package claudesessions

import (
	"context"
	"database/sql"
	"log/slog"
	"sync"
	"time"

	"github.com/shaharia-lab/agento/internal/eventbus"
	"github.com/shaharia-lab/agento/internal/pricing"
)

// CacheTTL is the duration after which the SQLite-backed cache is considered
// stale and an incremental rescan is triggered.
const CacheTTL = 1 * time.Hour

// pricingRevUnknown marks a resolver snapshot whose revision could not be
// read; it never matches a stored revision, so a degraded pricing store
// behaves as "cost data may be stale" rather than asserting it is fresh.
const pricingRevUnknown int64 = -1

// packagePricing holds the process-wide pricing snapshot. It is set once by
// Cache.WithPricingStore during startup wiring, before any scan or insight
// run, and read by the per-message cost accumulator and the read-time
// analytics paths. The mutex serializes refresh against reads.
var packagePricing = struct {
	sync.RWMutex
	resolver *pricing.Resolver
	revision int64
}{revision: pricingRevUnknown}

// defaultPricingResolver returns the current process-wide resolver, or nil
// when pricing has not been wired (unit tests, tooling) — cost accumulation
// is inert then, matching the pre-#186 behavior of "no stored cost".
func defaultPricingResolver() *pricing.Resolver {
	packagePricing.RLock()
	defer packagePricing.RUnlock()
	return packagePricing.resolver
}

// currentPricingRevision returns the fingerprint of the catalog this process
// last loaded, or pricingRevUnknown when no pricing store is wired.
func currentPricingRevision() int64 {
	packagePricing.RLock()
	defer packagePricing.RUnlock()
	return packagePricing.revision
}

// pricingChanged reports whether the stored costs were computed under a
// different catalog than the one now loaded. Unknown (no pricing wired) is
// never a change — an unpriced process must not force an endless re-scan.
func (c *Cache) pricingChanged() bool {
	live := currentPricingRevision()
	return live != pricingRevUnknown && storedPricingRevision(c.db) != live
}

// Cache is a SQLite-backed cache of Claude Code session summaries with
// TTL-based invalidation and incremental scanning. It is safe for concurrent use.
type Cache struct {
	mu      sync.Mutex
	db      *sql.DB
	logger  *slog.Logger
	bus     eventbus.EventBus // optional; publishes session events on scan
	pricing *pricing.Store    // optional; enables catalog-backed cost computation
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

// WithPricingStore attaches the pricing catalog and performs the startup seed:
// built-in rates are (re-)upserted — never over user-modified rows — and the
// process-wide resolver snapshot is loaded. Cost computation is inert until
// this runs, so it must be called before any scan or insight processing.
func (c *Cache) WithPricingStore(store *pricing.Store) *Cache {
	c.pricing = store
	ctx := context.Background()
	written, err := store.Seed(ctx)
	if err != nil {
		c.logger.Warn("claude sessions: pricing seed failed; cost computation degraded", "error", err)
		return c
	}
	if written > 0 {
		c.logger.Info("claude sessions: pricing catalog seeded", "rows_written", written)
	}
	c.refreshPricingResolver()
	return c
}

// refreshPricingResolver reloads the in-memory resolver snapshot when the
// catalog's revision moved. A rate edit changes the revision; the next cache
// List (hourly at worst) picks the new snapshot up, and read-time cost
// computation follows immediately.
func (c *Cache) refreshPricingResolver() {
	if c.pricing == nil {
		return
	}
	ctx := context.Background()
	rev, err := c.pricing.Revision(ctx)
	if err != nil {
		c.logger.Warn("claude sessions: pricing revision unreadable; keeping previous snapshot", "error", err)
		return
	}
	packagePricing.RLock()
	current := packagePricing.revision
	packagePricing.RUnlock()
	if current == rev {
		return
	}
	rates, err := c.pricing.Snapshot(ctx)
	if err != nil {
		c.logger.Warn("claude sessions: pricing snapshot failed; keeping previous snapshot", "error", err)
		return
	}
	packagePricing.Lock()
	packagePricing.resolver = pricing.NewResolver(rates)
	packagePricing.revision = rev
	packagePricing.Unlock()
	if current != pricingRevUnknown {
		c.logger.Info("claude sessions: pricing catalog changed; costs recompute from the new rates",
			"rates", len(rates))
	}
}

// notify publishes a session event to the bus if one is configured.
// isNew distinguishes a newly discovered session (EventSessionDiscovered) from
// a session whose JSONL file changed since last scan (EventSessionUpdated).
func (c *Cache) notify(sessionID, filePath string, isNew bool) {
	if c.bus == nil {
		return
	}
	eventType := eventbus.EventSessionUpdated
	if isNew {
		eventType = eventbus.EventSessionDiscovered
	}
	c.bus.Publish(eventType, map[string]string{
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

	// A rate edit must not wait for the hourly TTL to take effect in the cost
	// figures: refresh the resolver snapshot first. Since #188 cost is stored on
	// the row rather than recomputed here, so a changed catalog also has to skip
	// the freshness short-circuit — the scan is what re-reads the transcripts
	// and re-prices them, and cached rows would otherwise serve stale costs
	// indefinitely.
	c.refreshPricingResolver()

	if c.isFresh() && !c.pricingChanged() {
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

// loadAll queries all cached sessions, with their sub-agent roll-up, ordered by
// last_activity desc.
func (c *Cache) loadAll() ([]ClaudeSessionSummary, error) {
	return querySessionSummaries(c.db, c.logger)
}

// ListSubagents returns the cached sub-agent transcripts of one session.
// It takes the cache mutex, so it must not be called from a method that
// already holds it.
func (c *Cache) ListSubagents(sessionID string) []ClaudeSubagent {
	c.mu.Lock()
	defer c.mu.Unlock()
	subagents, err := ListSubagents(c.db, c.logger, sessionID)
	if err != nil {
		c.logger.Warn("claude sessions: failed to list sub-agents", "session_id", sessionID, "error", err)
		return []ClaudeSubagent{}
	}
	return subagents
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
	if row.Scan(&title) != nil {
		return ""
	}
	return title
}

// GetTitles returns the cached native and AI titles for a session. Both are
// empty when the session is not cached or Claude Code recorded no title.
func (c *Cache) GetTitles(sessionID string) (nativeTitle, aiTitle string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	row := c.db.QueryRowContext(context.Background(),
		`SELECT native_title, ai_title FROM claude_session_cache WHERE session_id = ?`,
		sessionID,
	)
	if row.Scan(&nativeTitle, &aiTitle) != nil {
		return "", ""
	}
	return nativeTitle, aiTitle
}

// UpdateFavorite sets the is_favorite flag for the given session. The value
// is preserved across incremental rescans (same pattern as custom_title).
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
	if row.Scan(&v) != nil {
		return false
	}
	return v
}
