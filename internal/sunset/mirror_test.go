package sunset

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// frontendMirror is the TypeScript half of this package's constants. The banner
// has to render offline and instantly, so the cutoff and the download URL are
// compiled into the frontend rather than fetched — which means they exist twice.
const frontendMirror = "../../frontend/src/lib/sunset.ts"

// TestFrontendMirrorAgrees fails when the Go and TypeScript copies of a shared
// constant drift apart.
//
// A comment saying "change both together" is not a guarantee: each language's
// own suite asserts its own copy, so editing one and not the other leaves both
// green while the CLI and the web UI state different cutoff dates. This repo
// already rejected that arrangement once — session_metric_vectors.json is read
// by both internal/claudesessions/session_page_test.go and
// frontend/src/lib/sessionMetrics.test.ts precisely so a one-sided change fails
// the other language. Same rule here, at a fraction of the machinery, because
// there are three literals rather than a metric suite.
func TestFrontendMirrorAgrees(t *testing.T) {
	t.Parallel()

	raw, err := os.ReadFile(filepath.Clean(frontendMirror))
	if err != nil {
		// Not skipped: the `desktop` branch has no frontend/ tree, but this
		// package only exists on main, where the file must be present.
		t.Fatalf("reading the frontend mirror at %s: %v", frontendMirror, err)
	}
	src := string(raw)

	cases := []struct {
		tsConst string
		goValue string
		goName  string
	}{
		{"SUNSET_CUTOFF", CutoffDate, "sunset.CutoffDate"},
		{"DESKTOP_RELEASES_URL", DesktopReleasesURL, "sunset.DesktopReleasesURL"},
	}
	for _, tc := range cases {
		t.Run(tc.tsConst, func(t *testing.T) {
			t.Parallel()
			got, ok := tsStringConst(src, tc.tsConst)
			if !ok {
				t.Fatalf("%s declares no exported const %s", frontendMirror, tc.tsConst)
			}
			if got != tc.goValue {
				t.Errorf("%s = %q but %s = %q — change both together",
					tc.tsConst, got, tc.goName, tc.goValue)
			}
		})
	}

	// The shared database path is the one fact that makes the migration a
	// non-event for the user, so both notices must name the same file.
	shared, ok := tsStringConst(src, "SHARED_DB_PATH")
	if !ok {
		t.Fatal("the frontend mirror declares no SHARED_DB_PATH")
	}
	if !strings.Contains(FullNotice(), shared) {
		t.Errorf("SHARED_DB_PATH = %q but FullNotice() does not mention it", shared)
	}
}

// tsStringConst pulls the value out of `export const NAME = '...'`, accepting
// either quote style. It is deliberately a small string scan rather than a
// parser: the mirror file is a handful of literals by design, and a parser
// dependency would be far more machinery than the thing it guards.
func tsStringConst(src, name string) (string, bool) {
	marker := "export const " + name + " = "
	i := strings.Index(src, marker)
	if i < 0 {
		return "", false
	}
	rest := src[i+len(marker):]
	if rest == "" {
		return "", false
	}
	quote := rest[0]
	if quote != '\'' && quote != '"' {
		return "", false
	}
	end := strings.IndexByte(rest[1:], quote)
	if end < 0 {
		return "", false
	}
	return rest[1 : 1+end], true
}
