package claudesessions_test

import (
	"encoding/json"
	"math"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/claudesessions"
)

// ─── helpers ──────────────────────────────────────────────────────────────────

func makeEvent(eventType string, opts ...func(*claudesessions.ProcessableEvent)) claudesessions.ProcessableEvent {
	ev := claudesessions.ProcessableEvent{
		Type:      eventType,
		Timestamp: time.Now(),
	}
	for _, o := range opts {
		o(&ev)
	}
	return ev
}

func withTS(t time.Time) func(*claudesessions.ProcessableEvent) {
	return func(ev *claudesessions.ProcessableEvent) { ev.Timestamp = t }
}

func withSidechain() func(*claudesessions.ProcessableEvent) {
	return func(ev *claudesessions.ProcessableEvent) { ev.IsSidechain = true }
}

func withMessage(role, model string, content any, usage *claudesessions.EventUsage) func(*claudesessions.ProcessableEvent) {
	return func(ev *claudesessions.ProcessableEvent) {
		raw, _ := json.Marshal(content)
		ev.Message = &claudesessions.EventMessage{
			Role:    role,
			Model:   model,
			Content: raw,
			Usage:   usage,
		}
	}
}

func toolUseBlocks(names ...string) []map[string]any {
	blocks := make([]map[string]any, len(names))
	for i, n := range names {
		blocks[i] = map[string]any{"type": "tool_use", "id": "id-" + n, "name": n}
	}
	return blocks
}

func toolResultBlocks(errFlags ...bool) []map[string]any {
	blocks := make([]map[string]any, len(errFlags))
	for i, isErr := range errFlags {
		blocks[i] = map[string]any{"type": "tool_result", "tool_use_id": "id", "is_error": isErr}
	}
	return blocks
}

func textBlocks(s string) []map[string]any {
	return []map[string]any{{"type": "text", "text": s}}
}

func runProcessors(evs []claudesessions.ProcessableEvent, processors ...claudesessions.SessionProcessor) *claudesessions.SessionInsight {
	for _, p := range processors {
		p.Reset()
	}
	for _, ev := range evs {
		for _, p := range processors {
			p.Process(ev)
		}
	}
	insight := &claudesessions.SessionInsight{ToolBreakdown: make(map[string]int)}
	for _, p := range processors {
		p.Finalize(insight)
	}
	return insight
}

func writeJSONLFile(t *testing.T, rows []map[string]any) string {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "session.jsonl")
	data := make([]byte, 0, len(rows)*128)
	for _, row := range rows {
		b, err := json.Marshal(row)
		if err != nil {
			t.Fatal(err)
		}
		data = append(data, b...)
		data = append(data, '\n')
	}
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatal(err)
	}
	return path
}

// ─── TurnCountProcessor ───────────────────────────────────────────────────────

func TestTurnCountProcessor_NoEvents(t *testing.T) {
	insight := runProcessors(nil, &claudesessions.TurnCountProcessor{})
	if insight.TurnCount != 0 {
		t.Errorf("expected 0 turns, got %d", insight.TurnCount)
	}
	if insight.StepsPerTurnAvg != 0 {
		t.Errorf("expected 0 steps avg, got %f", insight.StepsPerTurnAvg)
	}
}

func TestTurnCountProcessor_BasicTurns(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user", withMessage("user", "", textBlocks("hello"), nil)), // turn 1
		makeEvent("assistant", withMessage("assistant", "", textBlocks("hi"), nil)),
		makeEvent("user", withMessage("user", "", textBlocks("again"), nil)), // turn 2
		makeEvent("assistant", withMessage("assistant", "", textBlocks("ok"), nil)),
	}
	insight := runProcessors(evs, &claudesessions.TurnCountProcessor{})
	if insight.TurnCount != 2 {
		t.Errorf("expected 2 turns, got %d", insight.TurnCount)
	}
	// StepsPerTurnAvg = 4 events / 2 turns = 2
	if insight.StepsPerTurnAvg != 2.0 {
		t.Errorf("expected steps_per_turn_avg=2.0, got %f", insight.StepsPerTurnAvg)
	}
}

func TestTurnCountProcessor_ToolResultIsNotATurn(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user", withMessage("user", "", textBlocks("hi"), nil)),
		makeEvent("user", withMessage("user", "", toolResultBlocks(false), nil)), // NOT a turn
	}
	insight := runProcessors(evs, &claudesessions.TurnCountProcessor{})
	if insight.TurnCount != 1 {
		t.Errorf("expected 1 turn, got %d", insight.TurnCount)
	}
}

func TestTurnCountProcessor_SidechainIgnored(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user", withMessage("user", "", textBlocks("real"), nil)),
		makeEvent("user", withMessage("user", "", textBlocks("side"), nil), withSidechain()),
	}
	insight := runProcessors(evs, &claudesessions.TurnCountProcessor{})
	if insight.TurnCount != 1 {
		t.Errorf("expected 1 turn (sidechain skipped), got %d", insight.TurnCount)
	}
}

func TestTurnCountProcessor_Reset(t *testing.T) {
	p := &claudesessions.TurnCountProcessor{}
	p.Process(makeEvent("user", withMessage("user", "", textBlocks("hi"), nil)))
	p.Reset()
	insight := &claudesessions.SessionInsight{ToolBreakdown: make(map[string]int)}
	p.Finalize(insight)
	if insight.TurnCount != 0 {
		t.Errorf("expected 0 after reset, got %d", insight.TurnCount)
	}
}

// ─── AutonomyScoreProcessor ───────────────────────────────────────────────────

func TestAutonomyScoreProcessor_OneTurnManySteps(t *testing.T) {
	insight := &claudesessions.SessionInsight{
		TurnCount:       1,
		StepsPerTurnAvg: 20,
		ToolBreakdown:   make(map[string]int),
	}
	p := &claudesessions.AutonomyScoreProcessor{}
	p.Finalize(insight)
	if insight.AutonomyScore <= 50 {
		t.Errorf("expected high autonomy score for 1 turn/20 steps, got %f", insight.AutonomyScore)
	}
	if insight.AutonomyScore > 100 {
		t.Errorf("score exceeds 100: %f", insight.AutonomyScore)
	}
}

func TestAutonomyScoreProcessor_ManyTurns(t *testing.T) {
	insight := &claudesessions.SessionInsight{
		TurnCount:       10,
		StepsPerTurnAvg: 1,
		ToolBreakdown:   make(map[string]int),
	}
	p := &claudesessions.AutonomyScoreProcessor{}
	p.Finalize(insight)
	if insight.AutonomyScore >= 20 {
		t.Errorf("expected low score for 10 turns/1 step, got %f", insight.AutonomyScore)
	}
}

// A session with no turns AND no steps is genuinely empty, and 0 is the right
// score for it. This is the degenerate case, not the skill-driven one — see
// TestAutonomyScoreProcessor_ZeroTurnsWithStepsScoresHigh.
func TestAutonomyScoreProcessor_ZeroTurnsAndNoSteps(t *testing.T) {
	insight := &claudesessions.SessionInsight{ToolBreakdown: make(map[string]int)}
	p := &claudesessions.AutonomyScoreProcessor{}
	p.Finalize(insight)
	if insight.AutonomyScore != 0 {
		t.Errorf("expected 0 score for an empty session, got %f", insight.AutonomyScore)
	}
}

// Since #226 a session driven entirely by one skill invocation has no genuine
// user turn — the argument is embedded in the injected preamble. Those are the
// most autonomous sessions in the corpus and must not score 0, the opposite
// extreme, just because the turn counter reads 0.
func TestAutonomyScoreProcessor_ZeroTurnsWithStepsScoresHigh(t *testing.T) {
	insight := &claudesessions.SessionInsight{
		TurnCount:       0,
		StepsPerTurnAvg: 20,
		ToolBreakdown:   make(map[string]int),
	}
	p := &claudesessions.AutonomyScoreProcessor{}
	p.Finalize(insight)
	if insight.AutonomyScore <= 50 {
		t.Errorf("expected a high score for an unattended 20-step session, got %f", insight.AutonomyScore)
	}

	oneTurn := &claudesessions.SessionInsight{
		TurnCount:       1,
		StepsPerTurnAvg: 20,
		ToolBreakdown:   make(map[string]int),
	}
	p.Finalize(oneTurn)
	if insight.AutonomyScore != oneTurn.AutonomyScore {
		t.Errorf("zero turns scored %f but one turn scored %f; zero interventions is not less autonomous than one",
			insight.AutonomyScore, oneTurn.AutonomyScore)
	}
}

// The averages are what feed AutonomyScore, so the max(1, turnCount) divisor
// belongs to the counters rather than to each consumer.
func TestTurnCountProcessor_ZeroTurnsStillReportsSteps(t *testing.T) {
	p := &claudesessions.TurnCountProcessor{}
	for i := 0; i < 12; i++ {
		p.Process(claudesessions.ProcessableEvent{Type: "assistant"})
	}
	insight := &claudesessions.SessionInsight{ToolBreakdown: make(map[string]int)}
	p.Finalize(insight)

	if insight.TurnCount != 0 {
		t.Fatalf("TurnCount = %d, want 0", insight.TurnCount)
	}
	if insight.StepsPerTurnAvg != 12 {
		t.Errorf("StepsPerTurnAvg = %f, want 12: an unattended run of n steps is n steps per turn",
			insight.StepsPerTurnAvg)
	}
}

func TestAutonomyScoreProcessor_ScoreClamped(t *testing.T) {
	insight := &claudesessions.SessionInsight{
		TurnCount:       1,
		StepsPerTurnAvg: 1e9, // absurdly high
		ToolBreakdown:   make(map[string]int),
	}
	p := &claudesessions.AutonomyScoreProcessor{}
	p.Finalize(insight)
	if insight.AutonomyScore > 100 {
		t.Errorf("score should be clamped to 100, got %f", insight.AutonomyScore)
	}
}

// ─── ToolUsageProcessor ───────────────────────────────────────────────────────

func TestToolUsageProcessor_CountsTools(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("assistant", withMessage("assistant", "", toolUseBlocks("bash", "bash", "read"), nil)),
		makeEvent("assistant", withMessage("assistant", "", toolUseBlocks("write"), nil)),
	}
	insight := runProcessors(evs, &claudesessions.ToolUsageProcessor{})
	if insight.ToolCallsTotal != 4 {
		t.Errorf("expected 4 tool calls, got %d", insight.ToolCallsTotal)
	}
	if insight.ToolBreakdown["bash"] != 2 {
		t.Errorf("expected bash=2, got %d", insight.ToolBreakdown["bash"])
	}
	if insight.ToolBreakdown["read"] != 1 {
		t.Errorf("expected read=1, got %d", insight.ToolBreakdown["read"])
	}
}

func TestToolUsageProcessor_NoTools(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("assistant", withMessage("assistant", "", textBlocks("hello"), nil)),
	}
	insight := runProcessors(evs, &claudesessions.ToolUsageProcessor{})
	if insight.ToolCallsTotal != 0 {
		t.Errorf("expected 0 tool calls, got %d", insight.ToolCallsTotal)
	}
}

// ─── TimeProfileProcessor ─────────────────────────────────────────────────────

func TestTimeProfileProcessor_Duration(t *testing.T) {
	t0 := time.Date(2025, 1, 1, 10, 0, 0, 0, time.UTC)
	t1 := t0.Add(5 * time.Minute)
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user", withTS(t0)),
		makeEvent("assistant", withTS(t1)),
	}
	insight := runProcessors(evs, &claudesessions.TimeProfileProcessor{})
	want := int64(5 * 60 * 1000)
	if insight.TotalDurationMs != want {
		t.Errorf("expected %dms, got %d", want, insight.TotalDurationMs)
	}
}

func TestTimeProfileProcessor_ActiveDurationCapsIdleGaps(t *testing.T) {
	// A sitting, a resume 28 days later, and another sitting: the span covers
	// the idle month, the active duration counts each sitting plus one capped
	// gap for the resume. This is the exact shape that put the dashboard's
	// average at 8 hours while the median sitting was 17 minutes.
	t0 := time.Date(2025, 1, 1, 10, 0, 0, 0, time.UTC)
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user", withTS(t0)),
		makeEvent("assistant", withTS(t0.Add(2*time.Minute))),
		makeEvent("user", withTS(t0.Add(28*24*time.Hour))), // resume
		makeEvent("assistant", withTS(t0.Add(28*24*time.Hour+3*time.Minute))),
	}
	insight := runProcessors(evs, &claudesessions.TimeProfileProcessor{})

	wantSpan := (28*24*time.Hour + 3*time.Minute).Milliseconds()
	if insight.TotalDurationMs != wantSpan {
		t.Errorf("span = %dms, want %d", insight.TotalDurationMs, wantSpan)
	}
	wantActive := (2*time.Minute + claudesessions.IdleGapThreshold() + 3*time.Minute).Milliseconds()
	if insight.ActiveDurationMs != wantActive {
		t.Errorf("active = %dms, want %d", insight.ActiveDurationMs, wantActive)
	}
}

func TestTimeProfileProcessor_ClaudeWorkingTimeIsAssistantGaps(t *testing.T) {
	// Gaps ending at an assistant event are Claude working; the gap ending at
	// the user's next message is not.
	t0 := time.Date(2025, 1, 1, 10, 0, 0, 0, time.UTC)
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user", withTS(t0)),
		makeEvent("assistant", withTS(t0.Add(30*time.Second))),
		makeEvent("assistant", withTS(t0.Add(90*time.Second))),
		makeEvent("user", withTS(t0.Add(5*time.Minute))),
	}
	insight := runProcessors(evs, &claudesessions.TimeProfileProcessor{})

	want := (90 * time.Second).Milliseconds()
	if insight.ClaudeWorkingTimeMs != want {
		t.Errorf("claude working = %dms, want %d", insight.ClaudeWorkingTimeMs, want)
	}
	if insight.ActiveDurationMs != (5 * time.Minute).Milliseconds() {
		t.Errorf("active = %dms, want %d", insight.ActiveDurationMs, (5 * time.Minute).Milliseconds())
	}
}

func TestTimeProfileProcessor_SubagentEventsFillParentWaitGaps(t *testing.T) {
	// The pipeline feeds the parent transcript first and sub-agent transcripts
	// after it, so timestamps arrive out of order. The tracker sorts before
	// walking: a 40-minute delegated run inside the parent's Task wait must be
	// credited, not collapsed to one capped gap.
	t0 := time.Date(2025, 1, 1, 10, 0, 0, 0, time.UTC)
	evs := []claudesessions.ProcessableEvent{
		// Parent: Task tool_use at t0, tool_result 40 minutes later.
		makeEvent("assistant", withTS(t0)),
		makeEvent("user", withTS(t0.Add(40*time.Minute))),
		// Sub-agent transcript, processed afterwards, working every 5 minutes.
		makeEvent("assistant", withTS(t0.Add(5*time.Minute)), withSidechain()),
		makeEvent("assistant", withTS(t0.Add(10*time.Minute)), withSidechain()),
		makeEvent("assistant", withTS(t0.Add(20*time.Minute)), withSidechain()),
		makeEvent("assistant", withTS(t0.Add(30*time.Minute)), withSidechain()),
		makeEvent("assistant", withTS(t0.Add(38*time.Minute)), withSidechain()),
	}
	insight := runProcessors(evs, &claudesessions.TimeProfileProcessor{})

	// Sorted gaps: 5,5,10,10,8,2 minutes — all under the cap, so the whole 40
	// minutes counts. Without the merge it would be one capped 10-minute gap.
	want := (40 * time.Minute).Milliseconds()
	if insight.ActiveDurationMs != want {
		t.Errorf("active = %dms, want %d", insight.ActiveDurationMs, want)
	}
}

// ─── TokenProfileProcessor ────────────────────────────────────────────────────

func TestTokenProfileProcessor_CacheHitRate(t *testing.T) {
	usage := &claudesessions.EventUsage{
		CacheCreationInputTokens: 30,
		CacheReadInputTokens:     70,
	}
	p := &claudesessions.TokenProfileProcessor{}
	p.Process(makeEvent("assistant", withMessage("assistant", "claude-sonnet-4-6", nil, usage)))
	insight := &claudesessions.SessionInsight{TurnCount: 1, ToolBreakdown: make(map[string]int)}
	p.Finalize(insight)

	// 70 / (30+70) = 0.7
	if math.Abs(insight.CacheHitRate-0.7) > 1e-9 {
		t.Errorf("expected cache hit rate 0.7, got %f", insight.CacheHitRate)
	}
}

func TestTokenProfileProcessor_CostEstimateHaiku(t *testing.T) {
	usage := &claudesessions.EventUsage{
		InputTokens:  1_000_000,
		OutputTokens: 1_000_000,
	}
	evs := []claudesessions.ProcessableEvent{
		makeEvent("assistant", withMessage("assistant", "claude-haiku-4-5", nil, usage)),
	}
	insight := runProcessors(evs, &claudesessions.TokenProfileProcessor{})
	// Haiku: $1/M input + $5/M output = $6
	if math.Abs(insight.CostEstimateUSD-6.0) > 0.01 {
		t.Errorf("expected $6.0 (Haiku), got $%f", insight.CostEstimateUSD)
	}
}

func TestTokenProfileProcessor_CostEstimateOpus(t *testing.T) {
	usage := &claudesessions.EventUsage{
		InputTokens:  1_000_000,
		OutputTokens: 1_000_000,
	}
	evs := []claudesessions.ProcessableEvent{
		makeEvent("assistant", withMessage("assistant", "claude-opus-4-6", nil, usage)),
	}
	insight := runProcessors(evs, &claudesessions.TokenProfileProcessor{})
	// Opus pricing (from analytics.go): $5/M input + $25/M output = $30
	if math.Abs(insight.CostEstimateUSD-30.0) > 0.01 {
		t.Errorf("expected $30.0 (Opus per analytics pricing), got $%f", insight.CostEstimateUSD)
	}
}

func TestTokenProfileProcessor_NoCacheTokens(t *testing.T) {
	insight := runProcessors(nil, &claudesessions.TokenProfileProcessor{})
	if insight.CacheHitRate != 0 {
		t.Errorf("expected 0 cache hit rate with no events, got %f", insight.CacheHitRate)
	}
}

// ─── ErrorRateProcessor ───────────────────────────────────────────────────────

func TestErrorRateProcessor_NoErrors(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user", withMessage("user", "", toolResultBlocks(false, false), nil)),
	}
	insight := runProcessors(evs, &claudesessions.ErrorRateProcessor{})
	if insight.HasErrors {
		t.Error("expected HasErrors=false")
	}
	if insight.ToolErrorCount != 0 {
		t.Errorf("expected 0 errors, got %d", insight.ToolErrorCount)
	}
}

func TestErrorRateProcessor_WithErrors(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user", withMessage("user", "", toolResultBlocks(true, true, false), nil)),
	}
	insight := runProcessors(evs, &claudesessions.ErrorRateProcessor{})
	if !insight.HasErrors {
		t.Error("expected HasErrors=true")
	}
	if insight.ToolErrorCount != 2 {
		t.Errorf("expected 2 errors, got %d", insight.ToolErrorCount)
	}
	expected := 2.0 / 3.0
	if math.Abs(insight.ToolErrorRate-expected) > 1e-9 {
		t.Errorf("expected error rate %f, got %f", expected, insight.ToolErrorRate)
	}
}

func TestErrorRateProcessor_AllErrors(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user", withMessage("user", "", toolResultBlocks(true), nil)),
	}
	insight := runProcessors(evs, &claudesessions.ErrorRateProcessor{})
	if insight.ToolErrorRate != 1.0 {
		t.Errorf("expected error rate 1.0, got %f", insight.ToolErrorRate)
	}
}

// ─── ConversationDepthProcessor ───────────────────────────────────────────────

func TestConversationDepthProcessor_MaxConsecutive(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("assistant", withMessage("assistant", "", toolUseBlocks("a", "b", "c"), nil)), // 3
		makeEvent("assistant", withMessage("assistant", "", toolUseBlocks("d"), nil)),           // 1
	}
	insight := runProcessors(evs, &claudesessions.ConversationDepthProcessor{})
	if insight.MaxConsecutiveToolCalls != 3 {
		t.Errorf("expected max_consecutive=3, got %d", insight.MaxConsecutiveToolCalls)
	}
	if insight.LongestAutonomousChain != 4 {
		t.Errorf("expected longest_chain=4, got %d", insight.LongestAutonomousChain)
	}
}

func TestConversationDepthProcessor_ChainResetsOnUserInput(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("assistant", withMessage("assistant", "", toolUseBlocks("a", "b"), nil)), // chain=2
		makeEvent("user", withMessage("user", "", textBlocks("ok"), nil)),                  // reset
		makeEvent("assistant", withMessage("assistant", "", toolUseBlocks("c"), nil)),      // chain=1
	}
	insight := runProcessors(evs, &claudesessions.ConversationDepthProcessor{})
	if insight.LongestAutonomousChain != 2 {
		t.Errorf("expected longest_chain=2, got %d", insight.LongestAutonomousChain)
	}
}

func TestConversationDepthProcessor_InterleavedTextResetsConsecutive(t *testing.T) {
	// [tool_use, text, tool_use] — text breaks the consecutive run; max is 1, not 2.
	blocks := []map[string]any{
		{"type": "tool_use", "id": "id-a", "name": "a"},
		{"type": "text", "text": "thinking"},
		{"type": "tool_use", "id": "id-b", "name": "b"},
	}
	evs := []claudesessions.ProcessableEvent{
		makeEvent("assistant", withMessage("assistant", "", blocks, nil)),
	}
	insight := runProcessors(evs, &claudesessions.ConversationDepthProcessor{})
	if insight.MaxConsecutiveToolCalls != 1 {
		t.Errorf("expected max_consecutive=1 (text breaks run), got %d", insight.MaxConsecutiveToolCalls)
	}
}

func TestConversationDepthProcessor_NoToolCalls(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("assistant", withMessage("assistant", "", textBlocks("hello"), nil)),
	}
	insight := runProcessors(evs, &claudesessions.ConversationDepthProcessor{})
	if insight.MaxConsecutiveToolCalls != 0 {
		t.Errorf("expected 0 consecutive calls, got %d", insight.MaxConsecutiveToolCalls)
	}
}

// ─── SessionRhythmProcessor ───────────────────────────────────────────────────

func TestSessionRhythmProcessor_BasicRhythm(t *testing.T) {
	t0 := time.Date(2025, 1, 1, 10, 0, 0, 0, time.UTC)
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user", withTS(t0), withMessage("user", "", textBlocks("go"), nil)),
		makeEvent("assistant", withTS(t0.Add(2*time.Second)), withMessage("assistant", "", nil, nil)),
		makeEvent("user", withTS(t0.Add(12*time.Second)), withMessage("user", "", textBlocks("more"), nil)),
		makeEvent("assistant", withTS(t0.Add(14*time.Second)), withMessage("assistant", "", nil, nil)),
	}
	insight := runProcessors(evs, &claudesessions.SessionRhythmProcessor{})

	// User responded 10s after Claude (t0+2s → t0+12s)
	if math.Abs(insight.AvgUserResponseTimeMs-10000) > 1 {
		t.Errorf("expected avg_user_response=10000ms, got %f", insight.AvgUserResponseTimeMs)
	}
	// Claude responded 2s both times
	if math.Abs(insight.AvgClaudeResponseTimeMs-2000) > 1 {
		t.Errorf("expected avg_claude_response=2000ms, got %f", insight.AvgClaudeResponseTimeMs)
	}
}

func TestSessionRhythmProcessor_ExcludesIdleGaps(t *testing.T) {
	// A resume after days is a new sitting, not a reply: the corpus contained a
	// 226-hour "user response time" from exactly this shape. The idle gap is
	// dropped entirely rather than capped — capped, every resumed session's
	// average would converge on the cap.
	t0 := time.Date(2025, 1, 1, 10, 0, 0, 0, time.UTC)
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user", withTS(t0), withMessage("user", "", textBlocks("go"), nil)),
		makeEvent("assistant", withTS(t0.Add(2*time.Second)), withMessage("assistant", "", nil, nil)),
		// Resume seven days later.
		makeEvent("user", withTS(t0.Add(7*24*time.Hour)), withMessage("user", "", textBlocks("more"), nil)),
		makeEvent("assistant", withTS(t0.Add(7*24*time.Hour+4*time.Second)), withMessage("assistant", "", nil, nil)),
	}
	insight := runProcessors(evs, &claudesessions.SessionRhythmProcessor{})

	if insight.AvgUserResponseTimeMs != 0 {
		t.Errorf("expected the resume gap excluded, got avg_user_response=%f", insight.AvgUserResponseTimeMs)
	}
	// Claude's replies (2s and 4s) are unaffected.
	if math.Abs(insight.AvgClaudeResponseTimeMs-3000) > 1 {
		t.Errorf("expected avg_claude_response=3000ms, got %f", insight.AvgClaudeResponseTimeMs)
	}
}

func TestSessionRhythmProcessor_NoInteractions(t *testing.T) {
	insight := runProcessors(nil, &claudesessions.SessionRhythmProcessor{})
	if insight.AvgUserResponseTimeMs != 0 || insight.AvgClaudeResponseTimeMs != 0 {
		t.Error("expected 0 rhythms with no events")
	}
}

func TestSessionRhythmProcessor_Reset(t *testing.T) {
	p := &claudesessions.SessionRhythmProcessor{}
	t0 := time.Date(2025, 1, 1, 10, 0, 0, 0, time.UTC)
	p.Process(makeEvent("user", withTS(t0), withMessage("user", "", textBlocks("hi"), nil)))
	p.Reset()
	insight := &claudesessions.SessionInsight{ToolBreakdown: make(map[string]int)}
	p.Finalize(insight)
	if insight.AvgUserResponseTimeMs != 0 || insight.AvgClaudeResponseTimeMs != 0 {
		t.Error("expected 0 after reset")
	}
}

// ─── ProcessorRegistry ────────────────────────────────────────────────────────

func TestProcessorRegistry_RunSession(t *testing.T) {
	lines := []map[string]any{
		{
			"type":      "user",
			"timestamp": "2025-01-01T10:00:00Z",
			"message": map[string]any{
				"role":    "user",
				"content": textBlocks("hello"),
			},
		},
		{
			"type":      "assistant",
			"timestamp": "2025-01-01T10:00:02Z",
			"message": map[string]any{
				"role":    "assistant",
				"model":   "claude-sonnet-4-6",
				"content": toolUseBlocks("bash"),
				"usage": map[string]any{
					"input_tokens":  100,
					"output_tokens": 50,
				},
			},
		},
		{
			"type":      "user",
			"timestamp": "2025-01-01T10:00:03Z",
			"message": map[string]any{
				"role":    "user",
				"content": toolResultBlocks(false),
			},
		},
	}

	path := writeJSONLFile(t, lines)
	registry := claudesessions.DefaultProcessorRegistry(nil)
	insight, err := registry.RunSession("test-session-id", path)
	if err != nil {
		t.Fatalf("RunSession failed: %v", err)
	}

	if insight.SessionID != "test-session-id" {
		t.Errorf("expected session ID 'test-session-id', got %q", insight.SessionID)
	}
	if insight.TurnCount != 1 {
		t.Errorf("expected 1 turn, got %d", insight.TurnCount)
	}
	if insight.ToolCallsTotal != 1 {
		t.Errorf("expected 1 tool call, got %d", insight.ToolCallsTotal)
	}
	if insight.ToolBreakdown["bash"] != 1 {
		t.Errorf("expected bash=1 in breakdown, got %d", insight.ToolBreakdown["bash"])
	}
	if insight.ProcessorVersion != claudesessions.CurrentProcessorVersion {
		t.Errorf("expected processor version %d, got %d",
			claudesessions.CurrentProcessorVersion, insight.ProcessorVersion)
	}
	// 3 events spanning t0 → t0+3s (user→assistant at 2s, assistant→tool_result at 3s)
	if insight.TotalDurationMs != 3000 {
		t.Errorf("expected 3000ms duration, got %d", insight.TotalDurationMs)
	}
}

func TestProcessorRegistry_SkipsHistorySnapshot(t *testing.T) {
	lines := []map[string]any{
		{"type": "file-history-snapshot", "timestamp": "2025-01-01T10:00:00Z"},
		{
			"type":      "user",
			"timestamp": "2025-01-01T10:00:01Z",
			"message": map[string]any{
				"role":    "user",
				"content": textBlocks("hi"),
			},
		},
	}
	path := writeJSONLFile(t, lines)
	registry := claudesessions.DefaultProcessorRegistry(nil)
	insight, err := registry.RunSession("sid", path)
	if err != nil {
		t.Fatal(err)
	}
	if insight.TurnCount != 1 {
		t.Errorf("expected 1 turn (snapshot skipped), got %d", insight.TurnCount)
	}
}

func TestProcessorRegistry_MissingFile(t *testing.T) {
	registry := claudesessions.DefaultProcessorRegistry(nil)
	_, err := registry.RunSession("sid", "/nonexistent/path/session.jsonl")
	if err == nil {
		t.Error("expected error for missing file")
	}
}

func TestProcessorRegistry_FullPipeline(t *testing.T) {
	// End-to-end: verify AutonomyScore is computed after TurnCount.
	lines := []map[string]any{
		{
			"type":      "user",
			"timestamp": "2025-01-01T10:00:00Z",
			"message": map[string]any{
				"role":    "user",
				"content": textBlocks("start"),
			},
		},
		{
			"type":      "assistant",
			"timestamp": "2025-01-01T10:00:01Z",
			"message": map[string]any{
				"role":    "assistant",
				"model":   "claude-sonnet-4-6",
				"content": toolUseBlocks("bash", "read", "write"),
				"usage":   map[string]any{"input_tokens": 500, "output_tokens": 200},
			},
		},
	}
	path := writeJSONLFile(t, lines)
	registry := claudesessions.DefaultProcessorRegistry(nil)
	insight, err := registry.RunSession("full-test", path)
	if err != nil {
		t.Fatal(err)
	}
	if insight.AutonomyScore <= 0 {
		t.Errorf("expected positive autonomy score, got %f", insight.AutonomyScore)
	}
	if insight.ToolCallsTotal != 3 {
		t.Errorf("expected 3 tool calls, got %d", insight.ToolCallsTotal)
	}
	if insight.CostEstimateUSD <= 0 {
		t.Errorf("expected positive cost estimate, got %f", insight.CostEstimateUSD)
	}
}

// ─── AttributionProcessor ─────────────────────────────────────────────────────

// withAttribution stamps the attribution fields Claude Code puts at the top
// level of an assistant event.
func withAttribution(skill, plugin string) func(*claudesessions.ProcessableEvent) {
	return func(ev *claudesessions.ProcessableEvent) {
		ev.AttributionSkill = skill
		ev.AttributionPlugin = plugin
	}
}

// withEffort sets the reasoning-effort tier the turn ran at.
func withEffort(effort string) func(*claudesessions.ProcessableEvent) {
	return func(ev *claudesessions.ProcessableEvent) { ev.Effort = effort }
}

// withAgent sets the sub-agent type owning the turn. Claude Code stamps this on
// sub-agent transcripts only, never on the parent.
func withAgent(agent string) func(*claudesessions.ProcessableEvent) {
	return func(ev *claudesessions.ProcessableEvent) { ev.AttributionAgent = agent }
}

// TestAttributionProcessor_CountsAgentsPerToolCall covers the dimension #202
// added. attributionAgent was previously decoded and dropped, so the risk is
// the same trap the sibling dimensions document: counting per event rather
// than per tool_use block inflates every number by a variable factor.
func TestAttributionProcessor_CountsAgentsPerToolCall(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		// One message, three tool_use blocks -> 3, not 1.
		makeEvent("assistant",
			withMessage("assistant", "", toolUseBlocks("Bash", "Read", "Grep"), nil),
			withAgent("Explore")),
		// A second event of the same turn carrying no tool calls must add nothing.
		makeEvent("assistant",
			withMessage("assistant", "", textBlocks("thinking"), nil),
			withAgent("Explore")),
		// A different sub-agent type is tracked separately.
		makeEvent("assistant",
			withMessage("assistant", "", toolUseBlocks("Write"), nil),
			withAgent("general-purpose")),
		// Main-thread work carries no agent and must not land in a catch-all.
		makeEvent("assistant",
			withMessage("assistant", "", toolUseBlocks("Edit"), nil)),
	}
	insight := runProcessors(evs, &claudesessions.AttributionProcessor{})

	if got := insight.AgentBreakdown["Explore"]; got != 3 {
		t.Errorf("agent_breakdown[Explore] = %d, want 3 (per tool_use block, not per event)", got)
	}
	if got := insight.AgentBreakdown["general-purpose"]; got != 1 {
		t.Errorf("agent_breakdown[general-purpose] = %d, want 1", got)
	}
	if len(insight.AgentBreakdown) != 2 {
		t.Errorf("agent_breakdown = %v, want exactly 2 entries — unattributed work must not bucket",
			insight.AgentBreakdown)
	}
	if _, ok := insight.AgentBreakdown[""]; ok {
		t.Error(`agent_breakdown has an "" key; an absent agent must contribute nothing`)
	}
}

// TestAttributionProcessor_CountsSkillsAndReconciles is the issue's headline
// case: attributed calls land under their skill, unattributed ones are counted
// explicitly, and together they account for every tool call.
func TestAttributionProcessor_CountsSkillsAndReconciles(t *testing.T) {
	evs := make([]claudesessions.ProcessableEvent, 0, 15)
	// 10 tool calls attributed to one skill.
	for range 10 {
		evs = append(evs, makeEvent("assistant",
			withMessage("assistant", "", toolUseBlocks("Bash"), nil),
			withAttribution("lab-workflow:review-pr", "lab-workflow"),
			withEffort("high"),
		))
	}
	// 5 with no attribution at all — plain built-in tool use.
	for range 5 {
		evs = append(evs, makeEvent("assistant",
			withMessage("assistant", "", toolUseBlocks("Read"), nil)))
	}

	insight := runProcessors(evs,
		&claudesessions.AttributionProcessor{}, &claudesessions.ToolUsageProcessor{})

	if got := insight.SkillBreakdown["lab-workflow:review-pr"]; got != 10 {
		t.Errorf("skill_breakdown[review-pr] = %d, want 10", got)
	}
	if insight.UnattributedCalls != 5 {
		t.Errorf("unattributed_calls = %d, want 5", insight.UnattributedCalls)
	}
	if got := insight.PluginBreakdown["lab-workflow"]; got != 10 {
		t.Errorf("plugin_breakdown[lab-workflow] = %d, want 10", got)
	}
	if got := insight.EffortBreakdown["high"]; got != 10 {
		t.Errorf("effort_breakdown[high] = %d, want 10", got)
	}

	// The reconciliation the issue asks for: nothing double-counted, nothing lost.
	sum := 0
	for _, v := range insight.SkillBreakdown {
		sum += v
	}
	if sum+insight.UnattributedCalls != insight.ToolCallsTotal {
		t.Errorf("sum(skill_breakdown)=%d + unattributed=%d != tool_calls_total=%d",
			sum, insight.UnattributedCalls, insight.ToolCallsTotal)
	}
}

// TestAttributionProcessor_CountsPerToolCallNotPerEvent pins the deviation from
// the issue's HOW. Claude Code splits one assistant message into several events
// carrying identical attribution, so counting per event inflates every number.
func TestAttributionProcessor_CountsPerToolCallNotPerEvent(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		// Same turn, three events, same attribution — only one has tool calls.
		makeEvent("assistant",
			withMessage("assistant", "", textBlocks("thinking out loud"), nil),
			withAttribution("vibexp:prime", "vibexp"), withEffort("low")),
		makeEvent("assistant",
			withMessage("assistant", "", toolUseBlocks("Bash", "Read"), nil),
			withAttribution("vibexp:prime", "vibexp"), withEffort("low")),
		makeEvent("assistant",
			withMessage("assistant", "", textBlocks("done"), nil),
			withAttribution("vibexp:prime", "vibexp"), withEffort("low")),
	}

	insight := runProcessors(evs,
		&claudesessions.AttributionProcessor{}, &claudesessions.ToolUsageProcessor{})

	if got := insight.SkillBreakdown["vibexp:prime"]; got != 2 {
		t.Errorf("skill_breakdown = %d, want 2 — one per tool call, not one per event", got)
	}
	if insight.ToolCallsTotal != 2 {
		t.Errorf("tool_calls_total = %d, want 2", insight.ToolCallsTotal)
	}
}

// TestAttributionProcessor_MCPFromToolName covers the other deviation: MCP
// attribution is parsed from the tool_use name, because the attributionMcp*
// fields are sticky and mostly disagree with the call actually being made.
func TestAttributionProcessor_MCPFromToolName(t *testing.T) {
	ev := makeEvent("assistant",
		withMessage("assistant", "", toolUseBlocks(
			"mcp__vibexp_io_vibexp_team__vibexp_io_post_to_feed",
			"mcp__vibexp_io_vibexp_team__vibexp_io_search",
			"mcp__claude-in-chrome__navigate",
			"Bash",
		), nil))
	// A stale field naming a completely different server, as real transcripts have.
	ev.AttributionMcpServer = "claude.ai VibeXP"
	ev.AttributionMcpTool = "vibexp_io_reply_to_feed_item"

	insight := runProcessors([]claudesessions.ProcessableEvent{ev},
		&claudesessions.AttributionProcessor{})

	if got := insight.McpServerBreakdown["vibexp_io_vibexp_team"]; got != 2 {
		t.Errorf("mcp_server_breakdown[vibexp_io_vibexp_team] = %d, want 2", got)
	}
	if got := insight.McpServerBreakdown["claude-in-chrome"]; got != 1 {
		t.Errorf("mcp_server_breakdown[claude-in-chrome] = %d, want 1", got)
	}
	// The sticky field must not have been counted.
	if _, ok := insight.McpServerBreakdown["claude.ai VibeXP"]; ok {
		t.Error("the sticky attributionMcpServer field was counted; it must not be")
	}
	// Server and tool are countable independently.
	if got := insight.McpToolBreakdown["vibexp_io_post_to_feed"]; got != 1 {
		t.Errorf("mcp_tool_breakdown[vibexp_io_post_to_feed] = %d, want 1", got)
	}
	if _, ok := insight.McpToolBreakdown["vibexp_io_reply_to_feed_item"]; ok {
		t.Error("the sticky attributionMcpTool field was counted; it must not be")
	}
	// A non-MCP tool contributes to neither MCP map.
	if len(insight.McpServerBreakdown) != 2 {
		t.Errorf("mcp_server_breakdown has %d entries, want 2", len(insight.McpServerBreakdown))
	}
}

func TestAttributionProcessor_NoAttributionFieldsYieldsEmptyMaps(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("assistant", withMessage("assistant", "", toolUseBlocks("Bash"), nil)),
	}
	insight := runProcessors(evs, &claudesessions.AttributionProcessor{})

	// Empty, not nil — so the columns marshal to {} rather than null.
	for name, m := range map[string]map[string]int{
		"skill_breakdown":      insight.SkillBreakdown,
		"plugin_breakdown":     insight.PluginBreakdown,
		"mcp_server_breakdown": insight.McpServerBreakdown,
		"mcp_tool_breakdown":   insight.McpToolBreakdown,
		"effort_breakdown":     insight.EffortBreakdown,
		"agent_breakdown":      insight.AgentBreakdown,
	} {
		if m == nil {
			t.Errorf("%s is nil, want an empty map", name)
		}
		if len(m) != 0 {
			t.Errorf("%s has %d entries, want 0", name, len(m))
		}
	}
	if insight.UnattributedCalls != 1 {
		t.Errorf("unattributed_calls = %d, want 1", insight.UnattributedCalls)
	}
}

func TestAttributionProcessor_Reset(t *testing.T) {
	p := &claudesessions.AttributionProcessor{}
	runProcessors([]claudesessions.ProcessableEvent{
		makeEvent("assistant",
			withMessage("assistant", "", toolUseBlocks("Bash"), nil),
			withAttribution("s", "p"), withEffort("high")),
	}, p)

	insight := runProcessors(nil, p)
	if len(insight.SkillBreakdown) != 0 || insight.UnattributedCalls != 0 {
		t.Errorf("Reset did not clear state: %+v / %d", insight.SkillBreakdown, insight.UnattributedCalls)
	}
}

// TestAttributionProcessor_UserEventsIgnored — attribution is only ever stamped
// on assistant events; a user event must never contribute.
func TestAttributionProcessor_UserEventsIgnored(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("user",
			withMessage("user", "", toolUseBlocks("Bash"), nil),
			withAttribution("some:skill", "some"), withEffort("high")),
	}
	insight := runProcessors(evs, &claudesessions.AttributionProcessor{})
	if len(insight.SkillBreakdown) != 0 || insight.UnattributedCalls != 0 {
		t.Errorf("a user event contributed to the breakdown: %+v", insight.SkillBreakdown)
	}
}

// TestAttributionProcessor_DecodesRawJSONKeys feeds the real JSON key names
// through the registry rather than setting struct fields directly. Every other
// attribution test constructs ProcessableEvent in Go, so a typo in a `json:`
// tag would zero every breakdown in production with a green suite.
func TestAttributionProcessor_DecodesRawJSONKeys(t *testing.T) {
	// Field names and nesting exactly as Claude Code writes them: attribution
	// is top-level on the event, never inside message.
	lines := []map[string]any{
		{
			"type":              "assistant",
			"uuid":              "a1",
			"timestamp":         "2026-01-01T10:00:00Z",
			"attributionSkill":  "lab-workflow:review-pr",
			"attributionPlugin": "lab-workflow",
			"effort":            "high",
			"message": map[string]any{
				"role":  "assistant",
				"model": "claude-opus-4-8",
				"content": []map[string]any{
					{"type": "tool_use", "id": "t1", "name": "Bash"},
					{"type": "tool_use", "id": "t2", "name": "mcp__vibexp_io_vibexp_team__vibexp_io_search"},
				},
			},
		},
	}

	registry := claudesessions.DefaultProcessorRegistry(nil)
	insight, err := registry.RunSession("json-keys", writeJSONLFile(t, lines))
	if err != nil {
		t.Fatalf("run session: %v", err)
	}

	if got := insight.SkillBreakdown["lab-workflow:review-pr"]; got != 2 {
		t.Errorf("skill_breakdown = %d, want 2 — the attributionSkill json tag is wrong", got)
	}
	if got := insight.PluginBreakdown["lab-workflow"]; got != 2 {
		t.Errorf("plugin_breakdown = %d, want 2 — the attributionPlugin json tag is wrong", got)
	}
	if got := insight.EffortBreakdown["high"]; got != 2 {
		t.Errorf("effort_breakdown = %d, want 2 — the effort json tag is wrong", got)
	}
	if got := insight.McpServerBreakdown["vibexp_io_vibexp_team"]; got != 1 {
		t.Errorf("mcp_server_breakdown = %d, want 1", got)
	}
	if insight.UnattributedCalls != 0 {
		t.Errorf("unattributed_calls = %d, want 0", insight.UnattributedCalls)
	}
	// And the invariant holds through the real registry.
	sum := 0
	for _, v := range insight.SkillBreakdown {
		sum += v
	}
	if sum+insight.UnattributedCalls != insight.ToolCallsTotal {
		t.Errorf("sum(skills)=%d + unattributed=%d != tool_calls_total=%d",
			sum, insight.UnattributedCalls, insight.ToolCallsTotal)
	}
}

// TestCurrentProcessorVersion_BumpedForAttribution ties the constant to this
// feature: without the bump, NeedsProcessing never returns existing sessions
// and the new columns stay empty forever on an upgrade.
func TestCurrentProcessorVersion_BumpedForAttribution(t *testing.T) {
	if claudesessions.CurrentProcessorVersion < 4 {
		t.Errorf("CurrentProcessorVersion = %d, want >= 4 so attribution backfills existing rows",
			claudesessions.CurrentProcessorVersion)
	}
}
