package parity

import (
	"bytes"
	"encoding/json"
	"os"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/claudesessions"
)

// The golden response for GET /api/claude-analytics over the fixture corpus
// below.
//
// Written by Go (`go test ./desktop/parity/ -update-golden`) and asserted by
// both languages: the Rust port builds the identical sessions in
// desktop/src-tauri/src/native/analytics/tests_golden.rs and must produce this
// file byte for byte. The live diff proves the port against the real corpus,
// but it needs a running server and a database; this runs in CI.
const analyticsGoldenFile = "claude_analytics_golden.json"

// The fixture deliberately contains **no ties on any sort key**.
//
// Several analytics builders collect into a Go map and then sort with
// `sort.Slice`, which is unstable, so two rows tying on the sort key come out
// in a random order and Go's own output is not reproducible. That is real —
// `sessions_per_model` flaps on the reference corpus — and a golden file built
// over a fixture with ties would be flaky in Go before Rust ever saw it. Every
// model, project, cost, duration and token total below is therefore distinct.
//
// What it does cover: three bucket-relevant timezone effects (a session
// crossing local midnight, a session spanning several hours, a session with no
// duration at all), the DST spring-forward hour that does not exist, delegated
// work under a different model, an unpriced model, a `<synthetic>` session that
// every breakdown must skip, empty buckets inside the window, and a session
// outside the window that must still appear in the project picker.
func analyticsFixture(t *testing.T) []claudesessions.ClaudeSessionSummary {
	t.Helper()

	at := func(text string) time.Time {
		parsed, err := time.Parse(time.RFC3339, text)
		if err != nil {
			t.Fatalf("fixture timestamp %q: %v", text, err)
		}
		return parsed
	}
	cost := func(in, out, read, write, total float64) claudesessions.SessionCost {
		return claudesessions.SessionCost{
			InputUSD: in, OutputUSD: out, CacheReadUSD: read, CacheWriteUSD: write, TotalUSD: total,
		}
	}
	usage := func(in, out, read, write int) claudesessions.TokenUsage {
		return claudesessions.TokenUsage{
			InputTokens: in, OutputTokens: out,
			CacheReadTokens: read, CacheCreationTokens: write,
			CacheCreation5mTokens: write, CacheCreation1hTokens: 0,
		}
	}

	return []claudesessions.ClaudeSessionSummary{
		{
			// Spans four local hours, and delegates to a different model — the
			// case model attribution exists for.
			SessionID: "s1", ProjectPath: "/work/alpha", Preview: "alpha one",
			Model:     "claude-opus-5",
			StartTime: at("2026-03-20T08:00:00Z"), LastActivity: at("2026-03-20T11:30:00Z"),
			ActiveDurationMs: 600000, SubagentActiveDurationMs: 180000,
			MessageCount: 12, EventCount: 40,
			Usage:         usage(1000, 200, 50000, 3000),
			SubagentCount: 2,
			SubagentUsage: usage(500, 100, 0, 0),
			SubagentUsageByModel: map[string]claudesessions.TokenUsage{
				"claude-haiku-4-5-20251001": usage(500, 100, 0, 0),
			},
			Cost:         cost(2, 3, 1, 4, 10),
			SubagentCost: cost(0.5, 1.25, 0.25, 1, 3),
			CostByModel: map[string]claudesessions.SessionCost{
				"claude-opus-5": cost(2, 3, 1, 4, 10),
			},
			SubagentCostByModel: map[string]claudesessions.SessionCost{
				"claude-haiku-4-5-20251001": cost(0.5, 1.25, 0.25, 1, 3),
			},
		},
		{
			// Crosses local midnight, so its bucket is decided in Berlin rather
			// than in UTC. A non-caching backend: most of the tokens, almost
			// none of them served from cache — the low-cache card's subject.
			SessionID: "s2", ProjectPath: "/work/alpha", Preview: "alpha two",
			CustomTitle: "renamed by hand",
			Model:       "k3",
			StartTime:   at("2026-03-25T22:00:00Z"), LastActivity: at("2026-03-26T01:00:00Z"),
			ActiveDurationMs: 900000,
			MessageCount:     30, EventCount: 96,
			Usage: usage(200000, 50000, 5000, 0),
			Cost:  cost(8, 6, 0.1, 0, 14.1),
			CostByModel: map[string]claudesessions.SessionCost{
				"k3": cost(8, 6, 0.1, 0, 14.1),
			},
		},
		{
			// Spans the EU spring-forward: 02:00–03:00 local does not exist on
			// 2026-03-29, so an hour walk that stepped UTC hours would land in a
			// cell no clock ever showed.
			SessionID: "s3", ProjectPath: "/work/beta", Preview: "beta one",
			Model:     "claude-opus-5",
			StartTime: at("2026-03-29T00:30:00Z"), LastActivity: at("2026-03-29T02:30:00Z"),
			ActiveDurationMs: 300000,
			MessageCount:     8, EventCount: 25,
			Usage: usage(3000, 700, 80000, 4000),
			Cost:  cost(1.25, 2, 1.5, 2.5, 7.25),
			CostByModel: map[string]claudesessions.SessionCost{
				"claude-opus-5": cost(1.25, 2, 1.5, 2.5, 7.25),
			},
		},
		{
			// A model with no published rate: it contributes tokens but no cost,
			// and says so through the unpriced bucket rather than being priced
			// as something else.
			SessionID: "s4", ProjectPath: "/work/gamma", Preview: "gamma one",
			Model:     "glm-5.2",
			StartTime: at("2026-04-01T09:00:00Z"), LastActivity: at("2026-04-01T09:45:00Z"),
			ActiveDurationMs: 150000,
			MessageCount:     5, EventCount: 14,
			Usage:          usage(9000, 1200, 0, 0),
			UnpricedModels: []string{"glm-5.2"},
			UnpricedTokens: 10200,
		},
		{
			// Locally generated, never billed. Every model breakdown skips it,
			// but its tokens still reach the totals.
			SessionID: "s5", ProjectPath: "/work/beta", Preview: "beta two",
			Model:     "<synthetic>",
			StartTime: at("2026-04-02T12:00:00Z"), LastActivity: at("2026-04-02T12:05:00Z"),
			ActiveDurationMs: 30000,
			MessageCount:     2, EventCount: 4,
			Usage: usage(50, 10, 0, 0),
		},
		{
			// No measurable duration: one hour cell at full weight.
			SessionID: "s6", ProjectPath: "/work/alpha", Preview: "alpha three",
			Model:     "claude-opus-5",
			StartTime: at("2026-04-03T10:00:00Z"), LastActivity: at("2026-04-03T10:00:00Z"),
			ActiveDurationMs: 450000,
			MessageCount:     20, EventCount: 61,
			Usage: usage(4000, 900, 20000, 1000),
			Cost:  cost(4, 9, 2, 7, 22),
			CostByModel: map[string]claudesessions.SessionCost{
				"claude-opus-5": cost(4, 9, 2, 7, 22),
			},
		},
		{
			SessionID: "s8", ProjectPath: "/work/delta", Preview: "delta one",
			Model:     "k3",
			StartTime: at("2026-03-31T16:00:00Z"), LastActivity: at("2026-03-31T17:15:00Z"),
			ActiveDurationMs: 240000,
			MessageCount:     9, EventCount: 27,
			Usage: usage(6000, 1500, 0, 0),
			Cost:  cost(2, 3, 0, 0.5, 5.5),
			CostByModel: map[string]claudesessions.SessionCost{
				"k3": cost(2, 3, 0, 0.5, 5.5),
			},
		},
		{
			// Outside the window: filtered out of every figure, but its project
			// must still be offered by the picker, or a user who filters into an
			// empty window cannot filter back out of it.
			SessionID: "s7", ProjectPath: "/work/epsilon", Preview: "epsilon one",
			Model:     "claude-opus-5",
			StartTime: at("2026-01-05T10:00:00Z"), LastActivity: at("2026-01-05T12:00:00Z"),
			ActiveDurationMs: 111000,
			MessageCount:     3, EventCount: 9,
			Usage: usage(700, 100, 0, 0),
			Cost:  cost(1, 1, 0, 0, 2),
		},
	}
}

// analyticsParams is the window the golden is built over, hand-built to exactly
// what `parseAnalyticsParams` produces for
// `?from=2026-03-20&to=2026-04-03&tz=Europe/Berlin` — a bare date is a *local*
// day, and a bare range end is that day's final second. The Rust side parses
// that query string rather than hand-building, so the parser is checked against
// this too.
func analyticsParams(t *testing.T) claudesessions.AnalyticsParams {
	t.Helper()
	loc, err := time.LoadLocation("Europe/Berlin")
	if err != nil {
		t.Fatalf("loading Europe/Berlin: %v", err)
	}
	return claudesessions.AnalyticsParams{
		From:    time.Date(2026, 3, 20, 0, 0, 0, 0, loc),
		To:      time.Date(2026, 4, 3, 23, 59, 59, 0, loc),
		Project: "",
		Loc:     loc,
	}
}

func TestClaudeAnalyticsGolden(t *testing.T) {
	// No pricing store is wired in this package, so `defaultPricingResolver`
	// answers nil and the cache-savings card — the one figure that prices a
	// counterfactual rather than reading a stored total — is not emitted. The
	// Rust side passes no resolver for the same reason. Wiring one here would
	// seed the whole built-in catalog and make the golden move whenever
	// catalog.json does; the live diff covers that card against real rates.
	report := claudesessions.AggregateAnalytics(analyticsFixture(t), analyticsParams(t))

	// Exactly what internal/api.Server.writeJSON does, newline included.
	var buf bytes.Buffer
	if err := json.NewEncoder(&buf).Encode(report); err != nil {
		t.Fatalf("encoding report: %v", err)
	}
	got := buf.String()

	if *updateGolden {
		if err := os.WriteFile(analyticsGoldenFile, []byte(got), 0o600); err != nil {
			t.Fatalf("writing %s: %v", analyticsGoldenFile, err)
		}
		t.Logf("wrote %s (%d bytes)", analyticsGoldenFile, len(got))
		return
	}

	want, err := os.ReadFile(analyticsGoldenFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-golden): %v", analyticsGoldenFile, err)
	}
	if got != string(want) {
		t.Errorf("analytics JSON drifted from %s.\n got: %s\nwant: %s", analyticsGoldenFile, got, want)
	}
}

// The fixture is only useful while it has no ties: Go's own output is not
// reproducible where two rows share a sort key, so a tie would make the golden
// flaky rather than wrong. This states the property the fixture is built to
// have, so a later edit that introduces one fails here rather than
// intermittently.
func TestAnalyticsFixtureHasNoTiesOnAnySortKey(t *testing.T) {
	report := claudesessions.AggregateAnalytics(analyticsFixture(t), analyticsParams(t))

	assertDistinct := func(name string, values []float64) {
		t.Helper()
		seen := map[float64]bool{}
		for _, v := range values {
			if seen[v] {
				t.Errorf("%s has a tie at %v; the golden would be nondeterministic", name, v)
			}
			seen[v] = true
		}
	}

	counts := make([]float64, 0, len(report.SessionsPerModel))
	tokens := make([]float64, 0, len(report.ModelBreakdown))
	costs := make([]float64, 0, len(report.CostByModel))
	days := make([]float64, 0, len(report.MostActiveDays))
	for _, m := range report.SessionsPerModel {
		counts = append(counts, float64(m.Sessions))
	}
	for _, m := range report.ModelBreakdown {
		tokens = append(tokens, float64(m.Tokens))
	}
	for _, m := range report.CostByModel {
		costs = append(costs, m.Cost.TotalUSD)
	}
	for _, d := range report.MostActiveDays {
		days = append(days, float64(d.Tokens))
	}
	assertDistinct("sessions_per_model", counts)
	assertDistinct("model_breakdown", tokens)
	assertDistinct("cost_by_model", costs)
	assertDistinct("most_active_days", days)

	boards := []struct {
		name  string
		rows  []claudesessions.SessionRanking
		score func(claudesessions.SessionRanking) float64
	}{
		{"top_sessions.by_cost", report.TopSessions.ByCost,
			func(r claudesessions.SessionRanking) float64 { return r.CostUSD }},
		{"top_sessions.by_duration", report.TopSessions.ByDuration,
			func(r claudesessions.SessionRanking) float64 { return float64(r.DurationMs) }},
		{"top_sessions.by_tokens", report.TopSessions.ByTokens,
			func(r claudesessions.SessionRanking) float64 { return float64(r.Tokens) }},
	}
	for _, board := range boards {
		var scores []float64
		for _, r := range board.rows {
			scores = append(scores, board.score(r))
		}
		assertDistinct(board.name, scores)
	}
}
