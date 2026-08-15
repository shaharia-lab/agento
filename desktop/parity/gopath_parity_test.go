// Cross-language vectors for Go's `path/filepath`.
//
// `GET /api/fs` is a thin wrapper around `filepath.Clean`, `filepath.Dir` and
// `filepath.Join`, so porting the endpoint means porting those three — and they
// are subtler than they look. `Clean` resolves `..` against a *lexical* root
// rather than the filesystem, so it keeps leading `..` on a relative path and
// drops them on a rooted one; `Dir` is `Clean` of everything before the last
// separator, which turns a bare name into `.`; `Join` cleans its result, so
// `Join("/a", "../b")` escapes upward.
//
// Every one of those is a path the picker would then hand to the user as a
// directory to work in, so a divergence is not cosmetic. This half asserts Go
// still produces what the frozen file records; the other half lives in
// desktop/src-tauri/src/native/gopath.rs and asserts Rust produces the same.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestGoPathVectors -update-gopath-vectors
//
// The vectors are Unix-shaped on purpose: `native/gopath.rs` implements the
// Unix rules and `native/fs.rs` does not claim the route on Windows, where
// `filepath.Clean`'s volume-name handling is a different algorithm.
package parity

import (
	"encoding/json"
	"flag"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

const gopathVectorsFile = "gopath_vectors.json"

var updateGoPathVectors = flag.Bool("update-gopath-vectors", false,
	"rewrite gopath_vectors.json from this Go toolchain")

type gopathVectors struct {
	Comment []string `json:"_comment"`
	Clean   []struct {
		Value string `json:"value"`
		Want  string `json:"want"`
	} `json:"clean"`
	Dir []struct {
		Value string `json:"value"`
		Want  string `json:"want"`
	} `json:"dir"`
	Join []struct {
		Elems []string `json:"elems"`
		Want  string   `json:"want"`
	} `json:"join"`
}

// cleanAndDirInputs covers each branch of Clean's loop: the rooted and
// unrooted `..` cases, repeated and trailing separators, a lone `.`, and the
// empty string — which is `.` rather than `""`, and is the one case a
// split-and-rejoin implementation gets wrong.
var cleanAndDirInputs = []string{
	"", ".", "..", "...", "/", "//", "///", "/.", "/..", "/../..",
	"a", "a/", "a//b", "a/./b", "a/../b", "a/..", "a/../..",
	"./a", "../a", "../../a", "/a/../..", "/a/b/../c/", "/../a",
	"/home/u/.claude/", "/home//u/./.claude", "/home/u/x/../.claude",
	"~", "~/foo", "/a/b/c/../../d", "foo/bar/..", "/a//b//c", "a/b/./../c",
	".hidden", "/.hidden/", "/a/b/..", "/a/b/../..", "/a/b/../../..",
}

// joinInputs include the pairs `Join`'s own cleaning makes surprising: an empty
// element is skipped entirely, and `..` in the second element escapes the first.
var joinInputs = [][]string{
	{"/a", "b"}, {"/", "b"}, {"", "b"}, {"a", ""}, {"/a/", "b"},
	{"a", "../b"}, {"/a", ".."}, {"/a/b", "../.."}, {"/", ".."},
	{"/home/u", ".claude"}, {"/a", "b/c"}, {".", "a"}, {"..", "a"},
}

func TestGoPathVectors(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("vectors record the Unix rules; the Rust port does not claim the route on Windows")
	}

	want := gopathVectors{
		Comment: []string{
			"Cross-language parity vectors for Go's path/filepath, Unix rules.",
			"Generated from Go, then frozen. Read by desktop/parity/gopath_parity_test.go",
			"(Go) and by desktop/src-tauri/src/native/gopath.rs (Rust). 'want' is exactly",
			"what Go produces, so a divergence fails one language against the other's",
			"real output rather than against a belief about it.",
			"Regenerate with: go test ./desktop/parity/ -run TestGoPathVectors -update-gopath-vectors",
		},
	}
	for _, in := range cleanAndDirInputs {
		want.Clean = append(want.Clean, struct {
			Value string `json:"value"`
			Want  string `json:"want"`
		}{in, filepath.Clean(in)})
		want.Dir = append(want.Dir, struct {
			Value string `json:"value"`
			Want  string `json:"want"`
		}{in, filepath.Dir(in)})
	}
	for _, elems := range joinInputs {
		want.Join = append(want.Join, struct {
			Elems []string `json:"elems"`
			Want  string   `json:"want"`
		}{elems, filepath.Join(elems...)})
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateGoPathVectors {
		if err := os.WriteFile(gopathVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", gopathVectorsFile, err)
		}
		t.Logf("wrote %s", gopathVectorsFile)
		return
	}

	frozen, err := os.ReadFile(gopathVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-gopath-vectors): %v", gopathVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this Go toolchain produces different results.\n"+
			"Regenerate with -update-gopath-vectors and check what moved — the Rust "+
			"port in native/gopath.rs reads the same file and will fail against it.",
			gopathVectorsFile)
	}
}
