package api

import (
	"encoding/json"
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/config"
)

func (s *Server) handleListAgents(w http.ResponseWriter, r *http.Request) {
	agents, err := s.agentSvc.List(r.Context())
	if err != nil {
		s.logger.Error("list agents failed", "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to list agents")
		return
	}
	s.writeJSON(w, http.StatusOK, agents)
}

func (s *Server) handleCreateAgent(w http.ResponseWriter, r *http.Request) {
	var req AgentRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	agent := &config.AgentConfig{
		Name:         req.Name,
		Slug:         req.Slug,
		Description:  req.Description,
		Model:        req.Model,
		Thinking:     req.Thinking,
		SystemPrompt: req.SystemPrompt,
		Capabilities: req.Capabilities,
	}

	created, err := s.agentSvc.Create(r.Context(), agent)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusCreated, created)
}

func (s *Server) handleGetAgent(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	agent, err := s.agentSvc.Get(r.Context(), slug)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	if agent == nil {
		s.writeError(w, http.StatusNotFound, "agent not found")
		return
	}
	s.writeJSON(w, http.StatusOK, agent)
}

func (s *Server) handleUpdateAgent(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")

	var req AgentRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	agent := &config.AgentConfig{
		Name:         req.Name,
		Slug:         slug,
		Description:  req.Description,
		Model:        req.Model,
		Thinking:     req.Thinking,
		SystemPrompt: req.SystemPrompt,
		Capabilities: req.Capabilities,
	}

	updated, err := s.agentSvc.Update(r.Context(), slug, agent)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, updated)
}

func (s *Server) handleDeleteAgent(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	if err := s.agentSvc.Delete(r.Context(), slug); err != nil {
		s.httpErr(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}
