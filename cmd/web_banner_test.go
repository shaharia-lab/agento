package cmd

import (
	"strings"
	"testing"

	"github.com/shaharia-lab/agento/internal/sunset"
)

// TestPrintBannerCarriesTheSunsetNotice pins issue #389's requirement that
// `agento web` states the retirement on startup.
//
// The notice lives in printBanner rather than in either rendering, so it cannot
// be lost by a change to only one of them — but nothing else asserts it, and
// the startup banner is the surface a long-running install is most likely to be
// read from. Both renderings are exercised because the color-profile branch
// picks between them at runtime.
func TestPrintBannerCarriesTheSunsetNotice(t *testing.T) {
	out := captureStdout(t, func() {
		printBanner("v0.11.2", "http://localhost:8990", "/tmp/agento.log")
	})

	for _, want := range []string{
		sunset.CutoffDate,
		sunset.DesktopReleasesURL,
		"~/.agento/agento.db",
	} {
		if !strings.Contains(out, want) {
			t.Errorf("the startup banner must mention %q:\n%s", want, out)
		}
	}

	// The app is not being switched off, and the banner has room to say so.
	if !strings.Contains(out, "keeps working") {
		t.Errorf("the banner must say the app keeps working past the cutoff:\n%s", out)
	}
}

// TestPrintPlainBannerStillCarriesTheVitals guards the non-color rendering,
// which is what a piped or dumb terminal actually gets.
func TestPrintPlainBannerStillCarriesTheVitals(t *testing.T) {
	out := captureStdout(t, func() {
		printPlainBanner("v0.11.2", "http://localhost:8990", "/tmp/agento.log")
	})

	for _, want := range []string{"v0.11.2", "http://localhost:8990", "/tmp/agento.log"} {
		if !strings.Contains(out, want) {
			t.Errorf("the plain banner must still show %q:\n%s", want, out)
		}
	}
}
