package api

import (
	"encoding/json"
	"fmt"
	"net/http"
	"regexp"
	"strings"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agents-platform-cc-go/internal/config"
)

var slugRE = regexp.MustCompile(`^[a-z0-9]+(?:-[a-z0-9]+)*$`)

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
	agents, err := s.agents.List()
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
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
	if req.Name == "" {
		writeError(w, http.StatusBadRequest, "name is required")
		return
	}
	if req.Slug == "" {
		req.Slug = toSlug(req.Name)
	}
	if !slugRE.MatchString(req.Slug) {
		writeError(w, http.StatusBadRequest, fmt.Sprintf("invalid slug %q: use lowercase letters, digits and hyphens", req.Slug))
		return
	}

	// Uniqueness check
	existing, err := s.agents.Get(req.Slug)
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	if existing != nil {
		writeError(w, http.StatusConflict, fmt.Sprintf("agent with slug %q already exists", req.Slug))
		return
	}

	if req.Model == "" {
		req.Model = "claude-sonnet-4-6"
	}
	if req.Thinking == "" {
		req.Thinking = "adaptive"
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
	if err := s.agents.Save(agent); err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusCreated, agent)
}

func (s *Server) handleGetAgent(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	agent, err := s.agents.Get(slug)
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	if agent == nil {
		writeError(w, http.StatusNotFound, fmt.Sprintf("agent %q not found", slug))
		return
	}
	writeJSON(w, http.StatusOK, agent)
}

func (s *Server) handleUpdateAgent(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")

	existing, err := s.agents.Get(slug)
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	if existing == nil {
		writeError(w, http.StatusNotFound, fmt.Sprintf("agent %q not found", slug))
		return
	}

	var req agentRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "invalid JSON body")
		return
	}
	if req.Name == "" {
		writeError(w, http.StatusBadRequest, "name is required")
		return
	}

	agent := &config.AgentConfig{
		Name:         req.Name,
		Slug:         slug, // slug is the filename, never changed via PUT
		Description:  req.Description,
		Model:        req.Model,
		Thinking:     req.Thinking,
		SystemPrompt: req.SystemPrompt,
		Capabilities: req.Capabilities,
	}
	if agent.Model == "" {
		agent.Model = "claude-sonnet-4-6"
	}
	if agent.Thinking == "" {
		agent.Thinking = "adaptive"
	}

	if err := s.agents.Save(agent); err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, agent)
}

func (s *Server) handleDeleteAgent(w http.ResponseWriter, r *http.Request) {
	slug := chi.URLParam(r, "slug")
	if err := s.agents.Delete(slug); err != nil {
		writeError(w, http.StatusNotFound, err.Error())
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// toSlug converts a human-readable name into a URL-safe slug.
func toSlug(name string) string {
	lower := strings.ToLower(name)
	var result []byte
	prevHyphen := false
	for i := 0; i < len(lower); i++ {
		c := lower[i]
		if (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') {
			result = append(result, c)
			prevHyphen = false
		} else if !prevHyphen && len(result) > 0 {
			result = append(result, '-')
			prevHyphen = true
		}
	}
	// trim trailing hyphen
	for len(result) > 0 && result[len(result)-1] == '-' {
		result = result[:len(result)-1]
	}
	return string(result)
}
