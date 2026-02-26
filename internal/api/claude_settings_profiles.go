package api

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"unicode"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/config"
)

// ClaudeSettingsProfileDetail extends ClaudeSettingsProfile with the settings content.
type ClaudeSettingsProfileDetail struct {
	config.ClaudeSettingsProfile
	Settings json.RawMessage `json:"settings"` // raw settings JSON, null if file missing
	Exists   bool            `json:"exists"`
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

// slugify converts a profile name into a safe filename slug:
// lowercase, spaces/dashes preserved as hyphens, non-alphanumeric stripped.
func slugify(name string) string {
	s := strings.ToLower(name)
	s = strings.Map(func(r rune) rune {
		if r == ' ' || r == '-' {
			return '-'
		}
		if unicode.IsLetter(r) || unicode.IsDigit(r) {
			return r
		}
		return -1
	}, s)
	re := regexp.MustCompile(`-+`)
	s = re.ReplaceAllString(s, "-")
	s = strings.Trim(s, "-")
	if s == "" {
		s = "profile"
	}
	return s
}

func saveProfilesMetadata(m config.ProfilesMetadata) error {
	path, err := config.ClaudeSettingsProfilesPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return err
	}
	data, err := json.MarshalIndent(m, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0600) //nolint:gosec // path from user home
}

// syncDefaultToSettingsJSON copies the default profile's content to ~/.claude/settings.json.
func syncDefaultToSettingsJSON(profile config.ClaudeSettingsProfile) error {
	data, err := os.ReadFile(profile.FilePath) //nolint:gosec // known-safe path from metadata
	if err != nil {
		if os.IsNotExist(err) {
			data = []byte("{}")
		} else {
			return err
		}
	}
	settingsPath, err := claudeSettingsPath()
	if err != nil {
		return err
	}
	return os.WriteFile(settingsPath, data, 0600) //nolint:gosec // path from user home
}

// resolveProfileFilePath returns the absolute path for a profile file based on its ID.
func resolveProfileFilePath(id string) (string, error) {
	dir, err := config.ClaudeSettingsDirPath()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "settings_"+id+".json"), nil
}

// ensureDefaultProfileExists creates a "Default" profile from the existing
// ~/.claude/settings.json (or empty {}) if no profiles have been configured yet.
func ensureDefaultProfileExists() error {
	m, err := config.LoadProfilesMetadata()
	if err != nil {
		return err
	}
	if len(m.Profiles) > 0 {
		return nil
	}

	dir, err := config.ClaudeSettingsDirPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}

	// Read existing settings.json (or use empty {}).
	settingsPath, err := claudeSettingsPath()
	if err != nil {
		return err
	}
	var content []byte
	if data, readErr := os.ReadFile(settingsPath); readErr == nil { //nolint:gosec
		content = data
	} else {
		content = []byte("{}")
	}

	// Pretty-print before writing the profile file.
	var pretty any
	if jsonErr := json.Unmarshal(content, &pretty); jsonErr != nil {
		pretty = map[string]any{}
	}
	out, _ := json.MarshalIndent(pretty, "", "  ")

	profileFilePath := filepath.Join(dir, "settings_default.json")
	if writeErr := os.WriteFile(profileFilePath, out, 0600); writeErr != nil { //nolint:gosec
		return writeErr
	}

	defaultProfile := config.ClaudeSettingsProfile{
		ID:        "default",
		Name:      "Default",
		FilePath:  profileFilePath,
		IsDefault: true,
	}
	m.Profiles = []config.ClaudeSettingsProfile{defaultProfile}
	return saveProfilesMetadata(m)
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

func (s *Server) handleListClaudeSettingsProfiles(w http.ResponseWriter, _ *http.Request) {
	if err := ensureDefaultProfileExists(); err != nil {
		s.logger.Error("ensure default profile failed", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to initialize profiles")
		return
	}
	m, err := config.LoadProfilesMetadata()
	if err != nil {
		s.logger.Error("load profiles metadata failed", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to load profiles")
		return
	}
	profiles := m.Profiles
	if profiles == nil {
		profiles = []config.ClaudeSettingsProfile{}
	}
	writeJSON(w, http.StatusOK, profiles)
}

func (s *Server) handleCreateClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	var req struct {
		Name string `json:"name"`
	}
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil || req.Name == "" {
		writeError(w, http.StatusBadRequest, "name is required")
		return
	}

	if err := ensureDefaultProfileExists(); err != nil {
		writeError(w, http.StatusInternalServerError, "failed to initialize profiles")
		return
	}

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to load profiles")
		return
	}

	id := slugify(req.Name)
	base := id
	for i := 2; ; i++ {
		conflict := false
		for _, p := range m.Profiles {
			if p.ID == id {
				conflict = true
				break
			}
		}
		if !conflict {
			break
		}
		id = base + "-" + string(rune('0'+i))
	}

	// Copy content from the current default profile.
	var content []byte
	for _, p := range m.Profiles {
		if p.IsDefault {
			if data, readErr := os.ReadFile(p.FilePath); readErr == nil { //nolint:gosec
				content = data
			}
			break
		}
	}
	if content == nil {
		content = []byte("{}")
	}

	filePath, err := resolveProfileFilePath(id)
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to resolve profile path")
		return
	}
	if err := os.WriteFile(filePath, content, 0600); err != nil { //nolint:gosec
		writeError(w, http.StatusInternalServerError, "failed to write profile file")
		return
	}

	newProfile := config.ClaudeSettingsProfile{
		ID:        id,
		Name:      req.Name,
		FilePath:  filePath,
		IsDefault: false,
	}
	m.Profiles = append(m.Profiles, newProfile)
	if err := saveProfilesMetadata(m); err != nil {
		writeError(w, http.StatusInternalServerError, "failed to save profiles metadata")
		return
	}

	writeJSON(w, http.StatusCreated, newProfile)
}

func (s *Server) handleGetClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to load profiles")
		return
	}

	var found *config.ClaudeSettingsProfile
	for i := range m.Profiles {
		if m.Profiles[i].ID == id {
			found = &m.Profiles[i]
			break
		}
	}
	if found == nil {
		writeError(w, http.StatusNotFound, "profile not found")
		return
	}

	detail := ClaudeSettingsProfileDetail{
		ClaudeSettingsProfile: *found,
	}
	if data, readErr := os.ReadFile(found.FilePath); readErr == nil { //nolint:gosec
		if json.Valid(data) {
			detail.Settings = json.RawMessage(data)
			detail.Exists = true
		}
	}

	writeJSON(w, http.StatusOK, detail)
}

func (s *Server) handleUpdateClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	var req struct {
		Name     *string         `json:"name"`
		Settings json.RawMessage `json:"settings"`
	}
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "invalid JSON body")
		return
	}

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to load profiles")
		return
	}

	idx := -1
	for i := range m.Profiles {
		if m.Profiles[i].ID == id {
			idx = i
			break
		}
	}
	if idx == -1 {
		writeError(w, http.StatusNotFound, "profile not found")
		return
	}

	profile := &m.Profiles[idx]

	// Update name (and rename file if needed).
	if req.Name != nil && *req.Name != "" && *req.Name != profile.Name {
		newID := slugify(*req.Name)
		if newID != id {
			for i, p := range m.Profiles {
				if i != idx && p.ID == newID {
					writeError(w, http.StatusConflict, "a profile with that name already exists")
					return
				}
			}
			newFilePath, err := resolveProfileFilePath(newID)
			if err != nil {
				writeError(w, http.StatusInternalServerError, "failed to resolve new profile path")
				return
			}
			if data, readErr := os.ReadFile(profile.FilePath); readErr == nil { //nolint:gosec
				if writeErr := os.WriteFile(newFilePath, data, 0600); writeErr != nil { //nolint:gosec
					writeError(w, http.StatusInternalServerError, "failed to rename profile file")
					return
				}
				_ = os.Remove(profile.FilePath)
			}
			profile.ID = newID
			profile.FilePath = newFilePath
		}
		profile.Name = *req.Name
	}

	// Update settings content.
	if len(req.Settings) > 0 && string(req.Settings) != "null" {
		if !json.Valid(req.Settings) {
			writeError(w, http.StatusBadRequest, "settings must be valid JSON")
			return
		}
		var pretty any
		_ = json.Unmarshal(req.Settings, &pretty)
		out, _ := json.MarshalIndent(pretty, "", "  ")
		if err := os.WriteFile(profile.FilePath, out, 0600); err != nil { //nolint:gosec
			writeError(w, http.StatusInternalServerError, "failed to write profile settings")
			return
		}
		if profile.IsDefault {
			if err := syncDefaultToSettingsJSON(*profile); err != nil {
				s.logger.Error("sync default profile failed", "error", err)
			}
		}
	}

	if err := saveProfilesMetadata(m); err != nil {
		writeError(w, http.StatusInternalServerError, "failed to save profiles metadata")
		return
	}

	detail := ClaudeSettingsProfileDetail{
		ClaudeSettingsProfile: *profile,
	}
	if data, readErr := os.ReadFile(profile.FilePath); readErr == nil { //nolint:gosec
		if json.Valid(data) {
			detail.Settings = json.RawMessage(data)
			detail.Exists = true
		}
	}

	writeJSON(w, http.StatusOK, detail)
}

func (s *Server) handleDeleteClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to load profiles")
		return
	}

	idx := -1
	for i := range m.Profiles {
		if m.Profiles[i].ID == id {
			idx = i
			break
		}
	}
	if idx == -1 {
		writeError(w, http.StatusNotFound, "profile not found")
		return
	}

	if m.Profiles[idx].IsDefault {
		writeError(w, http.StatusConflict, "cannot delete the default profile")
		return
	}

	filePath := m.Profiles[idx].FilePath
	m.Profiles = append(m.Profiles[:idx], m.Profiles[idx+1:]...)

	if err := saveProfilesMetadata(m); err != nil {
		writeError(w, http.StatusInternalServerError, "failed to save profiles metadata")
		return
	}

	_ = os.Remove(filePath)

	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleDuplicateClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to load profiles")
		return
	}

	var source *config.ClaudeSettingsProfile
	for i := range m.Profiles {
		if m.Profiles[i].ID == id {
			source = &m.Profiles[i]
			break
		}
	}
	if source == nil {
		writeError(w, http.StatusNotFound, "profile not found")
		return
	}

	newName := "Copy of " + source.Name
	newID := slugify(newName)
	base := newID
	for i := 2; ; i++ {
		conflict := false
		for _, p := range m.Profiles {
			if p.ID == newID {
				conflict = true
				break
			}
		}
		if !conflict {
			break
		}
		newID = base + "-" + string(rune('0'+i))
	}

	var content []byte
	if data, readErr := os.ReadFile(source.FilePath); readErr == nil { //nolint:gosec
		content = data
	} else {
		content = []byte("{}")
	}

	newFilePath, err := resolveProfileFilePath(newID)
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to resolve profile path")
		return
	}
	if err := os.WriteFile(newFilePath, content, 0600); err != nil { //nolint:gosec
		writeError(w, http.StatusInternalServerError, "failed to write duplicate profile file")
		return
	}

	newProfile := config.ClaudeSettingsProfile{
		ID:        newID,
		Name:      newName,
		FilePath:  newFilePath,
		IsDefault: false,
	}
	m.Profiles = append(m.Profiles, newProfile)
	if err := saveProfilesMetadata(m); err != nil {
		writeError(w, http.StatusInternalServerError, "failed to save profiles metadata")
		return
	}

	writeJSON(w, http.StatusCreated, newProfile)
}

func (s *Server) handleSetDefaultClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	if err := ensureDefaultProfileExists(); err != nil {
		writeError(w, http.StatusInternalServerError, "failed to initialize profiles")
		return
	}

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to load profiles")
		return
	}

	var newDefault *config.ClaudeSettingsProfile
	for i := range m.Profiles {
		if m.Profiles[i].ID == id {
			newDefault = &m.Profiles[i]
		}
		m.Profiles[i].IsDefault = (m.Profiles[i].ID == id)
	}
	if newDefault == nil {
		writeError(w, http.StatusNotFound, "profile not found")
		return
	}

	if err := syncDefaultToSettingsJSON(*newDefault); err != nil {
		s.logger.Error("sync new default profile to settings.json failed", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to sync settings")
		return
	}

	if err := saveProfilesMetadata(m); err != nil {
		writeError(w, http.StatusInternalServerError, "failed to save profiles metadata")
		return
	}

	writeJSON(w, http.StatusOK, *newDefault)
}
