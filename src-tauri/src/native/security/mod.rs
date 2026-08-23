//! The credential system: keys, tokens, and the routes that manage them (#405).
//!
//! This module owns what `/api` accepts as proof of identity. [`keys`] is the
//! per-install Ed25519 keypair, [`token`] is the JWT format signed by it, and
//! [`tokens`] is the `api_tokens` table plus the revocation set. `guards.rs`
//! calls exactly one function here, [`verify_request`]; everything else is the
//! surface that lets a user see and manage what they have issued.
//!
//! # It replaces #400's credential, not #400's structure
//!
//! `guards.rs` still runs one check, still scoped to `/api`, still positioned
//! between the `Host` and `Content-Type` ones. What changed is what a credential
//! *is*: an opaque per-launch string with no identity, no expiry, no scope and
//! no revocation short of restarting the app becomes a signed, scoped,
//! individually revocable JWT with an offline-verifiable signature.
//!
//! That buys three things #400 could not express, and each is the answer to a
//! question the maintainer asked:
//!
//! 1. **Another local process can be given access deliberately** — a script, a
//!    CI job, another of our services — instead of copying the app's own
//!    credential out of the debug token file.
//! 2. **A verifier that is not Agento can check a token**, holding only the
//!    public key and unable to mint. That is what `GET /.well-known/jwks.json`
//!    is for, and it is the requirement that made the design asymmetric rather
//!    than a table of hashed opaque keys.
//! 3. **Everything dies at once** on `POST /api/security/keys/regenerate`, with
//!    no denylist and no distributed state: every previously issued signature
//!    simply stops verifying.
//!
//! The counter-argument was made and overruled deliberately, and it is worth
//! keeping: for user-generated API keys, opaque random keys hashed in SQLite are
//! the more common answer (GitHub PATs, Stripe, Slack, Tailscale), because
//! revocation forces a datastore lookup anyway and a durable private key is a
//! *permanent* secret where #400's died per launch. Points 2 and 3 are what
//! outweighed it — opaque keys cannot do offline verification at all.
//!
//! # This is the port's second deliberate divergence from the Go surface
//!
//! The first is #400's 401, which Go never answers. These routes are the second:
//! `/api/security/*` and `/.well-known/jwks.json` exist in **no** Go router, so
//! `parity/read_routes.json` and `parity/write_routes.json` — both frozen since
//! #388, both generated from a `chi.Walk` — cannot record them without ceasing
//! to be what they are.
//!
//! Leaving them unrecorded was the other option and it was rejected: those files
//! exist so the route surface *cannot drift silently*, and #405's own scoping
//! flagged that adding routes to neither would quietly weaken that property.
//! So there is a third file, `parity/desktop_routes.json`, which is explicitly
//! **not** a Go golden — and it carries a property the two Go ones do not.
//! Theirs is one-directional: they iterate their own rows, so a route claimed
//! and never recorded passes. [`ROUTES`] is the single definition [`claims`]
//! matches against, and `desktop_routes_are_recorded_in_both_directions` asserts
//! set equality with the file — so here a route cannot be added without the file
//! moving, which is the property the Go files were meant to have.

pub mod keys;
pub mod token;
pub mod tokens;

use axum::http::{Method, StatusCode};

use super::writes::{self, WriteError};
use super::{gojson, gotime, Answer};
use token::{Denied, Scope, Verified};

/// The path JWKS is served at.
///
/// The `.well-known` spelling is RFC 8615's and is what a stock JWT library's
/// discovery expects, which is the whole point: a Go or Rust consumer points at
/// this URL and needs no PEM handling, no key format knowledge and no Agento
/// code. It is outside `/api`, so `guards.rs` never sees it — a public key is
/// public, and requiring a credential to fetch the thing that verifies
/// credentials is a bootstrap problem with no answer.
pub const JWKS_PATH: &str = "/.well-known/jwks.json";

/// Every route this module claims: the single definition [`claims`] matches
/// against and the golden file is compared to.
///
/// One list rather than a `match`, because a `match` cannot be enumerated — and
/// enumerating it is what makes the bidirectional assertion in the module header
/// possible.
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/security/keys"),
    ("POST", "/api/security/keys/regenerate"),
    ("GET", "/api/security/tokens"),
    ("POST", "/api/security/tokens"),
    ("DELETE", "/api/security/tokens/{id}"),
    ("GET", JWKS_PATH),
];

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "security",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    ROUTES
        .iter()
        .any(|(m, pattern)| method.as_str() == *m && path_matches(pattern, path))
}

/// Match a chi-style pattern against a concrete path.
///
/// A `{name}` segment matches exactly one **non-empty** segment, so
/// `/api/security/tokens/` is not a match — chi routes a trailing slash to
/// nothing, and so does every other module here.
fn path_matches(pattern: &str, path: &str) -> bool {
    let mut want = pattern.split('/');
    let mut have = path.split('/');
    loop {
        match (want.next(), have.next()) {
            (None, None) => return true,
            (Some(w), Some(h)) => {
                let ok = if w.starts_with('{') && w.ends_with('}') {
                    !h.is_empty()
                } else {
                    w == h
                };
                if !ok {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// The `{id}` of `/api/security/tokens/{id}`.
fn token_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/security/tokens/")?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    Some(rest)
}

// ─── The guard's entry point ──────────────────────────────────────────────────

/// What scope a request needs, given the method and the path it is on.
///
/// The rule is `guards::is_state_changing` — the safe methods need `read`, the
/// state-changing four need `write` — with **one** exception, and it is narrow
/// on purpose: everything under `/api/security/` needs `write` whatever its
/// method.
///
/// That is not the beginning of a per-route permission table (#405 defers that
/// explicitly). It is the observation that these particular reads *are* the
/// credential system: `GET /api/security/tokens` enumerates every credential
/// issued on this machine, and `GET /api/security/keys` names where the private
/// key is stored. Handing that to a `read` token would let the narrower
/// credential map the wider ones, which is the one place the two-level split
/// would have been actively misleading.
pub fn required_scope(method: &Method, path: &str) -> Scope {
    if path.starts_with("/api/security/") || path == "/api/security" {
        return Scope::Write;
    }
    if crate::guards::is_state_changing(method) {
        Scope::Write
    } else {
        Scope::Read
    }
}

/// Verify a presented credential for this request. **The one function
/// `guards.rs` calls.**
///
/// `None` for the keypair — a `setup` that failed before installing one —
/// refuses everything, which is the safe direction and the same fail-closed
/// default #400's missing token had: a listener that cannot verify a signature
/// cannot tell a forged token from a real one, so it must accept neither.
pub fn verify_request(presented: &str, method: &Method, path: &str) -> Result<Verified, Denied> {
    let verified = verify_with(
        presented,
        method,
        path,
        keys::current().as_deref(),
        &tokens::is_revoked,
    )?;

    // Off the request path by construction — `touch` spawns and returns. The
    // only signal that a leaked token is being used, and the reason it is worth
    // a write at all.
    if verified.subject != token::WEBVIEW_SUBJECT {
        if let Some(db) = crate::paths::database_path() {
            tokens::touch(&db, &verified.jti);
        }
    }

    Ok(verified)
}

/// [`verify_request`] with the key and the revocation predicate passed in.
///
/// Split out for one reason, and it is the case that cannot be arranged
/// otherwise: **`keypair: None` must refuse everything**, and the process-wide
/// key is installed at startup and never un-installed, so a test that wanted to
/// observe "no key" against the global could not — the same problem #400's
/// `token_rejection` solved by taking its expected value as a parameter.
fn verify_with(
    presented: &str,
    method: &Method,
    path: &str,
    keypair: Option<&keys::Keypair>,
    is_revoked: &dyn Fn(&str) -> bool,
) -> Result<Verified, Denied> {
    let Some(keypair) = keypair else {
        return Err(Denied::Unauthenticated);
    };
    token::verify_against(
        presented,
        required_scope(method, path),
        keypair.decoding(),
        is_revoked,
    )
}

/// Mint the app's own webview session token.
///
/// A self-signed JWT with **no database row**: it is minted on demand, so there
/// is nothing to store and nothing left behind when the app exits, and it is
/// revoked by regenerating the keypair like everything else. `sub` distinguishes
/// it in a log line and in [`verify_request`]'s `last_used_at` skip.
pub fn mint_session_token() -> Result<String, String> {
    let keypair = keys::current().ok_or("no signing key installed")?;
    token::mint(
        &keypair,
        token::WEBVIEW_SUBJECT,
        Scope::Write,
        token::SESSION_TTL_SECONDS,
    )
    .map(|minted| minted.token)
}

// ─── Serving ──────────────────────────────────────────────────────────────────

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<Answer, String> {
    match (req.method.as_str(), req.path) {
        ("GET", JWKS_PATH) => jwks(),
        ("GET", "/api/security/keys") => key_info(),
        ("POST", "/api/security/keys/regenerate") => writes::finish(regenerate()),
        ("GET", "/api/security/tokens") => list_tokens(&ctx.db_path),
        ("POST", "/api/security/tokens") => writes::finish(create_token(&ctx.db_path, req.body)),
        ("DELETE", path) => match token_id(path) {
            Some(id) => writes::finish(revoke_token(&ctx.db_path, id)),
            None => Err("DELETE /api/security/tokens has no id".to_string()),
        },
        _ => Err(format!(
            "{} {} is claimed but unhandled",
            req.method, req.path
        )),
    }
}

/// One JWK. Field order is the order a reader scans it in: what kind of key,
/// then the key, then which key.
#[derive(serde::Serialize)]
struct Jwk {
    kty: &'static str,
    crv: &'static str,
    x: String,
    kid: String,
    alg: &'static str,
    #[serde(rename = "use")]
    use_: &'static str,
}

#[derive(serde::Serialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

fn jwk_for(keypair: &keys::Keypair) -> Jwk {
    Jwk {
        // RFC 8037: an Ed25519 public key is an OKP with crv Ed25519 and the
        // raw 32 bytes base64url-encoded in `x`. This is the shape every stock
        // library expects; anything else would defeat the point of publishing
        // it at all.
        kty: "OKP",
        crv: "Ed25519",
        x: keys::public_key_b64(keypair),
        kid: keypair.kid().to_string(),
        alg: "EdDSA",
        use_: "sig",
    }
}

/// `GET /.well-known/jwks.json` — unauthenticated, by design.
///
/// The set holds exactly one key, always: there is one keypair per install and a
/// regenerate replaces it rather than adding to it. It is still a *set* because
/// that is what the format is and what a consumer's library parses — and because
/// `kid` then means something, so a verifier can tell "this token predates the
/// regenerate" from "this token is forged".
fn jwks() -> Result<Answer, String> {
    let Some(keypair) = keys::current() else {
        // Not `Err`, which would be a 500 saying nothing. A key that does not
        // exist yet is a service that is not ready to be verified against, and
        // 503 is what a consumer should retry.
        return Answer::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no signing key is installed",
        );
    };
    jwks_for(&keypair)
}

/// [`jwks`] over an explicit key.
///
/// Split for the same reason [`verify_with`] is, and after making the same
/// mistake: a test that reached these documents by *installing* a keypair
/// replaced the process-wide one, and every guard test minting against the
/// seeded key started answering 401 — in another module, intermittently, with
/// nothing pointing back here. `cargo test` runs a binary's tests in parallel;
/// a function that reads shared state needs a sibling that does not.
fn jwks_for(keypair: &keys::Keypair) -> Result<Answer, String> {
    let body = gojson::to_vec(&Jwks {
        keys: vec![jwk_for(keypair)],
    })
    .map_err(|e| format!("encoding jwks: {e}"))?;
    Ok(Answer::json(body))
}

/// What `GET /api/security/keys` answers.
///
/// **The private key is not here, and there is no route that would return it.**
/// Its *path* is, because a user asked to back it up or move it aside needs to
/// know where it is — a filename is not the secret.
#[derive(serde::Serialize)]
struct KeyInfo {
    kid: String,
    /// The public key, base64url — the same bytes as the JWK's `x`.
    public_key: String,
    algorithm: &'static str,
    jwks_path: &'static str,
    /// Where the two files live, for a user who wants to back one up.
    private_key_path: String,
    public_key_path: String,
}

fn key_info() -> Result<Answer, String> {
    let Some(keypair) = keys::current() else {
        return Answer::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no signing key is installed",
        );
    };
    let dir = crate::paths::data_dir().unwrap_or_default();
    Ok(Answer::json(key_info_body(&keypair, &dir)?))
}

/// The `KeyInfo` document, shared by the read and by the regenerate's answer so
/// the two cannot describe the same key differently. Explicit key, per
/// [`jwks_for`].
fn key_info_body(keypair: &keys::Keypair, dir: &std::path::Path) -> Result<Vec<u8>, String> {
    gojson::to_vec(&KeyInfo {
        kid: keypair.kid().to_string(),
        public_key: keys::public_key_b64(keypair),
        algorithm: "EdDSA",
        jwks_path: JWKS_PATH,
        private_key_path: keys::private_key_path(dir).display().to_string(),
        public_key_path: keys::public_key_path(dir).display().to_string(),
    })
    .map_err(|e| format!("encoding key info: {e}"))
}

/// `POST /api/security/keys/regenerate`.
///
/// Answers the *new* key's info, so the UI can show what it now is without a
/// second round trip — which matters here more than usual, because the caller's
/// own credential died the moment this returned and the follow-up `GET` would
/// 401 before `api.ts` re-mints.
///
/// Takes no database handle, and that is the whole shape of the revocation
/// story: nothing is written, no row is marked, no denylist grows. The old
/// signatures simply stop verifying, which is what made an asymmetric key worth
/// the durable secret it costs.
fn regenerate() -> Result<Answer, WriteError> {
    let dir = crate::paths::data_dir()
        .ok_or_else(|| WriteError::Fallback("no data dir to store the key in".to_string()))?;
    // Written first, installed second: `keys::regenerate` persists and returns
    // without touching the process-wide key, so a keypair that could not be
    // written never becomes the one this process signs with.
    let keypair = keys::install(keys::regenerate(&dir).map_err(WriteError::Fallback)?);

    // Every issued token is already dead by signature, so the recorded `jti`s
    // are pure growth. Emptying the set is not what revokes them.
    tokens::clear_revoked();

    let body = key_info_body(&keypair, &dir).map_err(WriteError::Fallback)?;

    // #335's service-log convention: `message key=value`, every string value
    // `{:?}`, `info`, and **after** the effect. Deliberately no token count —
    // the number invalidated is the number issued, and the token list still
    // shows them.
    log::info!("api signing key regenerated kid={:?}", keypair.kid());
    Ok(Answer::json_status(StatusCode::OK, body))
}

fn list_tokens(db_path: &std::path::Path) -> Result<Answer, String> {
    let conn = super::db::open_read_only(db_path)?;
    let rows = tokens::list(&conn)?;
    let body = gojson::to_vec(&rows).map_err(|e| format!("encoding tokens: {e}"))?;
    Ok(Answer::json(body))
}

/// `POST /api/security/tokens`.
///
/// Every field defaults, because Go's decoder leaves a missing key at its zero
/// value rather than failing — the convention every request struct in `native/`
/// follows, kept here even though these routes have no Go counterpart, so one
/// module does not decode differently from the rest.
#[derive(serde::Deserialize)]
struct CreateTokenRequest {
    #[serde(default, deserialize_with = "gojson::null_is_zero_value")]
    name: String,
    #[serde(default, deserialize_with = "gojson::null_is_zero_value")]
    scope: String,
    /// `0` or absent means [`DEFAULT_TOKEN_DAYS`].
    #[serde(default, deserialize_with = "gojson::null_is_zero_value")]
    expires_in_days: i64,
}

/// How long a user token lasts when the request does not say.
const DEFAULT_TOKEN_DAYS: i64 = 90;

/// The longest a user token may last. Ten years is not a security boundary, it
/// is a guard against an `i64` of days overflowing the `exp` arithmetic into
/// something that never expires by accident.
const MAX_TOKEN_DAYS: i64 = 3650;

/// What a creation answers: the row, plus the **only** copy of the token that
/// will ever exist.
#[derive(serde::Serialize)]
struct CreatedToken {
    #[serde(flatten)]
    token_row: tokens::TokenRow,
    /// Shown once and never retrievable — nothing stores it, so a second read
    /// is not "refused", it is impossible.
    token: String,
}

fn create_token(db_path: &std::path::Path, body: &[u8]) -> Result<Answer, WriteError> {
    let req: CreateTokenRequest = writes::decode_body(body)?;

    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(WriteError::validation("name", "name is required"));
    }
    // Defaulting an absent scope to `read` is the safe direction, and it is what
    // the form sends anyway — the point is that a request that *forgot* the
    // field does not get arbitrary command execution.
    let scope = if req.scope.is_empty() {
        Scope::Read
    } else {
        Scope::parse(&req.scope)
            .ok_or_else(|| WriteError::validation("scope", "scope must be \"read\" or \"write\""))?
    };
    let days = match req.expires_in_days {
        0 => DEFAULT_TOKEN_DAYS,
        d if !(1..=MAX_TOKEN_DAYS).contains(&d) => {
            return Err(WriteError::validation(
                "expires_in_days",
                format!("expires_in_days must be between 1 and {MAX_TOKEN_DAYS}"),
            ))
        }
        d => d,
    };

    let keypair = keys::current()
        .ok_or_else(|| WriteError::Fallback("no signing key installed".to_string()))?;

    // The id is minted first so it can be the token's `sub`: a decoded token
    // then names the row that can revoke it, which is what makes a token found
    // in someone's script traceable to a line in the Security tab.
    let id = uuid::Uuid::new_v4().to_string();
    let minted =
        token::mint(&keypair, &id, scope, days * 24 * 60 * 60).map_err(WriteError::Fallback)?;

    let created_at = gotime::now_go_text();
    let expires_at = gotime::go_text_at(minted.expires_at);

    let conn = super::db::open_read_write(db_path).map_err(WriteError::Fallback)?;
    super::migrate::verify(&conn).map_err(WriteError::Fallback)?;
    tokens::insert(
        &conn,
        &id,
        &name,
        scope,
        &minted.jti,
        &created_at,
        &expires_at,
    )
    .map_err(WriteError::Fallback)?;

    // Built by hand rather than re-read, so it must apply the same wire
    // conversion `TokenRow::from_row` does — otherwise the row a creation
    // answers with is spelled differently from the identical row the next
    // `GET /api/security/tokens` returns, and only one of them renders.
    let row = tokens::TokenRow {
        id: id.clone(),
        name: name.clone(),
        scope: scope.as_str().to_string(),
        created_at: tokens::wire_time(&created_at),
        expires_at: Some(tokens::wire_time(&expires_at)),
        last_used_at: None,
        revoked_at: None,
    };
    let body = gojson::to_vec(&CreatedToken {
        token_row: row,
        token: minted.token,
    })
    .map_err(|e| WriteError::Fallback(format!("encoding token: {e}")))?;

    // #335's convention. The **name and scope**, never the token or the `jti`:
    // the name is what makes the line worth having, and the credential is the
    // one thing this log must never carry.
    log::info!(
        "api token created id={id:?} name={name:?} scope={:?}",
        scope.as_str()
    );
    Ok(Answer::json_status(StatusCode::CREATED, body))
}

fn revoke_token(db_path: &std::path::Path, id: &str) -> Result<Answer, WriteError> {
    let conn = super::db::open_read_write(db_path).map_err(WriteError::Fallback)?;
    super::migrate::verify(&conn).map_err(WriteError::Fallback)?;
    match tokens::revoke(&conn, id).map_err(WriteError::Fallback)? {
        Some(_) => {
            log::info!("api token revoked id={id:?}");
            Ok(Answer::no_content())
        }
        None => Err(WriteError::NotFound {
            resource: "api token".to_string(),
            id: id.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_routes_are_claimed_and_their_neighbours_are_not() {
        assert!(claims(&Method::GET, "/api/security/keys"));
        assert!(claims(&Method::POST, "/api/security/keys/regenerate"));
        assert!(claims(&Method::GET, "/api/security/tokens"));
        assert!(claims(&Method::POST, "/api/security/tokens"));
        assert!(claims(&Method::DELETE, "/api/security/tokens/abc"));
        assert!(claims(&Method::GET, JWKS_PATH));

        // Method-separated, like every other module's routes.
        assert!(!claims(&Method::DELETE, "/api/security/keys"));
        assert!(!claims(&Method::GET, "/api/security/keys/regenerate"));
        assert!(!claims(&Method::PUT, "/api/security/tokens"));
        assert!(!claims(&Method::POST, JWKS_PATH));
        // A trailing slash is a different route to chi, and to this.
        assert!(!claims(&Method::GET, "/api/security/tokens/"));
        assert!(!claims(&Method::DELETE, "/api/security/tokens/"));
        // One segment only.
        assert!(!claims(&Method::DELETE, "/api/security/tokens/a/b"));
        assert!(!claims(&Method::GET, "/api/security"));
        assert!(!claims(&Method::GET, "/.well-known/openid-configuration"));
    }

    #[test]
    fn the_token_id_is_one_segment() {
        assert_eq!(token_id("/api/security/tokens/abc"), Some("abc"));
        assert_eq!(token_id("/api/security/tokens/"), None);
        assert_eq!(token_id("/api/security/tokens/a/b"), None);
        assert_eq!(token_id("/api/security/keys"), None);
    }

    /// The one exception to the method-based scope map, and the reason for it:
    /// these reads *are* the credential system, so a `read` token must not be
    /// able to enumerate every credential on the machine.
    #[test]
    fn the_security_reads_need_write_and_every_other_read_does_not() {
        assert_eq!(
            required_scope(&Method::GET, "/api/security/tokens"),
            Scope::Write
        );
        assert_eq!(
            required_scope(&Method::GET, "/api/security/keys"),
            Scope::Write
        );
        assert_eq!(required_scope(&Method::GET, "/api/agents"), Scope::Read);
        assert_eq!(required_scope(&Method::HEAD, "/api/agents"), Scope::Read);
        assert_eq!(required_scope(&Method::OPTIONS, "/api/agents"), Scope::Read);
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert_eq!(required_scope(&method, "/api/agents"), Scope::Write);
        }
        // JWKS is not under /api at all, so the guard never asks — but the
        // default must still not be `write`, or a future guard change would
        // lock out the one route that has to stay public.
        assert_eq!(required_scope(&Method::GET, JWKS_PATH), Scope::Read);
    }

    /// RFC 8037's shape, which is what makes "a stock library on the other side"
    /// true rather than hoped for.
    #[test]
    fn the_jwk_is_rfc_8037_shaped() {
        let keypair = keys::Keypair::generate().expect("generate");
        let jwk = jwk_for(&keypair);
        assert_eq!(jwk.kty, "OKP");
        assert_eq!(jwk.crv, "Ed25519");
        assert_eq!(jwk.alg, "EdDSA");
        assert_eq!(jwk.use_, "sig");
        assert_eq!(jwk.kid, keypair.kid());

        use base64::Engine;
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&jwk.x)
            .expect("x is base64url");
        assert_eq!(decoded, keypair.public_key());
    }

    /// **Neither key route's response carries the private key**, in any
    /// encoding.
    ///
    /// The other half of `keys::the_private_key_never_reaches_the_log`. Both
    /// documents here are *about* the keypair and one of them names the file it
    /// is stored in, which is exactly the shape someone widens by adding "and
    /// the key itself, for convenience". There is no route that returns it and
    /// there must not be one; this is what would fail if there were.
    #[test]
    fn no_key_route_can_answer_with_the_private_key() {
        use base64::Engine;

        // **Installs nothing.** `keys::install` replaces the process-wide key
        // every other test in this binary verifies against, and doing it here
        // made four `proxy` tests answer 401 — in another module, only under
        // parallelism. That is what `jwks_for` and `key_info_body` exist for.
        let keypair = keys::Keypair::generate().expect("generate");
        let pkcs8 = keypair.private_key_der_for_test().to_vec();
        let dir = std::path::Path::new("/tmp/agento-key-response-test");

        let bodies = [
            jwks_for(&keypair).expect("jwks").body.expect("body"),
            Answer::json(key_info_body(&keypair, dir).expect("keys"))
                .body
                .expect("body"),
        ];
        for body in bodies {
            let text = String::from_utf8(body).expect("utf8");
            for spelling in [
                base64::engine::general_purpose::STANDARD.encode(&pkcs8),
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&pkcs8),
                pkcs8.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            ] {
                assert!(
                    !text.contains(&spelling),
                    "a key route answered with the private key: {text}"
                );
            }
            // And the response is not empty, so the assertion above is about
            // what it contains rather than about there being nothing in it.
            assert!(text.contains("kid"), "{text}");
        }
    }

    /// **No key installed refuses everything.** The safe direction and the only
    /// one available: a listener that cannot verify a signature cannot tell a
    /// forged token from a real one, so it must accept neither. Reachable only
    /// through a `setup` that failed before loading the keypair, which `lib.rs`
    /// treats as fatal — but the guard must not depend on its caller for that,
    /// which is the lesson #400 recorded about its own empty-token case.
    #[test]
    fn no_installed_key_fails_closed() {
        let keypair = keys::Keypair::generate().expect("generate");
        let good = token::mint(&keypair, "s", Scope::Write, 600)
            .expect("mint")
            .token;

        for presented in ["", "anything", good.as_str()] {
            assert_eq!(
                verify_with(presented, &Method::GET, "/api/agents", None, &|_| false),
                Err(Denied::Unauthenticated),
                "{presented:?} must be refused when no key is installed"
            );
        }
        // ...and with the key it is the same token that is served, so the test
        // above is about the missing key rather than about the token.
        assert!(
            verify_with(&good, &Method::GET, "/api/agents", Some(&keypair), &|_| {
                false
            })
            .is_ok()
        );
    }

    /// Revocation reaches the guard's own entry point, not only the pure
    /// verifier — the wiring is where a predicate gets forgotten.
    #[test]
    fn a_revoked_credential_is_refused_at_the_guards_entry_point() {
        let keypair = keys::Keypair::generate().expect("generate");
        let minted = token::mint(&keypair, "s", Scope::Write, 600).expect("mint");
        let revoked = minted.jti.clone();

        assert!(verify_with(
            &minted.token,
            &Method::GET,
            "/api/agents",
            Some(&keypair),
            &|_| false
        )
        .is_ok());
        assert_eq!(
            verify_with(
                &minted.token,
                &Method::GET,
                "/api/agents",
                Some(&keypair),
                &|jti| jti == revoked
            ),
            Err(Denied::Unauthenticated)
        );
    }

    /// The serialized JWKS must carry `use`, not `use_` — the field is renamed,
    /// and a rename that silently stopped applying would produce a document a
    /// strict consumer rejects.
    #[test]
    fn the_jwks_document_uses_the_wire_field_names() {
        let keypair = keys::Keypair::generate().expect("generate");
        let body = gojson::to_vec(&Jwks {
            keys: vec![jwk_for(&keypair)],
        })
        .expect("encode");
        let text = String::from_utf8(body).expect("utf8");
        assert!(text.contains("\"use\":\"sig\""), "{text}");
        assert!(!text.contains("use_"), "{text}");
        assert!(text.contains("\"kty\":\"OKP\""));
        assert!(text.contains(&format!("\"kid\":\"{}\"", keypair.kid())));
    }
}
