package config_test

import (
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/config/mocks"
)

func TestNewSettingsManager(t *testing.T) {
	tests := []struct {
		name          string
		storeSettings config.UserSettings
		storeErr      error
		cfg           *config.AppConfig
		envVars       map[string]string
		wantErr       string
		wantSettings  func(t *testing.T, s config.UserSettings)
		wantModelEnv  bool
		wantLocked    map[string]string
	}{
		{
			name:          "load defaults when store returns empty settings",
			storeSettings: config.UserSettings{},
			cfg:           &config.AppConfig{},
			wantSettings: func(t *testing.T, s config.UserSettings) {
				assert.Equal(t, config.DefaultWorkingDir(), s.DefaultWorkingDir)
				assert.Equal(t, "sonnet", s.DefaultModel)
			},
			wantModelEnv: false,
			wantLocked:   map[string]string{},
		},
		{
			name: "store returns populated settings - preserved as is",
			storeSettings: config.UserSettings{
				DefaultWorkingDir:  "/custom/work",
				DefaultModel:       "claude-opus-4-20250514",
				OnboardingComplete: true,
				AppearanceDarkMode: true,
				AppearanceFontSize: 16,
			},
			cfg: &config.AppConfig{},
			wantSettings: func(t *testing.T, s config.UserSettings) {
				assert.Equal(t, "/custom/work", s.DefaultWorkingDir)
				assert.Equal(t, "claude-opus-4-20250514", s.DefaultModel)
				assert.True(t, s.OnboardingComplete)
				assert.True(t, s.AppearanceDarkMode)
				assert.Equal(t, 16, s.AppearanceFontSize)
			},
			wantModelEnv: false,
			wantLocked:   map[string]string{},
		},
		{
			name:          "model locked by AGENTO_DEFAULT_MODEL env var",
			storeSettings: config.UserSettings{DefaultModel: "store-model"},
			cfg:           &config.AppConfig{DefaultModel: "env-model"},
			envVars:       map[string]string{"AGENTO_DEFAULT_MODEL": "env-model"},
			wantSettings: func(t *testing.T, s config.UserSettings) {
				assert.Equal(t, "env-model", s.DefaultModel)
			},
			wantModelEnv: true,
			wantLocked:   map[string]string{"default_model": "AGENTO_DEFAULT_MODEL"},
		},
		{
			name:          "working dir locked by AGENTO_WORKING_DIR env var",
			storeSettings: config.UserSettings{DefaultWorkingDir: "/store/dir"},
			cfg:           &config.AppConfig{WorkingDir: "/env/dir"},
			envVars:       map[string]string{"AGENTO_WORKING_DIR": "/env/dir"},
			wantSettings: func(t *testing.T, s config.UserSettings) {
				assert.Equal(t, "/env/dir", s.DefaultWorkingDir)
			},
			wantModelEnv: false,
			wantLocked:   map[string]string{"default_working_dir": "AGENTO_WORKING_DIR"},
		},
		{
			name:          "both model and working dir locked",
			storeSettings: config.UserSettings{},
			cfg:           &config.AppConfig{DefaultModel: "locked-model", WorkingDir: "/locked/dir"},
			envVars: map[string]string{
				"AGENTO_DEFAULT_MODEL": "locked-model",
				"AGENTO_WORKING_DIR":   "/locked/dir",
			},
			wantSettings: func(t *testing.T, s config.UserSettings) {
				assert.Equal(t, "locked-model", s.DefaultModel)
				assert.Equal(t, "/locked/dir", s.DefaultWorkingDir)
			},
			wantModelEnv: true,
			wantLocked: map[string]string{
				"default_model":       "AGENTO_DEFAULT_MODEL",
				"default_working_dir": "AGENTO_WORKING_DIR",
			},
		},
		{
			name:          "ANTHROPIC_DEFAULT_SONNET_MODEL used as soft default when no model in store",
			storeSettings: config.UserSettings{},
			cfg:           &config.AppConfig{AnthropicDefaultSonnetModel: "soft-sonnet-model"},
			wantSettings: func(t *testing.T, s config.UserSettings) {
				assert.Equal(t, "soft-sonnet-model", s.DefaultModel)
			},
			wantModelEnv: true,
			wantLocked:   map[string]string{},
		},
		{
			name:          "ANTHROPIC_DEFAULT_SONNET_MODEL ignored when model already in store",
			storeSettings: config.UserSettings{DefaultModel: "user-chosen-model"},
			cfg:           &config.AppConfig{AnthropicDefaultSonnetModel: "soft-sonnet-model"},
			wantSettings: func(t *testing.T, s config.UserSettings) {
				assert.Equal(t, "user-chosen-model", s.DefaultModel)
			},
			wantModelEnv: false,
			wantLocked:   map[string]string{},
		},
		{
			name:          "AGENTO_DEFAULT_MODEL takes priority over ANTHROPIC_DEFAULT_SONNET_MODEL",
			storeSettings: config.UserSettings{},
			cfg: &config.AppConfig{
				DefaultModel:                "hard-locked",
				AnthropicDefaultSonnetModel: "soft-sonnet",
			},
			envVars:      map[string]string{"AGENTO_DEFAULT_MODEL": "hard-locked"},
			wantModelEnv: true,
			wantSettings: func(t *testing.T, s config.UserSettings) {
				assert.Equal(t, "hard-locked", s.DefaultModel)
			},
			wantLocked: map[string]string{"default_model": "AGENTO_DEFAULT_MODEL"},
		},
		{
			name:          "cfg.DefaultModel set but env var not set - not locked",
			storeSettings: config.UserSettings{},
			cfg:           &config.AppConfig{DefaultModel: "from-config"},
			wantSettings: func(t *testing.T, s config.UserSettings) {
				// Model not locked, so store value (default) is used
				assert.Equal(t, "sonnet", s.DefaultModel)
			},
			wantModelEnv: false,
			wantLocked:   map[string]string{},
		},
		{
			name:     "store Load error propagates",
			storeErr: errors.New("disk on fire"),
			cfg:      &config.AppConfig{},
			wantErr:  "loading settings: disk on fire",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Set env vars for this subtest; t.Setenv restores after test.
			for k, v := range tt.envVars {
				t.Setenv(k, v)
			}

			store := new(mocks.MockSettingsStore)
			store.On("Load").Return(tt.storeSettings, tt.storeErr)

			mgr, err := config.NewSettingsManager(store, tt.cfg)

			if tt.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tt.wantErr)
				assert.Nil(t, mgr)
				return
			}

			require.NoError(t, err)
			require.NotNil(t, mgr)

			if tt.wantSettings != nil {
				tt.wantSettings(t, mgr.Get())
			}
			assert.Equal(t, tt.wantModelEnv, mgr.ModelFromEnv())
			assert.Equal(t, tt.wantLocked, mgr.Locked())

			store.AssertExpectations(t)
		})
	}
}

func TestSettingsManager_Get(t *testing.T) {
	store := new(mocks.MockSettingsStore)
	store.On("Load").Return(config.UserSettings{
		DefaultModel:       "test-model",
		DefaultWorkingDir:  "/test/dir",
		OnboardingComplete: true,
	}, nil)

	mgr, err := config.NewSettingsManager(store, &config.AppConfig{})
	require.NoError(t, err)

	got := mgr.Get()
	assert.Equal(t, "test-model", got.DefaultModel)
	assert.Equal(t, "/test/dir", got.DefaultWorkingDir)
	assert.True(t, got.OnboardingComplete)
}

func TestSettingsManager_Locked_returns_copy(t *testing.T) {
	t.Setenv("AGENTO_DEFAULT_MODEL", "m")

	store := new(mocks.MockSettingsStore)
	store.On("Load").Return(config.UserSettings{}, nil)

	mgr, err := config.NewSettingsManager(store, &config.AppConfig{DefaultModel: "m"})
	require.NoError(t, err)

	locked1 := mgr.Locked()
	locked1["sneaky"] = "mutation"

	locked2 := mgr.Locked()
	_, found := locked2["sneaky"]
	assert.False(t, found, "Locked() should return a defensive copy")
}

func TestSettingsManager_Update(t *testing.T) {
	tests := []struct {
		name          string
		storeSettings config.UserSettings
		cfg           *config.AppConfig
		envVars       map[string]string
		incoming      config.UserSettings
		saveErr       error
		wantErr       string
		wantSaved     *config.UserSettings
	}{
		{
			name:          "update unlocked fields succeeds",
			storeSettings: config.UserSettings{},
			cfg:           &config.AppConfig{},
			incoming: config.UserSettings{
				DefaultModel:       "new-model",
				DefaultWorkingDir:  "/new/dir",
				OnboardingComplete: true,
				AppearanceDarkMode: true,
				AppearanceFontSize: 14,
			},
			wantSaved: &config.UserSettings{
				DefaultModel:       "new-model",
				DefaultWorkingDir:  "/new/dir",
				OnboardingComplete: true,
				AppearanceDarkMode: true,
				AppearanceFontSize: 14,
			},
		},
		{
			name:          "update locked model field returns error",
			storeSettings: config.UserSettings{},
			cfg:           &config.AppConfig{DefaultModel: "env-model"},
			envVars:       map[string]string{"AGENTO_DEFAULT_MODEL": "env-model"},
			incoming:      config.UserSettings{DefaultModel: "try-change"},
			wantErr:       "default_model is locked by environment variable AGENTO_DEFAULT_MODEL",
		},
		{
			name:          "update locked working dir field returns error",
			storeSettings: config.UserSettings{},
			cfg:           &config.AppConfig{WorkingDir: "/env/dir"},
			envVars:       map[string]string{"AGENTO_WORKING_DIR": "/env/dir"},
			incoming:      config.UserSettings{DefaultWorkingDir: "/try-change"},
			wantErr:       "default_working_dir is locked by environment variable AGENTO_WORKING_DIR",
		},
		{
			name:          "update locked model with same value succeeds",
			storeSettings: config.UserSettings{},
			cfg:           &config.AppConfig{DefaultModel: "env-model"},
			envVars:       map[string]string{"AGENTO_DEFAULT_MODEL": "env-model"},
			incoming:      config.UserSettings{DefaultModel: "env-model"},
			wantSaved: &config.UserSettings{
				DefaultModel: "env-model",
			},
		},
		{
			name:          "update locked model with empty value succeeds (keeps env value)",
			storeSettings: config.UserSettings{},
			cfg:           &config.AppConfig{DefaultModel: "env-model"},
			envVars:       map[string]string{"AGENTO_DEFAULT_MODEL": "env-model"},
			incoming:      config.UserSettings{DefaultModel: ""},
			wantSaved: &config.UserSettings{
				DefaultModel: "env-model",
			},
		},
		{
			name:          "save error is wrapped and returned",
			storeSettings: config.UserSettings{},
			cfg:           &config.AppConfig{},
			incoming:      config.UserSettings{DefaultModel: "m"},
			saveErr:       errors.New("write failed"),
			wantErr:       "persisting settings: write failed",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			for k, v := range tt.envVars {
				t.Setenv(k, v)
			}

			store := new(mocks.MockSettingsStore)
			store.On("Load").Return(tt.storeSettings, nil)

			if tt.wantSaved != nil {
				store.On("Save", *tt.wantSaved).Return(tt.saveErr)
			} else if tt.saveErr != nil {
				store.On("Save", tt.incoming).Return(tt.saveErr)
			}

			mgr, err := config.NewSettingsManager(store, tt.cfg)
			require.NoError(t, err)

			err = mgr.Update(tt.incoming)

			if tt.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tt.wantErr)
			} else {
				require.NoError(t, err)
				assert.Equal(t, *tt.wantSaved, mgr.Get())
			}

			store.AssertExpectations(t)
		})
	}
}

func TestDefaultWorkingDir(t *testing.T) {
	dir := config.DefaultWorkingDir()
	assert.NotEmpty(t, dir)
	assert.Contains(t, dir, ".agento")
	assert.Contains(t, dir, "work")
}
