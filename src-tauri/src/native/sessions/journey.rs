//! `GET /api/claude-sessions/{id}/journey` — one session as a turn-segmented
//! timeline, with each delegated sub-agent's steps nested under the `Task` call
//! that spawned it.
//!
//! Ported from the deleted `internal/claudesessions/journey.go`, last shaped by
//! #203's sub-agent nesting and #236's shared turn predicate. The route was
//! dropped at the port cut-over with `parity/read_routes.json` recording why —
//! *"no desktop view renders the journey timeline; add with the view if one is
//! built"* — and #479 is the view being built.
//!
//! **This is scanner work, not a SQLite read**, exactly as
//! [`super::detail`] is: the session's own JSONL is re-read per request, plus
//! every sub-agent transcript beside it. Only the sub-agent *labels* come from
//! the database.
//!
//! ## Four rules that are silent when broken
//!
//! 1. **Turn segmentation goes through
//!    [`transcript::is_user_turn_content`] and nothing else.** It is the one
//!    definition of a user turn, shared with the scanner's `message_count` and
//!    the insight pipeline's `turn_count`; #236 made the journey use it
//!    precisely so a fourth definition could not appear. Reimplementing the
//!    rule here would put a turn count on this page that disagrees with the one
//!    Insights reports for the same session, with nothing to say which is
//!    right.
//! 2. **A non-genuine user event still has its `tool_result` blocks
//!    attached**, and [`Builder::ensure_turn`] sits *inside* that loop. A
//!    transcript can open with a tool-result carrier — a resumed session, or one
//!    whose head was compacted away — and those steps must land somewhere;
//!    but an injected wrapper carries no blocks and must not conjure a turn out
//!    of nothing.
//! 3. **A sub-agent whose `tool_use` is not in the rendered transcript is
//!    appended to the last turn, never dropped.** Its spawning call may have
//!    been compacted away, or its sidecar may have carried no `toolUseId`; the
//!    session is still charged for the work either way, so the page has to show
//!    it.
//! 4. **Delegated timestamps are absorbed into the parent's active-time
//!    tracker** before `durations()` is read, so a 40-minute delegated run fills
//!    the parent's `Task` wait instead of collapsing to one capped gap.
//!
//!    **It does not make this agree with the sessions list, and the two figures
//!    are different questions rather than one of them being wrong.** Both are
//!    `Σ min(gap, idle_cap)` over consecutive stamps; what differs is the stamp
//!    set each sum is taken over. The list caps every transcript *on its own*
//!    (the scanner) and adds the results — `active_duration_ms +
//!    SUM(subagent active_duration_ms)`, via
//!    `summary.rs::total_active_duration_ms` — so a wall-clock minute in which
//!    the parent waits on a `Task` and its agent works is counted twice. This
//!    takes one sum over every stamp, so that minute is counted once, which is
//!    the honest reading of "how long was this session actually being worked
//!    on".
//!
//!    **Neither figure dominates the other, and the arithmetic is the reason.**
//!    It is tempting to call this a union of intervals and conclude it can never
//!    exceed the sum; it is not one. Absorbing a stamp *inside* a parent gap
//!    that was longer than the cap replaces one capped gap with two, so the
//!    merged total can come out **larger** — a sidecar holding a single logged
//!    event, whose own duration is therefore 0, is enough. So the relationship
//!    is stated and observed rather than asserted as an invariant: the fixture
//!    test `a_delegated_run_is_counted_once_here_and_twice_by_the_list` pins the
//!    concrete numbers for the ordinary shape, `tests/journey_live.rs` asserts
//!    only the sound bound (absorbing stamps never *lowers* the total) and
//!    counts the rest, and the view's tooltip names which figure it is showing.
//!
//!    One consequence worth knowing before reading a header: **`active_duration_ms`
//!    can exceed `total_duration_ms`.** `Builder::range` is widened only by the
//!    events the parent walk sees, and `build_subagent_steps` absorbs a
//!    sub-builder's tracker and deliberately not its range — which is Go's
//!    split, and moving `start_time`/`end_time` would change the wire. A sidecar
//!    stamped after the parent's last event therefore lands outside the span it
//!    is reported beside.
//!
//! ## Where this deliberately does not reproduce the Go
//!
//! Three places, each an improvement rather than a divergence to be fixed back:
//!
//! - **A `tool_result` written as an array of blocks is decoded.** Go decoded a
//!   carrier into `[]rawToolResultBlock`, whose `Content` is a `string`, so the
//!   whole `json.Unmarshal` failed for the array shape and the results were
//!   silently absent — the same class of gap #482 closed on the detail path.
//!   This goes through [`transcript::ContentBlock`] and
//!   [`transcript::extract_text_content`], which know both shapes.
//! - **The cache-write split is filled in.** Go's `accumulateUsage` copied four
//!   of `TokenUsage`'s six fields, leaving `cache_creation_5m_tokens` and
//!   `cache_creation_1h_tokens` at zero while the sessions list showed real
//!   figures for the same session. `Usage::split_cache_tiers` is what every
//!   other reader in the port already uses.
//! - **There is no 4 MiB line cap.** Go raised `bufio.Scanner`'s buffer to
//!   4 MiB because a transcript line can be large; `transcript::read` uses
//!   `BufReader::lines`, which has no line-length ceiling at all.
//!
//! And one thing that *is* reproduced although it reads as an oddity: only one
//! level of delegation nests. A sub-agent's own `subagents/` directory is not
//! walked, so a sub-sub-agent's steps are flattened into its parent's.
//!
//! ## What the re-read costs, measured
//!
//! A journey re-reads the whole transcript **plus every sub-agent's** on every
//! request, so it was measured before shipping one rather than argued about.
//! `tests/journey_live.rs` over the 60 heaviest sessions in the reference
//! corpus — the ones with the most delegated runs and the most messages, so a
//! deliberate worst case — built 31,012 steps at a **mean of 268 ms and a worst
//! case of 884 ms per journey, debug profile**. A release build is the number a
//! user sees and is faster; re-measure rather than quoting this one.
//!
//! That is a page open, not a poll, and the view fetches once per session. If it
//! ever needs to be faster the fix is a cap on step content — Go truncated a
//! `tool_result` to 2000 runes and `thinking` to 500/20000, which this keeps —
//! and **not** a cache: a cached journey is a fifth thing to invalidate when a
//! transcript grows, beside the scan's TTL, the pricing fingerprint, the idle
//! threshold and the processor version.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::value::RawValue;

use super::detail::TimeRange;
use super::summary::TokenUsage;
use crate::native::active_time::ActiveTimeTracker;
use crate::native::gotime::GoTime;
use crate::native::insights::{processors, transcript};
use crate::native::scanner::summary_file::truncate_chars;
use crate::native::{gojson, settings};

/// `truncateRunes(content, 200)` — the journey's own one-line summary.
const SUMMARY_MAX_CHARS: usize = 200;

/// How much of a tool result travels. The same cap
/// [`super::detail`] applies, and for the same reason: a `Bash` call can print
/// megabytes and this response already carries every step of a session.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// Thinking travels twice — a collapsed preview and the expanded body — because
/// the view shows the first until asked for the second.
const THINKING_PREVIEW_MAX_CHARS: usize = 500;
const THINKING_FULL_MAX_CHARS: usize = 20_000;

// ── The wire types ──────────────────────────────────────────────────────────
//
// Struct field order **is** the wire order, as everywhere else in `native/`.

/// The response body. Mirrors `claudesessions.SessionJourney`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Journey {
    pub session_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub git_branch: String,
    pub start_time: GoTime,
    pub end_time: GoTime,
    /// The raw start-to-end span. `active_duration_ms` caps every inter-event
    /// gap at the idle threshold and is what a header should show — a resumed
    /// session's span contains every idle day between sittings.
    pub total_duration_ms: i64,
    pub active_duration_ms: i64,
    pub total_turns: i64,
    /// Main-thread only, exactly as `ClaudeSessionSummary.usage` is.
    pub usage: TokenUsage,
    /// The summed usage of every sub-agent transcript this journey rendered.
    ///
    /// Reported *beside* `usage` rather than folded into it, because "this
    /// session's tokens" and "the model that spent them" are different
    /// questions — and because the header used to show `usage` alone while the
    /// sessions list showed the sum, so one session reported two different
    /// totals on two pages with nothing to explain the gap.
    pub subagent_usage: TokenUsage,
    pub subagent_count: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub summary: String,
    pub turns: Vec<JourneyTurn>,
}

/// One user→assistant interaction cycle. Mirrors `claudesessions.JourneyTurn`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct JourneyTurn {
    pub number: i64,
    pub start_time: GoTime,
    pub end_time: GoTime,
    pub duration_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    pub tool_calls: i64,
    /// Always an array, never `null`. Go marshalled a nil slice here as `null`
    /// and reached that case only for a turn opened by an assistant event with
    /// no message — a shape whose only consequence was that the page's
    /// `turn.steps.map(…)` threw. The narrowing is deliberate.
    pub steps: Vec<JourneyStep>,
}

/// One discrete event within a turn. Mirrors `claudesessions.JourneyStep`.
#[derive(Debug, Clone, Serialize)]
pub struct JourneyStep {
    #[serde(rename = "type")]
    pub step_type: String,
    pub timestamp: GoTime,
    #[serde(skip_serializing_if = "is_zero")]
    pub duration_ms: i64,
    /// The step's payload, kept **raw**.
    ///
    /// A `tool_call`'s `input` is the transcript's own bytes, so decoding this
    /// into a `serde_json::Value` anywhere on the way out would sort its keys
    /// and respell its numbers with nothing to signal it (#298). The rule lives
    /// on the field rather than at the construction sites, which is the other
    /// half of that lesson.
    #[serde(serialize_with = "crate::native::gojson::serialize_compacted")]
    pub data: Box<RawValue>,
    /// A sub-agent transcript's own steps, when this step spawned one. Only one
    /// level nests, so this is empty for every other step and adds nothing at
    /// all to a session that delegated no work.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<JourneyStep>,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

// ── The step payloads ───────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct UserInputData {
    content: String,
}

#[derive(Debug, Serialize)]
struct ThinkingData {
    preview: String,
    full: String,
}

#[derive(Debug, Serialize)]
struct TextResponseData {
    content: String,
}

#[derive(Debug, Serialize)]
struct ToolCallData {
    tool_use_id: String,
    tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Box<RawValue>>,
    /// Set only when this call spawned a sub-agent whose transcript is nested
    /// under the step (`steps`).
    #[serde(skip_serializing_if = "String::is_empty")]
    agent_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_usage: Option<TokenUsage>,
}

#[derive(Debug, Serialize)]
struct ToolResultData {
    tool_use_id: String,
    content: String,
    is_error: bool,
}

#[derive(Debug, Serialize)]
struct ThinkingDurationData {
    duration_ms: i64,
}

/// A delegated agent whose originating `tool_use` is **not** in the rendered
/// transcript, appended to its turn rather than dropped.
#[derive(Debug, Serialize)]
struct SubAgentData {
    #[serde(skip_serializing_if = "String::is_empty")]
    agent_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    agent_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<TokenUsage>,
}

/// Where the conversation was summarized to fit the context window. It reads as
/// a divider on the timeline: everything before it was summarized away, which is
/// usually the explanation for the sudden loss of context somebody came here to
/// find.
///
/// `dropped_tokens` is what **this** compaction dropped, derived from
/// pre/post — deliberately not `cumulativeDroppedTokens`, which is the
/// session's running total and is what `ClaudeSessionSummary.dropped_tokens`
/// reports.
#[derive(Debug, Serialize)]
struct CompactionData {
    #[serde(skip_serializing_if = "String::is_empty")]
    trigger: String,
    #[serde(skip_serializing_if = "is_zero")]
    pre_tokens: i64,
    #[serde(skip_serializing_if = "is_zero")]
    post_tokens: i64,
    #[serde(skip_serializing_if = "is_zero")]
    dropped_tokens: i64,
}

/// `json.Marshal` over a step payload, as Go's builder does before storing it in
/// a `json.RawMessage`.
///
/// A payload of ours cannot fail to encode — every field is a string, an `i64`
/// or a `RawValue` that came off a transcript — so the error arm answers `{}`
/// rather than failing a whole journey over one step.
fn payload(value: &impl Serialize) -> Box<RawValue> {
    let encoded = gojson::to_vec_marshal(value)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| RawValue::from_string(text).ok());
    match encoded {
        Some(raw) => raw,
        None => RawValue::from_string("{}".to_string()).expect("an empty object is valid JSON"),
    }
}

// ── The endpoint ────────────────────────────────────────────────────────────

/// Read one session's journey, or `None` when no config dir holds it.
///
/// `None` is also the answer for a transcript that yielded no timestamped event
/// at all — an empty file, or one whose every line was malformed. Go returned
/// `nil, nil` there and the handler turned it into its 404; there is nothing to
/// draw a timeline from either way.
///
/// **A transcript this process could not read is an `Err`, not a `None`.**
/// `find_session_file` has already established that the file exists, so the only
/// ways to fail past it are a permissions change, an unmounted drive or an I/O
/// error — and "we could not look" is not "it does not exist", which is the rule
/// the scanner's own delete pass is built on. Reporting it as the route's 404
/// would also have the two tabs of one session disagree about the same file at
/// the same instant, since `detail::get` propagates and answers 500. Only the
/// **parent** read is fatal; an unreadable sub-agent transcript contributes no
/// steps and is logged, which is what Go's `buildSubagentSteps` does.
pub fn get(db_path: &Path, session_id: &str) -> Result<Option<Journey>, String> {
    let conn = crate::native::db::open_read_only(db_path)?;
    let data_settings = settings::load(&conn);

    let Some((_, _, file)) =
        super::detail::find_session_file(&data_settings.indexed_config_dirs, session_id)
    else {
        return Ok(None);
    };

    let labels = subagent_labels(&conn, session_id);
    build(
        session_id,
        &file,
        data_settings.idle_gap_ms,
        &labels,
        &processors::subagent_files(session_id, &file),
    )
}

/// The `{agent_id → label}` index, read from `claude_subagent_cache`.
///
/// Go's `readSubagentMeta` re-read each `<agent-id>.meta.json` sidecar off disk
/// for `agentType`, `description` and `toolUseId`. The scanner already stores
/// all three, and [`super::detail`] already sources sub-agents from the cache
/// for exactly this reason, so this follows the port rather than the Go — at the
/// one cost that module already documents: a session the scanner has not reached
/// yet has no rows, so its delegations render unlabelled.
///
/// They still render. Keying the walk on the transcripts *on disk* rather than
/// on the rows means an unscanned session shows the delegated steps with no
/// agent type beside them, which is what keeps `subagent_count` equal to the
/// number of sub-agents the page draws.
fn subagent_labels(conn: &Connection, session_id: &str) -> HashMap<String, SubagentLabel> {
    super::detail::list_subagents(conn, session_id)
        .into_iter()
        .map(|s| {
            (
                s.agent_id,
                SubagentLabel {
                    agent_type: s.agent_type,
                    description: s.description,
                    tool_use_id: s.tool_use_id,
                },
            )
        })
        .collect()
}

/// What the cache knows about one delegated run that the transcript does not.
#[derive(Debug, Clone, Default)]
struct SubagentLabel {
    agent_type: String,
    description: String,
    tool_use_id: String,
}

/// One sub-agent transcript on disk, and the label the cache had for it.
#[derive(Debug, Clone)]
struct SubagentEntry {
    agent_id: String,
    agent_type: String,
    description: String,
    file: PathBuf,
    /// Whether a `tool_use` block has claimed it. An unclaimed entry is
    /// appended to the last turn by [`Builder::append_unmatched_subagents`].
    matched: bool,
}

/// `buildJourney`: the whole timeline from one transcript and its sub-agents'.
fn build(
    session_id: &str,
    file: &Path,
    idle_gap_ms: i64,
    labels: &HashMap<String, SubagentLabel>,
    subagent_files: &[PathBuf],
) -> Result<Option<Journey>, String> {
    let mut journey = Journey {
        session_id: session_id.to_string(),
        ..Default::default()
    };
    let mut builder = Builder::new(idle_gap_ms);
    builder.load_subagents(labels, subagent_files);
    builder.walk(file, &mut journey)?;
    builder.finalize(&mut journey);

    // Go: `if journey.StartTime.IsZero() { return nil, nil }`.
    Ok(builder.range.start.is_some().then_some(journey))
}

/// Accumulates state while scanning events.
struct Builder {
    range: TimeRange,
    /// Feeds `active_duration_ms`. Sub-agent builders' stamps are absorbed into
    /// this one, so delegated work fills the parent's `Task` wait gaps exactly
    /// as it does in the insight pipeline.
    active: ActiveTimeTracker,
    idle_gap_ms: i64,
    current: Option<JourneyTurn>,
    turns: Vec<JourneyTurn>,
    turn_number: i64,
    turn_usage: TokenUsage,
    turn_tool_calls: i64,
    /// Every sub-agent transcript, and the `tool_use` id → index over the ones
    /// whose sidecar recorded one. Both are empty for a session that delegated
    /// nothing.
    entries: Vec<SubagentEntry>,
    by_tool_use: HashMap<String, usize>,
    subagent_usage: TokenUsage,
    subagent_count: i64,
    /// Set while building a sub-agent's own steps: its transcript carries
    /// `isSidechain` on every event, which the parent builder skips.
    subagent_mode: bool,
}

impl Builder {
    fn new(idle_gap_ms: i64) -> Self {
        Builder {
            range: TimeRange::default(),
            active: ActiveTimeTracker::new(idle_gap_ms),
            idle_gap_ms,
            current: None,
            turns: Vec::new(),
            turn_number: 0,
            turn_usage: TokenUsage::default(),
            turn_tool_calls: 0,
            entries: Vec::new(),
            by_tool_use: HashMap::new(),
            subagent_usage: TokenUsage::default(),
            subagent_count: 0,
            subagent_mode: false,
        }
    }

    /// Index the session's delegated transcripts by the `tool_use` id that
    /// spawned each. On a collision the first seen wins, matching the cache's
    /// deterministic start-time ordering.
    fn load_subagents(&mut self, labels: &HashMap<String, SubagentLabel>, files: &[PathBuf]) {
        for file in files {
            let agent_id = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let label = labels.get(&agent_id).cloned().unwrap_or_default();
            self.entries.push(SubagentEntry {
                agent_id,
                agent_type: label.agent_type,
                description: label.description,
                file: file.clone(),
                matched: false,
            });
            if !label.tool_use_id.is_empty() {
                self.by_tool_use
                    .entry(label.tool_use_id)
                    .or_insert(self.entries.len() - 1);
            }
        }
    }

    /// Read one transcript into this builder.
    ///
    /// The error is propagated rather than logged: for the **parent** it is a
    /// 500, because the file's existence was already established and a failure
    /// past that point means this process could not read it. `walk_lenient` is
    /// the sub-agent arm.
    fn walk(&mut self, file: &Path, journey: &mut Journey) -> Result<(), String> {
        let events = transcript::read(file)?;
        for ev in &events {
            // `file-history-snapshot` events carry whole file contents and
            // nothing the journey uses. Skipped before the time range and the
            // active tracker see them, as Go skips them before `processEvent`.
            if ev.event_type == "file-history-snapshot" {
                continue;
            }
            self.process_event(ev, journey);
        }
        Ok(())
    }

    /// [`Self::walk`], with an unreadable file contributing nothing.
    ///
    /// This is the sub-agent arm, and it is Go's: `buildSubagentSteps` returns
    /// empty on an open failure rather than failing the whole journey, so one
    /// unreadable delegated transcript costs its own steps and not the page.
    fn walk_lenient(&mut self, file: &Path, journey: &mut Journey) {
        if let Err(e) = self.walk(file, journey) {
            log::warn!("native session journey: skipping a sub-agent transcript: {e}");
        }
    }

    fn process_event(&mut self, ev: &transcript::Event, journey: &mut Journey) {
        self.range.update(ev.timestamp);
        if let Some(ts) = ev.timestamp {
            self.active.observe(ts, ev.event_type == "assistant");
        }
        if journey.cwd.is_empty() && !ev.cwd.is_empty() {
            journey.cwd = ev.cwd.clone();
        }
        if journey.git_branch.is_empty() && !ev.git_branch.is_empty() {
            journey.git_branch = ev.git_branch.clone();
        }

        match ev.event_type.as_str() {
            "user" => self.process_user_event(ev, journey),
            "assistant" => self.process_assistant_event(ev, journey),
            "system" => self.process_system_event(ev),
            _ => {}
        }
    }

    fn start_new_turn(&mut self, ts: Option<DateTime<Utc>>) {
        if self.current.is_some() {
            self.close_turn();
        }
        self.turn_number += 1;
        self.current = Some(JourneyTurn {
            number: self.turn_number,
            start_time: go_time(ts),
            ..Default::default()
        });
        self.turn_usage = TokenUsage::default();
        self.turn_tool_calls = 0;
    }

    fn close_turn(&mut self) {
        let Some(mut turn) = self.current.take() else {
            return;
        };
        if let Some(last) = turn.steps.last() {
            if turn.end_time == GoTime::default()
                || last.timestamp.instant() > turn.end_time.instant()
            {
                turn.end_time = last.timestamp;
            }
        }
        if turn.end_time == GoTime::default() {
            turn.end_time = turn.start_time;
        }
        turn.duration_ms = millis_between(turn.start_time, turn.end_time);
        turn.tool_calls = self.turn_tool_calls;
        if self.turn_usage.input_tokens > 0 || self.turn_usage.output_tokens > 0 {
            turn.usage = Some(self.turn_usage.clone());
        }
        self.turns.push(turn);
    }

    /// Open a turn if none is open.
    ///
    /// **Load-bearing on the non-genuine user path**, where it sits inside the
    /// `tool_result` loop: a transcript can open with a tool-result carrier and
    /// those steps must land somewhere, while an injected wrapper carries no
    /// blocks and so reaches this not at all.
    fn ensure_turn(&mut self, ts: Option<DateTime<Utc>>) {
        if self.current.is_none() {
            self.start_new_turn(ts);
        }
    }

    fn add_step(&mut self, step: JourneyStep) {
        let Some(turn) = self.current.as_mut() else {
            return;
        };
        if step.timestamp.instant() > turn.end_time.instant() {
            turn.end_time = step.timestamp;
        }
        turn.steps.push(step);
    }

    fn process_user_event(&mut self, ev: &transcript::Event, journey: &mut Journey) {
        // In the parent transcript a sidechain user turn is an echo of a
        // delegated sub-agent, and is skipped — those are nested under the
        // `tool_use` that spawned them instead. `subagent_mode` is set only
        // while building a sub-agent's own steps, where *every* event carries
        // the flag and it therefore means nothing.
        if ev.is_sidechain && !self.subagent_mode {
            return;
        }

        // The shared predicate decides, so a journey's turns, the session's
        // `message_count` and the pipeline's `turn_count` all mean the same
        // thing. An event that is not genuine input is either a `tool_result`
        // carrier or one of Claude Code's injected wrappers: attach whatever
        // results it carries to the enclosing turn, and open no new one.
        let is_turn_start = ev
            .message
            .as_ref()
            .is_some_and(|m| transcript::is_user_turn_content(&m.content));
        if !is_turn_start {
            self.attach_tool_results(ev);
            return;
        }

        self.start_new_turn(ev.timestamp);
        let content = ev
            .message
            .as_ref()
            .map(|m| transcript::extract_text_content(&m.content))
            .unwrap_or_default();
        if journey.summary.is_empty() && !content.is_empty() {
            journey.summary = truncate_chars(&content, SUMMARY_MAX_CHARS);
        }
        let data = payload(&UserInputData { content });
        self.add_step(JourneyStep {
            step_type: "user_input".to_string(),
            timestamp: go_time(ev.timestamp),
            duration_ms: 0,
            data,
            steps: Vec::new(),
        });
    }

    /// Attach a user event's `tool_result` blocks as steps of the enclosing
    /// turn.
    ///
    /// Deliberately **not** the turn-start test — that is the shared predicate
    /// above. Because the decision no longer depends on this decode, every
    /// block is inspected rather than only the first: a carrier whose
    /// `tool_result` sits behind another block still has its results attached,
    /// and still opens no turn the session's own `message_count` does not count.
    fn attach_tool_results(&mut self, ev: &transcript::Event) {
        let Some(message) = &ev.message else {
            return;
        };
        for block in transcript::parse_content_blocks(&message.content) {
            if block.block_type != "tool_result" {
                continue;
            }
            self.ensure_turn(ev.timestamp);
            let data = payload(&ToolResultData {
                tool_use_id: block.tool_use_id,
                content: truncate_chars(
                    &transcript::extract_text_content(&block.content),
                    TOOL_RESULT_MAX_CHARS,
                ),
                is_error: block.is_error,
            });
            self.add_step(JourneyStep {
                step_type: "tool_result".to_string(),
                timestamp: go_time(ev.timestamp),
                duration_ms: 0,
                data,
                steps: Vec::new(),
            });
        }
    }

    fn process_assistant_event(&mut self, ev: &transcript::Event, journey: &mut Journey) {
        self.ensure_turn(ev.timestamp);

        let Some(message) = &ev.message else {
            return;
        };
        if journey.model.is_empty() && !message.model.is_empty() {
            journey.model = message.model.clone();
        }
        if let Some(usage) = &message.usage {
            let (five_min, one_hour) = usage.split_cache_tiers();
            let add = TokenUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_creation_tokens: usage.cache_creation_input_tokens,
                cache_creation_5m_tokens: five_min,
                cache_creation_1h_tokens: one_hour,
                cache_read_tokens: usage.cache_read_input_tokens,
            };
            add_usage(&mut self.turn_usage, &add);
            add_usage(&mut journey.usage, &add);
        }

        // The blocks are decoded from the message's own bytes, so a `tool_use`
        // input keeps the key order and number spelling it was written with.
        let Some(raw) = message.content_raw.as_deref() else {
            return;
        };
        for block in transcript::parse_content_blocks_raw(raw) {
            self.process_content_block(block, ev.timestamp);
        }
    }

    fn process_content_block(
        &mut self,
        block: transcript::ContentBlock,
        ts: Option<DateTime<Utc>>,
    ) {
        match block.block_type.as_str() {
            "thinking" => {
                if block.thinking.is_empty() {
                    return;
                }
                let data = payload(&ThinkingData {
                    preview: truncate_chars(&block.thinking, THINKING_PREVIEW_MAX_CHARS),
                    full: truncate_chars(&block.thinking, THINKING_FULL_MAX_CHARS),
                });
                self.add_step(JourneyStep {
                    step_type: "thinking".to_string(),
                    timestamp: go_time(ts),
                    duration_ms: 0,
                    data,
                    steps: Vec::new(),
                });
            }
            "text" => {
                if block.text.is_empty() {
                    return;
                }
                let data = payload(&TextResponseData {
                    content: block.text,
                });
                self.add_step(JourneyStep {
                    step_type: "text_response".to_string(),
                    timestamp: go_time(ts),
                    duration_ms: 0,
                    data,
                    steps: Vec::new(),
                });
            }
            "tool_use" => {
                self.turn_tool_calls += 1;
                let mut data = ToolCallData {
                    tool_use_id: block.id.clone(),
                    tool_name: block.name,
                    input: block.input,
                    agent_type: String::new(),
                    description: String::new(),
                    agent_usage: None,
                };
                // A `Task` call whose id matches a sub-agent transcript nests
                // that agent's own steps here, joined exactly on `toolUseId`.
                let mut nested = Vec::new();
                if let Some(&index) = self.by_tool_use.get(&block.id) {
                    self.entries[index].matched = true;
                    data.agent_type = self.entries[index].agent_type.clone();
                    data.description = self.entries[index].description.clone();
                    let (steps, usage) = self.build_subagent_steps(index);
                    nested = steps;
                    if usage.input_tokens > 0 || usage.output_tokens > 0 {
                        data.agent_usage = Some(usage);
                    }
                }
                let data = payload(&data);
                self.add_step(JourneyStep {
                    step_type: "tool_call".to_string(),
                    timestamp: go_time(ts),
                    duration_ms: 0,
                    data,
                    steps: nested,
                });
            }
            _ => {}
        }
    }

    /// Only `compact_boundary` and `turn_duration` are consumed; every other
    /// system subtype produces no step.
    fn process_system_event(&mut self, ev: &transcript::Event) {
        if ev.subtype == "compact_boundary" {
            self.add_compaction_step(ev);
            return;
        }
        if ev.subtype != "turn_duration" {
            return;
        }
        self.ensure_turn(ev.timestamp);
        let data = payload(&ThinkingDurationData {
            duration_ms: ev.duration_ms,
        });
        self.add_step(JourneyStep {
            step_type: "thinking_duration".to_string(),
            timestamp: go_time(ev.timestamp),
            duration_ms: ev.duration_ms,
            data,
            steps: Vec::new(),
        });
    }

    /// A compaction with no `compactMetadata` produces **no step** — the
    /// payload is what makes the step worth showing.
    fn add_compaction_step(&mut self, ev: &transcript::Event) {
        let Some(meta) = &ev.compact_metadata else {
            return;
        };
        self.ensure_turn(ev.timestamp);
        let data = payload(&CompactionData {
            trigger: meta.trigger.clone(),
            pre_tokens: meta.pre_tokens,
            post_tokens: meta.post_tokens,
            dropped_tokens: (meta.pre_tokens - meta.post_tokens).max(0),
        });
        self.add_step(JourneyStep {
            step_type: "compaction".to_string(),
            timestamp: go_time(ev.timestamp),
            duration_ms: meta.duration_ms,
            data,
            steps: Vec::new(),
        });
    }

    fn finalize(&mut self, journey: &mut Journey) {
        if self.current.is_some() {
            self.close_turn();
        }
        self.append_unmatched_subagents();

        journey.start_time = go_time(self.range.start);
        journey.end_time = go_time(self.range.last);
        if self.range.start.is_some() && self.range.last.is_some() {
            journey.total_duration_ms = millis_between(journey.start_time, journey.end_time);
        }
        journey.active_duration_ms = self.active.durations().0;
        journey.total_turns = self.turns.len() as i64;

        for turn in self.turns.iter_mut() {
            compute_step_durations(turn);
        }
        journey.turns = std::mem::take(&mut self.turns);
        journey.subagent_usage = self.subagent_usage.clone();
        journey.subagent_count = self.subagent_count;

        if journey.summary.is_empty() {
            journey.summary = first_user_input(&journey.turns);
        }
    }

    /// One journey pass over one sub-agent transcript, flattened across its own
    /// turns. It does not recurse into that agent's `subagents/` — deeper
    /// delegation is flattened, as it was in Go.
    ///
    /// The tally lives here rather than at the two call sites because this is
    /// the one place a sub-agent's usage is computed, and both the matched
    /// (nested under its `Task` call) and unmatched (appended to the last turn)
    /// paths reach it. Its timestamps are absorbed into the parent's tracker for
    /// the same reason.
    fn build_subagent_steps(&mut self, index: usize) -> (Vec<JourneyStep>, TokenUsage) {
        let file = self.entries[index].file.clone();
        let mut sub_journey = Journey::default();
        let mut sub = Builder::new(self.idle_gap_ms);
        sub.subagent_mode = true;
        sub.walk_lenient(&file, &mut sub_journey);
        sub.finalize(&mut sub_journey);

        let steps: Vec<JourneyStep> = sub_journey
            .turns
            .into_iter()
            .flat_map(|t| t.steps)
            .collect();

        self.active.absorb(&sub.active);
        self.subagent_count += 1;
        add_usage(&mut self.subagent_usage, &sub_journey.usage);
        (steps, sub_journey.usage)
    }

    /// Surface sub-agents whose `tool_use` is not in the rendered transcript,
    /// attaching them to the last turn so delegated work is never silently lost.
    /// With no turns to attach to there is nowhere to put them.
    fn append_unmatched_subagents(&mut self) {
        if self.entries.is_empty() || self.turns.is_empty() {
            return;
        }
        let unmatched: Vec<usize> = (0..self.entries.len())
            .filter(|&i| !self.entries[i].matched)
            .collect();
        for index in unmatched {
            let (steps, usage) = self.build_subagent_steps(index);
            let entry = &self.entries[index];
            let data = payload(&SubAgentData {
                agent_id: entry.agent_id.clone(),
                agent_type: entry.agent_type.clone(),
                description: entry.description.clone(),
                usage: (usage.input_tokens > 0 || usage.output_tokens > 0).then_some(usage),
            });
            let last = self.turns.last_mut().expect("checked non-empty");
            let timestamp = steps.first().map(|s| s.timestamp).unwrap_or(last.end_time);
            last.steps.push(JourneyStep {
                step_type: "sub_agent".to_string(),
                timestamp,
                duration_ms: 0,
                data,
                steps,
            });
        }
    }
}

/// The first `user_input` step's text, when nothing genuine seeded the summary
/// during the walk.
fn first_user_input(turns: &[JourneyTurn]) -> String {
    let Some(first) = turns.first() else {
        return String::new();
    };
    for step in &first.steps {
        if step.step_type != "user_input" {
            continue;
        }
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(step.data.get()) {
            if let Some(content) = data.get("content").and_then(|c| c.as_str()) {
                return truncate_chars(content, SUMMARY_MAX_CHARS);
            }
        }
    }
    String::new()
}

/// Each step's duration is the gap to the next one, and the last step's is the
/// gap to the turn's end. Both are floored at zero, because a transcript's
/// timestamps can go backwards.
///
/// A step that already carries a duration is skipped — `thinking_duration` and
/// `compaction` state their own, and it is the event's figure rather than an
/// estimate from the gap.
fn compute_step_durations(turn: &mut JourneyTurn) {
    let end = turn.end_time;
    let count = turn.steps.len();
    for i in 0..count {
        if turn.steps[i].duration_ms > 0 {
            continue;
        }
        let until = if i + 1 < count {
            turn.steps[i + 1].timestamp
        } else {
            end
        };
        turn.steps[i].duration_ms = millis_between(turn.steps[i].timestamp, until).max(0);
    }
}

fn add_usage(into: &mut TokenUsage, add: &TokenUsage) {
    into.input_tokens += add.input_tokens;
    into.output_tokens += add.output_tokens;
    into.cache_creation_tokens += add.cache_creation_tokens;
    into.cache_creation_5m_tokens += add.cache_creation_5m_tokens;
    into.cache_creation_1h_tokens += add.cache_creation_1h_tokens;
    into.cache_read_tokens += add.cache_read_tokens;
}

fn millis_between(from: GoTime, to: GoTime) -> i64 {
    (to.instant() - from.instant()).num_milliseconds()
}

/// A transcript timestamp as it travels, or the zero `time.Time` when absent.
fn go_time(at: Option<DateTime<Utc>>) -> GoTime {
    at.map(|t| GoTime(t.fixed_offset())).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SESSION: &str = "sess-479";
    const IDLE_GAP_MS: i64 = 10 * 60 * 1000;

    /// `t0 + seconds`, as RFC 3339 text a transcript would carry.
    fn at(seconds: i64) -> String {
        format!("2026-01-01T10:00:{seconds:02}Z")
    }

    fn user_event(uuid: &str, seconds: i64, content: serde_json::Value) -> String {
        json!({
            "type": "user", "uuid": uuid, "sessionId": SESSION,
            "timestamp": at(seconds),
            "message": {"role": "user", "content": content},
        })
        .to_string()
    }

    fn assistant_event(uuid: &str, seconds: i64, blocks: serde_json::Value) -> String {
        json!({
            "type": "assistant", "uuid": uuid, "sessionId": SESSION,
            "timestamp": at(seconds),
            "message": {
                "role": "assistant", "model": "claude-sonnet-4-6", "content": blocks,
                "usage": {
                    "input_tokens": 100, "output_tokens": 50,
                    "cache_creation_input_tokens": 20, "cache_read_input_tokens": 80,
                },
            },
        })
        .to_string()
    }

    fn tool_result_event(uuid: &str, seconds: i64, id: &str, text: &str, err: bool) -> String {
        user_event(
            uuid,
            seconds,
            json!([{"type": "tool_result", "tool_use_id": id, "content": text, "is_error": err}]),
        )
    }

    /// One session's transcript, plus any sub-agent transcripts, laid out the
    /// way Claude Code writes them: `<project>/<id>.jsonl` and
    /// `<project>/<id>/subagents/<agent>.jsonl`.
    struct Corpus {
        dir: tempfile::TempDir,
        parent: PathBuf,
    }

    impl Corpus {
        fn new(lines: &[String], subagents: &[(&str, Vec<String>)]) -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let project = dir.path().join("projects").join("-home-u-proj");
            std::fs::create_dir_all(&project).expect("project dir");
            let parent = project.join(format!("{SESSION}.jsonl"));
            std::fs::write(&parent, lines.join("\n")).expect("transcript");
            if !subagents.is_empty() {
                let sub_dir = project.join(SESSION).join("subagents");
                std::fs::create_dir_all(&sub_dir).expect("subagents dir");
                for (agent_id, agent_lines) in subagents {
                    std::fs::write(
                        sub_dir.join(format!("{agent_id}.jsonl")),
                        agent_lines.join("\n"),
                    )
                    .expect("sub-agent transcript");
                }
            }
            Corpus { dir, parent }
        }

        fn build(&self, labels: &[(&str, &str, &str, &str)]) -> Option<Journey> {
            let index: HashMap<String, SubagentLabel> = labels
                .iter()
                .map(|(agent_id, agent_type, description, tool_use_id)| {
                    (
                        (*agent_id).to_string(),
                        SubagentLabel {
                            agent_type: (*agent_type).to_string(),
                            description: (*description).to_string(),
                            tool_use_id: (*tool_use_id).to_string(),
                        },
                    )
                })
                .collect();
            super::build(
                SESSION,
                &self.parent,
                IDLE_GAP_MS,
                &index,
                &processors::subagent_files(SESSION, &self.parent),
            )
            .expect("the fixture transcript is readable")
        }

        /// The insight pipeline's own reading of the same transcript.
        fn turn_count(&self) -> i64 {
            let _ = &self.dir;
            processors::run(
                SESSION,
                std::slice::from_ref(&self.parent),
                &processors::Ctx {
                    idle_gap_ms: IDLE_GAP_MS,
                    resolver: None,
                },
                &mut crate::native::insights::index::DocAccumulator::new(),
            )
            .expect("insight")
            .turn_count
        }
    }

    fn step_types(turn: &JourneyTurn) -> Vec<&str> {
        turn.steps.iter().map(|s| s.step_type.as_str()).collect()
    }

    fn data_of(step: &JourneyStep) -> serde_json::Value {
        serde_json::from_str(step.data.get()).expect("step data is JSON")
    }

    /// A sidechain event as written to a sub-agent's own transcript, where
    /// **every** event carries the flag.
    fn sidechain(kind: &str, uuid: &str, seconds: i64, blocks: serde_json::Value) -> String {
        let message = if kind == "assistant" {
            json!({
                "role": "assistant", "content": blocks,
                "usage": {"input_tokens": 40, "output_tokens": 20},
            })
        } else {
            json!({"role": "user", "content": "delegated task"})
        };
        json!({
            "type": kind, "uuid": uuid, "sessionId": SESSION,
            "timestamp": at(seconds), "isSidechain": true, "message": message,
        })
        .to_string()
    }

    #[test]
    fn the_basic_flow_becomes_one_turn_of_five_steps() {
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Please read main.go")),
                assistant_event(
                    "a1",
                    2,
                    json!([
                        {"type": "text", "text": "Sure, let me read it."},
                        {"type": "tool_use", "id": "tool1", "name": "Read", "input": {"path": "main.go"}},
                    ]),
                ),
                tool_result_event("u2", 5, "tool1", "package main", false),
                assistant_event("a2", 8, json!([{"type": "text", "text": "Here it is."}])),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");

        assert_eq!(j.total_turns, 1);
        assert_eq!(j.model, "claude-sonnet-4-6");
        assert_eq!(j.turns[0].tool_calls, 1);
        assert_eq!(
            step_types(&j.turns[0]),
            vec![
                "user_input",
                "text_response",
                "tool_call",
                "tool_result",
                "text_response"
            ]
        );
        // Two assistant events, each carrying the same usage.
        assert_eq!(j.usage.input_tokens, 200);
        assert_eq!(j.usage.output_tokens, 100);
        // Go's `accumulateUsage` left the cache-write split at zero; every
        // other reader in the port fills it, and so does this one.
        assert_eq!(j.usage.cache_creation_tokens, 40);
        assert_eq!(j.usage.cache_creation_5m_tokens, 40);
        assert_eq!(j.usage.cache_creation_1h_tokens, 0);
        assert_eq!(
            j.turns[0].usage.as_ref().expect("turn usage").output_tokens,
            100
        );
    }

    #[test]
    fn a_second_genuine_prompt_opens_a_second_turn() {
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("First message")),
                assistant_event("a1", 2, json!([{"type": "text", "text": "First"}])),
                user_event("u2", 5, json!("Second message")),
                assistant_event("a2", 8, json!([{"type": "text", "text": "Second"}])),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(j.total_turns, 2);
        assert_eq!(j.turns[1].number, 2);
    }

    #[test]
    fn a_transcript_with_no_timestamped_event_has_no_journey() {
        // An empty file, and one whose every line is unparseable, are the same
        // answer — the handler's 404. There is nothing to draw a timeline from.
        assert!(Corpus::new(&[], &[]).build(&[]).is_none());
        assert!(Corpus::new(&["not json at all {{{".to_string()], &[])
            .build(&[])
            .is_none());
    }

    #[test]
    fn a_transcript_this_process_cannot_read_is_an_error_not_a_missing_session() {
        // "We could not look" is not "it does not exist": `find_session_file`
        // has already established the path is there, so a failure past it is a
        // permissions change or an I/O error — a 500, the way `detail::get`
        // answers for the same file. Answering the route's 404 instead would
        // have the two tabs of one session disagree about it at one instant.
        //
        // A directory where the transcript should be, rather than a `chmod 000`
        // file, so the test still fails for the right reason as root.
        let dir = tempfile::tempdir().expect("temp dir");
        let project = dir.path().join("projects").join("-home-u-proj");
        std::fs::create_dir_all(project.join(format!("{SESSION}.jsonl"))).expect("not a file");
        let root = dir.path().to_string_lossy().into_owned();
        let (_, _, file) = super::super::detail::find_session_file(&[root], SESSION)
            .expect("the path exists, so it is found");

        let err = super::build(SESSION, &file, IDLE_GAP_MS, &HashMap::new(), &[])
            .expect_err("an unreadable transcript must not read as a missing session");
        assert!(err.contains("transcript"), "unexpected error: {err}");
    }

    #[test]
    fn an_unreadable_subagent_transcript_costs_only_its_own_steps() {
        // Go's `buildSubagentSteps` returns empty on an open failure rather than
        // failing the journey, and the parent's own read is the only fatal one.
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Do it")),
                assistant_event(
                    "a1",
                    2,
                    json!([{"type": "tool_use", "id": "toolu_1", "name": "Task", "input": {}}]),
                ),
            ],
            &[],
        );
        // A real file `subagent_files` will list — it filters directories out —
        // that this process then cannot open.
        let sub_dir = corpus
            .parent
            .parent()
            .expect("project dir")
            .join(SESSION)
            .join("subagents");
        std::fs::create_dir_all(&sub_dir).expect("subagents dir");
        let unreadable = sub_dir.join("agent-x.jsonl");
        std::fs::write(&unreadable, "{}").expect("write");
        std::fs::set_permissions(
            &unreadable,
            std::os::unix::fs::PermissionsExt::from_mode(0o000),
        )
        .expect("chmod");
        if transcript::read(&unreadable).is_ok() {
            // Running as root, where a mode of 000 is not a barrier.
            eprintln!("skipping: this process can read a 0o000 file");
            return;
        }

        let j = corpus
            .build(&[("agent-x", "Explore", "explore", "toolu_1")])
            .expect("the parent is readable, so the journey still builds");
        assert_eq!(j.total_turns, 1);
        let call = j.turns[0]
            .steps
            .iter()
            .find(|s| s.step_type == "tool_call")
            .expect("tool_call");
        // Its identity still comes from the cache; only the steps are missing.
        assert_eq!(data_of(call)["agent_type"], "Explore");
        assert!(call.steps.is_empty());
        assert_eq!(j.subagent_count, 1);
    }

    #[test]
    fn a_malformed_line_is_skipped_and_the_rest_still_builds() {
        let corpus = Corpus::new(
            &[
                "not json at all {{{".to_string(),
                user_event("u1", 0, json!("Hello")),
                "also bad }}}".to_string(),
                assistant_event("a1", 2, json!([{"type": "text", "text": "Hi"}])),
            ],
            &[],
        );
        assert_eq!(corpus.build(&[]).expect("journey").total_turns, 1);
    }

    #[test]
    fn a_file_history_snapshot_contributes_nothing() {
        let snapshot =
            json!({"type": "file-history-snapshot", "messageId": "s", "snapshot": {}}).to_string();
        let corpus = Corpus::new(
            &[
                snapshot.clone(),
                user_event("u1", 0, json!("Do something")),
                snapshot.clone(),
                assistant_event("a1", 2, json!([{"type": "text", "text": "Done"}])),
                snapshot,
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(j.total_turns, 1);
        assert_eq!(step_types(&j.turns[0]), vec!["user_input", "text_response"]);
    }

    #[test]
    fn thinking_travels_as_a_preview_and_a_capped_body() {
        let long = "a".repeat(25_000);
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Think hard")),
                assistant_event("a1", 2, json!([{"type": "thinking", "thinking": long}])),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        let data = data_of(&j.turns[0].steps[1]);
        // The cap plus `truncate_chars`' one ellipsis.
        assert_eq!(data["preview"].as_str().unwrap().chars().count(), 501);
        assert_eq!(data["full"].as_str().unwrap().chars().count(), 20_001);
    }

    #[test]
    fn an_empty_thinking_or_text_block_produces_no_step() {
        // Redacted thinking is an empty block plus a signature: nothing to show.
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Go")),
                assistant_event(
                    "a1",
                    2,
                    json!([{"type": "thinking", "thinking": ""}, {"type": "text", "text": ""}]),
                ),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(step_types(&j.turns[0]), vec!["user_input"]);
    }

    #[test]
    fn a_negative_gap_floors_the_step_duration_at_zero() {
        let mut turn = JourneyTurn {
            start_time: go_time(Some(instant(0))),
            end_time: go_time(Some(instant(5))),
            ..Default::default()
        };
        turn.steps = vec![
            step_at("user_input", 5),
            // Backwards, as a malformed transcript can be.
            step_at("text_response", 0),
        ];
        compute_step_durations(&mut turn);
        assert_eq!(turn.steps[0].duration_ms, 0);
        assert_eq!(turn.steps[1].duration_ms, 5_000);
    }

    #[test]
    fn the_last_step_runs_to_the_turns_end() {
        let mut turn = JourneyTurn {
            start_time: go_time(Some(instant(0))),
            end_time: go_time(Some(instant(10))),
            steps: vec![step_at("user_input", 0)],
            ..Default::default()
        };
        compute_step_durations(&mut turn);
        assert_eq!(turn.steps[0].duration_ms, 10_000);
    }

    #[test]
    fn a_step_that_states_its_own_duration_keeps_it() {
        let mut turn = JourneyTurn {
            start_time: go_time(Some(instant(0))),
            end_time: go_time(Some(instant(10))),
            steps: vec![
                JourneyStep {
                    duration_ms: 145_993,
                    ..step_at("compaction", 0)
                },
                step_at("text_response", 2),
            ],
            ..Default::default()
        };
        compute_step_durations(&mut turn);
        assert_eq!(turn.steps[0].duration_ms, 145_993);
        assert_eq!(turn.steps[1].duration_ms, 8_000);
    }

    fn instant(seconds: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&at(seconds))
            .expect("timestamp")
            .with_timezone(&Utc)
    }

    fn step_at(step_type: &str, seconds: i64) -> JourneyStep {
        JourneyStep {
            step_type: step_type.to_string(),
            timestamp: go_time(Some(instant(seconds))),
            duration_ms: 0,
            data: payload(&json!({})),
            steps: Vec::new(),
        }
    }

    #[test]
    fn an_injected_wrapper_opens_no_turn_and_never_seeds_the_summary() {
        for wrapper in [
            "<task-notification>\n<status>completed</status>\n</task-notification>",
            "<command-message>lab-workflow:github-issue-to-pr</command-message>",
            "<command-name>/review-pr</command-name>",
            "<local-command-caveat>Caveat: the messages below</local-command-caveat>",
            "<local-command-stdout>(no content)</local-command-stdout>",
            "<system-reminder>\nThe user named this session\n</system-reminder>",
        ] {
            let corpus = Corpus::new(&[user_event("u1", 0, json!(wrapper))], &[]);
            // Still a journey: the wrapper is an event with a timestamp.
            let j = corpus.build(&[]).expect("journey");
            assert_eq!(j.total_turns, 0, "{wrapper} opened a turn nobody typed");
            assert!(j.turns.is_empty());
            assert_eq!(j.summary, "", "{wrapper} seeded the summary");
        }
    }

    #[test]
    fn a_tool_result_behind_another_block_is_still_a_carrier() {
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Please read main.go")),
                assistant_event(
                    "a1",
                    2,
                    json!([{"type": "tool_use", "id": "tool1", "name": "Read", "input": {}}]),
                ),
                // The `tool_result` is the SECOND block, behind a text block.
                user_event(
                    "u2",
                    5,
                    json!([
                        {"type": "text", "text": "here you go"},
                        {"type": "tool_result", "tool_use_id": "tool1", "content": "package main"},
                    ]),
                ),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(
            j.total_turns, 1,
            "a carrier is a carrier at any block position"
        );
        assert_eq!(
            step_types(&j.turns[0]),
            vec!["user_input", "tool_call", "tool_result"]
        );
    }

    #[test]
    fn a_carrier_as_the_first_event_still_gets_a_turn() {
        let corpus = Corpus::new(
            &[
                tool_result_event("u1", 0, "tool1", "orphan output", true),
                assistant_event("a1", 2, json!([{"type": "text", "text": "carrying on"}])),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(j.total_turns, 1, "the orphan steps need a turn to live in");
        assert_eq!(
            step_types(&j.turns[0]),
            vec!["tool_result", "text_response"]
        );
        let data = data_of(&j.turns[0].steps[0]);
        assert_eq!(data["tool_use_id"], "tool1");
        assert_eq!(data["content"], "orphan output");
        assert_eq!(data["is_error"], true);
    }

    #[test]
    fn an_array_shaped_tool_result_still_carries_its_text_and_error_flag() {
        // The one place this builder deliberately does more than the Go did:
        // Go decoded a carrier into a struct whose `Content` was a `string`, so
        // the whole array failed to decode and the result was silently absent.
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("run the tests")),
                assistant_event(
                    "a1",
                    2,
                    json!([{"type": "tool_use", "id": "t2", "name": "Bash", "input": {}}]),
                ),
                user_event(
                    "u2",
                    5,
                    json!([{
                        "type": "tool_result", "tool_use_id": "t2", "is_error": true,
                        "content": [{"type": "text", "text": "2 tests failed"}],
                    }]),
                ),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(
            step_types(&j.turns[0]),
            vec!["user_input", "tool_call", "tool_result"]
        );
        let data = data_of(&j.turns[0].steps[2]);
        assert_eq!(data["content"], "2 tests failed");
        assert_eq!(data["is_error"], true);
    }

    #[test]
    fn the_summary_comes_from_the_first_genuine_turn() {
        let corpus = Corpus::new(
            &[
                user_event(
                    "u1",
                    0,
                    json!("<command-message>review-pr</command-message>"),
                ),
                user_event("u2", 1, json!("review PR 42 for me")),
                assistant_event(
                    "a1",
                    2,
                    json!([{"type": "tool_use", "id": "tool1", "name": "Read", "input": {}}]),
                ),
                tool_result_event("u3", 5, "tool1", "contents", false),
                user_event(
                    "u4",
                    8,
                    json!("<task-notification>done</task-notification>"),
                ),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(
            j.total_turns, 1,
            "only one event here was typed by a person"
        );
        assert_eq!(j.summary, "review PR 42 for me");
        assert_eq!(
            step_types(&j.turns[0]),
            vec!["user_input", "tool_call", "tool_result"]
        );
    }

    #[test]
    fn only_compact_boundary_and_turn_duration_become_steps() {
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Hello")),
                json!({"type": "system", "subtype": "away_summary", "uuid": "s1",
                       "timestamp": at(2), "content": "You were away"})
                .to_string(),
                json!({"type": "system", "subtype": "local_command", "uuid": "s2",
                       "timestamp": at(3), "content": "/clear"})
                .to_string(),
                json!({"type": "system", "subtype": "turn_duration", "uuid": "s3",
                       "timestamp": at(4), "durationMs": 4_200})
                .to_string(),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(
            step_types(&j.turns[0]),
            vec!["user_input", "thinking_duration"]
        );
        let step = &j.turns[0].steps[1];
        assert_eq!(step.duration_ms, 4_200);
        assert_eq!(data_of(step)["duration_ms"], 4_200);
    }

    #[test]
    fn a_compaction_step_reports_its_own_drop_not_the_running_total() {
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Keep going")),
                json!({
                    "type": "system", "subtype": "compact_boundary", "uuid": "s1",
                    "timestamp": at(2),
                    "compactMetadata": {
                        "trigger": "auto", "preTokens": 166_513, "postTokens": 29_504,
                        // Deliberately not preTokens-postTokens.
                        "cumulativeDroppedTokens": 900_000, "durationMs": 145_993,
                    },
                })
                .to_string(),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        let step = &j.turns[0].steps[1];
        assert_eq!(step.step_type, "compaction");
        assert_eq!(step.duration_ms, 145_993);
        let data = data_of(step);
        assert_eq!(data["trigger"], "auto");
        assert_eq!(data["pre_tokens"], 166_513);
        assert_eq!(data["post_tokens"], 29_504);
        assert_eq!(
            data["dropped_tokens"], 137_009,
            "a step describes itself, not the session"
        );
    }

    #[test]
    fn a_compact_boundary_with_no_metadata_produces_no_step() {
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Hello")),
                json!({"type": "system", "subtype": "compact_boundary", "uuid": "s1",
                       "timestamp": at(2)})
                .to_string(),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(step_types(&j.turns[0]), vec!["user_input"]);
    }

    #[test]
    fn a_subagent_nests_under_the_tool_call_that_spawned_it() {
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Explore the repo")),
                assistant_event(
                    "a1",
                    2,
                    json!([{"type": "tool_use", "id": "toolu_1", "name": "Task",
                            "input": {"description": "explore"}}]),
                ),
            ],
            &[(
                "agent-x",
                vec![
                    sidechain("user", "su1", 3, json!(null)),
                    sidechain(
                        "assistant",
                        "sa1",
                        4,
                        json!([
                            {"type": "text", "text": "agent working"},
                            {"type": "tool_use", "id": "st1", "name": "Read", "input": {}},
                        ]),
                    ),
                ],
            )],
        );
        let j = corpus
            .build(&[("agent-x", "general-purpose", "explore the repo", "toolu_1")])
            .expect("journey");

        assert_eq!(j.total_turns, 1);
        assert!(
            !step_types(&j.turns[0]).contains(&"sub_agent"),
            "a matched sub-agent must not also leak to the top level"
        );
        let call = j.turns[0]
            .steps
            .iter()
            .find(|s| s.step_type == "tool_call")
            .expect("tool_call");
        let data = data_of(call);
        assert_eq!(data["agent_type"], "general-purpose");
        assert_eq!(data["description"], "explore the repo");
        assert_eq!(data["agent_usage"]["input_tokens"], 40);
        assert_eq!(data["agent_usage"]["output_tokens"], 20);
        // The sidechain guard is defeated inside a sub-agent's own transcript,
        // where every event carries the flag.
        assert_eq!(
            call.steps
                .iter()
                .map(|s| s.step_type.as_str())
                .collect::<Vec<_>>(),
            vec!["user_input", "text_response", "tool_call"]
        );
        assert_eq!(j.subagent_count, 1);
        assert_eq!(j.subagent_usage.input_tokens, 40);
        assert_eq!(j.subagent_usage.output_tokens, 20);
        // Reported separately, never folded in.
        assert_eq!(j.usage.input_tokens, 100);
    }

    #[test]
    fn an_unmatched_subagent_is_appended_to_the_last_turn() {
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Do it")),
                assistant_event(
                    "a1",
                    2,
                    json!([{"type": "text", "text": "the task result is hidden"}]),
                ),
            ],
            &[(
                "agent-y",
                vec![sidechain(
                    "assistant",
                    "oa1",
                    3,
                    json!([{"type": "text", "text": "orphan work"}]),
                )],
            )],
        );
        // The label names a `tool_use` that is not in the rendered transcript.
        let j = corpus
            .build(&[("agent-y", "general-purpose", "orphan task", "toolu_gone")])
            .expect("journey");

        let last = j.turns[0].steps.last().expect("a step");
        assert_eq!(last.step_type, "sub_agent");
        let data = data_of(last);
        assert_eq!(data["agent_id"], "agent-y");
        assert_eq!(data["description"], "orphan task");
        assert_eq!(data["usage"]["output_tokens"], 20);
        assert!(!last.steps.is_empty(), "it must carry its own steps");
        assert_eq!(j.subagent_count, 1);
    }

    #[test]
    fn a_subagent_with_no_cache_row_is_still_rendered_unlabelled() {
        // The asymmetry `detail.rs` already documents: a session the scanner has
        // not reached has no rows, so there is no label — but the delegated work
        // is on disk and is still shown, which is what keeps `subagent_count`
        // equal to the number of sub-agents the page draws.
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Do it")),
                assistant_event("a1", 2, json!([{"type": "text", "text": "delegating"}])),
            ],
            &[(
                "agent-z",
                vec![sidechain(
                    "assistant",
                    "oa1",
                    3,
                    json!([{"type": "text", "text": "unlabelled work"}]),
                )],
            )],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(j.subagent_count, 1);
        let last = j.turns[0].steps.last().expect("a step");
        assert_eq!(last.step_type, "sub_agent");
        let data = data_of(last);
        assert_eq!(data["agent_id"], "agent-z");
        assert!(data.get("agent_type").is_none());
    }

    #[test]
    fn a_session_with_no_subagents_carries_no_nesting_at_all() {
        let corpus = Corpus::new(
            &[
                user_event("u1", 0, json!("Read a file")),
                assistant_event(
                    "a1",
                    2,
                    json!([
                        {"type": "text", "text": "reading"},
                        {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {}},
                    ]),
                ),
                tool_result_event("u2", 5, "toolu_1", "contents", false),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(j.total_turns, 1);
        assert_eq!(j.subagent_count, 0);
        assert_eq!(
            step_types(&j.turns[0]),
            vec!["user_input", "text_response", "tool_call", "tool_result"]
        );
        for step in &j.turns[0].steps {
            assert!(step.steps.is_empty(), "{} nested something", step.step_type);
        }
        // No agent identity may leak onto an ordinary tool_call.
        let data = data_of(&j.turns[0].steps[2]);
        assert!(data.get("agent_type").is_none());
        assert!(data.get("description").is_none());
        assert!(data.get("agent_usage").is_none());
    }

    #[test]
    fn the_turn_count_agrees_with_the_insight_pipeline() {
        // #236's anti-drift check: both resolve "is this a turn?" through the
        // one `is_user_turn_content`, so a fourth definition cannot appear
        // quietly. The fixture opens with genuine input — a transcript opening
        // with a carrier is the one structural difference between the two paths
        // (`ensure_turn` gives those orphan steps somewhere to live while the
        // pipeline counts no turn) and has its own test above.
        let corpus = Corpus::new(
            &[
                user_event("u0", 0, json!("<command-name>/review-pr</command-name>")),
                user_event("u1", 1, json!("first prompt")),
                assistant_event("a1", 2, json!([{"type": "text", "text": "on it"}])),
                assistant_event(
                    "a2",
                    3,
                    json!([{"type": "tool_use", "id": "tu", "name": "Read", "input": {}}]),
                ),
                tool_result_event("u2", 4, "tu", "body", false),
                user_event(
                    "u3",
                    5,
                    json!("<task-notification>done</task-notification>"),
                ),
                user_event("u4", 6, json!("second prompt")),
                assistant_event("a3", 7, json!([{"type": "text", "text": "done"}])),
            ],
            &[],
        );
        let j = corpus.build(&[]).expect("journey");
        assert_eq!(
            j.turns.len() as i64,
            corpus.turn_count(),
            "the journey has drifted from the shared is_user_turn_content predicate"
        );
        assert_eq!(j.total_turns, j.turns.len() as i64);
    }

    /// The relationship between this figure and the sessions list's, pinned
    /// rather than left to prose — the claim it replaces was simply wrong.
    ///
    /// A `Task` is concurrent: the parent is parked while its agent works. The
    /// scanner caps each transcript alone and the list adds the parent's figure
    /// to `SUM(active_duration_ms)` over the sub-agent rows, so that minute is
    /// counted **twice** there; this is one tracker over every stamp, so it is
    /// counted **once**. Both bounds matter: without `absorb` the union
    /// collapses to the parent's own figure and the lower bound fails, and if
    /// the union ever reached the sum the upper bound fails.
    #[test]
    fn a_delegated_run_is_counted_once_here_and_twice_by_the_list() {
        let (parent_lines, delegated) = delegating_fixture();
        let delegated_only = Corpus::new(&delegated, &[])
            .build(&[])
            .expect("the delegated transcript is a journey of its own");
        let parent_only = Corpus::new(&parent_lines, &[]).build(&[]).expect("journey");
        let merged = Corpus::new(&parent_lines, &[("agent-x", delegated)])
            .build(&[("agent-x", "", "", "toolu_1")])
            .expect("journey");

        // What the sessions list would report for this session: each transcript
        // capped on its own, then added.
        let list = parent_only.active_duration_ms + delegated_only.active_duration_ms;
        assert_eq!(parent_only.active_duration_ms, (1 + 10) * 60_000);
        assert_eq!(delegated_only.active_duration_ms, 28 * 60_000);
        assert_eq!(list, 39 * 60_000);

        // What this reports: the same 31 minutes of wall clock, once.
        assert_eq!(merged.active_duration_ms, 31 * 60_000);
        assert!(
            merged.active_duration_ms > parent_only.active_duration_ms,
            "without the absorbed stamps the union is just the parent's figure"
        );
        assert!(
            merged.active_duration_ms < list,
            "on this shape the single sum must come out below the list's, which \
             counts the concurrent delegation twice"
        );
        // On *this* fixture every stamp falls inside the parent's span, so the
        // two agree. That is a property of the fixture: `Builder::range` is
        // widened only by the parent walk, so a sidecar stamped past the
        // parent's last event puts `active_duration_ms` above the span it is
        // reported beside — see the header.
        assert_eq!(merged.active_duration_ms, merged.total_duration_ms);
        assert!(list > merged.total_duration_ms);
    }

    /// A parent that delegates for 30 minutes, and the delegated transcript.
    /// The parent's own events are a minute apart either side of the wait.
    fn delegating_fixture() -> (Vec<String>, Vec<String>) {
        let at_minute = |m: i64| format!("2026-01-01T10:{m:02}:00Z");
        let parent = vec![
            json!({"type": "user", "uuid": "u1", "timestamp": at_minute(0),
                   "message": {"role": "user", "content": "delegate it"}})
            .to_string(),
            json!({"type": "assistant", "uuid": "a1", "timestamp": at_minute(1),
                   "message": {"role": "assistant", "model": "m",
                               "content": [{"type": "tool_use", "id": "toolu_1",
                                            "name": "Task", "input": {}}]}})
            .to_string(),
            json!({"type": "user", "uuid": "u2", "timestamp": at_minute(31),
                   "message": {"role": "user", "content":
                       [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}]}})
            .to_string(),
        ];
        let delegated: Vec<String> = (2..=30)
            .map(|m| {
                json!({"type": "assistant", "uuid": format!("s{m}"), "isSidechain": true,
                       "timestamp": at_minute(m),
                       "message": {"role": "assistant",
                                   "content": [{"type": "text", "text": "working"}]}})
                .to_string()
            })
            .collect();
        (parent, delegated)
    }

    #[test]
    fn delegated_stamps_fill_the_parents_task_wait() {
        // The parent's own events are a minute apart, then a 30-minute wait for
        // the `Task` to come back. Capped alone that wait is one idle gap; with
        // the delegated stamps absorbed it is credited minute by minute.
        let (parent, delegated) = delegating_fixture();
        let without = Corpus::new(&parent, &[]).build(&[]).expect("journey");
        let with = Corpus::new(&parent, &[("agent-x", delegated)])
            .build(&[("agent-x", "", "", "toolu_1")])
            .expect("journey");

        // 1 minute, then the 30-minute wait capped at the 10-minute threshold.
        assert_eq!(without.active_duration_ms, (1 + 10) * 60_000);
        // Every delegated minute is now its own gap, so the whole span counts.
        assert_eq!(with.active_duration_ms, 31 * 60_000);
        assert_eq!(with.total_duration_ms, 31 * 60_000);
    }

    /// The whole response of one fixture transcript, byte for byte.
    ///
    /// Hand-written beside the code, like `session_detail_blocks_golden.json`:
    /// the Go server this route came from is deleted, so there is nothing to
    /// record it from. **A change here is a change to the contract** — edit it
    /// deliberately, never by re-recording until the test passes.
    ///
    /// What it pins that no assertion above can: the key order of every object
    /// on the wire; that a `tool_call`'s `input` reaches it with the
    /// transcript's own key order and number spelling (`z` before `cmd`, and
    /// `1.50` rather than `1.5`) while still being **compacted and
    /// HTML-escaped**, which is the whole job of `serialize_compacted` on
    /// `data` and the only place that rule is observable; that a zero
    /// `duration_ms` is absent from a step and present on a turn; that a
    /// successful `tool_result` still carries `is_error`; and that a nested
    /// sub-agent's steps sit under the call rather than beside it.
    ///
    /// The nested-document half of that was **not** pinned when this test was
    /// first written: the fixture's input was already compact ASCII, so removing
    /// `serialize_compacted` left the golden green. A step's *own* fields are
    /// escaped by `payload`'s `to_vec_marshal` whatever happens, so the fixture
    /// has to put the whitespace and the `<` inside the `tool_use` input for the
    /// assertion to mean anything.
    ///
    /// The fixture has **no ties on any sort key** — every event has its own
    /// second — so only one ordering can be produced.
    #[test]
    fn the_journey_matches_the_golden_bytes() {
        let corpus = Corpus::new(
            &[
                user_event("u1", 1, json!("run the build & <ship> it")),
                assistant_event(
                    "a1",
                    2,
                    json!([
                        {"type": "thinking", "thinking": "which build"},
                        {"type": "text", "text": "Running it."},
                    ]),
                ),
                // The raw bytes, so the golden can pin that `1.50` is neither
                // resorted nor respelled on the way out.
                // `t1`'s input carries three things at once, and every one of
                // them is only observable from the raw bytes: `1.50` (a number
                // spelling `serde_json` would respell), a key order a `Value`
                // round trip would sort, and — inside the *nested* document
                // rather than in a field this code encodes — whitespace and a
                // `<`/`&` pair, which is what `serialize_compacted` on `data` is
                // for. Without it the whitespace survives and the `<` ships raw.
                format!(
                    r#"{{"type":"assistant","uuid":"a2","timestamp":"{}","message":{{"role":"assistant","model":"claude-sonnet-4-6","content":[{{"type":"tool_use","id":"t1","name":"Bash","input":{{ "z": 1.50, "cmd": "make && echo <ok>" }}}},{{"type":"tool_use","id":"t2","name":"Task","input":{{"description":"explore"}}}}],"usage":{{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":8,"cache_read_input_tokens":4,"cache_creation":{{"ephemeral_1h_input_tokens":3}}}}}}}}"#,
                    at(3)
                ),
                tool_result_event("u2", 4, "t1", "build ok", false),
                user_event(
                    "u3",
                    6,
                    json!("<task-notification>done</task-notification>"),
                ),
                json!({
                    "type": "system", "subtype": "compact_boundary", "uuid": "s1",
                    "timestamp": at(8),
                    "compactMetadata": {"trigger": "manual", "preTokens": 900,
                                        "postTokens": 200, "durationMs": 1_500},
                })
                .to_string(),
            ],
            // The delegated event falls inside the parent's span, as a real
            // one does — a `Task` is answered after its agent has finished.
            &[(
                "agent-x",
                vec![sidechain(
                    "assistant",
                    "sa1",
                    5,
                    json!([{"type": "text", "text": "explored"}]),
                )],
            )],
        );
        let journey = corpus
            .build(&[("agent-x", "Explore", "explore the repo", "t2")])
            .expect("journey");

        let got = String::from_utf8(gojson::to_vec(&journey).expect("encode")).expect("utf8");
        let want = include_str!("../../../../parity/journey_golden.json");
        assert_eq!(got, want, "the session journey drifted from its golden");
    }
}
