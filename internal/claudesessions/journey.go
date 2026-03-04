package claudesessions

import (
	"bufio"
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"time"
)

// ── Journey response types ──────────────────────────────────────────────────

// SessionJourney is the top-level response for the journey endpoint.
type SessionJourney struct {
	SessionID     string        `json:"session_id"`
	Model         string        `json:"model,omitempty"`
	CWD           string        `json:"cwd,omitempty"`
	GitBranch     string        `json:"git_branch,omitempty"`
	StartTime     time.Time     `json:"start_time"`
	EndTime       time.Time     `json:"end_time"`
	TotalDuration int64         `json:"total_duration_ms"`
	TotalTurns    int           `json:"total_turns"`
	Usage         TokenUsage    `json:"usage"`
	Summary       string        `json:"summary,omitempty"`
	Turns         []JourneyTurn `json:"turns"`
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
}

// ToolResultData is the data for a tool_result step.
type ToolResultData struct {
	ToolUseID string `json:"tool_use_id"`
	Content   string `json:"content"`
	IsError   bool   `json:"is_error"`
}

// BashOutputData is the data for a bash_output step.
type BashOutputData struct {
	Output string `json:"output"`
}

// ThinkingDurationData is the data for a thinking_duration step.
type ThinkingDurationData struct {
	DurationMs int64 `json:"duration_ms"`
}

// SubAgentData is the data for a sub_agent step.
type SubAgentData struct {
	AgentID string `json:"agent_id,omitempty"`
	Prompt  string `json:"prompt,omitempty"`
}

// SkillData is the data for a skill step.
type SkillData struct {
	Prompt string `json:"prompt,omitempty"`
}

// MCPToolData is the data for an mcp_tool step.
type MCPToolData struct {
	ServerName string `json:"server_name,omitempty"`
	ToolName   string `json:"tool_name,omitempty"`
	Status     string `json:"status,omitempty"`
}

// ── Internal raw types for JSONL fields not covered by scanner.go ───────────

// rawToolResultBlock represents a tool_result content block inside a user message.
type rawToolResultBlock struct {
	Type      string `json:"type"`
	ToolUseID string `json:"tool_use_id"`
	Content   string `json:"content"`
	IsError   bool   `json:"is_error"`
}

// rawProgressData is the data field in a progress event.
type rawProgressData struct {
	Type       string `json:"type"`
	Output     string `json:"output,omitempty"`
	FullOutput string `json:"fullOutput,omitempty"`
	AgentID    string `json:"agentId,omitempty"`
	Prompt     string `json:"prompt,omitempty"`
	ServerName string `json:"serverName,omitempty"`
	ToolName   string `json:"toolName,omitempty"`
	Status     string `json:"status,omitempty"`
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
}

// ── Journey builder ─────────────────────────────────────────────────────────

// GetSessionJourney reads a session's JSONL file and produces a structured journey.
func GetSessionJourney(sessionID string, logger *slog.Logger) (*SessionJourney, error) {
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

	for sc.Scan() {
		var ev rawJourneyEvent
		if json.Unmarshal(sc.Bytes(), &ev) != nil {
			continue
		}
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
	tr            timeRange
	currentTurn   *JourneyTurn
	turns         []JourneyTurn
	turnNumber    int
	turnUsage     TokenUsage
	turnToolCalls int
}

func (b *journeyBuilder) processEvent(ev rawJourneyEvent, j *SessionJourney) {
	b.tr.update(ev.Timestamp)
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
	case "progress":
		b.processProgressEvent(ev)
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
	if ev.IsSidechain {
		return
	}

	// Check if this is a tool_result event
	if b.tryProcessToolResults(ev) {
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

// tryProcessToolResults checks if the user event contains tool_result blocks and adds them as steps.
// Returns true if tool results were found and processed.
func (b *journeyBuilder) tryProcessToolResults(ev rawJourneyEvent) bool {
	if ev.Message == nil || len(ev.Message.Content) == 0 {
		return false
	}
	var blocks []rawToolResultBlock
	if json.Unmarshal(ev.Message.Content, &blocks) != nil || len(blocks) == 0 || blocks[0].Type != "tool_result" {
		return false
	}
	b.ensureTurn(ev.Timestamp)
	for _, blk := range blocks {
		if blk.Type != "tool_result" {
			continue
		}
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
	return true
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
			Full:    blk.Thinking,
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
		raw, _ := json.Marshal(data) //nolint:errcheck
		b.addStep(JourneyStep{Type: "tool_call", Timestamp: ts, Data: raw})
	}
}

func (b *journeyBuilder) processProgressEvent(ev rawJourneyEvent) {
	if len(ev.Data) == 0 {
		return
	}
	b.ensureTurn(ev.Timestamp)

	var pd rawProgressData
	if json.Unmarshal(ev.Data, &pd) != nil {
		return
	}

	switch pd.Type {
	case "bash_progress":
		output := pd.FullOutput
		if output == "" {
			output = pd.Output
		}
		if output == "" {
			return
		}
		data := BashOutputData{Output: truncateRunes(output, 2000)}
		raw, _ := json.Marshal(data) //nolint:errcheck
		b.addStep(JourneyStep{
			Type:      "bash_output",
			Timestamp: ev.Timestamp,
			Data:      raw,
		})
	case "agent_progress":
		data := SubAgentData{
			AgentID: pd.AgentID,
			Prompt:  truncateRunes(pd.Prompt, 500),
		}
		raw, _ := json.Marshal(data) //nolint:errcheck
		b.addStep(JourneyStep{
			Type:      "sub_agent",
			Timestamp: ev.Timestamp,
			Data:      raw,
		})
	case "skill_progress":
		data := SkillData{Prompt: truncateRunes(pd.Prompt, 500)}
		raw, _ := json.Marshal(data) //nolint:errcheck
		b.addStep(JourneyStep{
			Type:      "skill",
			Timestamp: ev.Timestamp,
			Data:      raw,
		})
	case "mcp_progress":
		data := MCPToolData{
			ServerName: pd.ServerName,
			ToolName:   pd.ToolName,
			Status:     pd.Status,
		}
		raw, _ := json.Marshal(data) //nolint:errcheck
		b.addStep(JourneyStep{
			Type:      "mcp_tool",
			Timestamp: ev.Timestamp,
			Data:      raw,
		})
	}
}

func (b *journeyBuilder) processSystemEvent(ev rawJourneyEvent) {
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

func (b *journeyBuilder) finalize(j *SessionJourney) {
	if b.currentTurn != nil {
		b.closeTurn()
	}

	j.StartTime = b.tr.start
	j.EndTime = b.tr.last
	if !j.StartTime.IsZero() && !j.EndTime.IsZero() {
		j.TotalDuration = j.EndTime.Sub(j.StartTime).Milliseconds()
	}
	j.TotalTurns = len(b.turns)

	for i := range b.turns {
		computeStepDurations(&b.turns[i])
	}

	j.Turns = b.turns
	if j.Turns == nil {
		j.Turns = []JourneyTurn{}
	}

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
