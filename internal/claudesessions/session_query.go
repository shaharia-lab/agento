package claudesessions

import (
	"fmt"
	"strings"
	"time"
)

// This file is the SQL half of the sessions list. The other half is
// frontend/src/lib/sessionMetrics.ts, and the two must agree exactly: a row
// showing $36.30 must not be hidden by "cost at most $40". That parity is
// asserted from a shared fixture — testdata/session_metric_vectors.json, read
// by session_query_test.go here and sessionMetrics.test.ts there — so a change
// to one definition fails the other language's test rather than silently
// disagreeing in production.

// The per-session figures the list filters and sorts on, as SQL over the
// summary select's `c` (session) and `sa` (sub-agent roll-up) aliases.
//
// Every one of these sums the main thread and its delegated sub-agents,
// mirroring sessionMetrics.ts, because that is what the list's columns render.
// COALESCE matches the roll-up's own treatment of a session that delegated
// nothing: absent means zero, never NULL.
// #nosec G101 -- SQL fragments, not credentials: "tokens" here means model
// tokens.
const (
	sqlInputTokens  = "(c.input_tokens + COALESCE(sa.it, 0))"
	sqlOutputTokens = "(c.output_tokens + COALESCE(sa.ot, 0))"
	sqlTokens       = "(c.input_tokens + COALESCE(sa.it, 0) + c.output_tokens + COALESCE(sa.ot, 0))"
	sqlCostUSD      = "(c.total_cost_usd + COALESCE(sa.tc, 0))"
	// Active duration, not the wall-clock span: a resumable session's span
	// counts every idle day between sittings. The filter is entered in minutes,
	// so the bound is converted rather than the column.
	sqlActiveDurationMs = "(c.active_duration_ms + COALESCE(sa.adm, 0))"
	// Message count stays main-thread, matching sessionMetrics.ts and the
	// column beside it: event_count and message_count are deliberately not
	// rolled up, exactly as Usage and Cost are.
	sqlMessageCount = "c.message_count"
)

// SessionSort names the order a page is returned in. Keyset pagination needs
// the sort column indexed and the tiebreak stable, so the set is closed rather
// than an arbitrary column name from the query string.
type SessionSort string

const (
	// SortRecent orders by last activity, newest first. It is the default and
	// the only order the day grouping is meaningful under.
	SortRecent SessionSort = "recent"
	// SortCost orders by total cost, most expensive first.
	SortCost SessionSort = "cost"
	// SortTokens orders by input+output tokens, largest first.
	SortTokens SessionSort = "tokens"
	// SortDuration orders by active duration, longest first.
	SortDuration SessionSort = "duration"
	// SortMessages orders by conversational turns, most first.
	SortMessages SessionSort = "messages"
)

// sortExpr returns the SQL the sort orders and pages on, and whether its values
// are timestamps (which page through a time.Time cursor rather than a float).
func (s SessionSort) sortExpr() (expr string, isTime bool) {
	switch s {
	case SortCost:
		return sqlCostUSD, false
	case SortTokens:
		return sqlTokens, false
	case SortDuration:
		return sqlActiveDurationMs, false
	case SortMessages:
		return sqlMessageCount, false
	case SortRecent:
		return "c.last_activity", true
	default:
		return "c.last_activity", true
	}
}

// valid reports whether s is one of the known sorts. An unknown value falls
// back to SortRecent rather than erroring: the list is a read-only view, and a
// stale bookmark is better rendered in the default order than refused.
func (s SessionSort) valid() bool {
	switch s {
	case SortRecent, SortCost, SortTokens, SortDuration, SortMessages:
		return true
	default:
		return false
	}
}

// LinkFilter selects on whether a session has linked pull requests.
type LinkFilter string

const (
	// LinksAny matches every session; the zero value.
	LinksAny LinkFilter = ""
	// LinksWith keeps only sessions with at least one linked PR.
	LinksWith LinkFilter = "with"
	// LinksWithout keeps only sessions with none.
	LinksWithout LinkFilter = "without"
)

// NumericRange is an inclusive numeric filter. One min/max pair expresses all
// three comparisons the UI offers — min alone is "at least", max alone "at
// most", both "between" — so no operator selector is needed beside each field.
type NumericRange struct {
	Min *float64
	Max *float64
}

// set reports whether the range constrains anything.
func (r NumericRange) set() bool { return r.Min != nil || r.Max != nil }

// TimeWindow is a half-open interval a drill-down selected: a session matches
// when its activity window overlaps it.
type TimeWindow struct {
	From time.Time
	To   time.Time
}

// maxDrilldownWindows bounds the OR-group a drill-down expands into.
//
// The UI caps an analytics range at 180 days and a drill-down selects at most
// one hour per day inside it (an hourly bar) or one per week (a heatmap cell),
// so a real request carries ≤180 windows. The cap exists so a hand-written
// query string cannot turn one request into a several-thousand-term predicate.
const maxDrilldownWindows = 512

// SessionQuery is everything the sessions list can narrow, sort and page by.
// The zero value selects every visible session, newest first.
type SessionQuery struct {
	// Project matches ClaudeSessionSummary.ProjectPath exactly. Empty = all.
	Project string
	// Search matches the session ID, the resolved titles, the preview or the
	// project path, case-insensitively, as a substring.
	Search string
	// FavoritesOnly keeps only starred sessions.
	FavoritesOnly bool
	Links         LinkFilter
	// PermissionMode and Model match exactly. Empty = all.
	PermissionMode string
	Model          string

	Messages        NumericRange
	DurationMinutes NumericRange
	TokensIn        NumericRange
	TokensOut       NumericRange
	Cost            NumericRange

	// From and To bound the session's *activity window* by overlap, not its
	// last activity by containment: a session that started before the window
	// and is still running belongs in it. This is the list's long-standing
	// definition and is deliberately not the analytics one (FilterSessions,
	// which contains last_activity), because the two answer different
	// questions — "what was I working on then" versus "what does this window
	// cost". A nil bound is open-ended.
	From *time.Time
	To   *time.Time
	// Windows, when non-empty, replaces From/To entirely: a drill-down from an
	// analytics chart selects explicit hour windows and a session matches when
	// it overlaps any of them.
	Windows []TimeWindow

	Sort SessionSort
	// Limit is the page size. Zero means DefaultPageSize; values above
	// MaxPageSize are clamped rather than refused.
	Limit int
	// Cursor continues a previous page. Empty starts at the first.
	Cursor string
}

// Page size bounds. The default is what the list requests; the maximum exists
// because the browser is the constraint this whole surface was rebuilt for —
// an unbounded page would simply move the wall back to where it was.
const (
	DefaultPageSize = 50
	MaxPageSize     = 200
)

// limit returns the clamped page size.
func (q SessionQuery) limit() int {
	switch {
	case q.Limit <= 0:
		return DefaultPageSize
	case q.Limit > MaxPageSize:
		return MaxPageSize
	default:
		return q.Limit
	}
}

// sort returns the effective sort, falling back to SortRecent.
func (q SessionQuery) sort() SessionSort {
	if q.Sort.valid() {
		return q.Sort
	}
	return SortRecent
}

// clause is an accumulated WHERE fragment with its bound arguments.
type clause struct {
	sql  []string
	args []any
}

func (c *clause) add(sql string, args ...any) {
	c.sql = append(c.sql, sql)
	c.args = append(c.args, args...)
}

// where renders the accumulated predicate, or an empty string when nothing was
// added. Always AND: every filter narrows.
func (c *clause) where() string {
	if len(c.sql) == 0 {
		return ""
	}
	return "\nWHERE " + strings.Join(c.sql, "\n  AND ")
}

// buildFilter turns a query into its WHERE clause, excluding pagination — the
// same predicate serves the page, the facet aggregate and the total count, so
// the counter in the toolbar and the rows below it cannot disagree.
//
// Hidden projects are applied here as well as at Cache.loadOrEmpty: this path
// never loads the full corpus, so it cannot inherit that filter and must
// reproduce it. Both read HiddenProjects(), the one definition.
func buildFilter(q SessionQuery) (*clause, error) {
	c := &clause{}

	for _, p := range HiddenProjects() {
		c.add("c.project_path != ?", p)
	}
	if q.Project != "" {
		c.add("c.project_path = ?", q.Project)
	}
	if q.FavoritesOnly {
		c.add("c.is_favorite = 1")
	}
	if q.PermissionMode != "" {
		c.add("c.permission_mode = ?", q.PermissionMode)
	}
	if q.Model != "" {
		c.add("c.model = ?", q.Model)
	}
	addSearch(c, q.Search)
	addLinks(c, q.Links)

	addRange(c, sqlMessageCount, q.Messages, 1)
	// The duration filter is entered in minutes; the column stores milliseconds.
	addRange(c, sqlActiveDurationMs, q.DurationMinutes, 60_000)
	addRange(c, sqlInputTokens, q.TokensIn, 1)
	addRange(c, sqlOutputTokens, q.TokensOut, 1)
	addRange(c, sqlCostUSD, q.Cost, 1)

	if err := addTimeFilter(c, q); err != nil {
		return nil, err
	}
	return c, nil
}

// addSearch matches the same four fields the client-side predicate did.
//
// A plain indexed-free LIKE rather than an FTS5 index: the corpus this serves
// is thousands of rows, not millions, and scanning five short columns over
// 5,000 rows is well under a millisecond. FTS5 would buy that back at the cost
// of a shadow table to keep in step with every upsert and delete in the
// scanner — a synchronization burden with a real failure mode (a stale index
// silently hides rows) for a saving no user could perceive.
//
// LOWER on both sides rather than COLLATE NOCASE, because NOCASE is ASCII-only
// in SQLite and project paths and titles are not.
func addSearch(c *clause, search string) {
	q := strings.ToLower(strings.TrimSpace(search))
	if q == "" {
		return
	}
	// The pattern is bound, not interpolated, so % and _ typed by the user are
	// matched literally via ESCAPE rather than acting as wildcards.
	pattern := "%" + escapeLike(q) + "%"
	c.add(`(LOWER(c.session_id) LIKE ? ESCAPE '\'
    OR LOWER(c.preview) LIKE ? ESCAPE '\'
    OR LOWER(c.custom_title) LIKE ? ESCAPE '\'
    OR LOWER(c.native_title) LIKE ? ESCAPE '\'
    OR LOWER(c.ai_title) LIKE ? ESCAPE '\'
    OR LOWER(c.project_path) LIKE ? ESCAPE '\')`,
		pattern, pattern, pattern, pattern, pattern, pattern)
}

// escapeLike neutralizes LIKE's wildcards so a search for "100%" does not match
// everything beginning with "100".
func escapeLike(s string) string {
	r := strings.NewReplacer(`\`, `\\`, `%`, `\%`, `_`, `\_`)
	return r.Replace(s)
}

func addLinks(c *clause, f LinkFilter) {
	const exists = "EXISTS (SELECT 1 FROM claude_session_pr p WHERE p.session_id = c.session_id)"
	switch f {
	case LinksWith:
		c.add(exists)
	case LinksWithout:
		c.add("NOT " + exists)
	case LinksAny:
	}
}

// addRange appends an inclusive bound on expr. scale converts the filter's unit
// into the column's (minutes → milliseconds) so the bound moves rather than the
// column, keeping the expression indexable and the comparison exact.
func addRange(c *clause, expr string, r NumericRange, scale float64) {
	if !r.set() {
		return
	}
	if r.Min != nil {
		c.add(expr+" >= ?", *r.Min*scale)
	}
	if r.Max != nil {
		c.add(expr+" <= ?", *r.Max*scale)
	}
}

// addTimeFilter applies either the drill-down windows or the from/to range —
// never both, matching the UI, where an active drill-down replaces the preset.
func addTimeFilter(c *clause, q SessionQuery) error {
	if len(q.Windows) > 0 {
		if len(q.Windows) > maxDrilldownWindows {
			return fmt.Errorf("claudesessions: %d drill-down windows exceeds the %d limit",
				len(q.Windows), maxDrilldownWindows)
		}
		terms := make([]string, 0, len(q.Windows))
		for _, w := range q.Windows {
			// Half-open, matching the client's overlapsAnyWindow: a session
			// starting exactly at a window's end is in the next window, not this
			// one.
			terms = append(terms, "(c.start_time < ? AND c.last_activity >= ?)")
			c.args = append(c.args, w.To.UTC(), w.From.UTC())
		}
		c.sql = append(c.sql, "("+strings.Join(terms, "\n    OR ")+")")
		return nil
	}
	if q.To != nil {
		c.add("c.start_time <= ?", q.To.UTC())
	}
	if q.From != nil {
		c.add("c.last_activity >= ?", q.From.UTC())
	}
	return nil
}
