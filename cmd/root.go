package cmd

import (
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/mattn/go-isatty"
	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/build"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/sunset"
	"github.com/shaharia-lab/agento/internal/updater"
)

// skipUpdateCheckEnv is the environment variable users can set to disable the
// pre-run update check entirely (useful in CI, scripts, or any environment
// where prompting or network calls are unwanted).
const skipUpdateCheckEnv = "AGENTO_SKIP_UPDATE_CHECK"

// updateCheckSkipCommands lists subcommand names that must never trigger the
// auto-update check. The "update" command runs its own (uncached) check,
// help/version are non-interactive metadata commands, and "service" manages
// the background daemon — it must stay fast and side-effect free.
var updateCheckSkipCommands = map[string]struct{}{ //nolint:gochecknoglobals
	"update":     {},
	"help":       {},
	"completion": {},
	"service":    {},
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
		Short:   "Agento: AI Agents Platform",
		Long:    "A platform for running Claude agents defined in YAML configuration files.",
		Version: build.String(),
		PersistentPreRunE: func(cmd *cobra.Command, _ []string) error {
			maybePrintSunsetNotice(cmd, cfg)
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
	root.AddCommand(NewServiceCmd(cfg))

	if err := root.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

// sunsetSkipCommands lists subcommands that must never print the one-line
// sunset notice, and why each one is on the list.
//
// The list is deliberately far shorter than updateCheckSkipCommands, and it
// skips for two different reasons:
//
//   - `completion` and `__complete`: cobra's shell integration parses their
//     stdout, and although the notice goes to stderr, a shell that merges the
//     two streams would ingest it as a completion candidate.
//   - `web` and `update`: both print the FULL notice themselves, so the
//     one-liner would be an immediate duplicate of a longer message the user is
//     about to read anyway. This skips a redundant line, not a user.
//
// Everything else — `service`, `help`, a piped `ask`, a cron-driven run — still
// prints, because none of those has a notice of its own and they are exactly
// the installs a TTY-gated notice would miss.
var sunsetSkipCommands = map[string]struct{}{ //nolint:gochecknoglobals
	"completion": {},
	"__complete": {},
	"web":        {},
	"update":     {},
}

// sunsetNow is a seam over time.Now so tests can drive the rate limit.
var sunsetNow = time.Now //nolint:gochecknoglobals

// maybePrintSunsetNotice prints the retirement notice ahead of the user's
// command, at most once a day.
//
// It deliberately does NOT reuse shouldRunAutoCheck's gating. That helper
// requires an interactive TTY on both stdin and stdout and skips `update` and
// `service` — reasonable for a prompt that expects an answer, but wrong for a
// one-way notice, because it would exclude exactly the people who most need to
// see it: anyone running agento from a script, from a cron job, or under
// `agento service`.
//
// The notice is static and offline. It makes no network call, so it cannot slow
// a command down, cannot time out, and cannot break when the desktop releases
// stop carrying their tag prefix.
func maybePrintSunsetNotice(cmd *cobra.Command, cfg *config.AppConfig) {
	if !shouldPrintSunsetNotice(cmd) {
		return
	}
	now := sunsetNow()
	if !sunset.ShouldPrint(cfg.DataDir, now) {
		return
	}
	fmt.Fprintf(os.Stderr, "\n%s\n\n", sunset.Notice())
	// Best-effort: a failed stamp only means the notice shows again sooner.
	_ = sunset.Stamp(cfg.DataDir, now) //nolint:errcheck
}

// shouldPrintSunsetNotice applies the notice's own skip rules.
func shouldPrintSunsetNotice(cmd *cobra.Command) bool {
	// A dev build is not an install anyone needs to migrate.
	if build.IsDevBuild(build.Version) {
		return false
	}
	// Honor the existing opt-out. It already means "do not talk to me about
	// versions", which is the same wish.
	if v := os.Getenv(skipUpdateCheckEnv); v == "1" || strings.EqualFold(v, "true") {
		return false
	}
	if cmd == nil || !cmd.Runnable() {
		return false
	}
	if _, skip := sunsetSkipCommands[cmd.Name()]; skip {
		return false
	}
	return true
}

// runAutoUpdateCheck is the PersistentPreRunE body. It performs a cached
// update check and, on a fresh hit, announces the newer release.
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

	announceUpdate(result)
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

	// Non-interactive (CI, pipes, redirected stdin/stdout). The announcement no
	// longer prompts, but a scripted run has already had the sunset notice —
	// which is the message that matters now — so this stays a skip rather than
	// becoming a second unsolicited line in every pipeline.
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
func isInteractive() bool {
	return isatty.IsTerminal(os.Stdin.Fd()) && isatty.IsTerminal(os.Stdout.Fd())
}

// announceUpdate tells the user a newer release exists and points them at
// `agento update`. It writes to stderr so it doesn't pollute the stdout of
// subcommands that emit machine-readable output.
//
// It deliberately does NOT install anything. This hook used to prompt and then
// replace the running binary in place, which cannot survive the retirement of
// this build: `agento update` no longer self-updates, and a binary that refuses
// to update itself on request while quietly doing it behind an unrelated command
// is incoherent. Removing the install path here is what makes "this build no
// longer self-updates" true of the binary rather than only of one subcommand.
//
// The check itself is kept because the version it reports is still useful
// information, and because it is the surface through which a user on an older
// build sees this release's title at all.
func announceUpdate(result *updater.CheckResult) {
	fmt.Fprintf(os.Stderr, "\nA newer release (v%s) is available: %s\n", result.LatestVersion, result.ReleaseURL)
	fmt.Fprintf(os.Stderr, "Run 'agento update' to see how to move to Agento Desktop.\n\n")
}
