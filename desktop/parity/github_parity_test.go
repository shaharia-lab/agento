// Cross-language vectors for the GitHub integration MCP server
// (`internal/integrations/github`, ported in #312 to
// `desktop/src-tauri/src/native/integrations/github/`).
//
// Four things about that server have to match byte for byte, and none of them
// is checkable from one language alone:
//
//   - **Which tools are hosted.** `buildAllowedSet` unions the `Tools` of every
//     *enabled* service and each `register*` skips a tool the set does not name
//     — except that an **empty** set registers everything, which is the rule a
//     port gets backwards. The names travel in every agent's stored
//     `capabilities.mcp` allowlist and in every `tool_use` block already written
//     to `chat_messages`, so a missing or renamed tool is a silent break of
//     agents that exist.
//   - **The advertised schema.** `tools/list` hands the CLI what the CLI hands
//     the model. It is reflected off the params struct by
//     `google/jsonschema-go`, and `schemars` — the Rust reflector — does not
//     agree with it by default. `jsonschema_reflect_vectors.json` is the map of
//     where the two diverge and what a port must write instead; this file is
//     where the twenty real schemas are pinned.
//   - **The request each tool builds.** `url.PathEscape` per segment,
//     `url.Values.Encode`'s sorted keys and `+`-for-space, the per-tool
//     `per_page` clamp, and `json.Marshal`'s sorted map keys and HTML escaping
//     in every request body. None of that is visible in a response, so the fake
//     GitHub below records the request it received and the vectors carry it.
//   - **The result text.** A tool's result is what the model reads and what
//     gets persisted, and on the error path it is the *message* — `new_tool`
//     and `mcp.AddTool` both pack a failed call into `CallToolResult` with
//     `IsError` rather than raising a protocol error. The raw GitHub bytes are
//     passed through verbatim, so a port that round-trips them through a JSON
//     value would reorder keys and respell numbers.
//
// Everything here is taken from the **running server** over its real HTTP MCP
// transport — `github.Start` builds it exactly as `cmd/web.go` does — so a
// change to the registration path, the client or a sentence fails this test
// rather than passing a stale belief through.
//
// The Rust half lives in
// `desktop/src-tauri/src/native/integrations/github/tests_vectors.rs` and reads
// this same file.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestGitHubVectors -update-github-vectors
package parity

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"sort"
	"strings"
	"sync"
	"testing"
	"time"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/integrations/github"
)

const githubVectorsFile = "github_vectors.json"

var updateGitHubVectors = flag.Bool("update-github-vectors", false,
	"rewrite github_vectors.json from this Go toolchain")

// githubParityToken is the personal access token every recorded request carries.
// It is a fixture, not a secret — and recording the whole `Authorization` header
// is deliberate: `Bearer ` versus GitHub's older `token ` prefix is exactly the
// kind of thing a port gets wrong and no response would reveal.
// A fixture, and gosec's pattern match is exactly right about the shape —
// which is the point: it has to look like a PAT for the vectors to pin the
// `Bearer ` prefix. It authenticates nothing.
const githubParityToken = "parity-pat-token" //nolint:gosec // a test fixture, not a credential

// githubParityID is the integration id, which is half of the server name
// (`github-<id>`) and therefore half of every `mcp__github-<id>__<tool>` in an
// agent's allowlist.
const githubParityID = "gh-parity"

type githubToolVector struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
}

// githubRequestVector is what the fake GitHub saw. `Target` is
// `URL.RequestURI()` — the encoded path plus the encoded, sorted query — which
// is the one string that pins `url.PathEscape` and `url.Values.Encode`
// together.
type githubRequestVector struct {
	Method        string `json:"method"`
	Target        string `json:"target"`
	Authorization string `json:"authorization"`
	Accept        string `json:"accept"`
	// Set only when the tool sent a body; `call` adds the header only then.
	ContentType string `json:"content_type"`
	Body        string `json:"body"`
}

// githubResponseScript is what the fake GitHub answered with. The Rust half
// replays exactly this, so the two languages see the same bytes.
type githubResponseScript struct {
	Status int `json:"status"`
	// Sent only when non-empty. The 302-with and 302-without pair is the whole
	// of `getRedirectURL`'s contract.
	Location string `json:"location,omitempty"`
	Body     string `json:"body"`
}

type githubCallVector struct {
	Case      string               `json:"case"`
	Tool      string               `json:"tool"`
	Arguments json.RawMessage      `json:"arguments"`
	Response  githubResponseScript `json:"response"`
	// nil when the tool failed before it made a request — the only such path is
	// `trigger_workflow`'s inputs parse.
	Request *githubRequestVector `json:"request"`
	IsError bool                 `json:"is_error"`
	Text    string               `json:"text"`
	// Set only where Rust cannot reproduce Go's text and the difference is
	// pinned rather than hidden. See `RustText`'s only users below: Go's
	// `encoding/json` *syntax* errors come out of a hand-written scanner whose
	// messages have no serde_json equivalent.
	RustText string `json:"rust_text,omitempty"`
	// Likewise for the request, and there is exactly one user: an integer
	// argument above 2^53 arrives at a Go handler **rounded**, because
	// `modelcontextprotocol/go-sdk` unmarshals `arguments` into a
	// `map[string]any`, applies schema defaults and re-marshals before the
	// typed decode (`mcp/tool.go`). `rmcp` deserializes straight into the
	// input struct, so Rust keeps the exact value. Unreachable with real
	// GitHub run ids, which are eleven digits.
	RustTarget string `json:"rust_target,omitempty"`
}

type githubHostingVector struct {
	Case     string                          `json:"case"`
	Services map[string]config.ServiceConfig `json:"services"`
	Tools    []string                        `json:"tools"`
}

type githubVectors struct {
	Comment       []string              `json:"_comment"`
	IntegrationID string                `json:"integration_id"`
	ServerName    string                `json:"server_name"`
	Version       string                `json:"version"`
	Token         string                `json:"token"`
	Tools         []githubToolVector    `json:"tools"`
	Hosting       []githubHostingVector `json:"hosting"`
	Calls         []githubCallVector    `json:"calls"`
}

// ─── The fake GitHub ─────────────────────────────────────────────────────────

// fakeGitHub plays a scripted GitHub and records what it was asked.
//
// One handler for every path, because the point is not to model the API — it is
// to capture the request the tool *built*. A router would have to agree with the
// port about the shape of each path, which is precisely the thing under test.
type fakeGitHub struct {
	mu     sync.Mutex
	script githubResponseScript
	seen   *githubRequestVector
}

func (f *fakeGitHub) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		body = nil
	}
	f.mu.Lock()
	script := f.script
	f.seen = &githubRequestVector{
		Method:        r.Method,
		Target:        r.URL.RequestURI(),
		Authorization: r.Header.Get("Authorization"),
		Accept:        r.Header.Get("Accept"),
		ContentType:   r.Header.Get("Content-Type"),
		Body:          string(body),
	}
	f.mu.Unlock()

	if script.Location != "" {
		w.Header().Set("Location", script.Location)
	}
	w.WriteHeader(script.Status)
	_, _ = w.Write([]byte(script.Body))
}

// arm sets the next reply and forgets the last request.
func (f *fakeGitHub) arm(script githubResponseScript) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.script = script
	f.seen = nil
}

func (f *fakeGitHub) recorded() *githubRequestVector {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.seen
}

// ─── Standing the real server up ─────────────────────────────────────────────

// githubSession starts the real integration for services and returns a client
// session against it. `github.Start` is the app's own entry point, so the
// authentication check, the credential parse, the server name and the tool
// registration are all the shipped ones.
func githubSession(
	t *testing.T, ctx context.Context, services map[string]config.ServiceConfig,
) *mcp.ClientSession {
	t.Helper()

	// gosec flags `GitHubCredentials` because the type has a `client_secret`
	// field — the OAuth mode this integration does not use. The value marshaled
	// here is the fixture above, and it reaches nothing but a loopback fake.
	credentials, err := json.Marshal(config.GitHubCredentials{ //nolint:gosec // fixture credentials
		AuthMode:            "pat",
		PersonalAccessToken: githubParityToken,
	})
	if err != nil {
		t.Fatalf("encoding credentials: %v", err)
	}

	cfg := &config.IntegrationConfig{
		ID:          githubParityID,
		Name:        "GitHub (parity)",
		Type:        "github",
		Enabled:     true,
		Credentials: credentials,
		// `IsAuthenticated` is "present and not the four bytes null"; a PAT
		// integration stores the validated login here.
		Auth:     json.RawMessage(`{"login":"octocat"}`),
		Services: services,
	}

	serverCfg, err := github.Start(ctx, cfg)
	if err != nil {
		t.Fatalf("starting the github integration: %v", err)
	}

	client := mcp.NewClient(&mcp.Implementation{Name: "parity", Version: "1"}, nil)
	session, err := client.Connect(ctx,
		&mcp.StreamableClientTransport{Endpoint: serverCfg.URL}, nil)
	if err != nil {
		t.Fatalf("connecting to %s: %v", serverCfg.URL, err)
	}
	t.Cleanup(func() { _ = session.Close() })
	return session
}

// allServices enables every service with an empty tool list, which is the
// "empty allowed set registers everything" rule — the configuration the twenty
// schemas and every call vector are taken under.
func allServices() map[string]config.ServiceConfig {
	return map[string]config.ServiceConfig{
		"repos":         {Enabled: true},
		"issues":        {Enabled: true},
		"pull_requests": {Enabled: true},
		"actions":       {Enabled: true},
		"releases":      {Enabled: true},
	}
}

// hostingCases pin the three gates that decide what a server hosts, each in the
// shape a real integration row takes.
var hostingCases = []githubHostingVector{
	{Case: "every service enabled, no tool lists — an empty allowed set hosts everything",
		Services: allServices()},
	{Case: "no services at all", Services: map[string]config.ServiceConfig{}},
	{Case: "a disabled service contributes neither its gate nor its tools",
		Services: map[string]config.ServiceConfig{
			"repos":  {Enabled: true, Tools: []string{"list_repos"}},
			"issues": {Enabled: false, Tools: []string{"list_issues"}},
		}},
	{Case: "a non-empty allowed set is a filter across every enabled service",
		Services: map[string]config.ServiceConfig{
			"repos":         {Enabled: true, Tools: []string{"get_repo"}},
			"issues":        {Enabled: true, Tools: []string{"create_issue", "update_issue"}},
			"pull_requests": {Enabled: true, Tools: []string{"get_pull_diff"}},
			"actions":       {Enabled: true, Tools: []string{"get_run_logs"}},
			"releases":      {Enabled: true, Tools: []string{"list_tags"}},
		}},
	// The union is what makes this one non-obvious: `repos` names a tool that
	// `issues` registers, so enabling `issues` with no list of its own does not
	// widen the set — but the name from `repos` still admits `list_issues`.
	{Case: "the allowed set is a union over services, not a per-service list",
		Services: map[string]config.ServiceConfig{
			"repos":  {Enabled: true, Tools: []string{"list_repos", "list_issues"}},
			"issues": {Enabled: true},
		}},
	{Case: "a tool named by a disabled service is not allowed anywhere",
		Services: map[string]config.ServiceConfig{
			"repos":  {Enabled: true, Tools: []string{"list_repos"}},
			"issues": {Enabled: true, Tools: nil},
			"nope":   {Enabled: false, Tools: []string{"get_repo"}},
		}},
}

// ─── The call cases ──────────────────────────────────────────────────────────

// okBody is a response body deliberately hostile to a re-encode: keys out of
// alphabetical order, a trailing-zero decimal, an integer too large for a
// float64 to hold exactly, and interior whitespace. Go passes the bytes through
// verbatim, so all four survive into the result text; a port that decoded to a
// JSON value and re-encoded would change every one of them.
const okBody = `[{"zebra":1,"id":10152021304050607,"rate":1.50, "name":"x"}]`

func jsonArgs(t *testing.T, args map[string]any) json.RawMessage {
	t.Helper()
	encoded, err := json.Marshal(args)
	if err != nil {
		t.Fatalf("encoding arguments: %v", err)
	}
	return encoded
}

// callCase is a call vector before the live run fills in what happened.
type callCase struct {
	name       string
	tool       string
	args       map[string]any
	script     githubResponseScript
	rustText   string
	rustTarget string
}

func ok(body string) githubResponseScript {
	return githubResponseScript{Status: http.StatusOK, Body: body}
}

func created(body string) githubResponseScript {
	return githubResponseScript{Status: http.StatusCreated, Body: body}
}

// githubCallCases is the exercise: every tool at least once on its success
// path, plus every distinct failure the client can produce.
//
// The argument maps are complete rather than minimal because **every field is
// required** — `google/jsonschema-go` marks a field optional only on
// `omitempty`/`omitzero`, and not one params struct in this integration carries
// either, so the server refuses a call that omits a field. That is itself part
// of the advertised surface, and it is why the empty-string and zero cases
// below are written as explicit zeros rather than as absent keys.
func githubCallCases() []callCase {
	const owner, repo = "my org", "a/b"
	return []callCase{
		// ─── repos ───────────────────────────────────────────────────────────
		{
			name:   "list_repos/zero values take the default page size and omit page",
			tool:   "list_repos",
			args:   map[string]any{"visibility": "", "sort": "", "per_page": 0, "page": 0},
			script: ok(okBody),
		},
		{
			name: "list_repos/every filter set, and per_page over 100 falls back to 30",
			tool: "list_repos",
			args: map[string]any{
				"visibility": "private", "sort": "full_name", "per_page": 101, "page": 3,
			},
			script: ok(okBody),
		},
		{
			name:   "list_repos/per_page 100 is the largest value that survives the clamp",
			tool:   "list_repos",
			args:   map[string]any{"visibility": "", "sort": "", "per_page": 100, "page": 1},
			script: ok(okBody),
		},
		{
			name:   "list_repos/a negative per_page is a zero, not an error",
			tool:   "list_repos",
			args:   map[string]any{"visibility": "", "sort": "", "per_page": -5, "page": 0},
			script: ok(okBody),
		},
		{
			name:   "get_repo/a space and a slash in the segments, PathEscape'd",
			tool:   "get_repo",
			args:   map[string]any{"owner": owner, "repo": repo},
			script: ok(`{"full_name":"my org/a/b"}`),
		},
		{
			// `PathEscape` is `encodePathSegment`, which is not the
			// `encodePath` `gourl::escape_path` already carries: a segment
			// escapes `/ ; , ?` and leaves `$ & + : = @` alone. Both halves are
			// here, plus multi-byte UTF-8, which becomes one `%XX` per byte in
			// uppercase hex.
			name:   "get_repo/the reserved bytes a segment keeps, the ones it escapes, and UTF-8",
			tool:   "get_repo",
			args:   map[string]any{"owner": "a$&+:=@b", "repo": "café;日本語,x?y"},
			script: ok(`{"full_name":"x"}`),
		},
		{
			name: "get_repo/a non-2xx status is the API's own body, verbatim",
			tool: "get_repo",
			args: map[string]any{"owner": "octocat", "repo": "ghost"},
			script: githubResponseScript{
				Status: http.StatusNotFound,
				Body:   `{"message":"Not Found","status":"404"}`,
			},
		},
		{
			name:   "get_repo/a 500 with an empty body still names the status",
			tool:   "get_repo",
			args:   map[string]any{"owner": "octocat", "repo": "ghost"},
			script: githubResponseScript{Status: http.StatusInternalServerError, Body: ""},
		},
		{
			// The one query value a user really types. `+`, the space and `~`
			// are the three bytes where Go's QueryEscape, WHATWG form encoding
			// and RFC 3986 all disagree.
			name: "search_code/QueryEscape, where a space is + and a tilde is not escaped",
			tool: "search_code",
			args: map[string]any{
				// `~` and `*` are the two bytes where Go's `QueryEscape` and
				// WHATWG form encoding disagree in *opposite* directions: Go
				// keeps `~` and escapes `*`, `form_urlencoded` does the
				// reverse. A port that reached for the crate already in the
				// tree would get both wrong and neither would show up in a
				// response.
				"query": "repo:o/r func foo+bar ~x/y&z=1 *", "per_page": 0, "page": 0,
			},
			script: ok(`{"total_count":1,"items":[]}`),
		},

		// ─── issues ──────────────────────────────────────────────────────────
		{
			name: "list_issues/every filter, and the sorted query Values.Encode produces",
			tool: "list_issues",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"state": "all", "labels": "bug,help wanted", "sort": "updated",
				"per_page": 50, "page": 2,
			},
			script: ok(okBody),
		},
		{
			name: "list_issues/empty filters drop out of the query entirely",
			tool: "list_issues",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"state": "", "labels": "", "sort": "", "per_page": 0, "page": 0,
			},
			script: ok(okBody),
		},
		{
			name:   "get_issue/the number is formatted with %d, not escaped",
			tool:   "get_issue",
			args:   map[string]any{"owner": "octocat", "repo": "hello-world", "number": 1347},
			script: ok(`{"number":1347}`),
		},
		{
			name: "create_issue/a full body, whose keys json.Marshal sorts",
			tool: "create_issue",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"title": "Found a bug", "body": "It <broke> & burned",
				"labels": "bug, help wanted ,", "assignees": "octocat,hubot",
			},
			script: created(`{"number":1347}`),
		},
		{
			name: "create_issue/only the required title reaches the body",
			tool: "create_issue",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"title": "Bare", "body": "", "labels": "", "assignees": "",
			},
			script: created(`{"number":1348}`),
		},
		{
			// splitCSV returns a **nil** slice for a string that is all
			// separators, and a nil slice marshals as `null` — not `[]`.
			name: "create_issue/a labels string of only separators is a null, not an empty array",
			tool: "create_issue",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"title": "Bare", "body": "", "labels": " , , ", "assignees": "",
			},
			script: created(`{"number":1349}`),
		},
		{
			name: "update_issue/every field set",
			tool: "update_issue",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "number": 1347,
				"title": "New", "body": "Text", "state": "closed", "labels": "bug",
			},
			script: ok(`{"number":1347,"state":"closed"}`),
		},
		{
			// Nothing is unconditional in this body, so an all-empty update
			// sends the two bytes `{}` — and still sends the Content-Type
			// header, because `call` keys that on `body != nil`.
			name: "update_issue/an empty update still sends a body, and it is {}",
			tool: "update_issue",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "number": 1347,
				"title": "", "body": "", "state": "", "labels": "",
			},
			script: ok(`{"number":1347}`),
		},

		// ─── pull_requests ───────────────────────────────────────────────────
		{
			name: "list_pulls/every filter",
			tool: "list_pulls",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"state": "open", "sort": "popularity", "base": "main",
				"per_page": 10, "page": 0,
			},
			script: ok(okBody),
		},
		{
			name:   "get_pull/the same %d path shape as get_issue",
			tool:   "get_pull",
			args:   map[string]any{"owner": "octocat", "repo": "hello-world", "number": 42},
			script: ok(`{"number":42}`),
		},
		{
			name: "create_pull/draft true adds the key; the three required fields are unconditional",
			tool: "create_pull",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"title": "A PR", "head": "feature", "base": "main",
				"body": "Body & <b>", "draft": true,
			},
			script: created(`{"number":43}`),
		},
		{
			// `if p.Draft` — false omits the key rather than sending `false`.
			name: "create_pull/draft false omits the key entirely",
			tool: "create_pull",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"title": "A PR", "head": "feature", "base": "main",
				"body": "", "draft": false,
			},
			script: created(`{"number":44}`),
		},
		{
			// The only tool on `callRaw`: a caller-chosen Accept, a 10 MB cap
			// and a body that is not JSON at all.
			name:   "get_pull_diff/a non-JSON body under a caller-chosen Accept",
			tool:   "get_pull_diff",
			args:   map[string]any{"owner": "octocat", "repo": "hello-world", "number": 42},
			script: ok("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n"),
		},
		{
			name: "get_pull_diff/callRaw reports a non-2xx exactly as call does",
			tool: "get_pull_diff",
			args: map[string]any{"owner": "octocat", "repo": "hello-world", "number": 42},
			script: githubResponseScript{
				Status: http.StatusNotAcceptable, Body: `{"message":"Unsupported media type"}`,
			},
		},
		{
			name: "list_pull_comments/no filters at all beyond paging",
			tool: "list_pull_comments",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "number": 42,
				"per_page": 0, "page": 0,
			},
			script: ok(okBody),
		},

		// ─── actions ─────────────────────────────────────────────────────────
		{
			name: "list_workflows/paging only",
			tool: "list_workflows",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "per_page": 0, "page": 0,
			},
			script: ok(okBody),
		},
		{
			name: "list_workflow_runs/no workflow_id takes the repository-wide path",
			tool: "list_workflow_runs",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "workflow_id": "",
				"status": "completed", "branch": "main", "per_page": 0, "page": 0,
			},
			script: ok(okBody),
		},
		{
			name: "list_workflow_runs/a workflow_id takes the per-workflow path and is PathEscape'd",
			tool: "list_workflow_runs",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "workflow_id": "ci build.yml",
				"status": "", "branch": "", "per_page": 0, "page": 0,
			},
			script: ok(okBody),
		},
		{
			name: "trigger_workflow/inputs parse into a map[string]string and are sorted",
			tool: "trigger_workflow",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"workflow_id": "ci.yml", "ref": "main",
				"inputs": `{"zebra":"z","alpha":"a"}`,
			},
			script: githubResponseScript{Status: http.StatusNoContent, Body: ""},
		},
		{
			name: "trigger_workflow/no inputs at all sends only the ref",
			tool: "trigger_workflow",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"workflow_id": "ci.yml", "ref": "main", "inputs": "",
			},
			script: githubResponseScript{Status: http.StatusNoContent, Body: ""},
		},
		{
			// A literal `null` unmarshals into a **nil map** without error, and
			// a nil map marshals as `null`. The guard is `!= ""`, so this
			// reaches the parse.
			name: "trigger_workflow/a literal null parses to a nil map and is sent as null",
			tool: "trigger_workflow",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"workflow_id": "ci.yml", "ref": "main", "inputs": "null",
			},
			script: githubResponseScript{Status: http.StatusNoContent, Body: ""},
		},
		{
			name: "trigger_workflow/an array is not a JSON object",
			tool: "trigger_workflow",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"workflow_id": "ci.yml", "ref": "main", "inputs": `["a","b"]`,
			},
			script: githubResponseScript{Status: http.StatusNoContent, Body: ""},
		},
		{
			name: "trigger_workflow/a non-string value is not a map[string]string",
			tool: "trigger_workflow",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"workflow_id": "ci.yml", "ref": "main", "inputs": `{"a":1}`,
			},
			script: githubResponseScript{Status: http.StatusNoContent, Body: ""},
		},
		{
			// The one place Rust cannot follow: `encoding/json`'s scanner has
			// its own vocabulary of syntax errors, and serde_json's is
			// different. Pinned as a divergence rather than papered over —
			// see `rust_text`.
			name: "trigger_workflow/a syntactically invalid payload — the one divergence",
			tool: "trigger_workflow",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"workflow_id": "ci.yml", "ref": "main", "inputs": `not json`,
			},
			script: githubResponseScript{Status: http.StatusNoContent, Body: ""},
			// Hand-written, and the only value in this file that is: it is
			// what `serde_json` says, so it is updated by editing this line
			// rather than by regenerating from Go. The Rust test fails
			// against it if serde's wording ever moves, which is the point.
			rustText: "parsing workflow inputs (must be a JSON object): " +
				"expected ident at line 1 column 2",
		},
		{
			// Truncation is reproducible, so it is pinned: serde and
			// encoding/json agree that an unterminated document ended early,
			// and Go's wording is short enough to reproduce exactly.
			name: "trigger_workflow/a truncated payload ends the same way in both languages",
			tool: "trigger_workflow",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world",
				"workflow_id": "ci.yml", "ref": "main", "inputs": `{"a":`,
			},
			script: githubResponseScript{Status: http.StatusNoContent, Body: ""},
		},
		{
			name: "get_workflow_run/a run id inside float64's exact range",
			tool: "get_workflow_run",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "run_id": 15678900123,
			},
			script: ok(`{"id":15678900123}`),
		},
		{
			// The one place the *request* diverges, and it is the Go MCP SDK's
			// doing rather than this integration's: `mcp/tool.go` unmarshals
			// `arguments` into a `map[string]any`, applies schema defaults and
			// re-marshals before the typed decode, so an `int64` above 2^53
			// reaches the handler rounded. `rmcp` deserializes straight into
			// the input struct and keeps the exact value. Pinned rather than
			// reproduced — deliberately degrading Rust to match would be worse
			// than the divergence, which no real GitHub run id can reach.
			name: "get_workflow_run/a run id above 2^53 — the Go SDK rounds it, Rust does not",
			tool: "get_workflow_run",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "run_id": 10152021304050607,
			},
			script:     ok(`{"id":10152021304050607}`),
			rustTarget: "/repos/octocat/hello-world/actions/runs/10152021304050607",
		},
		{
			name: "get_run_logs/a 302 with a Location is the answer",
			tool: "get_run_logs",
			args: map[string]any{"owner": "octocat", "repo": "hello-world", "run_id": 42},
			script: githubResponseScript{
				Status:   http.StatusFound,
				Location: "https://objects.example.invalid/logs.zip?token=abc",
				Body:     "",
			},
		},
		{
			name: "get_run_logs/a 301 is a redirect too",
			tool: "get_run_logs",
			args: map[string]any{"owner": "octocat", "repo": "hello-world", "run_id": 42},
			script: githubResponseScript{
				Status:   http.StatusMovedPermanently,
				Location: "https://objects.example.invalid/logs.zip",
				Body:     "",
			},
		},
		{
			name:   "get_run_logs/a 302 with no Location is its own sentence",
			tool:   "get_run_logs",
			args:   map[string]any{"owner": "octocat", "repo": "hello-world", "run_id": 42},
			script: githubResponseScript{Status: http.StatusFound, Body: ""},
		},
		{
			// getRedirectURL reads at most 512 bytes of a non-redirect body,
			// which is a different cap from `call`'s 2 MiB — so the error text
			// is truncated mid-JSON. 700 `x`s make the cut visible.
			name: "get_run_logs/a non-redirect body is truncated at 512 bytes",
			tool: "get_run_logs",
			args: map[string]any{"owner": "octocat", "repo": "hello-world", "run_id": 42},
			script: githubResponseScript{
				Status: http.StatusInternalServerError,
				Body:   `{"message":"` + strings.Repeat("x", 700) + `"}`,
			},
		},

		// ─── releases ────────────────────────────────────────────────────────
		{
			name: "list_releases/paging only",
			tool: "list_releases",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "per_page": 0, "page": 0,
			},
			script: ok(okBody),
		},
		{
			name: "create_release/every optional key present",
			tool: "create_release",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "tag_name": "v1.0.0",
				"name": "Version 1.0.0", "body": "Notes & <em>more</em>",
				"target_commitish": "main", "draft": true, "prerelease": true,
				"generate_release_notes": true,
			},
			script: created(`{"id":1,"tag_name":"v1.0.0"}`),
		},
		{
			name: "create_release/false booleans omit their keys",
			tool: "create_release",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "tag_name": "v1.0.1",
				"name": "", "body": "", "target_commitish": "",
				"draft": false, "prerelease": false, "generate_release_notes": false,
			},
			script: created(`{"id":2,"tag_name":"v1.0.1"}`),
		},
		{
			name: "list_tags/paging only",
			tool: "list_tags",
			args: map[string]any{
				"owner": "octocat", "repo": "hello-world", "per_page": 0, "page": 0,
			},
			script: ok(okBody),
		},
	}
}

// ─── The generator ───────────────────────────────────────────────────────────

func TestGitHubVectors(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	fake := &fakeGitHub{}
	srv := httptest.NewServer(fake)
	defer srv.Close()
	restore := github.SetAPIBase(srv.URL)
	defer restore()

	want := githubVectors{
		Comment: []string{
			"Cross-language parity vectors for internal/integrations/github, the GitHub",
			"integration's in-process MCP server. Generated from Go, then frozen. Read by",
			"desktop/parity/github_parity_test.go (Go) and by",
			"desktop/src-tauri/src/native/integrations/github/ (Rust). Every value is exactly",
			"what Go produces, so a divergence fails one language against the other's real",
			"output rather than against a belief about it.",
			"'tools' and 'hosting' come from live tools/list calls against servers built by",
			"github.Start; 'calls' come from live tools/call calls against a scripted fake",
			"GitHub that records the request each tool built.",
			"'token' is a fixture, not a secret: it is recorded so the Bearer prefix is pinned.",
			"Regenerate with: go test ./desktop/parity/ -run TestGitHubVectors -update-github-vectors",
		},
		IntegrationID: githubParityID,
		ServerName:    fmt.Sprintf("github-%s", githubParityID),
		Version:       "1.0.0",
		Token:         githubParityToken,
	}

	session := githubSession(t, ctx, allServices())
	listed, err := session.ListTools(ctx, nil)
	if err != nil {
		t.Fatalf("tools/list: %v", err)
	}
	for _, tool := range listed.Tools {
		schema, err := json.Marshal(tool.InputSchema)
		if err != nil {
			t.Fatalf("encoding %s's input schema: %v", tool.Name, err)
		}
		want.Tools = append(want.Tools, githubToolVector{
			Name:        tool.Name,
			Description: tool.Description,
			InputSchema: schema,
		})
	}

	for _, hosting := range hostingCases {
		hosting.Tools = hostedTools(t, ctx, hosting.Services)
		want.Hosting = append(want.Hosting, hosting)
	}

	for _, c := range githubCallCases() {
		want.Calls = append(want.Calls, runCallCase(t, ctx, session, fake, c))
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateGitHubVectors {
		if err := os.WriteFile(githubVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", githubVectorsFile, err)
		}
		t.Logf("wrote %s", githubVectorsFile)
		return
	}

	frozen, err := os.ReadFile(githubVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-github-vectors): %v",
			githubVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this Go toolchain produces different results.\n"+
			"Regenerate with -update-github-vectors and check what moved — the Rust "+
			"port in native/integrations/github/ reads the same file and will fail "+
			"against it. A moved tool name, schema, request or sentence is not a "+
			"cosmetic diff: the names are in every agent's stored allowlist and in "+
			"every tool_use block already written, and the sentences are what the "+
			"model reads.",
			githubVectorsFile)
	}
}

// hostedTools starts a server for services and asks it what it hosts.
//
// Sorted, because `tools/list` order is the registration order and the point
// here is the *set*; the twenty-tool `tools` block above keeps the order.
func hostedTools(
	t *testing.T, ctx context.Context, services map[string]config.ServiceConfig,
) []string {
	t.Helper()
	listed, err := githubSession(t, ctx, services).ListTools(ctx, nil)
	if err != nil {
		t.Fatalf("tools/list: %v", err)
	}
	names := make([]string, 0, len(listed.Tools))
	for _, tool := range listed.Tools {
		names = append(names, tool.Name)
	}
	sort.Strings(names)
	return names
}

// runCallCase arms the fake, makes one real tools/call and records everything
// observable: the request the tool built, and the result the model would read.
func runCallCase(
	t *testing.T, ctx context.Context,
	session *mcp.ClientSession, fake *fakeGitHub, c callCase,
) githubCallVector {
	t.Helper()

	fake.arm(c.script)
	args := jsonArgs(t, c.args)
	result, err := session.CallTool(ctx, &mcp.CallToolParams{
		Name: c.tool, Arguments: args,
	})
	if err != nil {
		t.Fatalf("%s: tools/call: %v", c.name, err)
	}
	if len(result.Content) != 1 {
		t.Fatalf("%s: want exactly one content block, got %d", c.name, len(result.Content))
	}
	text, ok := result.Content[0].(*mcp.TextContent)
	if !ok {
		t.Fatalf("%s: want text content, got %T", c.name, result.Content[0])
	}

	// The credential must never reach the model, on either path.
	if strings.Contains(text.Text, githubParityToken) {
		t.Fatalf("%s: the token leaked into a tool result: %s", c.name, text.Text)
	}

	return githubCallVector{
		Case:       c.name,
		Tool:       c.tool,
		Arguments:  args,
		Response:   c.script,
		Request:    fake.recorded(),
		IsError:    result.IsError,
		Text:       text.Text,
		RustText:   c.rustText,
		RustTarget: c.rustTarget,
	}
}
