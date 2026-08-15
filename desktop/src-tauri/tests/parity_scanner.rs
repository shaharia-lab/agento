//! Scanner parity: **every stored cache row, recomputed from its own
//! transcript**.
//!
//! There is no HTTP response to diff here — the scanner's output is a table —
//! so the parity bar is the rows Go already wrote. Each one is rebuilt from the
//! JSONL it was built from and compared field by field, across the whole local
//! corpus. On the reference machine that is ~900 sessions and about a gigabyte
//! of real transcripts, which is a far stronger check than any fixture: it
//! exercises every event shape the corpus happens to contain, including the
//! ones nobody thought to write a fixture for.
//!
//! Two anchors keep it honest, both learned from the insight parity suite:
//!
//! * **The row records the mtime it was read at.** A transcript that has grown
//!   since — the session running this very test is one — is not a divergence;
//!   the stored row describes a shorter file. Every figure would read as
//!   "computed is larger", which is exactly what an over-counting bug also
//!   looks like, so those rows are skipped rather than tolerated.
//! * **The scanner version and idle threshold must match.** A row written under
//!   a different version or threshold is a different computation. Comparing
//!   across one would fail as a thousand mismatches instead of one clear
//!   message.

mod parity_common;

use std::collections::BTreeMap;
use std::path::Path;

use agento_lib::native::gotime::GoTime;
use agento_lib::native::pricing;
use agento_lib::native::scanner::summary_file::{read_session_summary, read_subagent_summary};
use agento_lib::native::scanner::CURRENT_SCANNER_VERSION;
use agento_lib::native::sessions::summary::{SessionCost, SessionSummary};
use agento_lib::native::{db, settings};

use parity_common::live_db;

/// The stored row, in the columns `insertCacheRow` writes.
struct StoredRow {
    session_id: String,
    project_path: String,
    file_path: String,
    file_mtime: String,
    summary: SessionSummary,
}

#[test]
#[ignore = "needs a running Agento instance and its database"]
fn every_stored_session_row_recomputes_to_the_same_values() {
    let db_path = live_db();
    let conn = db::open_read_only(&db_path).expect("open database");

    let (stored_version, stored_threshold): (i64, i64) = conn
        .query_row(
            "SELECT COALESCE(scanner_version, 0), COALESCE(idle_threshold_ms, 0)
             FROM claude_cache_metadata WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("cache metadata");

    assert_eq!(
        stored_version, CURRENT_SCANNER_VERSION,
        "the corpus was scanned by a different scanner version; rescan before comparing"
    );
    let configured = settings::load(&conn).idle_gap_ms;
    assert_eq!(
        stored_threshold, configured,
        "the idle threshold moved since the last scan; rescan before comparing"
    );

    let resolver = pricing::Resolver::load(&conn).expect("pricing catalog");
    let rows = load_stored_rows(&conn);
    assert!(
        !rows.is_empty(),
        "no session rows to compare; let the Go scanner walk the corpus first"
    );
    println!("comparing {} stored session rows", rows.len());

    let mut mismatches: Vec<String> = Vec::new();
    let mut missing_files = 0usize;
    let mut moved_on = 0usize;
    let mut no_row = 0usize;
    let mut compared = 0usize;

    for row in &rows {
        let path = Path::new(&row.file_path);
        let Ok(meta) = std::fs::metadata(path) else {
            // Deleted since the row was written. Nothing to recompute from, and
            // not a divergence.
            missing_files += 1;
            continue;
        };

        // The row carries the mtime it was read at, so "has this file moved on"
        // is answerable exactly rather than by a timestamp comparison.
        if !mtime_matches(&meta, &row.file_mtime) {
            moved_on += 1;
            continue;
        }

        let got = match read_session_summary(
            &row.session_id,
            &row.project_path,
            path,
            Some(&resolver),
            stored_threshold,
        ) {
            Ok(Some(summary)) => summary,
            Ok(None) => {
                // The scanner writes no row for a transcript with no
                // timestamped event, so a stored row means one existed.
                no_row += 1;
                mismatches.push(format!(
                    "{}: recomputed to no row at all, but a row is stored",
                    row.session_id
                ));
                continue;
            }
            Err(e) => {
                mismatches.push(format!("{}: {e}", row.session_id));
                continue;
            }
        };

        compared += 1;
        if let Some(detail) = describe_mismatch(&row.summary, &got) {
            mismatches.push(format!("{} ({}):\n{detail}", row.session_id, row.file_path));
        }
    }

    println!(
        "compared {compared}, skipped {missing_files} deleted and {moved_on} grown since the scan"
    );
    assert_eq!(no_row, 0, "rows exist that recompute to nothing");
    assert!(
        compared > 0,
        "every row was skipped; nothing was actually compared"
    );

    if !mismatches.is_empty() {
        let shown: Vec<&String> = mismatches.iter().take(10).collect();
        panic!(
            "{} of {compared} rows diverged:\n\n{}\n\n(showing up to 10)",
            mismatches.len(),
            shown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        );
    }
}

/// Whether the file's current mtime is the one the row was built from.
///
/// The stored value is Go's `time.Time` rendering; comparing at second
/// granularity avoids a false mismatch from the two sides' sub-second
/// formatting while still catching any real append.
fn mtime_matches(meta: &std::fs::Metadata, stored: &str) -> bool {
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(stored) = GoTime::parse_any(stored) else {
        return false;
    };
    let current: chrono::DateTime<chrono::Utc> = modified.into();
    current.timestamp() == stored.instant().timestamp()
}

fn load_stored_rows(conn: &rusqlite::Connection) -> Vec<StoredRow> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, project_path, file_path, file_mtime,
                    preview, start_time, last_activity, message_count, event_count,
                    input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                    cache_creation_5m_tokens, cache_creation_1h_tokens,
                    git_branch, model, cwd, native_title, ai_title,
                    agent_name, permission_mode, mode, relocated_cwd,
                    worktree_name, worktree_branch, original_branch,
                    compaction_count, dropped_tokens,
                    input_cost_usd, output_cost_usd, cache_read_cost_usd,
                    cache_write_cost_usd, total_cost_usd, unpriced_models, unpriced_tokens,
                    cost_by_model, active_duration_ms
             FROM claude_session_cache",
        )
        .expect("prepare");

    let rows = stmt
        .query_map([], |r| {
            let mut summary = SessionSummary {
                session_id: r.get(0)?,
                project_path: r.get(1)?,
                preview: r.get(4)?,
                start_time: parse(r.get::<_, String>(5)?),
                last_activity: parse(r.get::<_, String>(6)?),
                message_count: r.get(7)?,
                event_count: r.get(8)?,
                git_branch: r.get(15)?,
                model: r.get(16)?,
                cwd: r.get(17)?,
                native_title: r.get(18)?,
                ai_title: r.get(19)?,
                agent_name: r.get(20)?,
                permission_mode: r.get(21)?,
                mode: r.get(22)?,
                relocated_cwd: r.get(23)?,
                worktree_name: r.get(24)?,
                worktree_branch: r.get(25)?,
                original_branch: r.get(26)?,
                compaction_count: r.get(27)?,
                dropped_tokens: r.get(28)?,
                unpriced_tokens: r.get(35)?,
                active_duration_ms: r.get(37)?,
                ..Default::default()
            };
            summary.usage.input_tokens = r.get(9)?;
            summary.usage.output_tokens = r.get(10)?;
            summary.usage.cache_creation_tokens = r.get(11)?;
            summary.usage.cache_read_tokens = r.get(12)?;
            summary.usage.cache_creation_5m_tokens = r.get(13)?;
            summary.usage.cache_creation_1h_tokens = r.get(14)?;
            summary.cost = SessionCost {
                input_usd: r.get(29)?,
                output_usd: r.get(30)?,
                cache_read_usd: r.get(31)?,
                cache_write_usd: r.get(32)?,
                total_usd: r.get(33)?,
            };
            // Newline-joined, not JSON: a model id may contain a slash but
            // never a newline.
            let unpriced: String = r.get(34)?;
            summary.unpriced_models = unpriced
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            let by_model: String = r.get(36)?;
            summary.cost_by_model = decode_cost_by_model(&by_model);

            Ok(StoredRow {
                session_id: r.get(0)?,
                project_path: r.get(1)?,
                file_path: r.get(2)?,
                file_mtime: r.get(3)?,
                summary,
            })
        })
        .expect("query")
        .filter_map(Result::ok)
        .collect();
    rows
}

fn parse(text: String) -> GoTime {
    GoTime::parse_any(&text).unwrap_or_default()
}

/// The stored `cost_by_model` column: JSON, and `""` rather than `"{}"` when
/// empty.
fn decode_cost_by_model(raw: &str) -> BTreeMap<String, SessionCost> {
    if raw.is_empty() || raw == "{}" {
        return BTreeMap::new();
    }
    serde_json::from_str(raw).unwrap_or_default()
}

/// A field-by-field report, so a divergence names the field rather than dumping
/// two structs.
fn describe_mismatch(want: &SessionSummary, got: &SessionSummary) -> Option<String> {
    let mut out = Vec::new();

    macro_rules! cmp {
        ($field:ident) => {
            if want.$field != got.$field {
                out.push(format!(
                    "  {}: stored {:?} vs computed {:?}",
                    stringify!($field),
                    want.$field,
                    got.$field
                ));
            }
        };
    }

    cmp!(preview);
    cmp!(message_count);
    cmp!(event_count);
    cmp!(git_branch);
    cmp!(model);
    cmp!(cwd);
    cmp!(native_title);
    cmp!(ai_title);
    cmp!(agent_name);
    cmp!(permission_mode);
    cmp!(mode);
    cmp!(relocated_cwd);
    cmp!(worktree_name);
    cmp!(worktree_branch);
    cmp!(original_branch);
    cmp!(compaction_count);
    cmp!(dropped_tokens);
    cmp!(active_duration_ms);
    cmp!(unpriced_tokens);
    cmp!(unpriced_models);

    if want.start_time.instant() != got.start_time.instant() {
        out.push(format!(
            "  start_time: stored {} vs computed {}",
            want.start_time.rfc3339_nano_utc(),
            got.start_time.rfc3339_nano_utc()
        ));
    }
    if want.last_activity.instant() != got.last_activity.instant() {
        out.push(format!(
            "  last_activity: stored {} vs computed {}",
            want.last_activity.rfc3339_nano_utc(),
            got.last_activity.rfc3339_nano_utc()
        ));
    }

    for (label, a, b) in [
        (
            "usage.input_tokens",
            want.usage.input_tokens,
            got.usage.input_tokens,
        ),
        (
            "usage.output_tokens",
            want.usage.output_tokens,
            got.usage.output_tokens,
        ),
        (
            "usage.cache_creation_tokens",
            want.usage.cache_creation_tokens,
            got.usage.cache_creation_tokens,
        ),
        (
            "usage.cache_read_tokens",
            want.usage.cache_read_tokens,
            got.usage.cache_read_tokens,
        ),
        (
            "usage.cache_creation_5m_tokens",
            want.usage.cache_creation_5m_tokens,
            got.usage.cache_creation_5m_tokens,
        ),
        (
            "usage.cache_creation_1h_tokens",
            want.usage.cache_creation_1h_tokens,
            got.usage.cache_creation_1h_tokens,
        ),
    ] {
        if a != b {
            out.push(format!("  {label}: stored {a} vs computed {b}"));
        }
    }

    // Money is compared with a tolerance: both sides sum the same per-message
    // amounts, but float addition is not associative and the two languages'
    // accumulation order need not match to the last bit.
    for (label, a, b) in [
        ("cost.input_usd", want.cost.input_usd, got.cost.input_usd),
        ("cost.output_usd", want.cost.output_usd, got.cost.output_usd),
        (
            "cost.cache_read_usd",
            want.cost.cache_read_usd,
            got.cost.cache_read_usd,
        ),
        (
            "cost.cache_write_usd",
            want.cost.cache_write_usd,
            got.cost.cache_write_usd,
        ),
        ("cost.total_usd", want.cost.total_usd, got.cost.total_usd),
    ] {
        if !close_enough(a, b) {
            out.push(format!("  {label}: stored {a} vs computed {b}"));
        }
    }

    let want_models: Vec<&String> = want.cost_by_model.keys().collect();
    let got_models: Vec<&String> = got.cost_by_model.keys().collect();
    if want_models != got_models {
        out.push(format!(
            "  cost_by_model keys: stored {want_models:?} vs computed {got_models:?}"
        ));
    } else {
        for (model, want_cost) in &want.cost_by_model {
            let got_cost = &got.cost_by_model[model];
            if !close_enough(want_cost.total_usd, got_cost.total_usd) {
                out.push(format!(
                    "  cost_by_model[{model}].total_usd: stored {} vs computed {}",
                    want_cost.total_usd, got_cost.total_usd
                ));
            }
        }
    }

    (!out.is_empty()).then(|| out.join("\n"))
}

/// Relative tolerance for a summed money figure.
fn close_enough(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    let scale = a.abs().max(b.abs()).max(1e-9);
    (a - b).abs() / scale < 1e-9
}

/// One stored sub-agent row, in the columns `upsertSubagentRow` writes.
struct StoredSubagent {
    parent_session_id: String,
    agent_id: String,
    file_path: String,
    file_mtime: String,
    summary: SessionSummary,
}

#[test]
#[ignore = "needs a running Agento instance and its database"]
fn every_stored_subagent_row_recomputes_to_the_same_values() {
    let db_path = live_db();
    let conn = db::open_read_only(&db_path).expect("open database");

    let (stored_version, stored_threshold): (i64, i64) = conn
        .query_row(
            "SELECT COALESCE(scanner_version, 0), COALESCE(idle_threshold_ms, 0)
             FROM claude_cache_metadata WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("cache metadata");
    assert_eq!(
        stored_version, CURRENT_SCANNER_VERSION,
        "the corpus was scanned by a different scanner version; rescan before comparing"
    );

    let resolver = pricing::Resolver::load(&conn).expect("pricing catalog");
    let rows = load_stored_subagents(&conn);
    if rows.is_empty() {
        println!("no sub-agent rows on this corpus; nothing to compare");
        return;
    }
    println!("comparing {} stored sub-agent rows", rows.len());

    let mut mismatches: Vec<String> = Vec::new();
    let (mut compared, mut skipped) = (0usize, 0usize);

    for row in &rows {
        let path = Path::new(&row.file_path);
        let Ok(meta) = std::fs::metadata(path) else {
            skipped += 1;
            continue;
        };
        if !mtime_matches(&meta, &row.file_mtime) {
            skipped += 1;
            continue;
        }

        // Every event in a sub-agent transcript carries `isSidechain`. Reading
        // one with the parent's rule would count no user turns at all, so
        // `message_count` would silently degrade to assistant-only — which is
        // exactly what this comparison would catch.
        let got = match read_subagent_summary(
            &row.parent_session_id,
            "",
            path,
            Some(&resolver),
            stored_threshold,
        ) {
            Ok(Some(summary)) => summary,
            Ok(None) => {
                mismatches.push(format!(
                    "{}/{}: recomputed to no row, but a row is stored",
                    row.parent_session_id, row.agent_id
                ));
                continue;
            }
            Err(e) => {
                mismatches.push(format!("{}/{}: {e}", row.parent_session_id, row.agent_id));
                continue;
            }
        };

        compared += 1;
        if let Some(detail) = describe_subagent_mismatch(&row.summary, &got) {
            mismatches.push(format!(
                "{}/{} ({}):\n{detail}",
                row.parent_session_id, row.agent_id, row.file_path
            ));
        }
    }

    println!("compared {compared}, skipped {skipped}");
    assert!(compared > 0, "every sub-agent row was skipped");
    if !mismatches.is_empty() {
        let shown: Vec<&str> = mismatches.iter().take(10).map(String::as_str).collect();
        panic!(
            "{} of {compared} sub-agent rows diverged:\n\n{}\n\n(showing up to 10)",
            mismatches.len(),
            shown.join("\n\n")
        );
    }
}

fn load_stored_subagents(conn: &rusqlite::Connection) -> Vec<StoredSubagent> {
    let mut stmt = conn
        .prepare(
            "SELECT parent_session_id, agent_id, file_path, file_mtime,
                    start_time, last_activity, message_count, event_count,
                    input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                    cache_creation_5m_tokens, cache_creation_1h_tokens, model,
                    input_cost_usd, output_cost_usd, cache_read_cost_usd,
                    cache_write_cost_usd, total_cost_usd, unpriced_tokens,
                    active_duration_ms
             FROM claude_subagent_cache",
        )
        .expect("prepare");

    stmt.query_map([], |r| {
        let mut summary = SessionSummary {
            message_count: r.get(6)?,
            event_count: r.get(7)?,
            model: r.get(14)?,
            unpriced_tokens: r.get(20)?,
            active_duration_ms: r.get(21)?,
            start_time: parse(r.get::<_, String>(4)?),
            last_activity: parse(r.get::<_, String>(5)?),
            ..Default::default()
        };
        summary.usage.input_tokens = r.get(8)?;
        summary.usage.output_tokens = r.get(9)?;
        summary.usage.cache_creation_tokens = r.get(10)?;
        summary.usage.cache_read_tokens = r.get(11)?;
        summary.usage.cache_creation_5m_tokens = r.get(12)?;
        summary.usage.cache_creation_1h_tokens = r.get(13)?;
        summary.cost = SessionCost {
            input_usd: r.get(15)?,
            output_usd: r.get(16)?,
            cache_read_usd: r.get(17)?,
            cache_write_usd: r.get(18)?,
            total_usd: r.get(19)?,
        };
        Ok(StoredSubagent {
            parent_session_id: r.get(0)?,
            agent_id: r.get(1)?,
            file_path: r.get(2)?,
            file_mtime: r.get(3)?,
            summary,
        })
    })
    .expect("query")
    .filter_map(Result::ok)
    .collect()
}

/// The sub-agent table stores a subset of the session columns — no preview, no
/// titles, no cwd, no `cost_by_model` — so only what it holds is compared.
fn describe_subagent_mismatch(want: &SessionSummary, got: &SessionSummary) -> Option<String> {
    let mut out = Vec::new();

    for (label, a, b) in [
        ("message_count", want.message_count, got.message_count),
        ("event_count", want.event_count, got.event_count),
        (
            "active_duration_ms",
            want.active_duration_ms,
            got.active_duration_ms,
        ),
        ("unpriced_tokens", want.unpriced_tokens, got.unpriced_tokens),
        (
            "usage.input_tokens",
            want.usage.input_tokens,
            got.usage.input_tokens,
        ),
        (
            "usage.output_tokens",
            want.usage.output_tokens,
            got.usage.output_tokens,
        ),
        (
            "usage.cache_creation_tokens",
            want.usage.cache_creation_tokens,
            got.usage.cache_creation_tokens,
        ),
        (
            "usage.cache_read_tokens",
            want.usage.cache_read_tokens,
            got.usage.cache_read_tokens,
        ),
        (
            "usage.cache_creation_5m_tokens",
            want.usage.cache_creation_5m_tokens,
            got.usage.cache_creation_5m_tokens,
        ),
        (
            "usage.cache_creation_1h_tokens",
            want.usage.cache_creation_1h_tokens,
            got.usage.cache_creation_1h_tokens,
        ),
    ] {
        if a != b {
            out.push(format!("  {label}: stored {a} vs computed {b}"));
        }
    }

    if want.model != got.model {
        out.push(format!(
            "  model: stored {:?} vs computed {:?}",
            want.model, got.model
        ));
    }
    if want.start_time.instant() != got.start_time.instant() {
        out.push(format!(
            "  start_time: stored {} vs computed {}",
            want.start_time.rfc3339_nano_utc(),
            got.start_time.rfc3339_nano_utc()
        ));
    }
    if want.last_activity.instant() != got.last_activity.instant() {
        out.push(format!(
            "  last_activity: stored {} vs computed {}",
            want.last_activity.rfc3339_nano_utc(),
            got.last_activity.rfc3339_nano_utc()
        ));
    }
    if !close_enough(want.cost.total_usd, got.cost.total_usd) {
        out.push(format!(
            "  cost.total_usd: stored {} vs computed {}",
            want.cost.total_usd, got.cost.total_usd
        ));
    }

    (!out.is_empty()).then(|| out.join("\n"))
}
