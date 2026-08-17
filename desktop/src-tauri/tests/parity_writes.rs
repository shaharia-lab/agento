//! Live parity for the **writes** (#274): drive each ported write against a
//! running Go server and against Rust, and compare the whole answer.
//!
//! ```sh
//! cd desktop && eval "$(./scripts/parity-instance.sh start)"
//! (cd src-tauri && cargo test --test parity_writes -- --ignored --nocapture)
//! ./scripts/parity-instance.sh stop
//! ```
//!
//! # Why this suite exists rather than trusting the unit tests
//!
//! The unit tests are good — real migrations rather than a fixture table, Go's
//! byte-walk slug, the nil-versus-empty capability shapes. But they encode *this
//! port's reading of Go*, and two divergences got through them anyway: a missing
//! chat answered 400 where Go answers 404 (the test asserted the message and not
//! the status), and the PATCH response carried the row's old `updated_at` where
//! Go carries the write's (the test asserted the stored title and never looked
//! at the body). Both are cases where the port and its tests were wrong in the
//! same direction. Only asking the real server breaks that circularity.
//!
//! # These tests WRITE, so they need their own instance
//!
//! Every other parity suite is read-only and safe against anything.
//! **This one is not.** It creates, renames and deletes rows, so it refuses to
//! run unless `AGENTO_LIVE_URL` is set — which `parity-instance.sh start`
//! exports, and which the default `:8990` fallback deliberately does not
//! satisfy. `parity-instance.sh` runs a Go server built from this checkout
//! against a **copy** of `~/.agento`, so the writes land on a scratch database.
//!
//! Each case cleans up after itself, and every id is prefixed so a leaked row is
//! obviously this suite's.
//!
//! # What is compared, and how — this is NOT the read suites' shape
//!
//! The read suites ask Go and Rust the same question and diff the two bodies.
//! A write cannot work that way: whichever implementation runs first changes
//! the state the second would see, so there is no shared question to ask.
//!
//! So these tests **pin Go's real answers as literals** — status and exact
//! bytes — and the unit tests in `native/agents.rs` and `native/chats.rs`
//! assert the *same* literals against Rust. The comparison is real, it just
//! happens across two suites instead of inside one, and this is the half that
//! cannot be wrong about what Go does, because it asked.
//!
//! Status is checked as carefully as the body. A create answered 200 instead of
//! 201 is a divergence no body comparison can see, and it is exactly what the
//! seam's new status plumbing could get wrong.

mod parity_common;

use parity_common::*;

use agento_lib::native::db;
use reqwest::Method;
use rusqlite::OptionalExtension;

/// Set an integration's `auth` column directly.
///
/// There is no endpoint that writes it: every path that does is an OAuth
/// callback or a token validator that dials the provider, and neither belongs in
/// a parity run. `db::open_read_write` is the helper the app's own native writes
/// use against this same file while the sidecar holds it — pragmas and busy
/// timeout included. A bare `rusqlite::Connection::open` is **not** the same
/// thing and must not be substituted here; see [`the_integration_id_write_answers_match_go`].
fn seed_auth(id: &str, auth: Option<&str>) {
    db::open_read_write(&live_db())
        .expect("opening the live database")
        .execute(
            "UPDATE integrations SET auth = ?2 WHERE id = ?1",
            rusqlite::params![id, auth],
        )
        .expect("seeding auth");
}

/// Refuse to run against anything but a scratch instance.
///
/// The read suites fall back to `:8990` — the developer's real Agento — which
/// is harmless for a GET and would be data loss here.
fn require_scratch_instance() {
    assert!(
        std::env::var("AGENTO_LIVE_URL").is_ok(),
        "parity_writes mutates data and refuses to guess an instance. \
         Run `eval \"$(./scripts/parity-instance.sh start)\"` first — it points \
         AGENTO_LIVE_URL at a Go server running on a copy of ~/.agento."
    );
}

/// Ask the Go server, and hand back exactly what it said.
///
/// `AGENTO_LIVE_URL` points at the Go parity instance, so this is Go's answer
/// and nothing else — the assertions around each call are the record of it.
async fn go_answer(method: Method, path: &str, body: Option<&str>) -> (u16, Vec<u8>) {
    send(method, path, body).await
}

// ─── Agents ───────────────────────────────────────────────────────────────────

/// Create, update and delete an agent, comparing every answer.
///
/// Driven as one case rather than three because the three share a lifecycle:
/// the update needs something to update, and leaving a test agent behind on a
/// scratch database would still be leaving it behind.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database, and it writes"]
async fn the_agent_write_answers_match_go() {
    require_scratch_instance();

    let slug = "parity-writes-agent";
    // A previous failed run may have left it behind.
    let _ = go_answer(Method::DELETE, &format!("/api/agents/{slug}"), None).await;

    let created = go_answer(
        Method::POST,
        "/api/agents",
        Some(&format!(
            r#"{{"name":"Parity Writes","slug":"{slug}","description":"d","system_prompt":"p"}}"#
        )),
    )
    .await;
    assert_eq!(created.0, 201, "create must be 201, got {}", created.0);
    println!("create: {}", String::from_utf8_lossy(&created.1));

    // The defaults the *service* applies, visible on the wire: an empty model
    // becomes claude-sonnet-4-6, empty thinking becomes adaptive, and
    // permission_mode stays "" because the request type cannot carry one.
    let body: serde_json::Value = serde_json::from_slice(&created.1).expect("json");
    assert_eq!(body["model"], "claude-sonnet-4-6");
    assert_eq!(body["thinking"], "adaptive");
    assert_eq!(body["permission_mode"], "");

    // A duplicate is a 409 with the service error's exact wording.
    let conflict = go_answer(
        Method::POST,
        "/api/agents",
        Some(&format!(r#"{{"name":"Again","slug":"{slug}"}}"#)),
    )
    .await;
    assert_eq!(conflict.0, 409, "duplicate slug must be 409");
    assert_eq!(
        String::from_utf8_lossy(&conflict.1).trim_end(),
        format!(r#"{{"error":"agent with id \"{slug}\" already exists"}}"#)
    );

    // A missing name is a 422 — not a 400, which is the distinction the port
    // has to keep.
    let invalid = go_answer(Method::POST, "/api/agents", Some(r#"{"description":"x"}"#)).await;
    assert_eq!(invalid.0, 422, "a validation failure must be 422");
    assert_eq!(
        String::from_utf8_lossy(&invalid.1).trim_end(),
        r#"{"error":"validation error for \"name\": name is required"}"#
    );

    // A JSON array body is a 400 in Go. This is the divergence serde's
    // positional struct deserialization would have hidden.
    let array_body = go_answer(Method::POST, "/api/agents", Some(r#"["Sneaky"]"#)).await;
    assert_eq!(array_body.0, 400, "an array body must be 400, not a create");

    let updated = go_answer(
        Method::PUT,
        &format!("/api/agents/{slug}"),
        Some(r#"{"name":"Renamed","description":"changed"}"#),
    )
    .await;
    assert_eq!(updated.0, 200, "update must be 200");
    let body: serde_json::Value = serde_json::from_slice(&updated.1).expect("json");
    assert_eq!(body["name"], "Renamed");
    assert_eq!(body["slug"], slug, "the path slug wins over the body");

    let deleted = go_answer(Method::DELETE, &format!("/api/agents/{slug}"), None).await;
    assert_eq!(deleted.0, 204, "delete must be 204");
    assert!(deleted.1.is_empty(), "204 carries no body");
}

// ─── Chats ────────────────────────────────────────────────────────────────────

/// Create, patch and delete a chat.
///
/// The patch cases are the ones this suite was added for: both divergences the
/// unit tests missed live here.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database, and it writes"]
async fn the_chat_write_answers_match_go() {
    require_scratch_instance();

    let created = go_answer(
        Method::POST,
        "/api/chats",
        Some(r#"{"working_directory":"/tmp","model":"parity-writes"}"#),
    )
    .await;
    assert_eq!(created.0, 201, "create must be 201");
    let session: serde_json::Value = serde_json::from_slice(&created.1).expect("json");
    let id = session["id"].as_str().expect("id").to_string();
    assert_eq!(session["title"], "New Chat");
    // omitempty: none of the zero counters is on the wire.
    assert!(session.get("total_input_tokens").is_none());
    assert!(session.get("is_favorite").is_none());
    let created_updated_at = session["updated_at"]
        .as_str()
        .expect("updated_at")
        .to_string();

    let patched = go_answer(
        Method::PATCH,
        &format!("/api/chats/{id}"),
        Some(r#"{"title":"  Renamed  "}"#),
    )
    .await;
    assert_eq!(patched.0, 200);
    let body: serde_json::Value = serde_json::from_slice(&patched.1).expect("json");
    assert_eq!(body["title"], "Renamed", "the title is stored trimmed");
    // The divergence: Go's response carries the *write's* timestamp, because
    // chatService.UpdateSession stamps the struct the handler then serializes.
    assert_ne!(
        body["updated_at"].as_str().expect("updated_at"),
        created_updated_at,
        "the PATCH response must carry the new updated_at, not the row's old one"
    );

    // The other divergence: a missing chat is a 404 with a bare message.
    let missing = go_answer(
        Method::PATCH,
        "/api/chats/parity-writes-no-such-chat",
        Some(r#"{"title":"x"}"#),
    )
    .await;
    assert_eq!(missing.0, 404, "a missing chat must be 404, not 400");
    assert_eq!(
        String::from_utf8_lossy(&missing.1).trim_end(),
        r#"{"error":"chat not found"}"#
    );

    // Handler-level rejections, which really are 400.
    for (body, want) in [
        ("{}", "no fields to update"),
        (r#"{"title":"   "}"#, "title cannot be empty"),
    ] {
        let rejected = go_answer(Method::PATCH, &format!("/api/chats/{id}"), Some(body)).await;
        assert_eq!(rejected.0, 400, "body {body} must be 400");
        assert_eq!(
            String::from_utf8_lossy(&rejected.1).trim_end(),
            format!(r#"{{"error":"{want}"}}"#)
        );
    }

    let deleted = go_answer(Method::DELETE, &format!("/api/chats/{id}"), None).await;
    assert_eq!(deleted.0, 204);
    assert!(deleted.1.is_empty());
}

/// The bulk delete, including its two bounds.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database, and it writes"]
async fn the_bulk_chat_delete_answers_match_go() {
    require_scratch_instance();

    let mut ids = Vec::new();
    for _ in 0..2 {
        let created = go_answer(
            Method::POST,
            "/api/chats",
            Some(r#"{"model":"parity-bulk"}"#),
        )
        .await;
        let session: serde_json::Value = serde_json::from_slice(&created.1).expect("json");
        ids.push(session["id"].as_str().expect("id").to_string());
    }

    for (body, want_status, want_error) in [
        (
            r#"{"ids":[]}"#.to_string(),
            400,
            Some("ids must not be empty"),
        ),
        (
            format!(r#"{{"ids":[{}]}}"#, vec!["\"x\""; 501].join(",")),
            400,
            Some("too many ids (max 500)"),
        ),
    ] {
        let rejected = go_answer(Method::DELETE, "/api/chats", Some(&body)).await;
        assert_eq!(rejected.0, want_status);
        if let Some(want) = want_error {
            assert_eq!(
                String::from_utf8_lossy(&rejected.1).trim_end(),
                format!(r#"{{"error":"{want}"}}"#)
            );
        }
    }

    // An id that does not exist is not an error for the bulk delete, unlike the
    // single one.
    let payload = format!(
        r#"{{"ids":["{}","{}","parity-writes-never-existed"]}}"#,
        ids[0], ids[1]
    );
    let deleted = go_answer(Method::DELETE, "/api/chats", Some(&payload)).await;
    assert_eq!(deleted.0, 204);
    assert!(deleted.1.is_empty());
}

// ─── Job history ──────────────────────────────────────────────────────────────

/// The one delete in this PR that is a genuine 404 rather than a forwarded 500,
/// because its service checks the row exists first.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database, and it writes"]
async fn the_job_history_delete_answers_match_go() {
    require_scratch_instance();

    let missing = go_answer(
        Method::DELETE,
        "/api/job-history/parity-writes-no-such-entry",
        None,
    )
    .await;
    assert_eq!(missing.0, 404, "job history's delete really is a 404");
    assert_eq!(
        String::from_utf8_lossy(&missing.1).trim_end(),
        r#"{"error":"job_history \"parity-writes-no-such-entry\" not found"}"#
    );

    for (body, want) in [
        (r#"{"ids":[]}"#, "ids must not be empty"),
        (r#"{}"#, "ids must not be empty"),
    ] {
        let rejected = go_answer(Method::DELETE, "/api/job-history", Some(body)).await;
        assert_eq!(rejected.0, 400);
        assert_eq!(
            String::from_utf8_lossy(&rejected.1).trim_end(),
            format!(r#"{{"error":"{want}"}}"#)
        );
    }
}

// ─── Integrations and trigger rules (#277) ────────────────────────────────────

/// Pin Go's answers for the integration writes that moved, and for the two that
/// deliberately did not.
///
/// The per-type credential validators are where this port's parity risk
/// concentrates — seven types, each with its own required fields and its own
/// exact 422 wording — so the messages are asserted verbatim rather than by
/// status alone.
#[tokio::test]
#[ignore = "requires a scratch Go instance; mutates it"]
async fn the_integration_write_answers_match_go() {
    require_scratch_instance();

    // A missing name is a 422 from the service, and it is checked before type.
    let no_name = go_answer(
        Method::POST,
        "/api/integrations",
        Some(r#"{"type":"telegram"}"#),
    )
    .await;
    assert_eq!(no_name.0, 422);
    assert_eq!(
        String::from_utf8_lossy(&no_name.1).trim_end(),
        r#"{"error":"validation error for \"name\": name is required"}"#
    );

    let no_type = go_answer(Method::POST, "/api/integrations", Some(r#"{"name":"n"}"#)).await;
    assert_eq!(no_type.0, 422);
    assert_eq!(
        String::from_utf8_lossy(&no_type.1).trim_end(),
        r#"{"error":"validation error for \"type\": type is required"}"#
    );

    // An **absent** credentials blob is "credentials are empty"…
    let absent = go_answer(
        Method::POST,
        "/api/integrations",
        Some(r#"{"name":"n","type":"telegram"}"#),
    )
    .await;
    assert_eq!(absent.0, 422);
    assert_eq!(
        String::from_utf8_lossy(&absent.1).trim_end(),
        r#"{"error":"validation error for \"credentials\": invalid telegram credentials: credentials are empty"}"#
    );

    // …but a literal `null` is four bytes, so it decodes to the zero value and
    // reports the missing *field* instead. This pair is the whole reason the
    // port captures the raw blob rather than letting serde fold null into None.
    let null_creds = go_answer(
        Method::POST,
        "/api/integrations",
        Some(r#"{"name":"n","type":"telegram","credentials":null}"#),
    )
    .await;
    assert_eq!(null_creds.0, 422);
    assert_eq!(
        String::from_utf8_lossy(&null_creds.1).trim_end(),
        r#"{"error":"validation error for \"credentials.bot_token\": bot_token is required"}"#
    );

    let created = go_answer(
        Method::POST,
        "/api/integrations",
        // Multi-key, out of order, with a trailing-zero decimal and interior
        // whitespace: a single-key blob is a fixed point of a `Value` round
        // trip and would prove nothing about verbatim storage.
        Some(
            r#"{"name":"Parity Writes","type":"telegram",
                 "credentials":{"zebra":"z", "bot_token":"tok","rate":1.50}}"#,
        ),
    )
    .await;
    assert_eq!(created.0, 201, "create must be 201, got {}", created.0);
    println!("create: {}", String::from_utf8_lossy(&created.1));
    let body: serde_json::Value = serde_json::from_slice(&created.1).expect("json");
    let id = body["id"].as_str().expect("an id").to_string();
    // An omitted services map is `{}`, never null, and no secret is echoed.
    assert_eq!(body["services"], serde_json::json!({}));
    assert_eq!(body["authenticated"], false);
    assert!(
        body.get("credentials").is_none(),
        "credentials must be scrubbed"
    );

    // ── Trigger rules ──
    let rule = go_answer(
        Method::POST,
        &format!("/api/integrations/{id}/triggers"),
        Some(r#"{"name":"R","agent_slug":"a","enabled":true}"#),
    )
    .await;
    assert_eq!(rule.0, 201, "rule create must be 201");
    let rule_body: serde_json::Value = serde_json::from_slice(&rule.1).expect("json");
    let rid = rule_body["id"].as_str().expect("a rule id").to_string();

    // An omitted field is cleared by an update, not preserved.
    let updated = go_answer(
        Method::PUT,
        &format!("/api/integrations/{id}/triggers/{rid}"),
        Some(r#"{"agent_slug":"b"}"#),
    )
    .await;
    assert_eq!(updated.0, 200);
    let updated_body: serde_json::Value = serde_json::from_slice(&updated.1).expect("json");
    assert_eq!(updated_body["agent_slug"], "b");
    assert_eq!(updated_body["name"], "", "an omitted name is cleared");

    // A rule id that exists but belongs elsewhere is 403 — and the check runs
    // before the body is decoded, so garbage still gets 403 rather than 400.
    let other = go_answer(
        Method::POST,
        "/api/integrations",
        Some(r#"{"name":"Other","type":"telegram","credentials":{"bot_token":"t2"}}"#),
    )
    .await;
    let other_id = serde_json::from_slice::<serde_json::Value>(&other.1).expect("json")["id"]
        .as_str()
        .expect("id")
        .to_string();
    for body in [r#"{"agent_slug":"c"}"#, "not json at all"] {
        let forbidden = go_answer(
            Method::PUT,
            &format!("/api/integrations/{other_id}/triggers/{rid}"),
            Some(body),
        )
        .await;
        assert_eq!(forbidden.0, 403, "body {body:?} must still be 403");
        assert_eq!(
            String::from_utf8_lossy(&forbidden.1).trim_end(),
            r#"{"error":"rule does not belong to this integration"}"#
        );
    }

    let missing = go_answer(
        Method::PUT,
        &format!("/api/integrations/{id}/triggers/no-such-rule"),
        Some(r#"{"agent_slug":"c"}"#),
    )
    .await;
    assert_eq!(missing.0, 404);
    assert_eq!(
        String::from_utf8_lossy(&missing.1).trim_end(),
        r#"{"error":"trigger_rule \"no-such-rule\" not found"}"#
    );

    let deleted = go_answer(
        Method::DELETE,
        &format!("/api/integrations/{id}/triggers/{rid}"),
        None,
    )
    .await;
    assert_eq!(deleted.0, 204);
    assert!(deleted.1.is_empty(), "204 carries no body");

    // Clean up through Go, which is also the side that owns these deletes.
    for cleanup in [&id, &other_id] {
        let gone = go_answer(
            Method::DELETE,
            &format!("/api/integrations/{cleanup}"),
            None,
        )
        .await;
        assert_eq!(gone.0, 204, "integration delete must be 204");
    }
}

/// Pin Go's answers for `PUT` and `DELETE /api/integrations/{id}` (#311).
///
/// Its own case rather than an addition to the one above, because the interest
/// is entirely in what an *existing* row does across a replace — and because
/// three of the answers are things only the database can show. The four
/// assertions worth naming, each of which reads as a bug until it is checked
/// against the real server:
///
/// - **A `PUT` that omits `credentials` wipes them.** The store's upsert
///   overwrites the column wholesale, and `Update` runs no validator, so an
///   omitted key stores `""`.
/// - **An omitted `services` is `null`, not `{}`.** `Create` fills a nil map
///   before saving and `Update` does not, so the same absence means different
///   things on the two verbs.
/// - **`auth` survives every `PUT`.** The request type has no `auth` field, so
///   `cfg.IsAuthenticated()` is always false on this path and the token is
///   always preserved — except that the two non-token spellings (`''` and the
///   literal `null`) fail that same test and become SQL `NULL`.
/// - **Nothing is validated.** An empty name, an empty type and a `{}`
///   credentials blob are all 200s here and 422s on a create.
#[tokio::test]
#[ignore = "requires a scratch Go instance; mutates it"]
async fn the_integration_id_write_answers_match_go() {
    require_scratch_instance();

    // Read a column straight out of the instance's database. Three of this
    // case's answers are invisible on the wire by design — `credentials` and
    // `auth` are scrubbed from every response — so the only way to see them is
    // to look.
    // **Read-only, through the crate's own helper**, never a bare
    // `rusqlite::Connection::open`. That opens `SQLITE_OPEN_READWRITE|CREATE`
    // and, against a WAL database the Go server currently holds, was observed to
    // reset the log: the row created two lines earlier was gone from Go's *own*
    // view immediately afterwards. Reading is the only thing wanted here anyway.
    let column = |sql: &'static str, id: String| {
        let db = live_db();
        move || -> Option<String> {
            db::open_read_only(&db)
                .expect("opening the live database")
                .query_row(sql, [&id], |row| row.get::<_, Option<String>>(0))
                .optional()
                .expect("querying the live database")
                .flatten()
        }
    };

    let created = go_answer(
        Method::POST,
        "/api/integrations",
        Some(
            r#"{"name":"P311","type":"telegram",
                 "credentials":{"zebra":"z", "bot_token":"tok","rate":1.50},
                 "services":{"messaging":{"enabled":true,"tools":["send_message"]}}}"#,
        ),
    )
    .await;
    assert_eq!(created.0, 201);
    let id = serde_json::from_slice::<serde_json::Value>(&created.1).expect("json")["id"]
        .as_str()
        .expect("an id")
        .to_string();
    let created_at = serde_json::from_slice::<serde_json::Value>(&created.1).expect("json")
        ["created_at"]
        .as_str()
        .expect("created_at")
        .to_string();

    let credentials = column(
        "SELECT credentials FROM integrations WHERE id = ?1",
        id.clone(),
    );
    let auth = column("SELECT auth FROM integrations WHERE id = ?1", id.clone());
    let services = column(
        "SELECT services FROM integrations WHERE id = ?1",
        id.clone(),
    );

    // ── The happy path: a full replace ──
    let updated = go_answer(
        Method::PUT,
        &format!("/api/integrations/{id}"),
        Some(
            r#"{"name":"Renamed","type":"telegram","enabled":true,
                 "credentials":{"zebra":"z", "bot_token":"new","rate":1.50},
                 "services":{"messaging":{"enabled":true,"tools":["read_chat"]}}}"#,
        ),
    )
    .await;
    assert_eq!(updated.0, 200, "update must be 200, got {}", updated.0);
    println!("update: {}", String::from_utf8_lossy(&updated.1));
    let body: serde_json::Value = serde_json::from_slice(&updated.1).expect("json");
    assert_eq!(body["name"], "Renamed");
    assert_eq!(body["enabled"], true);
    assert_eq!(
        body["services"],
        serde_json::json!({"messaging": {"enabled": true, "tools": ["read_chat"]}})
    );
    assert_eq!(
        body["created_at"], created_at,
        "created_at is preserved; it is not even in the upsert's DO UPDATE SET list"
    );
    assert_ne!(body["updated_at"], created_at, "updated_at is the write's");
    assert!(
        body.get("credentials").is_none(),
        "credentials are scrubbed"
    );
    assert!(body.get("auth").is_none(), "auth is scrubbed");
    // Stored verbatim: key order, `1.50` and the space after the first comma.
    assert_eq!(
        credentials().as_deref(),
        Some(r#"{"zebra":"z", "bot_token":"new","rate":1.50}"#)
    );

    // ── The token survives, and the credential does not ──
    // Seeded directly, because no endpoint writes `auth`: every path that does
    // is an OAuth callback or a token validator that dials the provider.
    // `open_read_write` is the helper the app's own native writes use against
    // this same file while the sidecar holds it, pragmas and all.
    seed_auth(&id, Some(r#"{"access_token":"KEEP-ME"}"#));

    let stripped = go_answer(
        Method::PUT,
        &format!("/api/integrations/{id}"),
        Some(r#"{"name":"N2","type":"telegram"}"#),
    )
    .await;
    assert_eq!(stripped.0, 200);
    let body: serde_json::Value = serde_json::from_slice(&stripped.1).expect("json");
    assert_eq!(body["authenticated"], true, "the token is preserved");
    assert_eq!(
        body["services"],
        serde_json::Value::Null,
        "Update does not default a nil services map the way Create does"
    );
    assert_eq!(services().as_deref(), Some("null"));
    assert_eq!(
        credentials().as_deref(),
        Some(""),
        "a PUT that omits credentials wipes them"
    );
    assert_eq!(auth().as_deref(), Some(r#"{"access_token":"KEEP-ME"}"#));

    // An empty *object* is still an empty object — the nil-versus-empty
    // distinction is untouched.
    let empty_services = go_answer(
        Method::PUT,
        &format!("/api/integrations/{id}"),
        Some(r#"{"name":"N2","type":"telegram","services":{}}"#),
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&empty_services.1).expect("json")["services"],
        serde_json::json!({})
    );

    // ── The two non-token spellings become SQL NULL ──
    for spelling in ["null", ""] {
        seed_auth(&id, Some(spelling));
        let answer = go_answer(
            Method::PUT,
            &format!("/api/integrations/{id}"),
            Some(r#"{"name":"N3","type":"telegram"}"#),
        )
        .await;
        let body: serde_json::Value = serde_json::from_slice(&answer.1).expect("json");
        assert_eq!(body["authenticated"], false, "auth {spelling:?}");
        assert_eq!(auth(), None, "auth {spelling:?} must become SQL NULL");
    }

    // ── Nothing is validated on an update ──
    for body in [
        r#"{}"#,
        r#"{"name":"","type":""}"#,
        r#"{"name":"N","type":"telegram","credentials":{}}"#,
        r#"{"name":"N","type":"jira","credentials":{"site_url":"nonsense"}}"#,
        r#"{"name":"N","type":"telegram","credentials":null}"#,
    ] {
        let accepted = go_answer(Method::PUT, &format!("/api/integrations/{id}"), Some(body)).await;
        assert_eq!(
            accepted.0, 200,
            "{body:?} is a 422 on a create and a 200 on an update"
        );
    }

    // ── The failure paths ──
    //
    // The decode runs before the lookup, so a malformed body aimed at a missing
    // row is a 400 and not a 404.
    for path in [
        format!("/api/integrations/{id}"),
        "/api/integrations/nope".into(),
    ] {
        let malformed = go_answer(Method::PUT, &path, Some("not json at all")).await;
        assert_eq!(malformed.0, 400, "{path}");
        assert_eq!(
            String::from_utf8_lossy(&malformed.1).trim_end(),
            r#"{"error":"invalid JSON body"}"#
        );
    }

    let missing = go_answer(
        Method::PUT,
        "/api/integrations/nope",
        Some(r#"{"name":"N"}"#),
    )
    .await;
    assert_eq!(missing.0, 404);
    assert_eq!(
        String::from_utf8_lossy(&missing.1).trim_end(),
        r#"{"error":"integration \"nope\" not found"}"#
    );

    let missing = go_answer(Method::DELETE, "/api/integrations/nope", None).await;
    assert_eq!(missing.0, 404);
    assert_eq!(
        String::from_utf8_lossy(&missing.1).trim_end(),
        r#"{"error":"integration \"nope\" not found"}"#
    );

    // ── The delete ──
    let deleted = go_answer(Method::DELETE, &format!("/api/integrations/{id}"), None).await;
    assert_eq!(deleted.0, 204, "delete must be 204");
    assert!(deleted.1.is_empty(), "204 carries no body");
    assert_eq!(credentials(), None, "the row is gone");

    let gone = go_answer(Method::GET, &format!("/api/integrations/{id}"), None).await;
    assert_eq!(gone.0, 404);
}

// ─── Pricing rates (#306) ─────────────────────────────────────────────────────

/// Pin Go's answers for the three rate writes.
///
/// The add-versus-correct split is the whole point of this surface, and its two
/// refusals are what a port most easily collapses into one upsert — so both are
/// driven here, including the **body** of the collision, which is not the bare
/// `{"error": …}` every other 409 in this suite carries.
///
/// The pattern is deliberately one no seed row can collide with: the catalog on
/// a copied database is already populated, and an add over a built-in row would
/// be asserting against whatever the seed happens to hold today.
#[tokio::test]
#[ignore = "requires a scratch Go instance; mutates it"]
async fn the_pricing_rate_write_answers_match_go() {
    require_scratch_instance();

    let pattern = "parity-writes-model";
    let key = format!("{pattern}@2026-01-01T00:00:00Z");
    let rate = |output: &str| {
        format!(
            r#"{{"provider":"parity","model_pattern":"{pattern}","match_type":"prefix",
                 "display_name":"Parity Writes Model","input_per_mtok":5,
                 "output_per_mtok":{output},"cache_write_5m_per_mtok":6.25,
                 "cache_write_1h_per_mtok":10,"cache_read_per_mtok":0.5,
                 "effective_from":"2026-01-01","source":"parity"}}"#
        )
    };
    // A previous failed run may have left the row behind.
    let _ = go_answer(
        Method::DELETE,
        &format!("/api/pricing/rates?model_pattern={pattern}&effective_from=2026-01-01"),
        None,
    )
    .await;

    let created = go_answer(Method::POST, "/api/pricing/rates", Some(&rate("25"))).await;
    assert_eq!(created.0, 201, "an appended rate is 201");
    let body: serde_json::Value = serde_json::from_slice(&created.1).expect("json");
    // A bare date is midnight UTC, and the row comes back with the flags the
    // store applied rather than an echo of the request.
    assert_eq!(body["effective_from"], "2026-01-01T00:00:00Z");
    assert_eq!(body["user_modified"], true);
    assert_eq!(body["is_builtin"], false);
    assert_eq!(body["billable"], true, "a nil *bool means billable");
    assert_eq!(body["match_type"], "prefix");
    // An untiered rate omits the key entirely rather than sending `[]`.
    assert!(body.get("tiers").is_none(), "{body}");

    // The collision carries the colliding row, so the UI can offer to correct
    // it. Keys are a Go map's, so `error` precedes `existing`.
    let conflict = go_answer(Method::POST, "/api/pricing/rates", Some(&rate("99"))).await;
    assert_eq!(conflict.0, 409, "add refuses to overwrite");
    let text = String::from_utf8_lossy(&conflict.1);
    assert!(
        text.starts_with(&format!(
            r#"{{"error":"rate with id \"{key}\" already exists","existing":{{"id":"#
        )),
        "{text}"
    );
    let conflict_body: serde_json::Value = serde_json::from_slice(&conflict.1).expect("json");
    assert_eq!(
        conflict_body["existing"]["output_per_mtok"], 25.0,
        "the add must not have overwritten the rate it collided with"
    );

    // Correcting is the other half, and it refuses to create.
    let missing = go_answer(
        Method::PUT,
        "/api/pricing/rates",
        Some(&rate("25").replace("2026-01-01", "2030-06-15")),
    )
    .await;
    assert_eq!(missing.0, 404, "correct refuses to create");
    assert_eq!(
        String::from_utf8_lossy(&missing.1).trim_end(),
        format!(r#"{{"error":"rate \"{pattern}@2030-06-15T00:00:00Z\" not found"}}"#)
    );

    let corrected = go_answer(Method::PUT, "/api/pricing/rates", Some(&rate("30"))).await;
    assert_eq!(corrected.0, 200, "a correction is 200, not 201");
    let corrected_body: serde_json::Value = serde_json::from_slice(&corrected.1).expect("json");
    assert_eq!(corrected_body["output_per_mtok"], 30.0);
    assert_eq!(
        corrected_body["id"], body["id"],
        "a correction edits the row rather than appending one"
    );

    // The handler's own 422s, which ship without the `validation error for …`
    // prefix a service error carries — and the service's, which keep it.
    let bad_date = go_answer(
        Method::POST,
        "/api/pricing/rates",
        Some(&rate("25").replace(
            r#""effective_from":"2026-01-01""#,
            r#""effective_from":"nope""#,
        )),
    )
    .await;
    assert_eq!(bad_date.0, 422);
    assert_eq!(
        String::from_utf8_lossy(&bad_date.1).trim_end(),
        r#"{"error":"effective_from must be YYYY-MM-DD or RFC3339"}"#
    );

    let not_billable = go_answer(
        Method::POST,
        "/api/pricing/rates",
        Some(&rate("25").replace(
            r#""source":"parity""#,
            r#""source":"parity","billable":false"#,
        )),
    )
    .await;
    assert_eq!(not_billable.0, 422);
    assert_eq!(
        String::from_utf8_lossy(&not_billable.1).trim_end(),
        r#"{"error":"validation error for \"billable\": a non-billable model must have every rate set to zero"}"#
    );

    let bad_json = go_answer(Method::POST, "/api/pricing/rates", Some("[]")).await;
    assert_eq!(bad_json.0, 400, "a malformed body is 400, not 422");
    assert_eq!(
        String::from_utf8_lossy(&bad_json.1).trim_end(),
        r#"{"error":"invalid JSON body"}"#
    );

    // Deleting takes its key from the query, because a model pattern is not
    // path-safe. A missing one is a real 404; the key's own failures are 422.
    let no_key = go_answer(Method::DELETE, "/api/pricing/rates", None).await;
    assert_eq!(no_key.0, 422);
    assert_eq!(
        String::from_utf8_lossy(&no_key.1).trim_end(),
        r#"{"error":"effective_from is required"}"#
    );

    let no_pattern = go_answer(
        Method::DELETE,
        "/api/pricing/rates?effective_from=2026-01-01",
        None,
    )
    .await;
    assert_eq!(no_pattern.0, 422);
    assert_eq!(
        String::from_utf8_lossy(&no_pattern.1).trim_end(),
        r#"{"error":"validation error for \"model_pattern\": model_pattern is required"}"#
    );

    let gone = go_answer(
        Method::DELETE,
        &format!("/api/pricing/rates?model_pattern={pattern}&effective_from=2026-01-01"),
        None,
    )
    .await;
    assert_eq!(gone.0, 204);
    assert!(gone.1.is_empty(), "204 carries no body");

    let again = go_answer(
        Method::DELETE,
        &format!("/api/pricing/rates?model_pattern={pattern}&effective_from=2026-01-01"),
        None,
    )
    .await;
    assert_eq!(again.0, 404);
    assert_eq!(
        String::from_utf8_lossy(&again.1).trim_end(),
        format!(r#"{{"error":"rate \"{key}\" not found"}}"#)
    );
}

// ─── Notification settings (#307) ─────────────────────────────────────────────

/// Pin Go's answers for the notification settings write.
///
/// The masked-password round trip is what this exists for. The UI never holds
/// the real password — `GET` answers `"***"` — so an ordinary save posts the
/// sentinel back, and a port that stored it verbatim would replace the user's
/// password with three asterisks. Nothing would report it: the save succeeds,
/// the form redisplays `"***"`, and the next send fails authentication.
///
/// `POST /api/notifications/test` is deliberately **not** driven here. It dials
/// a real SMTP server, so on a scratch instance it can only fail — and its
/// failure body is go-mail's and the Go runtime's wording, which is exactly why
/// the native handler forwards a failure rather than answering it. What is
/// asserted is that shape: a 400 whose text is environment-dependent.
#[tokio::test]
#[ignore = "requires a scratch Go instance; mutates it"]
async fn the_notification_settings_write_answers_match_go() {
    require_scratch_instance();

    let original = go_answer(Method::GET, "/api/notifications/settings", None).await;
    assert_eq!(original.0, 200);
    let original_body = String::from_utf8_lossy(&original.1).into_owned();

    let saved = go_answer(
        Method::PUT,
        "/api/notifications/settings",
        Some(
            r#"{"enabled":true,"provider":{"host":"smtp.parity.example","port":587,
                "username":"mailer","password":"parity-secret",
                "from_address":"agento@parity.example","to_addresses":"you@parity.example",
                "encryption":"starttls"},
                "preferences":{"scheduled_tasks":{"on_failed":false}}}"#,
        ),
    )
    .await;
    assert_eq!(saved.0, 200, "a settings save is 200, not 201 or 204");
    let body: serde_json::Value = serde_json::from_slice(&saved.1).expect("json");
    assert_eq!(
        body["provider"]["password"], "***",
        "the answer is a re-read, so the password is masked rather than echoed"
    );
    assert_eq!(body["provider"]["host"], "smtp.parity.example");
    // `*bool` with omitempty: a deliberate false ships, an unset one is absent.
    assert_eq!(body["preferences"]["scheduled_tasks"]["on_failed"], false);
    assert!(body["preferences"]["scheduled_tasks"]
        .get("on_finished")
        .is_none());

    // The sentinel means "keep it": saving again with `"***"` must not store
    // three asterisks as the password.
    let kept = go_answer(
        Method::PUT,
        "/api/notifications/settings",
        Some(
            r#"{"enabled":true,"provider":{"host":"smtp.changed.example","port":587,
                "username":"mailer","password":"***",
                "from_address":"agento@parity.example","to_addresses":"you@parity.example",
                "encryption":"starttls"}}"#,
        ),
    )
    .await;
    assert_eq!(kept.0, 200);
    let kept_body: serde_json::Value = serde_json::from_slice(&kept.1).expect("json");
    assert_eq!(kept_body["provider"]["host"], "smtp.changed.example");
    assert_eq!(
        kept_body["provider"]["password"], "***",
        "still masked — and still a real password underneath, which the next \
         GET's mask is the only visible evidence of"
    );

    let malformed = go_answer(Method::PUT, "/api/notifications/settings", Some("[]")).await;
    assert_eq!(malformed.0, 400, "a malformed body is 400");
    assert_eq!(
        String::from_utf8_lossy(&malformed.1).trim_end(),
        r#"{"error":"invalid JSON body"}"#
    );

    // The test send against a host that does not exist: a 400 whose body is
    // whichever sentence the resolver and go-mail produced.
    let failed = go_answer(Method::POST, "/api/notifications/test", None).await;
    assert_eq!(failed.0, 400, "an unreachable relay is a 400");
    assert!(
        String::from_utf8_lossy(&failed.1).starts_with(r#"{"error":"#),
        "the body is an error envelope; its text is not reproducible from Rust"
    );

    // Put the instance back the way it was found.
    let restored = go_answer(
        Method::PUT,
        "/api/notifications/settings",
        Some(&original_body),
    )
    .await;
    assert_eq!(restored.0, 200);
}

// ─── Uploads and continue (#308) ──────────────────────────────────────────────

/// Pin Go's answers for the one multipart route in the API.
///
/// The response is a single key, so what is actually being compared is the
/// **filename**: `sanitizeExtension` is the boundary between a name the caller
/// chose and a path this process creates, and the path it produces is injected
/// straight into a prompt. A port that differed on which extensions it accepts
/// would be invisible in the JSON and visible in the file the model is asked to
/// read.
///
/// Each case is asked of the real server, so the assertions below are Go's
/// behaviour rather than this port's reading of `filepath.Ext`.
#[tokio::test]
#[ignore = "requires a scratch Go instance; mutates it"]
async fn the_upload_answers_match_go() {
    require_scratch_instance();

    const BOUNDARY: &str = "----parityWritesBoundary";
    fn multipart(name: &str, filename: Option<&str>, content: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let disposition = match filename {
            Some(f) => {
                format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n")
            }
            None => format!("Content-Disposition: form-data; name=\"{name}\"\r\n"),
        };
        out.extend_from_slice(disposition.as_bytes());
        out.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        out.extend_from_slice(content);
        out.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        out
    }
    let content_type = format!("multipart/form-data; boundary={BOUNDARY}");

    // The happy path: 200, one key, an absolute path under tmp-uploads, and the
    // extension carried through with its case intact.
    let (status, body) = send_raw(
        Method::POST,
        "/api/uploads",
        &content_type,
        // A `\r\n` inside the content, which is what a reader that stops at the
        // first line break would truncate.
        multipart("file", Some("photo.PNG"), b"\x89PNG\r\n\x1a\ndata"),
    )
    .await;
    assert_eq!(status, 200, "an upload is 200, not 201");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let path = json["path"].as_str().expect("path");
    assert!(path.contains("/tmp-uploads/"), "{path}");
    assert!(
        path.ends_with(".PNG"),
        "the extension keeps its case: {path}"
    );
    // `<unix-millis>-<uuid><ext>`: 13 digits, a hyphen, a 36-character UUID.
    let name = path.rsplit('/').next().expect("basename");
    let (millis, rest) = name.split_once('-').expect("millis-uuid");
    assert!(millis.chars().all(|c| c.is_ascii_digit()), "{name}");
    assert_eq!(rest.len(), 36 + ".PNG".len(), "{name}");

    // The extension allowlist, as the server applies it.
    for (filename, want_suffix) in [
        ("archive.tar.gz", ".gz"),
        ("report.2026", ".2026"),
        // `filepath.Base` takes the last element, so a traversal has no
        // extension left to take.
        ("../../etc/passwd", ""),
        // Anything non-alphanumeric after the dot fails the allowlist outright
        // rather than being cleaned.
        ("evil.p g", ""),
        ("evil.p-g", ""),
        ("noextension", ""),
    ] {
        let (status, body) = send_raw(
            Method::POST,
            "/api/uploads",
            &content_type,
            multipart("file", Some(filename), b"x"),
        )
        .await;
        assert_eq!(status, 200, "{filename}");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        let path = json["path"].as_str().expect("path");
        let name = path.rsplit('/').next().expect("basename");
        if want_suffix.is_empty() {
            // No extension at all: the generated name ends at the UUID.
            assert_eq!(name.len(), 13 + 1 + 36, "{filename} → {name}");
        } else {
            assert!(name.ends_with(want_suffix), "{filename} → {name}");
        }
        // Whatever the caller sent, none of it is in the stored name.
        assert!(!name.contains(".."), "{name}");
        assert!(!name.contains("passwd"), "{name}");
    }

    // A part named `file` with **no filename** is a form *value* to
    // `multipart.readForm`, so `FormFile` answers ErrMissingFile — even though
    // a part with that name is right there.
    let (status, body) = send_raw(
        Method::POST,
        "/api/uploads",
        &content_type,
        multipart("file", None, b"x"),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        String::from_utf8_lossy(&body).trim_end(),
        r#"{"error":"missing required field: file"}"#
    );

    // A part under a different name is the same error.
    let (status, body) = send_raw(
        Method::POST,
        "/api/uploads",
        &content_type,
        multipart("attachment", Some("x.png"), b"x"),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        String::from_utf8_lossy(&body).trim_end(),
        r#"{"error":"missing required field: file"}"#
    );

    // Not multipart at all: the parse failure, whose message is shared with the
    // size cap because MaxBytesReader surfaces through ParseMultipartForm.
    let (status, body) = send_raw(
        Method::POST,
        "/api/uploads",
        "application/json",
        b"{}".to_vec(),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        String::from_utf8_lossy(&body).trim_end(),
        r#"{"error":"file too large or invalid multipart form"}"#
    );
}

/// Pin Go's answers for continuing a Claude session.
///
/// The 404 is the interesting one: it is the handler's own fixed string rather
/// than a `service.NotFoundError`, so it does not carry the `resource "id" not
/// found` shape every other 404 in this suite has.
///
/// The success path needs a real session id from this machine's corpus, so it
/// is driven only when the list has one — the alternative is asserting against
/// a 404 and calling it a test of the create path.
#[tokio::test]
#[ignore = "requires a scratch Go instance; mutates it"]
async fn the_continue_session_answers_match_go() {
    require_scratch_instance();

    let missing = go_answer(
        Method::POST,
        "/api/claude-sessions/parity-writes-no-such-session/continue",
        None,
    )
    .await;
    assert_eq!(missing.0, 404);
    assert_eq!(
        String::from_utf8_lossy(&missing.1).trim_end(),
        r#"{"error":"session not found"}"#,
        "the handler writes its own string, not the service error's shape"
    );

    let (_, listed) = send(Method::GET, "/api/claude-sessions?limit=1", None).await;
    let listed: serde_json::Value = serde_json::from_slice(&listed).expect("json");
    let Some(session_id) = listed["items"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["session_id"].as_str())
    else {
        panic!(
            "the parity instance has no Claude sessions, so the create half of \
             continue is not exercised — run this on a machine with a ~/.claude corpus"
        );
    };

    let created = go_answer(
        Method::POST,
        &format!("/api/claude-sessions/{session_id}/continue"),
        None,
    )
    .await;
    assert_eq!(created.0, 201, "a continued chat is 201");
    let body: serde_json::Value = serde_json::from_slice(&created.1).expect("json");
    let chat_id = body["chat_id"].as_str().expect("chat_id").to_string();
    // One key, and no others: this is a Go map, so a second one would sort.
    assert_eq!(body.as_object().expect("object").len(), 1);

    // The chat it made: no agent, the session's cwd and model, and the link
    // that makes it a continuation rather than a new conversation.
    let (status, chat) = send(Method::GET, &format!("/api/chats/{chat_id}"), None).await;
    assert_eq!(status, 200);
    let chat: serde_json::Value = serde_json::from_slice(&chat).expect("json");
    let session = &chat["session"];
    assert_eq!(session["sdk_session_id"], session_id);
    assert_eq!(session["agent_slug"], "");
    assert_eq!(session["title"], "New Chat");

    let deleted = go_answer(Method::DELETE, &format!("/api/chats/{chat_id}"), None).await;
    assert_eq!(deleted.0, 204);
}

// ─── Monitoring: a deliberate divergence (#309) ───────────────────────────────

/// The one case in this suite that pins what Go does in order to record what
/// this build **stops** doing.
///
/// Every other test here asserts that Rust reproduces Go. `PUT /api/monitoring`
/// and `POST /api/monitoring/test` do not: the desktop build exports no
/// telemetry, so `native/monitoring.rs` answers 501 rather than saving a
/// configuration that changes nothing. This exists so the decision is
/// falsifiable — if someone later ports the exporters, they should find this
/// test asserting the behaviour they are restoring, rather than discover the
/// divergence from a user.
///
/// It also captures the thing worth knowing about Go's test endpoint before it
/// disappears with the sidecar: it answers `ok: true` for an endpoint nothing is
/// listening on, because `grpc.Dial` is lazy and `Connecting` counts as success.
#[tokio::test]
#[ignore = "requires a scratch Go instance; mutates it"]
async fn the_monitoring_writes_are_what_the_desktop_build_declines() {
    require_scratch_instance();

    let original = go_answer(Method::GET, "/api/monitoring", None).await;
    assert_eq!(original.0, 200);
    let original: serde_json::Value = serde_json::from_slice(&original.1).expect("json");

    // Go saves and answers with the envelope, having rebuilt its providers.
    // Rust answers 501 — see `native::monitoring`'s header for why.
    let saved = go_answer(
        Method::PUT,
        "/api/monitoring",
        Some(
            r#"{"enabled":false,"metrics_exporter":"otlp","logs_exporter":"none",
                "otlp_endpoint":"127.0.0.1:4317","otlp_insecure":true,
                "metric_export_interval_ms":60000}"#,
        ),
    )
    .await;
    assert_eq!(saved.0, 200, "Go saves it; this build declines to");
    let saved_body: serde_json::Value = serde_json::from_slice(&saved.1).expect("json");
    assert_eq!(saved_body["settings"]["metrics_exporter"], "otlp");

    // An unknown exporter is a 400 from the handler's own validator, which is
    // the check a config port would have had to reproduce.
    let invalid = go_answer(
        Method::PUT,
        "/api/monitoring",
        Some(r#"{"metrics_exporter":"statsd"}"#),
    )
    .await;
    assert_eq!(invalid.0, 400);
    assert_eq!(
        String::from_utf8_lossy(&invalid.1).trim_end(),
        r#"{"error":"invalid metrics_exporter: \"statsd\""}"#
    );

    // The test endpoint: 200 with an `ok` field either way, and `ok` is `true`
    // for a port nothing is listening on.
    let unreachable = go_answer(
        Method::POST,
        "/api/monitoring/test",
        Some(r#"{"otlp_endpoint":"127.0.0.1:1","otlp_insecure":true}"#),
    )
    .await;
    assert_eq!(
        unreachable.0, 200,
        "the test endpoint never fails the request"
    );
    let result: serde_json::Value = serde_json::from_slice(&unreachable.1).expect("json");
    assert!(
        result["ok"] == true || result["ok"] == false,
        "whichever it is, it is a race — see native::monitoring's header"
    );

    // An empty endpoint is the one deterministic answer it has.
    let empty = go_answer(Method::POST, "/api/monitoring/test", Some(r#"{}"#)).await;
    assert_eq!(empty.0, 200);
    assert_eq!(
        String::from_utf8_lossy(&empty.1).trim_end(),
        r#"{"ok":false,"error":"OTLP endpoint is not configured"}"#
    );

    // Put the instance back.
    let restored = go_answer(
        Method::PUT,
        "/api/monitoring",
        Some(&original["settings"].to_string()),
    )
    .await;
    assert_eq!(restored.0, 200);
}
