//! What a remote credential check *concluded*, beyond the fact that it failed
//! (#521).
//!
//! # Why a failure needs a kind at all
//!
//! `POST /api/integrations/{id}/auth/validate` reports a failure as one 400
//! whatever went wrong, and that is the wire contract — it does not move. But
//! three behaviours compose into a state that lies, and telling the two failure
//! classes apart is what fixes it:
//!
//! - `PUT /api/integrations/{id}` overwrites `credentials` and deliberately
//!   **preserves a non-empty `auth`** in SQL, so the stored token is never read
//!   into this process;
//! - `authenticated` is computed from `auth` alone, and
//!   [`super::registry::HostingRow::is_startable`] is `enabled && authenticated`
//!   — neither consults `credentials`;
//! - the `PUT` reloads the MCP server *before* any check can run, because the
//!   check is a separate route reading what was stored.
//!
//! So replacing a working token with a **rejected** one left the badge reading
//! `Connected` about the previous authorisation while the hosted server answered
//! `tools/call` on the credential the provider had just refused. The fix is to
//! clear `auth` on that path — but only on that path: a **flat** clear would
//! disconnect a working integration every time the provider was briefly
//! unreachable, which is the same dishonesty pointing the other way.
//!
//! # The trichotomy, minus one
//!
//! `native/gateway_api/catalog.rs` already answers this exact question for the
//! gateway's provider check, as `unauthorized` / `unreachable` / `unexpected`.
//! Only the first of those is a state change here, and the other two are the
//! same answer — *change nothing* — so this is the same distinction with the
//! two harmless arms merged.
//!
//! **[`CheckKind::Unreachable`] is the safe default and every unrecognised
//! outcome takes it.** Misclassifying a refusal as unreachable leaves the
//! pre-existing bug standing for that one shape; misclassifying a transport
//! failure as a refusal disconnects a working integration. Only the second is
//! a new failure, so the doubt goes the other way.
//!
//! Grouping 401 **with** 403 is `catalog.rs`'s reasoning verbatim: some
//! providers report an exhausted quota with 403, and both are the provider
//! answering about the credential rather than failing to answer at all.

/// Which of the two classes a failed credential check falls into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    /// The provider answered and refused the credential — an HTTP 401 or 403,
    /// or a refusal carried in a 200 envelope (Slack's `ok:false`, Telegram's).
    ///
    /// This is the only kind that changes stored state.
    Rejected,
    /// Everything else: a transport failure, a timeout, a 404 on the base URL,
    /// a 5xx, a response that would not parse, a rate limit, or a credential
    /// that never reached the network because its own site URL was refused.
    ///
    /// Nothing about the authorisation on file is known, so nothing moves.
    Unreachable,
}

/// A failed remote check: what to say, and what class it was.
///
/// The message is the one that was already going on the wire — every
/// per-validator sentence is a parity fixture and none of them move here. Only
/// the kind is new.
pub struct CheckFailure {
    pub kind: CheckKind,
    pub message: String,
}

impl CheckFailure {
    /// The provider answered and refused the credential.
    pub fn rejected(message: impl Into<String>) -> Self {
        Self {
            kind: CheckKind::Rejected,
            message: message.into(),
        }
    }

    /// Everything else — see [`CheckKind::Unreachable`].
    pub fn unreachable(message: impl Into<String>) -> Self {
        Self {
            kind: CheckKind::Unreachable,
            message: message.into(),
        }
    }

    /// Whether an HTTP status is the provider refusing the credential.
    ///
    /// The one place the 401/403 pair is spelled, so the four validators that
    /// classify on a status cannot drift apart — and so widening it is one
    /// edit rather than four.
    pub fn from_status(status: u16, message: impl Into<String>) -> Self {
        if status == 401 || status == 403 {
            Self::rejected(message)
        } else {
            Self::unreachable(message)
        }
    }
}
