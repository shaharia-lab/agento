package parity

// The pricing-catalog seed moved into the desktop shell with the cut-over
// (#278): Go's `Store.Seed` ran on every `agento web` startup, and once the
// sidecar is gone `native/pricing_seed.rs` is what performs that startup
// effect. Both parse the SAME embedded `internal/pricing/catalog.json`, so the
// thing to pin is the normalization — cache-rate derivation, far-past
// defaulting, lowercasing, match-type defaulting, tier flattening.
//
// This test seeds a real migrated database through Go's own `Store.Seed` and
// dumps the resulting rows (minus timestamps, which are now()) to
// pricing_seed_vectors.json in insertion order. The Rust side flattens the
// same catalog and must produce identical rows.
//
// Regenerate after editing catalog.json:
//
//	go test ./desktop/parity/ -run TestPricingSeed -update-pricing-seed

import (
	"bytes"
	"context"
	"encoding/json"
	"flag"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"testing"

	"github.com/shaharia-lab/agento/internal/pricing"
	"github.com/shaharia-lab/agento/internal/storage"
)

const pricingSeedFile = "pricing_seed_vectors.json"

var updatePricingSeed = flag.Bool("update-pricing-seed", false,
	"rewrite "+pricingSeedFile+" from Go's seed output")

// seedRow mirrors the Rust side's SeedRate, field for field.
type seedRow struct {
	Provider            string        `json:"provider"`
	ModelPattern        string        `json:"model_pattern"`
	MatchType           string        `json:"match_type"`
	DisplayName         string        `json:"display_name"`
	InputPerMTok        float64       `json:"input_per_mtok"`
	OutputPerMTok       float64       `json:"output_per_mtok"`
	CacheWrite5mPerMTok float64       `json:"cache_write_5m_per_mtok"`
	CacheWrite1hPerMTok float64       `json:"cache_write_1h_per_mtok"`
	CacheReadPerMTok    float64       `json:"cache_read_per_mtok"`
	EffectiveFrom       string        `json:"effective_from"`
	Source              string        `json:"source"`
	Billable            bool          `json:"billable"`
	Estimated           bool          `json:"estimated"`
	Tiers               []seedTierRow `json:"tiers"`
}

type seedTierRow struct {
	MaxInputTokens      int64   `json:"max_input_tokens"`
	InputPerMTok        float64 `json:"input_per_mtok"`
	OutputPerMTok       float64 `json:"output_per_mtok"`
	CacheWrite5mPerMTok float64 `json:"cache_write_5m_per_mtok"`
	CacheWrite1hPerMTok float64 `json:"cache_write_1h_per_mtok"`
	CacheReadPerMTok    float64 `json:"cache_read_per_mtok"`
}

func seededRows(t *testing.T) []seedRow {
	t.Helper()
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))

	// A real migrated database: NewSQLiteDB runs the migrations, and Seed is
	// exactly the call `Cache.WithPricingStore` makes at server startup.
	db, _, err := storage.NewSQLiteDB(filepath.Join(t.TempDir(), "agento.db"), logger)
	if err != nil {
		t.Fatalf("opening database: %v", err)
	}
	t.Cleanup(func() {
		if cerr := db.Close(); cerr != nil {
			t.Logf("closing database: %v", cerr)
		}
	})

	ctx := context.Background()
	if _, err := pricing.NewStore(db, logger).Seed(ctx); err != nil {
		t.Fatalf("seeding: %v", err)
	}

	rows, err := db.QueryContext(ctx, `
		SELECT id, provider, model_pattern, match_type, display_name,
		       input_per_mtok, output_per_mtok,
		       cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok,
		       effective_from, source, billable, estimated
		FROM model_pricing ORDER BY id`)
	if err != nil {
		t.Fatalf("reading seeded rows: %v", err)
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			t.Logf("closing rows: %v", cerr)
		}
	}()

	var out []seedRow
	var ids []int64
	for rows.Next() {
		var id int64
		var r seedRow
		if err := rows.Scan(&id, &r.Provider, &r.ModelPattern, &r.MatchType,
			&r.DisplayName, &r.InputPerMTok, &r.OutputPerMTok,
			&r.CacheWrite5mPerMTok, &r.CacheWrite1hPerMTok, &r.CacheReadPerMTok,
			&r.EffectiveFrom, &r.Source, &r.Billable, &r.Estimated); err != nil {
			t.Fatalf("scanning seeded row: %v", err)
		}
		r.Tiers = []seedTierRow{}
		out = append(out, r)
		ids = append(ids, id)
	}
	if err := rows.Err(); err != nil {
		t.Fatalf("iterating seeded rows: %v", err)
	}

	for i, id := range ids {
		tiers, err := db.QueryContext(ctx, `
			SELECT max_input_tokens, input_per_mtok, output_per_mtok,
			       cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok
			FROM model_pricing_tier WHERE rate_id = ? ORDER BY max_input_tokens`, id)
		if err != nil {
			t.Fatalf("reading tiers: %v", err)
		}
		for tiers.Next() {
			var tr seedTierRow
			if err := tiers.Scan(&tr.MaxInputTokens, &tr.InputPerMTok, &tr.OutputPerMTok,
				&tr.CacheWrite5mPerMTok, &tr.CacheWrite1hPerMTok, &tr.CacheReadPerMTok); err != nil {
				t.Fatalf("scanning tier: %v", err)
			}
			out[i].Tiers = append(out[i].Tiers, tr)
		}
		if err := tiers.Err(); err != nil {
			t.Fatalf("iterating tiers: %v", err)
		}
		if cerr := tiers.Close(); cerr != nil {
			t.Fatalf("closing tiers: %v", cerr)
		}
	}
	return out
}

func TestPricingSeedVectors(t *testing.T) {
	got, err := json.MarshalIndent(seededRows(t), "", "  ")
	if err != nil {
		t.Fatalf("marshaling seed rows: %v", err)
	}
	got = append(got, '\n')

	if *updatePricingSeed {
		if err := os.WriteFile(pricingSeedFile, got, 0o600); err != nil {
			t.Fatalf("writing %s: %v", pricingSeedFile, err)
		}
		t.Logf("wrote %s (%d bytes)", pricingSeedFile, len(got))
		return
	}

	want, err := os.ReadFile(pricingSeedFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-pricing-seed): %v", pricingSeedFile, err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("%s is stale: Go's seed no longer matches it.\n"+
			"Regenerate with: go test ./desktop/parity/ -run TestPricingSeed -update-pricing-seed\n"+
			"— and note the Rust side (native/pricing_seed.rs) asserts the same file.", pricingSeedFile)
	}
}
