package api

import (
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/claudesessions"
)

// handleGetClaudeSessionJourney returns a structured turn-by-turn journey
// visualization for a single Claude Code session.
func (s *Server) handleGetClaudeSessionJourney(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	journey, err := claudesessions.GetSessionJourney(id, s.logger)
	if err != nil {
		s.logger.Error("get claude session journey failed", "session_id", id, "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to get session journey")
		return
	}
	if journey == nil {
		s.writeError(w, http.StatusNotFound, "session not found")
		return
	}
	s.writeJSON(w, http.StatusOK, journey)
}
