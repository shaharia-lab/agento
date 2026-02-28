package config

import (
	"fmt"
	"maps"
	"os"
	"path/filepath"
)

const defaultModel = "sonnet"

// DefaultWorkingDir returns the default working directory for agent sessions,
// located under the user's home directory (~/.agento/work) to avoid using
// predictable, publicly writable temp paths.
func DefaultWorkingDir() string {
	home, err := os.UserHomeDir()
	if err != nil {
		// Fall back to a path under the current working directory rather than
		// os.TempDir() which is predictable and publicly writable.
		cwd, cwdErr := os.Getwd()
		if cwdErr != nil {
			cwd = "."
		}
		return filepath.Join(cwd, ".agento", "work")
	}
	return filepath.Join(home, ".agento", "work")
}

// UserSettings holds persisted user preferences.
type UserSettings struct {
	DefaultWorkingDir    string `json:"default_working_dir"`
	DefaultModel         string `json:"default_model"`
	OnboardingComplete   bool   `json:"onboarding_complete"`
	AppearanceDarkMode   bool   `json:"appearance_dark_mode"`
	AppearanceFontSize   int    `json:"appearance_font_size"`
	AppearanceFontFamily string `json:"appearance_font_family"`
}

// SettingsStore defines the interface for persisting user settings.
type SettingsStore interface {
	Load() (UserSettings, error)
	Save(settings UserSettings) error
}

// SettingsManager loads and saves user settings via a SettingsStore, and exposes
// which fields are locked by environment variables.
type SettingsManager struct {
	store        SettingsStore
	settings     UserSettings
	locked       map[string]string // field name → env var name
	modelFromEnv bool              // true when the displayed model originates from an env var
	modelInFile  bool              // true when default_model was explicitly present in the store
}

// NewSettingsManager creates a SettingsManager backed by the given SettingsStore.
// Fields that are set via AppConfig environment variables are marked as locked.
func NewSettingsManager(store SettingsStore, cfg *AppConfig) (*SettingsManager, error) {
	m := &SettingsManager{
		store:  store,
		locked: make(map[string]string),
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
	settings, err := m.store.Load()
	if err != nil {
		return err
	}
	m.settings = settings

	// Track whether the model field was explicitly set.
	m.modelInFile = m.settings.DefaultModel != ""

	// Fill in any missing defaults.
	if m.settings.DefaultWorkingDir == "" {
		m.settings.DefaultWorkingDir = DefaultWorkingDir()
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

	if err := m.store.Save(m.settings); err != nil {
		return fmt.Errorf("persisting settings: %w", err)
	}
	return nil
}
