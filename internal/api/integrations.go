package api

import (
	"encoding/json"
	"errors"
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/service"
)

// handleListIntegrations returns all integrations (credentials are omitted).
func (s *Server) handleListIntegrations(w http.ResponseWriter, r *http.Request) {
	integrations, err := s.integrationSvc.List(r.Context())
	if err != nil {
		s.logger.Error("list integrations failed", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to list integrations")
		return
	}
	// Scrub secrets before returning to the client.
	scrubbed := make([]map[string]any, 0, len(integrations))
	for _, cfg := range integrations {
		scrubbed = append(scrubbed, scrubIntegration(cfg))
	}
	s.writeJSON(w, http.StatusOK, scrubbed)
}

// handleCreateIntegration creates a new integration.
func (s *Server) handleCreateIntegration(w http.ResponseWriter, r *http.Request) {
	var body CreateIntegrationRequest
	if json.NewDecoder(r.Body).Decode(&body) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	cfg := &config.IntegrationConfig{
		Name:        body.Name,
		Type:        body.Type,
		Enabled:     body.Enabled,
		Credentials: body.Credentials,
		Services:    body.Services,
	}

	created, err := s.integrationSvc.Create(r.Context(), cfg)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusCreated, scrubIntegration(created))
}

// handleGetIntegration returns a single integration by id (credentials scrubbed).
func (s *Server) handleGetIntegration(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	cfg, err := s.integrationSvc.Get(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, scrubIntegration(cfg))
}

// handleUpdateIntegration updates an integration and triggers an MCP server reload.
func (s *Server) handleUpdateIntegration(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	var body UpdateIntegrationRequest
	if json.NewDecoder(r.Body).Decode(&body) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	cfg := &config.IntegrationConfig{
		Name:        body.Name,
		Type:        body.Type,
		Enabled:     body.Enabled,
		Credentials: body.Credentials,
		Services:    body.Services,
	}

	updated, err := s.integrationSvc.Update(r.Context(), id, cfg)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, scrubIntegration(updated))
}

// handleDeleteIntegration removes an integration and stops its MCP server.
func (s *Server) handleDeleteIntegration(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := s.integrationSvc.Delete(r.Context(), id); err != nil {
		s.httpErr(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// handleAvailableTools returns all available tools across all connected integrations.
func (s *Server) handleAvailableTools(w http.ResponseWriter, r *http.Request) {
	tools, err := s.integrationSvc.AvailableTools(r.Context())
	if err != nil {
		s.logger.Error("list available tools failed", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to list available tools")
		return
	}
	s.writeJSON(w, http.StatusOK, tools)
}

// handleStartOAuth begins the OAuth2 flow for an integration and returns the auth URL.
func (s *Server) handleStartOAuth(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	authURL, err := s.integrationSvc.StartOAuth(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, map[string]string{"auth_url": authURL})
}

// handleGetAuthStatus polls whether the OAuth flow for an integration has completed.
func (s *Server) handleGetAuthStatus(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	authenticated, err := s.integrationSvc.GetAuthStatus(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, map[string]bool{"authenticated": authenticated})
}

// handleValidateAuth validates token-based auth for an integration.
func (s *Server) handleValidateAuth(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	cfg, err := s.integrationSvc.Get(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}

	if valErr := s.integrationSvc.ValidateTokenAuth(r.Context(), cfg); valErr != nil {
		s.writeJSON(w, http.StatusBadRequest, map[string]any{"valid": false, "validated": true, "error": valErr.Error()})
		return
	}

	// For types with real validation (e.g. telegram, confluence, jira), the credentials have been verified.
	// For types without validation, this is a no-op success.
	validated := cfg.Type == "telegram" || cfg.Type == "confluence" || cfg.Type == "jira"
	s.writeJSON(w, http.StatusOK, map[string]any{"valid": true, "validated": validated})
}

// scrubIntegration returns a map representation of the integration with secrets removed.
func scrubIntegration(cfg *config.IntegrationConfig) map[string]any {
	return map[string]any{
		"id":            cfg.ID,
		"name":          cfg.Name,
		"type":          cfg.Type,
		"enabled":       cfg.Enabled,
		"authenticated": cfg.IsAuthenticated(),
		"services":      cfg.Services,
		"created_at":    cfg.CreatedAt,
		"updated_at":    cfg.UpdatedAt,
	}
}

// httpErr maps service errors to HTTP status codes using errors.As to handle wrapped errors.
// The default case logs the internal error and returns a generic message to avoid
// leaking internal details (database errors, filesystem paths) to the client.
func (s *Server) httpErr(w http.ResponseWriter, err error) {
	var nfe *service.NotFoundError
	var ve *service.ValidationError
	var ce *service.ConflictError

	switch {
	case errors.As(err, &nfe):
		s.writeError(w, http.StatusNotFound, nfe.Error())
	case errors.As(err, &ve):
		s.writeError(w, http.StatusUnprocessableEntity, ve.Error())
	case errors.As(err, &ce):
		s.writeError(w, http.StatusConflict, ce.Error())
	default:
		s.logger.Error("internal server error", "error", err)
		s.writeError(w, http.StatusInternalServerError, "internal server error")
	}
}
