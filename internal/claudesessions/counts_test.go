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
// tool-call-only) + 5 tool_result carrier user events + 1 injected wrapper
// event = 15 raw events, 4 turns.
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

	// A wrapper Claude Code injected as a user event (#197). It is a raw event
	// like any other, but nobody typed it, so it is not a turn.
	appendEvent(userEvent(`"<task-notification>\n<status>completed</status>\n</task-notification>"`))

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
	if s.EventCount != 15 {
		t.Errorf("event_count = %d, want 15 raw events — the injected wrapper is still an event", s.EventCount)
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
	if detail.MessageCount != 4 || detail.EventCount != 15 {
		t.Errorf("detail counters = %d msgs / %d events, want 4 / 15",
			detail.MessageCount, detail.EventCount)
	}
	// Every event still renders, regardless of how it is counted.
	if len(detail.Messages) != 15 {
		t.Errorf("rendered %d messages, want all 15 events", len(detail.Messages))
	}
}

// TestScan_TypedStringContentUserEventIsATurn guards parseContentBlocks' early
// return on non-array content: a bare JSON string a person typed is a real
// prompt, not a carrier. Injected string content is the other half of that
// rule and is covered by TestScan_InjectedUserEventIsNotATurn.
func TestScan_TypedStringContentUserEventIsATurn(t *testing.T) {
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
		t.Errorf("message_count = %d, want 2 — typed string content must count as a turn", s.MessageCount)
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

	assertScannerAgreesWithPipeline(t, s, filepath.Join(projectDir, "session-agree.jsonl"))
}

// assertScannerAgreesWithPipeline replays a transcript through the real insight
// processor and checks the scanner's message_count against turn_count plus the
// assistant replies. Both sides read isUserTurnContent, so any divergence means
// the shared predicate stopped being shared.
func assertScannerAgreesWithPipeline(t *testing.T, s ClaudeSessionSummary, path string) {
	t.Helper()

	turns := &TurnCountProcessor{}
	assistantReplies := 0
	for _, ev := range readProcessableEvents(t, path) {
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
	if s.EventCount != 15 {
		t.Errorf("event_count = %d, want 15 after the version bump backfilled it", s.EventCount)
	}
}

// TestVersionConstants_BumpedTogetherForInjectedTurns pins #197's central risk,
// raised to #226's floor. isUserTurnContent feeds both message_count (scanner)
// and turn_count (insight pipeline), so bumping only one constant would leave
// the two recomputed at different times and disagreeing — exactly the drift
// #182 existed to remove. The floors move together on every change to the
// predicate; #226 extended it to array content, so both are one higher.
func TestVersionConstants_BumpedTogetherForInjectedTurns(t *testing.T) {
	if CurrentScannerVersion < 11 {
		t.Errorf("CurrentScannerVersion = %d, want >= 11 so cached message_count is recomputed",
			CurrentScannerVersion)
	}
	if CurrentProcessorVersion < 9 {
		t.Errorf("CurrentProcessorVersion = %d, want >= 9 so stored turn_count is reprocessed",
			CurrentProcessorVersion)
	}
}

// writeRawEvents marshals events to a session JSONL file, one per line.
func writeRawEvents(t *testing.T, dir, sessionID string, events []rawEvent) {
	t.Helper()
	data := make([]byte, 0, len(events)*256)
	for _, ev := range events {
		b, err := json.Marshal(ev)
		if err != nil {
			t.Fatalf("marshal: %v", err)
		}
		data = append(data, b...)
		data = append(data, '\n')
	}
	if err := os.WriteFile(filepath.Join(dir, sessionID+".jsonl"), data, 0600); err != nil {
		t.Fatalf("write jsonl: %v", err)
	}
}

// TestIsUserTurnContent covers the shared predicate directly — it is the single
// definition of "turn" for both message_count and the insight pipeline, so its
// edges are worth pinning independently of any scan.
func TestIsUserTurnContent(t *testing.T) {
	tests := []struct {
		name    string
		content string
		want    bool
	}{
		{"typed prose", `"please refactor this"`, true},
		{"empty string", `""`, true},

		// One case per injected wrapper Claude Code writes.
		{"task notification", `"<task-notification>\n<task-id>abc</task-id>\n"`, false},
		{"command message", `"<command-message>lab-workflow:review-pr</command-message>"`, false},
		{"command name", `"<command-name>/review-pr</command-name>"`, false},
		{"local command caveat", `"<local-command-caveat>Caveat: the messages below</local-command-caveat>"`, false},
		{"local command stdout", `"<local-command-stdout>(no content)</local-command-stdout>"`, false},
		{"system reminder", `"<system-reminder>\nThe user named this session\n</system-reminder>"`, false},

		// Leading whitespace must not smuggle a wrapper past the check.
		{"wrapper behind a newline", `"\n  <system-reminder>hi</system-reminder>"`, false},

		// ...but a person writing *about* a marker is still a turn. The real
		// corpus contains such a prompt, so this is the false positive the
		// prefix anchor exists to prevent.
		{
			"prose mentioning a marker mid-text",
			`"fix the bug where <system-reminder> events count as human turns"`,
			true,
		},
		{"prose ending with a marker", `"the wrapper is called <command-message>"`, true},

		// A marker-like tag that is not one of ours stays a turn.
		{"unrelated leading tag", `"<div>hello</div>"`, true},

		// Array content is unaffected by the *string* wrapper rule.
		{"array with tool_result", `[{"type":"tool_result","tool_use_id":"t1"}]`, false},
		{"array with text", `[{"type":"text","text":"hello"}]`, true},
		{
			"array whose text opens with a string-only wrapper is still a turn",
			`[{"type":"text","text":"<system-reminder>x</system-reminder>"}]`,
			true,
		},

		// #226: the injected classes that arrive as a lone text block.
		{
			"skill preamble",
			`[{"type":"text","text":"Base directory for this skill: ` +
				`/home/u/.claude/skills/review-pr\n\n# Strict PR Review"}]`,
			false,
		},
		{
			"interrupted by user",
			`[{"type":"text","text":"[Request interrupted by user]"}]`,
			false,
		},
		{
			"interrupted by user for tool use",
			`[{"type":"text","text":"[Request interrupted by user for tool use]"}]`,
			false,
		},

		// ...and the false positives the anchor, the path token and the
		// single-block rule exist to prevent.
		{
			"prose quoting the skill marker mid-sentence",
			`[{"type":"text","text":"the preamble reads Base directory for this skill: fix the parser"}]`,
			true,
		},
		{
			"prose opening with the skill words but no path token",
			`[{"type":"text","text":"Base directory for this skill:"}]`,
			true,
		},
		{
			"prose mentioning an interruption mid-sentence",
			`[{"type":"text","text":"why does [Request interrupted by user] count as a turn?"}]`,
			true,
		},
		{
			"multi-block array whose first text opens with a marker",
			`[{"type":"text","text":"[Request interrupted by user]"},` +
				`{"type":"text","text":"and here is what I meant"}]`,
			true,
		},
		{
			"marker text alongside an image block",
			`[{"type":"image","source":{}},` +
				`{"type":"text","text":"Base directory for this skill: /opt/skills/x"}]`,
			true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := isUserTurnContent(json.RawMessage(tt.content)); got != tt.want {
				t.Errorf("isUserTurnContent(%s) = %v, want %v", tt.content, got, tt.want)
			}
		})
	}
}

// TestScan_InjectedUserEventIsNotATurn is the acceptance case: the wrapper is
// still a raw event, but it is not a message, and it must not become the
// session preview either — preview is gated on the same predicate and is the
// last fallback in the display-title chain.
func TestScan_InjectedUserEventIsNotATurn(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)

	if err := os.MkdirAll(projectDir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	events := []rawEvent{
		{
			Type: "user", SessionID: "session-injected", Timestamp: ts, CWD: "/tmp",
			Message: &rawMessage{Role: "user", Content: json.RawMessage(
				`"<command-message>lab-workflow:github-issue-to-pr</command-message>"`)},
		},
		{
			Type: "user", SessionID: "session-injected", Timestamp: ts.Add(time.Second), CWD: "/tmp",
			Message: &rawMessage{Role: "user", Content: json.RawMessage(`"the real prompt"`)},
		},
		{
			Type: "user", SessionID: "session-injected", Timestamp: ts.Add(2 * time.Second), CWD: "/tmp",
			Message: &rawMessage{Role: "user", Content: json.RawMessage(
				`"<task-notification>\n<status>completed</status>\n</task-notification>"`)},
		},
	}
	writeRawEvents(t, projectDir, "session-injected", events)

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, "session-injected")

	if s.MessageCount != 1 {
		t.Errorf("message_count = %d, want 1 — only the typed prompt is a turn", s.MessageCount)
	}
	// event_count is deliberately untouched by this change: it is the raw
	// top-level event total, and the wrappers really are events.
	if s.EventCount != 3 {
		t.Errorf("event_count = %d, want 3 — filtering turns must not drop raw events", s.EventCount)
	}
	if s.Preview != "the real prompt" {
		t.Errorf("preview = %q, want the typed prompt — a wrapper must never seed the preview", s.Preview)
	}
}

// TestScan_ArrayInjectedUserEventsAreNotTurns is #226's acceptance case: the
// skill preamble and the interruption notice arrive as array content, so #197's
// string-only rule missed them and they kept counting as human turns. They are
// still raw events, and a genuine array message that merely quotes one is still
// a turn.
func TestScan_ArrayInjectedUserEventsAreNotTurns(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)

	if err := os.MkdirAll(projectDir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	const id = "session-array-injected"
	userEvent := func(offset int, content string) rawEvent {
		return rawEvent{
			Type: "user", SessionID: id, CWD: "/tmp",
			Timestamp: ts.Add(time.Duration(offset) * time.Second),
			Message:   &rawMessage{Role: "user", Content: json.RawMessage(content)},
		}
	}
	writeRawEvents(t, projectDir, id, []rawEvent{
		// A real prompt.
		userEvent(0, `[{"type":"text","text":"please refactor this"}]`),
		// A skill invocation the harness injected.
		userEvent(1, `[{"type":"text","text":"Base directory for this skill: `+
			`/home/u/.claude/skills/review-pr\n\n# Strict PR Review"}]`),
		// Two interruption notices, both variants.
		userEvent(2, `[{"type":"text","text":"[Request interrupted by user]"}]`),
		userEvent(3, `[{"type":"text","text":"[Request interrupted by user for tool use]"}]`),
		// A string wrapper, still excluded by #197.
		userEvent(4, `"<task-notification>\n<status>completed</status>\n</task-notification>"`),
		// A person writing *about* the markers. Prefix-anchored, never a
		// substring, so this is a turn.
		userEvent(5, `[{"type":"text","text":"why does [Request interrupted by user] count?"}]`),
	})

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, id)

	if s.MessageCount != 2 {
		t.Errorf("message_count = %d, want 2 — only the typed prompt and the prompt quoting a marker",
			s.MessageCount)
	}
	if s.EventCount != 6 {
		t.Errorf("event_count = %d, want 6 — filtering turns must not drop raw events", s.EventCount)
	}
	if s.Preview != "please refactor this" {
		t.Errorf("preview = %q, want the typed prompt", s.Preview)
	}
	assertScannerAgreesWithPipeline(t, s, filepath.Join(projectDir, id+".jsonl"))
}

// TestScan_SkillPreambleOnlySessionKeepsALabel is the preview half of #226. The
// preamble moved from the genuine-turn branch to the injected one, and both
// apply fallbackPreviewLabel — so a session that is nothing but a skill
// invocation must still name the skill rather than render as a blank row.
func TestScan_SkillPreambleOnlySessionKeepsALabel(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	if err := os.MkdirAll(projectDir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	const id = "session-skill-only"
	writeRawEvents(t, projectDir, id, []rawEvent{
		{
			Type: "user", SessionID: id, Timestamp: ts, CWD: "/tmp",
			Message: &rawMessage{Role: "user", Content: json.RawMessage(
				`[{"type":"text","text":"Base directory for this skill: ` +
					`/home/u/.claude/plugins/cache/lab/skills/github-issue-to-pr\n\n# Do the thing"}]`)},
		},
	})

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, id)

	if s.MessageCount != 0 {
		t.Errorf("message_count = %d, want 0 — nobody typed the preamble", s.MessageCount)
	}
	if s.Preview != "skill: github-issue-to-pr" {
		t.Errorf("preview = %q, want %q", s.Preview, "skill: github-issue-to-pr")
	}
	if s.ResolveDisplayTitle() == "" {
		t.Error("display title is empty — the session is unidentifiable in the list")
	}
}

// TestScan_WrapperOnlySessionKeepsALabel covers the one place turn filtering
// must NOT reach: Preview is the last fallback in ResolveDisplayTitle, so a
// transcript that is nothing but a slash command and its expansion would
// otherwise render as a blank row in the sessions list.
func TestScan_WrapperOnlySessionKeepsALabel(t *testing.T) {
	db := setupTestDB(t)
	projectDir := titleProjectDir(t)
	ts := time.Date(2025, 6, 1, 10, 0, 0, 0, time.UTC)
	if err := os.MkdirAll(projectDir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	writeRawEvents(t, projectDir, "session-wrapper-only", []rawEvent{
		{
			Type: "user", SessionID: "session-wrapper-only", Timestamp: ts, CWD: "/tmp",
			Message: &rawMessage{Role: "user", Content: json.RawMessage(
				`"<command-name>/plugin</command-name>"`)},
		},
		{
			Type: "user", SessionID: "session-wrapper-only", Timestamp: ts.Add(time.Second), CWD: "/tmp",
			Message: &rawMessage{Role: "user", Content: json.RawMessage(
				`"<local-command-stdout>(no content)</local-command-stdout>"`)},
		},
	})

	sessions, err := IncrementalScan(db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	s := findSession(t, sessions, "session-wrapper-only")

	if s.MessageCount != 0 {
		t.Errorf("message_count = %d, want 0 — nobody typed anything here", s.MessageCount)
	}
	if s.Preview == "" {
		t.Error("preview is empty — the session would render as a blank row")
	}
	if s.ResolveDisplayTitle() == "" {
		t.Error("display title is empty — the session is unidentifiable in the list")
	}
}
