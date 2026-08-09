package claudesessions

import (
	"testing"
	"time"
)

func mustLoad(t *testing.T, name string) *time.Location {
	t.Helper()
	loc, err := time.LoadLocation(name)
	if err != nil {
		t.Fatalf("LoadLocation(%q): %v — is time/tzdata embedded?", name, err)
	}
	return loc
}

// lateNightSession is 2026-08-08T00:30 in Berlin, i.e. 2026-08-07T22:30 UTC.
// Bucketed in UTC it lands on the 7th at hour 22; bucketed in Berlin it lands
// on the 8th at hour 0. That one session is the whole issue.
func lateNightSession() ClaudeSessionSummary {
	at := time.Date(2026, 8, 7, 22, 30, 0, 0, time.UTC)
	return ClaudeSessionSummary{
		SessionID: "late-night", Model: "claude-sonnet-4-6",
		StartTime: at, LastActivity: at,
		Usage: TokenUsage{InputTokens: 100, OutputTokens: 50},
	}
}

// TestBucketing_FollowsRequestedTimezone pins both behaviors at once: the
// acceptance criterion in the requested zone, and the unchanged UTC result for
// callers that send no timezone.
func TestBucketing_FollowsRequestedTimezone(t *testing.T) {
	berlin := mustLoad(t, "Europe/Berlin")
	sessions := []ClaudeSessionSummary{lateNightSession()}

	tests := []struct {
		name     string
		loc      *time.Location
		wantDay  string
		wantHour int
		// 2026-08-07 is a Friday, 2026-08-08 a Saturday.
		wantWeekday time.Weekday
	}{
		{"berlin puts late-night work on the local day", berlin, "2026-08-08", 0, time.Saturday},
		{"utc keeps the previous behavior", time.UTC, "2026-08-07", 22, time.Friday},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			days := buildMostActiveDays(sessions, tt.loc)
			if len(days) != 1 || days[0].Date != tt.wantDay {
				t.Errorf("most active day = %+v, want %s", days, tt.wantDay)
			}

			cells := buildHeatmap(sessions, tt.loc)
			if len(cells) != 1 {
				t.Fatalf("heatmap cells = %d, want 1", len(cells))
			}
			if cells[0].Hour != tt.wantHour {
				t.Errorf("heatmap hour = %d, want %d", cells[0].Hour, tt.wantHour)
			}
			if time.Weekday(cells[0].DayOfWeek) != tt.wantWeekday {
				t.Errorf("heatmap weekday = %v, want %v",
					time.Weekday(cells[0].DayOfWeek), tt.wantWeekday)
			}

			hourly := buildHourlyActivity(sessions, tt.loc)
			if hourly[tt.wantHour].Sessions != 1 {
				t.Errorf("hour %d has %d sessions, want 1", tt.wantHour, hourly[tt.wantHour].Sessions)
			}
		})
	}
}

// TestAnalyticsParams_NilLocationIsUTC keeps every caller that predates the
// timezone parameter working exactly as before.
func TestAnalyticsParams_NilLocationIsUTC(t *testing.T) {
	if got := (AnalyticsParams{}).location(); got != time.UTC {
		t.Errorf("location() = %v, want UTC", got)
	}
	berlin := mustLoad(t, "Europe/Berlin")
	if got := (AnalyticsParams{Loc: berlin}).location(); got != berlin {
		t.Errorf("location() = %v, want Europe/Berlin", got)
	}
}

// TestBucketKey_RendersInLocation covers the helper both other builders route
// through, at both granularities.
func TestBucketKey_RendersInLocation(t *testing.T) {
	tokyo := mustLoad(t, "Asia/Tokyo") // UTC+9, no DST
	at := time.Date(2026, 8, 7, 22, 30, 0, 0, time.UTC)

	if got := bucketKey(at, "daily", tokyo); got != "2026-08-08" {
		t.Errorf("daily key = %q, want 2026-08-08", got)
	}
	if got := bucketKey(at, "hourly", tokyo); got != "2026-08-08T07" {
		t.Errorf("hourly key = %q, want 2026-08-08T07", got)
	}
	if got := bucketKey(at, "daily", time.UTC); got != "2026-08-07" {
		t.Errorf("daily key in UTC = %q, want 2026-08-07", got)
	}
}

// TestWalkBuckets_DailyStepsCalendarDaysAcrossDST is why the stepping loops
// stopped adding 24 hours. Berlin's spring-forward day (2026-03-29) is 23 hours
// long, so a fixed 24h step drifts off the wall clock and emits one day twice
// while skipping another.
func TestWalkBuckets_DailyStepsCalendarDaysAcrossDST(t *testing.T) {
	berlin := mustLoad(t, "Europe/Berlin")
	from := time.Date(2026, 3, 27, 0, 0, 0, 0, berlin)
	to := time.Date(2026, 3, 31, 23, 59, 59, 0, berlin)

	var keys []string
	walkBuckets(from, to, "daily", berlin, func(key string, _ time.Time) {
		keys = append(keys, key)
	})

	want := []string{"2026-03-27", "2026-03-28", "2026-03-29", "2026-03-30", "2026-03-31"}
	if len(keys) != len(want) {
		t.Fatalf("keys = %v, want %v — a DST day must not be duplicated or skipped", keys, want)
	}
	for i := range want {
		if keys[i] != want[i] {
			t.Errorf("keys = %v, want %v", keys, want)
			break
		}
	}
}

// TestWalkBuckets_HourlyCoversFallBackDay checks the other DST direction. Berlin
// gains an hour on 2026-10-25, so that local day really does have 25 hours and
// the walk must emit them all rather than stopping an hour early.
func TestWalkBuckets_HourlyCoversFallBackDay(t *testing.T) {
	berlin := mustLoad(t, "Europe/Berlin")
	from := time.Date(2026, 10, 25, 0, 0, 0, 0, berlin)
	to := from.AddDate(0, 0, 1)

	count := 0
	walkBuckets(from, to, "hourly", berlin, func(string, time.Time) { count++ })

	// 25 wall-clock hours in the day, plus the inclusive endpoint at next midnight.
	if count != 26 {
		t.Errorf("hourly buckets across the fall-back day = %d, want 26", count)
	}
}

// TestAggregateAnalytics_ThreadsLocationToEveryBuilder guards against a builder
// being missed: half-converted analytics, where the heatmap is local but the
// day list is not, is worse than either being uniformly wrong.
func TestAggregateAnalytics_ThreadsLocationToEveryBuilder(t *testing.T) {
	berlin := mustLoad(t, "Europe/Berlin")
	sessions := []ClaudeSessionSummary{lateNightSession()}

	report := AggregateAnalytics(sessions, AnalyticsParams{
		From: time.Date(2026, 8, 1, 0, 0, 0, 0, berlin),
		To:   time.Date(2026, 8, 31, 23, 59, 59, 0, berlin),
		Loc:  berlin,
	})

	if len(report.MostActiveDays) != 1 || report.MostActiveDays[0].Date != "2026-08-08" {
		t.Errorf("most active days = %+v, want the 8th", report.MostActiveDays)
	}
	if len(report.Heatmap) != 1 || report.Heatmap[0].Hour != 0 {
		t.Errorf("heatmap = %+v, want hour 0", report.Heatmap)
	}
	if report.HourlyActivity[0].Sessions != 1 {
		t.Errorf("hourly activity hour 0 = %d sessions, want 1", report.HourlyActivity[0].Sessions)
	}

	// The daily series must carry the same local day key, or the chart and the
	// day list disagree about which day the work happened on.
	found := false
	for _, p := range report.TimeSeries {
		if p.Date == "2026-08-08" && p.Sessions == 1 {
			found = true
		}
	}
	if !found {
		t.Error("time series has no 2026-08-08 bucket with the session in it")
	}
}

// TestAggregateAnalytics_EmptyResultStillHonoursLocation covers the early-return
// branch, which builds hourly activity from a nil slice.
func TestAggregateAnalytics_EmptyResultStillHonoursLocation(t *testing.T) {
	berlin := mustLoad(t, "Europe/Berlin")
	report := AggregateAnalytics(nil, AnalyticsParams{
		From: time.Date(2026, 8, 1, 0, 0, 0, 0, berlin),
		To:   time.Date(2026, 8, 2, 0, 0, 0, 0, berlin),
		Loc:  berlin,
	})
	if len(report.HourlyActivity) != 24 {
		t.Errorf("hourly activity = %d buckets, want 24", len(report.HourlyActivity))
	}
}

// TestFilterSessions_UsesLocalDayBoundaries checks the range edges. A session at
// 00:30 Berlin on the 8th must fall inside a "8 Aug to 8 Aug" local range, even
// though its UTC instant is on the 7th.
func TestFilterSessions_UsesLocalDayBoundaries(t *testing.T) {
	berlin := mustLoad(t, "Europe/Berlin")
	sessions := []ClaudeSessionSummary{lateNightSession()}

	inLocalDay := FilterSessions(sessions, AnalyticsParams{
		From: time.Date(2026, 8, 8, 0, 0, 0, 0, berlin),
		To:   time.Date(2026, 8, 8, 23, 59, 59, 0, berlin),
		Loc:  berlin,
	})
	if len(inLocalDay) != 1 {
		t.Error("a session at 00:30 local on the 8th must be inside the local 8th")
	}

	// The same range expressed as UTC days excludes it, which is the old bug.
	inUTCDay := FilterSessions(sessions, AnalyticsParams{
		From: time.Date(2026, 8, 8, 0, 0, 0, 0, time.UTC),
		To:   time.Date(2026, 8, 8, 23, 59, 59, 0, time.UTC),
	})
	if len(inUTCDay) != 0 {
		t.Error("UTC day boundaries should not contain the 22:30Z session")
	}
}
