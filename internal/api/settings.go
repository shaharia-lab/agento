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
	s.writeJSON(w, http.StatusOK, settingsResponse{
		Settings:     s.settingsMgr.Get(),
		Locked:       s.settingsMgr.Locked(),
		ModelFromEnv: s.settingsMgr.ModelFromEnv(),
	})
}

func (s *Server) handleUpdateSettings(w http.ResponseWriter, r *http.Request) {
	var incoming config.UserSettings
	if json.NewDecoder(r.Body).Decode(&incoming) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	if err := s.settingsMgr.Update(incoming); err != nil {
		s.writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	s.writeJSON(w, http.StatusOK, settingsResponse{
		Settings:     s.settingsMgr.Get(),
		Locked:       s.settingsMgr.Locked(),
		ModelFromEnv: s.settingsMgr.ModelFromEnv(),
	})
}
