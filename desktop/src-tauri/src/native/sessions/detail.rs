//! `GET /api/claude-sessions/{id}` — one session's full message history.
//!
//! Mirrors `GetSessionDetail` / `readSessionDetail` (`internal/claudesessions/scanner.go`)
//! and the patching `handleGetClaudeSession` does on top of it
//! (`internal/api/claude_sessions.go`).
//!
//! **This is scanner work, not a SQLite read.** The detail re-reads the
//! session's own JSONL every time, which is why it waited for the scanner port:
//! the transcript carries the messages, the token counts and the session's own
//! metadata, and reading it means the detail is correct even for a session the
//! scanner has not reached yet.
//!
//! Four things come from the cache instead, and each for its own reason:
//!
//! - **`custom_title` and `is_favorite`** are the only two columns the user
//!   typed. They are not in the transcript at all.
//! - **The native and AI titles** are read *only as a fallback*, when the
//!   transcript carried none. Reading them unconditionally would blank them for
//!   a session Claude Code has just titled and the scanner has not re-read.
//! - **Cost** is accumulated per assistant message during a scan and stored
//!   (#188); a re-read has no per-message pricing context to recompute it from.
//!   A session the scanner has not reached keeps the zero value, which the UI
//!   shows as $0.00 rather than as a wrong figure.
//! - **Sub-agents** live in sibling transcripts under `<session-id>/subagents/`,
//!   so they come from `claude_subagent_cache` rather than from this file.
//!
//! Two asymmetries in `readSessionDetail` that look like bugs and are not, and
//! which this reproduces exactly:
//!
//! 1. **A sidechain *user* event is skipped; a sidechain *assistant* event is
//!    not.** Only `processDetailUserEvent` tests the flag, so delegated
//!    assistant messages appear in `messages` — with `is_sidechain` never set,
//!    because nothing assigns it.
//! 2. **`preview` here is the first user message with any text**, which is a
//!    weaker rule than the scanner's turn-aware preview. The list and the
//!    detail can therefore disagree about the preview of the same session, and
//!    the detail's is the one the transcript just produced.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::summary::{SessionCost, SessionSummary, TokenUsage};
use crate::native::insights::transcript::{self, Event};
use crate::native::scanner::summary_file;
use crate::native::{gopath, settings};

/// `previewMaxRunes`.
const PREVIEW_MAX_CHARS: usize = 120;

/// One rendered conversation event. Mirrors `claudesessions.ClaudeMessage`.
#[derive(Debug, Clone, Serialize)]
pub struct SessionMessage {
    pub uuid: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub parent_uuid: String,
    /// "user" or "assistant".
    #[serde(rename = "type")]
    pub message_type: String,
    pub timestamp: crate::native::gotime::GoTime,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// Plain text, for user messages.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<NormalizedBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub git_branch: String,
    /// Never set by the detail reader — see this module's header. Present
    /// because the field is on the wire and `omitempty` hides it either way.
    #[serde(skip_serializing_if = "is_false")]
    pub is_sidechain: bool,
    /// Reserved on the Go side too: the `progress` events that once nested here
    /// no longer exist, so it is always absent.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SessionMessage>,
}

/// A content block normalized to Agento's rendering format. Mirrors
/// `claudesessions.NormalizedBlock`.
///
/// A thinking block's text lands in `text`, not in a `thinking` field — that is
/// Agento's stored `MessageBlock` shape, and the frontend renders both the same
/// way.
#[derive(Debug, Clone, Serialize)]
pub struct NormalizedBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Carried verbatim through `compact`, so a stored `{"z":1.50,"a":1}` ships
    /// exactly that rather than a re-sorted, re-spelled copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Box<RawValue>>,
}

/// One todo from the session's todo list. Mirrors `claudesessions.ClaudeTodo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTodo {
    #[serde(default)]
    pub content: String,
    /// "completed", "in_progress" or "pending".
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub active_form: String,
}

/// One delegated sub-agent run. Mirrors `claudesessions.ClaudeSubagent`.
#[derive(Debug, Clone, Serialize)]
pub struct Subagent {
    pub agent_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub agent_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tool_use_id: String,
    pub start_time: crate::native::gotime::GoTime,
    pub last_activity: crate::native::gotime::GoTime,
    pub message_count: i64,
    pub event_count: i64,
    pub usage: TokenUsage,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
}

/// The response body. Mirrors `claudesessions.ClaudeSessionDetail`, whose
/// embedded `ClaudeSessionSummary` flattens into the same object — hence
/// `#[serde(flatten)]` rather than a nested key.
#[derive(Debug, Clone, Serialize)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub messages: Vec<SessionMessage>,
    pub todos: Vec<SessionTodo>,
    pub subagents: Vec<Subagent>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Locate a session's transcript across every configured config dir.
///
/// Returns the **config dir** alongside the path, not just the path: the
/// session's todo list lives under that same dir's `todos/`, and resolving it
/// against the default dir would return another account's todos, or none.
///
/// The id is validated here because this is the one place it becomes a
/// filesystem path — a route parameter containing `..` would otherwise walk out
/// of the projects directory.
pub fn find_session_file(dirs: &[String], session_id: &str) -> Option<(String, String, PathBuf)> {
    if !is_valid_session_id(session_id) {
        return None;
    }
    for dir in dirs {
        let projects_dir = Path::new(dir).join("projects");
        let Ok(entries) = std::fs::read_dir(&projects_dir) else {
            continue;
        };
        // `os.ReadDir` returns entries **sorted by filename** and
        // `std::fs::read_dir` does not. It decides which project directory wins
        // when one session id exists under two of them — rare, but the answer
        // has to be the same one every time and the same one Go gives.
        let mut names: Vec<String> = entries
            .flatten()
            // `IsDir()` on the Go side, which reads the type bit and does not
            // follow symlinks.
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            let file = projects_dir.join(&name).join(format!("{session_id}.jsonl"));
            if file.exists() {
                return Some((
                    dir.clone(),
                    crate::native::scanner::walk::decode_project_path(&name),
                    file,
                ));
            }
        }
    }
    None
}

/// `validSessionID`: `^[a-zA-Z0-9_-]+$`.
fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Read one session's detail, or `None` when no config dir holds it.
pub fn get(db_path: &Path, session_id: &str) -> Result<Option<SessionDetail>, String> {
    let conn = crate::native::db::open_read_only(db_path)?;
    let dirs = settings::load(&conn).indexed_config_dirs;

    let Some((config_dir, project_path, file)) = find_session_file(&dirs, session_id) else {
        return Ok(None);
    };

    let mut detail = read_detail(&config_dir, session_id, &project_path, &file)?;
    patch_from_cache(&conn, session_id, &mut detail);
    Ok(Some(detail))
}

/// `readSessionDetail`: the transcript, and only the transcript.
fn read_detail(
    config_dir: &str,
    session_id: &str,
    project_path: &str,
    file: &Path,
) -> Result<SessionDetail, String> {
    let events = transcript::read(file)?;

    let mut summary = SessionSummary {
        session_id: session_id.to_string(),
        project_path: project_path.to_string(),
        ..Default::default()
    };
    let mut messages: Vec<SessionMessage> = Vec::new();
    let mut range = TimeRange::default();

    for ev in &events {
        if ev.event_type == "file-history-snapshot" {
            continue;
        }
        // The same denylist the summary read uses, so the detail's start/end
        // cannot disagree with the list's for one session.
        if summary_file::bounds_session_time_range(&ev.event_type) {
            range.update(ev.timestamp);
        }
        summary_file::update_metadata_from_event(&mut summary, ev);
        process_event(&mut summary, &mut messages, ev);
    }

    // Both default to the zero time when the transcript carried no bounding
    // event at all, which is `0001-01-01T00:00:00Z` on the wire — Go's zero
    // `time.Time`, not an omitted key.
    summary.start_time = go_time(range.start);
    summary.last_activity = go_time(range.last);
    summary.preview = derive_preview(&messages);

    Ok(SessionDetail {
        summary,
        messages,
        todos: load_todos(config_dir, session_id),
        subagents: Vec::new(),
    })
}

fn process_event(summary: &mut SessionSummary, messages: &mut Vec<SessionMessage>, ev: &Event) {
    match ev.event_type.as_str() {
        "user" => process_user_event(summary, messages, ev),
        "assistant" => process_assistant_event(summary, messages, ev),
        // The detail reader walks the same file as the summary reader, so it
        // collects the session's own metadata directly rather than reading it
        // back from the cache.
        "pr-link" => summary_file::add_summary_pr_link(summary, ev),
        "system" => summary_file::add_summary_compaction(summary, ev),
        _ => summary_file::apply_session_metadata(summary, ev),
    }
}

fn process_user_event(
    summary: &mut SessionSummary,
    messages: &mut Vec<SessionMessage>,
    ev: &Event,
) {
    // Sidechain user turns belong to delegated sub-agents, read separately.
    if ev.is_sidechain {
        return;
    }
    let content = ev
        .message
        .as_ref()
        .map(|m| transcript::extract_text_content(&m.content))
        .unwrap_or_default();

    // Every event stays in the rendered list; only the counters distinguish
    // genuine turns from `tool_result` carriers.
    summary.event_count += 1;
    if let Some(m) = &ev.message {
        if transcript::is_user_turn_content(&m.content) {
            summary.message_count += 1;
        }
    }

    messages.push(SessionMessage {
        uuid: ev.uuid.clone(),
        parent_uuid: ev.parent_uuid.clone(),
        message_type: "user".to_string(),
        timestamp: go_time(ev.timestamp),
        role: "user".to_string(),
        content,
        blocks: Vec::new(),
        usage: None,
        git_branch: ev.git_branch.clone(),
        is_sidechain: false,
        children: Vec::new(),
    });
}

fn process_assistant_event(
    summary: &mut SessionSummary,
    messages: &mut Vec<SessionMessage>,
    ev: &Event,
) {
    let mut msg = SessionMessage {
        uuid: ev.uuid.clone(),
        parent_uuid: ev.parent_uuid.clone(),
        message_type: "assistant".to_string(),
        timestamp: go_time(ev.timestamp),
        role: "assistant".to_string(),
        content: String::new(),
        blocks: Vec::new(),
        usage: None,
        git_branch: ev.git_branch.clone(),
        is_sidechain: false,
        children: Vec::new(),
    };

    if let Some(m) = &ev.message {
        if summary.model.is_empty() && !m.model.is_empty() {
            summary.model = m.model.clone();
        }
        if let Some(u) = &m.usage {
            let (five_min, one_hour) = u.split_cache_tiers();
            let usage = TokenUsage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cache_creation_tokens: u.cache_creation_input_tokens,
                cache_creation_5m_tokens: five_min,
                cache_creation_1h_tokens: one_hour,
                cache_read_tokens: u.cache_read_input_tokens,
            };
            summary.usage.input_tokens += usage.input_tokens;
            summary.usage.output_tokens += usage.output_tokens;
            summary.usage.cache_creation_tokens += usage.cache_creation_tokens;
            summary.usage.cache_creation_5m_tokens += usage.cache_creation_5m_tokens;
            summary.usage.cache_creation_1h_tokens += usage.cache_creation_1h_tokens;
            summary.usage.cache_read_tokens += usage.cache_read_tokens;
            msg.usage = Some(usage);
        }
        msg.blocks = normalized_blocks(m.content_raw.as_deref());
    }

    summary.event_count += 1;
    if let Some(m) = &ev.message {
        if transcript::is_assistant_reply(&m.content) {
            summary.message_count += 1;
        }
    }
    messages.push(msg);
}

/// `populateAssistantBlocks` + `normalizeBlock`: only the three renderable
/// types survive, and anything else is dropped rather than passed through.
fn normalized_blocks(content: Option<&RawValue>) -> Vec<NormalizedBlock> {
    let Some(content) = content else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for b in transcript::parse_content_blocks_raw(content) {
        match b.block_type.as_str() {
            "thinking" => out.push(NormalizedBlock {
                block_type: "thinking".to_string(),
                text: b.thinking,
                id: String::new(),
                name: String::new(),
                input: None,
            }),
            "text" => out.push(NormalizedBlock {
                block_type: "text".to_string(),
                text: b.text,
                id: String::new(),
                name: String::new(),
                input: None,
            }),
            "tool_use" => out.push(NormalizedBlock {
                block_type: "tool_use".to_string(),
                text: String::new(),
                id: b.id,
                name: b.name,
                input: b.input.map(crate::native::gojson::compact_raw),
            }),
            _ => {}
        }
    }
    out
}

/// `derivePreview`: the first user message carrying any text.
///
/// Deliberately weaker than the scanner's preview, which distinguishes a
/// genuine turn from an injected wrapper. Both are Go's; they are two rules,
/// not one rule applied twice.
fn derive_preview(messages: &[SessionMessage]) -> String {
    for msg in messages {
        if msg.role == "user" && !msg.content.is_empty() {
            return summary_file::truncate_chars(&msg.content, PREVIEW_MAX_CHARS);
        }
    }
    String::new()
}

/// `loadTodos`: `<config dir>/todos/<id>-agent-<id>.json`, absent or malformed
/// reading as none.
fn load_todos(config_dir: &str, session_id: &str) -> Vec<SessionTodo> {
    if !is_valid_session_id(session_id) {
        return Vec::new();
    }
    let dir = if config_dir.is_empty() {
        settings::default_claude_config_dir()
    } else {
        config_dir.to_string()
    };
    let path = gopath::join(&[
        &dir,
        "todos",
        &format!("{session_id}-agent-{session_id}.json"),
    ]);
    let Ok(raw) = std::fs::read(&path) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<SessionTodo>>(&raw).unwrap_or_default()
}

/// The handler's own work: the columns the transcript cannot carry.
fn patch_from_cache(conn: &Connection, session_id: &str, detail: &mut SessionDetail) {
    detail.summary.custom_title = scalar(conn, "custom_title", session_id).unwrap_or_default();
    detail.summary.is_favorite = conn
        .query_row(
            "SELECT is_favorite FROM claude_session_cache WHERE session_id = ?1",
            [session_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .unwrap_or_default()
        .is_some_and(|v| v != 0);

    // Only as a fallback: reading these unconditionally would blank a title
    // Claude Code has just written but the scanner has not re-read.
    if detail.summary.native_title.is_empty() && detail.summary.ai_title.is_empty() {
        if let Some((native, ai)) = conn
            .query_row(
                "SELECT native_title, ai_title FROM claude_session_cache WHERE session_id = ?1",
                [session_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
            .unwrap_or_default()
        {
            detail.summary.native_title = native;
            detail.summary.ai_title = ai;
        }
    }
    detail.summary.display_title = super::summary::resolve_display_title(&detail.summary);

    if let Some(cached) = cached_costs(conn, session_id) {
        detail.summary.cost = cached.cost;
        detail.summary.subagent_cost = cached.subagent_cost;
        detail.summary.unpriced_models = cached.unpriced_models;
        detail.summary.unpriced_tokens = cached.unpriced_tokens;
    }

    detail.subagents = list_subagents(conn, session_id);
    detail.summary.subagent_count = detail.subagents.len() as i64;
    detail.summary.subagent_usage = TokenUsage::default();
    for sa in &detail.subagents {
        detail.summary.subagent_usage.input_tokens += sa.usage.input_tokens;
        detail.summary.subagent_usage.output_tokens += sa.usage.output_tokens;
        detail.summary.subagent_usage.cache_creation_tokens += sa.usage.cache_creation_tokens;
        detail.summary.subagent_usage.cache_read_tokens += sa.usage.cache_read_tokens;
    }
}

fn scalar(conn: &Connection, column: &str, session_id: &str) -> Option<String> {
    conn.query_row(
        &format!("SELECT {column} FROM claude_session_cache WHERE session_id = ?1"),
        [session_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .unwrap_or_default()
}

/// What `GetSummary` is consulted for. The four fields the handler copies, and
/// no more — the rest of the cached row would overwrite figures the transcript
/// just produced.
struct CachedCosts {
    cost: SessionCost,
    subagent_cost: SessionCost,
    unpriced_models: Vec<String>,
    unpriced_tokens: i64,
}

fn cached_costs(conn: &Connection, session_id: &str) -> Option<CachedCosts> {
    let sql = format!(
        "{}{}\n\tWHERE c.session_id = ?1",
        super::summary::SUMMARY_COLUMNS,
        super::summary::SUMMARY_SOURCE
    );
    let cached = conn
        .query_row(&sql, [session_id], super::summary::scan)
        .optional()
        .unwrap_or_else(|e| {
            log::warn!("native session detail: reading cached summary failed: {e}");
            None
        })?;
    Some(CachedCosts {
        cost: cached.cost,
        subagent_cost: cached.subagent_cost,
        unpriced_models: cached.unpriced_models,
        unpriced_tokens: cached.unpriced_tokens,
    })
}

/// `ListSubagents`: every delegated run, oldest first. An unreadable table is
/// an empty list rather than a failure, matching the Go accessor.
fn list_subagents(conn: &Connection, session_id: &str) -> Vec<Subagent> {
    let sql = "SELECT agent_id, agent_type, description, tool_use_id,
                      start_time, last_activity, message_count, event_count,
                      input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                      cache_creation_5m_tokens, cache_creation_1h_tokens, model
               FROM claude_subagent_cache
               WHERE parent_session_id = ?1
               ORDER BY start_time";
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            log::warn!("native session detail: preparing sub-agent query failed: {e}");
            return Vec::new();
        }
    };
    let rows = stmt.query_map([session_id], |row| {
        let start: String = row.get(4)?;
        let last: String = row.get(5)?;
        Ok(Subagent {
            agent_id: row.get(0)?,
            agent_type: row.get(1)?,
            description: row.get(2)?,
            tool_use_id: row.get(3)?,
            start_time: parse_time(&start, 4)?,
            last_activity: parse_time(&last, 5)?,
            message_count: row.get(6)?,
            event_count: row.get(7)?,
            usage: TokenUsage {
                input_tokens: row.get(8)?,
                output_tokens: row.get(9)?,
                cache_creation_tokens: row.get(10)?,
                cache_read_tokens: row.get(11)?,
                cache_creation_5m_tokens: row.get(12)?,
                cache_creation_1h_tokens: row.get(13)?,
            },
            model: row.get(14)?,
        })
    });
    match rows {
        Ok(rows) => rows.filter_map(Result::ok).collect(),
        Err(e) => {
            log::warn!("native session detail: listing sub-agents failed: {e}");
            Vec::new()
        }
    }
}

fn parse_time(text: &str, index: usize) -> rusqlite::Result<crate::native::gotime::GoTime> {
    crate::native::gotime::GoTime::parse_any(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e)),
        )
    })
}

/// A transcript timestamp as it travels, or the zero `time.Time` when absent.
fn go_time(at: Option<chrono::DateTime<chrono::Utc>>) -> crate::native::gotime::GoTime {
    at.map(|t| crate::native::gotime::GoTime(t.fixed_offset()))
        .unwrap_or_default()
}

/// The start/last bounds of a transcript, as `timeRange` keeps them.
#[derive(Default)]
struct TimeRange {
    start: Option<chrono::DateTime<chrono::Utc>>,
    last: Option<chrono::DateTime<chrono::Utc>>,
}

impl TimeRange {
    fn update(&mut self, at: Option<chrono::DateTime<chrono::Utc>>) {
        let Some(at) = at else { return };
        if self.start.map_or(true, |s| at < s) {
            self.start = Some(at);
        }
        if self.last.map_or(true, |l| at > l) {
            self.last = Some(at);
        }
    }
}

/// Placeholder so `BTreeMap` stays imported for the summary's by-model maps,
/// which the detail leaves empty.
#[allow(dead_code)]
type ByModel = BTreeMap<String, TokenUsage>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_id_that_could_become_a_path_is_rejected() {
        assert!(is_valid_session_id("abc-123_DEF"));
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id("../../etc/passwd"));
        assert!(!is_valid_session_id("abc/def"));
        assert!(!is_valid_session_id("abc.jsonl"));
    }

    /// A `tool_use` input keeps the key order and number spelling it was
    /// written with — the whole reason it travels as a `RawValue`.
    #[test]
    fn a_tool_use_input_is_carried_verbatim() {
        let content = RawValue::from_string(
            r#"[{"type":"tool_use","id":"t1","name":"Bash","input":{"z":1.50,"a":[1,2]}}]"#
                .to_string(),
        )
        .expect("content");
        let blocks = normalized_blocks(Some(&content));
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].input.as_ref().map(|v| v.get()),
            Some(r#"{"z":1.50,"a":[1,2]}"#)
        );
    }

    /// `<`, `>` and `&` are escaped by `compact`, wherever they appear.
    #[test]
    fn a_tool_use_input_is_html_escaped_the_way_marshal_escapes() {
        let content = RawValue::from_string(
            r#"[{"type":"tool_use","id":"t","name":"Bash","input":{"cmd":"a < b && c"}}]"#
                .to_string(),
        )
        .expect("content");
        let blocks = normalized_blocks(Some(&content));
        // `compact` escapes `<`, `>` and `&` wherever they appear, exactly as
        // `json.Marshal` does — so the wire form is the escaped one.
        assert_eq!(
            blocks[0].input.as_ref().map(|v| v.get()),
            Some(r#"{"cmd":"a \u003c b \u0026\u0026 c"}"#)
        );
    }

    /// Thinking text lands in `text`, and an unrenderable block is dropped
    /// rather than passed through with an empty type.
    #[test]
    fn only_the_three_renderable_block_types_survive() {
        let content = RawValue::from_string(
            r#"[{"type":"thinking","thinking":"hmm"},
                {"type":"text","text":"hi"},
                {"type":"tool_result","tool_use_id":"t1"}]"#
                .to_string(),
        )
        .expect("content");
        let blocks = normalized_blocks(Some(&content));
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].block_type, "thinking");
        assert_eq!(blocks[0].text, "hmm");
        assert_eq!(blocks[1].block_type, "text");
    }

    #[test]
    fn the_preview_is_the_first_user_message_with_text() {
        let msg = |role: &str, content: &str| SessionMessage {
            uuid: String::new(),
            parent_uuid: String::new(),
            message_type: role.to_string(),
            timestamp: Default::default(),
            role: role.to_string(),
            content: content.to_string(),
            blocks: Vec::new(),
            usage: None,
            git_branch: String::new(),
            is_sidechain: false,
            children: Vec::new(),
        };
        // An empty user message is skipped, and an assistant one never counts.
        let messages = vec![
            msg("user", ""),
            msg("assistant", "not this"),
            msg("user", "the preview"),
        ];
        assert_eq!(derive_preview(&messages), "the preview");
        assert_eq!(derive_preview(&[]), "");

        let long = "x".repeat(200);
        assert_eq!(
            derive_preview(&[msg("user", &long)]).chars().count(),
            PREVIEW_MAX_CHARS + 1,
            "truncation appends one ellipsis"
        );
    }
}
