package config

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
)

// ClaudeConfigDirEnvVar is the environment variable Claude Code itself reads to
// decide where it keeps credentials, projects and settings. Agento honors the
// same variable so a machine that switches accounts the documented way does not
// have to configure Agento separately.
const ClaudeConfigDirEnvVar = "CLAUDE_CONFIG_DIR"

// claudeConfigDirName is the directory Claude Code uses when the variable is
// unset. It is the fallback for every resolver in this file.
const claudeConfigDirName = ".claude"

// DefaultClaudeConfigDir returns the config dir Claude Code uses out of the box.
//
// The /root fallback mirrors what the scanner has always done when the home
// directory cannot be resolved (a distroless container running as root), so the
// behavior of an install that never configures anything is unchanged.
func DefaultClaudeConfigDir() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return filepath.Join("/root", claudeConfigDirName)
	}
	return filepath.Join(home, claudeConfigDirName)
}

// ClaudeConfigDirFromEnv returns the normalized CLAUDE_CONFIG_DIR value, or ""
// when the variable is unset or blank.
func ClaudeConfigDirFromEnv() string {
	return NormalizeClaudeConfigDir(os.Getenv(ClaudeConfigDirEnvVar))
}

// NormalizeClaudeConfigDir expands a leading ~ and cleans the path. A blank
// input stays blank so callers can distinguish "not set" from a real value.
//
// Normalizing matters beyond tidiness: the resolved dirs are deduplicated by
// string comparison and recorded on cached rows, so "~/.claude", "$HOME/.claude"
// and "$HOME/.claude/" must all collapse to one value or the same corpus would
// be walked twice and attributed to two different dirs.
func NormalizeClaudeConfigDir(p string) string {
	p = strings.TrimSpace(p)
	if p == "" {
		return ""
	}
	if p == "~" || strings.HasPrefix(p, "~/") {
		if home, err := os.UserHomeDir(); err == nil {
			p = filepath.Join(home, strings.TrimPrefix(p, "~"))
		}
	}
	return filepath.Clean(p)
}

// ValidateClaudeConfigDir rejects a path that cannot be a Claude config dir.
//
// A blank value is valid and means "use the default" — the same convention the
// other settings use for an unset preference. Anything else must already exist
// and be a directory: a typo would otherwise surface only as an empty sessions
// list hours later, with nothing to attribute it to.
func ValidateClaudeConfigDir(p string) error {
	p = NormalizeClaudeConfigDir(p)
	if p == "" {
		return nil
	}
	if !filepath.IsAbs(p) {
		return fmt.Errorf("claude config dir must be an absolute path, got %q", p)
	}
	info, err := os.Stat(p)
	if err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("claude config dir %q does not exist", p)
		}
		return fmt.Errorf("claude config dir %q is not readable: %w", p, err)
	}
	if !info.IsDir() {
		return fmt.Errorf("claude config dir %q is not a directory", p)
	}
	return nil
}

// claudeDirs is the process-wide snapshot of the user's Claude config dir
// preferences.
//
// A snapshot rather than a parameter threaded through the call graph, for the
// same reason dataSettings and packagePricing are: the readers are the session
// scanner, the projects list, the journey builder, the settings-profile service
// and the agent runner, none of which have a settings dependency and all of
// which must agree. It is written by ApplyClaudeDirs during startup wiring and
// again whenever the settings are saved; the mutex serializes those writes
// against reads.
//
// It deliberately stores what the user configured rather than the resolved
// paths: the default dir is derived from the home directory on every read, so
// the resolvers stay live the way ClaudeHome always was. Baking an absolute
// path here would make the snapshot outlive a changed HOME, which is precisely
// what the scanner's tests use to isolate themselves from the real corpus.
var claudeDirs = struct {
	sync.RWMutex
	runOverride string
	extra       []string
}{}

// ApplyClaudeDirs installs the user's Claude config dir preferences.
//
// runDir is the dir agent runs target unless an agent overrides it; extra are
// additional dirs to index. Both are advisory: CLAUDE_CONFIG_DIR wins over
// runDir (it is what the surrounding environment has already chosen, and
// SettingsManager refuses to store a conflicting value), and the default dir is
// always indexed so a misconfigured extra can never hide the corpus that was
// there before.
func ApplyClaudeDirs(runDir string, extra []string) {
	normalized := make([]string, 0, len(extra))
	for _, d := range extra {
		if d = NormalizeClaudeConfigDir(d); d != "" {
			normalized = append(normalized, d)
		}
	}

	claudeDirs.Lock()
	defer claudeDirs.Unlock()
	claudeDirs.runOverride = NormalizeClaudeConfigDir(runDir)
	claudeDirs.extra = normalized
}

// dedupeDirs normalizes, drops blanks, and removes duplicates while preserving
// first-seen order.
func dedupeDirs(dirs []string) []string {
	seen := make(map[string]struct{}, len(dirs))
	out := make([]string, 0, len(dirs))
	for _, d := range dirs {
		d = absoluteDir(NormalizeClaudeConfigDir(d))
		if d == "" {
			continue
		}
		if _, dup := seen[d]; dup {
			continue
		}
		seen[d] = struct{}{}
		out = append(out, d)
	}
	return out
}

// absoluteDir returns p when it is an absolute path, and "" otherwise.
//
// Every resolver funnels through this, because a relative config dir has two
// different meanings at once: Agento stats a file in it against the *server's*
// working directory, while Claude Code resolves the --settings path it is given
// against the *subprocess's*. The file checked would not be the file loaded, and
// under a per-agent working directory that second path lands inside the user's
// checked-out repository — where a settings.json carrying hooks and env would be
// read as trusted configuration. There is no correct interpretation of a
// relative value here, so it is discarded rather than guessed at.
//
// It is enforced here rather than only at the API boundary because the value
// can arrive from an agent YAML file, the FS→SQLite import, a hand-edited row,
// or the environment — none of which pass through the service validation.
func absoluteDir(p string) string {
	if p == "" || !filepath.IsAbs(p) {
		return ""
	}
	return p
}

// ClaudeRunConfigDir returns the config dir agent runs target by default:
// CLAUDE_CONFIG_DIR, else the configured run dir, else the default.
func ClaudeRunConfigDir() string {
	if env := absoluteDir(ClaudeConfigDirFromEnv()); env != "" {
		return env
	}
	claudeDirs.RLock()
	run := claudeDirs.runOverride
	claudeDirs.RUnlock()
	if run := absoluteDir(run); run != "" {
		return run
	}
	return DefaultClaudeConfigDir()
}

// ResolveAgentClaudeDir returns the config dir a run for the given agent should
// target: the agent's own override when set, otherwise the global default.
//
// A nil agent resolves to the global default, so callers that run without an
// agent config (the CLI's one-shot path) need no special case.
func ResolveAgentClaudeDir(agentCfg *AgentConfig) string {
	if agentCfg != nil {
		// The service rejects a relative override on save; absoluteDir is the
		// backstop for a value that never passed through it.
		if dir := absoluteDir(NormalizeClaudeConfigDir(agentCfg.ClaudeConfigDir)); dir != "" {
			return dir
		}
	}
	return ClaudeRunConfigDir()
}

// ClaudeConfigDirs returns every config dir Agento indexes, default first.
//
// This is the set the session scanner walks. Reading is a set and running is a
// choice: analytics is retrospective, so restricting it to one dir at a time
// would hide half the history behind a global switch, while a run authenticates
// as exactly one account by definition.
//
// Order is load-bearing — it decides which dir wins a session that exists in
// two of them (see claimSession) — so the default comes first, then the run
// dir, then the user's extras in the order they gave.
func ClaudeConfigDirs() []string {
	claudeDirs.RLock()
	extra := claudeDirs.extra
	claudeDirs.RUnlock()

	dirs := make([]string, 0, len(extra)+2)
	dirs = append(dirs, DefaultClaudeConfigDir(), ClaudeRunConfigDir())
	dirs = append(dirs, extra...)
	return dedupeDirs(dirs)
}

// IsIndexedClaudeDir reports whether the given dir is one Agento indexes.
//
// Rows belonging to a dir the user has removed from the set are filtered out of
// reads rather than deleted — the same rule hidden projects follow. Removing a
// dir is a display choice, and re-adding it must not cost a full re-read of a
// corpus that is still cached and still correct.
//
// The empty string reports true. Migration 27 cannot backfill the column — the
// home directory is not a SQL constant — so rows written before it carry a
// blank until the v13 re-read stamps them, and they belong to the default dir
// because no other dir could be configured then. Admitting blanks also keeps a
// row written by some future path that leaves it unset visible rather than
// silently absent from every figure.
func IsIndexedClaudeDir(dir string) bool {
	dir = NormalizeClaudeConfigDir(dir)
	if dir == "" {
		return true
	}
	for _, d := range ClaudeConfigDirs() {
		if d == dir {
			return true
		}
	}
	return false
}

// DiscoverCandidateClaudeDirs returns config dirs that exist beside the default
// one but are not configured yet, so Settings can offer them instead of asking
// the user to type a path.
//
// The rule is deliberately narrow — a sibling of the default dir whose name
// starts with ".claude" and which contains a "projects" subdirectory. The
// projects check is what keeps ".claude-backup" and ".claude.bak" out: they are
// the common shapes of a directory that merely looks like a config dir. Dirs
// living anywhere else are still added by hand; suggesting is not discovering.
func DiscoverCandidateClaudeDirs() []string {
	def := DefaultClaudeConfigDir()
	parent := filepath.Dir(def)
	entries, err := os.ReadDir(parent)
	if err != nil {
		return nil
	}

	configured := make(map[string]struct{})
	for _, d := range ClaudeConfigDirs() {
		configured[d] = struct{}{}
	}

	out := make([]string, 0, len(entries))
	for _, e := range entries {
		if !e.IsDir() || !strings.HasPrefix(e.Name(), claudeConfigDirName) {
			continue
		}
		candidate := filepath.Join(parent, e.Name())
		if _, already := configured[candidate]; already {
			continue
		}
		if info, statErr := os.Stat(filepath.Join(candidate, "projects")); statErr != nil || !info.IsDir() {
			continue
		}
		out = append(out, candidate)
	}
	sort.Strings(out)
	return out
}
