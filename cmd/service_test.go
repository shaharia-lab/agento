package cmd

import (
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

	done := make(chan string, 1)
	go func() {
		buf := make([]byte, 64*1024)
		n, _ := r.Read(buf)
		done <- string(buf[:n])
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
