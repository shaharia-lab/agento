package api

import (
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"unicode"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/config"
)

// Profile-related error message constants.
const (
	errInitProfiles     = "failed to initialize profiles"
	errLoadProfiles     = "failed to load profiles"
	errSaveProfilesMeta = "failed to save profiles metadata"
	errProfileNotFound  = "profile not found"
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
	if mkdirErr := os.MkdirAll(filepath.Dir(path), 0700); mkdirErr != nil {
		return mkdirErr
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
	if mkdirErr := os.MkdirAll(dir, 0700); mkdirErr != nil {
		return mkdirErr
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
	out, merr := json.MarshalIndent(pretty, "", "  ")
	if merr != nil {
		return fmt.Errorf("marshaling default profile settings: %w", merr)
	}

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

// findProfileIndex returns the index of the profile with the given ID, or -1.
func findProfileIndex(profiles []config.ClaudeSettingsProfile, id string) int {
	for i := range profiles {
		if profiles[i].ID == id {
			return i
		}
	}
	return -1
}

// deduplicateID ensures id is unique among the given profiles by appending a numeric suffix.
func deduplicateID(base string, profiles []config.ClaudeSettingsProfile) string {
	id := base
	for i := 2; findProfileIndex(profiles, id) != -1; i++ {
		id = base + "-" + string(rune('0'+i))
	}
	return id
}

// readDefaultProfileContent reads the content from the current default profile, falling back to "{}".
func readDefaultProfileContent(profiles []config.ClaudeSettingsProfile) []byte {
	for _, p := range profiles {
		if p.IsDefault {
			if data, err := os.ReadFile(p.FilePath); err == nil { //nolint:gosec
				return data
			}
			break
		}
	}
	return []byte("{}")
}

// buildProfileDetail creates a ClaudeSettingsProfileDetail from a profile,
// reading the settings file if it exists.
func buildProfileDetail(profile config.ClaudeSettingsProfile) ClaudeSettingsProfileDetail {
	detail := ClaudeSettingsProfileDetail{ClaudeSettingsProfile: profile}
	if data, err := os.ReadFile(profile.FilePath); err == nil { //nolint:gosec
		if json.Valid(data) {
			detail.Settings = json.RawMessage(data)
			detail.Exists = true
		}
	}
	return detail
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

func (s *Server) handleListClaudeSettingsProfiles(w http.ResponseWriter, _ *http.Request) {
	if err := ensureDefaultProfileExists(); err != nil {
		s.logger.Error("ensure default profile failed", "error", err)
		writeError(w, http.StatusInternalServerError, errInitProfiles)
		return
	}
	m, err := config.LoadProfilesMetadata()
	if err != nil {
		s.logger.Error("load profiles metadata failed", "error", err)
		writeError(w, http.StatusInternalServerError, errLoadProfiles)
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
		writeError(w, http.StatusInternalServerError, errInitProfiles)
		return
	}

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, errLoadProfiles)
		return
	}

	id := deduplicateID(slugify(req.Name), m.Profiles)
	content := readDefaultProfileContent(m.Profiles)

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
		ID: id, Name: req.Name, FilePath: filePath, IsDefault: false,
	}
	m.Profiles = append(m.Profiles, newProfile)
	if err := saveProfilesMetadata(m); err != nil {
		writeError(w, http.StatusInternalServerError, errSaveProfilesMeta)
		return
	}

	writeJSON(w, http.StatusCreated, newProfile)
}

func (s *Server) handleGetClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, errLoadProfiles)
		return
	}

	idx := findProfileIndex(m.Profiles, id)
	if idx == -1 {
		writeError(w, http.StatusNotFound, errProfileNotFound)
		return
	}

	writeJSON(w, http.StatusOK, buildProfileDetail(m.Profiles[idx]))
}

func (s *Server) handleUpdateClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	var req struct {
		Name     *string         `json:"name"`
		Settings json.RawMessage `json:"settings"`
	}
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, errLoadProfiles)
		return
	}

	idx := findProfileIndex(m.Profiles, id)
	if idx == -1 {
		writeError(w, http.StatusNotFound, errProfileNotFound)
		return
	}

	profile := &m.Profiles[idx]

	if req.Name != nil && *req.Name != "" && *req.Name != profile.Name {
		if err := renameProfile(profile, *req.Name, id, idx, m.Profiles); err != nil {
			writeError(w, err.code, err.msg)
			return
		}
	}

	if err := s.updateProfileSettings(profile, req.Settings); err != nil {
		writeError(w, err.code, err.msg)
		return
	}

	if err := saveProfilesMetadata(m); err != nil {
		writeError(w, http.StatusInternalServerError, errSaveProfilesMeta)
		return
	}

	writeJSON(w, http.StatusOK, buildProfileDetail(*profile))
}

// profileError is a helper for returning HTTP errors from profile sub-operations.
type profileError struct {
	code int
	msg  string
}

func (e *profileError) Error() string { return e.msg }

func renameProfile(
	profile *config.ClaudeSettingsProfile,
	newName, currentID string, currentIdx int,
	profiles []config.ClaudeSettingsProfile,
) *profileError {
	newID := slugify(newName)
	if newID != currentID {
		// Check for conflicts excluding the current profile.
		for i, p := range profiles {
			if i != currentIdx && p.ID == newID {
				return &profileError{http.StatusConflict, "a profile with that name already exists"}
			}
		}
		if err := moveProfileFile(profile, newID); err != nil {
			return err
		}
	}
	profile.Name = newName
	return nil
}

func moveProfileFile(profile *config.ClaudeSettingsProfile, newID string) *profileError {
	newFilePath, err := resolveProfileFilePath(newID)
	if err != nil {
		return &profileError{http.StatusInternalServerError, "failed to resolve new profile path"}
	}
	data, readErr := os.ReadFile(profile.FilePath) //nolint:gosec
	if readErr != nil {
		// No file to move; just update metadata.
		profile.ID = newID
		profile.FilePath = newFilePath
		return nil
	}
	if writeErr := os.WriteFile(newFilePath, data, 0600); writeErr != nil { //nolint:gosec
		return &profileError{http.StatusInternalServerError, "failed to rename profile file"}
	}
	if rmErr := os.Remove(profile.FilePath); rmErr != nil {
		log.Printf("failed to remove old profile file: %v", rmErr)
	}
	profile.ID = newID
	profile.FilePath = newFilePath
	return nil
}

func (s *Server) updateProfileSettings(profile *config.ClaudeSettingsProfile, settings json.RawMessage) *profileError {
	if len(settings) == 0 || string(settings) == "null" {
		return nil
	}
	if !json.Valid(settings) {
		return &profileError{http.StatusBadRequest, "settings must be valid JSON"}
	}
	var pretty any
	if uerr := json.Unmarshal(settings, &pretty); uerr != nil {
		return &profileError{http.StatusBadRequest, "failed to parse settings JSON"}
	}
	out, merr := json.MarshalIndent(pretty, "", "  ")
	if merr != nil {
		return &profileError{http.StatusInternalServerError, "failed to format settings JSON"}
	}
	if err := os.WriteFile(profile.FilePath, out, 0600); err != nil { //nolint:gosec
		return &profileError{http.StatusInternalServerError, "failed to write profile settings"}
	}
	if profile.IsDefault {
		if err := syncDefaultToSettingsJSON(*profile); err != nil {
			s.logger.Error("sync default profile failed", "error", err)
		}
	}
	return nil
}

func (s *Server) handleDeleteClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, errLoadProfiles)
		return
	}

	idx := findProfileIndex(m.Profiles, id)
	if idx == -1 {
		writeError(w, http.StatusNotFound, errProfileNotFound)
		return
	}

	if m.Profiles[idx].IsDefault {
		writeError(w, http.StatusConflict, "cannot delete the default profile")
		return
	}

	filePath := m.Profiles[idx].FilePath
	m.Profiles = append(m.Profiles[:idx], m.Profiles[idx+1:]...)

	if err := saveProfilesMetadata(m); err != nil {
		writeError(w, http.StatusInternalServerError, errSaveProfilesMeta)
		return
	}

	if rmErr := os.Remove(filePath); rmErr != nil {
		log.Printf("failed to remove profile file %s: %v", filePath, rmErr)
	}

	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleDuplicateClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, errLoadProfiles)
		return
	}

	idx := findProfileIndex(m.Profiles, id)
	if idx == -1 {
		writeError(w, http.StatusNotFound, errProfileNotFound)
		return
	}
	source := &m.Profiles[idx]

	newName := "Copy of " + source.Name
	newID := deduplicateID(slugify(newName), m.Profiles)

	content, readErr := os.ReadFile(source.FilePath) //nolint:gosec
	if readErr != nil || content == nil {
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
		ID: newID, Name: newName, FilePath: newFilePath, IsDefault: false,
	}
	m.Profiles = append(m.Profiles, newProfile)
	if err := saveProfilesMetadata(m); err != nil {
		writeError(w, http.StatusInternalServerError, errSaveProfilesMeta)
		return
	}

	writeJSON(w, http.StatusCreated, newProfile)
}

func (s *Server) handleSetDefaultClaudeSettingsProfile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	if err := ensureDefaultProfileExists(); err != nil {
		writeError(w, http.StatusInternalServerError, errInitProfiles)
		return
	}

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		writeError(w, http.StatusInternalServerError, errLoadProfiles)
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
		writeError(w, http.StatusNotFound, errProfileNotFound)
		return
	}

	if err := syncDefaultToSettingsJSON(*newDefault); err != nil {
		s.logger.Error("sync new default profile to settings.json failed", "error", err)
		writeError(w, http.StatusInternalServerError, "failed to sync settings")
		return
	}

	if err := saveProfilesMetadata(m); err != nil {
		writeError(w, http.StatusInternalServerError, errSaveProfilesMeta)
		return
	}

	writeJSON(w, http.StatusOK, *newDefault)
}
