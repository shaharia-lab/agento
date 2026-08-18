//! One inbound Telegram message, from matched rule to sent reply. Mirrors
//! `Dispatcher.processTelegramUpdate` and `executeAndReply`
//! (`internal/trigger/dispatcher.go`).
//!
//! # The fourth caller of the agent runner
//!
//! After chat (#276), the scheduler (#275) and `agento ask`, this is the fourth
//! place an agent runs — and the second headless one. It goes through
//! [`crate::native::agent_run`] for that reason: the one-shot-vs-`Session` trap
//! that made every scheduled run hang lives there now, in one place, so this
//! caller cannot reintroduce it.
//!
//! # Where this differs from the scheduler, and why
//!
//! - **The reply is the product.** A scheduled run's evidence is a
//!   `job_history` row; this one's is a message in a chat. So a failure that the
//!   scheduler records silently must here also *say something* — Go sends
//!   "Sorry, something went wrong." on every failure path, and a user who gets
//!   nothing back cannot tell a broken agent from an ignored message.
//! - **There is no job history at all.** The run is recorded only as chat
//!   messages on a `[Telegram] <rule>` session.
//! - **The timeout is a flat five minutes**, not the task's own — triggers have
//!   no configurable timeout.
//! - **Concurrency is bounded to 5**, not the scheduler's 3, and the bound is
//!   Go's `sem` on the dispatcher rather than a per-run permit.

use std::path::Path;

use tokio::sync::Semaphore;

use super::match_rule::{match_rule, RuleFilters};
use super::receiver::{TelegramMsg, TelegramUpdate};
use crate::native::agent_run;
use crate::native::agents::Agent;

/// `Dispatcher.sem`: `maxConcurrent` in `NewDispatcher`.
const MAX_CONCURRENT: usize = 5;

/// `context.WithTimeout(ctx, 5*time.Minute)` in `executeAndReply`.
const RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// What Go replies with on every failure path.
const ERROR_REPLY: &str = "Sorry, something went wrong.";

fn semaphore() -> &'static Semaphore {
    static SEM: std::sync::OnceLock<Semaphore> = std::sync::OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(MAX_CONCURRENT))
}

/// `HandleTelegramUpdate`: returns immediately, processes in the background,
/// bounded by the semaphore.
///
/// Spawned rather than awaited because the receiver has already answered 200 —
/// Telegram must not be held open for an agent run, and would retry if it were.
pub fn handle_update(
    db_path: &Path,
    integration_id: &str,
    bot_token: &str,
    update: TelegramUpdate,
) {
    let db_path = db_path.to_path_buf();
    let integration_id = integration_id.to_string();
    let bot_token = bot_token.to_string();
    tokio::spawn(async move {
        let Ok(_permit) = semaphore().acquire().await else {
            log::warn!(
                "dispatcher stopped, dropping telegram update integration_id={integration_id:?}"
            );
            return;
        };
        process(&db_path, &integration_id, &bot_token, update).await;
    });
}

/// `processTelegramUpdate`.
async fn process(db_path: &Path, integration_id: &str, bot_token: &str, update: TelegramUpdate) {
    // A non-message update, or one with no text, is not a trigger.
    let Some(msg) = update.message.filter(|m| !m.text.is_empty()) else {
        return;
    };

    // Claimed before the rules are read, so a Telegram retry cannot run the
    // agent twice — see `receiver::claim_update` for why the claim is atomic
    // here where Go's is two statements.
    if !super::receiver::claim_update(db_path, integration_id, update.update_id) {
        return;
    }

    let Some((rule, prompt)) = find_matching_rule(db_path, integration_id, &msg) else {
        return;
    };

    log::info!(
        "trigger rule matched rule_id={:?} rule_name={:?} agent_slug={:?} chat_id={}",
        rule.id,
        rule.name,
        rule.agent_slug,
        msg.chat.id
    );

    execute_and_reply(db_path, bot_token, &msg, &rule, &prompt).await;
}

/// One trigger rule, narrowed to what the dispatcher reads.
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub agent_slug: String,
    pub filters: RuleFilters,
}

/// `findMatchingRule`: the first **enabled** rule that matches, in the order the
/// store returns them (oldest first).
fn find_matching_rule(
    db_path: &Path,
    integration_id: &str,
    msg: &TelegramMsg,
) -> Option<(Rule, String)> {
    let rules = match load_rules(db_path, integration_id) {
        Ok(rules) => rules,
        Err(e) => {
            log::error!("failed to load trigger rules integration_id={integration_id:?} error={e}");
            return None;
        }
    };
    // `fmt.Sprintf("%d", msg.Chat.ID)`.
    let chat_id = msg.chat.id.to_string();
    rules
        .into_iter()
        .find_map(|rule| match_rule(&rule.filters, &msg.text, &chat_id).map(|p| (rule, p)))
}

/// `ListRules`, filtered to the enabled ones as `findMatchingRule` does.
fn load_rules(db_path: &Path, integration_id: &str) -> Result<Vec<Rule>, String> {
    let conn = crate::native::db::open_read_only(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, name, agent_slug, filter_prefix, filter_keywords, filter_chat_ids
             FROM trigger_rules
             WHERE integration_id = ?1 AND enabled = 1
             ORDER BY created_at ASC",
        )
        .map_err(|e| format!("preparing trigger rules query: {e}"))?;

    let rows = stmt
        .query_map([integration_id], |row| {
            let keywords: String = row.get(4)?;
            let chat_ids: String = row.get(5)?;
            Ok(Rule {
                id: row.get(0)?,
                name: row.get(1)?,
                agent_slug: row.get(2)?,
                filters: RuleFilters {
                    prefix: row.get(3)?,
                    keywords: decode_list(&keywords),
                    chat_ids: decode_list(&chat_ids),
                },
            })
        })
        .map_err(|e| format!("querying trigger rules: {e}"))?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("scanning trigger rule: {e}"))?);
    }
    Ok(out)
}

/// A stored `[]string` column. An unparseable or null value is an empty list,
/// which the matcher reads as "no filter" — the same answer Go's zero slice
/// gives.
fn decode_list(raw: &str) -> Vec<String> {
    serde_json::from_str::<Option<Vec<Option<String>>>>(raw)
        .ok()
        .flatten()
        .map(|list| list.into_iter().map(Option::unwrap_or_default).collect())
        .unwrap_or_default()
}

/// `executeAndReply`.
async fn execute_and_reply(
    db_path: &Path,
    bot_token: &str,
    msg: &TelegramMsg,
    rule: &Rule,
    prompt: &str,
) {
    // "typing…" while the agent runs. Best-effort in Go too — the result is
    // discarded.
    super::telegram_api::send_chat_action(bot_token, msg.chat.id).await;

    let agent = match resolve_agent(db_path, &rule.agent_slug) {
        Ok(agent) => agent,
        Err(e) => {
            log::error!(
                "failed to resolve agent for trigger agent_slug={:?} error={e}",
                rule.agent_slug
            );
            send_error_reply(bot_token, msg).await;
            return;
        }
    };

    // Go creates the session with **no** working directory, model or settings
    // profile — a trigger run is not configurable the way a task is.
    let chat_session_id = match create_trigger_session(db_path, rule) {
        Ok(id) => id,
        Err(e) => {
            log::error!("failed to create chat session for trigger: {e}");
            send_error_reply(bot_token, msg).await;
            return;
        }
    };

    let spec = agent_run::headless_spec(db_path, agent, String::new(), String::new());
    let result = agent_run::run_headless(&spec, prompt, RUN_TIMEOUT).await;

    let result = match result {
        Ok(result) => result,
        Err(e) => {
            log::error!(
                "agent execution failed for trigger rule_id={:?} error={e}",
                rule.id
            );
            send_error_reply(bot_token, msg).await;
            // The user turn is still stored, with no answer — so the chat shows
            // what was asked even when nothing came back.
            save_messages(db_path, &chat_session_id, prompt, "");
            return;
        }
    };

    save_messages(db_path, &chat_session_id, prompt, &result.answer);
    update_session_usage(db_path, &chat_session_id, &result);

    // An empty answer still gets a reply — silence would be indistinguishable
    // from the bot being broken.
    let reply = if result.answer.is_empty() {
        "No response generated."
    } else {
        &result.answer
    };
    if let Err(e) =
        super::telegram_api::send_reply(bot_token, msg.chat.id, msg.message_id, reply).await
    {
        log::error!(
            "failed to send telegram reply chat_id={} error={e}",
            msg.chat.id
        );
    }
}

/// `resolveAgent`: the named agent, or a synthesized config carrying only the
/// default model.
///
/// The no-slug branch returns a **synthesized `Agent`** rather than `None`, for
/// the reason the scheduler's does: Go builds a non-nil `config.AgentConfig`,
/// and `resolveToolsAndMCP` gives a non-nil config with empty capabilities all
/// twelve built-in tools where a nil one gets none.
fn resolve_agent(db_path: &Path, agent_slug: &str) -> Result<Agent, String> {
    if !agent_slug.is_empty() {
        return match crate::native::agents::get(db_path, agent_slug) {
            Ok(Some(agent)) => Ok(agent),
            Ok(None) => Err(format!("agent {agent_slug:?} not found")),
            Err(e) => Err(format!("loading agent {agent_slug:?}: {e}")),
        };
    }
    // Go's fallback here is the literal "sonnet" when there is no settings
    // manager, and the stored default otherwise — unlike the scheduler, which
    // has no literal.
    let model = {
        let settings = crate::native::chat::runner::TurnSettings::from_db(db_path);
        let model = settings.default_model();
        if model.is_empty() {
            "sonnet".to_string()
        } else {
            model
        }
    };
    Ok(Agent {
        name: String::new(),
        slug: String::new(),
        description: String::new(),
        model,
        thinking: "adaptive".to_string(),
        permission_mode: String::new(),
        system_prompt: String::new(),
        capabilities: Default::default(),
        claude_config_dir: String::new(),
    })
}

/// The `[Telegram] <rule>` chat a trigger run is recorded in.
fn create_trigger_session(db_path: &Path, rule: &Rule) -> Result<String, String> {
    let mut conn = crate::native::db::open_read_write(db_path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("begin trigger session: {e}"))?;
    let session = crate::native::chats::insert_session(&tx, &rule.agent_slug, "", "", "")
        .map_err(|e| e.message())?;
    tx.commit()
        .map_err(|e| format!("commit trigger session: {e}"))?;

    // Two writes, as Go has them: a failed title update is a warning, not a
    // failed run.
    let title = format!("[Telegram] {}", rule.name);
    if let Err(e) = conn.execute(
        "UPDATE chat_sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![title, crate::native::gotime::now_go_text(), session.id],
    ) {
        log::warn!("failed to update session title: {e}");
    }
    Ok(session.id)
}

/// `saveSessionMessages`.
///
/// **The user turn is stored even when there is no answer** — that is Go's
/// shape, and it is what makes a failed trigger visible in the chat rather than
/// leaving an empty session.
fn save_messages(db_path: &Path, chat_session_id: &str, prompt: &str, answer: &str) {
    let write = || -> Result<(), String> {
        let conn = crate::native::db::open_read_write(db_path)?;
        append_message(&conn, chat_session_id, "user", prompt)?;
        if !answer.is_empty() {
            append_message(&conn, chat_session_id, "assistant", answer)?;
        }
        Ok(())
    };
    if let Err(e) = write() {
        log::warn!("failed to store trigger messages: {e}");
    }
}

fn append_message(
    conn: &rusqlite::Connection,
    chat_session_id: &str,
    role: &str,
    content: &str,
) -> Result<(), String> {
    // `id` is `INTEGER PRIMARY KEY AUTOINCREMENT`, so it is not in the column
    // list; `blocks` defaults to `[]`, which every reader JSON-decodes.
    conn.execute(
        "INSERT INTO chat_messages (session_id, role, content, blocks, timestamp)
         VALUES (?1, ?2, ?3, '[]', ?4)",
        rusqlite::params![
            chat_session_id,
            role,
            content,
            crate::native::gotime::now_go_text()
        ],
    )
    .map_err(|e| format!("storing {role} message: {e}"))?;
    Ok(())
}

/// `updateSessionUsage`.
fn update_session_usage(db_path: &Path, chat_session_id: &str, result: &agent_run::RunResult) {
    let write = || -> Result<(), String> {
        let conn = crate::native::db::open_read_write(db_path)?;
        conn.execute(
            "UPDATE chat_sessions SET
                sdk_session_id = ?1, total_input_tokens = ?2, total_output_tokens = ?3,
                total_cache_creation_tokens = ?4, total_cache_read_tokens = ?5, updated_at = ?6
             WHERE id = ?7",
            rusqlite::params![
                result.session_id,
                result.input_tokens,
                result.output_tokens,
                result.cache_creation_tokens,
                result.cache_read_tokens,
                crate::native::gotime::now_go_text(),
                chat_session_id,
            ],
        )
        .map_err(|e| format!("updating chat session: {e}"))?;
        Ok(())
    };
    if let Err(e) = write() {
        log::warn!("failed to update chat session after trigger: {e}");
    }
}

async fn send_error_reply(bot_token: &str, msg: &TelegramMsg) {
    if let Err(e) =
        super::telegram_api::send_reply(bot_token, msg.chat.id, msg.message_id, ERROR_REPLY).await
    {
        log::error!(
            "failed to send error reply chat_id={} error={e}",
            msg.chat.id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated(dir: &Path) -> std::path::PathBuf {
        let db = dir.join("agento.db");
        let mut conn = rusqlite::Connection::open(&db).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO integrations (id, name, type, enabled, credentials, services,
                                       created_at, updated_at)
             VALUES ('tg', 'T', 'telegram', 1, '{}', '{}',
                     '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
            [],
        )
        .expect("seed integration");
        db
    }

    fn add_rule(
        db: &Path,
        id: &str,
        enabled: bool,
        prefix: &str,
        keywords: &str,
        chat_ids: &str,
        created_at: &str,
    ) {
        let conn = rusqlite::Connection::open(db).expect("open");
        conn.execute(
            "INSERT INTO trigger_rules
                (id, integration_id, name, agent_slug, enabled, filter_prefix,
                 filter_keywords, filter_chat_ids, created_at, updated_at)
             VALUES (?1, 'tg', ?1, 'a', ?2, ?3, ?4, ?5, ?6, ?6)",
            rusqlite::params![id, enabled, prefix, keywords, chat_ids, created_at],
        )
        .expect("seed rule");
    }

    fn msg(text: &str, chat_id: i64) -> TelegramMsg {
        TelegramMsg {
            message_id: 1,
            chat: super::super::receiver::TelegramChat { id: chat_id },
            text: text.to_string(),
        }
    }

    #[test]
    fn the_first_matching_enabled_rule_wins_in_store_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        // Oldest first, as `ListRules` orders them.
        add_rule(
            &db,
            "old",
            true,
            "",
            "[]",
            "[]",
            "2026-01-01 00:00:00 +0000 UTC",
        );
        add_rule(
            &db,
            "new",
            true,
            "",
            "[]",
            "[]",
            "2026-02-01 00:00:00 +0000 UTC",
        );

        let (rule, prompt) = find_matching_rule(&db, "tg", &msg("anything", 42)).expect("a match");
        assert_eq!(rule.id, "old", "oldest first, not newest");
        assert_eq!(prompt, "anything");
    }

    #[test]
    fn a_disabled_rule_is_never_considered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_rule(
            &db,
            "off",
            false,
            "",
            "[]",
            "[]",
            "2026-01-01 00:00:00 +0000 UTC",
        );
        assert!(find_matching_rule(&db, "tg", &msg("anything", 42)).is_none());

        // …and an enabled one after it still matches.
        add_rule(
            &db,
            "on",
            true,
            "",
            "[]",
            "[]",
            "2026-02-01 00:00:00 +0000 UTC",
        );
        assert_eq!(
            find_matching_rule(&db, "tg", &msg("anything", 42))
                .expect("a match")
                .0
                .id,
            "on"
        );
    }

    #[test]
    fn the_filters_come_off_the_row_and_are_applied() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_rule(
            &db,
            "filtered",
            true,
            "/ask",
            r#"["status"]"#,
            r#"["42"]"#,
            "2026-01-01 00:00:00 +0000 UTC",
        );

        let (_, prompt) =
            find_matching_rule(&db, "tg", &msg("/ask what is the status", 42)).expect("a match");
        assert_eq!(prompt, "what is the status", "the prefix is stripped");

        // Each filter can reject on its own.
        assert!(find_matching_rule(&db, "tg", &msg("what is the status", 42)).is_none());
        assert!(find_matching_rule(&db, "tg", &msg("/ask something else", 42)).is_none());
        assert!(find_matching_rule(&db, "tg", &msg("/ask what is the status", 99)).is_none());
    }

    #[test]
    fn rules_are_scoped_to_their_integration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_rule(
            &db,
            "mine",
            true,
            "",
            "[]",
            "[]",
            "2026-01-01 00:00:00 +0000 UTC",
        );
        assert!(find_matching_rule(&db, "other", &msg("hi", 42)).is_none());
    }

    #[test]
    fn a_null_or_broken_filter_column_is_no_filter_rather_than_no_match() {
        // Go decodes these into a nil slice, which `matchesKeywords` reads as
        // "everything". A port treating the failure as "match nothing" would
        // silently stop a working rule.
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_rule(
            &db,
            "nulls",
            true,
            "",
            "null",
            "not json",
            "2026-01-01 00:00:00 +0000 UTC",
        );
        assert!(find_matching_rule(&db, "tg", &msg("anything", 42)).is_some());
    }

    #[test]
    fn a_null_element_in_a_filter_list_is_an_empty_string() {
        // #295's rule: a `null` inside a list is the zero value, not an error.
        assert_eq!(
            decode_list(r#"["a",null]"#),
            vec!["a".to_string(), String::new()]
        );
        assert!(decode_list("null").is_empty());
        assert!(decode_list("").is_empty());
        assert!(decode_list("[]").is_empty());
    }

    #[test]
    fn a_synthesized_agent_carries_the_default_model_and_adaptive_thinking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("missing.db");
        let agent = resolve_agent(&db, "").expect("no slug is never an error");
        assert_eq!(agent.thinking, "adaptive");
        assert!(
            !agent.model.is_empty(),
            "Go falls back to a literal 'sonnet'"
        );
        assert!(
            agent.capabilities.built_in.is_none(),
            "empty caps, so all built-ins"
        );
    }

    #[test]
    fn an_unknown_agent_slug_is_an_error_rather_than_a_synthesized_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        let err = resolve_agent(&db, "nope").unwrap_err();
        assert_eq!(err, r#"agent "nope" not found"#);
    }
}
