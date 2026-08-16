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

use reqwest::Method;

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
