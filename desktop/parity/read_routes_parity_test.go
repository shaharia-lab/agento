package parity

// The read-side twin of write_routes_parity_test.go, added at the cut-over
// (#278).
//
// The write audit is writes-only by design — a read that missed was a fallback,
// not a double-apply — which meant nothing had checked the READ surface against
// the seam. That was survivable while the sidecar answered whatever Rust did
// not claim; with the sidecar gone an unclaimed read is a 404, so every GET
// route needs the same explicit decision a write has: native (Rust answers it)
// or dropped (the desktop build deliberately does not have it). There is no
// `deferred` here — there is nothing left to defer to.
//
// Regenerate with:
//
//	go test ./desktop/parity/ -run TestReadRoutes -update-read-routes

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

const readRoutesFile = "read_routes.json"

var updateReadRoutes = flag.Bool("update-read-routes", false,
	"rewrite "+readRoutesFile+" from the router walk and the dispositions below")

// readDispositions is every GET route the server mounts, and what the desktop
// shell does with it since the sidecar is gone.
var readDispositions = map[string]disposition{
	// ── The bulk of the surface: ported reads ───────────────────────────────
	"GET /api/agents":                           {statusNative, "#262", "the agents list"},
	"GET /api/agents/{slug}":                    {statusNative, "#262", "one agent; a missing slug answers handleGetAgent's own 404 since #278"},
	"GET /api/chats":                            {statusNative, "#265", "the chats list"},
	"GET /api/chats/{id}":                       {statusNative, "#265", "one chat with messages; missing answers Go's 404 since #278"},
	"GET /api/settings":                         {statusNative, "#266", "the user_settings row, resolved"},
	"GET /api/settings/claude-config-dirs":      {statusNative, "#305", "the config-dir probe; 501 on Windows since #278"},
	"GET /api/claude-settings":                  {statusNative, "#304", "Claude Code's own settings.json"},
	"GET /api/claude-settings/profiles":         {statusNative, "#304", "the profile index; seeds it, so the read is a write"},
	"GET /api/claude-settings/profiles/{id}":    {statusNative, "#304", "one profile"},
	"GET /api/version":                          {statusNative, "#267", "the build stamp"},
	"GET /api/version/update-check":             {statusNative, "#267", "the short-circuit for every build since #278; Tauri's updater owns real updates"},
	"GET /api/monitoring":                       {statusNative, "#309", "monitoring.json plus the OTEL_* locks; no exporters"},
	"GET /api/fs":                               {statusNative, "#296", "the working-dir picker's listing; 501 on Windows since #278"},
	"GET /api/claude-sessions":                  {statusNative, "#264", "the paged sessions list"},
	"GET /api/claude-sessions/facets":           {statusNative, "#264", "the filtered set's totals and options"},
	"GET /api/claude-sessions/projects":         {statusNative, "#269", "the project picker's list"},
	"GET /api/claude-sessions/status":           {statusNative, "#289", "the scan's own state; the shell runs the scan"},
	"GET /api/claude-sessions/insights/summary": {statusNative, "#263", "the aggregate insight summary"},
	"GET /api/claude-sessions/{id}":             {statusNative, "#270", "one session, re-read from its transcript"},
	"GET /api/claude-analytics":                 {statusNative, "#268", "the analytics report"},
	"GET /api/integrations":                     {statusNative, "#272", "the integrations list, credentials never selected"},
	"GET /api/integrations/available-tools":     {statusNative, "#272", "the tool catalog"},
	"GET /api/integrations/{id}":                {statusNative, "#272", "one integration; missing answers NotFoundError's 404 since #278"},
	"GET /api/integrations/{id}/auth/status":    {statusNative, "#318", "the shell owns the OAuth flow state"},
	"GET /api/integrations/{id}/triggers":       {statusNative, "#277", "the trigger rules"},
	"GET /api/integrations/{id}/webhook/status": {statusNative, "#319", "three columns off the row; no network"},
	"GET /api/notifications/settings":           {statusNative, "#307", "the settings read, password masked"},
	"GET /api/notifications/log":                {statusNative, "#307", "the notification log"},
	"GET /api/tasks":                            {statusNative, "#274", "the tasks list"},
	"GET /api/tasks/{id}":                       {statusNative, "#274", "one task"},
	"GET /api/tasks/{id}/job-history":           {statusNative, "#274", "one task's job history"},
	"GET /api/job-history":                      {statusNative, "#274", "all job history"},
	"GET /api/job-history/{id}":                 {statusNative, "#274", "one job"},
	"GET /api/pricing/catalog":                  {statusNative, "#262", "the pricing catalog"},

	// ── Outside /api ────────────────────────────────────────────────────────
	"GET /health": {statusNative, "#278", "one constant; anything external probing it keeps its answer"},
	// `native` in the same sense the monitoring writes are: claimed so the
	// decline is answered deliberately (501), never a version-mismatch-shaped
	// 404. The desktop build exports no telemetry (#309).
	"GET /metrics": {statusNative, "#278", "the read half of the declined telemetry feature; claimed by monitoring, answers 501"},

	// ── Dropped with the sidecar ────────────────────────────────────────────
	"GET /api/integrations/{id}/whatsapp/qr":     {statusDropped, "#273", "WhatsApp is dropped; dies with the sidecar"},
	"GET /api/integrations/{id}/whatsapp/status": {statusDropped, "#273", "WhatsApp is dropped; dies with the sidecar"},
	"GET /api/claude-sessions/{id}/insights":     {statusDropped, "#278", "no desktop view renders the per-session insight record; add with the view if one is built"},
	"GET /api/claude-sessions/{id}/journey":      {statusDropped, "#278", "no desktop view renders the journey timeline; add with the view if one is built"},
}

// walkReads returns every GET route the server mounts, minus the SPA catch-all:
// `GET /*` is the web frontend Go embeds, and the desktop shell serves its own
// embedded frontend for every non-API path — it is not an API surface either
// implementation routes by name.
func walkReads(t *testing.T) []string {
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
		if method != http.MethodGet {
			return nil
		}
		if route == "/*" {
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

func TestReadRoutes(t *testing.T) {
	routes := walkReads(t)

	seen := make(map[string]bool, len(routes))
	out := make([]writeRoute, 0, len(routes))
	for _, key := range routes {
		seen[key] = true
		d, ok := readDispositions[key]
		if !ok {
			t.Errorf("read route %q has no entry in `readDispositions`.\n"+
				"With the sidecar gone (#278) an unclaimed read is a 404, so every GET "+
				"route needs a decision: native, or dropped with the reason.", key)
			continue
		}
		method, route, _ := strings.Cut(key, " ")
		switch d.status {
		case statusNative, statusDropped:
		default:
			t.Errorf("read route %q has status %q; want native or dropped — "+
				"there is nothing left to defer to", key, d.status)
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
	for key := range readDispositions {
		if !seen[key] {
			t.Errorf("`readDispositions` classifies %q, which the router does not register — "+
				"delete the entry or fix the spelling", key)
		}
	}
	if t.Failed() {
		return
	}

	want := writeRoutes{
		Comment: []string{
			"Every GET route the server mounts, and what the desktop shell does with it",
			"since the sidecar cut-over (#278). Generated by",
			"desktop/parity/read_routes_parity_test.go from a chi.Walk over the router",
			"server.New builds. 'native' means the Rust registry claims it; 'dropped' means",
			"the desktop build deliberately does not have it (an unclaimed route answers",
			"chi's own 404 — except /metrics, which monitoring claims so the decline is a",
			"deliberate 501). Read by desktop/src-tauri/src/native/mod.rs, which asserts",
			"each route's real native::claims() against the 'status' column here.",
			"Regenerate with: go test ./desktop/parity/ -run TestReadRoutes -update-read-routes",
		},
		Routes: out,
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding read routes: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateReadRoutes {
		if err := os.WriteFile(readRoutesFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", readRoutesFile, err)
		}
		t.Logf("wrote %s (%d read routes)", readRoutesFile, len(out))
		return
	}

	frozen, err := os.ReadFile(readRoutesFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-read-routes): %v", readRoutesFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: the router or the dispositions have moved.\n"+
			"Regenerate with -update-read-routes — and note the Rust half reads the "+
			"same file, so a claim that no longer matches will fail there.", readRoutesFile)
	}
}
