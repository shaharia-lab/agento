//! Claude Code's session JSONL, decoded.
//!
//! Mirrors the event model in `internal/claudesessions/processor.go` plus the
//! two predicates it shares with the scanner. **Written to be the transcript
//! reader the scanner port needs too** (issue #270): everything here is about
//! the file format, not about insights, so that port should extend this module
//! rather than start a second decoder. Two readers of one format is exactly how
//! `message_count` and `turn_count` would drift apart.
//!
//! ## The predicate that decides three different numbers
//!
//! [`is_user_turn_content`] is the single rule behind the scanner's
//! `message_count`, this pipeline's `turn_count`, and the journey timeline's
//! turns. Changing it moves all three at once, plus everything derived from
//! `turn_count` — steps-per-turn, autonomy, tokens-per-turn, the longest
//! autonomous chain and the response-time averages. On the Go side that means
//! **both** `CurrentScannerVersion` and `CurrentProcessorVersion` must be bumped
//! together; bumping one recreates exactly the drift the shared predicate exists
//! to prevent.
//!
//! It rejects two classes of user event: carriers for `tool_result` blocks, and
//! content that *opens with* one of Claude Code's own injections. The match is
//! prefix-anchored after a trim and never a substring, because a person can
//! legitimately write *about* a marker — the reference corpus already contains a
//! genuine prompt quoting "system-reminder" mid-sentence, and a substring test
//! would silently stop counting it.
//!
//! Both marker tables are **empirical**: read off the local corpus, and a new
//! Claude Code release can add a form neither lists. That failure mode is
//! benign — a missed marker leaves the count where it already was — so
//! re-sampling is maintenance, not a correctness dependency.

use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// One decoded line of a session JSONL file.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Event {
    #[serde(default, rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub timestamp: Option<DateTime<Utc>>,
    /// Set on every event of a sub-agent transcript, and on delegated events
    /// inside a parent transcript. **The check is deliberately left to each
    /// caller**: the flag means "delegated work, skip" in a parent transcript
    /// but carries no such meaning in a sub-agent's own.
    #[serde(default, rename = "isSidechain")]
    pub is_sidechain: bool,
    #[serde(default)]
    pub message: Option<Message>,

    /// Stamped by Claude Code at the *top level* of assistant events — never
    /// inside `message`, and never on user events. It names which skill's
    /// instructions were in context when the turn ran, so on a `Skill` tool
    /// call it names the **caller**, not the skill being invoked.
    #[serde(default, rename = "attributionSkill")]
    pub attribution_skill: String,
    #[serde(default, rename = "attributionPlugin")]
    pub attribution_plugin: String,
    #[serde(default, rename = "attributionAgent")]
    pub attribution_agent: String,
    /// Decoded but deliberately **not counted**: these hold the last MCP tool
    /// touched and persist onto later, unrelated turns. MCP attribution comes
    /// from the `mcp__<server>__<tool>` block name instead, which is
    /// authoritative.
    #[serde(default, rename = "attributionMcpServer")]
    pub attribution_mcp_server: String,
    #[serde(default, rename = "attributionMcpTool")]
    pub attribution_mcp_tool: String,
    /// The reasoning-effort tier the turn ran at.
    #[serde(default)]
    pub effort: String,

    // ── Fields the scanner reads (issue #270) ───────────────────────────────
    //
    // The insight pipeline ignores all of these; they are here because this is
    // the one decoder for this file format, and a second struct over the same
    // lines is how two readers of one format drift apart.
    /// First non-empty value wins — the session's working directory.
    #[serde(default)]
    pub cwd: String,
    #[serde(default, rename = "gitBranch")]
    pub git_branch: String,

    /// Title events carry no timestamp and no message — only these fields.
    /// Claude Code re-appends them on every resume, so last-in-file wins.
    #[serde(default, rename = "customTitle")]
    pub custom_title: String,
    /// Claude Code's own auto-title, distinct from a user rename.
    #[serde(default, rename = "aiTitle")]
    pub ai_title: String,

    /// `pr-link`: the pull request a session produced. Unlike the other
    /// metadata events this one carries a **real** timestamp, which is why
    /// `bounds_session_time_range` has to exclude it explicitly.
    #[serde(default, rename = "prNumber")]
    pub pr_number: i64,
    #[serde(default, rename = "prUrl")]
    pub pr_url: String,
    #[serde(default, rename = "prRepository")]
    pub pr_repository: String,

    /// Session metadata events. Like the title events these carry no timestamp
    /// and are re-appended on every resume, so the last one in the file wins.
    #[serde(default, rename = "agentName")]
    pub agent_name: String,
    #[serde(default, rename = "permissionMode")]
    pub permission_mode: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default, rename = "relocatedCwd")]
    pub relocated_cwd: String,
    #[serde(default, rename = "worktreeSession")]
    pub worktree_session: Option<WorktreeSession>,

    /// `system` events: the subtype selects the payload, and only
    /// `compact_boundary` carries compaction metadata.
    #[serde(default)]
    pub subtype: String,
    #[serde(default, rename = "compactMetadata")]
    pub compact_metadata: Option<CompactMetadata>,
}

/// The payload of a `worktree-state` event: the throwaway worktree a session
/// ran in, plus where it came from.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorktreeSession {
    #[serde(default, rename = "worktreeName")]
    pub worktree_name: String,
    #[serde(default, rename = "worktreeBranch")]
    pub worktree_branch: String,
    #[serde(default, rename = "originalBranch")]
    pub original_branch: String,
    #[serde(default, rename = "originalCwd")]
    pub original_cwd: String,
    #[serde(default, rename = "originalHeadCommit")]
    pub original_head_commit: String,
}

/// The payload of a `system`/`compact_boundary` event.
///
/// `cumulative_dropped_tokens` is a **running total** across the session, not a
/// delta — the largest value seen is the session's figure, and summing would
/// multiply-count it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CompactMetadata {
    #[serde(default)]
    pub trigger: String,
    #[serde(default, rename = "preTokens")]
    pub pre_tokens: i64,
    #[serde(default, rename = "postTokens")]
    pub post_tokens: i64,
    #[serde(default, rename = "cumulativeDroppedTokens")]
    pub cumulative_dropped_tokens: i64,
    #[serde(default, rename = "durationMs")]
    pub duration_ms: i64,
}

/// The message payload of a user or assistant event.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub model: String,
    /// Left raw: it is a bare JSON string on some events and an array of
    /// content blocks on others, and telling those apart is what the turn
    /// predicates are about.
    #[serde(default)]
    pub content: serde_json::Value,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// Token counters attached to an assistant message.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_creation_input_tokens: i64,
    #[serde(default)]
    pub cache_read_input_tokens: i64,
    /// Absent on transcripts written before Claude Code emitted the split.
    #[serde(default)]
    pub cache_creation: Option<CacheCreation>,
}

/// The cache-TTL split of `cache_creation_input_tokens`. The tiers bill at
/// different multiples of the input rate (1.25× for 5-minute, 2× for 1-hour),
/// which is the only reason the split is carried at all.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CacheCreation {
    #[serde(default)]
    pub ephemeral_5m_input_tokens: i64,
    #[serde(default)]
    pub ephemeral_1h_input_tokens: i64,
}

impl Usage {
    /// Attribute the cache-creation total across the 5m and 1h buckets.
    ///
    /// The nested 1h figure is trusted but clamped: it is the *reported* split
    /// of a total the same message also reports, and an inconsistent pair must
    /// not produce a negative 5m bucket.
    pub fn split_cache_tiers(&self) -> (i64, i64) {
        let nested_1h = self
            .cache_creation
            .as_ref()
            .map_or(0, |c| c.ephemeral_1h_input_tokens);
        split_cache_tiers(self.cache_creation_input_tokens, nested_1h)
    }
}

/// `splitCacheTiers` from `scanner.go`, shared so the two decoders cannot drift.
pub fn split_cache_tiers(total: i64, nested_1h: i64) -> (i64, i64) {
    let one_hour = nested_1h.clamp(0, total.max(0));
    (total - one_hour, one_hour)
}

/// One block within a message's content array.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContentBlock {
    #[serde(default, rename = "type")]
    pub block_type: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub thinking: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub input: serde_json::Value,
    #[serde(default)]
    pub tool_use_id: String,
    #[serde(default)]
    pub is_error: bool,
}

/// Decode array content into blocks. String content and anything undecodable
/// yield none, matching Go's `parseContentBlocks`.
pub fn parse_content_blocks(content: &serde_json::Value) -> Vec<ContentBlock> {
    let Some(items) = content.as_array() else {
        return Vec::new();
    };
    // Go decodes the whole array or nothing: a single malformed block makes
    // `json.Unmarshal` fail and the function return nil.
    let mut blocks = Vec::with_capacity(items.len());
    for item in items {
        match serde_json::from_value::<ContentBlock>(item.clone()) {
            Ok(block) => blocks.push(block),
            Err(_) => return Vec::new(),
        }
    }
    blocks
}

/// The wrappers Claude Code writes as user-role events that nobody typed:
/// slash-command expansions, local command output, sub-agent completion
/// notices, and injected reminders. These only ever appear as *string* content.
const INJECTED_TURN_MARKERS: [&str; 6] = [
    "<task-notification>",
    "<command-message>",
    "<command-name>",
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<system-reminder>",
];

/// The injected user events that arrive as *array* content — a single text
/// block, no `tool_result`.
///
/// A separate table because the two populations do not overlap. Both entries
/// are required: neither is a prefix of the other, since the shorter one closes
/// its bracket where the longer one continues (`" for tool use]"`).
const INJECTED_ARRAY_TURN_MARKERS: [&str; 2] = [
    "[Request interrupted by user]",
    "[Request interrupted by user for tool use]",
];

/// Whether content is one of Claude Code's own injections rather than something
/// a person typed. Handles both shapes the harness writes.
///
/// Public because the scanner's *preview* rule is deliberately weaker than
/// its turn rule: an injected wrapper is not a turn, but it is still a
/// preview candidate when nothing better ever arrives.
pub fn is_injected_user_content(content: &serde_json::Value) -> bool {
    if let Some(s) = content.as_str() {
        return has_injected_prefix(s.trim(), &INJECTED_TURN_MARKERS);
    }
    if content.is_array() {
        let blocks = parse_content_blocks(content);
        // Any other array shape — several blocks, or a block that is not text —
        // is genuine, because the injected forms are always emitted alone.
        if blocks.len() != 1 || blocks[0].block_type != "text" {
            return false;
        }
        let text = blocks[0].text.trim();
        return has_injected_prefix(text, &INJECTED_ARRAY_TURN_MARKERS) || is_skill_preamble(text);
    }
    false
}

/// `skillPreamblePattern`: `^Base directory for this skill:\s*(\S+)`.
///
/// Matched as a pattern rather than a bare prefix so the colon must be followed
/// by a path token — `Base directory for this skill:` alone is ordinary prose a
/// person could type. Hand-written rather than pulling in a regex crate for one
/// anchored pattern.
fn is_skill_preamble(text: &str) -> bool {
    skill_preamble_path(text).is_some()
}

/// The `(\S+)` capture of `skillPreamblePattern` — the skill's directory.
///
/// The scanner turns it into a readable preview label; the turn predicate
/// only cares whether it matched at all. One implementation so the two
/// cannot disagree about what counts as a preamble.
pub fn skill_preamble_path(text: &str) -> Option<String> {
    const PREFIX: &str = "Base directory for this skill:";
    let rest = text.strip_prefix(PREFIX)?;
    // `\s*` then `\S+`: at least one non-space character must follow.
    let token: String = rest
        .trim_start()
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    (!token.is_empty()).then_some(token)
}

fn has_injected_prefix(s: &str, markers: &[&str]) -> bool {
    markers.iter().any(|m| s.starts_with(m))
}

/// Whether a user message's content is genuine human input.
///
/// The sidechain check is deliberately **not** here — see the module docs.
pub fn is_user_turn_content(content: &serde_json::Value) -> bool {
    if parse_content_blocks(content)
        .iter()
        .any(|b| b.block_type == "tool_result")
    {
        return false;
    }
    !is_injected_user_content(content)
}

/// Whether an assistant message contains something the user actually saw — at
/// least one text block. A turn that only issues tool calls is a round-trip,
/// not a message.
pub fn is_assistant_reply(content: &serde_json::Value) -> bool {
    let blocks = parse_content_blocks(content);
    if blocks.is_empty() {
        // No decodable blocks: absent, null, or non-array content. Only a
        // non-empty bare string carries text the user saw.
        return content.as_str().is_some_and(|s| !s.is_empty());
    }
    blocks.iter().any(|b| b.block_type == "text")
}

/// Whether this event starts a turn: a non-sidechain user message carrying
/// genuine input.
pub fn is_turn_start(ev: &Event) -> bool {
    if ev.event_type != "user" || ev.is_sidechain {
        return false;
    }
    ev.message
        .as_ref()
        .is_some_and(|m| is_user_turn_content(&m.content))
}

/// Read a transcript, dropping lines that do not decode.
///
/// A malformed line is skipped rather than failing the file: transcripts are
/// appended to live by a process that can be killed mid-write, so a truncated
/// last line is normal rather than exceptional.
pub fn read(path: &Path) -> Result<Vec<Event>, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("opening transcript {}: {e}", path.display()))?;
    let mut events = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            // An unreadable line mid-file means the rest is unreliable too.
            Err(e) => return Err(format!("reading transcript {}: {e}", path.display())),
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Event>(&line) {
            events.push(event);
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tool_result_carrier_is_not_a_turn() {
        let content = json!([{"type": "tool_result", "tool_use_id": "t1"}]);
        assert!(!is_user_turn_content(&content));
        // Position does not matter: a carrier is a carrier wherever the block
        // sits.
        let mixed = json!([{"type": "text", "text": "hi"}, {"type": "tool_result"}]);
        assert!(!is_user_turn_content(&mixed));
    }

    #[test]
    fn string_wrappers_are_rejected_only_at_the_start() {
        assert!(!is_user_turn_content(&json!(
            "<system-reminder>do the thing</system-reminder>"
        )));
        // Leading whitespace is trimmed first.
        assert!(!is_user_turn_content(&json!("\n  <command-name>/foo")));
        // …but a person writing *about* a marker is still a turn. The corpus
        // contains exactly this.
        assert!(is_user_turn_content(&json!(
            "why does the <system-reminder> block keep appearing?"
        )));
    }

    #[test]
    fn the_array_shaped_injections_are_rejected_and_only_when_alone() {
        let interrupted = json!([{"type": "text", "text": "[Request interrupted by user]"}]);
        assert!(!is_user_turn_content(&interrupted));

        let for_tool_use =
            json!([{"type": "text", "text": "[Request interrupted by user for tool use]"}]);
        assert!(!is_user_turn_content(&for_tool_use));

        // Two blocks is a genuine turn — the injected forms are emitted alone.
        let two = json!([
            {"type": "text", "text": "[Request interrupted by user]"},
            {"type": "text", "text": "actually, do this instead"}
        ]);
        assert!(is_user_turn_content(&two));
    }

    #[test]
    fn the_skill_preamble_needs_a_path_after_the_colon() {
        let preamble = json!([{
            "type": "text",
            "text": "Base directory for this skill: /home/u/.claude/skills/foo"
        }]);
        assert!(!is_user_turn_content(&preamble));

        // Prose that merely opens with the same words stays a turn.
        let prose = json!([{
            "type": "text",
            "text": "Base directory for this skill: what should it be?"
        }]);
        assert!(!is_user_turn_content(&prose), "a word still counts as \\S+");

        let no_token = json!([{"type": "text", "text": "Base directory for this skill:"}]);
        assert!(is_user_turn_content(&no_token));
    }

    #[test]
    fn an_assistant_reply_needs_text_the_user_saw() {
        assert!(is_assistant_reply(
            &json!([{"type": "text", "text": "done"}])
        ));
        assert!(is_assistant_reply(&json!("plain string")));
        assert!(!is_assistant_reply(&json!("")));
        // Tool calls alone are a round-trip, not a message.
        assert!(!is_assistant_reply(&json!([
            {"type": "tool_use", "name": "Bash", "id": "t1"}
        ])));
        assert!(!is_assistant_reply(&json!(null)));
    }

    #[test]
    fn a_sidechain_user_event_never_starts_a_turn_in_a_parent_transcript() {
        let mut ev = Event {
            event_type: "user".into(),
            message: Some(Message {
                content: json!("real input"),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(is_turn_start(&ev));
        ev.is_sidechain = true;
        assert!(!is_turn_start(&ev));
    }

    #[test]
    fn cache_tiers_split_and_clamp_an_inconsistent_pair() {
        assert_eq!(split_cache_tiers(100, 40), (60, 40));
        assert_eq!(split_cache_tiers(100, 0), (100, 0));
        // A reported 1h figure larger than the total must not make 5m negative.
        assert_eq!(split_cache_tiers(100, 500), (0, 100));
        assert_eq!(split_cache_tiers(100, -5), (100, 0));
    }

    #[test]
    fn a_malformed_block_discards_the_whole_array_as_gos_decoder_does() {
        // `is_error` is a bool in the schema; a string fails the decode, and Go
        // returns nil for the array rather than the blocks it managed to read.
        let bad = json!([{"type": "tool_result", "is_error": "yes"}]);
        assert!(parse_content_blocks(&bad).is_empty());
        // …so it is not seen as a carrier, exactly as on the Go side.
        assert!(is_user_turn_content(&bad));
    }
}

/// Plain text from a message's content field, which is either a JSON string or
/// an array of content blocks. Ported from `extractTextContent` in
/// `scanner.go`; text blocks are joined with newlines and everything else is
/// dropped.
pub fn extract_text_content(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if !content.is_array() {
        return String::new();
    }
    parse_content_blocks(content)
        .iter()
        .filter(|b| b.block_type == "text" && !b.text.is_empty())
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod scanner_shared_tests {
    use super::*;

    #[test]
    fn a_skill_preamble_yields_its_path_and_a_bare_colon_does_not() {
        assert_eq!(
            skill_preamble_path("Base directory for this skill: /a/b/skills/x"),
            Some("/a/b/skills/x".to_string())
        );
        // Ordinary prose a person could type must not match.
        assert_eq!(skill_preamble_path("Base directory for this skill:"), None);
        assert_eq!(
            skill_preamble_path("Base directory for this skill:   "),
            None
        );
    }

    #[test]
    fn text_is_extracted_from_both_content_shapes() {
        assert_eq!(extract_text_content(&serde_json::json!("plain")), "plain");
        assert_eq!(
            extract_text_content(&serde_json::json!([
                {"type": "text", "text": "one"},
                {"type": "tool_use", "name": "Bash"},
                {"type": "text", "text": "two"}
            ])),
            "one\ntwo"
        );
        assert_eq!(extract_text_content(&serde_json::json!({"a": 1})), "");
    }
}
