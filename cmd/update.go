package cmd

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"os"
	"runtime"
	"strings"
	"time"

	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/build"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/daemon"
	"github.com/shaharia-lab/agento/internal/sunset"
	"github.com/shaharia-lab/agento/internal/updater"
)

// NewUpdateCmd returns the "update" subcommand.
//
// It no longer self-updates the Go binary. The Go/web build is being retired in
// favor of Agento Desktop, so this command resolves the current desktop
// release and hands the user the direct installer link for their platform.
//
// Retiring the command rather than deleting it is deliberate: `agento update`
// is the one place a user of the old build already goes when they want to be
// current, so it is the most reliable channel we have for telling them where
// "current" now lives.
func NewUpdateCmd(cfg *config.AppConfig) *cobra.Command {
	var yes bool
	var noRestart bool

	cmd := &cobra.Command{
		Use:   "update",
		Short: "Show how to move to Agento Desktop (this build is no longer self-updating)",
		Long: "The Go/web build of Agento is being retired. This command resolves the current " +
			"Agento Desktop release and prints the download for your platform. It also offers to " +
			"remove a leftover background service, which would otherwise run a second scheduler " +
			"alongside the desktop app.",
		RunE: func(cmd *cobra.Command, _ []string) error {
			return runUpdate(cmd.Context(), cfg, yes)
		},
	}

	cmd.Flags().BoolVarP(&yes, "yes", "y", false, "Accept the service-removal prompt without asking")
	cmd.Flags().BoolVar(&noRestart, "no-restart", false, "No longer used; kept so existing scripts do not break")
	// The flag is retained rather than removed so a script or a service unit
	// passing it keeps working instead of failing on an unknown flag.
	_ = cmd.Flags().MarkDeprecated("no-restart", "this build no longer self-updates") //nolint:errcheck

	return cmd
}

// outf and outln write terminal output to w. The write error is deliberately
// discarded: this is the user's console, and there is no recovery available if
// writing to it fails — nor any way to report the failure.
func outf(w io.Writer, format string, args ...any) {
	_, _ = fmt.Fprintf(w, format, args...) //nolint:errcheck
}

func outln(w io.Writer, args ...any) {
	_, _ = fmt.Fprintln(w, args...) //nolint:errcheck
}

// newServiceManager is a seam over daemon.New so tests can substitute a stub
// Manager without touching a real init system.
var newServiceManager = daemon.New

// updateNow is a seam over time.Now so tests can drive the cutoff branch.
var updateNow = time.Now

func runUpdate(ctx context.Context, cfg *config.AppConfig, skipConfirm bool) error {
	return runUpdateTo(ctx, os.Stdout, os.Stdin, cfg, skipConfirm)
}

// runUpdateTo is runUpdate with its streams injected, so the whole flow is
// testable without touching the process's stdio.
func runUpdateTo(ctx context.Context, out io.Writer, in io.Reader, cfg *config.AppConfig, skipConfirm bool) error {
	outf(out, "Current version: %s\n\n", build.Version)

	// Past the cutoff there is nothing to offer, so say so and stop. Note what
	// this does NOT do: the binary keeps working, every other command is
	// untouched, and nothing here refuses to run. Only updating stops.
	if sunset.Passed(updateNow()) {
		outln(out, sunset.MigrationMessage())
	} else {
		printDesktopMigration(ctx, out)
	}

	offerServiceRemoval(ctx, out, in, cfg, skipConfirm)
	return nil
}

// printDesktopMigration resolves the current desktop release and prints the
// platform-specific download. Resolution failures degrade to the releases page
// rather than failing the command — the user asked how to get current, and
// "here is the page" always answers that.
func printDesktopMigration(ctx context.Context, out io.Writer) {
	outln(out, sunset.FullNotice())
	outln(out)

	rel, err := updater.ResolveDesktopRelease(ctx)
	if err != nil {
		outf(out, "Download Agento Desktop: %s\n", updater.ReleasesPageURL())
		return
	}

	outf(out, "Latest Agento Desktop: %s\n", rel.Version)
	if rel.DownloadURL != "" {
		outf(out, "  Download   %s\n", rel.DownloadURL)
	} else {
		outf(out, "  Download   no installer is published for %s/%s — see the release page\n",
			runtime.GOOS, runtime.GOARCH)
	}
	outf(out, "  Release    %s\n", rel.ReleasePage)
}

// offerServiceRemoval checks for a leftover `agento service` unit and offers to
// remove it.
//
// This matters more than it looks. A leftover launchd/systemd unit holds :8990,
// runs its OWN scheduler — so every scheduled task fires twice once the desktop
// app is installed — and re-registers the Telegram webhook under whichever
// instance started last. None of that is visible to the user.
//
// It is an offer, never an action. Removing someone's service unit unprompted
// is destructive, so a declined prompt, an unreadable answer, and an
// unsupported platform all leave the unit exactly where it is.
func offerServiceRemoval(ctx context.Context, out io.Writer, in io.Reader, cfg *config.AppConfig, skipConfirm bool) {
	mgr, err := newServiceManager(cfg)
	if err != nil {
		// Unsupported OS or a daemon-layer error: there is no managed service
		// we can see, so there is nothing to offer.
		return
	}
	status, err := mgr.Status(ctx)
	if err != nil || !status.Installed {
		return
	}

	outln(out)
	outln(out, "A background agento service is still installed on this machine.")
	outln(out, "Leaving it in place alongside Agento Desktop means two schedulers:")
	outln(out, "every scheduled task fires twice, and the Telegram webhook is claimed")
	outln(out, "by whichever instance started last.")

	if !skipConfirm && !confirm(out, in, "Remove the agento background service now? [y/N] ") {
		outln(out, "Left in place. Remove it later with 'agento service uninstall'.")
		return
	}

	if err := mgr.Uninstall(ctx); err != nil {
		outf(out, "warning: removing the service failed (%v). Run 'agento service uninstall' manually\n", err)
		return
	}
	outln(out, "Removed the agento background service.")
}

// confirm reads a yes/no answer. A read error or EOF is a decline, so a piped
// or closed stdin can never be mistaken for consent to remove a service unit.
func confirm(out io.Writer, in io.Reader, prompt string) bool {
	outf(out, "%s", prompt)
	line, err := bufio.NewReader(in).ReadString('\n')
	if err != nil && line == "" {
		outln(out)
		return false
	}
	answer := strings.TrimSpace(strings.ToLower(line))
	return answer == "y" || answer == "yes"
}
