// Cross-language vectors for the Google integration's MCP server
// (`internal/integrations/google`, ported in #313 to
// `desktop/src-tauri/src/native/integrations/google/`).
//
// Four things have to match byte for byte, none checkable from one language
// alone: which tools are hosted, each advertised schema, the request each tool
// builds, and the result text of every success and every failure. The reasoning
// is in `confluence_parity_test.go`'s header and is not repeated.
//
// # Why this one exists more urgently than the other five
//
// The other five build their requests with `http.NewRequest` and `json.Marshal`,
// so a reviewer can read the bytes off `tools.go` and check the port by eye.
// Google calls the **generated** client libraries (`calendar/v3`, `gmail/v1`,
// `drive/v3`) over an `oauth2` transport, and what those put on the wire is in
// neither this repository nor the port. Without this file the Rust half would be
// a guess about a third party's code generator, and nothing would notice when
// that generator changed. So the recordings here are the specification: the
// resolved URL, the **whole** sorted query (`alt=json&prettyPrint=false` on every
// call, the field masks, `uploadType=multipart`), the `omitempty` bodies, the
// `Authorization` header, the `multipart/related` parts, and every sentence.
//
// # What is pinned as divergence rather than matched
//
//   - `X-Goog-Api-Client` and `User-Agent` embed the *Go toolchain* and *client
//     library* versions. No Rust build can emit `gl-go/…`, and freezing the value
//     would break on a `go.mod` bump. Neither header is recorded.
//   - The `multipart/related` **boundary** is random per request in both
//     languages, so the parts are recorded and the byte stream is not.
//   - A transport failure and a refresh failure carry Go's `*url.Error` text,
//     which embeds the fake's ephemeral port and the resolver's own message. Those
//     cases carry `rust_text`.
//   - **Three nil-dereference panics**, none of which can be recorded in a
//     vector and none of which is a behavior worth reproducing. Two are the
//     handlers': `read_email`/`search_email` do `msg.Payload.Headers` and
//     `view_events` does `ev.Start.DateTime`, both without a nil check. The third
//     is not the handlers' at all and was found by trying to vector it: **a
//     response body of exactly `null` panics every one of the eight tools**,
//     because the generated clients decode into a `**T` — `json.Unmarshal` of
//     `null` into a pointer-to-pointer *nils the pointer*, and the handler then
//     reads a field off it. That is a 200 with a two-word body, which is to say a
//     shape a proxy or a misconfigured gateway produces. The port returns the zero
//     value in all three cases. The `null` case is deliberately **absent** from the
//     vectors below: recording it would mean recording a crash.
//
// # Google-specific things pinned here that no earlier integration could reach
//
//   - **Repeated query parameters.** `MetadataHeaders("Subject","From","Date")`
//     encodes as three `metadataHeaders=` pairs in insertion order under one
//     sorted key. That is why `gourl::Values` stopped being single-valued.
//   - **N+1 requests from one tool call.** `search_email` lists, then fetches each
//     message, **skips** a failed fetch, and reports `len(list.Messages)` — the
//     listed count, not the rendered one. A partial failure therefore produces a
//     sentence whose number does not match its body.
//   - **A sniffed upload content type.** `create_file`'s media part takes its type
//     from `http.DetectContentType` over the content, not from the tool's
//     `mime_type` argument, which reaches the metadata JSON alone.
//   - **An absolute relative-reference.** Drive's upload resolves to
//     `/upload/drive/v3/files`, replacing the base's whole path.
//   - **OAuth2 refresh**, which #318 will share: when it fires, what it sends, and
//     that the refreshed token reaches the very next request.
//
// One value is not deterministic: `view_events`' `time_min` defaults to
// `time.Now()`. That case records the query with the value replaced by
// `«now»`; both languages substitute before comparing, and both assert the
// original parses as a seconds-precision RFC3339 instant in UTC.
//
// The Rust half lives in
// `desktop/src-tauri/src/native/integrations/google/tests_vectors.rs`.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestGoogleVectors -update-google-vectors
package parity

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"mime"
	"mime/multipart"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"regexp"
	"sort"
	"strings"
	"sync"
	"testing"
	"time"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
	"golang.org/x/oauth2"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/integrations/google"
)

const googleVectorsFile = "google_vectors.json"

var updateGoogleVectors = flag.Bool("update-google-vectors", false,
	"rewrite google_vectors.json from this Go toolchain")

const googleParityID = "goog-parity"

// Fixtures, not secrets. The client secret and refresh token are recorded in the
// refresh request body, because that body is a parity surface — and because a
// port that sent them as a `Basic` header instead would otherwise pass.
const (
	googleClientID     = "parity-client-id.apps.googleusercontent.com"
	googleClientSecret = "GOCSPX-parity-client-secret" //nolint:gosec // a test fixture
	googleRefreshToken = "1//parity-refresh-token"     //nolint:gosec // a test fixture
	googleAccessToken  = "ya29.parity-access-token"    //nolint:gosec // a test fixture
)

// The placeholder `time_min` collapses to. Both languages substitute it.
const googleNowPlaceholder = "«now»"

// The placeholder the fake's own base URL collapses to, so a Go sentence that
// embeds it stays stable across runs.
const googleFakeBase = "«api»"

// ─── Vector shapes ───────────────────────────────────────────────────────────

type googleToolVector struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
}

// googlePartVector is one part of a `multipart/related` upload. Recorded instead
// of the raw body because the boundary is random in both languages.
type googlePartVector struct {
	ContentType string `json:"content_type"`
	Body        string `json:"body"`
}

// googleRequestVector is what the fake Google saw.
//
// `Authorization` is recorded in full: it is how a refresh is observed reaching
// the next request, and the token is a fixture.
type googleRequestVector struct {
	Method        string `json:"method"`
	Target        string `json:"target"`
	Authorization string `json:"authorization"`
	ContentType   string `json:"content_type"`
	Body          string `json:"body,omitempty"`
	// Set instead of Body for a `multipart/related` upload.
	Parts []googlePartVector `json:"parts,omitempty"`
}

type googleResponseScript struct {
	Status int    `json:"status"`
	Body   string `json:"body"`
}

type googleCallVector struct {
	Case      string          `json:"case"`
	Tool      string          `json:"tool"`
	Arguments json.RawMessage `json:"arguments"`
	// Consumed in order; the last entry is reused once exhausted, which is what
	// lets one `search_email` case script the list and every fetch behind it.
	Responses []googleResponseScript `json:"responses"`
	Requests  []googleRequestVector  `json:"requests"`
	IsError   bool                   `json:"is_error"`
	Text      string                 `json:"text"`
	// A pinned divergence — see the file header.
	RustText string `json:"rust_text,omitempty"`
	// Go made requests and this port deliberately makes none.
	RustNoRequest bool `json:"rust_no_request,omitempty"`
}

// googleRefreshVector is a call made through a token source in a stated state,
// so the *whole* refresh decision is recorded rather than asserted: whether a
// refresh happened at all, what it sent, and which access token the API call
// that followed carried.
type googleRefreshVector struct {
	Case string `json:"case"`
	// Seconds from now; nil is Go's zero `time.Time`, which never expires.
	ExpiresIn *int64 `json:"expires_in"`
	// The scripted answer from the token endpoint. Absent when no refresh is
	// expected.
	TokenResponse *googleResponseScript `json:"token_response"`
	APIResponse   googleResponseScript  `json:"api_response"`
	// nil when no refresh was made — which is itself the assertion.
	RefreshRequest *googleRequestVector `json:"refresh_request"`
	APIRequest     *googleRequestVector `json:"api_request"`
	IsError        bool                 `json:"is_error"`
	Text           string               `json:"text"`
	RustText       string               `json:"rust_text,omitempty"`
}

type googleHostingVector struct {
	Case     string                          `json:"case"`
	Services map[string]config.ServiceConfig `json:"services"`
	Tools    []string                        `json:"tools"`
}

type googleVectors struct {
	Comment        []string              `json:"_comment"`
	IntegrationID  string                `json:"integration_id"`
	ServerName     string                `json:"server_name"`
	Version        string                `json:"version"`
	ClientID       string                `json:"client_id"`
	ClientSecret   string                `json:"client_secret"`
	RefreshToken   string                `json:"refresh_token"`
	AccessToken    string                `json:"access_token"`
	NowPlaceholder string                `json:"now_placeholder"`
	Tools          []googleToolVector    `json:"tools"`
	Hosting        []googleHostingVector `json:"hosting"`
	Calls          []googleCallVector    `json:"calls"`
	Refreshes      []googleRefreshVector `json:"refreshes"`
}

// ─── The fake Google ─────────────────────────────────────────────────────────

// fakeGoogle answers both the API bases and the token endpoint, recording each
// separately: a refresh is the one request whose *absence* is the assertion.
type fakeGoogle struct {
	mu sync.Mutex

	api       []googleResponseScript
	apiSeen   []googleRequestVector
	token     *googleResponseScript
	tokenSeen *googleRequestVector

	// Replaces the fake's own base URL in a recorded sentence.
	baseURL string
}

func (f *fakeGoogle) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	recorded := recordGoogleRequest(r)

	f.mu.Lock()
	var script googleResponseScript
	if r.URL.Path == "/token" {
		f.tokenSeen = &recorded
		if f.token != nil {
			script = *f.token
		} else {
			// No refresh was scripted, so one arriving is a failure the vector
			// should show rather than hide.
			script = googleResponseScript{Status: http.StatusInternalServerError,
				Body: `{"error":"unexpected refresh"}`}
		}
	} else {
		f.apiSeen = append(f.apiSeen, recorded)
		switch {
		case len(f.api) == 0:
			script = googleResponseScript{Status: http.StatusInternalServerError,
				Body: `{"error":"no response scripted"}`}
		case len(f.api) == 1:
			script = f.api[0] // the last entry is reused
		default:
			script, f.api = f.api[0], f.api[1:]
		}
	}
	f.mu.Unlock()

	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(script.Status)
	_, _ = w.Write([]byte(script.Body))
}

// recordGoogleRequest captures exactly the fields the port must reproduce —
// deliberately not the version-stamped headers, which it cannot.
func recordGoogleRequest(r *http.Request) googleRequestVector {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		body = nil
	}
	contentType := r.Header.Get("Content-Type")
	recorded := googleRequestVector{
		Method:        r.Method,
		Target:        r.URL.RequestURI(),
		Authorization: r.Header.Get("Authorization"),
		ContentType:   contentType,
	}

	mediaType, params, mimeErr := mime.ParseMediaType(contentType)
	if mimeErr == nil && strings.HasPrefix(mediaType, "multipart/") {
		// The boundary is random, so record the media type without it and the
		// parts instead of the byte stream.
		recorded.ContentType = mediaType
		reader := multipart.NewReader(strings.NewReader(string(body)), params["boundary"])
		for {
			part, partErr := reader.NextPart()
			if partErr != nil {
				break
			}
			partBody, _ := io.ReadAll(part)
			recorded.Parts = append(recorded.Parts, googlePartVector{
				ContentType: part.Header.Get("Content-Type"),
				Body:        string(partBody),
			})
			_ = part.Close()
		}
		return recorded
	}

	recorded.Body = string(body)
	return recorded
}

func (f *fakeGoogle) armAPI(scripts []googleResponseScript) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.api = append([]googleResponseScript(nil), scripts...)
	f.apiSeen = nil
}

func (f *fakeGoogle) armToken(script *googleResponseScript) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.token = script
	f.tokenSeen = nil
}

func (f *fakeGoogle) recordedAPI() []googleRequestVector {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.apiSeen
}

func (f *fakeGoogle) recordedToken() *googleRequestVector {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.tokenSeen
}

// ─── Standing the real server up ─────────────────────────────────────────────

// googleSession starts the real integration against the fake and returns a
// client session. expiresIn is the stored token's lifetime; nil is Go's zero
// `time.Time`, which never expires.
func googleSession(
	t *testing.T, ctx context.Context,
	services map[string]config.ServiceConfig, expiresIn *int64,
) *mcp.ClientSession {
	t.Helper()

	credentials, err := json.Marshal(config.GoogleCredentials{
		ClientID:     googleClientID,
		ClientSecret: googleClientSecret,
	})
	if err != nil {
		t.Fatalf("encoding credentials: %v", err)
	}

	// An `oauth2.Token` rather than a hand-built map, because `ParseOAuthToken`
	// decodes into one: a zero `Expiry` is what "no expiry" *is*, and it
	// round-trips through this type and no other.
	token := oauth2.Token{
		AccessToken:  googleAccessToken,
		RefreshToken: googleRefreshToken,
		TokenType:    "Bearer",
	}
	if expiresIn != nil {
		token.Expiry = time.Now().Add(time.Duration(*expiresIn) * time.Second)
	}
	encodedAuth, err := json.Marshal(&token)
	if err != nil {
		t.Fatalf("encoding auth: %v", err)
	}

	cfg := &config.IntegrationConfig{
		ID:          googleParityID,
		Name:        "Google (parity)",
		Type:        "google",
		Enabled:     true,
		Credentials: credentials,
		Auth:        encodedAuth,
		Services:    services,
	}

	serverCfg, err := google.Start(ctx, cfg)
	if err != nil {
		t.Fatalf("starting the google integration: %v", err)
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

func googleAllServices() map[string]config.ServiceConfig {
	return map[string]config.ServiceConfig{
		"calendar": {Enabled: true},
		"gmail":    {Enabled: true},
		"drive":    {Enabled: true},
	}
}

// googleHour is the ordinary case: a token that will not expire during the run.
func googleHour() *int64 {
	hour := int64(3600)
	return &hour
}

var googleHostingCases = []googleHostingVector{
	{Case: "every service enabled with no tool list — an empty allowed set hosts everything",
		Services: googleAllServices()},
	{Case: "no services at all", Services: map[string]config.ServiceConfig{}},
	{Case: "one service enabled hosts only its tools",
		Services: map[string]config.ServiceConfig{"calendar": {Enabled: true}}},
	{Case: "a disabled service contributes neither its gate nor its names",
		Services: map[string]config.ServiceConfig{
			"calendar": {Enabled: true},
			"gmail":    {Enabled: true},
			"drive":    {Enabled: false, Tools: []string{"create_event"}},
		}},
	{
		// The union half, which only Google can exercise: naming one Gmail tool
		// silences Calendar and Drive too.
		Case: "one tool named under one service narrows every enabled service",
		Services: map[string]config.ServiceConfig{
			"calendar": {Enabled: true},
			"gmail":    {Enabled: true, Tools: []string{"send_email"}},
			"drive":    {Enabled: true},
		},
	},
	{
		// …and a name contributed by one service admits the tool wherever it is
		// registered, across service boundaries.
		Case: "a name contributed by one service admits a tool belonging to another",
		Services: map[string]config.ServiceConfig{
			"calendar": {Enabled: true},
			"gmail":    {Enabled: true, Tools: []string{"send_email", "list_files"}},
			"drive":    {Enabled: true},
		},
	},
	{Case: "an enabled but unknown service still narrows the ones that exist",
		Services: map[string]config.ServiceConfig{
			"calendar": {Enabled: true},
			"other":    {Enabled: true, Tools: []string{"view_events"}},
		}},
	{Case: "a tool named by a disabled service is not allowed anywhere",
		Services: map[string]config.ServiceConfig{
			"drive":    {Enabled: true, Tools: []string{"list_files"}},
			"calendar": {Enabled: false, Tools: []string{"create_event"}},
		}},
}

// ─── The call cases ──────────────────────────────────────────────────────────

type googleCallCase struct {
	name      string
	tool      string
	args      map[string]any
	scripts   []googleResponseScript
	rustText  string
	rustNoReq bool
}

func googleOK(body string) []googleResponseScript {
	return []googleResponseScript{{Status: http.StatusOK, Body: body}}
}

//nolint:funlen // one flat table; splitting it would hide what is covered
func googleCallCases() []googleCallCase {
	const createdEvent = `{"id":"ev1","summary":"Standup","htmlLink":"https://cal/ev1"}`

	return []googleCallCase{
		// ─── create_event ────────────────────────────────────────────────────
		{
			// Every key present, and `json.Marshal`'s HTML escaping in the
			// summary — `<`, which `serde_json` does not do.
			name: "create_event/a full event, with encoding/json's HTML escaping",
			tool: "create_event",
			args: map[string]any{
				"summary": "Standup <daily> & sync", "start": "2026-03-01T10:00:00-07:00",
				"end": "2026-03-01T10:30:00-07:00", "description": "notes & more",
			},
			scripts: googleOK(createdEvent),
		},
		{
			// `calendar.Event`'s fields carry `omitempty`, so this sends no
			// `description` key at all.
			name: "create_event/an empty description sends no key",
			tool: "create_event",
			args: map[string]any{
				"summary": "Standup", "start": "2026-03-01T10:00:00Z",
				"end": "2026-03-01T10:30:00Z", "description": "",
			},
			scripts: googleOK(createdEvent),
		},
		{
			// The result reads the **response**, so a server that renamed the
			// event is what the model is told.
			name: "create_event/the result reads the response and not the request",
			tool: "create_event",
			args: map[string]any{
				"summary": "sent", "start": "2026-03-01T10:00:00Z",
				"end": "2026-03-01T10:30:00Z", "description": "",
			},
			scripts: googleOK(`{"id":"ev2","summary":"renamed by the server","htmlLink":"https://cal/ev2"}`),
		},
		{
			name: "create_event/null fields decode as zero values",
			tool: "create_event",
			args: map[string]any{
				"summary": "s", "start": "2026-03-01T10:00:00Z",
				"end": "2026-03-01T10:30:00Z", "description": "",
			},
			scripts: googleOK(`{"id":null,"summary":null,"htmlLink":null}`),
		},
		{
			// serde would build the struct from a sequence positionally without
			// `GoStruct`, turning this into a success.
			name: "create_event/a JSON array is not a struct",
			tool: "create_event",
			args: map[string]any{
				"summary": "s", "start": "2026-03-01T10:00:00Z",
				"end": "2026-03-01T10:30:00Z", "description": "",
			},
			scripts:  googleOK(`[true]`),
			rustText: "creating calendar event: decoding response: invalid type: sequence, expected a JSON object at line 1 column 0",
		},

		// ─── googleapi.Error, all four shapes ────────────────────────────────
		{
			name: "create_event/an error with one reason",
			tool: "create_event",
			args: map[string]any{
				"summary": "s", "start": "2026-03-01T10:00:00Z",
				"end": "2026-03-01T10:30:00Z", "description": "",
			},
			scripts: []googleResponseScript{{Status: http.StatusForbidden,
				Body: `{"error":{"code":403,"message":"Insufficient Permission","errors":[{"message":"Insufficient Permission","domain":"global","reason":"insufficientPermissions"}]}}`}},
		},
		{
			name: "create_event/an error with no errors array",
			tool: "create_event",
			args: map[string]any{
				"summary": "s", "start": "2026-03-01T10:00:00Z",
				"end": "2026-03-01T10:30:00Z", "description": "",
			},
			scripts: []googleResponseScript{{Status: http.StatusNotFound,
				Body: `{"error":{"code":404,"message":"Not Found"}}`}},
		},
		{
			name: "create_event/an error with several reasons",
			tool: "create_event",
			args: map[string]any{
				"summary": "s", "start": "2026-03-01T10:00:00Z",
				"end": "2026-03-01T10:30:00Z", "description": "",
			},
			scripts: []googleResponseScript{{Status: http.StatusBadRequest,
				Body: `{"error":{"code":400,"message":"Bad Request","errors":[{"message":"m1","reason":"r1"},{"message":"m2","reason":"r2"}]}}`}},
		},
		{
			// An `error` that is a **string** is not a Google error document, so
			// it falls through to the raw form.
			name: "create_event/a body that is not a Google error document",
			tool: "create_event",
			args: map[string]any{
				"summary": "s", "start": "2026-03-01T10:00:00Z",
				"end": "2026-03-01T10:30:00Z", "description": "",
			},
			scripts: []googleResponseScript{{Status: http.StatusForbidden,
				Body: `{"error":"invalid_grant","error_description":"Token has been expired or revoked."}`}},
		},
		{
			name: "create_event/an error body that is not JSON at all",
			tool: "create_event",
			args: map[string]any{
				"summary": "s", "start": "2026-03-01T10:00:00Z",
				"end": "2026-03-01T10:30:00Z", "description": "",
			},
			scripts: []googleResponseScript{{Status: http.StatusBadGateway,
				Body: `<html>nope</html>`}},
		},

		// ─── view_events ─────────────────────────────────────────────────────
		{
			name: "view_events/an explicit range, with both bounds",
			tool: "view_events",
			args: map[string]any{
				"time_min": "2026-03-01T00:00:00Z", "time_max": "2026-03-08T00:00:00Z",
				"max_results": 25,
			},
			scripts: googleOK(`{"items":[{"id":"e1","summary":"One","htmlLink":"https://cal/e1","start":{"dateTime":"2026-03-02T09:00:00Z"}}]}`),
		},
		{
			// The non-deterministic one: `time_min` defaults to `time.Now()`,
			// recorded as «now».
			name:    "view_events/an empty time_min defaults to now and no time_max key is sent",
			tool:    "view_events",
			args:    map[string]any{"time_min": "", "time_max": "", "max_results": 0},
			scripts: googleOK(`{"items":[]}`),
		},
		{
			name: "view_events/max_results above the maximum falls back to the default",
			tool: "view_events",
			args: map[string]any{
				"time_min": "2026-03-01T00:00:00Z", "time_max": "", "max_results": 101,
			},
			scripts: googleOK(`{"items":[]}`),
		},
		{
			name: "view_events/the maximum itself is sent",
			tool: "view_events",
			args: map[string]any{
				"time_min": "2026-03-01T00:00:00Z", "time_max": "", "max_results": 100,
			},
			scripts: googleOK(`{"items":[]}`),
		},
		{
			// An all-day event has `date` and no `dateTime`.
			name: "view_events/an all-day event falls back to date",
			tool: "view_events",
			args: map[string]any{
				"time_min": "2026-03-01T00:00:00Z", "time_max": "", "max_results": 10,
			},
			scripts: googleOK(`{"items":[{"id":"e2","summary":"Holiday","htmlLink":"https://cal/e2","start":{"date":"2026-03-03"}},{"id":"e3","summary":"Timed","htmlLink":"https://cal/e3","start":{"dateTime":"2026-03-04T09:00:00Z","date":"ignored"}}]}`),
		},
		{
			name: "view_events/no items at all",
			tool: "view_events",
			args: map[string]any{
				"time_min": "2026-03-01T00:00:00Z", "time_max": "", "max_results": 10,
			},
			scripts: googleOK(`{"items":null}`),
		},

		// ─── send_email ──────────────────────────────────────────────────────
		{
			// The RFC822 message is built by the handler and base64url-encoded
			// **with padding**.
			name: "send_email/the RFC822 message is padded base64url in the body",
			tool: "send_email",
			args: map[string]any{
				"to": "alice@example.com, bob@example.com", "subject": "Hello & <hi>",
				"body": "Line one\nLine two",
			},
			scripts: googleOK(`{"id":"m1"}`),
		},
		{
			name:    "send_email/an empty body still produces a message",
			tool:    "send_email",
			args:    map[string]any{"to": "a@b", "subject": "", "body": ""},
			scripts: googleOK(`{"id":"m2"}`),
		},
		{
			name:    "send_email/non-ASCII is encoded as UTF-8 bytes before base64",
			tool:    "send_email",
			args:    map[string]any{"to": "a@b", "subject": "naïve — ☕", "body": "héllo"},
			scripts: googleOK(`{"id":"m3"}`),
		},

		// ─── read_email ──────────────────────────────────────────────────────
		{
			name: "read_email/headers and a text/plain body",
			tool: "read_email",
			args: map[string]any{"message_id": "18c0f0a1b2c3d4e5"},
			// `aGVsbG8gdGhlcmU=` is "hello there", padded.
			scripts: googleOK(`{"id":"18c0f0a1b2c3d4e5","payload":{"mimeType":"text/plain","body":{"data":"aGVsbG8gdGhlcmU="},"headers":[{"name":"Subject","value":"Re: lunch"},{"name":"From","value":"alice@example.com"},{"name":"Date","value":"Mon, 2 Mar 2026 09:00:00 +0000"},{"name":"X-Other","value":"ignored"}]}}`),
		},
		{
			// A later header of the same name wins, because Go's switch assigns
			// on every iteration.
			name:    "read_email/a later header of the same name wins",
			tool:    "read_email",
			args:    map[string]any{"message_id": "m"},
			scripts: googleOK(`{"payload":{"headers":[{"name":"Subject","value":"first"},{"name":"Subject","value":"second"}]}}`),
		},
		{
			// Unpadded base64url **fails** `URLEncoding` and falls through to
			// the next part rather than erroring.
			name:    "read_email/an unpadded part is skipped and the next one is used",
			tool:    "read_email",
			args:    map[string]any{"message_id": "m"},
			scripts: googleOK(`{"payload":{"mimeType":"multipart/mixed","headers":[],"parts":[{"mimeType":"text/plain","body":{"data":"aGk"}},{"mimeType":"text/plain","body":{"data":"Ynll"}}]}}`),
		},
		{
			name:    "read_email/only text/plain is decoded, depth first",
			tool:    "read_email",
			args:    map[string]any{"message_id": "m"},
			scripts: googleOK(`{"payload":{"mimeType":"multipart/alternative","headers":[],"parts":[{"mimeType":"text/html","body":{"data":"PGI-PC9iPg=="}},{"mimeType":"text/plain","body":{"data":"aGk="}}]}}`),
		},
		{
			// The id is a **path** parameter, escaped per segment.
			name:    "read_email/a message id needing escaping",
			tool:    "read_email",
			args:    map[string]any{"message_id": "a/b c?d&e"},
			scripts: googleOK(`{"payload":{"headers":[]}}`),
		},
		{
			name: "read_email/a 404 quotes the id with %q",
			tool: "read_email",
			args: map[string]any{"message_id": "missing\"quoted"},
			scripts: []googleResponseScript{{Status: http.StatusNotFound,
				Body: `{"error":{"code":404,"message":"Requested entity was not found."}}`}},
		},

		// ─── search_email ────────────────────────────────────────────────────
		{
			// The N+1: one list, then one fetch per message, each carrying three
			// `metadataHeaders` values under one sorted key.
			name: "search_email/one list and one fetch per message, with repeated query keys",
			tool: "search_email",
			args: map[string]any{"query": "from:alice@example.com is:unread", "max_results": 2},
			scripts: []googleResponseScript{
				{Status: http.StatusOK, Body: `{"messages":[{"id":"m1"},{"id":"m2"}]}`},
				{Status: http.StatusOK, Body: `{"payload":{"headers":[{"name":"Subject","value":"One"},{"name":"From","value":"a@b"},{"name":"Date","value":"D1"}]}}`},
				{Status: http.StatusOK, Body: `{"payload":{"headers":[{"name":"Subject","value":"Two"},{"name":"From","value":"c@d"},{"name":"Date","value":"D2"}]}}`},
			},
		},
		{
			// The count is `len(list.Messages)` — the **listed** total. A skipped
			// fetch leaves the sentence's number disagreeing with its body.
			name: "search_email/a failed fetch is skipped and the count still counts it",
			tool: "search_email",
			args: map[string]any{"query": "x", "max_results": 3},
			scripts: []googleResponseScript{
				{Status: http.StatusOK, Body: `{"messages":[{"id":"m1"},{"id":"m2"},{"id":"m3"}]}`},
				{Status: http.StatusOK, Body: `{"payload":{"headers":[{"name":"Subject","value":"One"}]}}`},
				{Status: http.StatusForbidden, Body: `{"error":{"code":403,"message":"nope"}}`},
				{Status: http.StatusOK, Body: `{"payload":{"headers":[{"name":"Subject","value":"Three"}]}}`},
			},
		},
		{
			// Unlike Drive's, Gmail's `q` is unconditional: an empty search still
			// sends `q=`.
			name:    "search_email/an empty query still sends the q key",
			tool:    "search_email",
			args:    map[string]any{"query": "", "max_results": 0},
			scripts: googleOK(`{"messages":[]}`),
		},
		{
			// 50 here, not 100 — every clamp in this integration differs.
			name:    "search_email/max_results above 50 falls back to the default",
			tool:    "search_email",
			args:    map[string]any{"query": "x", "max_results": 51},
			scripts: googleOK(`{"messages":null}`),
		},
		{
			name:    "search_email/50 itself is sent",
			tool:    "search_email",
			args:    map[string]any{"query": "x", "max_results": 50},
			scripts: googleOK(`{"messages":[]}`),
		},

		// ─── list_files ──────────────────────────────────────────────────────
		{
			name:    "list_files/a query is sent when set",
			tool:    "list_files",
			args:    map[string]any{"query": "name contains 'report' and trashed = false", "max_results": 3},
			scripts: googleOK(`{"files":[{"id":"f1","name":"Report.pdf","mimeType":"application/pdf","modifiedTime":"2026-03-01T09:00:00Z","webViewLink":"https://drive/f1"}]}`),
		},
		{
			// Conditional, unlike Gmail's: an empty Drive query sends no key.
			name:    "list_files/an empty query sends no q key at all",
			tool:    "list_files",
			args:    map[string]any{"query": "", "max_results": 0},
			scripts: googleOK(`{"files":[]}`),
		},
		{
			name:    "list_files/max_results above the maximum falls back to the default",
			tool:    "list_files",
			args:    map[string]any{"query": "", "max_results": 101},
			scripts: googleOK(`{"files":null}`),
		},
		{
			name:    "list_files/several files are joined by a rule",
			tool:    "list_files",
			args:    map[string]any{"query": "", "max_results": 2},
			scripts: googleOK(`{"files":[{"id":"f1","name":"A","mimeType":"text/plain","modifiedTime":"t1","webViewLink":"l1"},{"id":"f2","name":"B","mimeType":null,"modifiedTime":null,"webViewLink":null}]}`),
		},

		// ─── create_file ─────────────────────────────────────────────────────
		{
			// The upload path is **absolute**, so it replaces the base's whole
			// path; and the media part's type is sniffed rather than taken from
			// `mime_type`, which reaches only the metadata JSON.
			name: "create_file/the media type is sniffed and the mime_type argument is metadata only",
			tool: "create_file",
			args: map[string]any{
				"name": "notes.md", "content": "# Notes\n\nplain text",
				"mime_type": "text/markdown",
			},
			scripts: googleOK(`{"id":"f9","name":"notes.md","webViewLink":"https://drive/f9"}`),
		},
		{
			name:    "create_file/an empty mime_type defaults to text/plain in the metadata",
			tool:    "create_file",
			args:    map[string]any{"name": "a.txt", "content": "hello", "mime_type": ""},
			scripts: googleOK(`{"id":"f10","name":"a.txt","webViewLink":"https://drive/f10"}`),
		},
		{
			// HTML content sniffs as `text/html`, whatever `mime_type` said.
			name: "create_file/HTML content sniffs as text/html regardless of mime_type",
			tool: "create_file",
			args: map[string]any{
				"name": "page.html", "content": "<html><body>hi</body></html>",
				"mime_type": "application/json",
			},
			scripts: googleOK(`{"id":"f11","name":"page.html","webViewLink":"https://drive/f11"}`),
		},
		{
			name:    "create_file/an empty content still uploads a media part",
			tool:    "create_file",
			args:    map[string]any{"name": "empty", "content": "", "mime_type": ""},
			scripts: googleOK(`{"id":"f12","name":"empty","webViewLink":"https://drive/f12"}`),
		},

		// ─── download_file ───────────────────────────────────────────────────
		{
			// The only call in the integration that is not `alt=json`, and the
			// only result that is the response body verbatim.
			name:    "download_file/the body is returned verbatim",
			tool:    "download_file",
			args:    map[string]any{"file_id": "f1"},
			scripts: googleOK("line one\nline two\n"),
		},
		{
			name:    "download_file/a file id needing escaping",
			tool:    "download_file",
			args:    map[string]any{"file_id": "a/b c?d"},
			scripts: googleOK("x"),
		},
		{
			name:    "download_file/an empty file is an empty result",
			tool:    "download_file",
			args:    map[string]any{"file_id": "f2"},
			scripts: googleOK(""),
		},
		{
			name: "download_file/a 403 quotes the id with %q",
			tool: "download_file",
			args: map[string]any{"file_id": "f3"},
			scripts: []googleResponseScript{{Status: http.StatusForbidden,
				Body: `{"error":{"code":403,"message":"Insufficient Permission","errors":[{"reason":"insufficientPermissions","message":"Insufficient Permission"}]}}`}},
		},

		// ─── a zero-fraction float for an integer field ───────────────────────
		{
			name:      "view_events/a zero-fraction float is an integer to Go and not to serde",
			tool:      "view_events",
			args:      map[string]any{"time_min": "2026-03-01T00:00:00Z", "time_max": "", "max_results": json.RawMessage("10.0")},
			scripts:   googleOK(`{"items":[]}`),
			rustText:  "failed to deserialize parameters: invalid type: floating point `10.0`, expected i64",
			rustNoReq: true,
		},
	}
}

// ─── The refresh cases ───────────────────────────────────────────────────────

type googleRefreshCase struct {
	name          string
	expiresIn     *int64
	tokenResponse *googleResponseScript
	apiResponse   googleResponseScript
	rustText      string
}

func googleSeconds(n int64) *int64 { return &n }

func googleRefreshCases() []googleRefreshCase {
	const listed = `{"files":[]}`
	ok := googleResponseScript{Status: http.StatusOK, Body: listed}

	return []googleRefreshCase{
		{
			// An hour out: no refresh, and the stored access token is what the
			// API sees.
			name:        "a token an hour from expiry is reused without a refresh",
			expiresIn:   googleSeconds(3600),
			apiResponse: ok,
		},
		{
			// Go's zero `time.Time` never expires — which is what an integration
			// authenticated before the expiry was stored looks like.
			name:        "a token with no expiry never refreshes",
			expiresIn:   nil,
			apiResponse: ok,
		},
		{
			// Inside the 10-second `expiryDelta`, so it refreshes even though it
			// has not expired.
			name:      "a token five seconds from expiry refreshes inside the delta",
			expiresIn: googleSeconds(5),
			tokenResponse: &googleResponseScript{Status: http.StatusOK,
				Body: `{"access_token":"ya29.refreshed","token_type":"Bearer","expires_in":3599}`},
			apiResponse: ok,
		},
		{
			// A response with no `refresh_token` keeps the old one — not
			// observable in one request, but the next refresh would send it.
			name:      "an expired token refreshes and the new access token reaches the request",
			expiresIn: googleSeconds(-60),
			tokenResponse: &googleResponseScript{Status: http.StatusOK,
				Body: `{"access_token":"ya29.refreshed-2","token_type":"Bearer","expires_in":3599}`},
			apiResponse: ok,
		},
		{
			// The failure surfaces as the **tool's** failure, because `oauth2`'s
			// transport returns it from `RoundTrip`. Go's text is an
			// `*url.Error` around `oauth2`'s own; the port's is `oauth2`'s alone.
			name:      "a refused refresh surfaces as the tool's own failure",
			expiresIn: googleSeconds(-60),
			tokenResponse: &googleResponseScript{Status: http.StatusBadRequest,
				Body: `{"error":"invalid_grant","error_description":"Token has been expired or revoked."}`},
			apiResponse: ok,
			// Only the `*url.Error` wrapper diverges. `RetrieveError.Error()`
			// has two forms and this is the *other* one — an `error` code in
			// the body wins over the status line — which the port now
			// reproduces because this recording is what revealed it.
			rustText: `listing drive files: oauth2: "invalid_grant" "Token has been expired or revoked."`,
		},
	}
}

// ─── The generator ───────────────────────────────────────────────────────────

func TestGoogleVectors(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()

	fake := &fakeGoogle{}
	srv := httptest.NewServer(fake)
	defer srv.Close()
	fake.baseURL = srv.URL
	restore := google.SetEndpoints(srv.URL, srv.URL+"/token")
	defer restore()

	want := googleVectors{
		Comment: []string{
			"Cross-language parity vectors for internal/integrations/google, the Google",
			"integration's in-process MCP server. Generated from Go, then frozen. Read by",
			"desktop/parity/google_parity_test.go (Go) and by",
			"desktop/src-tauri/src/native/integrations/google/ (Rust).",
			"Unlike the other five integrations, Google's requests are built by generated",
			"client libraries rather than by the handlers, so these recordings are the only",
			"written specification of what goes on the wire — see the test file's header.",
			"'tools' and 'hosting' come from live tools/list calls against servers built by",
			"google.Start; 'calls' and 'refreshes' from live tools/call calls against a",
			"scripted fake Google that records every request each tool built.",
			"The client secret, refresh token and access token are fixtures, not secrets:",
			"they are recorded because the refresh request body is a parity surface.",
			"X-Goog-Api-Client and User-Agent are deliberately not recorded (they embed the",
			"Go toolchain version) and neither is the random multipart boundary.",
			"Regenerate with: go test ./desktop/parity/ -run TestGoogleVectors -update-google-vectors",
		},
		IntegrationID:  googleParityID,
		ServerName:     fmt.Sprintf("google-%s", googleParityID),
		Version:        "1.0.0",
		ClientID:       googleClientID,
		ClientSecret:   googleClientSecret,
		RefreshToken:   googleRefreshToken,
		AccessToken:    googleAccessToken,
		NowPlaceholder: googleNowPlaceholder,
	}

	session := googleSession(t, ctx, googleAllServices(), googleHour())
	listed, err := session.ListTools(ctx, nil)
	if err != nil {
		t.Fatalf("tools/list: %v", err)
	}
	for _, tool := range listed.Tools {
		schema, err := json.Marshal(tool.InputSchema)
		if err != nil {
			t.Fatalf("encoding %s's input schema: %v", tool.Name, err)
		}
		want.Tools = append(want.Tools, googleToolVector{
			Name:        tool.Name,
			Description: tool.Description,
			InputSchema: schema,
		})
	}

	for _, hosting := range googleHostingCases {
		hosting.Tools = googleHostedTools(t, ctx, hosting.Services)
		want.Hosting = append(want.Hosting, hosting)
	}

	for _, c := range googleCallCases() {
		want.Calls = append(want.Calls, runGoogleCallCase(t, ctx, session, fake, c))
	}

	for _, c := range googleRefreshCases() {
		want.Refreshes = append(want.Refreshes, runGoogleRefreshCase(t, ctx, fake, c))
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateGoogleVectors {
		if err := os.WriteFile(googleVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", googleVectorsFile, err)
		}
		t.Logf("wrote %s", googleVectorsFile)
		return
	}

	frozen, err := os.ReadFile(googleVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-google-vectors): %v",
			googleVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this Go toolchain produces different results.\n"+
			"Regenerate with -update-google-vectors and check what moved — the Rust port "+
			"in native/integrations/google/ reads the same file and will fail against it. "+
			"A moved tool name, schema, request or sentence is not a cosmetic diff: the "+
			"names are in every agent's stored allowlist and in every tool_use block "+
			"already written, and the sentences are what the model reads. Note that a "+
			"generated-client or google.golang.org/api bump can move a query or a header "+
			"without anything in this repository changing.",
			googleVectorsFile)
	}
}

func googleHostedTools(
	t *testing.T, ctx context.Context, services map[string]config.ServiceConfig,
) []string {
	t.Helper()
	listed, err := googleSession(t, ctx, services, googleHour()).ListTools(ctx, nil)
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

func runGoogleCallCase(
	t *testing.T, ctx context.Context,
	session *mcp.ClientSession, fake *fakeGoogle, c googleCallCase,
) googleCallVector {
	t.Helper()

	fake.armAPI(c.scripts)
	// No refresh is expected for these: the shared session's token is an hour
	// out, so one arriving would be a defect the vector shows.
	fake.armToken(nil)

	args := jsonArgs(t, c.args)
	result, err := session.CallTool(ctx, &mcp.CallToolParams{Name: c.tool, Arguments: args})
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
	if refresh := fake.recordedToken(); refresh != nil {
		t.Fatalf("%s: an hour-valid token refreshed, which it must not", c.name)
	}

	requests := fake.recordedAPI()
	for i := range requests {
		requests[i].Target = redactGoogleNow(t, c.name, requests[i].Target)
	}

	// A result must never carry the client secret or the refresh token, in
	// either language.
	if strings.Contains(text.Text, googleClientSecret) ||
		strings.Contains(text.Text, googleRefreshToken) {
		t.Fatalf("%s: a credential leaked into a tool result: %s", c.name, text.Text)
	}

	return googleCallVector{
		Case:          c.name,
		Tool:          c.tool,
		Arguments:     args,
		Responses:     c.scripts,
		Requests:      requests,
		IsError:       result.IsError,
		Text:          redactGoogleBase(fake, text.Text),
		RustText:      c.rustText,
		RustNoRequest: c.rustNoReq,
	}
}

func runGoogleRefreshCase(
	t *testing.T, ctx context.Context, fake *fakeGoogle, c googleRefreshCase,
) googleRefreshVector {
	t.Helper()

	fake.armAPI([]googleResponseScript{c.apiResponse})
	fake.armToken(c.tokenResponse)

	// `list_files` with no arguments: the shortest call that reaches the
	// transport, so the vector is about the token and nothing else.
	session := googleSession(t, ctx,
		map[string]config.ServiceConfig{"drive": {Enabled: true, Tools: []string{"list_files"}}},
		c.expiresIn)
	args := jsonArgs(t, map[string]any{"query": "", "max_results": 1})
	result, err := session.CallTool(ctx, &mcp.CallToolParams{Name: "list_files", Arguments: args})
	if err != nil {
		t.Fatalf("%s: tools/call: %v", c.name, err)
	}
	text, ok := result.Content[0].(*mcp.TextContent)
	if !ok {
		t.Fatalf("%s: want text content, got %T", c.name, result.Content[0])
	}

	var apiRequest *googleRequestVector
	if seen := fake.recordedAPI(); len(seen) > 0 {
		apiRequest = &seen[0]
	}

	return googleRefreshVector{
		Case:           c.name,
		ExpiresIn:      c.expiresIn,
		TokenResponse:  c.tokenResponse,
		APIResponse:    c.apiResponse,
		RefreshRequest: fake.recordedToken(),
		APIRequest:     apiRequest,
		IsError:        result.IsError,
		Text:           redactGoogleBase(fake, text.Text),
		RustText:       c.rustText,
	}
}

// googleNowPattern is `time.RFC3339` at seconds precision in UTC — the one shape
// `view_events`' default `time_min` can take.
var googleNowPattern = regexp.MustCompile(`^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$`)

// googleTimeMinPattern finds the **raw** `timeMin` pair in a recorded target.
var googleTimeMinPattern = regexp.MustCompile(`timeMin=([^&]*)`)

// redactGoogleNow replaces a `timeMin` that came from `time.Now()` with the
// placeholder, after checking it is the shape the port must produce. Anything
// else — including an explicit `time_min` the caller passed — is left alone.
//
// The substitution is over the **raw** target rather than a decoded-and-
// re-encoded one, so every other byte of the query is what the wire carried:
// re-encoding would make this function, not the generated client, the thing the
// vectors record.
func redactGoogleNow(t *testing.T, caseName, target string) string {
	t.Helper()
	match := googleTimeMinPattern.FindStringSubmatchIndex(target)
	if match == nil {
		return target
	}
	raw := target[match[2]:match[3]]
	decoded, err := url.QueryUnescape(raw)
	if err != nil {
		t.Fatalf("%s: undecodable timeMin %q in %q", caseName, raw, target)
	}
	if !googleNowPattern.MatchString(decoded) {
		return target
	}
	// An explicit RFC3339 argument matches the pattern too, so only replace one
	// that is actually close to now.
	instant, err := time.Parse(time.RFC3339, decoded)
	if err != nil || time.Since(instant).Abs() > time.Hour {
		return target
	}
	return target[:match[2]] + url.QueryEscape(googleNowPlaceholder) + target[match[3]:]
}

// redactGoogleBase collapses the fake's ephemeral base URL, which appears in
// Go's `*url.Error` text and would otherwise change on every run.
func redactGoogleBase(fake *fakeGoogle, text string) string {
	if fake.baseURL == "" {
		return text
	}
	return strings.ReplaceAll(text, fake.baseURL, googleFakeBase)
}
