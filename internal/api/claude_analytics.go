package api

import (
	"net/http"
	"time"

	"github.com/shaharia-lab/agento/internal/claudesessions"
)

// handleGetClaudeAnalytics aggregates token usage from all Claude Code sessions
// and returns a single JSON payload suitable for the analytics dashboard.
//
// Query params:
//
//	from    YYYY-MM-DD or RFC3339 start (default: 30 days ago)
//	to      YYYY-MM-DD or RFC3339 end   (default: now)
//	project decoded project path to filter by (optional, empty = all projects)
func (s *Server) handleGetClaudeAnalytics(w http.ResponseWriter, r *http.Request) {
	q := r.URL.Query()

	now := time.Now()
	from := now.AddDate(0, 0, -30)
	to := now

	if raw := q.Get("from"); raw != "" {
		if t, err := parseAnalyticsDate(raw); err == nil {
			from = t
		}
	}
	if raw := q.Get("to"); raw != "" {
		if t, err := parseAnalyticsDate(raw); err == nil {
			// Make the end date inclusive by advancing to end of day.
			to = time.Date(t.Year(), t.Month(), t.Day(), 23, 59, 59, 0, t.Location())
		}
	}

	params := claudesessions.AnalyticsParams{
		From:    from,
		To:      to,
		Project: q.Get("project"),
	}

	sessions := s.claudeSessionCache.List()
	report := claudesessions.AggregateAnalytics(sessions, params)
	writeJSON(w, http.StatusOK, report)
}

// parseAnalyticsDate tries RFC3339 first, then YYYY-MM-DD.
func parseAnalyticsDate(s string) (time.Time, error) {
	if t, err := time.Parse(time.RFC3339, s); err == nil {
		return t, nil
	}
	return time.Parse("2006-01-02", s)
}
