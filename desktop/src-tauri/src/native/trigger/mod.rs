//! The Telegram trigger path: matching an inbound message to a rule, and
//! running the agent it names (#319).
//!
//! Mirrors `internal/trigger`.

pub mod match_rule;
pub mod receiver;
