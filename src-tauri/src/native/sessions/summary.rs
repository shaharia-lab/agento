//! One row of the sessions list, and the projection every reader shares.
//!
//! Mirrors `claudesessions.ClaudeSessionSummary` and the
//! `sessionSummaryColumns` / `sessionSummarySource` constants in
//! `internal/claudesessions/scanner.go`. The SQL is reproduced verbatim rather
//! than rewritten, because the sub-agent roll-up's column aliases (`it`, `ot`,
//! `tc`, `adm`, …) are what the filter expressions in `query.rs` are written
//! against, and because a grouped sub-select is what stops a session with
//! several sub-agents from being multiplied out by the join.
//!
//! **Field order is wire order.** Every struct below declares its fields in the
//! order the Go struct declares them; `serde` and `encoding/json` both emit
//! declaration order, so re-sorting them for tidiness changes the bytes.
//!
//! **Maps are `BTreeMap`.** Go marshals a map with its keys sorted, so a
//! `HashMap` here would emit the same data in an order that changes run to run.

use std::collections::BTreeMap;

use rusqlite::Row;
use serde::Serialize;

use crate::native::gotime::GoTime;

/// Token consumption. Mirrors `claudesessions.TokenUsage`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// The authoritative cache-write total; the 5m/1h fields split it by TTL
    /// (they bill at 1.25× and 2× input) and always sum to this.
    pub cache_creation_tokens: i64,
    pub cache_creation_5m_tokens: i64,
    pub cache_creation_1h_tokens: i64,
    pub cache_read_tokens: i64,
}

/// USD cost, broken down by token category. Mirrors `claudesessions.SessionCost`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionCost {
    pub input_usd: f64,
    pub output_usd: f64,
    pub cache_read_usd: f64,
    pub cache_write_usd: f64,
    pub total_usd: f64,
}

/// A pull request a session was linked to.
#[derive(Debug, Clone, Serialize)]
pub struct SessionPR {
    pub pr_number: i64,
    pub pr_url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pr_repository: String,
    pub first_seen_at: GoTime,
}

/// One session as the list renders it.
///
/// `Default` exists for the scanner, which builds a row field by field from a
/// transcript rather than from a query.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub project_path: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub config_dir: String,
    pub preview: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub custom_title: String,
    #[serde(skip_serializing_if = "is_false")]
    pub is_favorite: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub native_title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ai_title: String,
    pub display_title: String,
    pub start_time: GoTime,
    pub last_activity: GoTime,
    pub active_duration_ms: i64,
    pub subagent_active_duration_ms: i64,
    pub message_count: i64,
    pub event_count: i64,
    /// Main thread only — `subagent_usage` holds delegated work.
    pub usage: TokenUsage,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub git_branch: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    pub subagent_count: i64,
    pub subagent_usage: TokenUsage,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub subagent_usage_by_model: BTreeMap<String, TokenUsage>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub agent_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub permission_mode: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mode: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub relocated_cwd: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub worktree_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub worktree_branch: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub original_branch: String,
    pub compaction_count: i64,
    pub dropped_tokens: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub prs: Vec<SessionPR>,
    pub cost: SessionCost,
    pub subagent_cost: SessionCost,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub cost_by_model: BTreeMap<String, SessionCost>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub subagent_cost_by_model: BTreeMap<String, SessionCost>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unpriced_models: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub unpriced_tokens: i64,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

impl SessionSummary {
    /// Total input+output tokens, main thread plus delegated — what the cursor
    /// renders for the token sort.
    pub fn total_conversation_tokens(&self) -> i64 {
        self.usage.input_tokens
            + self.usage.output_tokens
            + self.subagent_usage.input_tokens
            + self.subagent_usage.output_tokens
    }

    /// Main-thread plus delegated cost, the figure the cost sort pages on.
    pub fn total_cost_usd(&self) -> f64 {
        self.cost.total_usd + self.subagent_cost.total_usd
    }

    /// Main-thread plus delegated active duration.
    pub fn total_active_duration_ms(&self) -> i64 {
        self.active_duration_ms + self.subagent_active_duration_ms
    }

    /// Main-thread plus delegated token usage — what aggregate reporting reads,
    /// since `usage` deliberately excludes delegated work.
    pub fn total_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.usage.input_tokens + self.subagent_usage.input_tokens,
            output_tokens: self.usage.output_tokens + self.subagent_usage.output_tokens,
            cache_creation_tokens: self.usage.cache_creation_tokens
                + self.subagent_usage.cache_creation_tokens,
            cache_creation_5m_tokens: self.usage.cache_creation_5m_tokens
                + self.subagent_usage.cache_creation_5m_tokens,
            cache_creation_1h_tokens: self.usage.cache_creation_1h_tokens
                + self.subagent_usage.cache_creation_1h_tokens,
            cache_read_tokens: self.usage.cache_read_tokens + self.subagent_usage.cache_read_tokens,
        }
    }

    /// Main-thread plus delegated cost. The component order matters: the
    /// summary adds the four categories separately and totals them, while the
    /// cache-savings card sums `total_usd` per session, and the two arrive at
    /// *different* doubles for the same money. Both are reproduced as written.
    pub fn total_cost(&self) -> SessionCost {
        let mut total = self.cost.clone();
        total.add(&self.subagent_cost);
        total
    }

    /// The session's whole cost keyed by the model that spent it — main-thread
    /// and delegated together, which is the attribution rule delegated tokens
    /// already follow.
    pub fn total_cost_by_model(&self) -> BTreeMap<String, SessionCost> {
        let mut out: BTreeMap<String, SessionCost> = BTreeMap::new();
        for breakdown in [&self.cost_by_model, &self.subagent_cost_by_model] {
            for (model, cost) in breakdown {
                out.entry(model.clone()).or_default().add(cost);
            }
        }
        out
    }

    /// The token counterpart. Main-thread usage is attributed to the session's
    /// own model; delegated usage to each sub-agent's. A session that delegated
    /// but has no per-model breakdown loaded falls back to the parent's model
    /// rather than dropping the tokens.
    pub fn total_usage_by_model(&self) -> BTreeMap<String, TokenUsage> {
        let mut out: BTreeMap<String, TokenUsage> = BTreeMap::new();
        let mut add = |model: &str, u: &TokenUsage| {
            let entry = out.entry(display_model(model)).or_default();
            entry.input_tokens += u.input_tokens;
            entry.output_tokens += u.output_tokens;
            entry.cache_creation_tokens += u.cache_creation_tokens;
            entry.cache_creation_5m_tokens += u.cache_creation_5m_tokens;
            entry.cache_creation_1h_tokens += u.cache_creation_1h_tokens;
            entry.cache_read_tokens += u.cache_read_tokens;
        };

        add(&self.model, &self.usage);
        if !self.subagent_usage_by_model.is_empty() {
            for (model, u) in &self.subagent_usage_by_model {
                add(model, u);
            }
            return out;
        }
        add(&self.model, &self.subagent_usage);
        out
    }
}

impl SessionCost {
    /// Accumulate another breakdown, component by component, in Go's order.
    pub fn add(&mut self, o: &SessionCost) {
        self.input_usd += o.input_usd;
        self.output_usd += o.output_usd;
        self.cache_read_usd += o.cache_read_usd;
        self.cache_write_usd += o.cache_write_usd;
        self.total_usd += o.total_usd;
    }
}

/// The projection every reader of a session summary shares, so the list, the
/// detail page and the paged query cannot drift into reporting different
/// figures for the same row.
pub const SUMMARY_COLUMNS: &str = "
	SELECT c.session_id, c.project_path, c.preview, c.custom_title, c.is_favorite,
	       c.start_time, c.last_activity, c.message_count, c.event_count,
	       c.input_tokens, c.output_tokens, c.cache_creation_tokens, c.cache_read_tokens,
	       c.cache_creation_5m_tokens, c.cache_creation_1h_tokens,
	       c.git_branch, c.model, c.cwd, c.native_title, c.ai_title,
	       c.agent_name, c.permission_mode, c.mode, c.relocated_cwd,
	       c.worktree_name, c.worktree_branch, c.original_branch,
	       c.compaction_count, c.dropped_tokens,
	       c.input_cost_usd, c.output_cost_usd, c.cache_read_cost_usd,
	       c.cache_write_cost_usd, c.total_cost_usd, c.unpriced_models, c.unpriced_tokens,
	       c.cost_by_model, c.active_duration_ms, c.config_dir,
	       COALESCE(sa.n, 0), COALESCE(sa.it, 0), COALESCE(sa.ot, 0),
	       COALESCE(sa.cct, 0), COALESCE(sa.crt, 0),
	       COALESCE(sa.c5m, 0), COALESCE(sa.c1h, 0),
	       COALESCE(sa.ic, 0), COALESCE(sa.oc, 0), COALESCE(sa.crc, 0),
	       COALESCE(sa.cwc, 0), COALESCE(sa.tc, 0), COALESCE(sa.ut, 0),
	       COALESCE(sa.um, ''), COALESCE(sa.adm, 0)";

/// The FROM/JOIN half, split out so an aggregate can reuse it without the
/// projection. Its aliases are what `query.rs`'s metric expressions name.
pub const SUMMARY_SOURCE: &str = "
	FROM claude_session_cache c
	LEFT JOIN (
		SELECT parent_session_id,
		       COUNT(*) AS n,
		       SUM(input_tokens) AS it,
		       SUM(output_tokens) AS ot,
		       SUM(cache_creation_tokens) AS cct,
		       SUM(cache_read_tokens) AS crt,
		       SUM(cache_creation_5m_tokens) AS c5m,
		       SUM(cache_creation_1h_tokens) AS c1h,
		       SUM(input_cost_usd) AS ic,
		       SUM(output_cost_usd) AS oc,
		       SUM(cache_read_cost_usd) AS crc,
		       SUM(cache_write_cost_usd) AS cwc,
		       SUM(total_cost_usd) AS tc,
		       SUM(unpriced_tokens) AS ut,
		       SUM(active_duration_ms) AS adm,
		       GROUP_CONCAT(NULLIF(unpriced_models, ''), char(10)) AS um
		FROM claude_subagent_cache
		GROUP BY parent_session_id
	) sa ON sa.parent_session_id = c.session_id";

/// Scan one row of `SUMMARY_COLUMNS`, applying the same post-processing
/// `scanSessionSummary` does.
pub fn scan(row: &Row<'_>) -> rusqlite::Result<SessionSummary> {
    let start_time: String = row.get(5)?;
    let last_activity: String = row.get(6)?;
    let unpriced: String = row.get(34)?;
    let cost_by_model: String = row.get(36)?;
    let subagent_unpriced_tokens: i64 = row.get(51)?;
    let subagent_unpriced_models: String = row.get(52)?;

    let mut s = SessionSummary {
        session_id: row.get(0)?,
        project_path: row.get(1)?,
        config_dir: row.get(38)?,
        preview: row.get(2)?,
        custom_title: row.get(3)?,
        is_favorite: row.get::<_, i64>(4)? == 1,
        native_title: row.get(18)?,
        ai_title: row.get(19)?,
        display_title: String::new(),
        start_time: crate::native::gotime::from_sql_text(&start_time, 5)?,
        last_activity: crate::native::gotime::from_sql_text(&last_activity, 6)?,
        active_duration_ms: row.get(37)?,
        subagent_active_duration_ms: row.get(53)?,
        message_count: row.get(7)?,
        event_count: row.get(8)?,
        usage: TokenUsage {
            input_tokens: row.get(9)?,
            output_tokens: row.get(10)?,
            cache_creation_tokens: row.get(11)?,
            cache_creation_5m_tokens: row.get(13)?,
            cache_creation_1h_tokens: row.get(14)?,
            cache_read_tokens: row.get(12)?,
        },
        git_branch: row.get(15)?,
        model: row.get(16)?,
        cwd: row.get(17)?,
        subagent_count: row.get(39)?,
        subagent_usage: TokenUsage {
            input_tokens: row.get(40)?,
            output_tokens: row.get(41)?,
            cache_creation_tokens: row.get(42)?,
            cache_creation_5m_tokens: row.get(44)?,
            cache_creation_1h_tokens: row.get(45)?,
            cache_read_tokens: row.get(43)?,
        },
        subagent_usage_by_model: BTreeMap::new(),
        agent_name: row.get(20)?,
        permission_mode: row.get(21)?,
        mode: row.get(22)?,
        relocated_cwd: row.get(23)?,
        worktree_name: row.get(24)?,
        worktree_branch: row.get(25)?,
        original_branch: row.get(26)?,
        compaction_count: row.get(27)?,
        dropped_tokens: row.get(28)?,
        prs: Vec::new(),
        cost: SessionCost {
            input_usd: row.get(29)?,
            output_usd: row.get(30)?,
            cache_read_usd: row.get(31)?,
            cache_write_usd: row.get(32)?,
            total_usd: row.get(33)?,
        },
        subagent_cost: SessionCost {
            input_usd: row.get(46)?,
            output_usd: row.get(47)?,
            cache_read_usd: row.get(48)?,
            cache_write_usd: row.get(49)?,
            total_usd: row.get(50)?,
        },
        cost_by_model: decode_cost_by_model(&cost_by_model),
        subagent_cost_by_model: BTreeMap::new(),
        unpriced_models: merge_unpriced_models(&unpriced, &subagent_unpriced_models),
        unpriced_tokens: row.get::<_, i64>(35)? + subagent_unpriced_tokens,
    };
    s.display_title = resolve_display_title(&s);
    Ok(s)
}

/// The label the UI renders. Agento's own rename wins, then Claude Code's
/// native title, then its AI-generated one, then the first prompt.
pub fn resolve_display_title(s: &SessionSummary) -> String {
    for candidate in [&s.custom_title, &s.native_title, &s.ai_title, &s.preview] {
        if !candidate.is_empty() {
            return candidate.clone();
        }
    }
    String::new()
}

/// Decode the `cost_by_model` JSON column. Anything unusable yields no map at
/// all rather than a partial one, matching Go's `decodeCostByModel`.
fn decode_cost_by_model(raw: &str) -> BTreeMap<String, SessionCost> {
    if raw.is_empty() || raw == "{}" {
        return BTreeMap::new();
    }
    serde_json::from_str(raw).unwrap_or_default()
}

/// Merge the session's own unpriced models with its sub-agents', deduplicated
/// and sorted.
///
/// Both halves count toward the same disclosure: reading one without the other
/// would let a row report excluded tokens attributed to no model, or show a
/// confident total for a session that is only partly priced.
fn merge_unpriced_models(own: &str, delegated: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    for group in [own, delegated] {
        for model in group.split('\n') {
            if !model.is_empty() {
                seen.insert(model.to_string());
            }
        }
    }
    seen.into_iter().collect()
}

/// `displayModel`: an empty model ID reads as "unknown" rather than as a blank
/// key.
pub fn display_model(model: &str) -> String {
    if model.is_empty() {
        "unknown".to_string()
    } else {
        model.to_string()
    }
}

/// Deserialization counterpart for the `cost_by_model` column, whose stored
/// shape is this struct's own JSON.
impl<'de> serde::Deserialize<'de> for SessionCost {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            #[serde(default)]
            input_usd: f64,
            #[serde(default)]
            output_usd: f64,
            #[serde(default)]
            cache_read_usd: f64,
            #[serde(default)]
            cache_write_usd: f64,
            #[serde(default)]
            total_usd: f64,
        }
        let r = Raw::deserialize(d)?;
        Ok(SessionCost {
            input_usd: r.input_usd,
            output_usd: r.output_usd,
            cache_read_usd: r.cache_read_usd,
            cache_write_usd: r.cache_write_usd,
            total_usd: r.total_usd,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_title_precedence_puts_the_users_rename_first() {
        let mut s = blank();
        s.preview = "first prompt".into();
        assert_eq!(resolve_display_title(&s), "first prompt");
        s.ai_title = "AI title".into();
        assert_eq!(resolve_display_title(&s), "AI title");
        s.native_title = "native".into();
        assert_eq!(resolve_display_title(&s), "native");
        s.custom_title = "mine".into();
        assert_eq!(resolve_display_title(&s), "mine");
    }

    #[test]
    fn unpriced_models_merge_dedupe_and_sort_across_both_halves() {
        assert_eq!(
            merge_unpriced_models("kimi-k2\nglm-4.6", "glm-4.6\nqwen-plus"),
            vec!["glm-4.6", "kimi-k2", "qwen-plus"]
        );
        assert!(merge_unpriced_models("", "").is_empty());
    }

    #[test]
    fn a_malformed_cost_column_yields_no_map_rather_than_half_a_map() {
        assert!(decode_cost_by_model("").is_empty());
        assert!(decode_cost_by_model("{}").is_empty());
        assert!(decode_cost_by_model("not json").is_empty());

        let decoded = decode_cost_by_model(r#"{"opus":{"input_usd":1.5,"total_usd":2}}"#);
        assert_eq!(decoded["opus"].input_usd, 1.5);
        assert_eq!(decoded["opus"].total_usd, 2.0);
        assert_eq!(decoded["opus"].output_usd, 0.0);
    }

    fn blank() -> SessionSummary {
        SessionSummary {
            session_id: String::new(),
            project_path: String::new(),
            config_dir: String::new(),
            preview: String::new(),
            custom_title: String::new(),
            is_favorite: false,
            native_title: String::new(),
            ai_title: String::new(),
            display_title: String::new(),
            start_time: GoTime::parse("2026-01-01T00:00:00Z").expect("parse"),
            last_activity: GoTime::parse("2026-01-01T00:00:00Z").expect("parse"),
            active_duration_ms: 0,
            subagent_active_duration_ms: 0,
            message_count: 0,
            event_count: 0,
            usage: TokenUsage::default(),
            git_branch: String::new(),
            model: String::new(),
            cwd: String::new(),
            subagent_count: 0,
            subagent_usage: TokenUsage::default(),
            subagent_usage_by_model: BTreeMap::new(),
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
            cost: SessionCost::default(),
            subagent_cost: SessionCost::default(),
            cost_by_model: BTreeMap::new(),
            subagent_cost_by_model: BTreeMap::new(),
            unpriced_models: Vec::new(),
            unpriced_tokens: 0,
        }
    }
}
