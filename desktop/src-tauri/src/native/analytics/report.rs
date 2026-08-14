//! The `GET /api/claude-analytics` payload and every aggregate in it.
//!
//! Mirrors `internal/claudesessions/analytics.go` builder for builder. Read
//! that file alongside this one; the comments there carry the *why* and are not
//! repeated here except where the port itself needs a decision.
//!
//! ## What decides the bytes
//!
//! - **Field order is wire order.** Every struct declares its fields in the
//!   order the Go struct declares them.
//! - **Every cost figure is read, never re-derived.** Since #188 the scan
//!   stores each session's cost, priced per assistant message at that message's
//!   own model and instant. Re-pricing here could only approximate it by
//!   picking one model and one instant for a whole session — and would make the
//!   session list and the dashboard disagree.
//! - **Accumulation order is part of the answer.** Floating-point addition is
//!   not associative, and Go's summary sums the four cost categories separately
//!   before totalling them while the cache-savings card sums `total_usd` per
//!   session. On the reference corpus those two arrive at 30775.990068829993
//!   and 30775.990068829982 for the same money. Both spellings are reproduced
//!   by summing in the same order over the same slice.
//!
//! ## Where Go is not deterministic, and this is
//!
//! Several builders collect into a Go map and then sort with `sort.Slice`,
//! which is unstable — so **two rows tying on the sort key come out in a random
//! order, and Go's own response is not byte-stable across requests**. It is
//! observable: on the reference corpus `sessions_per_model` has two models with
//! one session each, and repeated uncached requests swap them.
//!
//! Those builders use a `BTreeMap` here plus a stable sort, so a tie breaks on
//! the model or project name and the response is reproducible. That is a strict
//! improvement, but it means a live diff can legitimately differ from *one* of
//! Go's orderings — which is why the live parity test retries.

use std::collections::BTreeMap;

use chrono_tz::Tz;
use serde::Serialize;

use super::buckets::{bucket_key, walk_buckets, walk_session_hours, Granularity};
use super::cards::{build_insight_cards, InsightCard};
use super::params::AnalyticsParams;
use crate::native::gotime::GoTime;
use crate::native::sessions::summary::{display_model, SessionCost, SessionSummary};

/// The placeholder Claude Code records for locally generated events that never
/// hit the API. Billed at zero and kept out of the model breakdowns rather than
/// priced as a real model.
pub const SYNTHETIC_MODEL: &str = "<synthetic>";

/// Names the folded tail row of the project table. The UI has to recognize it:
/// it is a bucket, not a project, so it must not be clickable as a filter.
const OTHER_PROJECTS_LABEL: &str = "Other projects";

/// How many projects the table lists before folding the tail into one row.
const TOP_PROJECTS_LISTED: usize = 20;
/// How many projects the project×bucket strip charts.
const TOP_PROJECTS_CHARTED: usize = 8;
/// How many rows each leaderboard carries.
const TOP_SESSIONS_PER_BOARD: usize = 10;

// ─── Output types ─────────────────────────────────────────────────────────────

/// The complete response payload.
#[derive(Debug, Serialize)]
pub struct AnalyticsReport {
    pub summary: AnalyticsSummary,
    pub time_series: Vec<TimeSeriesPoint>,
    pub cache_efficiency: Vec<CacheEfficiencyPoint>,
    pub model_breakdown: Vec<ModelStat>,
    pub sessions_per_model: Vec<ModelSessionStat>,
    pub cost_by_model: Vec<ModelCostStat>,
    pub insight_cards: Vec<InsightCard>,
    pub project_breakdown: Vec<ProjectStat>,
    pub project_activity: Vec<ProjectDayActivity>,
    pub top_sessions: TopSessions,
    pub cost_over_time_by_model: Vec<StackedCostPoint>,
    pub most_active_days: Vec<DayActivity>,
    pub heatmap: Vec<HeatmapCell>,
    pub hourly_activity: Vec<HourlyActivity>,
    pub cost_over_time: Vec<CostPoint>,
    pub cost_summary: CostSummary,
    pub projects: Vec<String>,
    /// The bucket width every series here was built at. It travels with the
    /// report because a key alone no longer says how wide its bucket is.
    pub granularity: String,
}

/// The top-level KPI values.
#[derive(Debug, Default, Serialize)]
pub struct AnalyticsSummary {
    pub total_sessions: i64,
    /// The projects the *filtered* sessions belong to — not the length of
    /// `AnalyticsReport::projects`, which is built before filtering because it
    /// populates the picker.
    pub unique_projects: i64,
    pub total_tokens: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read_tokens: i64,
    pub total_cache_creation_tokens: i64,
    pub most_used_model: String,
    pub avg_tokens_per_session: f64,
    pub estimated_cost_usd: f64,
    /// Tokens belonging to models with no published rates. They contribute
    /// nothing to `estimated_cost_usd`; the count keeps that omission visible
    /// rather than making the total look complete.
    pub unknown_pricing_tokens: i64,
    /// `None` only in the empty report, where Go leaves the zero-valued struct's
    /// nil slice to marshal as `null`.
    pub unknown_pricing_models: Option<Vec<String>>,
}

/// One bucket of the token-usage-over-time chart.
#[derive(Debug, Default, Clone, Serialize)]
pub struct TimeSeriesPoint {
    pub date: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    #[serde(rename = "cache_creation_tokens")]
    pub cache_write_tokens: i64,
    pub total_tokens: i64,
    pub sessions: i64,
}

/// Per-bucket cache hit rate.
#[derive(Debug, Serialize)]
pub struct CacheEfficiencyPoint {
    pub date: String,
    /// 0–100 %.
    pub cache_hit_rate: f64,
    pub cached_tokens: i64,
    /// Every input-side token in the bucket — fresh input, cache reads and
    /// cache writes — which is the denominator `cache_hit_rate` is taken over.
    pub total_input_tokens: i64,
}

/// Token distribution across models.
#[derive(Debug, Serialize)]
pub struct ModelStat {
    pub model: String,
    pub tokens: i64,
    pub percentage: f64,
}

/// One model's share of spend.
#[derive(Debug, Serialize)]
pub struct ModelCostStat {
    pub model: String,
    /// Derived from the model id, so it needs no catalog lookup and stays
    /// correct for a model the catalog has no rate for.
    pub provider: String,
    pub cost: SessionCost,
    pub percentage: f64,
    /// How many sessions this model spent money in — context for a large
    /// total, which may be one runaway session or a hundred small ones.
    pub sessions: i64,
}

/// One bucket's cost split by model. The values sum to the same bucket's
/// `CostPoint`.
#[derive(Debug, Serialize)]
pub struct StackedCostPoint {
    pub date: String,
    /// Buckets are independent: a model absent from one spent nothing in it.
    pub cost_by_model: BTreeMap<String, f64>,
}

/// One project's activity over the window.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ProjectStat {
    pub project: String,
    pub sessions: i64,
    /// Conversation tokens (input+output); `total_tokens` includes cache
    /// traffic. Both are reported because they answer different questions.
    pub tokens: i64,
    pub total_tokens: i64,
    pub cost: SessionCost,
    /// Share of the window's cost.
    pub percentage: f64,
    pub last_activity: GoTime,
    /// Non-zero only on the "Other projects" row, saying how many projects it
    /// stands for.
    #[serde(skip_serializing_if = "is_zero")]
    pub folded_projects: i64,
}

/// One project's activity in one local bucket, for the "what did I work on
/// when" strip.
#[derive(Debug, Serialize)]
pub struct ProjectDayActivity {
    pub project: String,
    pub date: String,
    pub sessions: i64,
    pub cost_usd: f64,
}

/// One row of a leaderboard.
#[derive(Debug, Clone, Serialize)]
pub struct SessionRanking {
    pub session_id: String,
    pub title: String,
    pub project: String,
    pub model: String,
    pub cost_usd: f64,
    pub duration_ms: i64,
    pub tokens: i64,
    pub subagent_count: i64,
    pub last_activity: GoTime,
}

/// The leaderboards: the same sessions ranked three ways, because "expensive",
/// "long" and "large" pick out different sessions.
#[derive(Debug, Serialize)]
pub struct TopSessions {
    pub by_cost: Vec<SessionRanking>,
    pub by_duration: Vec<SessionRanking>,
    pub by_tokens: Vec<SessionRanking>,
}

/// Session count per model.
#[derive(Debug, Serialize)]
pub struct ModelSessionStat {
    pub model: String,
    pub sessions: i64,
}

/// Aggregated activity for a single calendar day.
#[derive(Debug, Serialize)]
pub struct DayActivity {
    pub date: String,
    pub sessions: i64,
    pub tokens: i64,
}

/// One cell of the day-of-week × hour-of-day grid.
#[derive(Debug, Serialize)]
pub struct HeatmapCell {
    /// 0=Sunday … 6=Saturday.
    pub day_of_week: i64,
    /// 0–23.
    pub hour: i64,
    pub sessions: i64,
    pub tokens: i64,
}

/// Activity for each hour of the day.
#[derive(Debug, Serialize)]
pub struct HourlyActivity {
    pub hour: i64,
    pub sessions: i64,
    pub tokens: i64,
}

/// Estimated USD cost for a single bucket.
#[derive(Debug, Default, Clone, Serialize)]
pub struct CostPoint {
    pub date: String,
    pub estimated_cost_usd: f64,
}

/// Total cost broken down by token category.
#[derive(Debug, Default, Serialize)]
pub struct CostSummary {
    pub input_cost_usd: f64,
    pub output_cost_usd: f64,
    pub cache_read_cost_usd: f64,
    pub cache_write_cost_usd: f64,
    pub total_cost_usd: f64,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

// ─── Aggregation ──────────────────────────────────────────────────────────────

/// Build the report from a corpus of session summaries. Filtering, bucketing
/// and aggregation all happen in memory — no disk I/O.
pub fn aggregate(
    sessions: &[SessionSummary],
    p: &AnalyticsParams,
    resolver: Option<&crate::native::pricing::Resolver>,
) -> AnalyticsReport {
    // Collected before filtering: the picker must keep offering every project,
    // or a user who filters into an empty window cannot filter back out of it.
    let projects: Vec<String> = sessions
        .iter()
        .map(|s| s.project_path.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    let loc = p.loc;
    let granularity = p.granularity();
    let filtered = filter_sessions(sessions, p);

    if filtered.is_empty() {
        return empty_report(projects, loc, granularity);
    }

    let (summary, cost_summary) = build_summary(&filtered);
    let project_breakdown = build_project_breakdown(&filtered);
    let cost_by_model = build_cost_by_model(&filtered);
    let time_series = build_time_series(&filtered, p, granularity, loc);

    AnalyticsReport {
        summary,
        cache_efficiency: build_cache_efficiency(&time_series),
        time_series,
        model_breakdown: build_model_breakdown(&filtered),
        sessions_per_model: build_sessions_per_model(&filtered),
        insight_cards: build_insight_cards(&filtered, &cost_by_model, resolver),
        cost_by_model,
        project_activity: build_project_activity(&filtered, &project_breakdown, granularity, loc),
        project_breakdown,
        top_sessions: build_top_sessions(&filtered),
        cost_over_time_by_model: build_cost_over_time_by_model(&filtered, p, granularity, loc),
        most_active_days: build_most_active_days(&filtered, loc),
        heatmap: build_heatmap(&filtered, loc),
        hourly_activity: build_hourly_activity(&filtered, loc),
        cost_over_time: build_cost_over_time(&filtered, p, granularity, loc),
        cost_summary,
        projects,
        granularity: granularity.as_str().to_string(),
    }
}

/// What a window with no sessions returns.
///
/// Every slice is empty rather than nil so the JSON carries `[]` — with one
/// deliberate exception: `summary` is Go's zero-valued struct, whose nil
/// `unknown_pricing_models` marshals as `null`. `projects` is still populated.
fn empty_report(projects: Vec<String>, loc: Tz, granularity: Granularity) -> AnalyticsReport {
    AnalyticsReport {
        summary: AnalyticsSummary::default(),
        time_series: Vec::new(),
        cache_efficiency: Vec::new(),
        model_breakdown: Vec::new(),
        sessions_per_model: Vec::new(),
        cost_by_model: Vec::new(),
        insight_cards: Vec::new(),
        project_breakdown: Vec::new(),
        project_activity: Vec::new(),
        top_sessions: TopSessions {
            by_cost: Vec::new(),
            by_duration: Vec::new(),
            by_tokens: Vec::new(),
        },
        cost_over_time_by_model: Vec::new(),
        most_active_days: Vec::new(),
        heatmap: Vec::new(),
        hourly_activity: build_hourly_activity(&[], loc),
        cost_over_time: Vec::new(),
        cost_summary: CostSummary::default(),
        projects,
        granularity: granularity.as_str().to_string(),
    }
}

/// The single definition of "which sessions does this window contain": last
/// activity within `[from, to]` inclusive, and — when set — matching project.
///
/// Deliberately *not* the sessions list's overlap rule. More than one endpoint
/// has to agree on this, and the insights summary answering it with its own SQL
/// predicate over `start_time` is what made two dashboards report different
/// totals for one window.
fn filter_sessions<'a>(
    sessions: &'a [SessionSummary],
    p: &AnalyticsParams,
) -> Vec<&'a SessionSummary> {
    sessions
        .iter()
        .filter(|s| {
            let at = s.last_activity.instant();
            at >= p.from && at <= p.to && (p.project.is_empty() || s.project_path == p.project)
        })
        .collect()
}

fn build_summary(sessions: &[&SessionSummary]) -> (AnalyticsSummary, CostSummary) {
    let (mut input, mut output, mut cache_read, mut cache_write) = (0i64, 0i64, 0i64, 0i64);
    let mut model_count: BTreeMap<String, i64> = BTreeMap::new();
    let mut projects: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut cost = CostSummary::default();
    let mut unpriced_models: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut unpriced_tokens = 0i64;

    for s in sessions {
        projects.insert(&s.project_path);
        let u = s.total_usage();
        input += u.input_tokens;
        output += u.output_tokens;
        cache_read += u.cache_read_tokens;
        cache_write += u.cache_creation_tokens;

        let m = display_model(&s.model);
        if m != SYNTHETIC_MODEL {
            // Kept out of most_used_model for the same reason it is kept out of
            // the model breakdowns — it is not a model anyone ran.
            *model_count.entry(m).or_default() += 1;
        }

        // Read, not re-derived; and summed category by category, which is what
        // makes this total a different double from the card's.
        let c = s.total_cost();
        cost.input_cost_usd += c.input_usd;
        cost.output_cost_usd += c.output_usd;
        cost.cache_read_cost_usd += c.cache_read_usd;
        cost.cache_write_cost_usd += c.cache_write_usd;

        for m in &s.unpriced_models {
            unpriced_models.insert(m.clone());
        }
        unpriced_tokens += s.unpriced_tokens;
    }
    cost.total_cost_usd = cost.input_cost_usd
        + cost.output_cost_usd
        + cost.cache_read_cost_usd
        + cost.cache_write_cost_usd;

    let total = input + output;
    let avg = if sessions.is_empty() {
        0.0
    } else {
        round_to(total as f64 / sessions.len() as f64, 10.0)
    };

    (
        AnalyticsSummary {
            total_sessions: sessions.len() as i64,
            unique_projects: projects.len() as i64,
            total_tokens: total,
            total_input_tokens: input,
            total_output_tokens: output,
            total_cache_read_tokens: cache_read,
            total_cache_creation_tokens: cache_write,
            most_used_model: most_frequent(&model_count),
            avg_tokens_per_session: avg,
            estimated_cost_usd: cost.total_cost_usd,
            unknown_pricing_tokens: unpriced_tokens,
            unknown_pricing_models: Some(unpriced_models.into_iter().collect()),
        },
        cost,
    )
}

/// The key with the highest count, or `""` when empty. A tie takes the
/// lexically first key; Go takes whichever its map iteration reached first.
fn most_frequent(counts: &BTreeMap<String, i64>) -> String {
    let mut best = ("", 0i64);
    for (k, c) in counts {
        if *c > best.1 {
            best = (k, *c);
        }
    }
    best.0.to_string()
}

fn build_time_series(
    sessions: &[&SessionSummary],
    p: &AnalyticsParams,
    granularity: Granularity,
    loc: Tz,
) -> Vec<TimeSeriesPoint> {
    let mut buckets: BTreeMap<String, TimeSeriesPoint> = BTreeMap::new();
    for s in sessions {
        let key = bucket_key(s.last_activity.instant(), granularity, loc);
        let b = buckets
            .entry(key.clone())
            .or_insert_with(|| TimeSeriesPoint {
                date: key,
                ..Default::default()
            });
        let u = s.total_usage();
        b.input_tokens += u.input_tokens;
        b.output_tokens += u.output_tokens;
        b.cache_read_tokens += u.cache_read_tokens;
        b.cache_write_tokens += u.cache_creation_tokens;
        b.total_tokens += u.input_tokens + u.output_tokens;
        b.sessions += 1;
    }

    let mut out = Vec::new();
    walk_buckets(p.from, p.to, granularity, loc, |key, _| {
        out.push(
            buckets
                .get(key)
                .cloned()
                .unwrap_or_else(|| TimeSeriesPoint {
                    date: key.to_string(),
                    ..Default::default()
                }),
        );
    });
    out
}

/// The read share of *every* input-side token — fresh input, cache writes and
/// cache reads together.
///
/// The one definition in the codebase (`cache_hit_rate.go`). Two formulas used
/// to share the name: `cacheRead/(input+cacheRead)` is pinned near 100% for any
/// long conversation and so carries no information, and
/// `cacheRead/(cacheCreation+cacheRead)` silently excuses a model that never
/// caches at all. This denominator is the only one under which such a model
/// scores 0.
pub fn cache_hit_rate(input: i64, cache_read: i64, cache_creation: i64) -> f64 {
    let denom = input + cache_read + cache_creation;
    if denom <= 0 {
        return 0.0;
    }
    cache_read as f64 / denom as f64
}

fn build_cache_efficiency(ts: &[TimeSeriesPoint]) -> Vec<CacheEfficiencyPoint> {
    ts.iter()
        .map(|p| CacheEfficiencyPoint {
            date: p.date.clone(),
            cache_hit_rate: round_to(
                cache_hit_rate(p.input_tokens, p.cache_read_tokens, p.cache_write_tokens) * 100.0,
                100.0,
            ),
            cached_tokens: p.cache_read_tokens,
            total_input_tokens: p.input_tokens + p.cache_read_tokens + p.cache_write_tokens,
        })
        .collect()
}

/// The one builder that deliberately does **not** read `total_usage()`.
///
/// Every other aggregate wants "this session's tokens" and does not care which
/// model spent them; this one answers "which model did the work", so crediting
/// delegated tokens to the delegating parent would make it the single chart
/// that cannot answer the question it exists for — whether delegation is
/// actually routing work to cheaper models.
fn build_model_breakdown(sessions: &[&SessionSummary]) -> Vec<ModelStat> {
    let mut tokens_by_model: BTreeMap<String, i64> = BTreeMap::new();
    let mut total = 0i64;
    let mut add = |model: &str, input: i64, output: i64| {
        if model == SYNTHETIC_MODEL {
            return; // locally generated, never billed — not a real model
        }
        let t = input + output;
        *tokens_by_model.entry(display_model(model)).or_default() += t;
        total += t;
    };

    for s in sessions {
        // Main thread only — the delegated half is attributed per model below,
        // so reading total_usage() here would count it twice.
        add(&s.model, s.usage.input_tokens, s.usage.output_tokens);

        if !s.subagent_usage_by_model.is_empty() {
            for (model, u) in &s.subagent_usage_by_model {
                add(model, u.input_tokens, u.output_tokens);
            }
            continue;
        }
        // No per-model breakdown loaded, but the session did delegate: fall
        // back to the parent's model. Misattributing those tokens is the bug
        // being fixed, but dropping them would be worse — the chart's total
        // would silently stop matching every other total on the dashboard.
        add(
            &s.model,
            s.subagent_usage.input_tokens,
            s.subagent_usage.output_tokens,
        );
    }

    let mut out: Vec<ModelStat> = tokens_by_model
        .into_iter()
        .map(|(model, tokens)| ModelStat {
            model,
            tokens,
            percentage: share(tokens as f64, total as f64),
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.tokens));
    out
}

/// A model-id prefix and the provider that publishes it.
///
/// A prefix map rather than a catalog lookup: the provider is a property of the
/// identifier, and deriving it keeps a model with no published rate — the case
/// the unpriced bucket exists for — grouped correctly instead of falling into
/// "unknown provider" precisely when a reader is trying to find it.
const PROVIDER_PREFIXES: [(&str, &str); 4] = [
    ("claude-", "Anthropic"),
    ("glm-", "Z.ai"),
    ("qwen", "Alibaba"),
    ("k", "Moonshot"),
];

/// The provider behind a model id, or "Other". Declaration order is the match
/// order, so the single-letter Moonshot prefix is checked last and cannot
/// swallow another vendor's id.
fn provider_for(model: &str) -> String {
    for (prefix, provider) in PROVIDER_PREFIXES {
        if model.starts_with(prefix) {
            return provider.to_string();
        }
    }
    "Other".to_string()
}

/// Attributes spend to the model that spent it — the chart the dashboards were
/// missing, and the one the token breakdown beside it cannot stand in for: on
/// any corpus mixing a caching backend with a non-caching one, cache reads and
/// writes are most of the money and none of the tokens that chart plots.
fn build_cost_by_model(sessions: &[&SessionSummary]) -> Vec<ModelCostStat> {
    let mut costs: BTreeMap<String, SessionCost> = BTreeMap::new();
    let mut session_count: BTreeMap<String, i64> = BTreeMap::new();
    let mut total = 0.0;

    for s in sessions {
        for (model, c) in s.total_cost_by_model() {
            if model == SYNTHETIC_MODEL {
                continue; // never billed; see build_model_breakdown
            }
            costs.entry(model.clone()).or_default().add(&c);
            *session_count.entry(model).or_default() += 1;
            total += c.total_usd;
        }
    }

    let mut out: Vec<ModelCostStat> = costs
        .into_iter()
        .map(|(model, cost)| ModelCostStat {
            provider: provider_for(&model),
            percentage: share(cost.total_usd, total),
            sessions: session_count.get(&model).copied().unwrap_or_default(),
            model,
            cost,
        })
        .collect();
    out.sort_by(|a, b| desc(a.cost.total_usd, b.cost.total_usd));
    out
}

/// Splits the cost series by model, so "did switching models actually change
/// what I spend" is legible over a period. Buckets follow `build_cost_over_time`
/// exactly — same key, same walk — so the stacked chart and the plain one line
/// up bar for bar.
fn build_cost_over_time_by_model(
    sessions: &[&SessionSummary],
    p: &AnalyticsParams,
    granularity: Granularity,
    loc: Tz,
) -> Vec<StackedCostPoint> {
    let mut buckets: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    for s in sessions {
        let key = bucket_key(s.last_activity.instant(), granularity, loc);
        let bucket = buckets.entry(key).or_default();
        for (model, c) in s.total_cost_by_model() {
            if model == SYNTHETIC_MODEL {
                continue;
            }
            *bucket.entry(model).or_default() += c.total_usd;
        }
    }

    let mut out = Vec::new();
    walk_buckets(p.from, p.to, granularity, loc, |key, _| {
        out.push(StackedCostPoint {
            date: key.to_string(),
            cost_by_model: buckets.get(key).cloned().unwrap_or_default(),
        });
    });
    out
}

/// Aggregates the window by project, ordered by spend.
fn build_project_breakdown(sessions: &[&SessionSummary]) -> Vec<ProjectStat> {
    let mut stats: BTreeMap<String, ProjectStat> = BTreeMap::new();
    let mut total = 0.0;

    for s in sessions {
        let p = stats
            .entry(s.project_path.clone())
            .or_insert_with(|| ProjectStat {
                project: s.project_path.clone(),
                ..Default::default()
            });
        let u = s.total_usage();
        let c = s.total_cost();
        p.sessions += 1;
        p.tokens += u.input_tokens + u.output_tokens;
        p.total_tokens +=
            u.input_tokens + u.output_tokens + u.cache_read_tokens + u.cache_creation_tokens;
        p.cost.add(&c);
        if s.last_activity.instant() > p.last_activity.instant() {
            p.last_activity = s.last_activity;
        }
        total += c.total_usd;
    }

    let mut out: Vec<ProjectStat> = stats
        .into_values()
        .map(|mut p| {
            if total > 0.0 {
                p.percentage = share(p.cost.total_usd, total);
            }
            p
        })
        .collect();
    out.sort_by(|a, b| {
        desc(a.cost.total_usd, b.cost.total_usd)
            // A tie is common when several projects are entirely unpriced;
            // ordering by name then keeps the response stable across requests.
            .then_with(|| a.project.cmp(&b.project))
    });
    fold_project_tail(out)
}

/// Keeps the top projects by spend and sums the rest into one row, preserving
/// every figure's total.
///
/// Folding rather than truncating, per the no-silent-caps convention: a chart
/// that shows 20 of 500 bars without saying so reads as "these are all the
/// projects". The row carries the count it stands for so the UI can state it.
fn fold_project_tail(ranked: Vec<ProjectStat>) -> Vec<ProjectStat> {
    if ranked.len() <= TOP_PROJECTS_LISTED + 1 {
        // +1 because folding a single project into "Other (1 project)" is
        // strictly worse than naming it.
        return ranked;
    }
    let mut head = ranked;
    let tail = head.split_off(TOP_PROJECTS_LISTED);

    let mut other = ProjectStat {
        project: OTHER_PROJECTS_LABEL.to_string(),
        folded_projects: tail.len() as i64,
        ..Default::default()
    };
    for p in &tail {
        other.sessions += p.sessions;
        other.tokens += p.tokens;
        other.total_tokens += p.total_tokens;
        other.cost.add(&p.cost);
        other.percentage += p.percentage;
        if p.last_activity.instant() > other.last_activity.instant() {
            other.last_activity = p.last_activity;
        }
    }
    other.percentage = round_to(other.percentage, 10.0);
    head.push(other);
    head
}

/// The project×bucket strip: which projects were worked on when, for the
/// busiest projects in the window.
fn build_project_activity(
    sessions: &[&SessionSummary],
    ranked: &[ProjectStat],
    granularity: Granularity,
    loc: Tz,
) -> Vec<ProjectDayActivity> {
    let charted: std::collections::BTreeSet<&str> = ranked
        .iter()
        .take(TOP_PROJECTS_CHARTED)
        .map(|p| p.project.as_str())
        .collect();

    let mut cells: BTreeMap<(String, String), ProjectDayActivity> = BTreeMap::new();
    for s in sessions {
        if !charted.contains(s.project_path.as_str()) {
            continue;
        }
        let date = bucket_key(s.last_activity.instant(), granularity, loc);
        let cell = cells
            .entry((s.project_path.clone(), date.clone()))
            .or_insert_with(|| ProjectDayActivity {
                project: s.project_path.clone(),
                date,
                sessions: 0,
                cost_usd: 0.0,
            });
        cell.sessions += 1;
        cell.cost_usd += s.total_cost().total_usd;
    }
    // The BTreeMap is already keyed (project, date), which is exactly the order
    // Go sorts into.
    cells.into_values().collect()
}

/// Ranks the window's sessions three ways.
///
/// The rankings ship with the report rather than being sorted client-side so a
/// dashboard is self-contained and the ids can deep-link straight to a session.
fn build_top_sessions(sessions: &[&SessionSummary]) -> TopSessions {
    let rankings: Vec<SessionRanking> = sessions
        .iter()
        .map(|s| {
            let u = s.total_usage();
            SessionRanking {
                session_id: s.session_id.clone(),
                title: s.display_title.clone(),
                project: s.project_path.clone(),
                model: display_model(&s.model),
                cost_usd: s.total_cost().total_usd,
                // Active time, not the start/last span: ranking by span makes
                // "Longest" a leaderboard of which sessions were resumed after
                // the longest break.
                duration_ms: s.total_active_duration_ms(),
                tokens: u.input_tokens
                    + u.output_tokens
                    + u.cache_read_tokens
                    + u.cache_creation_tokens,
                subagent_count: s.subagent_count,
                last_activity: s.last_activity,
            }
        })
        .collect();

    TopSessions {
        by_cost: top_by(&rankings, |r| r.cost_usd),
        by_duration: top_by(&rankings, |r| r.duration_ms as f64),
        by_tokens: top_by(&rankings, |r| r.tokens as f64),
    }
}

/// The highest-scoring rankings, dropping zero scores: a leaderboard padded
/// with $0.00 rows to reach ten states nothing.
fn top_by(
    rankings: &[SessionRanking],
    score: impl Fn(&SessionRanking) -> f64,
) -> Vec<SessionRanking> {
    let mut out: Vec<SessionRanking> = rankings
        .iter()
        .filter(|r| score(r) > 0.0)
        .cloned()
        .collect();
    out.sort_by(|a, b| desc(score(a), score(b)));
    out.truncate(TOP_SESSIONS_PER_BOARD);
    out
}

fn build_sessions_per_model(sessions: &[&SessionSummary]) -> Vec<ModelSessionStat> {
    let mut count: BTreeMap<String, i64> = BTreeMap::new();
    for s in sessions {
        if s.model == SYNTHETIC_MODEL {
            continue; // see build_model_breakdown
        }
        *count.entry(display_model(&s.model)).or_default() += 1;
    }
    let mut out: Vec<ModelSessionStat> = count
        .into_iter()
        .map(|(model, sessions)| ModelSessionStat { model, sessions })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.sessions));
    out
}

fn build_most_active_days(sessions: &[&SessionSummary], loc: Tz) -> Vec<DayActivity> {
    let mut by_day: BTreeMap<String, DayActivity> = BTreeMap::new();
    for s in sessions {
        // Via bucket_key so there is exactly one place that formats a day.
        let key = bucket_key(s.last_activity.instant(), Granularity::Daily, loc);
        let d = by_day.entry(key.clone()).or_insert_with(|| DayActivity {
            date: key,
            sessions: 0,
            tokens: 0,
        });
        let u = s.total_usage();
        d.sessions += 1;
        d.tokens += u.input_tokens + u.output_tokens;
    }
    let mut out: Vec<DayActivity> = by_day.into_values().collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.tokens));
    out.truncate(30);
    out
}

fn build_heatmap(sessions: &[&SessionSummary], loc: Tz) -> Vec<HeatmapCell> {
    use chrono::{Datelike, Timelike};

    let mut cells: BTreeMap<(i64, i64), HeatmapCell> = BTreeMap::new();
    for s in sessions {
        let u = s.total_usage();
        let tokens = (u.input_tokens + u.output_tokens) as f64;
        walk_session_hours(
            s.start_time.instant(),
            s.last_activity.instant(),
            loc,
            |at, share| {
                let local = at.with_timezone(&loc);
                let key = (
                    i64::from(local.weekday().num_days_from_sunday()),
                    i64::from(local.hour()),
                );
                let cell = cells.entry(key).or_insert_with(|| HeatmapCell {
                    day_of_week: key.0,
                    hour: key.1,
                    sessions: 0,
                    tokens: 0,
                });
                // Sessions counts presence — a session active across three
                // hours is one session in each, so column totals exceed the
                // session count by design. Tokens are shared out, so they still
                // sum to the corpus.
                cell.sessions += 1;
                cell.tokens += (tokens * share).round() as i64;
            },
        );
    }
    // Keyed (day_of_week, hour), which is the order Go sorts into.
    cells.into_values().collect()
}

fn build_hourly_activity(sessions: &[&SessionSummary], loc: Tz) -> Vec<HourlyActivity> {
    use chrono::Timelike;

    let mut hours: Vec<HourlyActivity> = (0..24)
        .map(|hour| HourlyActivity {
            hour,
            sessions: 0,
            tokens: 0,
        })
        .collect();

    for s in sessions {
        let u = s.total_usage();
        let tokens = (u.input_tokens + u.output_tokens) as f64;
        // Same span-based attribution as the heatmap — see walk_session_hours.
        walk_session_hours(
            s.start_time.instant(),
            s.last_activity.instant(),
            loc,
            |at, share| {
                let h = at.with_timezone(&loc).hour() as usize;
                hours[h].sessions += 1;
                hours[h].tokens += (tokens * share).round() as i64;
            },
        );
    }
    hours
}

fn build_cost_over_time(
    sessions: &[&SessionSummary],
    p: &AnalyticsParams,
    granularity: Granularity,
    loc: Tz,
) -> Vec<CostPoint> {
    let mut buckets: BTreeMap<String, CostPoint> = BTreeMap::new();
    for s in sessions {
        let key = bucket_key(s.last_activity.instant(), granularity, loc);
        let b = buckets.entry(key.clone()).or_insert_with(|| CostPoint {
            date: key,
            estimated_cost_usd: 0.0,
        });
        // Stored cost, for the same reason build_summary reads it — the two
        // must add up to the same money.
        b.estimated_cost_usd += s.total_cost().total_usd;
    }

    let mut out = Vec::new();
    walk_buckets(p.from, p.to, granularity, loc, |key, _| {
        out.push(buckets.get(key).cloned().unwrap_or_else(|| CostPoint {
            date: key.to_string(),
            estimated_cost_usd: 0.0,
        }));
    });
    out
}

// ─── Arithmetic shared with Go ────────────────────────────────────────────────

/// `math.Round(x * factor) / factor`. Both languages round half away from zero,
/// so this is the same value bit for bit.
pub fn round_to(x: f64, factor: f64) -> f64 {
    (x * factor).round() / factor
}

/// A percentage to one decimal place, guarding the division rather than leaving
/// the encoder to notice: Go fails an encode of NaN outright, after `writeJSON`
/// has already committed a 200.
fn share(part: f64, total: f64) -> f64 {
    if total <= 0.0 {
        return 0.0;
    }
    round_to(part / total * 100.0, 10.0)
}

/// Descending order for a float sort key, with ties left to the caller.
fn desc(a: f64, b: f64) -> std::cmp::Ordering {
    b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provider_prefixes_match_longest_first_by_declaration_order() {
        assert_eq!(provider_for("claude-opus-4-8"), "Anthropic");
        assert_eq!(provider_for("glm-5.2"), "Z.ai");
        assert_eq!(provider_for("qwen3-max"), "Alibaba");
        assert_eq!(provider_for("k3"), "Moonshot");
        // The single-letter Moonshot prefix must not swallow another vendor.
        assert_eq!(provider_for("kimi-k2"), "Moonshot");
        assert_eq!(provider_for("gpt-4"), "Other");
        assert_eq!(provider_for("unknown"), "Other");
    }

    #[test]
    fn cache_hit_rate_counts_every_input_side_token() {
        // A model with no prompt caching scores 0 rather than being excused.
        assert_eq!(cache_hit_rate(1000, 0, 0), 0.0);
        // 3 of every 4 input-side tokens served from cache.
        assert_eq!(cache_hit_rate(100, 300, 0), 0.75);
        assert_eq!(cache_hit_rate(0, 300, 100), 0.75);
        // An empty bucket is 0, not a division by zero.
        assert_eq!(cache_hit_rate(0, 0, 0), 0.0);
    }

    #[test]
    fn a_share_of_nothing_is_zero_rather_than_nan() {
        assert_eq!(share(0.0, 0.0), 0.0);
        assert_eq!(share(1.0, 4.0), 25.0);
        assert_eq!(share(1.0, 3.0), 33.3);
    }

    #[test]
    fn most_frequent_takes_the_highest_count_and_empty_means_empty() {
        let mut counts = BTreeMap::new();
        assert_eq!(most_frequent(&counts), "");
        counts.insert("a".to_string(), 2i64);
        counts.insert("b".to_string(), 5i64);
        assert_eq!(most_frequent(&counts), "b");
        // A tie takes the lexically first key; Go takes whichever its map
        // iteration reached first, which is why this is deterministic and Go
        // is not.
        counts.insert("a".to_string(), 5i64);
        assert_eq!(most_frequent(&counts), "a");
    }
}
