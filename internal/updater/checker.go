// Package updater provides shared update-check and self-update logic used by
// both the explicit `agento update` command and the implicit pre-run hook that
// fires before every other command.
//
// The package is intentionally narrow: it caches release-detection results to
// avoid hammering GitHub on every CLI invocation, and it dispatches between
// in-place self-update and Homebrew-managed installs.
package updater

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"github.com/Masterminds/semver/v3"
	"github.com/creativeprojects/go-selfupdate"
)

// DefaultCacheTTL is the time the cached release-check result is considered fresh.
// While the cache is fresh the checker will not contact GitHub.
const DefaultCacheTTL = 24 * time.Hour

// DefaultCheckTimeout is the hard upper bound for a single network call to GitHub.
// On timeout the checker returns an error so callers can fail silently in the
// auto-check path without blocking the user's command.
const DefaultCheckTimeout = 5 * time.Second

// repoSlug is the GitHub repository the checker queries for releases.
const repoSlug = "shaharia-lab/agento"

// CacheFileName is the name of the cache file written under AGENTO_DATA_DIR.
const CacheFileName = "update-check.json"

// CheckResult describes what was found during a release check.
// A CheckResult with UpdateAvailable=false is still valid and should be cached
// to avoid re-querying GitHub on every command.
type CheckResult struct {
	UpdateAvailable bool   `json:"update_available"`
	LatestVersion   string `json:"latest_version"`
	ReleaseURL      string `json:"release_url"`
	CurrentVersion  string `json:"current_version"`
	// Platform is the GOOS/GOARCH the result was computed for. A cache entry
	// from a different platform (e.g. shared ~/.agento across machines) must
	// be invalidated because the install step would fail on missing assets.
	Platform  string    `json:"platform"`
	CheckedAt time.Time `json:"checked_at"`
}

// currentPlatform returns the runtime platform string used for cache validation.
func currentPlatform() string {
	return runtime.GOOS + "/" + runtime.GOARCH
}

// Checker performs cached release detection against GitHub.
//
// A zero Checker is valid but will use the current working directory for the
// cache file. Callers should set CacheDir explicitly (typically AGENTO_DATA_DIR).
type Checker struct {
	// CacheDir is the directory where the cache file is written.
	// If empty, caching is effectively disabled (a fresh check runs every time).
	CacheDir string

	// CacheTTL is the duration a cached result is considered fresh.
	// If zero, DefaultCacheTTL is used.
	CacheTTL time.Duration

	// Timeout is the hard upper bound for a single network call.
	// If zero, DefaultCheckTimeout is used.
	Timeout time.Duration

	// now is overridable for tests. If nil, time.Now is used.
	now func() time.Time

	// detectLatest is overridable for tests. If nil, the real go-selfupdate path is used.
	// Returns (latestVersion, releaseURL, found, error).
	detectLatest func(ctx context.Context) (latestVersion, releaseURL string, found bool, err error)
}

// nowFunc returns the time provider to use, defaulting to time.Now.
func (c *Checker) nowFunc() func() time.Time {
	if c.now != nil {
		return c.now
	}
	return time.Now
}

func (c *Checker) cacheTTL() time.Duration {
	if c.CacheTTL > 0 {
		return c.CacheTTL
	}
	return DefaultCacheTTL
}

func (c *Checker) timeout() time.Duration {
	if c.Timeout > 0 {
		return c.Timeout
	}
	return DefaultCheckTimeout
}

// Check returns the freshest available CheckResult for currentVersion.
//
// If forceFresh is false and the on-disk cache is still within TTL, the cached
// result is returned without contacting the network. If forceFresh is true (used
// by the explicit `agento update` command) the cache is bypassed and a live
// check is always performed; the cache is then updated with the new result.
//
// currentVersion may include a leading "v"; it is trimmed before comparison.
// Non-semver versions (e.g. "dev", "unknown", a bare git SHA) cause Check to
// return ErrNotReleaseBuild so callers can skip silently.
func (c *Checker) Check(ctx context.Context, currentVersion string, forceFresh bool) (*CheckResult, error) {
	currentTrimmed := strings.TrimPrefix(currentVersion, "v")
	if _, err := semver.NewVersion(currentTrimmed); err != nil {
		return nil, ErrNotReleaseBuild
	}

	if !forceFresh {
		if cached, ok := c.readCache(currentVersion); ok {
			return cached, nil
		}
	}

	result, err := c.fetch(ctx, currentTrimmed)
	if err != nil {
		return nil, err
	}
	result.CurrentVersion = currentVersion
	result.Platform = currentPlatform()
	result.CheckedAt = c.nowFunc()()

	// Best-effort cache write. Failures here are logged-via-return only and
	// must not break the caller — auto-check should never block a user.
	_ = c.writeCache(result) //nolint:errcheck

	return result, nil
}

// ErrNotReleaseBuild is returned by Check when the running binary's version
// is not a parseable semver release (e.g. "dev", "unknown", a commit SHA).
// Callers in the auto-check path should treat this as "skip silently".
var ErrNotReleaseBuild = errors.New("not a release build")

// fetch performs a live release-detection call against GitHub with a hard timeout.
func (c *Checker) fetch(ctx context.Context, currentTrimmed string) (*CheckResult, error) {
	ctx, cancel := context.WithTimeout(ctx, c.timeout())
	defer cancel()

	detect := c.detectLatest
	if detect == nil {
		detect = defaultDetectLatest
	}

	latestVersion, releaseURL, found, err := detect(ctx)
	if err != nil {
		return nil, fmt.Errorf("detecting latest release: %w", err)
	}

	result := &CheckResult{}
	if !found {
		return result, nil
	}

	// Compare semver-trimmed strings. We trust go-selfupdate to return a parseable version.
	latestSemver, err := semver.NewVersion(strings.TrimPrefix(latestVersion, "v"))
	if err != nil {
		// Not a semver release — treat as no update.
		return result, nil
	}
	currentSemver, err := semver.NewVersion(currentTrimmed)
	if err != nil {
		return result, nil
	}
	if latestSemver.GreaterThan(currentSemver) {
		result.UpdateAvailable = true
		result.LatestVersion = latestVersion
		result.ReleaseURL = releaseURL
	}
	return result, nil
}

// defaultDetectLatest is the production path: call go-selfupdate against the GitHub repo.
func defaultDetectLatest(ctx context.Context) (string, string, bool, error) {
	upd, err := selfupdate.NewUpdater(selfupdate.Config{})
	if err != nil {
		return "", "", false, fmt.Errorf("creating updater: %w", err)
	}
	release, found, err := upd.DetectLatest(ctx, selfupdate.ParseSlug(repoSlug))
	if err != nil {
		return "", "", false, err
	}
	if !found {
		return "", "", false, nil
	}
	url := fmt.Sprintf("https://github.com/%s/releases/tag/v%s", repoSlug, release.Version())
	return release.Version(), url, true, nil
}

// cachePath returns the absolute path to the cache file, or "" if caching is disabled.
func (c *Checker) cachePath() string {
	if c.CacheDir == "" {
		return ""
	}
	return filepath.Join(c.CacheDir, CacheFileName)
}

// readCache returns the cached CheckResult if it is present, fresh, and was
// written for the same currentVersion. A cache entry from a different version
// is treated as stale (otherwise an upgrade-and-revert cycle could mask updates).
func (c *Checker) readCache(currentVersion string) (*CheckResult, bool) {
	path := c.cachePath()
	if path == "" {
		return nil, false
	}
	data, err := os.ReadFile(path) //nolint:gosec // path is under our own data dir
	if err != nil {
		return nil, false
	}
	var cached CheckResult
	if err := json.Unmarshal(data, &cached); err != nil {
		return nil, false
	}
	if cached.CurrentVersion != currentVersion {
		return nil, false
	}
	// Invalidate cache entries that were written for a different GOOS/GOARCH
	// (e.g. when ~/.agento is shared across machines via dotfiles or syncthing).
	// Empty Platform on legacy cache files is also treated as a miss.
	if cached.Platform != currentPlatform() {
		return nil, false
	}
	if c.nowFunc()().Sub(cached.CheckedAt) >= c.cacheTTL() {
		return nil, false
	}
	return &cached, true
}

// writeCache persists result to disk. The cache directory is created if needed.
func (c *Checker) writeCache(result *CheckResult) error {
	path := c.cachePath()
	if path == "" {
		return nil
	}
	if err := os.MkdirAll(c.CacheDir, 0o700); err != nil {
		return fmt.Errorf("creating cache dir: %w", err)
	}
	data, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling cache: %w", err)
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		return fmt.Errorf("writing cache: %w", err)
	}
	return nil
}
