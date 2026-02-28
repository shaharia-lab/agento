package api

import (
	"encoding/json"
	"net/http"
	"os"
	"path/filepath"
	"strings"
)

type fsEntry struct {
	Name  string `json:"name"`
	IsDir bool   `json:"is_dir"`
	Path  string `json:"path"`
}

type fsListResponse struct {
	Path    string    `json:"path"`
	Parent  string    `json:"parent"`
	Entries []fsEntry `json:"entries"`
}

func (s *Server) handleFSList(w http.ResponseWriter, r *http.Request) {
	rawPath := r.URL.Query().Get("path")

	// Expand ~ to home directory.
	if rawPath == "" || rawPath == "~" {
		home, err := os.UserHomeDir()
		if err != nil {
			writeError(w, http.StatusInternalServerError, "could not determine home directory")
			return
		}
		rawPath = home
	}

	// Clean and validate path to prevent traversal issues.
	clean := filepath.Clean(rawPath)

	entries, err := os.ReadDir(clean)
	if err != nil {
		if os.IsNotExist(err) {
			writeError(w, http.StatusNotFound, "path not found")
			return
		}
		writeError(w, http.StatusBadRequest, "cannot read directory")
		return
	}

	result := make([]fsEntry, 0, len(entries))
	for _, e := range entries {
		// Only include directories for cleaner browsing UX.
		if !e.IsDir() {
			continue
		}
		result = append(result, fsEntry{
			Name:  e.Name(),
			IsDir: true,
			Path:  filepath.Join(clean, e.Name()),
		})
	}

	parent := filepath.Dir(clean)
	if parent == clean {
		parent = clean // at filesystem root
	}

	writeJSON(w, http.StatusOK, fsListResponse{
		Path:    clean,
		Parent:  parent,
		Entries: result,
	})
}

type fsMkdirRequest struct {
	Path string `json:"path"`
}

func (s *Server) handleFSMkdir(w http.ResponseWriter, r *http.Request) {
	var req fsMkdirRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}
	if req.Path == "" {
		writeError(w, http.StatusBadRequest, "path is required")
		return
	}

	clean := filepath.Clean(req.Path)
	if !filepath.IsAbs(clean) || strings.Contains(clean, "..") {
		writeError(w, http.StatusBadRequest, "invalid path")
		return
	}
	if os.MkdirAll(clean, 0750) != nil {
		writeError(w, http.StatusInternalServerError, "failed to create directory")
		return
	}

	writeJSON(w, http.StatusOK, map[string]string{"path": clean})
}
