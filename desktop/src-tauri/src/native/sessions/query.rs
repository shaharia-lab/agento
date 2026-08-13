//! The sessions list's filter, sort and page position.
//!
//! Mirrors `internal/claudesessions/session_query.go` and the cursor half of
//! `session_page.go`, plus `sessionQueryFromRequest` in
//! `internal/api/claude_sessions.go`.
//!
//! The metric expressions below are the load-bearing part. They must agree
//! exactly with `frontend/src/lib/sessionMetrics.ts`, which renders the columns:
//! a row showing $36.30 must not be hidden by "cost at most $40". Go and
//! TypeScript already assert that agreement from
//! `internal/claudesessions/testdata/session_metric_vectors.json`; this file's
//! tests make Rust the third reader of the same fixture.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::native::gojson;
use crate::native::gotime::GoTime;

/// Per-session figures as SQL over the summary select's `c` (session) and `sa`
/// (sub-agent roll-up) aliases. Every one but `messages` sums the main thread
/// and its delegated sub-agents, because that is what the list's columns show.
pub const SQL_INPUT_TOKENS: &str = "(c.input_tokens + COALESCE(sa.it, 0))";
pub const SQL_OUTPUT_TOKENS: &str = "(c.output_tokens + COALESCE(sa.ot, 0))";
pub const SQL_TOKENS: &str =
    "(c.input_tokens + COALESCE(sa.it, 0) + c.output_tokens + COALESCE(sa.ot, 0))";
pub const SQL_COST_USD: &str = "(c.total_cost_usd + COALESCE(sa.tc, 0))";
/// Active duration, not the wall-clock span: a resumable session's span counts
/// every idle day between sittings.
pub const SQL_ACTIVE_DURATION_MS: &str = "(c.active_duration_ms + COALESCE(sa.adm, 0))";
/// Message count stays main-thread, matching the column beside it.
pub const SQL_MESSAGE_COUNT: &str = "c.message_count";

/// The order a page is returned in. A closed set, because keyset pagination
/// needs the sort column indexed and the tiebreak stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Sort {
    #[default]
    Recent,
    Cost,
    Tokens,
    Duration,
    Messages,
}

impl Sort {
    /// Parse the `sort` parameter. An unknown value falls back to `Recent`
    /// rather than erroring: the list is a read-only view, and a stale bookmark
    /// is better rendered in the default order than refused.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "cost" => Sort::Cost,
            "tokens" => Sort::Tokens,
            "duration" => Sort::Duration,
            "messages" => Sort::Messages,
            _ => Sort::Recent,
        }
    }

    /// The wire name, which the cursor carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Sort::Recent => "recent",
            Sort::Cost => "cost",
            Sort::Tokens => "tokens",
            Sort::Duration => "duration",
            Sort::Messages => "messages",
        }
    }

    /// The SQL the sort orders and pages on, and whether its values are
    /// timestamps (which page through a time cursor rather than a float).
    pub fn expr(self) -> (&'static str, bool) {
        match self {
            Sort::Cost => (SQL_COST_USD, false),
            Sort::Tokens => (SQL_TOKENS, false),
            Sort::Duration => (SQL_ACTIVE_DURATION_MS, false),
            Sort::Messages => (SQL_MESSAGE_COUNT, false),
            Sort::Recent => ("c.last_activity", true),
        }
    }
}

/// Whether a session must have linked pull requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Links {
    #[default]
    Any,
    With,
    Without,
}

/// An inclusive numeric filter. One min/max pair expresses all three
/// comparisons the UI offers.
#[derive(Debug, Clone, Copy, Default)]
pub struct NumericRange {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl NumericRange {
    fn set(&self) -> bool {
        self.min.is_some() || self.max.is_some()
    }
}

/// A half-open interval a drill-down selected.
#[derive(Debug, Clone, Copy)]
pub struct TimeWindow {
    pub from_ms: i64,
    pub to_ms: i64,
}

/// Bounds the OR-group a drill-down expands into, so a hand-written query
/// string cannot turn one request into a several-thousand-term predicate.
const MAX_DRILLDOWN_WINDOWS: usize = 512;

/// Page size bounds.
pub const DEFAULT_PAGE_SIZE: i64 = 50;
pub const MAX_PAGE_SIZE: i64 = 200;

/// Everything the sessions list can narrow, sort and page by. The default
/// selects every visible session, newest first.
#[derive(Debug, Clone, Default)]
pub struct SessionQuery {
    pub project: String,
    pub config_dir: String,
    pub search: String,
    pub favorites_only: bool,
    pub links: Links,
    pub permission_mode: String,
    pub model: String,
    pub messages: NumericRange,
    pub duration_minutes: NumericRange,
    pub tokens_in: NumericRange,
    pub tokens_out: NumericRange,
    pub cost: NumericRange,
    pub from: Option<GoTime>,
    pub to: Option<GoTime>,
    pub windows: Vec<TimeWindow>,
    pub sort: Sort,
    pub limit: i64,
    pub cursor: String,
}

impl SessionQuery {
    /// The clamped page size.
    pub fn page_size(&self) -> i64 {
        match self.limit {
            n if n <= 0 => DEFAULT_PAGE_SIZE,
            n if n > MAX_PAGE_SIZE => MAX_PAGE_SIZE,
            n => n,
        }
    }

    /// Parse the list's parameters from a raw query string.
    ///
    /// A malformed numeric bound is ignored rather than rejected: these arrive
    /// from number inputs a user is part-way through typing, and refusing the
    /// request would blank the list between keystrokes. A malformed *window* is
    /// an error, because the windows **are** the filter when a drill-down is
    /// active and silently dropping half of them would show a plausible-looking
    /// but wrong set of sessions.
    pub fn parse(raw_query: &str) -> Result<Self, String> {
        let params = parse_params(raw_query);
        let get = |k: &str| params.get(k).cloned().unwrap_or_default();

        let links = match get("links").as_str() {
            "" => Links::Any,
            "with" => Links::With,
            "without" => Links::Without,
            other => return Err(format!("invalid links filter {other:?}")),
        };

        let limit = match get("limit") {
            raw if raw.is_empty() => 0,
            raw => raw
                .parse::<i64>()
                .map_err(|_| format!("invalid limit {raw:?}"))?,
        };

        Ok(SessionQuery {
            project: get("project"),
            config_dir: get("config_dir"),
            search: get("q"),
            favorites_only: get("favorites") == "true",
            links,
            permission_mode: get("permission_mode"),
            model: get("model"),
            messages: numeric_range(&params, "messages"),
            duration_minutes: numeric_range(&params, "duration"),
            tokens_in: numeric_range(&params, "tokens_in"),
            tokens_out: numeric_range(&params, "tokens_out"),
            cost: numeric_range(&params, "cost"),
            from: optional_time(&get("from")),
            to: optional_time(&get("to")),
            windows: parse_windows(&get("windows"))?,
            sort: Sort::parse(&get("sort")),
            limit,
            cursor: get("cursor"),
        })
    }
}

/// Decode a query string into first-value-wins parameters, as Go's
/// `url.Values.Get` reads them.
fn parse_params(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (k, v) in form_urlencoded::parse(raw.as_bytes()) {
        out.entry(k.into_owned()).or_insert_with(|| v.into_owned());
    }
    out
}

fn numeric_range(params: &HashMap<String, String>, name: &str) -> NumericRange {
    NumericRange {
        min: optional_float(params.get(&format!("{name}_min"))),
        max: optional_float(params.get(&format!("{name}_max"))),
    }
}

/// `None` for an absent or unparseable value, which the filter reads as
/// "unbounded on that side" — distinct from zero, which is a real bound.
fn optional_float(raw: Option<&String>) -> Option<f64> {
    raw.filter(|s| !s.is_empty())?.parse::<f64>().ok()
}

fn optional_time(raw: &str) -> Option<GoTime> {
    if raw.is_empty() {
        return None;
    }
    GoTime::parse(raw).ok()
}

/// Decode the `fromMs-toMs,fromMs-toMs` form the analytics charts link with.
fn parse_windows(raw: &str) -> Result<Vec<TimeWindow>, String> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut windows = Vec::new();
    for part in raw.split(',') {
        let (from, to) = part
            .split_once('-')
            .ok_or_else(|| format!("invalid drill-down window {part:?}"))?;
        let from_ms = from
            .parse::<i64>()
            .map_err(|_| format!("invalid drill-down window start {from:?}"))?;
        let to_ms = to
            .parse::<i64>()
            .map_err(|_| format!("invalid drill-down window end {to:?}"))?;
        if to_ms <= from_ms {
            return Err(format!("drill-down window {part:?} ends before it starts"));
        }
        windows.push(TimeWindow { from_ms, to_ms });
    }
    Ok(windows)
}

/// An accumulated WHERE fragment with its bound arguments.
#[derive(Default)]
pub struct Filter {
    sql: Vec<String>,
    pub args: Vec<Value>,
}

/// A bound parameter. Timestamps are bound as the text the column holds, so the
/// comparison is against the same rendering the driver wrote.
#[derive(Debug, Clone)]
pub enum Value {
    Text(String),
    Real(f64),
    Int(i64),
}

impl rusqlite::ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        match self {
            Value::Text(s) => s.to_sql(),
            Value::Real(f) => f.to_sql(),
            Value::Int(i) => i.to_sql(),
        }
    }
}

impl Filter {
    pub fn add(&mut self, sql: impl Into<String>, args: Vec<Value>) {
        self.sql.push(sql.into());
        self.args.extend(args);
    }

    /// Copy another filter's terms and arguments in, for the facet queries
    /// that start from the visible-session scope and add one term.
    pub fn add_all(&mut self, other: &Filter) {
        self.sql.extend(other.sql.iter().cloned());
        self.args.extend(other.args.iter().cloned());
    }

    /// The rendered predicate, or empty when nothing was added. Always AND:
    /// every filter narrows.
    pub fn where_clause(&self) -> String {
        if self.sql.is_empty() {
            return String::new();
        }
        format!("\nWHERE {}", self.sql.join("\n  AND "))
    }
}

/// Turn a query into its WHERE clause, excluding pagination — the same
/// predicate serves the page, the facet aggregate and the total count, so the
/// counter in the toolbar and the rows below it cannot disagree.
pub fn build_filter(
    q: &SessionQuery,
    hidden_projects: &[String],
    indexed_dirs: &[String],
) -> Result<Filter, String> {
    let mut f = Filter::default();

    for p in hidden_projects {
        f.add("c.project_path != ?", vec![Value::Text(p.clone())]);
    }
    add_config_dir_scope(&mut f, indexed_dirs);

    if !q.project.is_empty() {
        f.add("c.project_path = ?", vec![Value::Text(q.project.clone())]);
    }
    if !q.config_dir.is_empty() {
        f.add("c.config_dir = ?", vec![Value::Text(q.config_dir.clone())]);
    }
    if q.favorites_only {
        f.add("c.is_favorite = 1", vec![]);
    }
    if !q.permission_mode.is_empty() {
        f.add(
            "c.permission_mode = ?",
            vec![Value::Text(q.permission_mode.clone())],
        );
    }
    if !q.model.is_empty() {
        f.add("c.model = ?", vec![Value::Text(q.model.clone())]);
    }
    add_search(&mut f, &q.search);
    add_links(&mut f, q.links);

    add_range(&mut f, SQL_MESSAGE_COUNT, q.messages, 1.0);
    // The duration filter is entered in minutes; the column stores milliseconds.
    add_range(&mut f, SQL_ACTIVE_DURATION_MS, q.duration_minutes, 60_000.0);
    add_range(&mut f, SQL_INPUT_TOKENS, q.tokens_in, 1.0);
    add_range(&mut f, SQL_OUTPUT_TOKENS, q.tokens_out, 1.0);
    add_range(&mut f, SQL_COST_USD, q.cost, 1.0);

    add_time_filter(&mut f, q)?;
    Ok(f)
}

/// Restrict results to the config dirs currently indexed.
///
/// Removing a dir hides its sessions rather than deleting them. The empty
/// string is always admitted: rows written before the column existed carry it
/// and belong to the default dir.
pub fn add_config_dir_scope(f: &mut Filter, dirs: &[String]) {
    if dirs.is_empty() {
        return;
    }
    let placeholders = vec!["?"; dirs.len()].join(", ");
    let args = dirs.iter().cloned().map(Value::Text).collect();
    f.add(
        format!("(c.config_dir = '' OR c.config_dir IN ({placeholders}))"),
        args,
    );
}

/// Match the same fields the client-side predicate did, case-insensitively.
///
/// `LOWER` on both sides rather than `COLLATE NOCASE`, because NOCASE is
/// ASCII-only in SQLite and project paths and titles are not.
fn add_search(f: &mut Filter, search: &str) {
    let q = search.trim().to_lowercase();
    if q.is_empty() {
        return;
    }
    // Bound, not interpolated, so % and _ typed by the user match literally.
    let pattern = format!("%{}%", escape_like(&q));
    f.add(
        r"(LOWER(c.session_id) LIKE ? ESCAPE '\'
    OR LOWER(c.preview) LIKE ? ESCAPE '\'
    OR LOWER(c.custom_title) LIKE ? ESCAPE '\'
    OR LOWER(c.native_title) LIKE ? ESCAPE '\'
    OR LOWER(c.ai_title) LIKE ? ESCAPE '\'
    OR LOWER(c.project_path) LIKE ? ESCAPE '\')",
        vec![Value::Text(pattern); 6],
    );
}

/// Neutralize LIKE's wildcards so a search for "100%" does not match
/// everything beginning with "100".
fn escape_like(s: &str) -> String {
    s.replace('\\', r"\\")
        .replace('%', r"\%")
        .replace('_', r"\_")
}

const PR_EXISTS: &str =
    "EXISTS (SELECT 1 FROM claude_session_pr p WHERE p.session_id = c.session_id)";

pub fn add_links(f: &mut Filter, links: Links) {
    match links {
        Links::With => f.add(PR_EXISTS, vec![]),
        Links::Without => f.add(format!("NOT {PR_EXISTS}"), vec![]),
        Links::Any => {}
    }
}

/// An inclusive bound on `expr`. `scale` converts the filter's unit into the
/// column's, so the bound moves rather than the column — keeping the expression
/// indexable and the comparison exact.
fn add_range(f: &mut Filter, expr: &str, r: NumericRange, scale: f64) {
    if !r.set() {
        return;
    }
    if let Some(min) = r.min {
        f.add(format!("{expr} >= ?"), vec![Value::Real(min * scale)]);
    }
    if let Some(max) = r.max {
        f.add(format!("{expr} <= ?"), vec![Value::Real(max * scale)]);
    }
}

/// Apply either the drill-down windows or the from/to range — never both,
/// matching the UI, where an active drill-down replaces the preset.
fn add_time_filter(f: &mut Filter, q: &SessionQuery) -> Result<(), String> {
    if !q.windows.is_empty() {
        if q.windows.len() > MAX_DRILLDOWN_WINDOWS {
            return Err(format!(
                "claudesessions: {} drill-down windows exceeds the {} limit",
                q.windows.len(),
                MAX_DRILLDOWN_WINDOWS
            ));
        }
        let mut terms = Vec::with_capacity(q.windows.len());
        let mut args = Vec::with_capacity(q.windows.len() * 2);
        for w in &q.windows {
            // Half-open, matching the client's overlapsAnyWindow: a session
            // starting exactly at a window's end is in the next window.
            terms.push("(c.start_time < ? AND c.last_activity >= ?)");
            args.push(Value::Text(sql_time_from_millis(w.to_ms)));
            args.push(Value::Text(sql_time_from_millis(w.from_ms)));
        }
        f.add(format!("({})", terms.join("\n    OR ")), args);
        return Ok(());
    }
    if let Some(to) = q.to {
        f.add("c.start_time <= ?", vec![Value::Text(sql_time(to))]);
    }
    if let Some(from) = q.from {
        f.add("c.last_activity >= ?", vec![Value::Text(sql_time(from))]);
    }
    Ok(())
}

/// Render a timestamp the way the driver rendered the stored one.
///
/// The DATETIME columns hold Go's `time.Time.String()` text, and both the
/// `ORDER BY` and every time predicate compare it **as text** — an ordering
/// that matches chronological order only because every value is UTC with its
/// trailing zeros trimmed. A bound formatted any other way would compare
/// against a different string shape and silently mis-filter.
pub fn sql_time(t: GoTime) -> String {
    crate::native::gotime::to_go_string_utc(t)
}

fn sql_time_from_millis(ms: i64) -> String {
    crate::native::gotime::go_string_from_millis(ms)
}

/// A keyset position: the sort value of the last row returned, plus its session
/// ID as the tiebreak.
///
/// Keyset rather than OFFSET, because OFFSET makes the database walk and
/// discard every skipped row, and because a scan completing mid-scroll would
/// shift every subsequent page.
///
/// Field order is the wire order Go marshals, and the short names are Go's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    #[serde(rename = "s")]
    pub sort: String,
    /// The sort column's value: RFC3339Nano for the time sort, a decimal
    /// otherwise. A string either way, so one encoding covers both.
    #[serde(rename = "v")]
    pub value: String,
    pub id: String,
}

/// Returned when a cursor was minted under a different sort than the request
/// asks for. Continuing would page through one ordering using another's
/// position, silently skipping and repeating rows.
pub const ERR_CURSOR_MISMATCH: &str = "claudesessions: cursor does not match the requested sort";

impl Cursor {
    pub fn encode(&self) -> String {
        match gojson::to_vec_marshal(self) {
            Ok(bytes) => base64_url_nopad(&bytes),
            Err(_) => String::new(),
        }
    }

    pub fn decode(raw: &str, want: Sort) -> Result<Option<Cursor>, String> {
        if raw.is_empty() {
            return Ok(None);
        }
        let bytes = base64_url_nopad_decode(raw)
            .ok_or_else(|| "claudesessions: malformed cursor".to_string())?;
        let c: Cursor = serde_json::from_slice(&bytes)
            .map_err(|e| format!("claudesessions: malformed cursor: {e}"))?;
        if c.sort != want.as_str() {
            return Err(ERR_CURSOR_MISMATCH.to_string());
        }
        Ok(Some(c))
    }

    /// The argument the keyset predicate compares against.
    pub fn bind(&self, is_time: bool) -> Result<Value, String> {
        if is_time {
            let t = GoTime::parse(&self.value)
                .map_err(|e| format!("claudesessions: malformed cursor timestamp: {e}"))?;
            return Ok(Value::Text(sql_time(t)));
        }
        self.value
            .parse::<f64>()
            .map(Value::Real)
            .map_err(|e| format!("claudesessions: malformed cursor value: {e}"))
    }
}

/// Render a row's sort value for the next cursor, in the spelling Go uses:
/// `strconv.FormatFloat(_, 'g', -1, 64)` for the cost, plain integers for the
/// counts, and RFC3339Nano for the timestamp.
pub fn cursor_value(s: &super::summary::SessionSummary, sort: Sort) -> String {
    match sort {
        Sort::Cost => gojson::format_g(s.total_cost_usd()),
        Sort::Tokens => s.total_conversation_tokens().to_string(),
        Sort::Duration => s.total_active_duration_ms().to_string(),
        Sort::Messages => s.message_count.to_string(),
        Sort::Recent => s.last_activity.to_rfc3339_nano(),
    }
}

/// base64 RawURLEncoding: URL alphabet, no padding.
fn base64_url_nopad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let indices = [n >> 18 & 63, n >> 12 & 63, n >> 6 & 63, n & 63];
        for (i, idx) in indices.iter().enumerate() {
            if i <= chunk.len() {
                out.push(ALPHABET[*idx as usize] as char);
            }
        }
    }
    out
}

fn base64_url_nopad_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for chunk in text.as_bytes().chunks(4) {
        if chunk.len() == 1 {
            return None;
        }
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= value(*c)? << (18 - 6 * i);
        }
        for i in 0..chunk.len() - 1 {
            out.push(((n >> (16 - 8 * i)) & 0xFF) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_in_gos_raw_url_alphabet() {
        for payload in [
            &b""[..],
            b"a",
            b"ab",
            b"abc",
            b"abcd",
            br#"{"s":"cost","v":"36.3","id":"abc-123"}"#,
            &[0xff, 0xfe, 0xfd][..],
        ] {
            let encoded = base64_url_nopad(payload);
            assert!(!encoded.contains('='), "padding in {encoded}");
            assert_eq!(
                base64_url_nopad_decode(&encoded).as_deref(),
                Some(payload),
                "round trip of {payload:?}"
            );
        }
    }

    #[test]
    fn a_cursor_from_another_sort_is_rejected() {
        let c = Cursor {
            sort: "cost".into(),
            value: "36.3".into(),
            id: "s1".into(),
        };
        let encoded = c.encode();
        assert!(Cursor::decode(&encoded, Sort::Cost)
            .expect("decode")
            .is_some());
        let err = Cursor::decode(&encoded, Sort::Recent).expect_err("mismatch");
        assert_eq!(err, ERR_CURSOR_MISMATCH);
    }

    #[test]
    fn an_empty_cursor_is_the_first_page_not_an_error() {
        assert!(Cursor::decode("", Sort::Recent).expect("decode").is_none());
    }

    #[test]
    fn the_cursor_encodes_the_fields_go_encodes_in_gos_order() {
        let c = Cursor {
            sort: "recent".into(),
            value: "2026-08-13T10:00:00Z".into(),
            id: "s1".into(),
        };
        let decoded = base64_url_nopad_decode(&c.encode()).expect("decode");
        assert_eq!(
            String::from_utf8(decoded).expect("utf-8"),
            r#"{"s":"recent","v":"2026-08-13T10:00:00Z","id":"s1"}"#
        );
    }

    #[test]
    fn an_unknown_sort_falls_back_to_recent_rather_than_erroring() {
        assert_eq!(Sort::parse("nonsense"), Sort::Recent);
        assert_eq!(Sort::parse(""), Sort::Recent);
        assert_eq!(Sort::parse("cost"), Sort::Cost);
    }

    #[test]
    fn query_parsing_matches_the_go_handler() {
        let q = SessionQuery::parse(
            "project=%2Fhome%2Fu%2Fp&q=hello+world&favorites=true&links=with\
             &permission_mode=bypass&model=opus&cost_min=1.5&cost_max=40\
             &duration_min=5&sort=cost&limit=25&cursor=abc",
        )
        .expect("parse");

        assert_eq!(q.project, "/home/u/p");
        assert_eq!(q.search, "hello world");
        assert!(q.favorites_only);
        assert_eq!(q.links, Links::With);
        assert_eq!(q.permission_mode, "bypass");
        assert_eq!(q.model, "opus");
        assert_eq!(q.cost.min, Some(1.5));
        assert_eq!(q.cost.max, Some(40.0));
        assert_eq!(q.duration_minutes.min, Some(5.0));
        assert_eq!(q.sort, Sort::Cost);
        assert_eq!(q.limit, 25);
        assert_eq!(q.cursor, "abc");
    }

    #[test]
    fn a_half_typed_number_is_ignored_but_a_bad_window_is_an_error() {
        let q = SessionQuery::parse("cost_min=&cost_max=notanumber").expect("parse");
        assert_eq!(q.cost.min, None);
        assert_eq!(q.cost.max, None);

        assert!(SessionQuery::parse("windows=123-456").is_ok());
        assert!(SessionQuery::parse("windows=nonsense").is_err());
        assert!(SessionQuery::parse("windows=abc-456").is_err());
    }

    #[test]
    fn an_invalid_links_filter_is_rejected() {
        assert!(SessionQuery::parse("links=maybe").is_err());
        assert!(SessionQuery::parse("links=").is_ok());
    }

    #[test]
    fn page_size_is_clamped_not_refused() {
        let mut q = SessionQuery::default();
        assert_eq!(q.page_size(), DEFAULT_PAGE_SIZE);
        q.limit = 1000;
        assert_eq!(q.page_size(), MAX_PAGE_SIZE);
        q.limit = 25;
        assert_eq!(q.page_size(), 25);
    }

    #[test]
    fn search_wildcards_are_escaped_so_they_match_literally() {
        assert_eq!(escape_like("100%"), r"100\%");
        assert_eq!(escape_like("a_b"), r"a\_b");
        assert_eq!(escape_like(r"back\slash"), r"back\\slash");
    }
}
