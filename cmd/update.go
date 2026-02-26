package cmd

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/creativeprojects/go-selfupdate"
	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/build"
)

// NewUpdateCmd returns the "update" subcommand that self-updates the binary.
func NewUpdateCmd() *cobra.Command {
	var yes bool

	cmd := &cobra.Command{
		Use:   "update",
		Short: "Update agento to the latest release",
		Long:  "Check GitHub releases for a newer version of agento and update the binary in place.",
		RunE: func(cmd *cobra.Command, _ []string) error {
			return runUpdate(yes)
		},
	}

	cmd.Flags().BoolVarP(&yes, "yes", "y", false, "Skip confirmation prompt")
	return cmd
}

func runUpdate(skipConfirm bool) error {
	current := strings.TrimPrefix(build.Version, "v")
	if current == "dev" || current == "unknown" {
		return fmt.Errorf("cannot update a dev build; install a tagged release first")
	}

	fmt.Printf("Current version: %s\n", build.Version)
	fmt.Print("Checking for updates... ")

	updater, err := selfupdate.NewUpdater(selfupdate.Config{})
	if err != nil {
		return fmt.Errorf("creating updater: %w", err)
	}

	release, found, err := updater.DetectLatest(context.Background(), selfupdate.ParseSlug("shaharia-lab/agento"))
	if err != nil {
		return fmt.Errorf("checking for updates: %w", err)
	}

	if !found || !release.GreaterThan(current) {
		fmt.Println("already up to date.")
		return nil
	}

	fmt.Printf("found %s\n", release.Version())

	if !skipConfirm {
		fmt.Printf("Update to %s? [y/N] ", release.Version())
		var input string
		fmt.Scanln(&input) //nolint:errcheck,gosec
		if input != "y" && input != "Y" {
			fmt.Println("Update canceled.")
			return nil
		}
	}

	exe, err := os.Executable()
	if err != nil {
		return fmt.Errorf("finding current executable: %w", err)
	}

	fmt.Printf("Updating to %s...\n", release.Version())
	if err := updater.UpdateTo(context.Background(), release, exe); err != nil {
		return fmt.Errorf("updating: %w", err)
	}

	fmt.Printf("Updated to %s. Restart agento to use the new version.\n", release.Version())
	return nil
}
