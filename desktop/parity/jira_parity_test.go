// Cross-language vectors for the Jira integration MCP server
// (`internal/integrations/jira`, ported in #316 to
// `desktop/src-tauri/src/native/integrations/jira/`).
//
// Four things have to match byte for byte, none checkable from one language
// alone: which tools are hosted, each advertised schema, the request each tool
// builds, and the result text of every success and every failure. The reasoning
// for each is in `confluence_parity_test.go`'s header and is not repeated.
//
// **This file needs no seam into the jira package, and that absence is the
// point.** `confluence.Start` validates the site URL (HTTPS only) before it
// builds anything, so `desktop/parity` had to be handed
// `confluence.StartAtSiteURL` to stand a server up against a plaintext loopback
// fake. `jira.Start` does not look at the site URL at all — it reads
// `creds.SiteURL` and passes it to `buildMCPServer` — so putting the
// `httptest.Server`'s URL in the credentials *is* the shipped path. #277 pinned
// that the two validators differ deliberately; this is that difference showing up
// as one fewer exported function.
//
// Jira-specific things pinned here that a port would otherwise get wrong:
//
//   - `list_projects` binds `*struct{}`, so its schema is an empty object. No
//     other tool in the six integrations has no fields.
//   - `create_issue` runs `url.PathEscape` over the project key **inside the JSON
//     body**, and over nothing else.
//   - `update_issue` and `transition_issue` build their result text from the
//     *arguments* and discard the response.
//   - `/rest/api/3/issue/` carries its trailing slash in the constant while
//     `/rest/api/3/project` does not.
//
// The Rust half lives in
// `desktop/src-tauri/src/native/integrations/jira/tests_vectors.rs` and reads this
// same file.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestJiraVectors -update-jira-vectors
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
	"github.com/shaharia-lab/agento/internal/integrations/jira"
)

const jiraVectorsFile = "jira_vectors.json"

var updateJiraVectors = flag.Bool("update-jira-vectors", false,
	"rewrite jira_vectors.json from this Go toolchain")

// Fixtures, not secrets. Recorded so the base64 of the Basic header is pinned:
// that a port used the same separator, the same order and standard (not URL-safe)
// base64 is exactly what no response would reveal.
const (
	jiraParityEmail = "parity@example.com"
	jiraParityToken = "parity-jira-api-token" //nolint:gosec // a test fixture, not a credential
)

// jiraParityID is the integration id, and half of the server name (`jira-<id>`).
const jiraParityID = "jira-parity"

type jiraToolVector struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
}

// jiraRequestVector is what the fake Jira saw. `Target` is `URL.RequestURI()`.
type jiraRequestVector struct {
	Method        string `json:"method"`
	Target        string `json:"target"`
	Authorization string `json:"authorization"`
	Accept        string `json:"accept"`
	// Set only when the tool sent a body; `call` adds the header only then.
	ContentType string `json:"content_type"`
	Body        string `json:"body"`
}

type jiraResponseScript struct {
	Status int    `json:"status"`
	Body   string `json:"body"`
}

type jiraCallVector struct {
	Case string `json:"case"`
	Tool string `json:"tool"`
	// The path appended to the fake's URL to make this case's site URL, empty
	// for the fake's root. A site URL is per row, so the base is part of every
	// request a tool builds — and the only part a person typed.
	BasePath  string             `json:"base_path,omitempty"`
	Arguments json.RawMessage    `json:"arguments"`
	Response  jiraResponseScript `json:"response"`
	Request   *jiraRequestVector `json:"request"`
	IsError   bool               `json:"is_error"`
	Text      string             `json:"text"`
	// A pinned divergence rather than a match. Two users, both inherited from
	// #312 and #317: the dot-segment refusal, and a zero-fraction float for an
	// integer field, which this SDK re-marshals into an integer and `serde_json`
	// refuses.
	RustText string `json:"rust_text,omitempty"`
	// Go made a request and this port deliberately makes none — the same two
	// causes.
	RustNoRequest bool `json:"rust_no_request,omitempty"`
}

type jiraHostingVector struct {
	Case     string                          `json:"case"`
	Services map[string]config.ServiceConfig `json:"services"`
	Tools    []string                        `json:"tools"`
}

// jiraSiteURLVector pins that a site URL Go serves and this build cannot send a
// request through leaves the **tool set** untouched, which is the property that
// made Jira answer per call rather than refuse to host.
type jiraSiteURLVector struct {
	Case string `json:"case"`
	// Substituted for the fake's URL wholesale, so this is an absolute URL and
	// no request can reach the fake.
	SiteURL string   `json:"site_url"`
	Tools   []string `json:"tools"`
	// What a tools/call answers on the Go side.
	CallText string `json:"call_text"`
	IsError  bool   `json:"is_error"`
	// Set where the Rust port answers a different sentence, and there is one
	// cause: for a site URL `url.Parse` itself rejects, Go fails inside
	// `http.NewRequestWithContext` and answers `creating request: parse "…": …`
	// — `net/url`'s own vocabulary, `%q`-quoted over the stored site URL. The
	// port refuses before it builds anything and answers the transport sentence
	// instead. Neither reaches the network, and the port's is the *narrower* of
	// the two: Go's interpolates the site URL into text the model reads and a
	// `tool_result` stores.
	//
	// The other two site URLs need no entry: Go gets as far as a DNS lookup that
	// fails, so both languages answer `calling Jira GET …: request failed` for
	// the same call by different routes.
	RustCallText string `json:"rust_call_text,omitempty"`
}

type jiraVectors struct {
	Comment       []string            `json:"_comment"`
	IntegrationID string              `json:"integration_id"`
	ServerName    string              `json:"server_name"`
	Version       string              `json:"version"`
	Email         string              `json:"email"`
	APIToken      string              `json:"api_token"`
	Tools         []jiraToolVector    `json:"tools"`
	Hosting       []jiraHostingVector `json:"hosting"`
	SiteURLs      []jiraSiteURLVector `json:"site_urls"`
	Calls         []jiraCallVector    `json:"calls"`
}

// ─── The fake Jira ───────────────────────────────────────────────────────────

// fakeJira plays a scripted Jira and records what it was asked. One handler for
// every path, because the point is to capture the request the tool *built*.
type fakeJira struct {
	mu     sync.Mutex
	script jiraResponseScript
	seen   *jiraRequestVector
}

func (f *fakeJira) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		body = nil
	}
	f.mu.Lock()
	script := f.script
	f.seen = &jiraRequestVector{
		Method:        r.Method,
		Target:        r.URL.RequestURI(),
		Authorization: r.Header.Get("Authorization"),
		Accept:        r.Header.Get("Accept"),
		ContentType:   r.Header.Get("Content-Type"),
		Body:          string(body),
	}
	f.mu.Unlock()

	w.WriteHeader(script.Status)
	_, _ = w.Write([]byte(script.Body))
}

func (f *fakeJira) arm(script jiraResponseScript) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.script = script
	f.seen = nil
}

func (f *fakeJira) recorded() *jiraRequestVector {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.seen
}

// ─── Standing the real server up ─────────────────────────────────────────────

// jiraSession starts the real integration against siteURL and returns a client
// session. `jira.Start` is the app's own entry point — no seam, because it does
// not validate the site URL.
func jiraSession(
	t *testing.T, ctx context.Context, siteURL string, services map[string]config.ServiceConfig,
) *mcp.ClientSession {
	t.Helper()

	credentials, err := json.Marshal(config.AtlassianCredentials{
		SiteURL:  siteURL,
		Email:    jiraParityEmail,
		APIToken: jiraParityToken,
	})
	if err != nil {
		t.Fatalf("encoding credentials: %v", err)
	}

	cfg := &config.IntegrationConfig{
		ID:          jiraParityID,
		Name:        "Jira (parity)",
		Type:        "jira",
		Enabled:     true,
		Credentials: credentials,
		// `IsAuthenticated` is "present and not the four bytes null"; a
		// token-validated Jira integration stores the display name here.
		Auth:     json.RawMessage(`{"display_name":"Parity"}`),
		Services: services,
	}

	serverCfg, err := jira.Start(ctx, cfg)
	if err != nil {
		t.Fatalf("starting the jira integration: %v", err)
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

// jiraAllServices enables the one service with an empty tool list — the "empty
// allowed set registers everything" rule, and the configuration the nine schemas
// and every call vector are taken under.
func jiraAllServices() map[string]config.ServiceConfig {
	return map[string]config.ServiceConfig{"project_management": {Enabled: true}}
}

var jiraHostingCases = []jiraHostingVector{
	{Case: "the service enabled with no tool list — an empty allowed set hosts everything",
		Services: jiraAllServices()},
	{Case: "no services at all", Services: map[string]config.ServiceConfig{}},
	{Case: "the service disabled contributes neither its gate nor its tools",
		Services: map[string]config.ServiceConfig{
			"project_management": {Enabled: false, Tools: []string{"get_issue"}},
		}},
	{Case: "a non-empty allowed set is a filter",
		Services: map[string]config.ServiceConfig{
			"project_management": {Enabled: true, Tools: []string{"get_issue", "add_comment"}},
		}},
	// The union half: an enabled service Jira does not know still contributes
	// its names, so it narrows the one that exists.
	{Case: "an enabled but unknown service still narrows the one that exists",
		Services: map[string]config.ServiceConfig{
			"project_management": {Enabled: true},
			"other":              {Enabled: true, Tools: []string{"list_projects"}},
		}},
	{Case: "a tool named by a disabled service is not allowed anywhere",
		Services: map[string]config.ServiceConfig{
			"project_management": {Enabled: true, Tools: []string{"get_project"}},
			"other":              {Enabled: false, Tools: []string{"create_issue"}},
		}},
}

// jiraSiteURLCases are the site URLs `jira.Start` accepts without a glance and
// the Rust port cannot send a request through.
//
// The assertion is the **tool set**, not the refusal: Go hosts all nine whatever
// the base says, and the advertised set is what every agent's stored
// `capabilities.mcp` allowlist depends on — so a port that refused to host would
// break agents that exist. The Rust half asserts the same nine tools plus its own
// per-call refusal.
//
// Each host is one that does not resolve, so Go's own answer is its transport
// sentence and nothing leaves the machine.
var jiraSiteURLCases = []struct {
	name         string
	siteURL      string
	rustCallText string
}{
	{
		// `url` treats `\` as an authority separator and `net/url` rejects the
		// userinfo that leaves, so the two read a different host. Go's
		// `http.NewRequestWithContext` fails on it outright.
		name:         "a backslash before the userinfo, which url reads as a different host",
		siteURL:      `https://evil.invalid\@jira.invalid`,
		rustCallText: "calling Jira GET /rest/api/3/project: request failed",
	},
	{
		// `parseHost` rejects a `%` escape that decodes to a byte it would have
		// escaped; `url` decodes them all.
		name:         "a percent escape in the host, which url decodes to another domain",
		siteURL:      "https://jira.invalid%2Eevil.invalid",
		rustCallText: "calling Jira GET /rest/api/3/project: request failed",
	},
	{
		// Go sends the dot segments verbatim; `url` collapses them.
		name:    "a dot segment in the base path, which url collapses",
		siteURL: "https://jira.invalid/a/../b",
	},
	{
		// Plaintext, which Confluence refuses at Start and Jira does not — the
		// create-time validator accepts `http` for Jira. Nothing about this one
		// diverges; it is here because it is the difference #277 pinned.
		name:    "plaintext, which Jira's own validator allows and Confluence's does not",
		siteURL: "http://jira.invalid",
	},
}

// ─── The call cases ──────────────────────────────────────────────────────────

// jiraOKBody is hostile to a re-encode: keys out of alphabetical order, a
// trailing-zero decimal, an integer too large for a float64 to hold exactly, and
// interior whitespace. Go passes the bytes through verbatim.
const jiraOKBody = `{"issues":[{"zebra":1,"id":10152021304050607,"rate":1.50, "key":"PROJ-1"}]}`

type jiraCallCase struct {
	name          string
	tool          string
	basePath      string
	args          map[string]any
	script        jiraResponseScript
	rustText      string
	rustNoRequest bool
}

func jiraOK(body string) jiraResponseScript {
	return jiraResponseScript{Status: http.StatusOK, Body: body}
}

// jiraCallCases is the exercise: every tool at least once on its success path,
// plus every distinct failure the client can produce.
//
// The argument maps are complete rather than minimal because **every field is
// required** — `jsonschema-go` marks a field optional only on
// `omitempty`/`omitzero`, and no params struct here carries either.
func jiraCallCases() []jiraCallCase {
	return []jiraCallCase{
		// ─── list_projects, the only tool with no arguments ───────────────────
		{
			name:   "list_projects/no arguments at all, because the Go handler binds *struct{}",
			tool:   "list_projects",
			args:   map[string]any{},
			script: jiraOK(jiraOKBody),
		},
		{
			name:   "list_projects/a non-2xx status is Jira's own body, verbatim",
			tool:   "list_projects",
			args:   map[string]any{},
			script: jiraResponseScript{Status: 403, Body: `{"message":"Forbidden <you>"}`},
		},
		{
			// The gate is the 2xx *range*, not `== 200`.
			name:   "list_projects/a 204 is a success, and its empty body is the result",
			tool:   "list_projects",
			args:   map[string]any{},
			script: jiraResponseScript{Status: http.StatusNoContent, Body: ""},
		},

		// ─── get_project ─────────────────────────────────────────────────────
		{
			name:   "get_project/a plain key, with the slash the constant does not carry",
			tool:   "get_project",
			args:   map[string]any{"key": "PROJ"},
			script: jiraOK(`{"key":"PROJ"}`),
		},
		{
			// `PathEscape` is `encodePathSegment`: it escapes `/ ; , ?` and
			// leaves `$ & + : = @` alone, and multi-byte UTF-8 becomes one `%XX`
			// per byte in uppercase hex.
			name:   "get_project/the reserved bytes a segment keeps, the ones it escapes, and UTF-8",
			tool:   "get_project",
			args:   map[string]any{"key": "a$&+:=@b my key/x;y,z?q café日本語"},
			script: jiraOK(`{"key":"X"}`),
		},

		// ─── search_issues ───────────────────────────────────────────────────
		{
			// JQL travels in the **body**, so nothing escapes it — unlike
			// Confluence's CQL, which is a query parameter.
			name: "search_issues/JQL goes in the body unescaped, and the clamp falls back to 50",
			tool: "search_issues",
			args: map[string]any{
				"jql": `project = PROJ AND status = "In Progress" & x`, "max_results": 0,
			},
			script: jiraOK(jiraOKBody),
		},
		{
			name:   "search_issues/over 100 falls back to 50 rather than clamping to 100",
			tool:   "search_issues",
			args:   map[string]any{"jql": "type=Bug", "max_results": 101},
			script: jiraOK(jiraOKBody),
		},
		{
			name:   "search_issues/100 is the largest value that survives",
			tool:   "search_issues",
			args:   map[string]any{"jql": "type=Bug", "max_results": 100},
			script: jiraOK(jiraOKBody),
		},
		{
			name:   "search_issues/a negative max_results is a zero, not an error",
			tool:   "search_issues",
			args:   map[string]any{"jql": "", "max_results": -5},
			script: jiraOK(jiraOKBody),
		},

		// ─── get_issue ───────────────────────────────────────────────────────
		{
			name:   "get_issue/the constant already ends in a slash",
			tool:   "get_issue",
			args:   map[string]any{"key": "PROJ-123"},
			script: jiraOK(`{"key":"PROJ-123"}`),
		},

		// ─── create_issue ────────────────────────────────────────────────────
		{
			// The project key is `PathEscape`d **inside the body**, and nothing
			// else is. A space becomes `%20` in JSON.
			name: "create_issue/the project key is path-escaped inside the body and nothing else is",
			tool: "create_issue",
			args: map[string]any{
				"project_key": "MY PROJ/X", "issue_type": "Bug",
				"summary": "It <broke> & burned", "description": "", "priority": "",
			},
			script: jiraResponseScript{Status: http.StatusCreated, Body: `{"key":"PROJ-9"}`},
		},
		{
			name: "create_issue/a description becomes an ADF document, sorted at every level",
			tool: "create_issue",
			args: map[string]any{
				"project_key": "PROJ", "issue_type": "Story",
				"summary": "s", "description": "a <b> & c", "priority": "High",
			},
			script: jiraResponseScript{Status: http.StatusCreated, Body: `{"key":"PROJ-10"}`},
		},
		{
			name: "create_issue/an empty description and priority leave no keys at all",
			tool: "create_issue",
			args: map[string]any{
				"project_key": "", "issue_type": "", "summary": "",
				"description": "", "priority": "",
			},
			script: jiraResponseScript{Status: 400, Body: `{"errors":{"project":"required"}}`},
		},

		// ─── update_issue ────────────────────────────────────────────────────
		{
			// Every field is conditional, so this sends `{"fields":{}}` — still a
			// body, and therefore still a Content-Type.
			name: "update_issue/an all-empty update still sends a body, and the text ignores the response",
			tool: "update_issue",
			args: map[string]any{
				"key": "PROJ-1", "summary": "", "description": "", "priority": "",
			},
			script: jiraOK(`{"ignored":true}`),
		},
		{
			name: "update_issue/every field set, description as ADF",
			tool: "update_issue",
			args: map[string]any{
				"key": "PROJ-1", "summary": "new", "description": "d", "priority": "Low",
			},
			script: jiraResponseScript{Status: http.StatusNoContent, Body: ""},
		},
		{
			name: "update_issue/a key needing escaping appears escaped in the path and raw in the text",
			tool: "update_issue",
			args: map[string]any{
				"key": "a/b c", "summary": "s", "description": "", "priority": "",
			},
			script: jiraOK(`{}`),
		},

		// ─── add_comment ─────────────────────────────────────────────────────
		{
			name:   "add_comment/the comment becomes an ADF document",
			tool:   "add_comment",
			args:   map[string]any{"key": "PROJ-1", "comment": "looks good & <shipped>"},
			script: jiraResponseScript{Status: http.StatusCreated, Body: `{"id":"10000"}`},
		},
		{
			name:   "add_comment/an empty comment is still a document",
			tool:   "add_comment",
			args:   map[string]any{"key": "PROJ-1", "comment": ""},
			script: jiraResponseScript{Status: http.StatusCreated, Body: `{"id":"10001"}`},
		},

		// ─── list_transitions / transition_issue ─────────────────────────────
		{
			name:   "list_transitions/a GET under the issue path",
			tool:   "list_transitions",
			args:   map[string]any{"key": "PROJ-1"},
			script: jiraOK(`{"transitions":[{"id":"31"}]}`),
		},
		{
			name:   "transition_issue/the text ignores the response, like update_issue's",
			tool:   "transition_issue",
			args:   map[string]any{"key": "PROJ-1", "transition_id": "31"},
			script: jiraResponseScript{Status: http.StatusNoContent, Body: ""},
		},
		{
			name:   "transition_issue/a failure is the API's body, not the cheerful sentence",
			tool:   "transition_issue",
			args:   map[string]any{"key": "PROJ-1", "transition_id": "nope"},
			script: jiraResponseScript{Status: 400, Body: `{"errorMessages":["bad transition"]}`},
		},

		// ─── a site URL with a base path of its own ───────────────────────────
		{
			name:     "get_issue/a site URL with a base path prefixes the target",
			tool:     "get_issue",
			basePath: "/jira",
			args:     map[string]any{"key": "PROJ-1"},
			script:   jiraOK(`{"key":"PROJ-1"}`),
		},
		{
			// Not percent-encoded, and both parsers encode it the same way.
			name:     "add_comment/an unencoded base path is escaped by both sides, on a write",
			tool:     "add_comment",
			basePath: "/my jira",
			args:     map[string]any{"key": "PROJ-1", "comment": "hi"},
			script:   jiraResponseScript{Status: http.StatusCreated, Body: `{"id":"1"}`},
		},

		// ─── the dot-segment refusal ─────────────────────────────────────────
		{
			// Go sends `/rest/api/3/issue/..` verbatim; `url` would resolve it to
			// `/rest/api/3/`, a different endpoint answering a request that
			// carries the API token.
			name:          "get_issue/a dot-dot segment reaches a different endpoint under url",
			tool:          "get_issue",
			args:          map[string]any{"key": ".."},
			script:        jiraResponseScript{Status: 404, Body: `{"message":"Not Found"}`},
			rustText:      "calling Jira GET /rest/api/3/issue/..: request failed",
			rustNoRequest: true,
		},
		{
			name:          "get_project/a single dot segment is sent literally too",
			tool:          "get_project",
			args:          map[string]any{"key": "."},
			script:        jiraResponseScript{Status: 404, Body: `{"message":"Not Found"}`},
			rustText:      "calling Jira GET /rest/api/3/project/.: request failed",
			rustNoRequest: true,
		},
		{
			// The write path: a PUT whose body is built and sent before the
			// status is looked at.
			name: "update_issue/dot segments reach the write path too",
			tool: "update_issue",
			args: map[string]any{
				"key": "..", "summary": "s", "description": "", "priority": "",
			},
			script:        jiraResponseScript{Status: 404, Body: `{"message":"Not Found"}`},
			rustText:      "calling Jira PUT /rest/api/3/issue/..: request failed",
			rustNoRequest: true,
		},
		{
			name:          "list_transitions/a dot segment before a suffix is refused as well",
			tool:          "list_transitions",
			args:          map[string]any{"key": ".."},
			script:        jiraResponseScript{Status: 404, Body: `{"message":"Not Found"}`},
			rustText:      "calling Jira GET /rest/api/3/issue/../transitions: request failed",
			rustNoRequest: true,
		},

		// ─── a zero-fraction float, which models do emit ──────────────────────
		{
			// The Go SDK unmarshals `arguments` into a `map[string]any`,
			// validates against the reflected schema — where JSON Schema counts
			// `50.0` as an `integer` — and re-marshals before the typed decode,
			// so the handler sees 50. `serde_json::from_value::<i64>` refuses.
			name:          "search_issues/a zero-fraction float is an integer to Go and not to serde",
			tool:          "search_issues",
			args:          map[string]any{"jql": "type=Bug", "max_results": json.RawMessage("50.0")},
			script:        jiraOK(jiraOKBody),
			rustText:      "failed to deserialize parameters: invalid type: floating point `50.0`, expected i64",
			rustNoRequest: true,
		},
	}
}

// ─── The generator ───────────────────────────────────────────────────────────

func TestJiraVectors(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	fake := &fakeJira{}
	srv := httptest.NewServer(fake)
	defer srv.Close()

	want := jiraVectors{
		Comment: []string{
			"Cross-language parity vectors for internal/integrations/jira, the Jira",
			"integration's in-process MCP server. Generated from Go, then frozen. Read by",
			"desktop/parity/jira_parity_test.go (Go) and by",
			"desktop/src-tauri/src/native/integrations/jira/ (Rust). Every value is exactly",
			"what Go produces, so a divergence fails one language against the other's real",
			"output rather than against a belief about it.",
			"'tools' and 'hosting' come from live tools/list calls against servers built by",
			"jira.Start; 'calls' from live tools/call calls against a scripted fake Jira that",
			"records the request each tool built; 'site_urls' pin that a site URL Go serves",
			"and the Rust port cannot send a request through leaves the tool set untouched.",
			"'email' and 'api_token' are fixtures, not secrets: they are recorded so the",
			"base64 of the Basic header is pinned.",
			"Regenerate with: go test ./desktop/parity/ -run TestJiraVectors -update-jira-vectors",
		},
		IntegrationID: jiraParityID,
		ServerName:    fmt.Sprintf("jira-%s", jiraParityID),
		Version:       "1.0.0",
		Email:         jiraParityEmail,
		APIToken:      jiraParityToken,
	}

	session := jiraSession(t, ctx, srv.URL, jiraAllServices())
	listed, err := session.ListTools(ctx, nil)
	if err != nil {
		t.Fatalf("tools/list: %v", err)
	}
	for _, tool := range listed.Tools {
		schema, err := json.Marshal(tool.InputSchema)
		if err != nil {
			t.Fatalf("encoding %s's input schema: %v", tool.Name, err)
		}
		want.Tools = append(want.Tools, jiraToolVector{
			Name:        tool.Name,
			Description: tool.Description,
			InputSchema: schema,
		})
	}

	for _, hosting := range jiraHostingCases {
		hosting.Tools = jiraHostedTools(t, ctx, srv.URL, hosting.Services)
		want.Hosting = append(want.Hosting, hosting)
	}

	for _, c := range jiraSiteURLCases {
		vector := runJiraSiteURLCase(t, ctx, c.name, c.siteURL)
		vector.RustCallText = c.rustCallText
		want.SiteURLs = append(want.SiteURLs, vector)
	}

	// One session per distinct site URL. Most cases run against the fake's own
	// root; the base-path ones need a server whose credentials carry that base,
	// because a site URL is not a per-call argument.
	sessions := map[string]*mcp.ClientSession{"": session}
	for _, c := range jiraCallCases() {
		if _, ok := sessions[c.basePath]; !ok {
			sessions[c.basePath] = jiraSession(t, ctx, srv.URL+c.basePath, jiraAllServices())
		}
		want.Calls = append(want.Calls, runJiraCallCase(t, ctx, sessions[c.basePath], fake, c))
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateJiraVectors {
		if err := os.WriteFile(jiraVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", jiraVectorsFile, err)
		}
		t.Logf("wrote %s", jiraVectorsFile)
		return
	}

	frozen, err := os.ReadFile(jiraVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-jira-vectors): %v", jiraVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this Go toolchain produces different results.\n"+
			"Regenerate with -update-jira-vectors and check what moved — the Rust port in "+
			"native/integrations/jira/ reads the same file and will fail against it. A moved "+
			"tool name, schema, request or sentence is not a cosmetic diff: the names are in "+
			"every agent's stored allowlist and in every tool_use block already written, and "+
			"the sentences are what the model reads.",
			jiraVectorsFile)
	}
}

// jiraHostedTools starts a server for services and asks it what it hosts.
//
// The sort makes explicit what both SDKs already do: `tools/list` answers in name
// order. Registration order is pinned on the Rust side instead.
func jiraHostedTools(
	t *testing.T, ctx context.Context, siteURL string, services map[string]config.ServiceConfig,
) []string {
	t.Helper()
	listed, err := jiraSession(t, ctx, siteURL, services).ListTools(ctx, nil)
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

// runJiraSiteURLCase hosts a server on a site URL that never resolves and records
// what it advertises and what one call answers.
func runJiraSiteURLCase(
	t *testing.T, ctx context.Context, name, siteURL string,
) jiraSiteURLVector {
	t.Helper()
	session := jiraSession(t, ctx, siteURL, jiraAllServices())

	listed, err := session.ListTools(ctx, nil)
	if err != nil {
		t.Fatalf("%s: tools/list: %v", name, err)
	}
	names := make([]string, 0, len(listed.Tools))
	for _, tool := range listed.Tools {
		names = append(names, tool.Name)
	}
	sort.Strings(names)

	result, err := session.CallTool(ctx, &mcp.CallToolParams{
		Name: "list_projects", Arguments: json.RawMessage(`{}`),
	})
	if err != nil {
		t.Fatalf("%s: tools/call: %v", name, err)
	}
	text, ok := result.Content[0].(*mcp.TextContent)
	if !ok {
		t.Fatalf("%s: want text content, got %T", name, result.Content[0])
	}
	for _, secret := range []string{jiraParityToken, jiraParityEmail} {
		if strings.Contains(text.Text, secret) {
			t.Fatalf("%s: a credential leaked into a tool result: %s", name, text.Text)
		}
	}

	return jiraSiteURLVector{
		Case: name, SiteURL: siteURL, Tools: names,
		CallText: text.Text, IsError: result.IsError,
	}
}

// runJiraCallCase arms the fake, makes one real tools/call and records everything
// observable: the request the tool built, and the result the model would read.
func runJiraCallCase(
	t *testing.T, ctx context.Context,
	session *mcp.ClientSession, fake *fakeJira, c jiraCallCase,
) jiraCallVector {
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

	for _, secret := range []string{jiraParityToken, jiraParityEmail} {
		if strings.Contains(text.Text, secret) {
			t.Fatalf("%s: a credential leaked into a tool result: %s", c.name, text.Text)
		}
	}

	return jiraCallVector{
		Case:          c.name,
		Tool:          c.tool,
		BasePath:      c.basePath,
		Arguments:     args,
		Response:      c.script,
		Request:       fake.recorded(),
		IsError:       result.IsError,
		Text:          text.Text,
		RustText:      c.rustText,
		RustNoRequest: c.rustNoRequest,
	}
}
