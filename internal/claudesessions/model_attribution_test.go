package claudesessions

import (
	"testing"
	"time"
)

// modelAttributionSessions builds a corpus that exercises the whole rule: a
// parent on model A that delegated to a cheaper model B, a parent that
// delegated to a nameless model, a parent that delegated to <synthetic>, and
// a session that delegated nothing at all.
func modelAttributionSessions() []ClaudeSessionSummary {
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	return []ClaudeSessionSummary{
		{
			SessionID: "mixed", Model: "claude-opus-4-8", StartTime: ts, LastActivity: ts,
			Usage:         TokenUsage{InputTokens: 100, OutputTokens: 50},
			SubagentUsage: TokenUsage{InputTokens: 800, OutputTokens: 50},
			SubagentUsageByModel: map[string]TokenUsage{
				"claude-haiku-4-5": {InputTokens: 800, OutputTokens: 50},
			},
		},
		{
			SessionID: "nameless", Model: "claude-opus-4-8", StartTime: ts, LastActivity: ts,
			Usage:         TokenUsage{InputTokens: 10},
			SubagentUsage: TokenUsage{InputTokens: 40},
			SubagentUsageByModel: map[string]TokenUsage{
				"": {InputTokens: 40},
			},
		},
		{
			SessionID: "synthetic-delegate", Model: "claude-opus-4-8", StartTime: ts, LastActivity: ts,
			Usage:         TokenUsage{InputTokens: 5},
			SubagentUsage: TokenUsage{InputTokens: 1000},
			SubagentUsageByModel: map[string]TokenUsage{
				syntheticModel: {InputTokens: 1000},
			},
		},
		{
			SessionID: "solo", Model: "claude-fable-5", StartTime: ts, LastActivity: ts,
			Usage: TokenUsage{InputTokens: 70, OutputTokens: 30},
		},
	}
}

func statFor(t *testing.T, stats []ModelStat, model string) ModelStat {
	t.Helper()
	for _, s := range stats {
		if s.Model == model {
			return s
		}
	}
	t.Fatalf("model %q missing from breakdown %+v", model, stats)
	return ModelStat{}
}

// TestBuildModelBreakdown_AttributesDelegatedTokensToTheirOwnModel is #192's
// acceptance case: a session on A that delegated to B must report B's tokens
// under B, not all of them under A.
func TestBuildModelBreakdown_AttributesDelegatedTokensToTheirOwnModel(t *testing.T) {
	got := buildModelBreakdown(modelAttributionSessions())

	// Haiku ran 800+50 delegated tokens and must be credited with exactly those.
	if h := statFor(t, got, "claude-haiku-4-5"); h.Tokens != 850 {
		t.Errorf("haiku tokens = %d, want 850 (the delegated work it actually did)", h.Tokens)
	}
	// Opus keeps only its own main-thread work across the three sessions it
	// parented: 150 + 10 + 5.
	if o := statFor(t, got, "claude-opus-4-8"); o.Tokens != 165 {
		t.Errorf("opus tokens = %d, want 165 (main thread only)", o.Tokens)
	}
	// A sub-agent with no model name lands in "unknown", matching the
	// parent-side fallback rather than being dropped.
	if u := statFor(t, got, "unknown"); u.Tokens != 40 {
		t.Errorf("unknown tokens = %d, want 40", u.Tokens)
	}
	// A delegated <synthetic> run is excluded, exactly as a parent one is.
	for _, s := range got {
		if s.Model == syntheticModel {
			t.Errorf("<synthetic> leaked into the breakdown via a sub-agent: %+v", s)
		}
	}
}

// TestBuildModelBreakdown_TotalsAreUnchanged is the guard the issue calls for
// first: this change redistributes attribution and must not move a single
// token. The expected total is computed from TotalUsage(), the definition every
// other builder uses, minus only what the synthetic rule excludes.
func TestBuildModelBreakdown_TotalsAreUnchanged(t *testing.T) {
	sessions := modelAttributionSessions()

	want := 0
	for _, s := range sessions {
		u := s.TotalUsage()
		want += u.InputTokens + u.OutputTokens
	}
	// The one deliberate exclusion: the 1000 synthetic delegated tokens.
	want -= 1000

	got := buildModelBreakdown(sessions)
	sum := 0
	pct := 0.0
	for _, s := range got {
		sum += s.Tokens
		pct += s.Percentage
	}
	if sum != want {
		t.Errorf("breakdown total = %d, want %d — attribution moved tokens instead of just re-keying them", sum, want)
	}
	if pct < 99.5 || pct > 100.5 {
		t.Errorf("percentages sum to %.1f, want ~100", pct)
	}
}

// TestBuildModelBreakdown_SubagentUsageSumsToTheAggregate pins the two
// sub-agent reads together. The summary select rolls delegated tokens up by
// session and attachSubagentUsageByModel rolls the same rows up by session and
// model; if they ever disagree, the breakdown and every other panel would
// silently describe different corpora.
func TestBuildModelBreakdown_SubagentUsageSumsToTheAggregate(t *testing.T) {
	for _, s := range modelAttributionSessions() {
		if len(s.SubagentUsageByModel) == 0 {
			continue
		}
		var sum TokenUsage
		for _, u := range s.SubagentUsageByModel {
			sum.InputTokens += u.InputTokens
			sum.OutputTokens += u.OutputTokens
			sum.CacheCreationTokens += u.CacheCreationTokens
			sum.CacheReadTokens += u.CacheReadTokens
		}
		if sum != s.SubagentUsage {
			t.Errorf("%s: per-model sum %+v != SubagentUsage %+v", s.SessionID, sum, s.SubagentUsage)
		}
	}
}

// TestBuildModelBreakdown_FallsBackWhenBreakdownAbsent covers a summary built
// without the per-model read: the tokens must still be counted somewhere.
// Misattributing them is the bug being fixed; dropping them would make this
// chart's total disagree with every other total on the dashboard.
func TestBuildModelBreakdown_FallsBackWhenBreakdownAbsent(t *testing.T) {
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{{
		Model: "claude-opus-4-8", StartTime: ts, LastActivity: ts,
		Usage:         TokenUsage{InputTokens: 100},
		SubagentUsage: TokenUsage{InputTokens: 900},
		// SubagentUsageByModel deliberately nil.
	}}

	got := buildModelBreakdown(sessions)
	if len(got) != 1 || got[0].Tokens != 1000 {
		t.Errorf("breakdown = %+v, want all 1000 tokens retained", got)
	}
}

// TestBuildModelBreakdown_NoSubagentsUnchanged is the regression guard for the
// overwhelmingly common case.
func TestBuildModelBreakdown_NoSubagentsUnchanged(t *testing.T) {
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{
		{Model: "claude-opus-4-8", StartTime: ts, LastActivity: ts, Usage: TokenUsage{InputTokens: 300, OutputTokens: 100}},
		{Model: "claude-fable-5", StartTime: ts, LastActivity: ts, Usage: TokenUsage{InputTokens: 100}},
	}
	got := buildModelBreakdown(sessions)
	if len(got) != 2 {
		t.Fatalf("expected 2 models, got %+v", got)
	}
	if o := statFor(t, got, "claude-opus-4-8"); o.Tokens != 400 || o.Percentage != 80 {
		t.Errorf("opus = %+v, want 400 tokens at 80%%", o)
	}
	if f := statFor(t, got, "claude-fable-5"); f.Tokens != 100 || f.Percentage != 20 {
		t.Errorf("fable = %+v, want 100 tokens at 20%%", f)
	}
}

// TestBuildSessionsPerModel_StillCountsUnderTheParent pins the deliberate
// asymmetry: a session's model is the parent's by definition, so the session
// count chart must NOT gain the sub-agent's model.
func TestBuildSessionsPerModel_StillCountsUnderTheParent(t *testing.T) {
	got := buildSessionsPerModel(modelAttributionSessions())
	for _, s := range got {
		if s.Model == "claude-haiku-4-5" {
			t.Error("sessions_per_model gained a delegated model — a session belongs to its parent")
		}
	}
	total := 0
	for _, s := range got {
		total += s.Sessions
	}
	if total != 4 {
		t.Errorf("session count total = %d, want 4 — one per session", total)
	}
}

// TestBuildModelBreakdown_SyntheticParentKeepsRealDelegatedTokens documents a
// deliberate consequence of moving the <synthetic> skip from per-session to
// per-model: a session whose own model is the placeholder used to have its
// delegated tokens dropped with it. Those tokens are real work by a real
// model, so they are now counted under that model — and only the placeholder's
// own tokens are excluded.
func TestBuildModelBreakdown_SyntheticParentKeepsRealDelegatedTokens(t *testing.T) {
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	sessions := []ClaudeSessionSummary{{
		Model: syntheticModel, StartTime: ts, LastActivity: ts,
		Usage:         TokenUsage{InputTokens: 500},
		SubagentUsage: TokenUsage{InputTokens: 300},
		SubagentUsageByModel: map[string]TokenUsage{
			"claude-haiku-4-5": {InputTokens: 300},
		},
	}}

	got := buildModelBreakdown(sessions)
	if len(got) != 1 {
		t.Fatalf("expected only the delegated model, got %+v", got)
	}
	if got[0].Model != "claude-haiku-4-5" || got[0].Tokens != 300 {
		t.Errorf("got %+v, want haiku with its 300 delegated tokens", got[0])
	}
	// The placeholder's own 500 tokens stay excluded — that is the rule that
	// has not changed.
	for _, s := range got {
		if s.Model == syntheticModel {
			t.Error("<synthetic> leaked into the breakdown")
		}
	}
}
