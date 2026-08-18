//! What a native write is allowed to answer, and what it must hand back to Go.
//!
//! # The statuses are not a house style
//!
//! Go's handlers do not share one error mapping. `httpErr`
//! (`internal/api/integrations.go`) maps the *service layer's* typed errors —
//! `NotFoundError` → 404, `ValidationError` → **422**, `ConflictError` → **409**
//! — but handler-level checks are 400, chats never use `httpErr` at all, and
//! pricing writes its own 409 with the colliding row in the body. So a port
//! cannot pick one convention; each route answers what its own Go handler
//! answers, and this type is only the vocabulary for saying so.
//!
//! # One variant is not a reproduction
//!
//! [`WriteError::NotImplemented`] is the exception to everything above: it is
//! **501**, and Go answers 501 nowhere. It exists because a desktop build can
//! decline a server feature outright — see `native/monitoring.rs` — and because
//! the alternative for a declined route is an answer that looks like success.
//!
//! # `Fallback` no longer forwards (#278)
//!
//! While the Go sidecar existed, anything Go answered with a 500 — and anything
//! this port was not certain it reproduced — became [`WriteError::Fallback`],
//! which the seam turned into an `Err` and forwarded, and Go produced its own
//! answer. The sidecar is gone, so there is nothing to forward to: `Fallback`
//! is now answered here as **500 `{"error":"internal server error"}`** —
//! `httpErr`'s own default body — with the specific reason going to the log,
//! where Go put it too (`s.logger.Error("internal server error", …)`). The
//! variant keeps its name across ~200 call sites because what it *classifies*
//! is unchanged: a failure of the machinery rather than of the request, whose
//! exact Go wording this build cannot reproduce.
//!
//! **The fail-before-mutate invariant still holds and still matters:** a write
//! must fail *before* it mutates — every handler validates, checks the schema
//! version and does the whole mutation in a single transaction. The seam's
//! forward no longer re-applies anything, but a half-applied write behind a
//! 500 would be exactly as corrupting as it always was.

use axum::http::StatusCode;
use serde::Serialize;

use super::gojson;

/// A write that did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// Malformed JSON body. Go answers 400 with a fixed message, from every
    /// handler, rather than the decoder's own error.
    InvalidBody,
    /// A handler-level check. 400, not 422 — the 422s come from the service
    /// layer, and reproducing the difference is the point.
    BadRequest(String),
    /// `service.ValidationError` → 422.
    Validation { field: String, message: String },
    /// `service.ConflictError` → 409.
    Conflict { resource: String, id: String },
    /// `service.NotFoundError` → 404, formatted as that error formats.
    NotFound { resource: String, id: String },
    /// A 404 whose body is a fixed string rather than the service error's
    /// wording. `handleUpdateChat` writes `chat not found` directly, so
    /// [`WriteError::NotFound`] — which would render `chat "abc" not found` —
    /// is the wrong shape, and `BadRequest` is the wrong *status*.
    NotFoundMessage(String),
    /// A 403 with a fixed message, written by the handler rather than raised by
    /// the service. The trigger-rule routes use it for a rule that exists but
    /// belongs to a different integration — note they check that *before*
    /// decoding the body, so a malformed payload on someone else's rule is this
    /// and not a 400.
    Forbidden(String),
    /// A route this build deliberately does not implement. **501**, and unlike
    /// every other variant it does not correspond to anything Go answers — it
    /// is the desktop app declining a server feature rather than reproducing
    /// one. `PUT /api/monitoring` and `POST /api/monitoring/test` are the only
    /// users (#309): the desktop build exports no telemetry, so a save that
    /// appeared to succeed would be the worst answer available.
    NotImplemented(String),
    /// Go answers **500 with this exact body**, and forwarding would answer
    /// something else.
    ///
    /// The distinction from [`Self::Fallback`] is the whole reason this exists.
    /// `Fallback` means "the sidecar can answer this better than I can", which
    /// holds for every 500 whose body comes from a Go error string. It stops
    /// holding the moment the state the answer depends on lives **here**:
    /// `GET /api/integrations/{id}/auth/status` reads the in-flight OAuth map,
    /// and since #318 that map is the shell's. Forwarding a failed flow would
    /// have Go answer from the stored token — `authenticated: false`, a
    /// plausible-looking lie — where Go itself would have answered 500. So the
    /// 500 is produced here, with `httpErr`'s default body verbatim.
    Internal(String),
    /// A failure of the machinery — driver errors, `os` errors — whose exact Go
    /// wording is not reproducible here. Answered as `httpErr`'s default:
    /// 500 `{"error":"internal server error"}`, with the carried reason logged.
    /// (Until #278 this forwarded to the sidecar; see the module header.)
    Fallback(String),
}

impl WriteError {
    /// A validation failure, spelled as `service.ValidationError` spells it.
    pub fn validation(field: &str, message: impl Into<String>) -> Self {
        Self::Validation {
            field: field.to_string(),
            message: message.into(),
        }
    }

    /// The status Go answers with.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::InvalidBody | Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Validation { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::NotFound { .. } | Self::NotFoundMessage(_) => StatusCode::NOT_FOUND,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
            Self::Internal(_) | Self::Fallback(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The message, formatted exactly as the corresponding Go error's
    /// `Error()`. `writeError` wraps it as `{"error": …}`, so these strings are
    /// on the wire and a paraphrase is a divergence.
    pub fn message(&self) -> String {
        match self {
            // `errInvalidJSONBody`, internal/api/server.go.
            Self::InvalidBody => "invalid JSON body".to_string(),
            Self::BadRequest(m) => m.clone(),
            // `service.ValidationError.Error()`.
            Self::Validation { field, message } => {
                if field.is_empty() {
                    message.clone()
                } else {
                    format!("validation error for {:?}: {message}", field)
                }
            }
            // `service.ConflictError.Error()`.
            Self::Conflict { resource, id } => {
                format!("{resource} with id {:?} already exists", id)
            }
            // `service.NotFoundError.Error()`.
            Self::NotFound { resource, id } => format!("{resource} {:?} not found", id),
            Self::NotFoundMessage(m) => m.clone(),
            Self::Forbidden(m) => m.clone(),
            Self::NotImplemented(m) => m.clone(),
            Self::Internal(m) | Self::Fallback(m) => m.clone(),
        }
    }
}

/// Go's `writeError` body: a one-key map, so the encoder sorts nothing.
#[derive(Serialize)]
pub struct ErrorBody<'a> {
    pub error: &'a str,
}

/// `writeError`'s body, for a handler that answers a typed error outside the
/// write path — `GET …/auth/status`, whose 404 and 500 are the *read* seam's
/// shape but Go's own `httpErr` mapping.
pub fn error_body(message: &str) -> ErrorBody<'_> {
    ErrorBody { error: message }
}

/// Turn a handler result into what the seam expects.
///
/// Every error becomes a real response with Go's status and Go's body. For
/// `Fallback` — a machinery failure whose Go wording this build cannot
/// reproduce — that is `httpErr`'s default 500, and the carried reason goes to
/// the log instead of the wire, which is where Go put it too. (Until #278
/// `Fallback` became `Err` and the proxy forwarded it to the sidecar.)
pub fn finish(result: Result<super::Answer, WriteError>) -> Result<super::Answer, String> {
    match result {
        Ok(answer) => Ok(answer),
        Err(WriteError::Fallback(reason)) => {
            log::warn!("internal server error: {reason}");
            let body = gojson::to_vec(&ErrorBody {
                error: "internal server error",
            })
            .map_err(|enc| format!("encoding error body: {enc}"))?;
            Ok(super::Answer::json_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                body,
            ))
        }
        Err(e) => {
            let message = e.message();
            let body = gojson::to_vec(&ErrorBody { error: &message })
                .map_err(|enc| format!("encoding error body: {enc}"))?;
            Ok(super::Answer::json_status(e.status(), body))
        }
    }
}

/// Decode a JSON request body the way Go's handlers do.
///
/// `json.NewDecoder(r.Body).Decode(&req)` is lenient in ways serde is not, and
/// two of them reach this path: an unknown field is ignored, and a `null` for a
/// scalar leaves the zero value rather than failing. Callers model their
/// request structs with `#[serde(default)]` plus `gojson::null_is_zero_value`
/// for exactly that reason; what is left here is the empty-body case.
///
/// **An empty body is not an error.** Several writes take no payload at all
/// (`POST /api/tasks/{id}/pause`, every `DELETE`), and Go's decoder returns
/// `io.EOF` which those handlers never check because they never decode. A
/// handler that *does* decode treats empty as malformed, matching Go, where
/// `Decode` on an empty body returns an error and the handler 400s.
///
/// # Why this goes through `Value` instead of straight to `T`
///
/// Two shapes where `serde_json::from_slice::<T>` and Go disagree, both of
/// which a direct deserialize gets wrong:
///
/// - **A JSON array deserializes into a struct.** serde supports the positional
///   form, so `[]` becomes a struct with every field defaulted and `["x"]`
///   fills the *first* field. Go answers
///   `cannot unmarshal array into Go value of type api.AgentRequest` and the
///   handler 400s. Without this check, `POST /api/agents` with a body of `[]`
///   would be a 422 here and a 400 there — and `["My Agent"]` would silently
///   *create an agent*.
/// - **A JSON `null` is not an error to Go.** `json.Unmarshal(null, &v)` is a
///   documented no-op that leaves the zero value and returns nil, so a `null`
///   body reaches the handler as an empty request and fails its own validation
///   — a 422, not a 400. serde would reject it outright.
///
/// Everything else — a number, a string, `true` — is a decode error in both.
///
/// # The same shape one level down (#337)
///
/// The array check above is at the **body** level and nothing here can reach
/// inside one: `T` is opaque, and a recursive walk of the parsed `Value` would
/// have to know which subtrees are structs and which are genuinely arrays, which
/// is exactly the information the type carries and the `Value` does not. So the
/// nested rule lives on the types, as [`super::gojson::GoStruct`], and this
/// table is what makes "every write body" auditable rather than asserted. A
/// partial check reads as coverage it does not have.
///
/// Every request body that reaches a typed decode, and what in it is a nested
/// struct:
///
/// | body | nested struct | wrapped |
/// |---|---|---|
/// | `agents::AgentRequest` | `capabilities`, and `Capabilities.mcp`'s values | yes |
/// | `integrations::CreateIntegrationRequest` | `services`' values | yes |
/// | `integrations::UpdateIntegrationRequest` | `services`' values | yes |
/// | `notifications::NotificationSettings` | `provider`, `preferences`, `NotificationPreferences.scheduled_tasks` | yes |
/// | `integrations::TriggerRuleRequest` | — (two `GoList<String>`) | n/a |
/// | `chats::CreateChatRequest`, `chats::PatchChatRequest` | — | n/a |
/// | `chats::BulkDeleteRequest`, `tasks::BulkDeleteRequest` | — (`GoList<String>`) | n/a |
/// | `pricing::RateRequest` | — (the bands are not expressible in a request) | n/a |
/// | `sessions::update::UpdateRequest` | — | n/a |
/// | `fs::MkdirRequest` | — | n/a |
/// | `chat::{SendMessageRequest, ProvideInputRequest, PermissionRequestBody}` | — | n/a |
/// | `claude_settings::profiles::{CreateRequest, UpdateRequest}` | — (`settings` is a `RawValue`, as Go's is) | n/a |
/// | `settings::UserSettings` | — | route is `deferred` (#305) |
///
/// Two bodies deliberately have no struct at all and so no row here:
/// `PUT /api/claude-settings` decodes into Go's `any` (`claude_settings::go_any`),
/// and `POST /api/uploads` is multipart.
///
/// `settings::UserSettings` is the one row worth a caveat: it needs no wrapper
/// because it holds no nested struct, but its `hidden_projects` and
/// `claude_config_dirs` are plain `Vec<String>` rather than
/// [`super::gojson::GoList`], so `[null]` is an over-*reject* there. That is
/// #295's rule rather than this one, and the route still forwards, so it is
/// recorded rather than changed.
pub fn decode_body<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, WriteError> {
    if body.is_empty() {
        return Err(WriteError::InvalidBody);
    }
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| WriteError::InvalidBody)?;
    match value {
        // Deserialize from the **original bytes**, not from the `Value` just
        // parsed. The `Value` is only a shape check.
        //
        // This is load-bearing for any field that captures raw JSON: serde_json
        // is built without `preserve_order`, so a `Value::Object` is a
        // `BTreeMap` — going through it sorts keys, respells numbers (`1.50` →
        // `1.5`, `1e3` → `1000.0`) and strips interior whitespace. A
        // `Box<RawValue>` field would then capture the *re-serialized* value
        // rather than what the client sent, and
        // `integrations.credentials` is stored verbatim by Go, so the two
        // databases would diverge on every blob with more than one key.
        serde_json::Value::Object(_) => serde_json::from_slice(body).map_err(|_| {
            // The strict path refuses one shape Go accepted — **duplicate
            // keys**: serde's derived impl errors with `duplicate field` where
            // `encoding/json` took the last occurrence. While the sidecar
            // existed, the well-typed duplicates forwarded so Go could keep
            // accepting them; with it gone (#278) they are a 400 like every
            // other body this decoder refuses. Decoding the already-collapsed
            // `Value` instead would lose byte-verbatim raw capture — a
            // `Box<RawValue>` field would store re-serialized bytes, and
            // `integrations.credentials` is stored verbatim — for a shape no
            // real client sends.
            WriteError::InvalidBody
        }),
        // Go's no-op: the zero value, and the handler validates it.
        serde_json::Value::Null => {
            serde_json::from_value(serde_json::Value::Object(serde_json::Map::new()))
                .map_err(|_| WriteError::InvalidBody)
        }
        _ => Err(WriteError::InvalidBody),
    }
}

// ─── The service-layer log lines (#335) ───────────────────────────────────────

/// The convention every service-layer log line in `native/` follows.
///
/// Nothing calls this; it exists so the rule has one address that a call site
/// can link to, the way the endpoint registry documents the seam. The
/// alternative — the rule restated at fifteen call sites — is what #301's own
/// argument against per-handler logging is about.
///
/// #301 ported Go's `requestLogger` — one access line per `/api` request, at the
/// seam, so it cannot go selectively sparse as routes move. It deliberately
/// stopped there, and #335 is the other half: **an access line carries neither
/// the entity nor the outcome.** `POST /api/integrations 201 12ms native` does
/// not say which integration, under what name; `DELETE /api/chats 204 3ms
/// native` cannot say how many sessions a bulk delete took. Only the handler
/// knows, so unlike the access line these *have* to live at the call sites — and
/// that is why they land **per subsystem, as its Go counterpart is ported**
/// rather than as one pass. A line for a service the sidecar still answers would
/// log an event this process did not cause.
///
/// Four rules, so fifteen call sites read as one record:
///
/// 1. **`message key=value …`, mirroring Go's `slog` call**, with the same
///    message string and the same keys in the same order. The message is what a
///    reader greps for, and a renamed one silently reworded every historical
///    line.
/// 2. **Every string value is `{:?}`; numbers are `{}`.** Go's `slog` quotes only
///    when it must, but half of these values are user-authored — an integration
///    name, an upload's filename — and one containing a space or an `=` makes an
///    unquoted line unparseable. Quoting always is the only rule that does not
///    depend on the value.
/// 3. **`info`, and after the effect.** The seam's split is failures at `warn`,
///    writes at `info`, successful reads at `debug`; these are all writes. Go
///    logs after the store call returns, so a line means it happened — logging
///    before the commit would announce a write a rollback then discarded.
/// 4. **The `#301` privacy rule still holds: no bodies, no headers, no query
///    string.** Two of these lines carry user-authored text that the access line
///    does not, and both are deliberate rather than overlooked, on the same terms
///    as the agent slug already in the path: `integration created … name=…`,
///    because a line that cannot say which integration was created is most of
///    what it is for; and `file uploaded path=…`, which is the destination
///    filename under the uploads dir — Go logs it and the response body returns
///    it. Nothing here logs a credential, a prompt or a message body, and a line
///    that wanted to would need to be argued here first.
///
/// The list of exceptions this paragraph used to carry — the scheduler's lines
/// (#275), `ValidateTokenAuth`'s (#318), `PUT /api/settings`' (#305/#278) — is
/// empty now: every subsystem has moved and the sidecar that emitted its own
/// lines is gone, so a write with no line here has simply lost its record.
pub mod service_log_convention {}

/// A `log` sink the write tests assert against.
///
/// A log line with no test is a line that quietly stops being emitted, which for
/// this half of #301 is the whole failure mode — the record going sparse without
/// anyone noticing. Installed once per process because `log::set_boxed_logger`
/// allows exactly one; tests filter by their own row's id, so the shared buffer
/// does not make them order-dependent.
#[cfg(test)]
pub(crate) mod testlog {
    use std::sync::{Mutex, Once, OnceLock};

    static LINES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    static INIT: Once = Once::new();

    struct Capture;

    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            if let Ok(mut lines) = lines().lock() {
                lines.push(format!("{} {}", record.level(), record.args()));
            }
        }
        fn flush(&self) {}
    }

    fn lines() -> &'static Mutex<Vec<String>> {
        LINES.get_or_init(Mutex::default)
    }

    /// Start capturing. Idempotent, and safe to call from every test.
    pub(crate) fn install() {
        INIT.call_once(|| {
            let _ = log::set_boxed_logger(Box::new(Capture));
            log::set_max_level(log::LevelFilter::Trace);
        });
    }

    /// Every captured line containing `needle`, level prefix included.
    pub(crate) fn matching(needle: &str) -> Vec<String> {
        install();
        lines()
            .lock()
            .map(|lines| {
                lines
                    .iter()
                    .filter(|line| line.contains(needle))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Assert at least one line was emitted at `INFO` matching `needle`.
    ///
    /// For the two `count=` lines, which carry no id: Go's carries only the
    /// count, so a suite sharing one buffer cannot tell two tests' lines apart
    /// and "exactly one" would be order-dependent rather than true.
    #[track_caller]
    pub(crate) fn assert_info_present(needle: &str) {
        let found = matching(needle);
        assert!(!found.is_empty(), "no line for {needle:?}");
        for line in &found {
            assert!(line.starts_with("INFO "), "a write logs at info: {line:?}");
        }
    }

    /// Assert exactly one line was emitted at `INFO` matching `needle`. Only
    /// safe for a line carrying an id unique to the calling test.
    #[track_caller]
    pub(crate) fn assert_info_once(needle: &str) {
        let found = matching(needle);
        assert_eq!(
            found.len(),
            1,
            "expected one line for {needle:?}: {found:?}"
        );
        assert!(
            found[0].starts_with("INFO "),
            "a write logs at info: {:?}",
            found[0]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field that captures raw JSON must capture what the **client sent**,
    /// not a re-serialization of it.
    ///
    /// `decode_body` shape-checks through a `serde_json::Value`, and serde_json
    /// here has no `preserve_order` — so deserializing *from that `Value`*
    /// silently sorted keys, respelled `1.50` as `1.5` and dropped interior
    /// whitespace. `integrations.credentials` is stored verbatim by Go, so that
    /// was a byte divergence on every blob with more than one key. Deserializing
    /// from the original bytes is what fixes it, and this pins it at the level
    /// the bug lived at rather than only at the one call site that noticed.
    #[test]
    fn a_captured_raw_field_keeps_the_bytes_the_client_sent() {
        #[derive(serde::Deserialize)]
        struct Body {
            #[serde(default, deserialize_with = "super::super::gojson::captured_raw")]
            blob: Option<Box<serde_json::value::RawValue>>,
        }
        let body = br#"{"blob":{"zebra":"z", "alpha":"a","rate":1.50,"n":1e3}}"#;
        let decoded: Body = decode_body(body).expect("decodes");
        assert_eq!(
            decoded.blob.expect("present").get(),
            r#"{"zebra":"z", "alpha":"a","rate":1.50,"n":1e3}"#,
            "key order, number spelling and interior whitespace must all survive"
        );
    }

    /// Duplicate keys forward, because only Go can answer them.
    ///
    /// `encoding/json` keeps the **last** occurrence but type-checks **every**
    /// one, so `{"n":"x","n":1}` is a 400 to Go even though the surviving value
    /// is fine. serde's derived impl refuses duplicates outright, and the
    /// collapsed `Value` has already thrown away the occurrences Go would have
    /// judged — so neither local path can reproduce the answer. Handing it over
    /// is exact in both directions.
    #[test]
    fn duplicate_keys_are_a_400_now_that_nothing_can_forward() {
        #[derive(Debug, serde::Deserialize)]
        struct Body {
            #[serde(default)]
            #[allow(dead_code)]
            s: String,
            #[serde(default)]
            #[allow(dead_code)]
            n: u8,
        }
        // Go took the last occurrence and would have accepted this one; with
        // the sidecar gone (#278) the strict decode's refusal is the answer.
        assert!(matches!(
            decode_body::<Body>(br#"{"s":"first","s":"last-wins"}"#).unwrap_err(),
            WriteError::InvalidBody
        ));
        assert!(matches!(
            decode_body::<Body>(br#"{"n":999,"n":7}"#).unwrap_err(),
            WriteError::InvalidBody
        ));
    }

    /// Ordinary bodies must not touch the duplicate path — verbatim capture
    /// still has to come from the strict decode.
    #[test]
    fn a_body_without_duplicates_never_takes_the_forward_path() {
        #[derive(serde::Deserialize)]
        struct Body {
            #[serde(default, deserialize_with = "super::super::gojson::captured_raw")]
            blob: Option<Box<serde_json::value::RawValue>>,
        }
        let decoded: Body = decode_body(br#"{"blob":{"z":"z", "a":"a","r":1.50}}"#).expect("ok");
        assert_eq!(
            decoded.blob.expect("present").get(),
            r#"{"z":"z", "a":"a","r":1.50}"#
        );
    }

    /// The fallback must not widen what is accepted beyond it.
    #[test]
    fn a_body_both_paths_reject_is_still_malformed() {
        #[derive(Debug, serde::Deserialize)]
        struct Body {
            #[serde(default)]
            #[allow(dead_code)] // Only the decode outcome is under test.
            n: u8,
        }
        assert_eq!(
            decode_body::<Body>(br#"{"n":"not a number"}"#).unwrap_err(),
            WriteError::InvalidBody
        );
    }

    /// These strings ship. `service.ValidationError.Error()` uses `%q`, which
    /// is Go's quoted form — so the field name arrives in quotes, and a port
    /// that formatted it bare would differ on every 422.
    #[test]
    fn the_messages_are_gos_messages() {
        assert_eq!(
            WriteError::validation("name", "name is required").message(),
            "validation error for \"name\": name is required"
        );
        assert_eq!(
            WriteError::Conflict {
                resource: "agent".into(),
                id: "my-agent".into()
            }
            .message(),
            "agent with id \"my-agent\" already exists"
        );
        assert_eq!(
            WriteError::NotFound {
                resource: "agent".into(),
                id: "my-agent".into()
            }
            .message(),
            "agent \"my-agent\" not found"
        );
        assert_eq!(WriteError::InvalidBody.message(), "invalid JSON body");
    }

    /// A `ValidationError` with no field falls back to the bare message —
    /// `Error()` branches on it, and the pricing writes use that form.
    #[test]
    fn a_fieldless_validation_error_is_just_its_message() {
        assert_eq!(
            WriteError::validation("", "effective_from is required").message(),
            "effective_from is required"
        );
    }

    #[test]
    fn the_statuses_are_422_and_409_not_400() {
        assert_eq!(
            WriteError::validation("name", "x").status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            WriteError::Conflict {
                resource: "agent".into(),
                id: "a".into()
            }
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            WriteError::NotFound {
                resource: "agent".into(),
                id: "a".into()
            }
            .status(),
            StatusCode::NOT_FOUND
        );
        // Handler-level checks stay 400 — the distinction the issue calls out.
        assert_eq!(
            WriteError::BadRequest("no fields to update".into()).status(),
            StatusCode::BAD_REQUEST
        );
        // …but a handler-level *404* is still a 404. `handleUpdateChat` writes
        // one with a fixed message, and calling that a 400 was a real
        // regression until it had its own variant.
        let missing = WriteError::NotFoundMessage("chat not found".into());
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(missing.message(), "chat not found");
    }

    /// With no sidecar to forward to (#278), a machinery failure answers
    /// `httpErr`'s default 500 — and the carried reason stays off the wire.
    #[test]
    fn a_fallback_is_answered_as_the_default_500() {
        let answer =
            finish(Err(WriteError::Fallback("nope".into()))).expect("answered, not forwarded");
        assert_eq!(answer.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            String::from_utf8(answer.body.expect("body")).unwrap(),
            "{\"error\":\"internal server error\"}\n"
        );
    }

    /// A 422 is an answer, not a failure to answer: it must not forward, or the
    /// sidecar would redo the work to reach the same conclusion.
    #[test]
    fn a_validation_failure_is_answered_here() {
        let answer = finish(Err(WriteError::validation("name", "name is required")))
            .expect("must be answered, not forwarded");
        assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            String::from_utf8(answer.body.expect("body")).unwrap(),
            "{\"error\":\"validation error for \\\"name\\\": name is required\"}\n"
        );
    }

    #[test]
    fn an_empty_body_is_malformed_for_a_handler_that_decodes() {
        let out: Result<serde_json::Value, _> = decode_body(b"");
        assert_eq!(out.unwrap_err(), WriteError::InvalidBody);

        let out: Result<serde_json::Value, _> = decode_body(b"{not json");
        assert_eq!(out.unwrap_err(), WriteError::InvalidBody);

        let ok: serde_json::Value = decode_body(b"{\"a\":1}").expect("valid");
        assert_eq!(ok["a"], 1);
    }

    #[derive(Debug, Default, PartialEq, serde::Deserialize)]
    #[serde(default)]
    struct Probe {
        name: String,
        other: String,
    }

    /// serde deserializes a struct from a JSON **array**, positionally. Go does
    /// not, and without the object check `["My Agent"]` would create an agent
    /// named "My Agent" on a request Go answers with a 400.
    #[test]
    fn a_json_array_is_not_a_struct() {
        assert_eq!(
            decode_body::<Probe>(b"[]").unwrap_err(),
            WriteError::InvalidBody
        );
        assert_eq!(
            decode_body::<Probe>(br#"["My Agent"]"#).unwrap_err(),
            WriteError::InvalidBody
        );
    }

    /// `json.Unmarshal(null, &v)` is a documented no-op in Go: no error, zero
    /// value. So a `null` body reaches the handler and fails *its* validation
    /// (422), rather than failing the decode (400).
    #[test]
    fn a_null_body_is_the_zero_value_not_an_error() {
        assert_eq!(
            decode_body::<Probe>(b"null").expect("null decodes"),
            Probe::default()
        );
    }

    #[test]
    fn a_scalar_body_is_malformed() {
        for body in [&b"123"[..], b"\"text\"", b"true"] {
            assert_eq!(
                decode_body::<Probe>(body).unwrap_err(),
                WriteError::InvalidBody,
                "body {body:?}"
            );
        }
    }

    /// Go's decoder ignores unknown fields; serde must too, or a frontend that
    /// sends one extra key gets a 400 from Rust and a 200 from Go.
    #[test]
    fn an_unknown_field_is_ignored() {
        let out: Probe = decode_body(br#"{"name":"n","surprise":1}"#).expect("valid");
        assert_eq!(out.name, "n");
    }
}
