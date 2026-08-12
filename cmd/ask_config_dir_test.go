package cmd

import (
	"log/slog"
	"os"
	"path/filepath"
	"testing"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/storage"
)

// storeSettings writes user settings to a fresh database and returns its path.
func storeSettings(t *testing.T, s config.UserSettings) string {
	t.Helper()
	dbPath := filepath.Join(t.TempDir(), "agento.db")
	db, _, err := storage.NewSQLiteDB(dbPath, slog.Default())
	if err != nil {
		t.Fatalf("creating db: %v", err)
	}
	if err := storage.NewSQLiteSettingsStore(db).Save(s); err != nil {
		t.Fatalf("saving settings: %v", err)
	}
	if err := db.Close(); err != nil {
		t.Fatalf("closing db: %v", err)
	}
	return dbPath
}

// The point of #244: a config dir chosen in the UI must reach the CLI, or a run
// authenticates as the wrong account and writes into the wrong corpus.
func TestApplyStoredClaudeDirs_Precedence(t *testing.T) {
	home := t.TempDir()
	stored := filepath.Join(home, "stored")
	envDir := filepath.Join(home, "from-env")
	agentDir := filepath.Join(home, "from-agent")

	dbPath := storeSettings(t, config.UserSettings{ClaudeConfigDir: stored})

	tests := []struct {
		name  string
		env   string
		agent *config.AgentConfig
		want  string
	}{
		{
			name: "stored value is used when nothing overrides it",
			want: stored,
		},
		{
			// ClaudeRunConfigDir's documented order: the environment has
			// already chosen for every subprocess we spawn.
			name: "CLAUDE_CONFIG_DIR wins over the stored value",
			env:  envDir,
			want: envDir,
		},
		{
			name:  "a per-agent override wins over both",
			env:   envDir,
			agent: &config.AgentConfig{ClaudeConfigDir: agentDir},
			want:  agentDir,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Setenv("HOME", home)
			t.Setenv(config.ClaudeConfigDirEnvVar, tc.env)
			config.ApplyClaudeDirs("", nil)
			t.Cleanup(func() { config.ApplyClaudeDirs("", nil) })

			applyStoredClaudeDirs(dbPath)

			if got := config.ResolveAgentClaudeDir(tc.agent); got != tc.want {
				t.Errorf("resolved = %q, want %q", got, tc.want)
			}
		})
	}
}

// A database that cannot be read is not a reason to refuse to answer a
// question: the resolver's own fallback was the behavior before this existed.
func TestApplyStoredClaudeDirs_UnreadableDatabaseDegrades(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv(config.ClaudeConfigDirEnvVar, "")
	config.ApplyClaudeDirs("", nil)
	t.Cleanup(func() { config.ApplyClaudeDirs("", nil) })

	def := filepath.Join(home, ".claude")

	// A path that does not exist. This is the ordinary state for someone who
	// has only ever used the CLI, so it must also be silent — asserted by the
	// stray-file check below rather than by capturing stderr.
	missing := filepath.Join(home, "nope", "agento.db")
	applyStoredClaudeDirs(missing)
	if got := config.ClaudeRunConfigDir(); got != def {
		t.Errorf("after a missing database, resolved = %q, want the default %q", got, def)
	}
	if _, err := os.Stat(filepath.Dir(missing)); err == nil {
		t.Error("reading settings created a directory for a database that does not exist")
	}

	// A file that exists but is not a database.
	corrupt := filepath.Join(home, "corrupt.db")
	if err := os.WriteFile(corrupt, []byte("this is not sqlite"), 0o600); err != nil {
		t.Fatalf("writing corrupt file: %v", err)
	}
	applyStoredClaudeDirs(corrupt)
	if got := config.ClaudeRunConfigDir(); got != def {
		t.Errorf("after a corrupt database, resolved = %q, want the default %q", got, def)
	}
}
