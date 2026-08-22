// Package sunset owns everything about the retirement of the Go/web build of
// Agento: the cutoff date, the user-facing notice text, and the once-per-day
// stamp that keeps the pre-run notice from becoming a nag.
//
// It is deliberately the single definition every Go caller reads — the pre-run
// hook in cmd/root.go, the `agento web` startup banner, and `agento update` all
// resolve the same date and the same words from here, the way internal/config
// owns values the layers above it must agree on.
//
// Everything in this package is static and offline. The notice makes no network
// call at all: a notice that had to reach GitHub could time out, could degrade,
// and would break outright once the desktop releases stop carrying the
// `desktop-v` tag prefix — which is precisely the event this notice exists to
// warn users about ahead of time.
package sunset

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// CutoffDate is the end of support for the Go/web build, as a plain calendar
// date for display. Its counterpart on the frontend is SUNSET_CUTOFF in
// frontend/src/lib/sunset.ts — change both together.
const CutoffDate = "1 September 2026"

// Cutoff is CutoffDate as an instant. It is the moment `agento update` stops
// offering a download; nothing else in the binary consults it, because the web
// build keeps working indefinitely after this date. Staying on it is the user's
// call — there is no kill switch, no license check and no date-gated refusal to
// start anywhere in this package or its callers.
var Cutoff = time.Date(2026, time.September, 1, 0, 0, 0, 0, time.UTC) //nolint:gochecknoglobals

// DesktopReleasesURL is where users get the replacement.
const DesktopReleasesURL = "https://github.com/shaharia-lab/agento/releases"

// Passed reports whether now is at or after the cutoff. The comparison is
// deliberately inclusive of the boundary instant: 1 September 2026 00:00:00 UTC
// is already "on or after 1 September".
func Passed(now time.Time) bool {
	return !now.UTC().Before(Cutoff)
}

// Notice is the one-line form printed before every command. It is written to
// stderr by the caller so it never pollutes the stdout of a subcommand that
// emits machine-readable output.
func Notice() string {
	return fmt.Sprintf(
		"Agento (web) is being retired: no updates after %s. "+
			"Agento Desktop reads the same ~/.agento/agento.db — install and go: %s",
		CutoffDate, DesktopReleasesURL,
	)
}

// FullNotice is the multi-line form printed by `agento web` on startup, where
// there is room to say why and what happens next.
func FullNotice() string {
	return fmt.Sprintf(`Agento (web) is being retired.

  What        This is the final release of the Go/web build.
  When        Support ends %s. After that date `+"`agento update`"+` stops
              offering updates — the app itself keeps working, indefinitely.
  Move to     Agento Desktop: %s
  Your data   Agento Desktop reads the same ~/.agento/agento.db.
              No export, no migration. Install it and your history is there.`,
		CutoffDate, DesktopReleasesURL,
	)
}

// MigrationMessage is what `agento update` prints once the cutoff has passed,
// in place of offering a download.
func MigrationMessage() string {
	return fmt.Sprintf(`The Go/web build of Agento is no longer updated (support ended %s).

Agento Desktop is the supported app, and it reads the same ~/.agento/agento.db —
no export and no migration are needed.

  %s

This command no longer updates the binary. Every other agento command is
unaffected and keeps working.`, CutoffDate, DesktopReleasesURL)
}

// StampFileName is the file recording when the pre-run notice was last shown.
//
// It is deliberately NOT updater.CacheFileName. That cache holds an
// updater.CheckResult keyed on both the running version and the GOOS/GOARCH,
// and it invalidates whenever either changes — sound for an update check, but
// it would re-fire the sunset notice on conditions that have nothing to do with
// whether the user has already read it. Same directory, separate file.
const StampFileName = "sunset-notice.json"

// noticeInterval is how long a shown notice suppresses the next one.
const noticeInterval = 24 * time.Hour

// stamp is the on-disk shape of the stamp file.
type stamp struct {
	LastShown time.Time `json:"last_shown"`
}

// stampPath returns the absolute path to the stamp file, or "" when there is no
// data dir to write into.
func stampPath(dataDir string) string {
	if dataDir == "" {
		return ""
	}
	return filepath.Join(dataDir, StampFileName)
}

// ShouldPrint reports whether the pre-run notice is due.
//
// Every failure path degrades to true — an unreadable directory, a corrupt
// stamp, a clock that moved backwards. Printing one extra line is a far better
// outcome than a user never learning their install is being retired, so the
// only thing that suppresses the notice is a stamp we could read and that is
// genuinely recent.
func ShouldPrint(dataDir string, now time.Time) bool {
	path := stampPath(dataDir)
	if path == "" {
		return true
	}
	data, err := os.ReadFile(path) //nolint:gosec // path is under our own data dir
	if err != nil {
		return true
	}
	var s stamp
	if err := json.Unmarshal(data, &s); err != nil {
		return true
	}
	if s.LastShown.IsZero() {
		return true
	}
	elapsed := now.Sub(s.LastShown)
	// A negative elapsed means the stamp is in the future — a clock change or a
	// synced data dir. Treat it as due rather than suppressing until the clock
	// catches up.
	if elapsed < 0 {
		return true
	}
	return elapsed >= noticeInterval
}

// Stamp records that the notice was shown at now. It is best-effort: a failure
// here means the notice shows again sooner than intended, which is harmless,
// so callers may ignore the error.
func Stamp(dataDir string, now time.Time) error {
	path := stampPath(dataDir)
	if path == "" {
		return nil
	}
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		return fmt.Errorf("creating data dir: %w", err)
	}
	data, err := json.Marshal(stamp{LastShown: now})
	if err != nil {
		return fmt.Errorf("marshaling sunset stamp: %w", err)
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		return fmt.Errorf("writing sunset stamp: %w", err)
	}
	return nil
}
