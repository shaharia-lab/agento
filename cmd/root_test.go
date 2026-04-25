package cmd

import (
	"testing"

	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/build"
)

// TestShouldRunAutoCheck_SkipDevBuild verifies that dev/unknown builds are
// skipped regardless of any other condition.
func TestShouldRunAutoCheck_SkipDevBuild(t *testing.T) {
	original := build.Version
	defer func() { build.Version = original }()

	for _, v := range []string{"dev", "unknown", "", "vdev"} {
		// "vdev" trims to "dev" → skip
		build.Version = v
		// Cobra command is present but should not influence the result.
		cmd := &cobra.Command{Use: "web"}
		if shouldRunAutoCheck(cmd) {
			t.Errorf("shouldRunAutoCheck() with version=%q should be false", v)
		}
	}
}

func TestShouldRunAutoCheck_SkipEnvVar(t *testing.T) {
	original := build.Version
	defer func() { build.Version = original }()
	build.Version = "v1.0.0"

	for _, v := range []string{"1", "true", "TRUE", "True"} {
		t.Setenv(skipUpdateCheckEnv, v)
		cmd := &cobra.Command{Use: "web"}
		if shouldRunAutoCheck(cmd) {
			t.Errorf("shouldRunAutoCheck() with %s=%q should be false", skipUpdateCheckEnv, v)
		}
	}
}

func TestShouldRunAutoCheck_SkipCommands(t *testing.T) {
	original := build.Version
	defer func() { build.Version = original }()
	build.Version = "v1.0.0"
	t.Setenv(skipUpdateCheckEnv, "")

	// Skipped commands are skipped regardless of TTY.
	for _, name := range []string{"update", "help", "completion", "__complete"} {
		cmd := &cobra.Command{Use: name}
		if shouldRunAutoCheck(cmd) {
			t.Errorf("shouldRunAutoCheck() for %q should be false", name)
		}
	}
}

// TestShouldRunAutoCheck_NonInteractive ensures the check is skipped when
// stdin/stdout aren't TTYs. In the `go test` runner stdin is generally not a
// TTY, so this also serves as a sanity check that we never auto-prompt during
// tests.
func TestShouldRunAutoCheck_NonInteractive(t *testing.T) {
	original := build.Version
	defer func() { build.Version = original }()
	build.Version = "v1.0.0"
	t.Setenv(skipUpdateCheckEnv, "")

	cmd := &cobra.Command{Use: "web"}
	if shouldRunAutoCheck(cmd) {
		t.Errorf("shouldRunAutoCheck() in non-interactive test runner should be false")
	}
}
