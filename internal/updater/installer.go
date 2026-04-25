package updater

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"

	"github.com/creativeprojects/go-selfupdate"
)

// InstallMethod describes how the running binary was installed, which determines
// the correct upgrade path.
type InstallMethod int

const (
	// InstallMethodSelfUpdate means the binary can be replaced in place using
	// go-selfupdate (works on Linux, macOS, and Windows for direct downloads).
	InstallMethodSelfUpdate InstallMethod = iota

	// InstallMethodHomebrew means the binary lives under a Homebrew prefix and
	// must be upgraded via `brew upgrade agento` to keep Homebrew's metadata in sync.
	InstallMethodHomebrew
)

// homebrewPathPrefixes lists path prefixes that uniquely identify a
// Homebrew-managed binary across the three supported install layouts.
//
// We deliberately match by prefix on the resolved executable path:
//   - macOS Intel:    /usr/local/Cellar/...   or  /usr/local/opt/...
//   - macOS Silicon:  /opt/homebrew/Cellar/...   or  /opt/homebrew/opt/...
//   - Linuxbrew:      /home/linuxbrew/.linuxbrew/...
//
// Note: prefixes use forward slashes intentionally — Homebrew does not exist on
// Windows, so checks against these prefixes are no-ops on a Windows binary path
// (which would be drive-letter rooted with backslashes).
var homebrewPathPrefixes = []string{ //nolint:gochecknoglobals
	"/usr/local/Cellar/",
	"/usr/local/opt/",
	"/opt/homebrew/Cellar/",
	"/opt/homebrew/opt/",
	"/home/linuxbrew/.linuxbrew/",
}

// DetectInstallMethod inspects the path of the running executable and returns
// the install method to use. It evaluates symlinks so a Homebrew binary linked
// into /usr/local/bin still resolves to its Cellar prefix.
//
// On any error resolving the executable, DetectInstallMethod falls back to
// InstallMethodSelfUpdate — this matches user expectation that an unknown layout
// should still be upgradable in place.
func DetectInstallMethod() InstallMethod {
	// Homebrew is not available on Windows; skip the path inspection entirely.
	if runtime.GOOS == "windows" {
		return InstallMethodSelfUpdate
	}

	exe, err := os.Executable()
	if err != nil {
		return InstallMethodSelfUpdate
	}
	resolved, err := filepath.EvalSymlinks(exe)
	if err != nil {
		// EvalSymlinks may fail on permissions or a missing target. Fall back
		// to the un-resolved path so we still attempt prefix matching.
		resolved = exe
	}
	if isHomebrewPath(resolved) {
		return InstallMethodHomebrew
	}
	return InstallMethodSelfUpdate
}

// isHomebrewPath reports whether path is rooted under a known Homebrew prefix.
// The check uses forward-slash prefixes so Windows paths (which use backslashes
// and drive letters) cannot accidentally match.
func isHomebrewPath(path string) bool {
	// Normalize to forward slashes for prefix matching. On Unix this is a no-op.
	normalized := filepath.ToSlash(path)
	for _, prefix := range homebrewPathPrefixes {
		if strings.HasPrefix(normalized, prefix) {
			return true
		}
	}
	return false
}

// HomebrewUpgradeMessage is the user-facing instruction printed when an update
// is available for a Homebrew-managed install. It is exported so callers can
// log or format it however they need.
const HomebrewUpgradeMessage = "You installed agento via Homebrew. Run: brew upgrade agento"

// ErrHomebrewManaged is returned by Install when the binary is Homebrew-managed
// and self-update was attempted. Callers should print HomebrewUpgradeMessage instead.
var ErrHomebrewManaged = errors.New("binary is managed by Homebrew; use 'brew upgrade agento'")

// Install replaces the running binary with the release matching latestVersion.
//
// Returns ErrHomebrewManaged when DetectInstallMethod reports a Homebrew install;
// in that case the caller is responsible for printing user instructions and
// must not treat the error as fatal.
//
// On success the caller should print a "restart agento" message — the new
// binary is in place but the running process is still the old one.
func Install(ctx context.Context, latestVersion string) error {
	if DetectInstallMethod() == InstallMethodHomebrew {
		return ErrHomebrewManaged
	}

	exe, err := os.Executable()
	if err != nil {
		return fmt.Errorf("finding current executable: %w", err)
	}

	upd, err := selfupdate.NewUpdater(selfupdate.Config{})
	if err != nil {
		return fmt.Errorf("creating updater: %w", err)
	}

	release, found, err := upd.DetectLatest(ctx, selfupdate.ParseSlug(repoSlug))
	if err != nil {
		return fmt.Errorf("detecting release: %w", err)
	}
	if !found {
		return fmt.Errorf("release %s not found on GitHub", latestVersion)
	}

	// Guard against the cache-vs-live race: between Check (cache hit) and Install,
	// a newer release may have landed on GitHub. Refuse to install a different
	// version than the user agreed to — they should re-run and confirm.
	if want := strings.TrimPrefix(latestVersion, "v"); want != "" {
		if got := strings.TrimPrefix(release.Version(), "v"); got != want {
			return fmt.Errorf(
				"release on GitHub (%s) no longer matches the version you confirmed (%s); please re-run",
				got, want,
			)
		}
	}

	// go-selfupdate handles the Windows rename trick internally, so the same
	// call path works on Linux, macOS, and Windows.
	if err := upd.UpdateTo(ctx, release, exe); err != nil {
		return fmt.Errorf("updating: %w", err)
	}
	return nil
}
