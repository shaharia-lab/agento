package api

import (
	"encoding/json"
	"net/http"

	"github.com/shaharia-lab/agento/internal/config"
)

type settingsResponse struct {
	Settings     config.UserSettings `json:"settings"`
	Locked       map[string]string   `json:"locked"`
	ModelFromEnv bool                `json:"model_from_env"`
}

func (s *Server) handleGetSettings(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, settingsResponse{
		Settings:     s.settingsMgr.Get(),
		Locked:       s.settingsMgr.Locked(),
		ModelFromEnv: s.settingsMgr.ModelFromEnv(),
	})
}

func (s *Server) handleUpdateSettings(w http.ResponseWriter, r *http.Request) {
	var incoming config.UserSettings
	if json.NewDecoder(r.Body).Decode(&incoming) != nil {
		writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	if err := s.settingsMgr.Update(incoming); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	writeJSON(w, http.StatusOK, settingsResponse{
		Settings:     s.settingsMgr.Get(),
		Locked:       s.settingsMgr.Locked(),
		ModelFromEnv: s.settingsMgr.ModelFromEnv(),
	})
}
