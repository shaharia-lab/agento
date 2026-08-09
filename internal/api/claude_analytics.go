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
//	tz      IANA timezone the buckets and day boundaries are derived in
//	        (default: UTC). Timestamps stay UTC on the wire either way.
func (s *Server) handleGetClaudeAnalytics(w http.ResponseWriter, r *http.Request) {
	q := r.URL.Query()
	loc := parseTimezone(q.Get("tz"))

	// A "day" is only meaningful in a timezone, so the default window is
	// anchored in the requested one rather than the server's.
	now := time.Now().In(loc)
	from := now.AddDate(0, 0, -30)
	to := now

	if raw := q.Get("from"); raw != "" {
		if t, err := parseAnalyticsDate(raw, loc); err == nil {
			from = t
		}
	}
	if raw := q.Get("to"); raw != "" {
		if t, err := parseAnalyticsDate(raw, loc); err == nil {
			// Make the end date inclusive by advancing to end of local day.
			to = time.Date(t.Year(), t.Month(), t.Day(), 23, 59, 59, 0, loc)
		}
	}

	params := claudesessions.AnalyticsParams{
		From:    from,
		To:      to,
		Project: q.Get("project"),
		Loc:     loc,
	}

	sessions := s.claudeSessionCache.List()
	report := claudesessions.AggregateAnalytics(sessions, params)
	s.writeJSON(w, http.StatusOK, report)
}

// parseAnalyticsDate tries RFC3339 first, then YYYY-MM-DD in loc.
//
// A bare date carries no offset, so it has to be interpreted in the requesting
// timezone — parsing it as UTC is what shifted every range edge by the user's
// offset. An RFC3339 value states its own offset and is taken at its word.
func parseAnalyticsDate(s string, loc *time.Location) (time.Time, error) {
	if t, err := time.Parse(time.RFC3339, s); err == nil {
		return t, nil
	}
	return time.ParseInLocation("2006-01-02", s, loc)
}

// parseTimezone resolves an IANA timezone name, falling back to UTC.
//
// A bad or missing zone is never an error: analytics is a read-only dashboard,
// and refusing to render it over an unrecognized timezone string would be a
// worse outcome than rendering it in UTC, which is what every caller got
// before this parameter existed.
func parseTimezone(name string) *time.Location {
	if name == "" {
		return time.UTC
	}
	loc, err := time.LoadLocation(name)
	if err != nil {
		return time.UTC
	}
	return loc
}
