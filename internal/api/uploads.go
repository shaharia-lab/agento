package api

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/google/uuid"
)

// maxUploadSize limits file uploads to 100 MB.
const maxUploadSize = 100 << 20 // 100 MB

// parseMemoryLimit controls how much multipart form data is kept in RAM.
// Data beyond this threshold is automatically spilled to temp files by the stdlib.
const parseMemoryLimit = 10 << 20 // 10 MB

// handleUploadFile handles POST /api/uploads.
// It reads a multipart file, saves it to the tmp-uploads directory, and returns the absolute path.
func (s *Server) handleUploadFile(w http.ResponseWriter, r *http.Request) {
	if s.appConfig == nil {
		s.writeError(w, http.StatusInternalServerError, "server configuration unavailable")
		return
	}

	r.Body = http.MaxBytesReader(w, r.Body, maxUploadSize)

	if err := r.ParseMultipartForm(parseMemoryLimit); err != nil {
		s.writeError(w, http.StatusBadRequest, "file too large or invalid multipart form")
		return
	}

	file, header, err := r.FormFile("file")
	if err != nil {
		s.writeError(w, http.StatusBadRequest, "missing required field: file")
		return
	}
	defer func() {
		if cerr := file.Close(); cerr != nil {
			s.logger.Error("failed to close uploaded file", "error", cerr)
		}
	}()

	// Extract and sanitize the file extension from the original filename.
	ext := sanitizeExtension(header.Filename)

	// Generate a unique filename: <unix-timestamp-ms>-<uuid><ext>
	filename := fmt.Sprintf("%d-%s%s", time.Now().UnixMilli(), uuid.New().String(), ext)

	uploadDir := s.appConfig.TmpUploadsDir()
	if err := os.MkdirAll(uploadDir, 0o750); err != nil {
		s.logger.Error("failed to create upload directory", "path", uploadDir, "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to create upload directory")
		return
	}

	destPath := filepath.Clean(filepath.Join(uploadDir, filename))

	// Verify the resolved path is still inside the upload directory (defense against path traversal).
	// Use Clean(uploadDir) + separator to prevent prefix collisions (e.g. tmp-uploads vs tmp-uploads-evil).
	if !strings.HasPrefix(destPath, filepath.Clean(uploadDir)+string(filepath.Separator)) {
		s.writeError(w, http.StatusBadRequest, "invalid filename")
		return
	}

	dst, err := os.Create(destPath) // #nosec G304 — destPath is constructed from sanitized components
	if err != nil {
		s.logger.Error("failed to create destination file", "path", destPath, "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to save file")
		return
	}
	defer func() {
		if cerr := dst.Close(); cerr != nil {
			s.logger.Error("failed to close destination file", "path", destPath, "error", cerr)
		}
	}()

	if _, err := io.Copy(dst, file); err != nil {
		s.logger.Error("failed to write uploaded file", "path", destPath, "error", err)
		s.writeError(w, http.StatusInternalServerError, "failed to save file")
		return
	}

	s.logger.Info("file uploaded", "path", destPath, "size", header.Size, "extension", ext)
	s.writeJSON(w, http.StatusOK, map[string]string{"path": destPath})
}

// sanitizeExtension extracts the file extension from a filename and validates it.
// Only the extension is used — the original filename is never part of the stored path.
// Returns an empty string if no valid extension can be extracted.
func sanitizeExtension(filename string) string {
	// filepath.Ext returns the extension including the dot, e.g. ".png"
	ext := filepath.Ext(filepath.Base(filename))

	// Only allow alphanumeric extensions (e.g. .png, .jpg, .txt, .pdf)
	// Reject anything with path separators, dots, or other special characters.
	cleaned := strings.TrimPrefix(ext, ".")
	for _, c := range cleaned {
		isLower := c >= 'a' && c <= 'z'
		isUpper := c >= 'A' && c <= 'Z'
		isDigit := c >= '0' && c <= '9'
		if !isLower && !isUpper && !isDigit {
			return ""
		}
	}

	return ext
}
