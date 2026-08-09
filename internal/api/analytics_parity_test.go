package api

import (
	"net/http/httptest"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/claudesessions"
)

// paritySessions spans the interesting edges: one session that starts before a
// window and ends inside it, one that ends exactly on the closing instant, one
// a second past it, and one in a different project.
func paritySessions() []claudesessions.ClaudeSessionSummary {
	berlin, _ := time.LoadLocation("Europe/Berlin")
	at := func(day, hour int) time.Time {
		return time.Date(2026, 8, day, hour, 0, 0, 0, berlin)
	}
	return []claudesessions.ClaudeSessionSummary{
		{
			SessionID:    "spans-into-window",
			ProjectPath:  "/proj/a",
			StartTime:    at(31, 20), // July 31 local — before the window opens
			LastActivity: time.Date(2026, 8, 1, 2, 0, 0, 0, berlin),
		},
		{SessionID: "inside", ProjectPath: "/proj/a", StartTime: at(3, 9), LastActivity: at(3, 11)},
		{
			SessionID:    "closing-instant",
			ProjectPath:  "/proj/b",
			StartTime:    at(5, 9),
			LastActivity: time.Date(2026, 8, 5, 23, 59, 59, 0, berlin),
		},
		{
			SessionID:    "just-after",
			ProjectPath:  "/proj/a",
			StartTime:    at(5, 9),
			LastActivity: time.Date(2026, 8, 6, 0, 0, 1, 0, berlin),
		},
	}
}

// TestInsightsSummaryCoversSameSessionsAsAnalytics is the regression guard for
// the defect that made two dashboards disagree: the insights summary resolved
// its own window, against start_time, with its own inclusivity rule, so the
// same date range reported a different session count and a different total cost
// on each page.
//
// Both endpoints now read the window through parseAnalyticsParams and apply it
// through claudesessions.FilterSessions, so this asserts what the user sees:
// one range, one set of sessions. It exercises the two handlers' shared code
// rather than the handlers themselves, because that shared code is the whole
// mechanism — a future endpoint that resolves dates itself is what would break
// this again.
func TestInsightsSummaryCoversSameSessionsAsAnalytics(t *testing.T) {
	sessions := paritySessions()

	for _, query := range []string{
		"?from=2026-08-01&to=2026-08-05&tz=Europe/Berlin",
		"?from=2026-08-01&to=2026-08-05&tz=Europe/Berlin&project=/proj/a",
		"?from=2026-08-01&to=2026-08-05", // no tz — UTC fallback, still one answer
		"?from=2026-08-01T00:00:00%2B02:00&to=2026-08-05T23:59:59%2B02:00&tz=Europe/Berlin",
	} {
		t.Run(query, func(t *testing.T) {
			params := parseAnalyticsParams(httptest.NewRequest("GET", "/api/x"+query, nil))

			report := claudesessions.AggregateAnalytics(sessions, params)
			ids := claudesessions.SessionIDs(claudesessions.FilterSessions(sessions, params))

			if len(ids) != report.Summary.TotalSessions {
				t.Errorf("insights would aggregate %d sessions (%v) but analytics counted %d",
					len(ids), ids, report.Summary.TotalSessions)
			}
		})
	}
}

// TestParseAnalyticsParams_WindowEdges pins the boundary behavior both
// endpoints now inherit. The closing instant is inclusive because a bare `to`
// names a whole local day; a second later is outside it.
func TestParseAnalyticsParams_WindowEdges(t *testing.T) {
	params := parseAnalyticsParams(
		httptest.NewRequest("GET", "/api/x?from=2026-08-01&to=2026-08-05&tz=Europe/Berlin", nil),
	)

	got := map[string]bool{}
	for _, s := range claudesessions.FilterSessions(paritySessions(), params) {
		got[s.SessionID] = true
	}

	for id, want := range map[string]bool{
		"spans-into-window": true, // ended inside the window, though it started before it
		"inside":            true,
		"closing-instant":   true,  // 23:59:59 on the closing day is in the day
		"just-after":        false, // one second into the next day is not
	} {
		if got[id] != want {
			t.Errorf("session %q in window = %v, want %v", id, got[id], want)
		}
	}
}

// TestParseRangeEnd covers the two shapes of a window end. A bare date names a
// day, so it must resolve to that day's last second — resolving it to midnight
// would make "to: today" exclude everything that happened today. An RFC3339
// value names an instant and is left alone.
func TestParseRangeEnd(t *testing.T) {
	berlin, err := time.LoadLocation("Europe/Berlin")
	if err != nil {
		t.Fatal(err)
	}

	end, err := parseRangeEnd("2026-08-05", berlin)
	if err != nil {
		t.Fatal(err)
	}
	if want := time.Date(2026, 8, 5, 23, 59, 59, 0, berlin); !end.Equal(want) {
		t.Errorf("bare date end = %s, want %s", end, want)
	}

	exact, err := parseRangeEnd("2026-08-05T10:30:00Z", berlin)
	if err != nil {
		t.Fatal(err)
	}
	if want := time.Date(2026, 8, 5, 10, 30, 0, 0, time.UTC); !exact.Equal(want) {
		t.Errorf("RFC3339 end = %s, want it taken at its word (%s)", exact, want)
	}

	if _, err := parseRangeEnd("not-a-date", berlin); err == nil {
		t.Error("an unparseable end must report an error so the caller can fall back")
	}
}

// TestIntersectIDs covers the narrowing contract: an explicit ids list restricts
// the window rather than replacing it, so naming a session outside the range
// cannot pull it back in.
func TestIntersectIDs(t *testing.T) {
	got := intersectIDs([]string{"a", "b", "c"}, []string{"c", "z", "a"})
	if len(got) != 2 || got[0] != "a" || got[1] != "c" {
		t.Errorf("intersectIDs = %v, want [a c] in base order", got)
	}
	if out := intersectIDs([]string{"a"}, []string{"z"}); len(out) != 0 {
		t.Errorf("disjoint sets must intersect to nothing, got %v", out)
	}
}

// TestParseSessionIDs covers the trimming the `ids` parameter needs.
func TestParseSessionIDs(t *testing.T) {
	if got := parseSessionIDs(" a , ,b "); len(got) != 2 || got[0] != "a" || got[1] != "b" {
		t.Errorf("parseSessionIDs = %v, want [a b]", got)
	}
	if got := parseSessionIDs(""); got != nil {
		t.Errorf("empty parameter must yield no filter, got %v", got)
	}
}
