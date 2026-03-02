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

// makeProjectDir creates a temp directory with a .claude/ subdirectory inside.
func makeProjectDir(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(dir, ".claude"), 0o750); err != nil {
		t.Fatal(err)
	}
	return dir
}

func TestAppendSettingsOpts(t *testing.T) {
	projectDir := makeProjectDir(t)
	plainDir := t.TempDir() // no .claude/ inside

	tests := []struct {
		name             string
		settingsFilePath string
		workingDir       string
		wantSources      []claude.SettingSource
		wantCWD          string
	}{
		{
			name:             "no settings file, no working dir — isolation mode",
			settingsFilePath: "",
			workingDir:       "",
			wantSources:      nil,
			wantCWD:          "",
		},
		{
			name:             "with settings file, no working dir — user source only",
			settingsFilePath: "/home/user/.claude/settings_myprofile.json",
			workingDir:       "",
			wantSources:      []claude.SettingSource{claude.SettingSourceUser},
			wantCWD:          "",
		},
		{
			name:             "no settings file, plain working dir — CWD only, no sources",
			settingsFilePath: "",
			workingDir:       plainDir,
			wantSources:      nil,
			wantCWD:          plainDir,
		},
		{
			name:             "no settings file, project working dir — project source",
			settingsFilePath: "",
			workingDir:       projectDir,
			wantSources:      []claude.SettingSource{claude.SettingSourceProject},
			wantCWD:          projectDir,
		},
		{
			name:             "with settings file and project working dir — both sources",
			settingsFilePath: "/home/user/.claude/settings_myprofile.json",
			workingDir:       projectDir,
			wantSources:      []claude.SettingSource{claude.SettingSourceProject, claude.SettingSourceUser},
			wantCWD:          projectDir,
		},
		{
			name:             "with settings file and plain working dir — user source only",
			settingsFilePath: "/home/user/.claude/settings_myprofile.json",
			workingDir:       plainDir,
			wantSources:      []claude.SettingSource{claude.SettingSourceUser},
			wantCWD:          plainDir,
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

			if o.CWD != tc.wantCWD {
				t.Errorf("CWD = %q, want %q", o.CWD, tc.wantCWD)
			}
		})
	}
}

func TestBuildSDKOptions_ProjectDirWithSettingsProfile(t *testing.T) {
	projectDir := makeProjectDir(t)

	agentCfg := &config.AgentConfig{
		Model:    "claude-sonnet-4-6",
		Thinking: "adaptive",
	}
	opts := RunOptions{
		WorkingDir:       projectDir,
		SettingsFilePath: "/home/user/.claude/settings_default.json",
	}

	sdkOpts := buildSDKOptions(context.Background(), agentCfg, opts, "You are helpful.")
	o := applyOpts(sdkOpts)

	if o.CWD != projectDir {
		t.Errorf("CWD = %q, want %q", o.CWD, projectDir)
	}

	hasProject := false
	hasUser := false
	for _, s := range o.SettingSources {
		if s == claude.SettingSourceProject {
			hasProject = true
		}
		if s == claude.SettingSourceUser {
			hasUser = true
		}
	}
	if !hasProject {
		t.Error("SettingSources missing SettingSourceProject for project dir")
	}
	if !hasUser {
		t.Error("SettingSources missing SettingSourceUser when settings file is set")
	}

	if o.Model != "claude-sonnet-4-6" {
		t.Errorf("Model = %q, want %q", o.Model, "claude-sonnet-4-6")
	}
	if o.SystemPrompt != "You are helpful." {
		t.Errorf("SystemPrompt = %q, want %q", o.SystemPrompt, "You are helpful.")
	}
}

func TestBuildSDKOptions_PlainDirNoSettingsProfile(t *testing.T) {
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

	if o.CWD != plainDir {
		t.Errorf("CWD = %q, want %q", o.CWD, plainDir)
	}

	// No .claude/ in the dir, so project source should NOT be included.
	for _, s := range o.SettingSources {
		if s == claude.SettingSourceProject {
			t.Error("SettingSources should NOT include SettingSourceProject for plain dir")
		}
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

	if o.CWD != "" {
		t.Errorf("CWD = %q, want empty string", o.CWD)
	}
	if len(o.SettingSources) != 0 {
		t.Errorf("SettingSources = %v, want empty (isolation mode)", o.SettingSources)
	}
}

func TestBuildSDKOptions_SessionIDResume(t *testing.T) {
	projectDir := makeProjectDir(t)

	agentCfg := &config.AgentConfig{
		Model: "claude-sonnet-4-6",
	}
	opts := RunOptions{
		SessionID:  "sess-abc-123",
		WorkingDir: projectDir,
	}

	sdkOpts := buildSDKOptions(context.Background(), agentCfg, opts, "")
	o := applyOpts(sdkOpts)

	if o.SessionID != "sess-abc-123" {
		t.Errorf("SessionID = %q, want %q", o.SessionID, "sess-abc-123")
	}
	if o.CWD != projectDir {
		t.Errorf("CWD = %q, want %q", o.CWD, projectDir)
	}
}

func TestIsProjectDir(t *testing.T) {
	projectDir := makeProjectDir(t)
	plainDir := t.TempDir()

	if !isProjectDir(projectDir) {
		t.Errorf("isProjectDir(%q) = false, want true", projectDir)
	}
	if isProjectDir(plainDir) {
		t.Errorf("isProjectDir(%q) = true, want false", plainDir)
	}
	if isProjectDir("/nonexistent/path") {
		t.Error("isProjectDir for nonexistent path should return false")
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
