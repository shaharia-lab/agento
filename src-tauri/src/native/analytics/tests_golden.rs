//! The cross-language golden: the same fixture corpus, built in Rust, encoded
//! and compared against the bytes Go wrote.
//!
//! The fixture mirrors `desktop/parity/claude_analytics_parity_test.go` session
//! for session and field for field — read the two side by side; the Go file
//! carries the reasoning for each row. Between them they cover the DST hour
//! that does not exist, a session crossing local midnight, a session with no
//! duration, delegated work under a different model, an unpriced model, a
//! `<synthetic>` session and a session outside the window.
//!
//! **No pricing resolver is passed**, matching the Go side: the cache-savings
//! card is the one figure that prices a counterfactual, and wiring a catalog
//! into the Go test would seed the whole built-in list and make the golden move
//! whenever `catalog.json` does. The live diff covers that card against real
//! rates.

use std::collections::BTreeMap;

use crate::native::gojson;
use crate::native::gotime::GoTime;
use crate::native::sessions::summary::{SessionCost, SessionSummary, TokenUsage};

use super::params::AnalyticsParams;
use super::report::aggregate;

fn at(text: &str) -> GoTime {
    GoTime::parse(text).expect("fixture timestamp")
}

fn cost(input: f64, output: f64, read: f64, write: f64, total: f64) -> SessionCost {
    SessionCost {
        input_usd: input,
        output_usd: output,
        cache_read_usd: read,
        cache_write_usd: write,
        total_usd: total,
    }
}

fn usage(input: i64, output: i64, read: i64, write: i64) -> TokenUsage {
    TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_tokens: write,
        cache_creation_5m_tokens: write,
        cache_creation_1h_tokens: 0,
        cache_read_tokens: read,
    }
}

/// A session with everything analytics does not read left at its zero value.
struct Row {
    id: &'static str,
    project: &'static str,
    /// Already resolved: Go's ranking reads `ResolveDisplayTitle()`, which is
    /// the rename when there is one and the first prompt otherwise.
    title: &'static str,
    model: &'static str,
    start: &'static str,
    last: &'static str,
    active_ms: i64,
    subagent_active_ms: i64,
    usage: TokenUsage,
    subagent_count: i64,
    subagent_usage: TokenUsage,
    subagent_usage_by_model: Vec<(&'static str, TokenUsage)>,
    cost: SessionCost,
    subagent_cost: SessionCost,
    cost_by_model: Vec<(&'static str, SessionCost)>,
    subagent_cost_by_model: Vec<(&'static str, SessionCost)>,
    unpriced_models: Vec<&'static str>,
    unpriced_tokens: i64,
}

impl Row {
    fn build(&self) -> SessionSummary {
        let map = |pairs: &[(&str, TokenUsage)]| -> BTreeMap<String, TokenUsage> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect()
        };
        let costs = |pairs: &[(&str, SessionCost)]| -> BTreeMap<String, SessionCost> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect()
        };
        SessionSummary {
            session_id: self.id.to_string(),
            project_path: self.project.to_string(),
            config_dir: String::new(),
            preview: self.title.to_string(),
            custom_title: String::new(),
            is_favorite: false,
            native_title: String::new(),
            ai_title: String::new(),
            display_title: self.title.to_string(),
            start_time: at(self.start),
            last_activity: at(self.last),
            active_duration_ms: self.active_ms,
            subagent_active_duration_ms: self.subagent_active_ms,
            message_count: 0,
            event_count: 0,
            usage: self.usage.clone(),
            git_branch: String::new(),
            model: self.model.to_string(),
            cwd: String::new(),
            subagent_count: self.subagent_count,
            subagent_usage: self.subagent_usage.clone(),
            subagent_usage_by_model: map(&self.subagent_usage_by_model),
            agent_name: String::new(),
            permission_mode: String::new(),
            mode: String::new(),
            relocated_cwd: String::new(),
            worktree_name: String::new(),
            worktree_branch: String::new(),
            original_branch: String::new(),
            compaction_count: 0,
            dropped_tokens: 0,
            prs: Vec::new(),
            cost: self.cost.clone(),
            subagent_cost: self.subagent_cost.clone(),
            cost_by_model: costs(&self.cost_by_model),
            subagent_cost_by_model: costs(&self.subagent_cost_by_model),
            unpriced_models: self
                .unpriced_models
                .iter()
                .map(|m| (*m).to_string())
                .collect(),
            unpriced_tokens: self.unpriced_tokens,
            // Never set outside a search response, and the analytics golden is
            // not one — see `SessionSummary::match_snippet`.
            match_snippet: String::new(),
        }
    }
}

fn fixture() -> Vec<SessionSummary> {
    let blank = SessionCost::default();
    let rows = [
        Row {
            id: "s1",
            project: "/work/alpha",
            title: "alpha one",
            model: "claude-opus-5",
            start: "2026-03-20T08:00:00Z",
            last: "2026-03-20T11:30:00Z",
            active_ms: 600_000,
            subagent_active_ms: 180_000,
            usage: usage(1000, 200, 50000, 3000),
            subagent_count: 2,
            subagent_usage: usage(500, 100, 0, 0),
            subagent_usage_by_model: vec![("claude-haiku-4-5-20251001", usage(500, 100, 0, 0))],
            cost: cost(2.0, 3.0, 1.0, 4.0, 10.0),
            subagent_cost: cost(0.5, 1.25, 0.25, 1.0, 3.0),
            cost_by_model: vec![("claude-opus-5", cost(2.0, 3.0, 1.0, 4.0, 10.0))],
            subagent_cost_by_model: vec![(
                "claude-haiku-4-5-20251001",
                cost(0.5, 1.25, 0.25, 1.0, 3.0),
            )],
            unpriced_models: vec![],
            unpriced_tokens: 0,
        },
        Row {
            id: "s2",
            project: "/work/alpha",
            // The rename wins over the preview, as ResolveDisplayTitle has it.
            title: "renamed by hand",
            model: "k3",
            start: "2026-03-25T22:00:00Z",
            last: "2026-03-26T01:00:00Z",
            active_ms: 900_000,
            subagent_active_ms: 0,
            usage: usage(200000, 50000, 5000, 0),
            subagent_count: 0,
            subagent_usage: usage(0, 0, 0, 0),
            subagent_usage_by_model: vec![],
            cost: cost(8.0, 6.0, 0.1, 0.0, 14.1),
            subagent_cost: blank.clone(),
            cost_by_model: vec![("k3", cost(8.0, 6.0, 0.1, 0.0, 14.1))],
            subagent_cost_by_model: vec![],
            unpriced_models: vec![],
            unpriced_tokens: 0,
        },
        Row {
            id: "s3",
            project: "/work/beta",
            title: "beta one",
            model: "claude-opus-5",
            start: "2026-03-29T00:30:00Z",
            last: "2026-03-29T02:30:00Z",
            active_ms: 300_000,
            subagent_active_ms: 0,
            usage: usage(3000, 700, 80000, 4000),
            subagent_count: 0,
            subagent_usage: usage(0, 0, 0, 0),
            subagent_usage_by_model: vec![],
            cost: cost(1.25, 2.0, 1.5, 2.5, 7.25),
            subagent_cost: blank.clone(),
            cost_by_model: vec![("claude-opus-5", cost(1.25, 2.0, 1.5, 2.5, 7.25))],
            subagent_cost_by_model: vec![],
            unpriced_models: vec![],
            unpriced_tokens: 0,
        },
        Row {
            id: "s4",
            project: "/work/gamma",
            title: "gamma one",
            model: "glm-5.2",
            start: "2026-04-01T09:00:00Z",
            last: "2026-04-01T09:45:00Z",
            active_ms: 150_000,
            subagent_active_ms: 0,
            usage: usage(9000, 1200, 0, 0),
            subagent_count: 0,
            subagent_usage: usage(0, 0, 0, 0),
            subagent_usage_by_model: vec![],
            cost: blank.clone(),
            subagent_cost: blank.clone(),
            cost_by_model: vec![],
            subagent_cost_by_model: vec![],
            unpriced_models: vec!["glm-5.2"],
            unpriced_tokens: 10200,
        },
        Row {
            id: "s5",
            project: "/work/beta",
            title: "beta two",
            model: "<synthetic>",
            start: "2026-04-02T12:00:00Z",
            last: "2026-04-02T12:05:00Z",
            active_ms: 30_000,
            subagent_active_ms: 0,
            usage: usage(50, 10, 0, 0),
            subagent_count: 0,
            subagent_usage: usage(0, 0, 0, 0),
            subagent_usage_by_model: vec![],
            cost: blank.clone(),
            subagent_cost: blank.clone(),
            cost_by_model: vec![],
            subagent_cost_by_model: vec![],
            unpriced_models: vec![],
            unpriced_tokens: 0,
        },
        Row {
            id: "s6",
            project: "/work/alpha",
            title: "alpha three",
            model: "claude-opus-5",
            start: "2026-04-03T10:00:00Z",
            last: "2026-04-03T10:00:00Z",
            active_ms: 450_000,
            subagent_active_ms: 0,
            usage: usage(4000, 900, 20000, 1000),
            subagent_count: 0,
            subagent_usage: usage(0, 0, 0, 0),
            subagent_usage_by_model: vec![],
            cost: cost(4.0, 9.0, 2.0, 7.0, 22.0),
            subagent_cost: blank.clone(),
            cost_by_model: vec![("claude-opus-5", cost(4.0, 9.0, 2.0, 7.0, 22.0))],
            subagent_cost_by_model: vec![],
            unpriced_models: vec![],
            unpriced_tokens: 0,
        },
        Row {
            id: "s8",
            project: "/work/delta",
            title: "delta one",
            model: "k3",
            start: "2026-03-31T16:00:00Z",
            last: "2026-03-31T17:15:00Z",
            active_ms: 240_000,
            subagent_active_ms: 0,
            usage: usage(6000, 1500, 0, 0),
            subagent_count: 0,
            subagent_usage: usage(0, 0, 0, 0),
            subagent_usage_by_model: vec![],
            cost: cost(2.0, 3.0, 0.0, 0.5, 5.5),
            subagent_cost: blank.clone(),
            cost_by_model: vec![("k3", cost(2.0, 3.0, 0.0, 0.5, 5.5))],
            subagent_cost_by_model: vec![],
            unpriced_models: vec![],
            unpriced_tokens: 0,
        },
        Row {
            id: "s7",
            project: "/work/epsilon",
            title: "epsilon one",
            model: "claude-opus-5",
            start: "2026-01-05T10:00:00Z",
            last: "2026-01-05T12:00:00Z",
            active_ms: 111_000,
            subagent_active_ms: 0,
            usage: usage(700, 100, 0, 0),
            subagent_count: 0,
            subagent_usage: usage(0, 0, 0, 0),
            subagent_usage_by_model: vec![],
            cost: cost(1.0, 1.0, 0.0, 0.0, 2.0),
            subagent_cost: blank,
            cost_by_model: vec![],
            subagent_cost_by_model: vec![],
            unpriced_models: vec![],
            unpriced_tokens: 0,
        },
    ];
    rows.iter().map(Row::build).collect()
}

/// The query the Go side hand-builds its params from. Parsed here rather than
/// hand-built, so the parser is checked against Go's interpretation too: a bare
/// date is a *local* day, and a bare range end is that day's final second.
const GOLDEN_QUERY: &str = "from=2026-03-20&to=2026-04-03&tz=Europe/Berlin";

#[test]
fn the_whole_report_matches_gos_bytes() {
    let p = AnalyticsParams::parse(GOLDEN_QUERY).expect("parse params");
    let report = aggregate(&fixture(), &p, None);
    let got = String::from_utf8(gojson::to_vec(&report).expect("encode")).expect("utf-8");

    let want = include_str!("../../../../parity/claude_analytics_golden.json");
    assert_eq!(
        got, want,
        "analytics JSON drifted from the Go golden \
         (regenerate with `go test ./desktop/parity/ -update-golden`)"
    );
}

/// The empty window is the one place Go's zero-valued summary shows: its nil
/// `unknown_pricing_models` marshals as `null` where every other response
/// carries `[]`.
#[test]
fn an_empty_window_returns_gos_zero_summary_with_a_null_model_list() {
    let p = AnalyticsParams::parse("from=1990-01-01&to=1990-12-31").expect("parse params");
    let report = aggregate(&fixture(), &p, None);
    let got = String::from_utf8(gojson::to_vec(&report).expect("encode")).expect("utf-8");

    assert!(got.contains(r#""unknown_pricing_models":null"#), "{got}");
    assert!(got.contains(r#""time_series":[]"#), "{got}");
    assert!(got.contains(r#""hourly_activity":[{"hour":0"#), "{got}");
    // The picker keeps every project, or a filter into an empty window cannot
    // be undone.
    assert!(got.contains(r#""projects":["/work/alpha""#), "{got}");
}
