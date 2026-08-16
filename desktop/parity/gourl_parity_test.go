// Cross-language vectors for the path a chi route actually matches on.
//
// Every claim function in `desktop/src-tauri/src/native/` matches on a path
// string, and until #294 that string was the raw request target. Go's router
// never sees that: `net/http` parses the URI into a `url.URL` first, and chi's
// `Mux.routeHTTP` then routes on `RawPath` when it is set and on `Path` when it
// is not — so Go's segment is the *encoded* one for `/api/agents/a%2Db` and the
// *decoded* one for `/api/agents/a%20b`. Which of the two is a property of
// `url.setPath`, not of the request, and it is not guessable by reading either
// package's documentation.
//
// This half asserts Go still produces what the frozen file records; the other
// half lives in desktop/src-tauri/src/native/gourl.rs and asserts Rust produces
// the same. `route_path` is derived by asking chi itself rather than by
// reimplementing its rule here, so the vectors record the router's behavior
// rather than this test's belief about it.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestGoURLVectors -update-gourl-vectors
package parity

import (
	"encoding/json"
	"flag"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"testing"

	"github.com/go-chi/chi/v5"
)

const gourlVectorsFile = "gourl_vectors.json"

var updateGoURLVectors = flag.Bool("update-gourl-vectors", false,
	"rewrite gourl_vectors.json from this Go toolchain")

type routePathCase struct {
	Raw string `json:"raw"`
	// Go's `URL.Path`, or null when `ParseRequestURI` rejected the target.
	//
	// **Context only, never assert on it.** A Go string is arbitrary bytes and
	// `json.Marshal` substitutes U+FFFD for the invalid ones, so the `%ff` row
	// records `/api/agents/�` rather than the byte chi saw. It stays
	// deterministic on both sides — Go compares marshaled bytes and Rust only
	// interpolates this into a failure message — but a Go-side assertion on
	// `Path` would not hold for that row. `route_path` is the field with the
	// answer, and it is ASCII whenever it is non-null.
	Path *string `json:"path"`
	// Go's `URL.RawPath` — empty unless the escaping is non-canonical.
	RawPath *string `json:"raw_path"`
	// What chi routes on, observed from a live router rather than derived.
	RoutePath *string `json:"route_path"`
}

type escapeCase struct {
	Value string `json:"value"`
	Want  string `json:"want"`
}

type gourlVectors struct {
	Comment    []string        `json:"_comment"`
	RoutePath  []routePathCase `json:"route_path"`
	EscapePath []escapeCase    `json:"escape_path"`
	// `url.PathEscape` and `url.QueryEscape` — the other two arms of
	// `shouldEscape`, added with #312 because every integration builds its
	// paths with the first and its queries with the second.
	PathEscape  []escapeCase `json:"path_escape"`
	QueryEscape []escapeCase `json:"query_escape"`
}

// routePathInputs cover each branch of `url.setPath`'s canonical check: a path
// needing no escaping at all, an escape that round-trips (space, non-ASCII,
// `?`), an escape that does not (`%2D` for a byte that needs none, lowercase
// hex), an encoded separator — which is what stops a one-segment route from
// becoming a two-segment miss — and the malformed forms Go answers 400 to.
var routePathInputs = []string{
	"/api/agents/my-agent",
	"/api/agents/a-b",
	"/api/agents/a%2Db",
	"/api/agents/a%2db",
	"/api/agents/a%20b",
	"/api/agents/caf%C3%A9",
	// The lowercase sibling of the row above: same bytes, non-canonical, so chi
	// routes on the raw target instead. It is also the case that proves the
	// canonical check must run on *bytes* — `%ff` decodes to something Rust
	// cannot hold, but the string chi routes on is plain ASCII.
	"/api/agents/caf%c3%a9",
	"/api/agents/%ff",
	"/api/agents/a%2Fb",
	"/api/agents/a%3Fb",
	"/api/agents/a+b",
	"/api/agents/a%2Bb",
	"/api/agents/%7Euser",
	"/api/agents/~user",
	"/api/agents/a%25b",
	"/api/chats/11111111-2222-3333-4444-555555555555",
	"/api/chats/a%20b/messages",
	"/api/tasks/a%20b/run",
	"/api/integrations/a%20b/triggers",
	// The rule's least obvious consequence: canonicality is a property of the
	// **whole** path, so one non-canonical escape anywhere leaves every segment
	// raw — `r%201` included, even though on its own it would have decoded.
	"/api/integrations/a%2Db/triggers/r%201",
	"/api/claude-sessions/a%20b/insights",
	"/api/agents/",
	"/api/agents/a%2",
	"/api/agents/a%",
	"/api/agents/a%zz",
}

// escapePathInputs pin `escape(s, encodePath)` itself, which is the half of the
// rule that decides *whether* the escaping was canonical. Uppercase hex and the
// reserved bytes a path may carry unescaped are both load-bearing: get either
// wrong and every escaped path looks non-canonical, so the decode branch that
// #294 exists to add never runs.
var escapePathInputs = []string{
	"", "a-b", "a b", "café", "a?b", "a/b", "$&+,/:;=@", "-_.~",
	"a%b", "a\tb", "a\"b", "a<b>c", "a#b", "a[b]", "a{b}",
	"~user", "a+b", "日本語", "a\x00b",
}

// segmentAndQueryInputs run through **both** `url.PathEscape` and
// `url.QueryEscape`, so the two vector blocks are the same inputs under the two
// modes and a reader can see where the arms part company at a glance.
//
// The reserved set is spelled out one byte at a time because that is exactly
// where `shouldEscape` branches: a segment escapes `/ ; , ?` and keeps
// `$ & + : = @`, a query escapes all ten. The three inputs after it are the
// bytes an off-the-shelf encoder gets wrong — `~` (Go keeps it, WHATWG form
// encoding escapes it), `*` (the reverse), and a space (`+` in a query,
// `%20` in a segment).
var segmentAndQueryInputs = []string{
	"", "a-b", "a b", "a/b", "a%b", "$", "&", "+", ",", "/", ":", ";", "=", "?", "@",
	"-_.~", "~user", "a*b", "repo:o/r func foo+bar", "café", "日本語",
	"a\tb", "a\nb", "a\"b", "a<b>c", "a#b", "a\x00b",
}

func TestGoURLVectors(t *testing.T) {
	want := gourlVectors{
		Comment: []string{
			"Cross-language parity vectors for the path chi routes on.",
			"Generated from Go, then frozen. Read by desktop/parity/gourl_parity_test.go",
			"(Go) and by desktop/src-tauri/src/native/gourl.rs (Rust). 'route_path' is",
			"observed from a live chi router, so a divergence fails one language against",
			"the router's real behavior rather than against a belief about it.",
			"Regenerate with: go test ./desktop/parity/ -run TestGoURLVectors -update-gourl-vectors",
		},
	}

	for _, raw := range routePathInputs {
		want.RoutePath = append(want.RoutePath, observeRoutePath(raw))
	}
	for _, in := range escapePathInputs {
		// `url.URL{Path: in}.EscapedPath()` is `escape(in, encodePath)` for any
		// input whose default encoding is what we are asking for — which is the
		// whole point, since RawPath is left empty here.
		u := url.URL{Path: in}
		want.EscapePath = append(want.EscapePath, escapeCase{Value: in, Want: u.EscapedPath()})
	}
	for _, in := range segmentAndQueryInputs {
		want.PathEscape = append(want.PathEscape,
			escapeCase{Value: in, Want: url.PathEscape(in)})
		want.QueryEscape = append(want.QueryEscape,
			escapeCase{Value: in, Want: url.QueryEscape(in)})
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateGoURLVectors {
		if err := os.WriteFile(gourlVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", gourlVectorsFile, err)
		}
		t.Logf("wrote %s", gourlVectorsFile)
		return
	}

	frozen, err := os.ReadFile(gourlVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-gourl-vectors): %v", gourlVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this Go toolchain or chi version produces different results.\n"+
			"Regenerate with -update-gourl-vectors and check what moved — the Rust "+
			"port in native/gourl.rs reads the same file and will fail against it.",
			gourlVectorsFile)
	}
}

// observeRoutePath asks a real chi router what string it routed on, rather than
// deriving it from `RawPath != "" ? RawPath : Path` — that expression is chi's
// *current* implementation, and observing it instead means a chi upgrade that
// changes the rule fails this test rather than silently agreeing with a stale
// Rust port.
//
// A root wildcard is the way to read it back: chi matches `/*` against its
// routing path and `URLParam(r, "*")` is the remainder, so `"/" + remainder` is
// the whole string the tree walked — for any target, including the ones no real
// route in the app would match.
func observeRoutePath(raw string) routePathCase {
	out := routePathCase{Raw: raw}

	// What `net/http` does with the target before any handler runs. A parse
	// failure is a 400 from inside the server, so there is no route path at all.
	u, err := url.ParseRequestURI(raw)
	if err != nil {
		return out
	}
	path, rawPath := u.Path, u.RawPath
	out.Path, out.RawPath = &path, &rawPath

	r := chi.NewRouter()
	r.Get("/*", func(w http.ResponseWriter, r *http.Request) {
		routed := "/" + chi.URLParam(r, "*")
		out.RoutePath = &routed
	})

	r.ServeHTTP(httptest.NewRecorder(), httptest.NewRequest(http.MethodGet, raw, nil))
	return out
}
