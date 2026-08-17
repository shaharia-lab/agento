//! `GET /api/claude-sessions` and `GET /api/claude-sessions/facets`.
//!
//! Mirrors `listSessionPage` and `sessionFacets` in
//! `internal/claudesessions/session_page.go`.
//!
//! The list used to ship every session to the browser and filter, sort, group
//! and render all of them there. That worked at 800 sessions and stopped
//! working well before 5,000. Everything here exists so the browser never holds
//! more than one page — which means the filtering, sorting and paging must be
//! SQL, and must produce exactly what the Go implementation produces.

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::Serialize;

use super::query::{
    add_config_dir_scope, add_links, build_filter, cursor_value, Cursor, Filter, Links,
    SessionQuery, Value, SQL_COST_USD, SQL_TOKENS,
};
use super::summary::{display_model, scan, SessionCost, SessionPR, SessionSummary, TokenUsage};
use super::summary::{SUMMARY_COLUMNS, SUMMARY_SOURCE};
use crate::native::gotime::GoTime;
use crate::native::settings::DataSettings;

/// One page of the sessions list.
#[derive(Debug, Serialize)]
pub struct SessionPage {
    pub items: Vec<SessionSummary>,
    /// Continues the list. Empty means this was the last page.
    pub next_cursor: String,
    /// `next_cursor != ""`, stated explicitly so a client need not know that.
    pub has_more: bool,
}

/// What the toolbar needs and a single page cannot answer.
#[derive(Debug, Default, Serialize)]
pub struct SessionFacets {
    pub total: i64,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
    /// The 90th percentile of input+output tokens across the filtered set — the
    /// reference length for the list's token bars. The 90th rather than the
    /// maximum, because one 75M-token session against a ~100K median would push
    /// every other bar below a pixel.
    pub token_p90: i64,
    /// Dropdown options, derived from every visible session rather than from
    /// the filtered set: a dropdown that removes the option you just picked
    /// cannot be un-picked.
    pub models: Vec<String>,
    pub permission_modes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub config_dirs: Vec<String>,
    pub has_favorites: bool,
    pub has_prs: bool,
}

/// Read one page of sessions matching `q`.
pub fn list_page(
    conn: &Connection,
    settings: &DataSettings,
    q: &SessionQuery,
) -> Result<SessionPage, String> {
    let (expr, is_time) = q.sort.expr();
    let mut filter = build_filter(q, &settings.hidden_projects, &settings.indexed_config_dirs)?;

    if let Some(cur) = Cursor::decode(&q.cursor, q.sort)? {
        let bound = cur.bind(is_time)?;
        // Strictly after the cursor in the same total order the ORDER BY
        // imposes. The tiebreak on session_id is what makes that order total:
        // without it two sessions sharing a cost or a timestamp would page
        // against each other, one repeating and one disappearing.
        filter.add(
            format!("({expr} < ? OR ({expr} = ? AND c.session_id < ?))"),
            vec![bound.clone(), bound, Value::Text(cur.id)],
        );
    }

    let limit = q.page_size();
    // One extra row, so "is there a next page" is answered without a second
    // COUNT over the same predicate.
    let sql = format!(
        "{SUMMARY_COLUMNS}{SUMMARY_SOURCE}{}\nORDER BY {expr} DESC, c.session_id DESC\nLIMIT {}",
        filter.where_clause(),
        limit + 1
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("claudesessions: preparing session page: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(filter.args.iter()), |row| {
            scan(row)
        })
        .map_err(|e| format!("claudesessions: querying session page: {e}"))?;

    let mut items = Vec::with_capacity(limit as usize);
    for row in rows {
        items.push(row.map_err(|e| format!("claudesessions: scanning session page: {e}"))?);
    }

    let mut page = SessionPage {
        items: Vec::new(),
        next_cursor: String::new(),
        has_more: false,
    };
    if items.len() as i64 > limit {
        items.truncate(limit as usize);
        if let Some(last) = items.last() {
            page.next_cursor = Cursor {
                sort: q.sort.as_str().to_string(),
                value: cursor_value(last, q.sort),
                id: last.session_id.clone(),
            }
            .encode();
            page.has_more = !page.next_cursor.is_empty();
        }
    }

    attach_prs(conn, &mut items);
    attach_subagent_usage_by_model(conn, &mut items);
    page.items = items;
    Ok(page)
}

/// The filtered totals and the filter options for `q`.
pub fn facets(
    conn: &Connection,
    settings: &DataSettings,
    q: &SessionQuery,
) -> Result<SessionFacets, String> {
    let filter = build_filter(q, &settings.hidden_projects, &settings.indexed_config_dirs)?;
    let where_clause = filter.where_clause();

    let totals_sql = format!(
        "SELECT COUNT(*), COALESCE(SUM({SQL_TOKENS}), 0), COALESCE(SUM({SQL_COST_USD}), 0)\
         {SUMMARY_SOURCE}{where_clause}"
    );
    let mut f: SessionFacets = conn
        .query_row(
            &totals_sql,
            rusqlite::params_from_iter(filter.args.iter()),
            |row| {
                Ok(SessionFacets {
                    total: row.get(0)?,
                    total_tokens: row.get(1)?,
                    total_cost_usd: row.get(2)?,
                    ..SessionFacets::default()
                })
            },
        )
        .map_err(|e| format!("claudesessions: session totals: {e}"))?;

    if f.total > 0 {
        // The same index the TypeScript implementation takes: floor(0.9*(n-1))
        // into the ascending series, so both languages pick the same row rather
        // than two neighbouring ones.
        let offset = (0.9 * (f.total - 1) as f64) as i64;
        let p90_sql = format!(
            "SELECT {SQL_TOKENS}{SUMMARY_SOURCE}{where_clause}\
             \nORDER BY {SQL_TOKENS} ASC\nLIMIT 1 OFFSET {offset}"
        );
        match conn.query_row(
            &p90_sql,
            rusqlite::params_from_iter(filter.args.iter()),
            |row| row.get::<_, i64>(0),
        ) {
            Ok(p90) => f.token_p90 = p90,
            // Warned rather than fatal, as in Go: the bars degrade to a shared
            // scale, the totals beside them stay correct.
            Err(e) => log::warn!("claude sessions: failed to compute token p90: {e}"),
        }
    }

    load_facet_options(conn, settings, &mut f)?;
    Ok(f)
}

/// Fill the dropdown options and the toggle gates.
///
/// Scoped to visible projects but not to the rest of the filter: the options
/// are what the corpus contains, so picking one never removes the others.
fn load_facet_options(
    conn: &Connection,
    settings: &DataSettings,
    f: &mut SessionFacets,
) -> Result<(), String> {
    let mut visible = Filter::default();
    for p in &settings.hidden_projects {
        visible.add("c.project_path != ?", vec![Value::Text(p.clone())]);
    }
    add_config_dir_scope(&mut visible, &settings.indexed_config_dirs);
    let where_clause = visible.where_clause();

    f.config_dirs = distinct_strings(
        conn,
        &format!(
            "SELECT DISTINCT c.config_dir FROM claude_session_cache c{where_clause} ORDER BY c.config_dir"
        ),
        &visible.args,
    )?;
    f.models = distinct_strings(
        conn,
        &format!(
            "SELECT DISTINCT c.model FROM claude_session_cache c{where_clause} ORDER BY c.model"
        ),
        &visible.args,
    )?;
    f.permission_modes = distinct_strings(
        conn,
        &format!(
            "SELECT DISTINCT c.permission_mode FROM claude_session_cache c{where_clause} ORDER BY c.permission_mode"
        ),
        &visible.args,
    )?;

    let mut favorites = Filter::default();
    favorites.add_all(&visible);
    favorites.add("c.is_favorite = 1", vec![]);
    f.has_favorites = exists_row(
        conn,
        &format!(
            "SELECT 1 FROM claude_session_cache c{} LIMIT 1",
            favorites.where_clause()
        ),
        &favorites.args,
    )?;

    let mut prs = Filter::default();
    prs.add_all(&visible);
    add_links(&mut prs, Links::With);
    f.has_prs = exists_row(
        conn,
        &format!(
            "SELECT 1 FROM claude_session_cache c{} LIMIT 1",
            prs.where_clause()
        ),
        &prs.args,
    )?;
    Ok(())
}

/// Distinct non-empty values, in the column's own order. Blank values are
/// dropped rather than offered as an unlabelled dropdown entry.
fn distinct_strings(conn: &Connection, sql: &str, args: &[Value]) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("claudesessions: reading facet options: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args.iter()), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| format!("claudesessions: reading facet options: {e}"))?;

    let mut out = Vec::new();
    for row in rows {
        let v = row.map_err(|e| format!("claudesessions: scanning facet option: {e}"))?;
        if !v.is_empty() {
            out.push(v);
        }
    }
    Ok(out)
}

fn exists_row(conn: &Connection, sql: &str, args: &[Value]) -> Result<bool, String> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("claudesessions: reading facet flag: {e}"))?;
    let mut rows = stmt
        .query(rusqlite::params_from_iter(args.iter()))
        .map_err(|e| format!("claudesessions: reading facet flag: {e}"))?;
    Ok(rows
        .next()
        .map_err(|e| format!("claudesessions: reading facet flag: {e}"))?
        .is_some())
}

/// Attach each session's linked pull requests, one query for the whole page.
///
/// Best-effort, as in Go: a failure here logs and leaves the lists empty rather
/// than failing a page that is otherwise complete.
fn attach_prs(conn: &Connection, sessions: &mut [SessionSummary]) {
    if sessions.is_empty() {
        return;
    }
    let placeholders = vec!["?"; sessions.len()].join(", ");
    let sql = format!(
        "SELECT session_id, pr_number, pr_url, pr_repository, first_seen_at
         FROM claude_session_pr WHERE session_id IN ({placeholders})
         ORDER BY first_seen_at, pr_url"
    );
    let ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();

    let mut by_session: BTreeMap<String, Vec<SessionPR>> = BTreeMap::new();
    let read = (|| -> rusqlite::Result<()> {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
            let session_id: String = row.get(0)?;
            let first_seen: String = row.get(4)?;
            Ok((
                session_id,
                SessionPR {
                    pr_number: row.get(1)?,
                    pr_url: row.get(2)?,
                    pr_repository: row.get(3)?,
                    first_seen_at: GoTime::parse_any(&first_seen).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::other(e)),
                        )
                    })?,
                },
            ))
        })?;
        for row in rows {
            let (session_id, pr) = row?;
            by_session.entry(session_id).or_default().push(pr);
        }
        Ok(())
    })();
    if let Err(e) = read {
        log::warn!("claude sessions: failed to load linked PRs for page: {e}");
        return;
    }

    // `get`, never `remove`: `claude_session_cache` is keyed on
    // `(session_id, project_path)` while `claude_session_pr` is keyed on the
    // session id alone, so one id legitimately yields two rows on a page and
    // draining the entry would leave every row after the first with `prs: []`.
    // Go re-reads the map per row, and an empty list renders as "no linked
    // PRs" rather than as an error, so the divergence would be silent.
    for s in sessions.iter_mut() {
        if let Some(prs) = by_session.get(&s.session_id) {
            s.prs = prs.clone();
        }
    }
}

/// Attach delegated usage and cost keyed by the sub-agent's own model.
///
/// The summary select's roll-up groups by parent session alone — deliberately,
/// so a multi-sub-agent session is not multiplied out — which collapses the
/// model dimension. This is the second grouped read that keeps it, and it is
/// what lets model attribution credit delegated tokens to the model that spent
/// them rather than to the delegating parent.
fn attach_subagent_usage_by_model(conn: &Connection, sessions: &mut [SessionSummary]) {
    if sessions.is_empty() {
        return;
    }
    let placeholders = vec!["?"; sessions.len()].join(", ");
    let sql = format!(
        "SELECT parent_session_id, model,
                SUM(input_tokens), SUM(output_tokens),
                SUM(cache_creation_tokens), SUM(cache_read_tokens),
                SUM(cache_creation_5m_tokens), SUM(cache_creation_1h_tokens),
                SUM(input_cost_usd), SUM(output_cost_usd),
                SUM(cache_read_cost_usd), SUM(cache_write_cost_usd), SUM(total_cost_usd)
         FROM claude_subagent_cache WHERE parent_session_id IN ({placeholders})
         GROUP BY parent_session_id, model"
    );
    let ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();

    type ByModel = BTreeMap<String, (BTreeMap<String, TokenUsage>, BTreeMap<String, SessionCost>)>;
    let mut by_session: ByModel = BTreeMap::new();
    let read = (|| -> rusqlite::Result<()> {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
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
        log::warn!("claude sessions: failed to load sub-agent usage by model for page: {e}");
        return;
    }

    // `get`, never `remove` — same reason as `attach_prs` above:
    // `claude_subagent_cache` is keyed on the parent session id alone, so a
    // session id appearing under two project paths must not have its
    // breakdown consumed by whichever of the two rows the page listed first.
    for s in sessions.iter_mut() {
        if let Some((usage, cost)) = by_session.get(&s.session_id) {
            s.subagent_usage_by_model = usage.clone();
            s.subagent_cost_by_model = cost.clone();
        }
    }
}
