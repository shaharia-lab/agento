// Cross-language vectors for the Claude config-dir rules.
//
// `GET /api/settings/claude-config-dirs` answers two things a port has to get
// exactly right, and neither is a row read:
//
//   - `indexed`, from `config.ClaudeConfigDirs` — the default dir, then the dir
//     a run targets (CLAUDE_CONFIG_DIR beating the configured one, and a
//     relative value being dropped rather than resolved), then the user's
//     extras, deduplicated;
//   - `candidates`, from `config.DiscoverCandidateClaudeDirs` — a filesystem
//     probe whose rule is four clauses long and whose exclusions are the part a
//     natural rewrite gets wrong.
//
// The exclusions are why this file exists rather than a hand-written Rust
// literal. No real `$HOME` contains a `.claude*` symlink, a `.claude*` dir
// without `projects`, one whose `projects` is a *file*, and a plain file with
// the prefix — so the live parity diff structurally cannot re-verify them, and
// an expectation transcribed by hand pins only what its author believed. Here
// the layout is built from this file, the expectations are what Go's own
// functions returned over it, and both languages assert against the result: a
// change to Go's rule fails Go's own suite, and a Rust divergence fails Rust's.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestClaudeDirsVectors -update-golden
//
// Paths are recorded with a literal `$HOME` token because the home directory is
// a fresh temp dir on every run; both readers substitute their own.
//
// Unix-shaped on purpose, exactly as `gopath_vectors.json` is:
// `native/settings.rs` forwards this route on Windows, where `filepath`'s
// volume handling is a different algorithm.
package parity

import (
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/shaharia-lab/agento/internal/config"
)

const claudeDirsVectorsFile = "claude_dirs_vectors.json"

// homeToken stands in for the temp home directory the layout is built in.
const homeToken = "$HOME"

type claudeDirsSymlink struct {
	Link   string `json:"link"`
	Target string `json:"target"`
}

type claudeDirsLayout struct {
	Dirs     []string            `json:"dirs"`
	Files    []string            `json:"files"`
	Symlinks []claudeDirsSymlink `json:"symlinks"`
}

type claudeDirsCase struct {
	Name       string   `json:"name"`
	Env        string   `json:"claude_config_dir_env"`
	RunDir     string   `json:"run_dir"`
	Extra      []string `json:"extra"`
	Indexed    []string `json:"indexed"`
	Candidates []string `json:"candidates"`
}

type claudeDirsVectors struct {
	Comment []string         `json:"_comment"`
	Layout  claudeDirsLayout `json:"layout"`
	Cases   []claudeDirsCase `json:"cases"`
}

// claudeDirsFixture is the home directory both languages build: every clause of
// the discovery rule exercised at once, including the four shapes that look
// like candidates and are not.
//
// `.claude.bak` and `.clauded` are deliberately present *with* a `projects`
// dir. Go's own comment claims the `projects` check "keeps .claude-backup and
// .claude.bak out"; it does not — the prefix match is literal and `projects` is
// the only other filter — and pinning the real answer is the point.
var claudeDirsFixture = claudeDirsLayout{
	Dirs: []string{
		".claude/projects",
		".claude-work/projects",
		".claude.bak/projects",
		".clauded/projects",
		".claude-alpha/projects",
		".claude-zeta/projects",
		// A `.claude*` dir with no `projects` — the `.claude-backup` shape.
		".claude-backup",
		// The right shape under a name without the prefix.
		"notclaude/projects",
		".claude-projfile",
	},
	Files: []string{
		// `projects` exists but is a file, not a directory.
		".claude-projfile/projects",
		// A plain file whose name has the prefix.
		".claude-file",
	},
	Symlinks: []claudeDirsSymlink{
		// `os.ReadDir`'s `DirEntry.IsDir` does not follow a symlink, so a link
		// to a perfectly good candidate is not one.
		{Link: ".claude-link", Target: ".claude-work"},
	},
}

// claudeDirsCases are the configurations asked of the layout. Only the inputs
// are written here; `Indexed` and `Candidates` are filled from Go.
var claudeDirsCases = []claudeDirsCase{
	{
		Name:  "nothing configured: the default dir alone is indexed",
		Extra: []string{},
	},
	{
		Name:   "a configured run dir is indexed and so is not suggested",
		RunDir: homeToken + "/.claude-zeta",
		Extra:  []string{},
	},
	{
		Name:  "extras follow the run dir, in the order they were given",
		Extra: []string{homeToken + "/.claude-work", homeToken + "/.claude-alpha"},
	},
	{
		Name:  "a duplicate, a trailing slash and a relative extra all collapse",
		Extra: []string{homeToken + "/.claude-work/", homeToken + "/.claude-work", "relative/dir"},
	},
	{
		// The run dir here is not one the server could install alongside this
		// env value — the wiring resolves the environment into the settings
		// before calling ApplyClaudeDirs — but the precedence itself is a rule
		// both languages implement, and this is where it is pinned.
		Name:   "CLAUDE_CONFIG_DIR beats the run dir it is given",
		Env:    homeToken + "/.claude-alpha",
		RunDir: homeToken + "/.claude-zeta",
		Extra:  []string{},
	},
	{
		Name:   "a relative CLAUDE_CONFIG_DIR is dropped, not resolved",
		Env:    "relative/dir",
		RunDir: homeToken + "/.claude-zeta",
		Extra:  []string{},
	},
}

func TestClaudeDirsVectors(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("vectors record the Unix rules; the Rust port forwards the route on Windows")
	}

	home := t.TempDir()
	buildClaudeDirsFixture(t, home)
	t.Setenv("HOME", home)
	// ApplyClaudeDirs writes a process-wide snapshot; leave it as it was found.
	t.Cleanup(func() { config.ApplyClaudeDirs("", nil) })

	want := claudeDirsVectors{
		Comment: []string{
			"Cross-language parity vectors for config.ClaudeConfigDirs and",
			"config.DiscoverCandidateClaudeDirs, Unix rules. Generated from Go, then",
			"frozen. Read by desktop/parity/claude_dirs_parity_test.go (Go) and by",
			"desktop/src-tauri/src/native/settings.rs (Rust). 'layout' is the home",
			"directory both languages build; 'indexed' and 'candidates' are exactly what",
			"Go answered over it, so a divergence fails one language against the other's",
			"real output rather than against a belief about it.",
			"$HOME stands for the temp home each side builds the layout in.",
			"Regenerate with: go test ./desktop/parity/ -run TestClaudeDirsVectors -update-golden",
		},
		Layout: claudeDirsFixture,
	}

	for _, tc := range claudeDirsCases {
		t.Setenv(config.ClaudeConfigDirEnvVar, expandHome(tc.Env, home))
		config.ApplyClaudeDirs(expandHome(tc.RunDir, home), expandHomeAll(tc.Extra, home))

		filled := tc
		filled.Indexed = tokenizeHomeAll(config.ClaudeConfigDirs(), home)
		filled.Candidates = tokenizeHomeAll(config.DiscoverCandidateClaudeDirs(), home)
		want.Cases = append(want.Cases, filled)
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateGolden {
		if err := os.WriteFile(claudeDirsVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", claudeDirsVectorsFile, err)
		}
		t.Logf("wrote %s", claudeDirsVectorsFile)
		return
	}

	frozen, err := os.ReadFile(claudeDirsVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-golden): %v", claudeDirsVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this checkout's Go answers differently.\n"+
			"Regenerate with -update-golden and check what moved — the Rust port in "+
			"native/settings.rs reads the same file and will fail against it.",
			claudeDirsVectorsFile)
	}
}

// buildClaudeDirsFixture lays the fixture out under root. The Rust side builds
// the same tree from the same JSON, which is what makes the two comparable.
func buildClaudeDirsFixture(t *testing.T, root string) {
	t.Helper()
	for _, dir := range claudeDirsFixture.Dirs {
		if err := os.MkdirAll(filepath.Join(root, dir), 0o750); err != nil {
			t.Fatalf("creating %s: %v", dir, err)
		}
	}
	for _, file := range claudeDirsFixture.Files {
		if err := os.WriteFile(filepath.Join(root, file), nil, 0o600); err != nil {
			t.Fatalf("creating %s: %v", file, err)
		}
	}
	for _, link := range claudeDirsFixture.Symlinks {
		if err := os.Symlink(filepath.Join(root, link.Target), filepath.Join(root, link.Link)); err != nil {
			t.Fatalf("linking %s: %v", link.Link, err)
		}
	}
}

func expandHome(p, home string) string {
	if p == "" {
		return ""
	}
	return strings.ReplaceAll(p, homeToken, home)
}

func expandHomeAll(paths []string, home string) []string {
	out := make([]string, 0, len(paths))
	for _, p := range paths {
		out = append(out, expandHome(p, home))
	}
	return out
}

func tokenizeHomeAll(paths []string, home string) []string {
	out := make([]string, 0, len(paths))
	for _, p := range paths {
		out = append(out, strings.ReplaceAll(p, home, homeToken))
	}
	return out
}
