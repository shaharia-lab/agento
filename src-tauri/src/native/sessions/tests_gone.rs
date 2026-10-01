//! `410 Gone` for an expired transcript (#709).
//!
//! The three routes that need the file — detail, journey, continue — answer
//! 410 with a typed body when the scanner stamped the session's cache row and
//! no config dir holds its transcript, and keep their 404 for everything else.
//! These run through `serve` and `continue_session`, so what they pin is the
//! status and bytes a client sees rather than the helper's return value.
//!
//! Built on `migrate::apply`, so `transcript_expired_at` is migration 46's own
//! column, and on a real transcript on disk under a config dir the settings row
//! names — nothing here reads the environment.

use axum::http::{Method, StatusCode};

use super::{continue_chat, serve};
use crate::native::{Answer, Ctx, Request};

const SESSION: &str = "aaa-session";

/// The stamp the scanner writes: `gotime::now_go_text()`'s shape.
const EXPIRED_AT: &str = "2026-09-01 08:30:00 +0000 UTC";

const TRANSCRIPT: &str = concat!(
    r#"{"type":"user","uuid":"u1","parentUuid":null,"timestamp":"2026-08-01T10:00:00Z","cwd":"/home/u/proj","message":{"role":"user","content":"do the thing"}}"#,
    "\n",
    r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"2026-08-01T10:00:05Z","cwd":"/home/u/proj","message":{"role":"assistant","model":"claude-opus-5","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
    "\n",
);

struct Fixture {
    _dir: tempfile::TempDir,
    ctx: Ctx,
    transcript: std::path::PathBuf,
}

impl Fixture {
    /// A migrated database whose settings row names a config dir, with no
    /// session in it and no transcript on disk.
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let claude = dir.path().join(".claude");
        let project = claude.join("projects").join("-home-u-proj");
        std::fs::create_dir_all(&project).expect("project dir");

        let db = dir.path().join("agento.db");
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO user_settings (id, claude_config_dir) VALUES (1, ?1)",
            [claude.to_string_lossy()],
        )
        .expect("settings row");

        Self {
            _dir: dir,
            ctx: Ctx { db_path: db },
            transcript: project.join(format!("{SESSION}.jsonl")),
        }
    }

    fn conn(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.ctx.db_path).expect("open")
    }

    /// The cache row a scan leaves behind, stamped expired or not.
    fn cached(self, expired: bool) -> Self {
        self.conn()
            .execute(
                "INSERT INTO claude_session_cache
                     (session_id, project_path, file_path, file_mtime, start_time,
                      last_activity, transcript_expired_at)
                 VALUES (?1, '/home/u/proj', ?2, '2026-08-01 10:00:05 +0000 UTC',
                         '2026-08-01 10:00:00 +0000 UTC', '2026-08-01 10:00:05 +0000 UTC', ?3)",
                rusqlite::params![
                    SESSION,
                    self.transcript.to_string_lossy(),
                    expired.then_some(EXPIRED_AT),
                ],
            )
            .expect("insert session");
        self
    }

    fn with_file(self, contents: &str) -> Self {
        std::fs::write(&self.transcript, contents).expect("write transcript");
        self
    }

    fn get(&self, path: &str) -> Answer {
        serve(
            &self.ctx,
            &Request {
                method: &Method::GET,
                path,
                query: "",
                content_type: "",
                secret_token: "",
                body: &[],
            },
        )
        .expect("answer")
    }

    fn detail(&self) -> Answer {
        self.get(&format!("/api/claude-sessions/{SESSION}"))
    }

    fn journey(&self) -> Answer {
        self.get(&format!("/api/claude-sessions/{SESSION}/journey"))
    }

    /// `POST …/continue`, through the same `writes::finish` the route uses.
    fn continue_chat(&self) -> Answer {
        crate::native::writes::finish(continue_chat::continue_session(&self.ctx.db_path, SESSION))
            .expect("answer")
    }

    fn chats(&self) -> i64 {
        self.conn()
            .query_row("SELECT COUNT(*) FROM chat_sessions", [], |r| r.get(0))
            .expect("count")
    }
}

fn body(answer: &Answer) -> &str {
    std::str::from_utf8(answer.body.as_deref().expect("body")).expect("utf-8")
}

const NOT_FOUND: &str = "{\"error\":\"session not found\"}\n";

/// The 410, byte for byte.
///
/// Hand-kept like the other post-Go goldens: the body is Agento's own, so **a
/// change here is a change to the contract** #713 renders from. `error` is the
/// first key, and `expired_at` is spelled as RFC 3339 like every other
/// timestamp on the wire.
#[test]
fn the_three_routes_answer_410_with_the_golden_bytes_for_an_expired_session() {
    let f = Fixture::new().cached(true);
    let want = include_str!("../../../../parity/session_expired_golden.json");

    for (route, answer) in [
        ("detail", f.detail()),
        ("journey", f.journey()),
        ("continue", f.continue_chat()),
    ] {
        assert_eq!(answer.status, StatusCode::GONE, "{route}");
        // `text: false` is what makes `proxy.rs` send `application/json`.
        assert!(!answer.text, "{route} must answer JSON");
        assert_eq!(body(&answer), want, "{route} drifted from its golden");
    }
    assert_eq!(f.chats(), 0, "an expired continue must write no chat row");
}

#[test]
fn an_id_with_no_cache_row_is_still_404() {
    let f = Fixture::new();
    for (route, answer) in [
        ("detail", f.detail()),
        ("journey", f.journey()),
        ("continue", f.continue_chat()),
    ] {
        assert_eq!(answer.status, StatusCode::NOT_FOUND, "{route}");
        assert_eq!(body(&answer), NOT_FOUND, "{route}");
    }
    assert_eq!(f.chats(), 0);
}

/// The scanner decides expiry, not the read: a row whose file is missing but
/// which no scan has stamped yet is the old 404.
#[test]
fn an_unstamped_row_whose_file_is_missing_is_still_404() {
    let f = Fixture::new().cached(false);
    for (route, answer) in [
        ("detail", f.detail()),
        ("journey", f.journey()),
        ("continue", f.continue_chat()),
    ] {
        assert_eq!(answer.status, StatusCode::NOT_FOUND, "{route}");
        assert_eq!(body(&answer), NOT_FOUND, "{route}");
    }
    assert_eq!(f.chats(), 0);
}

/// A file on disk wins over a stamp: a transcript restored before the next
/// scan reads exactly as it did before it went missing.
#[test]
fn a_stamped_row_whose_file_reappeared_answers_as_it_always_did() {
    let f = Fixture::new().cached(true).with_file(TRANSCRIPT);

    assert_eq!(f.detail().status, StatusCode::OK);
    assert_eq!(f.journey().status, StatusCode::OK);
    assert_eq!(f.continue_chat().status, StatusCode::CREATED);
    assert_eq!(f.chats(), 1);
}

/// The journey's other `None` — a file holding no timestamped event — must not
/// be mistaken for a missing file, or a stamped-then-restored empty transcript
/// would read as expired while it sits on disk.
#[test]
fn a_stamped_row_whose_file_has_no_journey_is_404_not_410() {
    let f = Fixture::new().cached(true).with_file("");
    let answer = f.journey();
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(body(&answer), NOT_FOUND);
}

/// "We could not look" is not "it does not exist": a stamp this build cannot
/// parse fails the request rather than quietly answering 404.
#[test]
fn an_unreadable_stamp_is_an_error_not_a_404() {
    let f = Fixture::new().cached(false);
    f.conn()
        .execute(
            "UPDATE claude_session_cache SET transcript_expired_at = 'not a time'",
            [],
        )
        .expect("corrupt the stamp");

    let err = super::detail::expiry(&f.ctx.db_path, SESSION).expect_err("must not be a 404");
    assert!(err.contains("transcript expiry"), "unexpected error: {err}");
}

/// A session id reaches SQL only after `validSessionID`. The row is stamped
/// under the invalid id itself, so without the guard the query would find it.
#[test]
fn an_invalid_session_id_is_never_expired() {
    let f = Fixture::new();
    let conn = f.conn();
    stamped_row(&conn, "../etc", "/home/u/proj", EXPIRED_AT);

    assert_eq!(
        super::detail::expired_at(&conn, "../etc").expect("lookup"),
        None
    );
}

/// One id under two projects (the #362 family), stamped by different scans:
/// the newest stamp is the one reported, whatever order the rows come back in.
#[test]
fn the_newest_stamp_wins_for_an_id_under_two_projects() {
    let f = Fixture::new();
    let conn = f.conn();
    stamped_row(
        &conn,
        SESSION,
        "/home/u/later",
        "2026-09-02 08:30:00 +0000 UTC",
    );
    stamped_row(&conn, SESSION, "/home/u/earlier", EXPIRED_AT);
    stamped_row(
        &conn,
        SESSION,
        "/home/u/middle",
        "2026-09-01 20:00:00 +0000 UTC",
    );

    let answer = f.detail();
    assert_eq!(answer.status, StatusCode::GONE);
    assert!(
        body(&answer).contains(r#""expired_at":"2026-09-02T08:30:00Z""#),
        "{}",
        body(&answer)
    );
}

fn stamped_row(conn: &rusqlite::Connection, id: &str, project: &str, stamp: &str) {
    conn.execute(
        "INSERT INTO claude_session_cache
             (session_id, project_path, file_path, file_mtime, start_time,
              last_activity, transcript_expired_at)
         VALUES (?1, ?2, 'gone.jsonl', '2026-08-01 10:00:05 +0000 UTC',
                 '2026-08-01 10:00:00 +0000 UTC', '2026-08-01 10:00:05 +0000 UTC', ?3)",
        rusqlite::params![id, project, stamp],
    )
    .expect("insert stamped row");
}
