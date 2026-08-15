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
    /// `service.NotFoundError` → 404.
    NotFound { resource: String, id: String },
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
            Self::NotFound { .. } => StatusCode::NOT_FOUND,
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
pub fn decode_body<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, WriteError> {
    if body.is_empty() {
        return Err(WriteError::InvalidBody);
    }
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| WriteError::InvalidBody)?;
    match value {
        serde_json::Value::Object(_) => {
            serde_json::from_value(value).map_err(|_| WriteError::InvalidBody)
        }
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
