//go:build scale

// Package-level scale harness. Excluded from `make test` by the `scale` build
// tag because generating and scanning the large corpus writes ~1 GB and takes
// minutes; `make bench-scale` is the entry point.
//
// Its purpose is to turn projections into assertions. Every scaling figure
// Agento had about itself came from extrapolating one developer's 798-session
// machine to the 5,000-session corpus the platform is specified for, and an
// extrapolation cannot fail a build. The budgets below can, so a change that
// reintroduces a full-corpus load per request is caught here rather than by the
// user it happens to.
//
//	SCALE=small|medium|large   corpus size (default medium)
package claudesessions

import (
	"encoding/json"
	"fmt"
	"os"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/claudesessions/synthcorpus"
	"github.com/shaharia-lab/agento/internal/storage"
)

// budget is what one corpus size is allowed to cost. The figures are generous
// against measurements on the reference machine — this guards against an order
// of magnitude, not against noise, because a benchmark that fails on a busy
// laptop gets deleted rather than fixed.
type budget struct {
	name string
	opts synthcorpus.Options
	// scan is the wall clock of a full cold scan of the whole corpus.
	scan time.Duration
	// listPage is one page of the sessions list, the interaction a user waits on.
	listPage time.Duration
	// facets is the aggregate behind the toolbar counters and filter options.
	facets time.Duration
	// analytics is one all-time analytics report, cold (no memo hit).
	analytics time.Duration
	// analyticsBytes bounds the marshaled all-time report. It grows with
	// calendar span, not corpus size, which is why it is asserted separately.
	analyticsBytes int
	// pageBytes bounds one marshaled page of sessions.
	pageBytes int
}

var budgets = map[string]budget{
	"small": {
		name: "small", opts: synthcorpus.Small(),
		scan: 20 * time.Second, listPage: 250 * time.Millisecond, facets: 250 * time.Millisecond,
		analytics: 2 * time.Second, analyticsBytes: 400 << 10, pageBytes: 300 << 10,
	},
	"medium": {
		name: "medium", opts: synthcorpus.Medium(),
		scan: 3 * time.Minute, listPage: 500 * time.Millisecond, facets: 750 * time.Millisecond,
		analytics: 5 * time.Second, analyticsBytes: 400 << 10, pageBytes: 300 << 10,
	},
	"large": {
		name: "large", opts: synthcorpus.Large(),
		scan: 20 * time.Minute, listPage: 1500 * time.Millisecond, facets: 3 * time.Second,
		analytics: 20 * time.Second, analyticsBytes: 400 << 10, pageBytes: 300 << 10,
	},
}

func TestScale(t *testing.T) {
	size := os.Getenv("SCALE")
	if size == "" {
		size = "medium"
	}
	b, ok := budgets[size]
	if !ok {
		t.Fatalf("unknown SCALE=%q; want small, medium or large", size)
	}

	home := t.TempDir()
	t.Setenv("HOME", home)
	// A fixed instant keeps the generated corpus reproducible; the analytics
	// window below is anchored to it rather than to time.Now.
	until := time.Date(2026, 8, 11, 12, 0, 0, 0, time.UTC)
	b.opts.Until = until

	genStart := time.Now()
	stats, err := synthcorpus.Generate(home, b.opts)
	if err != nil {
		t.Fatalf("generating corpus: %v", err)
	}
	t.Logf("corpus %s: %s (generated in %s)", b.name, stats, time.Since(genStart).Round(time.Millisecond))

	db, _, err := storage.NewSQLiteDB(t.TempDir()+"/scale.db", testLogger)
	if err != nil {
		t.Fatalf("opening database: %v", err)
	}
	defer func() { _ = db.Close() }()
	cache := NewCache(db, testLogger)

	report := map[string]string{}
	measure := func(label string, budget time.Duration, fn func() int) {
		start := time.Now()
		size := fn()
		elapsed := time.Since(start)
		note := fmt.Sprintf("%s (budget %s)", elapsed.Round(time.Millisecond), budget)
		if size >= 0 {
			note += fmt.Sprintf(", %.1f KB", float64(size)/1024)
		}
		report[label] = note
		if elapsed > budget {
			t.Errorf("%s took %s, over the %s budget", label, elapsed.Round(time.Millisecond), budget)
		}
	}

	measure("full cold scan", b.scan, func() int {
		if _, err := IncrementalScan(db, testLogger); err != nil {
			t.Fatalf("scan: %v", err)
		}
		return -1
	})

	measure("sessions page (50)", b.listPage, func() int {
		page, err := cache.ListPage(SessionQuery{Limit: 50})
		if err != nil {
			t.Fatalf("list page: %v", err)
		}
		if len(page.Items) == 0 {
			t.Fatal("list page returned nothing after a full scan")
		}
		return marshaledSize(t, page)
	})

	measure("facets", b.facets, func() int {
		f, err := cache.Facets(SessionQuery{})
		if err != nil {
			t.Fatalf("facets: %v", err)
		}
		if f.Total == 0 {
			t.Fatal("facets reported no sessions after a full scan")
		}
		t.Logf("facets: total=%d tokens=%d cost=%.2f models=%d",
			f.Total, f.TotalTokens, f.TotalCostUSD, len(f.Models))
		return -1
	})

	// All-time is the worst case for the analytics payload: it is the range that
	// used to emit one bucket per calendar day since 2020.
	params := AnalyticsParams{
		From: until.AddDate(-6, 0, 0), To: until, Loc: time.UTC,
	}
	sessions := cache.List()
	measure("analytics (all time, cold)", b.analytics, func() int {
		rep := AggregateAnalytics(sessions, params)
		if len(rep.TimeSeries) > maxBuckets {
			t.Errorf("all-time report emitted %d buckets, over the %d cap",
				len(rep.TimeSeries), maxBuckets)
		}
		return marshaledSize(t, rep)
	})

	if got := marshaledSize(t, AggregateAnalytics(sessions, params)); got > b.analyticsBytes {
		t.Errorf("all-time analytics payload is %d bytes, over the %d budget", got, b.analyticsBytes)
	}

	for label, note := range report {
		t.Logf("%-28s %s", label, note)
	}
}

func marshaledSize(t *testing.T, v any) int {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatalf("marshaling result: %v", err)
	}
	return len(b)
}
