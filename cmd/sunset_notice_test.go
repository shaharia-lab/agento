package cmd

import (
	"os"
	"strings"
	"testing"
	"time"

	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/build"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/sunset"
)

// withSunsetClock swaps the sunsetNow seam for the duration of a test.
func withSunsetClock(t *testing.T, now time.Time) {
	t.Helper()
	prev := sunsetNow
	sunsetNow = func() time.Time { return now }
	t.Cleanup(func() { sunsetNow = prev })
}

// withReleaseVersion pins build.Version to a real release for the duration of a
// test, since a dev build never prints the notice.
func withReleaseVersion(t *testing.T) {
	t.Helper()
	prev := build.Version
	build.Version = "v0.11.2"
	t.Cleanup(func() { build.Version = prev })
}

// TestShouldPrintSunsetNotice_ReachesWhereTheUpdateCheckDoesNot is the point of
// having separate gating. The auto-update check skips `update` and `service`
// and requires an interactive TTY; the notice must reach all of them, because
// a scripted or service-only install is exactly the user who would otherwise
// never learn this build is being retired.
func TestShouldPrintSunsetNotice_ReachesWhereTheUpdateCheckDoesNot(t *testing.T) {
	withReleaseVersion(t)

	// `service` and `help` are skipped by the update check but must still get
	// the notice: a service-only install has no other channel to learn from.
	for _, name := range []string{"ask", "service", "help"} {
		cmd := newTestCmd(name)
		if !shouldPrintSunsetNotice(cmd) {
			t.Errorf("the sunset notice must print for %q", name)
		}
		// Sanity: these really are commands the update check skips.
		if _, skipped := updateCheckSkipCommands[name]; skipped && shouldRunAutoCheck(cmd) {
			t.Errorf("precondition changed: %q is no longer skipped by the update check", name)
		}
	}
}

// TestShouldPrintSunsetNotice_SkipsCompletion guards shell completion. Its
// stdout is parsed by the shell, and a merged stderr would turn the notice into
// a completion candidate.
func TestShouldPrintSunsetNotice_SkipsCompletion(t *testing.T) {
	withReleaseVersion(t)

	for _, name := range []string{"completion", "__complete"} {
		if shouldPrintSunsetNotice(newTestCmd(name)) {
			t.Errorf("the sunset notice must never print for %q", name)
		}
	}
}

// TestShouldPrintSunsetNotice_SkipsCommandsThatPrintTheFullNotice avoids
// telling the user the same thing twice: `agento web` prints the full notice in
// its startup banner and `agento update` prints it as its whole reason for
// existing, so prefixing either with the one-liner is pure duplication.
func TestShouldPrintSunsetNotice_SkipsCommandsThatPrintTheFullNotice(t *testing.T) {
	withReleaseVersion(t)

	for _, name := range []string{"web", "update"} {
		if shouldPrintSunsetNotice(newTestCmd(name)) {
			t.Errorf("%q prints the full notice itself; the one-liner would duplicate it", name)
		}
	}
}

// TestShouldPrintSunsetNotice_SkipsDevBuilds keeps the notice off local builds,
// which are not installs anyone needs to migrate.
func TestShouldPrintSunsetNotice_SkipsDevBuilds(t *testing.T) {
	prev := build.Version
	t.Cleanup(func() { build.Version = prev })

	for _, v := range []string{"dev", "unknown", "v0.8.0-21-gc325de6-dirty"} {
		build.Version = v
		if shouldPrintSunsetNotice(newTestCmd("ask")) {
			t.Errorf("the sunset notice must not print for version %q", v)
		}
	}
}

// TestShouldPrintSunsetNotice_HonoursOptOut reuses the existing opt-out, which
// already means "do not talk to me about versions".
func TestShouldPrintSunsetNotice_HonoursOptOut(t *testing.T) {
	withReleaseVersion(t)

	for _, v := range []string{"1", "true", "TRUE"} {
		t.Setenv(skipUpdateCheckEnv, v)
		if shouldPrintSunsetNotice(newTestCmd("ask")) {
			t.Errorf("%s=%s must suppress the sunset notice", skipUpdateCheckEnv, v)
		}
	}
}

// TestShouldPrintSunsetNotice_SkipsNonRunnable mirrors the update check: a bare
// `agento` printing help is not a command anyone is running.
func TestShouldPrintSunsetNotice_SkipsNonRunnable(t *testing.T) {
	withReleaseVersion(t)

	if shouldPrintSunsetNotice(nil) {
		t.Error("a nil command must not print")
	}
	if shouldPrintSunsetNotice(newNonRunnableTestCmd()) {
		t.Error("a non-runnable command must not print")
	}
}

func newNonRunnableTestCmd() *cobra.Command {
	return &cobra.Command{Use: "agento"}
}

// captureStderr redirects os.Stderr for the duration of fn and returns what was
// written. It mirrors captureStdout in service_test.go.
func captureStderr(t *testing.T, fn func()) string {
	t.Helper()
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("pipe: %v", err)
	}
	orig := os.Stderr
	os.Stderr = w
	defer func() { os.Stderr = orig }()

	done := make(chan string, 1)
	go func() {
		buf := make([]byte, 64*1024)
		n, _ := r.Read(buf) //nolint:errcheck
		done <- string(buf[:n])
	}()

	fn()
	_ = w.Close() //nolint:errcheck
	os.Stderr = orig

	select {
	case out := <-done:
		return out
	case <-time.After(2 * time.Second):
		t.Fatal("timed out capturing stderr")
		return ""
	}
}

// TestMaybePrintSunsetNotice_RateLimits drives the whole hook against a real
// data dir: it prints once, stays quiet for the rest of the day, and returns
// the next day.
func TestMaybePrintSunsetNotice_RateLimits(t *testing.T) {
	withReleaseVersion(t)
	t.Setenv(skipUpdateCheckEnv, "")

	dir := t.TempDir()
	cfg := &config.AppConfig{DataDir: dir}
	start := time.Date(2026, time.August, 23, 9, 0, 0, 0, time.UTC)

	withSunsetClock(t, start)
	first := captureStderr(t, func() { maybePrintSunsetNotice(newTestCmd("ask"), cfg) })
	if !strings.Contains(first, sunset.CutoffDate) {
		t.Fatalf("the first run must print the notice, got %q", first)
	}

	withSunsetClock(t, start.Add(6*time.Hour))
	if second := captureStderr(t, func() { maybePrintSunsetNotice(newTestCmd("ask"), cfg) }); second != "" {
		t.Errorf("a second run the same day must be silent, got %q", second)
	}

	withSunsetClock(t, start.Add(25*time.Hour))
	if third := captureStderr(t, func() { maybePrintSunsetNotice(newTestCmd("ask"), cfg) }); !strings.Contains(third, sunset.CutoffDate) {
		t.Errorf("the next day must print again, got %q", third)
	}
}

// TestMaybePrintSunsetNotice_GoesToStderr keeps stdout clean for subcommands
// that emit machine-readable output.
func TestMaybePrintSunsetNotice_GoesToStderr(t *testing.T) {
	withReleaseVersion(t)
	t.Setenv(skipUpdateCheckEnv, "")
	withSunsetClock(t, time.Date(2026, time.August, 23, 9, 0, 0, 0, time.UTC))

	cfg := &config.AppConfig{DataDir: t.TempDir()}
	stdout := captureStdout(t, func() {
		_ = captureStderr(t, func() { maybePrintSunsetNotice(newTestCmd("ask"), cfg) })
	})
	if stdout != "" {
		t.Errorf("the notice must not reach stdout, got %q", stdout)
	}
}

// TestMaybePrintSunsetNotice_CarriesTheFacts asserts the printed line says
// where to go and that the database is shared — without those it is a warning
// with no action attached.
func TestMaybePrintSunsetNotice_CarriesTheFacts(t *testing.T) {
	withReleaseVersion(t)
	t.Setenv(skipUpdateCheckEnv, "")
	withSunsetClock(t, time.Date(2026, time.August, 23, 9, 0, 0, 0, time.UTC))

	cfg := &config.AppConfig{DataDir: t.TempDir()}
	out := captureStderr(t, func() { maybePrintSunsetNotice(newTestCmd("ask"), cfg) })

	for _, want := range []string{sunset.CutoffDate, sunset.DesktopReleasesURL, "~/.agento/agento.db"} {
		if !strings.Contains(out, want) {
			t.Errorf("the notice must mention %q, got %q", want, out)
		}
	}
}
