// Command gopath_windows_vectors generates parity/gopath_windows_vectors.json.
//
// It does NOT re-implement Go's Windows `path/filepath`; it vendors it. The
// three sections below are copied verbatim from the Go distribution:
//
//	internal/filepathlite/path.go          — lazybuf, Clean, Base, Dir,
//	                                         VolumeName, IsAbs, FromSlash
//	internal/filepathlite/path_windows.go  — Separator, IsPathSeparator,
//	                                         volumeNameLen, pathHasPrefixFold,
//	                                         uncLen, cutPath, postClean, toUpper
//	path/filepath/path_windows.go          — join (filepath.Join's Windows body)
//
// The only edits are mechanical: the build constraints are dropped so the
// Windows arm compiles on any host, `internal/stringslite` and
// `internal/bytealg` calls become their `strings` equivalents, and
// `os.IsPathSeparator` becomes the local `IsPathSeparator`. Nothing about the
// algorithm is retyped, because a generator that shares the port's belief
// about the rules agrees with a wrong port instead of failing it.
//
// The inputs are Go's own `path/filepath/path_test.go` tables — cleantests +
// wincleantests, jointests + winjointests, basetests + winbasetests, dirtests +
// windirtests — plus the Agento-shaped paths the three un-gated surfaces
// actually build.
package main

import (
	"encoding/json"
	"fmt"
	gofilepath "path/filepath"

	"os"
	"runtime"
	"slices"
	"strings"
)

// ───────────────────────── vendored: internal/filepathlite ────────────────────

const Separator = '\\'

func IsPathSeparator(c uint8) bool { return c == '\\' || c == '/' }

type lazybuf struct {
	path       string
	buf        []byte
	w          int
	volAndPath string
	volLen     int
}

func (b *lazybuf) index(i int) byte {
	if b.buf != nil {
		return b.buf[i]
	}
	return b.path[i]
}

func (b *lazybuf) append(c byte) {
	if b.buf == nil {
		if b.w < len(b.path) && b.path[b.w] == c {
			b.w++
			return
		}
		b.buf = make([]byte, len(b.path))
		copy(b.buf, b.path[:b.w])
	}
	b.buf[b.w] = c
	b.w++
}

func (b *lazybuf) prepend(prefix ...byte) {
	b.buf = slices.Insert(b.buf, 0, prefix...)
	b.w += len(prefix)
}

func (b *lazybuf) string() string {
	if b.buf == nil {
		return b.volAndPath[:b.volLen+b.w]
	}
	return b.volAndPath[:b.volLen] + string(b.buf[:b.w])
}

func Clean(path string) string {
	originalPath := path
	volLen := volumeNameLen(path)
	path = path[volLen:]
	if path == "" {
		if volLen > 1 && IsPathSeparator(originalPath[0]) && IsPathSeparator(originalPath[1]) {
			// should be UNC
			return FromSlash(originalPath)
		}
		return originalPath + "."
	}
	rooted := IsPathSeparator(path[0])

	n := len(path)
	out := lazybuf{path: path, volAndPath: originalPath, volLen: volLen}
	r, dotdot := 0, 0
	if rooted {
		out.append(Separator)
		r, dotdot = 1, 1
	}

	for r < n {
		switch {
		case IsPathSeparator(path[r]):
			r++
		case path[r] == '.' && (r+1 == n || IsPathSeparator(path[r+1])):
			r++
		case path[r] == '.' && path[r+1] == '.' && (r+2 == n || IsPathSeparator(path[r+2])):
			r += 2
			switch {
			case out.w > dotdot:
				out.w--
				for out.w > dotdot && !IsPathSeparator(out.index(out.w)) {
					out.w--
				}
			case !rooted:
				if out.w > 0 {
					out.append(Separator)
				}
				out.append('.')
				out.append('.')
				dotdot = out.w
			}
		default:
			if rooted && out.w != 1 || !rooted && out.w != 0 {
				out.append(Separator)
			}
			for ; r < n && !IsPathSeparator(path[r]); r++ {
				out.append(path[r])
			}
		}
	}

	if out.w == 0 {
		out.append('.')
	}

	postClean(&out)
	return FromSlash(out.string())
}

func FromSlash(path string) string { return replaceStringByte(path, '/', Separator) }

func replaceStringByte(s string, old, new byte) string {
	if strings.IndexByte(s, old) == -1 {
		return s
	}
	n := []byte(s)
	for i := range n {
		if n[i] == old {
			n[i] = new
		}
	}
	return string(n)
}

func Base(path string) string {
	if path == "" {
		return "."
	}
	for len(path) > 0 && IsPathSeparator(path[len(path)-1]) {
		path = path[0 : len(path)-1]
	}
	path = path[len(VolumeName(path)):]
	i := len(path) - 1
	for i >= 0 && !IsPathSeparator(path[i]) {
		i--
	}
	if i >= 0 {
		path = path[i+1:]
	}
	if path == "" {
		return string(Separator)
	}
	return path
}

func Dir(path string) string {
	vol := VolumeName(path)
	i := len(path) - 1
	for i >= len(vol) && !IsPathSeparator(path[i]) {
		i--
	}
	dir := Clean(path[len(vol) : i+1])
	if dir == "." && len(vol) > 2 {
		// must be UNC
		return vol
	}
	return vol + dir
}

func VolumeName(path string) string { return FromSlash(path[:volumeNameLen(path)]) }

func toUpper(c byte) byte {
	if 'a' <= c && c <= 'z' {
		return c - ('a' - 'A')
	}
	return c
}

func volumeNameLen(path string) int {
	switch {
	case len(path) >= 2 && path[1] == ':':
		return 2
	case len(path) == 0 || !IsPathSeparator(path[0]):
		return 0
	case pathHasPrefixFold(path, `\\.\UNC`):
		return uncLen(path, len(`\\.\UNC\`))
	case pathHasPrefixFold(path, `\\.`) ||
		pathHasPrefixFold(path, `\\?`) || pathHasPrefixFold(path, `\??`):
		if len(path) == 3 {
			return 3 // exactly \\.
		}
		_, rest, ok := cutPath(path[4:])
		if !ok {
			return len(path)
		}
		return len(path) - len(rest) - 1
	case len(path) >= 2 && IsPathSeparator(path[1]):
		return uncLen(path, 2)
	}
	return 0
}

func pathHasPrefixFold(s, prefix string) bool {
	if len(s) < len(prefix) {
		return false
	}
	for i := 0; i < len(prefix); i++ {
		if IsPathSeparator(prefix[i]) {
			if !IsPathSeparator(s[i]) {
				return false
			}
		} else if toUpper(prefix[i]) != toUpper(s[i]) {
			return false
		}
	}
	if len(s) > len(prefix) && !IsPathSeparator(s[len(prefix)]) {
		return false
	}
	return true
}

func uncLen(path string, prefixLen int) int {
	count := 0
	for i := prefixLen; i < len(path); i++ {
		if IsPathSeparator(path[i]) {
			count++
			if count == 2 {
				return i
			}
		}
	}
	return len(path)
}

func cutPath(path string) (before, after string, found bool) {
	for i := range path {
		if IsPathSeparator(path[i]) {
			return path[:i], path[i+1:], true
		}
	}
	return path, "", false
}

func postClean(out *lazybuf) {
	if out.volLen != 0 || out.buf == nil {
		return
	}
	for _, c := range out.buf {
		if IsPathSeparator(c) {
			break
		}
		if c == ':' {
			out.prepend('.', Separator)
			return
		}
	}
	if len(out.buf) >= 3 && IsPathSeparator(out.buf[0]) && out.buf[1] == '?' && out.buf[2] == '?' {
		out.prepend(Separator, '.')
	}
}

// IsAbs reports whether the path is absolute. Vendored from
// internal/filepathlite/path_windows.go.
func IsAbs(path string) (b bool) {
	l := volumeNameLen(path)
	if l == 0 {
		return false
	}
	// If the volume name starts with a double slash, this is an absolute path.
	if IsPathSeparator(path[0]) && IsPathSeparator(path[1]) {
		return true
	}
	path = path[l:]
	if path == "" {
		return false
	}
	return IsPathSeparator(path[0])
}

// ─────────────────── vendored: path/filepath/path_windows.go ──────────────────

func Join(elem ...string) string {
	var b strings.Builder
	var lastChar byte
	for _, e := range elem {
		switch {
		case b.Len() == 0:
			// Add the first non-empty path element unchanged.
		case IsPathSeparator(lastChar):
			for len(e) > 0 && IsPathSeparator(e[0]) {
				e = e[1:]
			}
			if b.Len() == 1 && strings.HasPrefix(e, "??") && (len(e) == len("??") || IsPathSeparator(e[2])) {
				b.WriteString(`.\`)
			}
		case lastChar == ':':
			// Keep the path relative to the current directory on a drive.
		default:
			b.WriteByte('\\')
			lastChar = '\\'
		}
		if len(e) > 0 {
			b.WriteString(e)
			lastChar = e[len(e)-1]
		}
	}
	if b.Len() == 0 {
		return ""
	}
	return Clean(b.String())
}

// ────────────────────────────────── inputs ────────────────────────────────────

// Go's `cleantests` + `wincleantests` (path_test.go), then the Agento-shaped
// paths the un-gated surfaces build.
var cleanInputs = []string{
	"abc", "abc/def", "a/b/c", ".", "..", "../..", "../../abc", "/abc", "/",
	"",
	"abc/", "abc/def/", "a/b/c/", "./", "../", "../../", "/abc/",
	"abc//def//ghi", "abc//",
	"abc/./def", "/./abc/def", "abc/.",
	"abc/def/ghi/../jkl", "abc/def/../ghi/../jkl", "abc/def/..", "abc/def/../..",
	"/abc/def/../..", "abc/def/../../..", "/abc/def/../../..",
	"abc/def/../../../ghi/jkl/../../../mno", "/../abc", "a/../b:/../../c",
	"abc/./../def", "abc//./../def", "abc/../../././../def",
	"//abc", "///abc", "//abc//",
	`c:`, `c:\`, `c:\abc`, `c:abc\..\..\.\.\..\def`, `c:\abc\def\..\..`,
	`c:\..\abc`, `c:..\abc`, `c:\b:\..\..\..\d`, `\`, `/`,
	`\\i\..\c$`, `\\i\..\i\c$`, `\\i\..\I\c$`,
	`\\host\share\foo\..\bar`, `//host/share/foo/../baz`,
	`\\host\share\foo\..\..\..\..\bar`,
	`\\.\C:\a\..\..\..\..\bar`, `\\.\C:\\\\a`, `\\a\b\..\c`, `\\a\b`,
	`.\c:`, `.\c:\foo`, `.\c:foo`,
	`\\?\C:\`, `\\?\C:\a`,
	`a/../c:`, `a\..\c:`, `a/../c:/a`, `a/../../c:`, `foo:bar`,
	`/a/../??/a`,
	// Agento shapes: the three surfaces and the four un-gated callers.
	`C:\Users\u\.claude`, `C:\Users\u\.claude\`, `C:\Users\u\.claude\projects`,
	`C:\Users\u/.claude-work`, `C:/Users/u/.claude`,
	`C:\Users\u\AppData\Roaming\..\Local`,
	`\\?\C:\Users\u\.claude\settings.json`,
	`\\nas\home\u\.claude`,
	`~`, `~/Projects`,
	"C:\\Users\\ü\\Projekte\\..\\.claude",
}

// Go's `basetests` + `winbasetests`, plus the encoded project-directory shape
// `sessions/projects.rs` derives.
var baseInputs = []string{
	"", ".", "/.", "/", "////", "x/", "abc", "abc/def", "a/b/.x", "a/b/c.",
	"a/b/c.x",
	`c:\`, `c:.`, `c:\a\b`, `c:a\b`, `c:a\b\c`,
	`\\host\share\`, `\\host\share\a`, `\\host\share\a\b`,
	`C:\Users\u\.claude\projects\-C--Users-u-proj`,
	`C:\Users\u\.claude\projects\-C--Users-u-proj\`,
}

// Go's `dirtests` + `windirtests`, plus the Agento shapes.
var dirInputs = []string{
	"", ".", "/.", "/", "/foo", "x/", "abc", "abc/def", "a/b/.x", "a/b/c.",
	"a/b/c.x", "////",
	`c:\`, `c:.`, `c:\a\b`, `c:a\b`, `c:a\b\c`,
	`\\host\share`, `\\host\share\`, `\\host\share\a`, `\\host\share\a\b`,
	`\\\\`,
	`C:\Users\u\.claude`, `C:\Users\u\.claude\projects\-C--Users-u-proj\s.jsonl`,
	`C:/Users/u/.claude`,
	`\\?\C:\Users\u\.claude`,
}

// Go's `jointests` + `winjointests`, plus the Agento shapes.
var joinInputs = [][]string{
	{},
	{""}, {"/"}, {"a"},
	{"a", "b"}, {"a", ""}, {"", "b"}, {"/", "a"}, {"/", "a/b"}, {"/", ""},
	{"/a", "b"}, {"a", "/b"}, {"/a", "/b"}, {"a/", "b"}, {"a/", ""}, {"", ""},
	{"/", "a", "b"},
	{"//", "a"},
	{`directory`, `file`},
	{`C:\Windows\`, `System32`}, {`C:\Windows\`, ``}, {`C:\`, `Windows`},
	{`C:`, `a`}, {`C:`, `a\b`}, {`C:`, `a`, `b`}, {`C:`, ``, `b`},
	{`C:`, ``, ``, `b`}, {`C:`, ``}, {`C:`, ``, ``}, {`C:`, `\a`},
	{`C:`, ``, `\a`}, {`C:.`, `a`}, {`C:a`, `b`}, {`C:a`, `b`, `d`},
	{`\\host\share`, `foo`}, {`\\host\share\foo`}, {`//host/share`, `foo/bar`},
	{`\`}, {`\`, ``}, {`\`, `a`}, {`\\`, `a`}, {`\`, `a`, `b`},
	{`\\`, `a`, `b`}, {`\`, `\\a\b`, `c`}, {`\\a`, `b`, `c`},
	{`\\a\`, `b`, `c`}, {`//`, `a`},
	{`a:\b\c`, `x\..\y:\..\..\z`}, {`\`, `??\a`},
	// Agento shapes.
	{`C:\Users\u`, `.claude`},
	{`C:\Users\u\.claude`, `settings.json`},
	{`C:\Users\u\.claude`, `settings_profiles.json`},
	{`C:\Users\u\.claude`, `todos`, `abc-agent-abc.json`},
	{`C:\Users\u`, `.claude-work`},
	{`C:\Users\u\.claude`, `projects`},
	{`C:\Users\u\AppData\Local\agento\uploads`, `1-uuid.png`},
	{`C:\Users\ü`, `文档`},
	{`\\nas\home\u`, `.claude`},
	{`C:\Users\u`, `/Projects`},
}

// Go's `basetests` (path_test.go), read through the host's real path/filepath,
// plus the encoded project-directory shape sessions/projects.rs derives.
var unixBaseInputs = []string{
	"", ".", "/.", "/", "////", "x/", "abc", "abc/def", "a/b/.x", "a/b/c.",
	"a/b/c.x",
	"/home/u/.claude/projects/-home-u-proj",
	"/home/u/.claude/projects/-home-u-proj/",
	"/home/ü/文档",
}

// Go's `isabstests` + `winisabstests` (path_test.go), plus the shapes
// `POST /api/fs/mkdir` has to accept and refuse.
var isAbsInputs = []string{
	"", "/", "/usr/bin/gcc", "..", "/a/../bb", ".", "./", "lala",
	`C:\`, `c\`, `c::`, `c:`, `/`, `\`, `\Windows`, `c:a\b`, `c:\a\b`,
	`c:/a/b`, `\\host\share`, `\\host\share\`, `\\host\share\foo`,
	`//host/share/foo/bar`, `\\?\a\b\c`, `\??\a\b\c`,
	`C:\Users\u\Projects`, `C:\Users\u\..\evil`,
}

// Inputs whose volume prefix is the thing under test.
var volumeInputs = []string{
	"", `c:`, `c:\`, `c:\a`, `C:/a`, `\`, `/`, `\\`, `\\host`, `\\host\share`,
	`\\host\share\a`, `//host/share/a`, `\\.\C:\a`, `\\?\C:\a`, `\??\C:\a`,
	`\\.\UNC\host\share\a`, `\\.`, `\\.\`, `\\?\`, `foo:bar`, `a/b`,
	`C:\Users\u\.claude`,
}

type pathCase struct {
	Value string `json:"value"`
	Want  string `json:"want"`
}

type boolCase struct {
	Value string `json:"value"`
	Want  bool   `json:"want"`
}

type joinCase struct {
	Elems []string `json:"elems"`
	Want  string   `json:"want"`
}

type vectors struct {
	Comment    []string   `json:"_comment"`
	Clean      []pathCase `json:"clean"`
	Dir        []pathCase `json:"dir"`
	Join       []joinCase `json:"join"`
	Base       []pathCase `json:"base"`
	VolumeName []pathCase `json:"volume_name"`
	UnixBase   []pathCase `json:"unix_base"`
	IsAbs      []boolCase `json:"is_abs"`
}

func main() {
	// The `unix_base` array is taken from the **host's own** path/filepath, so
	// this generator is only correct on a Unix host: run on Windows it would
	// silently fill that array with Windows answers, and the array exists
	// precisely because `gopath_vectors.json` is frozen and cannot hold it.
	// Loud rather than silent, per parity/README.md.
	if runtime.GOOS == "windows" || runtime.GOOS == "plan9" {
		fmt.Fprintf(os.Stderr,
			"refusing to run on %s: unix_base is read from the host's path/filepath and needs the Unix rules\n",
			runtime.GOOS)
		os.Exit(1)
	}

	v := vectors{Comment: []string{
		"Cross-language parity vectors for Go's path/filepath, WINDOWS rules.",
		"Sibling of gopath_vectors.json (Unix). 'want' is what Go's own",
		"internal/filepathlite and path/filepath Windows source produces, so a",
		"divergence fails the Rust port against Go's real output rather than",
		"against a belief about it. Read by src-tauri/src/native/gopath.rs on",
		"every host, not only on Windows.",
		"Generated from " + runtime.Version() + " with:",
		"  go run parity/generators/gopath_windows_vectors.go > parity/gopath_windows_vectors.json",
		"Inputs are Go's own path_test.go tables (cleantests+wincleantests,",
		"basetests+winbasetests, dirtests+windirtests, jointests+winjointests)",
		"plus the paths Agento's own Windows surfaces build.",
	}}
	for _, s := range cleanInputs {
		v.Clean = append(v.Clean, pathCase{s, Clean(s)})
	}
	for _, s := range dirInputs {
		v.Dir = append(v.Dir, pathCase{s, Dir(s)})
	}
	for _, e := range joinInputs {
		if e == nil {
			e = []string{}
		}
		v.Join = append(v.Join, joinCase{e, Join(e...)})
	}
	for _, s := range baseInputs {
		v.Base = append(v.Base, pathCase{s, Base(s)})
	}
	for _, s := range volumeInputs {
		v.VolumeName = append(v.VolumeName, pathCase{s, VolumeName(s)})
	}
	// The one Unix-rules array with no home in the frozen gopath_vectors.json:
	// filepath.Base is new to this port (sessions/projects.rs needs it), and
	// that file may not move. Produced by the host's real path/filepath, which
	// is the Unix build here.
	for _, s := range unixBaseInputs {
		v.UnixBase = append(v.UnixBase, pathCase{s, gofilepath.Base(s)})
	}
	for _, s := range isAbsInputs {
		v.IsAbs = append(v.IsAbs, boolCase{s, IsAbs(s)})
	}

	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
