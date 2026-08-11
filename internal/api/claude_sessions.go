package api

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/claudesessions"
)

// handleListClaudeSessions returns one page of Claude Code sessions.
//
// It used to return every session as a bare, unpaginated array and leave the
// browser to filter, sort, group and render all of them. That shipped 1.33 MB
// and ~54k DOM nodes on the reference corpus and projected to ~8.3 MB and
// ~340k nodes at 5,000 sessions, where a single keystroke in the search box
// freezes the tab for seconds. Every predicate the client applied is now SQL,
// and the response is an envelope carrying one keyset-paged page.
//
// See sessionQueryFromRequest for the parameters.
func (s *Server) handleListClaudeSessions(w http.ResponseWriter, r *http.Request) {
	q, err := sessionQueryFromRequest(r)
	if err != nil {
		s.writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	page, err := s.claudeSessionCache.ListPage(q)
	if err != nil {
		if errors.Is(err, claudesessions.ErrCursorMismatch) {
			s.writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		s.logger.Error("list claude sessions failed", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to list sessions")
		return
	}
	s.writeJSON(w, http.StatusOK, page)
}

// handleGetClaudeSessionFacets returns the aggregate a single page cannot
// answer: the totals across the whole filtered set, and the options the filter
// controls offer.
//
// A separate endpoint rather than a field on the page, because the two have
// different lifetimes: the totals change when the filter changes, the pages
// change as the user scrolls, and folding them together would recompute a
// corpus-wide aggregate on every scroll tick.
func (s *Server) handleGetClaudeSessionFacets(w http.ResponseWriter, r *http.Request) {
	q, err := sessionQueryFromRequest(r)
	if err != nil {
		s.writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	facets, err := s.claudeSessionCache.Facets(q)
	if err != nil {
		s.logger.Error("claude session facets failed", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to compute facets")
		return
	}
	s.writeJSON(w, http.StatusOK, facets)
}

// sessionQueryFromRequest reads the sessions list's filter, sort and page
// parameters. Both the list and the facets endpoint parse through it, so the
// counter in the toolbar and the rows below it are always describing the same
// predicate.
//
// Query params:
//
//	project           decoded project path, exact match
//	q                 case-insensitive substring over ID, titles, preview, path
//	favorites         "true" to keep only starred sessions
//	links             "with" | "without"
//	permission_mode   exact match
//	model             exact match
//	messages_min/max  inclusive bounds on conversational turns
//	duration_min/max  inclusive bounds on active duration, in minutes
//	tokens_in_min/max, tokens_out_min/max, cost_min/max
//	from, to          RFC3339; bound the session's activity window by overlap
//	windows           "fromMs-toMs,…" drill-down windows; replaces from/to
//	sort              recent | cost | tokens | duration | messages
//	limit             page size, clamped to MaxPageSize
//	cursor            continues a previous page
//
// A malformed numeric bound is ignored rather than rejected: these arrive from
// number inputs a user is mid-way through typing, and refusing the request
// would blank the list between keystrokes.
func sessionQueryFromRequest(r *http.Request) (claudesessions.SessionQuery, error) {
	v := r.URL.Query()
	q := claudesessions.SessionQuery{
		Project:         v.Get("project"),
		ConfigDir:       v.Get("config_dir"),
		Search:          v.Get("q"),
		FavoritesOnly:   v.Get("favorites") == "true",
		Links:           claudesessions.LinkFilter(v.Get("links")),
		PermissionMode:  v.Get("permission_mode"),
		Model:           v.Get("model"),
		Messages:        numericRange(v, "messages"),
		DurationMinutes: numericRange(v, "duration"),
		TokensIn:        numericRange(v, "tokens_in"),
		TokensOut:       numericRange(v, "tokens_out"),
		Cost:            numericRange(v, "cost"),
		Sort:            claudesessions.SessionSort(v.Get("sort")),
		Cursor:          v.Get("cursor"),
	}
	if q.Links != claudesessions.LinksAny &&
		q.Links != claudesessions.LinksWith && q.Links != claudesessions.LinksWithout {
		return claudesessions.SessionQuery{}, fmt.Errorf("invalid links filter %q", v.Get("links"))
	}
	if raw := v.Get("limit"); raw != "" {
		n, err := strconv.Atoi(raw)
		if err != nil {
			return claudesessions.SessionQuery{}, fmt.Errorf("invalid limit %q", raw)
		}
		q.Limit = n
	}
	q.From = optionalTime(v.Get("from"))
	q.To = optionalTime(v.Get("to"))
	windows, err := parseDrilldownWindows(v.Get("windows"))
	if err != nil {
		return claudesessions.SessionQuery{}, err
	}
	q.Windows = windows
	return q, nil
}

// numericRange reads "<name>_min" and "<name>_max" into an inclusive range.
func numericRange(v url.Values, name string) claudesessions.NumericRange {
	return claudesessions.NumericRange{
		Min: optionalFloat(v.Get(name + "_min")),
		Max: optionalFloat(v.Get(name + "_max")),
	}
}

// optionalFloat returns nil for an absent or unparseable value, which the
// filter reads as "unbounded on that side" — distinct from zero, which is a
// real bound.
func optionalFloat(raw string) *float64 {
	if raw == "" {
		return nil
	}
	n, err := strconv.ParseFloat(raw, 64)
	if err != nil {
		return nil
	}
	return &n
}

func optionalTime(raw string) *time.Time {
	if raw == "" {
		return nil
	}
	t, err := time.Parse(time.RFC3339, raw)
	if err != nil {
		return nil
	}
	return &t
}

// parseDrilldownWindows decodes the "fromMs-toMs,fromMs-toMs" form the
// analytics charts link with, mirroring the client's encodeWindows.
//
// A malformed window is an error rather than a silent drop: the windows *are*
// the filter when a drill-down is active, and quietly discarding half of them
// would show a plausible-looking but wrong set of sessions.
func parseDrilldownWindows(raw string) ([]claudesessions.TimeWindow, error) {
	if raw == "" {
		return nil, nil
	}
	parts := strings.Split(raw, ",")
	windows := make([]claudesessions.TimeWindow, 0, len(parts))
	for _, part := range parts {
		from, to, ok := strings.Cut(part, "-")
		if !ok {
			return nil, fmt.Errorf("invalid drill-down window %q", part)
		}
		fromMs, err := strconv.ParseInt(from, 10, 64)
		if err != nil {
			return nil, fmt.Errorf("invalid drill-down window start %q", from)
		}
		toMs, err := strconv.ParseInt(to, 10, 64)
		if err != nil {
			return nil, fmt.Errorf("invalid drill-down window end %q", to)
		}
		if toMs <= fromMs {
			return nil, fmt.Errorf("drill-down window %q ends before it starts", part)
		}
		windows = append(windows, claudesessions.TimeWindow{
			From: time.UnixMilli(fromMs).UTC(),
			To:   time.UnixMilli(toMs).UTC(),
		})
	}
	return windows, nil
}

// handleListClaudeProjects returns all distinct project directories containing sessions.
//
// Projects the user has hidden are omitted, so every project picker in the UI
// offers only what the figures beside it actually cover. The one caller that
// needs the full list is the Data & Analytics settings tab, which cannot let
// you unhide a project it is not allowed to show you: it passes
// include_hidden=true and reads the per-project Hidden flag.
func (s *Server) handleListClaudeProjects(w http.ResponseWriter, r *http.Request) {
	projects, err := claudesessions.ListProjects()
	if err != nil {
		s.logger.Error("list claude projects failed", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to list projects")
		return
	}

	includeHidden := r.URL.Query().Get("include_hidden") == "true"
	visible := make([]claudesessions.ClaudeProject, 0, len(projects))
	for _, p := range projects {
		p.Hidden = claudesessions.IsProjectHidden(p.DecodedPath)
		if p.Hidden && !includeHidden {
			continue
		}
		visible = append(visible, p)
	}
	s.writeJSON(w, http.StatusOK, visible)
}

// handleGetClaudeSession returns the full detail of a single Claude Code session
// including all messages, token usage, and todos.
func (s *Server) handleGetClaudeSession(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	detail, err := claudesessions.GetSessionDetail(id, s.logger)
	if err != nil {
		s.logger.Error("get claude session failed", "session_id", id, "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to get session")
		return
	}
	if detail == nil {
		s.writeError(w, http.StatusNotFound, "session not found")
		return
	}
	// Attach user-defined fields from the SQLite cache (not present in JSONL).
	detail.CustomTitle = s.claudeSessionCache.GetCustomTitle(id)
	detail.IsFavorite = s.claudeSessionCache.GetFavorite(id)
	// Claude Code's own titles, the linked PRs, the compaction counters and the
	// session metadata events all come from the transcript GetSessionDetail just
	// read. The cache is consulted for the titles only as a fallback, and only
	// when the transcript carried none — reading it unconditionally would blank
	// them for a session the scanner has not reached yet.
	if detail.NativeTitle == "" && detail.AITitle == "" {
		detail.NativeTitle, detail.AITitle = s.claudeSessionCache.GetTitles(id)
	}
	detail.DisplayTitle = detail.ResolveDisplayTitle()
	// Cost is stored per session by the scanner, not derivable from a re-read of
	// the transcript, so it comes from the cache along with the unpriced-model
	// disclosure that qualifies it. A session the scanner has not reached yet
	// keeps the zero value, which the UI shows as $0.00 rather than a wrong
	// figure.
	if cached := s.claudeSessionCache.GetSummary(id); cached != nil {
		detail.Cost = cached.Cost
		detail.SubagentCost = cached.SubagentCost
		detail.UnpricedModels = cached.UnpricedModels
		detail.UnpricedTokens = cached.UnpricedTokens
	}
	// Sub-agent transcripts live in sibling files, so they come from the cache
	// too rather than from the session JSONL this detail was read from.
	detail.Subagents = s.claudeSessionCache.ListSubagents(id)
	detail.SubagentCount = len(detail.Subagents)
	detail.SubagentUsage = claudesessions.TokenUsage{}
	for _, sa := range detail.Subagents {
		detail.SubagentUsage.InputTokens += sa.Usage.InputTokens
		detail.SubagentUsage.OutputTokens += sa.Usage.OutputTokens
		detail.SubagentUsage.CacheCreationTokens += sa.Usage.CacheCreationTokens
		detail.SubagentUsage.CacheReadTokens += sa.Usage.CacheReadTokens
	}
	s.writeJSON(w, http.StatusOK, detail)
}

// handleRefreshClaudeSessionCache invalidates the cached scan metadata and
// starts a rescan in the background, returning 202 immediately.
func (s *Server) handleRefreshClaudeSessionCache(w http.ResponseWriter, _ *http.Request) {
	s.claudeSessionCache.Invalidate()
	// EnsureScan rather than `go List()`: it admits exactly one scan, so a
	// double-click cannot start a second full re-read.
	s.claudeSessionCache.EnsureScan()
	w.WriteHeader(http.StatusAccepted)
}

// claudeSessionStatus tells the UI whether the cost figures it is showing are
// current. It is a separate endpoint rather than an envelope around
// GET /claude-sessions, which returns a bare array — wrapping that would break
// every existing client for no gain.
type claudeSessionStatus struct {
	// CostsStale means the served costs were computed under an older pricing
	// catalog and a re-cost is pending. The figures are not wrong for the rates
	// they were computed under, so they are labeled rather than withheld.
	CostsStale bool `json:"costs_stale"`
	// ScanInProgress means a background scan is running right now.
	ScanInProgress bool `json:"scan_in_progress"`
	// FilesDone and FilesTotal are the running scan's position. Both zero when
	// nothing is running or the scan had nothing to re-read.
	//
	// The list no longer blocks on a cold-start scan — at 5,000 sessions that
	// would be minutes and then a timeout — so an empty list during a first run
	// needs something to say beyond "no sessions".
	FilesDone  int `json:"files_done"`
	FilesTotal int `json:"files_total"`
	// LastScannedAt is empty when the cache has never been scanned.
	LastScannedAt string `json:"last_scanned_at"`
}

// handleGetClaudeSessionStatus reports cache freshness so the sessions list can
// show a pending-refresh indicator instead of blocking on a re-cost.
func (s *Server) handleGetClaudeSessionStatus(w http.ResponseWriter, _ *http.Request) {
	var lastScanned string
	if t := s.claudeSessionCache.LastScannedAt(); !t.IsZero() {
		lastScanned = t.UTC().Format(time.RFC3339)
	}
	done, total := s.claudeSessionCache.ScanProgress()
	s.writeJSON(w, http.StatusOK, claudeSessionStatus{
		CostsStale:     s.claudeSessionCache.CostsStale(),
		ScanInProgress: s.claudeSessionCache.ScanInProgress(),
		FilesDone:      done,
		FilesTotal:     total,
		LastScannedAt:  lastScanned,
	})
}

// handleUpdateClaudeSession updates mutable fields of a cached Claude Code session.
// Supports custom_title and is_favorite — all JSONL-derived fields are read-only.
func (s *Server) handleUpdateClaudeSession(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	var req struct {
		CustomTitle *string `json:"custom_title"`
		IsFavorite  *bool   `json:"is_favorite"`
	}
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}
	if req.CustomTitle == nil && req.IsFavorite == nil {
		s.writeError(w, http.StatusBadRequest, "no fields to update")
		return
	}
	if req.CustomTitle != nil {
		title := strings.TrimSpace(*req.CustomTitle)
		if err := s.claudeSessionCache.UpdateCustomTitle(id, title); err != nil {
			s.logger.Error("update claude session title failed", "session_id", id, "error", err)
			s.writeError(w, http.StatusInternalServerError, "failed to update title")
			return
		}
	}
	if req.IsFavorite != nil {
		if err := s.claudeSessionCache.UpdateFavorite(id, *req.IsFavorite); err != nil {
			s.logger.Error("update claude session favorite failed", "session_id", id, "error", err)
			s.writeError(w, http.StatusInternalServerError, "failed to update favorite")
			return
		}
	}
	w.WriteHeader(http.StatusNoContent)
}

// handleContinueClaudeSession creates a new Agento chat session that inherits the
// given Claude Code session ID so the SDK can resume the existing conversation.
func (s *Server) handleContinueClaudeSession(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	// Look up the session to get its working directory and model.
	detail, err := claudesessions.GetSessionDetail(id, s.logger)
	if err != nil {
		s.logger.Error("continue claude session: lookup failed", "session_id", id, "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to look up session")
		return
	}
	if detail == nil {
		s.writeError(w, http.StatusNotFound, "session not found")
		return
	}

	// Create a new Agento chat session with no agent slug, inheriting the session's cwd.
	chatSession, err := s.chatSvc.CreateSession(r.Context(), "", detail.CWD, detail.Model, "")
	if err != nil {
		s.logger.Error("continue claude session: create chat failed", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to create chat session")
		return
	}

	// Link the new Agento chat to the original Claude Code session so the SDK
	// picks up the conversation history when the first message is sent.
	chatSession.SDKSession = id
	if err := s.chatSvc.UpdateSession(r.Context(), chatSession); err != nil {
		s.logger.Error("continue claude session: update session failed", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to link session")
		return
	}

	s.writeJSON(w, http.StatusCreated, map[string]string{
		"chat_id": chatSession.ID,
	})
}
