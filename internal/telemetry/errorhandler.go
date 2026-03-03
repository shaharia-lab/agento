package telemetry

import (
	"log/slog"
	"sync"
	"time"
)

const errorSuppressDuration = 60 * time.Second

// rateLimitedErrorHandler implements otel.ErrorHandler. It routes OTel SDK
// export errors through slog.Warn but suppresses the same error message for
// errorSuppressDuration to avoid flooding the log file when a backend is down.
type rateLimitedErrorHandler struct {
	mu       sync.Mutex
	lastSeen map[string]time.Time
}

func newRateLimitedErrorHandler() *rateLimitedErrorHandler {
	return &rateLimitedErrorHandler{
		lastSeen: make(map[string]time.Time),
	}
}

// Handle implements otel.ErrorHandler.
func (h *rateLimitedErrorHandler) Handle(err error) {
	if err == nil {
		return
	}
	key := err.Error()

	h.mu.Lock()
	now := time.Now()
	last, seen := h.lastSeen[key]
	if seen && now.Sub(last) < errorSuppressDuration {
		h.mu.Unlock()
		return
	}
	h.lastSeen[key] = now

	// Evict entries older than 2× the suppression window to bound memory usage.
	if len(h.lastSeen) > 100 {
		for k, v := range h.lastSeen {
			if now.Sub(v) > 2*errorSuppressDuration {
				delete(h.lastSeen, k)
			}
		}
	}
	h.mu.Unlock()

	slog.Warn("otel export error", "error", err)
}
