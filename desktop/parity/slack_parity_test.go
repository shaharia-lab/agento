// Cross-language vectors for the Slack integration MCP server
// (`internal/integrations/slack`, ported in #315 to
// `desktop/src-tauri/src/native/integrations/slack/`).
//
// Four things have to match byte for byte, none checkable from one language
// alone: which tools are hosted, each advertised schema, the request each tool
// builds, and the result text of every success and every failure. The reasoning
// for each is in `confluence_parity_test.go`'s header and is not repeated.
//
// Slack-specific things pinned here that a port would otherwise get wrong:
//
//   - **`ok` decides, not the HTTP status.** `readSlackResponse` checks 429 and
//     then ignores the status, so a 500 carrying `{"ok":true}` is a **success**
//     and a 200 carrying `{"ok":false}` is a failure. Every other integration in
//     this tree gates on the 2xx range, so this is the one a port gets backwards.
//   - **Two encodings.** Five tools send `url.Values.Encode()` as
//     `application/x-www-form-urlencoded`; two send `json.Marshal` as
//     `application/json; charset=utf-8` — with the charset, which nothing else
//     sends.
//   - **Every clamp differs**: 1000/100 for the two listers, 100/20 for
//     `read_messages` and `search_messages`' count, and a floor of 1 with **no
//     ceiling** for `page`.
//   - **Five tools return Slack's body unlabelled**; the two senders prefix it.
//   - **Rate limiting is its own sentence**, interpolating `Retry-After`
//     verbatim — including when the header is absent, which lands as an empty
//     string mid-sentence.
//   - **`resolveToken` has three arms**, and the third is the one a port drops:
//     an unrecognized `auth_mode` falls back to a non-empty bot token. The
//     `tokens` block pins all of them, including that `oauth` reads the **`auth`
//     column** rather than the credentials blob.
//
// The Rust half lives in
// `desktop/src-tauri/src/native/integrations/slack/tests_vectors.rs` and reads
// this same file.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestSlackVectors -update-slack-vectors
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
	"github.com/shaharia-lab/agento/internal/integrations/slack"
)

const slackVectorsFile = "slack_vectors.json"

var updateSlackVectors = flag.Bool("update-slack-vectors", false,
	"rewrite slack_vectors.json from this Go toolchain")

// A fixture, not a secret. Recorded so the `Bearer ` prefix is pinned — Slack's
// own docs use `Bearer` and the older `token=` form still exists, so which one a
// port sends is exactly what no response reveals.
const slackParityToken = "xoxb-parity-slack-token" //nolint:gosec // a test fixture, not a credential

// slackParityID is the integration id, and half of the server name.
const slackParityID = "slack-parity"

type slackToolVector struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
}

// slackRequestVector is what the fake Slack saw. Slack takes everything in the
// body, so `Target` is only ever `/<method>` — the interesting fields are
// `ContentType` and `Body`.
type slackRequestVector struct {
	Method        string `json:"method"`
	Target        string `json:"target"`
	Authorization string `json:"authorization"`
	ContentType   string `json:"content_type"`
	Body          string `json:"body"`
}

// slackResponseScript is what the fake answered with. `RetryAfter` is sent only
// when non-empty, which is how the absent-header case is scripted.
type slackResponseScript struct {
	Status     int    `json:"status"`
	RetryAfter string `json:"retry_after,omitempty"`
	Body       string `json:"body"`
}

type slackCallVector struct {
	Case      string              `json:"case"`
	Tool      string              `json:"tool"`
	Arguments json.RawMessage     `json:"arguments"`
	Response  slackResponseScript `json:"response"`
	Request   *slackRequestVector `json:"request"`
	IsError   bool                `json:"is_error"`
	Text      string              `json:"text"`
	// A pinned divergence rather than a match. Two users: `encoding/json`'s
	// syntax-error vocabulary when Slack's body will not parse, and the
	// zero-fraction float the Go SDK re-marshals into an integer.
	RustText string `json:"rust_text,omitempty"`
	// Go made a request and this port deliberately makes none — one user, the
	// zero-fraction float, whose decode fails before any handler runs.
	RustNoRequest bool `json:"rust_no_request,omitempty"`
}

type slackHostingVector struct {
	Case     string                          `json:"case"`
	Services map[string]config.ServiceConfig `json:"services"`
	Tools    []string                        `json:"tools"`
}

// slackTokenVector pins `resolveToken`: which column the token comes from, and
// what each failure says.
//
// This is the only integration whose token can come from the **`auth`** column,
// so the port had to widen a projection that deliberately never selected it.
type slackTokenVector struct {
	Case string `json:"case"`
	// The `credentials` column, verbatim.
	Credentials string `json:"credentials"`
	// The `auth` column, verbatim. Also what `IsAuthenticated` is computed from,
	// so it is never empty in a case that reaches `resolveToken`.
	Auth string `json:"auth"`
	// The `Authorization` header the resulting server sends, which is how the
	// resolved token is observed without recording it directly. Empty when
	// starting failed.
	Authorization string `json:"authorization,omitempty"`
	// `Start`'s error, when there is one.
	Error string `json:"error,omitempty"`
	// Set where the port words the same refusal differently — one cause, an
	// `encoding/json` decode failure, whose Go text is not reproducible and
	// whose serde text would quote the token being decoded.
	RustError string `json:"rust_error,omitempty"`
}

type slackVectors struct {
	Comment       []string             `json:"_comment"`
	IntegrationID string               `json:"integration_id"`
	ServerName    string               `json:"server_name"`
	Version       string               `json:"version"`
	Token         string               `json:"token"`
	Tools         []slackToolVector    `json:"tools"`
	Hosting       []slackHostingVector `json:"hosting"`
	Tokens        []slackTokenVector   `json:"tokens"`
	Calls         []slackCallVector    `json:"calls"`
}

// ─── The fake Slack ──────────────────────────────────────────────────────────

type fakeSlack struct {
	mu     sync.Mutex
	script slackResponseScript
	seen   *slackRequestVector
}

func (f *fakeSlack) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		body = nil
	}
	f.mu.Lock()
	script := f.script
	f.seen = &slackRequestVector{
		Method:        r.Method,
		Target:        r.URL.RequestURI(),
		Authorization: r.Header.Get("Authorization"),
		ContentType:   r.Header.Get("Content-Type"),
		Body:          string(body),
	}
	f.mu.Unlock()

	if script.RetryAfter != "" {
		w.Header().Set("Retry-After", script.RetryAfter)
	}
	w.WriteHeader(script.Status)
	_, _ = w.Write([]byte(script.Body))
}

func (f *fakeSlack) arm(script slackResponseScript) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.script = script
	f.seen = nil
}

func (f *fakeSlack) recorded() *slackRequestVector {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.seen
}

// ─── Standing the real server up ─────────────────────────────────────────────

// slackConfig is a row in the shape `Start` reads.
func slackConfig(
	t *testing.T, services map[string]config.ServiceConfig, credentials, auth string,
) *config.IntegrationConfig {
	t.Helper()
	return &config.IntegrationConfig{
		ID:          slackParityID,
		Name:        "Slack (parity)",
		Type:        "slack",
		Enabled:     true,
		Credentials: json.RawMessage(credentials),
		Auth:        json.RawMessage(auth),
		Services:    services,
	}
}

// botTokenCredentials is the ordinary row: `bot_token` mode with the fixture.
func botTokenCredentials() string {
	return fmt.Sprintf(`{"auth_mode":"bot_token","bot_token":%q}`, slackParityToken)
}

func slackSession(
	t *testing.T, ctx context.Context, services map[string]config.ServiceConfig,
) *mcp.ClientSession {
	t.Helper()

	serverCfg, err := slack.Start(ctx, slackConfig(t, services, botTokenCredentials(), `{"team":"parity"}`))
	if err != nil {
		t.Fatalf("starting the slack integration: %v", err)
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

func slackAllServices() map[string]config.ServiceConfig {
	return map[string]config.ServiceConfig{"messaging": {Enabled: true}}
}

var slackHostingCases = []slackHostingVector{
	{Case: "the service enabled with no tool list — an empty allowed set hosts everything",
		Services: slackAllServices()},
	{Case: "no services at all", Services: map[string]config.ServiceConfig{}},
	{Case: "the service disabled contributes neither its gate nor its tools",
		Services: map[string]config.ServiceConfig{
			"messaging": {Enabled: false, Tools: []string{"send_message"}},
		}},
	{Case: "a non-empty allowed set is a filter",
		Services: map[string]config.ServiceConfig{
			"messaging": {Enabled: true, Tools: []string{"send_message", "list_users"}},
		}},
	{Case: "an enabled but unknown service still narrows the one that exists",
		Services: map[string]config.ServiceConfig{
			"messaging": {Enabled: true},
			"other":     {Enabled: true, Tools: []string{"read_messages"}},
		}},
	{Case: "a tool named by a disabled service is not allowed anywhere",
		Services: map[string]config.ServiceConfig{
			"messaging": {Enabled: true, Tools: []string{"get_channel_info"}},
			"other":     {Enabled: false, Tools: []string{"send_reply"}},
		}},
}

// slackTokenCases exercise `resolveToken` — every arm, and both columns.
var slackTokenCases = []struct {
	name        string
	credentials string
	auth        string
	rustError   string
}{
	{
		name:        "bot_token mode reads the credentials blob",
		credentials: botTokenCredentials(),
		auth:        `{"team":"parity"}`,
	},
	{
		// The arm no other integration has: the token is the **`auth` column**,
		// decoded as an `oauth2.Token`.
		name:        "oauth mode reads the auth column, not the credentials",
		credentials: `{"auth_mode":"oauth","bot_token":"xoxb-ignored-in-oauth-mode"}`,
		auth:        `{"access_token":"xoxp-parity-oauth-token","token_type":"Bearer"}`,
	},
	{
		// The fallback a port drops: an unrecognized mode still works if a bot
		// token is there.
		name:        "an unrecognized auth_mode falls back to a non-empty bot token",
		credentials: fmt.Sprintf(`{"auth_mode":"","bot_token":%q}`, slackParityToken),
		auth:        `{"team":"parity"}`,
	},
	{
		name:        "an unrecognized auth_mode with no bot token is refused",
		credentials: `{"auth_mode":"magic","bot_token":""}`,
		auth:        `{"team":"parity"}`,
	},
	{
		name:        "bot_token mode with an empty token is refused",
		credentials: `{"auth_mode":"bot_token","bot_token":""}`,
		auth:        `{"team":"parity"}`,
	},
	{
		// `oauth` with an `auth` column that is not a token: Go's
		// `encoding/json` wording is not reproducible, and serde's would quote
		// the value being decoded — which in this arm is the access token.
		name:        "oauth mode with an auth column that does not decode",
		credentials: `{"auth_mode":"oauth"}`,
		auth:        `["not","a","token"]`,
		rustError: "resolving slack token for \"slack-parity\": parsing oauth token: " +
			"does not decode at line 1 column 8",
	},
	{
		// An `auth` column that decodes but carries no access token yields an
		// **empty** bearer, which Go sends as `Bearer ` — a header a port might
		// "helpfully" omit.
		name:        "oauth mode with no access_token sends an empty bearer",
		credentials: `{"auth_mode":"oauth"}`,
		auth:        `{"token_type":"Bearer"}`,
	},
}

// ─── The call cases ──────────────────────────────────────────────────────────

// slackOK is a success envelope hostile to a re-encode: keys out of order, a
// trailing-zero decimal, an integer too large for a float64, interior
// whitespace. Go passes the bytes through verbatim.
const slackOK = `{"ok":true,"zebra":1,"id":10152021304050607,"rate":1.50, "channels":[]}`

type slackCallCase struct {
	name          string
	tool          string
	args          map[string]any
	script        slackResponseScript
	rustText      string
	rustNoRequest bool
}

func slackOKScript() slackResponseScript {
	return slackResponseScript{Status: http.StatusOK, Body: slackOK}
}

func slackCallCases() []slackCallCase {
	return []slackCallCase{
		// ─── list_channels ───────────────────────────────────────────────────
		{
			name:   "list_channels/zero limit takes the 100 fallback and sends no cursor",
			tool:   "list_channels",
			args:   map[string]any{"limit": 0, "cursor": ""},
			script: slackOKScript(),
		},
		{
			name:   "list_channels/over 1000 falls back to 100, and a cursor is url-encoded",
			tool:   "list_channels",
			args:   map[string]any{"limit": 1001, "cursor": "dXNlcjpVMDYx a+b"},
			script: slackOKScript(),
		},
		{
			name:   "list_channels/1000 is the largest value that survives",
			tool:   "list_channels",
			args:   map[string]any{"limit": 1000, "cursor": ""},
			script: slackOKScript(),
		},

		// ─── get_channel_info ────────────────────────────────────────────────
		{
			name:   "get_channel_info/one form field",
			tool:   "get_channel_info",
			args:   map[string]any{"channel": "C0123"},
			script: slackOKScript(),
		},
		{
			name:   "get_channel_info/a channel needing form escaping",
			tool:   "get_channel_info",
			args:   map[string]any{"channel": "a b&c=d/e日本語"},
			script: slackOKScript(),
		},

		// ─── read_messages ───────────────────────────────────────────────────
		{
			// 100/20 here, a tenth of the listers' ceiling.
			name:   "read_messages/the clamp is 100 and the fallback 20, not the listers' 1000 and 100",
			tool:   "read_messages",
			args:   map[string]any{"channel": "C1", "limit": 101, "cursor": ""},
			script: slackOKScript(),
		},
		{
			name:   "read_messages/100 survives, and a cursor is sent",
			tool:   "read_messages",
			args:   map[string]any{"channel": "C1", "limit": 100, "cursor": "next"},
			script: slackOKScript(),
		},

		// ─── send_message / send_reply ───────────────────────────────────────
		{
			// JSON, with the charset nothing else sends, and sorted keys.
			name:   "send_message/a JSON body with the charset content type",
			tool:   "send_message",
			args:   map[string]any{"channel": "C1", "text": "hello <there> & welcome"},
			script: slackOKScript(),
		},
		{
			name:   "send_reply/the same Slack method with one more key, and a different sentence",
			tool:   "send_reply",
			args:   map[string]any{"channel": "C1", "thread_ts": "1727700000.000100", "text": "ack"},
			script: slackOKScript(),
		},

		// ─── list_users ──────────────────────────────────────────────────────
		{
			name:   "list_users/no channel field at all, unlike read_messages",
			tool:   "list_users",
			args:   map[string]any{"limit": 0, "cursor": ""},
			script: slackOKScript(),
		},

		// ─── search_messages ─────────────────────────────────────────────────
		{
			name:   "search_messages/count clamps at 100 and page has a floor of 1",
			tool:   "search_messages",
			args:   map[string]any{"query": "from:me in:#general", "count": 0, "page": 0},
			script: slackOKScript(),
		},
		{
			// `page` has **no ceiling**, unlike every other numeric input.
			name:   "search_messages/page has no ceiling",
			tool:   "search_messages",
			args:   map[string]any{"query": "x", "count": 101, "page": 999999},
			script: slackOKScript(),
		},
		{
			name:   "search_messages/an empty query still sends the key",
			tool:   "search_messages",
			args:   map[string]any{"query": "", "count": 50, "page": 2},
			script: slackOKScript(),
		},

		// ─── the envelope, which decides instead of the status ────────────────
		{
			name:   "list_users/a 200 carrying ok:false is a failure",
			tool:   "list_users",
			args:   map[string]any{"limit": 0, "cursor": ""},
			script: slackResponseScript{Status: http.StatusOK, Body: `{"ok":false,"error":"not_authed"}`},
		},
		{
			// The one that reads backwards: a 500 with `ok:true` is a success,
			// and the body is returned verbatim.
			name:   "list_users/a 500 carrying ok:true is a success",
			tool:   "list_users",
			args:   map[string]any{"limit": 0, "cursor": ""},
			script: slackResponseScript{Status: http.StatusInternalServerError, Body: slackOK},
		},
		{
			name:   "get_channel_info/ok:false with no error field interpolates an empty one",
			tool:   "get_channel_info",
			args:   map[string]any{"channel": "C1"},
			script: slackResponseScript{Status: http.StatusOK, Body: `{"ok":false}`},
		},
		{
			// A body that is not JSON at all. Go's scanner vocabulary has no
			// serde equivalent, so the sentence is pinned as a divergence.
			name:     "get_channel_info/a body that is not JSON",
			tool:     "get_channel_info",
			args:     map[string]any{"channel": "C1"},
			script:   slackResponseScript{Status: http.StatusOK, Body: `<html>nope</html>`},
			rustText: "parsing response: expected value at line 1 column 1",
		},
		{
			// Slack sends `"error": null` on a success. `encoding/json` reads a
			// null as the zero value, so this is an ordinary success — where a
			// plain serde derive calls it a type error and turns a Go success into
			// a tool error.
			name:   "get_channel_info/a null error field is a zero value, not a type error",
			tool:   "get_channel_info",
			args:   map[string]any{"channel": "C1"},
			script: slackResponseScript{Status: http.StatusOK, Body: `{"ok":true,"error":null}`},
		},
		{
			name:   "get_channel_info/a null ok field is a false one",
			tool:   "get_channel_info",
			args:   map[string]any{"channel": "C1"},
			script: slackResponseScript{Status: http.StatusOK, Body: `{"ok":null}`},
		},
		{
			// The same rule one level further out: `json.Unmarshal` of a bare
			// `null` into a struct is a no-op returning nil, so Go falls through
			// to the `!ok` branch rather than failing to parse.
			name:   "get_channel_info/a bare null body is a no-op, not a parse failure",
			tool:   "get_channel_info",
			args:   map[string]any{"channel": "C1"},
			script: slackResponseScript{Status: http.StatusOK, Body: `null`},
		},
		{
			// The other direction: serde builds a struct from a sequence
			// positionally when every field has a default, so without `GoStruct`
			// this would decode to `ok: true` and return the array as a success.
			// Go refuses it, and the two refusals word it differently.
			name:     "get_channel_info/a JSON array is not a struct",
			tool:     "get_channel_info",
			args:     map[string]any{"channel": "C1"},
			script:   slackResponseScript{Status: http.StatusOK, Body: `[true]`},
			rustText: "parsing response: invalid type: sequence, expected a JSON object at line 1 column 0",
		},

		// ─── rate limiting ───────────────────────────────────────────────────
		{
			name:   "send_message/429 is its own sentence, carrying Retry-After",
			tool:   "send_message",
			args:   map[string]any{"channel": "C1", "text": "hi"},
			script: slackResponseScript{Status: http.StatusTooManyRequests, RetryAfter: "30", Body: ""},
		},
		{
			// An absent header is `""` to Go, which lands mid-sentence as two
			// spaces. A port that omitted it would read differently.
			name:   "read_messages/429 with no Retry-After leaves an empty gap in the sentence",
			tool:   "read_messages",
			args:   map[string]any{"channel": "C1", "limit": 0, "cursor": ""},
			script: slackResponseScript{Status: http.StatusTooManyRequests, Body: ""},
		},

		// ─── a zero-fraction float, which models do emit ──────────────────────
		{
			name:          "list_channels/a zero-fraction float is an integer to Go and not to serde",
			tool:          "list_channels",
			args:          map[string]any{"limit": json.RawMessage("100.0"), "cursor": ""},
			script:        slackOKScript(),
			rustText:      "failed to deserialize parameters: invalid type: floating point `100.0`, expected i64",
			rustNoRequest: true,
		},
	}
}

// ─── The generator ───────────────────────────────────────────────────────────

func TestSlackVectors(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	fake := &fakeSlack{}
	srv := httptest.NewServer(fake)
	defer srv.Close()
	restore := slack.SetAPIBase(srv.URL)
	defer restore()

	want := slackVectors{
		Comment: []string{
			"Cross-language parity vectors for internal/integrations/slack, the Slack",
			"integration's in-process MCP server. Generated from Go, then frozen. Read by",
			"desktop/parity/slack_parity_test.go (Go) and by",
			"desktop/src-tauri/src/native/integrations/slack/ (Rust). Every value is exactly",
			"what Go produces, so a divergence fails one language against the other's real",
			"output rather than against a belief about it.",
			"'tools' and 'hosting' come from live tools/list calls against servers built by",
			"slack.Start; 'calls' from live tools/call calls against a scripted fake Slack",
			"that records the request each tool built; 'tokens' pin resolveToken, including",
			"that oauth mode reads the auth column rather than the credentials blob.",
			"'token' is a fixture, not a secret: it is recorded so the Bearer prefix is pinned.",
			"Regenerate with: go test ./desktop/parity/ -run TestSlackVectors -update-slack-vectors",
		},
		IntegrationID: slackParityID,
		ServerName:    fmt.Sprintf("slack-%s", slackParityID),
		Version:       "1.0.0",
		Token:         slackParityToken,
	}

	session := slackSession(t, ctx, slackAllServices())
	listed, err := session.ListTools(ctx, nil)
	if err != nil {
		t.Fatalf("tools/list: %v", err)
	}
	for _, tool := range listed.Tools {
		schema, err := json.Marshal(tool.InputSchema)
		if err != nil {
			t.Fatalf("encoding %s's input schema: %v", tool.Name, err)
		}
		want.Tools = append(want.Tools, slackToolVector{
			Name:        tool.Name,
			Description: tool.Description,
			InputSchema: schema,
		})
	}

	for _, hosting := range slackHostingCases {
		hosting.Tools = slackHostedTools(t, ctx, hosting.Services)
		want.Hosting = append(want.Hosting, hosting)
	}

	for _, c := range slackTokenCases {
		vector := runSlackTokenCase(t, ctx, fake, c.name, c.credentials, c.auth)
		vector.RustError = c.rustError
		want.Tokens = append(want.Tokens, vector)
	}

	for _, c := range slackCallCases() {
		want.Calls = append(want.Calls, runSlackCallCase(t, ctx, session, fake, c))
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateSlackVectors {
		if err := os.WriteFile(slackVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", slackVectorsFile, err)
		}
		t.Logf("wrote %s", slackVectorsFile)
		return
	}

	frozen, err := os.ReadFile(slackVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-slack-vectors): %v", slackVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this Go toolchain produces different results.\n"+
			"Regenerate with -update-slack-vectors and check what moved — the Rust port in "+
			"native/integrations/slack/ reads the same file and will fail against it. A moved "+
			"tool name, schema, request or sentence is not a cosmetic diff: the names are in "+
			"every agent's stored allowlist and in every tool_use block already written, and "+
			"the sentences are what the model reads.",
			slackVectorsFile)
	}
}

func slackHostedTools(
	t *testing.T, ctx context.Context, services map[string]config.ServiceConfig,
) []string {
	t.Helper()
	listed, err := slackSession(t, ctx, services).ListTools(ctx, nil)
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

// runSlackTokenCase starts a server for the given columns and observes which
// token it resolved by making one call and reading the fake's `Authorization`
// header — rather than recording the token directly, which would put the
// resolution under test into the fixture that verifies it.
func runSlackTokenCase(
	t *testing.T, ctx context.Context, fake *fakeSlack, name, credentials, auth string,
) slackTokenVector {
	t.Helper()
	vector := slackTokenVector{Case: name, Credentials: credentials, Auth: auth}

	serverCfg, err := slack.Start(ctx, slackConfig(t, slackAllServices(), credentials, auth))
	if err != nil {
		vector.Error = err.Error()
		return vector
	}

	client := mcp.NewClient(&mcp.Implementation{Name: "parity", Version: "1"}, nil)
	session, err := client.Connect(ctx,
		&mcp.StreamableClientTransport{Endpoint: serverCfg.URL}, nil)
	if err != nil {
		t.Fatalf("%s: connecting: %v", name, err)
	}
	defer func() { _ = session.Close() }()

	fake.arm(slackOKScript())
	if _, err := session.CallTool(ctx, &mcp.CallToolParams{
		Name: "list_users", Arguments: jsonArgs(t, map[string]any{"limit": 0, "cursor": ""}),
	}); err != nil {
		t.Fatalf("%s: tools/call: %v", name, err)
	}
	recorded := fake.recorded()
	if recorded == nil {
		t.Fatalf("%s: no request reached the fake", name)
	}
	vector.Authorization = recorded.Authorization
	return vector
}

func runSlackCallCase(
	t *testing.T, ctx context.Context,
	session *mcp.ClientSession, fake *fakeSlack, c slackCallCase,
) slackCallVector {
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
	if strings.Contains(text.Text, slackParityToken) {
		t.Fatalf("%s: the token leaked into a tool result: %s", c.name, text.Text)
	}

	return slackCallVector{
		Case:          c.name,
		Tool:          c.tool,
		Arguments:     args,
		Response:      c.script,
		Request:       fake.recorded(),
		IsError:       result.IsError,
		Text:          text.Text,
		RustText:      c.rustText,
		RustNoRequest: c.rustNoRequest,
	}
}
