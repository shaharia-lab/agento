package claudesessions

import (
	"bufio"
	"context"
	"database/sql"
	"encoding/json"
	"log"
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
		files, rdErr := os.ReadDir(filepath.Join(projectsDir, e.Name()))
		if rdErr != nil {
			continue
		}
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
		sessions = scanProjectSessions(projectsDir, e.Name(), sessions)
	}

	sort.Slice(sessions, func(i, j int) bool {
		return sessions[i].LastActivity.After(sessions[j].LastActivity)
	})
	return sessions, nil
}

// scanProjectSessions scans all JSONL files in a single project directory.
func scanProjectSessions(projectsDir, dirName string, sessions []ClaudeSessionSummary) []ClaudeSessionSummary {
	projectPath := DecodeProjectPath(dirName)
	files, err := os.ReadDir(filepath.Join(projectsDir, dirName))
	if err != nil {
		return sessions
	}
	for _, f := range files {
		if f.IsDir() || !strings.HasSuffix(f.Name(), ".jsonl") {
			continue
		}
		sessionID := strings.TrimSuffix(f.Name(), ".jsonl")
		filePath := filepath.Join(projectsDir, dirName, f.Name())
		summary, err := readSessionSummary(sessionID, projectPath, filePath)
		if err != nil || summary == nil {
			continue
		}
		sessions = append(sessions, *summary)
	}
	return sessions
}

// diskFile holds the metadata for a JSONL file found on disk.
type diskFile struct {
	sessionID   string
	projectPath string
	filePath    string
	mtime       time.Time
}

// cachedEntry holds a cached session's file path and modification time.
type cachedEntry struct {
	filePath string
	mtime    time.Time
}

// IncrementalScan walks ~/.claude/projects/, compares files on disk with the
// SQLite cache, and only re-reads files whose mtime has changed. New files are
// inserted, modified files are updated, and deleted files are removed from the
// cache. Returns all cached sessions sorted by last_activity desc.
func IncrementalScan(db *sql.DB, logger *slog.Logger) ([]ClaudeSessionSummary, error) {
	projectsDir := filepath.Join(ClaudeHome(), "projects")

	onDisk, err := walkDiskFiles(projectsDir)
	if err != nil {
		if os.IsNotExist(err) {
			if _, execErr := db.ExecContext(context.Background(), "DELETE FROM claude_session_cache"); execErr != nil {
				logger.Warn("failed to clear session cache", "error", execErr)
			}
			updateLastScanned(db)
			return []ClaudeSessionSummary{}, nil
		}
		return nil, err
	}

	cached, err := loadCachedEntries(db)
	if err != nil {
		return nil, err
	}

	toUpsert, toDelete := diffDiskAndCache(onDisk, cached)
	applyChanges(db, logger, onDisk, toUpsert, toDelete)

	updateLastScanned(db)
	return loadAllSessions(db)
}

func walkDiskFiles(projectsDir string) (map[string]diskFile, error) {
	entries, err := os.ReadDir(projectsDir)
	if err != nil {
		return nil, err
	}
	onDisk := make(map[string]diskFile)
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		collectProjectDiskFiles(projectsDir, e.Name(), onDisk)
	}
	return onDisk, nil
}

func collectProjectDiskFiles(projectsDir, dirName string, onDisk map[string]diskFile) {
	projectPath := DecodeProjectPath(dirName)
	files, err := os.ReadDir(filepath.Join(projectsDir, dirName))
	if err != nil {
		return
	}
	for _, f := range files {
		if f.IsDir() || !strings.HasSuffix(f.Name(), ".jsonl") {
			continue
		}
		info, err := f.Info()
		if err != nil {
			continue
		}
		fp := filepath.Join(projectsDir, dirName, f.Name())
		onDisk[fp] = diskFile{
			sessionID:   strings.TrimSuffix(f.Name(), ".jsonl"),
			projectPath: projectPath,
			filePath:    fp,
			mtime:       info.ModTime().UTC(),
		}
	}
}

func loadCachedEntries(db *sql.DB) (map[string]cachedEntry, error) {
	cached := make(map[string]cachedEntry)
	rows, err := db.QueryContext(context.Background(), "SELECT file_path, file_mtime FROM claude_session_cache")
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			log.Printf("failed to close rows: %v", cerr)
		}
	}()

	for rows.Next() {
		var ce cachedEntry
		if err := rows.Scan(&ce.filePath, &ce.mtime); err != nil {
			return nil, err
		}
		cached[ce.filePath] = ce
	}
	return cached, rows.Err()
}

func diffDiskAndCache(onDisk map[string]diskFile, cached map[string]cachedEntry) ([]string, []string) {
	var toUpsert []string
	for fp, df := range onDisk {
		ce, exists := cached[fp]
		if !exists || !ce.mtime.Equal(df.mtime) {
			toUpsert = append(toUpsert, fp)
		}
	}

	var toDelete []string
	for fp := range cached {
		if _, exists := onDisk[fp]; !exists {
			toDelete = append(toDelete, fp)
		}
	}
	return toUpsert, toDelete
}

func applyChanges(db *sql.DB, logger *slog.Logger, onDisk map[string]diskFile, toUpsert, toDelete []string) {
	if len(toUpsert) > 0 || len(toDelete) > 0 {
		logger.Info("claude sessions: incremental scan",
			"new_or_modified", len(toUpsert),
			"deleted", len(toDelete),
			"unchanged", len(onDisk)-len(toUpsert))
	}

	for _, fp := range toUpsert {
		df := onDisk[fp]
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
		deleteQuery := "DELETE FROM claude_session_cache WHERE file_path = ?"
		if _, err := db.ExecContext(context.Background(), deleteQuery, fp); err != nil {
			logger.Warn("claude sessions: failed to delete cache row",
				"file", fp, "error", err)
		}
	}
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
	if _, err := db.ExecContext(ctx, `
		INSERT INTO claude_cache_metadata (id, last_scanned_at) VALUES (1, ?)
		ON CONFLICT(id) DO UPDATE SET last_scanned_at = excluded.last_scanned_at`,
		time.Now().UTC(),
	); err != nil {
		log.Printf("failed to update last_scanned_at: %v", err)
	}
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
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			log.Printf("failed to close rows: %v", cerr)
		}
	}()

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

// timeRange tracks the earliest and latest timestamps seen.
type timeRange struct {
	start time.Time
	last  time.Time
}

func (tr *timeRange) update(ts time.Time) {
	if ts.IsZero() {
		return
	}
	if tr.start.IsZero() || ts.Before(tr.start) {
		tr.start = ts
	}
	if ts.After(tr.last) {
		tr.last = ts
	}
}

// updateMetadataFromEvent sets CWD and GitBranch from the first event that has them.
func updateMetadataFromEvent(cwd, gitBranch *string, ev rawEvent) {
	if *cwd == "" && ev.CWD != "" {
		*cwd = ev.CWD
	}
	if *gitBranch == "" && ev.GitBranch != "" {
		*gitBranch = ev.GitBranch
	}
}

// addAssistantUsage accumulates assistant message usage into a TokenUsage.
func addAssistantUsage(usage *TokenUsage, msg *rawMessage) {
	if msg == nil || msg.Usage == nil {
		return
	}
	usage.InputTokens += msg.Usage.InputTokens
	usage.OutputTokens += msg.Usage.OutputTokens
	usage.CacheCreationTokens += msg.Usage.CacheCreationInputTokens
	usage.CacheReadTokens += msg.Usage.CacheReadInputTokens
}

// readSessionSummary reads a session JSONL file and extracts lightweight metadata.
func readSessionSummary(sessionID, projectPath, filePath string) (*ClaudeSessionSummary, error) {
	f, err := os.Open(filePath) //nolint:gosec
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := f.Close(); cerr != nil {
			log.Printf("failed to close file: %v", cerr)
		}
	}()

	summary := &ClaudeSessionSummary{
		SessionID:   sessionID,
		ProjectPath: projectPath,
	}

	var tr timeRange
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

		tr.update(ev.Timestamp)
		updateMetadataFromEvent(&summary.CWD, &summary.GitBranch, ev)
		processSummaryEvent(summary, ev)
	}

	summary.StartTime = tr.start
	summary.LastActivity = tr.last

	if summary.StartTime.IsZero() {
		return nil, nil
	}
	return summary, sc.Err()
}

func processSummaryEvent(summary *ClaudeSessionSummary, ev rawEvent) {
	switch ev.Type {
	case "user":
		if ev.IsSidechain {
			return
		}
		summary.MessageCount++
		if summary.Preview == "" && ev.Message != nil {
			summary.Preview = truncateRunes(extractTextContent(ev.Message.Content), previewMaxRunes)
		}
	case "assistant":
		summary.MessageCount++
		if ev.Message != nil && summary.Model == "" && ev.Message.Model != "" {
			summary.Model = ev.Message.Model
		}
		addAssistantUsage(&summary.Usage, ev.Message)
	}
}

// GetSessionDetail reads the full session JSONL and builds the complete message list.
// Returns nil if the session is not found.
func GetSessionDetail(sessionID string) (*ClaudeSessionDetail, error) {
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
	defer func() {
		if cerr := f.Close(); cerr != nil {
			log.Printf("failed to close file: %v", cerr)
		}
	}()

	detail := &ClaudeSessionDetail{}
	detail.SessionID = sessionID
	detail.ProjectPath = projectPath

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 4*1024*1024), 4*1024*1024)

	var tr timeRange
	var topLevel []ClaudeMessage
	progressMap := make(map[string][]ClaudeMessage)

	for sc.Scan() {
		var ev rawEvent
		if err := json.Unmarshal(sc.Bytes(), &ev); err != nil {
			continue
		}
		if ev.Type == "file-history-snapshot" {
			continue
		}
		tr.update(ev.Timestamp)
		updateMetadataFromEvent(&detail.CWD, &detail.GitBranch, ev)
		topLevel = processDetailEvent(detail, ev, progressMap, topLevel)
	}

	detail.StartTime = tr.start
	detail.LastActivity = tr.last
	attachProgressChildren(topLevel, progressMap)
	detail.Messages = topLevel
	if detail.Messages == nil {
		detail.Messages = []ClaudeMessage{}
	}
	detail.Todos = loadTodos(sessionID)
	if detail.Todos == nil {
		detail.Todos = []ClaudeTodo{}
	}
	detail.Preview = derivePreview(detail.Messages)
	return detail, sc.Err()
}

func processDetailEvent(
	detail *ClaudeSessionDetail, ev rawEvent,
	progressMap map[string][]ClaudeMessage,
	topLevel []ClaudeMessage,
) []ClaudeMessage {
	switch ev.Type {
	case "user":
		return processDetailUserEvent(detail, ev, topLevel)
	case "assistant":
		return processDetailAssistantEvent(detail, ev, topLevel)
	case "progress":
		processDetailProgressEvent(ev, progressMap)
	}
	return topLevel
}

func processDetailUserEvent(detail *ClaudeSessionDetail, ev rawEvent, topLevel []ClaudeMessage) []ClaudeMessage {
	if ev.IsSidechain {
		return topLevel
	}
	content := ""
	if ev.Message != nil {
		content = extractTextContent(ev.Message.Content)
	}
	detail.MessageCount++
	return append(topLevel, ClaudeMessage{
		UUID: ev.UUID, ParentUUID: ev.ParentUUID,
		Type: "user", Timestamp: ev.Timestamp,
		Role: "user", Content: content, GitBranch: ev.GitBranch,
	})
}

func processDetailAssistantEvent(detail *ClaudeSessionDetail, ev rawEvent, topLevel []ClaudeMessage) []ClaudeMessage {
	msg := ClaudeMessage{
		UUID: ev.UUID, ParentUUID: ev.ParentUUID,
		Type: "assistant", Timestamp: ev.Timestamp,
		Role: "assistant", GitBranch: ev.GitBranch,
	}
	if ev.Message != nil {
		if detail.Model == "" && ev.Message.Model != "" {
			detail.Model = ev.Message.Model
		}
		populateAssistantUsage(&msg, detail, ev.Message)
		populateAssistantBlocks(&msg, ev.Message)
	}
	detail.MessageCount++
	return append(topLevel, msg)
}

func populateAssistantUsage(msg *ClaudeMessage, detail *ClaudeSessionDetail, rawMsg *rawMessage) {
	if rawMsg.Usage == nil {
		return
	}
	u := TokenUsage{
		InputTokens:         rawMsg.Usage.InputTokens,
		OutputTokens:        rawMsg.Usage.OutputTokens,
		CacheCreationTokens: rawMsg.Usage.CacheCreationInputTokens,
		CacheReadTokens:     rawMsg.Usage.CacheReadInputTokens,
	}
	msg.Usage = &u
	detail.Usage.InputTokens += u.InputTokens
	detail.Usage.OutputTokens += u.OutputTokens
	detail.Usage.CacheCreationTokens += u.CacheCreationTokens
	detail.Usage.CacheReadTokens += u.CacheReadTokens
}

func populateAssistantBlocks(msg *ClaudeMessage, rawMsg *rawMessage) {
	var blocks []rawContentBlock
	if err := json.Unmarshal(rawMsg.Content, &blocks); err != nil {
		return
	}
	for _, b := range blocks {
		if nb := normalizeBlock(b); nb.Type != "" {
			msg.Blocks = append(msg.Blocks, nb)
		}
	}
}

func processDetailProgressEvent(ev rawEvent, progressMap map[string][]ClaudeMessage) {
	if ev.ParentUUID == "" {
		return
	}
	progressMap[ev.ParentUUID] = append(progressMap[ev.ParentUUID], ClaudeMessage{
		UUID: ev.UUID, ParentUUID: ev.ParentUUID,
		Type: "progress", Timestamp: ev.Timestamp, IsSidechain: ev.IsSidechain,
	})
}

func attachProgressChildren(topLevel []ClaudeMessage, progressMap map[string][]ClaudeMessage) {
	for i := range topLevel {
		if children, ok := progressMap[topLevel[i].UUID]; ok {
			topLevel[i].Children = children
		}
	}
}

func derivePreview(messages []ClaudeMessage) string {
	for _, msg := range messages {
		if msg.Role == "user" && msg.Content != "" {
			return truncateRunes(msg.Content, previewMaxRunes)
		}
	}
	return ""
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
