/**
 * The retirement of the Go/web build of Agento.
 *
 * This mirrors `internal/sunset/sunset.go` — change both together. The values
 * are fixed literals that do not move after this release ships, which is why a
 * mirrored constant is preferred over an API round-trip: the banner must render
 * instantly and offline, with no dependency on a backend call that could fail
 * and leave the user unaware.
 */

/** The end of support, matching sunset.CutoffDate on the Go side. */
export const SUNSET_CUTOFF = '1 September 2026'

/** Where the replacement lives, matching sunset.DesktopReleasesURL. */
export const DESKTOP_RELEASES_URL = 'https://github.com/shaharia-lab/agento/releases'

/**
 * The database Agento Desktop reads. It is the same file this build already
 * uses, which is the single most reassuring fact about the migration and so
 * appears in every notice.
 */
export const SHARED_DB_PATH = '~/.agento/agento.db'

/**
 * localStorage key recording that the user dismissed the sunset banner.
 *
 * Unlike the update banner's key this stores no version: dismissal is
 * permanent. Nothing re-arms this banner — not a timer, not a new release, not
 * a page navigation. The notice is informational, and a user who has read it
 * has read it.
 */
export const SUNSET_DISMISS_STORAGE_KEY = 'agento-sunset-dismissed'
