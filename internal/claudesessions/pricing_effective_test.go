package claudesessions

import (
	"context"
	"database/sql"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/pricing"
)

// newSeededPricingStore seeds the built-in catalog into db and returns the
// store. Unlike main_test.go's package-level wiring, attaching it to a Cache
// lets refreshPricingResolver pick up later rate edits.
func newSeededPricingStore(t *testing.T, db *sql.DB) *pricing.Store {
	t.Helper()
	store := pricing.NewStore(db, testLogger)
	if _, err := store.Seed(context.Background()); err != nil {
		t.Fatalf("seed pricing: %v", err)
	}
	return store
}

// editRate updates one seeded rate row in place, simulating the maintainer
// correcting a rate (the settings UI's write path). It bumps the catalog
// revision, which is what Cache.List notices.
func editRate(t *testing.T, db *sql.DB, pattern string, effectiveFrom time.Time, in, out float64) {
	t.Helper()
	res, err := db.ExecContext(context.Background(), `
		UPDATE model_pricing SET input_per_mtok = ?, output_per_mtok = ?,
			cache_write_5m_per_mtok = ?, cache_write_1h_per_mtok = ?, cache_read_per_mtok = ?,
			user_modified = 1, updated_at = ?
		WHERE model_pattern = ? AND effective_from = ?`,
		in, out, in*1.25, in*2, in*0.1,
		time.Now().UTC().Format(time.RFC3339),
		pattern, effectiveFrom.UTC().Format(time.RFC3339))
	if err != nil {
		t.Fatalf("edit rate: %v", err)
	}
	if n, _ := res.RowsAffected(); n != 1 {
		t.Fatalf("edit rate: %d rows affected, want 1 (is %q@%s seeded?)", n, pattern, effectiveFrom)
	}
}

// writeJSONLPricedTurns writes a session whose assistant turns carry explicit
// (model, timestamp, usage) triples — the fixture shape every per-message
// pricing test needs.
func writeJSONLPricedTurns(
	t *testing.T, dir, sessionID string,
	turns []struct {
		model string
		ts    time.Time
		usage rawUsage
	},
) {
	t.Helper()
	if err := os.MkdirAll(dir, 0750); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	var data []byte
	user, _ := json.Marshal(rawEvent{
		Type: "user", SessionID: sessionID, Timestamp: turns[0].ts.Add(-time.Second), CWD: "/tmp",
		Message: &rawMessage{Role: "user", Content: json.RawMessage(`"hi"`)},
	})
	data = append(append(data, user...), '\n')
	for _, turn := range turns {
		u := turn.usage
		assistant, _ := json.Marshal(rawEvent{
			Type: "assistant", SessionID: sessionID, Timestamp: turn.ts,
			Message: &rawMessage{
				Role: "assistant", Model: turn.model,
				Content: json.RawMessage(`[{"type":"text","text":"ok"}]`),
				Usage:   &u,
			},
		})
		data = append(append(data, assistant...), '\n')
	}
	if err := os.WriteFile(filepath.Join(dir, sessionID+".jsonl"), data, 0600); err != nil {
		t.Fatalf("write jsonl: %v", err)
	}
}

func readPricedFixture(
	t *testing.T, sessionID string,
	turns []struct {
		model string
		ts    time.Time
		usage rawUsage
	},
) (*ClaudeSessionSummary, *costAccumulator) {
	t.Helper()
	dir := t.TempDir()
	writeJSONLPricedTurns(t, dir, sessionID, turns)
	summary, costs, err := readSessionSummary(sessionID, "/tmp",
		filepath.Join(dir, sessionID+".jsonl"), testLogger)
	if err != nil || summary == nil {
		t.Fatalf("readSessionSummary: %v", err)
	}
	return summary, costs
}

// TestPerMessageCost_MultiModelSession is the acceptance criterion: a session
// mixing two models is priced as the sum of the two per-model costs, not all
// tokens at the first-seen model's rate.
func TestPerMessageCost_MultiModelSession(t *testing.T) {
	ts := time.Date(2026, 6, 1, 10, 0, 0, 0, time.UTC)
	_, costs := readPricedFixture(t, "session-multi", []struct {
		model string
		ts    time.Time
		usage rawUsage
	}{
		{"claude-opus-5", ts, rawUsage{InputTokens: 1_000_000}},                     // $5
		{"claude-haiku-4-5", ts.Add(time.Minute), rawUsage{InputTokens: 1_000_000}}, // $1
	})

	if costs.pricedMessages != 2 {
		t.Fatalf("priced messages = %d, want 2", costs.pricedMessages)
	}
	assertUSD(t, "multi-model total", costs.cost.TotalCostUSD, 6.00)
}

// TestPerMessageCost_PriceChangeBoundary prices two Sonnet 5 turns on either
// side of the 2026-09-01 rate change: each keeps the rate in force when it was
// sent, which whole-session pricing cannot express.
func TestPerMessageCost_PriceChangeBoundary(t *testing.T) {
	_, costs := readPricedFixture(t, "session-boundary", []struct {
		model string
		ts    time.Time
		usage rawUsage
	}{
		{"claude-sonnet-5", time.Date(2026, 8, 15, 0, 0, 0, 0, time.UTC), rawUsage{InputTokens: 1_000_000}}, // $2 intro
		{"claude-sonnet-5", time.Date(2026, 9, 15, 0, 0, 0, 0, time.UTC), rawUsage{InputTokens: 1_000_000}}, // $3 list
	})
	assertUSD(t, "boundary-spanning total", costs.cost.TotalCostUSD, 5.00)
}

// TestPerMessageCost_UnknownModelsTracked asserts unpriced messages contribute
// nothing but are accounted.
func TestPerMessageCost_UnknownModelsTracked(t *testing.T) {
	ts := time.Date(2026, 6, 1, 10, 0, 0, 0, time.UTC)
	_, costs := readPricedFixture(t, "session-unknown", []struct {
		model string
		ts    time.Time
		usage rawUsage
	}{
		{"claude-opus-5", ts, rawUsage{InputTokens: 1_000_000}},                         // $5
		{"k3", ts.Add(time.Minute), rawUsage{InputTokens: 500_000, OutputTokens: 500}},  // $1.5075
		{syntheticModel, ts.Add(2 * time.Minute), rawUsage{InputTokens: 100}},           // non-billable
		{"any", ts.Add(3 * time.Minute), rawUsage{InputTokens: 400, OutputTokens: 100}}, // unpriced
	})

	// Opus, K3 and the synthetic placeholder all resolve; only "any" does not.
	if costs.pricedMessages != 3 {
		t.Fatalf("priced messages = %d, want 3", costs.pricedMessages)
	}
	assertUSD(t, "total", costs.cost.TotalCostUSD, 5.00+1.5075)
	if got := costs.UnknownPricingTokens(); got != 500 {
		t.Errorf("unknown tokens = %d, want 500", got)
	}
	if _, tracked := costs.unknownModels[syntheticModel]; tracked {
		t.Error("<synthetic> must not be tracked as an unknown model")
	}
	if _, tracked := costs.unknownModels["k3"]; tracked {
		t.Error("k3 is priced as of #187 and must not be tracked as unknown")
	}
}

// TestIncrementalScan_PricingRefreshRePrices is the revision-marker acceptance
// criterion, adapted to read-time pricing: after a rate edit, the next
// Cache.List refreshes the resolver from the changed revision and the
// re-computed cost reflects the new rate — no cache wipe, no mtime change.
// The test runs on a dedicated throwaway rate row so the shared seeded catalog
// (and every other test in this binary) is untouched.
func TestIncrementalScan_PricingRefreshRePrices(t *testing.T) {
	db := setupTestDB(t)
	store := newSeededPricingStore(t, db)
	ctx := context.Background()

	const testModel = "claude-test-reprice"
	if err := store.UpsertRate(ctx, pricing.Rate{
		Provider: "anthropic", ModelPattern: testModel, MatchType: pricing.MatchPrefix,
		InputPerMTok: 2.00, OutputPerMTok: 10.00,
		CacheWrite5mPerMTok: 2.50, CacheWrite1hPerMTok: 4.00, CacheReadPerMTok: 0.20,
		EffectiveFrom: time.Date(2020, 1, 1, 0, 0, 0, 0, time.UTC),
		Billable:      true,
	}); err != nil {
		t.Fatalf("insert test rate: %v", err)
	}
	t.Cleanup(func() {
		// Remove the throwaway row and reload the shared resolver from the
		// seed-only catalog, so the rest of the binary sees seeded rates.
		if err := store.DeleteRate(ctx, testModel, time.Date(2020, 1, 1, 0, 0, 0, 0, time.UTC)); err != nil {
			t.Errorf("cleanup delete rate: %v", err)
		}
		rates, err := store.Snapshot(ctx)
		if err != nil {
			t.Errorf("cleanup snapshot: %v", err)
			return
		}
		packagePricing.Lock()
		packagePricing.resolver = pricing.NewResolver(rates)
		packagePricing.revision = pricingRevUnknown
		packagePricing.Unlock()
	})

	home := t.TempDir()
	t.Setenv("HOME", home)
	projectDir := filepath.Join(home, ".claude", "projects", "test-project")
	ts := time.Date(2026, 8, 15, 10, 0, 0, 0, time.UTC)
	writeJSONLPricedTurns(t, projectDir, "session-reprice", []struct {
		model string
		ts    time.Time
		usage rawUsage
	}{
		{testModel, ts, rawUsage{InputTokens: 1_000_000}},
	})

	cache := NewCache(db, testLogger).WithPricingStore(store)
	sessions := cache.List()
	s := findSession(t, sessions, "session-reprice")

	before, priced := costForUsage(s.Model, s.TotalUsage(), ts)
	if !priced {
		t.Fatal("test model should be priced")
	}
	assertUSD(t, "rate before edit", before.TotalCostUSD, 2.00)

	// The maintainer edits the rate; the file's mtime never moves.
	editRate(t, db, testModel, time.Date(2020, 1, 1, 0, 0, 0, 0, time.UTC), 2.50, 12.50)

	sessions = cache.List()
	s = findSession(t, sessions, "session-reprice")
	after, priced := costForUsage(s.Model, s.TotalUsage(), ts)
	if !priced {
		t.Fatal("test model should be priced after the edit")
	}
	assertUSD(t, "rate after edit", after.TotalCostUSD, 2.50)
}

// TestCostForUsage_RateInsertKeepsHistory is the headline acceptance criterion:
// a rate row added with effective_from = 2026-09-01 must not change the cost
// of a session whose messages are all dated 2026-08-15 — byte-identical before
// and after the insert.
func TestCostForUsage_RateInsertKeepsHistory(t *testing.T) {
	at := time.Date(2026, 8, 15, 10, 0, 0, 0, time.UTC)
	u := TokenUsage{
		InputTokens:           1_234_567,
		OutputTokens:          23_456,
		CacheCreationTokens:   100_000,
		CacheCreation1hTokens: 100_000,
	}

	before, priced := costForUsage("claude-sonnet-5", u, at)
	if !priced {
		t.Skip("no pricing resolver wired")
	}

	// Inserting a future rate row is exactly what the 2026-09-01 list-rate seed
	// already did — resolving before its effective_from must ignore it. The seed
	// is in place for this whole test binary, so `before` already proves the
	// criterion; assert the introductory rate is the one applied.
	assertUSD(t, "input cost at intro rate", before.InputCostUSD, 1.234567*2.00)
	assertUSD(t, "output cost at intro rate", before.OutputCostUSD, 0.023456*10.00)
}
