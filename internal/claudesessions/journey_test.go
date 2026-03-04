package claudesessions

import (
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"
)

var testLogger = slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelError}))

// writeJSONL writes lines to a temp file and returns its path.
func writeJourneyJSONL(t *testing.T, lines []string) string {
	t.Helper()
	f, err := os.CreateTemp(t.TempDir(), "session-*.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	for _, l := range lines {
		_, _ = f.WriteString(l + "\n")
	}
	_ = f.Close()
	return f.Name()
}

func mustMarshal(v any) string {
	b, err := json.Marshal(v)
	if err != nil {
		panic(err)
	}
	return string(b)
}

// ── Fixtures ─────────────────────────────────────────────────────────────────

var (
	t0 = time.Date(2026, 1, 1, 10, 0, 0, 0, time.UTC)
	t1 = t0.Add(2 * time.Second)
	t2 = t0.Add(5 * time.Second)
	t3 = t0.Add(8 * time.Second)
)

func ts(t time.Time) string { return t.Format(time.RFC3339Nano) }

func userInputEvent(uuid, ts_, content string) string {
	return mustMarshal(map[string]any{
		"type":      "user",
		"uuid":      uuid,
		"sessionId": "test-session",
		"timestamp": ts_,
		"message":   map[string]any{"role": "user", "content": content},
	})
}

func assistantEvent(uuid, parentUUID, ts_ string, contentBlocks []map[string]any) string {
	return mustMarshal(map[string]any{
		"type":       "assistant",
		"uuid":       uuid,
		"parentUuid": parentUUID,
		"sessionId":  "test-session",
		"timestamp":  ts_,
		"message": map[string]any{
			"role":    "assistant",
			"model":   "claude-sonnet-4-6",
			"content": contentBlocks,
			"usage": map[string]any{
				"input_tokens":                100,
				"output_tokens":               50,
				"cache_creation_input_tokens": 20,
				"cache_read_input_tokens":     80,
			},
		},
	})
}

func toolResultEvent(uuid, ts_, toolUseID, content string, isError bool) string {
	return mustMarshal(map[string]any{
		"type":      "user",
		"uuid":      uuid,
		"sessionId": "test-session",
		"timestamp": ts_,
		"message": map[string]any{
			"role": "user",
			"content": []map[string]any{
				{"type": "tool_result", "tool_use_id": toolUseID, "content": content, "is_error": isError},
			},
		},
	})
}

// ── Tests ─────────────────────────────────────────────────────────────────────

func TestBuildJourney_BasicFlow(t *testing.T) {
	lines := []string{
		userInputEvent("u1", ts(t0), "Please read main.go"),
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "text", "text": "Sure, let me read it."},
			{"type": "tool_use", "id": "tool1", "name": "Read", "input": map[string]any{"path": "main.go"}},
		}),
		toolResultEvent("u2", ts(t2), "tool1", "package main\n...", false),
		assistantEvent("a2", "u2", ts(t3), []map[string]any{
			{"type": "text", "text": "Here is the content."},
		}),
	}

	path := writeJourneyJSONL(t, lines)
	journey, err := buildJourney("test-session", path, testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if journey == nil {
		t.Fatal("expected non-nil journey")
	}

	if journey.TotalTurns != 1 {
		t.Errorf("want 1 turn, got %d", journey.TotalTurns)
	}
	if journey.Model != "claude-sonnet-4-6" {
		t.Errorf("want model claude-sonnet-4-6, got %q", journey.Model)
	}

	turn := journey.Turns[0]
	if turn.ToolCalls != 1 {
		t.Errorf("want 1 tool call, got %d", turn.ToolCalls)
	}

	// Expect steps: user_input, text_response, tool_call, tool_result, text_response
	types := make([]string, len(turn.Steps))
	for i, s := range turn.Steps {
		types[i] = s.Type
	}
	want := []string{"user_input", "text_response", "tool_call", "tool_result", "text_response"}
	if len(types) != len(want) {
		t.Fatalf("want steps %v, got %v", want, types)
	}
	for i := range want {
		if types[i] != want[i] {
			t.Errorf("step[%d]: want %q, got %q", i, want[i], types[i])
		}
	}
}

func TestBuildJourney_TwoTurns(t *testing.T) {
	lines := []string{
		userInputEvent("u1", ts(t0), "First message"),
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "text", "text": "First response"},
		}),
		userInputEvent("u3", ts(t2), "Second message"),
		assistantEvent("a2", "u3", ts(t3), []map[string]any{
			{"type": "text", "text": "Second response"},
		}),
	}

	path := writeJourneyJSONL(t, lines)
	journey, err := buildJourney("test-session", path, testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if journey.TotalTurns != 2 {
		t.Errorf("want 2 turns, got %d", journey.TotalTurns)
	}
}

func TestBuildJourney_EmptyFile(t *testing.T) {
	path := writeJourneyJSONL(t, []string{})
	journey, err := buildJourney("test-session", path, testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if journey != nil {
		t.Errorf("expected nil journey for empty file, got %+v", journey)
	}
}

func TestBuildJourney_MalformedLines(t *testing.T) {
	lines := []string{
		"not json at all {{{",
		userInputEvent("u1", ts(t0), "Hello"),
		"also bad }}}",
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "text", "text": "Hi"},
		}),
	}

	path := writeJourneyJSONL(t, lines)
	journey, err := buildJourney("test-session", path, testLogger)
	if err != nil {
		t.Fatalf("unexpected error on malformed lines: %v", err)
	}
	if journey == nil {
		t.Fatal("expected non-nil journey — valid events should still be processed")
	}
	if journey.TotalTurns != 1 {
		t.Errorf("want 1 turn, got %d", journey.TotalTurns)
	}
}

func TestBuildJourney_FileHistorySnapshotSkipped(t *testing.T) {
	snapshot := mustMarshal(map[string]any{
		"type":      "file-history-snapshot",
		"messageId": "snap1",
		"snapshot":  map[string]any{"trackedFileBackups": map[string]any{}},
	})
	lines := []string{
		snapshot,
		userInputEvent("u1", ts(t0), "Do something"),
		snapshot,
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "text", "text": "Done"},
		}),
		snapshot,
	}

	path := writeJourneyJSONL(t, lines)
	journey, err := buildJourney("test-session", path, testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if journey.TotalTurns != 1 {
		t.Errorf("want 1 turn, got %d", journey.TotalTurns)
	}
}

func TestBuildJourney_ThinkingTruncated(t *testing.T) {
	longThinking := string(make([]byte, 25000))
	for i := range []byte(longThinking) {
		longThinking = longThinking[:i] + "a" + longThinking[i+1:]
	}

	lines := []string{
		userInputEvent("u1", ts(t0), "Think hard"),
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "thinking", "thinking": longThinking},
			{"type": "text", "text": "Done thinking"},
		}),
	}

	path := writeJourneyJSONL(t, lines)
	journey, err := buildJourney("test-session", path, testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	var found bool
	for _, step := range journey.Turns[0].Steps {
		if step.Type != "thinking" {
			continue
		}
		found = true
		var d ThinkingData
		if err := json.Unmarshal(step.Data, &d); err != nil {
			t.Fatalf("failed to unmarshal ThinkingData: %v", err)
		}
		if len([]rune(d.Full)) > 20001 { // 20000 runes + ellipsis
			t.Errorf("Full thinking text exceeds 20000 runes, got %d", len([]rune(d.Full)))
		}
		if len([]rune(d.Preview)) > 501 { // 500 runes + ellipsis
			t.Errorf("Preview exceeds 500 runes, got %d", len([]rune(d.Preview)))
		}
	}
	if !found {
		t.Error("no thinking step found")
	}
}

func TestComputeStepDurations_NegativeClamped(t *testing.T) {
	// Steps where timestamps go backward (malformed data) → durations should be 0
	turn := JourneyTurn{
		StartTime: t0,
		EndTime:   t2,
		Steps: []JourneyStep{
			{Type: "user_input", Timestamp: t2},    // later timestamp first
			{Type: "text_response", Timestamp: t0}, // earlier timestamp second
		},
	}
	computeStepDurations(&turn)

	// Step 0 duration: t0 - t2 = negative → clamped to 0
	if turn.Steps[0].DurationMs < 0 {
		t.Errorf("expected duration >= 0, got %d", turn.Steps[0].DurationMs)
	}
}

func TestComputeStepDurations_LastStepUsesEndTime(t *testing.T) {
	endTime := t0.Add(10 * time.Second)
	turn := JourneyTurn{
		StartTime: t0,
		EndTime:   endTime,
		Steps: []JourneyStep{
			{Type: "user_input", Timestamp: t0},
		},
	}
	computeStepDurations(&turn)

	want := int64(10000)
	if turn.Steps[0].DurationMs != want {
		t.Errorf("last step duration: want %dms, got %dms", want, turn.Steps[0].DurationMs)
	}
}

func TestTryProcessToolResults_MixedContent(t *testing.T) {
	// A user event with mixed content (not all tool_result) should not be treated as tool results
	lines := []string{
		userInputEvent("u1", ts(t0), "Hello"),
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "text", "text": "Response"},
		}),
		// User event with plain string content (not tool_result array)
		userInputEvent("u2", ts(t2), "Follow up"),
	}

	path := writeJourneyJSONL(t, lines)
	journey, err := buildJourney("test-session", path, testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	// "Follow up" is a real user message, should start a new turn
	if journey.TotalTurns != 2 {
		t.Errorf("want 2 turns, got %d", journey.TotalTurns)
	}
}

func TestGetSessionJourney_InvalidSessionID(t *testing.T) {
	cases := []string{
		"../../etc/passwd",
		"../secret",
		"session with spaces",
		"session\x00null",
	}
	for _, id := range cases {
		_, err := GetSessionJourney(id, testLogger)
		if err == nil {
			t.Errorf("expected error for session ID %q, got nil", id)
		}
	}
}

func TestGetSessionJourney_ValidSessionID(t *testing.T) {
	// Valid UUIDs should pass validation (will return nil,nil if not found)
	validIDs := []string{
		"abc123",
		"550e8400-e29b-41d4-a716-446655440000",
		"session_id-with-hyphens",
	}
	// Point ClaudeHome at a temp dir so we don't scan real sessions
	origHome := os.Getenv("HOME")
	tmpHome := t.TempDir()
	_ = os.Setenv("HOME", tmpHome)
	defer func() { _ = os.Setenv("HOME", origHome) }()
	_ = os.MkdirAll(filepath.Join(tmpHome, ".claude", "projects"), 0750)

	for _, id := range validIDs {
		result, err := GetSessionJourney(id, testLogger)
		if err != nil {
			t.Errorf("unexpected error for valid session ID %q: %v", id, err)
		}
		if result != nil {
			t.Errorf("expected nil result for non-existent session %q", id)
		}
	}
}
