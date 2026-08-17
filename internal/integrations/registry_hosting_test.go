package integrations

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"testing"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/config"
)

// The switch's default is the important half: a plain `agento web` must keep
// hosting its integrations, so only the four spellings below turn hosting off
// and everything else — including a typo — leaves it on.
func TestHostingOffValue(t *testing.T) {
	off := []string{"off", "OFF", "0", "false", "False", "disabled", " off "}
	for _, v := range off {
		if !hostingOffValue(v) {
			t.Errorf("hostingOffValue(%q) = false, want true", v)
		}
	}

	on := []string{"", "on", "1", "true", "enabled", "yes", "no", "  ", "offf"}
	for _, v := range on {
		if hostingOffValue(v) {
			t.Errorf("hostingOffValue(%q) = true, want false — "+
				"an unrecognized value must not disable every integration", v)
		}
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

func testRegistry(t *testing.T, hostingOff bool) (*IntegrationRegistry, *fakeIntegrationStore, *int) {
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
	reg.hostingOff = hostingOff
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
	reg, store, started := testRegistry(t, false)
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

// With the switch off nothing is hosted, and nothing is even read to decide
// that — the Rust shell owns the hosting and a second listener holding the same
// credential is the whole hazard.
func TestRegistryHostsNothingWhenTheSwitchIsOff(t *testing.T) {
	reg, store, started := testRegistry(t, true)
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
	for _, hostingOff := range []bool{false, true} {
		reg, store, started := testRegistry(t, hostingOff)

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
