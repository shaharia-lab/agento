package updater

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
)

// realManifest mirrors the shape and values of the manifest published at the
// fixed `desktop-latest` tag, including the `desktop-v` tag prefix in the URLs.
const realManifest = `{
  "version": "0.1.1",
  "notes": "See https://github.com/shaharia-lab/agento/releases/tag/desktop-v0.1.1",
  "pub_date": "2026-08-20T11:45:05Z",
  "platforms": {
    "darwin-aarch64": {"signature": "x", "url": "https://github.com/shaharia-lab/agento/releases/download/desktop-v0.1.1/Agento_aarch64.app.tar.gz"},
    "darwin-x86_64": {"signature": "x", "url": "https://github.com/shaharia-lab/agento/releases/download/desktop-v0.1.1/Agento_x64.app.tar.gz"},
    "linux-aarch64": {"signature": "x", "url": "https://github.com/shaharia-lab/agento/releases/download/desktop-v0.1.1/Agento_0.1.1_aarch64.AppImage"},
    "linux-x86_64": {"signature": "x", "url": "https://github.com/shaharia-lab/agento/releases/download/desktop-v0.1.1/Agento_0.1.1_amd64.AppImage"},
    "windows-x86_64": {"signature": "x", "url": "https://github.com/shaharia-lab/agento/releases/download/desktop-v0.1.1/Agento_0.1.1_x64-setup.exe"}
  }
}`

// TestInstallerAssetNameMatrix pins every supported platform to the asset name
// actually published on a desktop release.
//
// These names are transcribed from a real release listing, and the arch token
// differs per package format — Debian's amd64/arm64 for the .deb, Tauri's
// x64/aarch64 for the macOS bundles. A "tidy-up" that unified them would
// produce five plausible 404s, which is exactly what this table prevents.
func TestInstallerAssetNameMatrix(t *testing.T) {
	t.Parallel()
	cases := []struct {
		goos, goarch string
		want         string
	}{
		{"darwin", "amd64", "Agento_0.1.1_x64.dmg"},
		{"darwin", "arm64", "Agento_0.1.1_aarch64.dmg"},
		{"linux", "amd64", "Agento_0.1.1_amd64.deb"},
		{"linux", "arm64", "Agento_0.1.1_arm64.deb"},
		{"windows", "amd64", "Agento_0.1.1_x64-setup.exe"},
		// Unsupported platforms name nothing rather than guessing.
		{"linux", "386", ""},
		{"windows", "arm64", ""},
		{"freebsd", "amd64", ""},
		{"darwin", "", ""},
	}
	for _, tc := range cases {
		t.Run(tc.goos+"/"+tc.goarch, func(t *testing.T) {
			t.Parallel()
			if got := InstallerAssetName(tc.goos, tc.goarch, "0.1.1"); got != tc.want {
				t.Fatalf("InstallerAssetName(%q, %q) = %q, want %q", tc.goos, tc.goarch, got, tc.want)
			}
		})
	}
}

// TestResolveDesktopReleaseFromManifest drives the happy path per platform
// against a served copy of the real manifest.
func TestResolveDesktopReleaseFromManifest(t *testing.T) {
	t.Parallel()
	srv := manifestServer(t, http.StatusOK, realManifest)

	cases := []struct {
		goos, goarch string
		wantURL      string
	}{
		{"darwin", "arm64", "https://github.com/shaharia-lab/agento/releases/download/desktop-v0.1.1/Agento_0.1.1_aarch64.dmg"},
		{"darwin", "amd64", "https://github.com/shaharia-lab/agento/releases/download/desktop-v0.1.1/Agento_0.1.1_x64.dmg"},
		{"linux", "amd64", "https://github.com/shaharia-lab/agento/releases/download/desktop-v0.1.1/Agento_0.1.1_amd64.deb"},
		{"linux", "arm64", "https://github.com/shaharia-lab/agento/releases/download/desktop-v0.1.1/Agento_0.1.1_arm64.deb"},
		{"windows", "amd64", "https://github.com/shaharia-lab/agento/releases/download/desktop-v0.1.1/Agento_0.1.1_x64-setup.exe"},
	}
	for _, tc := range cases {
		t.Run(tc.goos+"/"+tc.goarch, func(t *testing.T) {
			t.Parallel()
			rel, err := resolveDesktopRelease(context.Background(), srv.URL, tc.goos, tc.goarch)
			if err != nil {
				t.Fatalf("resolveDesktopRelease: %v", err)
			}
			if rel.Version != "0.1.1" {
				t.Errorf("Version = %q, want 0.1.1", rel.Version)
			}
			if rel.DownloadURL != tc.wantURL {
				t.Errorf("DownloadURL = %q, want %q", rel.DownloadURL, tc.wantURL)
			}
			want := "https://github.com/shaharia-lab/agento/releases/tag/desktop-v0.1.1"
			if rel.ReleasePage != want {
				t.Errorf("ReleasePage = %q, want %q", rel.ReleasePage, want)
			}
		})
	}
}

// TestResolveDesktopReleaseSurvivesTagRename is the whole reason the tag is read
// back out of the manifest instead of being reconstructed: the desktop tags
// lose their `desktop-` prefix eventually, and resolution must not notice.
func TestResolveDesktopReleaseSurvivesTagRename(t *testing.T) {
	t.Parallel()
	renamed := `{"version":"0.2.0","platforms":{"darwin-aarch64":{"url":"https://github.com/shaharia-lab/agento/releases/download/v0.2.0/Agento_aarch64.app.tar.gz"}}}`
	srv := manifestServer(t, http.StatusOK, renamed)

	rel, err := resolveDesktopRelease(context.Background(), srv.URL, "darwin", "arm64")
	if err != nil {
		t.Fatalf("resolveDesktopRelease: %v", err)
	}
	wantURL := "https://github.com/shaharia-lab/agento/releases/download/v0.2.0/Agento_0.2.0_aarch64.dmg"
	if rel.DownloadURL != wantURL {
		t.Errorf("DownloadURL = %q, want %q", rel.DownloadURL, wantURL)
	}
	if want := "https://github.com/shaharia-lab/agento/releases/tag/v0.2.0"; rel.ReleasePage != want {
		t.Errorf("ReleasePage = %q, want %q", rel.ReleasePage, want)
	}
}

// TestResolveDesktopReleaseUnknownPlatform asserts an unsupported platform
// still resolves the version and the release page, but names no download —
// sending someone to a fabricated URL is worse than sending them to the page.
func TestResolveDesktopReleaseUnknownPlatform(t *testing.T) {
	t.Parallel()
	srv := manifestServer(t, http.StatusOK, realManifest)

	rel, err := resolveDesktopRelease(context.Background(), srv.URL, "freebsd", "amd64")
	if err != nil {
		t.Fatalf("resolveDesktopRelease: %v", err)
	}
	if rel.DownloadURL != "" || rel.AssetName != "" {
		t.Errorf("an unsupported platform must name no asset, got %q / %q", rel.AssetName, rel.DownloadURL)
	}
	if rel.ReleasePage == "" {
		t.Error("the release page must still be resolved")
	}
}

// TestResolveDesktopReleaseFailures covers every way the manifest can fail to
// answer. All of them must wrap ErrDesktopReleaseUnavailable so the caller can
// fall back to the releases page instead of failing the command.
func TestResolveDesktopReleaseFailures(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name   string
		status int
		body   string
	}{
		{"not found", http.StatusNotFound, "nope"},
		{"server error", http.StatusInternalServerError, "boom"},
		{"malformed json", http.StatusOK, "{not json"},
		{"no version", http.StatusOK, `{"platforms":{}}`},
		{"blank version", http.StatusOK, `{"version":"   ","platforms":{}}`},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			srv := manifestServer(t, tc.status, tc.body)
			_, err := resolveDesktopRelease(context.Background(), srv.URL, "linux", "amd64")
			if !errors.Is(err, ErrDesktopReleaseUnavailable) {
				t.Fatalf("err = %v, want it to wrap ErrDesktopReleaseUnavailable", err)
			}
		})
	}
}

// TestResolveDesktopReleaseWithoutUsableTag falls back to the generic
// latest/download path when no platform URL reveals a tag, rather than
// producing a URL with an empty tag segment.
func TestResolveDesktopReleaseWithoutUsableTag(t *testing.T) {
	t.Parallel()
	srv := manifestServer(t, http.StatusOK, `{"version":"9.9.9","platforms":{"linux-x86_64":{"url":"https://example.invalid/elsewhere"}}}`)

	rel, err := resolveDesktopRelease(context.Background(), srv.URL, "linux", "amd64")
	if err != nil {
		t.Fatalf("resolveDesktopRelease: %v", err)
	}
	want := "https://github.com/shaharia-lab/agento/releases/latest/download/Agento_9.9.9_amd64.deb"
	if rel.DownloadURL != want {
		t.Errorf("DownloadURL = %q, want %q", rel.DownloadURL, want)
	}
	if rel.ReleasePage != ReleasesPageURL() {
		t.Errorf("ReleasePage = %q, want the plain releases page", rel.ReleasePage)
	}
}

// TestDesktopManifestURLIsTheFixedTag guards the anchor itself. Keying on
// anything else — /releases/latest, the newest tag, a `desktop-` prefix — is
// wrong for reasons documented on the constant, and all three are wrong today.
func TestDesktopManifestURLIsTheFixedTag(t *testing.T) {
	t.Parallel()
	if want := "https://github.com/shaharia-lab/agento/releases/download/desktop-latest/latest.json"; desktopManifestURL != want {
		t.Fatalf("desktopManifestURL = %q, want the fixed manifest tag %q", desktopManifestURL, want)
	}
}

func manifestServer(t *testing.T, status int, body string) *httptest.Server {
	t.Helper()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(status)
		_, _ = w.Write([]byte(body)) //nolint:errcheck
	}))
	t.Cleanup(srv.Close)
	return srv
}
