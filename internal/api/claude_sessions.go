package api

import (
	"net/http"
	"strings"

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
	writeJSON(w, http.StatusOK, sessions)
}

// handleListClaudeProjects returns all distinct project directories containing sessions.
func (s *Server) handleListClaudeProjects(w http.ResponseWriter, r *http.Request) {
	projects, err := claudesessions.ListProjects()
	if err != nil {
		s.logger.Error("list claude projects failed", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to list projects")
		return
	}
	if projects == nil {
		projects = []claudesessions.ClaudeProject{}
	}
	writeJSON(w, http.StatusOK, projects)
}

// handleGetClaudeSession returns the full detail of a single Claude Code session
// including all messages, token usage, and todos.
func (s *Server) handleGetClaudeSession(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	detail, err := claudesessions.GetSessionDetail(id)
	if err != nil {
		s.logger.Error("get claude session failed", "session_id", id, "error", err)
		writeError(w, http.StatusInternalServerError, "failed to get session")
		return
	}
	if detail == nil {
		writeError(w, http.StatusNotFound, "session not found")
		return
	}
	writeJSON(w, http.StatusOK, detail)
}

// handleRefreshClaudeSessionCache invalidates the in-memory session cache.
// The next call to List() will trigger a fresh scan.
func (s *Server) handleRefreshClaudeSessionCache(w http.ResponseWriter, _ *http.Request) {
	s.claudeSessionCache.Invalidate()
	// Trigger rescan in background so the next list request gets fresh data.
	go func() { s.claudeSessionCache.List() }()
	w.WriteHeader(http.StatusAccepted)
}

// handleContinueClaudeSession creates a new Agento chat session that inherits the
// given Claude Code session ID so the SDK can resume the existing conversation.
func (s *Server) handleContinueClaudeSession(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	// Look up the session to get its working directory and model.
	detail, err := claudesessions.GetSessionDetail(id)
	if err != nil {
		s.logger.Error("continue claude session: lookup failed", "session_id", id, "error", err)
		writeError(w, http.StatusInternalServerError, "failed to look up session")
		return
	}
	if detail == nil {
		writeError(w, http.StatusNotFound, "session not found")
		return
	}

	// Create a new Agento chat session with no agent slug, inheriting the session's cwd.
	chatSession, err := s.chatSvc.CreateSession(r.Context(), "", detail.CWD, detail.Model, "")
	if err != nil {
		s.logger.Error("continue claude session: create chat failed", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to create chat session")
		return
	}

	// Link the new Agento chat to the original Claude Code session so the SDK
	// picks up the conversation history when the first message is sent.
	chatSession.SDKSession = id
	if err := s.chatSvc.UpdateSession(r.Context(), chatSession); err != nil {
		s.logger.Error("continue claude session: update session failed", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to link session")
		return
	}

	writeJSON(w, http.StatusCreated, map[string]string{
		"chat_id": chatSession.ID,
	})
}
