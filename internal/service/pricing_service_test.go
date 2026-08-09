package service

import (
	"context"
	"errors"
	"log/slog"
	"testing"
	"time"

	"github.com/shaharia-lab/agento/internal/pricing"
	"github.com/shaharia-lab/agento/internal/storage"
)

func newPricingSvc(t *testing.T) (PricingService, *pricing.Store) {
	t.Helper()
	db, _, err := storage.NewSQLiteDB(":memory:", slog.Default())
	if err != nil {
		t.Fatalf("open db: %v", err)
	}
	t.Cleanup(func() { _ = db.Close() })

	store := pricing.NewStore(db, slog.Default())
	if _, err := store.Seed(context.Background()); err != nil {
		t.Fatalf("seed: %v", err)
	}
	return NewPricingService(store, nil, slog.Default()), store
}

func aRate(pattern string, from time.Time, in, out float64) pricing.Rate {
	return pricing.Rate{
		Provider: "test", ModelPattern: pattern, MatchType: pricing.MatchExact,
		InputPerMTok: in, OutputPerMTok: out,
		EffectiveFrom: from, Source: "test", Billable: true,
	}
}

// TestAddRate_AppendsWithoutTouchingHistory is the point of the whole feature.
// Adding a rate must leave every earlier rate byte-identical, because those are
// what past usage was costed against.
func TestAddRate_AppendsWithoutTouchingHistory(t *testing.T) {
	svc, store := newPricingSvc(t)
	ctx := context.Background()
	jan := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	jun := time.Date(2026, 6, 1, 0, 0, 0, 0, time.UTC)

	if _, err := svc.AddRate(ctx, aRate("test-model", jan, 1, 5)); err != nil {
		t.Fatalf("first add: %v", err)
	}
	before, err := store.Snapshot(ctx)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}

	if _, err := svc.AddRate(ctx, aRate("test-model", jun, 2, 10)); err != nil {
		t.Fatalf("second add: %v", err)
	}
	after, err := store.Snapshot(ctx)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}

	if len(after) != len(before)+1 {
		t.Fatalf("rows = %d, want %d — adding must append, not replace", len(after), len(before)+1)
	}
	for _, old := range before {
		found := false
		for _, now := range after {
			if now == old {
				found = true
				break
			}
		}
		if !found {
			t.Errorf("rate %q@%s changed when a later rate was added",
				old.ModelPattern, old.EffectiveFrom)
		}
	}

	// The resolver is what proves it: usage before the change keeps its price.
	res := pricing.NewResolver(after)
	got, ok := res.Resolve("test-model", time.Date(2026, 3, 1, 0, 0, 0, 0, time.UTC))
	if !ok || got.Rate.InputPerMTok != 1 {
		t.Errorf("March usage priced at %v, want the January rate of 1", got.Rate.InputPerMTok)
	}
	got, ok = res.Resolve("test-model", time.Date(2026, 7, 1, 0, 0, 0, 0, time.UTC))
	if !ok || got.Rate.InputPerMTok != 2 {
		t.Errorf("July usage priced at %v, want the June rate of 2", got.Rate.InputPerMTok)
	}
}

// TestAddRate_ConflictReturnsExistingRow — the UI needs the colliding row to
// offer "you already have a rate from that date; correct it instead?".
func TestAddRate_ConflictReturnsExistingRow(t *testing.T) {
	svc, _ := newPricingSvc(t)
	ctx := context.Background()
	jan := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)

	if _, err := svc.AddRate(ctx, aRate("test-model", jan, 1, 5)); err != nil {
		t.Fatalf("first add: %v", err)
	}
	existing, err := svc.AddRate(ctx, aRate("test-model", jan, 9, 99))

	var conflict *ConflictError
	if !errors.As(err, &conflict) {
		t.Fatalf("err = %v, want a ConflictError", err)
	}
	if existing == nil {
		t.Fatal("the conflicting row must be returned alongside the error")
	}
	if existing.InputPerMTok != 1 {
		t.Errorf("returned rate = %v, want the stored 1 (not the rejected 9)", existing.InputPerMTok)
	}
}

// TestCorrectRate_RefusesToCreate is the other half of the distinction: a
// correction that matches no row is a mistake, not an insert.
func TestCorrectRate_RefusesToCreate(t *testing.T) {
	svc, store := newPricingSvc(t)
	ctx := context.Background()
	jan := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)

	before, _ := store.Snapshot(ctx)
	_, err := svc.CorrectRate(ctx, aRate("never-seen", jan, 1, 5))

	var notFound *NotFoundError
	if !errors.As(err, &notFound) {
		t.Fatalf("err = %v, want a NotFoundError", err)
	}
	after, _ := store.Snapshot(ctx)
	if len(after) != len(before) {
		t.Error("a failed correction must not create a row")
	}
}

// TestCorrectRate_MarksUserModified is what stops the next startup re-seed from
// silently restoring the published value over a user's correction.
func TestCorrectRate_MarksUserModified(t *testing.T) {
	svc, store := newPricingSvc(t)
	ctx := context.Background()

	// Correct a built-in seeded row rather than a user-added one.
	snapshot, err := store.Snapshot(ctx)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	var seeded pricing.Rate
	for _, r := range snapshot {
		if r.IsBuiltin && r.Billable {
			seeded = r
			break
		}
	}
	if seeded.ModelPattern == "" {
		t.Fatal("expected at least one billable built-in rate in the seed")
	}

	corrected := seeded
	corrected.InputPerMTok = 42
	updated, err := svc.CorrectRate(ctx, corrected)
	if err != nil {
		t.Fatalf("correct: %v", err)
	}
	if !updated.UserModified {
		t.Error("a corrected rate must be marked user_modified")
	}
	if !updated.IsBuiltin {
		t.Error("correcting must not strip is_builtin — the row is still a seeded one")
	}

	// The re-seed must now leave it alone.
	if _, err := store.Seed(ctx); err != nil {
		t.Fatalf("re-seed: %v", err)
	}
	after, err := store.Snapshot(ctx)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	for _, r := range after {
		if r.ModelPattern == seeded.ModelPattern && r.EffectiveFrom.Equal(seeded.EffectiveFrom) {
			if r.InputPerMTok != 42 {
				t.Errorf("re-seed clobbered the correction: input = %v, want 42", r.InputPerMTok)
			}
			return
		}
	}
	t.Error("the corrected row vanished after a re-seed")
}

func TestValidatePricingRate(t *testing.T) {
	jan := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	tests := []struct {
		name      string
		rate      pricing.Rate
		wantField string
	}{
		{"missing model_pattern", pricing.Rate{EffectiveFrom: jan}, "model_pattern"},
		{"missing effective_from", pricing.Rate{ModelPattern: "x"}, "effective_from"},
		{
			"unknown match_type",
			pricing.Rate{ModelPattern: "x", EffectiveFrom: jan, MatchType: "fuzzy"},
			"match_type",
		},
		{
			"negative rate",
			pricing.Rate{
				ModelPattern: "x", EffectiveFrom: jan, MatchType: pricing.MatchExact,
				InputPerMTok: -1, OutputPerMTok: 5, Billable: true,
			},
			"input_per_mtok",
		},
		{
			"billable with no rates",
			pricing.Rate{
				ModelPattern: "x", EffectiveFrom: jan, MatchType: pricing.MatchExact,
				Billable: true,
			},
			"input_per_mtok",
		},
		{
			"non-billable still priced",
			pricing.Rate{
				ModelPattern: "x", EffectiveFrom: jan, MatchType: pricing.MatchExact,
				InputPerMTok: 1, OutputPerMTok: 5,
			},
			"billable",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			r := tt.rate
			err := validatePricingRate(&r)
			var ve *ValidationError
			if !errors.As(err, &ve) {
				t.Fatalf("err = %v, want a ValidationError", err)
			}
			if ve.Field != tt.wantField {
				t.Errorf("field = %q, want %q", ve.Field, tt.wantField)
			}
		})
	}
}

// TestValidatePricingRate_DefaultsMatchType keeps the wire contract forgiving:
// an omitted match_type means exact, matching the built-in catalog's own rule.
func TestValidatePricingRate_DefaultsMatchType(t *testing.T) {
	r := aRate("x", time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC), 1, 5)
	r.MatchType = ""
	if err := validatePricingRate(&r); err != nil {
		t.Fatalf("validate: %v", err)
	}
	if r.MatchType != pricing.MatchExact {
		t.Errorf("match_type = %q, want %q", r.MatchType, pricing.MatchExact)
	}
}

// TestCatalog_GroupsByModelWithCurrentRate checks the shape the tab renders:
// history newest first, and "current" skipping a future-dated row.
func TestCatalog_GroupsByModelWithCurrentRate(t *testing.T) {
	svc, _ := newPricingSvc(t)
	ctx := context.Background()
	past := time.Now().AddDate(0, -1, 0)
	future := time.Now().AddDate(0, 1, 0)

	if _, err := svc.AddRate(ctx, aRate("test-model", past, 1, 5)); err != nil {
		t.Fatalf("add past: %v", err)
	}
	if _, err := svc.AddRate(ctx, aRate("test-model", future, 2, 10)); err != nil {
		t.Fatalf("add future: %v", err)
	}

	catalog, err := svc.Catalog(ctx)
	if err != nil {
		t.Fatalf("catalog: %v", err)
	}
	var model *PricedModel
	for i := range catalog.Models {
		if catalog.Models[i].ModelPattern == "test-model" {
			model = &catalog.Models[i]
			break
		}
	}
	if model == nil {
		t.Fatal("test-model missing from the catalog")
	}
	if len(model.Rates) != 2 {
		t.Fatalf("rates = %d, want 2", len(model.Rates))
	}
	if !model.Rates[0].EffectiveFrom.After(model.Rates[1].EffectiveFrom) {
		t.Error("rate history must be newest first")
	}
	if model.Current == nil || model.Current.InputPerMTok != 1 {
		t.Error("current rate must be the newest one already in force, not the future one")
	}
	if catalog.Revision == 0 {
		t.Error("catalog must carry the pricing revision")
	}
}
