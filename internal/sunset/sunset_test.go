package sunset

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// TestPassedBoundary pins the inclusive boundary: the cutoff instant itself is
// already past, one nanosecond before it is not. A test that only probed days
// either side would not catch an accidental flip to a strict After.
func TestPassedBoundary(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		now  time.Time
		want bool
	}{
		{"a week before", Cutoff.Add(-7 * 24 * time.Hour), false},
		{"one nanosecond before", Cutoff.Add(-time.Nanosecond), false},
		{"the cutoff instant", Cutoff, true},
		{"one nanosecond after", Cutoff.Add(time.Nanosecond), true},
		{"a week after", Cutoff.Add(7 * 24 * time.Hour), true},
		{"non-UTC zone before", Cutoff.Add(-2 * time.Hour).In(time.FixedZone("UTC+5", 5*3600)), false},
		{"non-UTC zone after", Cutoff.Add(2 * time.Hour).In(time.FixedZone("UTC-5", -5*3600)), true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if got := Passed(tc.now); got != tc.want {
				t.Fatalf("Passed(%s) = %v, want %v", tc.now, got, tc.want)
			}
		})
	}
}

// TestNoticesCarryTheRequiredFacts asserts every notice states the cutoff, the
// download location, and that the database is shared. These three are the whole
// point of the release; a reworded notice that drops one is a regression.
func TestNoticesCarryTheRequiredFacts(t *testing.T) {
	t.Parallel()
	cases := map[string]string{
		"Notice":           Notice(),
		"FullNotice":       FullNotice(),
		"MigrationMessage": MigrationMessage(),
	}
	for name, text := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			for _, want := range []string{CutoffDate, DesktopReleasesURL, "~/.agento/agento.db"} {
				if !strings.Contains(text, want) {
					t.Errorf("%s() does not mention %q:\n%s", name, want, text)
				}
			}
		})
	}
}

// TestShouldPrintRoundTrip covers the ordinary lifecycle: due on a cold data
// dir, suppressed right after a stamp, due again a day later.
func TestShouldPrintRoundTrip(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	now := time.Date(2026, time.August, 23, 12, 0, 0, 0, time.UTC)

	if !ShouldPrint(dir, now) {
		t.Fatal("a data dir with no stamp should print")
	}
	if err := Stamp(dir, now); err != nil {
		t.Fatalf("Stamp: %v", err)
	}
	if ShouldPrint(dir, now.Add(time.Hour)) {
		t.Error("an hour after the stamp should not print")
	}
	if ShouldPrint(dir, now.Add(23*time.Hour+59*time.Minute)) {
		t.Error("just under 24h after the stamp should not print")
	}
	if !ShouldPrint(dir, now.Add(24*time.Hour)) {
		t.Error("exactly 24h after the stamp should print")
	}
}

// TestShouldPrintDegradesToPrinting is the load-bearing behavior: every way of
// failing to read a stamp must show the notice rather than swallow it. Silence
// is the failure mode this whole release exists to prevent.
func TestShouldPrintDegradesToPrinting(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, time.August, 23, 12, 0, 0, 0, time.UTC)

	t.Run("no data dir configured", func(t *testing.T) {
		t.Parallel()
		if !ShouldPrint("", now) {
			t.Error("an empty data dir should print")
		}
	})

	t.Run("corrupt stamp", func(t *testing.T) {
		t.Parallel()
		dir := t.TempDir()
		if err := os.WriteFile(filepath.Join(dir, StampFileName), []byte("{not json"), 0o600); err != nil {
			t.Fatal(err)
		}
		if !ShouldPrint(dir, now) {
			t.Error("a corrupt stamp should print")
		}
	})

	t.Run("zero timestamp", func(t *testing.T) {
		t.Parallel()
		dir := t.TempDir()
		if err := os.WriteFile(filepath.Join(dir, StampFileName), []byte(`{"last_shown":"0001-01-01T00:00:00Z"}`), 0o600); err != nil {
			t.Fatal(err)
		}
		if !ShouldPrint(dir, now) {
			t.Error("a zero timestamp should print")
		}
	})

	t.Run("stamp in the future", func(t *testing.T) {
		t.Parallel()
		dir := t.TempDir()
		if err := Stamp(dir, now.Add(72*time.Hour)); err != nil {
			t.Fatal(err)
		}
		if !ShouldPrint(dir, now) {
			t.Error("a stamp dated in the future should print, not suppress until the clock catches up")
		}
	})

	t.Run("unreadable directory", func(t *testing.T) {
		t.Parallel()
		// A path whose parent is a regular file cannot be read or created.
		dir := t.TempDir()
		file := filepath.Join(dir, "not-a-dir")
		if err := os.WriteFile(file, []byte("x"), 0o600); err != nil {
			t.Fatal(err)
		}
		if !ShouldPrint(filepath.Join(file, "nested"), now) {
			t.Error("an unreadable data dir should print")
		}
	})
}

// TestStampFileIsNotTheUpdateCache guards the deliberate separation from
// updater.CacheFileName — sharing that file would tie the notice's cadence to
// version and platform changes that say nothing about whether the user has read
// it.
func TestStampFileIsNotTheUpdateCache(t *testing.T) {
	t.Parallel()
	if StampFileName == "update-check.json" {
		t.Fatal("the sunset stamp must not share updater.CacheFileName")
	}
}
