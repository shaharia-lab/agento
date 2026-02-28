// Package logger provides structured slog loggers for system-wide and
// per-session logging. All logs are written in JSON format.
//
// Log files are organized as:
//
//	<logDir>/system.log              — application-level events
//	<logDir>/sessions/<id>.log       — per-session conversation events
package logger

import (
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
)

// NewSystemLogger creates a JSON slog.Logger that writes to <logDir>/system.log.
// The directory is created if it does not exist.
func NewSystemLogger(logDir string, level slog.Level) (*slog.Logger, error) {
	if err := os.MkdirAll(logDir, 0750); err != nil {
		return nil, fmt.Errorf("creating log directory %q: %w", logDir, err)
	}

	f, err := openLogFile(filepath.Join(logDir, "system.log"))
	if err != nil {
		return nil, err
	}

	handler := slog.NewJSONHandler(f, &slog.HandlerOptions{Level: level})
	return slog.New(handler), nil
}

// NewSessionLogger creates a JSON slog.Logger that writes to
// <logDir>/sessions/<sessionID>.log.
// The sessions sub-directory is created if it does not exist.
func NewSessionLogger(logDir string, sessionID string, level slog.Level) (*slog.Logger, error) {
	sessionsDir := filepath.Join(logDir, "sessions")
	if err := os.MkdirAll(sessionsDir, 0750); err != nil {
		return nil, fmt.Errorf("creating sessions log directory %q: %w", sessionsDir, err)
	}

	f, err := openLogFile(filepath.Join(sessionsDir, sessionID+".log"))
	if err != nil {
		return nil, err
	}

	handler := slog.NewJSONHandler(f, &slog.HandlerOptions{Level: level})
	return slog.New(handler).With("session_id", sessionID), nil
}

// openLogFile opens (or creates) a log file with append semantics.
func openLogFile(path string) (*os.File, error) {
	//nolint:gosec // path is constructed from admin-configured log dir
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0600)
	if err != nil {
		return nil, fmt.Errorf("opening log file %q: %w", path, err)
	}
	return f, nil
}
