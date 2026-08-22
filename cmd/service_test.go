package cmd

import (
	"io"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/config"
)

func TestServiceCmdRegistersSubcommands(t *testing.T) {
	t.Parallel()
	cfg := &config.AppConfig{DataDir: t.TempDir(), Port: 8990}
	root := NewServiceCmd(cfg)

	want := []string{"install", "uninstall", "start", "stop", "restart", "status", "logs"}
	for _, name := range want {
		found := false
		for _, sub := range root.Commands() {
			if sub.Name() == name {
				found = true
				break
			}
		}
		if !found {
			t.Errorf("subcommand %q not registered on service command", name)
		}
	}
}

func TestServiceSubcommandsSilenceUsageAndErrors(t *testing.T) {
	t.Parallel()
	cfg := &config.AppConfig{DataDir: t.TempDir(), Port: 8990}
	root := NewServiceCmd(cfg)

	// Runtime failures (e.g. "service is not running") must not dump usage or
	// print the error twice — the root Execute prints it once.
	for _, sub := range root.Commands() {
		if !sub.SilenceUsage {
			t.Errorf("service %s: SilenceUsage must be true", sub.Name())
		}
		if !sub.SilenceErrors {
			t.Errorf("service %s: SilenceErrors must be true", sub.Name())
		}
	}
}

func TestServiceLogsCmdFlags(t *testing.T) {
	t.Parallel()
	cfg := &config.AppConfig{DataDir: t.TempDir(), Port: 8990}
	root := NewServiceCmd(cfg)

	var logsCmdFound bool
	for _, sub := range root.Commands() {
		if sub.Name() != "logs" {
			continue
		}
		logsCmdFound = true
		if sub.Flags().Lookup("follow") == nil {
			t.Error("logs command missing --follow flag")
		}
		if sub.Flags().Lookup("lines") == nil {
			t.Error("logs command missing --lines flag")
		}
	}
	if !logsCmdFound {
		t.Fatal("logs subcommand not found")
	}
}

func TestRunServiceLogsPrintsTrailingLines(t *testing.T) {
	t.Parallel()
	logPath := filepath.Join(t.TempDir(), "service.log")
	content := "line1\nline2\nline3\nline4\n"
	if err := os.WriteFile(logPath, []byte(content), 0o600); err != nil {
		t.Fatalf("write log: %v", err)
	}

	out := captureStdout(t, func() {
		if err := runServiceLogs(t.Context(), logPath, false, 2); err != nil {
			t.Errorf("runServiceLogs: %v", err)
		}
	})
	if out != "line3\nline4\n" {
		t.Errorf("got %q, want %q", out, "line3\nline4\n")
	}
}

func TestRunServiceLogsMissingFile(t *testing.T) {
	t.Parallel()
	err := runServiceLogs(t.Context(), filepath.Join(t.TempDir(), "nope.log"), false, 10)
	if err == nil {
		t.Fatal("want error for missing log file")
	}
}

// writeLogFile writes content to a fresh temp file and returns it open for
// reading — the shape tailLines expects.
func writeLogFile(t *testing.T, content string) *os.File {
	t.Helper()
	f, err := os.CreateTemp(t.TempDir(), "log-*")
	if err != nil {
		t.Fatalf("create temp log: %v", err)
	}
	if _, err := f.WriteString(content); err != nil {
		t.Fatalf("write temp log: %v", err)
	}
	t.Cleanup(func() { _ = f.Close() })
	return f
}

func TestTailLines(t *testing.T) {
	t.Parallel()
	f := writeLogFile(t, "a\nb\nc\n")

	got, err := tailLines(f, 2)
	if err != nil {
		t.Fatalf("tailLines: %v", err)
	}
	if string(got) != "b\nc\n" {
		t.Errorf("got %q, want %q", got, "b\nc\n")
	}
}

func TestTailLinesMoreThanAvailable(t *testing.T) {
	t.Parallel()
	f := writeLogFile(t, "only\n")

	got, err := tailLines(f, 50)
	if err != nil {
		t.Fatalf("tailLines: %v", err)
	}
	if string(got) != "only\n" {
		t.Errorf("got %q, want %q", got, "only\n")
	}
}

// captureStdout redirects os.Stdout for the duration of fn and returns what
// was written.
func captureStdout(t *testing.T, fn func()) string {
	t.Helper()
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("pipe: %v", err)
	}
	orig := os.Stdout
	os.Stdout = w
	defer func() { os.Stdout = orig }()

	// Drain to EOF rather than issuing a single Read. A single Read returns as
	// soon as ANY bytes are available, so a function that makes several separate
	// writes — a multi-line banner, say — is captured as only its first chunk,
	// and whether that happens at all depends on goroutine scheduling. That made
	// it a test that passed locally and failed in CI.
	done := make(chan string, 1)
	go func() {
		out, _ := io.ReadAll(r) //nolint:errcheck
		done <- string(out)
	}()

	fn()
	_ = w.Close()
	os.Stdout = orig

	select {
	case out := <-done:
		return out
	case <-time.After(2 * time.Second):
		t.Fatal("timed out capturing stdout")
		return ""
	}
}
