//! The nine static-analysis passes over a session transcript.
//!
//! Mirrors `internal/claudesessions/`: `processor.go` (the record),
//! `processor_registry.go` (the pass), and one section below per
//! `*_processor.go`. Read them side by side — the Go comments carry the *why*
//! for each metric and are not repeated except where the port needs a decision.
//!
//! **This writes nothing**, and that is a property of the passes rather than a
//! deferral: a processor is a function from a transcript to a [`SessionInsight`]
//! and nothing more. `insights/store.rs` stores one and `insights/worker.rs` is
//! the loop that calls both (#408).
//!
//! ## Order is a dependency, not a style
//!
//! Processors run in registration order and some read what earlier ones wrote:
//! `AutonomyScore` is derived entirely from `TurnCount`'s two outputs, and
//! `TokenProfile` divides by `TurnCount`. Reordering [`PIPELINE`] changes the
//! numbers.
//!
//! ## Divide by one, never by zero
//!
//! Several metrics are per-turn averages, and a session can legitimately have
//! **zero** genuine turns: since #226 a skill-driven session's only user event
//! is the injected preamble, with the user's argument inside it. Those are
//! often long, highly autonomous runs. Reporting 0 steps-per-turn for them
//! would say the work did not happen, and `AutonomyScore` reads that value —
//! it would score the most autonomous sessions in the corpus at the wrong
//! extreme.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use super::index;
use super::transcript::{self, is_turn_start, parse_content_blocks, Event};
use crate::native::active_time::ActiveTimeTracker;
use crate::native::pricing::{Cost, PricedUsage, Resolver};

/// Bumped whenever any processor's logic changes; rows below it are reprocessed.
///
/// Kept in step with Go's `CurrentProcessorVersion` because the parity check
/// only compares rows written at this version — an older row holds figures a
/// *correct* port must disagree with.
pub const CURRENT_PROCESSOR_VERSION: i64 = 10;

/// Every computed metric for one session. Mirrors `claudesessions.SessionInsight`
/// minus the two fields that are not computed from the transcript
/// (`processor_version`, `scanned_at`) and the reserved `session_type`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionInsight {
    pub session_id: String,

    // TurnCountProcessor
    pub turn_count: i64,
    pub steps_per_turn_avg: f64,

    // AutonomyScoreProcessor
    pub autonomy_score: f64,

    // ToolUsageProcessor
    pub tool_calls_total: i64,
    pub tool_breakdown: BTreeMap<String, i64>,

    // AttributionProcessor. Every breakdown counts tool_use blocks, so they
    // share tool_calls_total as their denominator:
    // sum(skill_breakdown) + unattributed_calls == tool_calls_total.
    pub skill_breakdown: BTreeMap<String, i64>,
    pub plugin_breakdown: BTreeMap<String, i64>,
    pub mcp_server_breakdown: BTreeMap<String, i64>,
    pub mcp_tool_breakdown: BTreeMap<String, i64>,
    pub effort_breakdown: BTreeMap<String, i64>,
    pub agent_breakdown: BTreeMap<String, i64>,
    pub unattributed_calls: i64,

    // TimeProfileProcessor
    pub total_duration_ms: i64,
    pub active_duration_ms: i64,
    pub claude_working_time_ms: i64,

    // TokenProfileProcessor
    pub cache_hit_rate: f64,
    pub tokens_per_turn_avg: f64,
    pub cost_estimate_usd: f64,

    // ErrorRateProcessor
    pub tool_error_rate: f64,
    pub tool_error_count: i64,
    pub has_errors: bool,

    // ConversationDepthProcessor
    pub max_consecutive_tool_calls: i64,
    pub longest_autonomous_chain: i64,

    // SessionRhythmProcessor
    pub avg_user_response_time_ms: f64,
    pub avg_claude_response_time_ms: f64,
}

/// One pass over a session's events.
trait Processor {
    fn process(&mut self, ev: &Event);
    /// Written after every event, so a processor may read what an earlier one
    /// wrote.
    fn finalize(&self, insight: &mut SessionInsight);
}

/// The processors, in dependency order. **Reordering changes the numbers.**
fn pipeline<'a>(ctx: &Ctx<'a>) -> Vec<Box<dyn Processor + 'a>> {
    vec![
        Box::new(TurnCount::default()),
        Box::new(AutonomyScore),
        Box::new(ToolUsage::default()),
        Box::new(Attribution::default()),
        Box::new(TimeProfile::new(ctx.idle_gap_ms)),
        Box::new(TokenProfile::new(ctx)),
        Box::new(ErrorRate::default()),
        Box::new(ConversationDepth::default()),
        Box::new(SessionRhythm::new(ctx.idle_gap_ms)),
    ]
}

/// What a run needs from outside the transcript.
pub struct Ctx<'a> {
    /// The user-configurable "still working" threshold, in milliseconds. Read
    /// once per run: a settings save landing mid-pass must not judge two gaps
    /// of the same conversation by different rules.
    pub idle_gap_ms: i64,
    /// `None` prices nothing, matching Go's inert accumulator when no resolver
    /// is wired.
    pub resolver: Option<&'a Resolver>,
}

/// Run the pipeline over one session's transcripts.
///
/// **The parent must come first.** Turn-scoped processors derive their
/// structure from it, and every sub-agent event is flagged `isSidechain`, which
/// those processors deliberately do not treat as a new turn. Feeding one set of
/// processors means the insight covers delegated work additively — tool calls,
/// cost and error counts include what sub-agents did.
///
/// A sub-agent transcript that cannot be read is skipped; only a failure on the
/// parent is fatal.
///
/// `doc` accumulates the session's `session_search` document from **this same
/// read** (#435). It is a parameter rather than a tenth processor because a
/// `Processor` finalizes into a `SessionInsight`, which has nowhere to put three
/// text columns — and because passing it makes the extra work visible at the one
/// call site rather than hidden in `PIPELINE`, whose order is load-bearing for
/// the numbers. A caller that wants no document passes one and drops it; the
/// accumulator is inert once its budget is spent, so that costs a struct.
pub fn run(
    session_id: &str,
    files: &[std::path::PathBuf],
    ctx: &Ctx,
    doc: &mut index::DocAccumulator,
) -> Result<SessionInsight, String> {
    let Some((parent, subagents)) = files.split_first() else {
        return Err(format!("no session files given for {session_id:?}"));
    };

    let mut processors = pipeline(ctx);
    feed(parent, &mut processors, doc)?;
    for file in subagents {
        if let Err(e) = feed(file, &mut processors, doc) {
            log::warn!("skipping unreadable sub-agent transcript: {e}");
        }
    }

    let mut insight = SessionInsight {
        session_id: session_id.to_string(),
        ..Default::default()
    };
    for p in &processors {
        p.finalize(&mut insight);
    }
    Ok(insight)
}

fn feed(
    path: &std::path::Path,
    processors: &mut [Box<dyn Processor + '_>],
    doc: &mut index::DocAccumulator,
) -> Result<(), String> {
    for ev in transcript::read(path)? {
        // Not a conversation event; Go skips it before any processor sees it.
        // The indexer inherits the skip by sitting inside the same guard.
        if ev.event_type == "file-history-snapshot" {
            continue;
        }
        for p in processors.iter_mut() {
            p.process(&ev);
        }
        doc.observe(&ev);
    }
    Ok(())
}

// ─── turn_processor.go ────────────────────────────────────────────────────────

/// Genuine user turns, and the average number of events between them.
#[derive(Default)]
struct TurnCount {
    turns: i64,
    events: i64,
}

impl Processor for TurnCount {
    fn process(&mut self, ev: &Event) {
        self.events += 1;
        if is_turn_start(ev) {
            self.turns += 1;
        }
    }

    fn finalize(&self, insight: &mut SessionInsight) {
        insight.turn_count = self.turns;
        // One unattended run of n steps is n steps per turn, which is what
        // turn_count == 1 already means to every consumer.
        insight.steps_per_turn_avg = self.events as f64 / self.turns.max(1) as f64;
    }
}

// ─── autonomy_processor.go ────────────────────────────────────────────────────

/// How much the user let Claude run between interventions, 0–100.
///
/// Derives everything from `TurnCount`'s output, so it must run after it.
struct AutonomyScore;

impl Processor for AutonomyScore {
    fn process(&mut self, _ev: &Event) {}

    fn finalize(&self, insight: &mut SessionInsight) {
        let log_factor = (insight.steps_per_turn_avg + 1.0).ln() / 10f64.ln();
        let log_factor = log_factor.min(1.0);

        // Zero turns takes the same branch as one, deliberately: since #226 a
        // skill-driven session has no genuine user turn at all, and zero
        // interventions is more autonomous than one, not less.
        let score = if insight.turn_count <= 1 {
            100.0 * log_factor
        } else {
            100.0 * (1.0 / insight.turn_count as f64) * log_factor
        };
        insight.autonomy_score = score.clamp(0.0, 100.0);
    }
}

// ─── tool_processor.go ────────────────────────────────────────────────────────

/// How many tool calls were made, and which tools.
///
/// Error tracking is deliberately left to [`ErrorRate`], so there is one source
/// of truth for error metrics.
#[derive(Default)]
struct ToolUsage {
    breakdown: BTreeMap<String, i64>,
}

impl Processor for ToolUsage {
    fn process(&mut self, ev: &Event) {
        let Some(msg) = &ev.message else { return };
        if msg.role != "assistant" {
            return;
        }
        for b in parse_content_blocks(&msg.content) {
            if b.block_type == "tool_use" && !b.name.is_empty() {
                *self.breakdown.entry(b.name).or_default() += 1;
            }
        }
    }

    fn finalize(&self, insight: &mut SessionInsight) {
        insight.tool_calls_total = self.breakdown.values().sum();
        insight.tool_breakdown = self.breakdown.clone();
    }
}

// ─── attribution_processor.go ─────────────────────────────────────────────────

/// How Claude Code names a tool exposed by an MCP server.
const MCP_TOOL_PREFIX: &str = "mcp__";

/// Tool usage broken down by the skill, plugin, MCP server and sub-agent
/// responsible, plus the reasoning-effort tier.
///
/// **Counted per `tool_use` block, not per event.** Claude Code splits one
/// assistant message into several JSONL events — thinking, text, each tool call
/// — which all share a message id and therefore carry identical attribution
/// fields. Counting per event would inflate every number by a variable factor.
///
/// **MCP attribution comes from the block name**, not from
/// `attributionMcpServer`/`attributionMcpTool`. Those hold the last MCP tool
/// touched and persist onto later, unrelated turns: across the reference corpus
/// only ~63 of ~730 tool-bearing events had them agree with the block actually
/// being called.
#[derive(Default)]
struct Attribution {
    skills: BTreeMap<String, i64>,
    plugins: BTreeMap<String, i64>,
    mcp_servers: BTreeMap<String, i64>,
    mcp_tools: BTreeMap<String, i64>,
    efforts: BTreeMap<String, i64>,
    agents: BTreeMap<String, i64>,
    unattributed: i64,
}

impl Processor for Attribution {
    fn process(&mut self, ev: &Event) {
        let Some(msg) = &ev.message else { return };
        if msg.role != "assistant" {
            return;
        }
        for b in parse_content_blocks(&msg.content) {
            if b.block_type != "tool_use" || b.name.is_empty() {
                continue;
            }
            self.attribute(ev, &b.name);
        }
    }

    fn finalize(&self, insight: &mut SessionInsight) {
        insight.skill_breakdown = self.skills.clone();
        insight.plugin_breakdown = self.plugins.clone();
        insight.mcp_server_breakdown = self.mcp_servers.clone();
        insight.mcp_tool_breakdown = self.mcp_tools.clone();
        insight.effort_breakdown = self.efforts.clone();
        insight.agent_breakdown = self.agents.clone();
        insight.unattributed_calls = self.unattributed;
    }
}

impl Attribution {
    /// Credit one block to every dimension the event names. An empty field
    /// contributes nothing rather than creating a `""` bucket — except the
    /// skill, whose absence is itself the signal that this was built-in tool
    /// use, counted as unattributed so the totals reconcile.
    fn attribute(&mut self, ev: &Event, block_name: &str) {
        if ev.attribution_skill.is_empty() {
            self.unattributed += 1;
        } else {
            *self.skills.entry(ev.attribution_skill.clone()).or_default() += 1;
        }
        // A skill can be user-level rather than shipped by a plugin, so the
        // plugin is counted independently instead of nested under the skill.
        if !ev.attribution_plugin.is_empty() {
            *self
                .plugins
                .entry(ev.attribution_plugin.clone())
                .or_default() += 1;
        }
        if !ev.effort.is_empty() {
            *self.efforts.entry(ev.effort.clone()).or_default() += 1;
        }
        // Sub-agent transcripts carry the agent type that owns the turn; a
        // parent transcript leaves it empty, so main-thread work contributes
        // nothing here rather than landing in a catch-all bucket.
        if !ev.attribution_agent.is_empty() {
            *self.agents.entry(ev.attribution_agent.clone()).or_default() += 1;
        }
        if let Some((server, tool)) = split_mcp_tool_name(block_name) {
            *self.mcp_servers.entry(server.to_string()).or_default() += 1;
            *self.mcp_tools.entry(tool.to_string()).or_default() += 1;
        }
    }
}

/// Split `mcp__<server>__<tool>`.
///
/// Server names may themselves contain underscores (`vibexp_io_vibexp_team`),
/// so the split is on the **first** `__` after the prefix rather than on every
/// underscore.
fn split_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(MCP_TOOL_PREFIX)?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

// ─── time_processor.go + active_duration.go ───────────────────────────────────

/// The session's three time figures.
///
/// `total_duration_ms` is the raw first-to-last span — honest as "first seen →
/// last touched" and nothing else, since a resumed session's span contains
/// every idle day in between. `active_duration_ms` caps each inter-event gap at
/// the idle threshold and is the figure dashboards average.
/// `claude_working_time_ms` is the subset of that ending at an assistant event.
struct TimeProfile {
    first: Option<DateTime<Utc>>,
    last: Option<DateTime<Utc>>,
    /// The capped-gap walk itself lives in `native::active_time`, shared with
    /// the scanner: the same session's active duration is stored on both
    /// `session_insights` and `claude_session_cache`, and two implementations
    /// of one rule is how they would drift — invisibly, since the threshold is
    /// user-configurable.
    active: ActiveTimeTracker,
}

impl TimeProfile {
    fn new(idle_gap_ms: i64) -> Self {
        TimeProfile {
            first: None,
            last: None,
            active: ActiveTimeTracker::new(idle_gap_ms),
        }
    }
}

impl Processor for TimeProfile {
    fn process(&mut self, ev: &Event) {
        let Some(ts) = ev.timestamp else { return };
        if self.first.is_none_or_earlier(ts) {
            self.first = Some(ts);
        }
        if self.last.is_none_or_later(ts) {
            self.last = Some(ts);
        }
        self.active.observe(ts, ev.event_type == "assistant");
    }

    fn finalize(&self, insight: &mut SessionInsight) {
        if let (Some(first), Some(last)) = (self.first, self.last) {
            insight.total_duration_ms = (last - first).num_milliseconds();
        }
        let (active, assistant) = self.active.durations();
        insight.active_duration_ms = active;
        insight.claude_working_time_ms = assistant;
    }
}

/// `Option<DateTime>` comparisons spelled out, so the min/max reads the way
/// Go's zero-time checks do.
trait BoundExt {
    fn is_none_or_earlier(&self, ts: DateTime<Utc>) -> bool;
    fn is_none_or_later(&self, ts: DateTime<Utc>) -> bool;
}

impl BoundExt for Option<DateTime<Utc>> {
    fn is_none_or_earlier(&self, ts: DateTime<Utc>) -> bool {
        self.is_none() || self.is_some_and(|cur| ts < cur)
    }
    fn is_none_or_later(&self, ts: DateTime<Utc>) -> bool {
        // Go compares against the zero time, which every real timestamp is
        // after, so an unset bound always loses.
        self.is_none() || self.is_some_and(|cur| ts > cur)
    }
}

// ─── token_processor.go ───────────────────────────────────────────────────────

/// Token usage across assistant messages, and the cost and cache figures.
///
/// Cost is accumulated **per assistant message** at that message's own model
/// and timestamp — the same resolver the scanner uses, so a session's insight
/// cost and its analytics cost cannot diverge over a first-seen-model or a
/// price-boundary difference.
struct TokenProfile<'a> {
    resolver: Option<&'a Resolver>,
    input: i64,
    output: i64,
    cache_creation: i64,
    cache_read: i64,
    cost: Cost,
    priced_messages: i64,
}

impl<'a> TokenProfile<'a> {
    fn new(ctx: &Ctx<'a>) -> Self {
        TokenProfile {
            resolver: ctx.resolver,
            input: 0,
            output: 0,
            cache_creation: 0,
            cache_read: 0,
            cost: Cost::default(),
            priced_messages: 0,
        }
    }
}

impl Processor for TokenProfile<'_> {
    fn process(&mut self, ev: &Event) {
        let Some(msg) = &ev.message else { return };
        if msg.role != "assistant" {
            return;
        }
        let Some(u) = &msg.usage else { return };
        let (five_min, one_hour) = u.split_cache_tiers();
        self.input += u.input_tokens;
        self.output += u.output_tokens;
        self.cache_creation += u.cache_creation_input_tokens;
        self.cache_read += u.cache_read_input_tokens;

        let Some(resolver) = self.resolver else {
            return;
        };
        if u.input_tokens
            + u.output_tokens
            + u.cache_creation_input_tokens
            + u.cache_read_input_tokens
            == 0
        {
            return;
        }
        // An event with no timestamp is priced at Go's zero time, which
        // resolves to the earliest rate marked estimated rather than to
        // nothing.
        let at = ev.timestamp.unwrap_or(DateTime::<Utc>::MIN_UTC);
        self.price_at(resolver, &msg.model, five_min, one_hour, u, at);
    }

    fn finalize(&self, insight: &mut SessionInsight) {
        // Via the shared definition, so this and the analytics dashboard cannot
        // report two different numbers under the same name again.
        insight.cache_hit_rate = crate::native::analytics::report::cache_hit_rate(
            self.input,
            self.cache_read,
            self.cache_creation,
        );

        // Divide by one when there are no turns, for the reason in the module
        // docs: reporting 0 tokens per turn for a session that spent millions
        // says the opposite of what happened.
        insight.tokens_per_turn_avg =
            (self.input + self.output) as f64 / insight.turn_count.max(1) as f64;

        // A session with no usage-bearing messages — or run without a resolver
        // — leaves the estimate at zero, matching the semantics where an
        // unpriced model contributes no cost.
        if self.priced_messages > 0 {
            insight.cost_estimate_usd = self.cost.total_cost_usd;
        }
    }
}

impl TokenProfile<'_> {
    fn price_at(
        &mut self,
        resolver: &Resolver,
        model: &str,
        five_min: i64,
        one_hour: i64,
        u: &transcript::Usage,
        at: DateTime<Utc>,
    ) {
        // The synthetic placeholder and embedding models resolve to
        // non-billable catalog rows, so they price at $0.00 without being
        // mistaken for a gap in the catalog.
        let Some(resolved) = resolver.resolve(model, at) else {
            return;
        };
        let priced = resolved.rate.price(PricedUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_creation_5m_tokens: five_min,
            cache_creation_1h_tokens: one_hour,
            cache_read_tokens: u.cache_read_input_tokens,
        });
        self.cost.add(&priced);
        self.priced_messages += 1;
    }
}

// ─── error_processor.go ───────────────────────────────────────────────────────

/// Tool errors and the rate they occurred at.
#[derive(Default)]
struct ErrorRate {
    errors: i64,
    results: i64,
}

impl Processor for ErrorRate {
    fn process(&mut self, ev: &Event) {
        let Some(msg) = &ev.message else { return };
        if msg.role != "user" {
            return;
        }
        for b in parse_content_blocks(&msg.content) {
            if b.block_type == "tool_result" {
                self.results += 1;
                if b.is_error {
                    self.errors += 1;
                }
            }
        }
    }

    fn finalize(&self, insight: &mut SessionInsight) {
        insight.tool_error_count = self.errors;
        insight.has_errors = self.errors > 0;
        if self.results > 0 {
            insight.tool_error_rate = self.errors as f64 / self.results as f64;
        }
    }
}

// ─── depth_processor.go ───────────────────────────────────────────────────────

/// How deeply Claude goes without user intervention.
///
/// One known imprecision, measured and accepted: since #226 an interrupt notice
/// is injected content rather than a genuine turn, so it no longer breaks the
/// chain — even though it is literally the user intervening. On the reference
/// corpus 115 of 120 interrupts are not followed by a genuine turn, so the
/// break is lost rather than moved, and only 2 sessions change at all.
#[derive(Default)]
struct ConversationDepth {
    max_consecutive: i64,
    longest_chain: i64,
    current_chain: i64,
}

impl Processor for ConversationDepth {
    fn process(&mut self, ev: &Event) {
        match ev.event_type.as_str() {
            "assistant" => {
                if let Some(msg) = &ev.message {
                    self.process_assistant(&parse_content_blocks(&msg.content));
                }
            }
            // Genuine user input breaks the autonomous chain.
            "user" if is_turn_start(ev) => self.current_chain = 0,
            _ => {}
        }
    }

    fn finalize(&self, insight: &mut SessionInsight) {
        insight.max_consecutive_tool_calls = self.max_consecutive;
        insight.longest_autonomous_chain = self.longest_chain;
    }
}

impl ConversationDepth {
    /// A non-`tool_use` block (e.g. text) resets the per-message consecutive
    /// counter but not the session-wide chain.
    fn process_assistant(&mut self, blocks: &[transcript::ContentBlock]) {
        let mut consecutive = 0i64;
        for b in blocks {
            if b.block_type == "tool_use" {
                consecutive += 1;
                self.current_chain += 1;
            } else {
                self.max_consecutive = self.max_consecutive.max(consecutive);
                consecutive = 0;
            }
        }
        self.max_consecutive = self.max_consecutive.max(consecutive);
        self.longest_chain = self.longest_chain.max(self.current_chain);
    }
}

// ─── rhythm_processor.go ──────────────────────────────────────────────────────

/// How long the user and Claude each take to respond.
///
/// Gaps above the idle threshold are **excluded rather than capped**. A message
/// sent after lunch or on resuming days later is a new sitting, not a reply —
/// the corpus contained a 226-hour "user response time" from exactly that — and
/// capping would only make every resumed session's average converge on the cap.
/// On the Claude side a gap that large is a queued-message artifact: the event
/// carries the typed-at timestamp and delivery came after the running turn.
struct SessionRhythm {
    max_gap_ms: i64,
    last_assistant: Option<DateTime<Utc>>,
    last_genuine_user: Option<DateTime<Utc>>,
    user_gaps: Vec<i64>,
    claude_gaps: Vec<i64>,
}

impl SessionRhythm {
    fn new(idle_gap_ms: i64) -> Self {
        SessionRhythm {
            max_gap_ms: idle_gap_ms,
            last_assistant: None,
            last_genuine_user: None,
            user_gaps: Vec::new(),
            claude_gaps: Vec::new(),
        }
    }
}

impl Processor for SessionRhythm {
    fn process(&mut self, ev: &Event) {
        let Some(ts) = ev.timestamp else { return };
        match ev.event_type.as_str() {
            "user" => {
                if !is_turn_start(ev) {
                    return;
                }
                if let Some(last) = self.last_assistant {
                    let gap = (ts - last).num_milliseconds();
                    if (0..=self.max_gap_ms).contains(&gap) {
                        self.user_gaps.push(gap);
                    }
                }
                self.last_genuine_user = Some(ts);
            }
            "assistant" => {
                if let Some(last) = self.last_genuine_user {
                    let gap = (ts - last).num_milliseconds();
                    if gap >= 0 {
                        if gap <= self.max_gap_ms {
                            self.claude_gaps.push(gap);
                        }
                        // Consume the pair even when the gap was an artifact,
                        // so a later assistant event is not measured against it
                        // too.
                        self.last_genuine_user = None;
                    }
                }
                self.last_assistant = Some(ts);
            }
            _ => {}
        }
    }

    fn finalize(&self, insight: &mut SessionInsight) {
        insight.avg_user_response_time_ms = mean(&self.user_gaps);
        insight.avg_claude_response_time_ms = mean(&self.claude_gaps);
    }
}

/// The mean of a slice, or 0 when empty.
fn mean(values: &[i64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<i64>() as f64 / values.len() as f64
}

// ─── Locating a session's transcripts ─────────────────────────────────────────

/// The sub-agent transcript paths belonging to the session whose own transcript
/// is at `session_file`, sorted — `SubagentFiles` in `scanner.go`.
///
/// They live at `<dir of parent>/<session-id>/subagents/*.jsonl`. A missing
/// directory means the session delegated nothing, which is not an error.
pub fn subagent_files(session_id: &str, session_file: &std::path::Path) -> Vec<std::path::PathBuf> {
    let dir = match session_file.parent() {
        Some(parent) => parent.join(session_id).join("subagents"),
        None => return Vec::new(),
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
        .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
        .map(|e| e.path())
        .collect();
    // Go sorts the joined paths as strings; within one directory that is the
    // same order as sorting the file names.
    paths.sort();
    paths
}

/// Every transcript one session's insight is computed from: the parent first,
/// then each sub-agent's.
pub fn session_files(session_id: &str, session_file: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = vec![session_file.to_path_buf()];
    files.extend(subagent_files(session_id, session_file));
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    /// Build a transcript from JSON lines and run the pipeline over it.
    fn insight_of(lines: &[serde_json::Value]) -> SessionInsight {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        let mut file = std::fs::File::create(&path).expect("create");
        for line in lines {
            writeln!(file, "{line}").expect("write");
        }
        drop(file);

        run(
            "s1",
            &[path],
            &Ctx {
                idle_gap_ms: 600_000,
                resolver: None,
            },
            &mut index::DocAccumulator::new(),
        )
        .expect("run")
    }

    fn user(content: serde_json::Value) -> serde_json::Value {
        json!({"type": "user", "message": {"role": "user", "content": content}})
    }

    fn assistant(content: serde_json::Value) -> serde_json::Value {
        json!({"type": "assistant", "message": {"role": "assistant", "content": content}})
    }

    fn tool_use(name: &str) -> serde_json::Value {
        json!({"type": "tool_use", "id": "t", "name": name, "input": {}})
    }

    #[test]
    fn a_skill_driven_session_has_no_turns_and_still_reports_its_work() {
        // The case #226 created: the only user event is the injected preamble,
        // so turn_count is legitimately 0 for a real, long, autonomous run.
        let insight = insight_of(&[
            user(json!([{
                "type": "text",
                "text": "Base directory for this skill: /home/u/.claude/skills/foo"
            }])),
            assistant(json!([tool_use("Bash")])),
            assistant(json!([tool_use("Read")])),
        ]);

        assert_eq!(insight.turn_count, 0);
        // Divided by one, not by zero: three events, one notional turn.
        assert_eq!(insight.steps_per_turn_avg, 3.0);
        // …and it scores as maximally autonomous rather than as zero.
        assert!(insight.autonomy_score > 0.0, "{}", insight.autonomy_score);
        assert_eq!(insight.tool_calls_total, 2);
    }

    #[test]
    fn tool_calls_are_counted_per_block_and_split_by_mcp_server() {
        let insight = insight_of(&[json!({
            "type": "assistant",
            "attributionSkill": "vibexp:prime",
            "effort": "high",
            "message": {"role": "assistant", "content": [
                tool_use("Bash"),
                tool_use("mcp__vibexp_io_vibexp_team__vibexp_io_search"),
            ]},
        })]);

        assert_eq!(insight.tool_calls_total, 2);
        assert_eq!(insight.tool_breakdown["Bash"], 1);
        // Both blocks are credited to the skill that was in context.
        assert_eq!(insight.skill_breakdown["vibexp:prime"], 2);
        assert_eq!(insight.effort_breakdown["high"], 2);
        // The server name contains underscores; the split is on the first `__`
        // after the prefix.
        assert_eq!(insight.mcp_server_breakdown["vibexp_io_vibexp_team"], 1);
        assert_eq!(insight.mcp_tool_breakdown["vibexp_io_search"], 1);
        assert_eq!(insight.unattributed_calls, 0);
    }

    #[test]
    fn built_in_tool_use_is_unattributed_so_the_totals_reconcile() {
        let insight = insight_of(&[
            assistant(json!([tool_use("Bash")])),
            json!({
                "type": "assistant",
                "attributionSkill": "update-docs",
                "message": {"role": "assistant", "content": [tool_use("Edit")]},
            }),
        ]);

        let attributed: i64 = insight.skill_breakdown.values().sum();
        assert_eq!(
            attributed + insight.unattributed_calls,
            insight.tool_calls_total
        );
        assert_eq!(insight.unattributed_calls, 1);
    }

    #[test]
    fn errors_come_from_tool_results_not_from_tool_calls() {
        let insight = insight_of(&[
            assistant(json!([tool_use("Bash")])),
            user(json!([{"type": "tool_result", "tool_use_id": "t", "is_error": true}])),
            user(json!([{"type": "tool_result", "tool_use_id": "t"}])),
        ]);

        assert_eq!(insight.tool_error_count, 1);
        assert!(insight.has_errors);
        assert_eq!(insight.tool_error_rate, 0.5);
        // A tool_result carrier is not a turn.
        assert_eq!(insight.turn_count, 0);
    }

    #[test]
    fn a_genuine_turn_breaks_the_autonomous_chain_and_an_injection_does_not() {
        let insight = insight_of(&[
            assistant(json!([tool_use("Bash"), tool_use("Read")])),
            // Injected, so the chain continues across it.
            user(json!("<system-reminder>keep going</system-reminder>")),
            assistant(json!([tool_use("Edit")])),
            // Genuine: breaks the chain.
            user(json!("now do something else")),
            assistant(json!([tool_use("Write")])),
        ]);

        assert_eq!(insight.turn_count, 1);
        assert_eq!(insight.longest_autonomous_chain, 3);
        // Within one message: two consecutive tool_use blocks.
        assert_eq!(insight.max_consecutive_tool_calls, 2);
    }

    #[test]
    fn a_text_block_resets_the_consecutive_run_but_not_the_chain() {
        let insight = insight_of(&[assistant(json!([
            tool_use("Bash"),
            {"type": "text", "text": "let me check"},
            tool_use("Read"),
            tool_use("Edit"),
        ]))]);

        assert_eq!(insight.max_consecutive_tool_calls, 2);
        assert_eq!(insight.longest_autonomous_chain, 3);
    }

    #[test]
    fn idle_gaps_are_capped_for_active_time_and_excluded_from_the_averages() {
        let at = |s: &str| json!(s);
        let insight = insight_of(&[
            json!({"type": "user", "timestamp": at("2026-08-01T10:00:00Z"),
                   "message": {"role": "user", "content": "start"}}),
            // 1 minute later: a real reply, counted in both.
            json!({"type": "assistant", "timestamp": at("2026-08-01T10:01:00Z"),
                   "message": {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}}),
            // 3 hours later: a new sitting, not a reply.
            json!({"type": "user", "timestamp": at("2026-08-01T13:01:00Z"),
                   "message": {"role": "user", "content": "back"}}),
            json!({"type": "assistant", "timestamp": at("2026-08-01T13:02:00Z"),
                   "message": {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}}),
        ]);

        // The raw span is the whole 3h02m…
        assert_eq!(insight.total_duration_ms, 3 * 3_600_000 + 120_000);
        // …while active time caps the idle gap at the 10-minute threshold:
        // 60s + 600s (capped) + 60s.
        assert_eq!(insight.active_duration_ms, 60_000 + 600_000 + 60_000);
        // Claude's working time is the subset ending at an assistant event.
        assert_eq!(insight.claude_working_time_ms, 120_000);
        // The 3-hour gap is excluded from the user average, not capped into it.
        assert_eq!(insight.avg_user_response_time_ms, 0.0);
        assert_eq!(insight.avg_claude_response_time_ms, 60_000.0);
    }

    #[test]
    fn a_file_history_snapshot_is_not_a_step() {
        let insight = insight_of(&[
            user(json!("hello")),
            json!({"type": "file-history-snapshot", "snapshot": {"a": 1}}),
            assistant(json!([{"type": "text", "text": "hi"}])),
        ]);
        // Two events, one turn — the snapshot is skipped before any processor.
        assert_eq!(insight.turn_count, 1);
        assert_eq!(insight.steps_per_turn_avg, 2.0);
    }

    #[test]
    fn cache_hit_rate_uses_the_shared_definition() {
        let insight = insight_of(&[json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "x"}],
                        "usage": {"input_tokens": 100, "output_tokens": 20,
                                  "cache_read_input_tokens": 300,
                                  "cache_creation_input_tokens": 0}},
        })]);
        assert_eq!(insight.cache_hit_rate, 0.75);
        // No resolver wired, so nothing is priced.
        assert_eq!(insight.cost_estimate_usd, 0.0);
        assert_eq!(insight.tokens_per_turn_avg, 120.0);
    }

    #[test]
    fn sub_agent_transcripts_are_found_beside_the_parent_and_sorted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let parent = dir.path().join("abc.jsonl");
        std::fs::write(&parent, "").expect("write parent");
        let subs = dir.path().join("abc").join("subagents");
        std::fs::create_dir_all(&subs).expect("mkdir");
        for name in ["agent-2.jsonl", "agent-1.jsonl", "agent-1.meta.json"] {
            std::fs::write(subs.join(name), "").expect("write");
        }

        let files = session_files("abc", &parent);
        assert_eq!(files.len(), 3, "{files:?}");
        assert_eq!(files[0], parent);
        assert!(files[1].ends_with("agent-1.jsonl"), "{files:?}");
        assert!(files[2].ends_with("agent-2.jsonl"), "{files:?}");

        // A session that delegated nothing yields the parent alone.
        let lone = dir.path().join("none.jsonl");
        std::fs::write(&lone, "").expect("write");
        assert_eq!(session_files("none", &lone), vec![lone]);
    }
}
