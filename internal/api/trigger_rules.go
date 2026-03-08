package api

import (
	"encoding/json"
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/config"
)

// handleListTriggerRules returns all trigger rules for an integration.
func (s *Server) handleListTriggerRules(w http.ResponseWriter, r *http.Request) {
	integrationID := chi.URLParam(r, "id")
	rules, err := s.triggerSvc.ListRules(r.Context(), integrationID)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, rules)
}

// handleCreateTriggerRule creates a new trigger rule for an integration.
func (s *Server) handleCreateTriggerRule(w http.ResponseWriter, r *http.Request) {
	integrationID := chi.URLParam(r, "id")

	var req CreateTriggerRuleRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	rule := &config.TriggerRule{
		IntegrationID:  integrationID,
		Name:           req.Name,
		AgentSlug:      req.AgentSlug,
		Enabled:        req.Enabled,
		FilterPrefix:   req.FilterPrefix,
		FilterKeywords: req.FilterKeywords,
		FilterChatIDs:  req.FilterChatIDs,
	}

	created, err := s.triggerSvc.CreateRule(r.Context(), rule)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusCreated, created)
}

// handleUpdateTriggerRule updates an existing trigger rule.
func (s *Server) handleUpdateTriggerRule(w http.ResponseWriter, r *http.Request) {
	integrationID := chi.URLParam(r, "id")
	ruleID := chi.URLParam(r, "rid")

	// Verify the rule belongs to this integration.
	existing, err := s.triggerSvc.GetRule(r.Context(), ruleID)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	if existing.IntegrationID != integrationID {
		s.writeError(w, http.StatusForbidden, "rule does not belong to this integration")
		return
	}

	var req UpdateTriggerRuleRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	rule := &config.TriggerRule{
		Name:           req.Name,
		AgentSlug:      req.AgentSlug,
		Enabled:        req.Enabled,
		FilterPrefix:   req.FilterPrefix,
		FilterKeywords: req.FilterKeywords,
		FilterChatIDs:  req.FilterChatIDs,
	}

	updated, err := s.triggerSvc.UpdateRule(r.Context(), ruleID, rule)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, updated)
}

// handleDeleteTriggerRule removes a trigger rule.
func (s *Server) handleDeleteTriggerRule(w http.ResponseWriter, r *http.Request) {
	integrationID := chi.URLParam(r, "id")
	ruleID := chi.URLParam(r, "rid")

	// Verify the rule belongs to this integration.
	existing, err := s.triggerSvc.GetRule(r.Context(), ruleID)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	if existing.IntegrationID != integrationID {
		s.writeError(w, http.StatusForbidden, "rule does not belong to this integration")
		return
	}

	if err := s.triggerSvc.DeleteRule(r.Context(), ruleID); err != nil {
		s.httpErr(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// handleRegisterWebhook registers a Telegram webhook for the integration.
func (s *Server) handleRegisterWebhook(w http.ResponseWriter, r *http.Request) {
	integrationID := chi.URLParam(r, "id")
	if err := s.triggerSvc.RegisterWebhook(r.Context(), integrationID); err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, map[string]string{"status": "registered"})
}

// handleDeleteWebhook removes the Telegram webhook for the integration.
func (s *Server) handleDeleteWebhook(w http.ResponseWriter, r *http.Request) {
	integrationID := chi.URLParam(r, "id")
	if err := s.triggerSvc.DeleteWebhook(r.Context(), integrationID); err != nil {
		s.httpErr(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// handleGetWebhookStatus returns the webhook status for an integration.
func (s *Server) handleGetWebhookStatus(w http.ResponseWriter, r *http.Request) {
	integrationID := chi.URLParam(r, "id")
	status, err := s.triggerSvc.GetWebhookStatus(r.Context(), integrationID)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, status)
}

// handleRegenerateWebhookSecret regenerates the webhook secret and re-registers.
func (s *Server) handleRegenerateWebhookSecret(w http.ResponseWriter, r *http.Request) {
	integrationID := chi.URLParam(r, "id")
	if err := s.triggerSvc.RegenerateSecret(r.Context(), integrationID); err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, map[string]string{"status": "regenerated"})
}
