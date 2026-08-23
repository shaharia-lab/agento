//! The per-install Ed25519 signing keypair (#405).
//!
//! One keypair per install, created on first run beside the database and reused
//! on every launch after it. Everything that authenticates against `/api` is a
//! JWT signed by it — the app's own webview session included — so this file is
//! the root of the whole credential system, and the three properties below are
//! what make that safe.
//!
//! # It is create-if-absent, never regenerate-on-doubt
//!
//! [`load_or_create`] writes a keypair only when there is none to load. A
//! *failure* to read an existing private key is an error, never a silent
//! replacement: regenerating invalidates every token the user has issued and
//! signs the app out of itself, which is the correct answer to "revoke
//! everything now" and the wrong answer to a transient `EACCES`. The only path
//! that replaces key material is [`regenerate`], reached from
//! `POST /api/security/keys/regenerate` and nowhere else.
//!
//! # The private key has no reader outside this module
//!
//! It is never returned from an `/api` handler, never rendered in the UI and
//! never logged. [`Keypair`] deliberately derives neither `Serialize` nor
//! `Debug` — a `{keys:?}` in a log line is the same leak with a longer fuse,
//! which is the rule `native::integrations::registry::HostingRow` already
//! carries for credentials. What leaves this module is the **public** key, the
//! `kid`, and signatures.
//!
//! That is also why there is no "show me the private key" affordance anywhere:
//! `CLAUDE.md`'s standing rule — *"Do not introduce a UI that echoes them back;
//! the API scrubs them and the UI must not reintroduce them"* — applies to a
//! durable signing key more than to anything it was written for. Nothing needs
//! it. What another service consumes is the public half, over JWKS.
//!
//! # A durable secret is new here, and it is the price of the design
//!
//! #400's credential died with the process. This one mints valid tokens for as
//! long as it exists on disk, and a copy of it is silent. That cost buys the two
//! properties #405 exists for — offline verification by a process that is not
//! Agento, and regenerate-invalidates-everything with no denylist — and it is
//! bounded by `0600`, by having no reader, and by regenerate being one click.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use ring::signature::{Ed25519KeyPair, KeyPair};

/// `kid` length in hex characters: the first 16 bytes of the SHA-256 of the
/// public key.
///
/// 128 bits of a collision-resistant digest, which is far past what a per-install
/// key identifier needs, and the same 32-hex-character shape #400's token had —
/// so it reads as an Agento identifier to anyone who has seen one.
const KID_BYTES: usize = 16;

/// A loaded keypair: what signs, what verifies, and what names it.
///
/// Neither `Serialize` nor `Debug`, deliberately — see the module header.
pub struct Keypair {
    /// PKCS#8 v2 DER, exactly the bytes `ring` generated.
    ///
    /// Kept rather than re-derived because `jsonwebtoken`'s `EncodingKey` is
    /// opaque: signing needs the DER and so does writing the file, and deriving
    /// one from the other twice is one more place for them to disagree.
    pkcs8: Vec<u8>,
    /// The raw 32-byte Ed25519 public key — the `x` of the JWK, and what a
    /// verifier holds.
    public: Vec<u8>,
    kid: String,
    encoding: jsonwebtoken::EncodingKey,
    decoding: jsonwebtoken::DecodingKey,
}

impl Keypair {
    /// Build from PKCS#8 DER, deriving everything else.
    ///
    /// The parse is what rejects a corrupt private key, and it is why
    /// [`load_or_create`] can promise a clean failure rather than a keypair that
    /// signs nothing.
    fn from_pkcs8(pkcs8: Vec<u8>) -> Result<Self, String> {
        let parsed = Ed25519KeyPair::from_pkcs8(&pkcs8)
            .map_err(|_| "not a valid Ed25519 PKCS#8 private key".to_string())?;
        let public = parsed.public_key().as_ref().to_vec();
        let kid = kid_for(&public);
        Ok(Self {
            encoding: jsonwebtoken::EncodingKey::from_ed_der(&pkcs8),
            // Despite the name, `jsonwebtoken` hands these bytes straight to
            // `ring`'s `UnparsedPublicKey`, which for Ed25519 takes the **raw**
            // 32-byte key rather than a SubjectPublicKeyInfo wrapper. Pinned by
            // `a_minted_token_verifies_against_its_own_key`, which would fail
            // outright if this were the wrong encoding.
            decoding: jsonwebtoken::DecodingKey::from_ed_der(&public),
            pkcs8,
            public,
            kid,
        })
    }

    /// A fresh keypair from the OS CSPRNG. In memory only; nothing is written.
    pub fn generate() -> Result<Self, String> {
        let rng = ring::rand::SystemRandom::new();
        let document = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| "generating an Ed25519 keypair failed".to_string())?;
        Self::from_pkcs8(document.as_ref().to_vec())
    }

    /// The raw 32-byte public key.
    pub fn public_key(&self) -> &[u8] {
        &self.public
    }

    /// This key's identifier, carried in every token's JWT header and in the
    /// JWK. A verifier uses it to pick a key; a user uses it to tell at a glance
    /// whether a regenerate has happened.
    pub fn kid(&self) -> &str {
        &self.kid
    }

    pub fn encoding(&self) -> &jsonwebtoken::EncodingKey {
        &self.encoding
    }

    pub fn decoding(&self) -> &jsonwebtoken::DecodingKey {
        &self.decoding
    }

    /// The PKCS#8 DER, **for tests only**.
    ///
    /// `#[cfg(test)]` rather than a plain accessor, and that is the point: the
    /// private key must have no reader in a shipped binary, so the one thing
    /// that needs to *see* it — a test asserting it appears in no response and
    /// no log line — gets a door that compiles out. A `pub fn` here would be
    /// exactly the widening those tests exist to catch.
    #[cfg(test)]
    pub(crate) fn private_key_der_for_test(&self) -> &[u8] {
        &self.pkcs8
    }
}

/// The `kid` for a public key: base16 of the first [`KID_BYTES`] of its SHA-256.
fn kid_for(public: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, public);
    digest.as_ref()[..KID_BYTES]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The keypair this process signs and verifies with.
///
/// A `RwLock` rather than the `OnceLock` #400's token used, because
/// [`regenerate`] replaces it while the listener is serving: every request takes
/// a read, and one request — the regenerate itself — takes the write. It is the
/// same process-wide shape `native::scan::state`, `native::chat::live` and
/// `native::integrations::registry` use, for the same reason: `guards::reject`
/// is called from a plain router function with no `tauri::State` to extract.
static KEYPAIR: RwLock<Option<Arc<Keypair>>> = RwLock::new(None);

/// The keypair in force, or `None` before startup has installed one.
///
/// **`None` refuses every request**, which is the safe direction and the only
/// one available: a listener with no verifying key cannot tell a forged token
/// from a real one, so it must accept neither. The only way to reach it is a
/// `setup` that failed before [`load_or_create`], and `lib.rs` treats that as
/// fatal.
pub fn current() -> Option<Arc<Keypair>> {
    KEYPAIR
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(Arc::clone))
}

/// Install a keypair, replacing whatever was there.
pub fn install(keypair: Keypair) -> Arc<Keypair> {
    let shared = Arc::new(keypair);
    if let Ok(mut guard) = KEYPAIR.write() {
        *guard = Some(Arc::clone(&shared));
    }
    shared
}

/// Where the private key lives.
pub fn private_key_path(dir: &Path) -> PathBuf {
    dir.join("api-signing-key.pk8")
}

/// Where the public key lives, base64url-encoded so it can be read and pasted.
pub fn public_key_path(dir: &Path) -> PathBuf {
    dir.join("api-signing-key.pub")
}

/// Load the install's keypair, creating one on first run.
///
/// `dir` is the data directory — the keys sit beside `agento.db` because they
/// are per-install state exactly as it is, and because a debug build's separate
/// data dir then gets its own keypair for free, so a development launch can
/// never mint a token the release install would honour.
///
/// # Failure is loud
///
/// Every error here is returned, and `lib.rs` makes it fatal. An unreadable or
/// corrupt private key must not fall back to generating a new one (that would
/// silently invalidate every issued token on a transient permission problem) and
/// must not fall back to serving unauthenticated (which is the hole #400 closed).
///
/// # It does **not** install
///
/// The caller does, and that separation is load-bearing rather than tidy:
/// [`KEYPAIR`] is process-wide, and a function that both reads a directory and
/// replaces it cannot be called from a test without changing what every other
/// test in the binary verifies against. `cargo test` runs them in parallel
/// against this one static, so the tests below would have silently 401'd every
/// guard test that happened to run beside them.
pub fn load_or_create(dir: &Path) -> Result<Keypair, String> {
    let private = private_key_path(dir);
    let keypair = match std::fs::read(&private) {
        Ok(pkcs8) => {
            Keypair::from_pkcs8(pkcs8).map_err(|e| format!("reading {}: {e}", private.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let fresh = Keypair::generate()?;
            write_keypair(dir, &fresh)?;
            log::info!(
                "api signing keypair created kid={} path={}",
                fresh.kid(),
                private.display()
            );
            fresh
        }
        Err(e) => return Err(format!("reading {}: {e}", private.display())),
    };

    // The public file is a convenience copy of something derived from the
    // private key, so it is repaired rather than trusted: a missing or stale one
    // is rewritten, and a *wrong* one can never be believed, because nothing
    // reads it back. JWKS and the Security tab both answer from the loaded
    // keypair.
    ensure_public_key_file(dir, &keypair);

    Ok(keypair)
}

/// Replace the install's keypair, invalidating every token ever issued.
///
/// This is #405's revocation story in one call: there is no denylist and no
/// per-token bookkeeping, because every previously issued signature simply stops
/// verifying against the new key. It signs the app's own webview out too, which
/// is the point rather than a side effect — `api.ts` recovers by re-minting on
/// the 401.
///
/// Like [`load_or_create`] it **does not install** — the caller does, and only
/// after the write has succeeded, so a keypair that could not be persisted is
/// not one this process starts signing with. The alternative is an app that
/// works until it restarts and then refuses every token it ever issued,
/// including its own.
pub fn regenerate(dir: &Path) -> Result<Keypair, String> {
    let fresh = Keypair::generate()?;
    write_keypair(dir, &fresh)?;
    log::info!("api signing keypair regenerated kid={}", fresh.kid());
    Ok(fresh)
}

/// Write both halves, private first and `0600` before anything can read it.
fn write_keypair(dir: &Path, keypair: &Keypair) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;

    let private = private_key_path(dir);
    // Created `0600` **at open time**, not chmod-ed afterwards. The gap between
    // `write` and `set_permissions` is a real window in which the key is
    // world-readable under a default umask, and it is the window another local
    // account — the one this whole system exists to keep out — would need.
    write_private(&private, &keypair.pkcs8)
        .map_err(|e| format!("writing {}: {e}", private.display()))?;

    write_public(dir, keypair)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Windows has no mode bits to set at open time; the file inherits the data
/// directory's ACL, which is the user's own profile.
#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

/// The public half, base64url without padding — the same spelling the JWK's `x`
/// uses, so what is in the file is what a verifier sees.
fn write_public(dir: &Path, keypair: &Keypair) -> Result<(), String> {
    let path = public_key_path(dir);
    let encoded = public_key_b64(keypair);
    std::fs::write(&path, &encoded).map_err(|e| format!("writing {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 0644: a public key is public, and something else on this machine
        // reading it is the point of writing it at all.
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
    }
    Ok(())
}

/// Rewrite the public file if it is missing or does not match the loaded key.
fn ensure_public_key_file(dir: &Path, keypair: &Keypair) {
    let path = public_key_path(dir);
    let want = public_key_b64(keypair);
    if std::fs::read_to_string(&path).is_ok_and(|have| have == want) {
        return;
    }
    if let Err(e) = write_public(dir, keypair) {
        // Not fatal: nothing reads this file back, so a failure here costs a
        // convenience rather than a capability. JWKS still answers.
        log::warn!("api signing public key: {e}");
    }
}

/// The public key as base64url without padding — JWK `x`, and the file's
/// contents.
pub fn public_key_b64(keypair: &Keypair) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(keypair.public_key())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_keypair_signs_and_names_itself() {
        let keypair = Keypair::generate().expect("generate");
        assert_eq!(keypair.public_key().len(), 32, "raw Ed25519 public key");
        assert_eq!(keypair.kid().len(), KID_BYTES * 2, "hex of 16 digest bytes");
        assert!(keypair.kid().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn two_generated_keypairs_differ() {
        let a = Keypair::generate().expect("a");
        let b = Keypair::generate().expect("b");
        assert_ne!(a.public_key(), b.public_key());
        assert_ne!(a.kid(), b.kid());
    }

    /// The `kid` is a function of the public key alone, so a keypair reloaded
    /// from disk names itself the same thing it did before the restart.
    #[test]
    fn the_kid_survives_a_round_trip_through_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = load_or_create(dir.path()).expect("create");
        let reloaded =
            Keypair::from_pkcs8(std::fs::read(private_key_path(dir.path())).expect("read"))
                .expect("parse");
        assert_eq!(first.kid(), reloaded.kid());
        assert_eq!(first.public_key(), reloaded.public_key());
    }

    /// **The create-if-absent property.** A second run must reuse the key, or
    /// every token the user issued dies on every restart and the whole design is
    /// pointless.
    #[test]
    fn a_second_load_reuses_the_key_rather_than_regenerating() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = load_or_create(dir.path()).expect("first");
        let first_kid = first.kid().to_string();
        let first_public = first.public_key().to_vec();

        let second = load_or_create(dir.path()).expect("second");
        assert_eq!(second.kid(), first_kid);
        assert_eq!(second.public_key(), first_public);
    }

    /// ...and regenerate is the one thing that *does* replace it.
    #[test]
    fn regenerate_replaces_the_key_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let before = load_or_create(dir.path()).expect("create");
        let before_kid = before.kid().to_string();

        let after = regenerate(dir.path()).expect("regenerate");
        assert_ne!(after.kid(), before_kid);

        // And it is the *file* that changed, not just this process's copy.
        let reloaded =
            Keypair::from_pkcs8(std::fs::read(private_key_path(dir.path())).expect("read"))
                .expect("parse");
        assert_eq!(reloaded.kid(), after.kid());
    }

    /// A corrupt private key is a clean, actionable failure — not a silent
    /// regenerate (which would destroy every issued token on a bad read) and not
    /// a fall back to unauthenticated (which is the hole #400 closed).
    #[test]
    fn a_corrupt_private_key_fails_loudly() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path()).expect("mkdir");
        std::fs::write(private_key_path(dir.path()), b"not a key").expect("write");

        // `expect_err` would need `Debug` on the `Ok` side, and [`Keypair`]
        // deliberately has none — see the module header. Matching is the whole
        // cost of that property.
        let err = match load_or_create(dir.path()) {
            Ok(_) => panic!("a corrupt private key must not load"),
            Err(e) => e,
        };
        assert!(
            err.contains("not a valid Ed25519 PKCS#8 private key"),
            "{err:?} should say what is wrong with the file"
        );
        // The file is left exactly as it was, so the user can move it aside or
        // restore a backup rather than discovering it has been overwritten.
        assert_eq!(
            std::fs::read(private_key_path(dir.path())).expect("read"),
            b"not a key"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_private_key_is_private_and_the_public_one_is_not() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        load_or_create(dir.path()).expect("create");

        let private = std::fs::metadata(private_key_path(dir.path()))
            .expect("private")
            .permissions()
            .mode();
        assert_eq!(
            private & 0o777,
            0o600,
            "the signing key must not be group/world readable"
        );

        let public = std::fs::metadata(public_key_path(dir.path()))
            .expect("public")
            .permissions()
            .mode();
        assert_eq!(public & 0o777, 0o644);
    }

    /// **The private key reaches no log line**, on either path that touches it.
    ///
    /// This is the durable-secret half of the module header, and it is worth a
    /// test rather than a rule because the two functions below *do* log — they
    /// name the `kid` and the file path, which is exactly the shape a careless
    /// edit widens into naming the key. #400 pinned the same property for its
    /// token against `proxy.rs`'s access line; this is its successor, and it
    /// matters more, because #400's secret died with the process and this one
    /// does not.
    ///
    /// Both encodings are checked. `{:?}` on the raw bytes and base64 of them
    /// are what a debug print and a "just log the key so I can compare it" would
    /// each produce, and neither is what `Display` on this type would give —
    /// there is no `Display`, and no `Debug` either, which is the first line of
    /// defence and the reason this test can only fail through a deliberate edit.
    #[test]
    fn the_private_key_never_reaches_the_log() {
        use base64::Engine;
        crate::native::writes::testlog::install();

        let dir = tempfile::tempdir().expect("tempdir");
        let created = load_or_create(dir.path()).expect("create");
        let _reloaded = load_or_create(dir.path()).expect("reload");
        let regenerated = regenerate(dir.path()).expect("regenerate");

        for keypair in [&created, &regenerated] {
            let raw = std::fs::read(private_key_path(dir.path())).expect("read");
            for spelling in [
                base64::engine::general_purpose::STANDARD.encode(&keypair.pkcs8),
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&keypair.pkcs8),
                format!("{:?}", keypair.pkcs8),
                base64::engine::general_purpose::STANDARD.encode(&raw),
            ] {
                assert!(
                    crate::native::writes::testlog::matching(&spelling).is_empty(),
                    "the private key must not appear in any log line"
                );
            }
        }

        // ...and the lines that *are* emitted name the key by its `kid`, which
        // is a digest of the **public** half. Asserted so this test cannot pass
        // by nothing having been logged at all.
        assert!(
            !crate::native::writes::testlog::matching(created.kid()).is_empty(),
            "the creation should still be logged, by kid"
        );
    }

    /// The public file is derived, so a damaged one is repaired on the next
    /// load rather than believed.
    #[test]
    fn a_stale_public_key_file_is_rewritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let keypair = load_or_create(dir.path()).expect("create");
        std::fs::write(public_key_path(dir.path()), "stale").expect("write");

        load_or_create(dir.path()).expect("reload");
        assert_eq!(
            std::fs::read_to_string(public_key_path(dir.path())).expect("read"),
            public_key_b64(&keypair)
        );
    }
}
