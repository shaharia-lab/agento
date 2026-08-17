package integrations

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"os"
	"testing"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/config"
)

// The switch's default is the important half: a plain `agento web` must keep
// hosting its integrations, so only the four spellings below turn hosting off
// and everything else — including a typo — leaves it on.
func TestParseHostedTypes(t *testing.T) {
	// The blunt form: every type is somebody else's.
	all := []string{"off", "OFF", "0", "false", "False", "disabled", " off "}
	for _, v := range all {
		got := parseHostedTypes(v)
		if !got.all || !got.has("whatsapp") {
			t.Errorf("parseHostedTypes(%q) = %+v, want every type hosted elsewhere", v, got)
		}
	}

	// Unset, unrecognized, and an off-word with an **empty** list — which is
	// what a shell that has ported nothing yet would send — all leave hosting
	// entirely with this process.
	on := []string{"", "on", "1", "true", "enabled", "yes", "no", "  ", "offf", "off:", "off: , "}
	for _, v := range on {
		got := parseHostedTypes(v)
		if got.all || got.has("github") || got.has("whatsapp") {
			t.Errorf("parseHostedTypes(%q) = %+v, want hosting left on — "+
				"an unrecognized value must not disable every integration", v, got)
		}
	}

	// The per-type form, which is what the desktop shell actually sends.
	got := parseHostedTypes("off:github")
	if got.all {
		t.Error(`parseHostedTypes("off:github") claimed every type`)
	}
	if !got.has("github") {
		t.Error(`parseHostedTypes("off:github") does not cover github`)
	}
	for _, still := range []string{"whatsapp", "slack", "jira", "telegram", "confluence", "google"} {
		if got.has(still) {
			t.Errorf(`parseHostedTypes("off:github") covers %q, which Go must keep hosting`, still)
		}
	}

	multi := parseHostedTypes(" OFF: github , slack ")
	if !multi.has("github") || !multi.has("slack") || multi.has("jira") {
		t.Errorf("parseHostedTypes(off: github , slack) = %+v", multi)
	}
}

// The variable the snapshot is read from, asserted by its literal name so a
// typo in envHostingSwitch fails here rather than silently leaving hosting on
// in the desktop build.
func TestHostedElsewhereReadsAgentoIntegrations(t *testing.T) {
	if envHostingSwitch != "AGENTO_INTEGRATIONS" {
		t.Fatalf("envHostingSwitch = %q, want AGENTO_INTEGRATIONS", envHostingSwitch)
	}
	t.Setenv("AGENTO_INTEGRATIONS", "off:github")
	if got := parseHostedTypes(os.Getenv(envHostingSwitch)); !got.has("github") {
		t.Errorf("reading %s gave %+v", envHostingSwitch, got)
	}
}

// WhatsApp is the reason the gate is per type rather than per process. Its
// starter is not a pure MCP-server constructor: it opens a whatsmeow WebSocket
// and registers the live client in a package global that ConnectionStatus, the
// reconnect endpoint and QR pairing all read. So the value the desktop shell
// sends must never cover a type the shell does not host.
func TestTheDesktopSwitchLeavesTheUnportedTypesWithGo(t *testing.T) {
	store := &fakeIntegrationStore{rows: []*config.IntegrationConfig{
		{ID: "wa-1", Type: "whatsapp", Enabled: true, Auth: json.RawMessage(`{"t":1}`)},
		{ID: "gh-1", Type: "github", Enabled: true, Auth: json.RawMessage(`{"t":1}`)},
	}}
	reg := NewRegistry(store, slog.New(slog.NewTextHandler(io.Discard, nil)))
	reg.hostedElsewhere = parseHostedTypes("off:github")

	started := map[string]int{}
	for _, integrationType := range []string{"whatsapp", "github"} {
		reg.RegisterStarter(integrationType, func(
			_ context.Context, cfg *config.IntegrationConfig,
		) (claude.McpHTTPServer, error) {
			started[cfg.Type]++
			return claude.McpHTTPServer{Type: "http", URL: "http://127.0.0.1:1"}, nil
		})
	}

	ctx := context.Background()
	if err := reg.Start(ctx); err != nil {
		t.Fatalf("Start: %v", err)
	}
	if started["whatsapp"] != 1 {
		t.Errorf("whatsapp started %d times, want 1 — its client lives in this process",
			started["whatsapp"])
	}
	if started["github"] != 0 {
		t.Errorf("github started %d times, want 0 — the shell hosts it", started["github"])
	}

	// Reload keeps the same split, which is what makes the reconnect endpoint
	// and QR pairing work again.
	if err := reg.Reload(ctx, "wa-1"); err != nil {
		t.Fatalf("Reload(wa-1): %v", err)
	}
	if err := reg.Reload(ctx, "gh-1"); err != nil {
		t.Fatalf("Reload(gh-1): %v", err)
	}
	if started["whatsapp"] != 2 {
		t.Errorf("whatsapp started %d times after Reload, want 2", started["whatsapp"])
	}
	if started["github"] != 0 {
		t.Errorf("github started %d times after Reload, want 0", started["github"])
	}
	if _, ok := reg.GetServerConfig("gh-1"); ok {
		t.Error("a github server was hosted")
	}
	if _, ok := reg.GetServerConfig("wa-1"); !ok {
		t.Error("the whatsapp server is not hosted")
	}

	// Stopping a type hosted elsewhere is a safe no-op; stopping ours works.
	reg.Stop("gh-1")
	reg.Stop("wa-1")
	if _, ok := reg.GetServerConfig("wa-1"); ok {
		t.Error("Stop left the whatsapp server in the registry")
	}
}

// fakeIntegrationStore is the smallest store the registry needs, plus a record
// of whether it was read at all.
type fakeIntegrationStore struct {
	rows   []*config.IntegrationConfig
	listed int
	gets   int
}

func (s *fakeIntegrationStore) List(context.Context) ([]*config.IntegrationConfig, error) {
	s.listed++
	return s.rows, nil
}

func (s *fakeIntegrationStore) Get(_ context.Context, id string) (*config.IntegrationConfig, error) {
	s.gets++
	for _, row := range s.rows {
		if row.ID == id {
			return row, nil
		}
	}
	return nil, nil
}

func (s *fakeIntegrationStore) Save(context.Context, *config.IntegrationConfig) error { return nil }
func (s *fakeIntegrationStore) Delete(context.Context, string) error                  { return nil }

func testRegistry(
	t *testing.T, hostedElsewhere hostedTypes,
) (*IntegrationRegistry, *fakeIntegrationStore, *int) {
	t.Helper()

	store := &fakeIntegrationStore{rows: []*config.IntegrationConfig{{
		ID:      "int-1",
		Type:    "probe",
		Enabled: true,
		Auth:    json.RawMessage(`{"token":"t"}`),
		Services: map[string]config.ServiceConfig{
			"svc": {Enabled: true, Tools: []string{"a", "b"}},
		},
	}}}

	started := 0
	reg := NewRegistry(store, slog.New(slog.NewTextHandler(io.Discard, nil)))
	// Set the field directly rather than through the environment: the real
	// reader is a sync.Once on purpose, so it cannot be flipped twice in one
	// process — which is exactly what a test of both states needs to do.
	reg.hostedElsewhere = hostedElsewhere
	reg.RegisterStarter("probe", func(
		ctx context.Context, cfg *config.IntegrationConfig,
	) (claude.McpHTTPServer, error) {
		started++
		return claude.McpHTTPServer{Type: "http", URL: "http://127.0.0.1:1"}, nil
	})
	return reg, store, &started
}

// With the switch unset the registry behaves exactly as it always has.
func TestRegistryHostsWhenTheSwitchIsOn(t *testing.T) {
	reg, store, started := testRegistry(t, hostedTypes{})
	ctx := context.Background()

	if err := reg.Start(ctx); err != nil {
		t.Fatalf("Start: %v", err)
	}
	if store.listed != 1 {
		t.Errorf("store.List called %d times, want 1", store.listed)
	}
	if *started != 1 {
		t.Fatalf("starter called %d times, want 1", *started)
	}
	if _, ok := reg.GetServerConfig("int-1"); !ok {
		t.Fatal("the started server is not in the registry")
	}

	// Reload is an unconditional stop-then-start, so the starter runs again.
	if err := reg.Reload(ctx, "int-1"); err != nil {
		t.Fatalf("Reload: %v", err)
	}
	if *started != 2 {
		t.Errorf("starter called %d times after Reload, want 2", *started)
	}

	reg.Stop("int-1")
	if _, ok := reg.GetServerConfig("int-1"); ok {
		t.Error("Stop left the server in the registry")
	}
}

// With the blunt form nothing is hosted, and nothing is even read to decide
// that — a second listener holding the same credential is the whole hazard, and
// with every type claimed there is no row whose type could change the answer.
// Nothing sends this value today; it is what the switch means once #313–#317
// have landed.
func TestRegistryHostsNothingWhenEveryTypeIsClaimed(t *testing.T) {
	reg, store, started := testRegistry(t, hostedTypes{all: true})
	ctx := context.Background()

	if err := reg.Start(ctx); err != nil {
		t.Fatalf("Start: %v", err)
	}
	if err := reg.Reload(ctx, "int-1"); err != nil {
		t.Fatalf("Reload: %v", err)
	}
	reg.Stop("int-1")

	if *started != 0 {
		t.Errorf("starter called %d times, want 0", *started)
	}
	if store.listed != 0 || store.gets != 0 {
		t.Errorf("store read (list=%d get=%d) with hosting off, want neither",
			store.listed, store.gets)
	}
	if _, ok := reg.GetServerConfig("int-1"); ok {
		t.Error("a server was hosted with hosting off")
	}
	if len(reg.AllServerConfigs()) != 0 {
		t.Error("AllServerConfigs is not empty with hosting off")
	}
}

// The constraint the whole switch has to respect: an agent run builds its own
// filtered server from a fresh row read and never touches the hosted maps, so
// turning hosting off must not stop an integration-using run.
func TestStartFilteredServerIgnoresTheHostingSwitch(t *testing.T) {
	for _, hostedElsewhere := range []hostedTypes{
		{},
		{all: true},
		{types: map[string]bool{"probe": true}},
	} {
		hostingOff := hostedElsewhere.has("probe")
		reg, store, started := testRegistry(t, hostedElsewhere)

		cfg, err := reg.StartFilteredServer(context.Background(), "int-1", []string{"a"})
		if err != nil {
			t.Fatalf("hostingOff=%v: StartFilteredServer: %v", hostingOff, err)
		}
		if cfg.URL == "" {
			t.Errorf("hostingOff=%v: no server config returned", hostingOff)
		}
		if *started != 1 {
			t.Errorf("hostingOff=%v: starter called %d times, want 1", hostingOff, *started)
		}
		if store.gets != 1 {
			t.Errorf("hostingOff=%v: store.Get called %d times, want 1", hostingOff, store.gets)
		}
		// A per-run server is not recorded — that is what makes it per-run.
		if _, ok := reg.GetServerConfig("int-1"); ok {
			t.Errorf("hostingOff=%v: a filtered server was recorded as hosted", hostingOff)
		}
	}
}

// The other half of a run's tool wiring, which is pure string building and so
// cannot depend on the switch either.
func TestAllowedToolNames(t *testing.T) {
	got := AllowedToolNames("int-1", []string{"a", "b"})
	want := []string{"mcp__int-1__a", "mcp__int-1__b"}
	if len(got) != len(want) {
		t.Fatalf("AllowedToolNames = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("AllowedToolNames[%d] = %q, want %q", i, got[i], want[i])
		}
	}
}
