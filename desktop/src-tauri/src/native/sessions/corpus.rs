//! The whole visible corpus, in one read.
//!
//! Mirrors `Cache.loadOrEmpty` → `querySessionSummaries` →
//! `attachSubagentUsageByModel` → `VisibleSessions`.
//!
//! The paged list exists so the browser never holds more than a page. Analytics
//! is the opposite: every figure it reports is an aggregate over the window, so
//! it loads the corpus and walks it. That is what Go does too — the memoization
//! in `analytics_cache.go` sits *around* this load rather than replacing it.
//!
//! Two details are load-bearing:
//!
//! - **Row order is `last_activity DESC`**, exactly as `sessionSummarySelect`
//!   orders it. It is not cosmetic: several analytics builders sort with Go's
//!   `sort.Slice`, which is unstable, so the input order is what decides where
//!   equal-scoring rows land. Reordering here would silently reorder a
//!   leaderboard.
//! - **The per-model sub-agent breakdown is a second grouped read.** The
//!   summary select's roll-up groups by parent session alone — deliberately, so
//!   a session with several sub-agents is not multiplied out by the join — and
//!   that collapses the model dimension before analytics can see it.
//!
//! Linked pull requests are deliberately *not* attached: `attachPRs` runs on
//! the Go side because `Cache.List` also feeds the detail page, but no
//! analytics figure reads `prs`, and it is a third full-table read.

use std::collections::BTreeMap;

use rusqlite::Connection;

use super::summary::{
    display_model, scan, SessionCost, SessionSummary, TokenUsage, SUMMARY_COLUMNS, SUMMARY_SOURCE,
};
use crate::native::settings::DataSettings;

/// Every session the user's settings leave visible, newest activity first.
pub fn load(conn: &Connection, settings: &DataSettings) -> Result<Vec<SessionSummary>, String> {
    let sql = format!("{SUMMARY_COLUMNS}{SUMMARY_SOURCE}\n\tORDER BY c.last_activity DESC");

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("claudesessions: preparing corpus load: {e}"))?;
    let rows = stmt
        .query_map([], scan)
        .map_err(|e| format!("claudesessions: querying corpus: {e}"))?;

    let mut sessions = Vec::new();
    for row in rows {
        sessions.push(row.map_err(|e| format!("claudesessions: scanning corpus: {e}"))?);
    }

    attach_subagent_usage_by_model(conn, &mut sessions);

    // `VisibleSessions`: hidden projects and de-indexed config dirs are
    // filtered rather than left unscanned, so unhiding costs no re-read.
    sessions.retain(|s| {
        !settings.hidden_projects.contains(&s.project_path)
            && settings.is_indexed_config_dir(&s.config_dir)
    });
    Ok(sessions)
}

/// Fill in each session's delegated usage and cost broken down by the
/// sub-agent's own model.
///
/// Best-effort exactly as Go's is: a failure logs and leaves the breakdowns
/// empty, which makes `total_usage_by_model` fall back to the parent's model
/// rather than dropping the tokens.
fn attach_subagent_usage_by_model(conn: &Connection, sessions: &mut [SessionSummary]) {
    if sessions.is_empty() {
        return;
    }

    type ByModel = BTreeMap<String, (BTreeMap<String, TokenUsage>, BTreeMap<String, SessionCost>)>;
    let mut by_session: ByModel = BTreeMap::new();

    let read = (|| -> rusqlite::Result<()> {
        let mut stmt = conn.prepare(
            "SELECT parent_session_id, model,
                    SUM(input_tokens), SUM(output_tokens),
                    SUM(cache_creation_tokens), SUM(cache_read_tokens),
                    SUM(cache_creation_5m_tokens), SUM(cache_creation_1h_tokens),
                    SUM(input_cost_usd), SUM(output_cost_usd),
                    SUM(cache_read_cost_usd), SUM(cache_write_cost_usd), SUM(total_cost_usd)
             FROM claude_subagent_cache
             GROUP BY parent_session_id, model",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                display_model(&row.get::<_, String>(1)?),
                TokenUsage {
                    input_tokens: row.get(2)?,
                    output_tokens: row.get(3)?,
                    cache_creation_tokens: row.get(4)?,
                    cache_creation_5m_tokens: row.get(6)?,
                    cache_creation_1h_tokens: row.get(7)?,
                    cache_read_tokens: row.get(5)?,
                },
                SessionCost {
                    input_usd: row.get(8)?,
                    output_usd: row.get(9)?,
                    cache_read_usd: row.get(10)?,
                    cache_write_usd: row.get(11)?,
                    total_usd: row.get(12)?,
                },
            ))
        })?;
        for row in rows {
            let (session_id, model, usage, cost) = row?;
            let entry = by_session.entry(session_id).or_default();
            entry.0.insert(model.clone(), usage);
            entry.1.insert(model, cost);
        }
        Ok(())
    })();
    if let Err(e) = read {
        log::warn!("claude sessions: failed to load sub-agent usage by model: {e}");
        return;
    }

    // `get`, never `remove`. `claude_session_cache` is keyed on
    // `(session_id, project_path)` and `claude_subagent_cache` on the parent
    // session id alone, so one id can appear under two project paths and
    // draining the entry would leave the second row's breakdown empty — which
    // falls back to the parent's model rather than failing. Go re-reads the map
    // per row.
    for s in sessions.iter_mut() {
        if let Some((usage, cost)) = by_session.get(&s.session_id) {
            s.subagent_usage_by_model = usage.clone();
            s.subagent_cost_by_model = cost.clone();
        }
    }
}
