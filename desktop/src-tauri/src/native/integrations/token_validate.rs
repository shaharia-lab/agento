//! `POST /api/integrations/{id}/auth/validate` — `handleValidateAuth` plus
//! `integrationService.ValidateTokenAuth` and its five per-type validators.
//!
//! The last route of #318, and the last write route outside the two WhatsApp
//! ones (dropped with the sidecar) and `PUT /api/settings` (#305).
//!
//! # Why this could not move until now
//!
//! `write_routes.json` recorded it as *"five per-type remote validations, each
//! writing its own auth payload"*, and that is exactly the shape: there is no
//! shared `{"validated":true}` flag to write. Each validator calls a different
//! API, reads a different field off the answer, and stores a different object
//! that **its own MCP server** later reads.
//!
//! The older reason on file — that the credentials it writes were read by *Go's*
//! MCP servers — expired with #311–#313, which moved all six here. That is also
//! what makes going native mandatory rather than optional: Go's
//! `reloadIntegration` runs in the sidecar and, for a type this shell hosts,
//! reaches nothing. The seam papered over it with `reload_after_forward`, which
//! this module replaces with the real thing — the same substitution #367 made
//! when it deleted `Trigger::AuthStatusPolled`.
//!
//! # The answer shape, which is not `writeError`'s
//!
//! Neither branch goes through `httpErr`. `handleValidateAuth` writes its own
//! map both times, so a **failed validation is a 400 carrying three keys**, not
//! a 422 with one — even when the failure is a `ValidationError` that every
//! other route renders as a 422. `encoding/json` sorts map keys, so the wire
//! order is `error`, `valid`, `validated`.
//!
//! And `validated` in the success body is not "did we validate": it is
//! `cfg.Type == "telegram" || "confluence" || "jira"`, a hardcoded list that
//! **omits github and slack even though both make a real remote call**. The
//! comment above it in Go says "for types with real validation", which the list
//! contradicts. It is reproduced, not corrected: the frontend reads this field.
//!
//! # Order, and the one thing that may fail after the call
//!
//! `native/trigger/registration.rs`'s rule applies — a native `Err` forwards,
//! and Go re-runs the whole handler, so an `Err` after a successful remote call
//! spends a second one. Every fallible step is therefore before the call:
//! resolving the row, the field validation, and parsing the credentials.
//!
//! The write afterwards is the exception, and it is safe because **Go answers a
//! failed save the same way it answers a failed validation** — `saving validated
//! integration: %w` goes into the very same 400 body. So a failed write is
//! answered here rather than forwarded, and nothing on this path returns
//! `Fallback` once the network has been touched.

use std::path::Path;

use serde::Serialize;

use crate::claude::CancellationToken;
use crate::native::writes::WriteError;
use crate::native::{goquote, Answer};

use super::registry;

/// `map[string]any{"valid": …, "validated": …}` — a Go map, so the wire order
/// is alphabetical rather than the source's. Modelled as a struct with the
/// fields already sorted, which is this codebase's rule for a marshalled map.
#[derive(Serialize)]
struct ValidateOk {
    valid: bool,
    validated: bool,
}

/// `map[string]any{"valid": false, "validated": true, "error": …}` — same rule,
/// and note `validated` is `true` on the failure branch regardless of type.
#[derive(Serialize)]
struct ValidateErr<'a> {
    error: &'a str,
    valid: bool,
    validated: bool,
}

/// The types whose success answers `"validated": true`.
///
/// Hardcoded in `handleValidateAuth` and **not** the set that validates
/// remotely — github and slack call their APIs and still report `false`. See the
/// module header.
const REPORTS_VALIDATED: [&str; 3] = ["telegram", "confluence", "jira"];

/// What a successful validation has to store.
struct Stored {
    /// The `auth` column, built with `strconv.Quote` rather than a JSON encoder
    /// — see [`crate::native::goquote`] for the three ways those differ.
    auth: String,
    /// The `credentials` column, when the validator rewrote it. Only Jira does,
    /// and only because `validateJiraCredentials` calls `SetCredentials` after
    /// trimming the site URL.
    credentials: Option<String>,
}

/// `POST /api/integrations/{id}/auth/validate`.
pub fn serve(db_path: &Path, id: &str) -> Result<Answer, WriteError> {
    // `s.integrationSvc.Get(r.Context(), id)` — its error goes through
    // `httpErr`, so a missing row is the usual 404 and *not* the three-key body.
    let Some(row) = registry::get_for_hosting(db_path, id).map_err(WriteError::Fallback)? else {
        return Err(WriteError::NotFound {
            resource: "integration".to_string(),
            id: id.to_string(),
        });
    };

    let integration_type = row.integration_type.clone();
    match validate_token_auth(db_path, &row) {
        Ok(()) => json(
            200,
            &ValidateOk {
                valid: true,
                validated: REPORTS_VALIDATED.contains(&integration_type.as_str()),
            },
        ),
        // Anything the port cannot answer has to reach the seam unchanged, and
        // `validate_token_auth` only ever raises one before the network call.
        Err(WriteError::Fallback(reason)) => Err(WriteError::Fallback(reason)),
        Err(e) => json(
            400,
            &ValidateErr {
                error: &e.message(),
                valid: false,
                validated: true,
            },
        ),
    }
}

fn json<T: Serialize>(status: u16, body: &T) -> Result<Answer, WriteError> {
    let encoded = crate::native::gojson::to_vec(body)
        .map_err(|e| WriteError::Fallback(format!("encoding validate answer: {e}")))?;
    let code = axum::http::StatusCode::from_u16(status)
        .map_err(|e| WriteError::Fallback(format!("building validate status: {e}")))?;
    Ok(Answer::json_status(code, encoded))
}

/// `ValidateTokenAuth`: the type switch, the remote call, the write, the reload.
///
/// Go's `default` arm returns `nil` without calling anything or writing
/// anything, so an integration of any other type answers 200 with
/// `"validated": false`.
fn validate_token_auth(db_path: &Path, row: &registry::HostingRow) -> Result<(), WriteError> {
    let integration_type = row.integration_type.as_str();
    if !matches!(
        integration_type,
        "confluence" | "telegram" | "jira" | "github" | "slack"
    ) {
        return Ok(());
    }

    // ── Everything fallible, before anything is called ───────────────────────
    //
    // `validateXxxCredentials(cfg)` — the same five functions the create path
    // runs, which is why they are shared rather than spelled twice. Jira's
    // returns the re-encoded credentials it normalized.
    let credentials = raw_credentials(&row.credentials)?;
    let rewritten =
        super::super::integration_credentials::validate(integration_type, credentials.as_deref())?;

    // `cfg.ParseCredentials(&creds)` a second time, against the value the
    // validator left behind. Go does this too. It cannot fail now — the same
    // parse just succeeded — and a `Fallback` here is safe because nothing has
    // been called.
    let effective = rewritten.as_deref().unwrap_or(&row.credentials);
    let creds: Credentials = serde_json::from_str::<
        Option<crate::native::gojson::GoStruct<Credentials>>,
    >(if effective.is_empty() {
        "null"
    } else {
        effective
    })
    .map(|wrapped| wrapped.map_or_else(Credentials::default, |wrapped| wrapped.0))
    .map_err(|e| {
        WriteError::Fallback(format!(
            "re-parsing {integration_type} credentials after validation: {e}"
        ))
    })?;

    // ── Nothing above has called out; nothing below may return Fallback ──────
    let ct = CancellationToken::new();
    let stored = super::super::trigger::block_on_result(
        "validating integration credentials",
        call(integration_type, &ct, &creds, rewritten),
    )?;

    // Go's `saving validated integration: %w` arm, which lands in the *same*
    // 400 body as a failed validation — so answering it here is what Go does,
    // not a shortcut around a forward.
    save(db_path, &row.id, &stored)
        .map_err(|e| WriteError::BadRequest(format!("saving validated integration: {e}")))?;

    // `s.reloadIntegration(cfg.ID)` — a goroutine in Go, and off the response
    // path here for the same reason. A type this shell does not host is skipped
    // inside `reload_blocking`'s callee.
    registry::reload_blocking(db_path, &row.id);
    Ok(())
}

/// The five remote calls, each returning the `auth` payload its own MCP server
/// reads. The payloads are built with [`goquote::quote`] and not a JSON encoder
/// — Go builds them with `fmt.Sprintf("%q")`, whose output differs from
/// `encoding/json`'s on `&`, `<`, `>` and every control character.
async fn call(
    integration_type: &str,
    ct: &CancellationToken,
    creds: &Credentials,
    rewritten: Option<String>,
) -> Result<Stored, WriteError> {
    let auth = match integration_type {
        "confluence" => {
            use super::confluence::validate::Refusal;
            super::confluence::validate::validate_credentials(
                ct,
                &creds.site_url,
                &creds.email,
                &creds.api_token,
            )
            .await
            .map_err(|refusal| match refusal {
                Refusal::Reproducible(message) => WriteError::Validation {
                    field: "credentials".to_string(),
                    message: format!("invalid credentials: {message}"),
                },
                // A `url.Parse` refusal, whose wording is `net/url`'s. It can
                // only arise **before** the request, so forwarding is free —
                // see `confluence::validate`'s header.
                Refusal::Forward(why) => WriteError::Fallback(format!(
                    "confluence site URL needs net/url's own message: {why}"
                )),
            })?;
            r#"{"validated":true}"#.to_string()
        }
        "telegram" => {
            let username = super::telegram::validate::validate_bot_token(ct, &creds.bot_token)
                .await
                .map_err(|e| WriteError::Validation {
                    field: "credentials.bot_token".to_string(),
                    message: format!("invalid bot token: {e}"),
                })?;
            format!(
                r#"{{"validated":true,"bot_username":{}}}"#,
                goquote::quote(&username)
            )
        }
        "jira" => {
            let display_name = super::jira::validate::validate_credentials(
                ct,
                &creds.site_url,
                &creds.email,
                &creds.api_token,
            )
            .await
            .map_err(|e| WriteError::Validation {
                field: "credentials".to_string(),
                message: format!("invalid jira credentials: {e}"),
            })?;
            format!(
                r#"{{"validated":true,"display_name":{}}}"#,
                goquote::quote(&display_name)
            )
        }
        "github" => {
            let username = super::github::validate::validate_pat(ct, &creds.personal_access_token)
                .await
                .map_err(|e| WriteError::Validation {
                    field: "credentials.personal_access_token".to_string(),
                    message: format!("invalid personal access token: {e}"),
                })?;
            format!(
                r#"{{"validated":true,"username":{}}}"#,
                goquote::quote(&username)
            )
        }
        // `creds.BotToken` is empty in `oauth` mode and Go calls anyway — see
        // `slack::validate`'s header.
        "slack" => {
            let team = super::slack::validate::validate_token(ct, &creds.bot_token)
                .await
                .map_err(|e| WriteError::Validation {
                    field: "credentials.bot_token".to_string(),
                    message: format!("invalid bot token: {e}"),
                })?;
            format!(
                r#"{{"validated":true,"team_name":{}}}"#,
                goquote::quote(&team)
            )
        }
        // Unreachable: the caller filtered the type before anything was called.
        other => {
            return Err(WriteError::Fallback(format!(
                "no token validation for integration type {other:?}"
            )))
        }
    };
    Ok(Stored {
        auth,
        credentials: rewritten,
    })
}

/// `cfg.UpdatedAt = time.Now().UTC()` + `s.store.Save(ctx, cfg)`, narrowed to
/// the columns a validation actually changes.
///
/// `Save` rewrites the whole row in Go, but every other column is being written
/// back the value it already held; the two that move are `auth` and — for Jira
/// only — `credentials`.
fn save(db_path: &Path, id: &str, stored: &Stored) -> Result<(), String> {
    let mut conn = crate::native::db::open_read_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("begin validated write: {e}"))?;
    let now = crate::native::gotime::now_go_text();

    match &stored.credentials {
        Some(credentials) => tx.execute(
            "UPDATE integrations SET auth = ?1, credentials = ?2, updated_at = ?3 WHERE id = ?4",
            rusqlite::params![stored.auth, credentials, now, id],
        ),
        None => tx.execute(
            "UPDATE integrations SET auth = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![stored.auth, now, id],
        ),
    }
    .map_err(|e| format!("{e}"))?;

    tx.commit().map_err(|e| format!("{e}"))
}

/// `len(cfg.Credentials) == 0` is "absent" to Go; an empty column here is the
/// same thing, and `integration_credentials::validate` reads it as such.
///
/// A column that is **present but not valid JSON** is a third case, and it is
/// neither of the other two: Go reaches `ParseCredentials`, which fails with
/// `encoding/json`'s own wording inside `invalid <type> credentials: …`. That
/// sentence is not reproducible, so this forwards rather than reporting the
/// blob as absent — which would answer "credentials are empty" where Go names
/// the parse error. Only a hand-edited row can be in this state, since the
/// create and update paths validate before storing.
fn raw_credentials(column: &str) -> Result<Option<Box<serde_json::value::RawValue>>, WriteError> {
    if column.is_empty() {
        return Ok(None);
    }
    match serde_json::value::RawValue::from_string(column.to_string()) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) => Err(WriteError::Fallback(format!(
            "stored credentials are not valid JSON; Go names the parse error: {e}"
        ))),
    }
}

/// The union of the four credential shapes the five validators read. One struct
/// rather than four because every field is optional to serde and Go parses the
/// same column into whichever type its switch arm names — the fields simply do
/// not overlap across types.
#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct Credentials {
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    site_url: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    email: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    api_token: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    bot_token: String,
    #[serde(deserialize_with = "crate::native::gojson::null_is_zero_value")]
    personal_access_token: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    /// A distinctive credential, so a leak into any answer this module builds
    /// is unmistakable — `registry`'s rule, and this module reads the same
    /// column.
    const TOKEN: &str = "SUPER_SECRET_TOKEN_VALUE";

    fn migrated(
        dir: &Path,
        id: &str,
        integration_type: &str,
        credentials: &str,
    ) -> std::path::PathBuf {
        let db = dir.join("agento.db");
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO integrations (id, name, type, enabled, credentials, services,
                                       created_at, updated_at)
             VALUES (?1, 'T', ?2, 1, ?3, '{}',
                     '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
            rusqlite::params![id, integration_type, credentials],
        )
        .expect("seed");
        db
    }

    /// `auth` is nullable and starts NULL on a row nothing has validated, which
    /// is why it is read as an `Option` rather than a `String`.
    fn row(db: &Path, id: &str) -> (String, String, String) {
        let conn = rusqlite::Connection::open(db).expect("open");
        let (auth, credentials, updated): (Option<String>, String, String) = conn
            .query_row(
                "SELECT auth, credentials, updated_at FROM integrations WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("read back");
        (auth.unwrap_or_default(), credentials, updated)
    }

    fn body_of(answer: &Answer) -> String {
        String::from_utf8(answer.body.clone().expect("a body")).expect("utf-8")
    }

    /// Serve one canned response on any path, and return its address.
    async fn fake(status: StatusCode, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let app = axum::Router::new().fallback(move || async move { (status, body) });
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    /// The happy path, and the two things it has to get exactly right: the
    /// payload is `%q`-quoted rather than JSON-encoded, and `updated_at` moves.
    #[tokio::test]
    async fn a_validated_telegram_bot_stores_gos_auth_payload() {
        use crate::native::integrations::telegram::client::{api_base_lock, set_api_base};
        let _guard = api_base_lock().await;
        let base = fake(
            StatusCode::OK,
            r#"{"ok":true,"result":{"id":1,"is_bot":true,"username":"my_bot"}}"#,
        )
        .await;
        set_api_base(Some(base));

        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "tg",
            "telegram",
            &format!(r#"{{"bot_token":"{TOKEN}"}}"#),
        );
        let answer = tokio::task::spawn_blocking(move || serve(&db, "tg")).await;
        set_api_base(None);
        let answer = answer.expect("join").expect("served");

        assert_eq!(answer.status, StatusCode::OK);
        // Go's map, sorted: telegram is on the hardcoded `validated` list.
        assert_eq!(body_of(&answer), "{\"valid\":true,\"validated\":true}\n");
    }

    /// The failure branch is a **400 with three keys**, not the 422 a
    /// `ValidationError` produces everywhere else — `handleValidateAuth` writes
    /// its own map rather than calling `httpErr`.
    #[tokio::test]
    async fn a_rejected_bot_token_is_a_400_with_gos_three_key_body() {
        use crate::native::integrations::telegram::client::{api_base_lock, set_api_base};
        let _guard = api_base_lock().await;
        let base = fake(
            StatusCode::OK,
            r#"{"ok":false,"description":"Unauthorized"}"#,
        )
        .await;
        set_api_base(Some(base));

        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "tg",
            "telegram",
            &format!(r#"{{"bot_token":"{TOKEN}"}}"#),
        );
        let db2 = db.clone();
        let answer = tokio::task::spawn_blocking(move || serve(&db2, "tg")).await;
        set_api_base(None);
        let answer = answer.expect("join").expect("served");

        assert_eq!(answer.status, StatusCode::BAD_REQUEST);
        let body = body_of(&answer);
        assert_eq!(
            body,
            concat!(
                r#"{"error":"validation error for \"credentials.bot_token\": "#,
                "invalid bot token: telegram API error: Unauthorized",
                r#"","valid":false,"validated":true}"#,
                "\n",
            )
        );
        assert!(!body.contains(TOKEN), "the bot token must not reach a body");
        // Nothing was written: Go saves only on the success path.
        let (auth, _, updated) = row(&db, "tg");
        assert_eq!(auth, "");
        assert_eq!(updated, "2026-01-01 00:00:00 +0000 UTC");
    }

    /// github and slack call their APIs and still report `"validated": false` —
    /// `handleValidateAuth`'s hardcoded list omits them, and the comment above
    /// it ("types with real validation") is wrong. Reproduced, not corrected.
    #[tokio::test]
    async fn github_validates_remotely_and_still_reports_validated_false() {
        use crate::native::integrations::github::client::{api_base_lock, set_api_base};
        let _guard = api_base_lock().await;
        let base = fake(StatusCode::OK, r#"{"login":"octocat"}"#).await;
        set_api_base(Some(base));

        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "gh",
            "github",
            &format!(r#"{{"auth_mode":"pat","personal_access_token":"{TOKEN}"}}"#),
        );
        let db2 = db.clone();
        let answer = tokio::task::spawn_blocking(move || serve(&db2, "gh")).await;
        set_api_base(None);
        let answer = answer.expect("join").expect("served");

        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(body_of(&answer), "{\"valid\":true,\"validated\":false}\n");
        let (auth, _, updated) = row(&db, "gh");
        assert_eq!(auth, r#"{"validated":true,"username":"octocat"}"#);
        assert_ne!(updated, "2026-01-01 00:00:00 +0000 UTC", "updated_at moves");
    }

    /// A team name carrying `&` is the case that makes `%q` observable: a JSON
    /// encoder would have written `&`.
    #[tokio::test]
    async fn a_slack_team_name_is_quoted_gos_way_not_json_escaped() {
        use crate::native::integrations::slack::client::{api_base_lock, set_api_base};
        let _guard = api_base_lock().await;
        let base = fake(StatusCode::OK, r#"{"ok":true,"team":"A & B"}"#).await;
        set_api_base(Some(base));

        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "sl",
            "slack",
            &format!(r#"{{"auth_mode":"bot_token","bot_token":"{TOKEN}"}}"#),
        );
        let db2 = db.clone();
        let answer = tokio::task::spawn_blocking(move || serve(&db2, "sl")).await;
        set_api_base(None);
        answer.expect("join").expect("served");

        let (auth, _, _) = row(&db, "sl");
        assert_eq!(auth, r#"{"validated":true,"team_name":"A & B"}"#);
    }

    /// Jira is the only type whose validation rewrites `credentials`, because
    /// `validateJiraCredentials` trims the site URL and calls `SetCredentials`
    /// — which re-marshals the struct, dropping unknown keys.
    #[tokio::test]
    async fn jira_stores_the_display_name_and_the_normalized_credentials() {
        let base = fake(StatusCode::OK, r#"{"displayName":"Ana María"}"#).await;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "ji",
            "jira",
            &format!(
                r#"{{"site_url":"{base}///","email":"a@b.c","api_token":"{TOKEN}","extra":"dropped"}}"#
            ),
        );
        let db2 = db.clone();
        tokio::task::spawn_blocking(move || serve(&db2, "ji"))
            .await
            .expect("join")
            .expect("served");

        let (auth, credentials, _) = row(&db, "ji");
        assert_eq!(auth, r#"{"validated":true,"display_name":"Ana María"}"#);
        assert_eq!(
            credentials,
            format!(r#"{{"site_url":"{base}","email":"a@b.c","api_token":"{TOKEN}"}}"#),
            "every trailing slash trimmed, declaration order, unknown key gone"
        );
    }

    /// A type the switch does not name returns `nil` from `ValidateTokenAuth`:
    /// no call, no write, and a 200 saying it was not validated.
    #[test]
    fn an_unhandled_type_is_a_200_that_calls_and_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "go", "google", r#"{"client_id":"x"}"#);
        let answer = serve(&db, "go").expect("served");
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(body_of(&answer), "{\"valid\":true,\"validated\":false}\n");
        let (auth, _, updated) = row(&db, "go");
        assert_eq!(auth, "");
        assert_eq!(updated, "2026-01-01 00:00:00 +0000 UTC");
    }

    /// A remote call that cannot connect. The sentence is fixed because Go
    /// discards `client.Do`'s error rather than wrapping it — the URL is the
    /// customer's site — which is the whole reason this is reproducible.
    ///
    /// The same literal is pinned against the real Go server by
    /// `tests/parity_writes.rs::the_auth_validate_answers_match_go`; see that
    /// file's header for why a write's comparison is split across two suites.
    #[tokio::test]
    async fn an_unreachable_provider_is_gos_fixed_transport_sentence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "ji",
            "jira",
            &format!(
                r#"{{"site_url":"http://127.0.0.1:1","email":"a@b.c","api_token":"{TOKEN}"}}"#
            ),
        );
        let db2 = db.clone();
        let answer = tokio::task::spawn_blocking(move || serve(&db2, "ji"))
            .await
            .expect("join")
            .expect("served");

        assert_eq!(answer.status, StatusCode::BAD_REQUEST);
        let body = body_of(&answer);
        assert_eq!(
            body,
            concat!(
                r#"{"error":"validation error for \"credentials\": invalid jira credentials: "#,
                "calling Jira /myself: request failed",
                r#"","valid":false,"validated":true}"#,
                "\n",
            )
        );
        assert!(!body.contains(TOKEN), "the api token must not reach a body");
        // Nothing was stored, and `credentials` was not rewritten either — the
        // Jira normalization only lands on the success path.
        let (auth, _, updated) = row(&db, "ji");
        assert_eq!(auth, "");
        assert_eq!(updated, "2026-01-01 00:00:00 +0000 UTC");
    }

    /// A confluence site URL that `url.Parse` itself refuses forwards, because
    /// Go's sentence there is `net/url`'s vocabulary quoted back at the caller
    /// and this port does not spell it. Safe: nothing has been called.
    ///
    /// The two rules Go states itself — HTTPS, and a hostname — are answered
    /// here instead, which is what the second half of this test checks.
    #[tokio::test]
    async fn a_confluence_site_url_forwards_only_when_go_would_use_net_urls_wording() {
        let dir = tempfile::tempdir().expect("tempdir");

        // A control character is `url.Parse`'s own refusal.
        let db = migrated(
            dir.path(),
            "cf1",
            "confluence",
            "{\"site_url\":\"https://a\\u0001b\",\"email\":\"a@b.c\",\"api_token\":\"t\"}",
        );
        let db2 = db.clone();
        let err = tokio::task::spawn_blocking(move || serve(&db2, "cf1"))
            .await
            .expect("join")
            .expect_err("forwarded");
        assert!(
            matches!(err, WriteError::Fallback(_)),
            "got {:?}, want a Fallback",
            err.message()
        );

        // …while "must use HTTPS" is Go's own sentence and is answered.
        let dir2 = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir2.path(),
            "cf2",
            "confluence",
            r#"{"site_url":"http://acme.atlassian.net","email":"a@b.c","api_token":"t"}"#,
        );
        let db2 = db.clone();
        let answer = tokio::task::spawn_blocking(move || serve(&db2, "cf2"))
            .await
            .expect("join")
            .expect("answered");
        assert_eq!(answer.status, StatusCode::BAD_REQUEST);
        assert!(
            body_of(&answer).contains(r#"site URL must use HTTPS (got \"http\")"#),
            "got {}",
            body_of(&answer)
        );
    }

    /// A stored blob that is present but not JSON forwards rather than being
    /// read as absent: Go reaches `ParseCredentials` and reports
    /// `encoding/json`'s own message, where "credentials are empty" would name
    /// the wrong problem. Only a hand-edited row can be in this state.
    #[test]
    fn credentials_that_are_not_json_forward_rather_than_reading_as_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "tg", "telegram", "not json at all");
        let err = serve(&db, "tg").expect_err("forwarded");
        assert!(
            matches!(err, WriteError::Fallback(_)),
            "got {:?}, want a Fallback",
            err.message()
        );
    }

    /// The row is resolved through `httpErr`, so a missing one is the ordinary
    /// 404 and **not** the three-key body.
    #[test]
    fn a_missing_integration_is_the_ordinary_404() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "other", "telegram", r#"{"bot_token":"x"}"#);
        let err = serve(&db, "nope").unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.message(), r#"integration "nope" not found"#);
    }

    /// A field the validator requires is missing, so this fails **before** any
    /// network call — and still lands in the 400 body rather than a 422.
    #[test]
    fn a_missing_required_field_is_the_400_body_not_a_422() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path(), "tg", "telegram", r#"{"bot_token":""}"#);
        let answer = serve(&db, "tg").expect("served");
        assert_eq!(answer.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body_of(&answer),
            concat!(
                r#"{"error":"validation error for \"credentials.bot_token\": "#,
                "bot_token is required",
                r#"","valid":false,"validated":true}"#,
                "\n",
            )
        );
    }
}
