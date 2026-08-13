package parity

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"flag"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"testing"

	"github.com/shaharia-lab/agento/internal/pricing"
	"github.com/shaharia-lab/agento/internal/service"
	"github.com/shaharia-lab/agento/internal/storage"
)

// The golden response for GET /api/pricing/catalog over the fixture rows below.
//
// Written by Go (`go test ./desktop/parity/ -update-golden`) and asserted by
// both languages: the Rust port's own test in
// desktop/src-tauri/src/native/pricing.rs builds the identical rows and must
// produce this file byte for byte. That is the phase-2 bar — the frontend is
// shared, so a field-name, key-order, escaping or float-spelling difference is
// a regression, and only a byte comparison catches all four.
const goldenFile = "pricing_catalog_golden.json"

var updateGolden = flag.Bool("update-golden", false, "rewrite "+goldenFile+" from Go's output")

// fixtureRates are the rows both languages load. They are chosen to cover what
// the real catalog does to an encoder:
//
//   - a model ID needing HTML escaping (`<synthetic>`), at a deliberate $0
//   - two rates for one model, one of them future-dated, so `current` has to
//     pick the older row while the history stays newest-first
//   - whole-number and fractional rates, which Go spells `5` and `6.25`
//   - a tiered rate, whose bands ride along in the payload and in the revision
const fixtureRates = `
INSERT INTO model_pricing
    (id, provider, model_pattern, match_type, display_name,
     input_per_mtok, output_per_mtok,
     cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok,
     effective_from, source, is_builtin, user_modified, billable, estimated,
     created_at, updated_at)
VALUES
    (1, '', '<synthetic>', 'exact', 'Claude Code synthetic message',
     0, 0, 0, 0, 0,
     '2020-01-01T00:00:00Z', 'placeholder', 1, 0, 0, 0,
     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
    (2, 'anthropic', 'claude-opus-5', 'prefix', 'Claude Opus 5',
     5, 25, 6.25, 10, 0.5,
     '2026-01-01T00:00:00Z', 'pricing page', 1, 0, 1, 0,
     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
    (3, 'anthropic', 'claude-opus-5', 'prefix', 'Claude Opus 5',
     6, 30, 7.5, 12, 0.6,
     '2099-01-01T00:00:00Z', 'pricing page', 1, 0, 1, 0,
     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
    (4, 'alibaba', 'qwen3-max', 'prefix', 'Qwen3 Max',
     1.2, 6, 1.5, 2.4, 0.24,
     '2026-01-01T00:00:00Z', 'pricing page', 1, 0, 1, 0,
     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');

INSERT INTO model_pricing_tier
    (rate_id, max_input_tokens, input_per_mtok, output_per_mtok,
     cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok)
VALUES
    (4, 32000, 1.2, 6, 1.5, 2.4, 0.24),
    (4, 128000, 2.4, 12, 3, 4.8, 0.48);
`

// fixtureUnpriced is what the Rust side's claude_session_cache /
// claude_subagent_cache rows dedupe and sort down to. Supplied through the
// service's own interface here so this test does not depend on the session
// scanner's schema.
type fixtureUnpriced struct{}

func (fixtureUnpriced) UnpricedModels(context.Context) ([]string, error) {
	return []string{"glm-4.6", "kimi-k2", "qwen-plus"}, nil
}

func openFixtureDB(t *testing.T) *sql.DB {
	t.Helper()
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))

	// A real migrated database, not a hand-written schema: the port reads the
	// columns migrations actually produce.
	db, _, err := storage.NewSQLiteDB(filepath.Join(t.TempDir(), "agento.db"), logger)
	if err != nil {
		t.Fatalf("opening database: %v", err)
	}
	t.Cleanup(func() {
		if cerr := db.Close(); cerr != nil {
			t.Logf("closing database: %v", cerr)
		}
	})

	if _, err := db.ExecContext(context.Background(), fixtureRates); err != nil {
		t.Fatalf("inserting fixture rates: %v", err)
	}
	return db
}

func catalogJSON(t *testing.T) string {
	t.Helper()
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	db := openFixtureDB(t)

	svc := service.NewPricingService(pricing.NewStore(db, logger), fixtureUnpriced{}, logger)
	catalog, err := svc.Catalog(context.Background())
	if err != nil {
		t.Fatalf("building catalog: %v", err)
	}

	// Exactly what internal/api.Server.writeJSON does, newline included.
	var buf bytes.Buffer
	if err := json.NewEncoder(&buf).Encode(catalog); err != nil {
		t.Fatalf("encoding catalog: %v", err)
	}
	return buf.String()
}

func TestPricingCatalogGolden(t *testing.T) {
	got := catalogJSON(t)

	if *updateGolden {
		if err := os.WriteFile(goldenFile, []byte(got), 0o600); err != nil {
			t.Fatalf("writing %s: %v", goldenFile, err)
		}
		t.Logf("wrote %s (%d bytes)", goldenFile, len(got))
		return
	}

	want, err := os.ReadFile(goldenFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-golden): %v", goldenFile, err)
	}
	if got != string(want) {
		t.Errorf("catalog JSON drifted from %s.\n got: %s\nwant: %s", goldenFile, got, want)
	}
}
