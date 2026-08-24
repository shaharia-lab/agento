//! One row per served gateway request (#425).
//!
//! # Two hard parts, and neither is the SQL
//!
//! **Where the write happens.** A usage row must never be able to fail a
//! request the user has already paid for — the tokens are spent by the time
//! there is anything to record. So [`Accounting::finish`] spawns and is never
//! awaited, the insert goes through [`db::blocking`] (#366: a rusqlite call
//! meets a five-second `busy_timeout`, and this listener's streaming path is
//! not on the blocking pool), and every failure below it is a `warn` line.
//!
//! **How a stream reports what it saw.** A finished response carries its own
//! `usage`; a stream that was cut does not, and there may never be a final
//! chunk at all. So the counters live in a shared accumulator that the stream
//! updates as chunks pass ([`meter`]), and the terminal arm flushes whatever is
//! in it. That is what makes an interrupted stream record the tokens actually
//! consumed rather than nothing.
//!
//! # Exactly one row, enforced rather than hoped for
//!
//! Both surfaces have three ways to end — a clean finish, an upstream error,
//! and the client going away — and the Anthropic surface reaches them through
//! a translation layer that owns its own state machine. A missed arm writes no
//! row; a doubled arm writes two, and a usage log that sometimes double-counts
//! is worse than one that sometimes misses, because nothing in the numbers says
//! which. [`Accounting`] therefore carries a `done` flag and `finish` is a
//! compare-exchange: the first call wins, every later one returns.
//!
//! # Cost is stored, not derived
//!
//! Resolved against the pricing catalog **at write time** and stored, which is
//! the rule the scanner already enforces for Claude sessions. A later rate
//! correction must not retroactively rewrite what a past request cost, and
//! joining the catalog at read time reproduces exactly the
//! list-versus-dashboard disagreement the Claude side refuses. A model the
//! catalog does not price stores `NULL` with `unpriced = 1` — **never `0.0`**,
//! because unpriced models are disclosed rather than zeroed and a total has to
//! read as a floor. That case is common here rather than exotic: the catalog is
//! seeded for Claude models, so OpenAI, Gemini and GLM aliases routinely miss.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use ferrox_providers::providers::ProviderStream;
use ferrox_providers::types::{cache_tokens, Usage};

use crate::native::{db, gotime, pricing};

/// How a request ended, as the `status` column stores it.
///
/// `Interrupted` and `UpstreamError` are separate on purpose: a single "error"
/// would leave a dashboard unable to tell a provider outage from a closed tab,
/// and those call for opposite reactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The response completed.
    Ok,
    /// Every target failed, or the stream errored mid-flight.
    UpstreamError,
    /// The client went away before the response finished.
    Interrupted,
    /// Never dispatched — an unknown alias, or one with no enabled target.
    Refused,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::UpstreamError => "upstream_error",
            Self::Interrupted => "interrupted",
            Self::Refused => "refused",
        }
    }

    /// How a dispatch failure is classified.
    ///
    /// The two arms that never reached a provider are the operator's or the
    /// client's mistake rather than an upstream fault, and counting them as
    /// outages would make a misconfigured alias look like a failing provider.
    pub fn for_dispatch_error(e: &ferrox_providers::error::ProxyError) -> Self {
        use ferrox_providers::error::ProxyError;
        match e {
            ProxyError::ModelNotFound(_) | ProxyError::ConfigError(_) => Self::Refused,
            _ => Self::UpstreamError,
        }
    }
}

/// Everything known about a request before its outcome is.
pub struct Seed {
    pub alias: String,
    pub provider: String,
    pub model_id: String,
    pub surface: &'static str,
    pub streamed: bool,
    pub token_sub: String,
    pub started: std::time::Instant,
    pub at: DateTime<Utc>,
}

/// The token counts observed so far.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Observed {
    pub prompt: u64,
    pub completion: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// # Why four separate atomics rather than one `Mutex<Observed>`
///
/// Four independent stores are not an atomic group, so in general a reader
/// could see three fields from one chunk and one from the next. That cannot
/// happen here, and the reason is worth writing down because it is a property
/// of the *callers* rather than of this type: the counters are written only by
/// [`meter`], which runs inside the stream, and a stream advances only when
/// something polls it — which is the single frame loop that also calls
/// [`Accounting::finish`]. One task, so a write is never in flight while that
/// task reads.
///
/// If a second poller is ever introduced, this stops being true and a `Mutex`
/// becomes the honest answer. `done` is genuinely concurrent by contrast, which
/// is why it is a compare-exchange and not a `bool`.
struct Inner {
    db_path: PathBuf,
    seed: Seed,
    prompt: AtomicU64,
    completion: AtomicU64,
    cache_read: AtomicU64,
    cache_write: AtomicU64,
    done: AtomicBool,
}

/// The handle a request carries from dispatch to its last frame.
///
/// Cloneable, because the streaming path needs one copy inside the metering
/// adapter and one in the frame loop that ends the request.
#[derive(Clone)]
pub struct Accounting {
    inner: Arc<Inner>,
}

impl Accounting {
    pub fn new(db_path: PathBuf, seed: Seed) -> Self {
        Self {
            inner: Arc::new(Inner {
                db_path,
                seed,
                prompt: AtomicU64::new(0),
                completion: AtomicU64::new(0),
                cache_read: AtomicU64::new(0),
                cache_write: AtomicU64::new(0),
                done: AtomicBool::new(false),
            }),
        }
    }

    /// Record what a provider reported for one response or chunk.
    ///
    /// **Last write wins rather than accumulating**, because every provider in
    /// `ferrox-providers` reports usage as a running total for the request, not
    /// as a per-chunk delta — summing them would multiply a long stream's
    /// prompt tokens by its chunk count. A `None` (most chunks carry none)
    /// leaves the counters alone, so the last chunk that *did* carry usage is
    /// what an interrupted stream records.
    pub fn observe(&self, usage: Option<&Usage>) {
        let Some(usage) = usage else { return };
        let (read, write) = cache_tokens(usage);
        self.inner
            .prompt
            .store(u64::from(usage.prompt_tokens), Ordering::Relaxed);
        self.inner
            .completion
            .store(u64::from(usage.completion_tokens), Ordering::Relaxed);
        self.inner
            .cache_read
            .store(u64::from(read), Ordering::Relaxed);
        self.inner
            .cache_write
            .store(u64::from(write), Ordering::Relaxed);
    }

    /// What has been observed so far.
    pub fn observed(&self) -> Observed {
        Observed {
            prompt: self.inner.prompt.load(Ordering::Relaxed),
            completion: self.inner.completion.load(Ordering::Relaxed),
            cache_read: self.inner.cache_read.load(Ordering::Relaxed),
            cache_write: self.inner.cache_write.load(Ordering::Relaxed),
        }
    }

    /// Write the row. **The first call wins; every later one is a no-op.**
    ///
    /// Spawned and never awaited — see the module header. Outside a tokio
    /// runtime (only reachable from a synchronous test) it does nothing rather
    /// than panicking, which is `tokens::touch`'s behaviour for the same
    /// reason.
    pub fn finish(&self, status: Status) {
        if self
            .inner
            .done
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let observed = self.observed();
        let duration_ms =
            u64::try_from(self.inner.seed.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let row = UsageRow {
            id: uuid::Uuid::new_v4().to_string(),
            at: self.inner.seed.at,
            alias: self.inner.seed.alias.clone(),
            provider: self.inner.seed.provider.clone(),
            model_id: self.inner.seed.model_id.clone(),
            observed,
            duration_ms,
            status,
            streamed: self.inner.seed.streamed,
            surface: self.inner.seed.surface,
            token_sub: self.inner.seed.token_sub.clone(),
        };
        let db_path = self.inner.db_path.clone();

        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let Some(result) = db::blocking("gateway usage", move || insert(&db_path, &row)).await
            else {
                return;
            };
            if let Err(e) = result {
                // Dropped, never propagated: the request is already answered
                // and the money already spent.
                log::warn!("gateway usage row not recorded: {e}");
            }
        });
    }
}

/// Wrap a provider stream so every chunk's usage reaches `accounting`.
///
/// The Anthropic surface needs this rather than reading usage in its own frame
/// loop: `openai_stream_to_anthropic_frames` **consumes** the provider stream
/// and emits protocol frames, so by the time the frames are visible the
/// per-chunk `usage` is gone. Metering before the translation is what ferrox
/// does too, for the same reason.
pub fn meter(stream: ProviderStream, accounting: Accounting) -> ProviderStream {
    use tokio_stream::StreamExt as _;
    Box::pin(stream.map(move |item| {
        if let Ok(chunk) = &item {
            accounting.observe(chunk.usage.as_ref());
        }
        item
    }))
}

/// One row, ready to insert.
pub struct UsageRow {
    pub id: String,
    pub at: DateTime<Utc>,
    pub alias: String,
    pub provider: String,
    pub model_id: String,
    pub observed: Observed,
    pub duration_ms: u64,
    pub status: Status,
    pub streamed: bool,
    pub surface: &'static str,
    pub token_sub: String,
}

/// Resolve the cost and insert. Blocking; callers go through [`db::blocking`].
fn insert(db_path: &std::path::Path, row: &UsageRow) -> Result<(), String> {
    let conn = db::open_read_write(db_path)?;

    // One connection serves both the catalog read and the insert. The resolver
    // is a snapshot of the whole rate table, so this is a read of a small
    // seeded reference table rather than a per-row join.
    let (cost_usd, unpriced) = match pricing::Resolver::load(&conn) {
        Ok(resolver) => price(&resolver, row),
        Err(e) => {
            // A catalog this process cannot read is not the same as a model it
            // does not price, but the row is still worth having: recording the
            // tokens with the cost disclosed as unknown is strictly better than
            // recording nothing.
            log::warn!("gateway usage: pricing catalog unavailable, recording unpriced: {e}");
            (None, true)
        }
    };

    conn.execute(
        "INSERT INTO gateway_usage_log \
         (id, created_at, alias, provider, model_id, prompt_tokens, completion_tokens, \
          cache_read_tokens, cache_write_tokens, duration_ms, status, streamed, surface, \
          token_sub, cost_usd, unpriced) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        rusqlite::params![
            row.id,
            // `now_go_text` rather than RFC 3339: every DATETIME column in this
            // schema holds Go's `time.Time.String()` and is compared **as
            // text**, so another spelling sorts into the wrong place against
            // every row beside it — a silently wrong list rather than an error.
            gotime::go_text(&row.at),
            row.alias,
            row.provider,
            row.model_id,
            row.observed.prompt as i64,
            row.observed.completion as i64,
            row.observed.cache_read as i64,
            row.observed.cache_write as i64,
            row.duration_ms as i64,
            row.status.as_str(),
            row.streamed,
            row.surface,
            row.token_sub,
            cost_usd,
            unpriced,
        ],
    )
    .map_err(|e| format!("inserting gateway usage row: {e}"))?;
    Ok(())
}

/// `(cost_usd, unpriced)` for a row.
///
/// Split out from [`insert`] so the catalog behaviour is testable without a
/// database write, and because the `None` arm is the one that must not drift:
/// it returns `(None, true)`, never `(Some(0.0), false)`.
fn price(resolver: &pricing::Resolver, row: &UsageRow) -> (Option<f64>, bool) {
    let Some(resolved) = resolver.resolve(&row.model_id, row.at) else {
        return (None, true);
    };
    let cost = resolved.rate.price(pricing::PricedUsage {
        input_tokens: row.observed.prompt as i64,
        output_tokens: row.observed.completion as i64,
        cache_read_tokens: row.observed.cache_read as i64,
        // Providers report one combined cache-creation figure with no TTL
        // split, so there is nothing to attribute to the 1h band. The 5m band
        // is Anthropic's default TTL and therefore the right guess; picking the
        // 1h band instead would over-report, and splitting the number between
        // them would invent a breakdown nobody measured.
        cache_creation_5m_tokens: row.observed.cache_write as i64,
        cache_creation_1h_tokens: 0,
    });
    (Some(cost.total_cost_usd), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> Seed {
        Seed {
            alias: "my-alias".into(),
            provider: "p1".into(),
            model_id: "claude-sonnet-4-5".into(),
            surface: "openai",
            streamed: false,
            token_sub: "token-1".into(),
            started: std::time::Instant::now(),
            at: Utc::now(),
        }
    }

    fn usage(prompt: u32, completion: u32) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            extra: Default::default(),
        }
    }

    #[test]
    fn every_status_has_a_distinct_stored_spelling() {
        let all = [
            Status::Ok,
            Status::UpstreamError,
            Status::Interrupted,
            Status::Refused,
        ];
        let spellings: std::collections::BTreeSet<_> = all.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            spellings.len(),
            all.len(),
            "two statuses sharing a string would silently merge in every aggregate"
        );
        assert_eq!(Status::Ok.as_str(), "ok");
        assert_eq!(Status::Interrupted.as_str(), "interrupted");
    }

    /// The classification that decides whether a misconfigured alias looks like
    /// a failing provider.
    #[test]
    fn a_dispatch_failure_that_never_reached_a_provider_is_refused() {
        use ferrox_providers::error::ProxyError;
        assert_eq!(
            Status::for_dispatch_error(&ProxyError::ModelNotFound("x".into())),
            Status::Refused
        );
        assert_eq!(
            Status::for_dispatch_error(&ProxyError::ConfigError("x".into())),
            Status::Refused
        );
        assert_eq!(
            Status::for_dispatch_error(&ProxyError::ProviderError {
                provider: "p".into(),
                status: 500,
                message: "m".into(),
            }),
            Status::UpstreamError
        );
        assert_eq!(
            Status::for_dispatch_error(&ProxyError::UpstreamTimeout("t".into())),
            Status::UpstreamError
        );
    }

    /// Provider usage is a running total, so the counters must be *replaced*
    /// rather than summed.
    ///
    /// The failure this pins is quiet and large: a 200-chunk stream that
    /// accumulated would report 200× the prompt tokens, and a cost to match.
    #[test]
    fn observing_replaces_rather_than_accumulates() {
        let acct = Accounting::new(PathBuf::from("/nonexistent"), seed());
        acct.observe(Some(&usage(15, 1)));
        acct.observe(Some(&usage(15, 2)));
        acct.observe(Some(&usage(15, 3)));
        assert_eq!(
            acct.observed(),
            Observed {
                prompt: 15,
                completion: 3,
                cache_read: 0,
                cache_write: 0
            }
        );
    }

    /// Most chunks carry no usage; they must not reset what the last one said,
    /// or an interrupted stream records zeros.
    #[test]
    fn a_chunk_with_no_usage_leaves_the_counters_alone() {
        let acct = Accounting::new(PathBuf::from("/nonexistent"), seed());
        acct.observe(Some(&usage(15, 7)));
        acct.observe(None);
        acct.observe(None);
        assert_eq!(acct.observed().completion, 7);
    }

    /// The cache split comes from `ferrox-providers`' single reading, whose
    /// tuple is `(read, write)` while the carrier underneath is
    /// `(creation, read)`. Getting it backwards swaps two columns that look
    /// equally plausible in a dashboard.
    #[test]
    fn the_cache_tokens_split_is_read_then_write() {
        let mut u = usage(47, 2);
        u.extra.insert(
            "cache_read_input_tokens".to_string(),
            serde_json::json!(3968),
        );
        u.extra.insert(
            "cache_creation_input_tokens".to_string(),
            serde_json::json!(100),
        );
        let acct = Accounting::new(PathBuf::from("/nonexistent"), seed());
        acct.observe(Some(&u));
        let observed = acct.observed();
        assert_eq!(observed.cache_read, 3968);
        assert_eq!(observed.cache_write, 100);
    }

    /// Exactly one row, whatever the caller does.
    ///
    /// Both surfaces have three terminal arms and the Anthropic one runs its
    /// frames through a translation layer; a double-fire is the realistic
    /// mistake, and a usage log that sometimes double-counts is worse than one
    /// that sometimes misses because the numbers do not say which.
    #[tokio::test]
    async fn finish_writes_once_however_many_times_it_is_called() {
        let file = tempfile::NamedTempFile::new().expect("tempfile");
        {
            let mut conn = db::ensure_database(file.path()).expect("create");
            crate::native::migrate::apply(&mut conn).expect("migrate");
        }

        let acct = Accounting::new(file.path().to_path_buf(), seed());
        acct.observe(Some(&usage(15, 12)));
        acct.finish(Status::Ok);
        acct.finish(Status::Interrupted);
        acct.finish(Status::UpstreamError);

        // `finish` spawns; wait for the write rather than sleeping a fixed amount.
        let mut rows = 0;
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            let conn = db::open_read_only(file.path()).expect("open");
            rows = conn
                .query_row("SELECT COUNT(*) FROM gateway_usage_log", [], |r| {
                    r.get::<_, i64>(0)
                })
                .expect("count");
            if rows > 0 {
                break;
            }
        }
        assert_eq!(rows, 1, "three finishes must leave exactly one row");

        let conn = db::open_read_only(file.path()).expect("open");
        let (status, prompt, completion): (String, i64, i64) = conn
            .query_row(
                "SELECT status, prompt_tokens, completion_tokens FROM gateway_usage_log",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("row");
        assert_eq!(status, "ok", "the first finish is the one that counts");
        assert_eq!((prompt, completion), (15, 12));
    }

    /// An unwritable database must not panic, hang, or otherwise reach the
    /// caller — the request is already answered by the time this runs.
    #[tokio::test]
    async fn an_unwritable_database_is_dropped_rather_than_raised() {
        let acct = Accounting::new(
            PathBuf::from("/nonexistent/directory/agento.db"),
            Seed {
                streamed: true,
                ..seed()
            },
        );
        acct.finish(Status::Ok);
        // Nothing to assert but the absence of a panic; give the spawned task a
        // chance to run and fail.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    /// A model the catalog does not price stores `NULL` and `unpriced = 1` —
    /// never `0.0`, which would read as "this request was free".
    #[test]
    fn an_unpriced_model_is_disclosed_rather_than_zeroed() {
        let file = tempfile::NamedTempFile::new().expect("tempfile");
        let conn = {
            let mut conn = db::ensure_database(file.path()).expect("create");
            crate::native::migrate::apply(&mut conn).expect("migrate");
            crate::native::pricing_seed::seed(&conn).expect("seed the catalog");
            conn
        };
        let resolver = pricing::Resolver::load(&conn).expect("resolver");

        let row = UsageRow {
            id: "id".into(),
            at: Utc::now(),
            alias: "a".into(),
            provider: "p".into(),
            model_id: "definitely-not-a-real-model-xyz".into(),
            observed: Observed {
                prompt: 1000,
                completion: 1000,
                cache_read: 0,
                cache_write: 0,
            },
            duration_ms: 1,
            status: Status::Ok,
            streamed: false,
            surface: "openai",
            token_sub: "t".into(),
        };
        assert_eq!(
            price(&resolver, &row),
            (None, true),
            "an unknown model must be disclosed as unpriced, not costed at zero"
        );

        // ...and a model it does price produces a real, positive figure, so the
        // assertion above is not passing because pricing is broken outright.
        let priced = UsageRow {
            model_id: "claude-sonnet-4-5-20250929".into(),
            ..row
        };
        let (cost, unpriced) = price(&resolver, &priced);
        assert!(!unpriced);
        assert!(
            cost.is_some_and(|c| c > 0.0),
            "a catalogued model must cost something; got {cost:?}"
        );
    }
}

// ── Reading the log back (#426's `GET /api/gateway/usage`) ───────────────────

/// One recorded request, as a read sees it.
///
/// Deliberately not [`UsageRow`]: that type is what a *write* assembles and
/// carries a `&'static str` surface and a not-yet-resolved cost. This is what
/// came back out, cost included.
#[derive(Debug, Clone)]
pub struct Record {
    pub at: DateTime<Utc>,
    pub alias: String,
    pub provider: String,
    pub model_id: String,
    pub observed: Observed,
    pub duration_ms: i64,
    pub status: String,
    pub streamed: bool,
    pub surface: String,
    pub cost_usd: Option<f64>,
    pub unpriced: bool,
}

/// Every row in `[from, to]`, optionally narrowed to one alias.
///
/// # Why the window is compared as text
///
/// `created_at` holds Go's `time.Time.String()`, and every DATETIME column in
/// this schema is compared **as text** — that is the whole reason
/// `gotime::go_text` exists rather than each writer formatting its own.
/// Parsing every row to filter in Rust would read the whole table to answer a
/// one-day question.
///
/// That is sound because **`go_text` is lexicographically ordered**, and the
/// non-obvious part is the fraction, which is variable-width because trailing
/// zeros are trimmed. It still sorts right: the character after the seconds is
/// `.` for a row that has one and `' '` for a row that does not, and `'.' >
/// ' '`, so an unfractioned instant sorts before every fractioned one in the
/// same second — which is exactly its chronological place, since no fraction
/// means `.0`. Within a fraction the digits compare left to right and a shorter
/// one ends at the space, which is below every digit. Every row is written
/// `+0000 UTC`, so there is no second zone to break the ordering.
/// `the_stored_timestamp_sorts_as_text_the_way_it_sorts_in_time` pins it.
///
/// The bound is therefore the plain rendering of `to`. An earlier version
/// appended a sentinel to "reach past any fraction"; that was reasoning about a
/// problem that does not exist — a fractioned row in `to`'s second is *after*
/// `to` and is correctly excluded — and the sentinel changed no answer.
pub fn load_window(
    db_path: &std::path::Path,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    alias: &str,
) -> Result<Vec<Record>, String> {
    let conn = db::open_read_only(db_path)?;
    let lower = gotime::go_text(&from);
    let upper = gotime::go_text(&to);

    let sql = "SELECT created_at, alias, provider, model_id, prompt_tokens, completion_tokens, \
               cache_read_tokens, cache_write_tokens, duration_ms, status, streamed, surface, \
               cost_usd, unpriced \
               FROM gateway_usage_log \
               WHERE created_at >= ?1 AND created_at <= ?2 \
               AND (?3 = '' OR alias = ?3) \
               ORDER BY created_at";
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("preparing gateway usage read: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![lower, upper, alias], |r| {
            let created_at: String = r.get(0)?;
            Ok((
                created_at,
                Record {
                    // Replaced below; a parse failure decides the row's fate
                    // outside the closure, where an error can be reported.
                    at: from,
                    alias: r.get(1)?,
                    provider: r.get(2)?,
                    model_id: r.get(3)?,
                    observed: Observed {
                        prompt: r.get::<_, i64>(4)?.max(0) as u64,
                        completion: r.get::<_, i64>(5)?.max(0) as u64,
                        cache_read: r.get::<_, i64>(6)?.max(0) as u64,
                        cache_write: r.get::<_, i64>(7)?.max(0) as u64,
                    },
                    duration_ms: r.get(8)?,
                    status: r.get(9)?,
                    streamed: r.get(10)?,
                    surface: r.get(11)?,
                    cost_usd: r.get(12)?,
                    unpriced: r.get(13)?,
                },
            ))
        })
        .map_err(|e| format!("reading gateway usage: {e}"))?;

    let mut out = Vec::new();
    for row in rows {
        let (created_at, mut record) = row.map_err(|e| format!("reading gateway usage: {e}"))?;
        // A row this process cannot place in time is **skipped**, not defaulted:
        // it can only come from a hand-edited database, and folding it into the
        // window's first bucket would put spend on a day it did not happen.
        match gotime::GoTime::parse_go_string(&created_at) {
            Ok(t) => record.at = t.instant(),
            Err(e) => {
                log::warn!("gateway usage row has an unreadable created_at, skipping it: {e}");
                continue;
            }
        }
        out.push(record);
    }
    Ok(out)
}

#[cfg(test)]
mod window_tests {
    use super::*;

    /// The property [`load_window`]'s `WHERE` clause rests on.
    ///
    /// If text order ever stopped matching time order, the window would return
    /// the wrong rows — silently, and only near a boundary, which is the
    /// hardest kind of wrong to notice on a chart.
    #[test]
    fn the_stored_timestamp_sorts_as_text_the_way_it_sorts_in_time() {
        use chrono::TimeZone;
        let base = Utc.with_ymd_and_hms(2026, 8, 7, 23, 59, 59).unwrap();
        // Deliberately includes the no-fraction case beside fractioned ones in
        // the same second, which is where a naive format breaks.
        let instants = [
            Utc.with_ymd_and_hms(2026, 8, 7, 23, 59, 58).unwrap(),
            base,
            base + chrono::Duration::nanoseconds(250_000_000),
            base + chrono::Duration::nanoseconds(500_000_000),
            base + chrono::Duration::nanoseconds(510_000_000),
            base + chrono::Duration::nanoseconds(999_999_999),
            Utc.with_ymd_and_hms(2026, 8, 8, 0, 0, 0).unwrap(),
        ];
        let texts: Vec<String> = instants.iter().map(gotime::go_text).collect();

        let mut sorted = texts.clone();
        sorted.sort();
        assert_eq!(
            sorted, texts,
            "chronological order and text order must agree, or a windowed query \
             returns the wrong rows near its bounds"
        );
    }
}
