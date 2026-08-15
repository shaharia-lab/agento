//! `GET /api/claude-analytics`, ported from `internal/claudesessions`.
//!
//! The dashboard's whole payload in one response: KPIs, four bucketed series, a
//! model breakdown by tokens and another by cost, per-project totals and a
//! project×bucket strip, three leaderboards, a heatmap, and the Insights
//! cards. Go sources, which are the spec if the two ever disagree:
//!
//! - `internal/claudesessions/analytics.go`      — every aggregate
//! - `internal/claudesessions/insight_cards.go`  — the cards
//! - `internal/claudesessions/cache_hit_rate.go` — the one definition
//! - `internal/api/claude_analytics.go`          — `parseAnalyticsParams`
//!
//! ## Not memoized, deliberately
//!
//! Go memoizes the report (`analytics_cache.go`) in a 20-entry LRU keyed by the
//! window plus `last_scanned_at`, `pricing_rev`, `idle_threshold_ms` and the
//! hidden-project set — because rebuilding it meant a full corpus load and a
//! dozen walks over it, and a dashboard fires two or three per open. Measured
//! on the reference instance that rebuild is ~50 ms in Go, and this port loads
//! the same rows and walks them the same number of times. A cache is a second
//! thing to invalidate correctly; nothing about the *response* depends on one,
//! so there is none here until a measurement says otherwise.
//!
//! ## Reading is still what keeps the corpus fresh
//!
//! `Cache.Analytics` calls `ensureFresh` before it answers, so opening a
//! dashboard after a rate edit starts the re-cost rather than waiting for
//! someone to open the sessions list. Serving this route from Rust removes that
//! trigger, so the handler fires the same probe the sessions list does — see
//! `sessions::PROBE_PATH`, which delegates the decision to the Go code that
//! owns the rules rather than reimplementing four pieces of metadata here.

pub mod buckets;
pub mod cards;
pub mod params;
pub mod report;

#[cfg(test)]
mod tests_golden;

use axum::http::Method;
use rusqlite::Connection;

use self::params::AnalyticsParams;
use self::report::AnalyticsReport;
use crate::native::pricing::Resolver;
use crate::native::sessions::corpus;
use crate::native::settings::DataSettings;
use crate::native::{db, gojson, settings, Answer, Ctx, Endpoint, Request};

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: Endpoint = Endpoint {
    name: "claude analytics",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && path == "/api/claude-analytics"
}

fn serve(ctx: &Ctx, req: &Request) -> Result<Answer, String> {
    let conn = db::open_read_only(&ctx.db_path)?;
    let data_settings = settings::load(&conn);
    let report = analytics(&conn, &data_settings, req.query)?;
    // Cache.Analytics runs ensureFresh before it answers, so a dashboard
    // opened after a rate edit starts the re-cost.
    super::scan::ensure_scan(ctx.db_path.clone());
    Ok(Answer::json(
        gojson::to_vec(&report).map_err(|e| format!("encoding claude analytics: {e}"))?,
    ))
}

/// Build the report for one request's query string.
pub fn analytics(
    conn: &Connection,
    settings: &DataSettings,
    query: &str,
) -> Result<AnalyticsReport, String> {
    let p = AnalyticsParams::parse(query)?;
    let sessions = corpus::load(conn, settings)?;
    // Only the cache-savings card needs rates, but a catalog this cannot read
    // is an error rather than a dropped card: Go has a resolver wired in every
    // real process, so a silently missing card would be a divergence.
    let resolver = Resolver::load(conn)?;
    Ok(report::aggregate(&sessions, &p, Some(&resolver)))
}
