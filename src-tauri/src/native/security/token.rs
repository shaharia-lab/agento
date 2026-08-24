//! Minting and verifying the JWTs that authenticate `/api` (#405).
//!
//! Every credential this build accepts is an EdDSA (Ed25519) JWT signed by the
//! install's own keypair ([`super::keys`]) — the app's webview session, the dev
//! token file, and every token the user creates in Settings → Security. There is
//! one format and one verification path, so a token's *origin* changes nothing
//! about how it is checked.
//!
//! # This module is pure, and that is what makes the hard cases testable
//!
//! [`verify_against`] takes the keypair and a revocation predicate as
//! parameters rather than reading process-wide state. The single most important
//! property in #405 — *a token minted under key A is refused after regenerating
//! to key B* — is one call with two keypairs here, where against a global it
//! would mean mutating shared state that every other test in the binary reads.
//! `super::verify_request` is the thin wiring that supplies the globals.
//!
//! # Why EdDSA and not RS256
//!
//! Equivalent security at 32 bytes rather than ~2000, no padding scheme or
//! parameter choices to get wrong, and a `kid`+JWKS pair that a stock library on
//! the other side consumes without any PEM handling. Recorded in #405's scoping
//! so it is not relitigated.
//!
//! # The claims, and why each one is there
//!
//! - `iss` / `aud` — pinned to [`ISSUER`] and [`AUDIENCE`] and **required**, so
//!   a JWT this install signed for some other purpose is not an API credential.
//!   `aud` in particular is here from day one precisely because Agento issuing
//!   tokens *for* another service is explicitly out of scope: having the claim
//!   means that day does not need a format change.
//! - `sub` — who this is. `desktop-webview` for the app's own session, the
//!   token's row id for a user token. It is what a log line or the token list
//!   can name.
//! - `scope` — `read` or `write`, compared against what the request's method
//!   needs. Two levels, mapped onto `guards::is_state_changing`, so there is one
//!   definition of read-versus-write in the tree; a per-route permission model
//!   over ~90 endpoints is its own design and is out of scope.
//! - `jti` — the revocation handle. Regenerating the keypair kills everything at
//!   once; `jti` is what kills exactly one.
//! - `iat` / `exp` — with [`LEEWAY`] on the latter, because a laptop that
//!   suspends is the normal case rather than the exotic one.

use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, DecodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use super::keys::Keypair;

/// `iss`. Constant, and required on every token.
pub const ISSUER: &str = "agento";

/// `aud`. Names the *API* rather than the app, so a future token minted for
/// something else is distinguishable by audience alone.
pub const AUDIENCE: &str = "agento-api";

/// Slack allowed on `exp`, in seconds.
///
/// Two minutes, and it is for suspend and clock skew rather than for
/// convenience: a machine whose lid was shut across the boundary comes back with
/// a wall clock that jumped, and a verifier with no leeway turns that into a
/// session that has to be re-established for no reason. Small enough that it
/// buys a revoked token nothing.
pub const LEEWAY: u64 = 120;

/// How long the app's own webview session is good for.
///
/// Long, deliberately. `host_info` mints a fresh one on every invocation and
/// `api.ts` re-invokes it on a 401, so expiry is not what keeps the app working
/// — it is what bounds a token that leaked out of the dev token file or a
/// screenshot. Thirty days is past any plausible single session and far short of
/// forever.
pub const SESSION_TTL_SECONDS: i64 = 30 * 24 * 60 * 60;

/// `sub` for the app's own session. Carries no database row, by design: it is
/// minted on demand and revoked by regenerating the keypair, so there is nothing
/// to store and nothing to leave behind.
pub const WEBVIEW_SUBJECT: &str = "desktop-webview";

/// What a credential is allowed to do.
///
/// `read` and `write` are a **hierarchy** and map exactly onto the split
/// `guards::is_state_changing` already encodes (#405), so the guard has one
/// definition of read-versus-write rather than a second permission table that
/// can drift from the first. [`Llm`](Self::Llm) is not part of that hierarchy —
/// see its own note, and [`covers`](Self::covers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every safe method. **Not "harmless"** — a `read` token returns chat
    /// transcripts, agent system prompts and the integration list, which the
    /// creation UI says in as many words rather than implying a least privilege
    /// it does not have.
    Read,
    /// Everything, including creating a `bypass`-permission agent and running
    /// it — i.e. arbitrary command execution on this machine.
    Write,
    /// The LLM gateway's data plane, and **nothing else** (#423).
    ///
    /// Deliberately disjoint from the pair above rather than sitting under
    /// `write`, because the two directions are both wrong:
    ///
    /// - A gateway token is what gets pasted into `OPENAI_API_KEY`,
    ///   `ANTHROPIC_AUTH_TOKEN` and a Claude Code base-URL setup — plaintext on
    ///   disk, in files other tools read. A `write` token in that position is
    ///   arbitrary command execution; a `read` token in that position returns
    ///   every chat transcript on the machine. Neither is an acceptable price
    ///   for spending provider credits.
    /// - Conversely, a credential whose job is to spend provider credits has no
    ///   business reading chat history, so `llm` grants nothing on `/api`.
    ///
    /// The `aud` claim is still `agento-api` for all three: scope is the
    /// capability axis, and a second audience would mean a parallel validation
    /// path for no gain.
    Llm,
}

impl Scope {
    /// The wire spelling, which is also what the `api_tokens` row stores.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Llm => "llm",
        }
    }

    /// Parse the wire spelling. `None` for anything else — including a scope a
    /// *future* build might mint, which this one must refuse rather than guess
    /// at.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "llm" => Some(Self::Llm),
            _ => None,
        }
    }

    /// Whether a credential carrying `self` may serve a request needing
    /// `required`. `write` covers `read`; nothing covers `write`; and `llm`
    /// covers only itself, in both directions.
    ///
    /// **The arms are enumerated, and that is load-bearing.** This was
    /// `(Self::Write, _) | (Self::Read, Scope::Read)` until #423, and the
    /// wildcard was correct only while `Write` was the top of a two-element
    /// lattice. Adding [`Llm`](Self::Llm) under that wildcard would have made a
    /// `write` token a gateway credential — silently, with every existing test
    /// still green, and defeating the one property the scope exists for. Do not
    /// reintroduce a wildcard here: a new variant must be a deliberate line.
    pub fn covers(self, required: Scope) -> bool {
        matches!(
            (self, required),
            (Self::Write, Scope::Write)
                | (Self::Write, Scope::Read)
                | (Self::Read, Scope::Read)
                | (Self::Llm, Scope::Llm)
        )
    }
}

/// The claims, in the order they are written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub iss: String,
    pub aud: String,
    pub sub: String,
    pub scope: String,
    pub jti: String,
    pub iat: i64,
    pub exp: i64,
}

/// A freshly minted credential and the metadata a caller needs to record.
///
/// The token string itself is the only copy that will ever exist: nothing
/// persists it, and the creation UI displays it once. What is stored for a user
/// token is `jti` and the two timestamps.
pub struct Minted {
    pub token: String,
    pub jti: String,
    pub issued_at: i64,
    pub expires_at: i64,
}

/// A credential that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub subject: String,
    pub scope: Scope,
    pub jti: String,
}

/// Why a credential was refused, and with which status.
///
/// The split is the ordinary HTTP one and it is worth keeping: **401 means the
/// caller has not proved who it is** — absent, malformed, signed by another key,
/// expired, wrong `aud`, or revoked — and **403 means it has, and still may not
/// do this**. Collapsing revoked into 403 was considered and rejected: a revoked
/// token is not a credential at all any more, and answering 403 would tell a
/// holder that their token is still recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// 401.
    Unauthenticated,
    /// 403 — verified, but `scope` does not cover the request's method.
    InsufficientScope,
}

/// Seconds since the epoch, as the claims spell time.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Sign a token for `subject` with `scope`, good for `ttl_seconds`.
pub fn mint(
    keypair: &Keypair,
    subject: &str,
    scope: Scope,
    ttl_seconds: i64,
) -> Result<Minted, String> {
    let jti = uuid::Uuid::new_v4().simple().to_string();
    let issued_at = now();
    let expires_at = issued_at.saturating_add(ttl_seconds.max(1));

    let claims = Claims {
        iss: ISSUER.to_string(),
        aud: AUDIENCE.to_string(),
        sub: subject.to_string(),
        scope: scope.as_str().to_string(),
        jti: jti.clone(),
        iat: issued_at,
        exp: expires_at,
    };

    // The `kid` is what lets a verifier holding several keys pick one, and what
    // makes a regenerate legible in a decoded token rather than only in a
    // signature failure.
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some(keypair.kid().to_string());

    let token = jsonwebtoken::encode(&header, &claims, keypair.encoding())
        .map_err(|e| format!("signing token: {e}"))?;

    Ok(Minted {
        token,
        jti,
        issued_at,
        expires_at,
    })
}

/// The validation rules, built once per verification.
///
/// **`Validation::new(Algorithm::EdDSA)` is load-bearing**, not a default being
/// spelled out: it restricts the accepted `alg` to exactly one, which is what
/// stops the whole family of algorithm-confusion attacks — a token headed
/// `alg: none`, or `alg: HS256` signed with the *public* key as an HMAC secret,
/// which is a public key by definition. A validator that accepted whatever the
/// header claimed would verify both.
fn validation() -> Validation {
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[AUDIENCE]);
    validation.leeway = LEEWAY;
    // Beyond the default `exp`: a token missing any of these is not one this
    // build minted, and defaulting a missing claim would make the checks above
    // vacuous rather than failed.
    validation.required_spec_claims = ["exp", "iss", "aud", "sub", "jti"]
        .map(str::to_string)
        .into();
    validation
}

/// Verify a presented token against an explicit key and revocation predicate.
///
/// `is_revoked` is a parameter rather than a global for the reason in the module
/// header, and because it makes the ordering explicit: **the signature and the
/// claims are checked before the revocation list is consulted.** A forged token
/// must not get to ask whether an arbitrary `jti` is revoked.
pub fn verify_against(
    presented: &str,
    required: Scope,
    decoding: &DecodingKey,
    is_revoked: &dyn Fn(&str) -> bool,
) -> Result<Verified, Denied> {
    let data = jsonwebtoken::decode::<Claims>(presented, decoding, &validation())
        .map_err(|_| Denied::Unauthenticated)?;
    let claims = data.claims;

    if is_revoked(&claims.jti) {
        return Err(Denied::Unauthenticated);
    }

    // An unrecognised scope covers nothing. A token minted by a future build
    // with a third level must be refused here rather than guessed at — the
    // guess that widens is the one this port must never make.
    let scope = Scope::parse(&claims.scope).ok_or(Denied::InsufficientScope)?;
    if !scope.covers(required) {
        return Err(Denied::InsufficientScope);
    }

    Ok(Verified {
        subject: claims.sub,
        scope,
        jti: claims.jti,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_revoked(_: &str) -> bool {
        false
    }

    fn keypair() -> Keypair {
        Keypair::generate().expect("generate")
    }

    fn verify(token: &str, required: Scope, keypair: &Keypair) -> Result<Verified, Denied> {
        verify_against(token, required, keypair.decoding(), &never_revoked)
    }

    #[test]
    fn a_minted_token_verifies_against_its_own_key() {
        let keys = keypair();
        let minted = mint(&keys, WEBVIEW_SUBJECT, Scope::Write, 60).expect("mint");
        let verified = verify(&minted.token, Scope::Write, &keys).expect("verify");
        assert_eq!(verified.subject, WEBVIEW_SUBJECT);
        assert_eq!(verified.scope, Scope::Write);
        assert_eq!(verified.jti, minted.jti);
    }

    /// The token carries its key's `kid`, which is what lets an external
    /// verifier pick a key out of the JWKS rather than trying each in turn.
    #[test]
    fn the_header_names_the_key_and_the_algorithm() {
        let keys = keypair();
        let minted = mint(&keys, "s", Scope::Read, 60).expect("mint");
        let header = jsonwebtoken::decode_header(&minted.token).expect("header");
        assert_eq!(header.alg, Algorithm::EdDSA);
        assert_eq!(header.kid.as_deref(), Some(keys.kid()));
    }

    /// **The property the whole asymmetric design exists for.** Regenerating the
    /// keypair refuses every token ever issued, with no denylist and no
    /// per-token bookkeeping — every old signature simply stops verifying.
    #[test]
    fn a_token_from_the_old_key_is_refused_after_a_regenerate() {
        let before = keypair();
        let minted = mint(&before, WEBVIEW_SUBJECT, Scope::Write, 3600).expect("mint");
        assert!(verify(&minted.token, Scope::Write, &before).is_ok());

        let after = keypair();
        assert_eq!(
            verify(&minted.token, Scope::Write, &after),
            Err(Denied::Unauthenticated)
        );
    }

    #[test]
    fn an_expired_token_is_refused() {
        let keys = keypair();
        // Past the leeway, or this would assert the leeway rather than the
        // expiry.
        let claims = Claims {
            iss: ISSUER.to_string(),
            aud: AUDIENCE.to_string(),
            sub: "s".to_string(),
            scope: "write".to_string(),
            jti: "j".to_string(),
            iat: now() - 7200,
            exp: now() - (LEEWAY as i64) - 60,
        };
        let token = jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), &claims, keys.encoding())
            .expect("encode");
        assert_eq!(
            verify(&token, Scope::Read, &keys),
            Err(Denied::Unauthenticated)
        );
    }

    /// A token that expired *within* the leeway is still served — the suspend
    /// case, which is why the leeway exists.
    #[test]
    fn a_token_inside_the_leeway_is_still_served() {
        let keys = keypair();
        let claims = Claims {
            iss: ISSUER.to_string(),
            aud: AUDIENCE.to_string(),
            sub: "s".to_string(),
            scope: "read".to_string(),
            jti: "j".to_string(),
            iat: now() - 600,
            exp: now() - 10,
        };
        let token = jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), &claims, keys.encoding())
            .expect("encode");
        assert!(verify(&token, Scope::Read, &keys).is_ok());
    }

    #[test]
    fn the_wrong_issuer_or_audience_is_refused() {
        let keys = keypair();
        for (iss, aud) in [
            ("someone-else", AUDIENCE),
            (ISSUER, "some-other-api"),
            ("", ""),
        ] {
            let claims = Claims {
                iss: iss.to_string(),
                aud: aud.to_string(),
                sub: "s".to_string(),
                scope: "write".to_string(),
                jti: "j".to_string(),
                iat: now(),
                exp: now() + 600,
            };
            let token =
                jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), &claims, keys.encoding())
                    .expect("encode");
            assert_eq!(
                verify(&token, Scope::Read, &keys),
                Err(Denied::Unauthenticated),
                "iss={iss:?} aud={aud:?} should be refused"
            );
        }
    }

    /// The alg is pinned to EdDSA, which is what shuts out the confusion
    /// attacks. `alg: none` is the cheapest to demonstrate: it needs no key at
    /// all, so it is what an attacker who has read the JWKS reaches for first.
    #[test]
    fn an_unsigned_token_is_refused_however_well_formed() {
        use base64::Engine;
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let keys = keypair();
        let header = engine.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let claims = engine.encode(
            serde_json::to_vec(&Claims {
                iss: ISSUER.to_string(),
                aud: AUDIENCE.to_string(),
                sub: "s".to_string(),
                scope: "write".to_string(),
                jti: "j".to_string(),
                iat: now(),
                exp: now() + 600,
            })
            .expect("claims"),
        );
        let token = format!("{header}.{claims}.");
        assert_eq!(
            verify(&token, Scope::Read, &keys),
            Err(Denied::Unauthenticated)
        );
    }

    #[test]
    fn garbage_is_refused_rather_than_panicking() {
        let keys = keypair();
        for raw in ["", "not-a-jwt", "a.b.c", "....", "Bearer x"] {
            assert_eq!(
                verify(raw, Scope::Read, &keys),
                Err(Denied::Unauthenticated),
                "{raw:?} should be refused"
            );
        }
    }

    /// A revoked `jti` is refused while the keypair is unchanged — the
    /// per-token half of the revocation story, beside regenerate's all-at-once
    /// half.
    #[test]
    fn a_revoked_jti_is_refused() {
        let keys = keypair();
        let minted = mint(&keys, "token-1", Scope::Write, 3600).expect("mint");
        let revoked = minted.jti.clone();
        let is_revoked = move |jti: &str| jti == revoked;

        assert_eq!(
            verify_against(&minted.token, Scope::Write, keys.decoding(), &is_revoked),
            Err(Denied::Unauthenticated)
        );
        // A different token under the same key is untouched.
        let other = mint(&keys, "token-2", Scope::Write, 3600).expect("mint");
        assert!(verify_against(&other.token, Scope::Write, keys.decoding(), &is_revoked).is_ok());
    }

    /// **The scope split.** A `read` token serves the safe methods and is
    /// refused — 403, not 401 — on anything that changes state.
    #[test]
    fn a_read_token_cannot_write_and_a_write_token_can_read() {
        let keys = keypair();
        let read = mint(&keys, "r", Scope::Read, 600).expect("mint");
        let write = mint(&keys, "w", Scope::Write, 600).expect("mint");

        assert!(verify(&read.token, Scope::Read, &keys).is_ok());
        assert_eq!(
            verify(&read.token, Scope::Write, &keys),
            Err(Denied::InsufficientScope)
        );
        assert!(verify(&write.token, Scope::Read, &keys).is_ok());
        assert!(verify(&write.token, Scope::Write, &keys).is_ok());
    }

    /// A scope this build does not know covers nothing. The alternative — treat
    /// an unknown scope as `read` — is a guess that *widens*, which is the one
    /// direction a credential check must never move in.
    #[test]
    fn an_unrecognised_scope_covers_nothing() {
        let keys = keypair();
        let claims = Claims {
            iss: ISSUER.to_string(),
            aud: AUDIENCE.to_string(),
            sub: "s".to_string(),
            scope: "admin".to_string(),
            jti: "j".to_string(),
            iat: now(),
            exp: now() + 600,
        };
        let token = jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), &claims, keys.encoding())
            .expect("encode");
        assert_eq!(
            verify(&token, Scope::Read, &keys),
            Err(Denied::InsufficientScope)
        );
        assert_eq!(Scope::parse("admin"), None);
        assert_eq!(Scope::parse("READ"), None, "the spelling is exact");
        assert_eq!(Scope::parse("LLM"), None, "the spelling is exact");
    }

    /// The whole lattice, stated as a 3×3 matrix rather than spot-checked.
    ///
    /// Exhaustive because the interesting cases are the *false* ones, and a
    /// spot-check of the true ones is exactly what would have passed against the
    /// `(Write, _)` wildcard this replaced (#423): `Write.covers(Llm)` is the
    /// assertion that fails if a wildcard ever comes back.
    #[test]
    fn scope_covering_is_the_split_the_guard_needs() {
        // write: the top of the /api hierarchy, and nothing more.
        assert!(Scope::Write.covers(Scope::Write));
        assert!(Scope::Write.covers(Scope::Read));
        assert!(
            !Scope::Write.covers(Scope::Llm),
            "write must not reach the gateway: a token pasted into a tool config \
             would carry arbitrary command execution"
        );

        // read: itself only.
        assert!(Scope::Read.covers(Scope::Read));
        assert!(!Scope::Read.covers(Scope::Write));
        assert!(!Scope::Read.covers(Scope::Llm));

        // llm: itself only, in both directions.
        assert!(Scope::Llm.covers(Scope::Llm));
        assert!(
            !Scope::Llm.covers(Scope::Read),
            "a gateway credential must not read chat transcripts"
        );
        assert!(!Scope::Llm.covers(Scope::Write));

        assert_eq!(Scope::Read.as_str(), "read");
        assert_eq!(Scope::Write.as_str(), "write");
        assert_eq!(Scope::Llm.as_str(), "llm");
    }

    /// Every scope survives the wire, because the row stores the spelling and
    /// the guard parses it back. A variant added to one match arm and not the
    /// other is a token that mints and then never verifies.
    #[test]
    fn a_round_trip_through_the_wire_spelling_is_identity() {
        for scope in [Scope::Read, Scope::Write, Scope::Llm] {
            assert_eq!(
                Scope::parse(scope.as_str()),
                Some(scope),
                "{:?} must survive the wire",
                scope
            );
        }
    }

    /// Two mints are two tokens, so revoking one cannot touch the other.
    #[test]
    fn every_mint_gets_its_own_jti() {
        let keys = keypair();
        let a = mint(&keys, "s", Scope::Read, 60).expect("a");
        let b = mint(&keys, "s", Scope::Read, 60).expect("b");
        assert_ne!(a.jti, b.jti);
        assert_ne!(a.token, b.token);
    }

    /// The claims a *signature* cannot protect: `exp` must actually be in the
    /// future, or the app would mint itself a credential that is already dead.
    #[test]
    fn a_minted_token_expires_after_it_was_issued() {
        let keys = keypair();
        let minted = mint(&keys, "s", Scope::Read, SESSION_TTL_SECONDS).expect("mint");
        assert_eq!(minted.expires_at - minted.issued_at, SESSION_TTL_SECONDS);
        assert!(minted.expires_at > now());
    }
}
