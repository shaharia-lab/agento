package claudesessions

import (
	"fmt"
	"regexp"
	"testing"
	"time"
)

// bucketKeyPattern is the contract frontend/src/lib/analyticsMetrics.ts parses:
// every key is a YYYY-MM-DD, optionally suffixed with T and an hour. Weekly and
// monthly buckets are keyed by their first day rather than by "2026-W32" or
// "2026-08" precisely so this keeps holding.
var bucketKeyPattern = regexp.MustCompile(`^\d{4}-\d{2}-\d{2}(T\d{2})?$`)

func TestGranularity_CoarsensWithTheWindow(t *testing.T) {
	base := time.Date(2026, 8, 11, 12, 0, 0, 0, time.UTC)
	cases := []struct {
		span time.Duration
		want string
	}{
		{24 * time.Hour, GranularityHourly},
		{7 * 24 * time.Hour, GranularityHourly},
		{8 * 24 * time.Hour, GranularityDaily},
		{120 * 24 * time.Hour, GranularityDaily},
		{121 * 24 * time.Hour, GranularityWeekly},
		{3 * 365 * 24 * time.Hour, GranularityWeekly},
		{4 * 365 * 24 * time.Hour, GranularityMonthly},
		// "All time" starts in 2020; this is the range that used to emit 2,415
		// daily buckets and 790 KB of JSON on a 798-session corpus.
		{6 * 365 * 24 * time.Hour, GranularityMonthly},
		{12 * 365 * 24 * time.Hour, GranularityMonthly},
		// No UI offers this; from/to come from a query string, and a series
		// should degrade in resolution rather than in size.
		{40 * 365 * 24 * time.Hour, GranularityYearly},
	}
	for _, tc := range cases {
		p := AnalyticsParams{From: base.Add(-tc.span), To: base}
		if got := p.Granularity(); got != tc.want {
			t.Errorf("a %v window reported %q, want %q", tc.span, got, tc.want)
		}
	}
}

func TestWalkBuckets_StaysUnderTheBucketCeiling(t *testing.T) {
	base := time.Date(2026, 8, 11, 12, 0, 0, 0, time.UTC)
	// Every span a user can request, including the absurd ones a hand-written
	// query string can produce.
	spans := []int{1, 7, 8, 30, 120, 121, 365, 3 * 365, 4 * 365, 10 * 365, 12 * 365, 20 * 365, 150 * 365}
	for _, days := range spans {
		p := AnalyticsParams{From: base.AddDate(0, 0, -days), To: base, Loc: time.UTC}
		n := 0
		walkBuckets(p.From, p.To, p.Granularity(), time.UTC, func(key string, _ time.Time) {
			n++
			if !bucketKeyPattern.MatchString(key) {
				t.Fatalf("%d-day window emitted key %q, which analyticsMetrics.ts cannot parse", days, key)
			}
		})
		if n > maxBuckets {
			t.Errorf("a %d-day window emitted %d buckets, over the %d ceiling", days, n, maxBuckets)
		}
		if n == 0 {
			t.Errorf("a %d-day window emitted no buckets", days)
		}
	}
}

func TestBucketKey_AlignsSessionsWithTheWalkedSeries(t *testing.T) {
	// A window starting mid-week and mid-month: the walk has to begin at the
	// bucket containing `from`, not at `from` itself, or the first sessions key
	// into a bucket the series never emits and silently vanish from the chart.
	loc := time.UTC
	from := time.Date(2026, 8, 13, 9, 30, 0, 0, loc) // a Thursday
	to := from.AddDate(2, 0, 0)

	for _, granularity := range []string{GranularityWeekly, GranularityMonthly, GranularityYearly} {
		emitted := map[string]bool{}
		walkBuckets(from, to, granularity, loc, func(key string, _ time.Time) { emitted[key] = true })

		for _, at := range []time.Time{from, from.Add(time.Hour), from.AddDate(0, 0, 3), to} {
			key := bucketKey(at, granularity, loc)
			if !emitted[key] {
				t.Errorf("%s: a session at %s keys into %q, which the walk never emits",
					granularity, at.Format(time.RFC3339), key)
			}
		}
	}
}

func TestBucketStart_WeeksBeginOnMonday(t *testing.T) {
	loc := time.UTC
	// Sun 16 Aug 2026 belongs to the week beginning Mon 10 Aug, not to the one
	// beginning the following day.
	sunday := time.Date(2026, 8, 16, 23, 0, 0, 0, loc)
	if got := bucketStart(sunday, GranularityWeekly, loc); got.Weekday() != time.Monday {
		t.Errorf("week start is a %s, want Monday", got.Weekday())
	}
	monday := time.Date(2026, 8, 10, 0, 0, 0, 0, loc)
	if got := bucketStart(sunday, GranularityWeekly, loc); !got.Equal(monday) {
		t.Errorf("Sunday 16 Aug bucketed to %s, want %s", got.Format("2006-01-02"), monday.Format("2006-01-02"))
	}
}

func TestBucketStart_StepsTheCalendarAcrossDST(t *testing.T) {
	// Europe/Berlin springs forward on 29 March 2026. A fixed 24h step drifts
	// off the wall clock across it, duplicating one key and skipping another.
	loc, err := time.LoadLocation("Europe/Berlin")
	if err != nil {
		t.Skipf("timezone database unavailable: %v", err)
	}
	from := time.Date(2026, 3, 27, 0, 0, 0, 0, loc)
	to := time.Date(2026, 3, 31, 23, 59, 0, 0, loc)

	seen := map[string]int{}
	walkBuckets(from, to, GranularityDaily, loc, func(key string, _ time.Time) { seen[key]++ })
	for _, day := range []string{"2026-03-27", "2026-03-28", "2026-03-29", "2026-03-30", "2026-03-31"} {
		if seen[day] != 1 {
			t.Errorf("%s emitted %d times, want exactly 1", day, seen[day])
		}
	}
}

func TestFoldProjectTail_KeepsEveryFigureInTheTotal(t *testing.T) {
	// 500 projects is the target scale; the table has to stay readable without
	// the total quietly excluding 480 of them.
	ranked := make([]ProjectStat, 0, 500)
	var wantSessions int
	var wantCost float64
	for i := range 500 {
		cost := float64(500 - i)
		ranked = append(ranked, ProjectStat{
			Project:  fmt.Sprintf("/home/dev/repo-%03d", i),
			Sessions: 2,
			Tokens:   100,
			Cost:     SessionCost{TotalUSD: cost},
		})
		wantSessions += 2
		wantCost += cost
	}

	folded := foldProjectTail(ranked)
	if len(folded) != topProjectsListed+1 {
		t.Fatalf("folded to %d rows, want %d plus the Other row", len(folded), topProjectsListed)
	}

	other := folded[len(folded)-1]
	if other.Project != OtherProjectsLabel {
		t.Errorf("last row is %q, want the folded tail", other.Project)
	}
	if other.FoldedProjects != 500-topProjectsListed {
		t.Errorf("Other stands for %d projects, want %d", other.FoldedProjects, 500-topProjectsListed)
	}

	var gotSessions int
	var gotCost float64
	for _, p := range folded {
		gotSessions += p.Sessions
		gotCost += p.Cost.TotalUSD
	}
	if gotSessions != wantSessions {
		t.Errorf("folding lost sessions: %d, want %d", gotSessions, wantSessions)
	}
	if gotCost != wantCost {
		t.Errorf("folding lost cost: %v, want %v", gotCost, wantCost)
	}
}

func TestFoldProjectTail_NamesASingleTailProjectRatherThanBucketingIt(t *testing.T) {
	ranked := make([]ProjectStat, topProjectsListed+1)
	for i := range ranked {
		ranked[i] = ProjectStat{Project: fmt.Sprintf("/p/%d", i), Cost: SessionCost{TotalUSD: float64(100 - i)}}
	}
	folded := foldProjectTail(ranked)
	if len(folded) != len(ranked) {
		t.Errorf("folded %d rows into %d; one project is better named than bucketed",
			len(ranked), len(folded))
	}
	for _, p := range folded {
		if p.Project == OtherProjectsLabel {
			t.Error("a single tail project was replaced by an Other bucket")
		}
	}
}
