package claudesessions

import (
	"context"
	"testing"
	"time"
)

func analyticsWindow() AnalyticsParams {
	return AnalyticsParams{
		From: time.Date(2026, 7, 1, 0, 0, 0, 0, time.UTC),
		To:   time.Date(2026, 8, 31, 23, 59, 59, 0, time.UTC),
		Loc:  time.UTC,
	}
}

func TestAnalytics_ServesTheMemoWhenNothingChanged(t *testing.T) {
	c := newPageCache(t)
	insertTestSession(t, c.db, testSession{
		id: "a", inputTokens: 100, outputTokens: 50, costUSD: 1.25,
		last: time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC),
	})

	first := c.Analytics(analyticsWindow())
	if first.Summary.TotalSessions != 1 {
		t.Fatalf("first report saw %d sessions", first.Summary.TotalSessions)
	}

	// A row written behind the cache's back: a memo hit must not see it, which
	// is how we can tell the second call did not rebuild.
	insertTestSession(t, c.db, testSession{
		id: "b", inputTokens: 999, last: time.Date(2026, 8, 2, 12, 0, 0, 0, time.UTC),
	})
	if second := c.Analytics(analyticsWindow()); second.Summary.TotalSessions != 1 {
		t.Errorf("second report rebuilt (%d sessions); expected the memoized one",
			second.Summary.TotalSessions)
	}
}

func TestAnalytics_RebuildsWhenAScanMoves(t *testing.T) {
	c := newPageCache(t)
	insertTestSession(t, c.db, testSession{
		id: "a", last: time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC),
	})
	_ = c.Analytics(analyticsWindow())

	insertTestSession(t, c.db, testSession{
		id: "b", last: time.Date(2026, 8, 2, 12, 0, 0, 0, time.UTC),
	})
	// last_scanned_at is part of the key precisely so a completed scan reaches
	// every memoized window without any of them being tracked individually.
	if _, err := c.db.ExecContext(context.Background(),
		`UPDATE claude_cache_metadata SET last_scanned_at = ? WHERE id = 1`,
		time.Now().UTC().Add(time.Second)); err != nil {
		t.Fatalf("advancing the scan timestamp: %v", err)
	}

	if got := c.Analytics(analyticsWindow()).Summary.TotalSessions; got != 2 {
		t.Errorf("report saw %d sessions after a scan; want the rebuilt 2", got)
	}
}

func TestAnalytics_RebuildsWhenAProjectIsHidden(t *testing.T) {
	c := newPageCache(t)
	at := time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)
	insertTestSession(t, c.db, testSession{id: "shown", project: "/home/dev/shown", last: at})
	insertTestSession(t, c.db, testSession{id: "hidden", project: "/home/dev/secret", last: at})

	if got := c.Analytics(analyticsWindow()).Summary.TotalSessions; got != 2 {
		t.Fatalf("baseline report saw %d sessions, want 2", got)
	}

	// Hiding is process state rather than cached state, so it is the one
	// invalidation input the memo has to fingerprint itself.
	ApplyDataSettings(0, []string{"/home/dev/secret"})
	defer ApplyDataSettings(0, nil)

	if got := c.Analytics(analyticsWindow()).Summary.TotalSessions; got != 1 {
		t.Errorf("hiding a project left the report at %d sessions, want 1", got)
	}
}

func TestAnalytics_KeepsWindowsApart(t *testing.T) {
	c := newPageCache(t)
	insertTestSession(t, c.db, testSession{
		id: "july", last: time.Date(2026, 7, 15, 12, 0, 0, 0, time.UTC),
	})
	insertTestSession(t, c.db, testSession{
		id: "august", last: time.Date(2026, 8, 15, 12, 0, 0, 0, time.UTC),
	})

	july := c.Analytics(AnalyticsParams{
		From: time.Date(2026, 7, 1, 0, 0, 0, 0, time.UTC),
		To:   time.Date(2026, 7, 31, 23, 59, 59, 0, time.UTC),
		Loc:  time.UTC,
	})
	august := c.Analytics(AnalyticsParams{
		From: time.Date(2026, 8, 1, 0, 0, 0, 0, time.UTC),
		To:   time.Date(2026, 8, 31, 23, 59, 59, 0, time.UTC),
		Loc:  time.UTC,
	})
	// The dashboards fetch a window and the one before it together; serving one
	// from the other's entry would make every comparison read as no change.
	if july.Summary.TotalSessions != 1 || august.Summary.TotalSessions != 1 {
		t.Errorf("windows collided: july=%d august=%d",
			july.Summary.TotalSessions, august.Summary.TotalSessions)
	}
	if len(july.TimeSeries) == 0 || july.TimeSeries[0].Date == august.TimeSeries[0].Date {
		t.Error("the two windows produced the same first bucket")
	}
}

func TestAnalyticsMemo_EvictsLeastRecentlyUsed(t *testing.T) {
	m := newAnalyticsMemo()
	for i := range analyticsCacheSize + 5 {
		m.put(string(rune('a'+i)), AnalyticsReport{Granularity: GranularityDaily})
	}
	if got := m.order.Len(); got != analyticsCacheSize {
		t.Errorf("memo holds %d entries, want the %d ceiling", got, analyticsCacheSize)
	}
	if _, ok := m.get("a"); ok {
		t.Error("the oldest entry survived eviction")
	}
	if _, ok := m.get(string(rune('a' + analyticsCacheSize + 4))); !ok {
		t.Error("the newest entry was evicted")
	}
}
