package claudesessions

import (
	"bufio"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"
)

// ── Journey response types ──────────────────────────────────────────────────

// SessionJourney is the top-level response for the journey endpoint.
type SessionJourney struct {
	SessionID string    `json:"session_id"`
	Model     string    `json:"model,omitempty"`
	CWD       string    `json:"cwd,omitempty"`
	GitBranch string    `json:"git_branch,omitempty"`
	StartTime time.Time `json:"start_time"`
	EndTime   time.Time `json:"end_time"`
	// TotalDuration is the raw start-to-end span; ActiveDuration caps every
	// inter-event gap at IdleGapThreshold and is what the header should show —
	// a resumed session's span contains every idle day between sittings.
	TotalDuration  int64 `json:"total_duration_ms"`
	ActiveDuration int64 `json:"active_duration_ms"`
	TotalTurns     int   `json:"total_turns"`
	// Usage is main-thread only, exactly as ClaudeSessionSummary.Usage is.
	Usage TokenUsage `json:"usage"`
	// SubagentUsage is the summed usage of every sub-agent transcript this
	// journey rendered, and SubagentCount how many there were.
	//
	// They are reported alongside Usage rather than folded into it, for the
	// reason the session summary keeps the same split: "this session's tokens"
	// and "the model that spent them" are different questions. But the journey
	// header used to show Usage alone while the sessions list showed the sum, so
	// one session reported 784K output on one page and 1.8M on another with
	// nothing to explain the gap. Shipping both lets the header state the total
	// *and* say how much of it was delegated.
	SubagentUsage TokenUsage    `json:"subagent_usage"`
	SubagentCount int           `json:"subagent_count"`
	Summary       string        `json:"summary,omitempty"`
	Turns         []JourneyTurn `json:"turns"`
}

// TotalUsage is Usage plus every sub-agent's, the figure the sessions list and
// the analytics totals report for this session.
func (j SessionJourney) TotalUsage() TokenUsage {
	return TokenUsage{
		InputTokens:           j.Usage.InputTokens + j.SubagentUsage.InputTokens,
		OutputTokens:          j.Usage.OutputTokens + j.SubagentUsage.OutputTokens,
		CacheCreationTokens:   j.Usage.CacheCreationTokens + j.SubagentUsage.CacheCreationTokens,
		CacheCreation5mTokens: j.Usage.CacheCreation5mTokens + j.SubagentUsage.CacheCreation5mTokens,
		CacheCreation1hTokens: j.Usage.CacheCreation1hTokens + j.SubagentUsage.CacheCreation1hTokens,
		CacheReadTokens:       j.Usage.CacheReadTokens + j.SubagentUsage.CacheReadTokens,
	}
}

// JourneyTurn groups steps that belong to one user→assistant interaction cycle.
type JourneyTurn struct {
	Number     int           `json:"number"`
	StartTime  time.Time     `json:"start_time"`
	EndTime    time.Time     `json:"end_time"`
	DurationMs int64         `json:"duration_ms"`
	Usage      *TokenUsage   `json:"usage,omitempty"`
	ToolCalls  int           `json:"tool_calls"`
	Steps      []JourneyStep `json:"steps"`
}

// JourneyStep is one discrete event within a turn.
type JourneyStep struct {
	Type       string          `json:"type"`
	Timestamp  time.Time       `json:"timestamp"`
	DurationMs int64           `json:"duration_ms,omitempty"`
	Data       json.RawMessage `json:"data"`
	// Steps holds nested steps for a step that spawned a sub-agent — a Task
	// tool_use whose id matches a sub-agent transcript. Only one level is
	// nested; deeper delegation is flattened. Empty for every other step, so it
	// adds nothing to a session that delegated no work.
	Steps []JourneyStep `json:"steps,omitempty"`
}

// Step data types — serialized into JourneyStep.Data.

// UserInputData is the data for a user_input step.
type UserInputData struct {
	Content string `json:"content"`
}

// ThinkingData is the data for a thinking step.
type ThinkingData struct {
	Preview string `json:"preview"`
	Full    string `json:"full"`
}

// TextResponseData is the data for a text_response step.
type TextResponseData struct {
	Content string `json:"content"`
}

// ToolCallData is the data for a tool_call step.
type ToolCallData struct {
	ToolUseID string          `json:"tool_use_id"`
	ToolName  string          `json:"tool_name"`
	Input     json.RawMessage `json:"input,omitempty"`
	// AgentType, Description and AgentUsage are set only when this tool call
	// spawned a sub-agent whose transcript is nested under this step (Steps).
	AgentType   string      `json:"agent_type,omitempty"`
	Description string      `json:"description,omitempty"`
	AgentUsage  *TokenUsage `json:"agent_usage,omitempty"`
}

// ToolResultData is the data for a tool_result step.
type ToolResultData struct {
	ToolUseID string `json:"tool_use_id"`
	Content   string `json:"content"`
	IsError   bool   `json:"is_error"`
}

// ThinkingDurationData is the data for a thinking_duration step.
type ThinkingDurationData struct {
	DurationMs int64 `json:"duration_ms"`
}

// SubAgentData is the data for a sub_agent step: a delegated agent whose work
// is nested under this step (Steps). It is also used for sub-agents whose
// originating tool_use is not in the rendered transcript — those are appended
// at the end of their turn rather than silently dropped.
type SubAgentData struct {
	AgentID     string      `json:"agent_id,omitempty"`
	AgentType   string      `json:"agent_type,omitempty"`
	Description string      `json:"description,omitempty"`
	Usage       *TokenUsage `json:"usage,omitempty"`
}

// CompactionData is the data for a compaction step: the point where the
// conversation was summarized to fit the context window. Trigger is "auto" or
// "manual". DroppedTokens is what THIS compaction dropped, derived from
// pre/post — unlike ClaudeSessionSummary.DroppedTokens, which is the session's
// running total.
type CompactionData struct {
	Trigger       string `json:"trigger,omitempty"`
	PreTokens     int    `json:"pre_tokens,omitempty"`
	PostTokens    int    `json:"post_tokens,omitempty"`
	DroppedTokens int    `json:"dropped_tokens,omitempty"`
}

// ── Internal raw types for JSONL fields not covered by scanner.go ───────────

// rawToolResultBlock represents a tool_result content block inside a user message.
type rawToolResultBlock struct {
	Type      string `json:"type"`
	ToolUseID string `json:"tool_use_id"`
	Content   string `json:"content"`
	IsError   bool   `json:"is_error"`
}

// rawJourneyEvent extends rawEvent with extra fields we need for journey parsing.
type rawJourneyEvent struct {
	Type        string          `json:"type"`
	Subtype     string          `json:"subtype,omitempty"`
	UUID        string          `json:"uuid"`
	ParentUUID  string          `json:"parentUuid"`
	SessionID   string          `json:"sessionId"`
	Timestamp   time.Time       `json:"timestamp"`
	CWD         string          `json:"cwd"`
	GitBranch   string          `json:"gitBranch"`
	IsSidechain bool            `json:"isSidechain"`
	DurationMs  int64           `json:"durationMs,omitempty"`
	Message     *rawMessage     `json:"message,omitempty"`
	Data        json.RawMessage `json:"data,omitempty"`

	// CompactMetadata is present on system events with subtype compact_boundary.
	CompactMetadata *rawCompactMetadata `json:"compactMetadata,omitempty"`
}

// ── Journey builder ─────────────────────────────────────────────────────────

// validSessionID matches the UUID format used by Claude Code session IDs.
var validSessionID = regexp.MustCompile(`^[a-zA-Z0-9_-]+$`)

// GetSessionJourney reads a session's JSONL file and produces a structured journey.
func GetSessionJourney(sessionID string, logger *slog.Logger) (*SessionJourney, error) {
	if !validSessionID.MatchString(sessionID) {
		return nil, fmt.Errorf("invalid session ID format: %q", sessionID)
	}
	projectsDir := filepath.Join(ClaudeHome(), "projects")
	entries, rdErr := os.ReadDir(projectsDir)
	if rdErr != nil {
		if os.IsNotExist(rdErr) {
			return nil, nil
		}
		return nil, rdErr
	}
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		filePath := filepath.Join(projectsDir, e.Name(), sessionID+jsonlExt)
		if _, err := os.Stat(filePath); err == nil {
			return buildJourney(sessionID, filePath, logger)
		}
	}
	return nil, nil
}

func buildJourney(sessionID, filePath string, logger *slog.Logger) (*SessionJourney, error) {
	f, err := os.Open(filePath) //nolint:gosec
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := f.Close(); cerr != nil {
			logger.Warn("failed to close file", "file", filePath, "error", cerr)
		}
	}()

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 4*1024*1024), 4*1024*1024)

	journey := &SessionJourney{SessionID: sessionID}
	var builder journeyBuilder
	builder.logger = logger
	builder.loadSubagents(sessionID, filePath)

	for sc.Scan() {
		var ev rawJourneyEvent
		if json.Unmarshal(sc.Bytes(), &ev) != nil {
			continue
		}
		// Skip file-history-snapshot events — they can be very large (full file contents)
		// and carry no useful journey information. All other unrecognized types are
		// safely ignored by the switch in processEvent.
		if ev.Type == "file-history-snapshot" {
			continue
		}
		builder.processEvent(ev, journey)
	}

	builder.finalize(journey)

	if journey.StartTime.IsZero() {
		return nil, nil
	}
	return journey, sc.Err()
}

// journeyBuilder accumulates state while scanning events.
type journeyBuilder struct {
	tr timeRange
	// active feeds ActiveDuration. Sub-agent builders merge their stamps into
	// the parent's, so delegated work fills the parent's Task wait gaps exactly
	// as it does in the insight pipeline.
	active        activeTimeTracker
	currentTurn   *JourneyTurn
	turns         []JourneyTurn
	turnNumber    int
	turnUsage     TokenUsage
	turnToolCalls int

	// subagents indexes the session's delegated sub-agents by the tool_use id
	// that spawned them; subagentList keeps every entry so unmatched ones can
	// still be surfaced. Both are empty for a session that delegated nothing.
	subagents    map[string]*subagentEntry
	subagentList []*subagentEntry
	logger       *slog.Logger
	// subagentUsage and subagentCount tally delegated work as each sub-agent
	// transcript is read, so the journey can report it separately from the main
	// thread's rather than under-reporting the session's real total.
	subagentUsage TokenUsage
	subagentCount int

	// subagentMode is set when building a sub-agent's own steps: its transcript
	// carries isSidechain on every event, which the parent builder skips.
	subagentMode bool
}

// subagentEntry pairs a sub-agent transcript path with the metadata read from
// its sidecar, and tracks whether a tool_use block has claimed it.
type subagentEntry struct {
	agentID     string
	agentType   string
	description string
	filePath    string
	matched     bool
}

func (b *journeyBuilder) processEvent(ev rawJourneyEvent, j *SessionJourney) {
	b.tr.update(ev.Timestamp)
	b.active.observe(ev.Timestamp, ev.Type == "assistant")
	if j.CWD == "" && ev.CWD != "" {
		j.CWD = ev.CWD
	}
	if j.GitBranch == "" && ev.GitBranch != "" {
		j.GitBranch = ev.GitBranch
	}

	switch ev.Type {
	case "user":
		b.processUserEvent(ev, j)
	case "assistant":
		b.processAssistantEvent(ev, j)
	case "system":
		b.processSystemEvent(ev)
	}
}

func (b *journeyBuilder) startNewTurn(ts time.Time) {
	if b.currentTurn != nil {
		b.closeTurn()
	}
	b.turnNumber++
	b.currentTurn = &JourneyTurn{
		Number:    b.turnNumber,
		StartTime: ts,
	}
	b.turnUsage = TokenUsage{}
	b.turnToolCalls = 0
}

func (b *journeyBuilder) closeTurn() {
	if b.currentTurn == nil {
		return
	}
	if len(b.currentTurn.Steps) > 0 {
		lastStep := b.currentTurn.Steps[len(b.currentTurn.Steps)-1]
		if b.currentTurn.EndTime.IsZero() || lastStep.Timestamp.After(b.currentTurn.EndTime) {
			b.currentTurn.EndTime = lastStep.Timestamp
		}
	}
	if b.currentTurn.EndTime.IsZero() {
		b.currentTurn.EndTime = b.currentTurn.StartTime
	}
	b.currentTurn.DurationMs = b.currentTurn.EndTime.Sub(b.currentTurn.StartTime).Milliseconds()
	b.currentTurn.ToolCalls = b.turnToolCalls
	if b.turnUsage.InputTokens > 0 || b.turnUsage.OutputTokens > 0 {
		u := b.turnUsage
		b.currentTurn.Usage = &u
	}
	b.turns = append(b.turns, *b.currentTurn)
	b.currentTurn = nil
}

func (b *journeyBuilder) ensureTurn(ts time.Time) {
	if b.currentTurn == nil {
		b.startNewTurn(ts)
	}
}

func (b *journeyBuilder) addStep(step JourneyStep) {
	if b.currentTurn == nil {
		return
	}
	b.currentTurn.Steps = append(b.currentTurn.Steps, step)
	if step.Timestamp.After(b.currentTurn.EndTime) {
		b.currentTurn.EndTime = step.Timestamp
	}
}

func (b *journeyBuilder) processUserEvent(ev rawJourneyEvent, j *SessionJourney) {
	// In the parent transcript, sidechain user turns are echoes of delegated
	// sub-agents and are skipped — those sub-agents are instead nested under
	// the tool_use that spawned them. subagentMode is set only when building a
	// sub-agent's own steps, where every event is sidechain-flagged.
	if ev.IsSidechain && !b.subagentMode {
		return
	}

	// Turn segmentation goes through the shared predicate, so a journey's turns,
	// the session's message_count and the insight pipeline's turn_count all mean
	// the same thing. An event that is not genuine input is either a tool_result
	// carrier or one of Claude Code's injected wrappers: attach whatever
	// tool_result blocks it carries to the enclosing turn, and never open a new
	// one. A wrapper carries no blocks, so it is simply a no-op.
	if !isJourneyTurnStart(ev) {
		b.attachToolResults(ev)
		return
	}

	// Regular user input — starts a new turn
	b.startNewTurn(ev.Timestamp)
	content := ""
	if ev.Message != nil {
		content = extractTextContent(ev.Message.Content)
	}
	if j.Summary == "" && content != "" {
		j.Summary = truncateRunes(content, 200)
	}
	data := UserInputData{Content: content}
	raw, _ := json.Marshal(data) //nolint:errcheck
	b.addStep(JourneyStep{
		Type:      "user_input",
		Timestamp: ev.Timestamp,
		Data:      raw,
	})
}

// isJourneyTurnStart reports whether a user event in a journey is genuine human
// input, and so opens a turn.
//
// It is a thin wrapper over isUserTurnContent — the ONE definition of a user
// turn, shared with the scanner's message_count and the insight pipeline's
// isTurnStart. rawJourneyEvent and ProcessableEvent are different structs, so
// this cannot be isTurnStart itself; the predicate underneath is the point, and
// a change to it moves journey turns along with the other two.
//
// The isSidechain check is deliberately NOT made here, exactly as
// isUserTurnContent leaves it to its callers: the flag means "delegated work,
// skip" in a parent transcript but is set on every event of a sub-agent
// transcript, where it carries no such meaning. processUserEvent handles it.
func isJourneyTurnStart(ev rawJourneyEvent) bool {
	if ev.Message == nil {
		return false
	}
	return isUserTurnContent(ev.Message.Content)
}

// attachToolResults decodes the tool_result blocks of a user event and adds
// them as steps of the enclosing turn.
//
// It is deliberately NOT the turn-start test — that is isJourneyTurnStart, over
// the shared predicate. Because the decision no longer depends on this decode,
// every block is inspected rather than only the first: a carrier whose
// tool_result is not in first position still has its results attached, and no
// longer opens a turn the session's own message_count does not count.
func (b *journeyBuilder) attachToolResults(ev rawJourneyEvent) {
	if ev.Message == nil || len(ev.Message.Content) == 0 {
		return
	}
	var blocks []rawToolResultBlock
	if json.Unmarshal(ev.Message.Content, &blocks) != nil {
		return
	}
	for _, blk := range blocks {
		if blk.Type != "tool_result" {
			continue
		}
		// ensureTurn is load-bearing and sits inside the loop on purpose: a
		// transcript can open with a tool-result carrier and those steps must
		// still land somewhere, but an event carrying no tool_result at all —
		// an injected wrapper — must not conjure a turn out of nothing.
		b.ensureTurn(ev.Timestamp)
		data := ToolResultData{
			ToolUseID: blk.ToolUseID,
			Content:   truncateRunes(blk.Content, 2000),
			IsError:   blk.IsError,
		}
		raw, _ := json.Marshal(data) //nolint:errcheck
		b.addStep(JourneyStep{
			Type:      "tool_result",
			Timestamp: ev.Timestamp,
			Data:      raw,
		})
	}
}

func (b *journeyBuilder) processAssistantEvent(ev rawJourneyEvent, j *SessionJourney) {
	b.ensureTurn(ev.Timestamp)

	if ev.Message == nil {
		return
	}
	if j.Model == "" && ev.Message.Model != "" {
		j.Model = ev.Message.Model
	}

	b.accumulateUsage(ev.Message.Usage, j)

	// Parse content blocks
	var blocks []rawContentBlock
	if json.Unmarshal(ev.Message.Content, &blocks) != nil {
		return
	}

	for _, blk := range blocks {
		b.processContentBlock(blk, ev.Timestamp)
	}
}

// accumulateUsage adds message usage to both turn-level and session-level totals.
func (b *journeyBuilder) accumulateUsage(usage *rawUsage, j *SessionJourney) {
	if usage == nil {
		return
	}
	b.turnUsage.InputTokens += usage.InputTokens
	b.turnUsage.OutputTokens += usage.OutputTokens
	b.turnUsage.CacheCreationTokens += usage.CacheCreationInputTokens
	b.turnUsage.CacheReadTokens += usage.CacheReadInputTokens
	j.Usage.InputTokens += usage.InputTokens
	j.Usage.OutputTokens += usage.OutputTokens
	j.Usage.CacheCreationTokens += usage.CacheCreationInputTokens
	j.Usage.CacheReadTokens += usage.CacheReadInputTokens
}

// processContentBlock converts a single assistant content block into a journey step.
func (b *journeyBuilder) processContentBlock(blk rawContentBlock, ts time.Time) {
	switch blk.Type {
	case "thinking":
		if blk.Thinking == "" {
			return
		}
		data := ThinkingData{
			Preview: truncateRunes(blk.Thinking, 500),
			Full:    truncateRunes(blk.Thinking, 20000),
		}
		raw, _ := json.Marshal(data) //nolint:errcheck
		b.addStep(JourneyStep{Type: "thinking", Timestamp: ts, Data: raw})
	case "text":
		if blk.Text == "" {
			return
		}
		data := TextResponseData{Content: blk.Text}
		raw, _ := json.Marshal(data) //nolint:errcheck
		b.addStep(JourneyStep{Type: "text_response", Timestamp: ts, Data: raw})
	case "tool_use":
		b.turnToolCalls++
		data := ToolCallData{ToolUseID: blk.ID, ToolName: blk.Name, Input: blk.Input}
		step := JourneyStep{Type: "tool_call", Timestamp: ts}
		// A Task tool_use whose id matches a sub-agent transcript nests that
		// agent's own steps here, joined exactly on toolUseId.
		if entry, ok := b.subagents[blk.ID]; ok {
			entry.matched = true
			data.AgentType = entry.agentType
			data.Description = entry.description
			steps, usage := b.buildSubagentSteps(entry)
			step.Steps = steps
			if usage.InputTokens > 0 || usage.OutputTokens > 0 {
				u := usage
				data.AgentUsage = &u
			}
		}
		raw, _ := json.Marshal(data) //nolint:errcheck
		step.Data = raw
		b.addStep(step)
	}
}

func (b *journeyBuilder) processSystemEvent(ev rawJourneyEvent) {
	if ev.Subtype == "compact_boundary" {
		b.addCompactionStep(ev)
		return
	}
	if ev.Subtype != "turn_duration" {
		return
	}
	b.ensureTurn(ev.Timestamp)

	data := ThinkingDurationData{DurationMs: ev.DurationMs}
	raw, _ := json.Marshal(data) //nolint:errcheck
	b.addStep(JourneyStep{
		Type:       "thinking_duration",
		Timestamp:  ev.Timestamp,
		DurationMs: ev.DurationMs,
		Data:       raw,
	})

	// Update the turn end time with the system event's timestamp
	if b.currentTurn != nil && ev.Timestamp.After(b.currentTurn.EndTime) {
		b.currentTurn.EndTime = ev.Timestamp
	}
}

// addCompactionStep records where the conversation was compacted. It reads as a
// divider in the timeline: everything before it was summarized away, which is
// usually the explanation for a sudden drop in context the user is looking for.
func (b *journeyBuilder) addCompactionStep(ev rawJourneyEvent) {
	if ev.CompactMetadata == nil {
		return
	}
	b.ensureTurn(ev.Timestamp)

	data := CompactionData{
		Trigger:    ev.CompactMetadata.Trigger,
		PreTokens:  ev.CompactMetadata.PreTokens,
		PostTokens: ev.CompactMetadata.PostTokens,
	}
	if dropped := ev.CompactMetadata.PreTokens - ev.CompactMetadata.PostTokens; dropped > 0 {
		data.DroppedTokens = dropped
	}
	raw, _ := json.Marshal(data) //nolint:errcheck
	b.addStep(JourneyStep{
		Type:       "compaction",
		Timestamp:  ev.Timestamp,
		DurationMs: ev.CompactMetadata.DurationMs,
		Data:       raw,
	})
}

func (b *journeyBuilder) finalize(j *SessionJourney) {
	if b.currentTurn != nil {
		b.closeTurn()
	}
	// Sub-agents whose tool_use isn't in the rendered transcript (it was
	// compacted away, or the sidecar had no toolUseId) are still surfaced,
	// appended to the last turn — never silently dropped.
	b.appendUnmatchedSubagents()

	j.StartTime = b.tr.start
	j.EndTime = b.tr.last
	if !j.StartTime.IsZero() && !j.EndTime.IsZero() {
		j.TotalDuration = j.EndTime.Sub(j.StartTime).Milliseconds()
	}
	j.ActiveDuration, _ = b.active.durations()
	j.TotalTurns = len(b.turns)

	for i := range b.turns {
		computeStepDurations(&b.turns[i])
	}

	j.Turns = b.turns
	if j.Turns == nil {
		j.Turns = []JourneyTurn{}
	}

	j.SubagentUsage = b.subagentUsage
	j.SubagentCount = b.subagentCount

	if j.Summary == "" {
		j.Summary = extractFirstUserInput(j.Turns)
	}
}

// extractFirstUserInput finds the first user_input step text from the turns.
func extractFirstUserInput(turns []JourneyTurn) string {
	if len(turns) == 0 {
		return ""
	}
	for _, step := range turns[0].Steps {
		if step.Type == "user_input" {
			var d UserInputData
			if json.Unmarshal(step.Data, &d) == nil {
				return truncateRunes(d.Content, 200)
			}
		}
	}
	return ""
}

// computeStepDurations estimates duration of each step from the gap between consecutive timestamps.
func computeStepDurations(turn *JourneyTurn) {
	steps := turn.Steps
	for i := range steps {
		// Skip steps that already have a duration set (e.g. thinking_duration)
		if steps[i].DurationMs > 0 {
			continue
		}
		if i+1 < len(steps) {
			steps[i].DurationMs = steps[i+1].Timestamp.Sub(steps[i].Timestamp).Milliseconds()
			if steps[i].DurationMs < 0 {
				steps[i].DurationMs = 0
			}
		} else {
			// Last step — use turn end time
			steps[i].DurationMs = turn.EndTime.Sub(steps[i].Timestamp).Milliseconds()
			if steps[i].DurationMs < 0 {
				steps[i].DurationMs = 0
			}
		}
	}
}

// ── Sub-agent nesting ───────────────────────────────────────────────────────

// loadSubagents reads the session's delegated sub-agent transcripts from disk
// and indexes them by the tool_use id that spawned each one. A session with no
// subagents/ directory contributes nothing, leaving the journey unchanged.
func (b *journeyBuilder) loadSubagents(sessionID, filePath string) {
	b.subagents = make(map[string]*subagentEntry)
	for _, fp := range SubagentFiles(sessionID, filePath) {
		meta := readSubagentMeta(fp, b.logger)
		entry := &subagentEntry{
			agentID:     strings.TrimSuffix(filepath.Base(fp), jsonlExt),
			agentType:   meta.AgentType,
			description: meta.Description,
			filePath:    fp,
		}
		b.subagentList = append(b.subagentList, entry)
		// Index by tool_use id; on collision the first-seen wins, matching the
		// cache's deterministic start-time ordering.
		if meta.ToolUseID != "" {
			if _, exists := b.subagents[meta.ToolUseID]; !exists {
				b.subagents[meta.ToolUseID] = entry
			}
		}
	}
}

// buildSubagentSteps runs a journey pass over one sub-agent transcript and
// returns its steps (flattened across turns) and total usage. It does not
// recurse into the sub-agent's own subagents/ — deeper delegation is flattened.
func (b *journeyBuilder) buildSubagentSteps(e *subagentEntry) ([]JourneyStep, TokenUsage) {
	f, err := os.Open(e.filePath) //nolint:gosec
	if err != nil {
		return nil, TokenUsage{}
	}
	defer func() {
		if cerr := f.Close(); cerr != nil {
			b.logger.Warn("failed to close sub-agent file", "file", e.filePath, "error", cerr)
		}
	}()

	sub := &SessionJourney{}
	var sb journeyBuilder
	sb.logger = b.logger
	sb.subagentMode = true
	sb.subagents = map[string]*subagentEntry{}

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 4*1024*1024), 4*1024*1024)
	for sc.Scan() {
		var ev rawJourneyEvent
		if json.Unmarshal(sc.Bytes(), &ev) != nil {
			continue
		}
		if ev.Type == "file-history-snapshot" {
			continue
		}
		sb.processEvent(ev, sub)
	}
	sb.finalize(sub)

	var steps []JourneyStep
	for _, t := range sub.Turns {
		steps = append(steps, t.Steps...)
	}

	// Tallied here rather than at each call site: this is the one place a
	// sub-agent's usage is computed, and both the matched (nested under its
	// Task tool_use) and unmatched (appended to the last turn) paths reach it.
	// The sub-agent's timestamps merge into the parent's active tracker for the
	// same reason.
	b.active.stamps = append(b.active.stamps, sb.active.stamps...)
	b.subagentCount++
	b.subagentUsage.InputTokens += sub.Usage.InputTokens
	b.subagentUsage.OutputTokens += sub.Usage.OutputTokens
	b.subagentUsage.CacheCreationTokens += sub.Usage.CacheCreationTokens
	b.subagentUsage.CacheReadTokens += sub.Usage.CacheReadTokens

	return steps, sub.Usage
}

// appendUnmatchedSubagents surfaces sub-agents whose tool_use is not in the
// rendered transcript, attaching them to the last turn so delegated work is
// never silently lost. With no turns to attach to, there is nowhere to put them.
func (b *journeyBuilder) appendUnmatchedSubagents() {
	if len(b.subagentList) == 0 || len(b.turns) == 0 {
		return
	}
	last := &b.turns[len(b.turns)-1]
	for _, e := range b.subagentList {
		if e.matched {
			continue
		}
		steps, usage := b.buildSubagentSteps(e)
		data := SubAgentData{
			AgentID:     e.agentID,
			AgentType:   e.agentType,
			Description: e.description,
		}
		if usage.InputTokens > 0 || usage.OutputTokens > 0 {
			u := usage
			data.Usage = &u
		}
		raw, _ := json.Marshal(data) //nolint:errcheck
		stepTimestamp := last.EndTime
		if len(steps) > 0 {
			stepTimestamp = steps[0].Timestamp
		}
		last.Steps = append(last.Steps, JourneyStep{
			Type:      "sub_agent",
			Timestamp: stepTimestamp,
			Data:      raw,
			Steps:     steps,
		})
	}
}
