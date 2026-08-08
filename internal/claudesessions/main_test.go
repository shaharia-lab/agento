package claudesessions

import (
	"context"
	"testing"

	"github.com/shaharia-lab/agento/internal/pricing"
	"github.com/shaharia-lab/agento/internal/storage"
)

// TestMain wires the process-wide pricing resolver for the whole test binary.
// Cost paths are inert without it (the pre-#186 "no stored cost" behavior), so
// seeding the built-in catalog here keeps pricing assertions deterministic
// without every test constructing its own catalog.
func TestMain(m *testing.M) {
	db, _, err := storage.NewSQLiteDB(":memory:", testLogger)
	if err != nil {
		panic("opening pricing test database: " + err.Error())
	}
	defer func() { _ = db.Close() }()
	store := pricing.NewStore(db, testLogger)
	if _, err := store.Seed(context.Background()); err != nil {
		panic("seeding pricing catalog: " + err.Error())
	}
	rates, err := store.Snapshot(context.Background())
	if err != nil {
		panic("loading pricing snapshot: " + err.Error())
	}
	rev, err := store.Revision(context.Background())
	if err != nil {
		panic("reading pricing revision: " + err.Error())
	}
	packagePricing.Lock()
	packagePricing.resolver = pricing.NewResolver(rates)
	packagePricing.revision = rev
	packagePricing.Unlock()
	m.Run()
}
