package claudesessions

import (
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
)

// The project list, cached.
//
// ListProjects used to ReadDir every project directory on every call, to count
// the .jsonl files in it. At 34 projects that is invisible; at the 500 the
// platform is specified for it is 500 syscall round trips per request, and the
// sessions page asks for the list on every mount.
//
// The scan already walks every one of those directories, so the list it implies
// is free — it is published from there. A live walk remains the fallback for the
// window before the first scan finishes, and for any process that never scans.

var projectsCache = struct {
	sync.RWMutex
	projects []ClaudeProject
	loaded   bool
}{}

// cacheProjects publishes the project list a scan just derived. Passing nil
// publishes an empty list, which is what "the projects directory does not
// exist" means — distinct from "not scanned yet".
func cacheProjects(projects []ClaudeProject) {
	projectsCache.Lock()
	defer projectsCache.Unlock()
	projectsCache.projects = projects
	projectsCache.loaded = true
}

// cachedProjects returns the published list, or false when no scan has
// published one yet.
func cachedProjects() ([]ClaudeProject, bool) {
	projectsCache.RLock()
	defer projectsCache.RUnlock()
	if !projectsCache.loaded {
		return nil, false
	}
	out := make([]ClaudeProject, len(projectsCache.projects))
	copy(out, projectsCache.projects)
	return out, true
}

// projectsFromDiskFiles derives the project list from the files a scan walked.
//
// Sub-agent transcripts are excluded from the count so a project's session
// count means what the sessions list shows, not "transcripts on disk" — a
// session that delegated three times would otherwise be counted four.
func projectsFromDiskFiles(onDisk map[string]diskFile) []ClaudeProject {
	type entry struct {
		decoded string
		count   int
	}
	byEncoded := map[string]*entry{}
	for _, df := range onDisk {
		if df.isSubagent {
			continue
		}
		encoded := filepath.Base(filepath.Dir(df.filePath))
		e := byEncoded[encoded]
		if e == nil {
			e = &entry{decoded: df.projectPath}
			byEncoded[encoded] = e
		}
		e.count++
	}

	projects := make([]ClaudeProject, 0, len(byEncoded))
	for encoded, e := range byEncoded {
		projects = append(projects, ClaudeProject{
			EncodedName:  encoded,
			DecodedPath:  e.decoded,
			SessionCount: e.count,
		})
	}
	sortProjects(projects)
	return projects
}

func sortProjects(projects []ClaudeProject) {
	sort.Slice(projects, func(i, j int) bool {
		return projects[i].DecodedPath < projects[j].DecodedPath
	})
}

// ListProjects returns all projects found in ~/.claude/projects/.
//
// Served from the list the last scan published. Before the first scan of the
// process it falls back to walking the directory, so a cold start still offers
// a project picker rather than an empty one.
func ListProjects() ([]ClaudeProject, error) {
	if projects, ok := cachedProjects(); ok {
		return projects, nil
	}
	return walkProjects()
}

// walkProjects reads the project directories directly, as the fallback before
// the first scan publishes a list.
//
// It counts distinct session ids, matching what projectsFromDiskFiles derives
// for every project that has one. The one divergence is a readable but empty
// project directory, which appears here with a count of zero and is absent from
// the scan-derived list — cosmetic, and pre-dates the multi-dir fan-out.
func walkProjects() ([]ClaudeProject, error) {
	// Projects are keyed by their encoded directory name, so the same project
	// worked on under two config dirs folds into one entry — the project is one
	// project whichever account opened it.
	//
	// Session ids are counted in a set rather than summed per dir, so a corpus
	// copied between config dirs is counted once. Summing would disagree with
	// the scan-published list, which dedupes the same way (claimSession), and
	// the fallback would report doubled counts for exactly the setup this
	// supports until the first scan replaced them.
	byName := make(map[string]map[string]struct{})
	var order []string
	for _, dir := range ClaudeHomes() {
		if err := walkProjectsIn(dir, byName, &order); err != nil {
			return nil, err
		}
	}

	projects := make([]ClaudeProject, 0, len(order))
	for _, name := range order {
		projects = append(projects, ClaudeProject{
			EncodedName:  name,
			DecodedPath:  DecodeProjectPath(name),
			SessionCount: len(byName[name]),
		})
	}
	sortProjects(projects)
	return projects, nil
}

// walkProjectsIn accumulates one config dir's projects into byName — a set of
// session ids per encoded project name — appending newly seen names to order so
// the result is deterministic.
func walkProjectsIn(configDir string, byName map[string]map[string]struct{}, order *[]string) error {
	projectsDir := filepath.Join(configDir, "projects")
	entries, err := os.ReadDir(projectsDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}

	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		ids := transcriptIDs(filepath.Join(projectsDir, e.Name()))
		if ids == nil {
			continue
		}
		if _, seen := byName[e.Name()]; !seen {
			*order = append(*order, e.Name())
			byName[e.Name()] = make(map[string]struct{}, len(ids))
		}
		for _, id := range ids {
			byName[e.Name()][id] = struct{}{}
		}
	}
	return nil
}

// transcriptIDs returns the session ids of the .jsonl files directly in dir, or
// nil when the directory cannot be listed.
func transcriptIDs(dir string) []string {
	files, err := os.ReadDir(dir)
	if err != nil {
		return nil
	}
	ids := make([]string, 0, len(files))
	for _, f := range files {
		if !f.IsDir() && strings.HasSuffix(f.Name(), jsonlExt) {
			ids = append(ids, strings.TrimSuffix(f.Name(), jsonlExt))
		}
	}
	return ids
}
