package claudesessions

import (
	"log/slog"
	"sync"
	"time"
)

const defaultCacheTTL = 5 * time.Minute

// Cache is an in-memory cache of Claude Code session summaries with TTL-based invalidation.
// It is safe for concurrent use.
type Cache struct {
	mu        sync.RWMutex
	sessions  []ClaudeSessionSummary
	expiresAt time.Time
	ttl       time.Duration
	logger    *slog.Logger
}

// NewCache creates a new Cache with the default TTL.
func NewCache(logger *slog.Logger) *Cache {
	return &Cache{
		ttl:    defaultCacheTTL,
		logger: logger,
	}
}

// StartBackgroundScan starts the initial scan of ~/.claude/projects/ in a background
// goroutine so the server starts immediately while the cache is being populated.
func (c *Cache) StartBackgroundScan() {
	go func() {
		c.logger.Info("claude sessions: starting background scan")
		sessions, err := ScanAllSessions()
		if err != nil {
			c.logger.Warn("claude sessions: background scan failed", "error", err)
			return
		}
		c.mu.Lock()
		c.sessions = sessions
		c.expiresAt = time.Now().Add(c.ttl)
		c.mu.Unlock()
		c.logger.Info("claude sessions: scan complete", "count", len(sessions))
	}()
}

// List returns all cached session summaries. If the cache has expired or is empty,
// a synchronous rescan is performed before returning.
func (c *Cache) List() []ClaudeSessionSummary {
	c.mu.RLock()
	if c.sessions != nil && time.Now().Before(c.expiresAt) {
		sessions := c.sessions
		c.mu.RUnlock()
		return sessions
	}
	c.mu.RUnlock()

	// Cache expired or not yet populated — rescan synchronously.
	sessions, err := ScanAllSessions()
	if err != nil {
		c.logger.Warn("claude sessions: refresh scan failed", "error", err)
		// Return stale data if available rather than an error.
		c.mu.RLock()
		stale := c.sessions
		c.mu.RUnlock()
		if stale != nil {
			return stale
		}
		return []ClaudeSessionSummary{}
	}

	c.mu.Lock()
	c.sessions = sessions
	c.expiresAt = time.Now().Add(c.ttl)
	c.mu.Unlock()
	return sessions
}

// Invalidate clears the cache expiry so the next List() call triggers a rescan.
func (c *Cache) Invalidate() {
	c.mu.Lock()
	c.expiresAt = time.Time{}
	c.mu.Unlock()
}
