package api

import (
	"encoding/json"
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/service"
)

// handleListIntegrations returns all integrations (credentials are omitted).
func (s *Server) handleListIntegrations(w http.ResponseWriter, r *http.Request) {
	integrations, err := s.integrationSvc.List(r.Context())
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	// Scrub secrets before returning to the client.
	scrubbed := make([]map[string]any, 0, len(integrations))
	for _, cfg := range integrations {
		scrubbed = append(scrubbed, scrubIntegration(cfg))
	}
	writeJSON(w, http.StatusOK, scrubbed)
}

// handleCreateIntegration creates a new integration.
func (s *Server) handleCreateIntegration(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Name        string                          `json:"name"`
		Type        string                          `json:"type"`
		Enabled     bool                            `json:"enabled"`
		Credentials config.GoogleCredentials        `json:"credentials"`
		Services    map[string]config.ServiceConfig `json:"services"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		writeError(w, http.StatusBadRequest, "invalid request body")
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
		httpErr(w, err)
		return
	}
	writeJSON(w, http.StatusCreated, scrubIntegration(created))
}

// handleGetIntegration returns a single integration by id (credentials scrubbed).
func (s *Server) handleGetIntegration(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	cfg, err := s.integrationSvc.Get(r.Context(), id)
	if err != nil {
		httpErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, scrubIntegration(cfg))
}

// handleUpdateIntegration updates an integration and triggers an MCP server reload.
func (s *Server) handleUpdateIntegration(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	var body struct {
		Name        string                          `json:"name"`
		Type        string                          `json:"type"`
		Enabled     bool                            `json:"enabled"`
		Credentials config.GoogleCredentials        `json:"credentials"`
		Services    map[string]config.ServiceConfig `json:"services"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		writeError(w, http.StatusBadRequest, "invalid request body")
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
		httpErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, scrubIntegration(updated))
}

// handleDeleteIntegration removes an integration and stops its MCP server.
func (s *Server) handleDeleteIntegration(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := s.integrationSvc.Delete(r.Context(), id); err != nil {
		httpErr(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// handleAvailableTools returns all available tools across all connected integrations.
func (s *Server) handleAvailableTools(w http.ResponseWriter, r *http.Request) {
	tools, err := s.integrationSvc.AvailableTools(r.Context())
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, tools)
}

// handleStartOAuth begins the OAuth2 flow for an integration and returns the auth URL.
func (s *Server) handleStartOAuth(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	authURL, err := s.integrationSvc.StartOAuth(r.Context(), id)
	if err != nil {
		httpErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"auth_url": authURL})
}

// handleGetAuthStatus polls whether the OAuth flow for an integration has completed.
func (s *Server) handleGetAuthStatus(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	authenticated, err := s.integrationSvc.GetAuthStatus(r.Context(), id)
	if err != nil {
		httpErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, map[string]bool{"authenticated": authenticated})
}

// scrubIntegration returns a map representation of the integration with secrets removed.
func scrubIntegration(cfg *config.IntegrationConfig) map[string]any {
	return map[string]any{
		"id":            cfg.ID,
		"name":          cfg.Name,
		"type":          cfg.Type,
		"enabled":       cfg.Enabled,
		"authenticated": cfg.Auth != nil,
		"services":      cfg.Services,
		"created_at":    cfg.CreatedAt,
		"updated_at":    cfg.UpdatedAt,
	}
}

// httpErr maps service errors to HTTP status codes.
func httpErr(w http.ResponseWriter, err error) {
	switch e := err.(type) {
	case *service.NotFoundError:
		writeError(w, http.StatusNotFound, e.Error())
	case *service.ValidationError:
		writeError(w, http.StatusBadRequest, e.Error())
	case *service.ConflictError:
		writeError(w, http.StatusConflict, e.Error())
	default:
		writeError(w, http.StatusInternalServerError, err.Error())
	}
}
