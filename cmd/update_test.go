package cmd

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/daemon"
)

// stubManager implements daemon.Manager for update-restart tests, recording
// whether Restart was invoked and returning canned Status/Restart results.
type stubManager struct {
	status      daemon.Status
	statusErr   error
	restartErr  error
	restartHits int
}

func (m *stubManager) Install(context.Context, daemon.Options) error { return nil }
func (m *stubManager) Uninstall(context.Context) error               { return nil }
func (m *stubManager) Start(context.Context) error                   { return nil }
func (m *stubManager) Stop(context.Context) error                    { return nil }

func (m *stubManager) Restart(context.Context) error {
	m.restartHits++
	return m.restartErr
}

func (m *stubManager) Status(context.Context) (daemon.Status, error) {
	return m.status, m.statusErr
}

// TestRestartManagedServiceMatrix drives restartManagedService across the
// status matrix: only an installed AND running service is restarted, and a
// Restart error propagates (so the caller can warn) while a Status error is
// returned for the caller to treat as non-fatal.
func TestRestartManagedServiceMatrix(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name         string
		status       daemon.Status
		statusErr    error
		restartErr   error
		wantRestart  bool
		wantRestarts int
		wantErr      bool
	}{
		{
			name:         "installed and running restarts",
			status:       daemon.Status{Installed: true, Running: true, PID: 1234},
			wantRestart:  true,
			wantRestarts: 1,
		},
		{
			name:         "installed but stopped is left alone",
			status:       daemon.Status{Installed: true, Running: false},
			wantRestart:  false,
			wantRestarts: 0,
		},
		{
			name:         "not installed is left alone",
			status:       daemon.Status{Installed: false, Running: false},
			wantRestart:  false,
			wantRestarts: 0,
		},
		{
			name:         "restart error propagates",
			status:       daemon.Status{Installed: true, Running: true, PID: 1234},
			restartErr:   errors.New("systemctl failed"),
			wantRestart:  false,
			wantRestarts: 1,
			wantErr:      true,
		},
		{
			name:         "status error propagates without restart",
			status:       daemon.Status{},
			statusErr:    errors.New("status unavailable"),
			wantRestart:  false,
			wantRestarts: 0,
			wantErr:      true,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			mgr := &stubManager{status: tc.status, statusErr: tc.statusErr, restartErr: tc.restartErr}
			restarted, _, err := restartManagedService(context.Background(), mgr)
			if (err != nil) != tc.wantErr {
				t.Fatalf("err = %v, wantErr %v", err, tc.wantErr)
			}
			if restarted != tc.wantRestart {
				t.Errorf("restarted = %v, want %v", restarted, tc.wantRestart)
			}
			if mgr.restartHits != tc.wantRestarts {
				t.Errorf("Restart called %d times, want %d", mgr.restartHits, tc.wantRestarts)
			}
		})
	}
}

// TestMaybeRestartServiceUnsupportedOSFallsBackToHint asserts the
// ErrUnsupportedOS path (e.g. Windows) prints the manual-restart hint and
// never fails or attempts a restart.
func TestMaybeRestartServiceUnsupportedOSFallsBackToHint(t *testing.T) {
	prev := newServiceManager
	newServiceManager = func(*config.AppConfig) (daemon.Manager, error) {
		return nil, fmt.Errorf("wrap: %w", daemon.ErrUnsupportedOS)
	}
	t.Cleanup(func() { newServiceManager = prev })

	out := captureStdout(t, func() {
		maybeRestartService(context.Background(), &config.AppConfig{}, false)
	})
	if got := out; got != "Restart agento to use the new version.\n" {
		t.Errorf("got %q, want the manual-restart hint", got)
	}
}

// TestMaybeRestartServiceManagerErrorFallsBackToHint covers any non-OS daemon
// construction failure — same manual hint, no panic, exit code untouched.
func TestMaybeRestartServiceManagerErrorFallsBackToHint(t *testing.T) {
	prev := newServiceManager
	newServiceManager = func(*config.AppConfig) (daemon.Manager, error) {
		return nil, errors.New("boom")
	}
	t.Cleanup(func() { newServiceManager = prev })

	out := captureStdout(t, func() {
		maybeRestartService(context.Background(), &config.AppConfig{}, false)
	})
	if got := out; got != "Restart agento to use the new version.\n" {
		t.Errorf("got %q, want the manual-restart hint", got)
	}
}

// TestMaybeRestartServiceNoRestartSkipsManager asserts --no-restart short-circuits
// before any manager is even built (no systemctl/launchctl is possible) and
// prints the manual hint.
func TestMaybeRestartServiceNoRestartSkipsManager(t *testing.T) {
	prev := newServiceManager
	built := false
	newServiceManager = func(*config.AppConfig) (daemon.Manager, error) {
		built = true
		return &stubManager{}, nil
	}
	t.Cleanup(func() { newServiceManager = prev })

	out := captureStdout(t, func() {
		maybeRestartService(context.Background(), &config.AppConfig{}, true)
	})
	if built {
		t.Error("manager must not be built when --no-restart is set")
	}
	if got := out; got != "Restart agento to use the new version.\n" {
		t.Errorf("got %q, want the manual-restart hint", got)
	}
}

// TestMaybeRestartServiceMessages checks the user-facing wording for each
// outcome via a stubbed manager.
func TestMaybeRestartServiceMessages(t *testing.T) {
	cases := []struct {
		name       string
		mgr        *stubManager
		wantSubstr string
	}{
		{
			name:       "restarted",
			mgr:        &stubManager{status: daemon.Status{Installed: true, Running: true, PID: 42}},
			wantSubstr: "Restarted the agento service. The new version is live.",
		},
		{
			name:       "installed but stopped",
			mgr:        &stubManager{status: daemon.Status{Installed: true, Running: false}},
			wantSubstr: "installed but not running",
		},
		{
			name:       "no managed service",
			mgr:        &stubManager{status: daemon.Status{Installed: false, Running: false}},
			wantSubstr: "Restart agento to use the new version.",
		},
		{
			name: "restart failure warns",
			mgr: &stubManager{
				status:     daemon.Status{Installed: true, Running: true, PID: 42},
				restartErr: errors.New("kickstart failed"),
			},
			wantSubstr: "warning: restarting the agento service failed",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			prev := newServiceManager
			newServiceManager = func(*config.AppConfig) (daemon.Manager, error) { return tc.mgr, nil }
			t.Cleanup(func() { newServiceManager = prev })

			out := captureStdout(t, func() {
				maybeRestartService(context.Background(), &config.AppConfig{}, false)
			})
			if !strings.Contains(out, tc.wantSubstr) {
				t.Errorf("output %q missing %q", out, tc.wantSubstr)
			}
			// The restart-failure warning must name the actionable command.
			if tc.mgr.restartErr != nil && !strings.Contains(out, "agento service restart") {
				t.Errorf("warning %q must point at 'agento service restart'", out)
			}
		})
	}
}
