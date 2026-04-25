package cmd

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/mattn/go-isatty"
	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/build"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/updater"
)

// skipUpdateCheckEnv is the environment variable users can set to disable the
// pre-run update check entirely (useful in CI, scripts, or any environment
// where prompting or network calls are unwanted).
const skipUpdateCheckEnv = "AGENTO_SKIP_UPDATE_CHECK"

// updateCheckSkipCommands lists subcommand names that must never trigger the
// auto-update check. The "update" command runs its own (uncached) check, and
// help/version are non-interactive metadata commands.
var updateCheckSkipCommands = map[string]struct{}{ //nolint:gochecknoglobals
	"update":     {},
	"help":       {},
	"completion": {},
	"__complete": {}, // cobra's hidden shell-completion command
}

// NewRootCmd returns the root cobra command wired with the provided AppConfig.
//
// The root command attaches a PersistentPreRunE hook that performs an opportunistic
// update check before any subcommand runs. The hook is intentionally best-effort:
// it never returns an error to the caller, so a failed network call or a missing
// cache directory cannot prevent the user's command from running.
func NewRootCmd(cfg *config.AppConfig) *cobra.Command {
	root := &cobra.Command{
		Use:     "agento",
		Short:   "Agento — AI Agents Platform",
		Long:    "A platform for running Claude agents defined in YAML configuration files.",
		Version: build.String(),
		PersistentPreRunE: func(cmd *cobra.Command, _ []string) error {
			runAutoUpdateCheck(cmd, cfg)
			return nil
		},
	}
	root.SetVersionTemplate("{{.Version}}\n")
	return root
}

// Execute is the entrypoint called from main. It loads config, wires the
// command tree, and runs the root command.
func Execute() {
	cfg, err := config.Load()
	if err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}

	root := NewRootCmd(cfg)
	root.AddCommand(NewWebCmd(cfg))
	root.AddCommand(NewAskCmd(cfg))
	root.AddCommand(NewUpdateCmd(cfg))

	if err := root.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

// runAutoUpdateCheck is the PersistentPreRunE body. It performs a cached
// update check and, on a fresh hit, prompts the user to update.
//
// The function is split out so it can be unit-tested without spinning up cobra.
// All paths return without raising errors — auto-check must never fail the user's command.
func runAutoUpdateCheck(cmd *cobra.Command, cfg *config.AppConfig) {
	if !shouldRunAutoCheck(cmd) {
		return
	}

	checker := &updater.Checker{CacheDir: cfg.DataDir}
	result, err := checker.Check(cmd.Context(), build.Version, false)
	if err != nil {
		// Includes ErrNotReleaseBuild, network errors, and timeouts. All are
		// non-fatal — the user did not ask for an update check.
		return
	}
	if !result.UpdateAvailable {
		return
	}

	promptAndMaybeUpdate(cmd.Context(), result)
}

// shouldRunAutoCheck applies all skip rules and returns true only when the
// auto-check should proceed.
func shouldRunAutoCheck(cmd *cobra.Command) bool {
	// Skip dev/unknown builds — they cannot meaningfully update.
	current := strings.TrimPrefix(build.Version, "v")
	if current == "dev" || current == "unknown" || current == "" {
		return false
	}

	// User opt-out via env var.
	if v := os.Getenv(skipUpdateCheckEnv); v == "1" || strings.EqualFold(v, "true") {
		return false
	}

	// Non-interactive (CI, pipes, redirected stdin/stdout) — no point prompting.
	if !isInteractive() {
		return false
	}

	if cmd == nil {
		return false
	}
	// Skip when no subcommand is being run (bare `agento` prints help) or when
	// help was requested via --help/-h on any subcommand.
	if !cmd.Runnable() {
		return false
	}
	// GetBool returns an error only when the flag is undefined; cobra adds
	// --help to every command at execute time, but skip-on-error is the safe
	// default if it ever isn't there.
	if helpFlag, err := cmd.Flags().GetBool("help"); err == nil && helpFlag {
		return false
	}
	// Skip subcommands where the check is irrelevant or duplicative.
	if _, skip := updateCheckSkipCommands[cmd.Name()]; skip {
		return false
	}
	return true
}

// isInteractive reports whether both stdin and stdout are connected to a TTY.
// We require both because a yes/no prompt is meaningless if either side is piped.
func isInteractive() bool {
	return isatty.IsTerminal(os.Stdin.Fd()) && isatty.IsTerminal(os.Stdout.Fd())
}

// promptAndMaybeUpdate handles the interactive flow when an update is available.
// It writes prompts to stderr so it doesn't pollute the stdout of subcommands
// that emit machine-readable output.
//
// On confirmed update, it installs the new binary and exits the process — the
// user will rerun their command against the new binary. On decline or error,
// it returns and the original command proceeds.
func promptAndMaybeUpdate(ctx context.Context, result *updater.CheckResult) {
	// Homebrew install: print instructions and continue with the user's command.
	if updater.DetectInstallMethod() == updater.InstallMethodHomebrew {
		fmt.Fprintf(os.Stderr, "\nA new version (v%s) is available.\n", result.LatestVersion)
		fmt.Fprintln(os.Stderr, updater.HomebrewUpgradeMessage)
		fmt.Fprintln(os.Stderr)
		return
	}

	fmt.Fprintf(os.Stderr, "\nA new version (v%s) is available. Update now? [y/N] ", result.LatestVersion)
	// bufio.Reader is used (rather than fmt.Scanln) so the whole line — including
	// the trailing newline — is consumed. Otherwise leftover bytes leak into the
	// stdin of the user's subcommand (e.g. `agento ask` reading from a pipe).
	reader := bufio.NewReader(os.Stdin)
	line, err := reader.ReadString('\n')
	if err != nil {
		// EOF or read error → treat as decline. Continue with the user's command.
		return
	}
	answer := strings.TrimSpace(strings.ToLower(line))
	if answer != "y" && answer != "yes" {
		return
	}

	fmt.Fprintf(os.Stderr, "Updating to %s...\n", result.LatestVersion)
	if err := updater.Install(ctx, result.LatestVersion); err != nil {
		if errors.Is(err, updater.ErrHomebrewManaged) {
			fmt.Fprintln(os.Stderr, updater.HomebrewUpgradeMessage)
			return
		}
		fmt.Fprintf(os.Stderr, "Update failed: %v\n", err)
		return
	}
	fmt.Fprintf(os.Stderr, "Updated to %s. Re-run your command to use the new version.\n", result.LatestVersion)
	// Exit cleanly so the user re-runs against the new binary. We use 0 because
	// the update succeeded — the original subcommand simply hasn't been invoked.
	os.Exit(0)
}
