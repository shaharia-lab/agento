//! The Credentials Checker's detection core (#597, #599).
//!
//! Two parts, and **both are pure** — no I/O, no database, no state beyond the
//! compiled rule table:
//!
//! - [`rules`] is the vendored rule table: an id, a pattern, a provider label
//!   and a fixed confidence tier per rule, plus [`rules::CURRENT_RULESET_VERSION`].
//! - [`scan`] runs every rule over a piece of text and answers the matches as
//!   byte ranges.
//!
//! Persistence, the background worker and the `/api` surface are later issues
//! (#602–#604) and build on the rule ids and tiers defined here. This module is
//! deliberately not in `native::ENDPOINTS` yet.
//!
//! **A [`scan::Finding`] never holds the matched text** — only its byte range
//! into the input. The store (#602) derives its masked snippet from that range
//! while the input is still in hand; nothing this module exposes can carry a
//! raw secret any further than that.

pub mod rules;
pub mod scan;
