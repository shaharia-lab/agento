//! Per-message pricing, ported from the `costAccumulator` half of
//! `internal/claudesessions/types.go`.
//!
//! A session is priced **message by message**, each assistant message resolved
//! against the catalog at its own timestamp with its own model. That is what
//! makes a session which spans a price change — or mixes models — cost
//! correctly, and it is why the scanner is the only place that can compute
//! `cost_by_model`: the stored row carries neither the timing nor the model of
//! the messages behind its total, so no later pass could reconstruct it without
//! re-reading the transcript.
//!
//! A model with no known rate contributes **no cost and no silence**: its
//! tokens accumulate into the unpriced counters so an aggregate can disclose
//! what its total left out. Pricing an unknown model at another model's rate,
//! or at zero, are both worse than saying so.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::native::pricing::{PricedUsage, Resolver};
use crate::native::sessions::summary::{display_model, SessionCost, TokenUsage};

/// Prices a transcript as it is read.
///
/// With no resolver the accumulator is inert, so a fixture needs no pricing
/// setup to exercise everything else the reader does.
#[derive(Default)]
pub struct CostAccumulator<'a> {
    resolver: Option<&'a Resolver>,
    cost: SessionCost,
    priced_messages: i64,
    /// Input+output tokens seen per model with no known rate.
    unknown_models: BTreeMap<String, i64>,
    /// The same money, keyed by the model that spent it. Filled at the one
    /// point that knows both the model and the amount.
    by_model: BTreeMap<String, SessionCost>,
}

impl<'a> CostAccumulator<'a> {
    pub fn new(resolver: Option<&'a Resolver>) -> Self {
        CostAccumulator {
            resolver,
            ..Default::default()
        }
    }

    /// Prices one assistant message.
    ///
    /// A message with no tokens at all is skipped: it is not a pricing gap, and
    /// counting it would inflate `priced_messages` with nothing.
    pub fn add_assistant_message(&mut self, model: &str, usage: &TokenUsage, at: DateTime<Utc>) {
        let Some(resolver) = self.resolver else {
            return;
        };
        if usage.input_tokens
            + usage.output_tokens
            + usage.cache_creation_tokens
            + usage.cache_read_tokens
            == 0
        {
            return;
        }

        // The synthetic placeholder and the embedding models resolve to
        // non-billable catalog rows, so they price at $0.00 without being
        // mistaken for a gap in the catalog — no special case is needed here.
        let Some(resolved) = resolver.resolve(model, at) else {
            if !model.is_empty() {
                *self.unknown_models.entry(model.to_string()).or_insert(0) +=
                    usage.input_tokens + usage.output_tokens;
            }
            return;
        };

        let priced = resolved.rate.price(PricedUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_5m_tokens: usage.cache_creation_5m_tokens,
            cache_creation_1h_tokens: usage.cache_creation_1h_tokens,
            cache_read_tokens: usage.cache_read_tokens,
        });

        let one = SessionCost {
            input_usd: priced.input_cost_usd,
            output_usd: priced.output_cost_usd,
            cache_read_usd: priced.cache_read_cost_usd,
            cache_write_usd: priced.cache_write_cost_usd,
            total_usd: priced.total_cost_usd,
        };
        add_cost(&mut self.cost, &one);
        self.priced_messages += 1;

        let entry = self.by_model.entry(display_model(model)).or_default();
        add_cost(entry, &one);
    }

    /// The session's running total.
    pub fn total(&self) -> SessionCost {
        self.cost.clone()
    }

    /// The session's cost keyed by the model that spent it.
    ///
    /// The values sum to [`CostAccumulator::total`] exactly — this re-keys
    /// money, it never changes an amount. Empty when nothing was priced.
    pub fn cost_by_model(&self) -> BTreeMap<String, SessionCost> {
        self.by_model.clone()
    }

    /// Input+output tokens seen on models with no known rate, so an aggregate
    /// can state what its cost total left out.
    pub fn unknown_pricing_tokens(&self) -> i64 {
        self.unknown_models.values().sum()
    }

    /// The distinct models this session used that carry no known rate.
    ///
    /// Sorted, so the persisted list is deterministic and a rescan that changes
    /// nothing does not rewrite the row. (A `BTreeMap` gives that for free
    /// where Go has to `sort.Strings`.)
    pub fn unpriced_models(&self) -> Vec<String> {
        self.unknown_models.keys().cloned().collect()
    }
}

/// Go's `SessionCost.Add`. Kept here rather than on the type because the type
/// is the sessions list's wire shape and gains nothing from a mutator.
fn add_cost(into: &mut SessionCost, other: &SessionCost) {
    into.input_usd += other.input_usd;
    into.output_usd += other.output_usd;
    into.cache_read_usd += other.cache_read_usd;
    into.cache_write_usd += other.cache_write_usd;
    into.total_usd += other.total_usd;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: i64, output: i64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        }
    }

    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-03-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn without_a_resolver_it_is_inert() {
        let mut acc = CostAccumulator::new(None);
        acc.add_assistant_message("claude-opus-5", &usage(100, 50), at());
        assert_eq!(acc.total().total_usd, 0.0);
        assert!(acc.unpriced_models().is_empty(), "not a pricing gap either");
    }

    #[test]
    fn a_message_with_no_tokens_is_not_a_pricing_gap() {
        let mut acc = CostAccumulator::new(None);
        acc.add_assistant_message("who-knows", &usage(0, 0), at());
        assert_eq!(acc.unknown_pricing_tokens(), 0);
        assert!(acc.unpriced_models().is_empty());
    }
}
