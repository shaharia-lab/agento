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
//! # What exists so far
//!
//! **The engine, as of #424.** [`config`] is #422's settings model and its
//! SQLite storage; [`registry`], [`server`], [`dispatch`] and [`stream`] are
//! the listener that reads it:
//!
//! | module | what it owns |
//! |---|---|
//! | [`config`] | the three tables, and the mapping onto `ferrox-providers`' own config types |
//! | [`registry`] | the listener's lifecycle — start at boot, reload on a config write, and the *stored* [`Status`](registry::Status) a bind failure leaves behind |
//! | [`server`] | the five routes, the `Host` allowlist, the `llm`-scope auth layer, and the per-surface error dialect |
//! | [`dispatch`] | alias → ordered targets, retry on the same target, and the fallback walk |
//! | [`stream`] | the SSE bytes of both surfaces, and the `anthropic-beta` merge |
//!
//! What is still absent: **usage recording** (#425 — `server`'s handlers log a
//! line where the row will go), the **`/api/gateway/*` control API** (#426 —
//! which is what will read [`registry::status`] and call
//! [`registry::reload`]), and any **UI** (#427/#428). The gateway is disabled
//! by default and costs nothing when off: `start_if_enabled` reads one row and
//! returns.

pub mod config;
pub mod dispatch;
pub mod registry;
pub mod server;
pub mod stream;
pub mod usage;
