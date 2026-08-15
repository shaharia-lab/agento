package claudesessions

import (
	"context"
	"database/sql"
	"log/slog"
	"os"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/shaharia-lab/agento/internal/eventbus"
	"github.com/shaharia-lab/agento/internal/pricing"
)

// CacheTTL is the duration after which the SQLite-backed cache is considered
// stale and an incremental rescan is triggered.
const CacheTTL = 1 * time.Hour

// coldStartScanWait bounds how long a read blocks when the cache holds nothing
// to serve. Only the cold path waits: a first run has no rows, and handing back
// an empty list would look like "no sessions" rather than "not scanned yet".
// Every warm read returns immediately, however stale.
const coldStartScanWait = 30 * time.Second

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

// idleThresholdChanged reports whether the stored active durations were
// computed under a different idle-gap threshold than the one now configured.
// Like pricingChanged, this makes a read trigger the re-read the stored
// figures need — a settings save also triggers one directly, but a save that
// raced a restart must not leave the durations quietly wrong.
func (c *Cache) idleThresholdChanged() bool {
	_, stale := idleThresholdStaleness(c.db)
	return stale
}

// Cache is a SQLite-backed cache of Claude Code session summaries with
// TTL-based invalidation and incremental scanning. It is safe for concurrent use.
type Cache struct {
	// mu guards the fields below and the short metadata statements. It is
	// deliberately NOT held across a scan: a full re-read is ~18s on a large
	// corpus, and holding the lock for that long is the stall this design
	// exists to avoid.
	mu sync.Mutex
	// scanning admits exactly one scan at a time, so overlapping triggers
	// (two rate saves in a row) cannot queue a second full re-read.
	scanning bool
	// scanDone is closed when the in-flight scan finishes. Only the cold-cache
	// path waits on it.
	scanDone chan struct{}

	db      *sql.DB
	logger  *slog.Logger
	bus     eventbus.EventBus // optional; publishes session events on scan
	pricing *pricing.Store    // optional; enables catalog-backed cost computation

	// analytics memoizes built reports. See analytics_cache.go: the report is
	// a dozen passes over a full corpus load, and a dashboard fires two or
	// three of them per open.
	analytics *analyticsMemo

	// filesDone and filesTotal report the running scan's progress. Atomics
	// rather than mutex-guarded fields because the status endpoint polls them
	// every few seconds while the scan writes them once per batch, and neither
	// should ever wait on the other.
	filesDone  atomic.Int64
	filesTotal atomic.Int64
}

// NewCache creates a new Cache backed by the given SQLite database.
func NewCache(db *sql.DB, logger *slog.Logger) *Cache {
	return &Cache{
		db:        db,
		logger:    logger,
		analytics: newAnalyticsMemo(),
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

// pricingRefreshMu serializes refreshPricingResolver's read-compare-snapshot-
// store sequence. packagePricing's own RWMutex only makes each access atomic,
// not the sequence: two concurrent refreshes could both observe the old
// revision, both snapshot, and the later-finishing one could store an older
// resolver under the newer revision. That used to be prevented incidentally by
// c.mu, which List no longer holds (#208).
var pricingRefreshMu sync.Mutex

// refreshPricingResolver reloads the in-memory resolver snapshot when the
// catalog's revision moved. A rate edit changes the revision; the next cache
// List (hourly at worst) picks the new snapshot up, and read-time cost
// computation follows immediately.
func (c *Cache) refreshPricingResolver() {
	if c.pricing == nil {
		return
	}
	pricingRefreshMu.Lock()
	defer pricingRefreshMu.Unlock()
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
// scannerDisabled reports whether this process has been told not to scan.
//
// Read once: it is a deployment fact, not a setting, and re-reading it per call
// would let a scan start halfway through a run.
func scannerDisabled() bool {
	scannerDisabledOnce.Do(func() {
		scannerOff = scannerOffValue(os.Getenv("AGENTO_SCANNER"))
	})
	return scannerOff
}

// scannerOffValue is the parsing on its own, so it can be tested without the
// sync.Once that makes the real reader deliberately un-resettable.
//
// Unset is **on**: a plain `agento web` must keep scanning, and only a process
// that has been told otherwise stops. Anything unrecognized is also on, for the
// same reason — a typo in the variable must not silently disable the scan.
func scannerOffValue(raw string) bool {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case "off", "0", "false", "disabled":
		return true
	default:
		return false
	}
}

var (
	scannerDisabledOnce sync.Once
	scannerOff          bool
)

// StartBackgroundScan kicks off the boot-time scan.
//
// A no-op when the process has been told not to scan (AGENTO_SCANNER=off),
// because EnsureScan is where that is decided.
func (c *Cache) StartBackgroundScan() {
	c.EnsureScan()
}

// EnsureScan starts a background scan unless one is already running, and
// returns the channel closed when the running scan finishes.
//
// Admission is decided under c.mu but the scan itself runs outside it, which
// is the whole point: readers stay unblocked for the scan's full duration.
// Moving the scan into a goroutine without this would only unblock the
// triggering request — the next reader would wait on the mutex just as long.
func (c *Cache) EnsureScan() <-chan struct{} {
	// The desktop app owns the scan in its Rust shell (#289), and two writers on
	// one SQLite file is exactly what that port has been avoiding since #274. The
	// shell therefore starts this process with AGENTO_SCANNER=off.
	//
	// This is the single choke point: both the boot-time StartBackgroundScan and
	// every read path's ensureFresh reach a scan only through here, so one guard
	// disables all of them and no caller has to know.
	//
	// A **closed** channel rather than nil, because callers wait on the result —
	// Cache.List blocks on it for coldStartScanWait — and a nil channel blocks
	// forever. Closed means "the scan you asked for is already over", which is
	// true: someone else ran it.
	if scannerDisabled() {
		done := make(chan struct{})
		close(done)
		return done
	}

	c.mu.Lock()
	if c.scanning {
		done := c.scanDone
		c.mu.Unlock()
		return done
	}
	c.scanning = true
	done := make(chan struct{})
	c.scanDone = done
	c.mu.Unlock()

	go func() {
		defer func() {
			c.mu.Lock()
			c.scanning = false
			c.mu.Unlock()
			// Closed last, so a waiter that wakes on it never observes the
			// scan as still running.
			close(done)
		}()
		c.logger.Info("claude sessions: starting background scan")
		if _, err := IncrementalScanWith(c.db, c.logger, ScanOptions{
			Notify:   c.notify,
			Progress: c.recordProgress,
		}); err != nil {
			// pricing_rev and scanner_version are advanced inside the scan and
			// only after it applies its changes, so a failure leaves the drift
			// recorded and the next read retries it.
			c.logger.Warn("claude sessions: background scan failed", "error", err)
			return
		}
		c.logger.Info("claude sessions: background scan complete")
	}()
	return done
}

// recordProgress publishes the running scan's position for the status endpoint.
func (c *Cache) recordProgress(done, total int) {
	c.filesDone.Store(int64(done))
	c.filesTotal.Store(int64(total))
}

// ScanProgress reports how many transcripts the running scan has written and
// how many it has to write.
//
// Both are zero when no scan is running or when the last one had nothing to do.
// A first run on a large corpus takes minutes, and since the list no longer
// blocks on it, silence for that long is indistinguishable from a hang.
func (c *Cache) ScanProgress() (done, total int) {
	return int(c.filesDone.Load()), int(c.filesTotal.Load())
}

// ScanInProgress reports whether a background scan is currently running.
func (c *Cache) ScanInProgress() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.scanning
}

// CostsStale reports whether the cached costs were computed under a different
// pricing catalog than the one now loaded — i.e. the figures being served are
// correct for the old rates and a re-cost is pending.
func (c *Cache) CostsStale() bool {
	return c.pricingChanged()
}

// LastScannedAt returns when the cache was last fully scanned. The zero time
// means never.
func (c *Cache) LastScannedAt() time.Time {
	var t time.Time
	row := c.db.QueryRowContext(context.Background(),
		"SELECT last_scanned_at FROM claude_cache_metadata WHERE id = 1")
	if row.Scan(&t) != nil {
		return time.Time{}
	}
	return t
}

// List returns all cached session summaries, always from the cache. When the
// cache is stale — the TTL expired, or the pricing catalog moved since the
// costs were computed — the rescan is started in the background and the
// currently cached rows are returned immediately.
//
// Serving slightly stale figures beats blocking: since #188 a rate edit can no
// longer re-cost cached rows in place (re-pricing needs each message's own
// model and timestamp, which the row does not keep), so the only way to apply
// one is to re-read every transcript. That is ~18s on a large corpus, and #189
// made rate edits a routine UI action. Callers distinguish the two states via
// CostsStale/ScanInProgress and label the figures rather than stalling on them.
func (c *Cache) List() []ClaudeSessionSummary {
	done := c.ensureFresh()
	sessions := c.loadOrEmpty()
	if len(sessions) > 0 || done == nil {
		return sessions
	}

	// Cold cache: there is genuinely nothing to serve, and an empty list reads
	// as "no sessions" rather than "not scanned yet". Wait for the scan that is
	// already running, bounded so a pathological corpus cannot hang the request.
	select {
	case <-done:
		return c.loadOrEmpty()
	case <-time.After(coldStartScanWait):
		c.logger.Warn("claude sessions: cold-start scan still running; returning an empty list",
			"waited", coldStartScanWait)
		return sessions
	}
}

// ensureFresh picks up a new pricing catalog and starts a background rescan if
// the cached figures were computed under different inputs than the ones now
// configured. It returns the channel of the in-flight scan, or nil when nothing
// needed rescanning; it never waits.
//
// Every read path goes through it — the corpus load behind analytics and the
// paged list alike — so a rate edit or a threshold change reaches the figures
// whichever surface the user happens to open, and so the "one scan at a time"
// admission is decided in one place.
func (c *Cache) ensureFresh() <-chan struct{} {
	// A rate edit must not wait for the hourly TTL to reach the cost figures,
	// so pick up a new catalog snapshot before deciding anything.
	c.refreshPricingResolver()
	if !c.isFresh() || c.pricingChanged() || c.idleThresholdChanged() {
		return c.EnsureScan()
	}
	return nil
}

// loadOrEmpty reads the cached rows, degrading to an empty slice on error so
// the handler always marshals an array.
//
// Sessions belonging to hidden projects are dropped here, which is what makes
// hiding a project reach every reader at once: the sessions list, the
// analytics endpoint and the insights summary all begin at List. They are
// filtered rather than left unscanned, so unhiding costs nothing and the rows
// stay correct while hidden.
func (c *Cache) loadOrEmpty() []ClaudeSessionSummary {
	sessions, err := c.loadAll()
	if err != nil {
		c.logger.Warn("claude sessions: failed to load from cache", "error", err)
		return []ClaudeSessionSummary{}
	}
	return VisibleSessions(sessions)
}

// UnpricedModels returns the distinct model IDs seen in cached sessions that
// matched no rate, sorted. #188 stores the per-session list, so this is a
// cheap query rather than a corpus re-read; the pricing UI uses it to turn the
// unknown-pricing bucket into a list of models waiting to be priced.
func (c *Cache) UnpricedModels(ctx context.Context) ([]string, error) {
	rows, err := c.db.QueryContext(ctx, `
		SELECT unpriced_models FROM claude_session_cache WHERE unpriced_models != ''
		UNION ALL
		SELECT unpriced_models FROM claude_subagent_cache WHERE unpriced_models != ''`)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			c.logger.Warn("claude sessions: failed to close rows", "error", cerr)
		}
	}()

	seen := map[string]struct{}{}
	for rows.Next() {
		var packed string
		if err := rows.Scan(&packed); err != nil {
			return nil, err
		}
		for _, m := range strings.Split(packed, "\n") {
			if m != "" {
				seen[m] = struct{}{}
			}
		}
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	out := make([]string, 0, len(seen))
	for m := range seen {
		out = append(out, m)
	}
	sort.Strings(out)
	return out, nil
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

// GetSummary returns the cached summary row for one session, or nil when the
// scanner has not reached it yet.
//
// The detail endpoint reads the session's own JSONL, which carries token counts
// but no cost: cost is accumulated per assistant message during a scan and
// stored (#188), because a re-read has no per-message pricing context to work
// from. Without this the detail page would report $0.00 for a session the list
// prices correctly.
func (c *Cache) GetSummary(sessionID string) *ClaudeSessionSummary {
	c.mu.Lock()
	defer c.mu.Unlock()
	s, err := querySessionSummary(c.db, c.logger, sessionID)
	if err != nil {
		c.logger.Warn("claude sessions: failed to read cached summary",
			"session_id", sessionID, "error", err)
		return nil
	}
	return s
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
