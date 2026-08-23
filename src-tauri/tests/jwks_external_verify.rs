//! Can something that is not Agento verify an Agento token? (#405)
//!
//! This is the requirement that decided the whole design. #405 chose an
//! asymmetric signature over the more common answer — opaque random keys hashed
//! in SQLite, as GitHub PATs and Stripe and Tailscale do it — for exactly two
//! properties, and **offline verification by another process is the one opaque
//! keys cannot do at all**. So it is the one that has to be proven rather than
//! asserted: everything else in the port can be checked from inside the crate,
//! and this cannot.
//!
//! # What "external" means here, and what it does not
//!
//! This is a separate test binary, but it is still Rust and still links the same
//! `jsonwebtoken`. What makes it a real proof is not the language — it is the
//! **input**: every verification below is built from the bytes of the JWKS
//! document and nothing else. It never touches `security::keys::Keypair`, never
//! calls its `decoding()`, and never sees the private key. If the JWKS were
//! missing a field, spelled `x` in the wrong encoding, or naming an algorithm
//! that does not match how the token was signed, these tests fail — which is the
//! whole contract a consumer depends on.
//!
//! A genuinely different *implementation* is `scripts/verify-jwks.py`, which
//! does the same thing through PyJWT over OpenSSL rather than `ring`. It is not
//! run here because CI has no Python JWT library; it is the by-hand check for
//! anyone changing this surface, and its header says so.

use agento_lib::native::security::keys::Keypair;
use agento_lib::native::security::token::{self, Scope};
use agento_lib::native::security::JWKS_PATH;
use axum::http::Method;
use base64::Engine;

/// Serialises the two tests below.
///
/// Both install into `security::keys`' process-wide slot and then read the JWKS
/// the *route* serves, so run in parallel one would assert against the other's
/// key. That is not a hypothetical: the first assertion here is precisely
/// "the served document names the key in force", which is the one a swap
/// between the install and the read would break — and it would break
/// intermittently, which is worse than not having it. `cargo test` runs a
/// binary's tests on several threads by default; this is what makes that safe
/// without `--test-threads=1`, which would be a rule nobody remembers.
fn key_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Fetch the JWKS document the way the route serves it, through the real
/// registry rather than by calling the handler directly.
fn jwks_document() -> serde_json::Value {
    let answer = agento_lib::native::serve(&agento_lib::native::Request {
        method: &Method::GET,
        path: JWKS_PATH,
        query: "",
        content_type: "",
        secret_token: "",
        body: &[],
    })
    .expect("the jwks route answers");
    assert_eq!(answer.status, axum::http::StatusCode::OK);
    serde_json::from_slice(&answer.body.expect("a body")).expect("jwks is json")
}

/// Everything a consumer is allowed to know: the parsed JWKS, and nothing else.
struct ExternalVerifier {
    keys: Vec<(String, jsonwebtoken::DecodingKey)>,
}

impl ExternalVerifier {
    /// Build from the JWKS bytes alone.
    ///
    /// This is the code a consumer writes, and it is deliberately written the
    /// long way rather than through a helper: what is being tested is that the
    /// document contains enough to do this at all.
    fn from_jwks(document: &serde_json::Value) -> Self {
        let keys = document["keys"].as_array().expect("keys is an array");
        assert!(!keys.is_empty(), "an empty JWKS verifies nothing");
        Self {
            keys: keys
                .iter()
                .map(|jwk| {
                    assert_eq!(jwk["kty"], "OKP", "Ed25519 keys are OKP (RFC 8037)");
                    assert_eq!(jwk["crv"], "Ed25519");
                    assert_eq!(jwk["alg"], "EdDSA");
                    assert_eq!(jwk["use"], "sig");
                    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(jwk["x"].as_str().expect("x is a string"))
                        .expect("x is base64url");
                    assert_eq!(raw.len(), 32, "an Ed25519 public key is 32 bytes");
                    (
                        jwk["kid"].as_str().expect("kid is a string").to_string(),
                        jsonwebtoken::DecodingKey::from_ed_der(&raw),
                    )
                })
                .collect(),
        }
    }

    /// Verify a token, picking the key by the `kid` in its header — which is
    /// what the `kid` is for, and the reason JWKS is a *set* even though this
    /// install only ever has one key in it.
    fn verify(&self, token: &str) -> Result<String, String> {
        let header = jsonwebtoken::decode_header(token).map_err(|e| e.to_string())?;
        let kid = header.kid.ok_or("no kid in the header")?;
        let key = self
            .keys
            .iter()
            .find(|(k, _)| *k == kid)
            .map(|(_, key)| key)
            .ok_or_else(|| format!("no key for kid {kid}"))?;

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::EdDSA);
        validation.set_issuer(&["agento"]);
        validation.set_audience(&["agento-api"]);
        let data = jsonwebtoken::decode::<serde_json::Value>(token, key, &validation)
            .map_err(|e| e.to_string())?;
        Ok(data.claims["scope"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }
}

/// The whole requirement, in one test: a consumer holding only the JWKS accepts
/// what Agento issued and rejects what it did not.
#[test]
fn a_consumer_holding_only_the_jwks_verifies_agentos_tokens_and_rejects_others() {
    let _lock = key_lock();
    let keypair = Keypair::generate().expect("generate");
    let kid = keypair.kid().to_string();
    agento_lib::native::security::keys::install(keypair);

    let document = jwks_document();
    assert_eq!(
        document["keys"][0]["kid"].as_str(),
        Some(kid.as_str()),
        "the served JWKS must name the key actually in force"
    );

    let verifier = ExternalVerifier::from_jwks(&document);

    // Issued by Agento: accepted, with its scope legible to the consumer — the
    // point of putting the scope in the claims rather than in a lookup only
    // Agento can do.
    let installed = agento_lib::native::security::keys::current().expect("installed");
    let mine = token::mint(&installed, "some-token", Scope::Read, 600)
        .expect("mint")
        .token;
    assert_eq!(verifier.verify(&mine).as_deref(), Ok("read"));

    let write = token::mint(&installed, "some-token", Scope::Write, 600)
        .expect("mint")
        .token;
    assert_eq!(verifier.verify(&write).as_deref(), Ok("write"));

    // Signed by somebody else's Ed25519 key: refused. This is the half that
    // makes the JWKS worth publishing — anyone can *read* it, and reading it
    // does not let them mint.
    let impostor = Keypair::generate().expect("generate");
    let forged = token::mint(&impostor, "some-token", Scope::Write, 600)
        .expect("mint")
        .token;
    assert!(
        verifier.verify(&forged).is_err(),
        "a token signed by another key must not verify"
    );
}

/// A verifier that knows the `kid` but not the key must still fail.
///
/// The `kid` is a hint for *selecting* a key, never a credential — and reading
/// it out of a token anyone can decode is trivial, so a consumer that trusted it
/// would be trusting the attacker's own input.
#[test]
fn a_matching_kid_does_not_substitute_for_a_matching_signature() {
    let _lock = key_lock();
    let real = Keypair::generate().expect("generate");
    let real_kid = real.kid().to_string();
    agento_lib::native::security::keys::install(real);

    let verifier = ExternalVerifier::from_jwks(&jwks_document());

    // A token signed by another key, relabelled with the real key's `kid`.
    let impostor = Keypair::generate().expect("generate");
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA);
    header.kid = Some(real_kid);
    let forged = jsonwebtoken::encode(
        &header,
        &serde_json::json!({
            "iss": "agento",
            "aud": "agento-api",
            "sub": "impostor",
            "scope": "write",
            "jti": "j",
            "iat": 0,
            "exp": 4_102_444_800i64,
        }),
        impostor.encoding(),
    )
    .expect("encode");

    assert!(
        verifier.verify(&forged).is_err(),
        "the kid selects a key; it does not vouch for one"
    );
}

/// The JWKS answers a request that carries no credential at all.
///
/// Not a convenience: a public key is public, and requiring a credential to
/// fetch the thing credentials are verified against is a bootstrap problem with
/// no answer. Asserted here through the route's own placement outside `/api`,
/// which is what `guards::reject` keys on.
#[test]
fn the_jwks_route_is_outside_the_guarded_prefix() {
    assert!(
        !JWKS_PATH.starts_with("/api"),
        "{JWKS_PATH} would be credential-guarded if it were under /api"
    );
    // ...and it is still routed rather than falling through to the frontend
    // assets, which in a release build would answer 404 while every in-app test
    // still passed.
    assert!(agento_lib::native::claims(&Method::GET, JWKS_PATH));
}
