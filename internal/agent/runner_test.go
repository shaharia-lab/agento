package agent

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/config"
)

// applyOpts applies a slice of claude.Option to a fresh Options struct and
// returns the result. This lets us inspect what buildSDKOptions produces.
func applyOpts(opts []claude.Option) claude.Options {
	var o claude.Options
	for _, fn := range opts {
		fn(&o)
	}
	return o
}

// newConfigDir creates a Claude config dir, optionally containing a
// settings.json, and returns its path.
func newConfigDir(t *testing.T, withSettings bool) string {
	t.Helper()
	dir := t.TempDir()
	if withSettings {
		if err := os.WriteFile(
			filepath.Join(dir, "settings.json"), []byte(`{}`), 0o600,
		); err != nil {
			t.Fatalf("writing settings.json: %v", err)
		}
	}
	return dir
}

func TestAppendSettingsOpts(t *testing.T) {
	anyDir := t.TempDir()
	withSettings := newConfigDir(t, true)
	withoutSettings := newConfigDir(t, false)

	tests := []struct {
		name         string
		runDir       string
		agentDir     string
		workingDir   string
		wantSources  []claude.SettingSource
		wantSettings string
		wantCWD      string
		wantEnvDir   string
	}{
		{
			// The default dir has no settings.json under a temp HOME, so no
			// --settings is passed and no env override is needed.
			name:        "default config dir, no working dir — isolation mode",
			wantSources: nil,
			wantCWD:     "",
		},
		{
			name:         "run dir has settings.json — it is passed",
			runDir:       withSettings,
			wantSettings: filepath.Join(withSettings, "settings.json"),
			wantEnvDir:   withSettings,
		},
		{
			// The regression this issue exists for: a config dir Claude Code
			// has never written must not be handed another dir's settings.
			name:         "run dir has no settings.json — none is passed",
			runDir:       withoutSettings,
			wantSettings: "",
			wantEnvDir:   withoutSettings,
		},
		{
			name:        "working dir set — project source and CWD",
			workingDir:  anyDir,
			wantSources: []claude.SettingSource{claude.SettingSourceProject},
			wantCWD:     anyDir,
		},
		{
			// A per-agent override beats the global run dir, which is what
			// lets a work agent and a personal agent be live at once.
			name:         "agent overrides the run dir",
			runDir:       withoutSettings,
			agentDir:     withSettings,
			wantSettings: filepath.Join(withSettings, "settings.json"),
			wantEnvDir:   withSettings,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Setenv("HOME", t.TempDir())
			t.Setenv(config.ClaudeConfigDirEnvVar, "")
			config.ApplyClaudeDirs(tc.runDir, nil)

			var agentCfg *config.AgentConfig
			if tc.agentDir != "" {
				agentCfg = &config.AgentConfig{ClaudeConfigDir: tc.agentDir}
			}
			opts := RunOptions{WorkingDir: tc.workingDir}
			o := applyOpts(appendSettingsOpts(nil, opts, agentCfg))

			if len(o.SettingSources) != len(tc.wantSources) {
				t.Fatalf("SettingSources length = %d, want %d; got %v",
					len(o.SettingSources), len(tc.wantSources), o.SettingSources)
			}
			for i, want := range tc.wantSources {
				if o.SettingSources[i] != want {
					t.Errorf("SettingSources[%d] = %q, want %q", i, o.SettingSources[i], want)
				}
			}
			if o.Settings != tc.wantSettings {
				t.Errorf("Settings = %q, want %q", o.Settings, tc.wantSettings)
			}
			if o.CWD != tc.wantCWD {
				t.Errorf("CWD = %q, want %q", o.CWD, tc.wantCWD)
			}
			if got := o.Env[config.ClaudeConfigDirEnvVar]; got != tc.wantEnvDir {
				t.Errorf("Env[%s] = %q, want %q", config.ClaudeConfigDirEnvVar, got, tc.wantEnvDir)
			}
		})
	}
}

func TestBuildSDKOptions_WorkingDirWithSettingsProfile(t *testing.T) {
	workDir := t.TempDir()

	agentCfg := &config.AgentConfig{
		Model:    "claude-sonnet-4-6",
		Thinking: "adaptive",
	}
	configDir := newConfigDir(t, true)
	t.Setenv("HOME", t.TempDir())
	t.Setenv(config.ClaudeConfigDirEnvVar, "")
	config.ApplyClaudeDirs(configDir, nil)

	opts := RunOptions{WorkingDir: workDir}

	sdkOpts := buildSDKOptions(context.Background(), agentCfg, opts, "You are helpful.")
	o := applyOpts(sdkOpts)

	hasProject := false
	for _, s := range o.SettingSources {
		if s == claude.SettingSourceProject {
			hasProject = true
		}
	}
	if !hasProject {
		t.Error("SettingSources missing SettingSourceProject when working dir is set")
	}
	if want := filepath.Join(configDir, "settings.json"); o.Settings != want {
		t.Errorf("Settings = %q, want %q", o.Settings, want)
	}

	if o.CWD != workDir {
		t.Errorf("CWD = %q, want %q", o.CWD, workDir)
	}
	if o.Model != "claude-sonnet-4-6" {
		t.Errorf("Model = %q, want %q", o.Model, "claude-sonnet-4-6")
	}
	if o.SystemPrompt != "You are helpful." {
		t.Errorf("SystemPrompt = %q, want %q", o.SystemPrompt, "You are helpful.")
	}
}

func TestBuildSDKOptions_WorkingDirIncludesProjectSource(t *testing.T) {
	plainDir := t.TempDir()

	agentCfg := &config.AgentConfig{
		Model:    "claude-sonnet-4-6",
		Thinking: "disabled",
	}
	opts := RunOptions{
		WorkingDir: plainDir,
	}

	sdkOpts := buildSDKOptions(context.Background(), agentCfg, opts, "")
	o := applyOpts(sdkOpts)

	// Any working dir should include project source so the CLI can
	// discover .claude/skills/ if present.
	hasProject := false
	for _, s := range o.SettingSources {
		if s == claude.SettingSourceProject {
			hasProject = true
		}
	}
	if !hasProject {
		t.Error("SettingSources should include SettingSourceProject when working dir is set")
	}
}

func TestBuildSDKOptions_NoCWDWhenWorkingDirEmpty(t *testing.T) {
	agentCfg := &config.AgentConfig{
		Model:    "claude-sonnet-4-6",
		Thinking: "disabled",
	}
	opts := RunOptions{}

	sdkOpts := buildSDKOptions(context.Background(), agentCfg, opts, "")
	o := applyOpts(sdkOpts)

	if len(o.SettingSources) != 0 {
		t.Errorf("SettingSources = %v, want empty (isolation mode)", o.SettingSources)
	}
	if o.CWD != "" {
		t.Errorf("CWD = %q, want empty when no working dir", o.CWD)
	}
}

func TestBuildSDKOptions_SessionIDResume(t *testing.T) {
	agentCfg := &config.AgentConfig{
		Model: "claude-sonnet-4-6",
	}
	opts := RunOptions{
		ResumeSessionID: "sess-abc-123",
		WorkingDir:      t.TempDir(),
	}

	sdkOpts := buildSDKOptions(context.Background(), agentCfg, opts, "")
	o := applyOpts(sdkOpts)

	if o.ResumeSessionID != "sess-abc-123" {
		t.Errorf("ResumeSessionID = %q, want %q", o.ResumeSessionID, "sess-abc-123")
	}
}

func TestBuildSDKOptions_CustomSessionID(t *testing.T) {
	agentCfg := &config.AgentConfig{
		Model: "claude-sonnet-4-6",
	}
	opts := RunOptions{
		CustomSessionID: "chat-uuid-xyz",
		WorkingDir:      t.TempDir(),
	}

	sdkOpts := buildSDKOptions(context.Background(), agentCfg, opts, "")
	o := applyOpts(sdkOpts)

	if o.CustomSessionID != "chat-uuid-xyz" {
		t.Errorf("CustomSessionID = %q, want %q", o.CustomSessionID, "chat-uuid-xyz")
	}
}

func TestInterpolate(t *testing.T) {
	tests := []struct {
		name     string
		template string
		vars     map[string]string
		wantErr  bool
		check    func(t *testing.T, result string)
	}{
		{
			name:     "no variables",
			template: "Hello world",
			vars:     nil,
			check:    func(t *testing.T, r string) { assertEqual(t, r, "Hello world") },
		},
		{
			name:     "custom variable",
			template: "Hello {{name}}!",
			vars:     map[string]string{"name": "Alice"},
			check:    func(t *testing.T, r string) { assertEqual(t, r, "Hello Alice!") },
		},
		{
			name:     "builtin current_date",
			template: "Today is {{current_date}}",
			vars:     nil,
			check: func(t *testing.T, r string) {
				if len(r) < len("Today is 2024-01-01") {
					t.Errorf("expected date interpolation, got %q", r)
				}
			},
		},
		{
			name:     "missing variable",
			template: "Hello {{unknown}}",
			vars:     nil,
			wantErr:  true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			result, err := Interpolate(tc.template, tc.vars)
			if tc.wantErr {
				if err == nil {
					t.Fatal("expected error, got nil")
				}
				if _, ok := err.(*MissingVariableError); !ok {
					t.Errorf("expected MissingVariableError, got %T", err)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if tc.check != nil {
				tc.check(t, result)
			}
		})
	}
}

func assertEqual(t *testing.T, got, want string) {
	t.Helper()
	if got != want {
		t.Errorf("got %q, want %q", got, want)
	}
}
