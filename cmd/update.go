package cmd

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/build"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/updater"
)

// NewUpdateCmd returns the "update" subcommand that self-updates the binary.
//
// Unlike the auto-check hook, this command always performs a live network
// check (no 24-hour cache) and writes the freshly observed result back to the
// cache so the next auto-check can rely on it.
func NewUpdateCmd(cfg *config.AppConfig) *cobra.Command {
	var yes bool

	cmd := &cobra.Command{
		Use:   "update",
		Short: "Update agento to the latest release",
		Long:  "Check GitHub releases for a newer version of agento and update the binary in place.",
		RunE: func(cmd *cobra.Command, _ []string) error {
			return runUpdate(cmd.Context(), cfg, yes)
		},
	}

	cmd.Flags().BoolVarP(&yes, "yes", "y", false, "Skip confirmation prompt")
	return cmd
}

func runUpdate(ctx context.Context, cfg *config.AppConfig, skipConfirm bool) error {
	current := strings.TrimPrefix(build.Version, "v")
	if current == "dev" || current == "unknown" {
		return fmt.Errorf("cannot update a dev build; install a tagged release first")
	}

	fmt.Printf("Current version: %s\n", build.Version)
	fmt.Print("Checking for updates... ")

	checker := &updater.Checker{CacheDir: cfg.DataDir}
	result, err := checker.Check(ctx, build.Version, true) // forceFresh: explicit command bypasses cache
	if err != nil {
		if errors.Is(err, updater.ErrNotReleaseBuild) {
			fmt.Println()
			return fmt.Errorf("cannot update a non-release build (%s)", build.Version)
		}
		return fmt.Errorf("checking for updates: %w", err)
	}

	if !result.UpdateAvailable {
		fmt.Println("already up to date.")
		return nil
	}

	fmt.Printf("found %s\n", result.LatestVersion)

	// Homebrew installs cannot be self-updated; print instructions and return.
	if updater.DetectInstallMethod() == updater.InstallMethodHomebrew {
		fmt.Printf("\nA new version (v%s) is available.\n", result.LatestVersion)
		fmt.Println(updater.HomebrewUpgradeMessage)
		return nil
	}

	if !skipConfirm {
		fmt.Printf("Update to %s? [y/N] ", result.LatestVersion)
		var input string
		fmt.Scanln(&input) //nolint:errcheck,gosec
		if input != "y" && input != "Y" {
			fmt.Println("Update canceled.")
			return nil
		}
	}

	fmt.Printf("Updating to %s...\n", result.LatestVersion)
	if err := updater.Install(ctx, result.LatestVersion); err != nil {
		if errors.Is(err, updater.ErrHomebrewManaged) {
			// Defensive: the earlier branch already handled this, but keep the
			// guard so a future refactor cannot accidentally call Install on a
			// Homebrew binary.
			fmt.Println(updater.HomebrewUpgradeMessage)
			return nil
		}
		return fmt.Errorf("updating: %w", err)
	}

	fmt.Printf("Updated to %s. Restart agento to use the new version.\n", result.LatestVersion)

	// Belt-and-suspenders: ensure stdout is flushed before the process exits in
	// any embedded environment that may not auto-flush.
	_ = os.Stdout.Sync() //nolint:errcheck
	return nil
}
