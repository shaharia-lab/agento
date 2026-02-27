package api

import (
	"encoding/json"
	"errors"
	"net/http"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/service"
)

type agentRequest struct {
	Name         string                   `json:"name"`
	Slug         string                   `json:"slug"`
	Description  string                   `json:"description"`
	Model        string                   `json:"model"`
	Thinking     string                   `json:"thinking"`
	SystemPrompt string                   `json:"system_prompt"`
	Capabilities config.AgentCapabilities `json:"capabilities"`
}

func (s *Server) handleListAgents(w http.ResponseWriter, r *http.Request) {
	agents, err := s.agentSvc.List(r.Context())
	if err != nil {
		s.logger.Error("list agents failed", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to list agents")
		return
	}
	writeJSON(w, http.StatusOK, agents)
}

func (s *Server) handleCreateAgent(w http.ResponseWriter, r *http.Request) {
	var req agentRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "invalid JSON body")
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
		var ve *service.ValidationError
		var ce *service.ConflictError
		switch {
		case errors.As(err, &ve):
			writeError(w, http.StatusBadRequest, ve.Error())
		case errors.As(err, &ce):
			writeError(w, http.StatusConflict, ce.Error())
		default:
			s.logger.Error("create agent failed", "error", err)
			writeError(w, http.StatusInternalServerError, "failed to create agent")
		}
		return
	}
	writeJSON(w, http.StatusCreated, created)
}

func (s *Server) handleGetAgent(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	agent, err := s.agentSvc.Get(r.Context(), slug)
	if err != nil {
		s.logger.Error("get agent failed", "slug", slug, "error", err)
		writeError(w, http.StatusInternalServerError, "failed to get agent")
		return
	}
	if agent == nil {
		writeError(w, http.StatusNotFound, "agent not found")
		return
	}
	writeJSON(w, http.StatusOK, agent)
}

func (s *Server) handleUpdateAgent(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")

	var req agentRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "invalid JSON body")
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
		var ve *service.ValidationError
		var nfe *service.NotFoundError
		switch {
		case errors.As(err, &nfe):
			writeError(w, http.StatusNotFound, nfe.Error())
		case errors.As(err, &ve):
			writeError(w, http.StatusBadRequest, ve.Error())
		default:
			s.logger.Error("update agent failed", "slug", slug, "error", err)
			writeError(w, http.StatusInternalServerError, "failed to update agent")
		}
		return
	}
	writeJSON(w, http.StatusOK, updated)
}

func (s *Server) handleDeleteAgent(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	if err := s.agentSvc.Delete(r.Context(), slug); err != nil {
		var nfe *service.NotFoundError
		if errors.As(err, &nfe) {
			writeError(w, http.StatusNotFound, nfe.Error())
			return
		}
		s.logger.Error("delete agent failed", "slug", slug, "error", err)
		writeError(w, http.StatusInternalServerError, "failed to delete agent")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}
