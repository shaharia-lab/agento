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
//! - **The timeout is the matched rule's own**, and five minutes when the rule
//!   records none (#565) — see [`run_timeout`].
//! - **Concurrency is bounded to 10**, not the scheduler's 3, and the bound is
//!   Go's `sem` on the dispatcher rather than a per-run permit.

use std::path::Path;

use tokio::sync::Semaphore;

use super::match_rule::{match_rule, RuleFilters};
use super::receiver::{TelegramMsg, TelegramUpdate};
use crate::native::agent_run;
use crate::native::agents::Agent;
use crate::native::db;

/// `maxConcurrentExecutions`, the buffer on `Dispatcher.sem`.
///
/// **Ten, not the scheduler's three.** A busy group chat can fire several rules
/// at once, and a limit set too low makes the sixth message wait a whole run —
/// five minutes by default, and as much as a rule's own `timeout_minutes` since
/// #565 — before its "typing…" even appears.
const MAX_CONCURRENT: usize = 10;

/// `context.WithTimeout(ctx, 5*time.Minute)` in `executeAndReply`, and since
/// #565 the answer only for a rule that records no timeout of its own.
const RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// What Go replies with on every failure path.
const ERROR_REPLY: &str = "Sorry, something went wrong.";

/// `pub(crate)` rather than module-private since #567: Slack's Socket Mode
/// worker runs its handler under **this** bound, not a second one. Two
/// semaphores would each be ten, so a workspace busy on both transports could
/// have twenty agent runs in flight against a limit that says ten — and the
/// limit is about `claude` subprocesses, which do not care which transport
/// asked for them.
pub(crate) fn semaphore() -> &'static Semaphore {
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

/// What `db::blocking` answering `None` costs at each call site below, since it
/// is not the same everywhere: `claim_update` and `find_matching_rule` stop the
/// dispatch, exactly as a failed read already does; `resolve_agent` and
/// `create_trigger_session` become the error reply; and the two `save_messages`
/// calls are best-effort in Go too, so they are ignored.
///
/// `processTelegramUpdate`.
async fn process(db_path: &Path, integration_id: &str, bot_token: &str, update: TelegramUpdate) {
    // A non-message update, or one with no text, is not a trigger.
    // `GoStruct` is a decode-shape wrapper; the message itself is inside it.
    let Some(msg) = update.message.map(|m| m.0).filter(|m| !m.text.is_empty()) else {
        return;
    };

    // Claimed before the rules are read, so a Telegram retry cannot run the
    // agent twice — see `receiver::claim_update` for why the claim is atomic
    // here where Go's is two statements.
    // **Every database call this module makes goes through [`db::blocking`],**
    // each under its own label so the log says which one panicked. (Not every
    // call the *dispatch* makes: `run_headless` opens SQLite on the worker while
    // building its options, which chat and the scheduler share verbatim.)
    // `process` runs on an axum worker, and each of these opens a connection and
    // may sit on
    // `db.rs`'s five-second `busy_timeout` while the session scan batch-writes —
    // ten of them at `MAX_CONCURRENT` is every worker on a four-core machine,
    // stalling the SPA and any SSE stream. `proxy.rs` puts native handlers on the
    // blocking pool for exactly this reason.
    let claimed = {
        let (db, id, update_id) = (
            db_path.to_path_buf(),
            integration_id.to_string(),
            update.update_id,
        );
        db::blocking("telegram claim", move || {
            super::receiver::claim_update(&db, &id, update_id)
        })
        .await
    };
    if !claimed.unwrap_or(false) {
        return;
    }

    let matched = {
        let (db, id, msg) = (
            db_path.to_path_buf(),
            integration_id.to_string(),
            msg.clone(),
        );
        db::blocking("telegram rule match", move || {
            find_matching_rule(&db, &id, &msg)
        })
        .await
    };
    let Some(Some((rule, prompt))) = matched else {
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
    /// Whether the rule is on.
    ///
    /// A field rather than a `WHERE` clause because the two selectors disagree
    /// about disabled rules: [`find_matching_rule`] skips one, and Slack's
    /// [`super::select_rule::select_rule_for_channel`] has to *see* one, since
    /// a disabled channel-specific rule is that channel's off switch.
    pub enabled: bool,
    pub filters: RuleFilters,
    /// Migration 39's four spec-reachable execution settings (#563), as
    /// [`load_rules`] read them — with an unusable `permission_mode` already
    /// dropped, see [`usable_permission_mode`].
    pub settings: agent_run::ExecutionSettings,
    /// Migration 39's fifth. `0` is "the dispatcher's own default", **not** a
    /// run that times out instantly — see [`run_timeout`].
    pub timeout_minutes: i64,
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
        // The `enabled` test used to be `load_rules`' SQL. It is here now, and
        // the answer is the same one: the first enabled rule that matches, in
        // store order.
        .filter(|rule| rule.enabled)
        .find_map(|rule| match_rule(&rule.filters, &msg.text, &chat_id).map(|p| (rule, p)))
}

/// `ListRules` for one integration, oldest first — **including disabled rules**.
///
/// The `enabled = 1` test used to live in this SQL and now lives in
/// [`find_matching_rule`], which does not change Telegram's answer and does make
/// this loader usable by both selectors. Slack's
/// [`super::select_rule::select_rule_for_channel`] needs the disabled rows: a
/// disabled channel-specific rule is how a channel is switched off, so a loader
/// that hid it would silently turn that off switch into a fall-through to the
/// workspace-wide default.
///
/// `ORDER BY created_at ASC` is load-bearing for both: it is what makes "the
/// first match in store order" and "the earlier `created_at` wins a tie" the
/// same sentence.
pub fn load_rules(db_path: &Path, integration_id: &str) -> Result<Vec<Rule>, String> {
    let conn = crate::native::db::open_read_only(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, name, agent_slug, enabled, filter_prefix, filter_keywords,
                    filter_chat_ids, model, working_directory, settings_profile_id,
                    permission_mode, timeout_minutes
             FROM trigger_rules
             WHERE integration_id = ?1
             ORDER BY created_at ASC",
        )
        .map_err(|e| format!("preparing trigger rules query: {e}"))?;

    let rows = stmt
        .query_map([integration_id], |row| {
            let keywords: String = row.get(5)?;
            let chat_ids: String = row.get(6)?;
            let permission_mode: String = row.get(10)?;
            Ok(Rule {
                id: row.get(0)?,
                name: row.get(1)?,
                agent_slug: row.get(2)?,
                enabled: row.get(3)?,
                filters: RuleFilters {
                    prefix: row.get(4)?,
                    keywords: decode_list(&keywords),
                    chat_ids: decode_list(&chat_ids),
                },
                settings: agent_run::ExecutionSettings {
                    model: row.get(7)?,
                    working_directory: row.get(8)?,
                    settings_profile_id: row.get(9)?,
                    permission_mode: usable_permission_mode(permission_mode),
                },
                timeout_minutes: row.get(11)?,
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

/// A stored `permission_mode`, or empty when it is not one Agento knows.
///
/// `integrations::validate_rule_settings` rejects anything outside
/// [`crate::native::chats::CHAT_PERMISSION_MODES`] at the write (#563), but the
/// dispatcher reads *stored rows* — hand-edited, restored from a backup, or
/// written before that validation existed. An unknown mode is not inert:
/// `chat/runner.rs`' `match` routes everything it does not recognise into
/// `with_bypass_permissions()`, so one typo would run the agent with permissions
/// fully bypassed. Falling back to empty runs it on the agent's own configured
/// mode, which is what a rule that sets nothing already does.
fn usable_permission_mode(stored: String) -> String {
    if crate::native::chats::is_valid_permission_mode(&stored) {
        return stored;
    }
    log::warn!("ignoring unknown trigger rule permission mode {stored:?}");
    String::new()
}

/// How long one run of `rule` may take.
///
/// `timeout_minutes = 0` is "no choice recorded" and means [`RUN_TIMEOUT`] — the
/// flat five minutes every trigger run had before #565 — not a run that times
/// out instantly.
///
/// The write path **refuses** a value above
/// [`crate::native::integrations::RULE_MAX_TIMEOUT_MINUTES`] with a 422 rather
/// than clamping it, so a row holding one never came through
/// `validate_rule_settings`: it was hand-edited, restored, or written before
/// that validation existed. This clamps rather than refusing, because refusing
/// here means an inbound message silently going unanswered, and an absurd value
/// would otherwise hold one of the ten concurrency slots for as long as it says.
/// It is [`usable_permission_mode`]'s premise with the opposite answer, and for
/// the same reason both are stated: a clamped timeout is a run that still
/// happens, where a bad mode is a run that must not.
fn run_timeout(rule: &Rule) -> std::time::Duration {
    if rule.timeout_minutes <= 0 {
        return RUN_TIMEOUT;
    }
    let max = crate::native::integrations::RULE_MAX_TIMEOUT_MINUTES;
    let minutes = rule.timeout_minutes.min(max);
    if minutes != rule.timeout_minutes {
        log::warn!(
            "clamping trigger rule timeout {} to {max} minutes",
            rule.timeout_minutes
        );
    }
    std::time::Duration::from_secs(u64::try_from(minutes).unwrap_or(0) * 60)
}

/// Everything a matched rule decides about the run it is about to start: the
/// spec, and how long it may take.
///
/// One function rather than two lines in [`execute_and_reply`] so that the
/// fake-CLI suite (`tests/trigger_run.rs`) binds to the **shipped** call site.
/// `execute_and_reply` cannot be driven from `tests/` at all — it sends a
/// Telegram reply, and the base-URL seam that redirects one is `#[cfg(test)]` on
/// the library, so an integration-test crate cannot reach it — and a test that
/// re-spelled these two lines for itself would stay green if the dispatcher
/// stopped passing the rule's settings, which is the whole of #565.
pub fn run_inputs(
    db_path: &Path,
    agent: Agent,
    rule: &Rule,
) -> (crate::native::chat::runner::RunSpec, std::time::Duration) {
    (
        agent_run::headless_spec(db_path, agent, &rule.settings),
        run_timeout(rule),
    )
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

    let resolved = {
        let (db, slug) = (db_path.to_path_buf(), rule.agent_slug.clone());
        db::blocking("telegram agent lookup", move || resolve_agent(&db, &slug))
            .await
            .unwrap_or_else(|| Err("the agent lookup task failed".to_string()))
    };
    let agent = match resolved {
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

    // The chat is created with the rule's own working directory, model,
    // settings profile and permission mode (#565). Go created it with none of
    // them, because a trigger run was not configurable the way a task is;
    // migration 39 gave the rule those columns and this is the reader.
    let created = {
        let (db, rule) = (db_path.to_path_buf(), rule.clone());
        db::blocking("telegram session", move || {
            create_trigger_session(&db, &rule)
        })
        .await
        .unwrap_or_else(|| Err("the session task failed".to_string()))
    };
    let chat_session_id = match created {
        Ok(id) => id,
        Err(e) => {
            log::error!("failed to create chat session for trigger: {e}");
            send_error_reply(bot_token, msg).await;
            return;
        }
    };

    let (spec, timeout) = run_inputs(db_path, agent, rule);
    let result = agent_run::run_headless(&spec, prompt, timeout).await;

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
            let (db, session, prompt) = (
                db_path.to_path_buf(),
                chat_session_id.clone(),
                prompt.to_string(),
            );
            db::blocking("telegram failed turn", move || {
                save_messages(&db, &session, &prompt, "")
            })
            .await;
            return;
        }
    };

    {
        let (db, session, prompt) = (
            db_path.to_path_buf(),
            chat_session_id.clone(),
            prompt.to_string(),
        );
        let usage = result.clone();
        db::blocking("telegram turn", move || {
            save_messages(&db, &session, &prompt, &usage.answer);
            update_session_usage(&db, &session, &usage);
        })
        .await;
    }

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
    let session = crate::native::chats::insert_session(
        &tx,
        crate::native::chats::NewSessionParams {
            agent_slug: &rule.agent_slug,
            working_directory: &rule.settings.working_directory,
            model: &rule.settings.model,
            settings_profile_id: &rule.settings.settings_profile_id,
            permission_mode: &rule.settings.permission_mode,
        },
    )
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
    let conn = match crate::native::db::open_read_write(db_path) {
        Ok(conn) => conn,
        Err(e) => {
            log::warn!("failed to store user message: {e}");
            return;
        }
    };
    // **Each insert is logged on its own**, as `saveSessionMessages` does. A
    // shared `?` would let a failed user turn skip the assistant one, leaving
    // the session with neither where Go leaves it with the answer — and the
    // reply has already gone to Telegram either way.
    if let Err(e) = append_message(&conn, chat_session_id, "user", prompt) {
        log::warn!("failed to store user message: {e}");
    }
    if !answer.is_empty() {
        if let Err(e) = append_message(&conn, chat_session_id, "assistant", answer) {
            log::warn!("failed to store assistant message: {e}");
        }
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
            chat: crate::native::gojson::GoStruct(crate::native::trigger::receiver::TelegramChat {
                id: chat_id,
            }),
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

    /// A rule with migration 39's five columns set. Separate from [`add_rule`]
    /// rather than an extension of it: the tests above pin Telegram's selection
    /// and must keep seeding exactly the rows they seeded before #565.
    #[allow(clippy::too_many_arguments)]
    fn add_configured_rule(
        db: &Path,
        id: &str,
        enabled: bool,
        chat_ids: &str,
        model: &str,
        working_directory: &str,
        settings_profile_id: &str,
        permission_mode: &str,
        timeout_minutes: i64,
        created_at: &str,
    ) {
        let conn = rusqlite::Connection::open(db).expect("open");
        conn.execute(
            "INSERT INTO trigger_rules
                (id, integration_id, name, agent_slug, enabled, filter_prefix,
                 filter_keywords, filter_chat_ids, model, working_directory,
                 settings_profile_id, permission_mode, timeout_minutes,
                 created_at, updated_at)
             VALUES (?1, 'tg', ?1, 'a', ?2, '', '[]', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            rusqlite::params![
                id,
                enabled,
                chat_ids,
                model,
                working_directory,
                settings_profile_id,
                permission_mode,
                timeout_minutes,
                created_at
            ],
        )
        .expect("seed configured rule");
    }

    /// The five columns migration 39 added reach the loaded rule. Before #565
    /// the projection did not name them and every run used the agent's defaults
    /// in whatever directory the app happened to be started from.
    #[test]
    fn the_execution_settings_come_off_the_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_configured_rule(
            &db,
            "r",
            true,
            "[]",
            "opus",
            "/srv/repo",
            "profile-7",
            "plan",
            30,
            "2026-01-01 00:00:00 +0000 UTC",
        );

        let rules = load_rules(&db, "tg").expect("load");
        let rule = rules.first().expect("one rule");
        assert_eq!(rule.settings.model, "opus");
        assert_eq!(rule.settings.working_directory, "/srv/repo");
        assert_eq!(rule.settings.settings_profile_id, "profile-7");
        assert_eq!(rule.settings.permission_mode, "plan");
        assert_eq!(rule.timeout_minutes, 30);
    }

    /// `load_rules` returns disabled rules now, because Slack's selection is the
    /// one that has to see them. Telegram's answer is unchanged — the
    /// `enabled` test moved into `find_matching_rule`, which
    /// `a_disabled_rule_is_never_considered` still pins.
    #[test]
    fn the_loader_returns_disabled_rules_and_the_telegram_selector_skips_them() {
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

        let rules = load_rules(&db, "tg").expect("load");
        assert_eq!(rules.len(), 1, "the disabled row is loaded");
        assert!(!rules[0].enabled);
        assert!(
            find_matching_rule(&db, "tg", &msg("hi", 1)).is_none(),
            "and Telegram still ignores it"
        );
    }

    /// An unknown mode is not inert: `build_options`' catch-all would run the
    /// agent with permissions fully bypassed, so a stored value outside
    /// `CHAT_PERMISSION_MODES` is dropped rather than forwarded.
    #[test]
    fn an_unusable_permission_mode_is_dropped_rather_than_escalating_to_bypass() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_configured_rule(
            &db,
            "r",
            true,
            "[]",
            "",
            "",
            "",
            "yolo",
            0,
            "2026-01-01 00:00:00 +0000 UTC",
        );

        let rules = load_rules(&db, "tg").expect("load");
        assert_eq!(
            rules[0].settings.permission_mode, "",
            "\"yolo\" would reach `with_bypass_permissions()`; empty runs the agent's own mode"
        );
        for mode in crate::native::chats::CHAT_PERMISSION_MODES {
            assert_eq!(usable_permission_mode(mode.to_string()), mode);
        }
    }

    /// Zero is "no choice recorded", not a run that times out instantly, and a
    /// stored row is clamped because it never went through the write path's
    /// validation.
    #[test]
    fn the_run_timeout_is_the_rules_own_with_five_minutes_as_the_default() {
        let rule = |minutes: i64| Rule {
            id: "r".to_string(),
            name: "r".to_string(),
            agent_slug: "a".to_string(),
            enabled: true,
            filters: RuleFilters::default(),
            settings: Default::default(),
            timeout_minutes: minutes,
        };

        assert_eq!(run_timeout(&rule(0)), RUN_TIMEOUT);
        assert_eq!(
            run_timeout(&rule(-1)),
            RUN_TIMEOUT,
            "and so is a broken row"
        );
        assert_eq!(
            run_timeout(&rule(1)),
            std::time::Duration::from_secs(60),
            "one minute is one minute"
        );
        assert_eq!(
            run_timeout(&rule(crate::native::integrations::RULE_MAX_TIMEOUT_MINUTES)),
            std::time::Duration::from_secs(240 * 60)
        );
        assert_eq!(
            run_timeout(&rule(100_000)),
            std::time::Duration::from_secs(240 * 60),
            "a stored row is clamped: the write path caps it, this reads what is there"
        );
    }

    /// [`run_inputs`] is what the dispatcher calls, so this is where the two
    /// halves of the wiring are pinned. [`run_timeout`]'s own mapping is tested
    /// above; what a revert would break here is `run_inputs` *calling* it —
    /// which no fake-CLI test can see, because the timeout is only observable
    /// by outliving it and the smallest non-default rule timeout is a minute.
    #[test]
    fn run_inputs_carries_the_rules_settings_and_its_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_configured_rule(
            &db,
            "r",
            true,
            "[]",
            "opus",
            "/srv/repo",
            "profile-7",
            "plan",
            1,
            "2026-01-01 00:00:00 +0000 UTC",
        );
        let agent = resolve_agent(&db, "").expect("a synthesized agent");
        let rules = load_rules(&db, "tg").expect("load");

        let (spec, timeout) = run_inputs(&db, agent.clone(), &rules[0]);
        assert_eq!(spec.working_dir, "/srv/repo");
        assert_eq!(spec.settings_profile_id, "profile-7");
        assert_eq!(spec.permission_mode, "plan");
        assert_eq!(spec.agent.as_ref().map(|a| a.model.as_str()), Some("opus"));
        assert_eq!(
            timeout,
            std::time::Duration::from_secs(60),
            "the rule's one minute, not the flat five"
        );

        let mut unset = rules[0].clone();
        unset.timeout_minutes = 0;
        assert_eq!(
            run_inputs(&db, agent, &unset).1,
            RUN_TIMEOUT,
            "and a rule that records none still gets the flat five"
        );
    }

    /// The chat a run is recorded in carries the rule's settings, where it used
    /// to carry four empty strings.
    #[test]
    fn the_chat_row_carries_the_rules_execution_settings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_configured_rule(
            &db,
            "r",
            true,
            "[]",
            "opus",
            "/srv/repo",
            "profile-7",
            "dontAsk",
            0,
            "2026-01-01 00:00:00 +0000 UTC",
        );
        let rules = load_rules(&db, "tg").expect("load");

        let id = create_trigger_session(&db, &rules[0]).expect("session");
        let conn = rusqlite::Connection::open(&db).expect("open");
        let row: (String, String, String, String, String) = conn
            .query_row(
                "SELECT title, working_directory, model, settings_profile_id, permission_mode
                 FROM chat_sessions WHERE id = ?1",
                [&id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("the session row");

        assert_eq!(row.0, "[Telegram] r", "the title is unchanged");
        assert_eq!(row.1, "/srv/repo");
        assert_eq!(row.2, "opus");
        assert_eq!(row.3, "profile-7");
        assert_eq!(row.4, "dontAsk");
    }
}
