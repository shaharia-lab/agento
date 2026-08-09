package pricing

import (
	"context"
	"log/slog"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/storage"
)

func newStore(t *testing.T) *Store {
	t.Helper()
	db, _, err := storage.NewSQLiteDB(":memory:", slog.Default())
	if err != nil {
		t.Fatalf("opening test database: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })
	return NewStore(db, slog.Default())
}

func TestSeed_InsertsBuiltinCatalog(t *testing.T) {
	s := newStore(t)
	ctx := context.Background()

	written, err := s.Seed(ctx)
	if err != nil {
		t.Fatalf("seed: %v", err)
	}
	if written == 0 {
		t.Fatal("seed wrote nothing")
	}

	rates, err := s.Snapshot(ctx)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if len(rates) != written {
		t.Errorf("snapshot has %d rows, seed reported %d", len(rates), written)
	}
	for _, r := range rates {
		if !r.IsBuiltin {
			t.Errorf("seeded row %q not marked builtin", r.ModelPattern)
		}
		if r.UserModified {
			t.Errorf("seeded row %q marked user_modified", r.ModelPattern)
		}
		// Anthropic rows derive their cache columns from input; third-party rows
		// carry the provider's own published cache prices, so only the former
		// are checked against the TTL multipliers.
		if r.Provider == "anthropic" {
			if r.CacheWrite5mPerMTok != r.InputPerMTok*1.25 ||
				r.CacheWrite1hPerMTok != r.InputPerMTok*2 ||
				r.CacheReadPerMTok != r.InputPerMTok*0.1 {
				t.Errorf("seeded row %q has non-derived cache rates", r.ModelPattern)
			}
		}
	}
}

func TestSeed_Idempotent(t *testing.T) {
	s := newStore(t)
	ctx := context.Background()
	if _, err := s.Seed(ctx); err != nil {
		t.Fatalf("seed: %v", err)
	}
	before, err := s.Snapshot(ctx)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	revBefore, err := s.Revision(ctx)
	if err != nil {
		t.Fatalf("revision: %v", err)
	}

	if _, err := s.Seed(ctx); err != nil {
		t.Fatalf("re-seed: %v", err)
	}
	after, err := s.Snapshot(ctx)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	revAfter, err := s.Revision(ctx)
	if err != nil {
		t.Fatalf("revision: %v", err)
	}

	if len(before) != len(after) {
		t.Fatalf("row count changed across re-seed: %d -> %d", len(before), len(after))
	}
	for i := range before {
		if before[i] != after[i] {
			t.Errorf("row %d (%q) changed across idempotent re-seed", i, before[i].ModelPattern)
		}
	}
	if revBefore != revAfter {
		t.Errorf("revision changed across idempotent re-seed: %d -> %d", revBefore, revAfter)
	}
}

func TestSeed_NeverClobbersUserModifiedRow(t *testing.T) {
	s := newStore(t)
	ctx := context.Background()
	if _, err := s.Seed(ctx); err != nil {
		t.Fatalf("seed: %v", err)
	}

	// The user edits the built-in Sonnet 5 list-rate row.
	custom := Rate{
		Provider:            "anthropic",
		ModelPattern:        "claude-sonnet-5",
		MatchType:           MatchPrefix,
		DisplayName:         "Claude Sonnet 5 (custom)",
		InputPerMTok:        9.99,
		OutputPerMTok:       99.99,
		CacheWrite5mPerMTok: 9.99 * 1.25,
		CacheWrite1hPerMTok: 9.99 * 2,
		CacheReadPerMTok:    9.99 * 0.1,
		EffectiveFrom:       time.Date(2026, 9, 1, 0, 0, 0, 0, time.UTC),
		Source:              "user override",
		Billable:            true,
	}
	if err := s.UpsertRate(ctx, custom); err != nil {
		t.Fatalf("upsert: %v", err)
	}

	if _, err := s.Seed(ctx); err != nil {
		t.Fatalf("re-seed: %v", err)
	}

	rates, err := s.Snapshot(ctx)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	for _, r := range rates {
		if r.ModelPattern == "claude-sonnet-5" && r.EffectiveFrom.Equal(custom.EffectiveFrom) {
			if r.InputPerMTok != 9.99 || r.DisplayName != "Claude Sonnet 5 (custom)" || !r.UserModified {
				t.Errorf("user-modified row was clobbered by re-seed: %+v", r)
			}
			return
		}
	}
	t.Fatal("user-modified row missing after re-seed")
}

func TestRevision_ChangesOnInsertUpdateDelete(t *testing.T) {
	s := newStore(t)
	ctx := context.Background()
	if _, err := s.Seed(ctx); err != nil {
		t.Fatalf("seed: %v", err)
	}
	rev, err := s.Revision(ctx)
	if err != nil {
		t.Fatalf("revision: %v", err)
	}

	// Insert a brand-new rate row.
	r := Rate{
		Provider: "anthropic", ModelPattern: "claude-test-model", MatchType: MatchPrefix,
		InputPerMTok: 1, OutputPerMTok: 2, EffectiveFrom: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC),
		Billable: true,
	}
	if err := s.UpsertRate(ctx, r); err != nil {
		t.Fatalf("insert: %v", err)
	}
	revInsert, err := s.Revision(ctx)
	if err != nil {
		t.Fatalf("revision: %v", err)
	}
	if revInsert == rev {
		t.Error("revision unchanged after insert")
	}

	// Update the same row in place.
	r.InputPerMTok = 1.5
	if err := s.UpsertRate(ctx, r); err != nil {
		t.Fatalf("update: %v", err)
	}
	revUpdate, err := s.Revision(ctx)
	if err != nil {
		t.Fatalf("revision: %v", err)
	}
	if revUpdate == revInsert {
		t.Error("revision unchanged after update")
	}

	// Delete it — the revision must return to the seeded value, proving the
	// hash reflects contents rather than MAX(id), which a delete would break.
	if err := s.DeleteRate(ctx, r.ModelPattern, r.EffectiveFrom); err != nil {
		t.Fatalf("delete: %v", err)
	}
	revDelete, err := s.Revision(ctx)
	if err != nil {
		t.Fatalf("revision: %v", err)
	}
	if revDelete != rev {
		t.Errorf("revision after delete = %d, want seeded %d (hash must be content-based)", revDelete, rev)
	}
}

func TestDeleteRate_Unknown(t *testing.T) {
	s := newStore(t)
	if err := s.DeleteRate(context.Background(), "no-such-model", time.Now()); err == nil {
		t.Error("deleting a nonexistent rate should fail")
	}
}

func TestUpsertRate_Validation(t *testing.T) {
	s := newStore(t)
	ctx := context.Background()
	if err := s.UpsertRate(ctx, Rate{EffectiveFrom: time.Now()}); err == nil {
		t.Error("missing model_pattern should fail")
	}
	if err := s.UpsertRate(ctx, Rate{ModelPattern: "x"}); err == nil {
		t.Error("missing effective_from should fail")
	}

	// The write path obeys the same coherence rule as the seed, so the settings
	// UI cannot store a row that prices at $0.00 without saying it means to.
	// Billable's Go zero value is false, which makes the second case the one a
	// caller hits by accident.
	now := time.Now()
	if err := s.UpsertRate(ctx, Rate{
		ModelPattern: "x", EffectiveFrom: now, Billable: true,
	}); err == nil {
		t.Error("billable row with no rates should fail")
	}
	if err := s.UpsertRate(ctx, Rate{
		ModelPattern: "x", EffectiveFrom: now, InputPerMTok: 1, OutputPerMTok: 5,
	}); err == nil {
		t.Error("priced row left non-billable should fail")
	}
}

func TestUpsertRate_NormalizesPattern(t *testing.T) {
	s := newStore(t)
	ctx := context.Background()

	// Seed a lowercase row, then upsert the same rate with a mixed-case
	// pattern: it must land on the same UNIQUE key (SQLite's default UNIQUE
	// is case-sensitive) instead of sitting beside the seed row, and the
	// resolver must find it either way.
	from := time.Date(2026, 8, 1, 0, 0, 0, 0, time.UTC)
	seed := Rate{
		Provider: "anthropic", ModelPattern: "claude-test-case",
		MatchType: MatchPrefix, InputPerMTok: 1, OutputPerMTok: 5,
		EffectiveFrom: from, Source: "test", IsBuiltin: true,
		Billable: true,
	}
	if err := s.UpsertRate(ctx, seed); err != nil {
		t.Fatalf("seed: %v", err)
	}

	mixed := seed
	mixed.ModelPattern = "Claude-Test-CASE"
	mixed.InputPerMTok = 2.5
	mixed.OutputPerMTok = 12
	if err := s.UpsertRate(ctx, mixed); err != nil {
		t.Fatalf("upsert: %v", err)
	}
	after, err := s.Snapshot(ctx)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if len(after) != 1 {
		t.Fatalf("rows = %d, want 1 — mixed-case upsert must collide with the seed row, not sit beside it",
			len(after))
	}
	res := NewResolver(after)
	got, ok := res.Resolve("claude-test-case", from.Add(time.Hour))
	if !ok || got.Rate.InputPerMTok != 2.5 {
		t.Errorf("resolve after upsert = %+v ok=%v, want $2.50", got, ok)
	}

	if err := s.DeleteRate(ctx, "CLAUDE-TEST-CASE", from); err != nil {
		t.Fatalf("delete by different-case spelling: %v", err)
	}
}
