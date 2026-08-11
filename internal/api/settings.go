package api

import (
	"encoding/json"
	"net/http"

	"github.com/shaharia-lab/agento/internal/claudesessions"
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

// applyDataSettings installs the saved Data & Analytics preferences into the
// snapshot every reader consults, and starts the rescan a changed idle-gap
// threshold needs.
//
// Hiding a project takes effect on the next read — it is a filter over cached
// rows. Changing the threshold does not: active duration is stored per
// transcript, so the figures behind it must be recomputed from the events. The
// scan is asked for here so the correction starts on save rather than when
// something else happens to trigger a scan; it is idempotent, since the scanner
// compares the stored threshold itself and Cache.EnsureScan admits exactly one
// scan at a time.
func (s *Server) applyDataSettings(previousIdleGap int) {
	current := s.settingsMgr.Get()
	claudesessions.ApplyDataSettings(current.IdleGapThresholdMinutes, current.HiddenProjects)

	if s.claudeSessionCache == nil || current.IdleGapThresholdMinutes == previousIdleGap {
		return
	}
	s.logger.Info("claude sessions: idle-gap threshold changed; recomputing durations",
		"from_minutes", previousIdleGap, "to_minutes", current.IdleGapThresholdMinutes)
	s.claudeSessionCache.EnsureScan()
}

func (s *Server) handleUpdateSettings(w http.ResponseWriter, r *http.Request) {
	var incoming config.UserSettings
	if json.NewDecoder(r.Body).Decode(&incoming) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	previousIdleGap := s.settingsMgr.Get().IdleGapThresholdMinutes

	if err := s.settingsMgr.Update(incoming); err != nil {
		s.writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	s.applyDataSettings(previousIdleGap)

	s.writeJSON(w, http.StatusOK, settingsResponse{
		Settings:     s.settingsMgr.Get(),
		Locked:       s.settingsMgr.Locked(),
		ModelFromEnv: s.settingsMgr.ModelFromEnv(),
	})
}
