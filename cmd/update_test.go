package cmd

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/daemon"
	"github.com/shaharia-lab/agento/internal/sunset"
)

// stubManager implements daemon.Manager for update tests, recording whether
// Uninstall was invoked and returning canned Status/Uninstall results.
type stubManager struct {
	status        daemon.Status
	statusErr     error
	uninstallErr  error
	uninstallHits int
}

func (m *stubManager) Install(context.Context, daemon.Options) error { return nil }
func (m *stubManager) Start(context.Context) error                   { return nil }
func (m *stubManager) Stop(context.Context) error                    { return nil }
func (m *stubManager) Restart(context.Context) error                 { return nil }

func (m *stubManager) Uninstall(context.Context) error {
	m.uninstallHits++
	return m.uninstallErr
}

func (m *stubManager) Status(context.Context) (daemon.Status, error) {
	return m.status, m.statusErr
}

// withManager swaps the newServiceManager seam for the duration of a test.
func withManager(t *testing.T, mgr daemon.Manager, err error) {
	t.Helper()
	prev := newServiceManager
	newServiceManager = func(*config.AppConfig) (daemon.Manager, error) { return mgr, err }
	t.Cleanup(func() { newServiceManager = prev })
}

// withClock swaps the updateNow seam so the cutoff branch is reachable without
// waiting for September.
func withClock(t *testing.T, now time.Time) {
	t.Helper()
	prev := updateNow
	updateNow = func() time.Time { return now }
	t.Cleanup(func() { updateNow = prev })
}

// runUpdateCapturing drives the whole command with injected streams.
func runUpdateCapturing(t *testing.T, stdin string, skipConfirm bool) string {
	t.Helper()
	var out bytes.Buffer
	if err := runUpdateTo(context.Background(), &out, strings.NewReader(stdin), &config.AppConfig{}, skipConfirm); err != nil {
		t.Fatalf("runUpdateTo: %v", err)
	}
	return out.String()
}

// TestUpdateNeverSelfUpdates is the headline behavior of this release: the
// command no longer replaces the binary, it points at Agento Desktop. Both
// sides of the cutoff must offer a way to get the desktop app and neither may
// talk about updating this binary.
func TestUpdateNeverSelfUpdates(t *testing.T) {
	withManager(t, &stubManager{status: daemon.Status{Installed: false}}, nil)

	for _, tc := range []struct {
		name string
		now  time.Time
	}{
		{"before the cutoff", sunset.Cutoff.Add(-24 * time.Hour)},
		{"after the cutoff", sunset.Cutoff.Add(24 * time.Hour)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			withClock(t, tc.now)
			out := runUpdateCapturing(t, "", false)

			if !strings.Contains(out, sunset.DesktopReleasesURL) {
				t.Errorf("output must point at the desktop releases:\n%s", out)
			}
			for _, forbidden := range []string{"Updating to", "already up to date", "Update to"} {
				if strings.Contains(out, forbidden) {
					t.Errorf("output must not talk about self-updating, found %q:\n%s", forbidden, out)
				}
			}
		})
	}
}

// TestUpdateAfterCutoffOffersNoDownload asserts the post-cutoff branch prints
// only the migration message — and, just as importantly, that it says the rest
// of the app is unaffected. Disabling updates is not a kill switch.
func TestUpdateAfterCutoffOffersNoDownload(t *testing.T) {
	withManager(t, &stubManager{status: daemon.Status{Installed: false}}, nil)
	withClock(t, sunset.Cutoff.Add(time.Hour))

	out := runUpdateCapturing(t, "", false)

	if !strings.Contains(out, "no longer updated") {
		t.Errorf("output must state that updates have stopped:\n%s", out)
	}
	if !strings.Contains(out, "Every other agento command is\nunaffected") {
		t.Errorf("output must state the rest of the app still works:\n%s", out)
	}
	if strings.Contains(out, "Latest Agento Desktop:") {
		t.Errorf("the post-cutoff branch must not resolve a release:\n%s", out)
	}
}

// TestServiceRemovalPrompt covers the decision matrix around the leftover unit.
// The destructive direction requires an explicit yes; everything else — a
// declined prompt, EOF on a piped stdin, a stray answer — leaves the unit alone.
func TestServiceRemovalPrompt(t *testing.T) {
	cases := []struct {
		name          string
		status        daemon.Status
		statusErr     error
		stdin         string
		skipConfirm   bool
		wantUninstall int
		wantSubstr    string
	}{
		{
			name:          "installed and accepted",
			status:        daemon.Status{Installed: true},
			stdin:         "y\n",
			wantUninstall: 1,
			wantSubstr:    "Removed the agento background service.",
		},
		{
			name:          "installed and accepted with yes spelled out",
			status:        daemon.Status{Installed: true},
			stdin:         "YES\n",
			wantUninstall: 1,
			wantSubstr:    "Removed the agento background service.",
		},
		{
			name:          "installed and declined",
			status:        daemon.Status{Installed: true},
			stdin:         "n\n",
			wantUninstall: 0,
			wantSubstr:    "Left in place.",
		},
		{
			name:          "installed with empty answer",
			status:        daemon.Status{Installed: true},
			stdin:         "\n",
			wantUninstall: 0,
			wantSubstr:    "Left in place.",
		},
		{
			name:          "installed with EOF on stdin",
			status:        daemon.Status{Installed: true},
			stdin:         "",
			wantUninstall: 0,
			wantSubstr:    "Left in place.",
		},
		{
			name:          "installed with -y skips the prompt",
			status:        daemon.Status{Installed: true},
			stdin:         "",
			skipConfirm:   true,
			wantUninstall: 1,
			wantSubstr:    "Removed the agento background service.",
		},
		{
			name:          "not installed asks nothing",
			status:        daemon.Status{Installed: false},
			stdin:         "y\n",
			skipConfirm:   true,
			wantUninstall: 0,
			wantSubstr:    "",
		},
		{
			name:          "status error asks nothing",
			status:        daemon.Status{Installed: true},
			statusErr:     errors.New("systemctl unavailable"),
			stdin:         "y\n",
			skipConfirm:   true,
			wantUninstall: 0,
			wantSubstr:    "",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			mgr := &stubManager{status: tc.status, statusErr: tc.statusErr}
			withManager(t, mgr, nil)
			withClock(t, sunset.Cutoff.Add(time.Hour)) // avoid the network in the pre-cutoff branch

			out := runUpdateCapturing(t, tc.stdin, tc.skipConfirm)

			if mgr.uninstallHits != tc.wantUninstall {
				t.Errorf("Uninstall called %d times, want %d\n%s", mgr.uninstallHits, tc.wantUninstall, out)
			}
			if tc.wantSubstr != "" && !strings.Contains(out, tc.wantSubstr) {
				t.Errorf("output %q missing %q", out, tc.wantSubstr)
			}
			if tc.wantUninstall == 0 && tc.status.Installed && tc.statusErr == nil {
				if !strings.Contains(out, "two schedulers") {
					t.Errorf("a declined prompt must still have explained the hazard:\n%s", out)
				}
			}
		})
	}
}

// TestServiceRemovalExplainsTheHazard pins the wording that justifies the
// prompt. A user asked to remove a service deserves to know why: the leftover
// unit runs its own scheduler and claims the Telegram webhook.
func TestServiceRemovalExplainsTheHazard(t *testing.T) {
	withManager(t, &stubManager{status: daemon.Status{Installed: true}}, nil)
	withClock(t, sunset.Cutoff.Add(time.Hour))

	out := runUpdateCapturing(t, "n\n", false)

	for _, want := range []string{"two schedulers", "fires twice", "Telegram webhook", "agento service uninstall"} {
		if !strings.Contains(out, want) {
			t.Errorf("prompt must mention %q:\n%s", want, out)
		}
	}
}

// TestServiceRemovalFailureWarnsAndPointsAtTheCommand keeps a failed uninstall
// non-fatal and actionable.
func TestServiceRemovalFailureWarnsAndPointsAtTheCommand(t *testing.T) {
	withManager(t, &stubManager{
		status:       daemon.Status{Installed: true},
		uninstallErr: errors.New("systemctl failed"),
	}, nil)
	withClock(t, sunset.Cutoff.Add(time.Hour))

	out := runUpdateCapturing(t, "", true)

	if !strings.Contains(out, "warning: removing the service failed") {
		t.Errorf("a failed uninstall must warn:\n%s", out)
	}
	if !strings.Contains(out, "agento service uninstall") {
		t.Errorf("the warning must name the manual command:\n%s", out)
	}
}

// TestServiceRemovalUnsupportedOSIsSilent asserts an unsupported platform
// (daemon.New returns ErrUnsupportedOS, e.g. Windows) offers nothing rather
// than warning about a service that cannot exist there.
func TestServiceRemovalUnsupportedOSIsSilent(t *testing.T) {
	withManager(t, nil, fmt.Errorf("wrap: %w", daemon.ErrUnsupportedOS))
	withClock(t, sunset.Cutoff.Add(time.Hour))

	out := runUpdateCapturing(t, "", true)

	if strings.Contains(out, "background agento service") {
		t.Errorf("no service offer is possible on an unsupported OS:\n%s", out)
	}
}
