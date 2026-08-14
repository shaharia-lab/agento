//! `GET /api/pricing/catalog`, ported from Go.
//!
//! Go sources this mirrors, and which are the spec if the two ever disagree:
//!
//! - `internal/pricing/store.go`   — `Snapshot`, `attachTiers`, `Revision`
//! - `internal/service/pricing_service.go` — `Catalog`, `groupByModel`
//! - `internal/claudesessions/cache.go`    — `UnpricedModels`
//!
//! Three things here are load-bearing and easy to "clean up" into a regression:
//!
//! 1. **Field order is wire order.** Each struct's fields are declared in the
//!    order the Go struct declares them, because `serde` emits them in
//!    declaration order and so does `encoding/json`. Reordering for tidiness
//!    changes the response bytes.
//! 2. **Row order comes from SQL**, matching `Snapshot`'s `ORDER BY`. The
//!    revision hash walks rows in exactly that order, so a different sort here
//!    would produce a different fingerprint for identical data — and that
//!    fingerprint is what tells the scanner whether stored costs need
//!    re-pricing.
//! 3. **`current` is resolved inside the model's own rows**, never through the
//!    resolver, which answers for a model *ID* and would happily return a
//!    different, more specific pattern's rate.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use axum::http::Method;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::Serialize;

use super::db;
use super::gotime::GoTime;

/// One effective-dated price row. Mirrors `pricing.Rate`.
#[derive(Debug, Clone, Serialize)]
pub struct Rate {
    pub id: i64,
    pub provider: String,
    pub model_pattern: String,
    pub match_type: String,
    pub display_name: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_write_5m_per_mtok: f64,
    pub cache_write_1h_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub effective_from: GoTime,
    pub source: String,
    pub is_builtin: bool,
    pub user_modified: bool,
    pub billable: bool,
    pub estimated: bool,
    /// `json:"tiers,omitempty"` — an untiered rate omits the key entirely
    /// rather than sending `[]`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<TierRate>,
}

/// One context-length band. Mirrors `pricing.TierRate`.
#[derive(Debug, Clone, Serialize)]
pub struct TierRate {
    pub max_input_tokens: i64,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_write_5m_per_mtok: f64,
    pub cache_write_1h_per_mtok: f64,
    pub cache_read_per_mtok: f64,
}

/// One model with the rate in force and its full history. Mirrors
/// `service.PricedModel`.
#[derive(Debug, Clone, Serialize)]
pub struct PricedModel {
    pub model_pattern: String,
    pub provider: String,
    pub display_name: String,
    pub match_type: String,
    /// `null` only when every row for this model is future-dated.
    pub current: Option<Rate>,
    /// Newest first, so a past cost figure can be traced to the rate behind it.
    pub rates: Vec<Rate>,
}

/// The whole payload. Mirrors `service.PricingCatalog`.
#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub models: Vec<PricedModel>,
    pub unpriced_models: Vec<String>,
    pub revision: i64,
}

/// Build the catalog exactly as `pricingService.Catalog` does.
pub fn catalog(db_path: &Path) -> Result<Catalog, String> {
    let conn = db::open_read_only(db_path)?;
    let rates = snapshot(&conn)?;
    let revision = revision(&rates);
    Ok(Catalog {
        models: group_by_model(rates),
        unpriced_models: unpriced_models(&conn),
        revision,
    })
}

/// Every rate row, ordered for deterministic iteration and hashing.
///
/// The `ORDER BY` is `Snapshot`'s, character for character: SQLite's default
/// BINARY collation makes it the same total order on both sides.
pub fn snapshot(conn: &Connection) -> Result<Vec<Rate>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider, model_pattern, match_type, display_name,
                    input_per_mtok, output_per_mtok,
                    cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok,
                    effective_from, source, is_builtin, user_modified, billable, estimated
             FROM model_pricing
             ORDER BY model_pattern, match_type, effective_from",
        )
        .map_err(|e| format!("preparing rate query: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            let effective_from: String = row.get(10)?;
            // Go fails the whole request on an unparsable timestamp rather than
            // guessing one. The error reaches the proxy, which falls back to
            // the Go server.
            let effective_from = GoTime::parse(&effective_from).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    10,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::other(e)),
                )
            })?;
            Ok(Rate {
                id: row.get(0)?,
                provider: row.get(1)?,
                model_pattern: row.get(2)?,
                match_type: row.get(3)?,
                display_name: row.get(4)?,
                input_per_mtok: row.get(5)?,
                output_per_mtok: row.get(6)?,
                cache_write_5m_per_mtok: row.get(7)?,
                cache_write_1h_per_mtok: row.get(8)?,
                cache_read_per_mtok: row.get(9)?,
                effective_from,
                source: row.get(11)?,
                is_builtin: row.get::<_, i64>(12)? == 1,
                user_modified: row.get::<_, i64>(13)? == 1,
                billable: row.get::<_, i64>(14)? == 1,
                estimated: row.get::<_, i64>(15)? == 1,
                tiers: Vec::new(),
            })
        })
        .map_err(|e| format!("querying rates: {e}"))?;

    let mut rates = Vec::new();
    for row in rows {
        rates.push(row.map_err(|e| format!("reading rate: {e}"))?);
    }

    attach_tiers(conn, &mut rates)?;
    Ok(rates)
}

/// Load every band in one query and distribute by `rate_id` — an N+1 here would
/// sit on the path every cost figure goes through. Ascending
/// `max_input_tokens` is what lets band selection stop at the first match.
fn attach_tiers(conn: &Connection, rates: &mut [Rate]) -> Result<(), String> {
    if rates.is_empty() {
        return Ok(());
    }

    let mut stmt = conn
        .prepare(
            "SELECT rate_id, max_input_tokens,
                    input_per_mtok, output_per_mtok,
                    cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok
             FROM model_pricing_tier
             ORDER BY rate_id, max_input_tokens",
        )
        .map_err(|e| format!("preparing tier query: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                TierRate {
                    max_input_tokens: row.get(1)?,
                    input_per_mtok: row.get(2)?,
                    output_per_mtok: row.get(3)?,
                    cache_write_5m_per_mtok: row.get(4)?,
                    cache_write_1h_per_mtok: row.get(5)?,
                    cache_read_per_mtok: row.get(6)?,
                },
            ))
        })
        .map_err(|e| format!("querying tiers: {e}"))?;

    let mut by_id: HashMap<i64, usize> = HashMap::with_capacity(rates.len());
    for (i, rate) in rates.iter().enumerate() {
        by_id.insert(rate.id, i);
    }

    for row in rows {
        let (rate_id, tier) = row.map_err(|e| format!("reading tier: {e}"))?;
        // A band whose rate is gone is dropped, as Go's map lookup drops it.
        if let Some(&i) = by_id.get(&rate_id) {
            rates[i].tiers.push(tier);
        }
    }
    Ok(())
}

/// Fold the flat rate list into one entry per model pattern.
fn group_by_model(rates: Vec<Rate>) -> Vec<PricedModel> {
    let mut by_pattern: HashMap<String, Vec<Rate>> = HashMap::new();
    for rate in rates {
        by_pattern
            .entry(rate.model_pattern.clone())
            .or_default()
            .push(rate);
    }

    let now = Utc::now();
    let mut models: Vec<PricedModel> = by_pattern
        .into_iter()
        .map(|(pattern, mut group)| {
            // Newest first. The (model_pattern, effective_from) uniqueness key
            // rules out ties, which is what makes an unstable sort safe here
            // and on the Go side.
            group.sort_by_key(|r| std::cmp::Reverse(r.effective_from.instant()));

            // The identity columns come from the newest row: a rename or a
            // provider correction lands on a new rate, and the model should
            // show its current identity rather than its first.
            let head = &group[0];
            let (provider, display_name, match_type) = (
                head.provider.clone(),
                head.display_name.clone(),
                head.match_type.clone(),
            );

            let current = group
                .iter()
                .find(|r| r.effective_from.instant() <= now)
                .cloned();

            PricedModel {
                model_pattern: pattern,
                provider,
                display_name,
                match_type,
                current,
                rates: group,
            }
        })
        .collect();

    // Go compares Go strings, which is a byte-wise comparison — the same order
    // Rust's `Ord for str` gives.
    models.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.model_pattern.cmp(&b.model_pattern))
    });
    models
}

/// FNV-1a over the fully-ordered rows: any insert, update or delete moves it,
/// a mere re-ordering does not. Cached session costs record the revision they
/// were computed under, so this value decides whether a corpus re-price is due
/// — which is why it is reproduced byte for byte rather than approximated.
fn revision(rates: &[Rate]) -> i64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    let mut hash = OFFSET;
    let mut write = |bytes: &[u8]| {
        for b in bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(PRIME);
        }
    };

    for rate in rates {
        write(rate.model_pattern.as_bytes());
        write(&[0]);
        write(rate.match_type.as_bytes());
        write(&[0]);
        write(rate.provider.as_bytes());
        write(&[0]);
        write(rate.effective_from.rfc3339_nano_utc().as_bytes());
        for f in [
            rate.input_per_mtok,
            rate.output_per_mtok,
            rate.cache_write_5m_per_mtok,
            rate.cache_write_1h_per_mtok,
            rate.cache_read_per_mtok,
        ] {
            write(&f.to_bits().to_le_bytes());
        }
        // Both flags change what a lookup *means*, not just how it displays, so
        // toggling either has to force a re-cost.
        write(&[u8::from(rate.billable), u8::from(rate.estimated)]);
        // Bands are prices too: without them a band edit would leave the
        // fingerprint unchanged and stored costs would keep the pre-edit figure
        // forever, with nothing anywhere signalling it.
        for tier in &rate.tiers {
            write(&(tier.max_input_tokens as u64).to_le_bytes());
            for f in [
                tier.input_per_mtok,
                tier.output_per_mtok,
                tier.cache_write_5m_per_mtok,
                tier.cache_write_1h_per_mtok,
                tier.cache_read_per_mtok,
            ] {
                write(&f.to_bits().to_le_bytes());
            }
        }
    }

    // Kept non-negative so the value survives SQLite integer round-trips.
    (hash & 0x7FFF_FFFF_FFFF_FFFF) as i64
}

/// Model IDs seen in scanned sessions that matched no rate, sorted and
/// de-duplicated.
///
/// Best-effort, exactly as Go's `listUnpriced` is: the catalog is still useful
/// without it, and on a machine that has never scanned a session the tables may
/// not even be populated. A failure logs and yields an empty list — which
/// serializes as `[]`, never `null`, because Go builds it with `make`.
fn unpriced_models(conn: &Connection) -> Vec<String> {
    match read_unpriced(conn) {
        Ok(models) => models,
        Err(e) => {
            log::warn!("native pricing: failed to list unpriced models: {e}");
            Vec::new()
        }
    }
}

fn read_unpriced(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT unpriced_models FROM claude_session_cache WHERE unpriced_models != ''
             UNION ALL
             SELECT unpriced_models FROM claude_subagent_cache WHERE unpriced_models != ''",
        )
        .map_err(|e| format!("preparing unpriced query: {e}"))?;

    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("querying unpriced models: {e}"))?;

    // A BTreeSet dedupes and sorts in one step, and its byte-wise ordering is
    // what Go's sort.Strings gives.
    let mut seen = BTreeSet::new();
    for row in rows {
        let packed = row.map_err(|e| format!("reading unpriced models: {e}"))?;
        for model in packed.split('\n') {
            if !model.is_empty() {
                seen.insert(model.to_string());
            }
        }
    }
    Ok(seen.into_iter().collect())
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "pricing",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && path == "/api/pricing/catalog"
}

fn serve(ctx: &super::Ctx, _req: &super::Request) -> Result<super::Answer, String> {
    let catalog = catalog(&ctx.db_path)?;
    Ok(super::Answer {
        body: super::gojson::to_vec(&catalog)
            .map_err(|e| format!("encoding pricing catalog: {e}"))?,
        probe: None,
    })
}

// ─── Resolver ─────────────────────────────────────────────────────────────────

/// A rate lookup result. Mirrors `pricing.Resolved`.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub rate: Rate,
    /// True when the spend predates every row for the matched pattern, or the
    /// matched rate is itself a best-effort one.
    #[allow(dead_code)]
    pub estimated: bool,
}

/// Answers `(model_id, spent_at)` lookups against a snapshot of the catalog.
/// Mirrors `internal/pricing/resolver.go`, whose two-stage selection this
/// reproduces exactly — including that the snapshot's row order breaks ties.
///
/// Only the cache-savings insight card needs this: it is the one figure
/// anywhere that prices a counterfactual (what cache reads would have cost at
/// input rates) and so cannot read a stored total. Every other cost figure in
/// analytics is the value the scanner already stored.
pub struct Resolver {
    rates: Vec<Rate>,
}

impl Resolver {
    /// Build a resolver over the catalog the Go server would have snapshotted.
    pub fn load(conn: &Connection) -> Result<Self, String> {
        Ok(Self {
            rates: snapshot(conn)?,
        })
    }

    /// The rate governing `model_id` at `at`, or `None` when no pattern matches
    /// at all — the caller accounts those tokens as unpriced rather than
    /// inventing a cost.
    pub fn resolve(&self, model_id: &str, at: DateTime<Utc>) -> Option<Resolved> {
        let lower = model_id.trim().to_lowercase();
        if lower.is_empty() {
            return None;
        }
        let best = self.most_specific(&lower)?;
        Some(self.effective_at(best, at))
    }

    /// Exact beats prefix, ties broken by the longer pattern, and a tie on both
    /// by the earlier row — which is why the snapshot's `ORDER BY` is part of
    /// this answer rather than an implementation detail.
    fn most_specific(&self, lower: &str) -> Option<usize> {
        let (mut best, mut best_exact, mut best_len) = (None, false, -1i64);
        for (i, rate) in self.rates.iter().enumerate() {
            if !matches(rate, lower) {
                continue;
            }
            let len = rate.model_pattern.to_lowercase().len() as i64;
            let exact = rate.match_type == "exact";
            if exact && (!best_exact || best_len < len) {
                best = Some(i);
                best_exact = true;
                best_len = len;
            } else if !exact && !best_exact && best_len < len {
                best = Some(i);
                best_len = len;
            }
        }
        best
    }

    /// Among the rows sharing the winning pattern, the newest not after `at` —
    /// falling back to the earliest, marked estimated, when the spend predates
    /// every row.
    fn effective_at(&self, best: usize, at: DateTime<Utc>) -> Resolved {
        let pattern = self.rates[best].model_pattern.to_lowercase();
        let match_type = self.rates[best].match_type.clone();

        let mut winner: Option<&Rate> = None;
        let mut earliest: Option<&Rate> = None;
        for cand in &self.rates {
            if cand.model_pattern.to_lowercase() != pattern || cand.match_type != match_type {
                continue;
            }
            // Written as a match rather than `is_none_or`, which this crate's
            // MSRV predates.
            let earlier = match earliest {
                None => true,
                Some(e) => cand.effective_from.instant() < e.effective_from.instant(),
            };
            if earlier {
                earliest = Some(cand);
            }
            if cand.effective_from.instant() > at {
                continue;
            }
            let newer = match winner {
                None => true,
                Some(w) => cand.effective_from.instant() > w.effective_from.instant(),
            };
            if newer {
                winner = Some(cand);
            }
        }
        match winner {
            Some(w) => Resolved {
                rate: w.clone(),
                estimated: w.estimated,
            },
            // `earliest` is always set: `best` matched, so at least one row
            // shares its pattern.
            None => Resolved {
                rate: earliest
                    .cloned()
                    .unwrap_or_else(|| self.rates[best].clone()),
                estimated: true,
            },
        }
    }
}

/// Whether a rate's pattern applies to an already-lowercased model ID.
fn matches(rate: &Rate, lower: &str) -> bool {
    let pattern = rate.model_pattern.to_lowercase();
    if rate.match_type == "exact" {
        lower == pattern
    } else {
        lower.starts_with(&pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::gojson;

    /// The subset of the Go schema this endpoint reads. Kept verbatim from
    /// `internal/storage/sqlite.go` migrations 17, 18 and the tier table, so a
    /// column type change upstream shows up as a test failure here.
    const SCHEMA: &str = "
        CREATE TABLE model_pricing (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            provider TEXT NOT NULL DEFAULT '',
            model_pattern TEXT NOT NULL,
            match_type TEXT NOT NULL DEFAULT 'exact',
            display_name TEXT NOT NULL DEFAULT '',
            input_per_mtok REAL NOT NULL DEFAULT 0,
            output_per_mtok REAL NOT NULL DEFAULT 0,
            cache_write_5m_per_mtok REAL NOT NULL DEFAULT 0,
            cache_write_1h_per_mtok REAL NOT NULL DEFAULT 0,
            cache_read_per_mtok REAL NOT NULL DEFAULT 0,
            effective_from DATETIME NOT NULL,
            source TEXT NOT NULL DEFAULT '',
            is_builtin INTEGER NOT NULL DEFAULT 0,
            user_modified INTEGER NOT NULL DEFAULT 0,
            billable INTEGER NOT NULL DEFAULT 1,
            estimated INTEGER NOT NULL DEFAULT 0,
            created_at DATETIME NOT NULL,
            updated_at DATETIME NOT NULL,
            UNIQUE(model_pattern, effective_from)
        );
        CREATE TABLE model_pricing_tier (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            rate_id INTEGER NOT NULL REFERENCES model_pricing(id) ON DELETE CASCADE,
            max_input_tokens INTEGER NOT NULL,
            input_per_mtok REAL NOT NULL,
            output_per_mtok REAL NOT NULL,
            cache_write_5m_per_mtok REAL NOT NULL,
            cache_write_1h_per_mtok REAL NOT NULL,
            cache_read_per_mtok REAL NOT NULL,
            UNIQUE(rate_id, max_input_tokens)
        );
        CREATE TABLE claude_session_cache (unpriced_models TEXT NOT NULL DEFAULT '');
        CREATE TABLE claude_subagent_cache (unpriced_models TEXT NOT NULL DEFAULT '');
    ";

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory database");
        conn.execute_batch(SCHEMA).expect("schema");
        conn.execute_batch(
            "
            INSERT INTO model_pricing
                (id, provider, model_pattern, match_type, display_name,
                 input_per_mtok, output_per_mtok,
                 cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok,
                 effective_from, source, is_builtin, user_modified, billable, estimated,
                 created_at, updated_at)
            VALUES
                -- A model whose ID needs HTML escaping, priced at a deliberate zero.
                (1, '', '<synthetic>', 'exact', 'Claude Code synthetic message',
                 0, 0, 0, 0, 0,
                 '2020-01-01T00:00:00Z', 'placeholder', 1, 0, 0, 0,
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                -- Two rates for one model: the older is in force, the newer is
                -- future-dated and must not become `current`.
                (2, 'anthropic', 'claude-opus-5', 'prefix', 'Claude Opus 5',
                 5, 25, 6.25, 10, 0.5,
                 '2026-01-01T00:00:00Z', 'pricing page', 1, 0, 1, 0,
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                (3, 'anthropic', 'claude-opus-5', 'prefix', 'Claude Opus 5',
                 6, 30, 7.5, 12, 0.6,
                 '2099-01-01T00:00:00Z', 'pricing page', 1, 0, 1, 0,
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                -- A tiered rate, to prove bands ride along and hash in.
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

            INSERT INTO claude_session_cache (unpriced_models)
            VALUES ('kimi-k2\nglm-4.6'), (''), ('kimi-k2');
            INSERT INTO claude_subagent_cache (unpriced_models) VALUES ('glm-4.6\nqwen-plus');
            ",
        )
        .expect("fixture rows");
        conn
    }

    fn fixture_catalog() -> Catalog {
        let conn = fixture();
        let rates = snapshot(&conn).expect("snapshot");
        let revision = revision(&rates);
        Catalog {
            models: group_by_model(rates),
            unpriced_models: unpriced_models(&conn),
            revision,
        }
    }

    #[test]
    fn models_are_ordered_by_provider_then_pattern() {
        let catalog = fixture_catalog();
        let order: Vec<_> = catalog
            .models
            .iter()
            .map(|m| m.model_pattern.as_str())
            .collect();
        // The empty provider sorts first, then alibaba, then anthropic.
        assert_eq!(order, vec!["<synthetic>", "qwen3-max", "claude-opus-5"]);
    }

    #[test]
    fn history_is_newest_first_and_current_skips_the_future() {
        let catalog = fixture_catalog();
        let opus = catalog
            .models
            .iter()
            .find(|m| m.model_pattern == "claude-opus-5")
            .expect("opus in catalog");

        assert_eq!(opus.rates.len(), 2);
        assert_eq!(opus.rates[0].id, 3, "newest rate first");
        assert_eq!(
            opus.current.as_ref().map(|r| r.id),
            Some(2),
            "a future-dated rate is not in force"
        );
        // Identity comes from the newest row, not from whichever is current.
        assert_eq!(opus.display_name, "Claude Opus 5");
    }

    #[test]
    fn tiers_ride_along_and_untiered_rates_omit_the_key() {
        let catalog = fixture_catalog();
        let qwen = catalog
            .models
            .iter()
            .find(|m| m.model_pattern == "qwen3-max")
            .expect("qwen in catalog");
        assert_eq!(qwen.rates[0].tiers.len(), 2);
        assert_eq!(qwen.rates[0].tiers[0].max_input_tokens, 32000);

        let json = String::from_utf8(gojson::to_vec(&catalog).expect("encode")).expect("utf-8");
        assert!(json.contains(r#""max_input_tokens":32000"#));

        // The synthetic model has no bands, so its object must carry no key at
        // all. Its pattern is HTML-escaped on the wire, which is the form to
        // search for.
        let synthetic_object = json
            .split(r#"{"model_pattern":"#)
            .find(|chunk| chunk.starts_with(r#""\u003csynthetic"#))
            .expect("synthetic model object");
        assert!(!synthetic_object.contains("\"tiers\""));
    }

    #[test]
    fn unpriced_models_are_deduped_across_both_tables_and_sorted() {
        let catalog = fixture_catalog();
        assert_eq!(
            catalog.unpriced_models,
            vec!["glm-4.6", "kimi-k2", "qwen-plus"]
        );
    }

    #[test]
    fn an_empty_catalog_serializes_as_empty_arrays_not_null() {
        let conn = Connection::open_in_memory().expect("in-memory database");
        conn.execute_batch(SCHEMA).expect("schema");
        let rates = snapshot(&conn).expect("snapshot");
        let catalog = Catalog {
            revision: revision(&rates),
            models: group_by_model(rates),
            unpriced_models: unpriced_models(&conn),
        };

        let json = String::from_utf8(gojson::to_vec(&catalog).expect("encode")).expect("utf-8");
        assert_eq!(
            json,
            format!(
                "{{\"models\":[],\"unpriced_models\":[],\"revision\":{}}}\n",
                catalog.revision
            )
        );
    }

    /// The fixture's whole response, byte for byte. Generated by the Go
    /// implementation against the same rows (see
    /// `desktop/parity/pricing_catalog_parity_test.go`), so this asserts
    /// against Go's real output rather than against this file's own behaviour.
    #[test]
    fn the_whole_response_matches_gos_bytes() {
        let want = include_str!("../../../parity/pricing_catalog_golden.json");
        let got =
            String::from_utf8(gojson::to_vec(&fixture_catalog()).expect("encode")).expect("utf-8");
        assert_eq!(got, want);
    }
}
