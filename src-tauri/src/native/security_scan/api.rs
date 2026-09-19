//! The Credentials Checker's `/api` surface (#604, design doc §5.1, §6): the
//! findings, the two verdicts a user can give one, the whitelist, and whether
//! the checker is on.
//!
//! Every read and write goes through [`super::store`], which owns these tables
//! and the two invariants its header states; this module only decodes, checks
//! and encodes.
//!
//! **No Go counterpart**, like `native::security`: the routes are recorded in
//! `parity/desktop_routes.json` through [`ROUTES`], asserted there as set
//! equality, and every stored `DATETIME` goes out as RFC 3339 through
//! `security::tokens::wire_time`.
//!
//! **No `match_hash` is ever on the wire** — not on a finding, not on a
//! whitelist entry. It is a plain SHA-256 of a leaked secret: not the secret,
//! but a value that confirms a guess of it. Whitelisting one value goes through
//! `POST …/findings/{id}/whitelist`, which reads the hash server-side, and a
//! value entry is listed by its `kind`, `reason` and date. The direct
//! `POST /api/security-scan/whitelist` still accepts a `match_hash` (the
//! issue's value-level entry), for a caller that computed one itself.
//!
//! **One entry per target.** Both whitelist writes go through
//! `store::ensure_whitelist_entry`, so repeating one answers the existing entry
//! with `200` instead of `201` and a second row; a rule-level entry must name a
//! rule in `rules::compiled()`, or it would be stored and never match.
//!
//! `GET /api/security-scan/status` reports `enabled` from the stored setting and
//! `running` from the worker itself: the two differ when the worker failed to
//! spawn, and during the moment between a settings save and its `sync`.

use axum::http::{Method, StatusCode};

use super::store::{self, WhitelistTarget};
use crate::native::security::tokens::wire_time;
use crate::native::writes::{self, WriteError};
use crate::native::{db, gojson, migrate, Answer, Ctx, Request};

/// Every route this module claims: the single definition [`claims`] matches
/// against, asserted as set equality with `parity/desktop_routes.json`.
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/security-scan/findings"),
    ("POST", "/api/security-scan/findings/{id}/whitelist"),
    ("POST", "/api/security-scan/findings/{id}/false-positive"),
    ("GET", "/api/security-scan/whitelist"),
    ("POST", "/api/security-scan/whitelist"),
    ("DELETE", "/api/security-scan/whitelist/{id}"),
    ("GET", "/api/security-scan/status"),
];

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: crate::native::Endpoint = crate::native::Endpoint {
    name: "security-scan",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    ROUTES
        .iter()
        .any(|(m, pattern)| method.as_str() == *m && path_matches(pattern, path))
}

/// A chi-style pattern against a concrete path; `{name}` matches exactly one
/// non-empty segment, so a trailing slash is a different route.
fn path_matches(pattern: &str, path: &str) -> bool {
    let mut want = pattern.split('/');
    let mut have = path.split('/');
    loop {
        match (want.next(), have.next()) {
            (None, None) => return true,
            (Some(w), Some(h)) => {
                let ok = if w.starts_with('{') && w.ends_with('}') {
                    !h.is_empty()
                } else {
                    w == h
                };
                if !ok {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// The `{id}` segment of a claimed `…/{id}` or `…/{id}/<action>` path, as the
/// raw text — parsed by the handler, so a non-numeric id is a 404 like any
/// other id that names nothing.
fn id_segment<'a>(path: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    let id = path.strip_prefix(prefix)?.strip_suffix(suffix)?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

const FINDINGS: &str = "/api/security-scan/findings/";
const WHITELIST: &str = "/api/security-scan/whitelist/";

fn serve(ctx: &Ctx, req: &Request) -> Result<Answer, String> {
    let db = ctx.db_path.as_path();
    match (req.method.as_str(), req.path) {
        ("GET", "/api/security-scan/findings") => list_findings(db, req.query),
        ("GET", "/api/security-scan/whitelist") => list_whitelist(db),
        ("POST", "/api/security-scan/whitelist") => writes::finish(add_whitelist(db, req.body)),
        ("GET", "/api/security-scan/status") => status(db),
        ("POST", path) => {
            if let Some(id) = id_segment(path, FINDINGS, "/whitelist") {
                writes::finish(whitelist_finding(db, id, req.body))
            } else if let Some(id) = id_segment(path, FINDINGS, "/false-positive") {
                writes::finish(mark_false_positive(db, id))
            } else {
                Err(format!("POST {path} is claimed but unhandled"))
            }
        }
        ("DELETE", path) => match id_segment(path, WHITELIST, "") {
            Some(id) => writes::finish(remove_whitelist(db, id)),
            None => Err(format!("DELETE {path} has no id")),
        },
        _ => Err(format!(
            "{} {} is claimed but unhandled",
            req.method, req.path
        )),
    }
}

// ─── Wire shapes ──────────────────────────────────────────────────────────────

/// One finding on the wire. Field order is the order a reader scans a row in.
#[derive(Debug, serde::Serialize)]
struct FindingRow {
    id: i64,
    session_id: String,
    project_path: String,
    rule_id: String,
    confidence: String,
    masked_snippet: String,
    status: String,
    /// RFC 3339.
    detected_at: String,
}

impl From<store::FindingRecord> for FindingRow {
    fn from(f: store::FindingRecord) -> Self {
        Self {
            id: f.id,
            session_id: f.session_id,
            project_path: f.project_path,
            rule_id: f.rule_id,
            confidence: f.confidence,
            masked_snippet: f.masked_snippet,
            status: f.status,
            detected_at: wire_time(&f.detected_at),
        }
    }
}

/// One whitelist entry on the wire. `kind` is `"rule"` (and `rule_id` names
/// it) or `"value"` (and `rule_id` is `null`); the value's hash is not sent.
#[derive(Debug, serde::Serialize)]
struct WhitelistRow {
    id: i64,
    kind: &'static str,
    rule_id: Option<String>,
    reason: Option<String>,
    /// RFC 3339.
    created_at: String,
}

impl From<store::WhitelistEntry> for WhitelistRow {
    fn from(e: store::WhitelistEntry) -> Self {
        Self {
            id: e.id,
            kind: if e.rule_id.is_some() { "rule" } else { "value" },
            rule_id: e.rule_id,
            reason: e.reason,
            created_at: wire_time(&e.created_at),
        }
    }
}

#[derive(Debug, serde::Serialize)]
struct FindingCounts {
    open: i64,
    whitelisted: i64,
    false_positive: i64,
}

#[derive(Debug, serde::Serialize)]
struct Status {
    /// The stored `credentials_checker_enabled`.
    enabled: bool,
    /// Whether the worker is running in this process right now.
    running: bool,
    findings: FindingCounts,
    /// RFC 3339; `null` before the first scan.
    last_scanned_at: Option<String>,
}

fn encode<T: serde::Serialize>(value: &T, what: &str) -> Result<Vec<u8>, String> {
    gojson::to_vec(value).map_err(|e| format!("encoding {what}: {e}"))
}

// ─── Reads ────────────────────────────────────────────────────────────────────

/// `GET /api/security-scan/findings[?status=open|whitelisted|false_positive]`.
/// An empty `status` is no filter; any other value is a 400 rather than an
/// empty list, so a misspelt filter cannot read as "nothing found".
fn list_findings(db_path: &std::path::Path, query: &str) -> Result<Answer, String> {
    let status = crate::native::analytics::params::query_value(query, "status");
    let filter = match status.as_str() {
        "" => None,
        s if store::STATUSES.contains(&s) => Some(s),
        _ => {
            return Answer::error(
                StatusCode::BAD_REQUEST,
                "status must be \"open\", \"whitelisted\" or \"false_positive\"",
            )
        }
    };
    let conn = db::open_read_only(db_path)?;
    let rows: Vec<FindingRow> = store::list_findings(&conn, filter)?
        .into_iter()
        .map(FindingRow::from)
        .collect();
    Ok(Answer::json(encode(&rows, "findings")?))
}

fn list_whitelist(db_path: &std::path::Path) -> Result<Answer, String> {
    let conn = db::open_read_only(db_path)?;
    let rows: Vec<WhitelistRow> = store::list_whitelist(&conn)?
        .into_iter()
        .map(WhitelistRow::from)
        .collect();
    Ok(Answer::json(encode(&rows, "whitelist")?))
}

fn status(db_path: &std::path::Path) -> Result<Answer, String> {
    let conn = db::open_read_only(db_path)?;
    let enabled = crate::native::settings::load_stored(&conn).credentials_checker_enabled;
    let summary = store::summary(&conn)?;
    let body = Status {
        enabled,
        running: super::worker::is_running(),
        findings: FindingCounts {
            open: summary.open,
            whitelisted: summary.whitelisted,
            false_positive: summary.false_positive,
        },
        last_scanned_at: summary.last_scanned_at.as_deref().map(wire_time),
    };
    Ok(Answer::json(encode(&body, "status")?))
}

// ─── Writes ───────────────────────────────────────────────────────────────────

fn open_for_write(db_path: &std::path::Path) -> Result<rusqlite::Connection, WriteError> {
    let conn = db::open_read_write(db_path).map_err(WriteError::Fallback)?;
    migrate::verify(&conn).map_err(WriteError::Fallback)?;
    Ok(conn)
}

fn finding_not_found(id: &str) -> WriteError {
    WriteError::NotFound {
        resource: "finding".to_string(),
        id: id.to_string(),
    }
}

/// The optional body of `POST …/findings/{id}/whitelist`.
#[derive(Debug, Default, serde::Deserialize)]
struct ReasonRequest {
    #[serde(default)]
    reason: Option<String>,
}

/// A `reason` trimmed, with blank meaning none.
fn reason_of(reason: Option<String>) -> Option<String> {
    reason
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
}

/// `POST /api/security-scan/findings/{id}/whitelist` — whitelist the finding's
/// value by its hash, which suppresses every finding of that value.
///
/// The body is optional (`{"reason": …}`). `201` with the new entry, or `200`
/// with the one that already covers this value.
fn whitelist_finding(
    db_path: &std::path::Path,
    raw_id: &str,
    body: &[u8],
) -> Result<Answer, WriteError> {
    let req: ReasonRequest = if body.iter().all(u8::is_ascii_whitespace) {
        ReasonRequest::default()
    } else {
        writes::decode_body(body)?
    };
    let id: i64 = raw_id.parse().map_err(|_| finding_not_found(raw_id))?;

    let mut conn = open_for_write(db_path)?;
    let hash = match store::finding_match_hash(&conn, id).map_err(WriteError::Fallback)? {
        None => return Err(finding_not_found(raw_id)),
        Some(None) => {
            return Err(WriteError::validation(
                "match_hash",
                "this finding has no value hash yet; it gets one when its session is rescanned",
            ))
        }
        Some(Some(hash)) => hash,
    };

    let reason = reason_of(req.reason);
    let (entry_id, created) =
        store::ensure_whitelist_entry(&mut conn, &WhitelistTarget::Hash(hash), reason.as_deref())
            .map_err(WriteError::Fallback)?;
    log::info!(
        "credential finding whitelisted finding_id={id} entry_id={entry_id} created={created}"
    );
    entry_answer(&conn, entry_id, created)
}

/// `POST /api/security-scan/findings/{id}/false-positive`. `204`.
fn mark_false_positive(db_path: &std::path::Path, raw_id: &str) -> Result<Answer, WriteError> {
    let id: i64 = raw_id.parse().map_err(|_| finding_not_found(raw_id))?;
    let conn = open_for_write(db_path)?;
    if !store::mark_false_positive(&conn, id).map_err(WriteError::Fallback)? {
        return Err(finding_not_found(raw_id));
    }
    log::info!("credential finding marked false positive finding_id={id}");
    Ok(Answer::no_content())
}

/// The body of `POST /api/security-scan/whitelist`: exactly one of `rule_id`
/// and `match_hash`, and an optional `reason`. A blank string counts as unset.
#[derive(Debug, Default, serde::Deserialize)]
struct AddWhitelistRequest {
    #[serde(default)]
    rule_id: Option<String>,
    #[serde(default)]
    match_hash: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /api/security-scan/whitelist`. `201` with the new entry, or `200` with
/// the one that already names this target.
fn add_whitelist(db_path: &std::path::Path, body: &[u8]) -> Result<Answer, WriteError> {
    let req: AddWhitelistRequest = writes::decode_body(body)?;
    let rule_id = reason_of(req.rule_id);
    let match_hash = reason_of(req.match_hash);
    let target = match (rule_id, match_hash) {
        (Some(rule), None) => {
            if !super::rules::compiled().iter().any(|(r, _)| r.id == rule) {
                return Err(WriteError::validation(
                    "rule_id",
                    format!("no detection rule has the id {rule:?}"),
                ));
            }
            WhitelistTarget::Rule(rule)
        }
        (None, Some(hash)) => {
            if !store::is_match_hash(&hash) {
                return Err(WriteError::validation(
                    "match_hash",
                    "match_hash must be 64 lowercase hex digits",
                ));
            }
            WhitelistTarget::Hash(hash)
        }
        _ => {
            return Err(WriteError::validation(
                "",
                "exactly one of rule_id and match_hash is required",
            ))
        }
    };

    let mut conn = open_for_write(db_path)?;
    let reason = reason_of(req.reason);
    let (entry_id, created) = store::ensure_whitelist_entry(&mut conn, &target, reason.as_deref())
        .map_err(WriteError::Fallback)?;
    log::info!("credential whitelist entry added entry_id={entry_id} created={created}");
    entry_answer(&conn, entry_id, created)
}

/// The entry, read back so the answer is the stored row: `201` when this
/// request created it, `200` when it already existed.
fn entry_answer(
    conn: &rusqlite::Connection,
    entry_id: i64,
    created: bool,
) -> Result<Answer, WriteError> {
    let entry = store::list_whitelist(conn)
        .map_err(WriteError::Fallback)?
        .into_iter()
        .find(|e| e.id == entry_id)
        .ok_or_else(|| WriteError::Fallback(format!("whitelist entry {entry_id} vanished")))?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(Answer::json_status(
        status,
        encode(&WhitelistRow::from(entry), "whitelist entry").map_err(WriteError::Fallback)?,
    ))
}

/// `DELETE /api/security-scan/whitelist/{id}`. `204`, or `404` for an id that
/// names no entry.
fn remove_whitelist(db_path: &std::path::Path, raw_id: &str) -> Result<Answer, WriteError> {
    let not_found = || WriteError::NotFound {
        resource: "whitelist entry".to_string(),
        id: raw_id.to_string(),
    };
    let id: i64 = raw_id.parse().map_err(|_| not_found())?;
    let mut conn = open_for_write(db_path)?;
    if !store::remove_whitelist_entry(&mut conn, id).map_err(WriteError::Fallback)? {
        return Err(not_found());
    }
    log::info!("credential whitelist entry removed entry_id={id}");
    Ok(Answer::no_content())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::security::required_scope;
    use crate::native::security::token::Scope;
    use crate::native::security_scan::rules::Confidence;
    use crate::native::security_scan::scan::Finding;
    use rusqlite::params;
    use serde_json::Value;
    use tempfile::NamedTempFile;

    // Synthetic, non-credential-shaped values: the store only ever sees the
    // range it is handed, as its own tests say.
    const TEXT: &str = "one leaked-value-alpha-0001 and leaked-value-bravo-0002 here";

    fn finding(rule_id: &'static str, needle: &str) -> Finding {
        let start = TEXT.find(needle).expect("needle in text");
        Finding {
            rule_id,
            confidence: Confidence::High,
            start,
            end: start + needle.len(),
        }
    }

    fn alpha() -> Finding {
        finding("github-pat", "leaked-value-alpha-0001")
    }

    fn bravo() -> Finding {
        finding("aws-access-key-id", "leaked-value-bravo-0002")
    }

    fn migrated() -> NamedTempFile {
        let file = NamedTempFile::new().expect("tempfile");
        let mut conn = db::ensure_database(file.path()).expect("create");
        migrate::apply(&mut conn).expect("migrate");
        file
    }

    fn conn(file: &NamedTempFile) -> rusqlite::Connection {
        db::open_read_write(file.path()).expect("open")
    }

    fn scan(file: &NamedTempFile, session: &str, findings: &[Finding]) {
        store::record_scan(&mut conn(file), session, "/p", 1, TEXT, findings).expect("record");
    }

    fn call(file: &NamedTempFile, method: &str, path: &str, query: &str, body: &str) -> Answer {
        let method = Method::from_bytes(method.as_bytes()).expect("method");
        let ctx = Ctx {
            db_path: file.path().to_path_buf(),
        };
        serve(
            &ctx,
            &Request {
                method: &method,
                path,
                query,
                content_type: "application/json",
                secret_token: "",
                body: body.as_bytes(),
            },
        )
        .expect("answer")
    }

    fn json(answer: &Answer) -> Value {
        serde_json::from_slice(answer.body.as_deref().expect("a body")).expect("json")
    }

    /// `(id, rule_id, status)` of every finding the list answers, in its order.
    fn listed(file: &NamedTempFile, query: &str) -> Vec<(i64, String, String)> {
        let answer = call(file, "GET", "/api/security-scan/findings", query, "");
        assert_eq!(answer.status, StatusCode::OK);
        json(&answer)
            .as_array()
            .expect("array")
            .iter()
            .map(|f| {
                (
                    f["id"].as_i64().unwrap(),
                    f["rule_id"].as_str().unwrap().to_string(),
                    f["status"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn id_of(file: &NamedTempFile, session: &str, rule: &str) -> i64 {
        conn(file)
            .query_row(
                "SELECT id FROM credential_findings WHERE session_id = ?1 AND rule_id = ?2",
                params![session, rule],
                |r| r.get(0),
            )
            .expect("finding id")
    }

    fn status_of(file: &NamedTempFile, id: i64) -> String {
        conn(file)
            .query_row(
                "SELECT status FROM credential_findings WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .expect("status")
    }

    #[test]
    fn the_routes_are_claimed_and_their_neighbours_are_not() {
        assert!(claims(&Method::GET, "/api/security-scan/findings"));
        assert!(claims(
            &Method::POST,
            "/api/security-scan/findings/7/whitelist"
        ));
        assert!(claims(
            &Method::POST,
            "/api/security-scan/findings/7/false-positive"
        ));
        assert!(claims(&Method::GET, "/api/security-scan/whitelist"));
        assert!(claims(&Method::POST, "/api/security-scan/whitelist"));
        assert!(claims(&Method::DELETE, "/api/security-scan/whitelist/3"));
        assert!(claims(&Method::GET, "/api/security-scan/status"));

        // Wrong method.
        assert!(!claims(&Method::POST, "/api/security-scan/findings"));
        assert!(!claims(
            &Method::GET,
            "/api/security-scan/findings/7/whitelist"
        ));
        assert!(!claims(&Method::DELETE, "/api/security-scan/whitelist"));
        assert!(!claims(&Method::PUT, "/api/security-scan/whitelist/3"));
        assert!(!claims(&Method::POST, "/api/security-scan/status"));
        // A trailing slash is a different route.
        assert!(!claims(&Method::GET, "/api/security-scan/findings/"));
        assert!(!claims(&Method::GET, "/api/security-scan/status/"));
        assert!(!claims(&Method::DELETE, "/api/security-scan/whitelist/"));
        assert!(!claims(
            &Method::POST,
            "/api/security-scan/findings//whitelist"
        ));
        // An extra or missing segment.
        assert!(!claims(&Method::DELETE, "/api/security-scan/whitelist/3/x"));
        assert!(!claims(
            &Method::POST,
            "/api/security-scan/findings/7/whitelist/x"
        ));
        assert!(!claims(&Method::POST, "/api/security-scan/findings/7"));
        assert!(!claims(&Method::GET, "/api/security-scan"));
        // Not the credential system's routes, which `security` owns.
        assert!(!claims(&Method::GET, "/api/security/tokens"));
    }

    #[test]
    fn the_reads_need_read_and_the_writes_need_write() {
        // `/api/security-scan/` is not under `/api/security/`, so the default
        // rule applies — no special case, and none is wanted.
        for (method, pattern) in ROUTES {
            let path = pattern.replace("{id}", "1");
            let want = if *method == "GET" {
                Scope::Read
            } else {
                Scope::Write
            };
            let method = Method::from_bytes(method.as_bytes()).unwrap();
            assert_eq!(required_scope(&method, &path), want, "{method} {path}");
        }
    }

    #[test]
    fn findings_are_listed_newest_first_filtered_and_without_their_hash() {
        let file = migrated();
        scan(&file, "s1", &[alpha(), bravo()]);
        let a = id_of(&file, "s1", "github-pat");
        let b = id_of(&file, "s1", "aws-access-key-id");
        store::mark_false_positive(&conn(&file), b).unwrap();

        // One scan stamps both rows alike, so `id` decides: newest first.
        assert_eq!(
            listed(&file, ""),
            vec![
                (b, "aws-access-key-id".into(), "false_positive".into()),
                (a, "github-pat".into(), "open".into())
            ]
        );
        assert_eq!(
            listed(&file, "status=open"),
            vec![(a, "github-pat".into(), "open".into())]
        );
        assert_eq!(
            listed(&file, "status=false_positive"),
            vec![(b, "aws-access-key-id".into(), "false_positive".into())]
        );
        assert!(listed(&file, "status=whitelisted").is_empty());

        let answer = call(&file, "GET", "/api/security-scan/findings", "", "");
        let row = &json(&answer)[1];
        // `Value` sorts its keys, so the set is checked here and the wire
        // order — the struct's — on the raw bytes.
        let mut keys: Vec<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "confidence",
                "detected_at",
                "id",
                "masked_snippet",
                "project_path",
                "rule_id",
                "session_id",
                "status"
            ]
        );
        let raw = std::str::from_utf8(answer.body.as_deref().unwrap()).unwrap();
        assert!(raw.starts_with(r#"[{"id":"#), "{raw}");
        assert!(raw.contains(r#""status":"open","detected_at":"#), "{raw}");
        assert_eq!(row["masked_snippet"], "leak********0001");
        assert_eq!(row["confidence"], "high");
        // RFC 3339, not the stored Go text.
        let detected = row["detected_at"].as_str().unwrap();
        assert!(
            detected.contains('T') && !detected.contains(" +0000"),
            "{detected}"
        );
        // Neither the value nor its hash is anywhere in the answer.
        let raw = String::from_utf8(answer.body.unwrap()).unwrap();
        assert!(!raw.contains("leaked-value"));
        assert!(!raw.contains(&store::hash_match("leaked-value-alpha-0001")));

        let bad = call(
            &file,
            "GET",
            "/api/security-scan/findings",
            "status=closed",
            "",
        );
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn whitelisting_a_finding_suppresses_its_value_everywhere_once() {
        let file = migrated();
        scan(&file, "s1", &[alpha(), bravo()]);
        scan(&file, "s2", &[alpha()]);
        let a1 = id_of(&file, "s1", "github-pat");
        let a2 = id_of(&file, "s2", "github-pat");
        let b1 = id_of(&file, "s1", "aws-access-key-id");

        let path = format!("/api/security-scan/findings/{a1}/whitelist");
        let answer = call(&file, "POST", &path, "", r#"{"reason":"  test fixture  "}"#);
        assert_eq!(answer.status, StatusCode::CREATED);
        let entry = json(&answer);
        assert_eq!(entry["kind"], "value");
        assert_eq!(entry["rule_id"], Value::Null);
        assert_eq!(entry["reason"], "test fixture");
        // The hash is resolved server-side and never answered, here or in
        // the list.
        let hash = store::hash_match("leaked-value-alpha-0001");
        assert!(!std::str::from_utf8(answer.body.as_deref().unwrap())
            .unwrap()
            .contains(&hash));

        // The same value in another session went with it; another value did not.
        assert_eq!(status_of(&file, a1), "whitelisted");
        assert_eq!(status_of(&file, a2), "whitelisted");
        assert_eq!(status_of(&file, b1), "open");

        // A second click answers the entry that exists rather than adding one.
        let again = call(
            &file,
            "POST",
            &format!("/api/security-scan/findings/{a2}/whitelist"),
            "",
            "",
        );
        assert_eq!(again.status, StatusCode::OK);
        assert_eq!(json(&again)["id"], entry["id"]);
        let list = call(&file, "GET", "/api/security-scan/whitelist", "", "");
        assert_eq!(json(&list).as_array().unwrap().len(), 1);
        assert!(!std::str::from_utf8(list.body.as_deref().unwrap())
            .unwrap()
            .contains(&hash));
    }

    #[test]
    fn whitelisting_needs_a_finding_with_a_hash() {
        let file = migrated();
        scan(&file, "s1", &[alpha()]);
        let a = id_of(&file, "s1", "github-pat");

        for path in [
            "/api/security-scan/findings/999/whitelist",
            "/api/security-scan/findings/abc/whitelist",
        ] {
            assert_eq!(
                call(&file, "POST", path, "", "").status,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }

        // A row written before migration 43 and not rescanned since.
        conn(&file)
            .execute("UPDATE credential_findings SET match_hash = NULL", [])
            .unwrap();
        let path = format!("/api/security-scan/findings/{a}/whitelist");
        let answer = call(&file, "POST", &path, "", "");
        assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(status_of(&file, a), "open");
        let list = call(&file, "GET", "/api/security-scan/whitelist", "", "");
        assert!(json(&list).as_array().unwrap().is_empty());

        // A body that is there has to be JSON.
        assert_eq!(
            call(&file, "POST", &path, "", "{").status,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn a_false_positive_survives_a_rescan_and_the_whitelist() {
        let file = migrated();
        scan(&file, "s1", &[alpha()]);
        let a = id_of(&file, "s1", "github-pat");

        let path = format!("/api/security-scan/findings/{a}/false-positive");
        let answer = call(&file, "POST", &path, "", "");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        assert!(answer.body.is_none());
        assert_eq!(status_of(&file, a), "false_positive");

        scan(&file, "s1", &[alpha()]);
        assert_eq!(status_of(&file, a), "false_positive");
        let rule = call(
            &file,
            "POST",
            "/api/security-scan/whitelist",
            "",
            r#"{"rule_id":"github-pat"}"#,
        );
        assert_eq!(rule.status, StatusCode::CREATED);
        assert_eq!(status_of(&file, a), "false_positive");

        for path in [
            "/api/security-scan/findings/999/false-positive",
            "/api/security-scan/findings/x/false-positive",
        ] {
            assert_eq!(
                call(&file, "POST", path, "", "").status,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
    }

    #[test]
    fn a_whitelist_entry_names_exactly_one_target() {
        let file = migrated();
        let hash = store::hash_match("leaked-value-alpha-0001");
        for body in [
            format!(r#"{{"rule_id":"github-pat","match_hash":"{hash}"}}"#),
            "{}".to_string(),
            r#"{"rule_id":"  ","match_hash":null}"#.to_string(),
            r#"{"match_hash":"not-a-hash"}"#.to_string(),
            format!(r#"{{"match_hash":"{}"}}"#, hash.to_uppercase()),
            // Not a rule: it would be stored and never suppress anything.
            r#"{"rule_id":"github-pats"}"#.to_string(),
        ] {
            let answer = call(&file, "POST", "/api/security-scan/whitelist", "", &body);
            assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        }
        assert_eq!(
            call(&file, "POST", "/api/security-scan/whitelist", "", "").status,
            StatusCode::BAD_REQUEST
        );
        let list = call(&file, "GET", "/api/security-scan/whitelist", "", "");
        assert!(json(&list).as_array().unwrap().is_empty());

        let by_hash = call(
            &file,
            "POST",
            "/api/security-scan/whitelist",
            "",
            &format!(r#"{{"match_hash":"{hash}","reason":""}}"#),
        );
        assert_eq!(by_hash.status, StatusCode::CREATED);
        let entry = json(&by_hash);
        assert_eq!(entry["kind"], "value");
        assert_eq!(entry["reason"], Value::Null);
        assert!(entry["created_at"].as_str().unwrap().contains('T'));

        // Repeating either kind of entry answers the one that exists.
        let body = r#"{"rule_id":"github-pat"}"#;
        let by_rule = call(&file, "POST", "/api/security-scan/whitelist", "", body);
        assert_eq!(by_rule.status, StatusCode::CREATED);
        assert_eq!(json(&by_rule)["kind"], "rule");
        assert_eq!(json(&by_rule)["rule_id"], "github-pat");
        let again = call(&file, "POST", "/api/security-scan/whitelist", "", body);
        assert_eq!(again.status, StatusCode::OK);
        assert_eq!(json(&again)["id"], json(&by_rule)["id"]);
        let hash_again = format!(r#"{{"match_hash":"{hash}"}}"#);
        let again = call(
            &file,
            "POST",
            "/api/security-scan/whitelist",
            "",
            &hash_again,
        );
        assert_eq!(again.status, StatusCode::OK);
        assert_eq!(json(&again)["id"], entry["id"]);
        let list = call(&file, "GET", "/api/security-scan/whitelist", "", "");
        assert_eq!(json(&list).as_array().unwrap().len(), 2);
    }

    #[test]
    fn deleting_an_entry_reopens_what_it_suppressed() {
        let file = migrated();
        scan(&file, "s1", &[alpha()]);
        let a = id_of(&file, "s1", "github-pat");
        let added = call(
            &file,
            "POST",
            "/api/security-scan/whitelist",
            "",
            r#"{"rule_id":"github-pat"}"#,
        );
        let entry = json(&added)["id"].as_i64().unwrap();
        assert_eq!(status_of(&file, a), "whitelisted");

        for path in [
            "/api/security-scan/whitelist/999",
            "/api/security-scan/whitelist/x",
        ] {
            assert_eq!(
                call(&file, "DELETE", path, "", "").status,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        assert_eq!(status_of(&file, a), "whitelisted");

        let path = format!("/api/security-scan/whitelist/{entry}");
        let answer = call(&file, "DELETE", &path, "", "");
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
        assert_eq!(status_of(&file, a), "open");
        assert_eq!(
            call(&file, "DELETE", &path, "", "").status,
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn status_reports_the_setting_the_counts_and_the_last_scan() {
        let file = migrated();
        let status = || json(&call(&file, "GET", "/api/security-scan/status", "", ""));

        let zero = status();
        assert_eq!(zero["enabled"], false);
        assert_eq!(zero["running"], false);
        assert_eq!(
            zero["findings"],
            serde_json::json!({"open": 0, "whitelisted": 0, "false_positive": 0})
        );
        assert_eq!(zero["last_scanned_at"], Value::Null);

        conn(&file)
            .execute(
                "INSERT INTO user_settings (id, credentials_checker_enabled) VALUES (1, 1)",
                [],
            )
            .unwrap();
        scan(&file, "s1", &[alpha()]);
        let one = status();
        assert_eq!(one["enabled"], true);
        assert_eq!(
            one["findings"],
            serde_json::json!({"open": 1, "whitelisted": 0, "false_positive": 0})
        );
        assert!(one["last_scanned_at"].as_str().unwrap().contains('T'));

        scan(&file, "s2", &[alpha(), bravo()]);
        scan(&file, "s3", &[bravo()]);
        store::mark_false_positive(&conn(&file), id_of(&file, "s3", "aws-access-key-id")).unwrap();
        call(
            &file,
            "POST",
            "/api/security-scan/whitelist",
            "",
            r#"{"rule_id":"github-pat"}"#,
        );
        assert_eq!(
            status()["findings"],
            serde_json::json!({"open": 1, "whitelisted": 2, "false_positive": 1})
        );
    }
}
