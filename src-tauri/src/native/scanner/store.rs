//! Reading and writing the cache tables, ported from the persistence half of
//! `internal/claudesessions/scanner.go`.
//!
//! ## Two columns are never written
//!
//! `custom_title` and `is_favorite` appear in neither the insert list nor the
//! `DO UPDATE SET` list, deliberately: they are the user's, and a rescan must
//! preserve them. `native_title` and `ai_title` *are* refreshed, because they
//! mirror Claude Code's own title events — including a rename that clears one —
//! and `custom_title` wins over both when the display title is resolved.
//!
//! ## Two encodings that are not what they look like
//!
//! `cost_by_model` is JSON, but an empty map stores as `""` rather than `"{}"`.
//! `unpriced_models` is **not** JSON at all: it is newline-joined, because a
//! model id may contain a slash but never a newline.
//!
//! ## Nothing here touches the application's own database
//!
//! Every function takes a connection the caller opened. The app's own handle is
//! read-only ([`crate::native::db`]), so the only connections these can be
//! handed are the scratch databases the tests build. See the module docs on
//! [`super`] for why the port stops short of writing for real.

use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::{params, Connection, Transaction};

use crate::native::gotime::{to_go_string_utc, GoTime};
use crate::native::sessions::summary::SessionSummary;

use super::diff::CachedEntry;
use super::walk::DiskFile;

/// Loads every cached row from both tables, keyed by file path.
///
/// The union is what the diff needs: one path either has a row or it does not,
/// whichever table it lives in.
pub fn load_cached_entries(conn: &Connection) -> Result<HashMap<PathBuf, CachedEntry>, String> {
    let mut cached = HashMap::new();
    load_from(conn, &mut cached, false)?;
    load_from(conn, &mut cached, true)?;
    Ok(cached)
}

fn load_from(
    conn: &Connection,
    cached: &mut HashMap<PathBuf, CachedEntry>,
    is_subagent: bool,
) -> Result<(), String> {
    let sql = if is_subagent {
        "SELECT file_path, file_mtime, COALESCE(config_dir, ''), parent_session_id, agent_id
         FROM claude_subagent_cache"
    } else {
        "SELECT file_path, file_mtime, COALESCE(config_dir, ''), session_id, project_path
         FROM claude_session_cache"
    };

    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let file_path: String = r.get(0)?;
            let mtime: String = r.get(1)?;
            let config_dir: String = r.get(2)?;
            let key_a: String = r.get(3)?;
            let key_b: String = r.get(4)?;
            Ok(CachedEntry {
                file_path: PathBuf::from(file_path),
                mtime: GoTime::parse_any(&mtime)
                    .map(|t| t.instant())
                    .unwrap_or(chrono::DateTime::UNIX_EPOCH),
                is_subagent,
                config_dir,
                session_id: key_a,
                project_path: if is_subagent {
                    String::new()
                } else {
                    key_b.clone()
                },
                agent_id: if is_subagent { key_b } else { String::new() },
            })
        })
        .map_err(|e| e.to_string())?;

    for row in rows.flatten() {
        cached.insert(row.file_path.clone(), row);
    }
    Ok(())
}

/// Writes one session row.
///
/// The conflict target is the table's primary key, so a claim shift — the same
/// row arriving under a new path — updates in place and the stale path is
/// reconciled away by the delete pass afterwards.
pub fn insert_cache_row(
    tx: &Transaction,
    file: &DiskFile,
    s: &SessionSummary,
) -> Result<(), String> {
    tx.execute(
        "INSERT INTO claude_session_cache (
             session_id, project_path, file_path, file_mtime,
             preview, start_time, last_activity, message_count, event_count,
             input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
             cache_creation_5m_tokens, cache_creation_1h_tokens,
             git_branch, model, cwd, native_title, ai_title,
             agent_name, permission_mode, mode, relocated_cwd,
             worktree_name, worktree_branch, original_branch,
             compaction_count, dropped_tokens,
             input_cost_usd, output_cost_usd, cache_read_cost_usd,
             cache_write_cost_usd, total_cost_usd, unpriced_models, unpriced_tokens,
             cost_by_model, active_duration_ms, config_dir
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
             ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
             ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
             ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39
         )
         ON CONFLICT(session_id, project_path) DO UPDATE SET
             file_path = excluded.file_path,
             file_mtime = excluded.file_mtime,
             preview = excluded.preview,
             start_time = excluded.start_time,
             last_activity = excluded.last_activity,
             message_count = excluded.message_count,
             event_count = excluded.event_count,
             input_tokens = excluded.input_tokens,
             output_tokens = excluded.output_tokens,
             cache_creation_tokens = excluded.cache_creation_tokens,
             cache_read_tokens = excluded.cache_read_tokens,
             cache_creation_5m_tokens = excluded.cache_creation_5m_tokens,
             cache_creation_1h_tokens = excluded.cache_creation_1h_tokens,
             git_branch = excluded.git_branch,
             model = excluded.model,
             cwd = excluded.cwd,
             native_title = excluded.native_title,
             ai_title = excluded.ai_title,
             agent_name = excluded.agent_name,
             permission_mode = excluded.permission_mode,
             mode = excluded.mode,
             relocated_cwd = excluded.relocated_cwd,
             worktree_name = excluded.worktree_name,
             worktree_branch = excluded.worktree_branch,
             original_branch = excluded.original_branch,
             compaction_count = excluded.compaction_count,
             dropped_tokens = excluded.dropped_tokens,
             input_cost_usd = excluded.input_cost_usd,
             output_cost_usd = excluded.output_cost_usd,
             cache_read_cost_usd = excluded.cache_read_cost_usd,
             cache_write_cost_usd = excluded.cache_write_cost_usd,
             total_cost_usd = excluded.total_cost_usd,
             unpriced_models = excluded.unpriced_models,
             unpriced_tokens = excluded.unpriced_tokens,
             cost_by_model = excluded.cost_by_model,
             active_duration_ms = excluded.active_duration_ms,
             config_dir = excluded.config_dir",
        params![
            s.session_id,
            s.project_path,
            file.file_path.to_string_lossy(),
            to_go_string_utc(GoTime(file.mtime.fixed_offset())),
            s.preview,
            to_go_string_utc(s.start_time),
            to_go_string_utc(s.last_activity),
            s.message_count,
            s.event_count,
            s.usage.input_tokens,
            s.usage.output_tokens,
            s.usage.cache_creation_tokens,
            s.usage.cache_read_tokens,
            s.usage.cache_creation_5m_tokens,
            s.usage.cache_creation_1h_tokens,
            s.git_branch,
            s.model,
            s.cwd,
            s.native_title,
            s.ai_title,
            s.agent_name,
            s.permission_mode,
            s.mode,
            s.relocated_cwd,
            s.worktree_name,
            s.worktree_branch,
            s.original_branch,
            s.compaction_count,
            s.dropped_tokens,
            s.cost.input_usd,
            s.cost.output_usd,
            s.cost.cache_read_usd,
            s.cost.cache_write_usd,
            s.cost.total_usd,
            encode_unpriced_models(&s.unpriced_models),
            s.unpriced_tokens,
            encode_cost_by_model(s),
            s.active_duration_ms,
            file.config_dir,
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("writing session row {}: {e}", s.session_id))
}

/// Replaces a session's linked pull requests.
///
/// A full replace rather than a merge, so a PR link removed upstream disappears
/// here too. It runs in the same transaction as the session row: the row
/// carries the file's mtime, so a PR write failing after the row committed
/// would leave the file looking unchanged to the next diff and the PR rows
/// would never be rebuilt.
pub fn replace_pr_rows(tx: &Transaction, s: &SessionSummary) -> Result<(), String> {
    tx.execute(
        "DELETE FROM claude_session_pr WHERE session_id = ?1",
        params![s.session_id],
    )
    .map_err(|e| format!("clearing PR rows for {}: {e}", s.session_id))?;

    for pr in &s.prs {
        tx.execute(
            "INSERT INTO claude_session_pr
                 (session_id, pr_url, pr_number, pr_repository, first_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                s.session_id,
                pr.pr_url,
                pr.pr_number,
                pr.pr_repository,
                to_go_string_utc(pr.first_seen_at),
            ],
        )
        .map_err(|e| format!("writing PR row for {}: {e}", s.session_id))?;
    }
    Ok(())
}

/// Metadata from a sub-agent transcript's `.meta.json` sidecar.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SubagentMeta {
    #[serde(default, rename = "agentType")]
    pub agent_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "toolUseId")]
    pub tool_use_id: String,
}

/// Reads the sidecar beside a sub-agent transcript.
///
/// A missing or malformed sidecar yields the zero value rather than an error:
/// the transcript is the data, and the sidecar is a label for it.
pub fn read_subagent_meta(transcript: &std::path::Path) -> SubagentMeta {
    // Trim the suffix and append, rather than swapping extensions: an agent id
    // containing a dot — `agent-1.2.jsonl` — would lose its last component to
    // `with_extension`, and the sidecar would be looked for under a name that
    // does not exist.
    let path = PathBuf::from(format!(
        "{}.meta.json",
        transcript.to_string_lossy().trim_end_matches(".jsonl")
    ));

    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Writes one sub-agent row.
///
/// This table stores a subset of the session columns — no preview, no titles,
/// no `cost_by_model`, no `project_path`.
pub fn upsert_subagent_row(
    tx: &Transaction,
    file: &DiskFile,
    s: &SessionSummary,
    meta: &SubagentMeta,
) -> Result<(), String> {
    tx.execute(
        "INSERT INTO claude_subagent_cache (
             parent_session_id, agent_id, file_path, file_mtime,
             agent_type, description, tool_use_id,
             start_time, last_activity, message_count, event_count,
             input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
             cache_creation_5m_tokens, cache_creation_1h_tokens,
             model,
             input_cost_usd, output_cost_usd, cache_read_cost_usd,
             cache_write_cost_usd, total_cost_usd, unpriced_models, unpriced_tokens,
             active_duration_ms, config_dir
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
             ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
             ?21, ?22, ?23, ?24, ?25, ?26, ?27
         )
         ON CONFLICT(parent_session_id, agent_id) DO UPDATE SET
             file_path = excluded.file_path,
             file_mtime = excluded.file_mtime,
             agent_type = excluded.agent_type,
             description = excluded.description,
             tool_use_id = excluded.tool_use_id,
             start_time = excluded.start_time,
             last_activity = excluded.last_activity,
             message_count = excluded.message_count,
             event_count = excluded.event_count,
             input_tokens = excluded.input_tokens,
             output_tokens = excluded.output_tokens,
             cache_creation_tokens = excluded.cache_creation_tokens,
             cache_read_tokens = excluded.cache_read_tokens,
             cache_creation_5m_tokens = excluded.cache_creation_5m_tokens,
             cache_creation_1h_tokens = excluded.cache_creation_1h_tokens,
             model = excluded.model,
             input_cost_usd = excluded.input_cost_usd,
             output_cost_usd = excluded.output_cost_usd,
             cache_read_cost_usd = excluded.cache_read_cost_usd,
             cache_write_cost_usd = excluded.cache_write_cost_usd,
             total_cost_usd = excluded.total_cost_usd,
             unpriced_models = excluded.unpriced_models,
             unpriced_tokens = excluded.unpriced_tokens,
             active_duration_ms = excluded.active_duration_ms,
             config_dir = excluded.config_dir",
        params![
            file.session_id,
            file.agent_id,
            file.file_path.to_string_lossy(),
            to_go_string_utc(GoTime(file.mtime.fixed_offset())),
            meta.agent_type,
            meta.description,
            meta.tool_use_id,
            to_go_string_utc(s.start_time),
            to_go_string_utc(s.last_activity),
            s.message_count,
            s.event_count,
            s.usage.input_tokens,
            s.usage.output_tokens,
            s.usage.cache_creation_tokens,
            s.usage.cache_read_tokens,
            s.usage.cache_creation_5m_tokens,
            s.usage.cache_creation_1h_tokens,
            s.model,
            s.cost.input_usd,
            s.cost.output_usd,
            s.cost.cache_read_usd,
            s.cost.cache_write_usd,
            s.cost.total_usd,
            encode_unpriced_models(&s.unpriced_models),
            s.unpriced_tokens,
            s.active_duration_ms,
            file.config_dir,
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("writing sub-agent row {}: {e}", file.agent_id))
}

/// Removes one cached row and, for a session, its pull requests.
///
/// The PR delete resolves the session id *through* the session row, so it must
/// run before that row is removed.
pub fn delete_cached_file(tx: &Transaction, entry: &CachedEntry) -> Result<(), String> {
    let path = entry.file_path.to_string_lossy().into_owned();

    if !entry.is_subagent {
        tx.execute(
            "DELETE FROM claude_session_pr WHERE session_id IN
                 (SELECT session_id FROM claude_session_cache WHERE file_path = ?1)",
            params![path],
        )
        .map_err(|e| format!("clearing PR rows for {path}: {e}"))?;
    }

    let table = if entry.is_subagent {
        "claude_subagent_cache"
    } else {
        "claude_session_cache"
    };
    tx.execute(
        &format!("DELETE FROM {table} WHERE file_path = ?1"),
        params![path],
    )
    .map(|_| ())
    .map_err(|e| format!("deleting {path}: {e}"))
}

/// Newline-joined, not JSON: a model id may contain a slash but never a
/// newline.
fn encode_unpriced_models(models: &[String]) -> String {
    models.join("\n")
}

/// JSON, but an empty map stores as `""` rather than `"{}"` — the sessions list
/// reads both as "nothing".
fn encode_cost_by_model(s: &SessionSummary) -> String {
    if s.cost_by_model.is_empty() {
        return String::new();
    }
    serde_json::to_string(&s.cost_by_model).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_cost_map_stores_as_empty_rather_than_an_empty_object() {
        let s = SessionSummary::default();
        assert_eq!(encode_cost_by_model(&s), "");
    }

    #[test]
    fn unpriced_models_are_newline_joined_not_json() {
        // A model id may contain a slash — `moonshot/kimi-k2` — but never a
        // newline, which is why this is not JSON.
        assert_eq!(
            encode_unpriced_models(&["a/b".to_string(), "c".to_string()]),
            "a/b\nc"
        );
        assert_eq!(encode_unpriced_models(&[]), "");
    }

    #[test]
    fn a_missing_sidecar_yields_the_zero_value() {
        let meta = read_subagent_meta(std::path::Path::new("/nonexistent/agent-1.jsonl"));
        assert_eq!(meta.agent_type, "");
        assert_eq!(meta.description, "");
    }

    #[test]
    fn a_sidecar_is_read_from_beside_its_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("agent-1.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        std::fs::write(
            dir.path().join("agent-1.meta.json"),
            r#"{"agentType":"Explore","description":"map it","toolUseId":"tu_1"}"#,
        )
        .unwrap();

        let meta = read_subagent_meta(&transcript);
        assert_eq!(meta.agent_type, "Explore");
        assert_eq!(meta.description, "map it");
        assert_eq!(meta.tool_use_id, "tu_1");
    }

    #[test]
    fn an_agent_id_containing_a_dot_still_finds_its_sidecar() {
        // `with_extension` would turn `agent-1.2.jsonl` into `agent-1.meta.json`
        // and quietly find nothing.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("agent-1.2.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        std::fs::write(
            dir.path().join("agent-1.2.meta.json"),
            r#"{"agentType":"Explore"}"#,
        )
        .unwrap();

        assert_eq!(read_subagent_meta(&transcript).agent_type, "Explore");
    }

    #[test]
    fn a_malformed_sidecar_degrades_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("agent-1.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        std::fs::write(dir.path().join("agent-1.meta.json"), "not json").unwrap();

        // The transcript is the data; the sidecar is only a label for it.
        assert_eq!(read_subagent_meta(&transcript).agent_type, "");
    }
}
