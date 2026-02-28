package api

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
)

type claudeSettingsResponse struct {
	Exists   bool            `json:"exists"`
	Settings json.RawMessage `json:"settings,omitempty"`
}

func claudeSettingsPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".claude", "settings.json"), nil
}

func (s *Server) handleGetClaudeSettings(w http.ResponseWriter, _ *http.Request) {
	path, err := claudeSettingsPath()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to resolve home directory")
		return
	}

	data, err := os.ReadFile(path) //nolint:gosec // path constructed from user home directory
	if err != nil {
		if os.IsNotExist(err) {
			writeJSON(w, http.StatusOK, claudeSettingsResponse{Exists: false})
			return
		}
		writeError(w, http.StatusInternalServerError, "failed to read Claude settings file")
		return
	}

	// Validate the file contains valid JSON before returning.
	if !json.Valid(data) {
		writeError(w, http.StatusInternalServerError, "Claude settings file contains invalid JSON")
		return
	}

	writeJSON(w, http.StatusOK, claudeSettingsResponse{
		Exists:   true,
		Settings: json.RawMessage(data),
	})
}

func (s *Server) handleUpdateClaudeSettings(w http.ResponseWriter, r *http.Request) {
	var incoming json.RawMessage
	if err := json.NewDecoder(r.Body).Decode(&incoming); err != nil {
		writeError(w, http.StatusBadRequest, "invalid JSON body")
		return
	}

	path, err := claudeSettingsPath()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to resolve home directory")
		return
	}

	// Ensure the .claude directory exists.
	if mkdirErr := os.MkdirAll(filepath.Dir(path), 0700); mkdirErr != nil {
		writeError(w, http.StatusInternalServerError, "failed to create .claude directory")
		return
	}

	// Pretty-print before writing so the file remains human-readable.
	var pretty any
	if unmarshalErr := json.Unmarshal(incoming, &pretty); unmarshalErr != nil {
		writeError(w, http.StatusBadRequest, "invalid JSON settings")
		return
	}
	out, err := json.MarshalIndent(pretty, "", "  ")
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to marshal settings")
		return
	}

	if err := os.WriteFile(path, out, 0600); err != nil { //nolint:gosec // path constructed from user home directory
		writeError(w, http.StatusInternalServerError, "failed to write Claude settings file")
		return
	}

	writeJSON(w, http.StatusOK, claudeSettingsResponse{
		Exists:   true,
		Settings: json.RawMessage(out),
	})
}
