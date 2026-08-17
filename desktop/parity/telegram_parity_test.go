// Cross-language vectors for the Telegram integration's **outbound** MCP server
// (`internal/integrations/telegram`, ported in #314 to
// `desktop/src-tauri/src/native/integrations/telegram/`).
//
// Four things have to match byte for byte, none checkable from one language
// alone: which tools are hosted, each advertised schema, the request each tool
// builds, and the result text of every success and every failure. The reasoning
// is in `confluence_parity_test.go`'s header and is not repeated.
//
// This is the outbound half only. `POST /webhooks/telegram/{id}` and
// `internal/trigger/` are #319 and are not exercised here.
//
// Telegram-specific things pinned here, several of which no earlier integration
// could reach:
//
//   - **`create_poll` takes a `[]string`** — the first slice parameter in any of
//     the six. `jsonschema-go` renders every slice as `["null","array"]` where
//     `schemars` renders a bare `array`, which `claude/schema_vectors.rs` left
//     documented rather than reconciled because nothing reached it. The schema
//     block is what pins the fix.
//   - **`send_location` takes `float64`** — the first floats, so the request
//     bodies pin `encoding/json`'s own float spelling (`1e+21`, `1e-7`) rather
//     than `serde_json`'s.
//   - **The envelope decides and the status never does** — not even a 429, which
//     Slack's client does check. A 500 carrying `{"ok":true}` is a success.
//   - **`result` is a `json.RawMessage`**, so an *absent* one renders as the
//     empty string and an explicit `null` as the four bytes `null`. Both land in
//     a result sentence the model reads.
//   - **`read_messages` sends `offset` on `!= 0`**, so a negative offset is sent;
//     its limit falls back to its own maximum; and `timeout: 0` is always sent.
//   - **`create_poll` refuses 0-1 or 11+ options before any request is made.**
//
// The Rust half lives in
// `desktop/src-tauri/src/native/integrations/telegram/tests_vectors.rs`.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestTelegramVectors -update-telegram-vectors
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
	"github.com/shaharia-lab/agento/internal/integrations/telegram"
)

const telegramVectorsFile = "telegram_vectors.json"

var updateTelegramVectors = flag.Bool("update-telegram-vectors", false,
	"rewrite telegram_vectors.json from this Go toolchain")

// A fixture, not a secret — and this one travels in the **URL path**, which is
// why the recorded request target is worth pinning at all.
const telegramParityToken = "123456:AAF-parity-bot-token" //nolint:gosec // a test fixture

const telegramParityID = "tg-parity"

type telegramToolVector struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
}

// telegramRequestVector is what the fake Telegram saw. `Target` carries the bot
// token, because Telegram's API puts it in the path — which is exactly the part a
// port could get wrong invisibly.
type telegramRequestVector struct {
	Method      string `json:"method"`
	Target      string `json:"target"`
	ContentType string `json:"content_type"`
	Body        string `json:"body"`
}

type telegramResponseScript struct {
	Status int    `json:"status"`
	Body   string `json:"body"`
}

type telegramCallVector struct {
	Case      string                 `json:"case"`
	Tool      string                 `json:"tool"`
	Arguments json.RawMessage        `json:"arguments"`
	Response  telegramResponseScript `json:"response"`
	// nil where the tool answered without making a request — `create_poll`'s
	// option-count refusal is the only such path in Go.
	Request *telegramRequestVector `json:"request"`
	IsError bool                   `json:"is_error"`
	Text    string                 `json:"text"`
	// A pinned divergence. Two users: `encoding/json`'s syntax-error vocabulary,
	// and the zero-fraction float the Go SDK re-marshals into an integer.
	RustText string `json:"rust_text,omitempty"`
	// Go made a request and this port deliberately makes none.
	RustNoRequest bool `json:"rust_no_request,omitempty"`
}

type telegramHostingVector struct {
	Case     string                          `json:"case"`
	Services map[string]config.ServiceConfig `json:"services"`
	Tools    []string                        `json:"tools"`
}

type telegramVectors struct {
	Comment       []string                `json:"_comment"`
	IntegrationID string                  `json:"integration_id"`
	ServerName    string                  `json:"server_name"`
	Version       string                  `json:"version"`
	Token         string                  `json:"token"`
	Tools         []telegramToolVector    `json:"tools"`
	Hosting       []telegramHostingVector `json:"hosting"`
	Calls         []telegramCallVector    `json:"calls"`
}

// ─── The fake Telegram ───────────────────────────────────────────────────────

type fakeTelegram struct {
	mu     sync.Mutex
	script telegramResponseScript
	seen   *telegramRequestVector
}

func (f *fakeTelegram) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		body = nil
	}
	f.mu.Lock()
	script := f.script
	f.seen = &telegramRequestVector{
		Method:      r.Method,
		Target:      r.URL.RequestURI(),
		ContentType: r.Header.Get("Content-Type"),
		Body:        string(body),
	}
	f.mu.Unlock()

	w.WriteHeader(script.Status)
	_, _ = w.Write([]byte(script.Body))
}

func (f *fakeTelegram) arm(script telegramResponseScript) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.script = script
	f.seen = nil
}

func (f *fakeTelegram) recorded() *telegramRequestVector {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.seen
}

// ─── Standing the real server up ─────────────────────────────────────────────

func telegramSession(
	t *testing.T, ctx context.Context, services map[string]config.ServiceConfig,
) *mcp.ClientSession {
	t.Helper()

	credentials, err := json.Marshal(config.TelegramCredentials{BotToken: telegramParityToken})
	if err != nil {
		t.Fatalf("encoding credentials: %v", err)
	}

	cfg := &config.IntegrationConfig{
		ID:          telegramParityID,
		Name:        "Telegram (parity)",
		Type:        "telegram",
		Enabled:     true,
		Credentials: credentials,
		Auth:        json.RawMessage(`{"username":"parity_bot"}`),
		Services:    services,
	}

	serverCfg, err := telegram.Start(ctx, cfg)
	if err != nil {
		t.Fatalf("starting the telegram integration: %v", err)
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

func telegramAllServices() map[string]config.ServiceConfig {
	return map[string]config.ServiceConfig{"messaging": {Enabled: true}}
}

var telegramHostingCases = []telegramHostingVector{
	{Case: "the service enabled with no tool list — an empty allowed set hosts everything",
		Services: telegramAllServices()},
	{Case: "no services at all", Services: map[string]config.ServiceConfig{}},
	{Case: "the service disabled contributes neither its gate nor its tools",
		Services: map[string]config.ServiceConfig{
			"messaging": {Enabled: false, Tools: []string{"send_message"}},
		}},
	{Case: "a non-empty allowed set is a filter",
		Services: map[string]config.ServiceConfig{
			"messaging": {Enabled: true, Tools: []string{"send_message", "pin_message"}},
		}},
	{Case: "an enabled but unknown service still narrows the one that exists",
		Services: map[string]config.ServiceConfig{
			"messaging": {Enabled: true},
			"other":     {Enabled: true, Tools: []string{"create_poll"}},
		}},
	{Case: "a tool named by a disabled service is not allowed anywhere",
		Services: map[string]config.ServiceConfig{
			"messaging": {Enabled: true, Tools: []string{"get_chat_info"}},
			"other":     {Enabled: false, Tools: []string{"send_photo"}},
		}},
}

// ─── The call cases ──────────────────────────────────────────────────────────

// telegramOK is a success envelope whose `result` is hostile to a re-encode:
// keys out of order, a trailing-zero decimal, an integer too large for a float64,
// interior whitespace. Go passes `result`'s bytes through verbatim.
const telegramOK = `{"ok":true,"result":{"zebra":1,"id":10152021304050607,"rate":1.50, "message_id":7}}`

type telegramCallCase struct {
	name          string
	tool          string
	args          map[string]any
	script        telegramResponseScript
	rustText      string
	rustNoRequest bool
}

func telegramOKScript() telegramResponseScript {
	return telegramResponseScript{Status: http.StatusOK, Body: telegramOK}
}

func telegramCallCases() []telegramCallCase {
	return []telegramCallCase{
		// ─── send_message ────────────────────────────────────────────────────
		{
			name:   "send_message/an empty parse_mode leaves no key at all",
			tool:   "send_message",
			args:   map[string]any{"chat_id": "@channel", "text": "hi <there> & bye", "parse_mode": ""},
			script: telegramOKScript(),
		},
		{
			name:   "send_message/a parse_mode is sent when set",
			tool:   "send_message",
			args:   map[string]any{"chat_id": "-1001234567890", "text": "*bold*", "parse_mode": "Markdown"},
			script: telegramOKScript(),
		},

		// ─── send_photo ──────────────────────────────────────────────────────
		{
			name:   "send_photo/an empty caption leaves no key",
			tool:   "send_photo",
			args:   map[string]any{"chat_id": "@c", "photo": "https://example.com/a.png", "caption": ""},
			script: telegramOKScript(),
		},
		{
			name:   "send_photo/a caption is sent when set",
			tool:   "send_photo",
			args:   map[string]any{"chat_id": "@c", "photo": "https://example.com/a.png", "caption": "look"},
			script: telegramOKScript(),
		},

		// ─── send_location, the first float parameters in the six ────────────
		{
			name:   "send_location/ordinary coordinates",
			tool:   "send_location",
			args:   map[string]any{"chat_id": "@c", "latitude": 51.5074, "longitude": -0.1278},
			script: telegramOKScript(),
		},
		{
			// `encoding/json` switches to exponent form outside [1e-6, 1e21) and
			// spells the exponent its own way. `serde_json` does not agree, so
			// this is what `gojson::go_float` is for.
			name:   "send_location/the exponent forms encoding/json switches to",
			tool:   "send_location",
			args:   map[string]any{"chat_id": "@c", "latitude": 1e21, "longitude": 1e-7},
			script: telegramOKScript(),
		},
		{
			name:   "send_location/a whole number has no decimal point",
			tool:   "send_location",
			args:   map[string]any{"chat_id": "@c", "latitude": 90, "longitude": -180},
			script: telegramOKScript(),
		},

		// ─── create_poll, the first slice parameter in the six ───────────────
		{
			name: "create_poll/two options is the minimum, and the slice is sent as an array",
			tool: "create_poll",
			args: map[string]any{
				"chat_id": "@c", "question": "Tea or coffee?",
				"options": []string{"Tea", "Coffee"}, "is_anonymous": false, "type": "",
			},
			script: telegramOKScript(),
		},
		{
			name: "create_poll/an anonymous quiz sends both conditional keys",
			tool: "create_poll",
			args: map[string]any{
				"chat_id": "@c", "question": "2+2?",
				"options": []string{"3", "4", "5"}, "is_anonymous": true, "type": "quiz",
			},
			script: telegramOKScript(),
		},
		{
			// Go refuses **before** it builds a request.
			name: "create_poll/one option is refused before any request",
			tool: "create_poll",
			args: map[string]any{
				"chat_id": "@c", "question": "?",
				"options": []string{"only"}, "is_anonymous": false, "type": "",
			},
			script: telegramOKScript(),
		},
		{
			name: "create_poll/eleven options is refused before any request",
			tool: "create_poll",
			args: map[string]any{
				"chat_id": "@c", "question": "?",
				"options":      []string{"1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11"},
				"is_anonymous": false, "type": "",
			},
			script: telegramOKScript(),
		},
		{
			// The whole reason the schema carries `"type": ["null","array"]`: Go
			// accepts a null for any slice and it decodes to a nil one, whose
			// length is 0 — so it reaches the same refusal rather than a decode
			// error.
			name: "create_poll/a null options list is a nil slice, so it is a zero-length refusal",
			tool: "create_poll",
			args: map[string]any{
				"chat_id": "@c", "question": "?",
				"options": nil, "is_anonymous": false, "type": "",
			},
			script: telegramOKScript(),
		},

		// ─── read_messages ───────────────────────────────────────────────────
		{
			name:   "read_messages/a zero offset leaves no key, and the limit falls back to its own maximum",
			tool:   "read_messages",
			args:   map[string]any{"offset": 0, "limit": 0},
			script: telegramOKScript(),
		},
		{
			// `!= 0`, not `> 0`: a negative offset is Telegram's idiom for "the
			// last N updates" and is deliberately sent.
			name:   "read_messages/a negative offset is sent, because the test is not-zero",
			tool:   "read_messages",
			args:   map[string]any{"offset": -5, "limit": 10},
			script: telegramOKScript(),
		},
		{
			name:   "read_messages/over 100 falls back to 100",
			tool:   "read_messages",
			args:   map[string]any{"offset": 42, "limit": 101},
			script: telegramOKScript(),
		},

		// ─── the reads ───────────────────────────────────────────────────────
		{
			name:   "get_chat_info/one key",
			tool:   "get_chat_info",
			args:   map[string]any{"chat_id": "@c"},
			script: telegramOKScript(),
		},
		{
			name:   "get_chat_members/the deprecated method name Go still sends",
			tool:   "get_chat_members",
			args:   map[string]any{"chat_id": "@c"},
			script: telegramResponseScript{Status: http.StatusOK, Body: `{"ok":true,"result":42}`},
		},

		// ─── the management tools ────────────────────────────────────────────
		{
			name:   "forward_message/three keys, one an integer",
			tool:   "forward_message",
			args:   map[string]any{"chat_id": "@to", "from_chat_id": "@from", "message_id": 99},
			script: telegramOKScript(),
		},
		{
			name:   "edit_message/an empty parse_mode leaves no key",
			tool:   "edit_message",
			args:   map[string]any{"chat_id": "@c", "message_id": 5, "text": "new", "parse_mode": ""},
			script: telegramOKScript(),
		},
		{
			// The response is discarded — the sentence is fixed.
			name:   "delete_message/the response is discarded",
			tool:   "delete_message",
			args:   map[string]any{"chat_id": "@c", "message_id": 5},
			script: telegramResponseScript{Status: http.StatusOK, Body: `{"ok":true,"result":true}`},
		},
		{
			name:   "pin_message/a false disable_notification leaves no key",
			tool:   "pin_message",
			args:   map[string]any{"chat_id": "@c", "message_id": 5, "disable_notification": false},
			script: telegramResponseScript{Status: http.StatusOK, Body: `{"ok":true,"result":true}`},
		},
		{
			name:   "pin_message/a true one sends the literal true",
			tool:   "pin_message",
			args:   map[string]any{"chat_id": "@c", "message_id": 5, "disable_notification": true},
			script: telegramResponseScript{Status: http.StatusOK, Body: `{"ok":true,"result":true}`},
		},

		// ─── the envelope, which decides instead of the status ────────────────
		{
			name:   "send_message/ok:false is the failure, whatever the status",
			tool:   "send_message",
			args:   map[string]any{"chat_id": "@c", "text": "x", "parse_mode": ""},
			script: telegramResponseScript{Status: http.StatusOK, Body: `{"ok":false,"description":"Bad Request: chat not found"}`},
		},
		{
			// The one that reads backwards.
			name:   "get_chat_info/a 500 carrying ok:true is a success",
			tool:   "get_chat_info",
			args:   map[string]any{"chat_id": "@c"},
			script: telegramResponseScript{Status: http.StatusInternalServerError, Body: telegramOK},
		},
		{
			// Not even a 429 is looked at, unlike Slack's client.
			name:   "get_chat_info/a 429 is not special-cased the way Slack's is",
			tool:   "get_chat_info",
			args:   map[string]any{"chat_id": "@c"},
			script: telegramResponseScript{Status: http.StatusTooManyRequests, Body: `{"ok":false,"description":"Too Many Requests: retry after 30"}`},
		},
		{
			name:   "get_chat_info/a null description interpolates an empty one",
			tool:   "get_chat_info",
			args:   map[string]any{"chat_id": "@c"},
			script: telegramResponseScript{Status: http.StatusOK, Body: `{"ok":false,"description":null}`},
		},
		{
			// `result` is a RawMessage: absent is the empty string.
			name:   "get_chat_info/an absent result renders as the empty string",
			tool:   "get_chat_info",
			args:   map[string]any{"chat_id": "@c"},
			script: telegramResponseScript{Status: http.StatusOK, Body: `{"ok":true}`},
		},
		{
			// …and an explicit null is the four bytes.
			name:   "get_chat_info/an explicit null result renders as the four bytes",
			tool:   "get_chat_info",
			args:   map[string]any{"chat_id": "@c"},
			script: telegramResponseScript{Status: http.StatusOK, Body: `{"ok":true,"result":null}`},
		},
		{
			name:   "get_chat_info/a bare null body is a no-op, not a parse failure",
			tool:   "get_chat_info",
			args:   map[string]any{"chat_id": "@c"},
			script: telegramResponseScript{Status: http.StatusOK, Body: `null`},
		},
		{
			// serde would build the struct from a sequence positionally without
			// `GoStruct`, turning this into a success.
			name:     "get_chat_info/a JSON array is not a struct",
			tool:     "get_chat_info",
			args:     map[string]any{"chat_id": "@c"},
			script:   telegramResponseScript{Status: http.StatusOK, Body: `[true]`},
			rustText: "parsing response: invalid type: sequence, expected a JSON object at line 1 column 0",
		},
		{
			name:     "get_chat_info/a body that is not JSON at all",
			tool:     "get_chat_info",
			args:     map[string]any{"chat_id": "@c"},
			script:   telegramResponseScript{Status: http.StatusOK, Body: `<html>nope</html>`},
			rustText: "parsing response: expected value at line 1 column 1",
		},

		// ─── a zero-fraction float for an integer field ───────────────────────
		{
			name:          "read_messages/a zero-fraction float is an integer to Go and not to serde",
			tool:          "read_messages",
			args:          map[string]any{"offset": 0, "limit": json.RawMessage("50.0")},
			script:        telegramOKScript(),
			rustText:      "failed to deserialize parameters: invalid type: floating point `50.0`, expected i64",
			rustNoRequest: true,
		},
	}
}

// ─── The generator ───────────────────────────────────────────────────────────

func TestTelegramVectors(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	fake := &fakeTelegram{}
	srv := httptest.NewServer(fake)
	defer srv.Close()
	restore := telegram.SetAPIBase(srv.URL)
	defer restore()

	want := telegramVectors{
		Comment: []string{
			"Cross-language parity vectors for internal/integrations/telegram, the outbound",
			"half of the Telegram integration's in-process MCP server (the inbound webhook",
			"and trigger dispatcher are #319 and are not covered here). Generated from Go,",
			"then frozen. Read by desktop/parity/telegram_parity_test.go (Go) and by",
			"desktop/src-tauri/src/native/integrations/telegram/ (Rust).",
			"'tools' and 'hosting' come from live tools/list calls against servers built by",
			"telegram.Start; 'calls' from live tools/call calls against a scripted fake",
			"Telegram that records the request each tool built.",
			"'token' is a fixture, not a secret: it is recorded because Telegram puts the bot",
			"token in the URL path, so the request target pins it.",
			"Regenerate with: go test ./desktop/parity/ -run TestTelegramVectors -update-telegram-vectors",
		},
		IntegrationID: telegramParityID,
		ServerName:    fmt.Sprintf("telegram-%s", telegramParityID),
		Version:       "1.0.0",
		Token:         telegramParityToken,
	}

	session := telegramSession(t, ctx, telegramAllServices())
	listed, err := session.ListTools(ctx, nil)
	if err != nil {
		t.Fatalf("tools/list: %v", err)
	}
	for _, tool := range listed.Tools {
		schema, err := json.Marshal(tool.InputSchema)
		if err != nil {
			t.Fatalf("encoding %s's input schema: %v", tool.Name, err)
		}
		want.Tools = append(want.Tools, telegramToolVector{
			Name:        tool.Name,
			Description: tool.Description,
			InputSchema: schema,
		})
	}

	for _, hosting := range telegramHostingCases {
		hosting.Tools = telegramHostedTools(t, ctx, hosting.Services)
		want.Hosting = append(want.Hosting, hosting)
	}

	for _, c := range telegramCallCases() {
		want.Calls = append(want.Calls, runTelegramCallCase(t, ctx, session, fake, c))
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateTelegramVectors {
		if err := os.WriteFile(telegramVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", telegramVectorsFile, err)
		}
		t.Logf("wrote %s", telegramVectorsFile)
		return
	}

	frozen, err := os.ReadFile(telegramVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-telegram-vectors): %v",
			telegramVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this Go toolchain produces different results.\n"+
			"Regenerate with -update-telegram-vectors and check what moved — the Rust port "+
			"in native/integrations/telegram/ reads the same file and will fail against it. "+
			"A moved tool name, schema, request or sentence is not a cosmetic diff: the "+
			"names are in every agent's stored allowlist and in every tool_use block "+
			"already written, and the sentences are what the model reads.",
			telegramVectorsFile)
	}
}

func telegramHostedTools(
	t *testing.T, ctx context.Context, services map[string]config.ServiceConfig,
) []string {
	t.Helper()
	listed, err := telegramSession(t, ctx, services).ListTools(ctx, nil)
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

func runTelegramCallCase(
	t *testing.T, ctx context.Context,
	session *mcp.ClientSession, fake *fakeTelegram, c telegramCallCase,
) telegramCallVector {
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
	// The bot token travels in the URL, so a result that echoed the request
	// would leak it. Neither language must.
	if strings.Contains(text.Text, telegramParityToken) {
		t.Fatalf("%s: the bot token leaked into a tool result: %s", c.name, text.Text)
	}

	return telegramCallVector{
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
