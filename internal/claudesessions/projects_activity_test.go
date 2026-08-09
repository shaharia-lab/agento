package claudesessions

import (
	"math"
	"testing"
	"time"
)

// spanSession is a session with an explicit start and end, for the activity
// bucketing tests.
func spanSession(id, project string, start, end time.Time, tokens int, costUSD float64) ClaudeSessionSummary {
	return ClaudeSessionSummary{
		SessionID:    id,
		ProjectPath:  project,
		Model:        "claude-opus-5",
		StartTime:    start,
		LastActivity: end,
		Usage:        TokenUsage{InputTokens: tokens},
		Cost:         SessionCost{InputUSD: costUSD, TotalUSD: costUSD},
		CostByModel:  map[string]SessionCost{"claude-opus-5": {InputUSD: costUSD, TotalUSD: costUSD}},
	}
}

// TestHourlyActivity_SpreadsOverTheSessionSpan is the fix for a chart whose
// title and content disagreed: bucketing a session at its last activity made
// "Activity by Hour of Day" answer "when do my sessions end".
func TestHourlyActivity_SpreadsOverTheSessionSpan(t *testing.T) {
	// 09:00 → 12:00: three full hours of work.
	start := time.Date(2026, 8, 3, 9, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{
		spanSession("long", "/proj", start, start.Add(3*time.Hour), 300, 3),
	}

	hours := buildHourlyActivity(sessions, time.UTC)
	for _, h := range []int{9, 10, 11} {
		if hours[h].Sessions != 1 {
			t.Errorf("hour %d: sessions = %d, want the session counted as active", h, hours[h].Sessions)
		}
		if hours[h].Tokens != 100 {
			t.Errorf("hour %d: tokens = %d, want an even third of 300", h, hours[h].Tokens)
		}
	}
	if hours[12].Sessions != 0 {
		t.Errorf("hour 12 is the instant it ended, not an hour worked: got %d", hours[12].Sessions)
	}

	// Tokens are shared out, never multiplied: the corpus total must survive.
	total := 0
	for _, h := range hours {
		total += h.Tokens
	}
	if total != 300 {
		t.Errorf("tokens across the day = %d, want the session's 300", total)
	}
}

// TestHourlyActivity_InstantSessionOccupiesOneHour covers the degenerate span.
func TestHourlyActivity_InstantSessionOccupiesOneHour(t *testing.T) {
	at := time.Date(2026, 8, 3, 14, 30, 0, 0, time.UTC)
	hours := buildHourlyActivity([]ClaudeSessionSummary{
		spanSession("instant", "/proj", at, at, 50, 1),
	}, time.UTC)

	if hours[14].Sessions != 1 || hours[14].Tokens != 50 {
		t.Errorf("hour 14 = %+v, want the whole session", hours[14])
	}
}

// TestHourlyActivity_AbsurdSpanFallsBackToTheEndHour keeps one broken time
// range from painting thousands of cells and swamping the chart.
func TestHourlyActivity_AbsurdSpanFallsBackToTheEndHour(t *testing.T) {
	start := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	end := start.AddDate(0, 0, 60)
	hours := buildHourlyActivity([]ClaudeSessionSummary{
		spanSession("broken", "/proj", start, end, 100, 1),
	}, time.UTC)

	active := 0
	for _, h := range hours {
		active += h.Sessions
	}
	if active != 1 {
		t.Errorf("a 60-day span must collapse to one hour, got %d active hours", active)
	}
	if hours[end.Hour()].Sessions != 1 {
		t.Errorf("the fallback hour should be the end hour (%d), got %+v", end.Hour(), hours)
	}
}

// TestHeatmap_SpansMidnight checks a session that crosses into the next day
// lands on both weekdays, which is what the drill-down has always selected.
func TestHeatmap_SpansMidnight(t *testing.T) {
	start := time.Date(2026, 8, 3, 23, 0, 0, 0, time.UTC) // Monday 23:00
	sessions := []ClaudeSessionSummary{
		spanSession("nightowl", "/proj", start, start.Add(2*time.Hour), 200, 2),
	}

	cells := buildHeatmap(sessions, time.UTC)
	got := map[string]int{}
	for _, c := range cells {
		got[time.Weekday(c.DayOfWeek).String()+"-"+itoa(c.Hour)] = c.Sessions
	}
	for _, key := range []string{"Monday-23", "Tuesday-0"} {
		if got[key] != 1 {
			t.Errorf("cell %s = %d, want the session counted there too (cells: %+v)", key, got[key], got)
		}
	}
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	digits := ""
	for n > 0 {
		digits = string(rune('0'+n%10)) + digits
		n /= 10
	}
	return digits
}

// TestBuildProjectBreakdown covers the aggregate that gives "which project is
// my money going to" its first answer anywhere in the product.
func TestBuildProjectBreakdown(t *testing.T) {
	at := time.Date(2026, 8, 3, 9, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{
		spanSession("a1", "/proj/alpha", at, at.Add(time.Hour), 100, 10),
		spanSession("a2", "/proj/alpha", at.Add(2*time.Hour), at.Add(3*time.Hour), 100, 5),
		spanSession("b1", "/proj/beta", at, at.Add(time.Hour), 400, 2),
	}

	stats := buildProjectBreakdown(sessions)
	if len(stats) != 2 {
		t.Fatalf("expected two projects, got %+v", stats)
	}
	// Ranked by spend, not by session count or tokens: beta has four times the
	// tokens and an eighth of the cost.
	if stats[0].Project != "/proj/alpha" {
		t.Errorf("first by spend = %q, want /proj/alpha", stats[0].Project)
	}
	if stats[0].Sessions != 2 || math.Abs(stats[0].Cost.TotalUSD-15) > 1e-9 {
		t.Errorf("alpha = %+v, want 2 sessions costing 15", stats[0])
	}
	if !stats[0].LastActivity.Equal(at.Add(3 * time.Hour)) {
		t.Errorf("alpha last activity = %s, want the latest of its sessions", stats[0].LastActivity)
	}
	if math.Abs(stats[0].Percentage-88.2) > 0.1 {
		t.Errorf("alpha share = %v%%, want ~88.2", stats[0].Percentage)
	}
}

// TestBuildTopSessions covers the leaderboards, including that they rank by
// different measures rather than being three views of one order.
func TestBuildTopSessions(t *testing.T) {
	at := time.Date(2026, 8, 3, 9, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{
		spanSession("cheap-but-long", "/p", at, at.Add(8*time.Hour), 10, 1),
		spanSession("expensive-but-short", "/p", at, at.Add(5*time.Minute), 20, 100),
		spanSession("free", "/p", at, at.Add(time.Minute), 0, 0),
	}

	top := buildTopSessions(sessions)
	if len(top.ByCost) == 0 || top.ByCost[0].SessionID != "expensive-but-short" {
		t.Errorf("ByCost[0] = %+v, want expensive-but-short", top.ByCost)
	}
	if len(top.ByDuration) == 0 || top.ByDuration[0].SessionID != "cheap-but-long" {
		t.Errorf("ByDuration[0] = %+v, want cheap-but-long", top.ByDuration)
	}
	// A $0.00 row on a "most expensive" board states nothing, so zero scores are
	// dropped rather than padding the board to its full length.
	for _, r := range top.ByCost {
		if r.SessionID == "free" {
			t.Error("a zero-cost session must not appear on the cost leaderboard")
		}
	}
	if len(top.ByTokens) != 2 {
		t.Errorf("ByTokens = %+v, want only the two sessions with tokens", top.ByTokens)
	}
}

// TestBuildProjectActivity limits the strip to the busiest projects while the
// table keeps them all.
func TestBuildProjectActivity(t *testing.T) {
	at := time.Date(2026, 8, 3, 9, 0, 0, 0, time.UTC)
	sessions := make([]ClaudeSessionSummary, 0, topProjectsCharted+3)
	for i := range topProjectsCharted + 3 {
		project := "/proj/" + itoa(i)
		// Descending cost, so the last three are the ones dropped.
		sessions = append(sessions, spanSession("s"+itoa(i), project, at, at.Add(time.Hour), 10, float64(100-i)))
	}

	stats := buildProjectBreakdown(sessions)
	activity := buildProjectActivity(sessions, stats, time.UTC)

	if len(stats) != topProjectsCharted+3 {
		t.Errorf("the table must keep every project, got %d", len(stats))
	}
	charted := map[string]struct{}{}
	for _, a := range activity {
		charted[a.Project] = struct{}{}
	}
	if len(charted) != topProjectsCharted {
		t.Errorf("the strip charted %d projects, want %d", len(charted), topProjectsCharted)
	}
}

// TestHourlyActivity_HalfHourOffsetZone guards the cell boundary against the
// zones where a UTC-based truncation goes wrong.
//
// time.Truncate works in UTC, so at +05:30 it puts the boundary at :30 local,
// splitting one local hour across two cells and counting the session in it
// twice. India, Nepal and Iran are all in this class.
func TestHourlyActivity_HalfHourOffsetZone(t *testing.T) {
	for _, zone := range []string{"Asia/Kolkata", "Asia/Kathmandu"} {
		loc, err := time.LoadLocation(zone)
		if err != nil {
			t.Skipf("%s unavailable: %v", zone, err)
		}
		t.Run(zone, func(t *testing.T) {
			// 09:00 → 11:00 local is exactly two hours of work.
			start := time.Date(2026, 8, 3, 9, 0, 0, 0, loc)
			hours := buildHourlyActivity([]ClaudeSessionSummary{
				spanSession("s", "/p", start, start.Add(2*time.Hour), 200, 2),
			}, loc)

			for hour, want := range map[int]int{9: 1, 10: 1} {
				if hours[hour].Sessions != want {
					t.Errorf("hour %d: sessions = %d, want %d", hour, hours[hour].Sessions, want)
				}
			}
			cells, tokens := 0, 0
			for _, h := range hours {
				cells += h.Sessions
				tokens += h.Tokens
			}
			if cells != 2 {
				t.Errorf("session counted in %d hour cells, want 2", cells)
			}
			if tokens != 200 {
				t.Errorf("tokens across the day = %d, want the session's 200", tokens)
			}
		})
	}
}
