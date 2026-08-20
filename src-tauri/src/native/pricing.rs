//! `GET /api/pricing/catalog` and the three rate writes, ported from Go.
//!
//! Go sources this mirrors, and which are the spec if the two ever disagree:
//!
//! - `internal/pricing/store.go`   — `Snapshot`, `attachTiers`, `Revision`,
//!   `UpsertRate`, `DeleteRate`
//! - `internal/service/pricing_service.go` — `Catalog`, `groupByModel`,
//!   `AddRate`, `CorrectRate`, `DeleteRate`, `validatePricingRate`
//! - `internal/api/pricing.go`             — the handlers and `afterRateChange`
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
//!
//! ## The writes (#306)
//!
//! Four things decide whether this port is right, and each is silent when it is
//! not:
//!
//! - **Add and correct are deliberately not one upsert.** Appending leaves
//!   history priced at what it was charged; correcting rewrites already-reported
//!   costs. So `AddRate` refuses to overwrite — and answers the collision with
//!   the *colliding row* in the body, not a bare 409, because the UI offers
//!   "correct it instead?" and needs the row to do that — while `CorrectRate`
//!   refuses to create.
//! - **`effective_from` is normalized to second precision**, because RFC 3339 is
//!   what the column round-trips. Skip it and the read-back after a save finds
//!   nothing, so a written row is unreachable by the value that wrote it.
//! - **`UpsertRate` clears the rate's bands.** [`Rate::price`] picks a band
//!   *before* applying any price, so a correction that left the seeded bands in
//!   place would save and then change nothing at any request size. The
//!   correct-rate form says so; this keeps it true. Tier *editing* is
//!   deliberately not offered anywhere, and is not added here.
//! - **Every mutation has to move `pricing_rev`**, or #188's stored per-session
//!   costs keep the pre-edit figure with nothing to signal it. Go's
//!   `afterRateChange` invalidates its cache and lets the next read's freshness
//!   gate re-scan; since #289 that scan is *ours*, so the re-read is ours to
//!   trigger — see [`super::scan::after_pricing_change`].

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, NaiveDate, SecondsFormat, TimeZone, Timelike, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::db;
use super::gotime::GoTime;
use super::writes::{decode_body, finish, WriteError};

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
/// Just the catalog fingerprint.
///
/// Split out from [`catalog`] because the scan's freshness gate and `/status`
/// need only this number, and building the whole catalog to get it is expensive
/// in a way that scales with the corpus: `unpriced_models` runs a `UNION ALL`
/// over `claude_session_cache` and `claude_subagent_cache` on an unindexed
/// column. `/status` is polled by the sessions list every few seconds, so that
/// was two full table scans on a timer to colour one badge. Go pays nothing
/// comparable — its `pricingChanged()` compares an in-memory int.
pub fn revision_of(db_path: &Path) -> Result<i64, String> {
    let conn = db::open_read_only(db_path)?;
    Ok(revision(&snapshot(&conn)?))
}

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

// ─── Pricing arithmetic ───────────────────────────────────────────────────────

/// Token counts as a rate prices them. Mirrors `pricing.Usage`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PricedUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_5m_tokens: i64,
    pub cache_creation_1h_tokens: i64,
    pub cache_read_tokens: i64,
}

impl PricedUsage {
    /// The count a tiered rate's band is chosen by: **all** input-side tokens,
    /// fresh plus cache-read plus cache-write.
    ///
    /// Alibaba states how cached tokens are billed but not whether they count
    /// toward the context-length bound. The reading that they do is encoded
    /// here, in one place, and is the only place to change it.
    fn tier_input_tokens(&self) -> i64 {
        self.input_tokens
            + self.cache_read_tokens
            + self.cache_creation_5m_tokens
            + self.cache_creation_1h_tokens
    }
}

/// USD cost of one priced unit. Mirrors `pricing.Cost`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cost {
    pub input_cost_usd: f64,
    pub output_cost_usd: f64,
    pub cache_read_cost_usd: f64,
    pub cache_write_cost_usd: f64,
    pub total_cost_usd: f64,
}

impl Cost {
    /// Accumulate another cost, component by component, in Go's order.
    pub fn add(&mut self, o: &Cost) {
        self.input_cost_usd += o.input_cost_usd;
        self.output_cost_usd += o.output_cost_usd;
        self.cache_read_cost_usd += o.cache_read_cost_usd;
        self.cache_write_cost_usd += o.cache_write_cost_usd;
        self.total_cost_usd += o.total_cost_usd;
    }
}

impl Rate {
    /// The cost of `u` under this rate.
    ///
    /// Bands are **selected, not accumulated** — a provider that tiers by
    /// context length bills every token of a request at the chosen band's
    /// rate, so there is no progressive-bracket arithmetic — and a request
    /// above every declared bound uses the highest band rather than falling
    /// back to flat or to zero.
    pub fn price(&self, u: PricedUsage) -> Cost {
        let band = self.tier_for(u.tier_input_tokens());
        let mut c = Cost {
            input_cost_usd: u.input_tokens as f64 / 1_000_000.0 * band.input_per_mtok,
            output_cost_usd: u.output_tokens as f64 / 1_000_000.0 * band.output_per_mtok,
            cache_read_cost_usd: u.cache_read_tokens as f64 / 1_000_000.0
                * band.cache_read_per_mtok,
            cache_write_cost_usd: u.cache_creation_5m_tokens as f64 / 1_000_000.0
                * band.cache_write_5m_per_mtok
                + u.cache_creation_1h_tokens as f64 / 1_000_000.0 * band.cache_write_1h_per_mtok,
            total_cost_usd: 0.0,
        };
        c.total_cost_usd =
            c.input_cost_usd + c.output_cost_usd + c.cache_read_cost_usd + c.cache_write_cost_usd;
        c
    }

    /// The prices governing a request of the given input size. An untiered rate
    /// is one empty-slice check and nothing else, so the flat path is unchanged.
    fn tier_for(&self, input_tokens: i64) -> TierRate {
        let flat = TierRate {
            max_input_tokens: 0,
            input_per_mtok: self.input_per_mtok,
            output_per_mtok: self.output_per_mtok,
            cache_write_5m_per_mtok: self.cache_write_5m_per_mtok,
            cache_write_1h_per_mtok: self.cache_write_1h_per_mtok,
            cache_read_per_mtok: self.cache_read_per_mtok,
        };
        let Some(highest) = self.tiers.last() else {
            return flat;
        };
        self.tiers
            .iter()
            .find(|t| input_tokens <= t.max_input_tokens)
            .unwrap_or(highest)
            .clone()
    }
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "pricing",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    match path {
        "/api/pricing/catalog" => method == Method::GET,
        // One path, three methods — chi routes them to three handlers, and the
        // key is a query pair rather than a path segment because a model
        // pattern is not path-safe (`mixedbread-ai/` carries a slash,
        // `<synthetic>` angle brackets).
        "/api/pricing/rates" => {
            method == Method::POST || method == Method::PUT || method == Method::DELETE
        }
        _ => false,
    }
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    match (req.method, req.path) {
        (&Method::GET, "/api/pricing/catalog") => {
            let catalog = catalog(&ctx.db_path)?;
            Ok(super::Answer::json(
                super::gojson::to_vec(&catalog)
                    .map_err(|e| format!("encoding pricing catalog: {e}"))?,
            ))
        }
        (&Method::POST, "/api/pricing/rates") => finish(after_rate_change(
            &ctx.db_path,
            add_rate(&ctx.db_path, req.body),
        )),
        (&Method::PUT, "/api/pricing/rates") => finish(after_rate_change(
            &ctx.db_path,
            correct_rate(&ctx.db_path, req.body),
        )),
        (&Method::DELETE, "/api/pricing/rates") => finish(after_rate_change(
            &ctx.db_path,
            delete_rate(&ctx.db_path, req.query),
        )),
        _ => Err(format!(
            "{} {} is not a pricing route",
            req.method, req.path
        )),
    }
}

/// `Server.afterRateChange`, fired where Go fires it: in the **handler**, after
/// the service call came back, and never on a path that changed nothing.
///
/// It sits here rather than inside the three write functions for the same
/// reason it sits in `internal/api/pricing.go` rather than in the service — and
/// because a post-commit side effect wired into the mutation would run in every
/// unit test, kicking a background walk of the developer's real `~/.claude`
/// corpus into a temporary database.
///
/// **The success test is the status, not `is_ok`.** A rate collision is an
/// `Ok(409)` carrying the colliding row, and Go returns from that arm *before*
/// reaching `afterRateChange` — nothing was written, so nothing needs
/// re-pricing. Every arm that did write answers 200, 201 or 204.
fn after_rate_change(
    db_path: &Path,
    result: Result<super::Answer, WriteError>,
) -> Result<super::Answer, WriteError> {
    if result.as_ref().is_ok_and(|a| a.status.is_success()) {
        super::scan::after_pricing_change(db_path);
    }
    result
}

// ─── Writes ───────────────────────────────────────────────────────────────────

/// `api.PricingRateRequest`.
///
/// Every field defaults, because Go's decoder leaves a missing key at its zero
/// value — and a stored `null` for a scalar is a zero value to Go and a type
/// error to serde.
///
/// `billable` is the one field that is **not** a plain zero value: Go declares
/// it `*bool` and reads a nil pointer as `true`, because the zero value of a Go
/// bool would silently mark every priced model free. `Option<bool>` reproduces
/// that, and an explicit `null` lands on `None` exactly as a nil pointer does.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RateRequest {
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    provider: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    model_pattern: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    match_type: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    display_name: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    input_per_mtok: f64,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    output_per_mtok: f64,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    cache_write_5m_per_mtok: f64,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    cache_write_1h_per_mtok: f64,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    cache_read_per_mtok: f64,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    effective_from: String,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    source: String,
    billable: Option<bool>,
    #[serde(deserialize_with = "super::gojson::null_is_zero_value")]
    estimated: bool,
}

/// The columns one write puts in the row. Not [`Rate`]: that carries the `id`
/// and the bands, neither of which a request can express.
#[derive(Debug, Clone)]
struct RateInput {
    provider: String,
    model_pattern: String,
    match_type: String,
    display_name: String,
    input_per_mtok: f64,
    output_per_mtok: f64,
    cache_write_5m_per_mtok: f64,
    cache_write_1h_per_mtok: f64,
    cache_read_per_mtok: f64,
    effective_from: DateTime<Utc>,
    source: String,
    /// Preserved from the row being corrected; always false on a create, since
    /// the request cannot carry one.
    is_builtin: bool,
    billable: bool,
    estimated: bool,
}

impl RateRequest {
    /// `PricingRateRequest.toRate`.
    fn into_input(self) -> Result<RateInput, WriteError> {
        let effective_from = parse_effective_from(&self.effective_from)?;
        Ok(RateInput {
            provider: self.provider,
            model_pattern: self.model_pattern,
            match_type: self.match_type,
            display_name: self.display_name,
            input_per_mtok: self.input_per_mtok,
            output_per_mtok: self.output_per_mtok,
            cache_write_5m_per_mtok: self.cache_write_5m_per_mtok,
            cache_write_1h_per_mtok: self.cache_write_1h_per_mtok,
            cache_read_per_mtok: self.cache_read_per_mtok,
            effective_from,
            source: self.source,
            is_builtin: false,
            billable: self.billable.unwrap_or(true),
            estimated: self.estimated,
        })
    }
}

/// Go's zero `time.Time`, which is what `IsZero` tests and what an
/// `0001-01-01T00:00:00Z` in the request parses to.
fn go_zero_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(1, 1, 1, 0, 0, 0)
        .single()
        .unwrap_or_else(Utc::now)
}

/// `parseEffectiveFrom`: RFC 3339 first, then a bare `YYYY-MM-DD` meaning
/// midnight UTC — what an `<input type="date">` produces.
///
/// Both messages are handler-level `writeError(422, err.Error())` calls rather
/// than `service.ValidationError`s, so they ship **without** the
/// `validation error for "field":` prefix. That is why the field is empty here.
///
/// The two shape guards are not decoration. `time.Parse` matches its layout
/// exactly, while chrono is looser at both ends: `parse_from_rfc3339` accepts a
/// space where Go demands `T`/`t`, and `%Y` accepts year lengths other than
/// four. Without them a body Go answers 422 to would be accepted here.
fn parse_effective_from(s: &str) -> Result<DateTime<Utc>, WriteError> {
    if s.is_empty() {
        return Err(WriteError::validation("", "effective_from is required"));
    }
    let separator_is_t = s
        .as_bytes()
        .get(10)
        .is_some_and(|c| *c == b'T' || *c == b't');
    if separator_is_t {
        if let Ok(t) = DateTime::parse_from_rfc3339(s) {
            return Ok(t.with_timezone(&Utc));
        }
    }
    let dash_positions = s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-';
    if dash_positions {
        if let Some(day) = NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
        {
            return Ok(Utc.from_utc_datetime(&day));
        }
    }
    Err(WriteError::validation(
        "",
        "effective_from must be YYYY-MM-DD or RFC3339",
    ))
}

/// `normalizePattern`: trim, then lowercase.
fn normalize_pattern(p: &str) -> String {
    p.trim().to_lowercase()
}

/// `normalizeEffectiveFrom`: `t.UTC().Truncate(time.Second)`.
///
/// `Truncate` rounds toward the zero time, which for any date this catalog can
/// hold is simply dropping the sub-second part.
fn normalize_effective_from(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_nanosecond(0).unwrap_or(t)
}

/// `rateKey`: what a conflict or a 404 names the row by.
fn rate_key(model_pattern: &str, effective_from: DateTime<Utc>) -> String {
    format!(
        "{}@{}",
        normalize_pattern(model_pattern),
        normalize_effective_from(effective_from).to_rfc3339_opts(SecondsFormat::Secs, true)
    )
}

/// The text form the column holds: `EffectiveFrom.UTC().Format(time.RFC3339)`.
fn stored_effective_from(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `validatePricingRate`, in its order — which is observable, because the first
/// failure is the one the user sees.
fn validate(input: &mut RateInput) -> Result<(), WriteError> {
    input.model_pattern = normalize_pattern(&input.model_pattern);
    if input.model_pattern.is_empty() {
        return Err(WriteError::validation(
            "model_pattern",
            "model_pattern is required",
        ));
    }
    if input.effective_from == go_zero_time() {
        return Err(WriteError::validation(
            "effective_from",
            "effective_from is required",
        ));
    }
    input.effective_from = normalize_effective_from(input.effective_from);
    if input.match_type.is_empty() {
        input.match_type = "exact".to_string();
    }
    if input.match_type != "exact" && input.match_type != "prefix" {
        return Err(WriteError::validation(
            "match_type",
            r#"match_type must be "exact" or "prefix""#,
        ));
    }
    validate_amounts(input)
}

/// `validateRateAmounts`.
///
/// The billable rule is what makes a `$0.00` row mean something: a billable
/// model prices every category, a non-billable one prices none. Without it a
/// zeroed-out row reads as free rather than as unfilled.
fn validate_amounts(input: &RateInput) -> Result<(), WriteError> {
    let amounts = [
        ("input_per_mtok", input.input_per_mtok),
        ("output_per_mtok", input.output_per_mtok),
        ("cache_write_5m_per_mtok", input.cache_write_5m_per_mtok),
        ("cache_write_1h_per_mtok", input.cache_write_1h_per_mtok),
        ("cache_read_per_mtok", input.cache_read_per_mtok),
    ];
    let mut non_zero = false;
    for (name, value) in amounts {
        if value < 0.0 {
            return Err(WriteError::validation(name, "rate must not be negative"));
        }
        if value != 0.0 {
            non_zero = true;
        }
    }
    if input.billable && (input.input_per_mtok <= 0.0 || input.output_per_mtok <= 0.0) {
        return Err(WriteError::validation(
            "input_per_mtok",
            "a billable model needs a positive input and output rate",
        ));
    }
    if !input.billable && non_zero {
        return Err(WriteError::validation(
            "billable",
            "a non-billable model must have every rate set to zero",
        ));
    }
    Ok(())
}

/// The conflict body: **not** a bare `{"error": …}`.
///
/// `handleAddPricingRate` writes a two-key map, so the UI can offer to correct
/// the row it collided with instead of making the user guess what they hit.
/// `encoding/json` sorts map keys and `error` precedes `existing`, so the
/// declaration order here is already the wire order.
#[derive(Serialize)]
struct RateConflict<'a> {
    error: &'a str,
    existing: &'a Rate,
}

fn open_for_write(db_path: &Path) -> Result<Connection, WriteError> {
    let conn = db::open_read_write(db_path).map_err(WriteError::Fallback)?;
    super::migrate::verify(&conn).map_err(WriteError::Fallback)?;
    Ok(conn)
}

/// `findRate`: scan the snapshot the resolver already loads. The store exposes
/// no read-by-key, and the catalog is tens of rows.
///
/// **A read failure is not "no such rate".** Go returns the error and the
/// handler answers 500; collapsing it into `None` here would make [`add_rate`]
/// see no conflict and write straight over an existing row through the upsert —
/// silent data loss on the one path whose whole job is to refuse it. So the
/// failure is its own arm, and it forwards.
fn find_rate(
    conn: &Connection,
    model_pattern: &str,
    at: DateTime<Utc>,
) -> Result<Option<Rate>, WriteError> {
    let want_pattern = normalize_pattern(model_pattern);
    let want_from = normalize_effective_from(at);
    let rates = snapshot(conn)
        .map_err(|e| WriteError::Fallback(format!("loading pricing catalog: {e}")))?;
    Ok(rates
        .into_iter()
        .find(|r| r.model_pattern == want_pattern && r.effective_from.instant() == want_from))
}

/// `pricingService.AddRate` then `handleAddPricingRate`.
fn add_rate(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    let mut input = decode_body::<RateRequest>(body)?.into_input()?;
    validate(&mut input)?;

    let mut conn = open_for_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin add rate: {e}")))?;

    // The existence check and the write are one transaction, or two concurrent
    // adds both see "no such rate" and the second silently overwrites the first
    // through the upsert. Go is not exposed to that only because it holds a
    // single serialized connection; a second process is.
    if let Some(existing) = find_rate(&tx, &input.model_pattern, input.effective_from)? {
        let message = WriteError::Conflict {
            resource: "rate".to_string(),
            id: rate_key(&input.model_pattern, input.effective_from),
        }
        .message();
        let body = super::gojson::to_vec(&RateConflict {
            error: &message,
            existing: &existing,
        })
        .map_err(|e| WriteError::Fallback(format!("encoding rate conflict: {e}")))?;
        // Nothing was written, so dropping the transaction rolls back nothing.
        return Ok(super::Answer::json_status(StatusCode::CONFLICT, body));
    }

    let answer = save(&tx, &input, StatusCode::CREATED)?;
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit add rate: {e}")))?;
    Ok(answer)
}

/// `pricingService.CorrectRate` then `handleCorrectPricingRate`.
fn correct_rate(db_path: &Path, body: &[u8]) -> Result<super::Answer, WriteError> {
    let mut input = decode_body::<RateRequest>(body)?.into_input()?;
    validate(&mut input)?;

    let mut conn = open_for_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin correct rate: {e}")))?;

    let Some(existing) = find_rate(&tx, &input.model_pattern, input.effective_from)? else {
        return Err(WriteError::NotFound {
            resource: "rate".to_string(),
            id: rate_key(&input.model_pattern, input.effective_from),
        });
    };
    // A correction never turns a seeded row into a user-authored one. It does
    // mark it `user_modified`, which is what stops the next startup re-seed
    // restoring the published value over the correction.
    input.is_builtin = existing.is_builtin;

    let answer = save(&tx, &input, StatusCode::OK)?;
    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit correct rate: {e}")))?;
    Ok(answer)
}

/// `pricingService.save`: write, then read the row back so the caller answers
/// with exactly what was persisted — the normalization and the `user_modified`
/// flag the store applies on the way in included.
///
/// The answer is encoded **before** the commit. Everything after a commit must
/// be infallible, because an `Err` forwards to Go and Go would apply the write
/// a second time.
fn save(
    tx: &rusqlite::Transaction,
    input: &RateInput,
    status: StatusCode,
) -> Result<super::Answer, WriteError> {
    upsert_rate(tx, input)?;
    let Some(saved) = find_rate(tx, &input.model_pattern, input.effective_from)? else {
        // Go's "vanished after write" — a 500. Nothing is committed yet, so
        // rolling back and forwarding is safe.
        return Err(WriteError::Fallback(format!(
            "saving rate: {} vanished after write",
            rate_key(&input.model_pattern, input.effective_from)
        )));
    };
    let body = super::gojson::to_vec(&saved)
        .map_err(|e| WriteError::Fallback(format!("encoding rate: {e}")))?;
    Ok(super::Answer::json_status(status, body))
}

/// `Store.UpsertRate`, including the band clear.
///
/// The `DELETE` is not housekeeping. [`Rate::price`] selects a band before
/// applying any price, so leaving the seeded bands would make a hand-entered
/// rate unreachable at every request size — the edit would appear to save and
/// then change nothing. The settings form cannot express bands, so entering a
/// price here asserts that this is *the* price, which is the same
/// user-intent-wins rule `user_modified` encodes everywhere else.
fn upsert_rate(tx: &rusqlite::Transaction, input: &RateInput) -> Result<(), WriteError> {
    let effective_from = stored_effective_from(input.effective_from);
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    tx.execute(
        "INSERT INTO model_pricing (
             provider, model_pattern, match_type, display_name,
             input_per_mtok, output_per_mtok,
             cache_write_5m_per_mtok, cache_write_1h_per_mtok, cache_read_per_mtok,
             effective_from, source, is_builtin, user_modified, billable, estimated,
             created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13, ?14, ?15, ?16)
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
             updated_at = excluded.updated_at",
        rusqlite::params![
            input.provider,
            input.model_pattern,
            input.match_type,
            input.display_name,
            input.input_per_mtok,
            input.output_per_mtok,
            input.cache_write_5m_per_mtok,
            input.cache_write_1h_per_mtok,
            input.cache_read_per_mtok,
            effective_from,
            input.source,
            input.is_builtin,
            input.billable,
            input.estimated,
            now,
            now,
        ],
    )
    .map_err(|e| WriteError::Fallback(format!("saving rate {:?}: {e}", input.model_pattern)))?;

    tx.execute(
        "DELETE FROM model_pricing_tier
         WHERE rate_id = (SELECT id FROM model_pricing
                          WHERE model_pattern = ?1 AND effective_from = ?2)",
        rusqlite::params![input.model_pattern, effective_from],
    )
    .map_err(|e| WriteError::Fallback(format!("clearing rate bands: {e}")))?;
    Ok(())
}

/// `pricingService.DeleteRate` then `handleDeletePricingRate`.
///
/// The key arrives as a query pair. `parseEffectiveFrom` runs in the handler, so
/// a malformed date is the handler's fieldless 422; the emptiness checks run in
/// the service and carry their field.
fn delete_rate(db_path: &Path, query: &str) -> Result<super::Answer, WriteError> {
    let pattern = super::query::value(query, "model_pattern");
    let from = parse_effective_from(&super::query::value(query, "effective_from"))?;

    if pattern.trim().is_empty() {
        return Err(WriteError::validation(
            "model_pattern",
            "model_pattern is required",
        ));
    }
    if from == go_zero_time() {
        return Err(WriteError::validation(
            "effective_from",
            "effective_from is required",
        ));
    }
    let from = normalize_effective_from(from);

    let mut conn = open_for_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| WriteError::Fallback(format!("begin delete rate: {e}")))?;

    if find_rate(&tx, &pattern, from)?.is_none() {
        return Err(WriteError::NotFound {
            resource: "rate".to_string(),
            id: rate_key(&pattern, from),
        });
    }
    // `ON DELETE CASCADE` takes the bands with it — which is why the write
    // handle sets `foreign_keys=ON` per connection.
    let affected = tx
        .execute(
            "DELETE FROM model_pricing WHERE model_pattern = ?1 AND effective_from = ?2",
            rusqlite::params![normalize_pattern(&pattern), stored_effective_from(from)],
        )
        .map_err(|e| WriteError::Fallback(format!("deleting rate: {e}")))?;
    if affected == 0 {
        // `Store.DeleteRate`'s own "no rate for …" error, which is a 500. The
        // transaction has not committed, so forwarding cannot double-apply.
        return Err(WriteError::Fallback(format!(
            "pricing: no rate for {:?} at {}",
            pattern,
            stored_effective_from(from)
        )));
    }

    tx.commit()
        .map_err(|e| WriteError::Fallback(format!("commit delete rate: {e}")))?;
    Ok(super::Answer::no_content())
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

    // ─── Writes ───────────────────────────────────────────────────────────────

    /// Built by the **real** migrations rather than the hand-written `SCHEMA`
    /// above: the write path checks the recorded schema version, and a fixture
    /// table is exactly where a column default drifts away from production's.
    fn migrated() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        super::super::migrate::apply(&mut conn).expect("migrate");
        file
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("timestamp")
            .with_timezone(&Utc)
    }

    fn stored_rate(file: &tempfile::NamedTempFile, pattern: &str, from: &str) -> Option<Rate> {
        let conn = rusqlite::Connection::open(file.path()).expect("open");
        find_rate(&conn, pattern, at(from)).expect("read the catalog")
    }

    fn body_of(answer: super::super::Answer) -> String {
        String::from_utf8(answer.body.expect("body")).expect("utf-8")
    }

    const OPUS: &str = r#"{"provider":"anthropic","model_pattern":"claude-opus-5",
        "match_type":"prefix","display_name":"Claude Opus 5","input_per_mtok":5,
        "output_per_mtok":25,"cache_write_5m_per_mtok":6.25,"cache_write_1h_per_mtok":10,
        "cache_read_per_mtok":0.5,"effective_from":"2026-01-01","source":"pricing page"}"#;

    #[test]
    fn adding_a_rate_answers_201_and_stores_it_user_modified() {
        let file = migrated();
        let answer = add_rate(file.path(), OPUS.as_bytes()).expect("add");
        assert_eq!(answer.status, StatusCode::CREATED);

        let body = body_of(answer);
        // The response is the row that was read back, so it carries the id and
        // the flags the store applied — not an echo of the request.
        assert!(
            body.starts_with(r#"{"id":1,"provider":"anthropic""#),
            "{body}"
        );
        assert!(body.contains(r#""user_modified":true"#), "{body}");
        assert!(body.contains(r#""is_builtin":false"#), "{body}");
        // A bare date means midnight UTC on that day.
        assert!(
            body.contains(r#""effective_from":"2026-01-01T00:00:00Z""#),
            "{body}"
        );
        // `billable` defaults to true: Go reads a nil `*bool` that way, because
        // a Go bool's zero value would mark every priced model free.
        assert!(body.contains(r#""billable":true"#), "{body}");

        let saved = stored_rate(&file, "claude-opus-5", "2026-01-01T00:00:00Z").expect("stored");
        assert_eq!(saved.output_per_mtok, 25.0);
        assert_eq!(saved.match_type, "prefix");
    }

    /// A collision is **not** a bare 409: the colliding row ships in the body so
    /// the UI can offer to correct it. And nothing may be written — an add that
    /// overwrote would be silent data loss reported as an error.
    #[test]
    fn adding_over_an_existing_rate_is_409_carrying_the_colliding_row() {
        let file = migrated();
        add_rate(file.path(), OPUS.as_bytes()).expect("add");

        let cheaper = OPUS.replace(r#""input_per_mtok":5"#, r#""input_per_mtok":1"#);
        let answer = add_rate(file.path(), cheaper.as_bytes()).expect("answered, not forwarded");
        assert_eq!(answer.status, StatusCode::CONFLICT);

        let body = body_of(answer);
        assert!(
            body.starts_with(
                r#"{"error":"rate with id \"claude-opus-5@2026-01-01T00:00:00Z\" already exists","existing":{"id":1"#
            ),
            "{body}"
        );
        assert_eq!(
            stored_rate(&file, "claude-opus-5", "2026-01-01T00:00:00Z")
                .expect("stored")
                .input_per_mtok,
            5.0,
            "the add must not have overwritten the existing rate"
        );
    }

    #[test]
    fn correcting_a_missing_rate_is_404_and_writes_nothing() {
        let file = migrated();
        let err = correct_rate(file.path(), OPUS.as_bytes()).unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            err.message(),
            "rate \"claude-opus-5@2026-01-01T00:00:00Z\" not found"
        );
        assert!(stored_rate(&file, "claude-opus-5", "2026-01-01T00:00:00Z").is_none());
    }

    #[test]
    fn correcting_a_rate_rewrites_it_in_place_and_answers_200() {
        let file = migrated();
        add_rate(file.path(), OPUS.as_bytes()).expect("add");

        let fixed = OPUS.replace(r#""output_per_mtok":25"#, r#""output_per_mtok":30"#);
        let answer = correct_rate(file.path(), fixed.as_bytes()).expect("correct");
        assert_eq!(answer.status, StatusCode::OK);
        assert!(body_of(answer).contains(r#""output_per_mtok":30"#));

        let saved = stored_rate(&file, "claude-opus-5", "2026-01-01T00:00:00Z").expect("stored");
        assert_eq!(saved.output_per_mtok, 30.0);
        assert_eq!(
            saved.id, 1,
            "a correction edits the row, it does not append"
        );
    }

    /// A correction of a seeded row stays seeded but becomes user-modified —
    /// which is what stops the next startup re-seed restoring the published
    /// value over the correction.
    #[test]
    fn a_correction_keeps_is_builtin_and_sets_user_modified() {
        let file = migrated();
        {
            let conn = rusqlite::Connection::open(file.path()).expect("open");
            conn.execute(
                "INSERT INTO model_pricing (provider, model_pattern, match_type, display_name,
                     input_per_mtok, output_per_mtok, cache_write_5m_per_mtok,
                     cache_write_1h_per_mtok, cache_read_per_mtok, effective_from, source,
                     is_builtin, user_modified, billable, estimated, created_at, updated_at)
                 VALUES ('anthropic', 'claude-opus-5', 'prefix', 'Claude Opus 5',
                     5, 25, 6.25, 10, 0.5, '2026-01-01T00:00:00Z', 'pricing page',
                     1, 0, 1, 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                [],
            )
            .expect("seed row");
        }

        correct_rate(file.path(), OPUS.as_bytes()).expect("correct");
        let saved = stored_rate(&file, "claude-opus-5", "2026-01-01T00:00:00Z").expect("stored");
        assert!(saved.is_builtin, "a correction does not un-seed a row");
        assert!(saved.user_modified);
    }

    /// The trap the correct-rate form warns about: `Rate::price` picks a band
    /// *before* applying any price, so a correction that left the seeded bands
    /// in place would save and then change nothing at any request size.
    #[test]
    fn a_correction_clears_the_rates_bands() {
        let file = migrated();
        add_rate(
            file.path(),
            br#"{"provider":"alibaba","model_pattern":"qwen3-max","match_type":"prefix",
                 "display_name":"Qwen3 Max","input_per_mtok":1.2,"output_per_mtok":6,
                 "cache_write_5m_per_mtok":1.5,"cache_write_1h_per_mtok":2.4,
                 "cache_read_per_mtok":0.24,"effective_from":"2026-01-01"}"#,
        )
        .expect("add");
        {
            let conn = rusqlite::Connection::open(file.path()).expect("open");
            conn.execute(
                "INSERT INTO model_pricing_tier (rate_id, max_input_tokens, input_per_mtok,
                     output_per_mtok, cache_write_5m_per_mtok, cache_write_1h_per_mtok,
                     cache_read_per_mtok)
                 VALUES (1, 32000, 1.2, 6, 1.5, 2.4, 0.24), (1, 128000, 2.4, 12, 3, 4.8, 0.48)",
                [],
            )
            .expect("bands");
        }
        assert_eq!(
            stored_rate(&file, "qwen3-max", "2026-01-01T00:00:00Z")
                .expect("stored")
                .tiers
                .len(),
            2
        );

        correct_rate(
            file.path(),
            br#"{"provider":"alibaba","model_pattern":"qwen3-max","match_type":"prefix",
                 "display_name":"Qwen3 Max","input_per_mtok":2,"output_per_mtok":8,
                 "effective_from":"2026-01-01"}"#,
        )
        .expect("correct");

        let saved = stored_rate(&file, "qwen3-max", "2026-01-01T00:00:00Z").expect("stored");
        assert!(
            saved.tiers.is_empty(),
            "a hand-entered rate is flat, or its new figures are unreachable"
        );
        // …and now the flat price is the one that applies at every size.
        assert_eq!(
            saved
                .price(PricedUsage {
                    input_tokens: 1_000_000,
                    ..Default::default()
                })
                .input_cost_usd,
            2.0
        );
    }

    /// The written row has to be findable by the value that wrote it. RFC 3339
    /// storage carries seconds, so a sub-second `effective_from` that was not
    /// truncated would be saved and then never read back.
    #[test]
    fn a_sub_second_effective_from_is_truncated_so_the_read_back_finds_it() {
        let file = migrated();
        let precise = OPUS.replace(
            r#""effective_from":"2026-01-01""#,
            r#""effective_from":"2026-01-01T00:00:00.987654321Z""#,
        );
        let answer = add_rate(file.path(), precise.as_bytes()).expect("add");
        assert_eq!(answer.status, StatusCode::CREATED);
        assert!(body_of(answer).contains(r#""effective_from":"2026-01-01T00:00:00Z""#));
    }

    /// A non-UTC offset is converted rather than stored as given, so two clients
    /// naming the same instant collide on the uniqueness key as they should.
    #[test]
    fn an_offset_timestamp_is_normalized_to_utc() {
        let file = migrated();
        let offset = OPUS.replace(
            r#""effective_from":"2026-01-01""#,
            r#""effective_from":"2026-01-01T05:30:00+05:30""#,
        );
        add_rate(file.path(), offset.as_bytes()).expect("add");
        assert!(stored_rate(&file, "claude-opus-5", "2026-01-01T00:00:00Z").is_some());
    }

    /// `parseEffectiveFrom` fails in the **handler**, so its messages ship
    /// without the `validation error for "field":` prefix a service error
    /// carries. Getting that wrong changes the text on every bad date.
    #[test]
    fn a_malformed_effective_from_is_a_fieldless_422() {
        let file = migrated();
        for bad in ["", "01/02/2026", "2026-1-1", "2026-01-01 00:00:00Z"] {
            let body = OPUS.replace(
                r#""effective_from":"2026-01-01""#,
                &format!(r#""effective_from":"{bad}""#),
            );
            let err = add_rate(file.path(), body.as_bytes()).unwrap_err();
            assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY, "{bad:?}");
            assert!(
                err.message() == "effective_from is required"
                    || err.message() == "effective_from must be YYYY-MM-DD or RFC3339",
                "{bad:?} gave {:?}",
                err.message()
            );
        }
    }

    /// The rule that makes a `$0.00` row mean something. Each of these is a
    /// service `ValidationError`, so each *does* carry its field.
    #[test]
    fn the_billable_coherence_rules_are_422s_with_their_fields() {
        let file = migrated();
        let cases = [
            (
                r#""billable":false,"input_per_mtok":5,"output_per_mtok":25"#,
                "validation error for \"billable\": a non-billable model must have every rate set to zero",
            ),
            (
                r#""input_per_mtok":5,"output_per_mtok":0"#,
                "validation error for \"input_per_mtok\": a billable model needs a positive input and output rate",
            ),
            (
                r#""input_per_mtok":5,"output_per_mtok":25,"cache_read_per_mtok":-1"#,
                "validation error for \"cache_read_per_mtok\": rate must not be negative",
            ),
        ];
        for (fields, want) in cases {
            let body = format!(r#"{{"model_pattern":"m","effective_from":"2026-01-01",{fields}}}"#);
            let err = add_rate(file.path(), body.as_bytes()).unwrap_err();
            assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(err.message(), want);
        }
        // A non-billable model with everything zeroed is the *supported* way to
        // record a deliberate $0.00, so it must be accepted.
        add_rate(
            file.path(),
            br#"{"model_pattern":"<synthetic>","effective_from":"2026-01-01","billable":false}"#,
        )
        .expect("a zeroed non-billable rate is valid");
    }

    #[test]
    fn an_unknown_match_type_is_422_and_an_empty_one_defaults_to_exact() {
        let file = migrated();
        let err = add_rate(
            file.path(),
            OPUS.replace(r#""match_type":"prefix""#, r#""match_type":"glob""#)
                .as_bytes(),
        )
        .unwrap_err();
        assert_eq!(
            err.message(),
            "validation error for \"match_type\": match_type must be \"exact\" or \"prefix\""
        );

        add_rate(
            file.path(),
            br#"{"model_pattern":"m","effective_from":"2026-01-01",
                 "input_per_mtok":1,"output_per_mtok":2}"#,
        )
        .expect("add");
        assert_eq!(
            stored_rate(&file, "m", "2026-01-01T00:00:00Z")
                .expect("stored")
                .match_type,
            "exact"
        );
    }

    /// The pattern is the uniqueness key, so it is normalized on the way in —
    /// otherwise `Claude-Opus-5` and `claude-opus-5` would be two rows and the
    /// resolver, which lowercases, would find only one of them.
    #[test]
    fn the_model_pattern_is_trimmed_and_lowercased() {
        let file = migrated();
        add_rate(
            file.path(),
            OPUS.replace(r#""claude-opus-5""#, r#""  Claude-OPUS-5 ""#)
                .as_bytes(),
        )
        .expect("add");
        assert!(stored_rate(&file, "claude-opus-5", "2026-01-01T00:00:00Z").is_some());

        let err = add_rate(
            file.path(),
            br#"{"model_pattern":"   ","effective_from":"2026-01-01"}"#,
        )
        .unwrap_err();
        assert_eq!(
            err.message(),
            "validation error for \"model_pattern\": model_pattern is required"
        );
    }

    #[test]
    fn deleting_a_rate_answers_204_with_no_body_and_takes_its_bands() {
        let file = migrated();
        add_rate(file.path(), OPUS.as_bytes()).expect("add");
        {
            let conn = super::db::open_read_write(file.path()).expect("open rw");
            conn.execute(
                "INSERT INTO model_pricing_tier (rate_id, max_input_tokens, input_per_mtok,
                     output_per_mtok, cache_write_5m_per_mtok, cache_write_1h_per_mtok,
                     cache_read_per_mtok) VALUES (1, 32000, 5, 25, 6.25, 10, 0.5)",
                [],
            )
            .expect("band");
        }

        let answer = delete_rate(
            file.path(),
            "model_pattern=claude-opus-5&effective_from=2026-01-01",
        )
        .expect("delete");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        assert!(answer.body.is_none(), "204 carries no body and no header");
        assert!(stored_rate(&file, "claude-opus-5", "2026-01-01T00:00:00Z").is_none());

        let conn = rusqlite::Connection::open(file.path()).expect("open");
        let bands: i64 = conn
            .query_row("SELECT COUNT(*) FROM model_pricing_tier", [], |r| r.get(0))
            .expect("count");
        assert_eq!(bands, 0, "ON DELETE CASCADE did not fire");
    }

    /// The key is percent-encoded in the query because a model pattern is not
    /// path-safe — which is the whole reason it is a query pair.
    #[test]
    fn a_percent_encoded_pattern_round_trips_through_the_query() {
        let file = migrated();
        add_rate(
            file.path(),
            br#"{"model_pattern":"<synthetic>","effective_from":"2026-01-01","billable":false}"#,
        )
        .expect("add");
        delete_rate(
            file.path(),
            "model_pattern=%3Csynthetic%3E&effective_from=2026-01-01T00%3A00%3A00Z",
        )
        .expect("delete");
        assert!(stored_rate(&file, "<synthetic>", "2026-01-01T00:00:00Z").is_none());
    }

    #[test]
    fn deleting_a_missing_rate_is_404_and_a_missing_key_is_422() {
        let file = migrated();
        let err =
            delete_rate(file.path(), "model_pattern=nope&effective_from=2026-01-01").unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            err.message(),
            "rate \"nope@2026-01-01T00:00:00Z\" not found"
        );

        // No `effective_from` at all is the handler's fieldless 422…
        let err = delete_rate(file.path(), "model_pattern=nope").unwrap_err();
        assert_eq!(err.message(), "effective_from is required");
        // …and an empty pattern is the service's, which carries its field.
        let err = delete_rate(file.path(), "effective_from=2026-01-01").unwrap_err();
        assert_eq!(
            err.message(),
            "validation error for \"model_pattern\": model_pattern is required"
        );
    }

    /// The reason any of this has to invalidate: a write that left the
    /// fingerprint alone would leave every stored per-session cost at its
    /// pre-edit figure with nothing anywhere to signal it.
    #[test]
    fn every_mutation_moves_the_catalog_revision() {
        let file = migrated();
        let revision_now = || revision_of(file.path()).expect("revision");

        let empty = revision_now();
        add_rate(file.path(), OPUS.as_bytes()).expect("add");
        let added = revision_now();
        assert_ne!(added, empty, "an insert must move the fingerprint");

        correct_rate(
            file.path(),
            &OPUS
                .replace(r#""output_per_mtok":25"#, r#""output_per_mtok":30"#)
                .into_bytes(),
        )
        .expect("correct");
        let corrected = revision_now();
        assert_ne!(corrected, added, "a correction must move it too");

        delete_rate(
            file.path(),
            "model_pattern=claude-opus-5&effective_from=2026-01-01",
        )
        .expect("delete");
        assert_eq!(revision_now(), empty, "a delete restores the empty catalog");
    }

    /// A body Go answers 400 to must not become a 422 here, and a `null` body
    /// must reach the handler's own validation rather than failing the decode.
    #[test]
    fn a_malformed_body_is_400_and_a_null_body_is_422() {
        let file = migrated();
        assert_eq!(
            add_rate(file.path(), b"[]").unwrap_err(),
            WriteError::InvalidBody
        );
        assert_eq!(
            add_rate(file.path(), b"").unwrap_err(),
            WriteError::InvalidBody
        );
        // `json.Unmarshal(null, &v)` is a documented no-op in Go: zero value, no
        // error — so this fails `parseEffectiveFrom`, not the decoder.
        let err = add_rate(file.path(), b"null").unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.message(), "effective_from is required");
    }
}
