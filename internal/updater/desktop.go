package updater

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"runtime"
	"sort"
	"strings"
	"time"
)

// The Go/web build is being retired in favor of Agento Desktop, so `agento
// update` no longer self-updates — it points the user at the desktop installer
// for their platform. Resolving that installer cannot go through
// go-selfupdate: its asset matcher builds every candidate as
// "<os><sep><arch><ext>" and requires the OS token, which no Tauri asset
// carries. A desktop-tagged release is therefore skipped entirely and the
// picker silently falls back to the newest release that does have
// agento_Darwin_arm64.tar.gz and friends. That is why this file exists.

// desktopManifestURL is the Tauri updater manifest, published at a FIXED tag
// that every release re-points at the newest build.
//
// Anchoring on this tag rather than on a release listing is deliberate, because
// three separate things make "the newest release" the wrong answer:
//
//   - GET /releases/latest returns the Go build (v0.11.1), by design: desktop
//     releases are published with `--latest=false` precisely so the CLI's
//     go-selfupdate keeps resolving the Go release.
//   - `desktop-latest` is itself marked as a prerelease.
//   - the newest desktop tag is often still a draft, whose assets are not
//     publicly downloadable.
//
// The fixed tag sidesteps all three, and it survives the eventual rename of
// `desktop-v*` to plain `v*` — nothing here may key on the tag prefix.
const desktopManifestURL = "https://github.com/shaharia-lab/agento/releases/download/desktop-latest/latest.json"

// releasesPageURL is the fallback we print when a platform-specific asset
// cannot be named. It is always correct, just less convenient.
const releasesPageURL = "https://github.com/shaharia-lab/agento/releases"

// desktopFetchTimeout bounds the manifest fetch. `agento update` is
// interactive, so a slow network must not hang it.
const desktopFetchTimeout = 10 * time.Second

// ErrDesktopReleaseUnavailable is returned when the desktop release could not
// be resolved at all. Callers should fall back to printing releasesPageURL
// rather than failing the command.
var ErrDesktopReleaseUnavailable = errors.New("could not resolve the latest Agento Desktop release")

// DesktopRelease is the resolved answer: which version is current, where its
// release page is, and the direct installer download for the running platform.
type DesktopRelease struct {
	// Version is the desktop version, without a leading "v" (e.g. "0.1.1").
	Version string
	// ReleasePage is the human-readable release page for that version.
	ReleasePage string
	// DownloadURL is the direct installer link for the running GOOS/GOARCH,
	// or "" when the platform has no published installer.
	DownloadURL string
	// AssetName is the file name behind DownloadURL, or "" alongside an empty
	// DownloadURL.
	AssetName string
}

// desktopManifest is the subset of Tauri's latest.json we read. The manifest
// lists the *auto-updater* artifacts (.app.tar.gz, .AppImage, the NSIS .exe),
// which are not the artifacts a person installing for the first time wants — so
// we take the version and the release tag from it and name the installer
// ourselves.
type desktopManifest struct {
	Version   string `json:"version"`
	Platforms map[string]struct {
		URL string `json:"url"`
	} `json:"platforms"`
}

// desktopInstallers maps "GOOS/GOARCH" to the format string naming its
// preferred installer asset, taking the version exactly once.
//
// The arch token is NOT uniform across package formats: the .deb uses Debian's
// arch names (amd64/arm64), the .rpm uses RPM's (x86_64/aarch64) plus a release
// number, and the macOS bundles use Tauri's (x64/aarch64). Each row is
// transcribed from the assets actually published on a desktop release; do not
// collapse them into one pattern.
//
// Linux publishes three formats per arch (.deb, .rpm, .AppImage); we name the
// .deb because it is the one most Linux desktops install with a double-click,
// and the release page carries the rest.
var desktopInstallers = map[string]string{ //nolint:gochecknoglobals
	"darwin/amd64":  "Agento_%s_x64.dmg",
	"darwin/arm64":  "Agento_%s_aarch64.dmg",
	"linux/amd64":   "Agento_%s_amd64.deb",
	"linux/arm64":   "Agento_%s_arm64.deb",
	"windows/amd64": "Agento_%s_x64-setup.exe",
}

// InstallerAssetName returns the installer file name for the given platform and
// version, or "" when that platform has no published installer. It is exported
// for tests and kept free of any network dependency.
func InstallerAssetName(goos, goarch, version string) string {
	pattern, ok := desktopInstallers[goos+"/"+goarch]
	if !ok {
		return ""
	}
	return fmt.Sprintf(pattern, version)
}

// ResolveDesktopRelease fetches the desktop manifest and returns the release
// plus the direct installer link for the running platform.
//
// An unknown GOOS/GOARCH yields a DesktopRelease with an empty DownloadURL
// rather than a guessed URL: sending a user to a 404 is worse than sending them
// to the release page.
func ResolveDesktopRelease(ctx context.Context) (*DesktopRelease, error) {
	return resolveDesktopRelease(ctx, desktopManifestURL, runtime.GOOS, runtime.GOARCH)
}

// resolveDesktopRelease is ResolveDesktopRelease with its inputs injected, so
// tests can drive it against a local server and an arbitrary platform.
func resolveDesktopRelease(ctx context.Context, manifestURL, goos, goarch string) (*DesktopRelease, error) {
	ctx, cancel := context.WithTimeout(ctx, desktopFetchTimeout)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, manifestURL, nil)
	if err != nil {
		return nil, fmt.Errorf("%w: building request: %w", ErrDesktopReleaseUnavailable, err)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("%w: %w", ErrDesktopReleaseUnavailable, err)
	}
	defer func() { _ = resp.Body.Close() }() //nolint:errcheck

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("%w: manifest returned %s", ErrDesktopReleaseUnavailable, resp.Status)
	}

	var manifest desktopManifest
	if err := json.NewDecoder(resp.Body).Decode(&manifest); err != nil {
		return nil, fmt.Errorf("%w: decoding manifest: %w", ErrDesktopReleaseUnavailable, err)
	}
	version := strings.TrimPrefix(strings.TrimSpace(manifest.Version), "v")
	if version == "" {
		return nil, fmt.Errorf("%w: manifest carries no version", ErrDesktopReleaseUnavailable)
	}

	rel := &DesktopRelease{
		Version:     version,
		ReleasePage: releasePageFrom(manifest),
	}
	if asset := InstallerAssetName(goos, goarch, version); asset != "" {
		rel.AssetName = asset
		rel.DownloadURL = downloadBase(manifest) + "/" + asset
	}
	return rel, nil
}

// releasePageFrom derives the release page for the manifest's version.
//
// The tag is read back out of a platform URL rather than reconstructed from the
// version, because the tag prefix is exactly the thing that is going to change:
// `desktop-v0.1.1` today, plain `v0.1.1` after the flip. Whatever tag the
// manifest points at is the right one by construction.
func releasePageFrom(m desktopManifest) string {
	if tag := tagFromManifest(m); tag != "" {
		return releasesPageURL + "/tag/" + tag
	}
	// No usable URL in the manifest — the plain releases page still gets the
	// user there.
	return releasesPageURL
}

// downloadBase returns the ".../download/<tag>" prefix the installer asset
// hangs off, falling back to the releases page's download root.
func downloadBase(m desktopManifest) string {
	if tag := tagFromManifest(m); tag != "" {
		return releasesPageURL + "/download/" + tag
	}
	return releasesPageURL + "/latest/download"
}

// tagFromManifest extracts the release tag from a platform download URL, which
// has the shape ".../releases/download/<tag>/<asset>".
//
// Platform keys are visited in sorted order rather than map order: every
// platform in a manifest points at the same tag, but relying on that while
// iterating a map non-deterministically would make any future violation show up
// as a flake instead of a failure.
func tagFromManifest(m desktopManifest) string {
	keys := make([]string, 0, len(m.Platforms))
	for k := range m.Platforms {
		keys = append(keys, k)
	}
	sort.Strings(keys)

	const marker = "/releases/download/"
	for _, k := range keys {
		url := m.Platforms[k].URL
		i := strings.Index(url, marker)
		if i < 0 {
			continue
		}
		rest := url[i+len(marker):]
		if j := strings.Index(rest, "/"); j > 0 {
			return rest[:j]
		}
	}
	return ""
}

// ReleasesPageURL exposes the releases page for callers that need a fallback
// when resolution fails.
func ReleasesPageURL() string { return releasesPageURL }
