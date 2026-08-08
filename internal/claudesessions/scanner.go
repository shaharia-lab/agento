package claudesessions

import (
	"bufio"
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
	"unicode/utf8"
)

const (
	previewMaxRunes = 120
	jsonlExt        = ".jsonl"

	// CurrentScannerVersion is bumped whenever the scanner extracts something
	// new from a transcript that already-cached rows would be missing. Cached
	// rows carry the version that produced them; when this constant is ahead of
	// the stored one, the next incremental scan re-reads every file even though
	// no mtime changed, then records the new version.
	//
	// v1: cache-creation tokens are split by cache TTL (5m vs 1h), which bill at
	// different rates — rows written before the split have both columns at zero.
	// v2: Claude Code's own `custom-title` / `ai-title` events are read into
	// native_title / ai_title — blank on every row written before v2.
	// v3: message_count switched from raw event volume to conversational turns
	// and event_count was added — rows written before v3 hold the old inflated
	// number in message_count and zero in event_count.
	// v4: pr-link, agent-name, permission-mode, mode, relocated, worktree-state
	// and system/compact_boundary are read — rows written before v4 have every
	// one of those columns empty and no claude_session_pr rows at all. The same
	// bump also drops pr-link from the session time range, so last_activity is
	// recomputed.
	CurrentScannerVersion = 4
)

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

	// Title events carry no timestamp and no message — only these fields.
	CustomTitle string `json:"customTitle,omitempty"` // native /rename
	AITitle     string `json:"aiTitle,omitempty"`     // Claude Code's auto-title

	// pr-link: the pull request a session produced. Carries a real timestamp.
	PRNumber     int    `json:"prNumber,omitempty"`
	PRUrl        string `json:"prUrl,omitempty"`
	PRRepository string `json:"prRepository,omitempty"`

	// Session metadata events. Like the title events these carry no timestamp
	// and are re-appended on every resume, so the last one in the file wins.
	AgentName       string              `json:"agentName,omitempty"`
	PermissionMode  string              `json:"permissionMode,omitempty"`
	Mode            string              `json:"mode,omitempty"`
	RelocatedCWD    string              `json:"relocatedCwd,omitempty"`
	WorktreeSession *rawWorktreeSession `json:"worktreeSession,omitempty"`

	// system events: Subtype selects the payload; compact_boundary carries
	// CompactMetadata.
	Subtype         string              `json:"subtype,omitempty"`
	CompactMetadata *rawCompactMetadata `json:"compactMetadata,omitempty"`
}

// rawWorktreeSession is the payload of a worktree-state event: the throwaway
// worktree a session ran in, plus where it came from.
type rawWorktreeSession struct {
	WorktreeName       string `json:"worktreeName,omitempty"`
	WorktreeBranch     string `json:"worktreeBranch,omitempty"`
	OriginalBranch     string `json:"originalBranch,omitempty"`
	OriginalCwd        string `json:"originalCwd,omitempty"`
	OriginalHeadCommit string `json:"originalHeadCommit,omitempty"`
}

// rawCompactMetadata is the payload of a system/compact_boundary event.
// CumulativeDroppedTokens is a running total across the session, not a delta.
type rawCompactMetadata struct {
	Trigger                 string `json:"trigger,omitempty"`
	PreTokens               int    `json:"preTokens,omitempty"`
	PostTokens              int    `json:"postTokens,omitempty"`
	CumulativeDroppedTokens int    `json:"cumulativeDroppedTokens,omitempty"`
	DurationMs              int64  `json:"durationMs,omitempty"`
}

type rawMessage struct {
	Role    string          `json:"role"`
	Model   string          `json:"model,omitempty"`
	Content json.RawMessage `json:"content"` // string or array of content blocks
	Usage   *rawUsage       `json:"usage,omitempty"`
}

type rawUsage struct {
	InputTokens              int               `json:"input_tokens"`
	OutputTokens             int               `json:"output_tokens"`
	CacheCreationInputTokens int               `json:"cache_creation_input_tokens"`
	CacheReadInputTokens     int               `json:"cache_read_input_tokens"`
	CacheCreation            *rawCacheCreation `json:"cache_creation,omitempty"`
}

// rawCacheCreation is the nested split of cache_creation_input_tokens by cache
// TTL. Absent on transcripts written before Claude Code emitted it.
type rawCacheCreation struct {
	Ephemeral5mInputTokens int `json:"ephemeral_5m_input_tokens"`
	Ephemeral1hInputTokens int `json:"ephemeral_1h_input_tokens"`
}

// splitCacheCreation attributes a flat cache-creation total across the 5m and
// 1h buckets. See splitCacheTiers for the rules.
func splitCacheCreation(u *rawUsage) (fiveMin, oneHour int) {
	if u.CacheCreation == nil {
		return splitCacheTiers(u.CacheCreationInputTokens, 0)
	}
	return splitCacheTiers(u.CacheCreationInputTokens, u.CacheCreation.Ephemeral1hInputTokens)
}

// splitCacheTiers divides a cache-creation total between the two billing tiers.
//
// The flat total is authoritative — the acceptance criterion for #180 is that
// the buckets sum to it for every session — so only the 1-hour count is taken
// from the nested object and the 5-minute bucket is derived as the remainder.
// On consistent input that reproduces the nested 5m value exactly; on
// inconsistent input the invariant still holds, whereas adding the two nested
// fields could exceed the total and overcharge.
//
// A transcript with no nested object (written before Claude Code emitted the
// split) yields nested1h == 0, putting the whole total in the 5m bucket — what
// the cost model assumed before the split existed, so those files keep costing
// exactly what they did.
func splitCacheTiers(total, nested1h int) (fiveMin, oneHour int) {
	oneHour = nested1h
	if oneHour > total {
		oneHour = total
	}
	if oneHour < 0 {
		oneHour = 0
	}
	return total - oneHour, oneHour
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
			if !f.IsDir() && strings.HasSuffix(f.Name(), jsonlExt) {
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
func ScanAllSessions(logger *slog.Logger) ([]ClaudeSessionSummary, error) {
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
		sessions = scanProjectSessions(projectsDir, e.Name(), sessions, logger)
	}

	sort.Slice(sessions, func(i, j int) bool {
		return sessions[i].LastActivity.After(sessions[j].LastActivity)
	})
	return sessions, nil
}

// scanProjectSessions scans all JSONL files in a single project directory.
func scanProjectSessions(
	projectsDir, dirName string, sessions []ClaudeSessionSummary, logger *slog.Logger,
) []ClaudeSessionSummary {
	projectPath := DecodeProjectPath(dirName)
	files, err := os.ReadDir(filepath.Join(projectsDir, dirName))
	if err != nil {
		return sessions
	}
	for _, f := range files {
		if f.IsDir() || !strings.HasSuffix(f.Name(), jsonlExt) {
			continue
		}
		sessionID := strings.TrimSuffix(f.Name(), jsonlExt)
		filePath := filepath.Join(projectsDir, dirName, f.Name())
		summary, err := readSessionSummary(sessionID, projectPath, filePath, logger)
		if err != nil || summary == nil {
			continue
		}
		sessions = append(sessions, *summary)
	}
	return sessions
}

// diskFile holds the metadata for a JSONL file found on disk. It covers both
// top-level session transcripts and the sub-agent transcripts nested under
// <session-id>/subagents/, distinguished by isSubagent.
type diskFile struct {
	sessionID   string // for a sub-agent file this is the PARENT session's id
	projectPath string
	filePath    string
	mtime       time.Time

	// Sub-agent files only.
	isSubagent bool
	agentID    string // filename stem, e.g. "agent-adb233f74d4331d56"
	// parentFilePath is the parent session's own .jsonl. Insight processing is
	// notified against this path so a changed sub-agent re-runs the whole
	// session (parent + every sub-agent) rather than the fragment alone.
	parentFilePath string
}

// cachedEntry holds a cached file's path and modification time. isSubagent
// records which table the row came from, so deletions — where the file is gone
// and no diskFile survives — can still be routed to the right table.
type cachedEntry struct {
	filePath   string
	mtime      time.Time
	isSubagent bool
}

// IncrementalScan walks ~/.claude/projects/, compares files on disk with the
// SQLite cache, and only re-reads files whose mtime has changed. New files are
// inserted, modified files are updated, and deleted files are removed from the
// cache. Returns all cached sessions sorted by last_activity desc.
func IncrementalScan(db *sql.DB, logger *slog.Logger) ([]ClaudeSessionSummary, error) {
	return IncrementalScanWithNotify(db, logger, nil)
}

// IncrementalScanWithNotify is like IncrementalScan but calls notify for each
// session that is newly inserted (isNew=true) or updated (isNew=false).
// notify may be nil.
func IncrementalScanWithNotify(
	db *sql.DB, logger *slog.Logger, notify func(sessionID, filePath string, isNew bool),
) ([]ClaudeSessionSummary, error) {
	projectsDir := filepath.Join(ClaudeHome(), "projects")

	onDisk, err := walkDiskFiles(projectsDir)
	if err != nil {
		if os.IsNotExist(err) {
			for _, table := range []string{
				"claude_session_cache", "claude_subagent_cache", "claude_session_pr",
			} {
				// #nosec G202 -- table is a package-internal constant, never user input.
				if _, execErr := db.ExecContext(context.Background(), "DELETE FROM "+table); execErr != nil {
					logger.Warn("failed to clear session cache", "table", table, "error", execErr)
				}
			}
			updateLastScanned(db, logger)
			return []ClaudeSessionSummary{}, nil
		}
		return nil, err
	}

	cached, err := loadCachedEntries(db, logger)
	if err != nil {
		return nil, err
	}

	// A scanner-version bump means the cached rows are missing data the current
	// reader extracts, so mtime comparison would wrongly report them unchanged.
	// Dropping the cached set makes every file look new for exactly one scan.
	staleReader := storedScannerVersion(db) < CurrentScannerVersion
	if staleReader {
		invalidateCachedMtimes(cached, logger)
	}

	diff := diffDiskAndCache(onDisk, cached)
	applyChangesWithNotify(db, logger, onDisk, diff, notify)

	updateLastScanned(db, logger)
	if staleReader {
		recordScannerVersion(db, logger)
	}
	return loadAllSessions(db, logger)
}

// invalidateCachedMtimes zeroes every cached mtime so the next diff treats all
// files as modified, forcing a re-read after a scanner-version bump.
//
// The entries are invalidated rather than dropped: the files themselves are
// unchanged — only the rows are incomplete — so they must re-read as updates to
// existing sessions rather than as newly discovered ones, and a row whose file
// is gone must still be detected as a deletion.
func invalidateCachedMtimes(cached map[string]cachedEntry, logger *slog.Logger) {
	if len(cached) == 0 {
		return
	}
	logger.Info("claude sessions: scanner version bumped, re-reading all transcripts",
		"current", CurrentScannerVersion, "rows", len(cached))
	for path, ce := range cached {
		ce.mtime = time.Time{}
		cached[path] = ce
	}
}

// storedScannerVersion returns the scanner version the cached rows were written
// by, or 0 when the cache is empty or the value is unreadable — both of which
// correctly trigger a full re-read.
func storedScannerVersion(db *sql.DB) int {
	var v int
	row := db.QueryRowContext(context.Background(),
		"SELECT scanner_version FROM claude_cache_metadata WHERE id = 1")
	if row.Scan(&v) != nil {
		return 0
	}
	return v
}

func recordScannerVersion(db *sql.DB, logger *slog.Logger) {
	if _, err := db.ExecContext(context.Background(),
		"UPDATE claude_cache_metadata SET scanner_version = ? WHERE id = 1",
		CurrentScannerVersion,
	); err != nil {
		logger.Warn("claude sessions: failed to record scanner version", "error", err)
	}
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
	projectDir := filepath.Join(projectsDir, dirName)
	files, err := os.ReadDir(projectDir)
	if err != nil {
		return
	}

	// Session ids are needed before their sibling directories can be matched,
	// and ReadDir gives no ordering guarantee, so collect the flat files first.
	sessionIDs := make(map[string]struct{}, len(files))
	for _, f := range files {
		if f.IsDir() || !strings.HasSuffix(f.Name(), jsonlExt) {
			continue
		}
		info, err := f.Info()
		if err != nil {
			continue
		}
		sessionID := strings.TrimSuffix(f.Name(), jsonlExt)
		sessionIDs[sessionID] = struct{}{}
		fp := filepath.Join(projectDir, f.Name())
		onDisk[fp] = diskFile{
			sessionID:   sessionID,
			projectPath: projectPath,
			filePath:    fp,
			mtime:       info.ModTime().UTC(),
		}
	}

	for _, f := range files {
		if !f.IsDir() {
			continue
		}
		if _, ok := sessionIDs[f.Name()]; !ok {
			continue
		}
		collectSubagentDiskFiles(projectDir, f.Name(), projectPath, onDisk)
	}
}

// collectSubagentDiskFiles emits one diskFile per sub-agent transcript under
// <projectDir>/<sessionID>/subagents/. Claude Code moved delegated work out of
// the parent JSONL into this directory; a session with no such directory simply
// contributes nothing.
func collectSubagentDiskFiles(projectDir, sessionID, projectPath string, onDisk map[string]diskFile) {
	subagentsDir := filepath.Join(projectDir, sessionID, "subagents")
	entries, err := os.ReadDir(subagentsDir)
	if err != nil {
		return
	}
	parentFilePath := filepath.Join(projectDir, sessionID+jsonlExt)
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), jsonlExt) {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		fp := filepath.Join(subagentsDir, e.Name())
		onDisk[fp] = diskFile{
			sessionID:      sessionID,
			projectPath:    projectPath,
			filePath:       fp,
			mtime:          info.ModTime().UTC(),
			isSubagent:     true,
			agentID:        strings.TrimSuffix(e.Name(), jsonlExt),
			parentFilePath: parentFilePath,
		}
	}
}

func loadCachedEntries(db *sql.DB, logger *slog.Logger) (map[string]cachedEntry, error) {
	cached := make(map[string]cachedEntry)
	if err := loadCachedEntriesFrom(db, logger, "claude_session_cache", false, cached); err != nil {
		return nil, err
	}
	if err := loadCachedEntriesFrom(db, logger, "claude_subagent_cache", true, cached); err != nil {
		return nil, err
	}
	return cached, nil
}

// loadCachedEntriesFrom reads the (file_path, file_mtime) pairs of one cache
// table into cached. Both tables are keyed by distinct absolute paths, so they
// share a single map and therefore a single mtime diff.
func loadCachedEntriesFrom(
	db *sql.DB, logger *slog.Logger, table string, isSubagent bool, cached map[string]cachedEntry,
) error {
	// #nosec G202 -- table is a package-internal constant, never user input.
	rows, err := db.QueryContext(context.Background(), "SELECT file_path, file_mtime FROM "+table)
	if err != nil {
		return err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			logger.Warn("failed to close rows", "error", cerr)
		}
	}()

	for rows.Next() {
		ce := cachedEntry{isSubagent: isSubagent}
		if err := rows.Scan(&ce.filePath, &ce.mtime); err != nil {
			return err
		}
		cached[ce.filePath] = ce
	}
	return rows.Err()
}

// diskDiff groups file paths by their change type.
type diskDiff struct {
	toInsert []string      // file paths not present in the cache
	toUpdate []string      // file paths present in the cache but with a changed mtime
	toDelete []cachedEntry // cache rows whose file is no longer on disk
}

func diffDiskAndCache(onDisk map[string]diskFile, cached map[string]cachedEntry) diskDiff {
	var d diskDiff
	for fp, df := range onDisk {
		ce, exists := cached[fp]
		switch {
		case !exists:
			d.toInsert = append(d.toInsert, fp)
		case !ce.mtime.Equal(df.mtime):
			d.toUpdate = append(d.toUpdate, fp)
		}
	}
	for fp, ce := range cached {
		if _, exists := onDisk[fp]; !exists {
			d.toDelete = append(d.toDelete, ce)
		}
	}
	return d
}

func applyChangesWithNotify(
	db *sql.DB, logger *slog.Logger,
	onDisk map[string]diskFile,
	diff diskDiff,
	notify func(sessionID, filePath string, isNew bool),
) {
	total := len(diff.toInsert) + len(diff.toUpdate)
	if total > 0 || len(diff.toDelete) > 0 {
		logger.Info("claude sessions: incremental scan",
			"new", len(diff.toInsert),
			"modified", len(diff.toUpdate),
			"deleted", len(diff.toDelete),
			"unchanged", len(onDisk)-total)
	}

	// Insights are computed per session over the parent transcript plus all of
	// its sub-agent transcripts, so a session is worth notifying about at most
	// once per scan however many of its files changed. Collect first, emit after:
	// a session with N changed sub-agents would otherwise enqueue N+1 items that
	// each re-read all N+1 files, and on a first scan the resulting fan-out
	// overflows the worker queue.
	pending := make(map[string]pendingNotify)
	for _, fp := range diff.toInsert {
		applyOne(db, logger, onDisk[fp], true, pending)
	}
	for _, fp := range diff.toUpdate {
		applyOne(db, logger, onDisk[fp], false, pending)
	}
	if notify != nil {
		for sessionID, p := range pending {
			notify(sessionID, p.filePath, p.isNew)
		}
	}

	for _, ce := range diff.toDelete {
		deleteCachedFile(db, logger, ce)
	}
}

// deleteCachedFile removes every cache row belonging to a transcript that is no
// longer on disk.
func deleteCachedFile(db *sql.DB, logger *slog.Logger, ce cachedEntry) {
	table := "claude_session_cache"
	if ce.isSubagent {
		table = "claude_subagent_cache"
	} else {
		// Linked PRs hang off the session row with no foreign key, so they must
		// be cleared here or they outlive the session forever — and attachPRs
		// reads the whole table on every list. This runs before the session row
		// is deleted, because it resolves the session through it.
		if _, err := db.ExecContext(context.Background(),
			`DELETE FROM claude_session_pr WHERE session_id IN (
				SELECT session_id FROM claude_session_cache WHERE file_path = ?)`,
			ce.filePath); err != nil {
			logger.Warn("claude sessions: failed to delete linked PRs",
				"file", ce.filePath, "error", err)
		}
	}
	// #nosec G202 -- table is a package-internal constant, never user input.
	deleteQuery := "DELETE FROM " + table + " WHERE file_path = ?"
	if _, err := db.ExecContext(context.Background(), deleteQuery, ce.filePath); err != nil {
		logger.Warn("claude sessions: failed to delete cache row",
			"file", ce.filePath, "error", err)
	}
}

// pendingNotify is one session's queued insight notification for this scan.
type pendingNotify struct {
	filePath string
	isNew    bool
}

// applyOne upserts a single changed file into the appropriate cache table and
// records the session's insight notification in pending.
//
// A sub-agent file is recorded against its PARENT session id and file path,
// because a changed fragment must re-run the whole session. It never marks the
// session as new — the session already existed — and never overwrites an entry
// the parent file recorded, so a genuinely new session still reports as new
// regardless of the order the two are applied in.
func applyOne(
	db *sql.DB, logger *slog.Logger, df diskFile, isNew bool, pending map[string]pendingNotify,
) {
	if df.isSubagent {
		if !applySubagentUpsert(db, logger, df) {
			return
		}
		if _, exists := pending[df.sessionID]; !exists {
			pending[df.sessionID] = pendingNotify{filePath: df.parentFilePath, isNew: false}
		}
		return
	}
	if !applyUpsert(db, logger, df) {
		return
	}
	pending[df.sessionID] = pendingNotify{filePath: df.filePath, isNew: isNew}
}

// applyUpsert reads the session summary and writes it to the cache.
// Returns true on success.
func applyUpsert(db *sql.DB, logger *slog.Logger, df diskFile) bool {
	summary, err := readSessionSummary(df.sessionID, df.projectPath, df.filePath, logger)
	if err != nil || summary == nil {
		return false
	}
	if err := upsertCacheRow(db, df, summary); err != nil {
		logger.Warn("claude sessions: failed to upsert cache row",
			"file", df.filePath, "error", err)
		return false
	}
	return true
}

// upsertCacheRow writes one session's cache row and its linked pull requests.
//
// Both happen in a single transaction: the row carries the file's mtime, so a
// PR write failing after the row committed would leave the file looking
// unchanged to diffDiskAndCache, and the PR rows would never be rebuilt — not
// until an unrelated scanner-version bump or a touch of the file.
func upsertCacheRow(db *sql.DB, df diskFile, s *ClaudeSessionSummary) (err error) {
	ctx := context.Background()
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() {
		if err != nil {
			if rbErr := tx.Rollback(); rbErr != nil && !errors.Is(rbErr, sql.ErrTxDone) {
				err = errors.Join(err, rbErr)
			}
		}
	}()

	if err = insertCacheRow(ctx, tx, df, s); err != nil {
		return err
	}
	if err = replacePRRows(ctx, tx, s.SessionID, s.PRs); err != nil {
		return err
	}
	return tx.Commit()
}

// insertCacheRow writes the session's own row. custom_title and is_favorite are
// intentionally absent from both the INSERT and the UPDATE SET so user-defined
// values survive a rescan; everything else is derived from the transcript and
// must refresh.
func insertCacheRow(ctx context.Context, tx *sql.Tx, df diskFile, s *ClaudeSessionSummary) error {
	_, err := tx.ExecContext(ctx, `
		INSERT INTO claude_session_cache (
			session_id, project_path, file_path, file_mtime,
			preview, start_time, last_activity, message_count, event_count,
			input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
			cache_creation_5m_tokens, cache_creation_1h_tokens,
			git_branch, model, cwd, native_title, ai_title,
			agent_name, permission_mode, mode, relocated_cwd,
			worktree_name, worktree_branch, original_branch,
			compaction_count, dropped_tokens
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
		          ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(session_id, project_path) DO UPDATE SET
			file_path = excluded.file_path,
			file_mtime = excluded.file_mtime,
			preview = excluded.preview,
			start_time = excluded.start_time,
			last_activity = excluded.last_activity,
			message_count = excluded.message_count,
			event_count = excluded.event_count,
			input_tokens = excluded.input_tokens,
			output_tokens = excluded.output_tokens,
			cache_creation_tokens = excluded.cache_creation_tokens,
			cache_read_tokens = excluded.cache_read_tokens,
			cache_creation_5m_tokens = excluded.cache_creation_5m_tokens,
			cache_creation_1h_tokens = excluded.cache_creation_1h_tokens,
			git_branch = excluded.git_branch,
			model = excluded.model,
			cwd = excluded.cwd,
			native_title = excluded.native_title,
			ai_title = excluded.ai_title,
			agent_name = excluded.agent_name,
			permission_mode = excluded.permission_mode,
			mode = excluded.mode,
			relocated_cwd = excluded.relocated_cwd,
			worktree_name = excluded.worktree_name,
			worktree_branch = excluded.worktree_branch,
			original_branch = excluded.original_branch,
			compaction_count = excluded.compaction_count,
			dropped_tokens = excluded.dropped_tokens`,
		// custom_title and is_favorite are intentionally omitted from both INSERT and UPDATE SET
		// so any user-defined values are preserved across rescans.
		//
		// native_title and ai_title are deliberately NOT excluded: they mirror
		// Claude Code's own title events and must track them, including when a
		// native rename changes or clears one. They cannot clobber a user's
		// Agento rename because that lives in the separate custom_title column
		// and wins the precedence in ResolveDisplayTitle.
		s.SessionID, s.ProjectPath, df.filePath, df.mtime,
		s.Preview, s.StartTime, s.LastActivity, s.MessageCount, s.EventCount,
		s.Usage.InputTokens, s.Usage.OutputTokens,
		s.Usage.CacheCreationTokens, s.Usage.CacheReadTokens,
		s.Usage.CacheCreation5mTokens, s.Usage.CacheCreation1hTokens,
		s.GitBranch, s.Model, s.CWD, s.NativeTitle, s.AITitle,
		// Derived from transcript metadata events, so unlike custom_title these
		// must refresh on every rescan.
		s.AgentName, s.PermissionMode, s.Mode, s.RelocatedCWD,
		s.WorktreeName, s.WorktreeBranch, s.OriginalBranch,
		s.CompactionCount, s.DroppedTokens,
	)
	return err
}

// replacePRRows rewrites a session's linked pull requests. The transcript is
// the source of truth and a rescan re-reads it whole, so the rows are replaced
// rather than merged — that way a PR link removed upstream disappears here too.
//
// The DELETE clears every row for this session first, and summary.PRs is
// already deduplicated by URL, so the insert cannot conflict.
func replacePRRows(ctx context.Context, tx *sql.Tx, sessionID string, prs []ClaudeSessionPR) error {
	if _, err := tx.ExecContext(ctx,
		`DELETE FROM claude_session_pr WHERE session_id = ?`, sessionID); err != nil {
		return err
	}
	for _, pr := range prs {
		if _, err := tx.ExecContext(ctx, `
			INSERT INTO claude_session_pr (session_id, pr_url, pr_number, pr_repository, first_seen_at)
			VALUES (?, ?, ?, ?, ?)`,
			sessionID, pr.PRURL, pr.PRNumber, pr.PRRepository, pr.FirstSeenAt,
		); err != nil {
			return err
		}
	}
	return nil
}

// subagentMeta is the agent-<id>.meta.json sidecar written next to each
// sub-agent transcript. Fields are best-effort: older transcripts omit some,
// and a missing or malformed sidecar must not lose the transcript itself.
type subagentMeta struct {
	AgentType   string `json:"agentType"`
	Description string `json:"description"`
	ToolUseID   string `json:"toolUseId"`
}

// readSubagentMeta reads the sidecar next to a sub-agent transcript. A missing
// or unreadable sidecar yields a zero value rather than an error — the token
// usage in the transcript is the part that matters.
func readSubagentMeta(filePath string, logger *slog.Logger) subagentMeta {
	metaPath := strings.TrimSuffix(filePath, jsonlExt) + ".meta.json"
	data, err := os.ReadFile(metaPath) //nolint:gosec // path derived from a scanned transcript
	if err != nil {
		if !os.IsNotExist(err) {
			logger.Debug("claude sessions: failed to read sub-agent meta", "file", metaPath, "error", err)
		}
		return subagentMeta{}
	}
	var m subagentMeta
	if err := json.Unmarshal(data, &m); err != nil {
		logger.Debug("claude sessions: malformed sub-agent meta", "file", metaPath, "error", err)
		return subagentMeta{}
	}
	return m
}

// applySubagentUpsert reads a sub-agent transcript and writes it to the
// sub-agent cache. Returns true on success.
func applySubagentUpsert(db *sql.DB, logger *slog.Logger, df diskFile) bool {
	summary, err := readSubagentSummary(df.sessionID, df.projectPath, df.filePath, logger)
	if err != nil || summary == nil {
		return false
	}
	meta := readSubagentMeta(df.filePath, logger)
	if err := upsertSubagentRow(db, df, summary, meta); err != nil {
		logger.Warn("claude sessions: failed to upsert sub-agent cache row",
			"file", df.filePath, "error", err)
		return false
	}
	return true
}

func upsertSubagentRow(db *sql.DB, df diskFile, s *ClaudeSessionSummary, meta subagentMeta) error {
	ctx := context.Background()
	_, err := db.ExecContext(ctx, `
		INSERT INTO claude_subagent_cache (
			parent_session_id, agent_id, file_path, file_mtime,
			agent_type, description, tool_use_id,
			start_time, last_activity, message_count,
			input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
			cache_creation_5m_tokens, cache_creation_1h_tokens,
			model
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(parent_session_id, agent_id) DO UPDATE SET
			file_path = excluded.file_path,
			file_mtime = excluded.file_mtime,
			agent_type = excluded.agent_type,
			description = excluded.description,
			tool_use_id = excluded.tool_use_id,
			start_time = excluded.start_time,
			last_activity = excluded.last_activity,
			message_count = excluded.message_count,
			input_tokens = excluded.input_tokens,
			output_tokens = excluded.output_tokens,
			cache_creation_tokens = excluded.cache_creation_tokens,
			cache_read_tokens = excluded.cache_read_tokens,
			cache_creation_5m_tokens = excluded.cache_creation_5m_tokens,
			cache_creation_1h_tokens = excluded.cache_creation_1h_tokens,
			model = excluded.model`,
		df.sessionID, df.agentID, df.filePath, df.mtime,
		meta.AgentType, meta.Description, meta.ToolUseID,
		s.StartTime, s.LastActivity, s.MessageCount,
		s.Usage.InputTokens, s.Usage.OutputTokens,
		s.Usage.CacheCreationTokens, s.Usage.CacheReadTokens,
		s.Usage.CacheCreation5mTokens, s.Usage.CacheCreation1hTokens,
		s.Model,
	)
	return err
}

// ListSubagents returns the cached sub-agent transcripts of one session,
// ordered by start time.
func ListSubagents(db *sql.DB, logger *slog.Logger, sessionID string) ([]ClaudeSubagent, error) {
	rows, err := db.QueryContext(context.Background(), `
		SELECT agent_id, agent_type, description, tool_use_id,
		       start_time, last_activity, message_count,
		       input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
		       cache_creation_5m_tokens, cache_creation_1h_tokens, model
		FROM claude_subagent_cache
		WHERE parent_session_id = ?
		ORDER BY start_time`, sessionID)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			logger.Warn("failed to close rows", "error", cerr)
		}
	}()

	subagents := []ClaudeSubagent{}
	for rows.Next() {
		var sa ClaudeSubagent
		if err := rows.Scan(
			&sa.AgentID, &sa.AgentType, &sa.Description, &sa.ToolUseID,
			&sa.StartTime, &sa.LastActivity, &sa.MessageCount,
			&sa.Usage.InputTokens, &sa.Usage.OutputTokens,
			&sa.Usage.CacheCreationTokens, &sa.Usage.CacheReadTokens,
			&sa.Usage.CacheCreation5mTokens, &sa.Usage.CacheCreation1hTokens, &sa.Model,
		); err != nil {
			return nil, err
		}
		subagents = append(subagents, sa)
	}
	return subagents, rows.Err()
}

// SubagentFiles returns the sub-agent transcript paths belonging to the session
// whose own transcript is at sessionFilePath. Returns nil when the session
// delegated nothing.
func SubagentFiles(sessionID, sessionFilePath string) []string {
	subagentsDir := filepath.Join(filepath.Dir(sessionFilePath), sessionID, "subagents")
	entries, err := os.ReadDir(subagentsDir)
	if err != nil {
		return nil
	}
	paths := make([]string, 0, len(entries))
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), jsonlExt) {
			continue
		}
		paths = append(paths, filepath.Join(subagentsDir, e.Name()))
	}
	if len(paths) == 0 {
		return nil
	}
	sort.Strings(paths)
	return paths
}

func updateLastScanned(db *sql.DB, logger *slog.Logger) {
	ctx := context.Background()
	if _, err := db.ExecContext(ctx, `
		INSERT INTO claude_cache_metadata (id, last_scanned_at) VALUES (1, ?)
		ON CONFLICT(id) DO UPDATE SET last_scanned_at = excluded.last_scanned_at`,
		time.Now().UTC(),
	); err != nil {
		logger.Warn("failed to update last_scanned_at", "error", err)
	}
}

// sessionSummarySelect loads every cached session with its sub-agent roll-up
// folded in. The aggregate is a grouped sub-select rather than a join against
// the raw rows so a session with several sub-agents is not multiplied out, and
// COALESCE keeps the LEFT JOIN's NULLs from reaching the int scans.
const sessionSummarySelect = `
	SELECT c.session_id, c.project_path, c.preview, c.custom_title, c.is_favorite,
	       c.start_time, c.last_activity, c.message_count, c.event_count,
	       c.input_tokens, c.output_tokens, c.cache_creation_tokens, c.cache_read_tokens,
	       c.cache_creation_5m_tokens, c.cache_creation_1h_tokens,
	       c.git_branch, c.model, c.cwd, c.native_title, c.ai_title,
	       c.agent_name, c.permission_mode, c.mode, c.relocated_cwd,
	       c.worktree_name, c.worktree_branch, c.original_branch,
	       c.compaction_count, c.dropped_tokens,
	       COALESCE(sa.n, 0), COALESCE(sa.it, 0), COALESCE(sa.ot, 0),
	       COALESCE(sa.cct, 0), COALESCE(sa.crt, 0),
	       COALESCE(sa.c5m, 0), COALESCE(sa.c1h, 0)
	FROM claude_session_cache c
	LEFT JOIN (
		SELECT parent_session_id,
		       COUNT(*) AS n,
		       SUM(input_tokens) AS it,
		       SUM(output_tokens) AS ot,
		       SUM(cache_creation_tokens) AS cct,
		       SUM(cache_read_tokens) AS crt,
		       SUM(cache_creation_5m_tokens) AS c5m,
		       SUM(cache_creation_1h_tokens) AS c1h
		FROM claude_subagent_cache
		GROUP BY parent_session_id
	) sa ON sa.parent_session_id = c.session_id
	ORDER BY c.last_activity DESC`

// querySessionSummaries runs sessionSummarySelect and scans the result. Both
// loadAllSessions and Cache.loadAll share it so the two paths cannot drift.
func querySessionSummaries(db *sql.DB, logger *slog.Logger) ([]ClaudeSessionSummary, error) {
	rows, err := db.QueryContext(context.Background(), sessionSummarySelect)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			logger.Warn("failed to close rows", "error", cerr)
		}
	}()

	sessions := []ClaudeSessionSummary{}
	for rows.Next() {
		var s ClaudeSessionSummary
		if err := rows.Scan(
			&s.SessionID, &s.ProjectPath, &s.Preview, &s.CustomTitle, &s.IsFavorite,
			&s.StartTime, &s.LastActivity, &s.MessageCount, &s.EventCount,
			&s.Usage.InputTokens, &s.Usage.OutputTokens,
			&s.Usage.CacheCreationTokens, &s.Usage.CacheReadTokens,
			&s.Usage.CacheCreation5mTokens, &s.Usage.CacheCreation1hTokens,
			&s.GitBranch, &s.Model, &s.CWD, &s.NativeTitle, &s.AITitle,
			&s.AgentName, &s.PermissionMode, &s.Mode, &s.RelocatedCWD,
			&s.WorktreeName, &s.WorktreeBranch, &s.OriginalBranch,
			&s.CompactionCount, &s.DroppedTokens,
			&s.SubagentCount, &s.SubagentUsage.InputTokens, &s.SubagentUsage.OutputTokens,
			&s.SubagentUsage.CacheCreationTokens, &s.SubagentUsage.CacheReadTokens,
			&s.SubagentUsage.CacheCreation5mTokens, &s.SubagentUsage.CacheCreation1hTokens,
		); err != nil {
			return nil, err
		}
		s.DisplayTitle = s.ResolveDisplayTitle()
		sessions = append(sessions, s)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	attachPRs(db, logger, sessions)
	return sessions, nil
}

// attachPRs fills in each session's linked pull requests with a single query
// over the whole table, rather than one query per session.
func attachPRs(db *sql.DB, logger *slog.Logger, sessions []ClaudeSessionSummary) {
	if len(sessions) == 0 {
		return
	}
	rows, err := db.QueryContext(context.Background(), `
		SELECT session_id, pr_number, pr_url, pr_repository, first_seen_at
		FROM claude_session_pr ORDER BY first_seen_at, pr_url`)
	if err != nil {
		logger.Warn("claude sessions: failed to load linked PRs", "error", err)
		return
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			logger.Warn("failed to close rows", "error", cerr)
		}
	}()

	bySession := make(map[string][]ClaudeSessionPR)
	for rows.Next() {
		var sessionID string
		var pr ClaudeSessionPR
		if err := rows.Scan(&sessionID, &pr.PRNumber, &pr.PRURL, &pr.PRRepository, &pr.FirstSeenAt); err != nil {
			logger.Warn("claude sessions: failed to scan linked PR", "error", err)
			return
		}
		bySession[sessionID] = append(bySession[sessionID], pr)
	}
	if err := rows.Err(); err != nil {
		logger.Warn("claude sessions: failed to read linked PRs", "error", err)
		return
	}
	for i := range sessions {
		sessions[i].PRs = bySession[sessions[i].SessionID]
	}
}

func loadAllSessions(db *sql.DB, logger *slog.Logger) ([]ClaudeSessionSummary, error) {
	return querySessionSummaries(db, logger)
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
	fiveMin, oneHour := splitCacheCreation(msg.Usage)
	usage.InputTokens += msg.Usage.InputTokens
	usage.OutputTokens += msg.Usage.OutputTokens
	usage.CacheCreationTokens += msg.Usage.CacheCreationInputTokens
	usage.CacheCreation5mTokens += fiveMin
	usage.CacheCreation1hTokens += oneHour
	usage.CacheReadTokens += msg.Usage.CacheReadInputTokens
}

// readSessionSummary reads a session JSONL file and extracts lightweight metadata.
func readSessionSummary(sessionID, projectPath, filePath string, logger *slog.Logger) (*ClaudeSessionSummary, error) {
	return readSummaryFile(sessionID, projectPath, filePath, false, logger)
}

// readSubagentSummary reads a sub-agent transcript. Sub-agent files use the
// same event schema as a parent session, but every event in them is flagged
// isSidechain — the marker a parent transcript uses for delegated turns it
// should not count twice. Inside a sub-agent file that marker is universal and
// carries no such meaning, so sidechain user turns are counted here; otherwise
// message_count would silently degrade to assistant-only.
//
// The turn/event split applies here as well: a sub-agent's message_count counts
// genuine turns, not tool_result carriers. Its EventCount is computed but has no
// column in claude_subagent_cache, so it is not persisted — see #196.
func readSubagentSummary(
	sessionID, projectPath, filePath string, logger *slog.Logger,
) (*ClaudeSessionSummary, error) {
	return readSummaryFile(sessionID, projectPath, filePath, true, logger)
}

func readSummaryFile(
	sessionID, projectPath, filePath string, countSidechainUsers bool, logger *slog.Logger,
) (*ClaudeSessionSummary, error) {
	f, err := os.Open(filePath) //nolint:gosec
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := f.Close(); cerr != nil {
			logger.Warn("failed to close file", "file", filePath, "error", cerr)
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
		if json.Unmarshal(sc.Bytes(), &ev) != nil {
			continue
		}
		if ev.Type == "file-history-snapshot" {
			continue
		}

		if boundsSessionTimeRange(ev.Type) {
			tr.update(ev.Timestamp)
		}
		updateMetadataFromEvent(&summary.CWD, &summary.GitBranch, ev)
		processSummaryEvent(summary, ev, countSidechainUsers)
	}

	summary.StartTime = tr.start
	summary.LastActivity = tr.last

	if summary.StartTime.IsZero() {
		return nil, nil
	}
	return summary, sc.Err()
}

// boundsSessionTimeRange reports whether an event type may extend the session's
// start/last-activity range.
//
// This is a denylist rather than an allowlist on purpose. Many event types
// carry timestamps and legitimately bound the range — `queue-operation` and
// `file-history-delta` among them — so enumerating the ones that count would
// silently shrink the range for existing sessions as Claude Code adds types.
//
// Excluded are the events that *describe* the session rather than occur within
// it. The title and metadata events carry no timestamp today, so excluding them
// is a no-op that timeRange's zero check already handles — but a future release
// adding one must not drag start_time backwards. `pr-link` is the exception
// that matters today: it carries a real timestamp which can post-date the last
// conversation event, and letting it extend last_activity would reorder the
// sessions list by something that is not conversation.
func boundsSessionTimeRange(eventType string) bool {
	switch eventType {
	case "custom-title", "ai-title",
		"pr-link",
		"agent-name", "permission-mode", "mode", "relocated", "worktree-state":
		return false
	default:
		return true
	}
}

func processSummaryEvent(summary *ClaudeSessionSummary, ev rawEvent, countSidechainUsers bool) {
	switch ev.Type {
	case "user":
		if ev.IsSidechain && !countSidechainUsers {
			return
		}
		addSummaryUserEvent(summary, ev)
	case "assistant":
		addSummaryAssistantEvent(summary, ev)
	case "pr-link":
		addSummaryPRLink(summary, ev)
	case "system":
		addSummaryCompaction(summary, ev)
	default:
		applySessionMetadata(summary, ev)
	}
}

// applySessionMetadata records the events that describe the session rather than
// occurring within it. Claude Code re-appends every one of them on each resume,
// so unconditional assignment during a sequential read gives last-wins for free
// — which is the correct rule, since the final value is the current one.
func applySessionMetadata(summary *ClaudeSessionSummary, ev rawEvent) {
	switch ev.Type {
	case "custom-title":
		summary.NativeTitle = ev.CustomTitle
	case "ai-title":
		summary.AITitle = ev.AITitle
	case "agent-name":
		summary.AgentName = ev.AgentName
	case "permission-mode":
		summary.PermissionMode = ev.PermissionMode
	case "mode":
		summary.Mode = ev.Mode
	case "relocated":
		summary.RelocatedCWD = ev.RelocatedCWD
	case "worktree-state":
		if ev.WorktreeSession != nil {
			summary.WorktreeName = ev.WorktreeSession.WorktreeName
			summary.WorktreeBranch = ev.WorktreeSession.WorktreeBranch
			summary.OriginalBranch = ev.WorktreeSession.OriginalBranch
		}
	}
}

// addSummaryPRLink records a linked pull request, deduplicated by URL. Claude
// Code re-emits the event on every resume, so the same PR appears many times in
// one file; the earliest sighting keeps its timestamp.
func addSummaryPRLink(summary *ClaudeSessionSummary, ev rawEvent) {
	if ev.PRUrl == "" {
		return
	}
	for _, pr := range summary.PRs {
		if pr.PRURL == ev.PRUrl {
			return
		}
	}
	summary.PRs = append(summary.PRs, ClaudeSessionPR{
		PRNumber:     ev.PRNumber,
		PRURL:        ev.PRUrl,
		PRRepository: ev.PRRepository,
		FirstSeenAt:  ev.Timestamp,
	})
}

// addSummaryCompaction records a conversation compaction. Only the
// compact_boundary subtype carries compaction metadata; every other system
// subtype is ignored here.
//
// CumulativeDroppedTokens is a running total across the session, so the largest
// value seen is the session's figure — summing would multiply-count.
func addSummaryCompaction(summary *ClaudeSessionSummary, ev rawEvent) {
	if ev.Subtype != "compact_boundary" || ev.CompactMetadata == nil {
		return
	}
	summary.CompactionCount++
	if d := ev.CompactMetadata.CumulativeDroppedTokens; d > 0 {
		if d > summary.DroppedTokens {
			summary.DroppedTokens = d
		}
		return
	}
	// Older Claude Code releases omit cumulativeDroppedTokens while still
	// reporting preTokens/postTokens. Reporting zero there would be a visibly
	// wrong headline number — one real transcript compacts 1,000,563 tokens
	// down to 26,087 — so this boundary's own drop is accumulated instead.
	if dropped := ev.CompactMetadata.PreTokens - ev.CompactMetadata.PostTokens; dropped > 0 {
		summary.DroppedTokens += dropped
	}
}

// addSummaryUserEvent records one user event. Every event bumps EventCount, but
// only genuine human input counts as a message — the bulk of user events merely
// carry tool_result blocks back to the model.
func addSummaryUserEvent(summary *ClaudeSessionSummary, ev rawEvent) {
	summary.EventCount++
	if ev.Message == nil || !isUserTurnContent(ev.Message.Content) {
		return
	}
	summary.MessageCount++
	// Seeding the preview is gated on the same predicate, so it can never be
	// taken from a tool_result carrier.
	if summary.Preview == "" {
		summary.Preview = truncateRunes(extractTextContent(ev.Message.Content), previewMaxRunes)
	}
}

// addSummaryAssistantEvent records one assistant event: always an event, but a
// message only when it contains text the user actually saw.
func addSummaryAssistantEvent(summary *ClaudeSessionSummary, ev rawEvent) {
	summary.EventCount++
	if ev.Message == nil {
		return
	}
	if isAssistantReply(ev.Message.Content) {
		summary.MessageCount++
	}
	if summary.Model == "" && ev.Message.Model != "" {
		summary.Model = ev.Message.Model
	}
	addAssistantUsage(&summary.Usage, ev.Message)
}

// GetSessionDetail reads the full session JSONL and builds the complete message list.
// Returns nil if the session is not found.
func GetSessionDetail(sessionID string, logger *slog.Logger) (*ClaudeSessionDetail, error) {
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
			projectPath := DecodeProjectPath(e.Name())
			return readSessionDetail(sessionID, projectPath, filePath, logger)
		}
	}
	return nil, nil
}

// readSessionDetail reads a session JSONL file and builds the full detail including
// message tree with progress events nested under their parent assistant turns.
func readSessionDetail(sessionID, projectPath, filePath string, logger *slog.Logger) (*ClaudeSessionDetail, error) {
	f, err := os.Open(filePath) //nolint:gosec
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := f.Close(); cerr != nil {
			logger.Warn("failed to close file", "file", filePath, "error", cerr)
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
		if json.Unmarshal(sc.Bytes(), &ev) != nil {
			continue
		}
		if ev.Type == "file-history-snapshot" {
			continue
		}
		// Same denylist as the summary read, so the detail view's start/end
		// cannot disagree with the list's for the same session.
		if boundsSessionTimeRange(ev.Type) {
			tr.update(ev.Timestamp)
		}
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
	// The detail reader walks the same file as the summary reader, so it
	// collects the session's own metadata directly rather than reading it back
	// from the cache. That keeps the two views in agreement by construction, and
	// makes the detail correct even for a session the scanner has not reached.
	case "pr-link":
		addSummaryPRLink(&detail.ClaudeSessionSummary, ev)
	case "system":
		addSummaryCompaction(&detail.ClaudeSessionSummary, ev)
	default:
		applySessionMetadata(&detail.ClaudeSessionSummary, ev)
	}
	return topLevel
}

func processDetailUserEvent(detail *ClaudeSessionDetail, ev rawEvent, topLevel []ClaudeMessage) []ClaudeMessage {
	// Sidechain user turns belong to delegated sub-agents, which are read from
	// <session-id>/subagents/ and reported separately — see ClaudeSubagent.
	if ev.IsSidechain {
		return topLevel
	}
	content := ""
	if ev.Message != nil {
		content = extractTextContent(ev.Message.Content)
	}
	// Every event stays in the rendered list; only the counters distinguish
	// genuine turns from tool_result carriers — see ClaudeSessionSummary.
	detail.EventCount++
	if ev.Message != nil && isUserTurnContent(ev.Message.Content) {
		detail.MessageCount++
	}
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
	detail.EventCount++
	if ev.Message != nil && isAssistantReply(ev.Message.Content) {
		detail.MessageCount++
	}
	return append(topLevel, msg)
}

func populateAssistantUsage(msg *ClaudeMessage, detail *ClaudeSessionDetail, rawMsg *rawMessage) {
	if rawMsg.Usage == nil {
		return
	}
	fiveMin, oneHour := splitCacheCreation(rawMsg.Usage)
	u := TokenUsage{
		InputTokens:           rawMsg.Usage.InputTokens,
		OutputTokens:          rawMsg.Usage.OutputTokens,
		CacheCreationTokens:   rawMsg.Usage.CacheCreationInputTokens,
		CacheCreation5mTokens: fiveMin,
		CacheCreation1hTokens: oneHour,
		CacheReadTokens:       rawMsg.Usage.CacheReadInputTokens,
	}
	msg.Usage = &u
	detail.Usage.InputTokens += u.InputTokens
	detail.Usage.OutputTokens += u.OutputTokens
	detail.Usage.CacheCreationTokens += u.CacheCreationTokens
	detail.Usage.CacheCreation5mTokens += u.CacheCreation5mTokens
	detail.Usage.CacheCreation1hTokens += u.CacheCreation1hTokens
	detail.Usage.CacheReadTokens += u.CacheReadTokens
}

func populateAssistantBlocks(msg *ClaudeMessage, rawMsg *rawMessage) {
	var blocks []rawContentBlock
	if json.Unmarshal(rawMsg.Content, &blocks) != nil {
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
	if json.Unmarshal(raw, &s) == nil {
		return s
	}
	// Try array of content blocks; concatenate text blocks.
	var blocks []rawContentBlock
	if json.Unmarshal(raw, &blocks) != nil {
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
	if json.Unmarshal(data, &todos) != nil {
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
