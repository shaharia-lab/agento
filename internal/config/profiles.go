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

// ClaudeSettingsDirPath returns the Claude config dir agent runs target by
// default — $HOME/.claude unless the user or CLAUDE_CONFIG_DIR says otherwise.
//
// Profiles are a global CRUD surface, so they live in the run default rather
// than in any per-agent override: an agent that overrides its config dir
// resolves its settings file inside that dir at run time instead
// (ClaudeSettingsJSONPathIn / LoadProfileFilePathIn).
//
// The error return is retained for callers even though the resolver cannot
// fail; removing it would churn eight call sites for no gain.
func ClaudeSettingsDirPath() (string, error) {
	return ClaudeRunConfigDir(), nil
}

// ClaudeSettingsJSONPath returns the path to settings.json in the run default
// config dir.
func ClaudeSettingsJSONPath() (string, error) {
	dir, err := ClaudeSettingsDirPath()
	if err != nil {
		return "", err
	}
	return ClaudeSettingsJSONPathIn(dir), nil
}

// ClaudeSettingsJSONPathIn returns the path to settings.json inside the given
// config dir.
func ClaudeSettingsJSONPathIn(dir string) string {
	return filepath.Join(dir, "settings.json")
}

// ClaudeSettingsProfilesPath returns the path to settings_profiles.json in the
// run default config dir.
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

// LoadProfileFilePath returns the settings file path for the given profile ID,
// resolved in the run default config dir.
func LoadProfileFilePath(profileID string) (string, error) {
	return LoadProfileFilePathIn(ClaudeRunConfigDir(), profileID), nil
}

// LoadProfileFilePathIn returns the settings file path for the given profile
// ID, resolved inside dir.
//
// An explicitly named profile keeps its recorded absolute path, because a
// profile is a file the user created and pointed at; only the unnamed fallback
// follows dir. That fallback is the case this issue exists for: it used to
// hand every run $HOME/.claude/settings.json regardless of which config dir the
// run targeted, applying one account's settings to the other account's session.
//
// The returned path is not guaranteed to exist — callers pass it to Claude Code
// only after checking, since a dir that has never been used by Claude Code has
// no settings.json and naming a missing file is worse than naming none.
func LoadProfileFilePathIn(dir, profileID string) string {
	fallback := ClaudeSettingsJSONPathIn(dir)
	if profileID == "" {
		return fallback
	}
	m, err := LoadProfilesMetadata()
	if err != nil {
		return fallback
	}
	for _, p := range m.Profiles {
		if p.ID == profileID {
			return p.FilePath
		}
	}
	// Fall back to default profile.
	for _, p := range m.Profiles {
		if p.IsDefault {
			return p.FilePath
		}
	}
	return fallback
}
