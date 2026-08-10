package claudesessions

import (
	"bytes"
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
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

// ── Sub-agent nesting (issue #185) ──────────────────────────────────────────

// subagentSidechainEvent builds a sidechain (delegated) event as written to a
// sub-agent's own transcript, where every event carries isSidechain:true.
func subagentSidechainEvent(kind, uuid, ts_ string, contentBlocks []map[string]any) string {
	ev := map[string]any{
		"type":        kind,
		"uuid":        uuid,
		"sessionId":   "test-session",
		"timestamp":   ts_,
		"isSidechain": true,
	}
	if kind == "assistant" {
		ev["message"] = map[string]any{
			"role":    "assistant",
			"content": contentBlocks,
			"usage":   map[string]any{"input_tokens": 40, "output_tokens": 20},
		}
	} else {
		ev["message"] = map[string]any{"role": "user", "content": "delegated task"}
	}
	return mustMarshal(ev)
}

// writeSubagentFixture lays a parent transcript and its sub-agent transcripts
// out the way Claude Code does, and returns the parent path.
func writeSubagentFixture(t *testing.T, parentLines []string, subagents map[string]subagentFixture) string {
	t.Helper()
	sessionID := "sess-185"
	projectDir := t.TempDir()
	parentPath := filepath.Join(projectDir, sessionID+jsonlExt)

	var parent bytes.Buffer
	for _, l := range parentLines {
		parent.WriteString(l + "\n")
	}
	if err := os.WriteFile(parentPath, parent.Bytes(), 0600); err != nil {
		t.Fatal(err)
	}

	subagentsDir := filepath.Join(projectDir, sessionID, "subagents")
	for agentID, sa := range subagents {
		if err := os.MkdirAll(subagentsDir, 0750); err != nil {
			t.Fatal(err)
		}
		base := filepath.Join(subagentsDir, agentID)
		var buf bytes.Buffer
		for _, l := range sa.lines {
			buf.WriteString(l + "\n")
		}
		if err := os.WriteFile(base+jsonlExt, buf.Bytes(), 0600); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(base+".meta.json", []byte(sa.meta), 0600); err != nil {
			t.Fatal(err)
		}
	}
	return parentPath
}

type subagentFixture struct {
	meta  string
	lines []string
}

func subagentMetaJSON(toolUseID, agentType, description string) string {
	return mustMarshal(map[string]any{
		"agentType": agentType, "description": description, "toolUseId": toolUseID,
	})
}

// A Task tool_use whose id matches a sub-agent's toolUseId must nest that
// agent's steps under it — not at top level, and carrying identity + usage.
func TestBuildJourney_SubagentNestedUnderToolCall(t *testing.T) {
	parentLines := []string{
		userInputEvent("u1", ts(t0), "Explore the repo"),
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "tool_use", "id": "toolu_1", "name": "Task", "input": map[string]any{"description": "explore"}},
		}),
	}
	subagents := map[string]subagentFixture{
		"agent-x": {
			meta: subagentMetaJSON("toolu_1", "general-purpose", "explore the repo"),
			lines: []string{
				subagentSidechainEvent("user", "su1", ts(t1.Add(time.Second)), nil),
				subagentSidechainEvent("assistant", "sa1", ts(t1.Add(2*time.Second)), []map[string]any{
					{"type": "text", "text": "agent working"},
					{"type": "tool_use", "id": "st1", "name": "Read", "input": map[string]any{"path": "a.go"}},
				}),
			},
		},
	}

	journey, err := buildJourney("sess-185", writeSubagentFixture(t, parentLines, subagents), testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if journey == nil || len(journey.Turns) != 1 {
		t.Fatalf("expected 1 turn, got %+v", journey)
	}

	var toolCall *JourneyStep
	for i := range journey.Turns[0].Steps {
		if journey.Turns[0].Steps[i].Type == "tool_call" {
			toolCall = &journey.Turns[0].Steps[i]
		}
		if journey.Turns[0].Steps[i].Type == "sub_agent" {
			t.Error("sub-agent leaked to top level; it must be nested under its tool_call")
		}
	}
	if toolCall == nil {
		t.Fatal("expected a tool_call step")
	}

	var td ToolCallData
	if err := json.Unmarshal(toolCall.Data, &td); err != nil {
		t.Fatalf("decode tool_call data: %v", err)
	}
	if td.AgentType != "general-purpose" || td.Description != "explore the repo" {
		t.Errorf("agent identity = type %q desc %q", td.AgentType, td.Description)
	}
	if td.AgentUsage == nil {
		t.Fatal("expected agent_usage on the spawning tool_call")
	}
	if td.AgentUsage.InputTokens != 40 || td.AgentUsage.OutputTokens != 20 {
		t.Errorf("agent_usage = %+v", td.AgentUsage)
	}

	// Nested steps are the sub-agent's own, sidechain-guard defeated.
	types := make([]string, 0, len(toolCall.Steps))
	for _, s := range toolCall.Steps {
		types = append(types, s.Type)
	}
	want := []string{"user_input", "text_response", "tool_call"}
	if len(types) != len(want) {
		t.Fatalf("nested steps = %v, want %v", types, want)
	}
	for i := range want {
		if types[i] != want[i] {
			t.Errorf("nested step[%d] = %q, want %q", i, types[i], want[i])
		}
	}
}

// A sub-agent whose toolUseId matches no tool_use in the rendered parent must
// still appear, appended at the end of its turn rather than silently dropped.
func TestBuildJourney_OrphanSubagentAppended(t *testing.T) {
	parentLines := []string{
		userInputEvent("u1", ts(t0), "Do it"),
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "text", "text": "the task result is hidden"},
		}),
	}
	subagents := map[string]subagentFixture{
		"agent-y": {
			meta: subagentMetaJSON("toolu_gone", "general-purpose", "orphan task"),
			lines: []string{
				subagentSidechainEvent("assistant", "oa1", ts(t1.Add(time.Second)), []map[string]any{
					{"type": "text", "text": "orphan work"},
				}),
			},
		},
	}

	journey, err := buildJourney("sess-185", writeSubagentFixture(t, parentLines, subagents), testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	steps := journey.Turns[0].Steps
	last := steps[len(steps)-1]
	if last.Type != "sub_agent" {
		t.Fatalf("last step = %q, want sub_agent appended", last.Type)
	}
	var sd SubAgentData
	if err := json.Unmarshal(last.Data, &sd); err != nil {
		t.Fatalf("decode sub_agent data: %v", err)
	}
	if sd.AgentID != "agent-y" || sd.Description != "orphan task" {
		t.Errorf("sub_agent = %+v", sd)
	}
	if sd.Usage == nil {
		t.Error("expected usage on the orphan sub_agent step")
	}
	if len(last.Steps) == 0 {
		t.Error("orphan sub_agent should carry its own steps")
	}
}

// A session with no subagents/ directory must produce exactly the journey it
// produced before this feature: flat, no nesting, no extra steps.
func TestBuildJourney_NoSubagentsUnchanged(t *testing.T) {
	parentLines := []string{
		userInputEvent("u1", ts(t0), "Read a file"),
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "text", "text": "reading"},
			{"type": "tool_use", "id": "toolu_1", "name": "Read", "input": map[string]any{"path": "a.go"}},
		}),
		toolResultEvent("u2", ts(t2), "toolu_1", "contents", false),
	}

	journey, err := buildJourney("sess-185", writeSubagentFixture(t, parentLines, nil), testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if journey.TotalTurns != 1 {
		t.Fatalf("want 1 turn, got %d", journey.TotalTurns)
	}
	types := make([]string, 0, len(journey.Turns[0].Steps))
	for _, s := range journey.Turns[0].Steps {
		types = append(types, s.Type)
		if len(s.Steps) > 0 {
			t.Errorf("step %q has unexpected nested steps", s.Type)
		}
	}
	want := []string{"user_input", "text_response", "tool_call", "tool_result"}
	if len(types) != len(want) {
		t.Fatalf("steps = %v, want %v", types, want)
	}
	for i := range want {
		if types[i] != want[i] {
			t.Errorf("step[%d] = %q, want %q", i, types[i], want[i])
		}
	}

	// No agent identity or usage may leak onto an ordinary tool_call.
	var td ToolCallData
	for _, s := range journey.Turns[0].Steps {
		if s.Type == "tool_call" {
			_ = json.Unmarshal(s.Data, &td)
		}
	}
	if td.AgentType != "" || td.Description != "" || td.AgentUsage != nil {
		t.Errorf("ordinary tool_call carries agent data: %+v", td)
	}
}

// Criterion 4: no executing code path may still render "progress" events.
func TestNoProgressCaseRemains(t *testing.T) {
	entries, err := os.ReadDir(".")
	if err != nil {
		t.Fatal(err)
	}
	for _, e := range entries {
		name := e.Name()
		if e.IsDir() || !strings.HasSuffix(name, ".go") || strings.HasSuffix(name, "_test.go") {
			continue
		}
		content, err := os.ReadFile(name) //nolint:gosec
		if err != nil {
			t.Fatal(err)
		}
		if strings.Contains(string(content), `"progress"`) {
			t.Errorf("%s still contains an executing \"progress\" path", name)
		}
	}
}

// TestBuildJourney_CompactBoundaryStep covers the compaction step: a compacted
// conversation is one of the most useful things to see on a timeline, since it
// explains an abrupt loss of context.
func TestBuildJourney_CompactBoundaryStep(t *testing.T) {
	lines := []string{
		userInputEvent("u1", ts(t0), "Keep going"),
		mustMarshal(map[string]any{
			"type":      "system",
			"subtype":   "compact_boundary",
			"uuid":      "s1",
			"timestamp": ts(t1),
			"content":   "Conversation compacted",
			"compactMetadata": map[string]any{
				"trigger":    "auto",
				"preTokens":  166513,
				"postTokens": 29504,
				// Deliberately NOT equal to preTokens-postTokens, so the test
				// distinguishes this step's own drop from the session's
				// running total.
				"cumulativeDroppedTokens": 900000,
				"durationMs":              145993,
			},
		}),
	}

	journey, err := buildJourney("test-session", writeJourneyJSONL(t, lines), testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if journey == nil || len(journey.Turns) == 0 {
		t.Fatal("expected a turn")
	}

	var step *JourneyStep
	for i := range journey.Turns[0].Steps {
		if journey.Turns[0].Steps[i].Type == "compaction" {
			step = &journey.Turns[0].Steps[i]
			break
		}
	}
	if step == nil {
		t.Fatal("expected a compaction step")
	}
	if step.DurationMs != 145993 {
		t.Errorf("duration_ms = %d, want 145993", step.DurationMs)
	}

	var data CompactionData
	if err := json.Unmarshal(step.Data, &data); err != nil {
		t.Fatalf("decode compaction data: %v", err)
	}
	if data.Trigger != "auto" || data.PreTokens != 166513 || data.PostTokens != 29504 {
		t.Errorf("compaction data = %+v", data)
	}
	// A journey step describes itself, so dropped_tokens is this compaction's
	// own drop — not the session-wide cumulative figure.
	if want := 166513 - 29504; data.DroppedTokens != want {
		t.Errorf("dropped_tokens = %d, want %d (this step's own drop, not the cumulative total)",
			data.DroppedTokens, want)
	}
}

// TestBuildJourney_OtherSystemSubtypesProduceNoStep guards the negative case —
// only compact_boundary and turn_duration are consumed.
func TestBuildJourney_OtherSystemSubtypesProduceNoStep(t *testing.T) {
	lines := []string{
		userInputEvent("u1", ts(t0), "Hello"),
		mustMarshal(map[string]any{
			"type": "system", "subtype": "away_summary",
			"uuid": "s1", "timestamp": ts(t1), "content": "You were away",
		}),
		mustMarshal(map[string]any{
			"type": "system", "subtype": "local_command",
			"uuid": "s2", "timestamp": ts(t2), "content": "/clear",
		}),
	}

	journey, err := buildJourney("test-session", writeJourneyJSONL(t, lines), testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for _, turn := range journey.Turns {
		for _, s := range turn.Steps {
			if s.Type == "compaction" || s.Type == "thinking_duration" {
				t.Errorf("unexpected step %q from an unconsumed system subtype", s.Type)
			}
		}
	}
}

// TestBuildJourney_CompactBoundaryWithoutMetadata must not emit a step — the
// payload is what makes the step worth showing.
func TestBuildJourney_CompactBoundaryWithoutMetadata(t *testing.T) {
	lines := []string{
		userInputEvent("u1", ts(t0), "Hello"),
		mustMarshal(map[string]any{
			"type": "system", "subtype": "compact_boundary",
			"uuid": "s1", "timestamp": ts(t1),
		}),
	}

	journey, err := buildJourney("test-session", writeJourneyJSONL(t, lines), testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for _, turn := range journey.Turns {
		for _, s := range turn.Steps {
			if s.Type == "compaction" {
				t.Error("expected no compaction step when compactMetadata is absent")
			}
		}
	}
}

// ── Turn segmentation through the shared predicate (issue #227) ─────────────

// rawContentUserEvent builds a user event whose message content is the given
// raw JSON, so a test can write block shapes userInputEvent/toolResultEvent do
// not cover — notably a mixed array where the tool_result is not first.
func rawContentUserEvent(uuid, ts_ string, content string) string {
	return mustMarshal(map[string]any{
		"type":      "user",
		"uuid":      uuid,
		"sessionId": "test-session",
		"timestamp": ts_,
		"message":   map[string]any{"role": "user", "content": json.RawMessage(content)},
	})
}

// stepTypes flattens a turn's step types for comparison.
func stepTypes(turn JourneyTurn) []string {
	types := make([]string, 0, len(turn.Steps))
	for _, s := range turn.Steps {
		types = append(types, s.Type)
	}
	return types
}

// TestBuildJourney_InjectedWrapperOpensNoTurn is #227's headline case: the
// wrappers Claude Code writes as user events are not turns for message_count or
// turn_count, and after this change they are not turns on the timeline either —
// nor may their raw tag soup become a user_input step or the journey summary.
func TestBuildJourney_InjectedWrapperOpensNoTurn(t *testing.T) {
	wrappers := []string{
		"<task-notification>\n<status>completed</status>\n</task-notification>",
		"<command-message>lab-workflow:github-issue-to-pr</command-message>",
		"<command-name>/review-pr</command-name>",
		"<local-command-caveat>Caveat: the messages below</local-command-caveat>",
		"<local-command-stdout>(no content)</local-command-stdout>",
		"<system-reminder>\nThe user named this session\n</system-reminder>",
	}
	for _, wrapper := range wrappers {
		t.Run(wrapper[:min(len(wrapper), 24)], func(t *testing.T) {
			journey, err := buildJourney("test-session",
				writeJourneyJSONL(t, []string{userInputEvent("u1", ts(t0), wrapper)}), testLogger)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if journey == nil {
				t.Fatal("expected a journey — the wrapper is still an event with a timestamp")
			}
			if journey.TotalTurns != 0 {
				t.Errorf("total_turns = %d, want 0 — nobody typed the wrapper", journey.TotalTurns)
			}
			for _, turn := range journey.Turns {
				for _, s := range turn.Steps {
					if s.Type == "user_input" {
						t.Errorf("wrapper produced a user_input step: %s", s.Data)
					}
				}
			}
			if journey.Summary != "" {
				t.Errorf("summary = %q, want empty — never seeded from a wrapper", journey.Summary)
			}
		})
	}
}

// TestBuildJourney_ToolResultNotFirstOpensNoTurn is the explicit regression for
// the block-order divergence: the old test decoded only blocks[0], so a carrier
// whose tool_result sat behind another block opened a turn the session's own
// message_count never counted. The shared predicate scans every block.
func TestBuildJourney_ToolResultNotFirstOpensNoTurn(t *testing.T) {
	lines := []string{
		userInputEvent("u1", ts(t0), "Please read main.go"),
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "tool_use", "id": "tool1", "name": "Read", "input": map[string]any{"path": "main.go"}},
		}),
		// The tool_result is the SECOND block, behind a text block.
		rawContentUserEvent("u2", ts(t2),
			`[{"type":"text","text":"here you go"},`+
				`{"type":"tool_result","tool_use_id":"tool1","content":"package main","is_error":false}]`),
	}

	journey, err := buildJourney("test-session", writeJourneyJSONL(t, lines), testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if journey.TotalTurns != 1 {
		t.Fatalf("total_turns = %d, want 1 — a tool_result carrier is a carrier at any block position",
			journey.TotalTurns)
	}
	want := []string{"user_input", "tool_call", "tool_result"}
	if got := stepTypes(journey.Turns[0]); !equalStrings(got, want) {
		t.Errorf("steps = %v, want %v — the result must still attach to the enclosing turn", got, want)
	}
}

// TestBuildJourney_CarrierAsFirstEventStillCreatesTurn pins the load-bearing
// ensureTurn on the non-genuine path: a transcript can open with a tool-result
// carrier (a resumed session, or one whose head was compacted away), and
// short-circuiting that branch would lose the steps entirely.
func TestBuildJourney_CarrierAsFirstEventStillCreatesTurn(t *testing.T) {
	lines := []string{
		toolResultEvent("u1", ts(t0), "tool1", "orphan output", true),
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "text", "text": "carrying on"},
		}),
	}

	journey, err := buildJourney("test-session", writeJourneyJSONL(t, lines), testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if journey.TotalTurns != 1 {
		t.Fatalf("total_turns = %d, want 1 — the orphan steps need a turn to live in", journey.TotalTurns)
	}
	want := []string{"tool_result", "text_response"}
	if got := stepTypes(journey.Turns[0]); !equalStrings(got, want) {
		t.Fatalf("steps = %v, want %v", got, want)
	}
	// The payload still round-trips through the attach path, error flag included.
	var d ToolResultData
	if err := json.Unmarshal(journey.Turns[0].Steps[0].Data, &d); err != nil {
		t.Fatalf("decode tool_result: %v", err)
	}
	if d.ToolUseID != "tool1" || d.Content != "orphan output" || !d.IsError {
		t.Errorf("tool_result data = %+v", d)
	}
}

// TestBuildJourney_SummaryComesFromFirstGenuineTurn covers a mixed transcript:
// the slash-command expansion that opens most real sessions must not become the
// journey's summary; the prompt behind it must.
func TestBuildJourney_SummaryComesFromFirstGenuineTurn(t *testing.T) {
	lines := []string{
		userInputEvent("u1", ts(t0), "<command-message>lab-workflow:review-pr</command-message>"),
		userInputEvent("u2", ts(t0.Add(time.Second)), "review PR 42 for me"),
		assistantEvent("a1", "u2", ts(t1), []map[string]any{
			{"type": "tool_use", "id": "tool1", "name": "Read", "input": map[string]any{"path": "a.go"}},
		}),
		toolResultEvent("u3", ts(t2), "tool1", "contents", false),
		userInputEvent("u4", ts(t3), "<task-notification>done</task-notification>"),
	}

	journey, err := buildJourney("test-session", writeJourneyJSONL(t, lines), testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if journey.TotalTurns != 1 {
		t.Fatalf("total_turns = %d, want 1 — only one event here was typed by a person", journey.TotalTurns)
	}
	if journey.Summary != "review PR 42 for me" {
		t.Errorf("summary = %q, want the genuine prompt", journey.Summary)
	}
	want := []string{"user_input", "tool_call", "tool_result"}
	if got := stepTypes(journey.Turns[0]); !equalStrings(got, want) {
		t.Errorf("steps = %v, want %v", got, want)
	}
	var d UserInputData
	if err := json.Unmarshal(journey.Turns[0].Steps[0].Data, &d); err != nil {
		t.Fatalf("decode user_input: %v", err)
	}
	if d.Content != "review PR 42 for me" {
		t.Errorf("user_input content = %q, want the genuine prompt", d.Content)
	}
}

// TestBuildJourney_SubagentNestingSurvivesWrapperTurns is the required nesting
// regression: #227 moves turn boundaries underneath the toolUseId join, so both
// the matched (nested) and unmatched (appended) sub-agent paths are re-checked
// on a transcript whose first user event is a wrapper.
func TestBuildJourney_SubagentNestingSurvivesWrapperTurns(t *testing.T) {
	parentLines := []string{
		userInputEvent("u0", ts(t0), "<command-name>/explore</command-name>"),
		userInputEvent("u1", ts(t0.Add(time.Second)), "Explore the repo"),
		assistantEvent("a1", "u1", ts(t1), []map[string]any{
			{"type": "tool_use", "id": "toolu_1", "name": "Task", "input": map[string]any{"description": "explore"}},
		}),
		toolResultEvent("u2", ts(t2), "toolu_1", "agent done", false),
	}
	subagents := map[string]subagentFixture{
		"agent-x": {
			meta: subagentMetaJSON("toolu_1", "general-purpose", "explore the repo"),
			lines: []string{
				subagentSidechainEvent("user", "su1", ts(t1.Add(time.Second)), nil),
				subagentSidechainEvent("assistant", "sa1", ts(t1.Add(2*time.Second)), []map[string]any{
					{"type": "text", "text": "agent working"},
				}),
			},
		},
		"agent-orphan": {
			meta: subagentMetaJSON("toolu_gone", "Explore", "orphan task"),
			lines: []string{
				subagentSidechainEvent("assistant", "oa1", ts(t3), []map[string]any{
					{"type": "text", "text": "orphan work"},
				}),
			},
		},
	}

	journey, err := buildJourney("sess-185", writeSubagentFixture(t, parentLines, subagents), testLogger)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if journey.TotalTurns != 1 {
		t.Fatalf("total_turns = %d, want 1 — the wrapper must not open a turn", journey.TotalTurns)
	}
	if journey.Summary != "Explore the repo" {
		t.Errorf("summary = %q, want the genuine prompt", journey.Summary)
	}

	var toolCall, orphan *JourneyStep
	for i := range journey.Turns[0].Steps {
		switch journey.Turns[0].Steps[i].Type {
		case "tool_call":
			toolCall = &journey.Turns[0].Steps[i]
		case "sub_agent":
			orphan = &journey.Turns[0].Steps[i]
		}
	}
	if toolCall == nil {
		t.Fatal("expected the spawning tool_call step")
	}
	var td ToolCallData
	if err := json.Unmarshal(toolCall.Data, &td); err != nil {
		t.Fatalf("decode tool_call data: %v", err)
	}
	if td.AgentType != "general-purpose" || td.Description != "explore the repo" {
		t.Errorf("agent identity = type %q desc %q — the toolUseId join must be unchanged",
			td.AgentType, td.Description)
	}
	if len(toolCall.Steps) == 0 {
		t.Error("delegated steps no longer nest under the spawning Task tool_use")
	}
	if orphan == nil {
		t.Fatal("the unmatched sub-agent was dropped rather than appended to its turn")
	}
	if journey.SubagentCount != 2 {
		t.Errorf("subagent_count = %d, want 2", journey.SubagentCount)
	}
}

// TestBuildJourney_TurnsAgreeWithInsightPipeline is #227's anti-drift check, the
// journey-path counterpart of TestScan_AgreesWithInsightPipeline: the number of
// turns a journey renders must equal the insight pipeline's turn_count over the
// same transcript, because both now resolve "is this a turn?" through the one
// isUserTurnContent predicate. A fourth definition cannot be added quietly.
//
// Both fixtures deliberately open with genuine input. A transcript whose first
// user event is a tool-result carrier is the one structural difference between
// the two paths — ensureTurn must give those orphan steps somewhere to live,
// while the pipeline counts no turn — and is covered on its own by
// TestBuildJourney_CarrierAsFirstEventStillCreatesTurn.
func TestBuildJourney_TurnsAgreeWithInsightPipeline(t *testing.T) {
	base := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)

	userEvent := func(sessionID string, at int, content string) rawEvent {
		return rawEvent{
			Type: "user", SessionID: sessionID, CWD: "/tmp",
			Timestamp: base.Add(time.Duration(at) * time.Second),
			Message:   &rawMessage{Role: "user", Content: json.RawMessage(content)},
		}
	}
	assistantEv := func(sessionID string, at int, content string) rawEvent {
		return rawEvent{
			Type: "assistant", SessionID: sessionID,
			Timestamp: base.Add(time.Duration(at) * time.Second),
			Message: &rawMessage{
				Role: "assistant", Model: "claude-opus-4-8",
				Content: json.RawMessage(content),
				Usage:   &rawUsage{InputTokens: 1, OutputTokens: 1},
			},
		}
	}

	cases := []struct {
		name  string
		write func(t *testing.T, dir, sessionID string)
	}{
		{
			// The very fixture the scanner/pipeline agreement test uses.
			name: "counts fixture",
			write: func(t *testing.T, dir, sessionID string) {
				countsFixture(t, dir, sessionID, base)
			},
		},
		{
			// Wrappers interleaved with two genuine turns and a carrier.
			name: "wrappers between genuine turns",
			write: func(t *testing.T, dir, sessionID string) {
				writeRawEvents(t, dir, sessionID, []rawEvent{
					userEvent(sessionID, 0, `"<command-name>/review-pr</command-name>"`),
					userEvent(sessionID, 1, `"first prompt"`),
					assistantEv(sessionID, 2, `[{"type":"text","text":"on it"}]`),
					assistantEv(sessionID, 3, `[{"type":"tool_use","id":"tu","name":"Read","input":{}}]`),
					userEvent(sessionID, 4, `[{"type":"tool_result","tool_use_id":"tu","content":"body"}]`),
					userEvent(sessionID, 5, `"<task-notification>\n<status>completed</status>\n</task-notification>"`),
					userEvent(sessionID, 6, `"second prompt"`),
					assistantEv(sessionID, 7, `[{"type":"text","text":"done"}]`),
				})
			},
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			const sessionID = "session-journey-drift"
			tc.write(t, dir, sessionID)
			path := filepath.Join(dir, sessionID+jsonlExt)

			journey, err := buildJourney(sessionID, path, testLogger)
			if err != nil {
				t.Fatalf("build journey: %v", err)
			}
			if journey == nil {
				t.Fatal("expected a journey")
			}

			turns := &TurnCountProcessor{}
			for _, ev := range readProcessableEvents(t, path) {
				turns.Process(ev)
			}
			var insight SessionInsight
			turns.Finalize(&insight)

			if len(journey.Turns) != insight.TurnCount {
				t.Errorf("journey turns = %d, insight turn_count = %d — the journey has drifted "+
					"from the shared isUserTurnContent predicate",
					len(journey.Turns), insight.TurnCount)
			}
			if journey.TotalTurns != len(journey.Turns) {
				t.Errorf("total_turns = %d, len(turns) = %d", journey.TotalTurns, len(journey.Turns))
			}
		})
	}
}

// equalStrings compares two string slices element-wise.
func equalStrings(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
