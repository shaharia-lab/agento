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
func (s *Server) applyDataSettings(previousIdleGap int, previousDirs []string) {
	current := s.settingsMgr.Get()
	claudesessions.ApplyDataSettings(current.IdleGapThresholdMinutes, current.HiddenProjects)
	config.ApplyClaudeDirs(current.ClaudeConfigDir, current.ClaudeConfigDirs)

	if s.claudeSessionCache == nil {
		return
	}
	if current.IdleGapThresholdMinutes != previousIdleGap {
		s.logger.Info("claude sessions: idle-gap threshold changed; recomputing durations",
			"from_minutes", previousIdleGap, "to_minutes", current.IdleGapThresholdMinutes)
		s.claudeSessionCache.EnsureScan()
		return
	}
	// A newly added config dir has never been walked, so unlike hiding a
	// project this cannot wait for the next read: there are no cached rows to
	// filter. Removing one needs no scan — its rows are filtered out, not
	// deleted — but the comparison is on the resolved set either way, so an
	// unchanged save costs nothing.
	if dirsDiffer(previousDirs, claudesessions.ClaudeHomes()) {
		s.logger.Info("claude sessions: config dirs changed; indexing",
			"from", previousDirs, "to", claudesessions.ClaudeHomes())
		s.claudeSessionCache.EnsureScan()
	}
}

// dirsDiffer reports whether two resolved config-dir sets differ. Order is
// significant: it decides which dir wins a duplicated session.
func dirsDiffer(a, b []string) bool {
	if len(a) != len(b) {
		return true
	}
	for i := range a {
		if a[i] != b[i] {
			return true
		}
	}
	return false
}

func (s *Server) handleUpdateSettings(w http.ResponseWriter, r *http.Request) {
	var incoming config.UserSettings
	if json.NewDecoder(r.Body).Decode(&incoming) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	previousIdleGap := s.settingsMgr.Get().IdleGapThresholdMinutes
	previousDirs := claudesessions.ClaudeHomes()

	if err := s.settingsMgr.Update(incoming); err != nil {
		s.writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	s.applyDataSettings(previousIdleGap, previousDirs)

	s.writeJSON(w, http.StatusOK, settingsResponse{
		Settings:     s.settingsMgr.Get(),
		Locked:       s.settingsMgr.Locked(),
		ModelFromEnv: s.settingsMgr.ModelFromEnv(),
	})
}

// claudeConfigDirsResponse reports which Claude config dirs are indexed and
// which unconfigured ones exist beside the default.
type claudeConfigDirsResponse struct {
	// Indexed is the resolved set the scanner walks, default first.
	Indexed []string `json:"indexed"`
	// Candidates are dirs that look like Claude config dirs but are not
	// configured yet. Suggested, never enabled on the user's behalf.
	Candidates []string `json:"candidates"`
	// Default is the dir Claude Code uses out of the box, always indexed.
	Default string `json:"default"`
}

// handleClaudeConfigDirs powers the config-dir editor.
//
// Candidates exist so the union is not purely manual: someone running a second
// account almost always put it beside the first, and typing an absolute path
// to discover that is a poor trade for one directory listing.
func (s *Server) handleClaudeConfigDirs(w http.ResponseWriter, _ *http.Request) {
	s.writeJSON(w, http.StatusOK, claudeConfigDirsResponse{
		Indexed:    config.ClaudeConfigDirs(),
		Candidates: config.DiscoverCandidateClaudeDirs(),
		Default:    config.DefaultClaudeConfigDir(),
	})
}
