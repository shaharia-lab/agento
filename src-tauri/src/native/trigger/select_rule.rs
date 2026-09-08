//! Slack's rule selection: most specific first, with a per-channel off switch.
//!
//! **A second selection function, not a generalization of Telegram's.**
//! `dispatcher::find_matching_rule` keeps first-enabled-in-store-order because
//! changing it would change what an existing install already does; Slack has no
//! stored behaviour to preserve and needs a rule Telegram does not have (#565).
//!
//! The two are also asked different questions. Telegram matches a *message* —
//! prefix, keywords and chat id together, via `match_rule`. This picks the rule
//! that owns a *channel*, before anything has been said in it; whether the
//! message then matters is the caller's business (#568).

use super::dispatcher::Rule;

/// The rule that answers for `channel_id`, or `None` for silence.
///
/// `rules` is one integration's rules in `created_at` order, oldest first, and
/// **including the disabled ones** — which is exactly what
/// `dispatcher::load_rules` returns, and the reason it returns them.
///
/// In order of preference:
///
/// 1. A rule whose `filter_chat_ids` names the channel beats a rule with none,
///    **whether that specific rule is enabled or disabled**.
/// 2. Otherwise the enabled default: a rule with an empty `filter_chat_ids`.
/// 3. Otherwise `None`.
///
/// # A disabled channel-specific rule means silence
///
/// This is the clause a reader will assume is a bug, so it is stated here: rule
/// 1 does not skip disabled rules, so a disabled channel-specific rule stops the
/// channel outright rather than falling through to the workspace-wide default.
///
/// That is the whole point of the ordering. A default rule makes **every**
/// channel the app is in respond, so a channel needs an off switch, and
/// "disable that channel's rule" is the gesture that reads naturally in the
/// rules list — where the alternative would be a rule that exists only to say
/// no. Skipping disabled rules here would make that gesture do the opposite of
/// what it looks like: the channel would keep answering, on the default.
///
/// # Ties
///
/// Two rules naming one channel, or two defaults, resolve to the earlier
/// `created_at` — which is simply the first of them in `rules`.
pub fn select_rule_for_channel<'a>(rules: &'a [Rule], channel_id: &str) -> Option<&'a Rule> {
    if let Some(specific) = rules
        .iter()
        .find(|rule| rule.filters.chat_ids.iter().any(|id| id == channel_id))
    {
        // Deliberately not `rules.iter().find(...).filter(|r| r.enabled)`: the
        // search must not walk *past* a disabled specific rule to a later one.
        return specific.enabled.then_some(specific);
    }
    rules
        .iter()
        .find(|rule| rule.enabled && rule.filters.chat_ids.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Vectors {
        cases: Vec<Case>,
    }

    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        #[allow(dead_code)]
        note: String,
        channel_id: String,
        /// Oldest first, as `dispatcher::load_rules` returns them.
        rules: Vec<VectorRule>,
        /// The `id` of the rule that answers, or `null` for silence.
        selected: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct VectorRule {
        id: String,
        enabled: bool,
        filter_chat_ids: Option<Vec<String>>,
    }

    fn rule(v: &VectorRule) -> Rule {
        Rule {
            id: v.id.clone(),
            name: v.id.clone(),
            agent_slug: "a".to_string(),
            enabled: v.enabled,
            filters: super::super::match_rule::RuleFilters {
                chat_ids: v.filter_chat_ids.clone().unwrap_or_default(),
                ..Default::default()
            },
            settings: Default::default(),
            timeout_minutes: 0,
        }
    }

    #[test]
    fn every_case_selects_what_the_rule_says() {
        let raw = include_str!("../../../../parity/trigger_select_vectors.json");
        let vectors: Vectors = serde_json::from_str(raw).expect("vectors decode");
        assert!(!vectors.cases.is_empty(), "the vector file is empty");

        for case in &vectors.cases {
            let rules: Vec<Rule> = case.rules.iter().map(rule).collect();
            let got = select_rule_for_channel(&rules, &case.channel_id);
            assert_eq!(
                got.map(|r| r.id.as_str()),
                case.selected.as_deref(),
                "case {:?}",
                case.name
            );
        }
    }

    /// The clause the doc comment warns about, spelled out on its own so a
    /// change to it fails a test named for it rather than a numbered vector.
    #[test]
    fn a_disabled_channel_specific_rule_is_silence_not_a_fall_through() {
        let rules = vec![
            rule(&VectorRule {
                id: "default".to_string(),
                enabled: true,
                filter_chat_ids: None,
            }),
            rule(&VectorRule {
                id: "muted".to_string(),
                enabled: false,
                filter_chat_ids: Some(vec!["C-muted".to_string()]),
            }),
        ];

        assert_eq!(
            select_rule_for_channel(&rules, "C-muted").map(|r| r.id.as_str()),
            None,
            "the channel's own rule is off, so the channel is off — the enabled \
             default must not answer for it"
        );
        assert_eq!(
            select_rule_for_channel(&rules, "C-other").map(|r| r.id.as_str()),
            Some("default"),
            "and every other channel still gets the default"
        );
    }
}
