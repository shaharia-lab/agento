//! The sessions list's filter, sort and page position.
//!
//! Mirrors `internal/claudesessions/session_query.go` and the cursor half of
//! `session_page.go`, plus `sessionQueryFromRequest` in
//! `internal/api/claude_sessions.go`.
//!
//! The metric expressions below are the load-bearing part: a row showing $36.30
//! must not be hidden by "cost at most $40". That used to be a three-language
//! agreement — this SQL, `session_query.go` and
//! `frontend/src/lib/sessionMetrics.ts`, all asserting the same fixture. #391
//! deleted the other two, so this is the only implementation left and
//! `parity/session_metric_vectors.json` is a frozen record of the figures Go
//! produced. `tests_db.rs` is its one reader.

use std::collections::HashMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::native::gojson;
use crate::native::gotime::GoTime;
use crate::native::search;

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

/// The relevance sort's key, over the FTS join `page::list_page` adds (#437).
///
/// **Negated, so that "best first" is `DESC` like every other sort.** SQLite's
/// `bm25()` is already negative — more negative is a better match — so `-rank`
/// is non-negative and larger is better, which is exactly the direction the
/// existing keyset predicate and `ORDER BY … DESC` are written for. Negating
/// here rather than special-casing the comparison is what keeps relevance one
/// more arm of the machinery instead of a second ordering to keep in step.
///
/// The `COALESCE` is the LIKE half. A search is an `OR` (#436): a row can match
/// on its session id, its project path or a title and have no index hit at all,
/// which the `LEFT JOIN` leaves NULL. Left as NULL those rows sort last under
/// `DESC` — correct — but `NULL < ?` is NULL, so the keyset predicate would
/// exclude every one of them from the second page onwards and they would
/// silently vanish mid-scroll. [`RELEVANCE_UNRANKED`] is strictly below every
/// real value because `-bm25` is never negative.
pub const SQL_RELEVANCE: &str = "COALESCE(-fts.rank, -1.0)";

/// The sort value a row with no index hit takes, and the whole sort key when the
/// index cannot answer at all — see [`SQL_RELEVANCE`].
pub const RELEVANCE_UNRANKED: f64 = -1.0;

/// The relevance key when there is no FTS join to read: every row unranked.
///
/// `usable_fts_query` degrades to the metadata-only clause on a database with no
/// `session_search` table or an expression FTS5 refuses, and then there is no
/// `fts` alias for [`SQL_RELEVANCE`] to name. Every row ties at
/// [`RELEVANCE_UNRANKED`], the `(session_id, project_path)` tiebreak makes the
/// order total anyway, and the cursor round-trips through the same constant —
/// so a degraded relevance sort pages correctly rather than erroring.
pub const SQL_RELEVANCE_UNRANKED: &str = "-1.0";

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
    /// Best match first, over the weighted bm25 of the content index (#437).
    /// Only meaningful with a search term — see [`resolve_sort`].
    Relevance,
}

impl Sort {
    /// Parse the `sort` parameter. An unknown value falls back to `Recent`
    /// rather than erroring: the list is a read-only view, and a stale bookmark
    /// is better rendered in the default order than refused.
    ///
    /// `relevance` without a search term is one of those unknown values — the
    /// rule is applied by [`resolve_sort`], which is the only caller that knows
    /// whether `q` is set.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "cost" => Sort::Cost,
            "tokens" => Sort::Tokens,
            "duration" => Sort::Duration,
            "messages" => Sort::Messages,
            "relevance" => Sort::Relevance,
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
            Sort::Relevance => "relevance",
        }
    }

    /// The SQL the sort orders and pages on, and whether its values are
    /// timestamps (which page through a time cursor rather than a float).
    ///
    /// The relevance arm names the `fts` alias, which only `page::list_page`
    /// joins in — and only when the index can answer. That is why it takes the
    /// expression from [`page_sort_expr`] rather than from here.
    pub fn expr(self) -> (&'static str, bool) {
        match self {
            Sort::Cost => (SQL_COST_USD, false),
            Sort::Tokens => (SQL_TOKENS, false),
            Sort::Duration => (SQL_ACTIVE_DURATION_MS, false),
            Sort::Messages => (SQL_MESSAGE_COUNT, false),
            Sort::Recent => ("c.last_activity", true),
            Sort::Relevance => (SQL_RELEVANCE, false),
        }
    }
}

/// The sort a page actually orders by, given whether the FTS join is present.
///
/// One function rather than a branch at the call site, because the *cursor* has
/// to agree with it: a degraded relevance page mints
/// [`RELEVANCE_UNRANKED`] as its value and reads it back through the same
/// constant, so the two spellings cannot drift into paging one ordering with
/// another's position.
pub fn page_sort_expr(sort: Sort, ranked: bool) -> (&'static str, bool) {
    match (sort, ranked) {
        (Sort::Relevance, false) => (SQL_RELEVANCE_UNRANKED, false),
        (other, _) => other.expr(),
    }
}

/// Resolve the `sort` parameter against the search term.
///
/// Two rules, and both are the existing fallback rather than new behaviour:
///
/// * **`q` set with no explicit `sort` is `relevance`.** Somebody who typed a
///   search wants the best match first; somebody who did not has no ranking to
///   sort by. An explicit `sort` always wins, so a user who picked "most recent"
///   keeps it while typing.
/// * **`relevance` with no `q` is `recent`**, which is exactly what
///   [`Sort::parse`] already does with any value it does not recognise. Without
///   a search term there is no `MATCH`, so there is no rank — and a stale
///   bookmark carrying `sort=relevance` renders in the default order rather
///   than being refused.
///
/// The term is trimmed because [`add_search`] trims it: `q=%20` adds no clause
/// at all, so it must not select an ordering that depends on one.
pub fn resolve_sort(raw_sort: &str, search: &str) -> Sort {
    let searching = !search.trim().is_empty();
    if raw_sort.is_empty() {
        return if searching {
            Sort::Relevance
        } else {
            Sort::Recent
        };
    }
    match Sort::parse(raw_sort) {
        Sort::Relevance if !searching => Sort::Recent,
        other => other,
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
        let search = get("q");

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
            sort: resolve_sort(&get("sort"), &search),
            search,
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
    /// The FTS5 expression the search clause matched on, when there was one and
    /// the index could answer it (#437).
    ///
    /// Carried out of [`add_search`] rather than rebuilt by the caller, because
    /// deciding it costs a real prepare-and-step against the index — see
    /// [`usable_fts_query`] — and `page::list_page` needs the same answer twice
    /// more: once for the ranked join and once for the snippet read. Asking
    /// again would pay that probe three times per page and, worse, could answer
    /// differently if the index changed underneath, leaving a join whose rows
    /// the filter did not select.
    ///
    /// `None` means the metadata-only clause: no search term, nothing left after
    /// tokenizing, no `session_search` table, or an expression FTS5 refused.
    pub fts: Option<String>,
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
    ///
    /// `fts` is deliberately **not** copied: it is not a predicate but a handle
    /// for the ranked join, every caller here starts from a filter that has no
    /// search clause, and a copy would advertise a join the copy's SQL does not
    /// select through.
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
///
/// `conn` is read only to decide whether the search term can go through the
/// full-text index — see [`add_search`]. Every other term is a pure function of
/// `q`.
pub fn build_filter(
    conn: &Connection,
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
    add_search(conn, &mut f, &q.search);
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

/// The six metadata columns the list has always matched, case-insensitively.
///
/// `LOWER` on both sides rather than `COLLATE NOCASE`, because NOCASE is
/// ASCII-only in SQLite and project paths and titles are not. Six `?`, in this
/// order, all bound to the same pattern.
const LIKE_COLUMNS: &str = r"LOWER(c.session_id) LIKE ? ESCAPE '\'
    OR LOWER(c.preview) LIKE ? ESCAPE '\'
    OR LOWER(c.custom_title) LIKE ? ESCAPE '\'
    OR LOWER(c.native_title) LIKE ? ESCAPE '\'
    OR LOWER(c.ai_title) LIKE ? ESCAPE '\'
    OR LOWER(c.project_path) LIKE ? ESCAPE '\'";

/// Match the search term against the full-text index **or** the six metadata
/// columns.
///
/// One `Filter` clause, deliberately: facets, the config-dir scope, every
/// numeric and time bound and the keyset predicate all compose through the same
/// funnel, so adding a term here is the whole change and nothing downstream has
/// to know a content index exists.
///
/// The two halves are OR'd rather than one replacing the other, because they
/// answer different questions and neither is a superset:
///
/// * the index holds a session's **content**, which the cache row never has —
///   only a 120-char `preview` is stored;
/// * the LIKE clause matches the **session id**, the **project path** and the
///   titles, which are UNINDEXED in `session_search` and so can never be a
///   content hit — and it matches sessions the worker has not indexed yet, which
///   on a fresh install is all of them.
///
/// # The FTS half degrades rather than failing
///
/// A user's text reaches FTS5's own query grammar, where `-`, `OR`, `NOT`, `^`,
/// `*`, `:` and `"` are operators. [`build_fts_query`] neutralizes them by
/// quoting each token, which is a construction rule and not a sanitization one —
/// but "the builder can never produce a syntax error" is a claim about a parser
/// this code does not own, so it is *checked* rather than trusted: the built
/// expression is run against the index once, and anything that errors (a missing
/// table on a database that has not migrated, a query FTS5 refuses) falls back to
/// the LIKE-only clause this route answered with before #436. Search must never
/// answer 500 because of a character somebody typed.
fn add_search(conn: &Connection, f: &mut Filter, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    // Bound, not interpolated, so % and _ typed by the user match literally.
    let pattern = format!("%{}%", escape_like(&trimmed.to_lowercase()));
    let like_args = vec![Value::Text(pattern); 6];

    let Some(fts) = usable_fts_query(conn, trimmed) else {
        f.add(format!("({LIKE_COLUMNS})"), like_args);
        return;
    };

    // The subquery is re-projected to two columns: `match_sql()` also selects
    // `rank`, which #437 orders by and a row-value `IN` cannot accept — so the
    // rank reaches the ORDER BY through a second, joined copy of the same
    // subquery instead, over the expression recorded here.
    let mut args = vec![Value::Text(fts.clone())];
    args.extend(like_args);
    f.fts = Some(fts);
    f.add(
        format!(
            "((c.session_id, c.project_path) IN (
      SELECT session_id, project_path FROM (
{}
      ))
    OR {LIKE_COLUMNS})",
            search::match_sql()
        ),
        args,
    );
}

/// The FTS expression for `search`, or `None` when the index cannot answer it.
///
/// The probe is a real prepare-and-step rather than a `SELECT 1 FROM
/// session_search LIMIT 0` existence check, because the two failures it has to
/// catch surface at different points: a missing table fails the *prepare*, while
/// an expression FTS5 refuses fails on the *first step* — and an unstepped
/// statement never parses the MATCH argument at all — including against an
/// **empty** index, which is what a fresh install has and where a probe that
/// short-circuited would accept anything
/// (`the_probe_still_rejects_a_bad_expression_against_an_empty_index`).
///
/// The cost is one extra inverted-index lookup per `build_filter`, i.e. one per
/// call of `GET /api/claude-sessions` and one per `…/facets` — the same lookup
/// the clause itself then performs, stopped at the first row. Worth knowing
/// before #437, which composes this as a ranked join and may want to fold the
/// two together.
///
/// The two failures are logged differently on purpose, and neither line carries
/// the user's search text — `proxy.rs` keeps query strings out of this file, and
/// FTS5's own syntax-error message quotes the text it choked on.
fn usable_fts_query(conn: &Connection, text: &str) -> Option<String> {
    let query = build_fts_query(text);
    if query.is_empty() {
        return None;
    }
    let mut stmt =
        match conn.prepare("SELECT 1 FROM session_search WHERE session_search MATCH ?1 LIMIT 1") {
            Ok(stmt) => stmt,
            // Debug, and with the cause: on a database that has not migrated
            // this is the ordinary answer for every keystroke, and a warning per
            // keystroke is how a log stops being read. The message names the
            // missing table, nothing the user typed.
            Err(e) => {
                log::debug!("claude sessions: no search index, matching metadata only: {e}");
                return None;
            }
        };
    if stmt.exists(rusqlite::params![&query]).is_err() {
        // Warn, and *without* the cause: `build_fts_query` is meant to make this
        // unreachable, so reaching it is a defect worth seeing rather than
        // routine degradation — and the one thing rusqlite would tell us here is
        // the FTS5 message, which quotes the search term back.
        log::warn!(
            "claude sessions: the search index refused a built query, matching metadata only"
        );
        return None;
    }
    Some(query)
}

/// How many *alphanumeric* characters a trailing token needs before it is
/// extended into a prefix term.
///
/// `"a"*` matches roughly every session on the machine, so the first keystroke
/// of a query would flash the whole corpus in and out. Counted over the
/// alphanumerics rather than the whole token because those are what `unicode61`
/// keeps: on the raw length, `-a` would clear the bar and still produce `a*`.
const MIN_PREFIX_LEN: usize = 2;

/// Turn what a user typed into an FTS5 query that can only ever be a conjunction
/// of literal terms.
///
/// **Safe by construction, not by sanitization.** Nothing typed is ever passed
/// through: the input is split into tokens and each token is *re-emitted* inside
/// a double-quoted FTS5 string, where the grammar has no operators at all. So
/// `-foo`, `a OR b`, `NOT`, `^x`, `a*b`, `(a)` and `a:b` are terms rather than
/// syntax, and the function has no notion of "a character to escape" that a
/// future FTS5 could add to.
///
/// The rules, in the order a user meets them:
///
/// * **Whitespace separates tokens, and tokens are AND'd** — FTS5's implicit
///   operator between two phrases. `fix auth bug` finds a session containing all
///   three words in any order, which is what the old single `LIKE '%fix auth
///   bug%'` substring could not do.
/// * **A user-typed `"…"` span is kept as one phrase**, so quoting is the one
///   piece of query syntax that still means what people expect it to. An
///   unterminated quote is a phrase too — it is what half-typing one looks like.
/// * **The final token becomes a prefix term** (`"efficien"*`) so the list
///   narrows as the user types, unless the user closed a quote — which says the
///   word is finished — or the token carries fewer than [`MIN_PREFIX_LEN`]
///   alphanumerics.
/// * **Control characters are separators.** They are separators to `unicode61`
///   anyway, so this changes no result — but a NUL reaching FTS5's parser is an
///   error (`unterminated string`), and `%00` in a query string decodes to one.
/// * **A token with no alphanumeric character is dropped.** `unicode61` indexes
///   alphanumerics and treats everything else as a separator, so `"-"` is a
///   phrase of *zero* terms — which matches no document, and AND'd with the rest
///   empties the whole result. Without this, one stray dash in `fix auth -` costs
///   the user every hit. Dropping can only widen the FTS half, and it discards
///   nothing that could ever have matched; the LIKE half still sees the raw
///   text, so a search for `---` is unaffected.
///
/// Returns an empty string when nothing survives — an input of only separators.
/// An empty MATCH argument is itself a syntax error, so the caller reads that as
/// "no FTS half" rather than passing it on.
pub fn build_fts_query(text: &str) -> String {
    // (text, the user closed a quote around it)
    let mut tokens: Vec<(String, bool)> = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;

    for raw in text.chars() {
        let ch = if raw.is_control() { ' ' } else { raw };
        if in_quote {
            if ch == '"' {
                tokens.push((std::mem::take(&mut buf), true));
                in_quote = false;
            } else {
                buf.push(ch);
            }
        } else if ch == '"' {
            tokens.push((std::mem::take(&mut buf), false));
            in_quote = true;
        } else if ch.is_whitespace() {
            tokens.push((std::mem::take(&mut buf), false));
        } else {
            buf.push(ch);
        }
    }
    tokens.push((buf, false));
    tokens.retain(|(text, _)| text.chars().any(char::is_alphanumeric));

    let last = tokens.len().saturating_sub(1);
    let mut out = String::new();
    for (i, (text, closed)) in tokens.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push('"');
        // A `"` cannot reach here — one opens or closes a span — but the escape
        // is what makes the emitter safe on its own terms rather than safe
        // because of how the loop above happens to be written.
        out.push_str(&text.replace('"', "\"\""));
        out.push('"');
        if i == last
            && !closed
            && text.chars().filter(|c| c.is_alphanumeric()).count() >= MIN_PREFIX_LEN
        {
            out.push('*');
        }
    }
    out
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

/// A keyset position: the sort value of the last row returned, plus the row's
/// primary key as the tiebreak.
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
    /// The second half of the tiebreak. `claude_session_cache` is keyed on
    /// `(session_id, project_path)`, so a session id is not unique: one id
    /// legitimately yields two rows when the same session was seen under two
    /// project paths. On the id alone the pair is indistinguishable to the
    /// keyset predicate and the second row is skipped by every page, while
    /// `facets` still counts it.
    ///
    /// A cursor minted before this field existed carries no `p`, so it decodes
    /// empty (`#[serde(default)]`) and `c.project_path < ''` is never true —
    /// such a cursor pages exactly as it used to rather than losing a row
    /// mid-scroll.
    #[serde(rename = "p", default)]
    pub project: String,
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
/// `relevance` is the one value that is not a function of the row: it is the
/// negated bm25 the page's own query computed, so the caller reads it off the
/// result set and hands it in. Rows on a non-relevance page pass
/// [`RELEVANCE_UNRANKED`], which is never looked at.
pub fn cursor_value(s: &super::summary::SessionSummary, sort: Sort, relevance: f64) -> String {
    match sort {
        Sort::Cost => gojson::format_g(s.total_cost_usd()),
        Sort::Tokens => s.total_conversation_tokens().to_string(),
        Sort::Duration => s.total_active_duration_ms().to_string(),
        Sort::Messages => s.message_count.to_string(),
        Sort::Recent => s.last_activity.to_rfc3339_nano(),
        Sort::Relevance => gojson::format_g(relevance),
    }
}

/// base64 RawURLEncoding: URL alphabet, no padding.
pub(super) fn base64_url_nopad(bytes: &[u8]) -> String {
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
            project: "/home/u/p".into(),
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
            project: "/home/u/p".into(),
        };
        let decoded = base64_url_nopad_decode(&c.encode()).expect("decode");
        assert_eq!(
            String::from_utf8(decoded).expect("utf-8"),
            r#"{"s":"recent","v":"2026-08-13T10:00:00Z","id":"s1","p":"/home/u/p"}"#
        );
    }

    /// Go leaves a field absent from the JSON at its zero value and returns no
    /// error, so a cursor minted before `p` existed must decode rather than be
    /// refused — an over-reject here would break every scroll in flight across
    /// an upgrade.
    #[test]
    fn a_cursor_without_the_project_field_decodes_with_it_empty() {
        let raw = base64_url_nopad(br#"{"s":"cost","v":"36.3","id":"s1"}"#);
        let c = Cursor::decode(&raw, Sort::Cost)
            .expect("decode")
            .expect("some");
        assert_eq!(c.id, "s1");
        assert_eq!(c.project, "");
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

    /// Whitespace separates, tokens are AND'd, and the last one is a prefix.
    #[test]
    fn an_fts_query_ands_one_quoted_term_per_word() {
        assert_eq!(build_fts_query("fix auth bug"), r#""fix" "auth" "bug"*"#);
        assert_eq!(build_fts_query("  spaced   out  "), r#""spaced" "out"*"#);
        assert_eq!(build_fts_query("single"), r#""single"*"#);
    }

    /// Every character FTS5 treats as syntax is re-emitted inside a quoted
    /// string, so it reaches the tokenizer as text.
    ///
    /// The table is the acceptance criterion written out: `- * ^ ( ) : "` and
    /// the bare boolean keywords. What each *matches* is the tokenizer's
    /// business — what matters here is that none of them can still be an
    /// operator, and none of them can end a string early.
    #[test]
    fn every_fts_metacharacter_is_emitted_as_a_literal_term() {
        for (input, want) in [
            ("-foo", r#""-foo"*"#),
            ("foo -bar", r#""foo" "-bar"*"#),
            ("a OR b", r#""a" "OR" "b""#),
            ("NOT auth", r#""NOT" "auth"*"#),
            ("a AND b", r#""a" "AND" "b""#),
            ("^start", r#""^start"*"#),
            ("wild*card", r#""wild*card"*"#),
            ("(group)", r#""(group)"*"#),
            ("col:val", r#""col:val"*"#),
            // `b)` is two characters and one term, so it stays below the
            // prefix bar — see `MIN_PREFIX_LEN`.
            ("NEAR(a b)", r#""NEAR(a" "b)""#),
            // A lone quote opens a span that never closes and holds nothing.
            ("\"", ""),
            ("a\"", r#""a""#),
            // A token of pure punctuation is a phrase of zero terms, which
            // would AND the whole query down to nothing.
            ("auth *", r#""auth"*"#),
            ("-", ""),
            ("fix auth ---", r#""fix" "auth"*"#),
        ] {
            assert_eq!(build_fts_query(input), want, "input {input:?}");
        }
    }

    /// A user-typed phrase is the one piece of query syntax kept, because it is
    /// the one people mean.
    #[test]
    fn a_user_typed_phrase_stays_one_phrase() {
        assert_eq!(
            build_fts_query("\"auth failed\" retry"),
            r#""auth failed" "retry"*"#
        );
        // Closing the quote says the word is finished, so no prefix is added.
        assert_eq!(build_fts_query("\"auth failed\""), r#""auth failed""#);
        // Half-typed, though, is still as-you-type.
        assert_eq!(build_fts_query("\"auth fail"), r#""auth fail"*"#);
        // A doubled quote in the *input* is not read as FTS5's escape — every
        // `"` opens or closes a span, so this is two phrases. That is the point
        // of quoting on the way out rather than interpreting on the way in:
        // there is no input spelling that reaches the grammar.
        assert_eq!(build_fts_query("\"say \"\"hi\""), r#""say " "hi""#);
    }

    /// `"a"*` matches most of a corpus, so the first keystroke must not flash
    /// every session into the list.
    #[test]
    fn a_one_character_final_token_is_not_extended_into_a_prefix() {
        assert_eq!(build_fts_query("a"), r#""a""#);
        assert_eq!(build_fts_query("ab"), r#""ab"*"#);
        assert_eq!(build_fts_query("auth a"), r#""auth" "a""#);
        // The bar is the alphanumerics, not the token's length: `-a` is two
        // characters and one term, and `"-a"*` is the `a*` this rule exists to
        // prevent.
        assert_eq!(build_fts_query("-a"), r#""-a""#);
        assert_eq!(build_fts_query("-ab"), r#""-ab"*"#);
    }

    /// A NUL reaching FTS5's parser is an error, and `%00` in a query string
    /// decodes to one — so control characters are separators here, which is
    /// what they already are to `unicode61`.
    #[test]
    fn control_characters_are_separators_rather_than_text() {
        assert_eq!(build_fts_query("fix\u{0}auth"), r#""fix" "auth"*"#);
        assert_eq!(build_fts_query("line\nbreak"), r#""line" "break"*"#);
        assert_eq!(build_fts_query("\u{0}"), "");
        assert_eq!(build_fts_query("tab\tsep"), r#""tab" "sep"*"#);
    }

    /// Nothing left to search for is not an empty query — an empty MATCH
    /// argument is a syntax error, so it has to read as "no FTS half".
    #[test]
    fn an_input_of_only_separators_produces_no_expression() {
        for input in ["\"\"", "\"\"\"\"", " ", "\u{0}\u{1}", "-", "***", "()", ":"] {
            assert_eq!(build_fts_query(input), "", "input {input:?}");
        }
    }
}
