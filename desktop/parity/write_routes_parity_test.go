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
	"log/slog"
	"net/http"
	"os"
	"sort"
	"strings"
	"testing"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/api"
	"github.com/shaharia-lab/agento/internal/server"
)

const writeRoutesFile = "write_routes.json"

var updateWriteRoutes = flag.Bool("update-write-routes", false,
	"rewrite write_routes.json from the live router")

type writeRoute struct {
	Method string `json:"method"`
	Route  string `json:"route"`
	// What this repo has decided: `native` (Rust answers it), `deferred`
	// (Rust will answer it once the named issue lands) or `dropped` (Rust will
	// never answer it).
	//
	// **`deferred` and `dropped` are not the same answer**, and a boolean
	// cannot say so — the WhatsApp routes are waiting for nothing, while the
	// task writes are waiting for #275. For a file whose point is to be
	// queried, the distinction the reader cares about has to be a field rather
	// than a phrase inside `reason`.
	Status string `json:"status"`
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
	status string
	owner  string
	reason string
}

// The three values `disposition.status` may take.
const (
	statusNative   = "native"
	statusDeferred = "deferred"
	statusDropped  = "dropped"
)

// dispositions must cover the write surface exactly. Adding a route to the
// router without adding it here fails this test, which is the whole point.
var dispositions = map[string]disposition{
	// ── Rows and nothing else (#274) ────────────────────────────────────────
	"POST /api/agents":             {statusNative, "#274", "agent CRUD is only rows"},
	"PUT /api/agents/{slug}":       {statusNative, "#274", "agent CRUD is only rows"},
	"DELETE /api/agents/{slug}":    {statusNative, "#274", "agent CRUD is only rows"},
	"POST /api/chats":              {statusNative, "#274", "chat CRUD is only rows"},
	"DELETE /api/chats":            {statusNative, "#274", "chat CRUD is only rows"},
	"PATCH /api/chats/{id}":        {statusNative, "#274", "chat CRUD is only rows"},
	"DELETE /api/chats/{id}":       {statusNative, "#274", "chat CRUD is only rows"},
	"DELETE /api/job-history":      {statusNative, "#274", "job history is only rows"},
	"DELETE /api/job-history/{id}": {statusNative, "#274", "job history is only rows"},

	// ── The chat turn and the three routes that steer it (#276) ─────────────
	"POST /api/chats/{id}/messages":   {statusNative, "#276", "the SSE turn, on the ported SDK"},
	"POST /api/chats/{id}/input":      {statusNative, "#276", "steers a live session Rust holds; forwards when it does not"},
	"POST /api/chats/{id}/permission": {statusNative, "#276", "steers a live session Rust holds; forwards when it does not"},
	"POST /api/chats/{id}/stop":       {statusNative, "#276", "steers a live session Rust holds; forwards when it does not"},

	// ── Integrations that own no live state (#277) ──────────────────────────
	"POST /api/integrations":                       {statusNative, "#277", "Create makes no registry call"},
	"POST /api/integrations/{id}/triggers":         {statusNative, "#277", "trigger rules are only rows"},
	"PUT /api/integrations/{id}/triggers/{rid}":    {statusNative, "#277", "trigger rules are only rows"},
	"DELETE /api/integrations/{id}/triggers/{rid}": {statusNative, "#277", "trigger rules are only rows"},

	// ── Integrations that own live state, once the shell took it (#311) ─────
	//
	// The second ownership flip, and the same shape as #289's: the sidecar runs
	// with AGENTO_INTEGRATIONS=off, so IntegrationRegistry.Start/Reload/Stop
	// are no-ops there and the Rust registry is the only implementation. Until
	// that switch existed a native write here would have left the sidecar
	// hosting an unauthenticated loopback MCP server on a revoked credential.
	"PUT /api/integrations/{id}":    {statusNative, "#311", "reloads the MCP server, which the shell now hosts; the sidecar runs with AGENTO_INTEGRATIONS=off"},
	"DELETE /api/integrations/{id}": {statusNative, "#311", "stops the MCP server, which the shell now hosts; the sidecar runs with AGENTO_INTEGRATIONS=off"},

	// ── The scan, which the shell now owns (#289) ───────────────────────────
	"POST /api/claude-sessions/refresh": {statusNative, "#289", "the shell owns the scan; the sidecar runs with AGENTO_SCANNER=off"},

	// ── Pricing (#306), uploads and continue (#308), notifications (#307) ───
	"POST /api/pricing/rates":                 {statusNative, "#306", "add and correct are two endpoints; the write invalidates stored costs"},
	"PUT /api/pricing/rates":                  {statusNative, "#306", "add and correct are two endpoints; the write invalidates stored costs"},
	"DELETE /api/pricing/rates":               {statusNative, "#306", "the write invalidates stored costs"},
	"POST /api/uploads":                       {statusNative, "#308", "the one multipart route; its effect is a file on disk"},
	"POST /api/claude-sessions/{id}/continue": {statusNative, "#308", "two chat writes in one transaction"},
	"PUT /api/notifications/settings":         {statusNative, "#307", "touches one column, deliberately unlike Go's read-modify-write"},
	"POST /api/notifications/test":            {statusNative, "#307", "sends the mail; a failed send forwards, only success is answered"},

	// ── Claude settings and profiles (#327) ─────────────────────────────────
	"PUT /api/claude-settings":                          {statusNative, "#327", "a file under the run config dir"},
	"POST /api/claude-settings/profiles":                {statusNative, "#327", "profile files plus their metadata index"},
	"PUT /api/claude-settings/profiles/{id}":            {statusNative, "#327", "profile files plus their metadata index"},
	"DELETE /api/claude-settings/profiles/{id}":         {statusNative, "#327", "profile files plus their metadata index"},
	"POST /api/claude-settings/profiles/{id}/duplicate": {statusNative, "#327", "profile files plus their metadata index"},
	"PUT /api/claude-settings/profiles/{id}/default":    {statusNative, "#327", "profile files plus their metadata index"},

	// ── Declined outright (#309) ────────────────────────────────────────────
	"PUT /api/monitoring":       {statusNative, "#309", "answers 501: this build exports no telemetry, so a 200 would lie"},
	"POST /api/monitoring/test": {statusNative, "#309", "answers 501: this build exports no telemetry, so a 200 would lie"},

	// ── The two #293 missed, ported by #296 ─────────────────────────────────
	"POST /api/fs/mkdir":              {statusNative, "#296", "one effect: a directory on disk"},
	"PATCH /api/claude-sessions/{id}": {statusNative, "#296", "two UPDATEs; Cache holds no session state and the scanner writes neither column"},

	// ── The scheduler, and the five writes that register with it (#275) ─────
	//
	// These were deferred for as long as the scheduler was Go's: each also
	// registers or unregisters a cron entry, and a task stored by one process
	// and scheduled by the other is a task that never fires. #275 moved the
	// scheduler into the shell — AGENTO_SCHEDULER=off on the sidecar — so the
	// row write and the registration are the same edit again.
	"POST /api/tasks":             {statusNative, "#275", "registers a cron entry with the shell's own scheduler"},
	"PUT /api/tasks/{id}":         {statusNative, "#275", "re-registers the cron entry"},
	"DELETE /api/tasks/{id}":      {statusNative, "#275", "unregisters the cron entry"},
	"POST /api/tasks/{id}/pause":  {statusNative, "#275", "unregisters the cron entry"},
	"POST /api/tasks/{id}/resume": {statusNative, "#275", "re-registers the cron entry"},

	// ── Deferred: it talks to somebody else's server ────────────────────────
	"POST /api/integrations/{id}/auth/start":                {statusDeferred, "-", "mints an OAuth URL and holds the in-flight flow in memory"},
	"POST /api/integrations/{id}/auth/validate":             {statusDeferred, "-", "dials the remote service; the error text is not reproducible"},
	"POST /api/integrations/{id}/webhook/register":          {statusDeferred, "-", "registers the webhook with Telegram"},
	"DELETE /api/integrations/{id}/webhook/register":        {statusDeferred, "-", "deregisters the webhook with Telegram"},
	"POST /api/integrations/{id}/webhook/regenerate-secret": {statusDeferred, "-", "rotates the secret and re-registers with Telegram"},

	// ── Deferred: the sidecar's boot-time snapshot (#305) ───────────────────
	"PUT /api/settings": {statusDeferred, "#305", "the sidecar holds a snapshot these preferences resolve through; no AGENTO_SCANNER=off equivalent exists"},

	// ── Deferred: it arrives from outside and dispatches an agent run ───────
	//
	// The one write outside `/api`. It is authenticated by its own secret token
	// rather than by the two `/api` guards — it arrives from Telegram's servers
	// with a foreign `Host` — and its effect is a trigger-rule match plus an
	// agent run through the dispatcher, which is the scheduler's executor by
	// another name.
	"POST /webhooks/telegram/{id}": {statusDeferred, "#275", "dispatches an agent run; the executor is still Go's"},

	// ── Not deferred: dropped (#273) ────────────────────────────────────────
	"POST /api/integrations/{id}/whatsapp/pair":      {statusDropped, "#273", "WhatsApp is dropped, not deferred; dies with the sidecar"},
	"POST /api/integrations/{id}/whatsapp/reconnect": {statusDropped, "#273", "WhatsApp is dropped, not deferred; dies with the sidecar"},
}

// walkWrites returns every non-GET route the server actually mounts.
//
// **It asks the real router rather than rebuilding it.** `server.New` is what
// decides the route table — `/api` under two guards, the Telegram webhook at the
// root, `/health`, `/metrics` and the SPA — and an earlier version of this test
// reconstructed those mounts by hand and missed the webhook. Reconstructing them
// is the same drift one level up: the next root-level mount would escape a walk
// that claims to cover every mount. `Server.Routes()` exists for this.
//
// The zero values are enough. Nothing is dereferenced at construction, and
// `api.Server.Mount` registers every route unconditionally — there is no
// `if s.x != nil` arm — so a zero-value server yields the full surface rather
// than a subset, which is the failure that would make this check quietly
// incomplete.
func walkWrites(t *testing.T) []string {
	t.Helper()

	srv := server.New(
		&api.Server{},
		nil,
		0,
		slog.New(slog.DiscardHandler),
		nil,
		&api.TelegramWebhookHandler{},
		server.Options{},
	)

	var routes []string
	err := chi.Walk(srv.Routes(), func(method, route string, _ http.Handler, _ ...func(http.Handler) http.Handler) error {
		// GET routes are out of scope: a read that misses is a fallback, not a
		// double-apply. HEAD and OPTIONS come along with them.
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
		switch d.status {
		case statusNative, statusDeferred, statusDropped:
		default:
			t.Errorf("write route %q has status %q; want one of native/deferred/dropped", key, d.status)
			continue
		}
		out = append(out, writeRoute{
			Method: method,
			Route:  route,
			Status: d.status,
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
			"Every write route the server mounts, and what the Rust shell does with it.",
			"Generated by desktop/parity/write_routes_parity_test.go from a chi.Walk over the",
			"router server.New builds — both mounts, not just /api. It also fails when a",
			"route is unclassified or a classification is stale.",
			"Read by desktop/src-tauri/src/native/mod.rs, which asserts each route's real",
			"native::claims() matches the 'status' column here.",
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
