package api

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"

	"github.com/shaharia-lab/agento/internal/config"
)

type claudeSettingsResponse struct {
	Exists   bool            `json:"exists"`
	Settings json.RawMessage `json:"settings,omitempty"`
}

func (s *Server) handleGetClaudeSettings(w http.ResponseWriter, _ *http.Request) {
	path, err := config.ClaudeSettingsJSONPath()
	if err != nil {
		s.writeError(w, http.StatusInternalServerError, "failed to resolve home directory")
		return
	}

	data, err := os.ReadFile(path) //nolint:gosec // path constructed from user home directory
	if err != nil {
		if os.IsNotExist(err) {
			s.writeJSON(w, http.StatusOK, claudeSettingsResponse{Exists: false})
			return
		}
		s.writeError(w, http.StatusInternalServerError, "failed to read Claude settings file")
		return
	}

	// Validate the file contains valid JSON before returning.
	if !json.Valid(data) {
		s.writeError(w, http.StatusInternalServerError, "Claude settings file contains invalid JSON")
		return
	}

	s.writeJSON(w, http.StatusOK, claudeSettingsResponse{
		Exists:   true,
		Settings: json.RawMessage(data),
	})
}

func (s *Server) handleUpdateClaudeSettings(w http.ResponseWriter, r *http.Request) {
	var incoming json.RawMessage
	if json.NewDecoder(r.Body).Decode(&incoming) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	path, err := config.ClaudeSettingsJSONPath()
	if err != nil {
		s.writeError(w, http.StatusInternalServerError, "failed to resolve home directory")
		return
	}

	// Ensure the .claude directory exists.
	if os.MkdirAll(filepath.Dir(path), 0700) != nil {
		s.writeError(w, http.StatusInternalServerError, "failed to create .claude directory")
		return
	}

	// Pretty-print before writing so the file remains human-readable.
	var pretty any
	if json.Unmarshal(incoming, &pretty) != nil {
		s.writeError(w, http.StatusBadRequest, "invalid JSON settings")
		return
	}
	out, err := json.MarshalIndent(pretty, "", "  ")
	if err != nil {
		s.writeError(w, http.StatusInternalServerError, "failed to marshal settings")
		return
	}

	if os.WriteFile(path, out, 0600) != nil { //nolint:gosec // path constructed from user home directory
		s.writeError(w, http.StatusInternalServerError, "failed to write Claude settings file")
		return
	}

	s.writeJSON(w, http.StatusOK, claudeSettingsResponse{
		Exists:   true,
		Settings: json.RawMessage(out),
	})
}
