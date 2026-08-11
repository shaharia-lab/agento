package api_test

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/claudesessions"
)

// setupProjectDirs points HOME at a temp Claude home holding two projects with
// one transcript each, and restores the process-wide Data & Analytics snapshot
// afterwards so a hidden project cannot leak into another test in this binary.
//
// The workspaces are created for real and their encoded names derived from
// those paths, because DecodeProjectPath resolves an encoded name against the
// filesystem — a project directory that does not exist decodes to its own
// encoded form, which is not what the settings tab would be storing.
// Returns the decoded paths of the kept and dropped projects.
func setupProjectDirs(t *testing.T) (kept, dropped string) {
	t.Helper()
	home := t.TempDir()
	t.Setenv("HOME", home)

	workspace := t.TempDir()
	paths := make([]string, 2)
	for i, name := range []string{"kept", "dropped"} {
		paths[i] = filepath.Join(workspace, name)
		require.NoError(t, os.MkdirAll(paths[i], 0o750))

		encoded := strings.ReplaceAll(paths[i], string(filepath.Separator), "-")
		dir := filepath.Join(home, ".claude", "projects", encoded)
		require.NoError(t, os.MkdirAll(dir, 0o750))
		require.NoError(t, os.WriteFile(filepath.Join(dir, "session.jsonl"), []byte("{}\n"), 0o600))
	}

	t.Cleanup(func() { claudesessions.ApplyDataSettings(0, nil) })
	claudesessions.ApplyDataSettings(0, nil)
	return paths[0], paths[1]
}

func listProjects(t *testing.T, h *testHarness, query string) []claudesessions.ClaudeProject {
	t.Helper()
	w := h.do(httptest.NewRequest(http.MethodGet, "/claude-sessions/projects"+query, nil))
	require.Equal(t, http.StatusOK, w.Code)

	var projects []claudesessions.ClaudeProject
	require.NoError(t, json.Unmarshal(w.Body.Bytes(), &projects))
	return projects
}

// TestListClaudeProjects_HidesExcludedProjects covers the contract the whole
// feature rests on: once a project is hidden, no picker in the UI offers it —
// with the single exception of the settings tab that has to be able to unhide
// it, which asks for it explicitly.
func TestListClaudeProjects_HidesExcludedProjects(t *testing.T) {
	kept, dropped := setupProjectDirs(t)
	h := newHarness(t)

	assert.Len(t, listProjects(t, h, ""), 2, "nothing hidden yet")

	h.settingsStore.On("Save", mock.Anything).Return(nil)
	body, err := json.Marshal(map[string]any{"hidden_projects": []string{dropped}})
	require.NoError(t, err)
	req := httptest.NewRequest(http.MethodPut, "/settings", strings.NewReader(string(body)))
	req.Header.Set("Content-Type", "application/json")
	require.Equal(t, http.StatusOK, h.do(req).Code)

	visible := listProjects(t, h, "")
	require.Len(t, visible, 1)
	assert.Equal(t, kept, visible[0].DecodedPath)
	assert.False(t, visible[0].Hidden)

	all := listProjects(t, h, "?include_hidden=true")
	require.Len(t, all, 2)
	byPath := map[string]bool{}
	for _, p := range all {
		byPath[p.DecodedPath] = p.Hidden
	}
	assert.True(t, byPath[dropped], "the hidden project is flagged as hidden")
	assert.False(t, byPath[kept])
}

// TestUpdateSettings_RejectsOutOfRangeIdleThreshold checks the bound reaches
// the HTTP layer: the threshold defines what every duration on the dashboard
// means, so a nonsense value must fail loudly rather than be clamped silently.
func TestUpdateSettings_RejectsOutOfRangeIdleThreshold(t *testing.T) {
	_, _ = setupProjectDirs(t)
	h := newHarness(t)

	req := httptest.NewRequest(http.MethodPut, "/settings",
		strings.NewReader(`{"idle_gap_threshold_minutes":9000}`))
	req.Header.Set("Content-Type", "application/json")
	w := h.do(req)

	assert.Equal(t, http.StatusBadRequest, w.Code)
	assert.Contains(t, w.Body.String(), "idle_gap_threshold_minutes")
	h.settingsStore.AssertNotCalled(t, "Save", mock.Anything)
}

// TestUpdateSettings_AppliesIdleThreshold checks the save path installs the
// value rather than only persisting it: the snapshot is what the scanner and
// every processor read, so a stored-but-not-applied threshold would take a
// restart to mean anything.
func TestUpdateSettings_AppliesIdleThreshold(t *testing.T) {
	_, _ = setupProjectDirs(t)
	h := newHarness(t)
	h.settingsStore.On("Save", mock.Anything).Return(nil)

	req := httptest.NewRequest(http.MethodPut, "/settings",
		strings.NewReader(`{"idle_gap_threshold_minutes":25}`))
	req.Header.Set("Content-Type", "application/json")
	require.Equal(t, http.StatusOK, h.do(req).Code)

	assert.Equal(t, 25.0, claudesessions.IdleGapThreshold().Minutes())
}
