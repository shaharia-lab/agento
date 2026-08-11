package claudesessions

import (
	"bufio"
	"context"
	"database/sql"
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/shaharia-lab/agento/internal/config"
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
	// v5: cost is accumulated per assistant message against the pricing catalog
	// (#186), replacing first-seen-model whole-session pricing. Cost is not
	// cached — it is recomputed at read time — but the bump is still required:
	// it re-reads every transcript, and the resulting session events are what
	// reprocess the insight rows that carry stored costs.
	// v6: that per-message cost is now *stored* on the row (#188) so the list,
	// the detail page and the analytics totals read one number. Rows written
	// before v6 have zeros in every cost column, which is indistinguishable from
	// a genuinely free session, so they must be re-read rather than left alone.
	// v7: user events whose string content opens with a Claude Code injection
	// wrapper (task-notification, command-message, command-name,
	// local-command-caveat, local-command-stdout, system-reminder) no longer
	// count as messages (#197) — rows written before v7 count that machine
	// chatter as human turns, and may also have taken their preview from it.
	// v8: a sub-agent's event_count is persisted (#196) — the value was always
	// computed and then dropped for want of a column, so every sub-agent row
	// written before v8 reads back zero, which is indistinguishable from an
	// empty transcript.
	// v10: a session whose only user content is an injected wrapper takes its
	// preview from the command or skill name inside it rather than the raw
	// wrapper text, so rows stop reading "Base directory for this skill: …".
	// Stored previews written before v10 hold the raw text.
	// v9: each session's cost is also stored keyed by the model that spent it
	// (cost_by_model). It can only be produced while the transcript is being
	// priced — a stored total carries neither the model nor the timestamp of the
	// messages behind it — so rows written before v9 have an empty breakdown
	// that no later pass could fill in.
	// v11: the injected user events that arrive as *array* content — the
	// skill-invocation preamble and the "[Request interrupted by user]" notice
	// — stop counting as messages too (#226). v7 covered string content only,
	// so rows written before v11 still count that machine chatter as human
	// turns, and their previews were taken from the genuine-turn branch.
	// v12: active_duration_ms is stored for every transcript — parent and
	// sub-agent alike — as the sum of inter-event gaps capped at
	// IdleGapThreshold. Sessions are resumable, so the start/last span counts
	// every idle day between sittings; rows written before v12 hold 0, which is
	// indistinguishable from a single-event session.
	CurrentScannerVersion = 12
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

// ClaudeHome returns the Claude config dir agent runs target by default.
//
// Reads that need the whole corpus use ClaudeHomes instead; this remains for
// the single-dir lookups where one answer is the right answer.
func ClaudeHome() string {
	return config.ClaudeRunConfigDir()
}

// ClaudeHomes returns every Claude config dir Agento indexes, default first.
//
// Claude Code supports running several accounts side by side via
// CLAUDE_CONFIG_DIR, and analytics is retrospective: a machine with two
// accounts wants both corpora in every total, or "how much have I spent" answers
// only for whichever account happens to be selected right now.
func ClaudeHomes() []string {
	return config.ClaudeConfigDirs()
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

// ScanAllSessions scans all project directories and returns summaries for all sessions.
// Sessions are sorted by last activity, most recent first.
func ScanAllSessions(logger *slog.Logger) ([]ClaudeSessionSummary, error) {
	var sessions []ClaudeSessionSummary
	for _, dir := range ClaudeHomes() {
		projectsDir := filepath.Join(dir, "projects")
		entries, err := os.ReadDir(projectsDir)
		if err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return nil, err
		}
		for _, e := range entries {
			if !e.IsDir() {
				continue
			}
			sessions = scanProjectSessions(projectsDir, e.Name(), sessions, logger)
		}
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
		summary, _, err := readSessionSummary(sessionID, projectPath, filePath, logger)
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

	// configDir is the Claude config dir this file was found under. Stored on
	// the row so a session can be attributed and filtered by account, and so a
	// dir that fails to walk can have its rows protected from the delete pass.
	configDir string
}

// cachedEntry holds a cached file's path and modification time. isSubagent
// records which table the row came from, so deletions — where the file is gone
// and no diskFile survives — can still be routed to the right table. configDir
// records which config dir the row was indexed from, so a dir that could not be
// walked this scan can be excluded from deletion.
type cachedEntry struct {
	filePath   string
	mtime      time.Time
	isSubagent bool
	configDir  string
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
	return IncrementalScanWith(db, logger, ScanOptions{Notify: notify})
}

// ScanOptions carries the scan's optional callbacks. A struct rather than more
// parameters because the two have nothing to do with each other and most
// callers want neither.
type ScanOptions struct {
	// Notify is called once per session that was inserted or updated.
	Notify func(sessionID, filePath string, isNew bool)
	// Progress reports how many of the scan's files have been written, and how
	// many there are. It is called once before any work and once per committed
	// batch, so a first run on a large corpus can show something moving instead
	// of two minutes of silence.
	Progress func(done, total int)
}

// IncrementalScanWith is the full scan entry point.
func IncrementalScanWith(
	db *sql.DB, logger *slog.Logger, opts ScanOptions,
) ([]ClaudeSessionSummary, error) {
	dirs := ClaudeHomes()
	walk := walkAllDiskFiles(dirs, logger)
	onDisk := walk.files

	if len(walk.walked) == 0 {
		// Not one configured dir could be listed. Previously this was the
		// "wipe the cache" path, on the reasoning that a missing ~/.claude
		// means the user deleted their sessions. With several dirs that
		// inference no longer holds — an unplugged drive looks identical —
		// so leave every row alone and let the next scan reconcile.
		logger.Warn("claude sessions: no readable claude config dir, keeping cached rows",
			"dirs", dirs)
		updateLastScanned(db, logger)
		return loadAllSessions(db, logger)
	}

	cached, err := loadCachedEntries(db, logger)
	if err != nil {
		return nil, err
	}

	stale := detectStaleness(db)
	stale.invalidate(db, cached, logger)

	diff := diffDiskAndCache(onDisk, cached, walk.walked)
	applyChangesWithNotify(db, logger, onDisk, diff, opts.Notify, opts.Progress)

	// The scan has just walked every project directory, so the project list it
	// implies is free here and costs 500 ReadDir round trips per request
	// otherwise.
	cacheProjects(projectsFromDiskFiles(onDisk))
	updateLastScanned(db, logger)
	stale.record(db, logger)
	return loadAllSessions(db, logger)
}

// cacheStaleness collects every reason the cached rows may no longer describe
// what the current code would produce from the same files. All three share one
// remedy — re-read every transcript, since no file mtime changed — and one
// ordering rule: the new marker is written only after the re-read that earns
// it, so a failed scan leaves the drift recorded and retryable.
type cacheStaleness struct {
	// reader is a CurrentScannerVersion bump: cached rows are missing data the
	// current reader extracts, so mtime comparison wrongly reports them
	// unchanged.
	reader bool
	// pricing is a catalog edit (#188 stores cost per row, so a rate change
	// reaches nothing on its own).
	pricing    bool
	pricingRev int64
	// idle is a change to the user's idle-gap threshold, which redefines every
	// stored active duration and the insight rows derived from them.
	idle   bool
	idleMs int64
}

func detectStaleness(db *sql.DB) cacheStaleness {
	rev, stalePricing := pricingStaleness(db)
	idleMs, staleIdle := idleThresholdStaleness(db)
	return cacheStaleness{
		reader:     storedScannerVersion(db) < CurrentScannerVersion,
		pricing:    stalePricing,
		pricingRev: rev,
		idle:       staleIdle,
		idleMs:     idleMs,
	}
}

func (s cacheStaleness) any() bool { return s.reader || s.pricing || s.idle }

// invalidate forces the re-read each kind of staleness needs, plus the insight
// reprocessing that only a threshold change implies.
func (s cacheStaleness) invalidate(db *sql.DB, cached map[string]cachedEntry, logger *slog.Logger) {
	if !s.any() {
		return
	}
	invalidateCachedMtimes(cached, logger, s.reader, s.pricing, s.idle)
	if s.idle {
		invalidateInsightsForIdleThreshold(db, logger)
	}
}

// record stores the markers the next scan compares against.
func (s cacheStaleness) record(db *sql.DB, logger *slog.Logger) {
	if s.reader {
		recordScannerVersion(db, logger)
	}
	if s.pricing {
		recordPricingRevision(db, logger, s.pricingRev)
	}
	if s.idle {
		recordIdleThreshold(db, logger, s.idleMs)
	}
}

// invalidateCachedMtimes zeroes every cached mtime so the next diff treats all
// files as modified, forcing a re-read after a scanner-version bump.
//
// The entries are invalidated rather than dropped: the files themselves are
// unchanged — only the rows are incomplete — so they must re-read as updates to
// existing sessions rather than as newly discovered ones, and a row whose file
// is gone must still be detected as a deletion.
func invalidateCachedMtimes(
	cached map[string]cachedEntry, logger *slog.Logger, staleReader, stalePricing, staleIdle bool,
) {
	if len(cached) == 0 {
		return
	}
	logger.Info("claude sessions: re-reading all transcripts",
		"scanner_version_bumped", staleReader, "pricing_changed", stalePricing,
		"idle_threshold_changed", staleIdle,
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

// pricingStaleness reports the live catalog fingerprint and whether the cached
// costs were computed under a different one.
//
// Since #188 cost is stored on the row rather than recomputed per read, so a
// catalog edit no longer reaches cached sessions by itself. Re-pricing needs
// each message's own model and timestamp, which the row does not keep — so the
// only correct response is to re-read the transcripts, exactly as a scanner
// bump does. Rate edits are rare; a silently stale cost is not.
func pricingStaleness(db *sql.DB) (live int64, stale bool) {
	live = currentPricingRevision()
	if live == pricingRevUnknown {
		return live, false
	}
	return live, storedPricingRevision(db) != live
}

// storedPricingRevision returns the catalog fingerprint the cached costs were
// computed under, or 0 when unreadable — which differs from any real revision
// and so correctly forces a re-cost.
func storedPricingRevision(db *sql.DB) int64 {
	var v int64
	row := db.QueryRowContext(context.Background(),
		"SELECT pricing_rev FROM claude_cache_metadata WHERE id = 1")
	if row.Scan(&v) != nil {
		return 0
	}
	return v
}

func recordPricingRevision(db *sql.DB, logger *slog.Logger, rev int64) {
	if _, err := db.ExecContext(context.Background(),
		"UPDATE claude_cache_metadata SET pricing_rev = ? WHERE id = 1", rev,
	); err != nil {
		logger.Warn("claude sessions: failed to record pricing revision", "error", err)
	}
}

// idleThresholdStaleness reports the configured idle-gap threshold in
// milliseconds and whether the cached durations were computed under a
// different one.
//
// Active duration is stored per transcript, not derived on read, so changing
// what counts as continuous work cannot reach a cached row by itself — no
// transcript mtime changes because the user moved a slider. This is the same
// mechanism scanner_version and pricing_rev use, and for the same reason: the
// only correct response is to re-read, since recomputing needs every event's
// timestamp and the row keeps two.
func idleThresholdStaleness(db *sql.DB) (live int64, stale bool) {
	live = IdleGapThreshold().Milliseconds()
	return live, storedIdleThresholdMs(db) != live
}

// storedIdleThresholdMs returns the threshold the cached durations were
// computed under. Zero — an unreadable value, or a row written before the
// column existed — differs from every valid threshold and so correctly forces
// one re-read.
func storedIdleThresholdMs(db *sql.DB) int64 {
	var v int64
	row := db.QueryRowContext(context.Background(),
		"SELECT idle_threshold_ms FROM claude_cache_metadata WHERE id = 1")
	if row.Scan(&v) != nil {
		return 0
	}
	return v
}

func recordIdleThreshold(db *sql.DB, logger *slog.Logger, ms int64) {
	if _, err := db.ExecContext(context.Background(),
		"UPDATE claude_cache_metadata SET idle_threshold_ms = ? WHERE id = 1", ms,
	); err != nil {
		logger.Warn("claude sessions: failed to record idle threshold", "error", err)
	}
}

// invalidateInsightsForIdleThreshold forces the insight worker to reprocess
// every session after a threshold change.
//
// The stored insight rows carry their own active duration and the rhythm
// averages the threshold gates, and NeedsProcessing selects on
// processor_version alone — so zeroing it is what a version bump would do,
// except that this drift is caused by the user rather than by a release and
// therefore cannot be expressed as a constant. Written from here rather than
// through InsightStorer because it belongs with the re-read it accompanies:
// the two must happen together or the insights disagree with the sessions.
//
// A failure is logged and the scan continues: the threshold is recorded only
// after this runs, so the next scan retries.
func invalidateInsightsForIdleThreshold(db *sql.DB, logger *slog.Logger) {
	res, err := db.ExecContext(context.Background(),
		"UPDATE session_insights SET processor_version = 0")
	if err != nil {
		logger.Warn("claude sessions: failed to invalidate insights after idle-threshold change",
			"error", err)
		return
	}
	if rows, rerr := res.RowsAffected(); rerr == nil && rows > 0 {
		logger.Info("claude sessions: insights queued for reprocessing", "rows", rows)
	}
}

func recordScannerVersion(db *sql.DB, logger *slog.Logger) {
	if _, err := db.ExecContext(context.Background(),
		"UPDATE claude_cache_metadata SET scanner_version = ? WHERE id = 1",
		CurrentScannerVersion,
	); err != nil {
		logger.Warn("claude sessions: failed to record scanner version", "error", err)
	}
}

// diskWalk is the result of walking every configured Claude config dir.
//
// walked records which dirs produced a complete listing. It is not
// bookkeeping: a cached row whose config dir is absent from this set must be
// excluded from the delete pass, because "no file on disk" and "we could not
// look" are indistinguishable to diffDiskAndCache and only one of them means
// the session is gone. An unmounted home would otherwise delete its whole
// corpus — including custom_title and is_favorite, the two user-owned columns
// a rescan deliberately preserves.
type diskWalk struct {
	files  map[string]diskFile
	walked map[string]struct{}
}

// walkAllDiskFiles walks every configured config dir into one set.
//
// Failure is isolated per dir on purpose. A dir that cannot be listed is
// skipped and left out of walked, so the rest of the scan proceeds and that
// dir's rows are protected rather than deleted.
func walkAllDiskFiles(dirs []string, logger *slog.Logger) diskWalk {
	w := diskWalk{
		files:  make(map[string]diskFile),
		walked: make(map[string]struct{}, len(dirs)),
	}
	// Tracks which (session, project) pair a config dir already claimed, so a
	// corpus copied between dirs is indexed once. See claimSession.
	claimed := make(map[string]string)

	for _, dir := range dirs {
		if walkOneDir(dir, w.files, claimed, logger) {
			w.walked[dir] = struct{}{}
		}
	}
	return w
}

// walkOneDir collects one config dir's transcripts, reporting whether it was
// listed end to end.
//
// Only a dir listed completely may have its rows reconciled: reconciling a
// partial listing deletes sessions that are present but could not be seen.
func walkOneDir(
	dir string, onDisk map[string]diskFile, claimed map[string]string, logger *slog.Logger,
) bool {
	projectsDir := filepath.Join(dir, "projects")
	entries, err := os.ReadDir(projectsDir)
	if err != nil {
		// A config dir that exists but has no projects/ has genuinely never
		// run a session: it walked fine and contributed nothing, and its
		// (nonexistent) rows are safe to reconcile. A config dir that is
		// itself missing or unreadable is a different thing entirely — the
		// user may have unplugged a drive — and must protect its rows.
		if os.IsNotExist(err) && dirExists(dir) {
			return true
		}
		logger.Warn("claude sessions: skipping unreadable config dir",
			"config_dir", dir, "error", err)
		return false
	}

	complete := true
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		if !collectProjectDiskFiles(dir, projectsDir, e.Name(), onDisk, claimed, logger) {
			complete = false
		}
	}
	if !complete {
		logger.Warn("claude sessions: config dir listed incompletely, "+
			"its cached rows are preserved this scan", "config_dir", dir)
	}
	return complete
}

// dirExists reports whether path is an existing directory.
func dirExists(path string) bool {
	info, err := os.Stat(path)
	return err == nil && info.IsDir()
}

// claimSession decides whether a config dir may index a (session, project)
// pair, recording the winner so later dirs lose.
//
// The same session appearing under two config dirs is one session, not two:
// the ordinary way to set up a second account is to copy the first dir, which
// duplicates every session id under the same project paths. Indexing both
// would double that corpus's tokens and cost in every total, and — because
// claude_session_cache is keyed on (session_id, project_path) while file_path
// is only a non-unique index — would also leave the losing path permanently
// classified as an insert, re-firing EventSessionDiscovered on every scan.
// Dirs are walked default-first, so the default dir wins ties.
func claimSession(claimed map[string]string, key, dir string) bool {
	owner, seen := claimed[key]
	if !seen {
		claimed[key] = dir
		return true
	}
	return owner == dir
}

func collectProjectDiskFiles(
	configDir, projectsDir, dirName string,
	onDisk map[string]diskFile,
	claimed map[string]string,
	logger *slog.Logger,
) bool {
	projectPath := DecodeProjectPath(dirName)
	projectDir := filepath.Join(projectsDir, dirName)
	files, err := os.ReadDir(projectDir)
	if err != nil {
		// Previously silent. A project dir that cannot be listed drops every
		// file under it from onDisk, which diffDiskAndCache reads as "deleted"
		// — so it is logged, and reported as an incomplete listing so the
		// caller protects this config dir's rows rather than reconciling them.
		logger.Warn("claude sessions: skipping unreadable project dir",
			"project_dir", projectDir, "error", err)
		return false
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
		if !claimSession(claimed, sessionID+"\x00"+projectPath, configDir) {
			// Another config dir already indexed this exact session. Skipping
			// it here also keeps its sub-agent directory out, since pass 2
			// only descends into ids collected in pass 1.
			continue
		}
		sessionIDs[sessionID] = struct{}{}
		fp := filepath.Join(projectDir, f.Name())
		onDisk[fp] = diskFile{
			sessionID:   sessionID,
			projectPath: projectPath,
			filePath:    fp,
			mtime:       info.ModTime().UTC(),
			configDir:   configDir,
		}
	}

	for _, f := range files {
		if !f.IsDir() {
			continue
		}
		if _, ok := sessionIDs[f.Name()]; !ok {
			continue
		}
		collectSubagentDiskFiles(configDir, projectDir, f.Name(), projectPath, onDisk)
	}
	return true
}

// collectSubagentDiskFiles emits one diskFile per sub-agent transcript under
// <projectDir>/<sessionID>/subagents/. Claude Code moved delegated work out of
// the parent JSONL into this directory; a session with no such directory simply
// contributes nothing.
func collectSubagentDiskFiles(
	configDir, projectDir, sessionID, projectPath string, onDisk map[string]diskFile,
) {
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
			configDir:      configDir,
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
	rows, err := db.QueryContext(context.Background(),
		"SELECT file_path, file_mtime, config_dir FROM "+table)
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
		if err := rows.Scan(&ce.filePath, &ce.mtime, &ce.configDir); err != nil {
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

// diffDiskAndCache classifies every file into insert, update or delete.
//
// walked is the set of config dirs that produced a complete listing. A cached
// row belonging to any other dir is left untouched: its file's absence from
// onDisk means "we could not look", not "it is gone". A row whose config dir is
// blank predates the column and belongs to the default dir, which is always in
// the walk set when it is readable.
func diffDiskAndCache(
	onDisk map[string]diskFile, cached map[string]cachedEntry, walked map[string]struct{},
) diskDiff {
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
		if _, exists := onDisk[fp]; exists {
			continue
		}
		if !rowReconcilable(ce.configDir, walked) {
			continue
		}
		d.toDelete = append(d.toDelete, ce)
	}
	return d
}

// rowReconcilable reports whether a cached row's absence from disk can be
// trusted as a deletion.
func rowReconcilable(configDir string, walked map[string]struct{}) bool {
	if configDir == "" {
		// Pre-migration rows carry the default dir after the backfill; a blank
		// here means a row written by a path that did not stamp one. Trust the
		// default dir's walk for it, which is the dir it must have come from.
		configDir = config.DefaultClaudeConfigDir()
	}
	_, ok := walked[configDir]
	return ok
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
			compaction_count, dropped_tokens,
			input_cost_usd, output_cost_usd, cache_read_cost_usd,
			cache_write_cost_usd, total_cost_usd, unpriced_models, unpriced_tokens,
			cost_by_model, active_duration_ms, config_dir
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
		          ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
			dropped_tokens = excluded.dropped_tokens,
			input_cost_usd = excluded.input_cost_usd,
			output_cost_usd = excluded.output_cost_usd,
			cache_read_cost_usd = excluded.cache_read_cost_usd,
			cache_write_cost_usd = excluded.cache_write_cost_usd,
			total_cost_usd = excluded.total_cost_usd,
			unpriced_models = excluded.unpriced_models,
			unpriced_tokens = excluded.unpriced_tokens,
			cost_by_model = excluded.cost_by_model,
			active_duration_ms = excluded.active_duration_ms,
			config_dir = excluded.config_dir`,
		cacheRowArgs(df, s)...,
	)
	return err
}

// cacheRowArgs lays out the cache row's bind values in column order.
//
// custom_title and is_favorite are intentionally absent from both the INSERT
// and the UPDATE SET so any user-defined values are preserved across rescans.
//
// native_title and ai_title are deliberately NOT excluded: they mirror Claude
// Code's own title events and must track them, including when a native rename
// changes or clears one. They cannot clobber a user's Agento rename because
// that lives in the separate custom_title column and wins the precedence in
// ResolveDisplayTitle.
func cacheRowArgs(df diskFile, s *ClaudeSessionSummary) []any {
	return []any{
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
		s.Cost.InputUSD, s.Cost.OutputUSD, s.Cost.CacheReadUSD,
		s.Cost.CacheWriteUSD, s.Cost.TotalUSD,
		encodeUnpricedModels(s.UnpricedModels), s.UnpricedTokens,
		encodeCostByModel(s.CostByModel), s.ActiveDurationMs, df.configDir,
	}
}

// encodeCostByModel serializes the per-model cost breakdown for its column.
//
// An empty breakdown stores "" rather than "{}" so the column's default and a
// scanned-but-costless session are the same value, and neither is mistaken for
// data. Marshaling a map of plain float structs cannot fail, but the error is
// still handled rather than ignored — silently storing "" would turn a
// serialization bug into a chart that is quietly missing a session.
func encodeCostByModel(byModel map[string]SessionCost) string {
	if len(byModel) == 0 {
		return ""
	}
	b, err := json.Marshal(byModel)
	if err != nil {
		return ""
	}
	return string(b)
}

// decodeCostByModel is the inverse. Malformed or empty JSON yields nil: the
// breakdown re-keys a total that is stored independently, so losing it costs a
// chart's detail rather than any money — dropping the whole session row over
// one bad blob would be the worse failure.
func decodeCostByModel(raw string) map[string]SessionCost {
	if raw == "" || raw == "{}" {
		return nil
	}
	var out map[string]SessionCost
	if err := json.Unmarshal([]byte(raw), &out); err != nil {
		return nil
	}
	return out
}

// encodeUnpricedModels joins the list for storage. Newline-separated because a
// model ID may contain almost anything else — "mixedbread-ai/mxbai-embed-large-v1"
// already carries a slash — but never a newline.
func encodeUnpricedModels(models []string) string {
	return strings.Join(models, "\n")
}

// mergeUnpricedModels combines the session's own unpriced models with those of
// its sub-agents, deduplicated and sorted. The sub-agent side arrives from a
// GROUP_CONCAT, so the same model can appear once per delegating sub-agent.
// Returns nil when fully priced, so the JSON key is omitted entirely.
func mergeUnpricedModels(own, delegated string) []string {
	seen := map[string]struct{}{}
	for _, group := range []string{own, delegated} {
		if group == "" {
			continue
		}
		for _, m := range strings.Split(group, "\n") {
			if m != "" {
				seen[m] = struct{}{}
			}
		}
	}
	if len(seen) == 0 {
		return nil
	}
	out := make([]string, 0, len(seen))
	for m := range seen {
		out = append(out, m)
	}
	sort.Strings(out)
	return out
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
// upsertSubagentRow writes one delegated transcript's cache row. It takes an
// execer rather than a *sql.DB so the scan's batching writer can run it inside
// the same transaction as the session rows around it.
func upsertSubagentRow(
	ctx context.Context, db execer, df diskFile, s *ClaudeSessionSummary, meta subagentMeta,
) error {
	_, err := db.ExecContext(ctx, `
		INSERT INTO claude_subagent_cache (
			parent_session_id, agent_id, file_path, file_mtime,
			agent_type, description, tool_use_id,
			start_time, last_activity, message_count, event_count,
			input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
			cache_creation_5m_tokens, cache_creation_1h_tokens,
			model,
			input_cost_usd, output_cost_usd, cache_read_cost_usd,
			cache_write_cost_usd, total_cost_usd, unpriced_models, unpriced_tokens,
			active_duration_ms, config_dir
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(parent_session_id, agent_id) DO UPDATE SET
			file_path = excluded.file_path,
			file_mtime = excluded.file_mtime,
			agent_type = excluded.agent_type,
			description = excluded.description,
			tool_use_id = excluded.tool_use_id,
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
			model = excluded.model,
			input_cost_usd = excluded.input_cost_usd,
			output_cost_usd = excluded.output_cost_usd,
			cache_read_cost_usd = excluded.cache_read_cost_usd,
			cache_write_cost_usd = excluded.cache_write_cost_usd,
			total_cost_usd = excluded.total_cost_usd,
			unpriced_models = excluded.unpriced_models,
			unpriced_tokens = excluded.unpriced_tokens,
			active_duration_ms = excluded.active_duration_ms,
			config_dir = excluded.config_dir`,
		df.sessionID, df.agentID, df.filePath, df.mtime,
		meta.AgentType, meta.Description, meta.ToolUseID,
		s.StartTime, s.LastActivity, s.MessageCount, s.EventCount,
		s.Usage.InputTokens, s.Usage.OutputTokens,
		s.Usage.CacheCreationTokens, s.Usage.CacheReadTokens,
		s.Usage.CacheCreation5mTokens, s.Usage.CacheCreation1hTokens,
		s.Model,
		s.Cost.InputUSD, s.Cost.OutputUSD, s.Cost.CacheReadUSD,
		s.Cost.CacheWriteUSD, s.Cost.TotalUSD,
		encodeUnpricedModels(s.UnpricedModels), s.UnpricedTokens,
		s.ActiveDurationMs, df.configDir,
	)
	return err
}

// ListSubagents returns the cached sub-agent transcripts of one session,
// ordered by start time.
func ListSubagents(db *sql.DB, logger *slog.Logger, sessionID string) ([]ClaudeSubagent, error) {
	rows, err := db.QueryContext(context.Background(), `
		SELECT agent_id, agent_type, description, tool_use_id,
		       start_time, last_activity, message_count, event_count,
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
			&sa.StartTime, &sa.LastActivity, &sa.MessageCount, &sa.EventCount,
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

// sessionSummaryColumns is the projection every reader of a session summary
// shares, so the list, the detail page and the paged query cannot drift into
// reporting different figures for the same row.
const sessionSummaryColumns = `
	SELECT c.session_id, c.project_path, c.preview, c.custom_title, c.is_favorite,
	       c.start_time, c.last_activity, c.message_count, c.event_count,
	       c.input_tokens, c.output_tokens, c.cache_creation_tokens, c.cache_read_tokens,
	       c.cache_creation_5m_tokens, c.cache_creation_1h_tokens,
	       c.git_branch, c.model, c.cwd, c.native_title, c.ai_title,
	       c.agent_name, c.permission_mode, c.mode, c.relocated_cwd,
	       c.worktree_name, c.worktree_branch, c.original_branch,
	       c.compaction_count, c.dropped_tokens,
	       c.input_cost_usd, c.output_cost_usd, c.cache_read_cost_usd,
	       c.cache_write_cost_usd, c.total_cost_usd, c.unpriced_models, c.unpriced_tokens,
	       c.cost_by_model, c.active_duration_ms, c.config_dir,
	       COALESCE(sa.n, 0), COALESCE(sa.it, 0), COALESCE(sa.ot, 0),
	       COALESCE(sa.cct, 0), COALESCE(sa.crt, 0),
	       COALESCE(sa.c5m, 0), COALESCE(sa.c1h, 0),
	       COALESCE(sa.ic, 0), COALESCE(sa.oc, 0), COALESCE(sa.crc, 0),
	       COALESCE(sa.cwc, 0), COALESCE(sa.tc, 0), COALESCE(sa.ut, 0),
	       COALESCE(sa.um, ''), COALESCE(sa.adm, 0)`

// sessionSummarySource is the FROM/JOIN half, split out so an aggregate can
// reuse it without the projection.
//
// The sub-agent roll-up is a grouped sub-select rather than a join against the
// raw rows so a session with several sub-agents is not multiplied out, and
// COALESCE keeps the LEFT JOIN's NULLs from reaching the int scans. Its column
// aliases (it, ot, tc, adm, …) are what session_query.go's metric expressions
// are written against.
const sessionSummarySource = `
	FROM claude_session_cache c
	LEFT JOIN (
		SELECT parent_session_id,
		       COUNT(*) AS n,
		       SUM(input_tokens) AS it,
		       SUM(output_tokens) AS ot,
		       SUM(cache_creation_tokens) AS cct,
		       SUM(cache_read_tokens) AS crt,
		       SUM(cache_creation_5m_tokens) AS c5m,
		       SUM(cache_creation_1h_tokens) AS c1h,
		       SUM(input_cost_usd) AS ic,
		       SUM(output_cost_usd) AS oc,
		       SUM(cache_read_cost_usd) AS crc,
		       SUM(cache_write_cost_usd) AS cwc,
		       SUM(total_cost_usd) AS tc,
		       SUM(unpriced_tokens) AS ut,
		       SUM(active_duration_ms) AS adm,
		       -- NULLIF keeps fully-priced sub-agents from contributing blank
		       -- entries; duplicates across sub-agents are deduped in Go.
		       GROUP_CONCAT(NULLIF(unpriced_models, ''), char(10)) AS um
		FROM claude_subagent_cache
		GROUP BY parent_session_id
	) sa ON sa.parent_session_id = c.session_id`

// sessionSummaryFrom is the full unfiltered projection.
const sessionSummaryFrom = sessionSummaryColumns + sessionSummarySource

const sessionSummarySelect = sessionSummaryFrom + `
	ORDER BY c.last_activity DESC`

// sessionSummaryByID is the same projection narrowed to one session, so the
// detail endpoint reads back exactly the figures the list shows. It must stay
// derived from sessionSummaryFrom: the costs are stored rather than recomputed
// (#188), and a second hand-written projection would be free to drift.
const sessionSummaryByID = sessionSummaryFrom + `
	WHERE c.session_id = ?`

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
		s, err := scanSessionSummary(rows)
		if err != nil {
			return nil, err
		}
		sessions = append(sessions, s)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	attachPRs(db, logger, sessions)
	attachSubagentUsageByModel(db, logger, sessions)
	return sessions, nil
}

// attachSubagentUsageByModel fills in each session's delegated token usage
// broken down by the sub-agent's own model.
//
// The summary select's sub-agent roll-up is grouped by parent_session_id alone
// — deliberately, so a session with several sub-agents is not multiplied out —
// which collapses the model dimension before analytics can see it. This is a
// second grouped read that keeps it, run once for the whole load rather than
// per session, in the same shape as attachPRs above.
func attachSubagentUsageByModel(db *sql.DB, logger *slog.Logger, sessions []ClaudeSessionSummary) {
	if len(sessions) == 0 {
		return
	}
	rows, err := db.QueryContext(context.Background(), `
		SELECT parent_session_id, model,
		       SUM(input_tokens), SUM(output_tokens),
		       SUM(cache_creation_tokens), SUM(cache_read_tokens),
		       SUM(cache_creation_5m_tokens), SUM(cache_creation_1h_tokens),
		       SUM(input_cost_usd), SUM(output_cost_usd),
		       SUM(cache_read_cost_usd), SUM(cache_write_cost_usd), SUM(total_cost_usd)
		FROM claude_subagent_cache
		GROUP BY parent_session_id, model`)
	if err != nil {
		logger.Warn("claude sessions: failed to load sub-agent usage by model", "error", err)
		return
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			logger.Warn("failed to close rows", "error", cerr)
		}
	}()

	usage := map[string]map[string]TokenUsage{}
	cost := map[string]map[string]SessionCost{}
	for rows.Next() {
		var sessionID, model string
		var u TokenUsage
		var c SessionCost
		if err := rows.Scan(&sessionID, &model,
			&u.InputTokens, &u.OutputTokens,
			&u.CacheCreationTokens, &u.CacheReadTokens,
			&u.CacheCreation5mTokens, &u.CacheCreation1hTokens,
			&c.InputUSD, &c.OutputUSD,
			&c.CacheReadUSD, &c.CacheWriteUSD, &c.TotalUSD); err != nil {
			logger.Warn("claude sessions: failed to scan sub-agent usage by model", "error", err)
			return
		}
		model = displayModel(model)
		if usage[sessionID] == nil {
			usage[sessionID] = map[string]TokenUsage{}
			cost[sessionID] = map[string]SessionCost{}
		}
		usage[sessionID][model] = u
		cost[sessionID][model] = c
	}
	if err := rows.Err(); err != nil {
		logger.Warn("claude sessions: failed to read sub-agent usage by model", "error", err)
		return
	}

	for i := range sessions {
		if m, ok := usage[sessions[i].SessionID]; ok {
			sessions[i].SubagentUsageByModel = m
			sessions[i].SubagentCostByModel = cost[sessions[i].SessionID]
		}
	}
}

// querySessionSummary returns the cached summary for one session, or nil when
// the scanner has not reached it yet.
//
// Unlike querySessionSummaries this attaches neither the linked PRs nor the
// per-model sub-agent breakdown: its caller (the detail endpoint) reads the
// stored cost off this row and already has both from the transcript it parsed.
// A future caller needing them must attach them explicitly.
func querySessionSummary(db *sql.DB, logger *slog.Logger, sessionID string) (*ClaudeSessionSummary, error) {
	rows, err := db.QueryContext(context.Background(), sessionSummaryByID, sessionID)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			logger.Warn("failed to close rows", "error", cerr)
		}
	}()

	if !rows.Next() {
		return nil, rows.Err()
	}
	s, err := scanSessionSummary(rows)
	if err != nil {
		return nil, err
	}
	return &s, rows.Err()
}

// scanSessionSummary reads one row of the sessionSummaryFrom projection.
func scanSessionSummary(rows *sql.Rows) (ClaudeSessionSummary, error) {
	var s ClaudeSessionSummary
	var unpriced, subagentUnpricedModels, costByModel string
	var subagentUnpriced int
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
		&s.Cost.InputUSD, &s.Cost.OutputUSD, &s.Cost.CacheReadUSD,
		&s.Cost.CacheWriteUSD, &s.Cost.TotalUSD, &unpriced, &s.UnpricedTokens,
		&costByModel, &s.ActiveDurationMs, &s.ConfigDir,
		&s.SubagentCount, &s.SubagentUsage.InputTokens, &s.SubagentUsage.OutputTokens,
		&s.SubagentUsage.CacheCreationTokens, &s.SubagentUsage.CacheReadTokens,
		&s.SubagentUsage.CacheCreation5mTokens, &s.SubagentUsage.CacheCreation1hTokens,
		&s.SubagentCost.InputUSD, &s.SubagentCost.OutputUSD, &s.SubagentCost.CacheReadUSD,
		&s.SubagentCost.CacheWriteUSD, &s.SubagentCost.TotalUSD, &subagentUnpriced,
		&subagentUnpricedModels, &s.SubagentActiveDurationMs,
	); err != nil {
		return s, err
	}
	// Delegated work's unpriced models and tokens both count toward the
	// session's disclosure. Reading one without the other would let a row
	// report excluded tokens attributed to no model — or worse, show a
	// confident total for a session that is only partly priced.
	s.CostByModel = decodeCostByModel(costByModel)
	s.UnpricedModels = mergeUnpricedModels(unpriced, subagentUnpricedModels)
	s.UnpricedTokens += subagentUnpriced
	s.DisplayTitle = s.ResolveDisplayTitle()
	return s, nil
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

// update widens the range to include ts, normalized to UTC.
//
// The normalization is what makes the stored bounds orderable. SQLite holds
// them as the driver's rendering of time.Time — "2026-08-11 07:54:00.097 +0000
// UTC" — and both the ORDER BY behind the sessions list and the keyset
// predicate behind its pagination compare that as text. Lexical order matches
// chronological order only while every value carries the same zone suffix, so a
// transcript written with a non-UTC offset would sort itself into the wrong
// place. Claude Code writes Z-suffixed timestamps, so in practice this changes
// no stored value — which is why it needs no scanner-version bump.
func (tr *timeRange) update(ts time.Time) {
	if ts.IsZero() {
		return
	}
	ts = ts.UTC()
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

// readSessionSummary reads a session JSONL file and extracts lightweight metadata,
// along with the per-message cost accumulation over the pricing resolver.
func readSessionSummary(
	sessionID, projectPath, filePath string, logger *slog.Logger,
) (*ClaudeSessionSummary, *costAccumulator, error) {
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
// genuine turns, not tool_result carriers, and its EventCount carries the raw
// top-level event total — both persisted, exactly as for a parent session.
func readSubagentSummary(
	sessionID, projectPath, filePath string, logger *slog.Logger,
) (*ClaudeSessionSummary, *costAccumulator, error) {
	return readSummaryFile(sessionID, projectPath, filePath, true, logger)
}

func readSummaryFile(
	sessionID, projectPath, filePath string, countSidechainUsers bool, logger *slog.Logger,
) (*ClaudeSessionSummary, *costAccumulator, error) {
	f, err := os.Open(filePath) //nolint:gosec
	if err != nil {
		return nil, nil, err
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
	var active activeTimeTracker
	costs := newCostAccumulator(defaultPricingResolver())
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
			// The same event set that bounds the time range feeds active
			// duration, so active time is contained in [start, last] by
			// construction — a pr-link posted days after the conversation can
			// extend neither.
			active.observe(ev.Timestamp, ev.Type == "assistant")
		}
		updateMetadataFromEvent(&summary.CWD, &summary.GitBranch, ev)
		processSummaryEvent(summary, ev, countSidechainUsers)
		if ev.Type == "assistant" && ev.Message != nil && ev.Message.Usage != nil {
			var u TokenUsage
			addAssistantUsage(&u, ev.Message)
			costs.addAssistantMessage(ev.Message.Model, u, ev.Timestamp)
		}
	}

	summary.StartTime = tr.start
	summary.LastActivity = tr.last
	summary.ActiveDurationMs, _ = active.durations()
	// Carry the accumulated cost on the summary so every persistence path — the
	// session cache and the sub-agent cache alike — stores it without having to
	// thread the accumulator through. The accumulator is still returned for
	// callers that need the per-model detail behind the total.
	summary.Cost = sessionCostFromPricing(costs)
	summary.CostByModel = costs.CostByModel()
	summary.UnpricedModels = costs.UnpricedModels()
	summary.UnpricedTokens = costs.UnknownPricingTokens()

	if summary.StartTime.IsZero() {
		return nil, nil, sc.Err()
	}
	return summary, costs, sc.Err()
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
//
// Preview is seeded on a deliberately weaker rule than the message counter. A
// real prompt always wins, but a session that never contains one — a transcript
// consisting only of a slash command and its expansion — still needs a label,
// because Preview is the last fallback in ResolveDisplayTitle and an empty
// string there renders as a blank row in the sessions list. Showing the wrapper
// text is what those sessions displayed before turn filtering existed, and a
// noisy label beats an unidentifiable one.
func addSummaryUserEvent(summary *ClaudeSessionSummary, ev rawEvent) {
	summary.EventCount++
	if ev.Message == nil {
		return
	}
	if !isUserTurnContent(ev.Message.Content) {
		// Still a preview candidate if nothing better ever arrives — but never
		// a tool_result carrier, which is unreadable machine payload.
		if summary.Preview == "" && !isInjectedUserContent(ev.Message.Content) {
			return
		}
		if summary.Preview == "" {
			raw := extractTextContent(ev.Message.Content)
			summary.Preview = truncateRunes(fallbackPreviewLabel(raw), previewMaxRunes)
			summary.previewIsFallback = true
		}
		return
	}

	summary.MessageCount++
	// A genuine turn replaces a wrapper-sourced preview, so the label prefers
	// what the person actually typed even when a wrapper came first.
	//
	// Since #226 every known injected form — string wrapper, skill preamble and
	// interruption notice alike — is classified above and takes the fallback
	// branch, so this call is normally a no-op on real prose. It stays because
	// the marker tables are empirical: a wrapper shape a future Claude Code
	// release invents reaches this branch until the tables catch up, and
	// labeling it beats rendering raw tag soup. A real prompt matches neither
	// pattern and is returned unchanged.
	if summary.Preview == "" || summary.previewIsFallback {
		raw := extractTextContent(ev.Message.Content)
		summary.Preview = truncateRunes(fallbackPreviewLabel(raw), previewMaxRunes)
		summary.previewIsFallback = false
	}
}

// commandNamePattern captures the slash command Claude Code records when a
// session was started by one.
var commandNamePattern = regexp.MustCompile(`<command-name>([^<]+)</command-name>`)

// skillPreamblePattern captures the skill path from the preamble Claude Code
// injects when a skill is invoked.
//
// Since #226 this is not only a preview matcher: isInjectedUserContent
// (processor.go) uses it to decide that a skill preamble is not a human turn.
// Loosening it for a preview reason — dropping the required (\S+) so a
// colon-only line matches, say — would silently move message_count and
// turn_count corpus-wide, and everything derived from turn_count with them.
// Any change here needs both CurrentScannerVersion and CurrentProcessorVersion
// bumped, exactly as a change to isUserTurnContent does.
var skillPreamblePattern = regexp.MustCompile(`^Base directory for this skill:\s*(\S+)`)

// fallbackPreviewLabel turns an injected wrapper into something a person can
// recognize in a list.
//
// These previews are the last resort in ResolveDisplayTitle, and for a session
// that is only a slash command they are all there is. Unprocessed, they render
// as rows reading "<command-message>lab-workflow:github-issue-to-pr</command-
// message><command-name>…" or "Base directory for this skill: /home/user/
// .claude/plugins/cache/…" — three of nine rows on the reference corpus. The
// command or skill name was in there the whole time; this pulls it out.
//
// Anything that matches neither shape is returned unchanged, so a wrapper form
// this does not know about is still shown rather than blanked.
func fallbackPreviewLabel(raw string) string {
	trimmed := strings.TrimSpace(raw)

	if m := commandNamePattern.FindStringSubmatch(trimmed); m != nil {
		return "/" + strings.TrimSpace(m[1])
	}
	if m := skillPreamblePattern.FindStringSubmatch(trimmed); m != nil {
		if name := skillNameFromPath(m[1]); name != "" {
			return "skill: " + name
		}
	}
	return raw
}

// skillNameFromPath reads the skill's name out of its directory path. Claude
// Code lays these out as …/skills/<name>, so the segment after "skills" is the
// name; a path that does not contain one falls back to its last segment, which
// is the same thing for a bare skill directory.
func skillNameFromPath(path string) string {
	segments := strings.Split(strings.Trim(path, "/"), "/")
	for i, seg := range segments {
		if seg == "skills" && i+1 < len(segments) {
			return segments[i+1]
		}
	}
	if len(segments) > 0 {
		return segments[len(segments)-1]
	}
	return ""
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
	configDir, projectPath, filePath := findSessionFile(sessionID)
	if filePath == "" {
		return nil, nil
	}
	return readSessionDetail(configDir, sessionID, projectPath, filePath, logger)
}

// findSessionFile locates a session transcript across every configured config
// dir, returning the dir it was found in, the decoded project path and the
// file path. Empty strings when the session is not found anywhere.
//
// The config dir is returned rather than discarded because the session's todo
// list lives beside its transcript, under that same dir's todos/ — resolving it
// against the default dir would return another account's todos, or none.
func findSessionFile(sessionID string) (configDir, projectPath, filePath string) {
	for _, dir := range ClaudeHomes() {
		projectsDir := filepath.Join(dir, "projects")
		entries, err := os.ReadDir(projectsDir)
		if err != nil {
			continue
		}
		for _, e := range entries {
			if !e.IsDir() {
				continue
			}
			fp := filepath.Join(projectsDir, e.Name(), sessionID+jsonlExt)
			if _, statErr := os.Stat(fp); statErr == nil {
				return dir, DecodeProjectPath(e.Name()), fp
			}
		}
	}
	return "", "", ""
}

// readSessionDetail reads a session JSONL file and builds the full message
// detail. Sidechain (sub-agent) events are skipped here — sub-agents are read
// from <session-id>/subagents/ and reported separately (see ClaudeSubagent).
func readSessionDetail(
	configDir, sessionID, projectPath, filePath string, logger *slog.Logger,
) (*ClaudeSessionDetail, error) {
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
		topLevel = processDetailEvent(detail, ev, topLevel)
	}

	detail.StartTime = tr.start
	detail.LastActivity = tr.last
	detail.Messages = topLevel
	if detail.Messages == nil {
		detail.Messages = []ClaudeMessage{}
	}
	detail.Todos = loadTodos(configDir, sessionID)
	if detail.Todos == nil {
		detail.Todos = []ClaudeTodo{}
	}
	detail.Preview = derivePreview(detail.Messages)
	return detail, sc.Err()
}

func processDetailEvent(
	detail *ClaudeSessionDetail, ev rawEvent,
	topLevel []ClaudeMessage,
) []ClaudeMessage {
	switch ev.Type {
	case "user":
		return processDetailUserEvent(detail, ev, topLevel)
	case "assistant":
		return processDetailAssistantEvent(detail, ev, topLevel)
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

// loadTodos reads the session's todo list from
// <configDir>/todos/{id}-agent-{id}.json.
//
// The dir is the one the transcript was found in, not the run default: a
// session belongs to the account that produced it, and resolving its todos
// anywhere else returns another account's list or nothing at all. An empty
// configDir falls back to the run default so callers that never located a file
// behave as before.
func loadTodos(configDir, sessionID string) []ClaudeTodo {
	if configDir == "" {
		configDir = ClaudeHome()
	}
	todoPath := filepath.Join(configDir, "todos",
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
