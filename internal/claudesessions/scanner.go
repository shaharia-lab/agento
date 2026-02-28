package claudesessions

import (
	"bufio"
	"context"
	"database/sql"
	"encoding/json"
	"log/slog"
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

// DecodeProjectPath converts an encoded Claude Code directory name to the original
// filesystem path.
//
// Claude Code encodes project paths for use as directory names by replacing both
// '/' and '.' with '-' and prepending a leading '-'. Because literal hyphens in
// directory names are encoded identically, the mapping is ambiguous.
//
// This function resolves the ambiguity with a greedy filesystem walk: for each
// hyphen-separated token it checks whether the accumulated segment (or its
// dot-prefixed variant, to recover hidden directories like ".claude") forms an
// existing directory, advancing to the next level when it does. This correctly
// decodes the vast majority of real-world project paths (e.g. "homebrew-tap",
// "claude-agent-sdk-go", and worktree paths under ".claude/worktrees/").
//
// If the final resolved path does not exist on the filesystem (deleted projects,
// unresolvable worktrees, etc.) the raw encoded name is returned unchanged so
// callers always have something meaningful to display.
func DecodeProjectPath(encoded string) string {
	trimmed := strings.TrimPrefix(encoded, "-")
	tokens := strings.Split(trimmed, "-")

	currentPath := ""
	currentSegment := ""

	for _, token := range tokens {
		// Skip empty tokens produced by consecutive hyphens (e.g. from "--").
		// They are handled implicitly: the next token continues building the segment,
		// and findExistingDir also checks the dot-prefixed variant which covers
		// hidden directories like ".claude" that Claude Code encodes as "--claude".
		if token == "" {
			continue
		}

		if currentSegment == "" {
			currentSegment = token
		} else {
			currentSegment += "-" + token
		}

		// Greedily advance when the accumulated segment matches an existing directory.
		if next, ok := findExistingDir(currentPath, currentSegment); ok {
			currentPath = next
			currentSegment = ""
		}
	}

	result := currentPath
	if currentSegment != "" {
		result = currentPath + "/" + currentSegment
	}

	// Verify the resolved path exists; return the raw encoded name as fallback.
	if _, err := os.Stat(result); err == nil {
		return result
	}
	return encoded
}

// findExistingDir checks whether parent/segment or parent/.segment is an existing
// directory. The dot-prefix variant recovers hidden directories (e.g. ".claude")
// because Claude Code encodes '.' as '-', the same character used for '/'.
func findExistingDir(parent, segment string) (string, bool) {
	for _, name := range []string{segment, "." + segment} {
		candidate := parent + "/" + name
		if info, err := os.Stat(candidate); err == nil && info.IsDir() {
			return candidate, true
		}
	}
	return "", false
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

// diskFile holds the metadata for a JSONL file found on disk.
type diskFile struct {
	sessionID   string
	projectPath string
	filePath    string
	mtime       time.Time
}

// IncrementalScan walks ~/.claude/projects/, compares files on disk with the
// SQLite cache, and only re-reads files whose mtime has changed. New files are
// inserted, modified files are updated, and deleted files are removed from the
// cache. Returns all cached sessions sorted by last_activity desc.
func IncrementalScan(db *sql.DB, logger *slog.Logger) ([]ClaudeSessionSummary, error) {
	projectsDir := filepath.Join(ClaudeHome(), "projects")

	// 1. Walk disk and build map of current files keyed by file_path.
	onDisk := make(map[string]diskFile)
	entries, err := os.ReadDir(projectsDir)
	if err != nil {
		if os.IsNotExist(err) {
			// No projects dir — clear cache and return empty.
			_, _ = db.ExecContext(context.Background(), "DELETE FROM claude_session_cache")
			updateLastScanned(db)
			return []ClaudeSessionSummary{}, nil
		}
		return nil, err
	}

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
			fp := filepath.Join(projectsDir, e.Name(), f.Name())
			info, err := f.Info()
			if err != nil {
				continue
			}
			onDisk[fp] = diskFile{
				sessionID:   sessionID,
				projectPath: projectPath,
				filePath:    fp,
				mtime:       info.ModTime().UTC(),
			}
		}
	}

	// 2. Query existing cache entries.
	type cachedEntry struct {
		filePath string
		mtime    time.Time
	}
	cached := make(map[string]cachedEntry)
	ctx := context.Background()
	rows, err := db.QueryContext(ctx, "SELECT file_path, file_mtime FROM claude_session_cache")
	if err != nil {
		return nil, err
	}
	for rows.Next() {
		var ce cachedEntry
		if err := rows.Scan(&ce.filePath, &ce.mtime); err != nil {
			_ = rows.Close()
			return nil, err
		}
		cached[ce.filePath] = ce
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	_ = rows.Close()

	// 3. Determine changes.
	var toUpsert []diskFile
	for fp, df := range onDisk {
		ce, exists := cached[fp]
		if !exists || !ce.mtime.Equal(df.mtime) {
			toUpsert = append(toUpsert, df)
		}
	}

	var toDelete []string
	for fp := range cached {
		if _, exists := onDisk[fp]; !exists {
			toDelete = append(toDelete, fp)
		}
	}

	// 4. Process changes.
	if len(toUpsert) > 0 || len(toDelete) > 0 {
		logger.Info("claude sessions: incremental scan",
			"new_or_modified", len(toUpsert),
			"deleted", len(toDelete),
			"unchanged", len(onDisk)-len(toUpsert))
	}

	for _, df := range toUpsert {
		summary, err := readSessionSummary(df.sessionID, df.projectPath, df.filePath)
		if err != nil || summary == nil {
			continue
		}
		if err := upsertCacheRow(db, df, summary); err != nil {
			logger.Warn("claude sessions: failed to upsert cache row",
				"file", df.filePath, "error", err)
		}
	}

	for _, fp := range toDelete {
		if _, err := db.ExecContext(context.Background(), "DELETE FROM claude_session_cache WHERE file_path = ?", fp); err != nil {
			logger.Warn("claude sessions: failed to delete cache row",
				"file", fp, "error", err)
		}
	}

	// 5. Update metadata.
	updateLastScanned(db)

	// 6. Return all cached rows.
	return loadAllSessions(db)
}

func upsertCacheRow(db *sql.DB, df diskFile, s *ClaudeSessionSummary) error {
	ctx := context.Background()
	_, err := db.ExecContext(ctx, `
		INSERT INTO claude_session_cache (
			session_id, project_path, file_path, file_mtime,
			preview, start_time, last_activity, message_count,
			input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
			git_branch, model, cwd
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(session_id, project_path) DO UPDATE SET
			file_path = excluded.file_path,
			file_mtime = excluded.file_mtime,
			preview = excluded.preview,
			start_time = excluded.start_time,
			last_activity = excluded.last_activity,
			message_count = excluded.message_count,
			input_tokens = excluded.input_tokens,
			output_tokens = excluded.output_tokens,
			cache_creation_tokens = excluded.cache_creation_tokens,
			cache_read_tokens = excluded.cache_read_tokens,
			git_branch = excluded.git_branch,
			model = excluded.model,
			cwd = excluded.cwd`,
		s.SessionID, s.ProjectPath, df.filePath, df.mtime,
		s.Preview, s.StartTime, s.LastActivity, s.MessageCount,
		s.Usage.InputTokens, s.Usage.OutputTokens,
		s.Usage.CacheCreationTokens, s.Usage.CacheReadTokens,
		s.GitBranch, s.Model, s.CWD,
	)
	return err
}

func updateLastScanned(db *sql.DB) {
	ctx := context.Background()
	_, _ = db.ExecContext(ctx, `
		INSERT INTO claude_cache_metadata (id, last_scanned_at) VALUES (1, ?)
		ON CONFLICT(id) DO UPDATE SET last_scanned_at = excluded.last_scanned_at`,
		time.Now().UTC(),
	)
}

func loadAllSessions(db *sql.DB) ([]ClaudeSessionSummary, error) {
	ctx := context.Background()
	rows, err := db.QueryContext(ctx, `
		SELECT session_id, project_path, preview, start_time, last_activity,
		       message_count, input_tokens, output_tokens, cache_creation_tokens,
		       cache_read_tokens, git_branch, model, cwd
		FROM claude_session_cache
		ORDER BY last_activity DESC`)
	if err != nil {
		return nil, err
	}
	defer func() { _ = rows.Close() }()

	var sessions []ClaudeSessionSummary
	for rows.Next() {
		var s ClaudeSessionSummary
		if err := rows.Scan(
			&s.SessionID, &s.ProjectPath, &s.Preview,
			&s.StartTime, &s.LastActivity, &s.MessageCount,
			&s.Usage.InputTokens, &s.Usage.OutputTokens,
			&s.Usage.CacheCreationTokens, &s.Usage.CacheReadTokens,
			&s.GitBranch, &s.Model, &s.CWD,
		); err != nil {
			return nil, err
		}
		sessions = append(sessions, s)
	}
	if sessions == nil {
		sessions = []ClaudeSessionSummary{}
	}
	return sessions, rows.Err()
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
