package config

import (
	"encoding/json"
	"fmt"
	"maps"
	"os"
	"path/filepath"
)

const (
	defaultWorkingDir = "/tmp/agento/work"
	defaultModel      = "eu.anthropic.claude-sonnet-4-5-20250929-v1:0"
)

// UserSettings holds persisted user preferences.
type UserSettings struct {
	DefaultWorkingDir    string `json:"default_working_dir"`
	DefaultModel         string `json:"default_model"`
	OnboardingComplete   bool   `json:"onboarding_complete"`
	AppearanceDarkMode   bool   `json:"appearance_dark_mode"`
	AppearanceFontSize   int    `json:"appearance_font_size"`
	AppearanceFontFamily string `json:"appearance_font_family"`
}

// SettingsManager loads and saves user settings to disk, and exposes which
// fields are locked by environment variables.
type SettingsManager struct {
	filePath     string
	settings     UserSettings
	locked       map[string]string // field name → env var name
	modelFromEnv bool              // true when the displayed model originates from an env var
	modelInFile  bool              // true when default_model was explicitly present in the settings file
}

// NewSettingsManager creates a SettingsManager backed by dataDir/settings.json.
// Fields that are set via AppConfig environment variables are marked as locked.
func NewSettingsManager(dataDir string, cfg *AppConfig) (*SettingsManager, error) {
	m := &SettingsManager{
		filePath: filepath.Join(dataDir, "settings.json"),
		locked:   make(map[string]string),
	}

	// Determine which fields are locked by env vars.
	if cfg.DefaultModel != "" && os.Getenv("AGENTO_DEFAULT_MODEL") != "" {
		m.locked["default_model"] = "AGENTO_DEFAULT_MODEL"
	}
	if cfg.WorkingDir != "" && os.Getenv("AGENTO_WORKING_DIR") != "" {
		m.locked["default_working_dir"] = "AGENTO_WORKING_DIR"
	}

	if err := m.load(); err != nil {
		return nil, fmt.Errorf("loading settings: %w", err)
	}

	// Apply env-locked overrides so Get() always returns env values for locked fields.
	if _, ok := m.locked["default_model"]; ok {
		m.settings.DefaultModel = cfg.DefaultModel
		m.modelFromEnv = true
	} else if cfg.AnthropicDefaultSonnetModel != "" && !m.modelInFile {
		// ANTHROPIC_DEFAULT_SONNET_MODEL provides a soft default when the user has
		// not explicitly saved a model choice to the settings file.
		m.settings.DefaultModel = cfg.AnthropicDefaultSonnetModel
		m.modelFromEnv = true
	}

	if _, ok := m.locked["default_working_dir"]; ok {
		m.settings.DefaultWorkingDir = cfg.WorkingDir
	}

	return m, nil
}

func (m *SettingsManager) load() error {
	data, err := os.ReadFile(m.filePath) //nolint:gosec // path constructed from admin-configured data dir
	if err != nil {
		if os.IsNotExist(err) {
			// First run — initialize with defaults.
			m.settings = UserSettings{
				DefaultWorkingDir:  defaultWorkingDir,
				DefaultModel:       defaultModel,
				OnboardingComplete: false,
			}
			return nil
		}
		return fmt.Errorf("reading settings file: %w", err)
	}

	if err := json.Unmarshal(data, &m.settings); err != nil {
		return fmt.Errorf("parsing settings file: %w", err)
	}

	// Track whether each field was explicitly present in the file before we fill defaults.
	m.modelInFile = m.settings.DefaultModel != ""

	// Fill in any missing fields that were added after the file was created.
	if m.settings.DefaultWorkingDir == "" {
		m.settings.DefaultWorkingDir = defaultWorkingDir
	}
	if m.settings.DefaultModel == "" {
		m.settings.DefaultModel = defaultModel
	}
	return nil
}

// Get returns a copy of the current settings (env-locked fields return env value).
func (m *SettingsManager) Get() UserSettings {
	return m.settings
}

// ModelFromEnv returns true when the displayed default model originates from an
// environment variable (either AGENTO_DEFAULT_MODEL or ANTHROPIC_DEFAULT_SONNET_MODEL).
func (m *SettingsManager) ModelFromEnv() bool {
	return m.modelFromEnv
}

// Locked returns the map of field names to env var names for locked settings.
func (m *SettingsManager) Locked() map[string]string {
	result := make(map[string]string, len(m.locked))
	maps.Copy(result, m.locked)
	return result
}

// Update persists allowed fields, skipping any locked ones. Returns an error if
// the caller attempts to change a locked field.
func (m *SettingsManager) Update(incoming UserSettings) error {
	if _, ok := m.locked["default_model"]; ok {
		if incoming.DefaultModel != "" && incoming.DefaultModel != m.settings.DefaultModel {
			return fmt.Errorf("default_model is locked by environment variable %s", m.locked["default_model"])
		}
		// Keep the env value.
		incoming.DefaultModel = m.settings.DefaultModel
	}
	if _, ok := m.locked["default_working_dir"]; ok {
		if incoming.DefaultWorkingDir != "" && incoming.DefaultWorkingDir != m.settings.DefaultWorkingDir {
			return fmt.Errorf("default_working_dir is locked by environment variable %s", m.locked["default_working_dir"])
		}
		incoming.DefaultWorkingDir = m.settings.DefaultWorkingDir
	}

	m.settings = incoming

	data, err := json.MarshalIndent(m.settings, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling settings: %w", err)
	}
	if err := os.WriteFile(m.filePath, data, 0600); err != nil {
		return fmt.Errorf("writing settings file: %w", err)
	}
	return nil
}
