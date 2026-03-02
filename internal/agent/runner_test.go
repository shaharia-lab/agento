package agent

import (
	"context"
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

func TestAppendSettingsOpts(t *testing.T) {
	anyDir := t.TempDir()

	tests := []struct {
		name             string
		settingsFilePath string
		workingDir       string
		wantSources      []claude.SettingSource
		wantSettings     string
		wantCWD          string
	}{
		{
			name:             "no settings file, no working dir — isolation mode",
			settingsFilePath: "",
			workingDir:       "",
			wantSources:      nil,
			wantSettings:     "",
			wantCWD:          "",
		},
		{
			name:             "with settings file, no working dir — WithSettings used",
			settingsFilePath: "/home/user/.claude/settings_myprofile.json",
			workingDir:       "",
			wantSources:      nil,
			wantSettings:     "/home/user/.claude/settings_myprofile.json",
			wantCWD:          "",
		},
		{
			name:             "working dir set — project source and CWD",
			settingsFilePath: "",
			workingDir:       anyDir,
			wantSources:      []claude.SettingSource{claude.SettingSourceProject},
			wantSettings:     "",
			wantCWD:          anyDir,
		},
		{
			name:             "both settings file and working dir — project source + WithSettings",
			settingsFilePath: "/home/user/.claude/settings_myprofile.json",
			workingDir:       anyDir,
			wantSources:      []claude.SettingSource{claude.SettingSourceProject},
			wantSettings:     "/home/user/.claude/settings_myprofile.json",
			wantCWD:          anyDir,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			opts := RunOptions{
				SettingsFilePath: tc.settingsFilePath,
				WorkingDir:       tc.workingDir,
			}
			sdkOpts := appendSettingsOpts(nil, opts, nil)
			o := applyOpts(sdkOpts)

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
		})
	}
}

func TestBuildSDKOptions_WorkingDirWithSettingsProfile(t *testing.T) {
	workDir := t.TempDir()

	agentCfg := &config.AgentConfig{
		Model:    "claude-sonnet-4-6",
		Thinking: "adaptive",
	}
	opts := RunOptions{
		WorkingDir:       workDir,
		SettingsFilePath: "/home/user/.claude/settings_default.json",
	}

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
	if o.Settings != "/home/user/.claude/settings_default.json" {
		t.Errorf("Settings = %q, want settings file path", o.Settings)
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
		SessionID:  "sess-abc-123",
		WorkingDir: t.TempDir(),
	}

	sdkOpts := buildSDKOptions(context.Background(), agentCfg, opts, "")
	o := applyOpts(sdkOpts)

	if o.SessionID != "sess-abc-123" {
		t.Errorf("SessionID = %q, want %q", o.SessionID, "sess-abc-123")
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
