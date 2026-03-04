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

func withRaw(data map[string]any) func(*claudesessions.ProcessableEvent) {
	return func(ev *claudesessions.ProcessableEvent) {
		raw, _ := json.Marshal(data)
		ev.Raw = raw
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

func thinkingBlock(text string) map[string]any {
	return map[string]any{"type": "thinking", "thinking": text}
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

func TestAutonomyScoreProcessor_ZeroTurns(t *testing.T) {
	insight := &claudesessions.SessionInsight{ToolBreakdown: make(map[string]int)}
	p := &claudesessions.AutonomyScoreProcessor{}
	p.Finalize(insight)
	if insight.AutonomyScore != 0 {
		t.Errorf("expected 0 score for zero turns, got %f", insight.AutonomyScore)
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

func TestTimeProfileProcessor_ThinkingFallback(t *testing.T) {
	// 100 chars × 0.5ms/char = 50ms
	thinking := thinkingBlock(string(make([]byte, 100)))
	evs := []claudesessions.ProcessableEvent{
		makeEvent("assistant", withMessage("assistant", "", []map[string]any{thinking}, nil)),
	}
	insight := runProcessors(evs, &claudesessions.TimeProfileProcessor{})
	if insight.ThinkingTimeMs != 50 {
		t.Errorf("expected 50ms thinking (fallback), got %d", insight.ThinkingTimeMs)
	}
}

func TestTimeProfileProcessor_SystemTurnDuration(t *testing.T) {
	evs := []claudesessions.ProcessableEvent{
		makeEvent("system", withRaw(map[string]any{
			"type":        "system",
			"subtype":     "turn_duration",
			"duration_ms": 1234,
			"timestamp":   "2025-01-01T10:00:00Z",
		})),
	}
	insight := runProcessors(evs, &claudesessions.TimeProfileProcessor{})
	if insight.ThinkingTimeMs != 1234 {
		t.Errorf("expected 1234ms from system event, got %d", insight.ThinkingTimeMs)
	}
}

func TestTimeProfileProcessor_SystemDurationTakesPrecedence(t *testing.T) {
	// When system events provide duration, thinking blocks should NOT add to it.
	thinking := thinkingBlock(string(make([]byte, 100))) // would be 50ms
	evs := []claudesessions.ProcessableEvent{
		makeEvent("system", withRaw(map[string]any{
			"type":        "system",
			"subtype":     "turn_duration",
			"duration_ms": 200,
			"timestamp":   "2025-01-01T10:00:00Z",
		})),
		makeEvent("assistant", withMessage("assistant", "", []map[string]any{thinking}, nil)),
	}
	insight := runProcessors(evs, &claudesessions.TimeProfileProcessor{})
	if insight.ThinkingTimeMs != 200 {
		t.Errorf("expected 200ms (system wins), got %d", insight.ThinkingTimeMs)
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
