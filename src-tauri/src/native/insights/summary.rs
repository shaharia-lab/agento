//! `GET /api/claude-sessions/insights/summary`, ported from Go.
//!
//! Go sources this mirrors:
//!
//! - `internal/api/claude_session_insights.go` — the handler, the response type
//!   and `sortedToolCounts`
//! - `internal/api/insight_store_adapter.go`   — `GetSummary`, `mergeBreakdowns`
//! - `internal/storage/sqlite_session_insights_store.go` — `GetAggregateSummary`
//!
//! It resolves its window through `AnalyticsParams` and selects through
//! [`crate::native::analytics::report::filter_sessions`] — the Go counterpart of
//! which is exported precisely so more than one endpoint can share it. The
//! summary used to answer "which sessions does this window contain" with its own
//! SQL predicate over `start_time`, and the same range produced a different
//! session count and a different total cost on two dashboards showing the same
//! window.
//!
//! ## The ID set is the whole filter
//!
//! Windowing happens in Rust, over the corpus; the SQL sees only a set of
//! session IDs and never a date. That is deliberate on the Go side too, and not
//! merely for consistency: the DATETIME columns hold Go's `time.Time.String()`
//! rendering (`2024-03-15 12:00:00 +0000 UTC`), so comparing one against an
//! RFC 3339 bound compares `' '` with `'T'` at index 10 and misplaces every row
//! whose date equals a boundary's.
//!
//! **An empty set means empty, not everything.** The set travels as a single
//! JSON parameter expanded by `json_each` rather than one placeholder per ID,
//! because a window can legitimately hold thousands of sessions and a
//! placeholder each would hit SQLite's variable limit on exactly the corpora
//! this exists for.
//!
//! ## Every list is `[]`, never `null`
//!
//! `desktop/CLAUDE.md` and the porting issue both said this endpoint sends
//! `null` for its empty `top_*` lists. It does not, on either path that reaches
//! the zero case: `sortedToolCounts` builds with `make([]toolCount, 0, len)`,
//! which is non-nil when empty, and the zero branch returns each list as an
//! explicit `[]toolCount{}`. There is no code path that yields a nil slice.
//! (The *analytics* report does have one — its zero-valued summary sends
//! `"unknown_pricing_models":null` — which is where the belief came from.)

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::Serialize;

use crate::native::analytics::params::{query_value, AnalyticsParams};
use crate::native::analytics::report::filter_sessions;
use crate::native::gojson;
use crate::native::sessions::corpus;
use crate::native::settings::DataSettings;

/// How many entries each breakdown panel shows.
const TOP_BREAKDOWN_ENTRIES: usize = 10;

/// Aggregated statistics across the window's sessions. Mirrors
/// `api.insightsSummary`; field order is that struct's declaration order.
#[derive(Debug, Default, Serialize)]
pub struct InsightsSummary {
    pub total_sessions: i64,
    pub avg_autonomy_score: f64,
    pub avg_turn_count: f64,
    pub avg_tool_calls_total: f64,
    pub avg_cost_estimate_usd: f64,
    pub total_cost_estimate_usd: f64,
    pub avg_cache_hit_rate: f64,
    pub sessions_with_errors: i64,
    pub total_tool_errors: i64,
    pub avg_total_duration_ms: f64,
    /// The mean of per-session *active* durations — every inter-event gap
    /// capped at the idle threshold — which is what the dashboard labels "Avg
    /// Duration". `avg_total_duration_ms` is the raw span mean, kept because
    /// "first seen → last touched" is an honest answer to a different question.
    pub avg_active_duration_ms: f64,
    pub top_tools: Vec<ToolCount>,
    /// With `unattributed_calls`, gives the breakdowns a denominator: without
    /// them a "top skills" panel silently omits every built-in call.
    pub total_tool_calls: i64,
    pub unattributed_calls: i64,
    pub top_skills: Vec<ToolCount>,
    pub top_plugins: Vec<ToolCount>,
    pub top_mcp_servers: Vec<ToolCount>,
    /// The drill-down under `top_mcp_servers`, not a peer dimension.
    pub top_mcp_tools: Vec<ToolCount>,
    pub top_efforts: Vec<ToolCount>,
    pub top_agents: Vec<ToolCount>,
}

/// A name and its aggregate call count. Every ranked entry keys its label
/// `tool`, whatever the list is of.
#[derive(Debug, Serialize)]
pub struct ToolCount {
    pub tool: String,
    pub count: i64,
}

/// Build the summary for one request's query string.
pub fn summary(
    conn: &Connection,
    settings: &DataSettings,
    query: &str,
) -> Result<InsightsSummary, String> {
    let p = AnalyticsParams::parse(query)?;
    let sessions = corpus::load(conn, settings)?;

    let mut ids: Vec<String> = filter_sessions(&sessions, &p)
        .iter()
        .map(|s| s.session_id.clone())
        .collect();

    // An explicit `ids` list narrows the window rather than replacing it, so a
    // caller cannot accidentally widen the range by naming a session outside it.
    let explicit = parse_session_ids(&query_value(query, "ids"));
    if !explicit.is_empty() {
        ids.retain(|id| explicit.iter().any(|wanted| wanted == id));
    }

    Ok(build(aggregate(conn, &ids)?))
}

/// Split the comma-separated `ids` parameter, dropping blanks.
fn parse_session_ids(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

/// The SQL-computed scalars plus the merged breakdowns.
#[derive(Default)]
struct Aggregate {
    total_sessions: i64,
    avg_autonomy_score: f64,
    avg_turn_count: f64,
    avg_tool_calls_total: f64,
    total_cost_estimate_usd: f64,
    avg_cache_hit_rate: f64,
    avg_total_duration_ms: f64,
    avg_active_duration_ms: f64,
    sessions_with_errors: i64,
    total_tool_calls: i64,
    unattributed_calls: i64,
    total_tool_errors: i64,
    /// One merged map per dimension, in the order the columns are selected:
    /// tool, skill, plugin, MCP server, MCP tool, effort, agent.
    breakdowns: [BTreeMap<String, i64>; BREAKDOWN_COLUMNS],
}

const BREAKDOWN_COLUMNS: usize = 7;

/// Scalars in SQL — the aggregation never loads a row into memory.
const AGGREGATE_SQL: &str = "SELECT
	COUNT(*),
	COALESCE(AVG(autonomy_score), 0),
	COALESCE(AVG(turn_count), 0),
	COALESCE(AVG(tool_calls_total), 0),
	COALESCE(SUM(cost_estimate_usd), 0),
	COALESCE(AVG(cache_hit_rate), 0),
	COALESCE(AVG(total_duration_ms), 0),
	COALESCE(AVG(active_duration_ms), 0),
	COALESCE(SUM(has_errors), 0),
	COALESCE(SUM(tool_calls_total), 0),
	COALESCE(SUM(unattributed_calls), 0),
	COALESCE(SUM(tool_error_count), 0)
FROM session_insights";

/// One query for every breakdown column rather than one per column: the
/// row-scan cost then grows with the corpus only, not with how many dimensions
/// the feature has accumulated.
const BREAKDOWN_SQL: &str = "SELECT tool_breakdown, skill_breakdown, plugin_breakdown,
       mcp_server_breakdown, mcp_tool_breakdown, effort_breakdown, agent_breakdown
FROM session_insights";

/// Restricts a query to the given session IDs.
const ID_FILTER: &str = " WHERE session_id IN (SELECT value FROM json_each(?))";

fn aggregate(conn: &Connection, ids: &[String]) -> Result<Aggregate, String> {
    if ids.is_empty() {
        return Ok(Aggregate::default());
    }

    // Marshalled through the Go encoder so the bound parameter is the same text
    // Go binds.
    let bound = String::from_utf8(
        gojson::to_vec_marshal(&ids).map_err(|e| format!("marshaling session id filter: {e}"))?,
    )
    .map_err(|e| format!("session id filter is not utf-8: {e}"))?;

    let mut agg = conn
        .query_row(&format!("{AGGREGATE_SQL}{ID_FILTER}"), [&bound], |row| {
            Ok(Aggregate {
                total_sessions: row.get(0)?,
                avg_autonomy_score: row.get(1)?,
                avg_turn_count: row.get(2)?,
                avg_tool_calls_total: row.get(3)?,
                total_cost_estimate_usd: row.get(4)?,
                avg_cache_hit_rate: row.get(5)?,
                avg_total_duration_ms: row.get(6)?,
                avg_active_duration_ms: row.get(7)?,
                sessions_with_errors: row.get(8)?,
                total_tool_calls: row.get(9)?,
                unattributed_calls: row.get(10)?,
                total_tool_errors: row.get(11)?,
                breakdowns: Default::default(),
            })
        })
        .map_err(|e| format!("claudesessions: insight aggregate: {e}"))?;

    // No matching insight rows: Go returns before touching the breakdowns.
    if agg.total_sessions == 0 {
        return Ok(agg);
    }

    let mut stmt = conn
        .prepare(&format!("{BREAKDOWN_SQL}{ID_FILTER}"))
        .map_err(|e| format!("claudesessions: preparing insight breakdowns: {e}"))?;
    let rows = stmt
        .query_map([&bound], |row| {
            let mut blobs: [String; BREAKDOWN_COLUMNS] = Default::default();
            for (i, blob) in blobs.iter_mut().enumerate() {
                *blob = row.get(i)?;
            }
            Ok(blobs)
        })
        .map_err(|e| format!("claudesessions: querying insight breakdowns: {e}"))?;

    for row in rows {
        let blobs = row.map_err(|e| format!("claudesessions: reading insight breakdowns: {e}"))?;
        // Each column is merged independently — a row with tools but no skills
        // contributes to the tool totals alone. Skipping the whole row when any
        // column is empty would silently drop real data.
        for (dimension, blob) in agg.breakdowns.iter_mut().zip(blobs.iter()) {
            merge_breakdown(dimension, blob);
        }
    }
    Ok(agg)
}

/// Sum one session's breakdown blob into the running totals.
///
/// An empty string and `{}` are both "nothing attributed" — the columns default
/// to `'{}'`. A blob that fails to parse is skipped rather than failing the
/// summary: one bad row should not blank the whole insights page.
fn merge_breakdown(totals: &mut BTreeMap<String, i64>, blob: &str) {
    if blob.is_empty() || blob == "{}" {
        return;
    }
    let Ok(parsed) = serde_json::from_str::<BTreeMap<String, i64>>(blob) else {
        return;
    };
    for (key, count) in parsed {
        *totals.entry(key).or_default() += count;
    }
}

fn build(agg: Aggregate) -> InsightsSummary {
    if agg.total_sessions == 0 {
        // Go returns a zero-valued struct with every list explicitly empty, so
        // the response carries `[]` rather than `null` throughout.
        return InsightsSummary {
            top_tools: Vec::new(),
            top_skills: Vec::new(),
            top_plugins: Vec::new(),
            top_mcp_servers: Vec::new(),
            top_mcp_tools: Vec::new(),
            top_efforts: Vec::new(),
            top_agents: Vec::new(),
            ..Default::default()
        };
    }

    let n = agg.total_sessions as f64;
    let mut ranked = agg.breakdowns.into_iter().map(sorted_tool_counts);
    let mut next = || ranked.next().unwrap_or_default();

    InsightsSummary {
        total_sessions: agg.total_sessions,
        avg_autonomy_score: agg.avg_autonomy_score,
        avg_turn_count: agg.avg_turn_count,
        avg_tool_calls_total: agg.avg_tool_calls_total,
        avg_cost_estimate_usd: agg.total_cost_estimate_usd / n,
        total_cost_estimate_usd: agg.total_cost_estimate_usd,
        avg_cache_hit_rate: agg.avg_cache_hit_rate,
        sessions_with_errors: agg.sessions_with_errors,
        total_tool_errors: agg.total_tool_errors,
        avg_total_duration_ms: agg.avg_total_duration_ms,
        avg_active_duration_ms: agg.avg_active_duration_ms,
        top_tools: next(),
        total_tool_calls: agg.total_tool_calls,
        unattributed_calls: agg.unattributed_calls,
        top_skills: next(),
        top_plugins: next(),
        top_mcp_servers: next(),
        top_mcp_tools: next(),
        top_efforts: next(),
        top_agents: next(),
    }
}

/// The top entries by count, descending.
///
/// Go collects into a map — random iteration order — and then insertion-sorts,
/// so **two entries tying on count come out in either order**, and at the
/// boundary a tie changes which entry is in the top ten at all rather than just
/// where it sits. A `BTreeMap` plus a stable sort makes a tie break on the
/// name; see "Go itself is not always byte-stable" in `desktop/CLAUDE.md`.
fn sorted_tool_counts(totals: BTreeMap<String, i64>) -> Vec<ToolCount> {
    let mut counts: Vec<ToolCount> = totals
        .into_iter()
        .map(|(tool, count)| ToolCount { tool, count })
        .collect();
    counts.sort_by_key(|c| std::cmp::Reverse(c.count));
    counts.truncate(TOP_BREAKDOWN_ENTRIES);
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ids_parameter_drops_blanks_and_trims() {
        assert_eq!(parse_session_ids(""), Vec::<String>::new());
        assert_eq!(parse_session_ids(" , ,"), Vec::<String>::new());
        assert_eq!(parse_session_ids("a, b ,,c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn breakdown_blobs_merge_and_a_broken_one_is_skipped_not_fatal() {
        let mut totals = BTreeMap::new();
        merge_breakdown(&mut totals, r#"{"Bash":3,"Read":1}"#);
        merge_breakdown(&mut totals, r#"{"Bash":4}"#);
        // The two "nothing attributed" spellings, and blobs that are not a
        // string→int map at all.
        merge_breakdown(&mut totals, "{}");
        merge_breakdown(&mut totals, "");
        merge_breakdown(&mut totals, "not json");
        merge_breakdown(&mut totals, r#"{"Bash":"lots"}"#);

        assert_eq!(totals.get("Bash"), Some(&7));
        assert_eq!(totals.get("Read"), Some(&1));
        assert_eq!(totals.len(), 2);
    }

    #[test]
    fn ranked_entries_are_capped_at_ten_and_ties_break_on_name() {
        let totals: BTreeMap<String, i64> = (0..12)
            .map(|i| (format!("tool-{i:02}"), 100 - i))
            .chain([("zzz".to_string(), 100)])
            .collect();

        let ranked = sorted_tool_counts(totals);
        assert_eq!(ranked.len(), TOP_BREAKDOWN_ENTRIES);
        // tool-00 and zzz both score 100; the name decides, deterministically.
        assert_eq!(ranked[0].tool, "tool-00");
        assert_eq!(ranked[1].tool, "zzz");
        assert_eq!(ranked[2].tool, "tool-01");
    }

    /// The trap the issue and `CLAUDE.md` both got backwards.
    #[test]
    fn an_empty_aggregate_serializes_every_list_as_an_array_not_null() {
        let encoded =
            String::from_utf8(gojson::to_vec(&build(Aggregate::default())).expect("encode"))
                .expect("utf-8");
        assert!(!encoded.contains("null"), "{encoded}");
        assert!(encoded.contains(r#""top_tools":[]"#), "{encoded}");
        assert!(encoded.contains(r#""top_agents":[]"#), "{encoded}");
        assert!(encoded.contains(r#""total_sessions":0"#), "{encoded}");
    }
}
