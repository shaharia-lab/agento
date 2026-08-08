package daemon

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
)

// launchdLabel is the reverse-DNS label of the LaunchAgent.
const launchdLabel = "com.shaharialab.agento"

// launchd manages Agento as a macOS LaunchAgent. LaunchAgents start at user
// login (not boot) and run with the user's environment — exactly what Agento
// needs for its Claude Code CLI auth and ~/.agento data dir.
type launchd struct {
	runner  commandRunner
	homeDir string
}

// newLaunchd builds a launchd Manager for the current user.
func newLaunchd(runner commandRunner) *launchd {
	home, err := os.UserHomeDir()
	if err != nil {
		// Fall through with an empty home; plistPath will surface a clear
		// error before anything is written or shell commands run.
		home = ""
	}
	return &launchd{runner: runner, homeDir: home}
}

// newLaunchdForHome builds a launchd Manager rooted at an explicit home
// directory — used by tests.
func newLaunchdForHome(runner commandRunner, homeDir string) *launchd {
	return &launchd{runner: runner, homeDir: homeDir}
}

// plistPath returns where the LaunchAgent plist lives.
func (l *launchd) plistPath() (string, error) {
	if l.homeDir == "" {
		return "", fmt.Errorf("resolving home directory for LaunchAgents path")
	}
	return filepath.Join(l.homeDir, "Library", "LaunchAgents", launchdLabel+".plist"), nil
}

// domainTarget returns the launchd domain for the current user (gui/<uid>).
func (l *launchd) domainTarget() string {
	return fmt.Sprintf("gui/%d", currentUID())
}

// serviceTarget returns the full domain-qualified service name.
func (l *launchd) serviceTarget() string {
	return l.domainTarget() + "/" + launchdLabel
}

// Install writes the plist and bootstraps the agent (which also starts it,
// thanks to RunAtLoad). Re-installing over a running service is supported:
// the old instance is booted out first, so the port check only refuses when
// the occupant is NOT our own service.
func (l *launchd) Install(ctx context.Context, opts Options) error {
	if st, err := l.Status(ctx); err != nil || !st.Running {
		// Our service is not the one answering on the port — anything else
		// there (e.g. a foreground `agento web`) would crash-loop the install.
		if err := checkPortFree(opts.Port); err != nil {
			return err
		}
	}
	plist, err := l.plistPath()
	if err != nil {
		return err
	}
	content, err := render("agento.plist.tmpl", opts)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(plist), 0o750); err != nil {
		return fmt.Errorf("creating LaunchAgents directory: %w", err)
	}
	if err := os.MkdirAll(filepath.Dir(opts.LogPath), 0o750); err != nil {
		return fmt.Errorf("creating log directory: %w", err)
	}
	if err := os.WriteFile(plist, content, 0o600); err != nil {
		return fmt.Errorf("writing plist %s: %w", plist, err)
	}
	// Replace any previously bootstrapped copy so the new plist takes effect.
	// A bootout failure just means nothing was loaded — not an error.
	_, _ = l.runner.Run(ctx, "launchctl", "bootout", l.serviceTarget()) //nolint:errcheck // intentionally best-effort
	if _, err := l.runner.Run(ctx, "launchctl", "bootstrap", l.domainTarget(), plist); err != nil {
		return fmt.Errorf("bootstrapping LaunchAgent: %w", err)
	}
	return nil
}

// Uninstall boots the agent out and removes the plist. Both steps are
// idempotent — uninstalling a missing service is a no-op.
func (l *launchd) Uninstall(ctx context.Context) error {
	plist, err := l.plistPath()
	if err != nil {
		return err
	}
	//nolint:errcheck // best-effort: not-loaded is fine
	_, _ = l.runner.Run(ctx, "launchctl", "bootout", l.serviceTarget())
	if err := os.Remove(plist); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("removing plist %s: %w", plist, err)
	}
	return nil
}

// Start bootstraps the agent from its installed plist.
func (l *launchd) Start(ctx context.Context) error {
	plist, err := l.plistPath()
	if err != nil {
		return err
	}
	if _, statErr := os.Stat(plist); statErr != nil {
		return fmt.Errorf("service is not installed (no %s) — run `agento service install` first", plist)
	}
	if _, err := l.runner.Run(ctx, "launchctl", "bootstrap", l.domainTarget(), plist); err != nil {
		if strings.Contains(err.Error(), "service already bootstrapped") ||
			strings.Contains(err.Error(), "Bootstrap failed: 5:") {
			return nil // already running — start is idempotent
		}
		return fmt.Errorf("starting service: %w", err)
	}
	return nil
}

// Stop boots the agent out without removing the plist. Stopping a service
// that is not loaded is a no-op.
func (l *launchd) Stop(ctx context.Context) error {
	if _, err := l.runner.Run(ctx, "launchctl", "bootout", l.serviceTarget()); err != nil {
		if strings.Contains(err.Error(), "No such process") ||
			strings.Contains(err.Error(), "Boot-out failed: 5:") {
			return nil
		}
		return fmt.Errorf("stopping service: %w", err)
	}
	return nil
}

// Restart re-kicks the service in place via kickstart -k. kickstart requires
// a loaded service, so after a Stop (bootout) it falls back to Start —
// matching systemd, where `restart` also starts a stopped unit.
func (l *launchd) Restart(ctx context.Context) error {
	if _, err := l.runner.Run(ctx, "launchctl", "kickstart", "-k", l.serviceTarget()); err != nil {
		if strings.Contains(err.Error(), "Could not find service") {
			return l.Start(ctx)
		}
		return fmt.Errorf("restarting service: %w", err)
	}
	return nil
}

// launchdPIDRe matches the "pid = <n>" line of `launchctl print` output. The
// pid line only appears while the service has a live process, so a match is
// both the PID and the running signal.
var launchdPIDRe = regexp.MustCompile(`(?m)^\s*pid\s*=\s*(\d+)\s*$`) //nolint:gochecknoglobals

// Status inspects the plist on disk and the live launchd registration.
// Parse failures degrade to "not running" rather than erroring, since
// `launchctl print` output varies across macOS versions.
func (l *launchd) Status(ctx context.Context) (Status, error) {
	plist, err := l.plistPath()
	if err != nil {
		return Status{}, err
	}
	st := Status{}
	if _, statErr := os.Stat(plist); statErr == nil {
		st.Installed = true
		st.UnitPath = plist
	}
	out, printErr := l.runner.Run(ctx, "launchctl", "print", l.serviceTarget())
	if printErr != nil {
		return st, nil // not loaded — installed state still reported
	}
	// LaunchAgents have no separate enable/disable state: being registered in
	// the gui domain at all means the agent starts at login, so "loaded" is
	// the closest meaningful answer to "enabled".
	st.Enabled = true
	if m := launchdPIDRe.FindStringSubmatch(out); m != nil {
		if pid, convErr := strconv.Atoi(m[1]); convErr == nil {
			st.PID = pid
			st.Running = true
		}
	}
	return st, nil
}
