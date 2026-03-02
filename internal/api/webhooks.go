package api

import (
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/messaging"
)

// mountWebhookRoutes registers the generic webhook endpoint used by
// bidirectional messaging integrations (Telegram, Slack, etc.).
func (s *Server) mountWebhookRoutes(r chi.Router) {
	if s.messagingManager == nil {
		return
	}
	r.Post("/webhooks/{platform_type}/{integration_id}", s.handleWebhook)
}

// handleWebhook delegates an inbound webhook to the correct messaging platform adapter.
func (s *Server) handleWebhook(w http.ResponseWriter, r *http.Request) {
	platformType := chi.URLParam(r, "platform_type")
	integrationID := chi.URLParam(r, "integration_id")

	if platformType == "" || integrationID == "" {
		s.writeError(w, http.StatusBadRequest, "missing platform_type or integration_id")
		return
	}

	s.messagingManager.HandleWebhook(platformType, integrationID, w, r)
}

// SetMessagingManager sets the messaging manager on the API server.
// This is called during startup after the manager is constructed.
func (s *Server) SetMessagingManager(mgr *messaging.Manager) {
	s.messagingManager = mgr
}
