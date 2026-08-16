// The write surface, enumerated from the router itself and classified.
//
// #293 split the ~50 write routes into "moved" and "deferred" and accounted for
// the deferrals **by category** — scheduler, chat execution, integrations,
// scan. That is readable and it is not auditable: nothing said whether the
// categories covered every route, and #296 found two that escaped all of them.
// A prose table has the same problem one release later.
//
// So the table is generated. `chi.Walk` over the real router is the source of
// truth for *what exists*, `dispositions` below is the source of truth for
// *what each one should do*, and the two are cross-checked in both directions:
//
//   - a write route the router has and this file does not classify **fails
//     here**, so a new one cannot be added without someone deciding about it;
//   - a classification naming a route the router no longer has **fails here**,
//     so a deleted route cannot leave a stale row behind;
//   - and `desktop/src-tauri/src/native/mod.rs` reads the frozen file and
//     asserts every route's real `claims()` matches what it says, so a route
//     cannot be claimed or unclaimed without this file moving.
//
// GET routes are deliberately out of scope. The read surface has no
// forward-on-doubt hazard worth a table — a miss is a fallback, not a
// double-apply — and every read that can be answered from stored data is
// already native.
//
// Regenerate (only from Go, and only after classifying what changed):
//
//	go test ./desktop/parity/ -run TestWriteRoutes -update-write-routes
package parity

import (
	"encoding/json"
	"flag"
	"net/http"
	"os"
	"sort"
	"strings"
	"testing"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/api"
)

const writeRoutesFile = "write_routes.json"

var updateWriteRoutes = flag.Bool("update-write-routes", false,
	"rewrite write_routes.json from the live router")

type writeRoute struct {
	Method string `json:"method"`
	Route  string `json:"route"`
	// Whether the Rust shell answers this route. Asserted against the real
	// `native::claims` by the Rust half.
	Native bool `json:"native"`
	// The issue that owns the decision — the one that ported it, or the one it
	// waits on.
	Owner string `json:"owner"`
	// Why, in one line. For a deferral this is the *effect Rust cannot
	// reproduce*, which is the only thing #274's rule turns on.
	Reason string `json:"reason"`
}

type writeRoutes struct {
	Comment []string     `json:"_comment"`
	Routes  []writeRoute `json:"routes"`
}

// disposition is what this repo has decided about one write route, keyed by
// "METHOD route".
type disposition struct {
	native bool
	owner  string
	reason string
}

// dispositions must cover the write surface exactly. Adding a route to the
// router without adding it here fails this test, which is the whole point.
var dispositions = map[string]disposition{
	// ── Rows and nothing else (#274) ────────────────────────────────────────
	"POST /api/agents":             {true, "#274", "agent CRUD is only rows"},
	"PUT /api/agents/{slug}":       {true, "#274", "agent CRUD is only rows"},
	"DELETE /api/agents/{slug}":    {true, "#274", "agent CRUD is only rows"},
	"POST /api/chats":              {true, "#274", "chat CRUD is only rows"},
	"DELETE /api/chats":            {true, "#274", "chat CRUD is only rows"},
	"PATCH /api/chats/{id}":        {true, "#274", "chat CRUD is only rows"},
	"DELETE /api/chats/{id}":       {true, "#274", "chat CRUD is only rows"},
	"DELETE /api/job-history":      {true, "#274", "job history is only rows"},
	"DELETE /api/job-history/{id}": {true, "#274", "job history is only rows"},

	// ── The chat turn and the three routes that steer it (#276) ─────────────
	"POST /api/chats/{id}/messages":   {true, "#276", "the SSE turn, on the ported SDK"},
	"POST /api/chats/{id}/input":      {true, "#276", "steers a live session Rust holds; forwards when it does not"},
	"POST /api/chats/{id}/permission": {true, "#276", "steers a live session Rust holds; forwards when it does not"},
	"POST /api/chats/{id}/stop":       {true, "#276", "steers a live session Rust holds; forwards when it does not"},

	// ── Integrations that own no live state (#277) ──────────────────────────
	"POST /api/integrations":                       {true, "#277", "Create makes no registry call"},
	"POST /api/integrations/{id}/triggers":         {true, "#277", "trigger rules are only rows"},
	"PUT /api/integrations/{id}/triggers/{rid}":    {true, "#277", "trigger rules are only rows"},
	"DELETE /api/integrations/{id}/triggers/{rid}": {true, "#277", "trigger rules are only rows"},

	// ── The scan, which the shell now owns (#289) ───────────────────────────
	"POST /api/claude-sessions/refresh": {true, "#289", "the shell owns the scan; the sidecar runs with AGENTO_SCANNER=off"},

	// ── Pricing (#306), uploads and continue (#308), notifications (#307) ───
	"POST /api/pricing/rates":                 {true, "#306", "add and correct are two endpoints; the write invalidates stored costs"},
	"PUT /api/pricing/rates":                  {true, "#306", "add and correct are two endpoints; the write invalidates stored costs"},
	"DELETE /api/pricing/rates":               {true, "#306", "the write invalidates stored costs"},
	"POST /api/uploads":                       {true, "#308", "the one multipart route; its effect is a file on disk"},
	"POST /api/claude-sessions/{id}/continue": {true, "#308", "two chat writes in one transaction"},
	"PUT /api/notifications/settings":         {true, "#307", "touches one column, deliberately unlike Go's read-modify-write"},
	"POST /api/notifications/test":            {true, "#307", "sends the mail; a failed send forwards, only success is answered"},

	// ── Claude settings and profiles (#327) ─────────────────────────────────
	"PUT /api/claude-settings":                          {true, "#327", "a file under the run config dir"},
	"POST /api/claude-settings/profiles":                {true, "#327", "profile files plus their metadata index"},
	"PUT /api/claude-settings/profiles/{id}":            {true, "#327", "profile files plus their metadata index"},
	"DELETE /api/claude-settings/profiles/{id}":         {true, "#327", "profile files plus their metadata index"},
	"POST /api/claude-settings/profiles/{id}/duplicate": {true, "#327", "profile files plus their metadata index"},
	"PUT /api/claude-settings/profiles/{id}/default":    {true, "#327", "profile files plus their metadata index"},

	// ── Declined outright (#309) ────────────────────────────────────────────
	"PUT /api/monitoring":       {true, "#309", "answers 501: this build exports no telemetry, so a 200 would lie"},
	"POST /api/monitoring/test": {true, "#309", "answers 501: this build exports no telemetry, so a 200 would lie"},

	// ── The two #293 missed, ported by #296 ─────────────────────────────────
	"POST /api/fs/mkdir":              {true, "#296", "one effect: a directory on disk"},
	"PATCH /api/claude-sessions/{id}": {true, "#296", "two UPDATEs; Cache holds no session state and the scanner writes neither column"},

	// ── Deferred: the scheduler (#275) ──────────────────────────────────────
	"POST /api/tasks":             {false, "#275", "registers a cron entry; a stored task that never fires is worse than none"},
	"PUT /api/tasks/{id}":         {false, "#275", "re-registers the cron entry"},
	"DELETE /api/tasks/{id}":      {false, "#275", "unregisters the cron entry"},
	"POST /api/tasks/{id}/pause":  {false, "#275", "unregisters the cron entry"},
	"POST /api/tasks/{id}/resume": {false, "#275", "re-registers the cron entry"},

	// ── Deferred: the live MCP server (#282) ────────────────────────────────
	"PUT /api/integrations/{id}":    {false, "#282", "Reloads the live in-process MCP server"},
	"DELETE /api/integrations/{id}": {false, "#282", "Stops the live in-process MCP server"},

	// ── Deferred: it talks to somebody else's server ────────────────────────
	"POST /api/integrations/{id}/auth/start":                {false, "-", "mints an OAuth URL and holds the in-flight flow in memory"},
	"POST /api/integrations/{id}/auth/validate":             {false, "-", "dials the remote service; the error text is not reproducible"},
	"POST /api/integrations/{id}/webhook/register":          {false, "-", "registers the webhook with Telegram"},
	"DELETE /api/integrations/{id}/webhook/register":        {false, "-", "deregisters the webhook with Telegram"},
	"POST /api/integrations/{id}/webhook/regenerate-secret": {false, "-", "rotates the secret and re-registers with Telegram"},

	// ── Deferred: the sidecar's boot-time snapshot (#305) ───────────────────
	"PUT /api/settings": {false, "#305", "the sidecar holds a snapshot these preferences resolve through; no AGENTO_SCANNER=off equivalent exists"},

	// ── Not deferred: dropped (#273) ────────────────────────────────────────
	"POST /api/integrations/{id}/whatsapp/pair":      {false, "#273", "WhatsApp is dropped, not deferred; dies with the sidecar"},
	"POST /api/integrations/{id}/whatsapp/reconnect": {false, "#273", "WhatsApp is dropped, not deferred; dies with the sidecar"},
}

// walkWrites returns every non-GET route the real router registers.
//
// `&api.Server{}` is enough: `Mount` only takes method values, it never
// dereferences the receiver, so no service has to be constructed to ask the
// router what it has.
func walkWrites(t *testing.T) []string {
	t.Helper()

	r := chi.NewRouter()
	server := &api.Server{}
	r.Route("/api", func(r chi.Router) { server.Mount(r) })

	var routes []string
	err := chi.Walk(r, func(method, route string, _ http.Handler, _ ...func(http.Handler) http.Handler) error {
		if method == http.MethodGet || method == http.MethodHead || method == http.MethodOptions {
			return nil
		}
		routes = append(routes, method+" "+route)
		return nil
	})
	if err != nil {
		t.Fatalf("walking the router: %v", err)
	}
	sort.Strings(routes)
	return routes
}

func TestWriteRoutes(t *testing.T) {
	routes := walkWrites(t)

	// Both directions. A route with no decision recorded is the failure this
	// file exists to produce; a decision about a route that no longer exists is
	// the rot it exists to prevent.
	seen := make(map[string]bool, len(routes))
	out := make([]writeRoute, 0, len(routes))
	for _, key := range routes {
		seen[key] = true
		d, ok := dispositions[key]
		if !ok {
			t.Errorf("write route %q has no entry in `dispositions`.\n"+
				"Every write route needs a decision: does Rust reproduce *every* effect "+
				"it has? Add it as native with the issue that ported it, or as deferred "+
				"with the effect Rust cannot reproduce.", key)
			continue
		}
		method, route, _ := strings.Cut(key, " ")
		out = append(out, writeRoute{
			Method: method,
			Route:  route,
			Native: d.native,
			Owner:  d.owner,
			Reason: d.reason,
		})
	}
	for key := range dispositions {
		if !seen[key] {
			t.Errorf("`dispositions` classifies %q, which the router does not register — "+
				"delete the entry or fix the spelling", key)
		}
	}
	if t.Failed() {
		return
	}

	want := writeRoutes{
		Comment: []string{
			"Every write route the Go API registers, and what the Rust shell does with it.",
			"Generated from a chi.Walk over the real router by desktop/parity/write_routes_parity_test.go,",
			"which also fails when a route is unclassified or a classification is stale.",
			"Read by desktop/src-tauri/src/native/mod.rs, which asserts each route's real",
			"native::claims() matches the 'native' column here.",
			"Regenerate with: go test ./desktop/parity/ -run TestWriteRoutes -update-write-routes",
		},
		Routes: out,
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding write routes: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateWriteRoutes {
		if err := os.WriteFile(writeRoutesFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", writeRoutesFile, err)
		}
		t.Logf("wrote %s (%d write routes)", writeRoutesFile, len(out))
		return
	}

	frozen, err := os.ReadFile(writeRoutesFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-write-routes): %v", writeRoutesFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: the router or the dispositions have moved.\n"+
			"Regenerate with -update-write-routes — and note the Rust half reads the "+
			"same file, so a claim that no longer matches will fail there.", writeRoutesFile)
	}
}
