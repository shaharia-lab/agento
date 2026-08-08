package claudesessions

import (
	"bufio"
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// countsFixture writes a transcript matching the shape a real session has: one
// typed prompt, assistant turns that are mostly pure tool calls, and the
// tool_result carrier events those calls produce. Only the first and the
// text-bearing assistant turns are messages anybody would count.
//
// Layout: 1 real user prompt + 8 assistant events (3 with a text block, 5
// tool-call-only) + 5 tool_result carrier user events = 14 raw events, 4 turns.
func countsFixture(t *testing.T, dir, sessionID string, ts time.Time) {
	t.Helper()
	if err := os.MkdirAll(dir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	var data []byte
	at := 0
	appendEvent := func(ev rawEvent) {
		at++
		ev.SessionID = sessionID
		ev.Timestamp = ts.Add(time.Duration(at) * time.Second)
		b, err := json.Marshal(ev)
		if err != nil {
			t.Fatalf("marshal: %v", err)
		}
		data = append(data, b...)
		data = append(data, '\n')
	}

	userEvent := func(content string) rawEvent {
		return rawEvent{
			Type: "user", CWD: "/tmp",
			Message: &rawMessage{Role: "user", Content: json.RawMessage(content)},
		}
	}
	assistantEvent := func(content string) rawEvent {
		return rawEvent{
			Type: "assistant",
			Message: &rawMessage{
				Role: "assistant", Model: "claude-opus-4-8",
				Content: json.RawMessage(content),
				Usage:   &rawUsage{InputTokens: 1, OutputTokens: 1},
			},
		}
	}

	// The one thing a human actually typed.
	appendEvent(userEvent(`[{"type":"text","text":"please refactor this"}]`))

	// Five tool-call-only assistant turns, each answered by a tool_result
	// carrier the user never saw or wrote.
	for i := 0; i < 5; i++ {
		appendEvent(assistantEvent(`[{"type":"tool_use","id":"tu","name":"Read","input":{}}]`))
		appendEvent(userEvent(`[{"type":"tool_result","tool_use_id":"tu","content":"file body"}]`))
	}

	// Three assistant turns the user actually read.
	for i := 0; i < 3; i++ {
		appendEvent(assistantEvent(`[{"type":"text","text":"here is what I changed"}]`))
	}

	if err := os.WriteFile(filepath.Join(dir, sessionID+".jsonl"), data, 0600); err != nil {
		t.Fatalf("write jsonl: %v", err)
	}
}

// TestScan_MessageCountIsTurnsNotEvents is the headline behavior: message_count
// stops counting tool_result carriers and pure tool-call turns, and event_count
// preserves the number it used to report.
func TestScan_MessageCountIsTurnsNotEvents(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	countsFixture(t, projectDir, "session-counts", time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, "session-counts")

	if s.MessageCount != 4 {
		t.Errorf("message_count = %d, want 4 (1 user prompt + 3 assistant replies)", s.MessageCount)
	}
	if s.EventCount != 14 {
		t.Errorf("event_count = %d, want 14 raw events", s.EventCount)
	}
	// The preview must come from the typed prompt, never a tool_result carrier.
	if s.Preview != "please refactor this" {
		t.Errorf("preview = %q, want the typed prompt", s.Preview)
	}
}

// TestDetail_CountersMatchSummary pins the two paths together. The list view
// reads the summary and the detail view recounts from the message tree, so a
// change to one set of counter sites must not silently desynchronize the other.
func TestDetail_CountersMatchSummary(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	countsFixture(t, projectDir, "session-detail-counts", time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	summary := findSession(t, sessions, "session-detail-counts")

	detail, err := readSessionDetail(
		"session-detail-counts", projectDir,
		filepath.Join(projectDir, "session-detail-counts.jsonl"), testLogger,
	)
	if err != nil {
		t.Fatalf("detail: %v", err)
	}

	if detail.MessageCount != summary.MessageCount {
		t.Errorf("detail message_count = %d, summary = %d — the two paths disagree",
			detail.MessageCount, summary.MessageCount)
	}
	if detail.EventCount != summary.EventCount {
		t.Errorf("detail event_count = %d, summary = %d — the two paths disagree",
			detail.EventCount, summary.EventCount)
	}
	if detail.MessageCount != 4 || detail.EventCount != 14 {
		t.Errorf("detail counters = %d msgs / %d events, want 4 / 14",
			detail.MessageCount, detail.EventCount)
	}
	// Every event still renders, regardless of how it is counted.
	if len(detail.Messages) != 14 {
		t.Errorf("rendered %d messages, want all 14 events", len(detail.Messages))
	}
}

// TestScan_StringContentUserEventIsATurn guards parseContentBlocks' early return
// on non-array content: a bare JSON string is a real prompt, not a carrier.
func TestScan_StringContentUserEventIsATurn(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	writeJSONL(t, projectDir, "session-string", ts)

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, "session-string")

	// writeJSONL emits a string-content user event and a text-block assistant event.
	if s.MessageCount != 2 {
		t.Errorf("message_count = %d, want 2 — string content must count as a turn", s.MessageCount)
	}
	if s.EventCount != 2 {
		t.Errorf("event_count = %d, want 2", s.EventCount)
	}
}

// TestScan_SidechainUserEventsExcluded keeps delegated user turns out of the
// parent's numbers — they are reported through the sub-agent roll-up instead.
// Only the user case is guarded: assistant events carry no sidechain check,
// matching the counter this replaces, so event_count stays the old number.
func TestScan_SidechainUserEventsExcluded(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	if err := os.MkdirAll(projectDir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	var data []byte
	appendEvent := func(ev rawEvent) {
		b, err := json.Marshal(ev)
		if err != nil {
			t.Fatalf("marshal: %v", err)
		}
		data = append(data, b...)
		data = append(data, '\n')
	}
	appendEvent(rawEvent{
		Type: "user", SessionID: "session-sidechain", Timestamp: ts, CWD: "/tmp",
		Message: &rawMessage{Role: "user", Content: json.RawMessage(`"real prompt"`)},
	})
	appendEvent(rawEvent{
		Type: "user", SessionID: "session-sidechain", Timestamp: ts.Add(time.Second),
		IsSidechain: true,
		Message:     &rawMessage{Role: "user", Content: json.RawMessage(`"delegated prompt"`)},
	})

	if err := os.WriteFile(filepath.Join(projectDir, "session-sidechain.jsonl"), data, 0600); err != nil {
		t.Fatalf("write jsonl: %v", err)
	}

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, "session-sidechain")

	if s.MessageCount != 1 {
		t.Errorf("message_count = %d, want 1 (sidechain excluded)", s.MessageCount)
	}
	if s.EventCount != 1 {
		t.Errorf("event_count = %d, want 1 (sidechain excluded)", s.EventCount)
	}
}

// TestScan_AgreesWithInsightPipeline is the anti-drift check the issue asks for:
// the scanner's message_count must equal the insight pipeline's turn_count plus
// the assistant replies, since both now share one predicate.
func TestScan_AgreesWithInsightPipeline(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	countsFixture(t, projectDir, "session-agree", time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, "session-agree")

	// Replay the same transcript through the real insight processor.
	turns := &TurnCountProcessor{}
	assistantReplies := 0
	for _, ev := range readProcessableEvents(t, filepath.Join(projectDir, "session-agree.jsonl")) {
		turns.Process(ev)
		if ev.Type == "assistant" && ev.Message != nil && isAssistantReply(ev.Message.Content) {
			assistantReplies++
		}
	}
	var insight SessionInsight
	turns.Finalize(&insight)

	if want := insight.TurnCount + assistantReplies; s.MessageCount != want {
		t.Errorf("message_count = %d, want turn_count(%d) + assistant replies(%d) = %d",
			s.MessageCount, insight.TurnCount, assistantReplies, want)
	}
}

// readProcessableEvents decodes a transcript the way the insight pipeline does,
// so the two code paths are compared on identical input.
func readProcessableEvents(t *testing.T, path string) []ProcessableEvent {
	t.Helper()
	f, err := os.Open(path) //nolint:gosec // test fixture path built by the test itself
	if err != nil {
		t.Fatalf("open transcript: %v", err)
	}
	defer func() { _ = f.Close() }()

	var events []ProcessableEvent
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	for sc.Scan() {
		line := sc.Bytes()
		if len(line) == 0 {
			continue
		}
		var ev ProcessableEvent
		if err := json.Unmarshal(line, &ev); err != nil {
			t.Fatalf("decode event: %v", err)
		}
		events = append(events, ev)
	}
	if err := sc.Err(); err != nil {
		t.Fatalf("scan transcript: %v", err)
	}
	return events
}

// TestIncrementalScan_ScannerVersionRecomputesCounts covers the upgrade path:
// rows written by the old reader hold the inflated count and zero events, and
// their files never changed — only the scanner-version bump can fix them.
func TestIncrementalScan_ScannerVersionRecomputesCounts(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)

	countsFixture(t, projectDir, "session-backfill-counts", time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC))

	if _, err := IncrementalScan(db, testLogger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	// Simulate a pre-v3 row: message_count holds the raw event total, event_count
	// is zero, the file is untouched and the recorded version is rewound.
	ctx := context.Background()
	if _, err := db.ExecContext(ctx,
		`UPDATE claude_session_cache SET message_count = 14, event_count = 0`); err != nil {
		t.Fatalf("rewind counts: %v", err)
	}
	if _, err := db.ExecContext(ctx,
		`UPDATE claude_cache_metadata SET scanner_version = 2 WHERE id = 1`); err != nil {
		t.Fatalf("rewind version: %v", err)
	}

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("backfill scan: %v", err)
	}
	s := findSession(t, sessions, "session-backfill-counts")

	if s.MessageCount != 4 {
		t.Errorf("message_count = %d, want 4 after the version bump recomputed it", s.MessageCount)
	}
	if s.EventCount != 14 {
		t.Errorf("event_count = %d, want 14 after the version bump backfilled it", s.EventCount)
	}
}
