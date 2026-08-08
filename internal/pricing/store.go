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
	effective_from, source, is_builtin, user_modified`

func (s *Store) scanRate(row interface{ Scan(...any) error }) (Rate, error) {
	var r Rate
	var effectiveFrom, source, displayName, provider string
	var isBuiltin, userModified int
	err := row.Scan(
		&r.ID, &provider, &r.ModelPattern, &r.MatchType, &displayName,
		&r.InputPerMTok, &r.OutputPerMTok,
		&r.CacheWrite5mPerMTok, &r.CacheWrite1hPerMTok, &r.CacheReadPerMTok,
		&effectiveFrom, &source, &isBuiltin, &userModified,
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
	return r, nil
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
	return rates, rows.Err()
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
					effective_from, source, is_builtin, user_modified, created_at, updated_at
				) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 0, ?, ?)
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
					updated_at = excluded.updated_at
				WHERE model_pricing.user_modified = 0`,
				r.Provider, r.ModelPattern, r.MatchType, r.DisplayName,
				r.InputPerMTok, r.OutputPerMTok,
				r.CacheWrite5mPerMTok, r.CacheWrite1hPerMTok, r.CacheReadPerMTok,
				r.EffectiveFrom.UTC().Format(time.RFC3339), r.Source,
				time.Now().UTC().Format(time.RFC3339), time.Now().UTC().Format(time.RFC3339),
			)
			if err != nil {
				return written, fmt.Errorf("pricing: seeding %q: %w", r.ModelPattern, err)
			}
			if n, err := res.RowsAffected(); err == nil {
				written += int(n)
			}
		}
	}
	return written, nil
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
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := s.db.ExecContext(ctx, `
		INSERT INTO model_pricing (
			provider, model_pattern, match_type, display_name,
			input_per_mtok, output_per_mtok,
			cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok,
			effective_from, source, is_builtin, user_modified, created_at, updated_at
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)
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
			updated_at = excluded.updated_at`,
		r.Provider, r.ModelPattern, r.MatchType, r.DisplayName,
		r.InputPerMTok, r.OutputPerMTok,
		r.CacheWrite5mPerMTok, r.CacheWrite1hPerMTok, r.CacheReadPerMTok,
		r.EffectiveFrom.UTC().Format(time.RFC3339), r.Source, r.IsBuiltin,
		now, now,
	)
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
