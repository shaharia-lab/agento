//! The gateway's router, its two middleware layers, and the five handlers (#424).
//!
//! # This is not `proxy.rs`, and the differences are the point
//!
//! `proxy.rs` serves one origin in front of the UI and `/api`, guarded by
//! [`crate::guards`]. This listener serves two third-party wire formats to
//! whatever local tool the user pointed at it, and every request it accepts
//! spends the user's provider credits. So:
//!
//! - **The credential is [`Scope::Llm`], and nothing else opens this door.**
//!   Verification is [`token::verify_against`] — the pure four-argument
//!   function #405 built for exactly this second caller — and *not*
//!   [`crate::native::security::verify_request`], which derives a required
//!   scope from an `/api` method and path. A `read` or `write` token is a 403
//!   here, which is the disjointness #423 built and the whole reason a third
//!   scope exists.
//! - **Both header spellings are accepted.** OpenAI SDKs send
//!   `Authorization: Bearer`; the Anthropic SDK and Claude Code send
//!   `x-api-key`. A gateway that took only one would work with half the clients
//!   it exists for.
//! - **There is no CORS layer, and there must never be one.** ferrox's own
//!   server mounts `CorsLayer::permissive()`, which is correct for a service
//!   behind a network boundary and catastrophic here: it would let any web page
//!   the user has open spend their provider credits from the browser, since the
//!   page can read the response too. The `Host` allowlist below is the other
//!   half of that defence — it is what stops a DNS-rebinding page from reaching
//!   a loopback port at all.
//! - **The error body is the client's dialect, never Agento's.** SDKs branch on
//!   `error.type`; `{"error":"..."}` — which is what every `/api` failure
//!   answers — is a shape neither SDK can read.
//!
//! # Nothing fallible after the head commits
//!
//! Every handler encodes its error body before it starts a stream, exactly as
//! `notifications::test` encodes before it dials. Once [`stream`] has produced
//! a frame there is no status left to change, so a failure there is a frame.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::Utc;
use ferrox_providers::anthropic_types::{
    self, AnthropicMessagesRequest, AnthropicModelObject, AnthropicModelsResponse,
};
use ferrox_providers::error::{anthropic_error_body, openai_error_body, ProxyError};
use ferrox_providers::types::{ChatCompletionRequest, ModelObject, ModelsResponse};
use serde_json::Value;

use super::dispatch::{self, Dispatcher};
use super::{stream, usage};
use crate::native::security::token::{self, Denied, Scope};
use crate::native::security::{keys, tokens};

/// The Anthropic SDK's credential header, alongside `Authorization: Bearer`.
const X_API_KEY: &str = "x-api-key";

/// When the request's own work began, put on the extensions by [`authenticate`].
///
/// A newtype rather than a bare `Instant`, because request extensions are keyed
/// by type: a second `Instant` inserted by anything else would silently replace
/// this one.
#[derive(Clone, Copy)]
struct Started(Instant);

/// Everything a usage row needs that only the request knows.
///
/// Built once per handler from the extensions, so the two surfaces cannot drift
/// on which token or clock they attribute a row to.
fn seed_from(request: &RequestParts, alias: &str, surface: Surface, streamed: bool) -> usage::Seed {
    usage::Seed {
        alias: alias.to_string(),
        // Filled in once dispatch has chosen a target; a refused request keeps
        // them empty, which is what distinguishes it in the table.
        provider: String::new(),
        model_id: String::new(),
        surface: surface.as_str(),
        streamed,
        token_sub: request.token_sub.clone(),
        started: request.started,
        at: Utc::now(),
    }
}

/// The two extension values every handler reads, extracted once.
struct RequestParts {
    token_sub: String,
    started: Instant,
}

impl RequestParts {
    /// Both values are inserted by [`authenticate`], which every one of these
    /// handlers is behind — but a handler must not panic if that ever stops
    /// being true, so both have a defined absent case: no attributable token,
    /// and a duration measured from now (which reads as ~0 rather than as a
    /// wrong large number).
    fn of(extensions: &axum::http::Extensions) -> Self {
        Self {
            token_sub: extensions
                .get::<token::Verified>()
                .map(|v| v.subject.clone())
                .unwrap_or_default(),
            started: extensions
                .get::<Started>()
                .map(|s| s.0)
                .unwrap_or_else(Instant::now),
        }
    }
}

/// Everything a handler needs, cloned per request as an `Arc`.
#[derive(Clone)]
pub struct GatewayState {
    pub db_path: PathBuf,
    pub dispatcher: Arc<Dispatcher>,
}

/// Which wire format a request is on, and therefore which dialect its errors
/// are written in.
///
/// Derived from the path prefix rather than carried per route, so a route added
/// under `/anthropic/` cannot forget to answer in Anthropic's shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    OpenAi,
    Anthropic,
}

impl Surface {
    pub fn of(path: &str) -> Self {
        if path.starts_with("/anthropic/") {
            Self::Anthropic
        } else {
            Self::OpenAi
        }
    }

    /// The `surface` column's value. Stored rather than derived because it
    /// cannot be reconstructed from a row after the fact.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    /// A [`ProxyError`] as `(status, body)` in this surface's dialect.
    pub fn error(self, e: &ProxyError) -> Response {
        let (status, body) = match self {
            Self::OpenAi => openai_error_body(e),
            Self::Anthropic => anthropic_error_body(e),
        };
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
        (status, axum::Json(body)).into_response()
    }
}

/// The largest request body this gateway accepts.
///
/// **`axum`'s default is 2 MiB, and that default is wrong here.** It is sized
/// for APIs whose bodies are forms and small documents; this endpoint's body is
/// an entire conversation, resent in full on every turn, by clients that were
/// pointed here precisely so they would not have to think about it. A long
/// coding session crosses 2 MiB on its own, and one pasted screenshot does it
/// immediately at base64's 4/3 expansion. Worse than the refusal is its shape:
/// `axum` answers a bare `413` in neither surface's dialect, so an SDK reports
/// a transport failure and the user has nothing to go on.
///
/// 32 MiB is Anthropic's own documented request ceiling, so a body this refuses
/// is one the upstream would refuse anyway — with a message the client can
/// read. It is a real bound rather than a formality: the body is buffered whole
/// (and the Anthropic surface parses it twice, once typed and once raw, to
/// forward the client's own document verbatim), so this caps what a single
/// local request can make the app allocate.
const MAX_BODY: usize = 32 * 1024 * 1024;

/// The five routes, the three layers, and which of them each covers.
///
/// `/healthz` is outside the auth layer and inside the `Host` one: liveness is
/// not a secret (the same call `/health` and the JWKS document already make),
/// but it is still not something a foreign `Host` should reach. It is outside
/// the body limit too, having no body to limit.
pub fn router(state: GatewayState) -> Router {
    let authenticated = Router::new()
        .route("/v1/chat/completions", post(openai_chat))
        .route("/v1/models", get(openai_models))
        .route("/anthropic/v1/messages", post(anthropic_messages))
        .route("/anthropic/v1/models", get(anthropic_models))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        // Below the body limit, so an oversized body is refused before a
        // credential is verified — and above nothing else, because the `Host`
        // check outranks both.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            authenticate,
        ))
        .with_state(state);

    Router::new()
        .route("/healthz", get(healthz))
        .merge(authenticated)
        .layer(axum::middleware::from_fn(host_allowlist))
}

/// `GET /healthz` — ferrox answers `ok`, and so does this.
async fn healthz() -> &'static str {
    "ok"
}

// ── The two layers ───────────────────────────────────────────────────────────

/// Refuse a request whose `Host` is not one this listener is reachable at.
///
/// Reuses [`crate::guards::host_allowed`] rather than restating the set: the
/// reasoning is identical (the socket is bound to `127.0.0.1` and has no public
/// name, so a foreign `Host` is either a misconfiguration or a rebinding
/// attempt) and two copies of an allowlist is one copy too many.
///
/// The refusal is plain text with no dialect, because a rebinding page is not a
/// client whose SDK we are trying to satisfy.
async fn host_allowlist(request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| request.uri().authority().map(|a| a.as_str().to_owned()))
        .unwrap_or_default();

    if crate::guards::host_allowed(&host) {
        next.run(request).await
    } else {
        (
            StatusCode::FORBIDDEN,
            "request Host is not one this server is served under",
        )
            .into_response()
    }
}

/// Require an `llm`-scoped token, from either header spelling.
///
/// On success the [`token::Verified`] claims go into the request extensions.
/// #425's usage row wants `subject` (the `api_tokens` row id), and retrofitting
/// that after the handlers exist means touching all four of them.
async fn authenticate(
    State(state): State<GatewayState>,
    mut request: Request,
    next: Next,
) -> Response {
    let surface = Surface::of(request.uri().path());

    let presented = presented_credential(request.headers());

    // `keys::current()` being `None` is a 401, not a panic: the listener starts
    // strictly after `keys::install` (see `registry::start_if_enabled`), so
    // reaching this means the keypair was regenerated and swapped out from
    // under an in-flight request — which is a credential failure, and the same
    // answer every superseded token gets.
    let Some(keypair) = keys::current() else {
        log::warn!("gateway request arrived with no signing key installed");
        return surface.error(&ProxyError::Unauthorized(
            "this gateway has no signing key installed".to_string(),
        ));
    };

    match token::verify_against(
        &presented,
        Scope::Llm,
        keypair.decoding(),
        &tokens::is_revoked,
    ) {
        Ok(verified) => {
            // Already throttled to one write a minute per jti, and already
            // spawned onto the blocking pool by `tokens` itself — so this is not
            // a database call on the request path, and must not be wrapped in a
            // second `db::blocking`.
            tokens::touch(&state.db_path, &verified.jti);
            request.extensions_mut().insert(verified);
            // The clock starts **after** the credential verifies, so
            // `duration_ms` measures dispatch and upstream rather than the 401
            // path — a wall of refused requests would otherwise drag the
            // latency figure toward zero and hide a slow provider.
            request.extensions_mut().insert(Started(Instant::now()));
            next.run(request).await
        }
        Err(Denied::Unauthenticated) => surface.error(&ProxyError::Unauthorized(
            "a valid llm-scoped Agento token is required".to_string(),
        )),
        Err(Denied::InsufficientScope) => surface.error(&ProxyError::Forbidden(
            "this token's scope does not permit gateway requests; \
             mint one with the llm scope in Settings → Security"
                .to_string(),
        )),
    }
}

/// `Authorization: Bearer <t>` first, then `x-api-key`.
///
/// Order matters only when both are present, and then the explicit `Bearer`
/// wins because a client that set it meant it; Claude Code sets `x-api-key`
/// from `ANTHROPIC_API_KEY` and `Authorization` from `ANTHROPIC_AUTH_TOKEN`,
/// and the second is the one a user pastes a gateway token into.
fn presented_credential(headers: &HeaderMap) -> String {
    if let Some(bearer) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        if !bearer.is_empty() {
            return bearer.to_string();
        }
    }
    headers
        .get(X_API_KEY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// `POST /v1/chat/completions`.
async fn openai_chat(
    State(state): State<GatewayState>,
    extensions: axum::http::Extensions,
    body: axum::body::Bytes,
) -> Response {
    let surface = Surface::OpenAi;
    let parts = RequestParts::of(&extensions);

    let request: ChatCompletionRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        // The decode error names the offending field and offset, which is what
        // a client debugging its own request needs. It quotes no credential —
        // the body is a prompt, not a secret this module holds.
        //
        // **No usage row.** A body that will not decode has no alias, no
        // provider and no model — there is nothing to attribute and nothing was
        // spent, so a row here would be four empty columns diluting every
        // average. A *refused dispatch* below does get one, because it names an
        // alias the user configured and is therefore worth seeing.
        Err(e) => {
            return surface.error(&ProxyError::SerializationError(e));
        }
    };

    let alias = request.model.clone();
    if request.is_streaming() {
        let seed = seed_from(&parts, &alias, surface, true);
        match state.dispatcher.chat_stream(&alias, &request).await {
            Ok((upstream, served)) => {
                log::info!(
                    "gateway completion streaming alias={alias:?} provider={:?} model_id={:?}",
                    served.provider,
                    served.model_id
                );
                let accounting =
                    usage::Accounting::new(state.db_path.clone(), served.into_seed(seed));
                // Metered the same way as the Anthropic surface below, rather
                // than read inside `openai_sse`'s own loop. One surface *can*
                // read usage off its own frames and the other cannot, so two
                // mechanisms would work — and would be two places for the
                // definition of "what a chunk reports" to drift apart.
                let upstream = usage::meter(upstream, accounting.clone());
                sse_response(stream::openai_sse(upstream, accounting))
            }
            Err(f) => {
                record_failure(&state, seed, &f);
                surface.error(&f.error)
            }
        }
    } else {
        let seed = seed_from(&parts, &alias, surface, false);
        match state.dispatcher.chat(&alias, &request).await {
            Ok((response, served)) => {
                log::info!(
                    "gateway completion alias={alias:?} provider={:?} model_id={:?}",
                    served.provider,
                    served.model_id
                );
                let accounting =
                    usage::Accounting::new(state.db_path.clone(), served.into_seed(seed));
                accounting.observe(response.usage.as_ref());
                accounting.finish(usage::Status::Ok);
                axum::Json(response).into_response()
            }
            Err(f) => {
                record_failure(&state, seed, &f);
                surface.error(&f.error)
            }
        }
    }
}

/// Record a request that never produced a response.
///
/// Zero tokens by construction — nothing was sent upstream, or every target
/// failed before answering — with the status distinguishing "this alias is
/// misconfigured" (`refused`) from "the provider is down" (`upstream_error`).
///
/// The row names the **last target attempted** when there was one. An empty
/// `provider` therefore means something specific rather than "unknown": no
/// target was ever chosen, because resolution itself failed.
fn record_failure(state: &GatewayState, seed: usage::Seed, failure: &dispatch::Failure) {
    let seed = match &failure.attempted {
        Some(target) => usage::Seed {
            provider: target.provider.clone(),
            model_id: target.model_id.clone(),
            ..seed
        },
        None => seed,
    };
    usage::Accounting::new(state.db_path.clone(), seed)
        .finish(usage::Status::for_dispatch_error(&failure.error));
}

/// `GET /v1/models` — the configured aliases, in OpenAI's list shape.
async fn openai_models(State(state): State<GatewayState>) -> Response {
    if let Err(e) = models_precondition(&state) {
        return Surface::OpenAi.error(&e);
    }
    let body = ModelsResponse {
        object: "list".to_string(),
        data: state
            .dispatcher
            .aliases()
            .into_iter()
            .map(|id| ModelObject {
                id: id.to_string(),
                object: "model".to_string(),
                // ferrox emits 0 here because an alias is not versioned, and a
                // timestamp that moved every request would be a worse answer
                // than an obviously absent one.
                created: 0,
                owned_by: "agento".to_string(),
            })
            .collect(),
    };
    axum::Json(body).into_response()
}

/// `POST /anthropic/v1/messages`.
async fn anthropic_messages(
    State(state): State<GatewayState>,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
    body: axum::body::Bytes,
) -> Response {
    let surface = Surface::Anthropic;
    let parts = RequestParts::of(&extensions);

    // Decoded twice on purpose: once into the typed request the translation
    // needs, and once as the raw document, which is forwarded verbatim so every
    // field this crate does not model — `cache_control`, `thinking`,
    // `service_tier`, tool attributes — survives. Re-encoding the typed form
    // would drop all of them silently.
    let raw: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return surface.error(&ProxyError::SerializationError(e)),
    };
    let request: AnthropicMessagesRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return surface.error(&ProxyError::SerializationError(e)),
    };

    let alias = request.model.clone();
    let streaming = request.is_streaming();

    let beta = stream::merge_betas(
        headers.get("anthropic-beta").and_then(|v| v.to_str().ok()),
        &raw,
    );

    let mut internal = anthropic_types::to_chat_completion_request(request);
    internal.raw_anthropic_body = Some(raw);
    if let Some(beta) = beta {
        internal
            .extra_headers
            .insert("anthropic-beta".to_string(), beta);
    }

    if streaming {
        let seed = seed_from(&parts, &alias, surface, true);
        match state.dispatcher.chat_stream(&alias, &internal).await {
            Ok((upstream, served)) => {
                log::info!(
                    "gateway messages streaming alias={alias:?} provider={:?} model_id={:?}",
                    served.provider,
                    served.model_id
                );
                let accounting =
                    usage::Accounting::new(state.db_path.clone(), served.into_seed(seed));
                // Metered **before** the translation, not after: the emitter
                // consumes the provider stream to build protocol frames, so a
                // frame carries no `usage` for this surface's own loop to read.
                let upstream = usage::meter(upstream, accounting.clone());
                // The message id is minted here rather than taken from
                // upstream: the Anthropic protocol puts it in `message_start`,
                // which is emitted before the first upstream chunk arrives.
                let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
                let frames =
                    anthropic_types::openai_stream_to_anthropic_frames(alias, msg_id, upstream);
                sse_response(stream::anthropic_sse(frames, accounting))
            }
            Err(f) => {
                record_failure(&state, seed, &f);
                surface.error(&f.error)
            }
        }
    } else {
        let seed = seed_from(&parts, &alias, surface, false);
        match state.dispatcher.chat(&alias, &internal).await {
            Ok((response, served)) => {
                log::info!(
                    "gateway messages alias={alias:?} provider={:?} model_id={:?}",
                    served.provider,
                    served.model_id
                );
                let accounting =
                    usage::Accounting::new(state.db_path.clone(), served.into_seed(seed));
                accounting.observe(response.usage.as_ref());
                accounting.finish(usage::Status::Ok);
                axum::Json(anthropic_types::to_anthropic_response(response)).into_response()
            }
            Err(f) => {
                record_failure(&state, seed, &f);
                surface.error(&f.error)
            }
        }
    }
}

/// `GET /anthropic/v1/models` — the same aliases, in Anthropic's list shape.
async fn anthropic_models(State(state): State<GatewayState>) -> Response {
    if let Err(e) = models_precondition(&state) {
        return Surface::Anthropic.error(&e);
    }
    let aliases = state.dispatcher.aliases();
    let body = AnthropicModelsResponse {
        data: aliases
            .iter()
            .map(|id| AnthropicModelObject {
                object_type: "model".to_string(),
                id: (*id).to_string(),
                display_name: (*id).to_string(),
                // ferrox emits the epoch here for the same reason `created: 0`
                // is emitted above: an alias is not versioned, and a moving
                // timestamp would be a worse answer than an obviously fixed one.
                created_at: "1970-01-01T00:00:00Z".to_string(),
            })
            .collect(),
        has_more: false,
        first_id: aliases.first().map(|s| (*s).to_string()),
        last_id: aliases.last().map(|s| (*s).to_string()),
    };
    axum::Json(body).into_response()
}

/// A models list is only meaningful once a provider exists.
///
/// An empty `{"data":[]}` is a truthful answer to "which aliases do you serve"
/// and a misleading one to "is this gateway set up" — and the second is the
/// question a client listing models is really asking. A typed error gives the
/// user something to act on; an empty list gives their tool nothing to say.
fn models_precondition(state: &GatewayState) -> Result<(), ProxyError> {
    if state.dispatcher.has_providers() {
        Ok(())
    } else {
        // "enabled" rather than "configured": a row that exists but is switched
        // off produces exactly this state, and telling the user nothing is
        // configured would send them to add a second one.
        Err(ProxyError::ConfigError(
            "no enabled LLM provider is configured on this gateway".to_string(),
        ))
    }
}

/// The SSE response head, committed before the first frame.
fn sse_response(frames: stream::FrameStream) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        // The same header the chat turn sets, for the same reason: a proxy that
        // buffers an event stream turns it into one long wait.
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(frames))
        // Infallible in practice — every value above is a static, valid header —
        // and a 500 rather than a panic if that ever stops being true.
        .unwrap_or_else(|e| {
            log::error!("gateway could not build the sse response head: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surface_is_decided_by_the_path_prefix() {
        assert_eq!(Surface::of("/v1/chat/completions"), Surface::OpenAi);
        assert_eq!(Surface::of("/v1/models"), Surface::OpenAi);
        assert_eq!(Surface::of("/healthz"), Surface::OpenAi);
        assert_eq!(Surface::of("/anthropic/v1/messages"), Surface::Anthropic);
        assert_eq!(Surface::of("/anthropic/v1/models"), Surface::Anthropic);
        // The prefix is `/anthropic/` with the slash, so a route that merely
        // starts with the word is not the Anthropic surface.
        assert_eq!(Surface::of("/anthropicx"), Surface::OpenAi);
    }

    /// The bodies SDKs branch on. Both are `ferrox_providers`' own mappings —
    /// this asserts the surface picks the right one, which is the half that
    /// lives here.
    #[test]
    fn each_surface_denies_in_its_own_dialect() {
        let unauthorized = ProxyError::Unauthorized("no".into());
        let forbidden = ProxyError::Forbidden("wrong scope".into());

        for (surface, e, status) in [
            (Surface::OpenAi, &unauthorized, 401),
            (Surface::OpenAi, &forbidden, 403),
            (Surface::Anthropic, &unauthorized, 401),
            (Surface::Anthropic, &forbidden, 403),
        ] {
            assert_eq!(surface.error(e).status().as_u16(), status);
        }

        let (status, body) = openai_error_body(&unauthorized);
        assert_eq!(status, 401);
        assert_eq!(body["error"]["type"], "unauthorized");
        let (status, body) = anthropic_error_body(&unauthorized);
        assert_eq!(status, 401);
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "authentication_error");

        let (_, body) = anthropic_error_body(&forbidden);
        assert_eq!(
            body["error"]["type"], "permission_error",
            "the Anthropic SDK's retry behaviour branches on this string"
        );
    }

    #[test]
    fn both_header_spellings_are_read_and_bearer_wins_a_tie() {
        let mut headers = HeaderMap::new();
        assert_eq!(presented_credential(&headers), "");

        headers.insert(X_API_KEY, "from-x-api-key".parse().unwrap());
        assert_eq!(presented_credential(&headers), "from-x-api-key");

        headers.insert(header::AUTHORIZATION, "Bearer from-bearer".parse().unwrap());
        assert_eq!(presented_credential(&headers), "from-bearer");
    }

    /// An `Authorization` header that is not a `Bearer`, or is an empty one,
    /// must fall through to `x-api-key` rather than shadow it with `""`.
    #[test]
    fn a_useless_authorization_header_does_not_shadow_x_api_key() {
        for authorization in ["Basic abc", "bearer lowercase", "Bearer ", "Bearer"] {
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, authorization.parse().unwrap());
            headers.insert(X_API_KEY, "fallback".parse().unwrap());
            assert_eq!(
                presented_credential(&headers),
                "fallback",
                "{authorization:?} should not have shadowed x-api-key"
            );
        }
    }
}
