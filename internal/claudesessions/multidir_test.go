package claudesessions

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/config"
)

// writeSessionIn writes one transcript into <configDir>/projects/<project>/.
func writeSessionIn(t *testing.T, configDir, project, sessionID string, at time.Time) string {
	t.Helper()
	projectDir := filepath.Join(configDir, "projects", project)
	if err := os.MkdirAll(projectDir, 0o750); err != nil {
		t.Fatalf("creating project dir: %v", err)
	}
	writeJSONL(t, projectDir, sessionID, at)
	return filepath.Join(projectDir, sessionID+jsonlExt)
}

// useConfigDirs points the resolvers at a temp HOME plus the given extra dirs,
// and restores the empty configuration afterwards.
func useConfigDirs(t *testing.T, home string, extra ...string) {
	t.Helper()
	t.Setenv("HOME", home)
	t.Setenv(config.ClaudeConfigDirEnvVar, "")
	config.ApplyClaudeDirs("", extra)
	t.Cleanup(func() { config.ApplyClaudeDirs("", nil) })
}

func TestIncrementalScan_IndexesEveryConfiguredDir(t *testing.T) {
	home := t.TempDir()
	second := filepath.Join(t.TempDir(), ".claude-personal")
	at := time.Date(2026, 8, 1, 9, 0, 0, 0, time.UTC)

	writeSessionIn(t, filepath.Join(home, ".claude"), "-home-dev-work", "work-1", at)
	writeSessionIn(t, second, "-home-dev-personal", "personal-1", at.Add(time.Minute))

	useConfigDirs(t, home, second)

	c := newScanCache(t)
	sessions, err := IncrementalScan(c.db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}

	if len(sessions) != 2 {
		t.Fatalf("expected both dirs indexed, got %d sessions: %+v", len(sessions), sessions)
	}
	byID := map[string]string{}
	for _, s := range sessions {
		byID[s.SessionID] = s.ConfigDir
	}
	if got := byID["work-1"]; got != filepath.Join(home, ".claude") {
		t.Errorf("work-1 config_dir = %q, want the default dir", got)
	}
	if got := byID["personal-1"]; got != second {
		t.Errorf("personal-1 config_dir = %q, want %q", got, second)
	}
}

// A second account is usually set up by copying the first config dir, which
// duplicates every session id under the same project path. Indexing both
// copies would double that corpus's tokens and cost in every total, and leave
// the losing file permanently classified as an insert.
func TestIncrementalScan_DuplicatedSessionIsIndexedOnce(t *testing.T) {
	home := t.TempDir()
	copyDir := filepath.Join(t.TempDir(), ".claude-copy")
	at := time.Date(2026, 8, 1, 9, 0, 0, 0, time.UTC)

	const project, sessionID = "-home-dev-repo", "shared-1"
	writeSessionIn(t, filepath.Join(home, ".claude"), project, sessionID, at)
	writeSessionIn(t, copyDir, project, sessionID, at)

	useConfigDirs(t, home, copyDir)

	c := newScanCache(t)
	sessions, err := IncrementalScan(c.db, testLogger)
	if err != nil {
		t.Fatalf("scan: %v", err)
	}
	if len(sessions) != 1 {
		t.Fatalf("expected the duplicated session once, got %d", len(sessions))
	}
	// The default dir is walked first, so it wins the claim.
	if want := filepath.Join(home, ".claude"); sessions[0].ConfigDir != want {
		t.Errorf("config_dir = %q, want the first-walked dir %q", sessions[0].ConfigDir, want)
	}

	// A second scan must be stable: the losing path must not oscillate between
	// insert and delete.
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("rescan: %v", err)
	}
	if got := countRows(t, c, "claude_session_cache"); got != 1 {
		t.Errorf("rows after rescan = %d, want 1", got)
	}
}

// The top risk of the union: a dir that cannot be listed must not have its
// sessions read as deleted. An unplugged drive would otherwise wipe a whole
// account's corpus, taking custom_title and is_favorite with it.
func TestIncrementalScan_UnreadableDirPreservesItsRows(t *testing.T) {
	home := t.TempDir()
	second := filepath.Join(t.TempDir(), ".claude-personal")
	at := time.Date(2026, 8, 1, 9, 0, 0, 0, time.UTC)

	writeSessionIn(t, filepath.Join(home, ".claude"), "-home-dev-work", "work-1", at)
	writeSessionIn(t, second, "-home-dev-personal", "personal-1", at)

	useConfigDirs(t, home, second)

	c := newScanCache(t)
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("first scan: %v", err)
	}
	if got := countRows(t, c, "claude_session_cache"); got != 2 {
		t.Fatalf("expected 2 rows after first scan, got %d", got)
	}

	// Make the second dir vanish, as an unmounted volume would.
	if err := os.RemoveAll(second); err != nil {
		t.Fatalf("removing dir: %v", err)
	}

	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("second scan: %v", err)
	}
	if got := countRows(t, c, "claude_session_cache"); got != 2 {
		t.Errorf("rows after the dir became unreadable = %d, want both preserved", got)
	}
}

// A config dir that exists but has never run a session is a different case: it
// walked fine and contributed nothing, so its (absent) rows are reconcilable.
func TestIncrementalScan_PresentDirWithoutProjectsReconciles(t *testing.T) {
	home := t.TempDir()
	empty := filepath.Join(t.TempDir(), ".claude-empty")
	if err := os.MkdirAll(empty, 0o750); err != nil {
		t.Fatalf("creating dir: %v", err)
	}
	at := time.Date(2026, 8, 1, 9, 0, 0, 0, time.UTC)
	path := writeSessionIn(t, filepath.Join(home, ".claude"), "-home-dev-repo", "gone-1", at)

	useConfigDirs(t, home, empty)

	c := newScanCache(t)
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("first scan: %v", err)
	}
	if err := os.Remove(path); err != nil {
		t.Fatalf("removing transcript: %v", err)
	}
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("second scan: %v", err)
	}
	if got := countRows(t, c, "claude_session_cache"); got != 0 {
		t.Errorf("rows = %d, want the deleted transcript reconciled away", got)
	}
}

// Removing a dir from the set hides its sessions rather than deleting them, so
// re-adding it is immediate and costs no re-read.
func TestVisibleSessions_ExcludesUnconfiguredDirs(t *testing.T) {
	home := t.TempDir()
	second := filepath.Join(t.TempDir(), ".claude-personal")
	useConfigDirs(t, home, second)

	sessions := []ClaudeSessionSummary{
		{SessionID: "a", ConfigDir: filepath.Join(home, ".claude")},
		{SessionID: "b", ConfigDir: second},
		{SessionID: "c", ConfigDir: "/some/dir/nobody/configured"},
		{SessionID: "d"}, // pre-migration row
	}
	visible := VisibleSessions(sessions)
	if len(visible) != 3 {
		t.Fatalf("visible = %d, want 3 (a, b, d)", len(visible))
	}
	for _, s := range visible {
		if s.SessionID == "c" {
			t.Error("session from an unconfigured dir should be hidden")
		}
	}
}

func TestFindSessionFile_SearchesEveryDir(t *testing.T) {
	home := t.TempDir()
	second := filepath.Join(t.TempDir(), ".claude-personal")
	at := time.Date(2026, 8, 1, 9, 0, 0, 0, time.UTC)
	want := writeSessionIn(t, second, "-home-dev-personal", "only-in-second", at)

	useConfigDirs(t, home, second)

	dir, _, path := findSessionFile("only-in-second")
	if path != want {
		t.Errorf("path = %q, want %q", path, want)
	}
	// The dir is returned so the session's todos resolve beside its transcript
	// rather than in another account's todos/.
	if dir != second {
		t.Errorf("config dir = %q, want %q", dir, second)
	}
}
