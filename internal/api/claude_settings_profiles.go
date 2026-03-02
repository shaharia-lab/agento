package api

import (
	"encoding/json"
	"net/http"

	"github.com/go-chi/chi/v5"
)

// handleListClaudeSettingsProfiles returns all Claude settings profiles.
func (s *Server) handleListClaudeSettingsProfiles(w http.ResponseWriter, r *http.Request) {
	profiles, err := s.profileSvc.ListProfiles()
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, profiles)
}

// handleCreateClaudeSettingsProfile creates a new Claude settings profile.
func (s *Server) handleCreateClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	var req CreateProfileRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil || req.Name == "" {
		s.writeError(w, http.StatusBadRequest, "name is required")
		return
	}

	profile, err := s.profileSvc.CreateProfile(req.Name)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusCreated, profile)
}

// handleGetClaudeSettingsProfile returns a single profile with its settings content.
func (s *Server) handleGetClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	detail, err := s.profileSvc.GetProfile(id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, detail)
}

// handleUpdateClaudeSettingsProfile updates the name and/or settings of a profile.
func (s *Server) handleUpdateClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	var req UpdateProfileRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	detail, err := s.profileSvc.UpdateProfile(id, req.Name, req.Settings)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, detail)
}

// handleDeleteClaudeSettingsProfile removes a profile (default profile cannot be deleted).
func (s *Server) handleDeleteClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := s.profileSvc.DeleteProfile(id); err != nil {
		s.httpErr(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// handleDuplicateClaudeSettingsProfile creates a copy of the given profile.
func (s *Server) handleDuplicateClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	profile, err := s.profileSvc.DuplicateProfile(id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusCreated, profile)
}

// handleSetDefaultClaudeSettingsProfile marks a profile as default and syncs settings.json.
func (s *Server) handleSetDefaultClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	profile, err := s.profileSvc.SetDefaultProfile(id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, profile)
}
