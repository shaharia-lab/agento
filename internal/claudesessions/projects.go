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

// walkProjects reads the project directories directly. It counts the .jsonl
// files in each, which is the same count projectsFromDiskFiles derives.
func walkProjects() ([]ClaudeProject, error) {
	// Projects are keyed by their encoded directory name, so the same project
	// worked on under two config dirs folds into one entry with the counts
	// summed — which is what a project list should say, since the project is
	// one project whichever account opened it.
	byName := make(map[string]int)
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
			SessionCount: byName[name],
		})
	}
	sortProjects(projects)
	return projects, nil
}

// walkProjectsIn accumulates one config dir's projects into byName, appending
// newly seen encoded names to order so the result is deterministic.
func walkProjectsIn(configDir string, byName map[string]int, order *[]string) error {
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
		count := countTranscripts(filepath.Join(projectsDir, e.Name()))
		if count < 0 {
			continue
		}
		if _, seen := byName[e.Name()]; !seen {
			*order = append(*order, e.Name())
		}
		byName[e.Name()] += count
	}
	return nil
}

// countTranscripts returns the number of .jsonl files directly in dir, or -1
// when the directory cannot be listed.
func countTranscripts(dir string) int {
	files, err := os.ReadDir(dir)
	if err != nil {
		return -1
	}
	count := 0
	for _, f := range files {
		if !f.IsDir() && strings.HasSuffix(f.Name(), jsonlExt) {
			count++
		}
	}
	return count
}
