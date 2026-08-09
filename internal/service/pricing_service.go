package service

import (
	"context"
	"fmt"
	"log/slog"
	"sort"
	"strings"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/codes"

	"github.com/shaharia-lab/agento/internal/pricing"
)

// PricedModel is one model in the catalog with the rate currently in force and
// its full rate history, newest first. The UI needs both in one payload: the
// list shows the current rate, expanding a row shows how it got there.
type PricedModel struct {
	ModelPattern string            `json:"model_pattern"`
	Provider     string            `json:"provider"`
	DisplayName  string            `json:"display_name"`
	MatchType    pricing.MatchType `json:"match_type"`
	// Current is the rate governing usage right now — the newest row whose
	// effective_from has already passed. Nil only if every row is future-dated.
	Current *pricing.Rate `json:"current"`
	// Rates is every rate ever recorded for this model, newest first, so a past
	// cost figure can be traced to the rate that produced it.
	Rates []pricing.Rate `json:"rates"`
}

// PricingCatalog is the Model Pricing tab's whole payload.
type PricingCatalog struct {
	Models []PricedModel `json:"models"`
	// UnpricedModels are model IDs seen in real sessions that match no rate.
	// Surfacing them here turns the unknown-pricing bucket from a dead-end
	// diagnostic into the tab's most useful entry point.
	UnpricedModels []string `json:"unpriced_models"`
	// Revision is the catalog fingerprint the costs were last computed under.
	Revision int64 `json:"revision"`
}

// UnpricedModelLister reports model IDs that appeared in scanned sessions but
// matched no rate. Implemented by the Claude session cache; kept as a narrow
// interface here so the pricing service does not depend on that package.
type UnpricedModelLister interface {
	UnpricedModels(ctx context.Context) ([]string, error)
}

// PricingService is the business logic for maintaining the model pricing
// catalog.
//
// The central distinction it enforces is add-versus-correct. Appending a rate
// with a new effective_from leaves history priced at what it was charged;
// correcting one rewrites already-reported costs for that window. Conflating
// them silently rewrites a user's own history, so AddRate refuses to touch an
// existing row and CorrectRate refuses to create one.
type PricingService interface {
	// Catalog returns every model with its current rate and full history.
	Catalog(ctx context.Context) (*PricingCatalog, error)

	// AddRate appends a new effective-dated rate. It fails with a
	// ConflictError if a rate already exists for that model and date —
	// the caller is asking to correct, not to add.
	AddRate(ctx context.Context, r pricing.Rate) (*pricing.Rate, error)

	// CorrectRate edits an existing rate in place, for a value entered in
	// error. It fails with a NotFoundError if no such rate exists — the caller
	// is asking to add, not to correct.
	CorrectRate(ctx context.Context, r pricing.Rate) (*pricing.Rate, error)

	// DeleteRate removes one rate by its (model_pattern, effective_from) key.
	DeleteRate(ctx context.Context, modelPattern string, effectiveFrom time.Time) error
}

type pricingService struct {
	store    *pricing.Store
	unpriced UnpricedModelLister
	logger   *slog.Logger
}

// NewPricingService returns a PricingService over the catalog store. unpriced
// may be nil, in which case the catalog reports no unpriced models rather than
// failing.
func NewPricingService(
	store *pricing.Store, unpriced UnpricedModelLister, logger *slog.Logger,
) PricingService {
	return &pricingService{store: store, unpriced: unpriced, logger: logger}
}

func (s *pricingService) Catalog(ctx context.Context) (*PricingCatalog, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "pricing.catalog")
	defer span.End()

	rates, err := s.store.Snapshot(ctx)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("loading pricing catalog: %w", err)
	}
	rev, err := s.store.Revision(ctx)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, fmt.Errorf("reading pricing revision: %w", err)
	}

	out := &PricingCatalog{Models: groupByModel(rates), Revision: rev}
	out.UnpricedModels = s.listUnpriced(ctx)
	return out, nil
}

// listUnpriced is best-effort: the catalog is still useful without it, so a
// failure is logged rather than failing the whole request.
func (s *pricingService) listUnpriced(ctx context.Context) []string {
	if s.unpriced == nil {
		return []string{}
	}
	models, err := s.unpriced.UnpricedModels(ctx)
	if err != nil {
		s.logger.Warn("pricing: failed to list unpriced models", "error", err)
		return []string{}
	}
	if models == nil {
		return []string{}
	}
	return models
}

// groupByModel folds the flat rate list into one entry per model pattern,
// history newest first, and picks the rate in force now.
//
// "In force" is resolved within the model's own rows rather than through the
// resolver, because the resolver answers for a model *ID* and would happily
// return a different, more specific pattern's rate.
func groupByModel(rates []pricing.Rate) []PricedModel {
	byPattern := map[string][]pricing.Rate{}
	for _, r := range rates {
		byPattern[r.ModelPattern] = append(byPattern[r.ModelPattern], r)
	}

	models := make([]PricedModel, 0, len(byPattern))
	now := time.Now()
	for pattern, group := range byPattern {
		sort.Slice(group, func(i, j int) bool {
			return group[i].EffectiveFrom.After(group[j].EffectiveFrom)
		})
		m := PricedModel{
			ModelPattern: pattern,
			Provider:     group[0].Provider,
			DisplayName:  group[0].DisplayName,
			MatchType:    group[0].MatchType,
			Rates:        group,
		}
		for i := range group {
			if !group[i].EffectiveFrom.After(now) {
				m.Current = &group[i]
				break
			}
		}
		models = append(models, m)
	}

	sort.Slice(models, func(i, j int) bool {
		if models[i].Provider != models[j].Provider {
			return models[i].Provider < models[j].Provider
		}
		return models[i].ModelPattern < models[j].ModelPattern
	})
	return models
}

func (s *pricingService) AddRate(ctx context.Context, r pricing.Rate) (*pricing.Rate, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "pricing.add_rate")
	defer span.End()

	if err := validatePricingRate(&r); err != nil {
		return nil, err
	}
	existing, err := s.findRate(ctx, r.ModelPattern, r.EffectiveFrom)
	if err != nil {
		span.RecordError(err)
		return nil, err
	}
	if existing != nil {
		// Not a bare conflict: the UI offers "you already have a rate from that
		// date — correct it instead?", which needs the row it collided with.
		return existing, &ConflictError{
			Resource: "rate",
			ID:       rateKey(r.ModelPattern, r.EffectiveFrom),
		}
	}
	return s.save(ctx, r)
}

func (s *pricingService) CorrectRate(ctx context.Context, r pricing.Rate) (*pricing.Rate, error) {
	ctx, span := otel.Tracer("agento").Start(ctx, "pricing.correct_rate")
	defer span.End()

	if err := validatePricingRate(&r); err != nil {
		return nil, err
	}
	existing, err := s.findRate(ctx, r.ModelPattern, r.EffectiveFrom)
	if err != nil {
		span.RecordError(err)
		return nil, err
	}
	if existing == nil {
		return nil, &NotFoundError{
			Resource: "rate",
			ID:       rateKey(r.ModelPattern, r.EffectiveFrom),
		}
	}
	// A correction never turns a seeded row into a user-authored one; it marks
	// it user-modified, which is what stops the next startup re-seed from
	// silently restoring the published value.
	r.IsBuiltin = existing.IsBuiltin
	return s.save(ctx, r)
}

// save writes the rate and reads it back, so the caller returns exactly what
// was persisted — including the normalization and user_modified flag the store
// applies on the way in.
func (s *pricingService) save(ctx context.Context, r pricing.Rate) (*pricing.Rate, error) {
	if err := s.store.UpsertRate(ctx, r); err != nil {
		return nil, fmt.Errorf("saving rate: %w", err)
	}
	saved, err := s.findRate(ctx, r.ModelPattern, r.EffectiveFrom)
	if err != nil {
		return nil, err
	}
	if saved == nil {
		return nil, fmt.Errorf("saving rate: %s vanished after write",
			rateKey(r.ModelPattern, r.EffectiveFrom))
	}
	return saved, nil
}

func (s *pricingService) DeleteRate(
	ctx context.Context, modelPattern string, effectiveFrom time.Time,
) error {
	ctx, span := otel.Tracer("agento").Start(ctx, "pricing.delete_rate")
	defer span.End()

	if strings.TrimSpace(modelPattern) == "" {
		return &ValidationError{Field: "model_pattern", Message: "model_pattern is required"}
	}
	if effectiveFrom.IsZero() {
		return &ValidationError{Field: "effective_from", Message: "effective_from is required"}
	}
	effectiveFrom = normalizeEffectiveFrom(effectiveFrom)
	existing, err := s.findRate(ctx, modelPattern, effectiveFrom)
	if err != nil {
		span.RecordError(err)
		return err
	}
	if existing == nil {
		return &NotFoundError{Resource: "rate", ID: rateKey(modelPattern, effectiveFrom)}
	}
	if err := s.store.DeleteRate(ctx, modelPattern, effectiveFrom); err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return fmt.Errorf("deleting rate: %w", err)
	}
	return nil
}

// findRate looks one rate up by its natural key. The store exposes no
// read-by-key, and the catalog is small enough (tens of rows) that scanning the
// snapshot the resolver already loads is cheaper than a second query path.
func (s *pricingService) findRate(
	ctx context.Context, modelPattern string, effectiveFrom time.Time,
) (*pricing.Rate, error) {
	rates, err := s.store.Snapshot(ctx)
	if err != nil {
		return nil, fmt.Errorf("loading pricing catalog: %w", err)
	}
	want := normalizePattern(modelPattern)
	for i := range rates {
		if rates[i].ModelPattern == want &&
			rates[i].EffectiveFrom.Equal(normalizeEffectiveFrom(effectiveFrom)) {
			return &rates[i], nil
		}
	}
	return nil, nil
}

func normalizePattern(p string) string {
	return strings.ToLower(strings.TrimSpace(p))
}

// normalizeEffectiveFrom matches the precision the store round-trips through
// RFC3339, so a lookup by the same instant that was written actually matches.
func normalizeEffectiveFrom(t time.Time) time.Time {
	return t.UTC().Truncate(time.Second)
}

func rateKey(modelPattern string, effectiveFrom time.Time) string {
	return fmt.Sprintf("%s@%s", normalizePattern(modelPattern),
		normalizeEffectiveFrom(effectiveFrom).Format(time.RFC3339))
}

// validatePricingRate enforces the catalog's invariants at the service edge, so
// they surface as 422s with a usable message. The store validates again, but
// its errors are untyped and would collapse into a generic 500.
func validatePricingRate(r *pricing.Rate) error {
	r.ModelPattern = normalizePattern(r.ModelPattern)
	if r.ModelPattern == "" {
		return &ValidationError{Field: "model_pattern", Message: "model_pattern is required"}
	}
	if r.EffectiveFrom.IsZero() {
		return &ValidationError{Field: "effective_from", Message: "effective_from is required"}
	}
	// The store persists effective_from as RFC3339, which has second precision.
	// Normalizing here keeps a written row findable by the value that wrote it —
	// otherwise a sub-second timestamp round-trips to something else and the
	// read-back after a save finds nothing.
	r.EffectiveFrom = normalizeEffectiveFrom(r.EffectiveFrom)
	if r.MatchType == "" {
		r.MatchType = pricing.MatchExact
	}
	if r.MatchType != pricing.MatchExact && r.MatchType != pricing.MatchPrefix {
		return &ValidationError{
			Field:   "match_type",
			Message: `match_type must be "exact" or "prefix"`,
		}
	}

	return validateRateAmounts(r)
}

// validateRateAmounts checks the money, split out so the identity checks above
// stay readable.
func validateRateAmounts(r *pricing.Rate) error {
	amounts := []struct {
		name string
		val  float64
	}{
		{"input_per_mtok", r.InputPerMTok},
		{"output_per_mtok", r.OutputPerMTok},
		{"cache_write_5m_per_mtok", r.CacheWrite5mPerMTok},
		{"cache_write_1h_per_mtok", r.CacheWrite1hPerMTok},
		{"cache_read_per_mtok", r.CacheReadPerMTok},
	}
	nonZero := false
	for _, f := range amounts {
		if f.val < 0 {
			return &ValidationError{Field: f.name, Message: "rate must not be negative"}
		}
		if f.val != 0 {
			nonZero = true
		}
	}

	// The same rule the built-in catalog obeys: a billable model prices every
	// category, a non-billable one prices none. Without it a zeroed-out row
	// reads as free rather than as unfilled.
	if r.Billable && (r.InputPerMTok <= 0 || r.OutputPerMTok <= 0) {
		return &ValidationError{
			Field:   "input_per_mtok",
			Message: "a billable model needs a positive input and output rate",
		}
	}
	if !r.Billable && nonZero {
		return &ValidationError{
			Field:   "billable",
			Message: "a non-billable model must have every rate set to zero",
		}
	}
	return nil
}
