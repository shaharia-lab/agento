package pricing

import (
	"context"
	"database/sql"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/fnv"
	"log/slog"
	"math"
	"strings"
	"time"
)

// Store persists the pricing catalog in SQLite. Rates are effective-dated:
// the uniqueness key is (model_pattern, effective_from), so a price change is
// a new row, never an edit of the row history was priced against.
type Store struct {
	db     *sql.DB
	logger *slog.Logger
}

// NewStore wraps an open SQLite database that owns the model_pricing table.
func NewStore(db *sql.DB, logger *slog.Logger) *Store {
	return &Store{db: db, logger: logger}
}

const rateColumns = `id, provider, model_pattern, match_type, display_name,
	input_per_mtok, output_per_mtok,
	cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok,
	effective_from, source, is_builtin, user_modified, billable, estimated`

func (s *Store) scanRate(row interface{ Scan(...any) error }) (Rate, error) {
	var r Rate
	var effectiveFrom, source, displayName, provider string
	var isBuiltin, userModified, billable, estimated int
	err := row.Scan(
		&r.ID, &provider, &r.ModelPattern, &r.MatchType, &displayName,
		&r.InputPerMTok, &r.OutputPerMTok,
		&r.CacheWrite5mPerMTok, &r.CacheWrite1hPerMTok, &r.CacheReadPerMTok,
		&effectiveFrom, &source, &isBuiltin, &userModified, &billable, &estimated,
	)
	if err != nil {
		return Rate{}, err
	}
	t, err := time.Parse(time.RFC3339, effectiveFrom)
	if err != nil {
		return Rate{}, fmt.Errorf("pricing: rate %d: unparsable effective_from %q: %w", r.ID, effectiveFrom, err)
	}
	r.Provider, r.DisplayName, r.EffectiveFrom, r.Source = provider, displayName, t, source
	r.IsBuiltin, r.UserModified = isBuiltin == 1, userModified == 1
	r.Billable, r.Estimated = billable == 1, estimated == 1
	return r, nil
}

// boolToByte renders a flag as the single hash byte the revision fingerprint
// mixes in. Writes pass bools straight to the driver, which stores them as
// INTEGER — this exists only because the hash needs raw bytes.
func boolToByte(b bool) byte {
	if b {
		return 1
	}
	return 0
}

// Snapshot loads every rate row, ordered for deterministic downstream
// iteration (and hashing). This is the resolver's input.
func (s *Store) Snapshot(ctx context.Context) ([]Rate, error) {
	rows, err := s.db.QueryContext(ctx,
		`SELECT `+rateColumns+` FROM model_pricing
		 ORDER BY model_pattern, match_type, effective_from`)
	if err != nil {
		return nil, err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			s.logger.Warn("pricing: failed to close rows", "error", cerr)
		}
	}()

	rates := []Rate{}
	for rows.Next() {
		r, err := s.scanRate(rows)
		if err != nil {
			return nil, err
		}
		rates = append(rates, r)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}
	if err := s.attachTiers(ctx, rates); err != nil {
		return nil, err
	}
	return rates, nil
}

// attachTiers loads every context-length band in ONE query and distributes
// them by rate_id. Snapshot feeds the process-wide resolver, so a per-rate
// query here would be an N+1 on the path every cost figure goes through.
// Ordering by max_input_tokens is what lets tierFor stop at the first match.
func (s *Store) attachTiers(ctx context.Context, rates []Rate) error {
	if len(rates) == 0 {
		return nil
	}
	byID := make(map[int64]*Rate, len(rates))
	for i := range rates {
		byID[rates[i].ID] = &rates[i]
	}
	rows, err := s.db.QueryContext(ctx, `
		SELECT rate_id, max_input_tokens,
			input_per_mtok, output_per_mtok,
			cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok
		FROM model_pricing_tier
		ORDER BY rate_id, max_input_tokens`)
	if err != nil {
		return err
	}
	defer func() {
		if cerr := rows.Close(); cerr != nil {
			s.logger.Warn("pricing: failed to close tier rows", "error", cerr)
		}
	}()

	for rows.Next() {
		var rateID int64
		var t TierRate
		if err := rows.Scan(&rateID, &t.MaxInputTokens,
			&t.InputPerMTok, &t.OutputPerMTok,
			&t.CacheWrite5mPerMTok, &t.CacheWrite1hPerMTok, &t.CacheReadPerMTok); err != nil {
			return err
		}
		if r, ok := byID[rateID]; ok {
			r.Tiers = append(r.Tiers, t)
		}
	}
	return rows.Err()
}

// Revision is a stable fingerprint of the catalog's contents: FNV-1a over the
// fully-ordered rows, so any insert, update, or delete changes it, while a
// mere row re-ordering (or a vacuous rewrite) does not. Cached cost figures
// record the revision they were computed under; a drift triggers a re-cost.
func (s *Store) Revision(ctx context.Context) (int64, error) {
	rates, err := s.Snapshot(ctx)
	if err != nil {
		return 0, err
	}
	h := fnv.New64a()
	var buf [8]byte
	writeFloat := func(f float64) {
		binary.LittleEndian.PutUint64(buf[:], math.Float64bits(f))
		_, _ = h.Write(buf[:])
	}
	for _, r := range rates {
		_, _ = h.Write([]byte(r.ModelPattern))
		_, _ = h.Write([]byte{0})
		_, _ = h.Write([]byte(r.MatchType))
		_, _ = h.Write([]byte{0})
		_, _ = h.Write([]byte(r.Provider))
		_, _ = h.Write([]byte{0})
		_, _ = h.Write([]byte(r.EffectiveFrom.UTC().Format(time.RFC3339Nano)))
		writeFloat(r.InputPerMTok)
		writeFloat(r.OutputPerMTok)
		writeFloat(r.CacheWrite5mPerMTok)
		writeFloat(r.CacheWrite1hPerMTok)
		writeFloat(r.CacheReadPerMTok)
		// Both flags change what a lookup means, not just how it displays:
		// billable decides whether tokens land in the unknown bucket, and
		// estimated qualifies the figure. Toggling either must re-cost.
		_, _ = h.Write([]byte{boolToByte(r.Billable), boolToByte(r.Estimated)})
		// Tiers are prices too, so a band edit has to move the fingerprint —
		// otherwise #188's stored per-session costs keep the pre-edit figure
		// forever and nothing anywhere signals it. Snapshot returns bands in
		// ascending order, so this is deterministic.
		for _, t := range r.Tiers {
			binary.LittleEndian.PutUint64(buf[:], uint64(t.MaxInputTokens)) // #nosec G115 -- bounds are small positive ints.
			_, _ = h.Write(buf[:])
			writeFloat(t.InputPerMTok)
			writeFloat(t.OutputPerMTok)
			writeFloat(t.CacheWrite5mPerMTok)
			writeFloat(t.CacheWrite1hPerMTok)
			writeFloat(t.CacheReadPerMTok)
		}
	}
	// Keep the value non-negative so it survives SQLite integer round-trips.
	// #nosec G115 -- the mask clears the sign bit before the conversion.
	return int64(h.Sum64() & 0x7FFFFFFFFFFFFFFF), nil
}

// Seed inserts the built-in catalog. It is idempotent: rows are upserted on
// the (model_pattern, effective_from) key, and any row the user has modified
// is left untouched — a rate correction must never clobber a deliberate
// override. Returns the number of rows actually written.
func (s *Store) Seed(ctx context.Context) (int, error) {
	written := 0
	for _, entry := range BuiltinCatalog() {
		rates, err := entry.rates()
		if err != nil {
			return written, err
		}
		for _, r := range rates {
			res, err := s.db.ExecContext(ctx, `
				INSERT INTO model_pricing (
					provider, model_pattern, match_type, display_name,
					input_per_mtok, output_per_mtok,
					cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok,
					effective_from, source, is_builtin, user_modified, billable, estimated,
					created_at, updated_at
				) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 0, ?, ?, ?, ?)
				ON CONFLICT(model_pattern, effective_from) DO UPDATE SET
					provider = excluded.provider,
					match_type = excluded.match_type,
					display_name = excluded.display_name,
					input_per_mtok = excluded.input_per_mtok,
					output_per_mtok = excluded.output_per_mtok,
					cache_write_5m_per_mtok = excluded.cache_write_5m_per_mtok,
					cache_write_1h_per_mtok = excluded.cache_write_1h_per_mtok,
					cache_read_per_mtok = excluded.cache_read_per_mtok,
					source = excluded.source,
					billable = excluded.billable,
					estimated = excluded.estimated,
					updated_at = excluded.updated_at
				WHERE model_pricing.user_modified = 0`,
				r.Provider, r.ModelPattern, r.MatchType, r.DisplayName,
				r.InputPerMTok, r.OutputPerMTok,
				r.CacheWrite5mPerMTok, r.CacheWrite1hPerMTok, r.CacheReadPerMTok,
				r.EffectiveFrom.UTC().Format(time.RFC3339), r.Source,
				r.Billable, r.Estimated,
				time.Now().UTC().Format(time.RFC3339), time.Now().UTC().Format(time.RFC3339),
			)
			if err != nil {
				return written, fmt.Errorf("pricing: seeding %q: %w", r.ModelPattern, err)
			}
			if n, err := res.RowsAffected(); err == nil {
				written += int(n)
			}
			if err := s.seedTiers(ctx, r); err != nil {
				return written, fmt.Errorf("pricing: seeding tiers for %q: %w", r.ModelPattern, err)
			}
		}
	}
	return written, nil
}

// seedTiers brings one seeded rate's bands in line with the catalog. It
// upserts the declared bands and only then deletes the ones no longer
// declared, rather than delete-then-insert: Seed runs without a surrounding
// transaction, so a delete-first order would leave a crash-interrupted startup
// with a tiered rate carrying zero bands — which prices silently at the base
// tier, the exact under-reporting this table exists to end.
//
// A user-modified rate is skipped entirely, matching the rate upsert's
// WHERE user_modified = 0 guard: re-seeding must never overwrite a deliberate
// override, and that has to include its bands.
func (s *Store) seedTiers(ctx context.Context, r Rate) error {
	var rateID int64
	err := s.db.QueryRowContext(ctx,
		`SELECT id FROM model_pricing
		 WHERE model_pattern = ? AND effective_from = ? AND user_modified = 0`,
		r.ModelPattern, r.EffectiveFrom.UTC().Format(time.RFC3339)).Scan(&rateID)
	if errors.Is(err, sql.ErrNoRows) {
		return nil // user-modified row: leave it and its bands alone.
	}
	if err != nil {
		return err
	}

	keep := make([]any, 0, len(r.Tiers))
	for _, t := range r.Tiers {
		if _, err := s.db.ExecContext(ctx, `
			INSERT INTO model_pricing_tier (
				rate_id, max_input_tokens,
				input_per_mtok, output_per_mtok,
				cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok
			) VALUES (?, ?, ?, ?, ?, ?, ?)
			ON CONFLICT(rate_id, max_input_tokens) DO UPDATE SET
				input_per_mtok = excluded.input_per_mtok,
				output_per_mtok = excluded.output_per_mtok,
				cache_write_5m_per_mtok = excluded.cache_write_5m_per_mtok,
				cache_write_1h_per_mtok = excluded.cache_write_1h_per_mtok,
				cache_read_per_mtok = excluded.cache_read_per_mtok`,
			rateID, t.MaxInputTokens,
			t.InputPerMTok, t.OutputPerMTok,
			t.CacheWrite5mPerMTok, t.CacheWrite1hPerMTok, t.CacheReadPerMTok); err != nil {
			return err
		}
		keep = append(keep, t.MaxInputTokens)
	}

	// Prune bands the catalog dropped. With no bands declared this clears them
	// all, so a rate that stops being tiered stops being tiered here too.
	q := `DELETE FROM model_pricing_tier WHERE rate_id = ?`
	args := []any{rateID}
	if len(keep) > 0 {
		q += ` AND max_input_tokens NOT IN (?` + strings.Repeat(`, ?`, len(keep)-1) + `)`
		args = append(args, keep...)
	}
	_, err = s.db.ExecContext(ctx, q, args...)
	return err
}

// UpsertRate inserts or replaces one rate row, marking it user-modified so
// future re-seeds leave it alone. This is the write path the settings UI
// (#189) will use; editing an existing rate in place is allowed here because
// user intent overrides the append-only rule that governs the built-in seed.
// The pattern is normalized to lowercase so the UNIQUE key collides with the
// seed's rows (SQLite's default UNIQUE is case-sensitive) and the resolver's
// lowercased comparison can find the row.
func (s *Store) UpsertRate(ctx context.Context, r Rate) error {
	if r.ModelPattern == "" {
		return errors.New("pricing: model_pattern is required")
	}
	if r.EffectiveFrom.IsZero() {
		return errors.New("pricing: effective_from is required")
	}
	r.ModelPattern = strings.ToLower(strings.TrimSpace(r.ModelPattern))
	// The same coherence rule the built-in seed obeys. Without it this path —
	// the one the settings UI writes through — could store a billable row with
	// no rates, or a non-billable row that still carries prices, which is
	// precisely the silent $0 the Billable flag exists to rule out.
	if err := validateRate(r.ModelPattern, r); err != nil {
		return err
	}
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO model_pricing (
			provider, model_pattern, match_type, display_name,
			input_per_mtok, output_per_mtok,
			cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok,
			effective_from, source, is_builtin, user_modified, billable, estimated,
			created_at, updated_at
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?)
		ON CONFLICT(model_pattern, effective_from) DO UPDATE SET
			provider = excluded.provider,
			match_type = excluded.match_type,
			display_name = excluded.display_name,
			input_per_mtok = excluded.input_per_mtok,
			output_per_mtok = excluded.output_per_mtok,
			cache_write_5m_per_mtok = excluded.cache_write_5m_per_mtok,
			cache_write_1h_per_mtok = excluded.cache_write_1h_per_mtok,
			cache_read_per_mtok = excluded.cache_read_per_mtok,
			source = excluded.source,
			is_builtin = excluded.is_builtin,
			user_modified = 1,
			billable = excluded.billable,
			estimated = excluded.estimated,
			updated_at = excluded.updated_at`,
		r.Provider, r.ModelPattern, r.MatchType, r.DisplayName,
		r.InputPerMTok, r.OutputPerMTok,
		r.CacheWrite5mPerMTok, r.CacheWrite1hPerMTok, r.CacheReadPerMTok,
		r.EffectiveFrom.UTC().Format(time.RFC3339), r.Source, r.IsBuiltin,
		r.Billable, r.Estimated,
		now, now,
	)
	if err != nil {
		return err
	}
	// A rate written here is flat, so its catalog bands must go.
	//
	// This is not housekeeping: Price selects a band before applying any price,
	// so leaving the seeded bands in place would make the user's new figures
	// unreachable at every request size — the edit would appear to save and
	// then change nothing. The settings form cannot express bands, so entering
	// a price here is an assertion that this is *the* price, and user intent
	// overriding the built-in catalog is the same rule user_modified already
	// encodes everywhere else.
	_, err = s.db.ExecContext(ctx, `
		DELETE FROM model_pricing_tier
		WHERE rate_id = (SELECT id FROM model_pricing
		                 WHERE model_pattern = ? AND effective_from = ?)`,
		r.ModelPattern, r.EffectiveFrom.UTC().Format(time.RFC3339))
	return err
}

// DeleteRate removes the row with the given (model_pattern, effective_from)
// key. Deleting a rate changes the revision and therefore triggers a re-cost
// of the sessions that were priced with it. The pattern is lowercased to match
// how rows are stored.
func (s *Store) DeleteRate(ctx context.Context, modelPattern string, effectiveFrom time.Time) error {
	res, err := s.db.ExecContext(ctx,
		`DELETE FROM model_pricing WHERE model_pattern = ? AND effective_from = ?`,
		strings.ToLower(strings.TrimSpace(modelPattern)), effectiveFrom.UTC().Format(time.RFC3339))
	if err != nil {
		return err
	}
	n, err := res.RowsAffected()
	if err != nil {
		return err
	}
	if n == 0 {
		return fmt.Errorf("pricing: no rate for %q at %s", modelPattern, effectiveFrom.Format(time.RFC3339))
	}
	return nil
}
