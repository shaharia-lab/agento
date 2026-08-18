//! Live parity for the Claude session insights: the summary endpoint, and the
//! nine processors behind the rows it reads.
//!
//! Two different checks, because the two halves ship differently.
//!
//! **The summary** is a claimed route, so it is diffed byte for byte against a
//! running Go server like every other ported endpoint.
//!
//! **The processors write nothing** (see `native/insights/mod.rs`), so there is
//! no response to diff. Instead they are run over the same transcripts the Go
//! worker already processed and compared against the rows it stored — which is
//! a far stronger check than any fixture: ~900 real sessions, ~1 GB of JSONL,
//! every shape the corpus contains.
//!
//! Only rows at the current processor version are compared. An older row holds
//! figures a *correct* port must disagree with, since a version bump exists
//! precisely because the logic changed.
//!
//! ```sh
//! cd desktop && eval "$(./scripts/parity-instance.sh start)"
//! (cd src-tauri && cargo test --test parity_insights -- --ignored --nocapture)
//! ./scripts/parity-instance.sh stop
//! ```
//!
//! **Read-only.** GETs and a read-only database handle, nothing else.

mod parity_common;

use parity_common::*;

use agento_lib::native::insights::processors::{self, SessionInsight, CURRENT_PROCESSOR_VERSION};
use agento_lib::native::{db, gojson, insights, pricing, settings};

/// The summary endpoint, across the windows the dashboards request plus the
/// `ids` narrowing the analytics endpoint has no equivalent of.
///
/// Same caveat as the analytics suite: `sortedToolCounts` on the Go side ranks
/// a map's entries with an insertion sort, so entries tying on count come out
/// in either order — and at the tenth place a tie changes *membership*, not
/// just position. `fetch_until` re-asks rather than failing on the first
/// disagreement. This endpoint is not memoized, so no eviction is needed.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn insights_summary_matches_the_live_go_responses() {
    let db_path = live_db();
    let conn = db::open_read_only(&db_path).expect("open database");
    let data_settings = settings::load(&conn);

    let mut cases = vec![
        String::new(),
        "from=2026-06-01&to=2026-08-14".to_string(),
        "from=2026-06-01&to=2026-08-14&tz=Europe/Berlin".to_string(),
        "from=2020-01-01&to=2026-12-31".to_string(),
        // Empty window: the zero-valued summary, every list an empty array —
        // the case `CLAUDE.md` and the issue both described as `null`.
        "from=1990-01-01&to=1990-12-31".to_string(),
        // Blanks and whitespace in `ids` are dropped, not matched.
        "from=2020-01-01&to=2026-12-31&ids=%20,%20,".to_string(),
        // An id outside the window must not widen it.
        "from=1990-01-01&to=1990-12-31&ids=whatever".to_string(),
    ];

    // Real ids, taken from the corpus: the narrowing only means something
    // against sessions the window already contains.
    let listed = fetch("/api/claude-sessions?limit=3").await;
    let parsed: serde_json::Value = serde_json::from_slice(&listed).expect("json");
    let ids: Vec<String> = parsed["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|s| s["session_id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if !ids.is_empty() {
        cases.push(format!(
            "from=2020-01-01&to=2026-12-31&ids={}",
            ids.join(",")
        ));
        // One real id plus one that does not exist: the intersection keeps the
        // first and silently drops the second.
        cases.push(format!(
            "from=2020-01-01&to=2026-12-31&ids={},not-a-session",
            ids[0]
        ));
    }

    for case in &cases {
        let label = if case.is_empty() { "(defaults)" } else { case };
        let native = gojson::to_vec(
            &insights::summary::summary(&conn, &data_settings, case).expect("native summary"),
        )
        .expect("encode summary");

        let go = fetch(&format!("/api/claude-sessions/insights/summary?{case}")).await;
        assert_matches_modulo_ties(&format!("insights [{label}]"), &go, &native);
    }
}

/// The seven ranked lists, every one of which can tie.
const RANKED_LISTS: [&str; 7] = [
    "top_tools",
    "top_skills",
    "top_plugins",
    "top_mcp_servers",
    "top_mcp_tools",
    "top_efforts",
    "top_agents",
];

/// Byte-identical, or different only where Go itself is not reproducible.
///
/// `sortedToolCounts` ranks a Go map's entries with an insertion sort, so
/// entries **tying on count** come out in an order Go does not fix — and at the
/// tenth place a tie decides *membership*, not just position. On this corpus
/// `vibexp_io_update_artifact` and `vibexp_io_create_artifact` both score 217
/// and swap between requests, across seven such lists at once, so re-asking Go
/// until it agrees (what the analytics suite does for its single tie) would
/// almost never converge.
///
/// The rule instead: everything must match byte for byte, **except** that a
/// `tool` name may differ where its `count` is shared with another entry. Every
/// scalar, every count, every list length and the order of distinct counts are
/// still compared exactly, so a mis-keyed or miscounted breakdown still fails.
fn assert_matches_modulo_ties(label: &str, go: &[u8], native: &[u8]) {
    if go == native {
        println!("{label}: identical ({} bytes)", go.len());
        return;
    }

    let mut go_json: serde_json::Value = serde_json::from_slice(go).expect("go json");
    let mut native_json: serde_json::Value = serde_json::from_slice(native).expect("native json");
    let mut masked = 0;
    for list in RANKED_LISTS {
        masked += mask_tied_names(&mut go_json, &mut native_json, list);
    }

    assert_eq!(
        go_json,
        native_json,
        "{label}: differs outside the tied entries\n go:     {}\n native: {}",
        String::from_utf8_lossy(go),
        String::from_utf8_lossy(native)
    );
    assert!(
        masked > 0,
        "{label}: bodies differ but no tie explains it\n go:     {}\n native: {}",
        String::from_utf8_lossy(go),
        String::from_utf8_lossy(native)
    );
    println!("{label}: identical apart from {masked} tied entries");
}

/// Blank the `tool` of every entry the tie makes ambiguous, returning how many
/// were blanked. A count that is unique still has its name compared.
///
/// Two shapes are ambiguous, and the second is the one that bites: a count
/// shared *within* a list, and the **last entry of a full list**, where an
/// equal-scoring competitor was cut by the top-ten cap and so does not appear in
/// the response at all. On this corpus `vibexp_io_update_artifact` and
/// `vibexp_io_create_artifact` both score 217 and take turns being tenth, with
/// nothing in the payload to show a tie happened.
///
/// The residual gap is deliberate and small: a wrong name at exactly position
/// ten, with the right count, would pass. Every other position, every count and
/// every scalar are still compared exactly.
fn mask_tied_names(
    go: &mut serde_json::Value,
    native: &mut serde_json::Value,
    list: &str,
) -> usize {
    let counts = |v: &serde_json::Value| -> Vec<i64> {
        v[list]
            .as_array()
            .map(|a| a.iter().filter_map(|e| e["count"].as_i64()).collect())
            .unwrap_or_default()
    };
    let mut seen: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for c in counts(go).into_iter().chain(counts(native)) {
        *seen.entry(c).or_default() += 1;
    }
    // A count seen twice across the two lists is one entry in each; three or
    // more means it is shared within a list, which is the ambiguous case.
    let tied: std::collections::HashSet<i64> = seen
        .into_iter()
        .filter(|(_, n)| *n > 2)
        .map(|(c, _)| c)
        .collect();

    /// The cap `sortedToolCounts` truncates to.
    const TOP_BREAKDOWN_ENTRIES: usize = 10;

    let mut masked = 0;
    for value in [go, native] {
        let Some(entries) = value[list].as_array_mut() else {
            continue;
        };
        let truncated = entries.len() == TOP_BREAKDOWN_ENTRIES;
        let last = entries.len().saturating_sub(1);
        for (i, entry) in entries.iter_mut().enumerate() {
            let shared = entry["count"].as_i64().is_some_and(|c| tied.contains(&c));
            if shared || (truncated && i == last) {
                entry["tool"] = serde_json::Value::String("<tied>".into());
                masked += 1;
            }
        }
    }
    masked
}

/// One session's stored row, as the Go worker wrote it.
struct StoredRow {
    session_id: String,
    file_path: String,
    /// When the worker computed this row. If a transcript on disk is newer,
    /// the row describes a *shorter* file and cannot be compared.
    ///
    /// The insight's own timestamp, not the scanner's `file_mtime`: those are
    /// written by different passes, and on a machine still using Claude Code
    /// the scan routinely runs after the worker last processed a session.
    scanned_at: std::time::SystemTime,
    insight: SessionInsight,
}

/// Recompute every stored insight from its transcript and compare.
///
/// This is the processors' whole parity bar. It reads ~1 GB of JSONL on the
/// reference corpus, so it is slow by design — and it is the only check that
/// exercises the shapes real transcripts contain rather than the ones a fixture
/// imagines.
#[test]
#[ignore = "needs a scanned Agento database and its transcripts"]
fn every_stored_insight_recomputes_to_the_same_values() {
    let db_path = live_db();
    let conn = db::open_read_only(&db_path).expect("open database");

    // The threshold the *stored* rows were computed under, not the one
    // configured now. They agree at rest — a change re-reads and reprocesses —
    // but reading the recorded value means a mid-flight change fails loudly
    // rather than as a thousand duration mismatches.
    let stored_threshold: i64 = conn
        .query_row(
            "SELECT COALESCE(idle_threshold_ms, 0) FROM claude_cache_metadata WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .expect("cache metadata");
    let configured = settings::load(&conn).idle_gap_ms;
    assert_eq!(
        stored_threshold, configured,
        "the idle threshold moved since the last scan; rescan before comparing"
    );

    let resolver = pricing::Resolver::load(&conn).expect("pricing catalog");
    let ctx = processors::Ctx {
        idle_gap_ms: stored_threshold,
        resolver: Some(&resolver),
    };

    let rows = load_stored_rows(&conn);
    assert!(
        !rows.is_empty(),
        "no insight rows at version {CURRENT_PROCESSOR_VERSION}; \
         let the Go worker process the corpus first"
    );
    println!("comparing {} stored insights", rows.len());

    let mut mismatches = Vec::new();
    let mut missing_files = 0;
    let mut moved_on = 0;
    let mut tie_ambiguous = 0;
    for row in &rows {
        let path = std::path::Path::new(&row.file_path);
        if !path.exists() {
            // The transcript was deleted after the row was written. Nothing to
            // recompute from, and not a divergence.
            missing_files += 1;
            continue;
        }
        let files = processors::session_files(&row.session_id, path);
        // A transcript that has grown since the scan is the common case on a
        // machine that is still using Claude Code — the session running this
        // very test is one — and it is not a divergence: the stored row
        // describes a shorter file. Every figure would read as "computed is
        // larger", which is exactly what an over-counting bug also looks like,
        // so this has to be excluded rather than tolerated.
        if written_since(&files, row.scanned_at) {
            moved_on += 1;
            continue;
        }
        let got = match processors::run(&row.session_id, &files, &ctx) {
            Ok(insight) => insight,
            Err(e) => {
                mismatches.push(format!("{}: {e}", row.session_id));
                continue;
            }
        };
        if got == row.insight {
            continue;
        }
        // The one divergence Go cannot be held to: a tied timestamp makes the
        // stored working-time figure depend on an unstable sort's order. Only
        // that field, and only for a session that actually contains such a tie.
        let mut comparable = got.clone();
        comparable.claude_working_time_ms = row.insight.claude_working_time_ms;
        if comparable == row.insight && has_ambiguous_timestamp_tie(&files) {
            tie_ambiguous += 1;
            continue;
        }
        mismatches.push(describe_mismatch(&row.insight, &got));
    }

    if missing_files > 0 {
        println!("{missing_files} transcripts no longer on disk; skipped");
    }
    if moved_on > 0 {
        println!("{moved_on} transcripts grew since the row was written; skipped");
    }
    if tie_ambiguous > 0 {
        println!(
            "{tie_ambiguous} sessions differ only in claude_working_time_ms, \
             on a tied timestamp Go's unstable sort does not order"
        );
    }
    let compared = rows.len() - missing_files - moved_on;
    assert!(
        compared > 0,
        "every stored row was skipped; nothing was actually compared"
    );
    assert!(
        mismatches.is_empty(),
        "{} of {} sessions diverged:\n{}",
        mismatches.len(),
        rows.len(),
        mismatches
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    println!("all {compared} comparable sessions recompute identically");
}

/// The `time.Time.String()` text a DATETIME column holds, as a `SystemTime`.
fn parse_go_time(text: &str) -> std::time::SystemTime {
    match agento_lib::native::gotime::GoTime::parse_any(text) {
        Ok(t) => {
            std::time::UNIX_EPOCH
                + std::time::Duration::from_nanos(
                    t.instant().timestamp_nanos_opt().unwrap_or(0).max(0) as u64,
                )
        }
        // An unparsable mtime compares as "very old", so the row is skipped
        // rather than silently compared against a file it may not describe.
        Err(_) => std::time::UNIX_EPOCH,
    }
}

/// Whether any of a session's transcripts has been written since the row was
/// computed. A millisecond of slack absorbs the DATETIME column's text
/// round trip.
fn written_since(files: &[std::path::PathBuf], at: std::time::SystemTime) -> bool {
    let slack = std::time::Duration::from_millis(1);
    files.iter().any(|f| {
        std::fs::metadata(f)
            .and_then(|m| m.modified())
            .is_ok_and(|m| m > at + slack)
    })
}

/// Whether a session contains two events sharing a timestamp where one is an
/// assistant event and the other is not.
///
/// That is the one shape where Go's stored figure is **not reproducible**:
/// `activeTimeTracker.durations` sorts with `sort.Slice`, which is unstable, and
/// the gap *leading into* a tied pair is credited to whichever member sorts
/// first — so `claude_working_time_ms` depends on an order Go does not fix.
/// This port sorts stably, which is deterministic but agrees with only one of
/// the orders Go could have stored.
fn has_ambiguous_timestamp_tie(files: &[std::path::PathBuf]) -> bool {
    let mut stamps: Vec<(chrono::DateTime<chrono::Utc>, bool)> = Vec::new();
    for file in files {
        let Ok(events) = agento_lib::native::insights::transcript::read(file) else {
            continue;
        };
        for ev in events {
            if ev.event_type == "file-history-snapshot" {
                continue;
            }
            if let Some(ts) = ev.timestamp {
                stamps.push((ts, ev.event_type == "assistant"));
            }
        }
    }
    stamps.sort_by_key(|(ts, _)| *ts);
    stamps
        .windows(2)
        .any(|w| w[0].0 == w[1].0 && w[0].1 != w[1].1)
}

fn load_stored_rows(conn: &rusqlite::Connection) -> Vec<StoredRow> {
    let mut stmt = conn
        .prepare(
            "SELECT i.session_id, c.file_path, i.scanned_at,
                    i.turn_count, i.steps_per_turn_avg, i.autonomy_score,
                    i.tool_calls_total, i.tool_breakdown,
                    i.skill_breakdown, i.plugin_breakdown, i.mcp_server_breakdown,
                    i.mcp_tool_breakdown, i.effort_breakdown, i.agent_breakdown,
                    i.unattributed_calls,
                    i.total_duration_ms, i.active_duration_ms, i.claude_working_time_ms,
                    i.cache_hit_rate, i.tokens_per_turn_avg, i.cost_estimate_usd,
                    i.tool_error_rate, i.tool_error_count, i.has_errors,
                    i.max_consecutive_tool_calls, i.longest_autonomous_chain,
                    i.avg_user_response_time_ms, i.avg_claude_response_time_ms
             FROM session_insights i
             -- Both key columns, not just the id (#362). `claude_session_cache`
             -- is keyed on `(session_id, project_path)` and `session_insights`
             -- now is too, so joining on the id alone cross-products a
             -- duplicated id's rows against each other's transcripts and then
             -- compares one project's stored figures with the other project's
             -- file. That is what this suite had been reporting as a permanent
             -- one-session divergence.
             JOIN claude_session_cache c
               ON c.session_id = i.session_id AND c.project_path = i.project_path
             WHERE i.processor_version = ?",
        )
        .expect("prepare stored insights");

    let rows = stmt
        .query_map([CURRENT_PROCESSOR_VERSION], |row| {
            let decode = |raw: String| -> std::collections::BTreeMap<String, i64> {
                serde_json::from_str(&raw).unwrap_or_default()
            };
            let scanned_at: String = row.get(2)?;
            Ok(StoredRow {
                session_id: row.get(0)?,
                file_path: row.get(1)?,
                scanned_at: parse_go_time(&scanned_at),
                insight: SessionInsight {
                    session_id: row.get(0)?,
                    turn_count: row.get(3)?,
                    steps_per_turn_avg: row.get(4)?,
                    autonomy_score: row.get(5)?,
                    tool_calls_total: row.get(6)?,
                    tool_breakdown: decode(row.get(7)?),
                    skill_breakdown: decode(row.get(8)?),
                    plugin_breakdown: decode(row.get(9)?),
                    mcp_server_breakdown: decode(row.get(10)?),
                    mcp_tool_breakdown: decode(row.get(11)?),
                    effort_breakdown: decode(row.get(12)?),
                    agent_breakdown: decode(row.get(13)?),
                    unattributed_calls: row.get(14)?,
                    total_duration_ms: row.get(15)?,
                    active_duration_ms: row.get(16)?,
                    claude_working_time_ms: row.get(17)?,
                    cache_hit_rate: row.get(18)?,
                    tokens_per_turn_avg: row.get(19)?,
                    cost_estimate_usd: row.get(20)?,
                    tool_error_rate: row.get(21)?,
                    tool_error_count: row.get(22)?,
                    has_errors: row.get::<_, i64>(23)? == 1,
                    max_consecutive_tool_calls: row.get(24)?,
                    longest_autonomous_chain: row.get(25)?,
                    avg_user_response_time_ms: row.get(26)?,
                    avg_claude_response_time_ms: row.get(27)?,
                },
            })
        })
        .expect("query stored insights");

    rows.map(|r| r.expect("scan stored insight")).collect()
}

/// Name the fields that differ, rather than dumping two whole structs: a
/// hundred-session divergence should read as one repeated cause.
fn describe_mismatch(want: &SessionInsight, got: &SessionInsight) -> String {
    let mut fields = Vec::new();
    macro_rules! check {
        ($($field:ident),* $(,)?) => {$(
            if want.$field != got.$field {
                fields.push(format!(
                    "{}: stored {:?} != computed {:?}",
                    stringify!($field), want.$field, got.$field
                ));
            }
        )*};
    }
    check!(
        turn_count,
        steps_per_turn_avg,
        autonomy_score,
        tool_calls_total,
        tool_breakdown,
        skill_breakdown,
        plugin_breakdown,
        mcp_server_breakdown,
        mcp_tool_breakdown,
        effort_breakdown,
        agent_breakdown,
        unattributed_calls,
        total_duration_ms,
        active_duration_ms,
        claude_working_time_ms,
        cache_hit_rate,
        tokens_per_turn_avg,
        cost_estimate_usd,
        tool_error_rate,
        tool_error_count,
        has_errors,
        max_consecutive_tool_calls,
        longest_autonomous_chain,
        avg_user_response_time_ms,
        avg_claude_response_time_ms,
    );
    format!("{}\n    {}", want.session_id, fields.join("\n    "))
}
