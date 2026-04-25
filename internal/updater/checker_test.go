package updater

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestChecker_Check_NotReleaseBuild(t *testing.T) {
	t.Parallel()
	c := &Checker{CacheDir: t.TempDir()}
	for _, v := range []string{"dev", "unknown", "00f2331", ""} {
		v := v
		t.Run(v, func(t *testing.T) {
			t.Parallel()
			_, err := c.Check(context.Background(), v, false)
			if !errors.Is(err, ErrNotReleaseBuild) {
				t.Fatalf("expected ErrNotReleaseBuild for %q, got %v", v, err)
			}
		})
	}
}

func TestChecker_Check_FetchSuccess_UpdateAvailable(t *testing.T) {
	t.Parallel()
	tmp := t.TempDir()
	fixedNow := time.Date(2025, 1, 1, 0, 0, 0, 0, time.UTC)
	c := &Checker{
		CacheDir: tmp,
		now:      func() time.Time { return fixedNow },
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			return "1.2.3", "https://example.com/release", true, nil
		},
	}
	got, err := c.Check(context.Background(), "v1.0.0", false)
	if err != nil {
		t.Fatalf("Check returned error: %v", err)
	}
	if !got.UpdateAvailable {
		t.Fatalf("expected UpdateAvailable=true, got %+v", got)
	}
	if got.LatestVersion != "1.2.3" {
		t.Errorf("LatestVersion = %q, want %q", got.LatestVersion, "1.2.3")
	}
	if got.CurrentVersion != "v1.0.0" {
		t.Errorf("CurrentVersion = %q, want %q", got.CurrentVersion, "v1.0.0")
	}
	if !got.CheckedAt.Equal(fixedNow) {
		t.Errorf("CheckedAt = %v, want %v", got.CheckedAt, fixedNow)
	}

	// Cache file should exist on disk.
	data, err := os.ReadFile(filepath.Join(tmp, CacheFileName)) //nolint:gosec // test-controlled path under t.TempDir()
	if err != nil {
		t.Fatalf("expected cache file written, got: %v", err)
	}
	var cached CheckResult
	if err := json.Unmarshal(data, &cached); err != nil {
		t.Fatalf("cache file is not valid JSON: %v", err)
	}
	if cached.LatestVersion != "1.2.3" {
		t.Errorf("cached LatestVersion = %q, want %q", cached.LatestVersion, "1.2.3")
	}
}

func TestChecker_Check_FetchSuccess_NoUpdate(t *testing.T) {
	t.Parallel()
	c := &Checker{
		CacheDir: t.TempDir(),
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			return "1.0.0", "", true, nil
		},
	}
	got, err := c.Check(context.Background(), "v1.0.0", false)
	if err != nil {
		t.Fatalf("Check returned error: %v", err)
	}
	if got.UpdateAvailable {
		t.Errorf("expected UpdateAvailable=false, got %+v", got)
	}
}

func TestChecker_Check_LowerVersion(t *testing.T) {
	t.Parallel()
	c := &Checker{
		CacheDir: t.TempDir(),
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			return "0.9.0", "", true, nil
		},
	}
	got, err := c.Check(context.Background(), "v1.0.0", false)
	if err != nil {
		t.Fatalf("Check returned error: %v", err)
	}
	if got.UpdateAvailable {
		t.Errorf("a lower version should not be reported as an update")
	}
}

func TestChecker_Check_FetchError(t *testing.T) {
	t.Parallel()
	c := &Checker{
		CacheDir: t.TempDir(),
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			return "", "", false, errors.New("network down")
		},
	}
	_, err := c.Check(context.Background(), "v1.0.0", false)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestChecker_Check_UsesCacheWhenFresh(t *testing.T) {
	t.Parallel()
	tmp := t.TempDir()
	fixedNow := time.Date(2025, 1, 1, 12, 0, 0, 0, time.UTC)

	// Pre-populate the cache with a fresh entry.
	cached := CheckResult{
		UpdateAvailable: true,
		LatestVersion:   "9.9.9",
		ReleaseURL:      "https://example.com",
		CurrentVersion:  "v1.0.0",
		Platform:        currentPlatform(),
		CheckedAt:       fixedNow.Add(-1 * time.Hour),
	}
	data, _ := json.Marshal(cached)
	if err := os.WriteFile(filepath.Join(tmp, CacheFileName), data, 0o600); err != nil {
		t.Fatal(err)
	}

	called := false
	c := &Checker{
		CacheDir: tmp,
		now:      func() time.Time { return fixedNow },
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			called = true
			return "1.2.3", "", true, nil
		},
	}
	got, err := c.Check(context.Background(), "v1.0.0", false)
	if err != nil {
		t.Fatalf("Check returned error: %v", err)
	}
	if called {
		t.Fatalf("detectLatest must not be called when cache is fresh")
	}
	if got.LatestVersion != "9.9.9" {
		t.Errorf("LatestVersion = %q, want cached %q", got.LatestVersion, "9.9.9")
	}
}

func TestChecker_Check_BypassesCacheWhenStale(t *testing.T) {
	t.Parallel()
	tmp := t.TempDir()
	fixedNow := time.Date(2025, 1, 1, 12, 0, 0, 0, time.UTC)

	cached := CheckResult{
		UpdateAvailable: true,
		LatestVersion:   "9.9.9",
		CurrentVersion:  "v1.0.0",
		Platform:        currentPlatform(),
		CheckedAt:       fixedNow.Add(-25 * time.Hour), // older than DefaultCacheTTL
	}
	data, _ := json.Marshal(cached)
	if err := os.WriteFile(filepath.Join(tmp, CacheFileName), data, 0o600); err != nil {
		t.Fatal(err)
	}

	c := &Checker{
		CacheDir: tmp,
		now:      func() time.Time { return fixedNow },
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			return "1.2.3", "", true, nil
		},
	}
	got, err := c.Check(context.Background(), "v1.0.0", false)
	if err != nil {
		t.Fatalf("Check returned error: %v", err)
	}
	if got.LatestVersion != "1.2.3" {
		t.Errorf("LatestVersion = %q, want fresh %q", got.LatestVersion, "1.2.3")
	}
}

func TestChecker_Check_BypassesCacheWhenVersionDiffers(t *testing.T) {
	t.Parallel()
	tmp := t.TempDir()
	fixedNow := time.Date(2025, 1, 1, 12, 0, 0, 0, time.UTC)

	// Cache was written for v0.5.0 — we are now running v1.0.0.
	cached := CheckResult{
		UpdateAvailable: true,
		LatestVersion:   "0.6.0",
		CurrentVersion:  "v0.5.0",
		Platform:        currentPlatform(),
		CheckedAt:       fixedNow.Add(-1 * time.Hour),
	}
	data, _ := json.Marshal(cached)
	if err := os.WriteFile(filepath.Join(tmp, CacheFileName), data, 0o600); err != nil {
		t.Fatal(err)
	}

	called := false
	c := &Checker{
		CacheDir: tmp,
		now:      func() time.Time { return fixedNow },
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			called = true
			return "1.2.3", "", true, nil
		},
	}
	got, err := c.Check(context.Background(), "v1.0.0", false)
	if err != nil {
		t.Fatalf("Check returned error: %v", err)
	}
	if !called {
		t.Fatalf("detectLatest must be called when cached CurrentVersion mismatches")
	}
	if got.LatestVersion != "1.2.3" {
		t.Errorf("LatestVersion = %q, want %q", got.LatestVersion, "1.2.3")
	}
}

// TestChecker_Check_BypassesCacheWhenPlatformDiffers covers the case where
// ~/.agento is shared across machines (e.g. via dotfiles) and the cached
// release was written for a different GOOS/GOARCH.
func TestChecker_Check_BypassesCacheWhenPlatformDiffers(t *testing.T) {
	t.Parallel()
	tmp := t.TempDir()
	fixedNow := time.Date(2025, 1, 1, 12, 0, 0, 0, time.UTC)

	cached := CheckResult{
		UpdateAvailable: true,
		LatestVersion:   "9.9.9",
		CurrentVersion:  "v1.0.0",
		Platform:        "some-other-os/some-other-arch",
		CheckedAt:       fixedNow.Add(-1 * time.Hour),
	}
	data, _ := json.Marshal(cached)
	if err := os.WriteFile(filepath.Join(tmp, CacheFileName), data, 0o600); err != nil {
		t.Fatal(err)
	}

	called := false
	c := &Checker{
		CacheDir: tmp,
		now:      func() time.Time { return fixedNow },
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			called = true
			return "1.2.3", "", true, nil
		},
	}
	got, err := c.Check(context.Background(), "v1.0.0", false)
	if err != nil {
		t.Fatalf("Check returned error: %v", err)
	}
	if !called {
		t.Fatalf("detectLatest must be called when cached Platform mismatches")
	}
	if got.LatestVersion != "1.2.3" {
		t.Errorf("LatestVersion = %q, want %q", got.LatestVersion, "1.2.3")
	}
	if got.Platform != currentPlatform() {
		t.Errorf("Platform = %q, want %q", got.Platform, currentPlatform())
	}
}

// TestChecker_Check_LegacyCacheMissingPlatformIsInvalidated handles cache
// files written by an earlier version of agento that did not record Platform.
func TestChecker_Check_LegacyCacheMissingPlatformIsInvalidated(t *testing.T) {
	t.Parallel()
	tmp := t.TempDir()
	fixedNow := time.Date(2025, 1, 1, 12, 0, 0, 0, time.UTC)

	// Marshal a struct that omits the platform field entirely.
	legacy := struct {
		UpdateAvailable bool      `json:"update_available"`
		LatestVersion   string    `json:"latest_version"`
		CurrentVersion  string    `json:"current_version"`
		CheckedAt       time.Time `json:"checked_at"`
	}{
		UpdateAvailable: true,
		LatestVersion:   "9.9.9",
		CurrentVersion:  "v1.0.0",
		CheckedAt:       fixedNow.Add(-1 * time.Hour),
	}
	data, _ := json.Marshal(legacy)
	if err := os.WriteFile(filepath.Join(tmp, CacheFileName), data, 0o600); err != nil {
		t.Fatal(err)
	}

	called := false
	c := &Checker{
		CacheDir: tmp,
		now:      func() time.Time { return fixedNow },
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			called = true
			return "1.2.3", "", true, nil
		},
	}
	if _, err := c.Check(context.Background(), "v1.0.0", false); err != nil {
		t.Fatalf("Check returned error: %v", err)
	}
	if !called {
		t.Fatalf("detectLatest must be called when cached Platform is empty (legacy cache)")
	}
}

func TestChecker_Check_ForceFreshBypassesCache(t *testing.T) {
	t.Parallel()
	tmp := t.TempDir()
	fixedNow := time.Date(2025, 1, 1, 12, 0, 0, 0, time.UTC)

	cached := CheckResult{
		UpdateAvailable: true,
		LatestVersion:   "9.9.9",
		CurrentVersion:  "v1.0.0",
		Platform:        currentPlatform(),
		CheckedAt:       fixedNow.Add(-1 * time.Hour),
	}
	data, _ := json.Marshal(cached)
	if err := os.WriteFile(filepath.Join(tmp, CacheFileName), data, 0o600); err != nil {
		t.Fatal(err)
	}

	called := false
	c := &Checker{
		CacheDir: tmp,
		now:      func() time.Time { return fixedNow },
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			called = true
			return "1.2.3", "", true, nil
		},
	}
	got, err := c.Check(context.Background(), "v1.0.0", true)
	if err != nil {
		t.Fatalf("Check returned error: %v", err)
	}
	if !called {
		t.Fatalf("detectLatest must be called when forceFresh=true")
	}
	if got.LatestVersion != "1.2.3" {
		t.Errorf("LatestVersion = %q, want %q", got.LatestVersion, "1.2.3")
	}
}

func TestChecker_Check_NoCacheDirSkipsPersistence(t *testing.T) {
	t.Parallel()
	c := &Checker{
		CacheDir: "", // disabled
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			return "1.2.3", "", true, nil
		},
	}
	_, err := c.Check(context.Background(), "v1.0.0", false)
	if err != nil {
		t.Fatalf("Check returned error: %v", err)
	}
	// Nothing to assert — the test simply ensures no panic and no error when
	// CacheDir is empty.
}

func TestChecker_Check_CorruptedCacheFallsThroughToFetch(t *testing.T) {
	t.Parallel()
	tmp := t.TempDir()
	if err := os.WriteFile(filepath.Join(tmp, CacheFileName), []byte("not json"), 0o600); err != nil {
		t.Fatal(err)
	}
	called := false
	c := &Checker{
		CacheDir: tmp,
		detectLatest: func(_ context.Context) (string, string, bool, error) {
			called = true
			return "1.2.3", "", true, nil
		},
	}
	got, err := c.Check(context.Background(), "v1.0.0", false)
	if err != nil {
		t.Fatalf("Check returned error: %v", err)
	}
	if !called {
		t.Fatalf("expected fetch to be called when cache is corrupted")
	}
	if got.LatestVersion != "1.2.3" {
		t.Errorf("LatestVersion = %q, want %q", got.LatestVersion, "1.2.3")
	}
}

func TestChecker_Check_TimeoutPropagated(t *testing.T) {
	t.Parallel()
	c := &Checker{
		CacheDir: t.TempDir(),
		Timeout:  10 * time.Millisecond,
		detectLatest: func(ctx context.Context) (string, string, bool, error) {
			select {
			case <-time.After(200 * time.Millisecond):
				return "1.2.3", "", true, nil
			case <-ctx.Done():
				return "", "", false, ctx.Err()
			}
		},
	}
	_, err := c.Check(context.Background(), "v1.0.0", false)
	if err == nil {
		t.Fatalf("expected timeout error, got nil")
	}
}
