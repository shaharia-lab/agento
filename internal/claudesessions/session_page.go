package claudesessions

import (
	"context"
	"database/sql"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"strconv"
	"time"
)

// A page of the sessions list, plus the aggregate the toolbar shows.
//
// The list used to ship every session to the browser and filter, sort, group
// and render all of them there. That worked at 800 sessions and stopped working
// well before 5,000: 1.33 MB became ~8.3 MB per load, ~54k DOM nodes became
// ~340k, and every keystroke re-ran two full predicate passes and two array
// sorts over the whole corpus. Everything below exists so the browser never
// holds more than one page.

// SessionPage is one page of the sessions list.
type SessionPage struct {
	Items []ClaudeSessionSummary `json:"items"`
	// NextCursor continues the list. Empty means this was the last page.
	NextCursor string `json:"next_cursor"`
	// HasMore is NextCursor != "", stated explicitly so a client does not have
	// to know that.
	HasMore bool `json:"has_more"`
}

// SessionFacets is everything the sessions toolbar needs that a single page
// cannot answer: the totals across the whole filtered set, and the options the
// filter dropdowns offer.
type SessionFacets struct {
	// Total, TotalTokens and TotalCostUSD describe the sessions matching the
	// current filter — the counter beside the toolbar, which must agree with
	// the rows below it rather than with the page on screen.
	Total        int     `json:"total"`
	TotalTokens  int     `json:"total_tokens"`
	TotalCostUSD float64 `json:"total_cost_usd"`
	// TokenP90 is the 90th percentile of input+output tokens across the
	// filtered set: the reference length for the list's token bars. The 90th
	// rather than the maximum, because one 75M-token session against a corpus
	// whose median is ~100K would push every other bar below a pixel.
	TokenP90 int `json:"token_p90"`

	// Models and PermissionModes are the dropdown options, derived from every
	// visible session rather than from the filtered set: a dropdown that
	// removes the option you just picked cannot be un-picked.
	Models          []string `json:"models"`
	PermissionModes []string `json:"permission_modes"`
	// HasFavorites and HasPRs gate the toggles that would otherwise filter
	// nothing. Same basis as the dropdowns, and for the same reason.
	HasFavorites bool `json:"has_favorites"`
	HasPRs       bool `json:"has_prs"`
}

// ErrCursorMismatch is returned when a cursor was minted under a different sort
// than the request asks for. Continuing anyway would page through one ordering
// using another's position, silently skipping and repeating rows.
var ErrCursorMismatch = errors.New("claudesessions: cursor does not match the requested sort")

// cursor is a keyset position: the sort value of the last row returned, plus
// its session ID as the tiebreak.
//
// Keyset rather than OFFSET because OFFSET makes the database walk and discard
// every skipped row — page 50 costs fifty times page 1 — and because a scan
// completing mid-scroll would shift every subsequent page by the number of
// newly inserted sessions, showing some twice and skipping others.
type cursor struct {
	Sort SessionSort `json:"s"`
	// Value is the sort column's value, RFC3339Nano for the time sort and a
	// decimal for the numeric ones. A string either way so one encoding covers
	// both without a discriminator.
	Value string `json:"v"`
	ID    string `json:"id"`
}

func (c cursor) encode() string {
	b, err := json.Marshal(c)
	if err != nil {
		return ""
	}
	return base64.RawURLEncoding.EncodeToString(b)
}

func decodeCursor(raw string, want SessionSort) (cursor, error) {
	if raw == "" {
		return cursor{}, nil
	}
	b, err := base64.RawURLEncoding.DecodeString(raw)
	if err != nil {
		return cursor{}, fmt.Errorf("claudesessions: malformed cursor: %w", err)
	}
	var c cursor
	if err := json.Unmarshal(b, &c); err != nil {
		return cursor{}, fmt.Errorf("claudesessions: malformed cursor: %w", err)
	}
	if c.Sort != want {
		return cursor{}, ErrCursorMismatch
	}
	return c, nil
}

// bind converts the cursor's value into the argument the keyset predicate
// compares against: a time.Time for the recency sort, a float otherwise.
//
// The time is bound as a time.Time rather than a string so the driver renders
// it exactly as it rendered the stored value. The stored form is Go's
// time.Time.String() ("2026-08-11 07:54:00.097 +0000 UTC"), whose lexical order
// matches chronological order only because every value is UTC and fractional
// zeros are trimmed — comparing a hand-formatted string against it would be one
// formatting difference away from silently mis-paging.
func (c cursor) bind(isTime bool) (any, error) {
	if isTime {
		t, err := time.Parse(time.RFC3339Nano, c.Value)
		if err != nil {
			return nil, fmt.Errorf("claudesessions: malformed cursor timestamp: %w", err)
		}
		return t.UTC(), nil
	}
	n, err := strconv.ParseFloat(c.Value, 64)
	if err != nil {
		return nil, fmt.Errorf("claudesessions: malformed cursor value: %w", err)
	}
	return n, nil
}

// cursorValue renders a row's sort value for the next cursor.
func cursorValue(s ClaudeSessionSummary, sort SessionSort) string {
	switch sort {
	case SortCost:
		return strconv.FormatFloat(s.TotalCost().TotalUSD, 'g', -1, 64)
	case SortTokens:
		u := s.TotalUsage()
		return strconv.Itoa(u.InputTokens + u.OutputTokens)
	case SortDuration:
		return strconv.FormatInt(s.TotalActiveDurationMs(), 10)
	case SortMessages:
		return strconv.Itoa(s.MessageCount)
	case SortRecent:
		return s.LastActivity.UTC().Format(time.RFC3339Nano)
	default:
		return s.LastActivity.UTC().Format(time.RFC3339Nano)
	}
}

// ListPage returns one page of sessions matching q.
//
// Unlike List it never loads the corpus: the filter, the sort and the page
// bound are all SQL, so the cost is one indexed range scan rather than a full
// table read plus an in-memory pass per filter.
//
// It triggers a background rescan on the same conditions List does but never
// waits for one, not even on a cold cache. An empty first page during a scan is
// reported as "scanning" by GET /claude-sessions/status, which the list already
// polls — blocking the request instead would mean a first-run user on a large
// corpus waits out the whole scan and then times out anyway.
func (c *Cache) ListPage(q SessionQuery) (SessionPage, error) {
	c.ensureFresh()
	return listSessionPage(c.db, c.logger, q)
}

// Facets returns the filtered totals and the filter options for q.
func (c *Cache) Facets(q SessionQuery) (SessionFacets, error) {
	c.ensureFresh()
	return sessionFacets(c.db, c.logger, q)
}

func listSessionPage(db *sql.DB, logger *slog.Logger, q SessionQuery) (SessionPage, error) {
	sort := q.sort()
	expr, isTime := sort.sortExpr()

	filter, err := buildFilter(q)
	if err != nil {
		return SessionPage{}, err
	}
	cur, err := decodeCursor(q.Cursor, sort)
	if err != nil {
		return SessionPage{}, err
	}
	if cur.ID != "" {
		bound, bindErr := cur.bind(isTime)
		if bindErr != nil {
			return SessionPage{}, bindErr
		}
		// Strictly after the cursor in the same total order the ORDER BY below
		// imposes. The tiebreak on session_id is what makes the order total:
		// without it, two sessions sharing a cost or a timestamp would page
		// against each other, one repeating and one disappearing.
		filter.add(fmt.Sprintf("(%s < ? OR (%s = ? AND c.session_id < ?))", expr, expr),
			bound, bound, cur.ID)
	}

	limit := q.limit()
	// One extra row, so "is there a next page" is answered without a second
	// COUNT over the same predicate.
	query := sessionSummaryColumns + sessionSummarySource + filter.where() +
		fmt.Sprintf("\nORDER BY %s DESC, c.session_id DESC\nLIMIT %d", expr, limit+1)

	rows, err := db.QueryContext(context.Background(), query, filter.args...)
	if err != nil {
		return SessionPage{}, fmt.Errorf("claudesessions: querying session page: %w", err)
	}
	defer closeRows(rows, logger)

	items := make([]ClaudeSessionSummary, 0, limit)
	for rows.Next() {
		s, scanErr := scanSessionSummary(rows)
		if scanErr != nil {
			return SessionPage{}, fmt.Errorf("claudesessions: scanning session page: %w", scanErr)
		}
		items = append(items, s)
	}
	if err := rows.Err(); err != nil {
		return SessionPage{}, fmt.Errorf("claudesessions: reading session page: %w", err)
	}

	page := SessionPage{}
	if len(items) > limit {
		items = items[:limit]
		last := items[len(items)-1]
		page.NextCursor = cursor{Sort: sort, Value: cursorValue(last, sort), ID: last.SessionID}.encode()
		page.HasMore = page.NextCursor != ""
	}
	attachPRsFor(db, logger, items)
	attachSubagentUsageByModelFor(db, logger, items)
	page.Items = items
	return page, nil
}

// sessionFacets answers the two questions a single page cannot: what the whole
// filtered set totals, and what the filter dropdowns should offer.
func sessionFacets(db *sql.DB, logger *slog.Logger, q SessionQuery) (SessionFacets, error) {
	filter, err := buildFilter(q)
	if err != nil {
		return SessionFacets{}, err
	}

	var f SessionFacets
	totals := "SELECT COUNT(*), COALESCE(SUM(" + sqlTokens + "), 0), COALESCE(SUM(" + sqlCostUSD + "), 0)" +
		sessionSummarySource + filter.where()
	if err := db.QueryRowContext(context.Background(), totals, filter.args...).
		Scan(&f.Total, &f.TotalTokens, &f.TotalCostUSD); err != nil {
		return SessionFacets{}, fmt.Errorf("claudesessions: session totals: %w", err)
	}

	if f.Total > 0 {
		// The same index the TS implementation takes: floor(0.9 * (n-1)) into
		// the ascending series, so both languages pick the same row rather than
		// two neighboring ones.
		offset := int(0.9 * float64(f.Total-1))
		p90 := "SELECT " + sqlTokens + sessionSummarySource + filter.where() +
			fmt.Sprintf("\nORDER BY %s ASC\nLIMIT 1 OFFSET %d", sqlTokens, offset)
		if err := db.QueryRowContext(context.Background(), p90, filter.args...).Scan(&f.TokenP90); err != nil {
			logger.Warn("claude sessions: failed to compute token p90", "error", err)
		}
	}

	if err := loadFacetOptions(db, logger, &f); err != nil {
		return SessionFacets{}, err
	}
	return f, nil
}

// loadFacetOptions fills the dropdown options and the toggle gates.
//
// Scoped to visible projects but not to the rest of the filter: the options are
// what the corpus contains, so picking one never removes the others. This is
// what the client-side modelsOf/permissionModesOf did over the full list.
func loadFacetOptions(db *sql.DB, logger *slog.Logger, f *SessionFacets) error {
	visible := &clause{}
	for _, p := range HiddenProjects() {
		visible.add("c.project_path != ?", p)
	}
	where := visible.where()

	models, err := distinctStrings(db, logger,
		"SELECT DISTINCT c.model FROM claude_session_cache c"+where+" ORDER BY c.model", visible.args)
	if err != nil {
		return err
	}
	f.Models = models

	modes, err := distinctStrings(db, logger,
		"SELECT DISTINCT c.permission_mode FROM claude_session_cache c"+where+
			" ORDER BY c.permission_mode", visible.args)
	if err != nil {
		return err
	}
	f.PermissionModes = modes

	favClause := &clause{}
	favClause.sql = append(favClause.sql, visible.sql...)
	favClause.args = append(favClause.args, visible.args...)
	favClause.add("c.is_favorite = 1")
	if err := existsRow(db, "SELECT 1 FROM claude_session_cache c"+favClause.where()+" LIMIT 1",
		favClause.args, &f.HasFavorites); err != nil {
		return err
	}

	prClause := &clause{}
	prClause.sql = append(prClause.sql, visible.sql...)
	prClause.args = append(prClause.args, visible.args...)
	addLinks(prClause, LinksWith)
	return existsRow(db, "SELECT 1 FROM claude_session_cache c"+prClause.where()+" LIMIT 1",
		prClause.args, &f.HasPRs)
}

func distinctStrings(db *sql.DB, logger *slog.Logger, query string, args []any) ([]string, error) {
	rows, err := db.QueryContext(context.Background(), query, args...)
	if err != nil {
		return nil, fmt.Errorf("claudesessions: reading facet options: %w", err)
	}
	defer closeRows(rows, logger)

	out := []string{}
	for rows.Next() {
		var v string
		if err := rows.Scan(&v); err != nil {
			return nil, fmt.Errorf("claudesessions: scanning facet option: %w", err)
		}
		if v != "" {
			out = append(out, v)
		}
	}
	return out, rows.Err()
}

func existsRow(db *sql.DB, query string, args []any, out *bool) error {
	var one int
	err := db.QueryRowContext(context.Background(), query, args...).Scan(&one)
	switch {
	case errors.Is(err, sql.ErrNoRows):
		*out = false
		return nil
	case err != nil:
		return fmt.Errorf("claudesessions: reading facet flag: %w", err)
	default:
		*out = true
		return nil
	}
}

func closeRows(rows *sql.Rows, logger *slog.Logger) {
	if err := rows.Close(); err != nil {
		logger.Warn("claude sessions: failed to close rows", "error", err)
	}
}

// idPlaceholders renders "?, ?, …" and the matching arguments for a page's
// session IDs. A page is at most MaxPageSize rows, so this never approaches
// SQLite's variable limit — the corpus-wide reads it replaces are what did.
func idPlaceholders(sessions []ClaudeSessionSummary) (string, []any) {
	args := make([]any, 0, len(sessions))
	marks := make([]byte, 0, len(sessions)*3)
	for i := range sessions {
		if i > 0 {
			marks = append(marks, ',', ' ')
		}
		marks = append(marks, '?')
		args = append(args, sessions[i].SessionID)
	}
	return string(marks), args
}

// attachPRsFor is attachPRs narrowed to one page. The corpus-wide variant reads
// the whole claude_session_pr table, which is correct when the caller holds the
// whole corpus and wasteful when it holds fifty rows.
func attachPRsFor(db *sql.DB, logger *slog.Logger, sessions []ClaudeSessionSummary) {
	if len(sessions) == 0 {
		return
	}
	marks, args := idPlaceholders(sessions)
	// #nosec G202 -- marks is a generated run of "?" placeholders, never input.
	query := `
		SELECT session_id, pr_number, pr_url, pr_repository, first_seen_at
		FROM claude_session_pr WHERE session_id IN (` + marks + `)
		ORDER BY first_seen_at, pr_url`
	rows, err := db.QueryContext(context.Background(), query, args...)
	if err != nil {
		logger.Warn("claude sessions: failed to load linked PRs for page", "error", err)
		return
	}
	defer closeRows(rows, logger)

	bySession := make(map[string][]ClaudeSessionPR, len(sessions))
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

// attachSubagentUsageByModelFor is attachSubagentUsageByModel narrowed to one
// page, for the same reason attachPRsFor is.
func attachSubagentUsageByModelFor(db *sql.DB, logger *slog.Logger, sessions []ClaudeSessionSummary) {
	if len(sessions) == 0 {
		return
	}
	marks, args := idPlaceholders(sessions)
	// #nosec G202 -- marks is a generated run of "?" placeholders, never input.
	query := `
		SELECT parent_session_id, model,
		       SUM(input_tokens), SUM(output_tokens),
		       SUM(cache_creation_tokens), SUM(cache_read_tokens),
		       SUM(cache_creation_5m_tokens), SUM(cache_creation_1h_tokens),
		       SUM(input_cost_usd), SUM(output_cost_usd),
		       SUM(cache_read_cost_usd), SUM(cache_write_cost_usd), SUM(total_cost_usd)
		FROM claude_subagent_cache WHERE parent_session_id IN (` + marks + `)
		GROUP BY parent_session_id, model`
	rows, err := db.QueryContext(context.Background(), query, args...)
	if err != nil {
		logger.Warn("claude sessions: failed to load sub-agent usage by model for page", "error", err)
		return
	}
	defer closeRows(rows, logger)

	usage := map[string]map[string]TokenUsage{}
	cost := map[string]map[string]SessionCost{}
	for rows.Next() {
		var sessionID, model string
		var u TokenUsage
		var sc SessionCost
		if err := rows.Scan(&sessionID, &model,
			&u.InputTokens, &u.OutputTokens,
			&u.CacheCreationTokens, &u.CacheReadTokens,
			&u.CacheCreation5mTokens, &u.CacheCreation1hTokens,
			&sc.InputUSD, &sc.OutputUSD,
			&sc.CacheReadUSD, &sc.CacheWriteUSD, &sc.TotalUSD); err != nil {
			logger.Warn("claude sessions: failed to scan sub-agent usage by model", "error", err)
			return
		}
		model = displayModel(model)
		if usage[sessionID] == nil {
			usage[sessionID] = map[string]TokenUsage{}
			cost[sessionID] = map[string]SessionCost{}
		}
		usage[sessionID][model] = u
		cost[sessionID][model] = sc
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
