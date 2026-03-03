// Package logger provides structured slog loggers for system-wide and
// per-session logging. All logs are written in JSON format.
//
// Log files are organized as:
//
//	<logDir>/system.log              — application-level events
//	<logDir>/sessions/<id>.log       — per-session conversation events
package logger

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"

	"go.opentelemetry.io/contrib/bridges/otelslog"
	olog "go.opentelemetry.io/otel/log"
	"go.opentelemetry.io/otel/log/global"
	"go.opentelemetry.io/otel/log/noop"
	"gopkg.in/natefinch/lumberjack.v2"
)

// NewSystemLogger creates a JSON slog.Logger that writes to <logDir>/system.log
// with automatic log rotation. The directory is created if it does not exist.
// The returned cleanup function closes the underlying log file and should be
// called on shutdown (e.g. via defer).
//
// Log records are also forwarded to the global OTel LoggerProvider (no-op
// when telemetry is disabled, OTLP when enabled). The bridge reads the global
// provider on every write, so hot-reload of the OTel config is transparent.
func NewSystemLogger(logDir string, level slog.Level) (*slog.Logger, func(), error) {
	if err := os.MkdirAll(logDir, 0750); err != nil {
		return nil, nil, fmt.Errorf("creating log directory %q: %w", logDir, err)
	}

	rotatingFile := &lumberjack.Logger{
		Filename:   filepath.Join(logDir, "system.log"),
		MaxSize:    50, // megabytes
		MaxBackups: 3,
		MaxAge:     30, // days
		Compress:   true,
	}

	fileHandler := slog.NewJSONHandler(rotatingFile, &slog.HandlerOptions{Level: level})
	// Use a globalDelegatingProvider so the bridge always resolves the current
	// OTel global on every Emit call — hot-reload works without recreating the handler.
	otelHandler := otelslog.NewHandler("agento", otelslog.WithLoggerProvider(globalDelegatingProvider{}))
	handler := &fanoutHandler{handlers: []slog.Handler{fileHandler, otelHandler}}

	cleanup := func() {
		if err := rotatingFile.Close(); err != nil {
			fmt.Fprintf(os.Stderr, "closing log file: %v\n", err)
		}
	}
	return slog.New(handler), cleanup, nil
}

// globalDelegatingProvider is a log.LoggerProvider that always delegates to
// the current OTel global LoggerProvider. Embedding noop.LoggerProvider
// satisfies the embedded.LoggerProvider interface constraint.
type globalDelegatingProvider struct {
	noop.LoggerProvider
}

func (globalDelegatingProvider) Logger(name string, opts ...olog.LoggerOption) olog.Logger {
	return globalDelegatingLogger{name: name, opts: opts}
}

// globalDelegatingLogger is a log.Logger that delegates to the current OTel
// global LoggerProvider on every Emit/Enabled call, so hot-reload of the
// global is transparent. Embedding noop.Logger satisfies embedded.Logger.
type globalDelegatingLogger struct {
	noop.Logger
	name string
	opts []olog.LoggerOption
}

func (l globalDelegatingLogger) Emit(ctx context.Context, r olog.Record) {
	global.GetLoggerProvider().Logger(l.name, l.opts...).Emit(ctx, r)
}

func (l globalDelegatingLogger) Enabled(ctx context.Context, p olog.EnabledParameters) bool {
	return global.GetLoggerProvider().Logger(l.name, l.opts...).Enabled(ctx, p)
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

// fanoutHandler fans out slog records to multiple handlers.
type fanoutHandler struct {
	handlers []slog.Handler
}

func (f *fanoutHandler) Enabled(ctx context.Context, level slog.Level) bool {
	for _, h := range f.handlers {
		if h.Enabled(ctx, level) {
			return true
		}
	}
	return false
}

func (f *fanoutHandler) Handle(ctx context.Context, r slog.Record) error {
	var firstErr error
	for _, h := range f.handlers {
		if h.Enabled(ctx, r.Level) {
			if err := h.Handle(ctx, r); err != nil && firstErr == nil {
				firstErr = err
			}
		}
	}
	return firstErr
}

func (f *fanoutHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	handlers := make([]slog.Handler, len(f.handlers))
	for i, h := range f.handlers {
		handlers[i] = h.WithAttrs(attrs)
	}
	return &fanoutHandler{handlers: handlers}
}

func (f *fanoutHandler) WithGroup(name string) slog.Handler {
	handlers := make([]slog.Handler, len(f.handlers))
	for i, h := range f.handlers {
		handlers[i] = h.WithGroup(name)
	}
	return &fanoutHandler{handlers: handlers}
}
