package config

import (
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"strings"
)

const defaultModel = "sonnet"

// DefaultWorkingDir returns the default working directory for agent sessions.
// It uses the OS temp directory so it is always resolvable without knowing the
// user's home directory (e.g. /tmp/agento/work on Linux/macOS).
func DefaultWorkingDir() string {
	return filepath.Join(os.TempDir(), "agento", "work") // NOSONAR - intentional temp dir for agent working directory
}

// UserSettings holds persisted user preferences.
type UserSettings struct {
	DefaultWorkingDir      string `json:"default_working_dir"`
	DefaultModel           string `json:"default_model"`
	OnboardingComplete     bool   `json:"onboarding_complete"`
	AppearanceDarkMode     bool   `json:"appearance_dark_mode"`
	AppearanceFontSize     int    `json:"appearance_font_size"`
	AppearanceFontFamily   string `json:"appearance_font_family"`
	NotificationSettings   string `json:"notification_settings"`
	EventBusWorkerPoolSize int    `json:"event_bus_worker_pool_size"`
	PublicURL              string `json:"public_url"`

	// HiddenProjects are Claude Code project paths excluded from every figure
	// Agento reports — the sessions list, the analytics dashboard and the
	// insight cards alike. Paths are the decoded form
	// (/home/me/Projects/agento), matching what the projects endpoint returns.
	HiddenProjects []string `json:"hidden_projects"`

	// IdleGapThresholdMinutes is the largest gap between two transcript events
	// that still counts as continuous work when measuring how long a session
	// actually ran. Zero means "not chosen" and resolves to the built-in
	// default; anything else is validated against the bounds below.
	IdleGapThresholdMinutes int `json:"idle_gap_threshold_minutes"`

	// ClaudeConfigDir is the Claude Code config dir agent runs target unless an
	// agent overrides it — where Claude Code keeps credentials, projects and
	// settings. Empty means the default ($HOME/.claude). Locked by
	// CLAUDE_CONFIG_DIR, because the surrounding environment has already chosen.
	ClaudeConfigDir string `json:"claude_config_dir"`

	// ClaudeConfigDirs are additional config dirs to index. Reading is a set
	// and running is a choice: analytics is retrospective, so a machine with
	// two accounts wants both corpora in every total, while a run
	// authenticates as exactly one account by definition. The default dir and
	// ClaudeConfigDir are always indexed and need not be listed here.
	ClaudeConfigDirs []string `json:"claude_config_dirs"`
}

// Bounds for UserSettings.IdleGapThresholdMinutes, defined here because this
// is the package every layer may import; claudesessions.IdleGapThreshold
// documents what the value means and is the only place it is interpreted.
//
// The default is the figure #238 established from the reference corpus. The
// bounds exist because the setting is a definition of "still working", and
// neither a zero-length sitting nor a multi-day one is a definition anybody
// can read a dashboard against: below a minute, reading a single long reply
// already ends the sitting; past four hours, active duration stops differing
// from the wall-clock span it exists to be different from.
const (
	DefaultIdleGapThresholdMinutes = 10
	MinIdleGapThresholdMinutes     = 1
	MaxIdleGapThresholdMinutes     = 240
)

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

	m.detectLockedFields(cfg)

	if err := m.load(); err != nil {
		return nil, fmt.Errorf("loading settings: %w", err)
	}

	m.applyEnvOverrides(cfg)

	return m, nil
}

// detectLockedFields marks fields that are set via environment variables.
func (m *SettingsManager) detectLockedFields(cfg *AppConfig) {
	if cfg.DefaultModel != "" && os.Getenv("AGENTO_DEFAULT_MODEL") != "" {
		m.locked["default_model"] = "AGENTO_DEFAULT_MODEL"
	}
	if cfg.WorkingDir != "" && os.Getenv("AGENTO_WORKING_DIR") != "" {
		m.locked["default_working_dir"] = "AGENTO_WORKING_DIR"
	}
	if cfg.PublicURL != "" && os.Getenv("AGENTO_PUBLIC_URL") != "" {
		m.locked["public_url"] = "AGENTO_PUBLIC_URL"
	}
	// CLAUDE_CONFIG_DIR is Claude Code's own variable rather than one of ours,
	// so there is no AppConfig field gating it — its presence in the
	// environment is the whole condition. Claude Code has already made the
	// choice for every subprocess we spawn; a stored value that disagreed
	// would be silently ineffective, which is worse than being read-only.
	if ClaudeConfigDirFromEnv() != "" {
		m.locked["claude_config_dir"] = ClaudeConfigDirEnvVar
	}
}

// applyEnvOverrides sets field values from AppConfig for locked fields.
func (m *SettingsManager) applyEnvOverrides(cfg *AppConfig) {
	if _, ok := m.locked["default_model"]; ok {
		m.settings.DefaultModel = cfg.DefaultModel
		m.modelFromEnv = true
	} else if cfg.AnthropicDefaultSonnetModel != "" && !m.modelInFile {
		m.settings.DefaultModel = cfg.AnthropicDefaultSonnetModel
		m.modelFromEnv = true
	}

	if _, ok := m.locked["default_working_dir"]; ok {
		m.settings.DefaultWorkingDir = cfg.WorkingDir
	}

	if _, ok := m.locked["public_url"]; ok {
		m.settings.PublicURL = cfg.PublicURL
	}

	if _, ok := m.locked["claude_config_dir"]; ok {
		m.settings.ClaudeConfigDir = ClaudeConfigDirFromEnv()
	}
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

// validateIdleGapThreshold rejects an out-of-range threshold. Zero is allowed
// and means "not chosen": a client that does not know about the field — the
// settings form for any other tab posts the whole object back — must not be
// read as a request to set it to zero, and every reader resolves zero to the
// default.
func validateIdleGapThreshold(minutes int) error {
	if minutes == 0 {
		return nil
	}
	if minutes < MinIdleGapThresholdMinutes || minutes > MaxIdleGapThresholdMinutes {
		return fmt.Errorf(
			"idle_gap_threshold_minutes must be between %d and %d minutes, got %d",
			MinIdleGapThresholdMinutes, MaxIdleGapThresholdMinutes, minutes)
	}
	return nil
}

// validateClaudeConfigDirs rejects a run dir or an indexed dir that cannot be
// one. A blank run dir is allowed and means "use the default"; blank entries in
// the list are dropped rather than rejected, so a half-filled row in the UI is
// not an error the user has to clear before saving anything else.
//
// Only values the caller is actually *changing* are checked, against current.
// A directory that existed when it was stored can stop existing — an unmounted
// volume, or a CLAUDE_CONFIG_DIR exported in a shell profile that Claude Code
// has not created yet — and validating an unchanged value would then reject
// every save, including saves of unrelated fields, naming a field the user was
// not touching and (when env-locked) cannot even edit. That is also the rule the
// scanner already follows: an unreadable dir is tolerated at scan time.
func validateClaudeConfigDirs(incoming, current UserSettings) error {
	if NormalizeClaudeConfigDir(incoming.ClaudeConfigDir) !=
		NormalizeClaudeConfigDir(current.ClaudeConfigDir) {
		if err := ValidateClaudeConfigDir(incoming.ClaudeConfigDir); err != nil {
			return err
		}
	}

	existing := make(map[string]struct{}, len(current.ClaudeConfigDirs))
	for _, d := range current.ClaudeConfigDirs {
		existing[NormalizeClaudeConfigDir(d)] = struct{}{}
	}
	for _, d := range incoming.ClaudeConfigDirs {
		if strings.TrimSpace(d) == "" {
			continue
		}
		if _, kept := existing[NormalizeClaudeConfigDir(d)]; kept {
			continue
		}
		if err := ValidateClaudeConfigDir(d); err != nil {
			return err
		}
	}
	return nil
}

// normalizeClaudeConfigDirs cleans the stored list so the persisted value is
// the same one every reader compares against.
func normalizeClaudeConfigDirs(dirs []string) []string {
	if len(dirs) == 0 {
		return nil
	}
	out := make([]string, 0, len(dirs))
	seen := make(map[string]struct{}, len(dirs))
	for _, d := range dirs {
		d = NormalizeClaudeConfigDir(d)
		if d == "" {
			continue
		}
		if _, dup := seen[d]; dup {
			continue
		}
		seen[d] = struct{}{}
		out = append(out, d)
	}
	if len(out) == 0 {
		return nil
	}
	return out
}

// Update persists allowed fields, skipping any locked ones. Returns an error if
// the caller attempts to change a locked field.
func (m *SettingsManager) Update(incoming UserSettings) error {
	incoming, err := m.applyLockedFields(incoming)
	if err != nil {
		return err
	}

	if err := validateIdleGapThreshold(incoming.IdleGapThresholdMinutes); err != nil {
		return err
	}
	if err := validateClaudeConfigDirs(incoming, m.settings); err != nil {
		return err
	}

	incoming.ClaudeConfigDir = NormalizeClaudeConfigDir(incoming.ClaudeConfigDir)
	incoming.ClaudeConfigDirs = normalizeClaudeConfigDirs(incoming.ClaudeConfigDirs)

	m.settings = incoming

	if err := m.store.Save(m.settings); err != nil {
		return fmt.Errorf("persisting settings: %w", err)
	}
	return nil
}

// applyLockedFields rejects an attempt to change an env-locked field and pins
// each locked value to what the environment chose.
//
// A blank incoming value is never a conflict: the settings form posts the whole
// object back from every tab, so a client that does not know about a field must
// not be read as asking to clear it.
func (m *SettingsManager) applyLockedFields(incoming UserSettings) (UserSettings, error) {
	type lockedField struct {
		name    string
		current string
		set     func(string)
		// equal compares an incoming value with the current one. Nil means
		// plain string equality.
		equal func(incoming, current string) bool
	}

	fields := []lockedField{
		{"default_model", m.settings.DefaultModel, func(v string) { incoming.DefaultModel = v }, nil},
		{"default_working_dir", m.settings.DefaultWorkingDir,
			func(v string) { incoming.DefaultWorkingDir = v }, nil},
		{"public_url", m.settings.PublicURL, func(v string) { incoming.PublicURL = v }, nil},
		{"claude_config_dir", m.settings.ClaudeConfigDir,
			func(v string) { incoming.ClaudeConfigDir = v },
			// Compared after normalization so "~/.claude" and "$HOME/.claude"
			// are not read as a conflicting change.
			func(in, cur string) bool {
				return NormalizeClaudeConfigDir(in) == NormalizeClaudeConfigDir(cur)
			}},
	}

	values := map[string]string{
		"default_model":       incoming.DefaultModel,
		"default_working_dir": incoming.DefaultWorkingDir,
		"public_url":          incoming.PublicURL,
		"claude_config_dir":   incoming.ClaudeConfigDir,
	}

	for _, f := range fields {
		envVar, locked := m.locked[f.name]
		if !locked {
			continue
		}
		in := values[f.name]
		same := in == f.current
		if f.equal != nil {
			same = f.equal(in, f.current)
		}
		if in != "" && !same {
			return incoming, fmt.Errorf("%s is locked by environment variable %s", f.name, envVar)
		}
		f.set(f.current)
	}
	return incoming, nil
}
