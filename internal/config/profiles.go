package config

import (
	"encoding/json"
	"os"
	"path/filepath"
)

// ClaudeSettingsProfile describes a named Claude settings profile.
type ClaudeSettingsProfile struct {
	ID        string `json:"id"` // slugified name
	Name      string `json:"name"`
	FilePath  string `json:"file_path"` // absolute path to settings_<id>.json
	IsDefault bool   `json:"is_default"`
}

// ProfilesMetadata is the on-disk representation of the profiles index.
type ProfilesMetadata struct {
	Profiles []ClaudeSettingsProfile `json:"profiles"`
}

// ClaudeSettingsDirPath returns the path to the ~/.claude directory.
func ClaudeSettingsDirPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".claude"), nil
}

// ClaudeSettingsJSONPath returns the path to ~/.claude/settings.json.
func ClaudeSettingsJSONPath() (string, error) {
	dir, err := ClaudeSettingsDirPath()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "settings.json"), nil
}

// ClaudeSettingsProfilesPath returns the path to ~/.claude/settings_profiles.json.
func ClaudeSettingsProfilesPath() (string, error) {
	dir, err := ClaudeSettingsDirPath()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "settings_profiles.json"), nil
}

// LoadProfilesMetadata reads the profiles metadata file.
// Returns an empty struct (no error) if the file doesn't exist yet.
func LoadProfilesMetadata() (ProfilesMetadata, error) {
	path, err := ClaudeSettingsProfilesPath()
	if err != nil {
		return ProfilesMetadata{}, err
	}
	data, err := os.ReadFile(path) //nolint:gosec // path constructed from user home
	if err != nil {
		if os.IsNotExist(err) {
			return ProfilesMetadata{}, nil
		}
		return ProfilesMetadata{}, err
	}
	var m ProfilesMetadata
	if err := json.Unmarshal(data, &m); err != nil {
		return ProfilesMetadata{}, err
	}
	return m, nil
}

// LoadProfileFilePath returns the settings file path for the given profile ID.
// If profileID is empty or not found, falls back to ~/.claude/settings.json.
func LoadProfileFilePath(profileID string) (string, error) {
	if profileID == "" {
		return ClaudeSettingsJSONPath()
	}
	m, err := LoadProfilesMetadata()
	if err != nil {
		return ClaudeSettingsJSONPath()
	}
	for _, p := range m.Profiles {
		if p.ID == profileID {
			return p.FilePath, nil
		}
	}
	// Fall back to default profile.
	for _, p := range m.Profiles {
		if p.IsDefault {
			return p.FilePath, nil
		}
	}
	return ClaudeSettingsJSONPath()
}
