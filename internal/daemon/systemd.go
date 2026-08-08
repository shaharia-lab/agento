package daemon

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"
)

// systemdUnitName is the unit file name registered with systemd --user.
const systemdUnitName = "agento.service"

// systemd manages Agento as a systemd user unit. User units run with the
// user's environment and (with lingering enabled) survive logout.
type systemd struct {
	runner  commandRunner
	homeDir string
}

// newSystemd builds a systemd Manager for the current user.
func newSystemd(runner commandRunner) *systemd {
	home, err := os.UserHomeDir()
	if err != nil {
		home = ""
	}
	return &systemd{runner: runner, homeDir: home}
}

// newSystemdForHome builds a systemd Manager rooted at an explicit home
// directory — used by tests.
func newSystemdForHome(runner commandRunner, homeDir string) *systemd {
	return &systemd{runner: runner, homeDir: homeDir}
}

// unitPath returns where the user unit file lives.
func (s *systemd) unitPath() (string, error) {
	if s.homeDir == "" {
		return "", fmt.Errorf("resolving home directory for systemd user unit path")
	}
	return filepath.Join(s.homeDir, ".config", "systemd", "user", systemdUnitName), nil
}

// Install renders the unit, reloads systemd, enables + starts the service,
// and enables lingering (best effort) so the unit survives logout on
// headless/SSH machines. Re-installing over a running service is supported,
// so the port check only refuses when the occupant is NOT our own service.
func (s *systemd) Install(ctx context.Context, opts Options) error {
	if st, err := s.Status(ctx); err != nil || !st.Running {
		// Our service is not the one answering on the port — anything else
		// there (e.g. a foreground `agento web`) would crash-loop the install.
		if err := checkPortFree(opts.Port); err != nil {
			return err
		}
	}
	unit, err := s.unitPath()
	if err != nil {
		return err
	}
	content, err := render("agento.service.tmpl", opts)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(unit), 0o750); err != nil {
		return fmt.Errorf("creating systemd user directory: %w", err)
	}
	if err := os.MkdirAll(filepath.Dir(opts.LogPath), 0o750); err != nil {
		return fmt.Errorf("creating log directory: %w", err)
	}
	if err := os.WriteFile(unit, content, 0o600); err != nil {
		return fmt.Errorf("writing unit %s: %w", unit, err)
	}
	if _, err := s.runner.Run(ctx, "systemctl", "--user", "daemon-reload"); err != nil {
		return fmt.Errorf("systemctl daemon-reload: %w", err)
	}
	if _, err := s.runner.Run(ctx, "systemctl", "--user", "enable", "--now", systemdUnitName); err != nil {
		return fmt.Errorf("enabling service: %w", err)
	}
	s.enableLinger(ctx)
	return nil
}

// enableLinger turns on lingering for the current user so the user unit keeps
// running after logout. Failures are a warning, not fatal — on a desktop
// machine with an always-open session, lingering is unnecessary.
func (s *systemd) enableLinger(ctx context.Context) {
	username := os.Getenv("USER")
	if username == "" {
		if u, err := user.Current(); err == nil {
			username = u.Username
		}
	}
	if username == "" {
		fmt.Fprintln(os.Stderr, "warning: could not determine username to enable lingering; "+
			"the service will stop at logout — run `loginctl enable-linger $USER` to fix")
		return
	}
	if _, err := s.runner.Run(ctx, "loginctl", "enable-linger", username); err != nil {
		fmt.Fprintf(os.Stderr, "warning: enabling lingering failed (%v); "+
			"the service will stop at logout — run `sudo loginctl enable-linger %s` to fix\n", err, username)
	}
}

// Uninstall disables + stops the unit and removes the unit file, then reloads
// systemd so no registration remains. Idempotent throughout.
func (s *systemd) Uninstall(ctx context.Context) error {
	unit, err := s.unitPath()
	if err != nil {
		return err
	}
	// enable --now is a no-op when the unit is already disabled/stopped.
	//nolint:errcheck // best-effort
	_, _ = s.runner.Run(ctx, "systemctl", "--user", "disable", "--now", systemdUnitName)
	if err := os.Remove(unit); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("removing unit %s: %w", unit, err)
	}
	// Reload so systemd forgets the removed unit. Failure here is benign.
	//nolint:errcheck // best-effort
	_, _ = s.runner.Run(ctx, "systemctl", "--user", "daemon-reload")
	return nil
}

// Start starts the installed unit. Idempotent.
func (s *systemd) Start(ctx context.Context) error {
	if _, err := s.runner.Run(ctx, "systemctl", "--user", "start", systemdUnitName); err != nil {
		return fmt.Errorf("starting service: %w", err)
	}
	return nil
}

// Stop stops the unit without disabling it. Idempotent.
func (s *systemd) Stop(ctx context.Context) error {
	if _, err := s.runner.Run(ctx, "systemctl", "--user", "stop", systemdUnitName); err != nil {
		return fmt.Errorf("stopping service: %w", err)
	}
	return nil
}

// Restart restarts the unit in place.
func (s *systemd) Restart(ctx context.Context) error {
	if _, err := s.runner.Run(ctx, "systemctl", "--user", "restart", systemdUnitName); err != nil {
		return fmt.Errorf("restarting service: %w", err)
	}
	return nil
}

// Status reads LoadState/UnitFileState/ActiveState/MainPID from
// `systemctl --user show`, matching by property name (not position) because
// output varies across systemd versions.
func (s *systemd) Status(ctx context.Context) (Status, error) {
	unit, err := s.unitPath()
	if err != nil {
		return Status{}, err
	}
	st := Status{}
	if _, statErr := os.Stat(unit); statErr == nil {
		st.Installed = true
		st.UnitPath = unit
	}
	out, showErr := s.runner.Run(ctx, "systemctl", "--user", "show", systemdUnitName,
		"--property=LoadState,UnitFileState,ActiveState,MainPID")
	if showErr != nil {
		return st, nil // unit unknown to systemd — installed state still reported
	}
	return parseSystemdShow(st, out), nil
}

// parseSystemdShow folds the key=value lines of `systemctl show` into Status.
func parseSystemdShow(st Status, out string) Status {
	props := map[string]string{}
	for _, line := range strings.Split(out, "\n") {
		key, value, ok := strings.Cut(strings.TrimSpace(line), "=")
		if ok {
			props[key] = value
		}
	}
	if props["LoadState"] == "loaded" {
		st.Installed = true
	}
	switch props["UnitFileState"] {
	case "enabled", "enabled-runtime", "alias", "static":
		st.Enabled = true
	}
	if props["ActiveState"] == "active" {
		st.Running = true
	}
	if pid, err := strconv.Atoi(props["MainPID"]); err == nil && pid > 0 {
		st.PID = pid
	}
	return st
}
