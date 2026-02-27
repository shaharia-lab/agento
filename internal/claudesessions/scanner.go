package claudesessions

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
	"unicode/utf8"
)

const previewMaxRunes = 120

// rawEvent is the raw JSON structure of a single line in a Claude Code session JSONL file.
type rawEvent struct {
	Type        string      `json:"type"`
	UUID        string      `json:"uuid"`
	ParentUUID  string      `json:"parentUuid"`
	SessionID   string      `json:"sessionId"`
	Timestamp   time.Time   `json:"timestamp"`
	CWD         string      `json:"cwd"`
	Version     string      `json:"version"`
	GitBranch   string      `json:"gitBranch"`
	IsSidechain bool        `json:"isSidechain"`
	Message     *rawMessage `json:"message,omitempty"`
}

type rawMessage struct {
	Role    string          `json:"role"`
	Model   string          `json:"model,omitempty"`
	Content json.RawMessage `json:"content"` // string or array of content blocks
	Usage   *rawUsage       `json:"usage,omitempty"`
}

type rawUsage struct {
	InputTokens              int `json:"input_tokens"`
	OutputTokens             int `json:"output_tokens"`
	CacheCreationInputTokens int `json:"cache_creation_input_tokens"`
	CacheReadInputTokens     int `json:"cache_read_input_tokens"`
}

type rawContentBlock struct {
	Type     string          `json:"type"`
	Text     string          `json:"text,omitempty"`
	Thinking string          `json:"thinking,omitempty"`
	ID       string          `json:"id,omitempty"`
	Name     string          `json:"name,omitempty"`
	Input    json.RawMessage `json:"input,omitempty"`
}

// ClaudeHome returns the path to the user's ~/.claude directory.
func ClaudeHome() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return filepath.Join("/root", ".claude")
	}
	return filepath.Join(home, ".claude")
}

// DecodeProjectPath converts an encoded Claude Code directory name to the original path.
// Claude Code encodes paths by replacing '/' separators with '-' and prepending '-'.
// e.g. "-home-user-Projects-foo" → "/home/user/Projects/foo"
//
// Note: this encoding is ambiguous when directory names contain hyphens, but it
// matches what Claude Code itself uses, so decoded paths may differ in that edge case.
func DecodeProjectPath(encoded string) string {
	trimmed := strings.TrimPrefix(encoded, "-")
	return "/" + strings.ReplaceAll(trimmed, "-", "/")
}

// ListProjects returns all projects found in ~/.claude/projects/.
func ListProjects() ([]ClaudeProject, error) {
	projectsDir := filepath.Join(ClaudeHome(), "projects")
	entries, err := os.ReadDir(projectsDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}

	projects := make([]ClaudeProject, 0, len(entries))
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		files, _ := os.ReadDir(filepath.Join(projectsDir, e.Name()))
		count := 0
		for _, f := range files {
			if !f.IsDir() && strings.HasSuffix(f.Name(), ".jsonl") {
				count++
			}
		}
		projects = append(projects, ClaudeProject{
			EncodedName:  e.Name(),
			DecodedPath:  DecodeProjectPath(e.Name()),
			SessionCount: count,
		})
	}
	sort.Slice(projects, func(i, j int) bool {
		return projects[i].DecodedPath < projects[j].DecodedPath
	})
	return projects, nil
}

// ScanAllSessions scans all project directories and returns summaries for all sessions.
// Sessions are sorted by last activity, most recent first.
func ScanAllSessions() ([]ClaudeSessionSummary, error) {
	projectsDir := filepath.Join(ClaudeHome(), "projects")
	entries, err := os.ReadDir(projectsDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}

	var sessions []ClaudeSessionSummary
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		projectPath := DecodeProjectPath(e.Name())
		files, err := os.ReadDir(filepath.Join(projectsDir, e.Name()))
		if err != nil {
			continue
		}
		for _, f := range files {
			if f.IsDir() || !strings.HasSuffix(f.Name(), ".jsonl") {
				continue
			}
			sessionID := strings.TrimSuffix(f.Name(), ".jsonl")
			filePath := filepath.Join(projectsDir, e.Name(), f.Name())
			summary, err := readSessionSummary(sessionID, projectPath, filePath)
			if err != nil || summary == nil {
				continue
			}
			sessions = append(sessions, *summary)
		}
	}

	sort.Slice(sessions, func(i, j int) bool {
		return sessions[i].LastActivity.After(sessions[j].LastActivity)
	})
	return sessions, nil
}

// readSessionSummary reads a session JSONL file and extracts lightweight metadata.
func readSessionSummary(sessionID, projectPath, filePath string) (*ClaudeSessionSummary, error) {
	f, err := os.Open(filePath) //nolint:gosec
	if err != nil {
		return nil, err
	}
	defer func() { _ = f.Close() }()

	summary := &ClaudeSessionSummary{
		SessionID:   sessionID,
		ProjectPath: projectPath,
	}

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 4*1024*1024), 4*1024*1024)

	for sc.Scan() {
		var ev rawEvent
		if err := json.Unmarshal(sc.Bytes(), &ev); err != nil {
			continue
		}
		if ev.Type == "file-history-snapshot" {
			continue
		}

		ts := ev.Timestamp
		if !ts.IsZero() {
			if summary.StartTime.IsZero() || ts.Before(summary.StartTime) {
				summary.StartTime = ts
			}
			if ts.After(summary.LastActivity) {
				summary.LastActivity = ts
			}
		}

		if summary.CWD == "" && ev.CWD != "" {
			summary.CWD = ev.CWD
		}
		if summary.GitBranch == "" && ev.GitBranch != "" {
			summary.GitBranch = ev.GitBranch
		}

		switch ev.Type {
		case "user":
			if ev.IsSidechain {
				continue
			}
			summary.MessageCount++
			if summary.Preview == "" && ev.Message != nil {
				summary.Preview = truncateRunes(extractTextContent(ev.Message.Content), previewMaxRunes)
			}

		case "assistant":
			summary.MessageCount++
			if ev.Message != nil {
				if summary.Model == "" && ev.Message.Model != "" {
					summary.Model = ev.Message.Model
				}
				if ev.Message.Usage != nil {
					summary.Usage.InputTokens += ev.Message.Usage.InputTokens
					summary.Usage.OutputTokens += ev.Message.Usage.OutputTokens
					summary.Usage.CacheCreationTokens += ev.Message.Usage.CacheCreationInputTokens
					summary.Usage.CacheReadTokens += ev.Message.Usage.CacheReadInputTokens
				}
			}
		}
	}

	if summary.StartTime.IsZero() {
		return nil, nil // empty or unreadable file
	}
	return summary, sc.Err()
}

// GetSessionDetail reads the full session JSONL and builds the complete message list.
// Returns nil if the session is not found.
func GetSessionDetail(sessionID string) (*ClaudeSessionDetail, error) {
	projectsDir := filepath.Join(ClaudeHome(), "projects")
	entries, _ := os.ReadDir(projectsDir)
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		filePath := filepath.Join(projectsDir, e.Name(), sessionID+".jsonl")
		if _, err := os.Stat(filePath); err == nil {
			projectPath := DecodeProjectPath(e.Name())
			return readSessionDetail(sessionID, projectPath, filePath)
		}
	}
	return nil, nil
}

// readSessionDetail reads a session JSONL file and builds the full detail including
// message tree with progress events nested under their parent assistant turns.
func readSessionDetail(sessionID, projectPath, filePath string) (*ClaudeSessionDetail, error) {
	f, err := os.Open(filePath) //nolint:gosec
	if err != nil {
		return nil, err
	}
	defer func() { _ = f.Close() }()

	detail := &ClaudeSessionDetail{}
	detail.SessionID = sessionID
	detail.ProjectPath = projectPath

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 4*1024*1024), 4*1024*1024)

	var topLevel []ClaudeMessage
	progressByParentUUID := make(map[string][]ClaudeMessage)

	for sc.Scan() {
		var ev rawEvent
		if err := json.Unmarshal(sc.Bytes(), &ev); err != nil {
			continue
		}
		if ev.Type == "file-history-snapshot" {
			continue
		}

		ts := ev.Timestamp
		if !ts.IsZero() {
			if detail.StartTime.IsZero() || ts.Before(detail.StartTime) {
				detail.StartTime = ts
			}
			if ts.After(detail.LastActivity) {
				detail.LastActivity = ts
			}
		}

		if detail.CWD == "" && ev.CWD != "" {
			detail.CWD = ev.CWD
		}
		if detail.GitBranch == "" && ev.GitBranch != "" {
			detail.GitBranch = ev.GitBranch
		}

		switch ev.Type {
		case "user":
			if ev.IsSidechain {
				continue
			}
			content := ""
			if ev.Message != nil {
				content = extractTextContent(ev.Message.Content)
			}
			msg := ClaudeMessage{
				UUID:       ev.UUID,
				ParentUUID: ev.ParentUUID,
				Type:       "user",
				Timestamp:  ev.Timestamp,
				Role:       "user",
				Content:    content,
				GitBranch:  ev.GitBranch,
			}
			detail.MessageCount++
			topLevel = append(topLevel, msg)

		case "assistant":
			msg := ClaudeMessage{
				UUID:       ev.UUID,
				ParentUUID: ev.ParentUUID,
				Type:       "assistant",
				Timestamp:  ev.Timestamp,
				Role:       "assistant",
				GitBranch:  ev.GitBranch,
			}
			if ev.Message != nil {
				if detail.Model == "" && ev.Message.Model != "" {
					detail.Model = ev.Message.Model
				}
				if ev.Message.Usage != nil {
					u := TokenUsage{
						InputTokens:         ev.Message.Usage.InputTokens,
						OutputTokens:        ev.Message.Usage.OutputTokens,
						CacheCreationTokens: ev.Message.Usage.CacheCreationInputTokens,
						CacheReadTokens:     ev.Message.Usage.CacheReadInputTokens,
					}
					msg.Usage = &u
					detail.Usage.InputTokens += u.InputTokens
					detail.Usage.OutputTokens += u.OutputTokens
					detail.Usage.CacheCreationTokens += u.CacheCreationTokens
					detail.Usage.CacheReadTokens += u.CacheReadTokens
				}
				// Parse and normalize content blocks.
				var blocks []rawContentBlock
				if err := json.Unmarshal(ev.Message.Content, &blocks); err == nil {
					for _, b := range blocks {
						if nb := normalizeBlock(b); nb.Type != "" {
							msg.Blocks = append(msg.Blocks, nb)
						}
					}
				}
			}
			detail.MessageCount++
			topLevel = append(topLevel, msg)

		case "progress":
			// Group progress events under their parent by UUID. We key on
			// ParentUUID which is the UUID of the assistant message that
			// spawned this sub-agent call.
			if ev.ParentUUID == "" {
				continue
			}
			progressByParentUUID[ev.ParentUUID] = append(progressByParentUUID[ev.ParentUUID], ClaudeMessage{
				UUID:        ev.UUID,
				ParentUUID:  ev.ParentUUID,
				Type:        "progress",
				Timestamp:   ev.Timestamp,
				IsSidechain: ev.IsSidechain,
			})
		}
	}

	// Attach collected progress children to their parent top-level messages.
	for i := range topLevel {
		if children, ok := progressByParentUUID[topLevel[i].UUID]; ok {
			topLevel[i].Children = children
		}
	}

	detail.Messages = topLevel
	if detail.Messages == nil {
		detail.Messages = []ClaudeMessage{}
	}

	detail.Todos = loadTodos(sessionID)
	if detail.Todos == nil {
		detail.Todos = []ClaudeTodo{}
	}

	// Derive preview from first user message.
	for _, msg := range detail.Messages {
		if msg.Role == "user" && msg.Content != "" {
			detail.Preview = truncateRunes(msg.Content, previewMaxRunes)
			break
		}
	}

	return detail, sc.Err()
}

// normalizeBlock converts a raw Claude Code content block to Agento's NormalizedBlock format.
// Thinking blocks use the "text" field to match Agento's stored MessageBlock format.
func normalizeBlock(b rawContentBlock) NormalizedBlock {
	switch b.Type {
	case "thinking":
		return NormalizedBlock{Type: "thinking", Text: b.Thinking}
	case "text":
		return NormalizedBlock{Type: "text", Text: b.Text}
	case "tool_use":
		return NormalizedBlock{Type: "tool_use", ID: b.ID, Name: b.Name, Input: b.Input}
	default:
		return NormalizedBlock{} // unknown type — skip
	}
}

// extractTextContent extracts plain text from a Claude Code message content field,
// which may be either a JSON string or an array of content blocks.
func extractTextContent(raw json.RawMessage) string {
	if len(raw) == 0 {
		return ""
	}
	// Try plain string first.
	var s string
	if err := json.Unmarshal(raw, &s); err == nil {
		return s
	}
	// Try array of content blocks; concatenate text blocks.
	var blocks []rawContentBlock
	if err := json.Unmarshal(raw, &blocks); err != nil {
		return ""
	}
	var sb strings.Builder
	for _, b := range blocks {
		if b.Type == "text" && b.Text != "" {
			if sb.Len() > 0 {
				sb.WriteString("\n")
			}
			sb.WriteString(b.Text)
		}
	}
	return sb.String()
}

// loadTodos reads the session's todo list from ~/.claude/todos/{id}-agent-{id}.json.
func loadTodos(sessionID string) []ClaudeTodo {
	todoPath := filepath.Join(ClaudeHome(), "todos",
		sessionID+"-agent-"+sessionID+".json")
	data, err := os.ReadFile(todoPath) //nolint:gosec
	if err != nil {
		return nil
	}
	var todos []ClaudeTodo
	if err := json.Unmarshal(data, &todos); err != nil {
		return nil
	}
	return todos
}

// truncateRunes truncates s to at most maxRunes Unicode code points, appending "…" if truncated.
func truncateRunes(s string, maxRunes int) string {
	if utf8.RuneCountInString(s) <= maxRunes {
		return s
	}
	runes := []rune(s)
	return string(runes[:maxRunes]) + "…"
}
