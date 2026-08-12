package claudesessions

import (
	"context"
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

// GetSessionJourney validated its session id; GetSessionDetail never did, and
// both now resolve through findSessionFile. A route param is not a filename.
func TestFindSessionFile_RejectsTraversal(t *testing.T) {
	home := t.TempDir()
	useConfigDirs(t, home)

	for _, id := range []string{
		"../../../etc/passwd",
		"..",
		"a/b",
		"foo\x00bar",
	} {
		dir, project, path := findSessionFile(id)
		if dir != "" || project != "" || path != "" {
			t.Errorf("findSessionFile(%q) = (%q,%q,%q), want all empty", id, dir, project, path)
		}
		if todos := loadTodos(filepath.Join(home, ".claude"), id); todos != nil {
			t.Errorf("loadTodos(%q) returned %v, want nil", id, todos)
		}
	}
}

// Protection is per project, not per config dir. One unreadable project — a
// root-owned directory, say — must not stop every other project's genuinely
// deleted transcripts from ever being reconciled away.
func TestIncrementalScan_UnreadableProjectProtectsOnlyItself(t *testing.T) {
	if os.Geteuid() == 0 {
		t.Skip("root ignores directory permissions")
	}
	home := t.TempDir()
	at := time.Date(2026, 8, 1, 9, 0, 0, 0, time.UTC)
	claudeDir := filepath.Join(home, ".claude")
	good := writeSessionIn(t, claudeDir, "-home-dev-good", "good-1", at)
	writeSessionIn(t, claudeDir, "-home-dev-bad", "bad-1", at)
	useConfigDirs(t, home)

	c := newScanCache(t)
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("first scan: %v", err)
	}
	if got := countRows(t, c, "claude_session_cache"); got != 2 {
		t.Fatalf("expected 2 rows, got %d", got)
	}

	badDir := filepath.Join(claudeDir, "projects", "-home-dev-bad")
	if err := os.Chmod(badDir, 0o000); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	// Restored so t.TempDir's cleanup can remove it.
	// #nosec G302 -- a directory needs the execute bit to be traversable.
	t.Cleanup(func() { _ = os.Chmod(badDir, 0o750) })

	// Delete a transcript in the *healthy* project.
	if err := os.Remove(good); err != nil {
		t.Fatalf("removing transcript: %v", err)
	}
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("second scan: %v", err)
	}

	// good-1 is reconciled away; bad-1 is preserved because its project could
	// not be listed and absence there means "we could not look".
	if got := countRows(t, c, "claude_session_cache"); got != 1 {
		t.Fatalf("rows = %d, want 1 (bad-1 preserved, good-1 reconciled)", got)
	}
	var id string
	if err := c.db.QueryRowContext(
		context.Background(), "SELECT session_id FROM claude_session_cache",
	).Scan(&id); err != nil {
		t.Fatalf("reading row: %v", err)
	}
	if id != "bad-1" {
		t.Errorf("surviving row = %q, want bad-1", id)
	}
}

// Migration 27 cannot backfill config_dir — the home directory is not a SQL
// constant — and a scan only re-reads a file whose mtime changed. Without the
// v13 scanner-version bump an upgraded corpus would keep an empty config_dir
// forever, and the account filter would match none of it.
func TestIncrementalScan_StampsConfigDirOnPreexistingRows(t *testing.T) {
	home := t.TempDir()
	at := time.Date(2026, 8, 1, 9, 0, 0, 0, time.UTC)
	writeSessionIn(t, filepath.Join(home, ".claude"), "-home-dev-repo", "legacy-1", at)
	useConfigDirs(t, home)

	c := newScanCache(t)
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	// Recreate the post-migration state: a row cached by an older scanner,
	// with no config_dir and the file untouched since.
	ctx := context.Background()
	if _, err := c.db.ExecContext(ctx,
		`UPDATE claude_session_cache SET config_dir = ''`); err != nil {
		t.Fatalf("blanking config_dir: %v", err)
	}
	// Pinned to the literal version that shipped before config_dir existed,
	// not to CurrentScannerVersion-1: a relative rewind would keep passing if
	// the bump were reverted, which is the one thing this test exists to catch.
	const versionBeforeConfigDir = 12
	if _, err := c.db.ExecContext(ctx,
		`UPDATE claude_cache_metadata SET scanner_version = ?`,
		versionBeforeConfigDir); err != nil {
		t.Fatalf("rewinding scanner version: %v", err)
	}
	if CurrentScannerVersion <= versionBeforeConfigDir {
		t.Fatalf("CurrentScannerVersion = %d, want > %d so cached rows are re-stamped",
			CurrentScannerVersion, versionBeforeConfigDir)
	}

	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("second scan: %v", err)
	}

	var got string
	if err := c.db.QueryRowContext(ctx,
		`SELECT config_dir FROM claude_session_cache`).Scan(&got); err != nil {
		t.Fatalf("reading config_dir: %v", err)
	}
	if want := filepath.Join(home, ".claude"); got != want {
		t.Errorf("config_dir = %q, want %q — the version bump must re-stamp existing rows", got, want)
	}
}
