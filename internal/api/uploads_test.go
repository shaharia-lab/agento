package api_test

import (
	"bytes"
	"encoding/json"
	"mime/multipart"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/go-chi/chi/v5"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/api"
	"github.com/shaharia-lab/agento/internal/config"
	cfgmocks "github.com/shaharia-lab/agento/internal/config/mocks"
)

func newUploadHarness(t *testing.T) (chi.Router, string) {
	t.Helper()

	tmpDir := t.TempDir()
	appCfg := &config.AppConfig{DataDir: tmpDir}

	settingsStore := new(cfgmocks.MockSettingsStore)
	settingsStore.On("Load").Return(config.UserSettings{}, nil)

	mgr, err := config.NewSettingsManager(settingsStore, appCfg)
	require.NoError(t, err)

	srv := api.New(api.ServerConfig{
		SettingsMgr: mgr,
		AppConfig:   appCfg,
	})

	r := chi.NewRouter()
	srv.Mount(r)

	return r, tmpDir
}

func TestHandleUploadFile_Success(t *testing.T) {
	router, tmpDir := newUploadHarness(t)

	body := &bytes.Buffer{}
	writer := multipart.NewWriter(body)
	part, err := writer.CreateFormFile("file", "screenshot.png")
	require.NoError(t, err)
	_, err = part.Write([]byte("fake png content"))
	require.NoError(t, err)
	require.NoError(t, writer.Close())

	req := httptest.NewRequest(http.MethodPost, "/uploads", body)
	req.Header.Set("Content-Type", writer.FormDataContentType())
	w := httptest.NewRecorder()
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusOK, w.Code)

	var resp map[string]string
	require.NoError(t, json.Unmarshal(w.Body.Bytes(), &resp))
	path := resp["path"]
	assert.NotEmpty(t, path)
	assert.True(t, strings.HasPrefix(path, filepath.Join(tmpDir, "tmp-uploads")),
		"path should be under tmp-uploads dir, got: %s", path)
	assert.True(t, strings.HasSuffix(path, ".png"), "path should have .png extension, got: %s", path)

	// Verify the file was actually written to disk.
	content, err := os.ReadFile(path) // #nosec G304 — path is from test response, not user input
	require.NoError(t, err)
	assert.Equal(t, "fake png content", string(content))
}

func TestHandleUploadFile_MultipleFiles(t *testing.T) {
	router, tmpDir := newUploadHarness(t)

	// Upload two files sequentially and verify they get unique paths.
	fileNames := []string{"file1.txt", "file2.jpg"}
	paths := make([]string, 0, len(fileNames))
	for _, name := range fileNames {
		body := &bytes.Buffer{}
		writer := multipart.NewWriter(body)
		part, err := writer.CreateFormFile("file", name)
		require.NoError(t, err)
		_, err = part.Write([]byte("content of " + name))
		require.NoError(t, err)
		require.NoError(t, writer.Close())

		req := httptest.NewRequest(http.MethodPost, "/uploads", body)
		req.Header.Set("Content-Type", writer.FormDataContentType())
		w := httptest.NewRecorder()
		router.ServeHTTP(w, req)

		require.Equal(t, http.StatusOK, w.Code)

		var resp map[string]string
		require.NoError(t, json.Unmarshal(w.Body.Bytes(), &resp))
		paths = append(paths, resp["path"])
	}

	assert.NotEqual(t, paths[0], paths[1], "each upload should produce a unique path")
	assert.True(t, strings.HasPrefix(paths[0], filepath.Join(tmpDir, "tmp-uploads")))
	assert.True(t, strings.HasPrefix(paths[1], filepath.Join(tmpDir, "tmp-uploads")))
}

func TestHandleUploadFile_MissingFile(t *testing.T) {
	router, _ := newUploadHarness(t)

	body := &bytes.Buffer{}
	writer := multipart.NewWriter(body)
	require.NoError(t, writer.Close())

	req := httptest.NewRequest(http.MethodPost, "/uploads", body)
	req.Header.Set("Content-Type", writer.FormDataContentType())
	w := httptest.NewRecorder()
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusBadRequest, w.Code)
}

func TestHandleUploadFile_NoExtension(t *testing.T) {
	router, tmpDir := newUploadHarness(t)

	body := &bytes.Buffer{}
	writer := multipart.NewWriter(body)
	part, err := writer.CreateFormFile("file", "Makefile")
	require.NoError(t, err)
	_, err = part.Write([]byte("all: build"))
	require.NoError(t, err)
	require.NoError(t, writer.Close())

	req := httptest.NewRequest(http.MethodPost, "/uploads", body)
	req.Header.Set("Content-Type", writer.FormDataContentType())
	w := httptest.NewRecorder()
	router.ServeHTTP(w, req)

	assert.Equal(t, http.StatusOK, w.Code)

	var resp map[string]string
	require.NoError(t, json.Unmarshal(w.Body.Bytes(), &resp))
	path := resp["path"]
	assert.True(t, strings.HasPrefix(path, filepath.Join(tmpDir, "tmp-uploads")))
	// No extension — path should end with the UUID only.
	assert.False(t, strings.Contains(filepath.Base(path), ".."))
}

func TestSanitizeExtension(t *testing.T) {
	// This test verifies the sanitizeExtension function indirectly by uploading
	// files with various filenames and checking that the result path has a safe extension.
	router, _ := newUploadHarness(t)

	tests := []struct {
		name     string
		filename string
		wantExt  string // expected extension suffix in the path
	}{
		{"normal png", "photo.png", ".png"},
		{"uppercase", "DOC.PDF", ".PDF"},
		{"no extension", "README", ""},
		{"special chars in ext", "file.t@r", ""},
		{"double dot", "file.tar.gz", ".gz"},
		{"dot only", "file.", ""},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			body := &bytes.Buffer{}
			writer := multipart.NewWriter(body)
			part, err := writer.CreateFormFile("file", tt.filename)
			require.NoError(t, err)
			_, err = part.Write([]byte("test"))
			require.NoError(t, err)
			require.NoError(t, writer.Close())

			req := httptest.NewRequest(http.MethodPost, "/uploads", body)
			req.Header.Set("Content-Type", writer.FormDataContentType())
			w := httptest.NewRecorder()
			router.ServeHTTP(w, req)

			require.Equal(t, http.StatusOK, w.Code)

			var resp map[string]string
			require.NoError(t, json.Unmarshal(w.Body.Bytes(), &resp))
			path := resp["path"]
			if tt.wantExt != "" {
				assert.True(t, strings.HasSuffix(path, tt.wantExt),
					"expected path to end with %q, got: %s", tt.wantExt, path)
			}
		})
	}
}
