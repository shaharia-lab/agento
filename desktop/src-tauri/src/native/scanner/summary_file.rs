//! One transcript → one cache row, ported from the `readSummaryFile` half of
//! `internal/claudesessions/scanner.go`.
//!
//! This is the function the whole scanner exists to call. Everything else —
//! the walk, the diff, the batching — decides *which* files reach it; this
//! decides what a row says.
//!
//! Five rules here are load-bearing, and each is a number a user reads:
//!
//! * **`message_count` counts turns, `event_count` counts events.** The turn
//!   test is [`transcript::is_user_turn_content`], shared with the insight
//!   pipeline and the journey — changing it moves all three, which is why Go
//!   requires both version constants to move together.
//! * **The time range is bounded by a denylist, not an allowlist**
//!   ([`bounds_session_time_range`]). `pr-link` is the one that matters today:
//!   it carries a real timestamp that can post-date the conversation, and
//!   letting it extend `last_activity` would reorder the sessions list by
//!   something that is not conversation.
//! * **Timestamps are normalized to UTC**, because SQLite stores the driver's
//!   rendering and both the list's `ORDER BY` and its keyset pagination compare
//!   that as *text*. Lexical order matches chronological order only while every
//!   value carries the same zone suffix.
//! * **Compaction's dropped-token figure is a maximum, not a sum** — the CLI
//!   reports a running total, so adding them up multiply-counts.
//! * **A transcript with no timestamped event produces no row at all.**
//!
//! The preview deliberately uses a *weaker* rule than the counter: a genuine
//! turn always wins, but a transcript that never contains one — a slash command
//! and its expansion — still takes its preview from the wrapper, because the
//! preview is the last fallback for a session's display title and an empty one
//! renders as a blank, unidentifiable row.

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::native::gotime::GoTime;
use crate::native::insights::transcript::{self, Event};
use crate::native::pricing::Resolver;
use crate::native::sessions::summary::{SessionPR, SessionSummary, TokenUsage};

use super::cost::CostAccumulator;
use crate::native::active_time::ActiveTimeTracker;

/// Preview truncation, in runes rather than bytes.
const PREVIEW_MAX_CHARS: usize = 120;

/// Reads a parent session transcript.
pub fn read_session_summary(
    session_id: &str,
    project_path: &str,
    file_path: &Path,
    resolver: Option<&Resolver>,
    idle_gap_ms: i64,
) -> Result<Option<SessionSummary>, String> {
    read_summary_file(
        session_id,
        project_path,
        file_path,
        false,
        resolver,
        idle_gap_ms,
    )
}

/// Reads a sub-agent transcript.
///
/// Sub-agent files use the same event schema as a parent session, but **every**
/// event in them is flagged `isSidechain` — the marker a parent transcript uses
/// for delegated turns it should not count twice. Inside a sub-agent file that
/// marker is universal and carries no such meaning, so sidechain user turns are
/// counted here; otherwise `message_count` would silently degrade to
/// assistant-only.
pub fn read_subagent_summary(
    session_id: &str,
    project_path: &str,
    file_path: &Path,
    resolver: Option<&Resolver>,
    idle_gap_ms: i64,
) -> Result<Option<SessionSummary>, String> {
    read_summary_file(
        session_id,
        project_path,
        file_path,
        true,
        resolver,
        idle_gap_ms,
    )
}

/// Reads one transcript into a summary.
///
/// Returns `Ok(None)` when the file yielded no timestamped event — Go's
/// `StartTime.IsZero()` check. The caller treats that exactly as it treats a
/// read failure: the file is counted as done, and no row is written.
fn read_summary_file(
    session_id: &str,
    project_path: &str,
    file_path: &Path,
    count_sidechain_users: bool,
    resolver: Option<&Resolver>,
    idle_gap_ms: i64,
) -> Result<Option<SessionSummary>, String> {
    let events = transcript::read(file_path)?;

    let mut summary = SessionSummary {
        session_id: session_id.to_string(),
        project_path: project_path.to_string(),
        ..Default::default()
    };
    // True while the preview came from an injected wrapper rather than a
    // genuine turn, so a real prompt arriving later can replace it.
    let mut preview_is_fallback = false;

    let mut range = TimeRange::default();
    let mut active = ActiveTimeTracker::new(idle_gap_ms);
    let mut costs = CostAccumulator::new(resolver);

    for ev in &events {
        // A file-history snapshot is neither conversation nor metadata; it is
        // skipped before anything observes it, including the time range.
        if ev.event_type == "file-history-snapshot" {
            continue;
        }

        if bounds_session_time_range(&ev.event_type) {
            if let Some(ts) = ev.timestamp {
                range.update(ts);
                // The same event set that bounds the time range feeds active
                // duration, so active time is contained in [start, last] by
                // construction — a pr-link posted days after the conversation
                // can extend neither.
                active.observe(ts, ev.event_type == "assistant");
            }
        }

        update_metadata_from_event(&mut summary, ev);
        process_summary_event(
            &mut summary,
            &mut preview_is_fallback,
            ev,
            count_sidechain_users,
        );

        if ev.event_type == "assistant" {
            if let Some(message) = &ev.message {
                if message.usage.is_some() {
                    let mut u = TokenUsage::default();
                    add_assistant_usage(&mut u, message);
                    if let Some(ts) = ev.timestamp {
                        costs.add_assistant_message(&message.model, &u, ts);
                    }
                }
            }
        }
    }

    // A transcript with only denylisted or timestampless events yields no row.
    let Some(start) = range.start else {
        return Ok(None);
    };

    summary.start_time = GoTime(start.fixed_offset());
    summary.last_activity = GoTime(range.last.unwrap_or(start).fixed_offset());
    summary.active_duration_ms = active.active_ms();
    summary.cost = costs.total();
    summary.cost_by_model = costs.cost_by_model();
    summary.unpriced_models = costs.unpriced_models();
    summary.unpriced_tokens = costs.unknown_pricing_tokens();

    Ok(Some(summary))
}

/// Whether an event type may extend the session's start/last-activity range.
///
/// A **denylist on purpose**. Many event types carry timestamps and
/// legitimately bound the range — `queue-operation` and `file-history-delta`
/// among them — so enumerating the ones that count would silently shrink the
/// range for existing sessions as Claude Code adds types.
///
/// Excluded are the events that *describe* the session rather than occur within
/// it. The title and metadata events carry no timestamp today, so excluding
/// them is a no-op the zero check already handles — but a future release adding
/// one must not drag `start_time` backwards. `pr-link` is the exception that
/// matters today.
pub fn bounds_session_time_range(event_type: &str) -> bool {
    !matches!(
        event_type,
        "custom-title"
            | "ai-title"
            | "pr-link"
            | "agent-name"
            | "permission-mode"
            | "mode"
            | "relocated"
            | "worktree-state"
    )
}

/// The session's `[start, last]` bounds.
#[derive(Default)]
struct TimeRange {
    start: Option<DateTime<Utc>>,
    last: Option<DateTime<Utc>>,
}

impl TimeRange {
    /// Widens the range to include `ts`, normalized to UTC.
    ///
    /// The normalization is what makes the stored bounds orderable: SQLite
    /// holds them as text, and lexical order matches chronological order only
    /// while every value carries the same zone suffix. Claude Code writes
    /// Z-suffixed timestamps, so in practice this changes no stored value —
    /// which is why Go needed no scanner-version bump for it.
    fn update(&mut self, ts: DateTime<Utc>) {
        match self.start {
            Some(start) if ts >= start => {}
            _ => self.start = Some(ts),
        }
        match self.last {
            Some(last) if ts <= last => {}
            _ => self.last = Some(ts),
        }
    }
}

/// Sets cwd and git branch from the first event that has them.
pub(crate) fn update_metadata_from_event(summary: &mut SessionSummary, ev: &Event) {
    if summary.cwd.is_empty() && !ev.cwd.is_empty() {
        summary.cwd = ev.cwd.clone();
    }
    if summary.git_branch.is_empty() && !ev.git_branch.is_empty() {
        summary.git_branch = ev.git_branch.clone();
    }
}

/// Accumulates one assistant message's usage.
fn add_assistant_usage(usage: &mut TokenUsage, message: &transcript::Message) {
    let Some(u) = &message.usage else {
        return;
    };
    let (five_min, one_hour) = u.split_cache_tiers();
    usage.input_tokens += u.input_tokens;
    usage.output_tokens += u.output_tokens;
    usage.cache_creation_tokens += u.cache_creation_input_tokens;
    usage.cache_creation_5m_tokens += five_min;
    usage.cache_creation_1h_tokens += one_hour;
    usage.cache_read_tokens += u.cache_read_input_tokens;
}

fn process_summary_event(
    summary: &mut SessionSummary,
    preview_is_fallback: &mut bool,
    ev: &Event,
    count_sidechain_users: bool,
) {
    match ev.event_type.as_str() {
        "user" => {
            if ev.is_sidechain && !count_sidechain_users {
                return;
            }
            add_summary_user_event(summary, preview_is_fallback, ev);
        }
        "assistant" => add_summary_assistant_event(summary, ev),
        "pr-link" => add_summary_pr_link(summary, ev),
        "system" => add_summary_compaction(summary, ev),
        _ => apply_session_metadata(summary, ev),
    }
}

/// Records the events that describe the session rather than occurring within
/// it.
///
/// Claude Code re-appends every one of them on each resume, so unconditional
/// assignment during a sequential read gives last-wins for free — which is the
/// correct rule, since the final value is the current one.
pub(crate) fn apply_session_metadata(summary: &mut SessionSummary, ev: &Event) {
    match ev.event_type.as_str() {
        "custom-title" => summary.native_title = ev.custom_title.clone(),
        "ai-title" => summary.ai_title = ev.ai_title.clone(),
        "agent-name" => summary.agent_name = ev.agent_name.clone(),
        "permission-mode" => summary.permission_mode = ev.permission_mode.clone(),
        "mode" => summary.mode = ev.mode.clone(),
        "relocated" => summary.relocated_cwd = ev.relocated_cwd.clone(),
        "worktree-state" => {
            if let Some(w) = &ev.worktree_session {
                summary.worktree_name = w.worktree_name.clone();
                summary.worktree_branch = w.worktree_branch.clone();
                summary.original_branch = w.original_branch.clone();
            }
        }
        _ => {}
    }
}

/// Records a linked pull request, deduplicated by URL.
///
/// Claude Code re-emits the event on every resume, so the same PR appears many
/// times in one file; the earliest sighting keeps its timestamp.
pub(crate) fn add_summary_pr_link(summary: &mut SessionSummary, ev: &Event) {
    if ev.pr_url.is_empty() {
        return;
    }
    if summary.prs.iter().any(|pr| pr.pr_url == ev.pr_url) {
        return;
    }
    summary.prs.push(SessionPR {
        pr_number: ev.pr_number,
        pr_url: ev.pr_url.clone(),
        pr_repository: ev.pr_repository.clone(),
        first_seen_at: ev
            .timestamp
            .map(|t| GoTime(t.fixed_offset()))
            .unwrap_or_default(),
    });
}

/// Records a conversation compaction.
///
/// Only the `compact_boundary` subtype carries compaction metadata; every other
/// system subtype is ignored here.
pub(crate) fn add_summary_compaction(summary: &mut SessionSummary, ev: &Event) {
    if ev.subtype != "compact_boundary" {
        return;
    }
    let Some(meta) = &ev.compact_metadata else {
        return;
    };
    summary.compaction_count += 1;

    // A running total across the session, so the largest value seen is the
    // session's figure — summing would multiply-count.
    if meta.cumulative_dropped_tokens > 0 {
        if meta.cumulative_dropped_tokens > summary.dropped_tokens {
            summary.dropped_tokens = meta.cumulative_dropped_tokens;
        }
        return;
    }
    // Older Claude Code releases omit cumulativeDroppedTokens while still
    // reporting preTokens/postTokens. Reporting zero there would be a visibly
    // wrong headline number — one real transcript compacts 1,000,563 tokens
    // down to 26,087 — so this boundary's own drop is accumulated instead.
    let dropped = meta.pre_tokens - meta.post_tokens;
    if dropped > 0 {
        summary.dropped_tokens += dropped;
    }
}

/// Records one user event.
///
/// Every event bumps `event_count`, but only genuine human input counts as a
/// message — the bulk of user events merely carry `tool_result` blocks back to
/// the model.
fn add_summary_user_event(
    summary: &mut SessionSummary,
    preview_is_fallback: &mut bool,
    ev: &Event,
) {
    summary.event_count += 1;
    let Some(message) = &ev.message else {
        return;
    };

    if !transcript::is_user_turn_content(&message.content) {
        // Still a preview candidate if nothing better ever arrives — but never
        // a tool_result carrier, which is unreadable machine payload.
        if summary.preview.is_empty() && !transcript::is_injected_user_content(&message.content) {
            return;
        }
        if summary.preview.is_empty() {
            let raw = transcript::extract_text_content(&message.content);
            summary.preview = truncate_chars(&fallback_preview_label(&raw), PREVIEW_MAX_CHARS);
            *preview_is_fallback = true;
        }
        return;
    }

    summary.message_count += 1;
    // A genuine turn replaces a wrapper-sourced preview, so the label prefers
    // what the person actually typed even when a wrapper came first.
    //
    // Every known injected form is classified above and takes the fallback
    // branch, so this call is normally a no-op on real prose. It stays because
    // the marker tables are empirical: a wrapper shape a future Claude Code
    // release invents reaches this branch until the tables catch up, and
    // labeling it beats rendering raw tag soup.
    if summary.preview.is_empty() || *preview_is_fallback {
        let raw = transcript::extract_text_content(&message.content);
        summary.preview = truncate_chars(&fallback_preview_label(&raw), PREVIEW_MAX_CHARS);
        *preview_is_fallback = false;
    }
}

/// Records one assistant event: always an event, but a message only when it
/// contains text the user actually saw.
fn add_summary_assistant_event(summary: &mut SessionSummary, ev: &Event) {
    summary.event_count += 1;
    let Some(message) = &ev.message else {
        return;
    };
    if transcript::is_assistant_reply(&message.content) {
        summary.message_count += 1;
    }
    if summary.model.is_empty() && !message.model.is_empty() {
        summary.model = message.model.clone();
    }
    add_assistant_usage(&mut summary.usage, message);
}

/// Turns an injected wrapper into something a person can recognize in a list.
///
/// These previews are the last resort for a session's display title, and for a
/// session that is only a slash command they are all there is. Unprocessed they
/// render as rows reading `<command-message>lab-workflow:…</command-message>…`
/// or `Base directory for this skill: /home/…` — three of nine rows on the
/// reference corpus. The command or skill name was in there the whole time.
///
/// Anything matching neither shape is returned **unchanged and untrimmed**, so
/// a wrapper form this does not know about is still shown rather than blanked.
pub fn fallback_preview_label(raw: &str) -> String {
    let trimmed = raw.trim();

    if let Some(name) = command_name(trimmed) {
        return format!("/{}", name.trim());
    }
    if let Some(path) = transcript::skill_preamble_path(trimmed) {
        let name = skill_name_from_path(&path);
        if !name.is_empty() {
            return format!("skill: {name}");
        }
    }
    raw.to_string()
}

/// Extracts the slash command Claude Code records when a session was started by
/// one — Go's `<command-name>([^<]+)</command-name>` regexp, hand-rolled to
/// keep the crate free of a regex dependency it otherwise does not need.
fn command_name(s: &str) -> Option<&str> {
    const OPEN: &str = "<command-name>";
    const CLOSE: &str = "</command-name>";
    let start = s.find(OPEN)? + OPEN.len();
    let rest = &s[start..];
    // `[^<]+` — the capture stops at the first `<`, which must be the closing
    // tag for the match to be the one Go's regexp finds.
    let end = rest.find('<')?;
    if end == 0 || !rest[end..].starts_with(CLOSE) {
        return None;
    }
    Some(&rest[..end])
}

/// Reads the skill's name out of its directory path.
///
/// Claude Code lays these out as `…/skills/<name>`, so the segment after
/// `skills` is the name; a path that does not contain one falls back to its
/// last segment, which is the same thing for a bare skill directory.
fn skill_name_from_path(path: &str) -> String {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    for (i, seg) in segments.iter().enumerate() {
        if *seg == "skills" && i + 1 < segments.len() {
            return segments[i + 1].to_string();
        }
    }
    segments.last().map(|s| s.to_string()).unwrap_or_default()
}

/// Truncates to `max` characters, appending an ellipsis — Go's
/// `truncateRunes`, which counts runes rather than bytes.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_denylist_is_exactly_the_eight_describing_events() {
        for denied in [
            "custom-title",
            "ai-title",
            "pr-link",
            "agent-name",
            "permission-mode",
            "mode",
            "relocated",
            "worktree-state",
        ] {
            assert!(!bounds_session_time_range(denied), "{denied}");
        }
        // Everything else bounds the range, including types this build has
        // never heard of — that is the point of a denylist.
        for allowed in [
            "user",
            "assistant",
            "system",
            "queue-operation",
            "file-history-delta",
            "some-future-event",
        ] {
            assert!(bounds_session_time_range(allowed), "{allowed}");
        }
    }

    #[test]
    fn a_command_wrapper_becomes_its_command_name() {
        assert_eq!(
            fallback_preview_label(
                "<command-message>lab-workflow:github-issue-to-pr</command-message>\
                 <command-name>lab-workflow:github-issue-to-pr</command-name>"
            ),
            "/lab-workflow:github-issue-to-pr"
        );
    }

    #[test]
    fn a_skill_preamble_becomes_its_skill_name() {
        assert_eq!(
            fallback_preview_label(
                "Base directory for this skill: /home/u/.claude/plugins/cache/x/skills/deploy-thing"
            ),
            "skill: deploy-thing"
        );
    }

    #[test]
    fn anything_else_is_returned_untouched_including_its_whitespace() {
        // Go returns the *untrimmed* raw on no match, though it matches against
        // the trimmed string — reproduce both halves.
        assert_eq!(fallback_preview_label("  hello  "), "  hello  ");
        assert_eq!(
            fallback_preview_label("<unknown-wrapper>x"),
            "<unknown-wrapper>x"
        );
    }

    #[test]
    fn a_skill_path_without_a_skills_segment_falls_back_to_its_last() {
        assert_eq!(skill_name_from_path("/a/b/deploy"), "deploy");
        assert_eq!(skill_name_from_path("/a/skills/deploy/sub"), "deploy");
        assert_eq!(skill_name_from_path("deploy/"), "deploy");
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        let s = "é".repeat(130);
        let out = truncate_chars(&s, PREVIEW_MAX_CHARS);
        assert_eq!(
            out.chars().count(),
            PREVIEW_MAX_CHARS + 1,
            "120 plus the ellipsis"
        );
        assert!(out.ends_with('…'));
        assert_eq!(truncate_chars("short", PREVIEW_MAX_CHARS), "short");
    }

    #[test]
    fn the_time_range_widens_in_both_directions() {
        let t = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let mut r = TimeRange::default();
        r.update(t("2026-03-15T12:00:00Z"));
        r.update(t("2026-03-15T10:00:00Z"));
        r.update(t("2026-03-15T14:00:00Z"));
        assert_eq!(r.start, Some(t("2026-03-15T10:00:00Z")));
        assert_eq!(r.last, Some(t("2026-03-15T14:00:00Z")));
    }
}
