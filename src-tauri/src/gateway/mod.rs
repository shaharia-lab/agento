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
//! **[`config`] only.** This is #422: the settings model and its SQLite
//! storage, and nothing else. There is no listener, no router, no dispatch and
//! no usage recording — those are #424 and #425, and nothing in the app reads
//! these tables yet. The module is wired into the crate so the schema and the
//! `ferrox-providers` dependency land, compile and are tested ahead of the
//! engine that needs them, rather than arriving inside it.

pub mod config;
