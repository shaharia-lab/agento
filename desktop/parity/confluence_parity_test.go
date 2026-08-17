// Cross-language vectors for the Confluence integration MCP server
// (`internal/integrations/confluence`, ported in #317 to
// `desktop/src-tauri/src/native/integrations/confluence/`).
//
// Five things about that server have to match byte for byte, and none of them is
// checkable from one language alone:
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
//     where the six real schemas are pinned.
//   - **The request each tool builds.** `url.PathEscape` per segment,
//     `url.QueryEscape` per query value (a space is `+`, not `%20`), the two
//     `limit` clamps and their *different* fallbacks, and `json.Marshal`'s
//     sorted map keys and HTML escaping in both request bodies — which matters
//     more here than anywhere in #312, because a Confluence page body is XHTML
//     and so escapes on every single call. None of that is visible in a
//     response, so the fake Confluence below records the request it received.
//   - **The result text.** A tool's result is what the model reads and what gets
//     persisted, and on the error path it is the *message* — `new_tool` and
//     `mcp.AddTool` both pack a failed call into `CallToolResult` with `IsError`
//     rather than raising a protocol error. The raw Confluence bytes are passed
//     through verbatim, so a port that round-tripped them through a JSON value
//     would reorder keys and respell numbers.
//   - **`ValidateSiteURL`.** The one piece of `Start` that is a decision rather
//     than plumbing: it is what refuses a plaintext site URL before the API
//     token is ever put in a `Basic` header. #277 pinned that Confluence's rule
//     is deliberately *not* Jira's, so it is pinned here per input rather than
//     described.
//
// Everything here is taken from the **running server** over its real HTTP MCP
// transport — `confluence.StartAtSiteURL` is `confluence.Start` with only the
// HTTPS check removed, so the authentication check, the credential parse, the
// server name and the tool registration are all the shipped ones.
//
// The Rust half lives in
// `desktop/src-tauri/src/native/integrations/confluence/tests_vectors.rs` and
// reads this same file.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestConfluenceVectors -update-confluence-vectors
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
	"github.com/shaharia-lab/agento/internal/integrations/confluence"
)

const confluenceVectorsFile = "confluence_vectors.json"

var updateConfluenceVectors = flag.Bool("update-confluence-vectors", false,
	"rewrite confluence_vectors.json from this Go toolchain")

// The credentials every recorded request carries. Fixtures, not secrets — and
// recording the whole `Authorization` header is deliberate: `SetBasicAuth` is
// base64 of `email + ":" + apiToken`, and that a port used the same separator,
// the same order and standard (not URL-safe) base64 is exactly the kind of thing
// no response would reveal.
const (
	confluenceParityEmail = "parity@example.com"
	// A fixture, and gosec's pattern match is right about the shape — which is
	// the point. It authenticates nothing.
	confluenceParityToken = "parity-confluence-api-token" //nolint:gosec // a test fixture, not a credential
)

// confluenceParityID is the integration id, and half of the server name
// (`confluence-<id>`).
const confluenceParityID = "cf-parity"

type confluenceToolVector struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
}

// confluenceRequestVector is what the fake Confluence saw. `Target` is
// `URL.RequestURI()` — the encoded path plus the encoded query — which is the
// one string that pins `url.PathEscape` and `url.QueryEscape` together.
type confluenceRequestVector struct {
	Method        string `json:"method"`
	Target        string `json:"target"`
	Authorization string `json:"authorization"`
	Accept        string `json:"accept"`
	// Set only when the tool sent a body; `callConfluence` adds the header only
	// then.
	ContentType string `json:"content_type"`
	Body        string `json:"body"`
}

// confluenceResponseScript is what the fake answered with. The Rust half replays
// exactly this, so the two languages see the same bytes.
type confluenceResponseScript struct {
	Status int    `json:"status"`
	Body   string `json:"body"`
}

type confluenceCallVector struct {
	Case string `json:"case"`
	Tool string `json:"tool"`
	// The path appended to the fake's own URL to make this case's site URL,
	// empty for the fake's root. A site URL is per row, so the base is part of
	// every request a tool builds — and the only part that is not already
	// percent-encoded, since a person typed it.
	BasePath  string                   `json:"base_path,omitempty"`
	Arguments json.RawMessage          `json:"arguments"`
	Response  confluenceResponseScript `json:"response"`
	// nil when the tool answered without making a request. No Confluence tool
	// does — both bodies are built from strings that cannot fail to marshal —
	// so this is nil only where the port itself declines, see RustNoRequest.
	Request *confluenceRequestVector `json:"request"`
	IsError bool                     `json:"is_error"`
	Text    string                   `json:"text"`
	// Set only where Rust cannot reproduce Go's text and the difference is
	// pinned rather than hidden. One user: the dot-segment refusal below.
	RustText string `json:"rust_text,omitempty"`
	// Set where Go made a request and Rust deliberately makes none: a path
	// holding a `.` or `..` segment. Go's `net/http` sends the path verbatim,
	// while `url::Url::parse` — which `reqwest` builds every request through —
	// applies WHATWG dot-segment removal, so `/wiki/api/v2/pages/..` would leave
	// as `/wiki/api/v2/`: the space listing rather than one page, on a request
	// already carrying the API token. Escaping does not help, `%2E%2E` is
	// collapsed too, and `reqwest` offers no unnormalized target, so the port
	// refuses the call rather than reaching a different endpoint than Go would.
	RustNoRequest bool `json:"rust_no_request,omitempty"`
}

type confluenceHostingVector struct {
	Case     string                          `json:"case"`
	Services map[string]config.ServiceConfig `json:"services"`
	Tools    []string                        `json:"tools"`
}

// confluenceSiteURLVector pins ValidateSiteURL per input: the cleaned value on
// success, the message on refusal.
type confluenceSiteURLVector struct {
	Case  string `json:"case"`
	Input string `json:"input"`
	Clean string `json:"clean,omitempty"`
	Error string `json:"error,omitempty"`
	// Set where the port refuses the same input under different wording. One
	// cause: `url.Parse` fails outright — a control character, a non-numeric
	// port, a bad `%` escape in the host — and answers with `net/url`'s own
	// vocabulary, `%q`-quoted over the caller's input. That is not reproducible
	// and it is a **log line rather than an interface**: `Start`'s error is
	// logged by the registry and never reaches a response or the model. The
	// classification is what is pinned.
	RustError string `json:"rust_error,omitempty"`
}

type confluenceVectors struct {
	Comment       []string                  `json:"_comment"`
	IntegrationID string                    `json:"integration_id"`
	ServerName    string                    `json:"server_name"`
	Version       string                    `json:"version"`
	Email         string                    `json:"email"`
	APIToken      string                    `json:"api_token"`
	SiteURLs      []confluenceSiteURLVector `json:"site_urls"`
	Tools         []confluenceToolVector    `json:"tools"`
	Hosting       []confluenceHostingVector `json:"hosting"`
	Calls         []confluenceCallVector    `json:"calls"`
}

// ─── The fake Confluence ─────────────────────────────────────────────────────

// fakeConfluence plays a scripted Confluence and records what it was asked.
//
// One handler for every path, because the point is not to model the API — it is
// to capture the request the tool *built*. A router would have to agree with the
// port about the shape of each path, which is precisely the thing under test.
type fakeConfluence struct {
	mu     sync.Mutex
	script confluenceResponseScript
	seen   *confluenceRequestVector
}

func (f *fakeConfluence) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		body = nil
	}
	f.mu.Lock()
	script := f.script
	f.seen = &confluenceRequestVector{
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

// arm sets the next reply and forgets the last request.
func (f *fakeConfluence) arm(script confluenceResponseScript) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.script = script
	f.seen = nil
}

func (f *fakeConfluence) recorded() *confluenceRequestVector {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.seen
}

// ─── Standing the real server up ─────────────────────────────────────────────

// confluenceSession starts the real integration for services against siteURL and
// returns a client session. `confluence.StartAtSiteURL` is `confluence.Start`
// with only the HTTPS check removed, so the authentication check, the credential
// parse, the server name and the tool registration are the shipped ones.
func confluenceSession(
	t *testing.T, ctx context.Context, siteURL string, services map[string]config.ServiceConfig,
) *mcp.ClientSession {
	t.Helper()

	credentials, err := json.Marshal(config.AtlassianCredentials{
		SiteURL:  siteURL,
		Email:    confluenceParityEmail,
		APIToken: confluenceParityToken,
	})
	if err != nil {
		t.Fatalf("encoding credentials: %v", err)
	}

	cfg := &config.IntegrationConfig{
		ID:          confluenceParityID,
		Name:        "Confluence (parity)",
		Type:        "confluence",
		Enabled:     true,
		Credentials: credentials,
		// `IsAuthenticated` is "present and not the four bytes null"; a
		// token-validated integration stores the outcome here.
		Auth:     json.RawMessage(`{"validated":true}`),
		Services: services,
	}

	serverCfg, err := confluence.StartAtSiteURL(ctx, cfg, siteURL)
	if err != nil {
		t.Fatalf("starting the confluence integration: %v", err)
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

// confluenceAllServices enables the one service with an empty tool list, which
// is the "empty allowed set registers everything" rule — the configuration the
// six schemas and every call vector are taken under.
func confluenceAllServices() map[string]config.ServiceConfig {
	return map[string]config.ServiceConfig{"content": {Enabled: true}}
}

// confluenceHostingCases pin the gates that decide what a server hosts, each in
// the shape a real integration row takes.
var confluenceHostingCases = []confluenceHostingVector{
	{Case: "the service enabled with no tool list — an empty allowed set hosts everything",
		Services: confluenceAllServices()},
	{Case: "no services at all", Services: map[string]config.ServiceConfig{}},
	{Case: "the service disabled contributes neither its gate nor its tools",
		Services: map[string]config.ServiceConfig{
			"content": {Enabled: false, Tools: []string{"list_spaces"}},
		}},
	{Case: "a non-empty allowed set is a filter",
		Services: map[string]config.ServiceConfig{
			"content": {Enabled: true, Tools: []string{"get_page", "list_spaces"}},
		}},
	// The union half of the rule has no shape to take with one service, but an
	// *unknown* service still contributes its names to the set — which is what
	// makes the set a union rather than the enabled service's own list.
	{Case: "an enabled but unknown service still narrows the one that exists",
		Services: map[string]config.ServiceConfig{
			"content": {Enabled: true},
			"other":   {Enabled: true, Tools: []string{"update_page"}},
		}},
	{Case: "a tool named by a disabled service is not allowed anywhere",
		Services: map[string]config.ServiceConfig{
			"content": {Enabled: true, Tools: []string{"get_space"}},
			"other":   {Enabled: false, Tools: []string{"create_page"}},
		}},
}

// confluenceSiteURLCases exercise ValidateSiteURL, which is the gate that keeps
// an API token off a plaintext connection.
//
// `rustError` marks the inputs where `url.Parse` itself fails and Go answers
// with `net/url`'s vocabulary; see confluenceSiteURLVector.
var confluenceSiteURLCases = []struct {
	name      string
	input     string
	rustError string
}{
	{name: "https with no trailing slash is returned unchanged",
		input: "https://acme.atlassian.net"},
	{name: "every trailing slash is trimmed, not just the last",
		input: "https://acme.atlassian.net///"},
	{name: "a base path survives, minus its trailing slash",
		input: "https://intranet.example.com/atlassian/"},
	{name: "a port survives", input: "https://acme.atlassian.net:8443"},
	{name: "the scheme is compared lower-cased, so this passes",
		input: "HTTPS://acme.atlassian.net"},
	{name: "plaintext is refused — the whole point of the check",
		input: "http://acme.atlassian.net"},
	{name: "a bare host has no scheme at all", input: "acme.atlassian.net"},
	{name: "an empty string has no scheme either", input: ""},
	{name: "another scheme is named in the message", input: "ftp://acme.atlassian.net"},
	{name: "https with no authority has no host", input: "https:acme.atlassian.net"},
	{name: "https with an empty authority has no host", input: "https://"},
	{name: "an authority that is only userinfo has no host", input: "https://user@"},
	{
		// No scheme means a *relative* URL to `net/url`, and it refuses one
		// whose first path segment holds a colon. The port classifies it the
		// same way and words it its own way; see the site-URL note below.
		name:      "a leading digit means no scheme, and then the colon is a parse failure",
		input:     "1https://acme.atlassian.net",
		rustError: "invalid site URL: its first path segment holds a colon",
	},
	{
		// `url.Parse` rejects ASCII control characters outright, with a message
		// built by `%q`-quoting the caller's input — Go string escaping this
		// port does not reproduce. The refusal is what matters and it is
		// reproduced; the sentence is pinned as a divergence.
		name:      "a control character is a parse failure, worded differently",
		input:     "https://acme.atlassian.net/\n",
		rustError: "invalid site URL: it holds a control character",
	},
	{
		// `getScheme` errors on a leading colon rather than answering "no
		// scheme", so this is a parse failure too.
		name:      "a leading colon is a parse failure, worded differently",
		input:     ":acme.atlassian.net",
		rustError: "invalid site URL: its first path segment holds a colon",
	},
	{
		// `Parse` cuts the fragment and `parse` cuts the query **before** the
		// relative-path colon check, so a colon in either is not a colon in the
		// first path segment. Both fall through to the scheme refusal.
		name:  "a colon in the query is not a colon in the first path segment",
		input: "acme.atlassian.net?x:y",
	},
	{
		name:  "a colon in the fragment is not one either",
		input: "acme.atlassian.net#x:y",
	},
	{
		// A user-typed site URL need not be percent-encoded: Go sends
		// `/my%20atlassian/...` via `EscapedPath`, and `url` encodes it
		// identically, so this is accepted on both sides and the request the
		// tools build matches. The call vectors below exercise it live.
		name:  "an unencoded base path is accepted — both sides encode it the same way",
		input: "https://intranet.example.com/my atlassian",
	},
	{
		// The three shapes `client::Client::absolute` cannot work behind, all
		// refused at Start rather than at every call. Go accepts the last two
		// and hosts six tools; this port declines to host, which is the safe
		// direction and is why each carries a rustError.
		name:      "a port url::Url cannot represent is refused before it is hosted",
		input:     "https://acme.atlassian.net:99999",
		rustError: "invalid site URL: it is not a URL this build can send a request to",
	},
	{
		name:      "a base carrying its own query leaves the path open",
		input:     "https://acme.atlassian.net?a=b",
		rustError: "invalid site URL: a query or fragment leaves the path open",
	},
	{
		name:      "a base holding a dot segment would move every request",
		input:     "https://acme.atlassian.net/a/../b",
		rustError: "invalid site URL: net/url and url encode its path differently",
	},
	{
		// The case that distinguishes the two parsers on the **authority**, and
		// the reason the host is compared at all. `url` treats `\\` as an
		// authority separator for a special scheme, so it reads the host as
		// `evil.com` and the rest as a path; `net/url` does not, and rejects the
		// userinfo that leaves. Go therefore hosts nothing. A port that compared
		// only url-parsed values would agree with itself and send the user's
		// Basic credentials to evil.com.
		name:      "a backslash before the userinfo is a different host to url and a parse error to Go",
		input:     `https://evil.com\@acme.atlassian.net`,
		rustError: "invalid site URL: its host is not a plain ASCII hostname",
	},
	{
		// Milder, same root: Go escapes `\\ ^ | [ ]` in a path and `url` does
		// not, so this would reach `/a/b` on the right host instead of
		// `/a%5Cb`.
		name:      "a backslash in the base path is escaped by Go and converted by url",
		input:     `https://acme.atlassian.net/a\b`,
		rustError: "invalid site URL: net/url and url encode its path differently",
	},
	{
		// Accepted, and the pair that shows the path comparison is against Go's
		// *escaped* rendering rather than the raw text.
		name:  "a pre-escaped base path is accepted, and so is the same path unescaped",
		input: "https://intranet.example.com/a%20b",
	},
	{
		// `EscapedPath` sends this verbatim even though `escape(Path,
		// encodePath)` would render it `/a%21b`, because `validEncoded` admits
		// `!`. A port comparing against `escape` alone would refuse a base that
		// works — which is why gourl carries `valid_encoded_path`.
		name:  "a base path validEncoded admits is sent verbatim, not re-escaped",
		input: "https://intranet.example.com/a!b(c)[d]",
	},
	{
		// The authority graft that a url-to-url comparison cannot see, because
		// both parsers read the same substring and only disagree on what it
		// means: `parseHost` rejects an escape that decodes to a byte it would
		// have escaped, and `url` decodes them all. This reads as the
		// legitimate host and resolves to a stranger's.
		name:      "a percent escape in the host decodes to a different domain under url",
		input:     "https://acme.atlassian.net%2Eevil.com",
		rustError: "invalid site URL: its host is not a plain ASCII hostname",
	},
	{
		// The third mechanism, and the one that shows why the rule has to be an
		// allowlist rather than a comparison between the two parsers: that is a
		// NO-BREAK SPACE. Go keeps the host literally and cannot resolve it;
		// `url` IDNA-maps it, joining the two labels into one name somebody else
		// owns.
		name:      "a byte Go keeps literal and url IDNA-maps joins two labels into one host",
		input:     "https://acme.atlassian.net\u00a0evil.com",
		rustError: "invalid site URL: its host is not a plain ASCII hostname",
	},
	{
		// Refused, and Go serves it. A site URL is not where credentials belong
		// — the row carries email and API token in their own fields — and
		// admitting userinfo is what lets a backslash hide in front of the `@`.
		name:      "userinfo is refused, because it is where a backslash hides",
		input:     "https://user:pw@acme.atlassian.net",
		rustError: "invalid site URL: its host is not a plain ASCII hostname",
	},
}

// ─── The call cases ──────────────────────────────────────────────────────────

// confluenceOKBody is a response body deliberately hostile to a re-encode: keys
// out of alphabetical order, a trailing-zero decimal, an integer too large for a
// float64 to hold exactly, and interior whitespace. Go passes the bytes through
// verbatim, so all four survive into the result text; a port that decoded to a
// JSON value and re-encoded would change every one of them.
const confluenceOKBody = `{"results":[{"zebra":1,"id":10152021304050607,"rate":1.50, "name":"x"}]}`

// confluenceCallCase is a call vector before the live run fills in what happened.
type confluenceCallCase struct {
	name string
	tool string
	// Appended to the fake's URL to make the site URL this case runs against;
	// empty means the fake's own root.
	basePath      string
	args          map[string]any
	script        confluenceResponseScript
	rustText      string
	rustNoRequest bool
}

func confluenceOK(body string) confluenceResponseScript {
	return confluenceResponseScript{Status: http.StatusOK, Body: body}
}

// confluenceCallCases is the exercise: every tool at least once on its success
// path, plus every distinct failure the client can produce.
//
// The argument maps are complete rather than minimal because **every field is
// required** — `google/jsonschema-go` marks a field optional only on
// `omitempty`/`omitzero`, and not one params struct in this integration carries
// either, so the server refuses a call that omits a field. That is itself part of
// the advertised surface, and it is why the empty-string and zero cases below are
// written as explicit zeros rather than as absent keys.
func confluenceCallCases() []confluenceCallCase {
	return []confluenceCallCase{
		// ─── list_spaces ─────────────────────────────────────────────────────
		{
			name:   "list_spaces/a zero limit takes the 50 fallback",
			tool:   "list_spaces",
			args:   map[string]any{"limit": 0},
			script: confluenceOK(confluenceOKBody),
		},
		{
			name:   "list_spaces/over 250 falls back too, rather than clamping to 250",
			tool:   "list_spaces",
			args:   map[string]any{"limit": 251},
			script: confluenceOK(confluenceOKBody),
		},
		{
			name:   "list_spaces/250 is the largest value that survives",
			tool:   "list_spaces",
			args:   map[string]any{"limit": 250},
			script: confluenceOK(confluenceOKBody),
		},
		{
			name:   "list_spaces/a negative limit is a zero, not an error",
			tool:   "list_spaces",
			args:   map[string]any{"limit": -5},
			script: confluenceOK(confluenceOKBody),
		},
		{
			name:   "list_spaces/a non-2xx status is the API's own body, verbatim",
			tool:   "list_spaces",
			args:   map[string]any{"limit": 1},
			script: confluenceResponseScript{Status: 403, Body: `{"message":"Forbidden <you>"}`},
		},
		{
			// The gate is the 2xx *range*, not `== 200`.
			name:   "list_spaces/a 204 is a success, and its empty body is the result",
			tool:   "list_spaces",
			args:   map[string]any{"limit": 10},
			script: confluenceResponseScript{Status: http.StatusNoContent, Body: ""},
		},

		// ─── get_space ───────────────────────────────────────────────────────
		{
			name:   "get_space/a plain id",
			tool:   "get_space",
			args:   map[string]any{"space_id": "123456"},
			script: confluenceOK(`{"key":"DEV"}`),
		},
		{
			// `PathEscape` is `encodePathSegment`, which escapes `/ ; , ?` and
			// leaves `$ & + : = @` alone. Both halves are here, plus multi-byte
			// UTF-8, which becomes one `%XX` per byte in uppercase hex.
			name:   "get_space/the reserved bytes a segment keeps, the ones it escapes, and UTF-8",
			tool:   "get_space",
			args:   map[string]any{"space_id": "a$&+:=@b my space/x;y,z?q café日本語"},
			script: confluenceOK(`{"key":"X"}`),
		},

		// ─── search_content ──────────────────────────────────────────────────
		{
			// `QueryEscape` is `encodeQueryComponent`: a space is `+`, not
			// `%20`, and `=` and `&` are escaped. Real CQL is all three.
			name: "search_content/CQL is query-escaped, so a space is a plus",
			tool: "search_content",
			args: map[string]any{
				"cql": "space = DEV AND type = page AND title ~ \"a&b\"", "limit": 0,
			},
			script: confluenceOK(confluenceOKBody),
		},
		{
			name:   "search_content/the fallback is 25 here, not list_spaces' 50",
			tool:   "search_content",
			args:   map[string]any{"cql": "type=page", "limit": 300},
			script: confluenceOK(confluenceOKBody),
		},
		{
			name:   "search_content/an empty CQL still sends the key",
			tool:   "search_content",
			args:   map[string]any{"cql": "", "limit": 7},
			script: confluenceOK(confluenceOKBody),
		},

		// ─── get_page ────────────────────────────────────────────────────────
		{
			name:   "get_page/an empty body_format takes the storage default",
			tool:   "get_page",
			args:   map[string]any{"page_id": "98765", "body_format": ""},
			script: confluenceOK(`{"title":"Home"}`),
		},
		{
			name:   "get_page/an explicit format is passed through",
			tool:   "get_page",
			args:   map[string]any{"page_id": "98765", "body_format": "view"},
			script: confluenceOK(`{"title":"Home"}`),
		},
		{
			// The default is applied on **empty**, not on an unrecognized value
			// — so garbage reaches Confluence and Confluence answers for it.
			name:   "get_page/an unrecognized format is not corrected, only escaped",
			tool:   "get_page",
			args:   map[string]any{"page_id": "98765", "body_format": "storage view&x"},
			script: confluenceResponseScript{Status: 400, Body: `{"message":"bad body-format"}`},
		},

		// ─── create_page ─────────────────────────────────────────────────────
		{
			// The body is XHTML, so `json.Marshal`'s HTML escaping fires on
			// every real call — and the map is sorted at both levels.
			name: "create_page/a nested body, sorted keys, and XHTML escaped",
			tool: "create_page",
			args: map[string]any{
				"space_id":  "SPACE1",
				"title":     "Release <1.0> & notes",
				"body":      "<p>hi &amp; bye</p>",
				"parent_id": "",
			},
			script: confluenceResponseScript{Status: http.StatusOK, Body: `{"id":"1"}`},
		},
		{
			name: "create_page/a parent adds the one conditional key",
			tool: "create_page",
			args: map[string]any{
				"space_id":  "SPACE1",
				"title":     "Child",
				"body":      "<p/>",
				"parent_id": "424242",
			},
			script: confluenceResponseScript{Status: http.StatusOK, Body: `{"id":"2"}`},
		},
		{
			name: "create_page/empty strings are still sent — only parent_id is conditional",
			tool: "create_page",
			args: map[string]any{
				"space_id": "", "title": "", "body": "", "parent_id": "",
			},
			script: confluenceResponseScript{Status: 400, Body: `{"message":"spaceId required"}`},
		},

		// ─── update_page ─────────────────────────────────────────────────────
		{
			name: "update_page/the version is incremented, and the id is in both the path and the body",
			tool: "update_page",
			args: map[string]any{
				"page_id": "98765", "title": "New title",
				"body": "<p>v2 &lt;b&gt;</p>", "version": 41,
			},
			script: confluenceResponseScript{Status: http.StatusOK, Body: `{"id":"98765"}`},
		},
		{
			name: "update_page/a zero version increments to one",
			tool: "update_page",
			args: map[string]any{
				"page_id": "1", "title": "t", "body": "b", "version": 0,
			},
			script: confluenceResponseScript{Status: http.StatusOK, Body: `{"id":"1"}`},
		},
		{
			name: "update_page/a negative version is not clamped",
			tool: "update_page",
			args: map[string]any{
				"page_id": "1", "title": "t", "body": "b", "version": -3,
			},
			script: confluenceResponseScript{Status: 409, Body: `{"message":"version conflict"}`},
		},
		{
			name: "update_page/an id needing escaping appears escaped in the path and raw in the body",
			tool: "update_page",
			args: map[string]any{
				"page_id": "a/b c", "title": "t", "body": "b", "version": 2,
			},
			script: confluenceResponseScript{Status: http.StatusOK, Body: `{"id":"a/b c"}`},
		},

		// ─── the dot-segment refusal ─────────────────────────────────────────
		{
			// Go sends `/wiki/api/v2/spaces/..` verbatim and gets a 404; `url`
			// would resolve it to `/wiki/api/v2/`, which is a *different
			// endpoint* answering with a request that carries the API token.
			name:          "get_space/a dot-dot segment reaches a different endpoint under url::Url",
			tool:          "get_space",
			args:          map[string]any{"space_id": ".."},
			script:        confluenceResponseScript{Status: 404, Body: `{"message":"Not Found"}`},
			rustText:      "calling confluence API: request failed",
			rustNoRequest: true,
		},
		{
			name:          "get_page/a single dot segment is sent literally too",
			tool:          "get_page",
			args:          map[string]any{"page_id": ".", "body_format": ""},
			script:        confluenceResponseScript{Status: 404, Body: `{"message":"Not Found"}`},
			rustText:      "calling confluence API: request failed",
			rustNoRequest: true,
		},
		{
			// The write path: a PUT whose body is built and sent before the
			// status is looked at, so the request lands whatever the answer is.
			name: "update_page/dot segments reach the write path too",
			tool: "update_page",
			args: map[string]any{
				"page_id": "..", "title": "t", "body": "b", "version": 1,
			},
			script:        confluenceResponseScript{Status: 404, Body: `{"message":"Not Found"}`},
			rustText:      "calling confluence API: request failed",
			rustNoRequest: true,
		},
		// ─── a site URL with a base path of its own ──────────────────────────
		//
		// Every case above runs against a bare `http://127.0.0.1:port`, which
		// hides the half of the request that is *not* built by a tool. A real
		// Atlassian site URL is `https://acme.atlassian.net`, but a self-hosted
		// one carries a path, and that path is user-typed and therefore not
		// necessarily encoded. Go sends `EscapedPath()`; `url::Url::parse`
		// encodes the same bytes the same way — so the two agree on the wire and
		// a port that compared against the *raw* string would refuse every one
		// of these.
		{
			name:     "list_spaces/a site URL with a base path prefixes the target",
			tool:     "list_spaces",
			basePath: "/atlassian",
			args:     map[string]any{"limit": 0},
			script:   confluenceOK(confluenceOKBody),
		},
		{
			name:     "get_page/an unencoded base path is escaped by both sides",
			tool:     "get_page",
			basePath: "/my atlassian",
			args:     map[string]any{"page_id": "98765", "body_format": ""},
			script:   confluenceOK(`{"title":"Home"}`),
		},
		{
			name:     "update_page/a base path prefixes the write too",
			tool:     "update_page",
			basePath: "/atlassian",
			args: map[string]any{
				"page_id": "1", "title": "t", "body": "b", "version": 2,
			},
			script: confluenceResponseScript{Status: http.StatusOK, Body: `{"id":"1"}`},
		},
		{
			// The guard has to keep firing behind a base it did not build.
			name:          "get_space/a dot segment behind a base path is still refused",
			tool:          "get_space",
			basePath:      "/atlassian",
			args:          map[string]any{"space_id": ".."},
			script:        confluenceResponseScript{Status: 404, Body: `{"message":"Not Found"}`},
			rustText:      "calling confluence API: request failed",
			rustNoRequest: true,
		},

		// ─── a zero-fraction float, which models do emit ─────────────────────
		//
		// `modelcontextprotocol/go-sdk` unmarshals `arguments` into a
		// `map[string]any`, validates it against the reflected schema — where
		// JSON Schema counts `50.0` as an `integer` — and re-marshals before the
		// typed decode, so the handler sees 50. `serde_json::from_value::<i64>`
		// refuses outright, so no handler runs and no request is made. The same
		// divergence #312 pinned, and reachable on a **write** here.
		{
			name:          "list_spaces/a zero-fraction float is an integer to Go and not to serde",
			tool:          "list_spaces",
			args:          map[string]any{"limit": json.RawMessage("50.0")},
			script:        confluenceOK(confluenceOKBody),
			rustText:      "failed to deserialize parameters: invalid type: floating point `50.0`, expected i64",
			rustNoRequest: true,
		},
		{
			name: "update_page/the same float reaches the write path, where Go bumps the version",
			tool: "update_page",
			args: map[string]any{
				"page_id": "1", "title": "t", "body": "b",
				"version": json.RawMessage("41.0"),
			},
			script:        confluenceResponseScript{Status: http.StatusOK, Body: `{"id":"1"}`},
			rustText:      "failed to deserialize parameters: invalid type: floating point `41.0`, expected i64",
			rustNoRequest: true,
		},
	}
}

// ─── The generator ───────────────────────────────────────────────────────────

func TestConfluenceVectors(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	fake := &fakeConfluence{}
	srv := httptest.NewServer(fake)
	defer srv.Close()

	want := confluenceVectors{
		Comment: []string{
			"Cross-language parity vectors for internal/integrations/confluence, the",
			"Confluence integration's in-process MCP server. Generated from Go, then frozen.",
			"Read by desktop/parity/confluence_parity_test.go (Go) and by",
			"desktop/src-tauri/src/native/integrations/confluence/ (Rust). Every value is",
			"exactly what Go produces, so a divergence fails one language against the other's",
			"real output rather than against a belief about it.",
			"'site_urls' come from confluence.ValidateSiteURL; 'tools' and 'hosting' from live",
			"tools/list calls against servers built by confluence.StartAtSiteURL; 'calls' from",
			"live tools/call calls against a scripted fake Confluence that records the request",
			"each tool built.",
			"'email' and 'api_token' are fixtures, not secrets: they are recorded so the",
			"base64 of the Basic header is pinned.",
			"Regenerate with: go test ./desktop/parity/ -run TestConfluenceVectors -update-confluence-vectors",
		},
		IntegrationID: confluenceParityID,
		ServerName:    fmt.Sprintf("confluence-%s", confluenceParityID),
		Version:       "1.0.0",
		Email:         confluenceParityEmail,
		APIToken:      confluenceParityToken,
	}

	for _, c := range confluenceSiteURLCases {
		vector := confluenceSiteURLVector{
			Case: c.name, Input: c.input, RustError: c.rustError,
		}
		clean, err := confluence.ValidateSiteURL(c.input)
		if err != nil {
			vector.Error = err.Error()
		} else {
			vector.Clean = clean
		}
		want.SiteURLs = append(want.SiteURLs, vector)
	}

	session := confluenceSession(t, ctx, srv.URL, confluenceAllServices())
	listed, err := session.ListTools(ctx, nil)
	if err != nil {
		t.Fatalf("tools/list: %v", err)
	}
	for _, tool := range listed.Tools {
		schema, err := json.Marshal(tool.InputSchema)
		if err != nil {
			t.Fatalf("encoding %s's input schema: %v", tool.Name, err)
		}
		want.Tools = append(want.Tools, confluenceToolVector{
			Name:        tool.Name,
			Description: tool.Description,
			InputSchema: schema,
		})
	}

	for _, hosting := range confluenceHostingCases {
		hosting.Tools = confluenceHostedTools(t, ctx, srv.URL, hosting.Services)
		want.Hosting = append(want.Hosting, hosting)
	}

	// One session per distinct site URL. Most cases run against the fake's own
	// root; the base-path ones need a server whose credentials carry that base,
	// because a site URL is not a per-call argument.
	sessions := map[string]*mcp.ClientSession{"": session}
	for _, c := range confluenceCallCases() {
		if _, ok := sessions[c.basePath]; !ok {
			sessions[c.basePath] = confluenceSession(
				t, ctx, srv.URL+c.basePath, confluenceAllServices())
		}
		want.Calls = append(want.Calls,
			runConfluenceCallCase(t, ctx, sessions[c.basePath], fake, c))
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateConfluenceVectors {
		if err := os.WriteFile(confluenceVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", confluenceVectorsFile, err)
		}
		t.Logf("wrote %s", confluenceVectorsFile)
		return
	}

	frozen, err := os.ReadFile(confluenceVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-confluence-vectors): %v",
			confluenceVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this Go toolchain produces different results.\n"+
			"Regenerate with -update-confluence-vectors and check what moved — the Rust "+
			"port in native/integrations/confluence/ reads the same file and will fail "+
			"against it. A moved tool name, schema, request or sentence is not a "+
			"cosmetic diff: the names are in every agent's stored allowlist and in "+
			"every tool_use block already written, and the sentences are what the "+
			"model reads.",
			confluenceVectorsFile)
	}
}

// confluenceHostedTools starts a server for services and asks it what it hosts.
//
// The point here is the *set*, and the sort only makes explicit what both SDKs
// already do: `tools/list` answers in **name** order, not registration order.
// Registration order is pinned on the Rust side instead, by
// `an_empty_allowed_set_hosts_every_tool` against `CONFLUENCE_TOOL_NAMES`.
func confluenceHostedTools(
	t *testing.T, ctx context.Context, siteURL string, services map[string]config.ServiceConfig,
) []string {
	t.Helper()
	listed, err := confluenceSession(t, ctx, siteURL, services).ListTools(ctx, nil)
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

// runConfluenceCallCase arms the fake, makes one real tools/call and records
// everything observable: the request the tool built, and the result the model
// would read.
func runConfluenceCallCase(
	t *testing.T, ctx context.Context,
	session *mcp.ClientSession, fake *fakeConfluence, c confluenceCallCase,
) confluenceCallVector {
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

	// The credentials must never reach the model, on either path.
	for _, secret := range []string{confluenceParityToken, confluenceParityEmail} {
		if strings.Contains(text.Text, secret) {
			t.Fatalf("%s: a credential leaked into a tool result: %s", c.name, text.Text)
		}
	}

	return confluenceCallVector{
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
