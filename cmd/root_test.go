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

	for _, v := range []string{"dev", "unknown", ""} {
		build.Version = v
		// Cobra command is present but should not influence the result.
		cmd := newTestCmd("web")
		if shouldRunAutoCheck(cmd) {
			t.Errorf("shouldRunAutoCheck() with version=%q should be false", v)
		}
	}
}

// newTestCmd builds a runnable cobra subcommand attached to a root, mirroring
// the real wiring so cmd.Runnable()/cmd.Flags() behave as they would at runtime.
func newTestCmd(name string) *cobra.Command {
	root := &cobra.Command{Use: "agento"}
	sub := &cobra.Command{
		Use: name,
		Run: func(_ *cobra.Command, _ []string) {},
	}
	root.AddCommand(sub)
	return sub
}

func TestShouldRunAutoCheck_SkipEnvVar(t *testing.T) {
	original := build.Version
	defer func() { build.Version = original }()
	build.Version = "v1.0.0"

	for _, v := range []string{"1", "true", "TRUE", "True"} {
		t.Setenv(skipUpdateCheckEnv, v)
		cmd := newTestCmd("web")
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
		cmd := newTestCmd(name)
		if shouldRunAutoCheck(cmd) {
			t.Errorf("shouldRunAutoCheck() for %q should be false", name)
		}
	}
}

// TestShouldRunAutoCheck_SkipNonRunnable ensures bare `agento` (no subcommand)
// and `--help` invocations don't trigger an interactive prompt.
func TestShouldRunAutoCheck_SkipNonRunnable(t *testing.T) {
	original := build.Version
	defer func() { build.Version = original }()
	build.Version = "v1.0.0"
	t.Setenv(skipUpdateCheckEnv, "")

	// Nil command (defensive).
	if shouldRunAutoCheck(nil) {
		t.Error("shouldRunAutoCheck(nil) should be false")
	}

	// Non-runnable command (e.g. bare `agento` printing its own help).
	bare := &cobra.Command{Use: "agento"}
	if shouldRunAutoCheck(bare) {
		t.Error("shouldRunAutoCheck() for non-runnable command should be false")
	}
}

// TestShouldRunAutoCheck_SkipHelpFlag verifies that `agento web --help` does
// not trigger an interactive prompt during help rendering.
func TestShouldRunAutoCheck_SkipHelpFlag(t *testing.T) {
	original := build.Version
	defer func() { build.Version = original }()
	build.Version = "v1.0.0"
	t.Setenv(skipUpdateCheckEnv, "")

	cmd := newTestCmd("web")
	// Cobra adds the --help flag during execution; simulate that here.
	cmd.Flags().BoolP("help", "h", false, "help for web")
	if err := cmd.Flags().Set("help", "true"); err != nil {
		t.Fatalf("setting --help flag: %v", err)
	}
	if shouldRunAutoCheck(cmd) {
		t.Error("shouldRunAutoCheck() with --help flag set should be false")
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

	cmd := newTestCmd("web")
	if shouldRunAutoCheck(cmd) {
		t.Errorf("shouldRunAutoCheck() in non-interactive test runner should be false")
	}
}
