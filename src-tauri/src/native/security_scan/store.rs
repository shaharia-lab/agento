//! The Credentials Checker's rows: what needs (re)scanning, the whitelist-aware
//! write, and the whitelist itself (#602, design doc §5.1–§5.2, §6).
//!
//! Every other part of the checker — the worker (#603), the `/api` surface
//! (#604) — reads and writes migration 41's tables through this module, so the
//! two invariants below are kept in one place rather than at every call site.
//!
//! ## A whitelisted match is never written as `open`
//!
//! [`record_scan`] checks `credential_whitelist` by rule id **and** by value
//! hash before it writes a finding, so a suppressed match never appears at all
//! rather than appearing and being hidden after the fact. And because a
//! whitelist entry added later has to reach findings already written,
//! [`add_whitelist_entry`] flips every matching `open` finding to `whitelisted`
//! in the same transaction as its insert — no rescan needed.
//!
//! ## No raw secret leaves [`record_scan`]
//!
//! A [`Finding`] is a byte range, not text. The matched bytes are sliced out of
//! the caller's input only long enough to compute the two redacted forms that
//! are stored — [`mask`] and [`hash_match`] — and are dropped before the row is
//! written.
//!
//! ## The hash scheme, decided here once
//!
//! `match_hash` on both tables is **SHA-256 of the matched bytes, lowercase
//! hex** ([`hash_match`]). Nothing else in the codebase computes it: a finding
//! gets it from [`record_scan`], and a by-value whitelist entry is created from
//! a finding's stored hash, which [`add_whitelist_entry`] refuses unless it has
//! that exact shape. Two schemes would make by-hash suppression silently never
//! fire, so there is exactly one function and one validator.
//!
//! Every statement keys on `(session_id, project_path)`, for the reason
//! `insights::store`'s header gives: a corpus can hold one session id under two
//! project paths.

use rusqlite::{params, Connection};

use super::scan::Finding;
use crate::native::gotime;

/// One session the worker still has to scan. The same shape, and the same key,
/// as `insights::store::Pending`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pending {
    pub session_id: String,
    pub project_path: String,
    pub file_path: String,
}

/// Every cached session with no `credential_scan_state` row, or one scanned
/// under an older ruleset. Passing [`super::rules::CURRENT_RULESET_VERSION`]
/// after a bump therefore answers the whole corpus again — the same contract as
/// `insights::store::needs_processing` and `CURRENT_PROCESSOR_VERSION`.
pub fn needs_scanning(conn: &Connection, ruleset_version: i64) -> Result<Vec<Pending>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT c.session_id, c.project_path, c.file_path
             FROM claude_session_cache c
             LEFT JOIN credential_scan_state s
                    ON c.session_id = s.session_id
                   AND c.project_path = s.project_path
             WHERE s.session_id IS NULL
                OR s.ruleset_version < ?1",
        )
        .map_err(|e| format!("preparing needs_scanning: {e}"))?;

    let rows = stmt
        .query_map(params![ruleset_version], |row| {
            Ok(Pending {
                session_id: row.get(0)?,
                project_path: row.get(1)?,
                file_path: row.get(2)?,
            })
        })
        .map_err(|e| format!("querying needs_scanning: {e}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("reading needs_scanning: {e}"))
}

/// The one hashing scheme `match_hash` uses on both tables: SHA-256 of the
/// matched bytes, lowercase hex. See the module header.
pub fn hash_match(matched: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, matched.as_bytes());
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `hash` has [`hash_match`]'s shape: 64 lowercase hex digits.
fn is_match_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The display form of a match (design doc §4.4): its first four and last four
/// characters around a fixed run of `*`.
///
/// **The run is fixed rather than one `*` per hidden character**, so the
/// stored snippet does not also record the secret's length, and a PEM block
/// does not become a two-kilobyte row of stars. A match of fewer than
/// [`MASK_MIN_CHARS`] characters shows nothing of itself: revealing eight of,
/// say, twelve characters is not masking. Counted in characters, not bytes, so
/// a cut never lands inside a UTF-8 sequence.
pub fn mask(matched: &str) -> String {
    let chars: Vec<char> = matched.chars().collect();
    if chars.len() < MASK_MIN_CHARS {
        return MASK_RUN.to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}{MASK_RUN}{tail}")
}

const MASK_RUN: &str = "********";
const MASK_MIN_CHARS: usize = 16;

/// Whether a match is suppressed — by a whitelist entry for its rule, or for
/// its value's hash.
pub fn is_whitelisted(conn: &Connection, rule_id: &str, match_hash: &str) -> Result<bool, String> {
    conn.prepare_cached(
        "SELECT EXISTS (
             SELECT 1 FROM credential_whitelist
              WHERE rule_id = ?1 OR match_hash = ?2
         )",
    )
    .and_then(|mut stmt| stmt.query_row(params![rule_id, match_hash], |row| row.get(0)))
    .map_err(|e| format!("checking the credential whitelist: {e}"))
}

/// Record one session's scan, in one transaction.
///
/// `text` is the input `findings` were computed from — their offsets index into
/// it. The `credential_scan_state` row is written **unconditionally**, zero
/// findings included: it is what makes a clean session not pending next pass.
/// Each finding is then written only if [`is_whitelisted`] says it is not.
///
/// A rescan of the same match is an upsert on the table's UNIQUE key, not a
/// failure. The update refreshes what a new ruleset may change and **keeps
/// `status` and `detected_at`**: a finding the user marked `false_positive`
/// stays marked, and "detected" means first detected.
///
/// **The call is the session's whole scan, so it is authoritative for `open`
/// rows**: an `open` finding of this session that this scan did not write —
/// a rule retired or tightened by a ruleset bump, or a match now whitelisted —
/// is deleted. `whitelisted` and `false_positive` rows are kept, since they
/// carry a verdict the user made. It follows that `text` must be the whole
/// transcript the offsets index into, never one chunk of it.
///
/// A finding whose range does not lie on character boundaries of `text` fails
/// the whole call and writes nothing — it means the caller passed the wrong
/// text, and a masked snippet of the wrong bytes would be a silent lie.
pub fn record_scan(
    conn: &mut Connection,
    session_id: &str,
    project_path: &str,
    ruleset_version: i64,
    text: &str,
    findings: &[Finding],
) -> Result<(), String> {
    let now = gotime::now_go_text();
    let tx = conn
        .transaction()
        .map_err(|e| format!("starting the credential scan write: {e}"))?;

    tx.execute(
        "INSERT INTO credential_scan_state (session_id, project_path, ruleset_version, scanned_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(session_id, project_path) DO UPDATE SET
             ruleset_version = excluded.ruleset_version,
             scanned_at = excluded.scanned_at",
        params![session_id, project_path, ruleset_version, now],
    )
    .map_err(|e| format!("recording the scan state for {session_id}: {e}"))?;

    // What this scan wrote, keyed the way the table's UNIQUE key is.
    let mut written: std::collections::HashSet<(&str, i64)> = std::collections::HashSet::new();
    for f in findings {
        // The only place the matched bytes exist; both redacted forms are
        // computed and the slice goes out of scope with this block.
        let (masked, hash) = {
            let matched = text.get(f.start..f.end).ok_or_else(|| {
                format!(
                    "a {} finding in {session_id} at {}..{} is not a range of the scanned text",
                    f.rule_id, f.start, f.end
                )
            })?;
            (mask(matched), hash_match(matched))
        };
        if is_whitelisted(&tx, f.rule_id, &hash)? {
            continue;
        }
        let mut stmt = tx
            .prepare_cached(
                "INSERT INTO credential_findings
                     (session_id, project_path, rule_id, confidence, masked_snippet,
                      location_start, location_end, ruleset_version, detected_at, match_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(session_id, project_path, rule_id, location_start) DO UPDATE SET
                     confidence = excluded.confidence,
                     masked_snippet = excluded.masked_snippet,
                     location_end = excluded.location_end,
                     ruleset_version = excluded.ruleset_version,
                     match_hash = excluded.match_hash",
            )
            .map_err(|e| format!("preparing the finding upsert: {e}"))?;
        stmt.execute(params![
            session_id,
            project_path,
            f.rule_id,
            f.confidence.as_str(),
            masked,
            f.start as i64,
            f.end as i64,
            ruleset_version,
            now,
            hash,
        ])
        .map_err(|e| format!("writing a {} finding for {session_id}: {e}", f.rule_id))?;
        written.insert((f.rule_id, f.start as i64));
    }

    let stale: Vec<(i64, String, i64)> = tx
        .prepare(
            "SELECT id, rule_id, location_start FROM credential_findings
              WHERE session_id = ?1 AND project_path = ?2 AND status = 'open'",
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![session_id, project_path], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect()
        })
        .map_err(|e| format!("reading {session_id}'s open findings: {e}"))?;
    for (id, rule_id, start) in stale {
        if !written.contains(&(rule_id.as_str(), start)) {
            tx.execute("DELETE FROM credential_findings WHERE id = ?1", params![id])
                .map_err(|e| format!("retracting a stale finding for {session_id}: {e}"))?;
        }
    }

    tx.commit()
        .map_err(|e| format!("committing the credential scan for {session_id}: {e}"))
}

/// What a whitelist entry suppresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhitelistTarget {
    /// One matched value, by its [`hash_match`] hash — read off a stored
    /// finding's `match_hash`, never computed by the caller.
    Hash(String),
    /// Every match of one rule.
    Rule(String),
}

/// One `credential_whitelist` row. Exactly one of `match_hash` and `rule_id`
/// is set on a row this module wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhitelistEntry {
    pub id: i64,
    pub match_hash: Option<String>,
    pub rule_id: Option<String>,
    pub reason: Option<String>,
    pub created_at: String,
}

/// Add a whitelist entry and **retroactively** suppress what it matches: every
/// existing `open` finding with that rule id, or that value hash, becomes
/// `whitelisted` in the same transaction (design doc §6). Answers the new
/// entry's id.
///
/// Only `open` findings move. A `false_positive` is the user's own verdict and
/// is not the whitelist's to overwrite.
///
/// A [`WhitelistTarget::Hash`] that is not [`hash_match`]'s shape is refused:
/// it would be stored and then never match anything, which is the silent
/// failure the module header's single scheme exists to prevent.
pub fn add_whitelist_entry(
    conn: &mut Connection,
    target: &WhitelistTarget,
    reason: Option<&str>,
) -> Result<i64, String> {
    let (match_hash, rule_id) = match target {
        WhitelistTarget::Hash(hash) => {
            if !is_match_hash(hash) {
                return Err(format!(
                    "a whitelist hash must be 64 lowercase hex digits, got {} characters",
                    hash.len()
                ));
            }
            (Some(hash.as_str()), None)
        }
        WhitelistTarget::Rule(rule) => {
            if rule.is_empty() {
                return Err("a whitelist rule id must not be empty".to_string());
            }
            (None, Some(rule.as_str()))
        }
    };

    let tx = conn
        .transaction()
        .map_err(|e| format!("starting the whitelist write: {e}"))?;
    tx.execute(
        "INSERT INTO credential_whitelist (match_hash, rule_id, reason, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![match_hash, rule_id, reason, gotime::now_go_text()],
    )
    .map_err(|e| format!("adding a whitelist entry: {e}"))?;
    let id = tx.last_insert_rowid();

    // One of the two parameters is NULL, and `x = NULL` is never true, so this
    // matches on exactly the column the entry sets.
    tx.execute(
        "UPDATE credential_findings SET status = 'whitelisted'
          WHERE status = 'open' AND (rule_id = ?1 OR match_hash = ?2)",
        params![rule_id, match_hash],
    )
    .map_err(|e| format!("applying a whitelist entry to existing findings: {e}"))?;

    tx.commit()
        .map_err(|e| format!("committing the whitelist entry: {e}"))?;
    Ok(id)
}

/// Delete one whitelist entry, and re-open what it alone was suppressing.
/// Answers whether a row was deleted.
///
/// A `whitelisted` finding goes back to `open` in the same transaction unless a
/// remaining entry still covers it by rule id or by hash — every finding stores
/// both, so that is an exact check, not a guess. This is what makes a whitelist
/// action reversible from Settings (design doc §5.3). `false_positive` rows are
/// the user's own verdict and are not touched.
pub fn remove_whitelist_entry(conn: &mut Connection, id: i64) -> Result<bool, String> {
    let tx = conn
        .transaction()
        .map_err(|e| format!("starting the whitelist removal: {e}"))?;
    let removed = tx
        .execute(
            "DELETE FROM credential_whitelist WHERE id = ?1",
            params![id],
        )
        .map_err(|e| format!("removing whitelist entry {id}: {e}"))?;
    if removed > 0 {
        tx.execute(
            "UPDATE credential_findings SET status = 'open'
              WHERE status = 'whitelisted'
                AND NOT EXISTS (
                    SELECT 1 FROM credential_whitelist w
                     WHERE w.rule_id = credential_findings.rule_id
                        OR w.match_hash = credential_findings.match_hash
                )",
            [],
        )
        .map_err(|e| format!("re-opening findings after removing whitelist entry {id}: {e}"))?;
    }
    tx.commit()
        .map_err(|e| format!("committing the whitelist removal: {e}"))?;
    Ok(removed > 0)
}

/// Every whitelist entry, oldest first.
pub fn list_whitelist(conn: &Connection) -> Result<Vec<WhitelistEntry>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, match_hash, rule_id, reason, created_at
               FROM credential_whitelist ORDER BY id",
        )
        .map_err(|e| format!("preparing the whitelist read: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(WhitelistEntry {
                id: row.get(0)?,
                match_hash: row.get(1)?,
                rule_id: row.get(2)?,
                reason: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .map_err(|e| format!("reading the whitelist: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("reading the whitelist: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::security_scan::rules::Confidence;

    // Every fixture is synthetic and has no credential's shape: the store never
    // looks at what a match *is*, only at the range it is handed, so a plain
    // word proves everything a real key would.

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().expect("in-memory db");
        crate::native::migrate::apply(&mut conn).expect("migrations");
        conn
    }

    fn cache_row(conn: &Connection, session_id: &str, project_path: &str) {
        conn.execute(
            "INSERT INTO claude_session_cache
                 (session_id, project_path, file_path, file_mtime, start_time, last_activity)
             VALUES (?1, ?2, ?3, '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00',
                     '2026-01-01 00:00:00+00:00')",
            params![
                session_id,
                project_path,
                format!("{project_path}/{session_id}.jsonl")
            ],
        )
        .expect("cache row");
    }

    const TEXT: &str = "one leaked-value-alpha-0001 and leaked-value-bravo-0002 here";

    /// A finding over the first occurrence of `needle` in [`TEXT`].
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
        finding("rule-a", "leaked-value-alpha-0001")
    }

    fn bravo() -> Finding {
        finding("rule-b", "leaked-value-bravo-0002")
    }

    /// `(rule_id, status, match_hash)` per stored finding, by rule.
    fn stored(conn: &Connection) -> Vec<(String, String, Option<String>)> {
        let mut stmt = conn
            .prepare(
                "SELECT rule_id, status, match_hash FROM credential_findings
                 ORDER BY rule_id, location_start",
            )
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows")
    }

    fn pending_ids(conn: &Connection, version: i64) -> Vec<(String, String)> {
        let mut pending = needs_scanning(conn, version).expect("needs_scanning");
        pending.sort();
        pending
            .into_iter()
            .map(|p| (p.session_id, p.project_path))
            .collect()
    }

    #[test]
    fn the_hash_is_sha256_hex_and_the_mask_hides_the_middle() {
        // The SHA-256 test vector for "abc", so the scheme is pinned by value
        // and not merely by agreeing with itself.
        assert_eq!(
            hash_match("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(is_match_hash(&hash_match("anything")));
        assert!(!is_match_hash(&hash_match("x").to_uppercase()));

        assert_eq!(mask("leaked-value-alpha-0001"), "leak********0001");
        assert_eq!(mask("short-value"), "********");
        // Characters, not bytes: a multi-byte head is not cut in half.
        assert_eq!(mask("ééééxxxxxxxxxxxxzzzz"), "éééé********zzzz");
    }

    #[test]
    fn needs_scanning_answers_unscanned_sessions_by_the_pair() {
        let mut conn = db();
        cache_row(&conn, "s1", "/a");
        cache_row(&conn, "s1", "/b");
        assert_eq!(
            pending_ids(&conn, 1),
            vec![("s1".into(), "/a".into()), ("s1".into(), "/b".into())]
        );

        // Scanning one of the pair leaves the other pending — a join on the id
        // alone would call both done.
        record_scan(&mut conn, "s1", "/a", 1, TEXT, &[]).expect("record");
        assert_eq!(pending_ids(&conn, 1), vec![("s1".into(), "/b".into())]);

        record_scan(&mut conn, "s1", "/b", 1, TEXT, &[]).expect("record");
        assert!(pending_ids(&conn, 1).is_empty());
    }

    #[test]
    fn a_ruleset_bump_requeues_every_session() {
        let mut conn = db();
        cache_row(&conn, "s1", "/a");
        cache_row(&conn, "s2", "/a");
        record_scan(&mut conn, "s1", "/a", 1, TEXT, &[alpha()]).expect("record");
        record_scan(&mut conn, "s2", "/a", 1, TEXT, &[]).expect("record");
        assert!(pending_ids(&conn, 1).is_empty());

        assert_eq!(
            pending_ids(&conn, 2),
            vec![("s1".into(), "/a".into()), ("s2".into(), "/a".into())]
        );
        record_scan(&mut conn, "s1", "/a", 2, TEXT, &[alpha()]).expect("rescan");
        assert_eq!(pending_ids(&conn, 2), vec![("s2".into(), "/a".into())]);
    }

    #[test]
    fn record_scan_stores_the_hash_and_mask_never_the_value() {
        let mut conn = db();
        record_scan(&mut conn, "s1", "/a", 1, TEXT, &[alpha(), bravo()]).expect("record");

        assert_eq!(
            stored(&conn),
            vec![
                (
                    "rule-a".into(),
                    "open".into(),
                    Some(hash_match("leaked-value-alpha-0001"))
                ),
                (
                    "rule-b".into(),
                    "open".into(),
                    Some(hash_match("leaked-value-bravo-0002"))
                ),
            ]
        );
        let snippet: String = conn
            .query_row(
                "SELECT masked_snippet FROM credential_findings WHERE rule_id = 'rule-a'",
                [],
                |r| r.get(0),
            )
            .expect("snippet");
        assert_eq!(snippet, "leak********0001");

        // No text column anywhere holds the raw value.
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM credential_findings
                  WHERE masked_snippet LIKE '%alpha%' OR match_hash LIKE '%alpha%'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(leaked, 0);
    }

    #[test]
    fn a_clean_scan_still_records_its_state() {
        let mut conn = db();
        record_scan(&mut conn, "s1", "/a", 1, TEXT, &[]).expect("record");
        let version: i64 = conn
            .query_row(
                "SELECT ruleset_version FROM credential_scan_state
                  WHERE session_id = 's1' AND project_path = '/a'",
                [],
                |r| r.get(0),
            )
            .expect("state row");
        assert_eq!(version, 1);
        assert!(stored(&conn).is_empty());
    }

    #[test]
    fn a_rescan_is_an_upsert_that_keeps_the_users_verdict() {
        let mut conn = db();
        record_scan(&mut conn, "s1", "/a", 1, TEXT, &[alpha()]).expect("record");
        conn.execute(
            "UPDATE credential_findings SET status = 'false_positive'",
            [],
        )
        .expect("mark");

        record_scan(&mut conn, "s1", "/a", 2, TEXT, &[alpha()]).expect("rescan");

        let rows: Vec<(String, i64)> = conn
            .prepare("SELECT status, ruleset_version FROM credential_findings")
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(rows, vec![("false_positive".to_string(), 2)]);
    }

    #[test]
    fn a_whitelisted_match_is_never_written_as_open() {
        let mut conn = db();
        add_whitelist_entry(
            &mut conn,
            &WhitelistTarget::Hash(hash_match("leaked-value-alpha-0001")),
            None,
        )
        .expect("by hash");
        add_whitelist_entry(&mut conn, &WhitelistTarget::Rule("rule-b".into()), None)
            .expect("by rule");

        let other = finding("rule-c", "here");
        record_scan(&mut conn, "s1", "/a", 1, TEXT, &[alpha(), bravo(), other]).expect("record");

        // Only the unsuppressed match exists at all.
        assert_eq!(
            stored(&conn),
            vec![("rule-c".into(), "open".into(), Some(hash_match("here")))]
        );
    }

    #[test]
    fn a_whitelist_by_hash_suppresses_existing_findings_without_a_rescan() {
        let mut conn = db();
        record_scan(&mut conn, "s1", "/a", 1, TEXT, &[alpha(), bravo()]).expect("record");
        // The same value leaked in a second session, under a different rule.
        let again = Finding {
            rule_id: "rule-z",
            ..alpha()
        };
        record_scan(&mut conn, "s2", "/a", 1, TEXT, &[again]).expect("record");

        // Created from a stored finding's hash, the way #604 will.
        let hash: String = conn
            .query_row(
                "SELECT match_hash FROM credential_findings
                  WHERE session_id = 's1' AND rule_id = 'rule-a'",
                [],
                |r| r.get(0),
            )
            .expect("hash");
        add_whitelist_entry(
            &mut conn,
            &WhitelistTarget::Hash(hash.clone()),
            Some("test key"),
        )
        .expect("whitelist");

        assert_eq!(
            stored(&conn),
            vec![
                ("rule-a".into(), "whitelisted".into(), Some(hash.clone())),
                (
                    "rule-b".into(),
                    "open".into(),
                    Some(hash_match("leaked-value-bravo-0002"))
                ),
                ("rule-z".into(), "whitelisted".into(), Some(hash)),
            ]
        );
    }

    #[test]
    fn a_whitelist_by_rule_suppresses_existing_findings_but_not_a_false_positive() {
        let mut conn = db();
        record_scan(&mut conn, "s1", "/a", 1, TEXT, &[alpha(), bravo()]).expect("record");
        record_scan(
            &mut conn,
            "s2",
            "/a",
            1,
            TEXT,
            &[finding("rule-a", "leaked-value-bravo-0002")],
        )
        .expect("record");
        conn.execute(
            "UPDATE credential_findings SET status = 'false_positive' WHERE session_id = 's2'",
            [],
        )
        .expect("mark");

        add_whitelist_entry(&mut conn, &WhitelistTarget::Rule("rule-a".into()), None)
            .expect("whitelist");

        let statuses: Vec<(String, String)> = stored(&conn)
            .into_iter()
            .map(|(rule, status, _)| (rule, status))
            .collect();
        assert_eq!(
            statuses,
            vec![
                ("rule-a".into(), "whitelisted".into()),
                ("rule-a".into(), "false_positive".into()),
                ("rule-b".into(), "open".into()),
            ]
        );
    }

    #[test]
    fn a_malformed_hash_or_empty_rule_is_refused_and_writes_nothing() {
        let mut conn = db();
        for bad in ["", "abc", &hash_match("x").to_uppercase()] {
            add_whitelist_entry(&mut conn, &WhitelistTarget::Hash(bad.to_string()), None)
                .expect_err("not hash_match's shape");
        }
        add_whitelist_entry(&mut conn, &WhitelistTarget::Rule(String::new()), None)
            .expect_err("empty rule");
        assert!(list_whitelist(&conn).expect("list").is_empty());
    }

    #[test]
    fn whitelist_entries_list_and_remove() {
        let mut conn = db();
        let hash = hash_match("leaked-value-alpha-0001");
        let a = add_whitelist_entry(
            &mut conn,
            &WhitelistTarget::Hash(hash.clone()),
            Some("fixture"),
        )
        .expect("add");
        let b = add_whitelist_entry(&mut conn, &WhitelistTarget::Rule("rule-b".into()), None)
            .expect("add");

        let all = list_whitelist(&conn).expect("list");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, a);
        assert_eq!(all[0].match_hash.as_deref(), Some(hash.as_str()));
        assert_eq!(all[0].rule_id, None);
        assert_eq!(all[0].reason.as_deref(), Some("fixture"));
        assert_eq!(all[1].id, b);
        assert_eq!(all[1].rule_id.as_deref(), Some("rule-b"));

        assert!(remove_whitelist_entry(&mut conn, a).expect("remove"));
        assert!(!remove_whitelist_entry(&mut conn, a).expect("remove again"));
        assert_eq!(list_whitelist(&conn).expect("list").len(), 1);

        // With the entry gone, the value is no longer suppressed at write time.
        assert!(!is_whitelisted(&conn, "rule-a", &hash).expect("check"));
        assert!(is_whitelisted(&conn, "rule-b", &hash).expect("check"));
    }

    #[test]
    fn removing_a_whitelist_entry_reopens_only_what_nothing_else_covers() {
        let mut conn = db();
        record_scan(&mut conn, "s1", "/a", 1, TEXT, &[alpha(), bravo()]).expect("record");
        conn.execute(
            "UPDATE credential_findings SET status = 'false_positive' WHERE rule_id = 'rule-b'",
            [],
        )
        .expect("mark");

        // Two entries both covering alpha, and one covering the marked bravo.
        let by_hash = add_whitelist_entry(
            &mut conn,
            &WhitelistTarget::Hash(hash_match("leaked-value-alpha-0001")),
            None,
        )
        .expect("by hash");
        let by_rule = add_whitelist_entry(&mut conn, &WhitelistTarget::Rule("rule-a".into()), None)
            .expect("by rule");
        let rule_b = add_whitelist_entry(&mut conn, &WhitelistTarget::Rule("rule-b".into()), None)
            .expect("rule b");

        let statuses = |conn: &Connection| -> Vec<String> {
            stored(conn)
                .into_iter()
                .map(|(_, status, _)| status)
                .collect()
        };
        assert_eq!(statuses(&conn), vec!["whitelisted", "false_positive"]);

        // The rule entry still covers alpha.
        assert!(remove_whitelist_entry(&mut conn, by_hash).expect("remove"));
        assert_eq!(statuses(&conn), vec!["whitelisted", "false_positive"]);

        // Nothing covers it now; the user's false_positive is left alone.
        assert!(remove_whitelist_entry(&mut conn, by_rule).expect("remove"));
        assert!(remove_whitelist_entry(&mut conn, rule_b).expect("remove"));
        assert_eq!(statuses(&conn), vec!["open", "false_positive"]);
    }

    #[test]
    fn a_rescan_retracts_open_findings_it_no_longer_produces() {
        let mut conn = db();
        record_scan(&mut conn, "s1", "/a", 1, TEXT, &[alpha(), bravo()]).expect("record");
        // Another session's findings are not this scan's to retract.
        record_scan(&mut conn, "s1", "/b", 1, TEXT, &[bravo()]).expect("record");
        conn.execute(
            "UPDATE credential_findings SET status = 'false_positive'
              WHERE project_path = '/a' AND rule_id = 'rule-a'",
            [],
        )
        .expect("mark");

        // Under v2 neither rule fires: the verdict survives, the open row goes.
        record_scan(&mut conn, "s1", "/a", 2, TEXT, &[]).expect("rescan");

        let rows: Vec<(String, String, String)> = conn
            .prepare(
                "SELECT project_path, rule_id, status FROM credential_findings
                 ORDER BY project_path, rule_id",
            )
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(
            rows,
            vec![
                ("/a".into(), "rule-a".into(), "false_positive".into()),
                ("/b".into(), "rule-b".into(), "open".into()),
            ]
        );
    }

    #[test]
    fn a_finding_outside_the_text_fails_the_whole_scan() {
        let mut conn = db();
        let bad = Finding {
            rule_id: "rule-a",
            confidence: Confidence::High,
            start: 0,
            end: TEXT.len() + 1,
        };
        record_scan(&mut conn, "s1", "/a", 1, TEXT, &[alpha(), bad]).expect_err("out of range");
        // Nothing committed: not the good finding, not the state row.
        assert!(stored(&conn).is_empty());
        cache_row(&conn, "s1", "/a");
        assert_eq!(pending_ids(&conn, 1), vec![("s1".into(), "/a".into())]);
    }
}
