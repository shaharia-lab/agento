//! The Insights page's actionable cards.
//!
//! Mirrors `internal/claudesessions/insight_cards.go`. Each card is a specific
//! fact with a number attached, replacing a 0–100 composite grade: a user
//! reading "58/100 Moderate" learns nothing they can act on, while "cache reads
//! saved you $X against uncached input" names something to do about it.
//!
//! A card is emitted only when its fact is true and material; there is no
//! placeholder card, because a page of empty cards is what the composite score
//! already was.
//!
//! **One card prices a counterfactual.** Every other cost figure in analytics
//! is the value the scanner stored. Cache savings is what cache reads *would*
//! have cost at input rates — tokens that were never billed that way — so it
//! cannot come from a stored total and is marked `estimated` for exactly that
//! reason. It still resolves per session at that session's own model and
//! instant rather than picking one rate for the whole window.

use std::collections::BTreeMap;

use serde::Serialize;

use super::report::{round_to, ModelCostStat};
use crate::native::pricing::Resolver;
use crate::native::sessions::summary::SessionSummary;

/// The read share below which a model is worth a card.
///
/// Not zero, and not a near-zero epsilon: on the reference corpus the
/// non-caching backend still shows a few per cent of cache reads, because a
/// handful of its sessions routed elsewhere, while every caching model sits
/// above 99%. Any threshold in the middle separates those two populations.
const LOW_CACHE_SHARE: f64 = 0.5;

/// How many top sessions the expensive-sessions card characterizes. Small
/// enough that "these specific runs" is actionable.
const EXPENSIVE_SESSION_SAMPLE: usize = 5;

/// Keeps trivia off the page: a model that spent a few cents is not worth a
/// card about its caching behaviour.
const MIN_CARD_COST_USD: f64 = 1.0;

/// One fact. Fields not relevant to a `kind` are left zero and omitted; the
/// frontend reads only the ones its phrasing for that kind uses.
#[derive(Debug, Serialize)]
pub struct InsightCard {
    pub kind: &'static str,
    /// The card's money figure: savings, spend, or delegated cost.
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub amount_usd: f64,
    /// Its share figure, 0–100.
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub percent: f64,
    /// A session, model or token count depending on `kind`.
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub count: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// The token figure behind `amount_usd`, when there is one.
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub tokens: i64,
    /// Mean active duration of the sessions a card covers — idle gaps above the
    /// threshold excluded, delegated work included.
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub avg_duration_ms: i64,
    /// The figure `amount_usd` should be read against. A saving of $102k means
    /// nothing until you know the bill was $20k.
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub comparison_usd: f64,
    /// Marks a figure derived from list rates rather than read from a stored
    /// total, so the UI can say "about" and mean it.
    #[serde(skip_serializing_if = "is_false")]
    pub estimated: bool,
}

impl Default for InsightCard {
    fn default() -> Self {
        InsightCard {
            kind: "",
            amount_usd: 0.0,
            percent: 0.0,
            count: 0,
            model: String::new(),
            tokens: 0,
            avg_duration_ms: 0,
            comparison_usd: 0.0,
            estimated: false,
        }
    }
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Derive the cards from the filtered window, in the order Go appends them.
pub fn build_insight_cards(
    sessions: &[&SessionSummary],
    cost_by_model: &[ModelCostStat],
    resolver: Option<&Resolver>,
) -> Vec<InsightCard> {
    let mut cards = Vec::with_capacity(4);
    if let Some(card) = cache_savings_card(sessions, resolver) {
        cards.push(card);
    }
    if let Some(card) = low_cache_card(sessions, cost_by_model) {
        cards.push(card);
    }
    if let Some(card) = delegation_card(sessions) {
        cards.push(card);
    }
    if let Some(card) = expensive_sessions_card(sessions) {
        cards.push(card);
    }
    cards
}

/// What cache reads saved against paying the input rate for the same tokens.
///
/// A missing resolver drops the card rather than pricing at zero, matching Go's
/// `defaultPricingResolver() == nil` guard.
fn cache_savings_card(
    sessions: &[&SessionSummary],
    resolver: Option<&Resolver>,
) -> Option<InsightCard> {
    let resolver = resolver?;

    let (mut savings, mut tokens, mut actual) = (0.0, 0i64, 0.0);
    for s in sessions {
        actual += s.total_cost().total_usd;
        for (model, u) in s.total_usage_by_model() {
            if u.cache_read_tokens == 0 {
                continue;
            }
            let Some(resolved) = resolver.resolve(&model, s.last_activity.instant()) else {
                continue;
            };
            if !resolved.rate.billable {
                continue;
            }
            let per_token =
                (resolved.rate.input_per_mtok - resolved.rate.cache_read_per_mtok) / 1_000_000.0;
            if per_token <= 0.0 {
                continue; // a provider whose cached reads cost no less than input
            }
            savings += u.cache_read_tokens as f64 * per_token;
            tokens += u.cache_read_tokens;
        }
    }

    if savings < MIN_CARD_COST_USD {
        return None;
    }
    Some(InsightCard {
        kind: "cache_savings",
        amount_usd: savings,
        comparison_usd: actual,
        tokens,
        estimated: true,
        ..Default::default()
    })
}

/// Names the costliest model served almost nothing from cache, so its context
/// is re-billed as fresh input on every turn.
///
/// This is the fact behind the token/cost inversion on the model charts, stated
/// directly instead of left for a reader to infer from two charts disagreeing.
fn low_cache_card(
    sessions: &[&SessionSummary],
    cost_by_model: &[ModelCostStat],
) -> Option<InsightCard> {
    let mut cache_reads: BTreeMap<String, i64> = BTreeMap::new();
    let mut input_tokens: BTreeMap<String, i64> = BTreeMap::new();
    for s in sessions {
        for (model, u) in s.total_usage_by_model() {
            *cache_reads.entry(model.clone()).or_default() += u.cache_read_tokens;
            *input_tokens.entry(model).or_default() += u.input_tokens;
        }
    }

    // cost_by_model is already ordered by spend, so the first match is the one
    // worth naming.
    for m in cost_by_model {
        if m.cost.total_usd < MIN_CARD_COST_USD {
            break;
        }
        let reads = cache_reads.get(&m.model).copied().unwrap_or_default();
        let input = input_tokens.get(&m.model).copied().unwrap_or_default();
        let input_side = reads + input;
        if input_side == 0 {
            continue;
        }
        let card_share = reads as f64 / input_side as f64;
        if card_share < LOW_CACHE_SHARE {
            return Some(InsightCard {
                kind: "model_low_cache",
                model: m.model.clone(),
                // amount_usd is what the model spent; percent is how much of
                // its input side came from cache, which is the number the card
                // is about. Its share of total spend is on the cost chart.
                amount_usd: m.cost.total_usd,
                percent: round_to(card_share * 100.0, 10.0),
                tokens: input,
                ..Default::default()
            });
        }
    }
    None
}

/// How much of the window's spend was delegated to sub-agents, and which model
/// took most of it — "is delegation routing work to cheaper models", answered
/// in dollars rather than left to be read off a chart.
fn delegation_card(sessions: &[&SessionSummary]) -> Option<InsightCard> {
    let (mut delegated, mut total) = (0.0, 0.0);
    let mut by_model: BTreeMap<String, f64> = BTreeMap::new();
    let mut delegating_sessions = 0i64;

    for s in sessions {
        total += s.total_cost().total_usd;
        if s.subagent_count == 0 {
            continue;
        }
        delegating_sessions += 1;
        delegated += s.subagent_cost.total_usd;
        for (model, c) in &s.subagent_cost_by_model {
            *by_model.entry(model.clone()).or_default() += c.total_usd;
        }
    }

    if delegated < MIN_CARD_COST_USD || total <= 0.0 {
        return None;
    }

    let mut top = ("", 0.0);
    for (model, cost) in &by_model {
        if *cost > top.1 {
            top = (model, *cost);
        }
    }

    Some(InsightCard {
        kind: "delegation_mix",
        amount_usd: delegated,
        percent: round_to(delegated / total * 100.0, 10.0),
        count: delegating_sessions,
        model: top.0.to_string(),
        ..Default::default()
    })
}

/// What the priciest handful of sessions cost together, what share of the
/// window that is, and how long they ran.
///
/// The concentrated share is the actionable part — if five sessions are a third
/// of the bill, they are where a change of habit pays.
fn expensive_sessions_card(sessions: &[&SessionSummary]) -> Option<InsightCard> {
    if sessions.len() < EXPENSIVE_SESSION_SAMPLE {
        return None;
    }

    let mut ranked: Vec<&&SessionSummary> = sessions.iter().collect();
    ranked.sort_by(|a, b| {
        b.total_cost()
            .total_usd
            .partial_cmp(&a.total_cost().total_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total: f64 = sessions.iter().map(|s| s.total_cost().total_usd).sum();

    let (mut top, mut duration_ms) = (0.0, 0i64);
    for s in ranked.iter().take(EXPENSIVE_SESSION_SAMPLE) {
        top += s.total_cost().total_usd;
        // Active time, not the start/last span: expensive sessions are exactly
        // the long-lived ones people resume, and a span including the idle week
        // between sittings would make "ran Xh on average" meaningless.
        duration_ms += s.total_active_duration_ms();
    }

    if top < MIN_CARD_COST_USD || total <= 0.0 {
        return None;
    }
    Some(InsightCard {
        kind: "expensive_sessions",
        amount_usd: top,
        percent: round_to(top / total * 100.0, 10.0),
        count: EXPENSIVE_SESSION_SAMPLE as i64,
        avg_duration_ms: duration_ms / EXPENSIVE_SESSION_SAMPLE as i64,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::gojson;

    #[test]
    fn omitempty_drops_every_field_a_kind_does_not_use() {
        let card = InsightCard {
            kind: "delegation_mix",
            amount_usd: 12.5,
            percent: 9.6,
            count: 236,
            model: "claude-opus-4-8".into(),
            ..Default::default()
        };
        let encoded = String::from_utf8(gojson::to_vec(&card).expect("encode")).expect("utf-8");
        assert_eq!(
            encoded.trim_end(),
            r#"{"kind":"delegation_mix","amount_usd":12.5,"percent":9.6,"count":236,"model":"claude-opus-4-8"}"#
        );
    }

    #[test]
    fn an_estimated_card_says_so_and_a_plain_one_omits_the_flag() {
        let estimated = InsightCard {
            kind: "cache_savings",
            amount_usd: 1.0,
            estimated: true,
            ..Default::default()
        };
        let encoded =
            String::from_utf8(gojson::to_vec(&estimated).expect("encode")).expect("utf-8");
        assert!(encoded.contains(r#""estimated":true"#), "{encoded}");

        let plain = InsightCard {
            kind: "cache_savings",
            amount_usd: 1.0,
            ..Default::default()
        };
        let encoded = String::from_utf8(gojson::to_vec(&plain).expect("encode")).expect("utf-8");
        assert!(!encoded.contains("estimated"), "{encoded}");
    }
}
