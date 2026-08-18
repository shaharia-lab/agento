//! The built-in pricing catalog seed, ported from Go for the cut-over (#278).
//!
//! Go's `NewSQLiteDB` ran `pricing.Store.Seed` on every startup: parse the
//! embedded `catalog.json`, upsert each rate on `(model_pattern,
//! effective_from)` without ever clobbering a `user_modified` row, and bring
//! each rate's tier bands in line. With the sidecar gone nothing else performs
//! that startup effect — a fresh install would have an empty `model_pricing`
//! table and every session would cost an unknown $0, and a build shipping a
//! corrected catalog would never reach existing installs. So the shell seeds,
//! right after `migrate::apply`, from **the same** `internal/pricing/catalog.json`
//! the Go server embeds — one file, two consumers, no second copy to drift.
//!
//! The normalization rules are `catalog.go`'s, pinned cross-language by
//! `desktop/parity/pricing_seed_vectors.json` (generated from Go's
//! `BuiltinCatalog` by `go test ./desktop/parity/ -run TestPricingSeed
//! -update-pricing-seed`): absent cache rates derive from input × the
//! Anthropic TTL multipliers (5m 1.25×, 1h 2×, read 0.1×), an absent
//! `effective_from` is the far past, patterns are lowercased, `match_type`
//! defaults to `exact`, and a billable rate must price every category above
//! zero — a half-filled entry fails rather than silently under-reporting.
//!
//! No revision bookkeeping happens here on purpose: `pricing::revision_of` is
//! a hash over the rows, so a seed that changes anything moves `pricing_rev`
//! by construction and the scanner's staleness gate re-prices stored costs,
//! exactly as it does after a rate edit.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// The embedded seed — the Go server's own file, not a copy.
const CATALOG: &str = include_str!("../../../../internal/pricing/catalog.json");

/// `farPast` in `catalog.go`: stamps models with a single, never-changed rate.
const FAR_PAST: &str = "2020-01-01T00:00:00Z";

#[derive(Deserialize)]
struct SeedEntry {
    provider: String,
    model_pattern: String,
    #[serde(default)]
    match_type: String,
    display_name: String,
    source: String,
    rates: Vec<SeedPrice>,
    /// Defaults to true when absent — every real model is billable, so only
    /// the exceptions say so. `false` is the only way to seed all-zero rates.
    billable: Option<bool>,
    #[serde(default)]
    estimated: bool,
}

#[derive(Deserialize)]
struct SeedPrice {
    #[serde(default)]
    effective_from: String,
    input: f64,
    output: f64,
    cache_write_5m: Option<f64>,
    cache_write_1h: Option<f64>,
    cache_read: Option<f64>,
    #[serde(default)]
    tiers: Vec<SeedTier>,
}

#[derive(Deserialize)]
struct SeedTier {
    max_input_tokens: i64,
    input: f64,
    output: f64,
    cache_write_5m: Option<f64>,
    cache_write_1h: Option<f64>,
    cache_read: Option<f64>,
}

/// One flattened, normalized seed row — what `builtinEntry.rates()` returns in
/// Go, and the shape `pricing_seed_vectors.json` pins.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SeedRate {
    pub provider: String,
    pub model_pattern: String,
    pub match_type: String,
    pub display_name: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_write_5m_per_mtok: f64,
    pub cache_write_1h_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub effective_from: String,
    pub source: String,
    pub billable: bool,
    pub estimated: bool,
    pub tiers: Vec<SeedTierRate>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SeedTierRate {
    pub max_input_tokens: i64,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_write_5m_per_mtok: f64,
    pub cache_write_1h_per_mtok: f64,
    pub cache_read_per_mtok: f64,
}

/// `orDerive`: the explicit override when present, else input × mult.
fn or_derive(override_: Option<f64>, input: f64, mult: f64) -> f64 {
    override_.unwrap_or(input * mult)
}

/// Parse and normalize the embedded catalog — `BuiltinCatalog` + `rates()`.
///
/// An error here is an authoring error in a file this binary embeds, so it is
/// a build-time fact: the unit tests parse the same bytes and fail first.
pub fn builtin_rates() -> Result<Vec<SeedRate>, String> {
    let entries: Vec<SeedEntry> = serde_json::from_str(CATALOG)
        .map_err(|e| format!("pricing: invalid embedded catalog: {e}"))?;
    let mut out = Vec::new();
    for e in entries {
        let billable = e.billable.unwrap_or(true);
        let match_type = if e.match_type.is_empty() {
            "exact".to_string()
        } else {
            e.match_type.clone()
        };
        for p in &e.rates {
            let from = if p.effective_from.is_empty() {
                FAR_PAST
            } else {
                &p.effective_from
            };
            // Go parses with time.RFC3339 and re-renders on insert; the values
            // in the catalog are already canonical RFC 3339 UTC, so a parse
            // here is a validity check rather than a conversion.
            if chrono::DateTime::parse_from_rfc3339(from).is_err() {
                return Err(format!(
                    "pricing: catalog entry {:?}: bad effective_from {from:?}",
                    e.model_pattern
                ));
            }
            let rate = SeedRate {
                provider: e.provider.clone(),
                model_pattern: e.model_pattern.to_lowercase(),
                match_type: match_type.clone(),
                display_name: e.display_name.clone(),
                input_per_mtok: p.input,
                output_per_mtok: p.output,
                cache_write_5m_per_mtok: or_derive(p.cache_write_5m, p.input, 1.25),
                cache_write_1h_per_mtok: or_derive(p.cache_write_1h, p.input, 2.0),
                cache_read_per_mtok: or_derive(p.cache_read, p.input, 0.1),
                effective_from: from.to_string(),
                source: e.source.clone(),
                billable,
                estimated: e.estimated,
                tiers: p
                    .tiers
                    .iter()
                    .map(|t| SeedTierRate {
                        max_input_tokens: t.max_input_tokens,
                        input_per_mtok: t.input,
                        output_per_mtok: t.output,
                        cache_write_5m_per_mtok: or_derive(t.cache_write_5m, t.input, 1.25),
                        cache_write_1h_per_mtok: or_derive(t.cache_write_1h, t.input, 2.0),
                        cache_read_per_mtok: or_derive(t.cache_read, t.input, 0.1),
                    })
                    .collect(),
            };
            validate(&rate)?;
            out.push(rate);
        }
    }
    Ok(out)
}

/// `validateRate` + `validateTiers`: the invariant that makes a $0.00 row
/// meaningful. A billable model prices every category above zero; a
/// non-billable one prices them all at exactly zero and declares no tiers.
fn validate(r: &SeedRate) -> Result<(), String> {
    let cols = [
        r.input_per_mtok,
        r.output_per_mtok,
        r.cache_write_5m_per_mtok,
        r.cache_write_1h_per_mtok,
        r.cache_read_per_mtok,
    ];
    if !r.billable {
        if cols.iter().any(|v| *v != 0.0) {
            return Err(format!(
                "pricing: rate {:?}: non-billable rates must all be zero",
                r.model_pattern
            ));
        }
        if !r.tiers.is_empty() {
            return Err(format!(
                "pricing: rate {:?}: non-billable rates must not declare tiers",
                r.model_pattern
            ));
        }
        return Ok(());
    }
    if r.input_per_mtok <= 0.0 || r.output_per_mtok <= 0.0 {
        return Err(format!(
            "pricing: rate {:?}: rates must be positive",
            r.model_pattern
        ));
    }
    if cols.iter().any(|v| *v < 0.0) {
        return Err(format!(
            "pricing: rate {:?}: cache rates must not be negative",
            r.model_pattern
        ));
    }
    if let Some(lo) = r.tiers.first() {
        let eps = 1e-12;
        if (lo.input_per_mtok - r.input_per_mtok).abs() >= eps
            || (lo.output_per_mtok - r.output_per_mtok).abs() >= eps
        {
            return Err(format!(
                "pricing: rate {:?}: the flat columns must equal the lowest tier",
                r.model_pattern
            ));
        }
    }
    let mut prev = 0i64;
    for (i, t) in r.tiers.iter().enumerate() {
        if t.max_input_tokens <= prev {
            return Err(format!(
                "pricing: rate {:?}: tier {i}: max_input_tokens must ascend",
                r.model_pattern
            ));
        }
        prev = t.max_input_tokens;
        if t.input_per_mtok <= 0.0 || t.output_per_mtok <= 0.0 {
            return Err(format!(
                "pricing: rate {:?}: tier {i}: rates must be positive",
                r.model_pattern
            ));
        }
        if t.cache_write_5m_per_mtok < 0.0
            || t.cache_write_1h_per_mtok < 0.0
            || t.cache_read_per_mtok < 0.0
        {
            return Err(format!(
                "pricing: rate {:?}: tier {i}: cache rates must not be negative",
                r.model_pattern
            ));
        }
    }
    Ok(())
}

/// `Store.Seed` + `Store.seedTiers`, statement for statement.
///
/// Upsert on `(model_pattern, effective_from)`, guarded by
/// `user_modified = 0` so a re-seed never overwrites a deliberate override —
/// including its bands, because `seed_tiers`' own `user_modified = 0` lookup
/// skips such a row entirely. Bands are upserted **then** pruned, never
/// delete-first: there is no surrounding transaction (matching Go), and a
/// crash between a delete and the re-insert would leave a tiered rate silently
/// priced at its base band.
pub fn seed(conn: &Connection) -> Result<usize, String> {
    let mut written = 0usize;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    for r in builtin_rates()? {
        let n = conn
            .execute(
                "INSERT INTO model_pricing (
                    provider, model_pattern, match_type, display_name,
                    input_per_mtok, output_per_mtok,
                    cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok,
                    effective_from, source, is_builtin, user_modified, billable, estimated,
                    created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, 0, ?12, ?13, ?14, ?15)
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
                WHERE model_pricing.user_modified = 0",
                rusqlite::params![
                    r.provider,
                    r.model_pattern,
                    r.match_type,
                    r.display_name,
                    r.input_per_mtok,
                    r.output_per_mtok,
                    r.cache_write_5m_per_mtok,
                    r.cache_write_1h_per_mtok,
                    r.cache_read_per_mtok,
                    r.effective_from,
                    r.source,
                    r.billable,
                    r.estimated,
                    now,
                    now,
                ],
            )
            .map_err(|e| format!("pricing: seeding {:?}: {e}", r.model_pattern))?;
        written += n;
        seed_tiers(conn, &r)
            .map_err(|e| format!("pricing: seeding tiers for {:?}: {e}", r.model_pattern))?;
    }
    Ok(written)
}

fn seed_tiers(conn: &Connection, r: &SeedRate) -> Result<(), String> {
    use rusqlite::OptionalExtension;
    let rate_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM model_pricing
             WHERE model_pattern = ?1 AND effective_from = ?2 AND user_modified = 0",
            rusqlite::params![r.model_pattern, r.effective_from],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    // User-modified row: leave it and its bands alone.
    let Some(rate_id) = rate_id else {
        return Ok(());
    };

    for t in &r.tiers {
        conn.execute(
            "INSERT INTO model_pricing_tier (
                rate_id, max_input_tokens,
                input_per_mtok, output_per_mtok,
                cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(rate_id, max_input_tokens) DO UPDATE SET
                input_per_mtok = excluded.input_per_mtok,
                output_per_mtok = excluded.output_per_mtok,
                cache_write_5m_per_mtok = excluded.cache_write_5m_per_mtok,
                cache_write_1h_per_mtok = excluded.cache_write_1h_per_mtok,
                cache_read_per_mtok = excluded.cache_read_per_mtok",
            rusqlite::params![
                rate_id,
                t.max_input_tokens,
                t.input_per_mtok,
                t.output_per_mtok,
                t.cache_write_5m_per_mtok,
                t.cache_write_1h_per_mtok,
                t.cache_read_per_mtok,
            ],
        )
        .map_err(|e| e.to_string())?;
    }

    // Prune bands the catalog dropped. With none declared this clears them
    // all, so a rate that stops being tiered stops being tiered here too.
    let keep: Vec<String> = r
        .tiers
        .iter()
        .map(|t| t.max_input_tokens.to_string())
        .collect();
    let mut q = "DELETE FROM model_pricing_tier WHERE rate_id = ?1".to_string();
    if !keep.is_empty() {
        q.push_str(&format!(
            " AND max_input_tokens NOT IN ({})",
            keep.join(", ")
        ));
    }
    conn.execute(&q, [rate_id]).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_conn() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        seed(&conn).expect("seed");
        conn
    }

    /// The flattened seed must match what Go's `BuiltinCatalog().rates()`
    /// produces — the vector is generated from Go
    /// (`go test ./desktop/parity/ -run TestPricingSeed -update-pricing-seed`)
    /// and both languages assert against it, so a drift in either port of the
    /// normalization rules fails one side's tests.
    #[test]
    fn the_flattened_seed_matches_gos_vector() {
        let vectors: Vec<SeedRate> =
            serde_json::from_str(include_str!("../../../parity/pricing_seed_vectors.json"))
                .expect("parsing pricing_seed_vectors.json — regenerate it from Go");
        let ours = builtin_rates().expect("builtin catalog parses");
        assert_eq!(
            ours.len(),
            vectors.len(),
            "seed row count differs from Go's"
        );
        for (a, b) in ours.iter().zip(vectors.iter()) {
            assert_eq!(a, b, "seed row for {:?} differs from Go's", b.model_pattern);
        }
    }

    #[test]
    fn seeding_is_idempotent_and_preserves_user_modified_rows() {
        let conn = seeded_conn();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM model_pricing", [], |r| r.get(0))
            .expect("count");
        assert!(count > 0, "the seed must write rows into an empty table");

        // A deliberate user override on a seeded row…
        conn.execute(
            "UPDATE model_pricing SET input_per_mtok = 99.0, user_modified = 1
             WHERE model_pattern = 'claude-opus-5'",
            [],
        )
        .expect("override");

        // …survives a re-seed untouched, while the row count is unchanged.
        seed(&conn).expect("re-seed");
        let count_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM model_pricing", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, count_after, "a re-seed must not duplicate rows");
        let overridden: f64 = conn
            .query_row(
                "SELECT input_per_mtok FROM model_pricing
                 WHERE model_pattern = 'claude-opus-5' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .expect("read override");
        assert_eq!(
            overridden, 99.0,
            "a re-seed must never clobber a user-modified row"
        );
    }

    /// Anthropic's TTL multipliers are the default; an explicit override wins.
    /// The tiered Alibaba entries are the rows that exercise both rules at
    /// band level.
    #[test]
    fn cache_rates_derive_from_input_unless_overridden() {
        let rates = builtin_rates().expect("parse");
        let opus = rates
            .iter()
            .find(|r| r.model_pattern == "claude-opus-5")
            .expect("opus seeded");
        assert_eq!(opus.cache_write_5m_per_mtok, opus.input_per_mtok * 1.25);
        assert_eq!(opus.cache_write_1h_per_mtok, opus.input_per_mtok * 2.0);
        assert_eq!(opus.cache_read_per_mtok, opus.input_per_mtok * 0.1);
        assert_eq!(opus.effective_from, FAR_PAST);
    }

    #[test]
    fn tier_bands_are_written_and_pruned_in_step_with_the_catalog() {
        let conn = seeded_conn();
        let tiered: i64 = conn
            .query_row("SELECT COUNT(*) FROM model_pricing_tier", [], |r| r.get(0))
            .expect("count tiers");
        let declared: usize = builtin_rates()
            .expect("parse")
            .iter()
            .map(|r| r.tiers.len())
            .sum();
        assert_eq!(tiered as usize, declared, "every declared band is stored");
    }
}
