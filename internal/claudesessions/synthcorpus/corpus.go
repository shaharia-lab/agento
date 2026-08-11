// Package synthcorpus writes a synthetic ~/.claude/projects tree of Claude Code
// transcripts.
//
// It exists because every scaling figure Agento has about itself was measured
// on one developer's machine — 34 projects, 1,671 transcripts — and then
// extrapolated to the 500-project / 5,000-session corpus the platform is meant
// to survive. An extrapolation cannot fail a build. A generated corpus can, so
// the projections behind the pagination and scanner work become assertions that
// hold or do not.
//
// The transcripts are deliberately *shaped* like real ones rather than merely
// large: user and assistant turns alternate, assistant messages carry per-model
// usage with the cache-TTL split the pricing catalog bills on, sub-agent work
// lives in the sibling `<session-id>/subagents/` directory with isSidechain set
// on every event, and titles, PR links and permission-mode metadata appear in
// the proportions the reference corpus showed. A generator that emitted one
// event type would measure JSON throughput and nothing else.
package synthcorpus

import (
	"encoding/json"
	"fmt"
	"math/rand"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// Options describes the corpus to generate. The zero value is not useful; use
// Small, Medium or Large, or set every field.
type Options struct {
	// Projects is how many project directories to create.
	Projects int
	// Sessions is the total number of parent transcripts, spread evenly across
	// the projects. It is the figure the scaling targets are stated in.
	Sessions int
	// EventsPerSession is the mean number of top-level events per parent
	// transcript; the actual count varies by ±40% so the corpus has a spread of
	// session sizes rather than one size repeated.
	EventsPerSession int
	// SubagentRatio is the fraction of sessions that delegate work, in [0,1].
	// The reference corpus ran 840 sub-agent transcripts against 798 sessions,
	// so roughly half of all sessions delegate, some more than once.
	SubagentRatio float64
	// EventsPerSubagent is the mean event count of a delegated transcript.
	EventsPerSubagent int
	// Span is how far back from Until the sessions are spread. It drives the
	// calendar width of the analytics payload, which is the dimension that grows
	// independently of corpus size.
	Span time.Duration
	// Until is the most recent activity timestamp. Zero means "now", which makes
	// a generated corpus non-deterministic; benchmarks pass a fixed instant.
	Until time.Time
	// Seed makes a corpus reproducible. Two runs with the same Options and Seed
	// produce byte-identical transcripts.
	Seed int64
}

// Small is a corpus roughly the size of a light user's machine. Cheap enough to
// generate inside an ordinary unit test.
func Small() Options {
	return Options{
		Projects: 5, Sessions: 50, EventsPerSession: 60,
		SubagentRatio: 0.4, EventsPerSubagent: 30,
		Span: 90 * 24 * time.Hour, Seed: 1,
	}
}

// Medium is the "already large" corpus the audit measured: ~800 sessions over
// ~34 projects.
func Medium() Options {
	return Options{
		Projects: 34, Sessions: 800, EventsPerSession: 220,
		SubagentRatio: 0.5, EventsPerSubagent: 90,
		Span: 2 * 365 * 24 * time.Hour, Seed: 2,
	}
}

// Large is the target the scaling work is specified against: 500 projects and
// 5,000 sessions.
func Large() Options {
	return Options{
		Projects: 500, Sessions: 5000, EventsPerSession: 220,
		SubagentRatio: 0.5, EventsPerSubagent: 90,
		Span: 3 * 365 * 24 * time.Hour, Seed: 3,
	}
}

// Stats reports what Generate actually wrote.
type Stats struct {
	Projects    int
	Sessions    int
	Subagents   int
	Transcripts int
	Events      int
	Bytes       int64
}

func (s Stats) String() string {
	return fmt.Sprintf("projects=%d sessions=%d subagents=%d transcripts=%d events=%d bytes=%.1fMB",
		s.Projects, s.Sessions, s.Subagents, s.Transcripts, s.Events, float64(s.Bytes)/(1<<20))
}

// models are the identifiers a mixed corpus actually contains: two Anthropic
// tiers, one non-Anthropic backend Claude Code is routinely pointed at, and the
// synthetic placeholder. The mix matters because model attribution, the
// unpriced-token disclosure and the cost-vs-token inversion are all things the
// benchmarks must keep exercising.
var models = []string{
	"claude-opus-4-6-20260115",
	"claude-sonnet-4-5-20250929",
	"claude-haiku-4-5-20251001",
	"kimi-k2-0905-preview",
	"<synthetic>",
}

var permissionModes = []string{"bypassPermissions", "acceptEdits", "plan", "default"}

var toolNames = []string{
	"Read", "Edit", "Write", "Bash", "Grep", "Glob", "Task",
	"mcp__github__list_issues", "mcp__slack__slack_send_message",
}

// filler is what body text and tool payloads are padded with.
//
// Line *size* matters as much as line count: the reference corpus is 1.09 GB
// across 375,291 lines — about 2.9 KB per line, because a Read result or an
// Edit's structuredPatch carries file contents. A generator emitting 400-byte
// lines would measure decode overhead against a corpus an eighth the real size
// and report a scan budget no actual machine could meet.
const filler = "func handle(ctx context.Context, req *Request) (*Response, error) { " +
	"if err := validate(req); err != nil { return nil, fmt.Errorf(\"validating: %w\", err) } " +
	"return svc.Process(ctx, req) }\n"

// pad returns roughly n bytes of plausible source text.
func pad(n int) string {
	if n <= 0 {
		return ""
	}
	var sb strings.Builder
	sb.Grow(n + len(filler))
	for sb.Len() < n {
		sb.WriteString(filler)
	}
	return sb.String()[:n]
}

// Generate writes the corpus under home/.claude/projects, creating home if it
// does not exist. An existing projects directory is removed first, so a
// benchmark that reruns measures a scan of exactly the corpus it asked for.
func Generate(home string, o Options) (Stats, error) {
	if o.Projects <= 0 || o.Sessions <= 0 {
		return Stats{}, fmt.Errorf("synthcorpus: Projects and Sessions must both be positive")
	}
	projectsDir := filepath.Join(home, ".claude", "projects")
	if err := os.RemoveAll(projectsDir); err != nil {
		return Stats{}, fmt.Errorf("synthcorpus: clearing %s: %w", projectsDir, err)
	}
	if err := os.MkdirAll(projectsDir, 0o750); err != nil {
		return Stats{}, fmt.Errorf("synthcorpus: creating %s: %w", projectsDir, err)
	}

	until := o.Until
	if until.IsZero() {
		until = time.Now().UTC()
	}
	rng := rand.New(rand.NewSource(o.Seed)) //nolint:gosec // corpus shape, not security

	g := &generator{opts: o, rng: rng, until: until.UTC(), projectsDir: projectsDir}
	for i := range o.Sessions {
		if err := g.writeSession(i); err != nil {
			return g.stats, err
		}
	}
	g.stats.Projects = o.Projects
	return g.stats, nil
}

type generator struct {
	opts        Options
	rng         *rand.Rand
	until       time.Time
	projectsDir string
	stats       Stats
}

// projectPath is the real filesystem path a project stands for. Claude Code
// encodes it into the directory name; the scanner decodes it back by walking
// the filesystem, so the encoded form has to be one the decoder can resolve.
func (g *generator) projectPath(i int) string {
	return fmt.Sprintf("/home/bench/Projects/repo-%03d", i)
}

func encodeProjectPath(p string) string {
	return strings.ReplaceAll(strings.ReplaceAll(p, "/", "-"), ".", "-")
}

// sessionUUID is a deterministic, correctly-shaped session identifier. Claude
// Code names transcripts by UUID and Agento keys everything on that string, so
// a sequential integer would exercise a different index selectivity than
// production.
func sessionUUID(n int) string {
	return fmt.Sprintf("%08x-%04x-4%03x-8%03x-%012x", n*2654435761&0xffffffff,
		n&0xffff, (n>>4)&0xfff, (n>>8)&0xfff, n*2246822519&0xffffffffffff)
}

func (g *generator) writeSession(idx int) error {
	projectIdx := idx % g.opts.Projects
	path := g.projectPath(projectIdx)
	dir := filepath.Join(g.projectsDir, encodeProjectPath(path))
	if err := os.MkdirAll(dir, 0o750); err != nil {
		return fmt.Errorf("synthcorpus: creating project dir: %w", err)
	}

	id := sessionUUID(idx)
	// Sessions land at a random point in the span, then run forward. Spreading
	// them rather than laying them end to end is what gives the analytics
	// benchmarks a realistic bucket population — a corpus stacked on one day
	// would make every time series a single point.
	offset := time.Duration(g.rng.Int63n(int64(g.opts.Span)))
	start := g.until.Add(-offset)

	events := g.jitter(g.opts.EventsPerSession)
	body, evCount, last := g.transcript(id, path, start, events, false, "")
	file := filepath.Join(dir, id+".jsonl")
	if err := writeFile(file, body); err != nil {
		return err
	}
	g.stats.Sessions++
	g.stats.Transcripts++
	g.stats.Events += evCount
	g.stats.Bytes += int64(len(body))

	if g.rng.Float64() >= g.opts.SubagentRatio {
		return nil
	}
	// A delegating session usually spawns one sub-agent and occasionally three,
	// matching the reference corpus's 840-against-798 ratio.
	n := 1
	if g.rng.Float64() < 0.2 {
		n = 3
	}
	subDir := filepath.Join(dir, id, "subagents")
	if err := os.MkdirAll(subDir, 0o750); err != nil {
		return fmt.Errorf("synthcorpus: creating subagent dir: %w", err)
	}
	for k := range n {
		agentID := fmt.Sprintf("agent-%s-%d", id[:8], k)
		sub, subEvents, _ := g.transcript(id, path, last, g.jitter(g.opts.EventsPerSubagent), true, agentID)
		if err := writeFile(filepath.Join(subDir, agentID+".jsonl"), sub); err != nil {
			return err
		}
		g.stats.Subagents++
		g.stats.Transcripts++
		g.stats.Events += subEvents
		g.stats.Bytes += int64(len(sub))
	}
	return nil
}

// jitter spreads a mean over ±40% so the corpus contains small and large
// transcripts rather than one size repeated. A uniform corpus hides exactly the
// tail behavior — the 20MB transcript, the 1,200-message session — that the
// pagination and detail work exists for.
func (g *generator) jitter(mean int) int {
	if mean < 5 {
		mean = 5
	}
	lo := mean * 6 / 10
	return lo + g.rng.Intn(mean*8/10+1)
}

func writeFile(path string, body []byte) error {
	if err := os.WriteFile(path, body, 0o600); err != nil {
		return fmt.Errorf("synthcorpus: writing %s: %w", path, err)
	}
	return nil
}

// transcript renders one JSONL file and returns it with its event count and
// last timestamp.
//
// Gaps between events are drawn so that most fall inside the idle-gap threshold
// and a few exceed it: active duration is the difference between the two, and a
// corpus whose events are evenly spaced would make active duration equal to the
// wall-clock span, quietly measuring nothing.
func (g *generator) transcript(
	sessionID, projectPath string, start time.Time, events int, sidechain bool, agentID string,
) ([]byte, int, time.Time) {
	var sb strings.Builder
	sb.Grow(events * 3000)
	at := start
	model := models[g.rng.Intn(len(models))]
	branch := fmt.Sprintf("feat/bench-%d", g.rng.Intn(40))
	count := 0

	emit := func(v any) {
		b, err := json.Marshal(v)
		if err != nil {
			return
		}
		sb.Write(b)
		sb.WriteByte('\n')
		count++
	}

	if !sidechain {
		emit(map[string]any{
			"type": "custom-title", "sessionId": sessionID,
			"customTitle": fmt.Sprintf("Bench session %s", sessionID[:8]),
		})
		emit(map[string]any{
			"type": "agent-name", "sessionId": sessionID, "timestamp": at,
			"permissionMode": permissionModes[g.rng.Intn(len(permissionModes))],
		})
	}

	for i := range events {
		at = at.Add(g.gap())
		if i%2 == 0 {
			emit(g.userEvent(sessionID, projectPath, branch, at, sidechain, agentID, i))
			continue
		}
		emit(g.assistantEvent(sessionID, projectPath, branch, at, sidechain, agentID, model, i))
	}

	// One session in eight links a pull request, which is what the has-PR filter
	// and the per-session PR table are sized against.
	if !sidechain && g.rng.Intn(8) == 0 {
		emit(map[string]any{
			"type": "pr-link", "sessionId": sessionID, "timestamp": at,
			"prNumber":     g.rng.Intn(900) + 100,
			"prUrl":        fmt.Sprintf("https://github.com/bench/repo/pull/%d", g.rng.Intn(900)+100),
			"prRepository": "bench/repo",
		})
	}
	return []byte(sb.String()), count, at
}

// gap draws the interval to the next event: usually seconds, sometimes a break
// long enough to end a sitting.
func (g *generator) gap() time.Duration {
	switch n := g.rng.Float64(); {
	case n < 0.85:
		return time.Duration(2+g.rng.Intn(50)) * time.Second
	case n < 0.97:
		return time.Duration(1+g.rng.Intn(8)) * time.Minute
	default:
		// Over the default 10-minute idle threshold: a resume, not a reply.
		return time.Duration(30+g.rng.Intn(600)) * time.Minute
	}
}

func (g *generator) userEvent(
	sessionID, projectPath, branch string, at time.Time, sidechain bool, agentID string, i int,
) map[string]any {
	ev := map[string]any{
		"type": "user", "uuid": fmt.Sprintf("%s-u%d", sessionID[:8], i),
		"sessionId": sessionID, "timestamp": at, "cwd": projectPath,
		"gitBranch": branch, "version": "2.1.224",
		"message": map[string]any{
			"role": "user",
			"content": fmt.Sprintf("Please look at the %s module and report what it does.\n%s",
				branch, pad(200+g.rng.Intn(3000))),
		},
	}
	if sidechain {
		ev["isSidechain"] = true
		ev["attributionAgent"] = agentID
	}
	return ev
}

func (g *generator) assistantEvent(
	sessionID, projectPath, branch string, at time.Time,
	sidechain bool, agentID, model string, i int,
) map[string]any {
	blocks := []map[string]any{
		{"type": "text", "text": "Looking at that now — here is what I found in the module.\n" +
			pad(400+g.rng.Intn(2200))},
	}
	// Roughly half of assistant turns call a tool, which is what the attribution
	// and tool-usage processors are measured on.
	if g.rng.Float64() < 0.5 {
		blocks = append(blocks, map[string]any{
			"type": "tool_use",
			"id":   fmt.Sprintf("toolu_%s_%d", sessionID[:8], i),
			"name": toolNames[g.rng.Intn(len(toolNames))],
			"input": map[string]any{
				"file_path": projectPath + "/internal/service/handler.go",
				"pattern":   branch,
				"content":   pad(1000 + g.rng.Intn(6000)),
			},
		})
	}

	cache5m := g.rng.Intn(4000)
	cache1h := g.rng.Intn(500)
	ev := map[string]any{
		"type": "assistant", "uuid": fmt.Sprintf("%s-a%d", sessionID[:8], i),
		"sessionId": sessionID, "timestamp": at, "cwd": projectPath,
		"gitBranch": branch, "version": "2.1.224",
		"message": map[string]any{
			"role": "assistant", "model": model, "content": blocks,
			"usage": map[string]any{
				"input_tokens":                g.rng.Intn(900) + 20,
				"output_tokens":               g.rng.Intn(1800) + 30,
				"cache_creation_input_tokens": cache5m + cache1h,
				"cache_read_input_tokens":     g.rng.Intn(40000),
				"cache_creation": map[string]any{
					"ephemeral_5m_input_tokens": cache5m,
					"ephemeral_1h_input_tokens": cache1h,
				},
			},
		},
	}
	if sidechain {
		ev["isSidechain"] = true
		ev["attributionAgent"] = agentID
	}
	return ev
}
