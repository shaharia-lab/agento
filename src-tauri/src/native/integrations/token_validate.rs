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
//! `native/trigger/registration.rs`'s rule applies — an `Err` after a
//! successful remote call answers 500 for work that landed, inviting a retry
//! that spends a second call. Every fallible step is therefore before the call:
//! resolving the row, the field validation, and parsing the credentials.
//!
//! The write afterwards is the exception, and it is safe because **Go answers a
//! failed save the same way it answers a failed validation** — `saving validated
//! integration: %w` goes into the very same 400 body. So a failed write is
//! answered with that 400, and nothing on this path returns `Fallback` once the
//! network has been touched.
//!
//! # A rejected check is a state change; an unreachable one is not (#521)
//!
//! There are now **two** writes, and the second is on the failure path. When the
//! provider *answers and refuses* the credential, [`clear_auth`] empties the
//! `auth` column and the server is reloaded — so `authenticated` goes false,
//! [`registry::reload`] hits its `!is_startable()` early return with the server
//! already stopped, and `available-tools` drops the row's tools. When the check
//! merely could not be completed, nothing moves at all.
//!
//! [`super::check`] carries the whole argument for why the distinction exists
//! and why `Unreachable` is the default. Two rules local to this module:
//!
//! - **The clear must not turn a 400 into a 500.** The credential is refused
//!   whichever way the write goes, so a failed `clear_auth` is logged and the
//!   400 is answered anyway — the same reasoning as the reload's own swallowing.
//! - **A row with nothing to clear is not written.** `updated_at` moving is
//!   observable, and a check refused against a row that was never authorised has
//!   changed nothing; the reload still runs, because it is what proves no handle
//!   is left.

use std::path::Path;

use serde::Serialize;

use crate::claude::CancellationToken;
use crate::native::writes::WriteError;
use crate::native::{goquote, Answer};

use super::check::{CheckFailure, CheckKind};
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
    // `trigger::block_on_result`'s body, spelled here — including its
    // no-runtime sentence verbatim — because that helper is fixed to
    // `WriteError` and this call has to carry the outcome's *kind* alongside
    // one.
    let called = match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(call(integration_type, &ct, &creds, rewritten)),
        Err(_) => Err(CallFailure {
            kind: CheckKind::Unreachable,
            error: WriteError::Fallback(
                "validating integration credentials: no tokio runtime on this thread".to_string(),
            ),
        }),
    };

    let stored = match called {
        Ok(stored) => stored,
        Err(failure) => {
            // The provider answered and refused it: the stored authorisation is
            // about a credential that is no longer in use, so it goes — and the
            // reload is what stops the server hosting the refused one.
            if failure.kind == CheckKind::Rejected {
                if row.is_authenticated() {
                    if let Err(e) = clear_auth(db_path, &row.id) {
                        log::warn!(
                            "could not clear the authorisation of an integration \
                             whose credential was refused: id={:?} error={e}",
                            row.id
                        );
                    }
                }
                registry::reload_blocking(db_path, &row.id);
            }
            return Err(failure.error);
        }
    };

    // Go's `saving validated integration: %w` arm, which lands in the *same*
    // 400 body as a failed validation — so this is the inherited behaviour,
    // not a shortcut.
    save(db_path, &row.id, &stored)
        .map_err(|e| WriteError::BadRequest(format!("saving validated integration: {e}")))?;

    // `s.reloadIntegration(cfg.ID)` — a goroutine in Go, and off the response
    // path here for the same reason. A type this shell does not host is skipped
    // inside `reload_blocking`'s callee.
    registry::reload_blocking(db_path, &row.id);
    Ok(())
}

/// A failed remote call: the answer it earns, and which class it was.
///
/// The `error` is exactly what this function used to return, so the 400 body is
/// byte-identical on both classes; `kind` is the new half, and it decides only
/// whether the caller clears the stored authorisation.
struct CallFailure {
    kind: CheckKind,
    error: WriteError,
}

impl CallFailure {
    /// Wrap a validator's [`CheckFailure`] in the `ValidationError` Go builds
    /// around it, keeping the kind. `field` and the message prefix are the
    /// per-type wording and do not move.
    fn validation(field: &str, prefix: &str, failure: CheckFailure) -> Self {
        Self {
            kind: failure.kind,
            error: WriteError::Validation {
                field: field.to_string(),
                message: format!("{prefix}{}", failure.message),
            },
        }
    }
}

/// The five remote calls, each returning the `auth` payload its own MCP server
/// reads. The payloads are built with [`goquote::quote`] and not a JSON encoder
/// — Go builds them with `fmt.Sprintf("%q")`, whose output differs from
/// `encoding/json`'s on `&`, `<`, `>` and every control character.
///
/// Each validator decides its own [`CheckKind`], because only it can see the
/// status or the envelope; this function does no classifying of its own beyond
/// giving the two shapes that never touched the network the safe default.
async fn call(
    integration_type: &str,
    ct: &CancellationToken,
    creds: &Credentials,
    rewritten: Option<String>,
) -> Result<Stored, CallFailure> {
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
                Refusal::Reproducible(failure) => {
                    CallFailure::validation("credentials", "invalid credentials: ", failure)
                }
                // A `url.Parse` refusal, whose wording is not reproducible. It
                // can only arise **before** the request, so a 500 costs nothing
                // — see `confluence::validate`'s header — and nothing was
                // refused, so it changes no stored state either.
                Refusal::Unreproducible(why) => CallFailure {
                    kind: CheckKind::Unreachable,
                    error: WriteError::Fallback(format!(
                        "confluence site URL needs net/url's own message: {why}"
                    )),
                },
            })?;
            r#"{"validated":true}"#.to_string()
        }
        "telegram" => {
            let username = super::telegram::validate::validate_bot_token(ct, &creds.bot_token)
                .await
                .map_err(|e| {
                    CallFailure::validation("credentials.bot_token", "invalid bot token: ", e)
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
            .map_err(|e| CallFailure::validation("credentials", "invalid jira credentials: ", e))?;
            format!(
                r#"{{"validated":true,"display_name":{}}}"#,
                goquote::quote(&display_name)
            )
        }
        "github" => {
            let username = super::github::validate::validate_pat(ct, &creds.personal_access_token)
                .await
                .map_err(|e| {
                    CallFailure::validation(
                        "credentials.personal_access_token",
                        "invalid personal access token: ",
                        e,
                    )
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
                .map_err(|e| {
                    CallFailure::validation("credentials.bot_token", "invalid bot token: ", e)
                })?;
            format!(
                r#"{{"validated":true,"team_name":{}}}"#,
                goquote::quote(&team)
            )
        }
        // Unreachable: the caller filtered the type before anything was called.
        other => {
            return Err(CallFailure {
                kind: CheckKind::Unreachable,
                error: WriteError::Fallback(format!(
                    "no token validation for integration type {other:?}"
                )),
            })
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

/// Empty the `auth` column of a row whose credential the provider refused
/// (#521) — the one write on a failure path, and the whole of the state change.
///
/// It is [`save`]'s shape with one column and no value: same immediate
/// transaction, same `updated_at`. `credentials` is deliberately untouched, so
/// the bytes the `PUT` stored survive and re-running the check after fixing the
/// token restores the authorisation.
///
/// `NULL` rather than `''` because that is what the column holds before anything
/// validates it, and what `native/integrations.rs`'s `PUT` normalises `''` and
/// the literal `null` back to — the three are one state to
/// `HOSTING_COLUMNS`'s `authenticated` expression, and writing the one the rest
/// of the port writes keeps them from drifting into two.
fn clear_auth(db_path: &Path, id: &str) -> Result<(), String> {
    let mut conn = crate::native::db::open_read_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("begin authorisation clear: {e}"))?;
    tx.execute(
        "UPDATE integrations SET auth = NULL, updated_at = ?1 WHERE id = ?2",
        rusqlite::params![crate::native::gotime::now_go_text(), id],
    )
    .map_err(|e| format!("{e}"))?;
    tx.commit().map_err(|e| format!("{e}"))
}

/// `len(cfg.Credentials) == 0` is "absent" to Go; an empty column here is the
/// same thing, and `integration_credentials::validate` reads it as such.
///
/// A column that is **present but not valid JSON** is a third case, and it is
/// neither of the other two: Go reaches `ParseCredentials`, which fails with
/// `encoding/json`'s own wording inside `invalid <type> credentials: …`. That
/// sentence is not reproducible, so this answers a 500 rather than reporting
/// the blob as absent — which would say "credentials are empty" and name the
/// wrong problem. Only a hand-edited row can be in this state, since the
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

    /// A confluence site URL that `url.Parse` itself refuses used to forward,
    /// because Go's sentence there was `net/url`'s vocabulary quoted back at
    /// the caller. With the sidecar gone (#278) it is answered as the same 400
    /// class with this build's own wording — safely, since nothing has been
    /// called.
    ///
    /// The two rules Go states itself — HTTPS, and a hostname — keep their
    /// verbatim sentences, which is what the second half of this test checks.
    #[tokio::test]
    async fn a_confluence_site_url_url_parse_would_refuse_is_answered_400() {
        let dir = tempfile::tempdir().expect("tempdir");

        // A control character is `url.Parse`'s own refusal.
        let db = migrated(
            dir.path(),
            "cf1",
            "confluence",
            "{\"site_url\":\"https://a\\u0001b\",\"email\":\"a@b.c\",\"api_token\":\"t\"}",
        );
        let db2 = db.clone();
        let answer = tokio::task::spawn_blocking(move || serve(&db2, "cf1"))
            .await
            .expect("join")
            .expect("answered");
        assert_eq!(answer.status, StatusCode::BAD_REQUEST);
        assert!(
            body_of(&answer).contains("invalid site URL"),
            "the refusal must name the site URL: {}",
            body_of(&answer)
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

    /// Give a seeded row an authorisation, the way a successful check would.
    ///
    /// `migrated` leaves `auth` NULL, which is the state of a row nothing has
    /// validated — and every failure-path assertion below is about a row that
    /// *had* one, because that is the state the bug produced a lie about.
    fn authorise(db: &Path, id: &str) {
        let conn = rusqlite::Connection::open(db).expect("open");
        conn.execute(
            "UPDATE integrations SET auth = ?1 WHERE id = ?2",
            rusqlite::params![r#"{"validated":true}"#, id],
        )
        .expect("authorise");
    }

    /// #521: a provider that **answers and refuses** the credential clears the
    /// stored authorisation and leaves nothing hosted.
    ///
    /// Both halves matter and neither implies the other. The column is what
    /// `authenticated` — and therefore the badge, `is_startable` and
    /// `available-tools` — is computed from; the handle is what would otherwise
    /// go on answering `tools/call` with the token the provider just refused,
    /// for the life of the process.
    ///
    /// The row is genuinely hosted first, so the second assertion is about a
    /// listener that existed rather than one that never started.
    #[tokio::test]
    async fn a_rejected_check_clears_the_authorisation_and_stops_the_server() {
        use crate::native::integrations::telegram::client::{api_base_lock, set_api_base};
        let _guard = api_base_lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "tg-521-rejected",
            "telegram",
            &format!(r#"{{"bot_token":"{TOKEN}"}}"#),
        );
        authorise(&db, "tg-521-rejected");
        registry::reload(&db, "tg-521-rejected")
            .await
            .expect("host the row");
        assert!(
            registry::registry().is_hosted("tg-521-rejected"),
            "the row has to be hosted before the check, or the assertion below \
             would pass against a server that never started"
        );

        // `{"ok":false}` is how Telegram refuses a bad token — at HTTP 200.
        let base = fake(
            StatusCode::OK,
            r#"{"ok":false,"description":"Unauthorized"}"#,
        )
        .await;
        set_api_base(Some(base));
        let db2 = db.clone();
        let answer = tokio::task::spawn_blocking(move || serve(&db2, "tg-521-rejected")).await;
        set_api_base(None);
        let answer = answer.expect("join").expect("served");

        // The wire is unmoved: the same three-key 400 as before #521.
        assert_eq!(answer.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body_of(&answer),
            concat!(
                r#"{"error":"validation error for \"credentials.bot_token\": "#,
                "invalid bot token: telegram API error: Unauthorized",
                r#"","valid":false,"validated":true}"#,
                "\n",
            )
        );

        let (auth, credentials, updated) = row(&db, "tg-521-rejected");
        assert_eq!(auth, "", "a refused credential leaves no authorisation");
        assert_eq!(
            credentials,
            format!(r#"{{"bot_token":"{TOKEN}"}}"#),
            "only `auth` is cleared — the credential the PUT stored survives"
        );
        assert_ne!(
            updated, "2026-01-01 00:00:00 +0000 UTC",
            "the row changed, so `updated_at` moves"
        );
        assert!(
            !registry::registry().is_hosted("tg-521-rejected"),
            "the refused credential must stop being served"
        );
    }

    /// #521's other half, and the one a flat clear would fail: a provider that
    /// **could not be reached** says nothing about the credential, so the
    /// authorisation, the row and the running server are left exactly as they
    /// were.
    ///
    /// Its row is authorised on purpose. The pre-existing unreachable test seeds
    /// a NULL `auth`, where a clear is unobservable — so it passes against a
    /// flat clear and this one does not.
    #[tokio::test]
    async fn an_unreachable_provider_leaves_the_authorisation_standing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "ji-521-unreachable",
            "jira",
            &format!(
                r#"{{"site_url":"http://127.0.0.1:1","email":"a@b.c","api_token":"{TOKEN}"}}"#
            ),
        );
        authorise(&db, "ji-521-unreachable");

        let db2 = db.clone();
        let answer = tokio::task::spawn_blocking(move || serve(&db2, "ji-521-unreachable"))
            .await
            .expect("join")
            .expect("served");

        assert_eq!(answer.status, StatusCode::BAD_REQUEST);
        let (auth, _, updated) = row(&db, "ji-521-unreachable");
        assert_eq!(
            auth, r#"{"validated":true}"#,
            "a provider that never answered must not disconnect a working integration"
        );
        assert_eq!(
            updated, "2026-01-01 00:00:00 +0000 UTC",
            "nothing is written on the unreachable path"
        );
    }

    /// The status classification itself, over the two shapes that share one
    /// sentence: GitHub reports a revoked token and a broken API the same way
    /// (`github API error: status N: …`), so only the kind tells them apart.
    ///
    /// A 500 first — proving the second assertion is not simply "the clear never
    /// runs" — then a 401 against the same row.
    #[tokio::test]
    async fn a_5xx_is_unreachable_where_a_401_is_a_refusal() {
        use crate::native::integrations::github::client::{api_base_lock, set_api_base};
        let _guard = api_base_lock().await;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(
            dir.path(),
            "gh-521",
            "github",
            &format!(r#"{{"auth_mode":"pat","personal_access_token":"{TOKEN}"}}"#),
        );
        authorise(&db, "gh-521");

        for (status, expected_auth, why) in [
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                r#"{"validated":true}"#,
                "a 5xx says nothing about the token",
            ),
            (
                StatusCode::UNAUTHORIZED,
                "",
                "a 401 is GitHub refusing the token",
            ),
        ] {
            let base = fake(status, r#"{"message":"nope"}"#).await;
            set_api_base(Some(base));
            let db2 = db.clone();
            let answer = tokio::task::spawn_blocking(move || serve(&db2, "gh-521")).await;
            set_api_base(None);
            let answer = answer.expect("join").expect("served");
            assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{why}");

            let (auth, _, _) = row(&db, "gh-521");
            assert_eq!(auth, expected_auth, "{why}");
        }
    }

    /// A stored blob that is present but not JSON is a 500 rather than being
    /// read as absent, where "credentials are empty" would name the wrong
    /// problem. Only a hand-edited row can be in this state.
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
