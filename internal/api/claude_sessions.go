package api

import (
	"encoding/json"
	"net/http"
	"strings"
	"time"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/claudesessions"
)

// handleListClaudeSessions returns all Claude Code sessions with optional filtering.
// Query params:
//   - project: filter by decoded project path (exact match)
//   - q: search by session ID prefix or preview text (case-insensitive substring)
func (s *Server) handleListClaudeSessions(w http.ResponseWriter, r *http.Request) {
	sessions := s.claudeSessionCache.List()

	project := r.URL.Query().Get("project")
	if project != "" {
		var filtered []claudesessions.ClaudeSessionSummary
		for _, sess := range sessions {
			if sess.ProjectPath == project {
				filtered = append(filtered, sess)
			}
		}
		sessions = filtered
	}

	q := strings.ToLower(r.URL.Query().Get("q"))
	if q != "" {
		var filtered []claudesessions.ClaudeSessionSummary
		for _, sess := range sessions {
			if strings.Contains(strings.ToLower(sess.SessionID), q) ||
				strings.Contains(strings.ToLower(sess.Preview), q) {
				filtered = append(filtered, sess)
			}
		}
		sessions = filtered
	}

	if sessions == nil {
		sessions = []claudesessions.ClaudeSessionSummary{}
	}
	s.writeJSON(w, http.StatusOK, sessions)
}

// handleListClaudeProjects returns all distinct project directories containing sessions.
func (s *Server) handleListClaudeProjects(w http.ResponseWriter, r *http.Request) {
	projects, err := claudesessions.ListProjects()
	if err != nil {
		s.logger.Error("list claude projects failed", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to list projects")
		return
	}
	if projects == nil {
		projects = []claudesessions.ClaudeProject{}
	}
	s.writeJSON(w, http.StatusOK, projects)
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
	s.writeJSON(w, http.StatusOK, claudeSessionStatus{
		CostsStale:     s.claudeSessionCache.CostsStale(),
		ScanInProgress: s.claudeSessionCache.ScanInProgress(),
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
