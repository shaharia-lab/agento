//! Alias → ordered targets, retry, and the fallback walk (#424).
//!
//! This is the gateway's router, and it is deliberately small. ferrox has a
//! `ModelRouter`/`RoutePool` pair (~320 lines) carrying weighted and
//! round-robin load balancing and a circuit breaker; v1 of this gateway has
//! neither, so what is left is a map from the alias a client sends as `model`
//! to an ordered list of `(adapter, upstream model_id)` pairs, walked until one
//! answers.
//!
//! # Three things a port of this gets wrong
//!
//! **The adapter takes `model_id` separately from `req.model`.**
//! [`ProviderAdapter::chat`] is `(&req, model_id)`, and `req.model` is left
//! exactly as the caller sent it — the alias. Passing the alias as `model_id`
//! too sends the alias upstream, where it is not a model.
//!
//! **`is_retryable` and `should_failover` are not the same predicate, and the
//! difference is one status.** They live in ferrox's **binary** crate
//! (`ferrox/src/retry.rs`), not in `ferrox-providers`, so they cannot be
//! imported and are copied below with their reasoning intact. A 403 from
//! *upstream* means quota on some providers, so it fails over without
//! retrying; the gateway's own 403 is a different variant and must not.
//!
//! **The registry is built once per listener start, not per request.**
//! `build_registry` constructs a `reqwest::Client` per provider, and doing that
//! per request would leak a connection pool on every call. A reload rebuilds
//! it, which is what makes a provider edit take effect.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ferrox_providers::config::RetryConfig;
use ferrox_providers::error::ProxyError;
use ferrox_providers::providers::{
    build_registry, ProviderAdapter, ProviderRegistry, ProviderStream,
};
use ferrox_providers::types::{ChatCompletionRequest, ChatCompletionResponse};

use super::config;

/// One place a request may be sent: an adapter, and the model id to ask it for.
///
/// `provider` is carried alongside because it is what a usage row (#425) and
/// every log line name — the adapter's own `name()` is the same string, but
/// reading it back through the trait object per call is noise.
pub struct Target {
    pub adapter: Arc<dyn ProviderAdapter>,
    pub provider: String,
    pub model_id: String,
}

/// What answered, for the caller's log line and #425's usage row.
pub struct Served {
    pub provider: String,
    pub model_id: String,
}

/// A dispatch that produced no response, and the target it died on.
///
/// `attempted` exists so a usage row can name the provider that failed. Without
/// it every failure row carries an empty `provider`, which makes the column
/// useless for the one question a failure row is asked — *which provider is
/// erroring* — and it cannot be reconstructed afterwards. It is `None` only
/// when resolution itself failed (an unknown alias, or one with no enabled
/// target), where there genuinely was no provider.
///
/// It names the **last** target tried, not the first: the walk gives up on the
/// last one, and on a chain whose earlier links merely failed over, the last is
/// the one whose failure is the answer.
pub struct Failure {
    pub error: ProxyError,
    pub attempted: Option<Served>,
}

impl Served {
    /// Fill in the two fields only dispatch knows.
    ///
    /// A seed is built before dispatch (it needs the request's clock and token,
    /// which are gone by the time a target is chosen) and completed after, so
    /// the row names the target that actually answered rather than the first
    /// one tried — which matters precisely when a fallback served it, since
    /// that is the request whose provider a bill will not match.
    pub fn into_seed(self, seed: super::usage::Seed) -> super::usage::Seed {
        super::usage::Seed {
            provider: self.provider,
            model_id: self.model_id,
            ..seed
        }
    }
}

/// The routing table and the adapters behind it, built once per listener start.
pub struct Dispatcher {
    registry: ProviderRegistry,
    /// alias → the routing rows in preference order (`targets` then
    /// `fallbacks`), as stored. Resolution against the adapter registry happens
    /// per request, so a provider missing from the registry is a per-alias
    /// failure rather than a build failure.
    routes: BTreeMap<String, Vec<config::RouteTarget>>,
    retry: RetryConfig,
}

impl Dispatcher {
    /// Read the provider and alias tables and build every adapter.
    ///
    /// Blocking: both loads open the database. Callers are on the listener's
    /// start path, not a request, and go through [`crate::native::db::blocking`].
    pub async fn build(db_path: &Path) -> Result<Self, String> {
        let path = db_path.to_path_buf();
        let providers =
            crate::native::db::blocking("gateway providers", move || config::load_providers(&path))
                .await
                .ok_or_else(|| {
                    "reading gateway providers: the database task failed".to_string()
                })??;

        let path = db_path.to_path_buf();
        let aliases = crate::native::db::blocking("gateway model aliases", move || {
            config::load_aliases(&path)
        })
        .await
        .ok_or_else(|| "reading gateway model aliases: the database task failed".to_string())??;

        // `load_providers` returns disabled rows too — its doc comment says
        // "every enabled provider" and the SQL has no WHERE clause. Filter here
        // rather than trusting the comment.
        let configs: Vec<_> = providers
            .iter()
            .filter(|row| row.enabled)
            .map(|row| row.to_ferrox())
            .collect();

        let defaults = config::defaults();
        let registry = build_registry(&configs, &defaults)
            .await
            // `anyhow::Error`'s Display carries the provider name via
            // `with_context`, which is the actionable half; the chain below it
            // is a reqwest builder error nobody can act on.
            .map_err(|e| format!("building gateway providers: {e}"))?;

        let routes = aliases
            .into_iter()
            .filter(|alias| alias.enabled)
            .map(|alias| {
                let mut ordered = alias.routing.targets;
                ordered.extend(alias.routing.fallbacks);
                (alias.alias, ordered)
            })
            .collect();

        Ok(Self {
            registry,
            routes,
            retry: defaults.retry,
        })
    }

    /// The aliases this gateway serves, for `/v1/models` and
    /// `/anthropic/v1/models`. Sorted, because `load_aliases` orders by alias
    /// and a `BTreeMap` keeps it that way — a models list that reordered run to
    /// run would be a poor answer to a question clients cache.
    pub fn aliases(&self) -> Vec<&str> {
        self.routes.keys().map(String::as_str).collect()
    }

    /// Whether any provider was configured at all.
    ///
    /// The models routes answer a typed error rather than an empty `200` when
    /// this is false: an empty list is a truthful answer to "which aliases do
    /// you serve" and a misleading one to "is this gateway configured", and a
    /// client that gets `{"data":[]}` has nothing to report to its user.
    pub fn has_providers(&self) -> bool {
        !self.registry.is_empty()
    }

    /// The ordered targets an alias resolves to.
    ///
    /// Two failures, and they are different: an alias nobody configured is the
    /// client's mistake (`404 model_not_found`), while an alias whose every
    /// target names a provider that is disabled or absent is the operator's
    /// (`500 config_error`) — retrying either is pointless, but only the second
    /// is a thing to go and fix in Settings.
    pub fn resolve(&self, alias: &str) -> Result<Vec<Target>, ProxyError> {
        let Some(rows) = self.routes.get(alias) else {
            return Err(ProxyError::ModelNotFound(format!(
                "model alias '{alias}' is not configured on this gateway"
            )));
        };

        let targets: Vec<Target> = rows
            .iter()
            .filter_map(|row| {
                self.registry.get(&row.provider).map(|adapter| Target {
                    adapter: Arc::clone(adapter),
                    provider: row.provider.clone(),
                    model_id: row.model_id.clone(),
                })
            })
            .collect();

        if targets.is_empty() {
            return Err(ProxyError::ConfigError(format!(
                "model alias '{alias}' has no enabled provider to route to"
            )));
        }
        Ok(targets)
    }

    /// A non-streaming completion, retried on the same target and then walked
    /// down the fallback chain.
    pub async fn chat(
        &self,
        alias: &str,
        req: &ChatCompletionRequest,
    ) -> Result<(ChatCompletionResponse, Served), Failure> {
        self.walk(alias, |adapter, model_id| async move {
            adapter.chat(req, &model_id).await
        })
        .await
    }

    /// A streaming completion. Only the *handshake* is retried — once the
    /// upstream has started sending chunks the response head here is already
    /// committed, so a mid-stream failure cannot be failed over without
    /// replaying tokens the client has seen.
    pub async fn chat_stream(
        &self,
        alias: &str,
        req: &ChatCompletionRequest,
    ) -> Result<(ProviderStream, Served), Failure> {
        self.walk(alias, |adapter, model_id| async move {
            adapter.chat_stream(req, &model_id).await
        })
        .await
    }

    /// Resolve, then try each target in turn with retries inside.
    ///
    /// The callback takes **owned** handles rather than a `&Target`: a borrow
    /// would have to outlive the future, and the obvious ways to arrange that
    /// (leaking the slice, or an `Arc<Vec<Target>>` threaded through every
    /// closure) each cost more than cloning an `Arc` and a short `String` once
    /// per attempt.
    async fn walk<T, F, Fut>(&self, alias: &str, call: F) -> Result<(T, Served), Failure>
    where
        F: Fn(Arc<dyn ProviderAdapter>, String) -> Fut,
        Fut: std::future::Future<Output = Result<T, ProxyError>>,
    {
        let targets = self.resolve(alias).map_err(|error| Failure {
            error,
            // Resolution failed, so no provider was ever chosen. This is the
            // only `None`, and it is what a `refused` row's empty provider
            // column truthfully means.
            attempted: None,
        })?;

        let mut last: Option<ProxyError> = None;
        let mut attempted: Option<Served> = None;
        for target in &targets {
            let attempt = || call(Arc::clone(&target.adapter), target.model_id.clone());
            match execute_with_retry(&self.retry, attempt).await {
                Ok(value) => {
                    return Ok((
                        value,
                        Served {
                            provider: target.provider.clone(),
                            model_id: target.model_id.clone(),
                        },
                    ))
                }
                Err(e) => {
                    attempted = Some(Served {
                        provider: target.provider.clone(),
                        model_id: target.model_id.clone(),
                    });
                    if !should_failover(&e) {
                        return Err(Failure {
                            error: e,
                            attempted,
                        });
                    }
                    log::warn!(
                        "gateway target failed, walking the chain: alias={alias:?} \
                         provider={:?} model_id={:?} error={e}",
                        target.provider,
                        target.model_id
                    );
                    last = Some(e);
                }
            }
        }

        Err(Failure {
            error: last.unwrap_or_else(|| {
                ProxyError::ConfigError(format!("model alias '{alias}' produced no target to try"))
            }),
            attempted,
        })
    }
}

// ── Retry, copied from ferrox/src/retry.rs ───────────────────────────────────
//
// These live in ferrox's binary crate rather than in `ferrox-providers`, so
// there is nothing to import. Copied with the reasoning, because the reasoning
// is the part that is easy to get wrong.

/// Whether an error is worth retrying **on the same target**.
///
/// Verbatim from `ferrox/src/retry.rs::is_retryable`, including the `429`
/// arm's placement: a `ProviderError` carrying 429 is an upstream rate limit
/// and backing off helps, while `ProxyError::RateLimited` is the gateway's own
/// verdict about the client and backing off does not.
pub fn is_retryable(e: &ProxyError) -> bool {
    match e {
        // Transient — retry on same target
        ProxyError::UpstreamTimeout(_) => true,
        ProxyError::CircuitOpen(_) => true,
        ProxyError::ProviderError { status, .. } => *status >= 500 || *status == 429,
        ProxyError::HttpClientError(e) => e.is_timeout() || e.is_connect(),
        ProxyError::StreamError(_) => true,

        // Non-transient — do not retry
        ProxyError::Unauthorized(_) => false,
        ProxyError::Forbidden(_) => false,
        ProxyError::ModelNotFound(_) => false,
        ProxyError::RateLimited(_) => false,
        ProxyError::ConfigError(_) => false,
        ProxyError::SerializationError(_) => false,
        ProxyError::AwsError(_) => false,
        ProxyError::BudgetExceeded(_) => false,
    }
}

/// Whether a target's failure should degrade to the next target in the chain.
///
/// Verbatim from `ferrox/src/retry.rs::should_failover`, and deliberately
/// **broader than [`is_retryable`] by exactly one case**: an upstream **403**.
///
/// Why 403: some providers report plan or quota exhaustion with 403 rather than
/// 429 (Moonshot Kimi's `access_terminated_error` when a coding-plan billing
/// cycle is used up). Retrying the *same* provider is pointless — it will keep
/// answering 403 — so `is_retryable` leaves it alone and no backoff is spent on
/// a provider known to be exhausted. But the request should still be served
/// from the other provider.
///
/// It matches on the **upstream** provider's status (`ProviderError`). This
/// gateway's own auth rejection is `ProxyError::Forbidden`, which must NOT fail
/// over — a client presenting a bad token should fail closed rather than burn
/// the next provider's quota.
pub fn should_failover(e: &ProxyError) -> bool {
    is_retryable(e) || matches!(e, ProxyError::ProviderError { status: 403, .. })
}

/// `min(initial_ms * 2^attempt, max_ms)` plus `jitter_ms`.
///
/// Split out from the sleep so the formula is testable without waiting for it,
/// and so the jitter is an argument rather than a call into a random number
/// generator: ferrox uses `rand::thread_rng`, and `rand` is not a direct
/// dependency of this crate. [`jitter_for`] supplies the value from the clock,
/// which is enough to keep concurrent retries from re-colliding and is the only
/// thing the jitter is for.
pub fn backoff_duration(attempt: u32, config: &RetryConfig, jitter_ms: u64) -> Duration {
    let shift = attempt.min(20);
    let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let base = config.initial_backoff_ms.saturating_mul(multiplier);
    let capped = base.min(config.max_backoff_ms);
    Duration::from_millis(capped.saturating_add(jitter_ms))
}

/// A jitter in `[0, initial_backoff_ms]`, from the wall clock's sub-second part.
fn jitter_for(config: &RetryConfig) -> u64 {
    if !config.jitter || config.initial_backoff_ms == 0 {
        return 0;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    nanos % (config.initial_backoff_ms + 1)
}

/// Run `f` up to `max_attempts` times, backing off between retryable failures.
///
/// Ported from `ferrox/src/retry.rs::execute_with_retry` without its Prometheus
/// counters. A non-retryable error returns immediately — that is what makes a
/// `400` cost one call rather than three.
async fn execute_with_retry<F, Fut, T>(config: &RetryConfig, f: F) -> Result<T, ProxyError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, ProxyError>>,
{
    let max_attempts = config.max_attempts.max(1);
    let mut last_error: Option<ProxyError> = None;

    for attempt in 0..max_attempts {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !is_retryable(&e) {
                    return Err(e);
                }
                last_error = Some(e);
                if attempt + 1 < max_attempts {
                    tokio::time::sleep(backoff_duration(attempt, config, jitter_for(config))).await;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| ProxyError::ProviderError {
        provider: String::new(),
        status: 502,
        message: "All retry attempts exhausted".to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn provider_error(status: u16) -> ProxyError {
        ProxyError::ProviderError {
            provider: "p".into(),
            status,
            message: "upstream said so".into(),
        }
    }

    fn fast_retry(max_attempts: u32) -> RetryConfig {
        RetryConfig {
            max_attempts,
            initial_backoff_ms: 0,
            max_backoff_ms: 0,
            jitter: false,
        }
    }

    /// The truth table, asserted in **both** directions.
    ///
    /// The false cells are the ones that matter: a predicate that answered
    /// `true` everywhere would pass an all-positive test while turning a
    /// malformed request into three upstream calls.
    #[test]
    fn the_retry_predicates_differ_by_exactly_the_upstream_403() {
        // Retryable, and therefore also failed over.
        for e in [
            ProxyError::UpstreamTimeout("t".into()),
            ProxyError::CircuitOpen("c".into()),
            ProxyError::StreamError("s".into()),
            provider_error(500),
            provider_error(503),
            provider_error(429),
        ] {
            assert!(is_retryable(&e), "should retry: {e}");
            assert!(should_failover(&e), "should fail over: {e}");
        }

        // Not retryable and not failed over.
        for e in [
            ProxyError::Unauthorized("u".into()),
            ProxyError::Forbidden("f".into()),
            ProxyError::ModelNotFound("m".into()),
            ProxyError::RateLimited("r".into()),
            ProxyError::ConfigError("c".into()),
            ProxyError::BudgetExceeded("b".into()),
            ProxyError::AwsError("a".into()),
            provider_error(400),
            provider_error(404),
            provider_error(422),
        ] {
            assert!(!is_retryable(&e), "must not retry: {e}");
            assert!(!should_failover(&e), "must not fail over: {e}");
        }

        // The one asymmetric case, and the reason the two functions exist.
        let quota = provider_error(403);
        assert!(
            !is_retryable(&quota),
            "an upstream 403 is quota — retrying the same provider only burns backoff"
        );
        assert!(
            should_failover(&quota),
            "an upstream 403 must still be served from the next target"
        );

        // ...and the gateway's own 403, which is a different variant and must
        // not reach the next provider's credentials.
        assert!(
            !should_failover(&ProxyError::Forbidden("bad token".into())),
            "the gateway's own Forbidden must fail closed, not burn a fallback"
        );
    }

    #[test]
    fn backoff_doubles_and_then_caps() {
        let config = RetryConfig {
            max_attempts: 5,
            initial_backoff_ms: 100,
            max_backoff_ms: 2000,
            jitter: false,
        };
        assert_eq!(backoff_duration(0, &config, 0).as_millis(), 100);
        assert_eq!(backoff_duration(1, &config, 0).as_millis(), 200);
        assert_eq!(backoff_duration(2, &config, 0).as_millis(), 400);
        assert_eq!(backoff_duration(3, &config, 0).as_millis(), 800);
        assert_eq!(backoff_duration(4, &config, 0).as_millis(), 1600);
        // Capped, not overflowed — the shift is clamped at 20 so a large
        // attempt count cannot wrap the multiplier to something small.
        assert_eq!(backoff_duration(5, &config, 0).as_millis(), 2000);
        assert_eq!(backoff_duration(64, &config, 0).as_millis(), 2000);
        // Jitter is added after the cap, exactly as ferrox does it.
        assert_eq!(backoff_duration(5, &config, 37).as_millis(), 2037);
    }

    #[test]
    fn jitter_stays_within_one_initial_backoff() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 2000,
            jitter: true,
        };
        for _ in 0..64 {
            assert!(jitter_for(&config) <= 100);
        }
        let off = RetryConfig {
            jitter: false,
            ..config
        };
        assert_eq!(jitter_for(&off), 0);
    }

    #[tokio::test]
    async fn a_retryable_error_is_retried_and_a_terminal_one_is_not() {
        let calls = AtomicU32::new(0);
        let result: Result<(), _> = execute_with_retry(&fast_retry(3), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(provider_error(429)) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 3, "429 retries to the limit");

        let calls = AtomicU32::new(0);
        let result: Result<(), _> = execute_with_retry(&fast_retry(3), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(provider_error(400)) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "400 is asked exactly once");
    }

    #[tokio::test]
    async fn a_retry_that_eventually_succeeds_stops_calling() {
        let calls = AtomicU32::new(0);
        let result = execute_with_retry(&fast_retry(5), || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(provider_error(503))
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert_eq!(result.expect("third attempt succeeds"), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn max_attempts_of_zero_still_calls_once() {
        let calls = AtomicU32::new(0);
        let _: Result<(), _> = execute_with_retry(&fast_retry(0), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(provider_error(400)) }
        })
        .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "`max(1)` is what stops a zero in the config from serving nothing"
        );
    }
}
