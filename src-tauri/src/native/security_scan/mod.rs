//! The Credentials Checker (#597): its detection core (#599) and its store.
//!
//! The detection half is two parts, and **both are pure** — no I/O, no
//! database, no state beyond the compiled rule table:
//!
//! - [`rules`] is the vendored rule table: an id, a pattern, a provider label
//!   and a fixed confidence tier per rule, plus [`rules::CURRENT_RULESET_VERSION`].
//! - [`scan`] runs every rule over a piece of text and answers the matches as
//!   byte ranges.
//!
//! [`store`] is the persistence over migration 41's tables: what needs
//! (re)scanning, the whitelist-aware write, and the whitelist itself (#602).
//!
//! The background worker and the `/api` surface are later issues (#603, #604)
//! and build on the rule ids and tiers defined here. This module is
//! deliberately not in `native::ENDPOINTS` yet.
//!
//! **A [`scan::Finding`] never holds the matched text** — only its byte range
//! into the input. The store derives its masked snippet and hash from that
//! range while the input is still in hand; nothing this module exposes can
//! carry a raw secret any further than that.

pub mod rules;
pub mod scan;
pub mod store;
