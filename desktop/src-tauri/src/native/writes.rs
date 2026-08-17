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
//! # `Fallback` is the important variant
//!
//! Anything Go answers with a 500 — and anything this port is not certain it
//! reproduces — becomes [`WriteError::Fallback`], which the seam turns into an
//! `Err` and forwards to the sidecar. Go then produces its own answer, whatever
//! it is. That is strictly better than guessing at a 500 body, and it is what
//! keeps "a ported route can only be as broken as an unported one" true for
//! writes as well as reads.
//!
//! **The invariant that makes forwarding safe is in the handlers, not here:** a
//! write must fail *before* it mutates, or the forward re-applies what already
//! happened. Every handler validates, checks the schema version and does the
//! whole mutation in a single transaction, so an `Err` means nothing was
//! written.

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
    /// Not reproducible here: let the sidecar answer.
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
            Self::Fallback(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
            Self::Fallback(m) => m.clone(),
        }
    }
}

/// Go's `writeError` body: a one-key map, so the encoder sorts nothing.
#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

/// Turn a handler result into what the seam expects.
///
/// `Fallback` becomes `Err`, which the proxy forwards. Everything else becomes
/// a real response with Go's status and Go's body — because a 422 telling the
/// user which field is wrong is an *answer*, not a failure to answer, and
/// forwarding it would make the sidecar redo the work to reach the same
/// conclusion.
pub fn finish(result: Result<super::Answer, WriteError>) -> Result<super::Answer, String> {
    match result {
        Ok(answer) => Ok(answer),
        Err(WriteError::Fallback(reason)) => Err(reason),
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
            // The strict path refuses one shape Go accepts — **duplicate keys**.
            // serde's derived impl errors with `duplicate field`;
            // `encoding/json` takes the last occurrence.
            //
            // The tempting repair is to decode the already-collapsed `Value`,
            // but that is a *superset* of Go rather than a match: Go validates
            // **every** occurrence, while the `Value` retained only the last. So
            // `{"count":"notanumber","count":1}` is a 400 to Go and would decode
            // cleanly here — an over-accept, which on `POST /api/integrations`
            // means creating a row Go would have refused. It would also lose
            // byte-verbatim raw capture, since the value came from the `Value`.
            //
            // Forwarding is exact in both directions and costs nothing: Go
            // answers the well-typed duplicates and 400s the ill-typed ones. Safe
            // because `decode_body` runs before any mutation in every caller.
            if serde_json::from_value::<T>(value).is_ok() {
                WriteError::Fallback(
                    "duplicate keys: Go validates every occurrence, so only it can answer".into(),
                )
            } else {
                WriteError::InvalidBody
            }
        }),
        // Go's no-op: the zero value, and the handler validates it.
        serde_json::Value::Null => {
            serde_json::from_value(serde_json::Value::Object(serde_json::Map::new()))
                .map_err(|_| WriteError::InvalidBody)
        }
        _ => Err(WriteError::InvalidBody),
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
    fn duplicate_keys_forward_rather_than_being_guessed_at() {
        #[derive(Debug, serde::Deserialize)]
        struct Body {
            #[serde(default)]
            #[allow(dead_code)]
            s: String,
            #[serde(default)]
            #[allow(dead_code)]
            n: u8,
        }
        // Go would accept this one…
        assert!(matches!(
            decode_body::<Body>(br#"{"s":"first","s":"last-wins"}"#).unwrap_err(),
            WriteError::Fallback(_)
        ));
        // …and 400 this one, because the *first* occurrence does not fit `u8`.
        // Both forward, so Go makes that distinction rather than this helper.
        assert!(matches!(
            decode_body::<Body>(br#"{"n":999,"n":7}"#).unwrap_err(),
            WriteError::Fallback(_)
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

    #[test]
    fn a_fallback_becomes_an_err_so_the_proxy_forwards() {
        let out = finish(Err(WriteError::Fallback("nope".into())));
        assert_eq!(out.err().as_deref(), Some("nope"));
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
