package config

import (
	"os"
	"path/filepath"
	"testing"
)

// resetClaudeDirs points the resolvers at a temp HOME with no configuration,
// restoring the empty state afterwards so tests cannot leak into each other.
func resetClaudeDirs(t *testing.T) string {
	t.Helper()
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv(ClaudeConfigDirEnvVar, "")
	ApplyClaudeDirs("", nil)
	t.Cleanup(func() { ApplyClaudeDirs("", nil) })
	return home
}

func TestClaudeRunConfigDir_Precedence(t *testing.T) {
	home := resetClaudeDirs(t)
	def := filepath.Join(home, ".claude")

	if got := ClaudeRunConfigDir(); got != def {
		t.Errorf("unconfigured = %q, want the default %q", got, def)
	}

	ApplyClaudeDirs("/opt/personal", nil)
	if got := ClaudeRunConfigDir(); got != "/opt/personal" {
		t.Errorf("configured = %q, want /opt/personal", got)
	}

	// The environment has already chosen for every subprocess we spawn, so it
	// wins over a stored value that would otherwise be silently ineffective.
	t.Setenv(ClaudeConfigDirEnvVar, "/opt/from-env")
	if got := ClaudeRunConfigDir(); got != "/opt/from-env" {
		t.Errorf("env set = %q, want /opt/from-env", got)
	}
}

func TestResolveAgentClaudeDir_AgentWins(t *testing.T) {
	resetClaudeDirs(t)
	ApplyClaudeDirs("/opt/global", nil)

	if got := ResolveAgentClaudeDir(nil); got != "/opt/global" {
		t.Errorf("nil agent = %q, want the global default", got)
	}
	if got := ResolveAgentClaudeDir(&AgentConfig{}); got != "/opt/global" {
		t.Errorf("no override = %q, want the global default", got)
	}
	if got := ResolveAgentClaudeDir(&AgentConfig{ClaudeConfigDir: "/opt/agent"}); got != "/opt/agent" {
		t.Errorf("override = %q, want /opt/agent", got)
	}
}

func TestClaudeConfigDirs_OrderAndDedupe(t *testing.T) {
	home := resetClaudeDirs(t)
	def := filepath.Join(home, ".claude")

	// The run dir and one extra duplicate the default; order must be stable
	// because it decides which dir wins a session present in two of them.
	ApplyClaudeDirs(def, []string{"/opt/b", def, "/opt/a", "/opt/b"})

	got := ClaudeConfigDirs()
	want := []string{def, "/opt/b", "/opt/a"}
	if len(got) != len(want) {
		t.Fatalf("dirs = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("dirs = %v, want %v", got, want)
		}
	}
}

func TestClaudeConfigDirs_DefaultAlwaysIndexed(t *testing.T) {
	home := resetClaudeDirs(t)
	def := filepath.Join(home, ".claude")
	ApplyClaudeDirs("/opt/personal", nil)

	dirs := ClaudeConfigDirs()
	if len(dirs) == 0 || dirs[0] != def {
		t.Errorf("dirs = %v, want the default %q first", dirs, def)
	}
}

func TestNormalizeClaudeConfigDir(t *testing.T) {
	home := resetClaudeDirs(t)

	cases := map[string]string{
		"":          "",
		"   ":       "",
		"~":         home,
		"~/.claude": filepath.Join(home, ".claude"),
		"/opt/x/":   "/opt/x",
		"/opt/./x":  "/opt/x",
		" /opt/x ":  "/opt/x",
		"/a/b":      "/a/b",
	}
	for in, want := range cases {
		if got := NormalizeClaudeConfigDir(in); got != want {
			t.Errorf("Normalize(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestValidateClaudeConfigDir(t *testing.T) {
	dir := t.TempDir()
	file := filepath.Join(dir, "a-file")
	if err := os.WriteFile(file, []byte("x"), 0o600); err != nil {
		t.Fatalf("writing file: %v", err)
	}

	if err := ValidateClaudeConfigDir(""); err != nil {
		t.Errorf("blank should mean 'use the default', got %v", err)
	}
	if err := ValidateClaudeConfigDir(dir); err != nil {
		t.Errorf("existing dir: %v", err)
	}
	// A typo must fail on save rather than surfacing as an empty sessions list.
	if err := ValidateClaudeConfigDir(filepath.Join(dir, "nope")); err == nil {
		t.Error("missing dir should be rejected")
	}
	if err := ValidateClaudeConfigDir(file); err == nil {
		t.Error("a file should be rejected")
	}
	if err := ValidateClaudeConfigDir("relative/path"); err == nil {
		t.Error("a relative path should be rejected")
	}
}

func TestIsIndexedClaudeDir(t *testing.T) {
	home := resetClaudeDirs(t)
	ApplyClaudeDirs("", []string{"/opt/second"})

	if !IsIndexedClaudeDir(filepath.Join(home, ".claude")) {
		t.Error("default dir should be indexed")
	}
	if !IsIndexedClaudeDir("/opt/second") {
		t.Error("configured extra should be indexed")
	}
	if IsIndexedClaudeDir("/opt/removed") {
		t.Error("unconfigured dir should not be indexed")
	}
	// Rows written before the column existed carry no dir and must stay visible.
	if !IsIndexedClaudeDir("") {
		t.Error("blank dir should be admitted")
	}
}

func TestDiscoverCandidateClaudeDirs(t *testing.T) {
	home := resetClaudeDirs(t)

	mkdirs := func(rel ...string) {
		t.Helper()
		for _, r := range rel {
			if err := os.MkdirAll(filepath.Join(home, r), 0o750); err != nil {
				t.Fatalf("mkdir %s: %v", r, err)
			}
		}
	}
	// A real second account, a lookalike with no projects/, and an unrelated dir.
	mkdirs(".claude/projects", ".claude-personal/projects", ".claude-backup", "notclaude/projects")

	got := DiscoverCandidateClaudeDirs()
	if len(got) != 1 || got[0] != filepath.Join(home, ".claude-personal") {
		t.Fatalf("candidates = %v, want only .claude-personal", got)
	}

	// Once configured it is no longer a suggestion.
	ApplyClaudeDirs("", []string{filepath.Join(home, ".claude-personal")})
	if got := DiscoverCandidateClaudeDirs(); len(got) != 0 {
		t.Errorf("candidates after configuring = %v, want none", got)
	}
}

func TestSettingsManager_ClaudeConfigDirLockedByEnv(t *testing.T) {
	home := resetClaudeDirs(t)
	envDir := filepath.Join(home, "env-chosen")
	if err := os.MkdirAll(envDir, 0o750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	t.Setenv(ClaudeConfigDirEnvVar, envDir)

	m, err := NewSettingsManager(&stubSettingsStore{}, &AppConfig{})
	if err != nil {
		t.Fatalf("manager: %v", err)
	}

	if got := m.Locked()["claude_config_dir"]; got != ClaudeConfigDirEnvVar {
		t.Errorf("locked[claude_config_dir] = %q, want %q", got, ClaudeConfigDirEnvVar)
	}
	if got := m.Get().ClaudeConfigDir; got != envDir {
		t.Errorf("value = %q, want the env value %q", got, envDir)
	}

	// This repo has no EnvLockedError and no 409 in the settings path; a
	// locked-field conflict is a plain error the handler maps to 400.
	other := t.TempDir()
	if err := m.Update(UserSettings{ClaudeConfigDir: other}); err == nil {
		t.Fatal("expected a conflicting update to be rejected")
	}
	if got := m.Get().ClaudeConfigDir; got != envDir {
		t.Errorf("value after rejected update = %q, want %q unchanged", got, envDir)
	}

	// Posting the same value back — which the settings form does for every
	// other tab — must not be read as a conflict.
	if err := m.Update(UserSettings{ClaudeConfigDir: envDir}); err != nil {
		t.Errorf("re-posting the env value: %v", err)
	}
}

func TestSettingsManager_RejectsMissingClaudeConfigDir(t *testing.T) {
	resetClaudeDirs(t)
	m, err := NewSettingsManager(&stubSettingsStore{}, &AppConfig{})
	if err != nil {
		t.Fatalf("manager: %v", err)
	}
	if err := m.Update(UserSettings{ClaudeConfigDir: "/definitely/not/here"}); err == nil {
		t.Error("a nonexistent run dir should be rejected at save time")
	}
	if err := m.Update(UserSettings{ClaudeConfigDirs: []string{"/definitely/not/here"}}); err == nil {
		t.Error("a nonexistent indexed dir should be rejected at save time")
	}
	// A blank entry is dropped, not rejected: a half-filled row in the UI is
	// not an error the user must clear before saving anything else.
	if err := m.Update(UserSettings{ClaudeConfigDirs: []string{"", "  "}}); err != nil {
		t.Errorf("blank entries should be dropped, got %v", err)
	}
	if got := m.Get().ClaudeConfigDirs; len(got) != 0 {
		t.Errorf("dirs = %v, want blanks dropped", got)
	}
}

// stubSettingsStore is an in-memory SettingsStore for the manager tests.
type stubSettingsStore struct{ saved UserSettings }

func (s *stubSettingsStore) Load() (UserSettings, error) { return s.saved, nil }
func (s *stubSettingsStore) Save(v UserSettings) error   { s.saved = v; return nil }

// A relative value has two different meanings at once — the server stats it
// against its own working directory, the subprocess resolves --settings against
// its own — so no resolver may return one. The agent-side guard alone was not
// enough: a relative global run dir defeated it via the fallback.
func TestClaudeDirs_RelativePathsAreNeverResolved(t *testing.T) {
	home := resetClaudeDirs(t)
	def := filepath.Join(home, ".claude")

	ApplyClaudeDirs("relative/run", []string{"also/relative"})
	if got := ClaudeRunConfigDir(); got != def {
		t.Errorf("relative run dir = %q, want the default %q", got, def)
	}
	if got := ClaudeConfigDirs(); len(got) != 1 || got[0] != def {
		t.Errorf("indexed dirs = %v, want only the default", got)
	}

	// A relative override must not be honored, and must not be rescued by a
	// relative global either.
	agent := &AgentConfig{ClaudeConfigDir: "relative/agent"}
	if got := ResolveAgentClaudeDir(agent); got != def {
		t.Errorf("relative agent override = %q, want the default %q", got, def)
	}

	// Same for the environment, which no service validation ever sees.
	t.Setenv(ClaudeConfigDirEnvVar, "relative/env")
	if got := ClaudeRunConfigDir(); got != def {
		t.Errorf("relative env value = %q, want the default %q", got, def)
	}
}

// The reported defect: a CLAUDE_CONFIG_DIR naming a directory Claude Code has
// not created yet must not make every settings save fail — including saves of
// unrelated fields, on a field the UI renders read-only.
func TestSettingsManager_EnvLockedMissingDirDoesNotBlockSaves(t *testing.T) {
	home := resetClaudeDirs(t)
	missing := filepath.Join(home, "not-created-yet")
	t.Setenv(ClaudeConfigDirEnvVar, missing)

	m, err := NewSettingsManager(&stubSettingsStore{}, &AppConfig{})
	if err != nil {
		t.Fatalf("manager: %v", err)
	}
	if got := m.Get().ClaudeConfigDir; got != missing {
		t.Fatalf("value = %q, want the env value even though it does not exist", got)
	}

	// A save of an entirely unrelated field must go through.
	if err := m.Update(UserSettings{AppearanceDarkMode: true}); err != nil {
		t.Fatalf("unrelated save was blocked by a nonexistent locked dir: %v", err)
	}
	if !m.Get().AppearanceDarkMode {
		t.Error("the unrelated field was not persisted")
	}
	// A genuinely new bad dir is still rejected.
	if err := m.Update(UserSettings{ClaudeConfigDirs: []string{"/definitely/not/here"}}); err == nil {
		t.Error("a new nonexistent indexed dir should still be rejected")
	}
}
