package claudesessions

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// writeCorpus writes n sessions, each delegating to one sub-agent, under a
// temporary HOME. It returns the number of transcripts on disk.
func writeCorpus(t *testing.T, home string, n int) int {
	t.Helper()
	projectDir := filepath.Join(home, ".claude", "projects", "-home-dev-repo")
	if err := os.MkdirAll(projectDir, 0o750); err != nil {
		t.Fatalf("creating project dir: %v", err)
	}
	base := time.Date(2026, 8, 1, 9, 0, 0, 0, time.UTC)

	for i := range n {
		id := fmt.Sprintf("session-%03d", i)
		at := base.Add(time.Duration(i) * time.Minute)
		writeJSONL(t, projectDir, id, at)

		subDir := filepath.Join(projectDir, id, "subagents")
		if err := os.MkdirAll(subDir, 0o750); err != nil {
			t.Fatalf("creating subagent dir: %v", err)
		}
		writeJSONL(t, subDir, "agent-1", at.Add(time.Second))
	}
	return n * 2
}

func countRows(t *testing.T, c *Cache, table string) int {
	t.Helper()
	var n int
	// #nosec G202 -- table is a test-local constant.
	if err := c.db.QueryRowContext(context.Background(), "SELECT COUNT(*) FROM "+table).Scan(&n); err != nil {
		t.Fatalf("counting %s: %v", table, err)
	}
	return n
}

func TestIncrementalScan_ParallelReadsWriteEveryRow(t *testing.T) {
	// The reader pool and the batching writer replaced a strictly serial loop
	// with one transaction per file. More sessions than one batch, so the
	// multi-batch path is the one under test.
	home := t.TempDir()
	t.Setenv("HOME", home)
	sessions := scanBatchSize + 37
	writeCorpus(t, home, sessions)

	c := newPageCache(t)
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("scan: %v", err)
	}

	if got := countRows(t, c, "claude_session_cache"); got != sessions {
		t.Errorf("cached %d sessions, want %d", got, sessions)
	}
	if got := countRows(t, c, "claude_subagent_cache"); got != sessions {
		t.Errorf("cached %d sub-agents, want %d", got, sessions)
	}
}

func TestIncrementalScan_ReportsProgress(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	transcripts := writeCorpus(t, home, scanBatchSize+5)

	c := newPageCache(t)
	type point struct{ done, total int }
	var seen []point
	if _, err := IncrementalScanWith(c.db, testLogger, ScanOptions{
		Progress: func(done, total int) { seen = append(seen, point{done, total}) },
	}); err != nil {
		t.Fatalf("scan: %v", err)
	}

	if len(seen) < 2 {
		t.Fatalf("progress reported %d times; a multi-batch scan should report more", len(seen))
	}
	// Reported before any work, so a first run shows 0 / N rather than nothing.
	if seen[0].done != 0 || seen[0].total != transcripts {
		t.Errorf("first progress report was %+v, want {0 %d}", seen[0], transcripts)
	}
	last := seen[len(seen)-1]
	if last.done != transcripts || last.total != transcripts {
		t.Errorf("final progress report was %+v, want {%d %d}", last, transcripts, transcripts)
	}
	for i := 1; i < len(seen); i++ {
		if seen[i].done < seen[i-1].done {
			t.Errorf("progress went backwards: %+v then %+v", seen[i-1], seen[i])
		}
	}
}

func TestIncrementalScan_NotifiesEachSessionOnce(t *testing.T) {
	// A session with a changed sub-agent must be notified against the PARENT
	// path and exactly once, however many of its files changed: the insight run
	// re-reads the whole session, so N+1 notifications would be N+1 runs of the
	// same work.
	home := t.TempDir()
	t.Setenv("HOME", home)
	writeCorpus(t, home, 12)

	c := newPageCache(t)
	notified := map[string]int{}
	newFlag := map[string]bool{}
	if _, err := IncrementalScanWith(c.db, testLogger, ScanOptions{
		Notify: func(sessionID, filePath string, isNew bool) {
			notified[sessionID]++
			newFlag[sessionID] = isNew
			if filepath.Base(filepath.Dir(filePath)) == "subagents" {
				t.Errorf("session %s was notified against a sub-agent path %q", sessionID, filePath)
			}
		},
	}); err != nil {
		t.Fatalf("scan: %v", err)
	}

	if len(notified) != 12 {
		t.Errorf("notified %d sessions, want 12", len(notified))
	}
	for id, n := range notified {
		if n != 1 {
			t.Errorf("session %s notified %d times, want 1", id, n)
		}
		if !newFlag[id] {
			t.Errorf("session %s was not reported as new on a first scan", id)
		}
	}
}

func TestIncrementalScan_DeletesRowsForRemovedTranscripts(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	writeCorpus(t, home, 4)

	c := newPageCache(t)
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("first scan: %v", err)
	}

	projectDir := filepath.Join(home, ".claude", "projects", "-home-dev-repo")
	if err := os.Remove(filepath.Join(projectDir, "session-000.jsonl")); err != nil {
		t.Fatalf("removing transcript: %v", err)
	}
	if err := os.RemoveAll(filepath.Join(projectDir, "session-001")); err != nil {
		t.Fatalf("removing sub-agent dir: %v", err)
	}
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("second scan: %v", err)
	}

	if got := countRows(t, c, "claude_session_cache"); got != 3 {
		t.Errorf("after deleting one session, %d rows remain, want 3", got)
	}
	// Two sub-agent rows go: the one whose directory was removed, and the one
	// belonging to the deleted parent. Sub-agent transcripts are enumerated by
	// walking each parent, so a parent that is gone takes its delegated
	// transcripts with it rather than leaving them orphaned in the cache.
	if got := countRows(t, c, "claude_subagent_cache"); got != 2 {
		t.Errorf("%d sub-agent rows remain, want 2", got)
	}
}

func TestIncrementalScan_UnreadableTranscriptDoesNotAbortTheScan(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	writeCorpus(t, home, 5)

	// A file that is not JSONL at all still parses line-by-line into nothing;
	// a directory where a transcript should be is the read failure this covers.
	projectDir := filepath.Join(home, ".claude", "projects", "-home-dev-repo")
	broken := filepath.Join(projectDir, "session-broken.jsonl")
	if err := os.Mkdir(broken, 0o750); err != nil {
		t.Fatalf("creating unreadable transcript: %v", err)
	}

	c := newPageCache(t)
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("scan: %v", err)
	}
	// The five readable sessions must all be cached: one bad file is a reason
	// to skip that file, not to leave the whole corpus unscanned.
	if got := countRows(t, c, "claude_session_cache"); got != 5 {
		t.Errorf("cached %d sessions past an unreadable one, want 5", got)
	}
}

func TestListProjects_ServedFromTheScanRatherThanARedundantWalk(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	writeCorpus(t, home, 3)

	c := newPageCache(t)
	if _, err := IncrementalScan(c.db, testLogger); err != nil {
		t.Fatalf("scan: %v", err)
	}

	projects, err := ListProjects()
	if err != nil {
		t.Fatalf("list projects: %v", err)
	}
	if len(projects) != 1 {
		t.Fatalf("listed %d projects, want 1", len(projects))
	}
	// Sub-agent transcripts must not inflate the count: a session that
	// delegated three times is still one session in the picker.
	if projects[0].SessionCount != 3 {
		t.Errorf("project reports %d sessions, want 3", projects[0].SessionCount)
	}

	// Removing the directory proves the answer came from the published list
	// rather than from a fresh walk.
	if err := os.RemoveAll(filepath.Join(home, ".claude", "projects")); err != nil {
		t.Fatalf("removing projects dir: %v", err)
	}
	cached, err := ListProjects()
	if err != nil {
		t.Fatalf("list projects from cache: %v", err)
	}
	if len(cached) != 1 {
		t.Errorf("cached list returned %d projects, want the published 1", len(cached))
	}
}
