//! The embedded LLM gateway (epic #421).
//!
//! # Why this is beside `native/` rather than inside it
//!
//! `native/` is the `/api` seam: a registry of endpoints, a wire format pinned
//! byte for byte against goldens in `parity/`, and a guard that decides what
//! credential each request needs. The gateway is **not part of that surface**.
//! It is a second listener, on its own user-configured port, speaking two
//! third-party wire formats (OpenAI's and Anthropic's) that are somebody else's
//! specification rather than ours to pin. None of the parity machinery applies,
//! and putting it under `native/` would imply it does.
//!
//! The two halves meet in exactly one place, and deliberately only there: the
//! gateway authenticates with the same per-install Ed25519 keypair `/api` uses,
//! through [`crate::native::security::token::verify_against`] — the pure
//! function #405 built for a second caller — requiring the disjoint
//! [`Scope::Llm`](crate::native::security::token::Scope::Llm) added by #423.
//!
//! # What is here
//!
//! [`config`] is #422's settings model and its SQLite storage; [`registry`],
//! [`server`], [`dispatch`] and [`stream`] are #424's listener that reads it;
//! [`usage`] is #425's row per served request, plus #428's retention prune.
//!
//! | module | what it owns |
//! |---|---|
//! | [`config`] | the three tables, and the mapping onto `ferrox-providers`' own config types |
//! | [`registry`] | the listener's lifecycle — start at boot, reload on a config write, and the *stored* [`Status`](registry::Status) a bind failure leaves behind |
//! | [`server`] | the five routes, the `Host` allowlist, the `llm`-scope auth layer, and the per-surface error dialect |
//! | [`dispatch`] | alias → ordered targets, retry on the same target, and the fallback walk |
//! | [`stream`] | the SSE bytes of both surfaces, and the `anthropic-beta` merge |
//! | [`usage`] | one row per served request, the cost resolved at write time, and the retention prune |
//!
//! **Nothing of the epic is absent any more.** The control plane is not here but
//! it does exist: `/api/gateway/*` is twelve routes in
//! [`crate::native::gateway_api`] (#426), under `native/` because it *is* the
//! `/api` seam where this listener is not, and the **LLM Gateway** section in
//! `src/views/gateway/` (#427) with its Usage dashboard (#428) is what drives
//! them. See `docs/development.md` for that split and the `ferrox-providers`
//! dependency policy, and `docs/user-guide.md` for the user-facing flow.
//!
//! The gateway is disabled by default and costs nothing when off:
//! `start_if_enabled` reads one row and returns.

pub mod config;
pub mod dispatch;
pub mod registry;
pub mod server;
pub mod stream;
pub mod usage;
