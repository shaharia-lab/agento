package claudesessions

import (
	"context"
	"database/sql"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/config"
)

// testSession is the subset of a cached row these tests vary. Everything else
// takes a schema default, so a new column cannot break every test here.
type testSession struct {
	id             string
	project        string
	preview        string
	customTitle    string
	favorite       bool
	permissionMode string
	model          string
	configDir      string
	start          time.Time
	last           time.Time
	messages       int
	inputTokens    int
	outputTokens   int
	costUSD        float64
	activeMs       int64

	// Sub-agent roll-up, written as a single delegated transcript.
	subInputTokens  int
	subOutputTokens int
	subCostUSD      float64
	subActiveMs     int64

	prURL string
}

func insertTestSession(t *testing.T, db *sql.DB, s testSession) {
	t.Helper()
	if s.project == "" {
		s.project = "/home/dev/repo"
	}
	if s.last.IsZero() {
		s.last = time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)
	}
	if s.start.IsZero() {
		s.start = s.last.Add(-time.Hour)
	}
	ctx := context.Background()
	_, err := db.ExecContext(ctx, `
		INSERT INTO claude_session_cache (
			session_id, project_path, file_path, file_mtime,
			preview, custom_title, is_favorite, permission_mode, model,
			start_time, last_activity, message_count,
			input_tokens, output_tokens, total_cost_usd, active_duration_ms,
			config_dir
		) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`,
		s.id, s.project, "/tmp/"+s.id+".jsonl", s.last,
		s.preview, s.customTitle, s.favorite, s.permissionMode, s.model,
		s.start, s.last, s.messages,
		s.inputTokens, s.outputTokens, s.costUSD, s.activeMs, s.configDir)
	if err != nil {
		t.Fatalf("inserting session %s: %v", s.id, err)
	}

	if s.subInputTokens != 0 || s.subOutputTokens != 0 || s.subCostUSD != 0 || s.subActiveMs != 0 {
		_, err = db.ExecContext(ctx, `
			INSERT INTO claude_subagent_cache (
				parent_session_id, agent_id, file_path, file_mtime,
				input_tokens, output_tokens, total_cost_usd, active_duration_ms
			) VALUES (?,?,?,?,?,?,?,?)`,
			s.id, "agent-1", "/tmp/"+s.id+"/agent-1.jsonl", s.last,
			s.subInputTokens, s.subOutputTokens, s.subCostUSD, s.subActiveMs)
		if err != nil {
			t.Fatalf("inserting sub-agent for %s: %v", s.id, err)
		}
	}

	if s.prURL != "" {
		_, err = db.ExecContext(ctx, `
			INSERT INTO claude_session_pr (session_id, pr_number, pr_url, pr_repository, first_seen_at)
			VALUES (?,?,?,?,?)`, s.id, 1, s.prURL, "org/repo", s.last)
		if err != nil {
			t.Fatalf("inserting PR for %s: %v", s.id, err)
		}
	}
}

// newPageCache returns a Cache whose rows these tests write directly, with every
// staleness marker already recorded.
//
// All four matter. ListPage, Facets and Analytics call ensureFresh, which starts
// a background scan if any marker disagrees with the live value — and a scan
// finding no ~/.claude/projects at all correctly concludes that every cached row
// describes a deleted transcript and clears the tables. Seeding only
// last_scanned_at leaves pricing_rev at 0 against TestMain's real catalog
// revision, so the scan runs anyway and races the assertions: locally it reads
// the developer's own corpus and usually loses the race, in CI it finds no
// corpus and reliably wipes the rows out from under the test.
func newPageCache(t *testing.T) *Cache {
	t.Helper()
	// No hidden projects, the default idle threshold, and no project list left
	// published by an earlier test — all three are process-wide, so without this
	// a test reads whichever one happened to run before it.
	ApplyDataSettings(0, nil)
	resetProjectsCache()
	// An empty HOME as well, so nothing here can read the developer's own
	// ~/.claude — which is the difference between these tests passing locally
	// and passing everywhere. Tests that want a corpus set their own HOME and
	// use newScanCache instead.
	t.Setenv("HOME", t.TempDir())
	db := setupTestDB(t)
	if _, err := db.ExecContext(context.Background(),
		`INSERT INTO claude_cache_metadata (id, last_scanned_at) VALUES (1, ?)`,
		time.Now().UTC()); err != nil {
		t.Fatalf("seeding scan metadata: %v", err)
	}
	recordScannerVersion(db, testLogger)
	recordPricingRevision(db, testLogger, currentPricingRevision())
	recordIdleThreshold(db, testLogger, IdleGapThreshold().Milliseconds())

	c := NewCache(db, testLogger)
	if c.pricingChanged() || c.idleThresholdChanged() || !c.isFresh() {
		t.Fatal("test cache still looks stale; ensureFresh would start a scan and clear these rows")
	}
	return c
}

// newScanCache returns a Cache for tests that drive the scanner themselves.
//
// It leaves HOME alone — the caller has pointed it at a generated corpus — and
// records no staleness markers, because those tests call IncrementalScan
// directly rather than through ensureFresh and want the scan to run.
func newScanCache(t *testing.T) *Cache {
	t.Helper()
	ApplyDataSettings(0, nil)
	resetProjectsCache()
	return NewCache(setupTestDB(t), testLogger)
}

// resetProjectsCache drops the project list a previous test's scan published.
// It is process-wide state, and ListProjects prefers it over a live walk.
func resetProjectsCache() {
	projectsCache.Lock()
	defer projectsCache.Unlock()
	projectsCache.projects = nil
	projectsCache.loaded = false
}

func ids(page SessionPage) []string {
	out := make([]string, 0, len(page.Items))
	for _, s := range page.Items {
		out = append(out, s.SessionID)
	}
	return out
}

func fptr(v float64) *float64 { return &v }

func TestListPage_KeysetPaginationVisitsEveryRowExactlyOnce(t *testing.T) {
	c := newPageCache(t)
	base := time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)
	const total = 25
	for i := range total {
		// Sub-millisecond spacing on purpose: the stored form is Go's
		// time.Time.String(), whose fractional part has its trailing zeros
		// trimmed, so neighboring timestamps exercise the text comparison the
		// keyset predicate relies on.
		insertTestSession(t, c.db, testSession{
			id:   fmt.Sprintf("s-%02d", i),
			last: base.Add(time.Duration(i) * 100 * time.Microsecond),
		})
	}

	seen := map[string]int{}
	cursor := ""
	pages := 0
	for {
		page, err := c.ListPage(SessionQuery{Limit: 7, Cursor: cursor})
		if err != nil {
			t.Fatalf("page %d: %v", pages, err)
		}
		for _, id := range ids(page) {
			seen[id]++
		}
		pages++
		if !page.HasMore {
			break
		}
		cursor = page.NextCursor
		if pages > 10 {
			t.Fatal("pagination did not terminate")
		}
	}

	if len(seen) != total {
		t.Errorf("saw %d distinct sessions, want %d", len(seen), total)
	}
	for id, n := range seen {
		if n != 1 {
			t.Errorf("session %s returned %d times, want exactly 1", id, n)
		}
	}
}

func TestListPage_TiesArePagedByTheFullRowKeyTiebreak(t *testing.T) {
	c := newPageCache(t)
	// Every session shares a cost, so only the tiebreak makes the order total.
	// Without it a page boundary in the middle of the tie repeats one row and
	// drops another.
	at := time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)
	for i := range 9 {
		insertTestSession(t, c.db, testSession{
			id: fmt.Sprintf("tie-%d", i), last: at, costUSD: 5,
		})
	}
	// …and one id under two project paths, tied with the rest. The tiebreak
	// has to run to the whole primary key: on session_id alone this pair is
	// one position, and whichever row loses is skipped by every page.
	for _, project := range []string{"/home/dev/a", "/home/dev/b"} {
		insertTestSession(t, c.db, testSession{
			id: "tie-dup", project: project, last: at, costUSD: 5,
		})
	}

	// Every page size, so the boundary is guaranteed to fall between the two
	// rows sharing an id as well as between two rows sharing a cost — a fixed
	// size can put the whole pair inside one page and see nothing.
	for limit := 1; limit <= 5; limit++ {
		seen := map[string]int{}
		cursor := ""
		for {
			page, err := c.ListPage(SessionQuery{Sort: SortCost, Limit: limit, Cursor: cursor})
			if err != nil {
				t.Fatalf("limit %d: paging: %v", limit, err)
			}
			for _, s := range page.Items {
				seen[rowKeyOf(s)]++
			}
			if !page.HasMore {
				break
			}
			cursor = page.NextCursor
		}
		if len(seen) != 11 {
			t.Errorf("limit %d: saw %d of 11 tied rows", limit, len(seen))
		}
		for key, n := range seen {
			if n != 1 {
				t.Errorf("limit %d: tied row %s returned %d times", limit, key, n)
			}
		}
	}
}

func TestListPage_ADuplicatedSessionIDOnATiedSortValueIsStillReachable(t *testing.T) {
	c := newPageCache(t)
	// The fixture from #364: claude_session_cache is keyed on
	// (session_id, project_path), so one id legitimately yields two rows. When
	// those two also tie on the sort column, a cursor that carries only the id
	// cannot tell them apart — the walk visits dup@/a then jumps straight to
	// other@/c, and dup@/b is never returned by any page.
	insertTestSession(t, c.db, testSession{id: "dup", project: "/a", costUSD: 5})
	insertTestSession(t, c.db, testSession{id: "dup", project: "/b", costUSD: 5})
	insertTestSession(t, c.db, testSession{id: "other", project: "/c", costUSD: 1})

	var walk []string
	cursor := ""
	for pages := 0; ; pages++ {
		if pages > 5 {
			t.Fatal("pagination did not terminate")
		}
		page, err := c.ListPage(SessionQuery{Sort: SortCost, Limit: 1, Cursor: cursor})
		if err != nil {
			t.Fatalf("paging: %v", err)
		}
		for _, s := range page.Items {
			walk = append(walk, rowKeyOf(s))
		}
		if !page.HasMore {
			break
		}
		cursor = page.NextCursor
	}

	want := []string{"dup@/b", "dup@/a", "other@/c"}
	if !slices.Equal(walk, want) {
		t.Errorf("walk = %v, want %v", walk, want)
	}

	// And the toolbar must not contradict the scroll: facets counts with
	// COUNT(*) over the same predicate, so a row the walk cannot reach shows
	// up as "3 sessions" above a list that stops at 2.
	f, err := c.Facets(SessionQuery{Sort: SortCost})
	if err != nil {
		t.Fatalf("facets: %v", err)
	}
	if f.Total != len(walk) {
		t.Errorf("facets total %d but the walk delivered %d rows", f.Total, len(walk))
	}
}

// rowKeyOf spells a row's primary key, which is what a duplicated session id
// makes these tests count on rather than the id alone.
func rowKeyOf(s ClaudeSessionSummary) string { return s.SessionID + "@" + s.ProjectPath }

func TestListPage_ACursorMintedBeforeTheProjectTiebreakStillPages(t *testing.T) {
	c := newPageCache(t)
	for i := range 5 {
		insertTestSession(t, c.db, testSession{id: fmt.Sprintf("s-%d", i), costUSD: float64(i)})
	}
	// A scroll in flight when the binary changed carries a cursor with no "p"
	// at all — spelled out here rather than minted, because the struct now
	// always emits the key. It decodes with Project empty, and
	// `c.project_path < ''` is never true, so the predicate degrades to the
	// id-only one it was minted under rather than dropping the rest of the
	// scroll.
	legacy := base64.RawURLEncoding.EncodeToString([]byte(`{"s":"cost","v":"3","id":"s-3"}`))
	page, err := c.ListPage(SessionQuery{Sort: SortCost, Limit: 10, Cursor: legacy})
	if err != nil {
		t.Fatalf("paging a legacy cursor: %v", err)
	}
	if got := ids(page); !slices.Equal(got, []string{"s-2", "s-1", "s-0"}) {
		t.Errorf("legacy cursor continued at %v, want [s-2 s-1 s-0]", got)
	}
}

func TestListPage_CostTiesPageExactlyAcrossFractionalValues(t *testing.T) {
	c := newPageCache(t)
	// The cost cursor carries Go's Cost.TotalUSD + SubagentCost.TotalUSD while
	// the keyset predicate compares SQL's c.total_cost_usd + COALESCE(sa.tc, 0).
	// The tiebreak is an equality on that value, so the two sums have to agree
	// to the bit — a coupling only fractional, delegated costs exercise, since
	// whole dollars round-trip through any arithmetic unchanged.
	const perSession = 36.30 // 12.05 main thread + 24.25 delegated
	for i := range 7 {
		insertTestSession(t, c.db, testSession{
			id: fmt.Sprintf("frac-%d", i), costUSD: 12.05, subCostUSD: 24.25,
		})
	}

	seen := map[string]int{}
	cursor := ""
	for {
		page, err := c.ListPage(SessionQuery{Sort: SortCost, Limit: 3, Cursor: cursor})
		if err != nil {
			t.Fatalf("paging: %v", err)
		}
		for _, s := range page.Items {
			seen[s.SessionID]++
			if got := s.TotalCost().TotalUSD; math.Abs(got-perSession) > 1e-9 {
				t.Errorf("session %s totals %v, want %v", s.SessionID, got, perSession)
			}
		}
		if !page.HasMore {
			break
		}
		cursor = page.NextCursor
	}

	if len(seen) != 7 {
		t.Errorf("saw %d of 7 sessions; a cursor value that does not match the SQL sum "+
			"skips the row it points at", len(seen))
	}
	for id, n := range seen {
		if n != 1 {
			t.Errorf("session %s returned %d times", id, n)
		}
	}
}

func TestListPage_CursorFromAnotherSortIsRejected(t *testing.T) {
	c := newPageCache(t)
	for i := range 3 {
		insertTestSession(t, c.db, testSession{id: fmt.Sprintf("s-%d", i), costUSD: float64(i)})
	}
	first, err := c.ListPage(SessionQuery{Sort: SortCost, Limit: 1})
	if err != nil {
		t.Fatalf("first page: %v", err)
	}
	// Continuing a cost-ordered scroll under the recency order would page
	// through one ordering using another's position.
	if _, err := c.ListPage(SessionQuery{Sort: SortRecent, Cursor: first.NextCursor}); err == nil {
		t.Fatal("expected a mismatched cursor to be rejected")
	}
}

func TestListPage_SortsByEveryOfferedColumn(t *testing.T) {
	c := newPageCache(t)
	base := time.Date(2026, 8, 1, 12, 0, 0, 0, time.UTC)
	insertTestSession(t, c.db, testSession{
		id: "cheap-recent", last: base.Add(2 * time.Hour),
		costUSD: 1, inputTokens: 10, outputTokens: 10, activeMs: 1000, messages: 1,
	})
	insertTestSession(t, c.db, testSession{
		id: "rich-old", last: base,
		costUSD: 50, inputTokens: 900, outputTokens: 900, activeMs: 900_000, messages: 90,
	})

	cases := map[SessionSort]string{
		SortRecent:   "cheap-recent",
		SortCost:     "rich-old",
		SortTokens:   "rich-old",
		SortDuration: "rich-old",
		SortMessages: "rich-old",
	}
	for sort, wantFirst := range cases {
		page, err := c.ListPage(SessionQuery{Sort: sort, Limit: 5})
		if err != nil {
			t.Fatalf("sort %s: %v", sort, err)
		}
		if got := ids(page); len(got) == 0 || got[0] != wantFirst {
			t.Errorf("sort %s returned %v, want %s first", sort, got, wantFirst)
		}
	}
}

func TestListPage_FiltersOnDelegatedTotalsNotJustTheMainThread(t *testing.T) {
	c := newPageCache(t)
	// The row renders $36.30 (main thread + delegated). Filtering on the main
	// thread's $12.05 alone would hide it from "cost at most 40" — the exact
	// bug sessionMetrics.ts was created to prevent, now reproduced in SQL.
	insertTestSession(t, c.db, testSession{
		id: "delegating", costUSD: 12.05, subCostUSD: 24.25,
		inputTokens: 1000, subInputTokens: 4500,
		activeMs: 900_000, subActiveMs: 2_400_000,
	})

	if page, err := c.ListPage(SessionQuery{Cost: NumericRange{Max: fptr(40)}}); err != nil {
		t.Fatalf("cost filter: %v", err)
	} else if len(page.Items) != 1 {
		t.Errorf("cost <= 40 returned %d sessions, want the $36.30 one", len(page.Items))
	}
	if page, err := c.ListPage(SessionQuery{Cost: NumericRange{Max: fptr(30)}}); err != nil {
		t.Fatalf("cost filter: %v", err)
	} else if len(page.Items) != 0 {
		t.Errorf("cost <= 30 returned %d sessions, want none", len(page.Items))
	}
	// 55 minutes of active duration, of which 40 are delegated.
	if page, err := c.ListPage(SessionQuery{
		DurationMinutes: NumericRange{Min: fptr(50)},
	}); err != nil {
		t.Fatalf("duration filter: %v", err)
	} else if len(page.Items) != 1 {
		t.Errorf("duration >= 50min returned %d sessions, want 1", len(page.Items))
	}
	if page, err := c.ListPage(SessionQuery{
		TokensIn: NumericRange{Min: fptr(5000)},
	}); err != nil {
		t.Fatalf("tokens filter: %v", err)
	} else if len(page.Items) != 1 {
		t.Errorf("tokens_in >= 5000 returned %d sessions, want 1", len(page.Items))
	}
}

func TestListPage_SearchMatchesTheSameFieldsTheClientDid(t *testing.T) {
	c := newPageCache(t)
	insertTestSession(t, c.db, testSession{id: "abc-123", preview: "fix the parser"})
	insertTestSession(t, c.db, testSession{id: "def-456", customTitle: "Parser rewrite"})
	insertTestSession(t, c.db, testSession{
		id: "ghi-789", project: "/home/dev/parser-tools", preview: "unrelated",
	})
	insertTestSession(t, c.db, testSession{id: "jkl-000", preview: "nothing to see"})

	page, err := c.ListPage(SessionQuery{Search: "PARSER"})
	if err != nil {
		t.Fatalf("search: %v", err)
	}
	if len(page.Items) != 3 {
		t.Errorf("case-insensitive search matched %v, want the three parser rows", ids(page))
	}

	byID, err := c.ListPage(SessionQuery{Search: "def-4"})
	if err != nil {
		t.Fatalf("search by id: %v", err)
	}
	if len(byID.Items) != 1 || byID.Items[0].SessionID != "def-456" {
		t.Errorf("id search returned %v", ids(byID))
	}
}

func TestListPage_SearchTreatsWildcardsLiterally(t *testing.T) {
	c := newPageCache(t)
	insertTestSession(t, c.db, testSession{id: "pct", preview: "coverage is 100% now"})
	insertTestSession(t, c.db, testSession{id: "other", preview: "coverage is fine"})

	page, err := c.ListPage(SessionQuery{Search: "100%"})
	if err != nil {
		t.Fatalf("search: %v", err)
	}
	// An unescaped % would make this match every row whose preview contains
	// "100" followed by anything — including, on a real corpus, nothing to do
	// with what was typed.
	if len(page.Items) != 1 || page.Items[0].SessionID != "pct" {
		t.Errorf("literal %% search returned %v, want just the 100%% row", ids(page))
	}
}

func TestListPage_TimeFilterKeepsSessionsWhoseWindowOverlaps(t *testing.T) {
	c := newPageCache(t)
	// Started before the window and still running inside it. Containment on
	// last_activity would keep it; containment on start_time would not. The
	// list has always used overlap, and it is the reading a person means by
	// "what was I working on then".
	insertTestSession(t, c.db, testSession{
		id:    "spans",
		start: time.Date(2026, 7, 30, 9, 0, 0, 0, time.UTC),
		last:  time.Date(2026, 8, 2, 9, 0, 0, 0, time.UTC),
	})
	insertTestSession(t, c.db, testSession{
		id:    "before",
		start: time.Date(2026, 7, 1, 9, 0, 0, 0, time.UTC),
		last:  time.Date(2026, 7, 1, 10, 0, 0, 0, time.UTC),
	})

	from := time.Date(2026, 8, 1, 0, 0, 0, 0, time.UTC)
	to := time.Date(2026, 8, 1, 23, 59, 59, 0, time.UTC)
	page, err := c.ListPage(SessionQuery{From: &from, To: &to})
	if err != nil {
		t.Fatalf("time filter: %v", err)
	}
	if len(page.Items) != 1 || page.Items[0].SessionID != "spans" {
		t.Errorf("overlap filter returned %v, want [spans]", ids(page))
	}
}

func TestListPage_DrilldownWindowsReplaceTheRange(t *testing.T) {
	c := newPageCache(t)
	hour := time.Date(2026, 8, 1, 14, 0, 0, 0, time.UTC)
	insertTestSession(t, c.db, testSession{
		id: "in-window", start: hour.Add(10 * time.Minute), last: hour.Add(40 * time.Minute),
	})
	insertTestSession(t, c.db, testSession{
		id: "next-hour", start: hour.Add(90 * time.Minute), last: hour.Add(100 * time.Minute),
	})

	page, err := c.ListPage(SessionQuery{
		Windows: []TimeWindow{{From: hour, To: hour.Add(time.Hour)}},
	})
	if err != nil {
		t.Fatalf("drill-down: %v", err)
	}
	if len(page.Items) != 1 || page.Items[0].SessionID != "in-window" {
		t.Errorf("drill-down returned %v, want [in-window]", ids(page))
	}
}

func TestListPage_RejectsAnAbsurdDrilldownWindowCount(t *testing.T) {
	c := newPageCache(t)
	windows := make([]TimeWindow, maxDrilldownWindows+1)
	base := time.Date(2026, 8, 1, 0, 0, 0, 0, time.UTC)
	for i := range windows {
		windows[i] = TimeWindow{From: base.Add(time.Duration(i) * time.Hour), To: base.Add(time.Duration(i+1) * time.Hour)}
	}
	if _, err := c.ListPage(SessionQuery{Windows: windows}); err == nil {
		t.Fatal("expected an over-long window list to be rejected")
	}
}

func TestListPage_HiddenProjectsAreFilteredHereToo(t *testing.T) {
	c := newPageCache(t)
	insertTestSession(t, c.db, testSession{id: "visible", project: "/home/dev/shown"})
	insertTestSession(t, c.db, testSession{id: "hidden", project: "/home/dev/secret"})

	// The paged path never loads the corpus, so it cannot inherit the filter
	// Cache.loadOrEmpty applies. Hiding must reach it independently or a hidden
	// project silently reappears on exactly the surface it is most visible on.
	ApplyDataSettings(0, []string{"/home/dev/secret"})
	defer ApplyDataSettings(0, nil)

	page, err := c.ListPage(SessionQuery{})
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(page.Items) != 1 || page.Items[0].SessionID != "visible" {
		t.Errorf("hidden project leaked into the page: %v", ids(page))
	}

	facets, err := c.Facets(SessionQuery{})
	if err != nil {
		t.Fatalf("facets: %v", err)
	}
	if facets.Total != 1 {
		t.Errorf("facets counted %d sessions, want 1 with a project hidden", facets.Total)
	}
}

func TestFacets_TotalsMatchTheFilteredSet(t *testing.T) {
	c := newPageCache(t)
	insertTestSession(t, c.db, testSession{
		id: "a", model: "opus", permissionMode: "plan",
		inputTokens: 100, outputTokens: 50, costUSD: 2, favorite: true,
	})
	insertTestSession(t, c.db, testSession{
		id: "b", model: "sonnet", permissionMode: "bypassPermissions",
		inputTokens: 10, outputTokens: 5, costUSD: 0.5, prURL: "https://example.test/pr/1",
	})

	all, err := c.Facets(SessionQuery{})
	if err != nil {
		t.Fatalf("facets: %v", err)
	}
	if all.Total != 2 || all.TotalTokens != 165 || math.Abs(all.TotalCostUSD-2.5) > 1e-9 {
		t.Errorf("unfiltered facets = %+v", all)
	}
	if !all.HasFavorites || !all.HasPRs {
		t.Errorf("expected both toggles to be offered, got %+v", all)
	}
	if len(all.Models) != 2 || len(all.PermissionModes) != 2 {
		t.Errorf("expected two models and two modes, got %v / %v", all.Models, all.PermissionModes)
	}

	// The options stay the whole corpus's even when the filter narrows to one
	// model — a dropdown that removes the option you just picked cannot be
	// un-picked.
	narrowed, err := c.Facets(SessionQuery{Model: "opus"})
	if err != nil {
		t.Fatalf("narrowed facets: %v", err)
	}
	if narrowed.Total != 1 || narrowed.TotalTokens != 150 {
		t.Errorf("narrowed facets = %+v", narrowed)
	}
	if len(narrowed.Models) != 2 {
		t.Errorf("filtering by model shrank the model options to %v", narrowed.Models)
	}
}

func TestFacets_TokenP90MatchesTheClientSideIndex(t *testing.T) {
	c := newPageCache(t)
	// 1..10 tokens. floor(0.9 * (10-1)) = 8 → the 9th smallest, which is 9.
	for i := 1; i <= 10; i++ {
		insertTestSession(t, c.db, testSession{id: fmt.Sprintf("s-%02d", i), inputTokens: i})
	}
	f, err := c.Facets(SessionQuery{})
	if err != nil {
		t.Fatalf("facets: %v", err)
	}
	if f.TokenP90 != 9 {
		t.Errorf("token p90 = %d, want 9 (floor(0.9*(n-1)) into the ascending series)", f.TokenP90)
	}
}

func TestListPage_LinksFilter(t *testing.T) {
	c := newPageCache(t)
	insertTestSession(t, c.db, testSession{id: "linked", prURL: "https://example.test/pr/7"})
	insertTestSession(t, c.db, testSession{id: "bare"})

	with, err := c.ListPage(SessionQuery{Links: LinksWith})
	if err != nil {
		t.Fatalf("links with: %v", err)
	}
	if len(with.Items) != 1 || with.Items[0].SessionID != "linked" {
		t.Errorf("links=with returned %v", ids(with))
	}
	if len(with.Items[0].PRs) != 1 {
		t.Errorf("expected the linked PR to be attached to the page row, got %d", len(with.Items[0].PRs))
	}

	without, err := c.ListPage(SessionQuery{Links: LinksWithout})
	if err != nil {
		t.Fatalf("links without: %v", err)
	}
	if len(without.Items) != 1 || without.Items[0].SessionID != "bare" {
		t.Errorf("links=without returned %v", ids(without))
	}
}

func TestListPage_LimitIsClamped(t *testing.T) {
	c := newPageCache(t)
	for i := range 5 {
		insertTestSession(t, c.db, testSession{id: fmt.Sprintf("s-%d", i)})
	}
	page, err := c.ListPage(SessionQuery{Limit: 10_000})
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(page.Items) != 5 {
		t.Errorf("got %d items", len(page.Items))
	}
	if q := (SessionQuery{Limit: 10_000}); q.limit() != MaxPageSize {
		t.Errorf("limit clamped to %d, want %d", q.limit(), MaxPageSize)
	}
}

// ─── Shared metric parity ─────────────────────────────────────────────────────

type metricVectors struct {
	Cases []struct {
		Name    string `json:"name"`
		Session struct {
			InputTokens              int     `json:"input_tokens"`
			OutputTokens             int     `json:"output_tokens"`
			SubagentInputTokens      int     `json:"subagent_input_tokens"`
			SubagentOutputTokens     int     `json:"subagent_output_tokens"`
			TotalCostUSD             float64 `json:"total_cost_usd"`
			SubagentCostUSD          float64 `json:"subagent_cost_usd"`
			ActiveDurationMs         int64   `json:"active_duration_ms"`
			SubagentActiveDurationMs int64   `json:"subagent_active_duration_ms"`
			MessageCount             int     `json:"message_count"`
		} `json:"session"`
		Expect struct {
			InputTokens     int     `json:"input_tokens"`
			OutputTokens    int     `json:"output_tokens"`
			Tokens          int     `json:"tokens"`
			CostUSD         float64 `json:"cost_usd"`
			DurationMs      int64   `json:"duration_ms"`
			DurationMinutes float64 `json:"duration_minutes"`
			Messages        int     `json:"messages"`
		} `json:"expect"`
	} `json:"cases"`
}

// TestSessionMetricSQL_MatchesTheSharedVectors asserts that the SQL the list
// filters and sorts by produces the figures the fixture declares.
//
// frontend/src/lib/sessionMetrics.test.ts asserts the TypeScript against the
// same file. Together they are what keeps a rendered column and the filter that
// hides its row from disagreeing — the bug sessionMetrics.ts was extracted to
// prevent, which moving the filtering into SQL would otherwise have reopened in
// a second language.
func TestSessionMetricSQL_MatchesTheSharedVectors(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join("testdata", "session_metric_vectors.json"))
	if err != nil {
		t.Fatalf("reading vectors: %v", err)
	}
	var vectors metricVectors
	if err := json.Unmarshal(raw, &vectors); err != nil {
		t.Fatalf("parsing vectors: %v", err)
	}
	if len(vectors.Cases) == 0 {
		t.Fatal("the shared vectors file declares no cases")
	}

	c := newPageCache(t)
	for i, tc := range vectors.Cases {
		id := fmt.Sprintf("vec-%d", i)
		insertTestSession(t, c.db, testSession{
			id:              id,
			inputTokens:     tc.Session.InputTokens,
			outputTokens:    tc.Session.OutputTokens,
			costUSD:         tc.Session.TotalCostUSD,
			activeMs:        tc.Session.ActiveDurationMs,
			messages:        tc.Session.MessageCount,
			subInputTokens:  tc.Session.SubagentInputTokens,
			subOutputTokens: tc.Session.SubagentOutputTokens,
			subCostUSD:      tc.Session.SubagentCostUSD,
			subActiveMs:     tc.Session.SubagentActiveDurationMs,
		})

		query := "SELECT " + sqlInputTokens + ", " + sqlOutputTokens + ", " + sqlTokens +
			", " + sqlCostUSD + ", " + sqlActiveDurationMs + ", " + sqlMessageCount +
			sessionSummarySource + "\nWHERE c.session_id = ?"
		var in, out, tokens, messages int
		var cost float64
		var durationMs int64
		if err := c.db.QueryRowContext(context.Background(), query, id).
			Scan(&in, &out, &tokens, &cost, &durationMs, &messages); err != nil {
			t.Fatalf("%s: querying metrics: %v", tc.Name, err)
		}

		check := func(label string, got, want int) {
			if got != want {
				t.Errorf("%s: %s = %d, want %d", tc.Name, label, got, want)
			}
		}
		check("input tokens", in, tc.Expect.InputTokens)
		check("output tokens", out, tc.Expect.OutputTokens)
		check("tokens", tokens, tc.Expect.Tokens)
		check("messages", messages, tc.Expect.Messages)
		if math.Abs(cost-tc.Expect.CostUSD) > 1e-9 {
			t.Errorf("%s: cost = %v, want %v", tc.Name, cost, tc.Expect.CostUSD)
		}
		if durationMs != tc.Expect.DurationMs {
			t.Errorf("%s: duration = %dms, want %dms", tc.Name, durationMs, tc.Expect.DurationMs)
		}
		if got := float64(durationMs) / 60_000; math.Abs(got-tc.Expect.DurationMinutes) > 1e-9 {
			t.Errorf("%s: duration = %v min, want %v", tc.Name, got, tc.Expect.DurationMinutes)
		}
	}
}

// The SQL half of the config-dir scope. The paged path never loads the corpus,
// so it cannot inherit the filter Cache.loadOrEmpty applies — the same reason
// TestListPage_HiddenProjectsAreFilteredHereToo exists for hidden projects.
func TestListPage_ConfigDirScopeIsAppliedHereToo(t *testing.T) {
	// newPageCache points HOME at its own temp dir, so the config dirs are
	// installed after it and the default is read back from there.
	c := newPageCache(t)
	second := filepath.Join(t.TempDir(), ".claude-personal")
	t.Setenv(config.ClaudeConfigDirEnvVar, "")
	config.ApplyClaudeDirs("", []string{second})
	t.Cleanup(func() { config.ApplyClaudeDirs("", nil) })

	insertTestSession(t, c.db, testSession{id: "default-dir", configDir: config.DefaultClaudeConfigDir()})
	insertTestSession(t, c.db, testSession{id: "second-dir", configDir: second})
	insertTestSession(t, c.db, testSession{id: "removed-dir", configDir: "/dir/nobody/configured"})
	insertTestSession(t, c.db, testSession{id: "legacy"}) // pre-migration row, blank dir

	page, err := c.ListPage(SessionQuery{})
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	got := ids(page)
	if len(got) != 3 {
		t.Fatalf("page = %v, want the three indexed rows (removed-dir excluded)", got)
	}
	for _, id := range got {
		if id == "removed-dir" {
			t.Error("a session from an unconfigured dir leaked into the page")
		}
	}

	facets, err := c.Facets(SessionQuery{})
	if err != nil {
		t.Fatalf("facets: %v", err)
	}
	if facets.Total != 3 {
		t.Errorf("facets counted %d, want 3 — totals must match the rows", facets.Total)
	}

	// Narrowing to one account returns exactly that account's sessions.
	narrowed, err := c.ListPage(SessionQuery{ConfigDir: second})
	if err != nil {
		t.Fatalf("list narrowed: %v", err)
	}
	if got := ids(narrowed); len(got) != 1 || got[0] != "second-dir" {
		t.Errorf("ConfigDir filter returned %v, want [second-dir]", got)
	}
	narrowedFacets, err := c.Facets(SessionQuery{ConfigDir: second})
	if err != nil {
		t.Fatalf("facets narrowed: %v", err)
	}
	if narrowedFacets.Total != 1 {
		t.Errorf("narrowed facets total = %d, want 1", narrowedFacets.Total)
	}
}
