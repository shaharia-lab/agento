package cmd

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"runtime"
	"strings"
	"time"

	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/daemon"
)

// NewServiceCmd returns the "service" command group that installs and manages
// Agento as a user-level background service (launchd on macOS, systemd user
// units on Linux).
func NewServiceCmd(cfg *config.AppConfig) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "service",
		Short: "Install and manage Agento as a background service",
		Long: `Install Agento (agento web --no-browser) as a user-level background service
so it survives logout, reboot, and crashes without a terminal staying open.

macOS uses a LaunchAgent (starts at login, auto-restarts on crash).
Linux uses a systemd user unit (Restart=on-failure; lingering is enabled so
the service keeps running after logout). Windows is not supported.`,
	}
	cmd.AddCommand(newServiceInstallCmd(cfg))
	cmd.AddCommand(newServiceSimpleCmd(cfg, "uninstall", "Stop, disable, and remove the Agento background service"))
	cmd.AddCommand(newServiceSimpleCmd(cfg, "start", "Start the installed Agento service"))
	cmd.AddCommand(newServiceSimpleCmd(cfg, "stop", "Stop the Agento service without removing it"))
	cmd.AddCommand(newServiceSimpleCmd(cfg, "restart", "Restart the Agento service"))
	cmd.AddCommand(newServiceStatusCmd(cfg))
	cmd.AddCommand(newServiceLogsCmd(cfg))
	for _, sub := range cmd.Commands() {
		// Failures are runtime conditions ("service is not running"), not
		// usage errors — keep the output to the single error line the root
		// Execute already prints; no usage dump, no cobra re-print.
		sub.SilenceUsage = true
		sub.SilenceErrors = true
	}
	return cmd
}

// newServiceInstallCmd builds `agento service install`.
func newServiceInstallCmd(cfg *config.AppConfig) *cobra.Command {
	return &cobra.Command{
		Use:   "install",
		Short: "Install Agento as a background service (enable + start)",
		RunE: func(cmd *cobra.Command, _ []string) error {
			return runServiceInstall(cmd.Context(), cfg)
		},
	}
}

// newServiceSimpleCmd builds the lifecycle subcommands that map 1:1 onto
// Manager methods (uninstall/start/stop/restart).
func newServiceSimpleCmd(cfg *config.AppConfig, use, short string) *cobra.Command {
	return &cobra.Command{
		Use:   use,
		Short: short,
		RunE: func(cmd *cobra.Command, _ []string) error {
			return runServiceLifecycle(cmd.Context(), cfg, use)
		},
	}
}

// newServiceStatusCmd builds `agento service status`.
func newServiceStatusCmd(cfg *config.AppConfig) *cobra.Command {
	return &cobra.Command{
		Use:   "status",
		Short: "Show installed/enabled/running state, PID, URL, and log path",
		RunE: func(cmd *cobra.Command, _ []string) error {
			return runServiceStatus(cmd.Context(), cfg)
		},
	}
}

// newServiceLogsCmd builds `agento service logs`.
func newServiceLogsCmd(cfg *config.AppConfig) *cobra.Command {
	var follow bool
	var lines int
	cmd := &cobra.Command{
		Use:   "logs",
		Short: "Print the service log (stdout/stderr of the daemonized process)",
		RunE: func(cmd *cobra.Command, _ []string) error {
			return runServiceLogs(cmd.Context(), daemon.ServiceLogPath(cfg), follow, lines)
		},
	}
	cmd.Flags().BoolVarP(&follow, "follow", "f", false, "Follow the log as new lines are written")
	cmd.Flags().IntVarP(&lines, "lines", "n", 50, "Number of trailing lines to print")
	return cmd
}

// newManager resolves the platform Manager or returns a user-facing
// unsupported-OS error.
func newManager(cfg *config.AppConfig) (daemon.Manager, error) {
	mgr, err := daemon.New(cfg)
	if err != nil {
		if errors.Is(err, daemon.ErrUnsupportedOS) {
			return nil, fmt.Errorf("`agento service` is not supported on %s (macOS and Linux only)", runtime.GOOS)
		}
		return nil, err
	}
	return mgr, nil
}

// runServiceInstall renders and installs the unit/plist, then starts the
// service, and prints where things landed plus the platform caveat.
func runServiceInstall(ctx context.Context, cfg *config.AppConfig) error {
	mgr, err := newManager(cfg)
	if err != nil {
		return err
	}
	opts, err := daemon.DefaultOptions(cfg)
	if err != nil {
		return err
	}
	if err := mgr.Install(ctx, opts); err != nil {
		if errors.Is(err, daemon.ErrAlreadyRunning) {
			return fmt.Errorf("%v", err)
		}
		return fmt.Errorf("installing service: %w", err)
	}
	fmt.Println("Agento service installed and started.")
	fmt.Printf("  URL:      http://localhost:%d\n", cfg.Port)
	fmt.Printf("  Logs:     %s\n", opts.LogPath)
	fmt.Printf("  Data dir: %s\n", cfg.DataDir)
	if runtime.GOOS == "darwin" {
		fmt.Println("Note: LaunchAgents start at login, not at boot — log in once after a restart.")
	} else {
		fmt.Println("Note: lingering was enabled so the service keeps running after logout.")
	}
	if !waitForRunning(ctx, mgr, 3*time.Second) {
		fmt.Println("warning: the service is not answering yet — run `agento service status` " +
			"and `agento service logs` to diagnose, or `agento service restart` to retry")
	}
	return nil
}

// waitForRunning polls the service status until it reports running or the
// timeout elapses. Right after install the process may need a moment to come
// up, so a single check would produce false alarms.
func waitForRunning(ctx context.Context, mgr daemon.Manager, timeout time.Duration) bool {
	deadline := time.Now().Add(timeout)
	for {
		st, err := mgr.Status(ctx)
		if err == nil && st.Running {
			return true
		}
		if time.Now().After(deadline) {
			return false
		}
		select {
		case <-ctx.Done():
			return false
		case <-time.After(500 * time.Millisecond):
		}
	}
}

// runServiceLifecycle dispatches uninstall/start/stop/restart to the Manager.
func runServiceLifecycle(ctx context.Context, cfg *config.AppConfig, action string) error {
	mgr, err := newManager(cfg)
	if err != nil {
		return err
	}
	switch action {
	case "uninstall":
		err = mgr.Uninstall(ctx)
	case "start":
		err = mgr.Start(ctx)
	case "stop":
		err = mgr.Stop(ctx)
	case "restart":
		err = mgr.Restart(ctx)
	default:
		return fmt.Errorf("unknown service action %q", action)
	}
	if err != nil {
		return fmt.Errorf("%s: %w", action, err)
	}
	fmt.Printf("Agento service %s: done.\n", action)
	return nil
}

// runServiceStatus prints the service state and exits non-zero when the
// service is not running, so the command is scriptable.
func runServiceStatus(ctx context.Context, cfg *config.AppConfig) error {
	mgr, err := newManager(cfg)
	if err != nil {
		return err
	}
	st, err := mgr.Status(ctx)
	if err != nil {
		return fmt.Errorf("reading service status: %w", err)
	}
	printServiceStatus(st, cfg)
	if !st.Running {
		return errors.New("service is not running")
	}
	return nil
}

// printServiceStatus renders the Status fields plus the URL and log path
// derived from config, in a stable, greppable shape.
func printServiceStatus(st daemon.Status, cfg *config.AppConfig) {
	fmt.Printf("Installed: %s\n", yesNo(st.Installed))
	fmt.Printf("Enabled:   %s\n", yesNo(st.Enabled))
	fmt.Printf("Running:   %s\n", yesNo(st.Running))
	if st.PID > 0 {
		fmt.Printf("PID:       %d\n", st.PID)
	}
	if st.UnitPath != "" {
		fmt.Printf("Unit:      %s\n", st.UnitPath)
	}
	fmt.Printf("URL:       http://localhost:%d\n", cfg.Port)
	fmt.Printf("Log:       %s\n", daemon.ServiceLogPath(cfg))
}

func yesNo(v bool) string {
	if v {
		return "yes"
	}
	return "no"
}

// runServiceLogs prints the trailing lines of the service log, optionally
// following it until ctx is canceled (Ctrl-C).
func runServiceLogs(ctx context.Context, logPath string, follow bool, lines int) error {
	if lines < 0 {
		return fmt.Errorf("--lines must be >= 0")
	}
	f, err := os.Open(logPath) //nolint:gosec // path is derived from the user's own data dir
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("no service log at %s — is the service installed and has it run yet?", logPath)
		}
		return fmt.Errorf("opening service log: %w", err)
	}
	defer func() { _ = f.Close() }() //nolint:errcheck // read-only handle — close errors are irrelevant

	printed, err := tailLines(f, lines)
	if err != nil {
		return err
	}
	fmt.Print(string(printed))

	if !follow {
		return nil
	}
	// Seek past what was already printed: the bufio reader in followLog reads
	// ahead of the offset the last line was printed at, so without this the
	// follow loop would re-print a buffered chunk.
	if _, err := f.Seek(0, io.SeekEnd); err != nil {
		return fmt.Errorf("following service log: %w", err)
	}
	return followLog(ctx, f)
}

// tailLines returns up to n trailing lines of f. n == 0 prints nothing; a
// negative n is rejected by the caller. The log is read whole — service logs
// are small, and this keeps the implementation simple.
func tailLines(f *os.File, n int) ([]byte, error) {
	if n == 0 {
		return nil, nil
	}
	if _, err := f.Seek(0, io.SeekStart); err != nil {
		return nil, fmt.Errorf("reading service log: %w", err)
	}
	all, err := io.ReadAll(f)
	if err != nil {
		return nil, fmt.Errorf("reading service log: %w", err)
	}
	parts := strings.Split(strings.TrimRight(string(all), "\n"), "\n")
	if len(parts) > n {
		parts = parts[len(parts)-n:]
	}
	if len(parts) == 1 && parts[0] == "" {
		return nil, nil
	}
	return []byte(strings.Join(parts, "\n") + "\n"), nil
}

// followLog streams newly appended log lines until ctx is canceled. It polls
// rather than using fsnotify to stay dependency-free.
func followLog(ctx context.Context, f *os.File) error {
	reader := bufio.NewReader(f)
	for {
		line, err := reader.ReadString('\n')
		if line != "" {
			fmt.Print(line)
		}
		if err == nil {
			continue
		}
		if !errors.Is(err, io.EOF) {
			return fmt.Errorf("following service log: %w", err)
		}
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(500 * time.Millisecond):
		}
	}
}
