package service

import (
	"encoding/json"
	"fmt"
	"log"
	"log/slog"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"unicode"

	"github.com/shaharia-lab/agento/internal/config"
)

// ClaudeSettingsProfileDetail extends ClaudeSettingsProfile with the settings content.
type ClaudeSettingsProfileDetail struct {
	config.ClaudeSettingsProfile
	Settings json.RawMessage `json:"settings"` // raw settings JSON, null if file missing
	Exists   bool            `json:"exists"`
}

// ClaudeSettingsProfileService defines the interface for managing Claude settings profiles.
type ClaudeSettingsProfileService interface {
	// EnsureDefault creates a "Default" profile from the existing settings.json if none exist.
	EnsureDefault() error
	// ListProfiles returns all profiles (empty slice, not nil, when none exist).
	ListProfiles() ([]config.ClaudeSettingsProfile, error)
	// CreateProfile creates a new profile with the given name, seeding content from the current default.
	CreateProfile(name string) (*config.ClaudeSettingsProfile, error)
	// GetProfile returns the profile plus its parsed settings content.
	GetProfile(id string) (*ClaudeSettingsProfileDetail, error)
	// UpdateProfile renames and/or updates the settings of an existing profile.
	UpdateProfile(id string, name *string, settings json.RawMessage) (*ClaudeSettingsProfileDetail, error)
	// DeleteProfile removes a profile (the default profile cannot be deleted).
	DeleteProfile(id string) error
	// DuplicateProfile creates a copy of the given profile.
	DuplicateProfile(id string) (*config.ClaudeSettingsProfile, error)
	// SetDefaultProfile marks the given profile as default and syncs its content to settings.json.
	SetDefaultProfile(id string) (*config.ClaudeSettingsProfile, error)
}

// safeProfileID only allows alphanumeric characters, hyphens, and underscores in
// profile IDs to prevent path-traversal attacks.
var safeProfileID = regexp.MustCompile(`^[a-zA-Z0-9_-]+$`)

type claudeSettingsProfileService struct {
	logger *slog.Logger
}

// NewClaudeSettingsProfileService returns a new ClaudeSettingsProfileService.
func NewClaudeSettingsProfileService(logger *slog.Logger) ClaudeSettingsProfileService {
	return &claudeSettingsProfileService{logger: logger}
}

// ─── public interface ─────────────────────────────────────────────────────────

func (s *claudeSettingsProfileService) EnsureDefault() error {
	return ensureDefaultProfileExists()
}

func (s *claudeSettingsProfileService) ListProfiles() ([]config.ClaudeSettingsProfile, error) {
	if err := ensureDefaultProfileExists(); err != nil {
		return nil, fmt.Errorf("initializing profiles: %w", err)
	}
	m, err := config.LoadProfilesMetadata()
	if err != nil {
		return nil, fmt.Errorf("loading profiles: %w", err)
	}
	if m.Profiles == nil {
		return []config.ClaudeSettingsProfile{}, nil
	}
	return m.Profiles, nil
}

func (s *claudeSettingsProfileService) CreateProfile(name string) (*config.ClaudeSettingsProfile, error) {
	if name == "" {
		return nil, &ValidationError{Field: "name", Message: "name is required"}
	}

	if err := ensureDefaultProfileExists(); err != nil {
		return nil, fmt.Errorf("initializing profiles: %w", err)
	}

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		return nil, fmt.Errorf("loading profiles: %w", err)
	}

	id := deduplicateID(slugify(name), m.Profiles)
	content := readDefaultProfileContent(m.Profiles)

	filePath, err := resolveProfileFilePath(id)
	if err != nil {
		return nil, fmt.Errorf("resolving profile path: %w", err)
	}
	if writeErr := os.WriteFile(filePath, content, 0600); writeErr != nil { //nolint:gosec
		return nil, fmt.Errorf("writing profile file: %w", writeErr)
	}

	newProfile := config.ClaudeSettingsProfile{
		ID: id, Name: name, FilePath: filePath, IsDefault: false,
	}
	m.Profiles = append(m.Profiles, newProfile)
	if saveErr := saveProfilesMetadata(m); saveErr != nil {
		return nil, fmt.Errorf("saving profiles metadata: %w", saveErr)
	}

	return &newProfile, nil
}

func (s *claudeSettingsProfileService) GetProfile(id string) (*ClaudeSettingsProfileDetail, error) {
	m, err := config.LoadProfilesMetadata()
	if err != nil {
		return nil, fmt.Errorf("loading profiles: %w", err)
	}

	idx := findProfileIndex(m.Profiles, id)
	if idx == -1 {
		return nil, &NotFoundError{Resource: "profile", ID: id}
	}

	detail := buildProfileDetail(m.Profiles[idx])
	return &detail, nil
}

func (s *claudeSettingsProfileService) UpdateProfile(
	id string, name *string, settings json.RawMessage,
) (*ClaudeSettingsProfileDetail, error) {
	m, err := config.LoadProfilesMetadata()
	if err != nil {
		return nil, fmt.Errorf("loading profiles: %w", err)
	}

	idx := findProfileIndex(m.Profiles, id)
	if idx == -1 {
		return nil, &NotFoundError{Resource: "profile", ID: id}
	}

	profile := &m.Profiles[idx]

	if name != nil && *name != "" && *name != profile.Name {
		if renameErr := renameProfile(profile, *name, id, idx, m.Profiles); renameErr != nil {
			return nil, renameErr
		}
	}

	if updateErr := s.updateProfileSettings(profile, settings); updateErr != nil {
		return nil, updateErr
	}

	if saveErr := saveProfilesMetadata(m); saveErr != nil {
		return nil, fmt.Errorf("saving profiles metadata: %w", saveErr)
	}

	detail := buildProfileDetail(*profile)
	return &detail, nil
}

func (s *claudeSettingsProfileService) DeleteProfile(id string) error {
	m, err := config.LoadProfilesMetadata()
	if err != nil {
		return fmt.Errorf("loading profiles: %w", err)
	}

	idx := findProfileIndex(m.Profiles, id)
	if idx == -1 {
		return &NotFoundError{Resource: "profile", ID: id}
	}

	if m.Profiles[idx].IsDefault {
		return &ConflictError{Resource: "profile", ID: id}
	}

	filePath := m.Profiles[idx].FilePath
	m.Profiles = append(m.Profiles[:idx], m.Profiles[idx+1:]...)

	if saveErr := saveProfilesMetadata(m); saveErr != nil {
		return fmt.Errorf("saving profiles metadata: %w", saveErr)
	}

	if rmErr := os.Remove(filePath); rmErr != nil {
		log.Printf("failed to remove profile file %s: %v", filePath, rmErr)
	}

	return nil
}

func (s *claudeSettingsProfileService) DuplicateProfile(id string) (*config.ClaudeSettingsProfile, error) {
	m, err := config.LoadProfilesMetadata()
	if err != nil {
		return nil, fmt.Errorf("loading profiles: %w", err)
	}

	idx := findProfileIndex(m.Profiles, id)
	if idx == -1 {
		return nil, &NotFoundError{Resource: "profile", ID: id}
	}
	source := &m.Profiles[idx]

	newName := "Copy of " + source.Name
	newID := deduplicateID(slugify(newName), m.Profiles)

	content, readErr := os.ReadFile(source.FilePath) //nolint:gosec
	if readErr != nil || content == nil {
		content = []byte("{}")
	}

	newFilePath, resolveErr := resolveProfileFilePath(newID)
	if resolveErr != nil {
		return nil, fmt.Errorf("resolving profile path: %w", resolveErr)
	}
	if writeErr := os.WriteFile(newFilePath, content, 0600); writeErr != nil { //nolint:gosec
		return nil, fmt.Errorf("writing duplicate profile file: %w", writeErr)
	}

	newProfile := config.ClaudeSettingsProfile{
		ID: newID, Name: newName, FilePath: newFilePath, IsDefault: false,
	}
	m.Profiles = append(m.Profiles, newProfile)
	if saveErr := saveProfilesMetadata(m); saveErr != nil {
		return nil, fmt.Errorf("saving profiles metadata: %w", saveErr)
	}

	return &newProfile, nil
}

func (s *claudeSettingsProfileService) SetDefaultProfile(id string) (*config.ClaudeSettingsProfile, error) {
	if err := ensureDefaultProfileExists(); err != nil {
		return nil, fmt.Errorf("initializing profiles: %w", err)
	}

	m, err := config.LoadProfilesMetadata()
	if err != nil {
		return nil, fmt.Errorf("loading profiles: %w", err)
	}

	var newDefault *config.ClaudeSettingsProfile
	for i := range m.Profiles {
		if m.Profiles[i].ID == id {
			newDefault = &m.Profiles[i]
		}
		m.Profiles[i].IsDefault = (m.Profiles[i].ID == id)
	}
	if newDefault == nil {
		return nil, &NotFoundError{Resource: "profile", ID: id}
	}

	if syncErr := syncDefaultToSettingsJSON(*newDefault); syncErr != nil {
		return nil, fmt.Errorf("syncing settings.json: %w", syncErr)
	}

	if saveErr := saveProfilesMetadata(m); saveErr != nil {
		return nil, fmt.Errorf("saving profiles metadata: %w", saveErr)
	}

	return newDefault, nil
}

// ─── private helpers ─────────────────────────────────────────────────────────

// slugify converts a profile name into a safe filename slug.
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

// saveProfilesMetadata writes the profiles metadata file.
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
	settingsPath, err := config.ClaudeSettingsJSONPath()
	if err != nil {
		return err
	}
	return os.WriteFile(settingsPath, data, 0600) //nolint:gosec // path from user home
}

// resolveProfileFilePath returns the absolute path for a profile file based on its ID.
func resolveProfileFilePath(id string) (string, error) {
	if !safeProfileID.MatchString(id) {
		return "", fmt.Errorf("invalid profile id: contains disallowed characters")
	}
	dir, err := config.ClaudeSettingsDirPath()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "settings_"+id+".json"), nil
}

// ensureDefaultProfileExists creates a "Default" profile if no profiles exist yet.
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

	settingsPath, err := config.ClaudeSettingsJSONPath()
	if err != nil {
		return err
	}
	var content []byte
	if data, readErr := os.ReadFile(settingsPath); readErr == nil { //nolint:gosec
		content = data
	} else {
		content = []byte("{}")
	}

	var pretty any
	if json.Unmarshal(content, &pretty) != nil {
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

// deduplicateID ensures id is unique among the given profiles by appending a suffix.
func deduplicateID(base string, profiles []config.ClaudeSettingsProfile) string {
	id := base
	for i := 2; findProfileIndex(profiles, id) != -1; i++ {
		id = fmt.Sprintf("%s-%d", base, i)
	}
	return id
}

// readDefaultProfileContent reads the content from the current default profile.
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

// buildProfileDetail creates a ClaudeSettingsProfileDetail from a profile.
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

// renameProfile renames a profile, moving the file if the slug changes.
// Returns a service error (NotFoundError, ConflictError, or a wrapped error).
func renameProfile(
	profile *config.ClaudeSettingsProfile,
	newName, currentID string, currentIdx int,
	profiles []config.ClaudeSettingsProfile,
) error {
	newID := slugify(newName)
	if newID != currentID {
		for i, p := range profiles {
			if i != currentIdx && p.ID == newID {
				return &ConflictError{Resource: "profile", ID: newID}
			}
		}
		if err := moveProfileFile(profile, newID); err != nil {
			return err
		}
	}
	profile.Name = newName
	return nil
}

// moveProfileFile moves the profile file to a new path derived from newID.
func moveProfileFile(profile *config.ClaudeSettingsProfile, newID string) error {
	newFilePath, err := resolveProfileFilePath(newID)
	if err != nil {
		return fmt.Errorf("resolving new profile path: %w", err)
	}
	data, readErr := os.ReadFile(profile.FilePath) //nolint:gosec
	if readErr != nil {
		// No file to move; just update metadata.
		profile.ID = newID
		profile.FilePath = newFilePath
		return nil
	}
	if writeErr := os.WriteFile(newFilePath, data, 0600); writeErr != nil { //nolint:gosec
		return fmt.Errorf("writing renamed profile file: %w", writeErr)
	}
	if rmErr := os.Remove(profile.FilePath); rmErr != nil {
		log.Printf("failed to remove old profile file: %v", rmErr)
	}
	profile.ID = newID
	profile.FilePath = newFilePath
	return nil
}

// updateProfileSettings writes new settings JSON to the profile's file.
func (s *claudeSettingsProfileService) updateProfileSettings(
	profile *config.ClaudeSettingsProfile, settings json.RawMessage,
) error {
	if len(settings) == 0 || string(settings) == "null" {
		return nil
	}
	if !json.Valid(settings) {
		return &ValidationError{Field: "settings", Message: "settings must be valid JSON"}
	}
	var pretty any
	if err := json.Unmarshal(settings, &pretty); err != nil {
		return &ValidationError{Field: "settings", Message: "failed to parse settings JSON"}
	}
	out, merr := json.MarshalIndent(pretty, "", "  ")
	if merr != nil {
		return fmt.Errorf("formatting settings JSON: %w", merr)
	}
	if writeErr := os.WriteFile(profile.FilePath, out, 0600); writeErr != nil { //nolint:gosec
		return fmt.Errorf("writing profile settings: %w", writeErr)
	}
	if profile.IsDefault {
		if syncErr := syncDefaultToSettingsJSON(*profile); syncErr != nil {
			s.logger.Error("sync default profile failed", "error", syncErr)
		}
	}
	return nil
}
