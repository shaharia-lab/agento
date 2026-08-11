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
	params := parseAnalyticsParams(r)
	// Memoized on the window plus every input that can change what it answers
	// (see claudesessions/analytics_cache.go). The dashboards fire two or three
	// of these per open — the current window and the one before it — and every
	// one of them used to rebuild the report from a full corpus load.
	s.writeJSON(w, http.StatusOK, s.claudeSessionCache.Analytics(params))
}

// parseAnalyticsParams reads the window, project and timezone every
// analytics-shaped endpoint accepts.
//
// It is shared rather than duplicated because two endpoints reading the same
// query string differently is exactly how the dashboards came to disagree: the
// insights summary resolved its own dates, against its own column, with its own
// inclusivity rule. One parser and one filter (claudesessions.FilterSessions)
// mean one answer to "which sessions are in this window".
//
// An unparseable from/to falls back to the default window rather than erroring,
// matching how parseTimezone treats a bad zone — a read-only dashboard is more
// useful rendered over a default range than refused.
func parseAnalyticsParams(r *http.Request) claudesessions.AnalyticsParams {
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
		if t, err := parseRangeEnd(raw, loc); err == nil {
			to = t
		}
	}

	return claudesessions.AnalyticsParams{
		From:    from,
		To:      to,
		Project: q.Get("project"),
		Loc:     loc,
	}
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

// parseRangeEnd parses an inclusive window end.
//
// A bare YYYY-MM-DD names a whole local day, so it resolves to that day's final
// second rather than its first — otherwise "to: today" would exclude everything
// that happened today. An RFC3339 value states its own instant and is taken at
// its word, matching parseAnalyticsDate.
//
// Every analytics-shaped endpoint resolves its window through this and
// parseAnalyticsDate, so the same from/to pair selects the same sessions
// wherever it is sent. The insights summary drifting off that basis is what
// made two dashboards report different totals for one window.
func parseRangeEnd(raw string, loc *time.Location) (time.Time, error) {
	t, err := parseAnalyticsDate(raw, loc)
	if err != nil {
		return time.Time{}, err
	}
	if _, rfcErr := time.Parse(time.RFC3339, raw); rfcErr == nil {
		return t, nil
	}
	return time.Date(t.Year(), t.Month(), t.Day(), 23, 59, 59, 0, loc), nil
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
