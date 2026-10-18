//! One inbound Telegram message, from matched rule to sent reply. Mirrors
//! `Dispatcher.processTelegramUpdate` and `executeAndReply`
//! (`internal/trigger/dispatcher.go`).
//!
//! # Two paths after the match (#684)
//!
//! [`process`] claims the update, finds the first matching rule, and branches
//! once on that rule's `task_id`:
//!
//! - **A linked rule** (a `task_id`) starts its task through
//!   `executor::run_event` with `triggered_by = telegram` ([`run_linked`]). The
//!   matched text is the event's payload — data inside the task's own
//!   instructions, never the prompt — and the run is a `job_history` row like a
//!   scheduled one. **Nothing is sent from here once the run has started**: the
//!   answer is the task's `reply` delivery, quoting the triggering message, so a
//!   linked task with no `reply` destination runs, is recorded, and answers
//!   nobody. The rule's `agent_slug`, execution settings and timeout are not
//!   read on this path; the task's are.
//! - **An unlinked rule** (`task_id = ""`, every rule migration 48 found) runs
//!   exactly as before #684, through [`execute_and_reply`] — the sections below
//!   describe that path.
//!
//! What a linked rule's refusals answer, none of which writes a `job_history`
//! row or falls back to the inline run (that would run the sender's text as
//! the prompt with no record, which linking a rule to a task exists to stop):
//!
//! | refusal | the sender gets |
//! |---|---|
//! | `Paused` | nothing — pause is the automation's off switch |
//! | `Dropped`, `RateLimited` (#691) | nothing — counted on the task |
//! | `NoSuchTask`, `Unavailable`, no scheduler in this process | [`ERROR_REPLY`] |
//!
//! **The ten-slot bound is the outer limit on both paths, taken around the
//! run.** An unlinked rule takes it around [`execute_and_reply`]. A linked one
//! is first admitted by its task's limiter (#691), so a message waiting in the
//! task's queue holds nothing global, and then takes it and holds it until the
//! run ends; the scheduler's three permits are taken inside it. Always in that
//! order, as Slack's linked path does, so the two cannot deadlock.
//!
//! # The fourth caller of the agent runner
//!
//! After chat (#276), the scheduler (#275) and `agento ask`, the unlinked path
//! is the fourth place an agent runs — and the second headless one. It goes
//! through [`crate::native::agent_run`] for that reason: the
//! one-shot-vs-`Session` trap that made every scheduled run hang lives there
//! now, in one place, so this caller cannot reintroduce it.
//!
//! # Where the unlinked path differs from the scheduler, and why
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
use std::sync::Arc;

use tokio::sync::Semaphore;

use super::match_rule::{match_rule, sender_allowed, RuleFilters};
use super::receiver::{TelegramMsg, TelegramUpdate};
use crate::native::agent_run::{self, Runner as _};
use crate::native::agents::Agent;
use crate::native::db;
use crate::native::schedule::delivery::ReplyTarget;
use crate::native::schedule::executor::{self, EventInput, EventRefused};
use crate::native::schedule::runtime::{self, Scheduler};
use crate::native::tasks;

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
///
/// `pub(crate)` since #568: Slack's inbound handler answers with **this** string
/// rather than one of its own. Two inbound transports spelling the same failure
/// differently is drift a reader would take for a difference in what failed.
pub(crate) const ERROR_REPLY: &str = "Sorry, something went wrong.";

/// What a run that finished with nothing to say replies with.
///
/// An empty answer still gets a reply — silence would be indistinguishable from
/// the bot being broken — and, like [`ERROR_REPLY`], it is one string for every
/// inbound transport.
pub(crate) const NO_RESPONSE_REPLY: &str = "No response generated.";

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
        // Read per update rather than once: `lib.rs` starts the integration
        // registry, and so the polling worker, before the scheduler.
        process(
            semaphore(),
            runtime::running(),
            &db_path,
            &integration_id,
            &bot_token,
            update,
        )
        .await;
    });
}

/// What `db::blocking` answering `None` costs at each call site below, since it
/// is not the same everywhere: `claim_update` and `find_matching_rule` stop the
/// dispatch, exactly as a failed read already does; `resolve_agent` and
/// `create_trigger_session` become the error reply; and the two `save_messages`
/// calls are best-effort in Go too, so they are ignored.
///
/// `processTelegramUpdate`. The bound and the scheduler are parameters so a
/// test can hand it a one-slot semaphore and a detached scheduler.
///
/// **The ten-slot bound is taken around the run, not around the update** —
/// for an unlinked rule around [`execute_and_reply`], for a linked one after
/// the task has admitted the event ([`run_linked`]) — so a message waiting in
/// its task's queue holds nothing global (#691).
async fn process(
    sem: &'static Semaphore,
    scheduler: Option<Arc<Scheduler>>,
    db_path: &Path,
    integration_id: &str,
    bot_token: &str,
    update: TelegramUpdate,
) {
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
    // call the *dispatch* makes: `Runner::run` opens SQLite on the worker while
    // building its options, which chat and the scheduler share verbatim.)
    // `process` runs on an axum worker, and each of these opens a connection and
    // may sit on
    // `db.rs`'s five-second `busy_timeout` while the session scan batch-writes —
    // a handful of them at once is every worker on a four-core machine,
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

    // An unknown chat is counted in the same blocking section that decided it,
    // and after the claim, so a redelivered update is counted once (#690).
    let selected = {
        let (db, id, msg) = (
            db_path.to_path_buf(),
            integration_id.to_string(),
            msg.clone(),
        );
        db::blocking("telegram rule match", move || {
            select_and_count(&db, &id, &msg)
        })
        .await
    };
    let Some(Selection::Matched(rule, prompt)) = selected else {
        return;
    };

    log::info!(
        "trigger rule matched rule_id={:?} rule_name={:?} agent_slug={:?} task_id={:?} chat_id={}",
        rule.id,
        rule.name,
        rule.agent_slug,
        rule.task_id,
        msg.chat.id
    );

    if rule.task_id.is_empty() {
        let Ok(_permit) = sem.acquire().await else {
            log::warn!(
                "dispatcher stopped, dropping telegram update integration_id={integration_id:?}"
            );
            return;
        };
        execute_and_reply(db_path, bot_token, &msg, &rule, &prompt).await;
        return;
    }

    // "typing…" while the task runs, best-effort as on the inline path.
    super::telegram_api::send_chat_action(bot_token, msg.chat.id).await;
    let task_id = rule.task_id.as_str();
    let Some(scheduler) = scheduler else {
        log::error!(
            "a telegram rule's task cannot run, this process has no scheduler rule_id={:?} \
             task_id={task_id:?}",
            rule.id
        );
        send_error_reply(bot_token, &msg).await;
        return;
    };
    match run_linked(sem, scheduler, integration_id, &msg, &rule, prompt).await {
        Ok(job_id) => warn_if_unanswered(db_path, &rule, &job_id).await,
        Err(EventRefused::Paused) => {
            log::info!(
                "telegram message ignored, the rule's task is paused rule_id={:?} \
                 task_id={task_id:?}",
                rule.id
            );
        }
        // Silence, as for a paused task: the limits are the owner's, and the
        // executor has counted the refusal on the task.
        Err(refused @ (EventRefused::Dropped | EventRefused::RateLimited)) => {
            log::info!(
                "telegram message ignored, the rule's task is at its run limits rule_id={:?} \
                 task_id={task_id:?} reason={refused:?}",
                rule.id
            );
        }
        Err(
            refused @ (EventRefused::NoSuchTask
            | EventRefused::NoSuchRun
            | EventRefused::Unavailable),
        ) => {
            log::error!(
                "a telegram rule's task did not run rule_id={:?} task_id={task_id:?} \
                 reason={refused:?}",
                rule.id
            );
            send_error_reply(bot_token, &msg).await;
        }
    }
}

/// One event run of `rule`'s task for `msg` (#684): `payload` is the text
/// [`find_matching_rule`] matched (prefix stripped), and the origin is the
/// message itself, so the task's `reply` delivery quotes it.
///
/// The task's own limits come first (`executor::admit_event`, #691): an event
/// waiting in its task's queue holds no permit of `sem`'s. Only an admitted
/// one takes a dispatcher permit, and holds it until `executor::run_admitted`
/// returns, which takes one of the scheduler's three inside it — so the bound
/// is always taken outside the scheduler's and the two cannot deadlock. A
/// closed `sem` is `Unavailable`.
///
/// Answers the job id once the run has finished, or why no run started. Sends
/// nothing to Telegram either way — [`process`] decides what a refusal says.
/// `pub` for [`run_inputs`]'s reason: `tests/trigger_run.rs` binds to the
/// shipped call rather than re-spelling it.
pub async fn run_linked(
    sem: &Semaphore,
    scheduler: Arc<Scheduler>,
    integration_id: &str,
    msg: &TelegramMsg,
    rule: &Rule,
    payload: String,
) -> Result<String, EventRefused> {
    let origin = ReplyTarget::Telegram {
        integration_id: integration_id.to_string(),
        chat_id: msg.chat.id,
        message_id: msg.message_id,
    };
    let Some(event) = EventInput::new(
        tasks::TriggeredBy::Telegram,
        payload,
        Some(origin),
        uuid::Uuid::new_v4().to_string(),
    ) else {
        // `Telegram` is an event source, so `new` cannot refuse it.
        log::error!(
            "a telegram message was refused as an event task_id={:?}",
            rule.task_id
        );
        return Err(EventRefused::Unavailable);
    };
    let admitted = executor::admit_event(&scheduler, &rule.task_id).await?;
    let Ok(_permit) = sem.acquire().await else {
        log::warn!(
            "dispatcher stopped, dropping a telegram event task_id={:?}",
            rule.task_id
        );
        return Err(EventRefused::Unavailable);
    };
    executor::run_admitted(scheduler, admitted, event).await
}

/// Names a linked task with no `reply` destination once per run: it ran and
/// was recorded, and the sender was not answered. A task deleted while it ran
/// is not named, as Slack's `map_run` does not.
async fn warn_if_unanswered(db_path: &Path, rule: &Rule, job_id: &str) {
    let (db, task_id) = (db_path.to_path_buf(), rule.task_id.clone());
    let replies = db::blocking("telegram event run task read", move || {
        tasks::get_task(&db, &task_id)
            .map(|task| task.map(|t| t.destinations.iter().any(|d| d.r#type == "reply")))
    })
    .await;
    if let Some(Ok(Some(false))) = replies {
        log::warn!(
            "a telegram rule's task has no reply destination, so the sender was not answered \
             rule_id={:?} task_id={:?} job_id={job_id:?}",
            rule.id,
            rule.task_id
        );
    }
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
    /// replaced by `dontAsk`, see [`usable_permission_mode`].
    pub settings: agent_run::ExecutionSettings,
    /// Migration 39's fifth. `0` is "the dispatcher's own default", **not** a
    /// run that times out instantly — see [`run_timeout`].
    pub timeout_minutes: i64,
    /// The task this rule starts through `executor::run_event` (Telegram
    /// #684, Slack #685); empty for a rule that runs its own agent, which is
    /// every rule migration 48 found.
    pub task_id: String,
    /// Whether a reply in a thread this rule's linked task answered continues
    /// that run's chat (#686). Read only while `task_id` is that task; see
    /// `slack/inbound.rs`. Migration 48 turned it on for every existing rule.
    pub continue_on_reply: bool,
}

/// What the enabled rules of an integration make of one message (#690).
#[derive(Debug)]
enum Selection {
    /// The first enabled rule that matches, and the prompt it extracted.
    Matched(Box<Rule>, String),
    /// A chat an enabled rule names, whose text no such rule accepts — or rules
    /// that could not be read. Not a dropped sender, so nothing is counted.
    NoMatch,
    /// A chat no enabled rule names, with the distinct tasks those rules link
    /// to: every one of them refused it, so every one of them counts it.
    UnknownChat(Vec<String>),
}

/// The rule selection `process` acts on, with an unknown chat already counted:
/// one call, so the decision and the count share one blocking section (#690).
fn select_and_count(db_path: &Path, integration_id: &str, msg: &TelegramMsg) -> Selection {
    let selection = find_matching_rule(db_path, integration_id, msg);
    if let Selection::UnknownChat(task_ids) = &selection {
        drop_unknown_chat(db_path, integration_id, msg, task_ids);
    }
    selection
}

/// `findMatchingRule`: the first **enabled** rule that matches, in the order the
/// store returns them (oldest first) — and since #690, which of the two kinds of
/// "no" a miss is.
fn find_matching_rule(db_path: &Path, integration_id: &str, msg: &TelegramMsg) -> Selection {
    let rules = match load_rules(db_path, integration_id) {
        Ok(rules) => rules,
        Err(e) => {
            log::error!("failed to load trigger rules integration_id={integration_id:?} error={e}");
            return Selection::NoMatch;
        }
    };
    // `fmt.Sprintf("%d", msg.Chat.ID)`.
    let chat_id = msg.chat.id.to_string();
    // The `enabled` test used to be `load_rules`' SQL. It is here now, and the
    // answer is the same one: the first enabled rule that matches, in store
    // order.
    let enabled: Vec<Rule> = rules.into_iter().filter(|rule| rule.enabled).collect();
    // Asked before prefix and keywords, so a known chat failing those is a
    // plain miss rather than a dropped sender.
    if !chat_is_known(&enabled, &chat_id) {
        return Selection::UnknownChat(linked_tasks(&enabled));
    }
    enabled
        .into_iter()
        // Senders are denied by default (#674): a rule is considered only for
        // a chat its list names, so an empty, null or unparseable
        // `filter_chat_ids` matches nobody. Asked here, ahead of `match_rule`,
        // whose own chat clause reads empty as "everything" for Slack.
        .filter(|rule| sender_allowed(&rule.filters.chat_ids, &chat_id))
        .find_map(|rule| match_rule(&rule.filters, &msg.text, &chat_id).map(|p| (rule, p)))
        .map_or(Selection::NoMatch, |(rule, prompt)| {
            Selection::Matched(Box::new(rule), prompt)
        })
}

/// Whether any of `rules` names `chat_id` — the one "chat is allowed" test,
/// [`sender_allowed`], asked of each. The caller passes only enabled rules, so
/// a chat named only by a disabled rule is unknown.
fn chat_is_known(rules: &[Rule], chat_id: &str) -> bool {
    rules
        .iter()
        .any(|rule| sender_allowed(&rule.filters.chat_ids, chat_id))
}

/// The distinct non-empty `task_id`s of `rules`, in store order. Two rules
/// linked to one task count a refused message on it once.
fn linked_tasks(rules: &[Rule]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for rule in rules {
        if !rule.task_id.is_empty() && !out.contains(&rule.task_id) {
            out.push(rule.task_id.clone());
        }
    }
    out
}

/// Record a message from a chat no enabled rule names (#690): one `debug` line,
/// no reply, and one count on each task in `task_ids`. Rules with no linked
/// task leave nowhere to count, so then it only logs.
fn drop_unknown_chat(db_path: &Path, integration_id: &str, msg: &TelegramMsg, task_ids: &[String]) {
    log::debug!(
        "telegram message ignored, unknown chat integration_id={integration_id:?} chat_id={} linked_tasks={}",
        msg.chat.id,
        task_ids.len()
    );
    for task_id in task_ids {
        if let Err(e) = crate::native::tasks::count_dropped_event(db_path, task_id) {
            log::warn!("failed to count a dropped telegram event task_id={task_id:?}: {e}");
        }
    }
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
                    permission_mode, timeout_minutes, task_id, filter_user_ids,
                    continue_on_reply
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
            let user_ids: String = row.get(13)?;
            Ok(Rule {
                id: row.get(0)?,
                name: row.get(1)?,
                agent_slug: row.get(2)?,
                enabled: row.get(3)?,
                filters: RuleFilters {
                    prefix: row.get(4)?,
                    keywords: decode_list(&keywords),
                    chat_ids: decode_list(&chat_ids),
                    user_ids: decode_list(&user_ids),
                },
                settings: agent_run::ExecutionSettings {
                    model: row.get(7)?,
                    working_directory: row.get(8)?,
                    settings_profile_id: row.get(9)?,
                    permission_mode: usable_permission_mode(permission_mode),
                    // A rule's own run has no task; a linked rule's run goes
                    // through the executor, which names the task's (#693).
                    task_mode: None,
                },
                timeout_minutes: row.get(11)?,
                task_id: row.get(12)?,
                continue_on_reply: row.get(14)?,
            })
        })
        .map_err(|e| format!("querying trigger rules: {e}"))?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("scanning trigger rule: {e}"))?);
    }
    Ok(out)
}

/// A stored `[]string` column. An unparseable or null value is an empty list:
/// "no filter" for keywords, and for Telegram's `filter_chat_ids` and Slack's
/// `filter_user_ids` an allowlist that names nobody (#674, #688).
fn decode_list(raw: &str) -> Vec<String> {
    serde_json::from_str::<Option<Vec<Option<String>>>>(raw)
        .ok()
        .flatten()
        .map(|list| list.into_iter().map(Option::unwrap_or_default).collect())
        .unwrap_or_default()
}

/// A stored `permission_mode`, or `dontAsk` when it is not one Agento knows.
///
/// `integrations::validate_rule_settings` rejects anything outside
/// [`crate::native::chats::CHAT_PERMISSION_MODES`] at the write (#563), but the
/// dispatcher reads *stored rows* — hand-edited, restored from a backup, or
/// written before that validation existed. `chat/runner.rs` refuses a mode it
/// does not know, and a refused run here is a sender who gets the error reply
/// for every message until somebody edits the row. So the rule runs with
/// prompts denied instead (#675): the most restrictive mode that still
/// answers. Not empty — empty would hand the choice to the agent's own mode,
/// and a rule that named *something* did not ask for that.
fn usable_permission_mode(stored: String) -> String {
    if crate::native::chats::is_valid_permission_mode(&stored) {
        return stored;
    }
    log::warn!("unknown trigger rule permission mode {stored:?}, running with prompts denied");
    "dontAsk".to_string()
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
/// It is [`usable_permission_mode`]'s premise and its answer: a row that never
/// went through validation still runs, on the nearest value that is safe.
pub(crate) fn run_timeout(rule: &Rule) -> std::time::Duration {
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
        let title = format!("[Telegram] {}", rule.name);
        db::blocking("telegram session", move || {
            create_trigger_session(&db, &rule, &title)
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
    let result = agent_run::runner().run(&spec, prompt, timeout, None).await;

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

    let reply = if result.answer.is_empty() {
        NO_RESPONSE_REPLY
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

/// The chat a trigger run is recorded in, titled `title`.
///
/// The title is the caller's since #568: Telegram's is `[Telegram] <rule>` and
/// Slack's is `[Slack] #channel: <the first 60 characters of the prompt>`, and
/// everything else about creating the chat — the rule's five execution settings
/// on the row, the immediate transaction, the title being a second write whose
/// failure is a warning rather than a failed run — is the same for both.
pub(crate) fn create_trigger_session(
    db_path: &Path,
    rule: &Rule,
    title: &str,
) -> Result<String, String> {
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

    /// [`find_matching_rule`]'s match, for the tests that ask only that.
    fn find(db: &Path, integration_id: &str, msg: &TelegramMsg) -> Option<(Rule, String)> {
        match find_matching_rule(db, integration_id, msg) {
            Selection::Matched(rule, prompt) => Some((*rule, prompt)),
            Selection::NoMatch | Selection::UnknownChat(_) => None,
        }
    }

    fn add_task(db: &Path, id: &str) {
        let conn = rusqlite::Connection::open(db).expect("open");
        conn.execute(
            "INSERT INTO scheduled_tasks
                (id, name, prompt, schedule_type, schedule_config, status,
                 run_count, stop_after_count, created_at, updated_at)
             VALUES (?1, ?1, 'p', 'interval', '{}', 'active', 0, 0,
                     '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
            [id],
        )
        .expect("seed task");
    }

    fn link(db: &Path, rule_id: &str, task_id: &str) {
        let conn = rusqlite::Connection::open(db).expect("open");
        conn.execute(
            "UPDATE trigger_rules SET task_id = ?2 WHERE id = ?1",
            [rule_id, task_id],
        )
        .expect("link rule");
    }

    fn dropped(db: &Path, task_id: &str) -> i64 {
        let conn = rusqlite::Connection::open(db).expect("open");
        conn.query_row(
            "SELECT dropped_event_count FROM scheduled_tasks WHERE id = ?1",
            [task_id],
            |row| row.get(0),
        )
        .expect("count")
    }

    /// [`select_and_count`] for one message to the `tg` integration.
    fn select(db: &Path, text: &str, chat_id: i64) -> Selection {
        select_and_count(db, "tg", &msg(text, chat_id))
    }

    /// #690: a chat no enabled rule names is counted once on each distinct
    /// linked task — two rules linked to one task count it there once.
    #[test]
    fn an_unknown_chat_is_counted_once_on_each_distinct_linked_task() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_task(&db, "t1");
        add_task(&db, "t2");
        add_task(&db, "other");
        for (rule, chats, task, at) in [
            ("a", r#"["42"]"#, "t1", "2026-01-01 00:00:00 +0000 UTC"),
            ("b", r#"["7"]"#, "t1", "2026-02-01 00:00:00 +0000 UTC"),
            ("c", r#"["8"]"#, "t2", "2026-03-01 00:00:00 +0000 UTC"),
        ] {
            add_rule(&db, rule, true, "", "[]", chats, at);
            link(&db, rule, task);
        }

        let selection = select(&db, "hello", 99);
        assert!(
            matches!(&selection, Selection::UnknownChat(t) if *t == ["t1", "t2"]),
            "{selection:?}"
        );
        assert_eq!(dropped(&db, "t1"), 1, "two rules, one task, one count");
        assert_eq!(dropped(&db, "t2"), 1);
        assert_eq!(
            dropped(&db, "other"),
            0,
            "a task no rule links is untouched"
        );
    }

    /// The chat is asked before prefix and keywords: a listed chat whose text
    /// fails them is a plain miss, not a dropped sender.
    #[test]
    fn a_known_chat_failing_the_prefix_or_keywords_counts_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_task(&db, "t1");
        add_rule(
            &db,
            "a",
            true,
            "/ask",
            r#"["status"]"#,
            r#"["42"]"#,
            "2026-01-01 00:00:00 +0000 UTC",
        );
        link(&db, "a", "t1");

        for text in ["no prefix status", "/ask no keyword"] {
            let selection = select(&db, text, 42);
            assert!(
                matches!(selection, Selection::NoMatch),
                "{text:?}: {selection:?}"
            );
        }
        assert_eq!(dropped(&db, "t1"), 0);
    }

    /// A disabled rule refuses nobody and names nobody: a chat only it lists is
    /// unknown, and its own linked task is not counted.
    #[test]
    fn a_disabled_rules_chat_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_task(&db, "on-task");
        add_task(&db, "off-task");
        add_rule(
            &db,
            "on",
            true,
            "",
            "[]",
            r#"["42"]"#,
            "2026-01-01 00:00:00 +0000 UTC",
        );
        add_rule(
            &db,
            "off",
            false,
            "",
            "[]",
            r#"["7"]"#,
            "2026-02-01 00:00:00 +0000 UTC",
        );
        link(&db, "on", "on-task");
        link(&db, "off", "off-task");

        let selection = select(&db, "hi", 7);
        assert!(
            matches!(&selection, Selection::UnknownChat(t) if *t == ["on-task"]),
            "{selection:?}"
        );
        assert_eq!(dropped(&db, "on-task"), 1);
        assert_eq!(dropped(&db, "off-task"), 0);
    }

    /// Rules with no linked task have nowhere to count: the drop writes nothing.
    #[test]
    fn an_unknown_chat_with_no_linked_task_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_task(&db, "t1");
        add_rule(
            &db,
            "a",
            true,
            "",
            "[]",
            r#"["42"]"#,
            "2026-01-01 00:00:00 +0000 UTC",
        );

        let selection = select(&db, "hi", 99);
        assert!(
            matches!(&selection, Selection::UnknownChat(t) if t.is_empty()),
            "{selection:?}"
        );
        assert_eq!(dropped(&db, "t1"), 0);
    }

    /// Through `process`, against a fake Bot API: an unknown chat sends
    /// nothing, starts no chat and no job, and a redelivered update is counted
    /// once because the count follows the claim.
    #[tokio::test]
    async fn a_redelivered_unknown_chat_is_counted_once_and_sends_nothing() {
        use crate::native::integrations::telegram::client::{api_base_lock, set_api_base};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let _guard = api_base_lock().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let app = axum::Router::new().fallback(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            async { r#"{"ok":true,"result":true}"# }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        set_api_base(Some(base));

        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_task(&db, "t1");
        add_rule(
            &db,
            "a",
            true,
            "",
            "[]",
            r#"["42"]"#,
            "2026-01-01 00:00:00 +0000 UTC",
        );
        link(&db, "a", "t1");

        let update = || TelegramUpdate {
            update_id: 1001,
            message: Some(crate::native::gojson::GoStruct(msg("hello", 99))),
        };
        process(semaphore(), None, &db, "tg", "token", update()).await;
        process(semaphore(), None, &db, "tg", "token", update()).await;
        set_api_base(None);

        assert_eq!(dropped(&db, "t1"), 1, "the same update_id is counted once");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "no Telegram call at all");
        let conn = rusqlite::Connection::open(&db).expect("open");
        for table in ["chat_sessions", "job_history"] {
            let rows: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .expect("count rows");
            assert_eq!(rows, 0, "{table}");
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
            r#"["42"]"#,
            "2026-01-01 00:00:00 +0000 UTC",
        );
        add_rule(
            &db,
            "new",
            true,
            "",
            "[]",
            r#"["42"]"#,
            "2026-02-01 00:00:00 +0000 UTC",
        );

        let (rule, prompt) = find(&db, "tg", &msg("anything", 42)).expect("a match");
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
            r#"["42"]"#,
            "2026-01-01 00:00:00 +0000 UTC",
        );
        assert!(find(&db, "tg", &msg("anything", 42)).is_none());

        // …and an enabled one after it still matches.
        add_rule(
            &db,
            "on",
            true,
            "",
            "[]",
            r#"["42"]"#,
            "2026-02-01 00:00:00 +0000 UTC",
        );
        assert_eq!(
            find(&db, "tg", &msg("anything", 42)).expect("a match").0.id,
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

        let (_, prompt) = find(&db, "tg", &msg("/ask what is the status", 42)).expect("a match");
        assert_eq!(prompt, "what is the status", "the prefix is stripped");

        // Each filter can reject on its own.
        assert!(find(&db, "tg", &msg("what is the status", 42)).is_none());
        assert!(find(&db, "tg", &msg("/ask something else", 42)).is_none());
        assert!(find(&db, "tg", &msg("/ask what is the status", 99)).is_none());
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
            r#"["42"]"#,
            "2026-01-01 00:00:00 +0000 UTC",
        );
        assert!(find(&db, "other", &msg("hi", 42)).is_none());
    }

    #[test]
    fn a_null_or_broken_keyword_column_is_no_filter_rather_than_no_match() {
        // Go decodes these into a nil slice, which `matchesKeywords` reads as
        // "everything". A port treating the failure as "match nothing" would
        // silently stop a working rule.
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        for (id, keywords) in [("null", "null"), ("broken", "not json")] {
            add_rule(
                &db,
                id,
                true,
                "",
                keywords,
                r#"["42"]"#,
                "2026-01-01 00:00:00 +0000 UTC",
            );
        }
        assert!(find(&db, "tg", &msg("anything", 42)).is_some());
    }

    /// #674: the allowed-chats list is the one filter where "unset" is not
    /// "everything". Whatever the column holds that names no chat — empty,
    /// null, unparseable, a blank entry — the rule answers nobody.
    #[test]
    fn a_rule_that_names_no_chat_answers_nobody() {
        for chat_ids in [
            "[]",
            "null",
            "",
            "not json",
            r#"[""]"#,
            "[null]",
            r#"{"42":1}"#,
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let db = migrated(dir.path());
            add_rule(
                &db,
                "open",
                true,
                "",
                "[]",
                chat_ids,
                "2026-01-01 00:00:00 +0000 UTC",
            );
            assert!(
                find(&db, "tg", &msg("anything", 42)).is_none(),
                "filter_chat_ids {chat_ids:?}"
            );
        }
    }

    /// The allowed, not-allowed and fall-through cases at the dispatcher: a
    /// sender one rule does not name is not answered by it, and is still
    /// answered by a later rule that does name it.
    #[test]
    fn a_sender_is_matched_only_by_a_rule_that_names_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = migrated(dir.path());
        add_rule(
            &db,
            "first",
            true,
            "",
            "[]",
            r#"["42"]"#,
            "2026-01-01 00:00:00 +0000 UTC",
        );
        add_rule(
            &db,
            "second",
            true,
            "",
            "[]",
            r#"["7","-100"]"#,
            "2026-02-01 00:00:00 +0000 UTC",
        );
        let matched = |chat| find(&db, "tg", &msg("hi", chat)).map(|(r, _)| r.id);
        assert_eq!(matched(42).as_deref(), Some("first"));
        assert_eq!(matched(-100).as_deref(), Some("second"));
        assert_eq!(matched(99), None);
        assert_eq!(matched(4), None, "an exact compare, not a prefix");
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
            find(&db, "tg", &msg("hi", 1)).is_none(),
            "and Telegram still ignores it"
        );
    }

    /// A stored value outside `CHAT_PERMISSION_MODES` is neither forwarded,
    /// which `build_options` would refuse, nor emptied, which would run the
    /// agent's own mode: it runs with prompts denied.
    #[test]
    fn an_unusable_permission_mode_runs_with_prompts_denied() {
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
            rules[0].settings.permission_mode, "dontAsk",
            "\"yolo\" is not a mode, and empty would run the agent's own"
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
            task_id: String::new(),
            continue_on_reply: false,
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

        let id = create_trigger_session(&db, &rules[0], "[Telegram] r").expect("session");
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

    // ─── A rule linked to a task (#684) ─────────────────────────────────────

    use crate::native::integrations::telegram::client::{api_base_lock, set_api_base};
    use std::sync::Mutex;

    /// Every Bot API call the fake received, as `(method, body)`.
    #[derive(Clone, Default)]
    struct FakeTelegram {
        calls: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    impl FakeTelegram {
        fn methods(&self) -> Vec<String> {
            let calls = self.calls.lock().expect("calls");
            calls.iter().map(|(m, _)| m.clone()).collect()
        }

        fn sent(&self) -> Vec<serde_json::Value> {
            let calls = self.calls.lock().expect("calls");
            calls
                .iter()
                .filter(|(m, _)| m == "sendMessage")
                .map(|(_, body)| body.clone())
                .collect()
        }
    }

    /// A Bot API that answers every method `ok`, with the base pointed at it.
    /// The caller holds `api_base_lock` and resets the base when done.
    async fn fake_telegram() -> FakeTelegram {
        let fake = FakeTelegram::default();
        let state = fake.clone();
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri, body: String| {
            let state = state.clone();
            async move {
                let method = uri
                    .path()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let body = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                state.calls.lock().expect("calls").push((method, body));
                axum::Json(serde_json::json!({"ok": true, "result": {}}))
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        set_api_base(Some(base));
        fake
    }

    const TASK_PROMPT: &str = "Triage what the sender reports.";
    const REPLY_ALWAYS: &str = r#"[{"type":"reply","when":"always"}]"#;

    /// A Telegram integration that can send, one `/ask` rule `r` for chat 42
    /// linked to `link`, and a task `t1` in `status` carrying `destinations`.
    /// The task's agent does not exist, so its run fails in `prepare` and no
    /// CLI is needed.
    fn linked(dir: &Path, status: &str, destinations: &str, link: &str) -> std::path::PathBuf {
        let db = migrated(dir);
        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.execute(
            r#"UPDATE integrations SET credentials = '{"bot_token":"123:T"}' WHERE id = 'tg'"#,
            [],
        )
        .expect("give the integration a token");
        add_rule(
            &db,
            "r",
            true,
            "/ask",
            "[]",
            r#"["42"]"#,
            "2026-01-01 00:00:00 +0000 UTC",
        );
        conn.execute(
            "INSERT INTO scheduled_tasks
                (id, name, prompt, agent_slug, schedule_type, schedule_config, status,
                 destinations, run_count, stop_after_count, created_at, updated_at)
             VALUES ('t1', 'Triage', ?1, 'nope', 'interval', '{}', ?2, ?3, 0, 0,
                     '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
            rusqlite::params![TASK_PROMPT, status, destinations],
        )
        .expect("seed the task");
        conn.execute(
            "UPDATE trigger_rules SET task_id = ?1 WHERE id = 'r'",
            [link],
        )
        .expect("link the rule");
        db
    }

    fn update(update_id: i64, text: &str, chat_id: i64) -> TelegramUpdate {
        TelegramUpdate {
            update_id,
            message: Some(crate::native::gojson::GoStruct(TelegramMsg {
                message_id: 1000 + update_id,
                ..msg(text, chat_id)
            })),
        }
    }

    fn one_slot() -> &'static Semaphore {
        Box::leak(Box::new(Semaphore::new(1)))
    }

    /// `(id, triggered_by, event_payload, status)` of every run.
    fn jobs(db: &Path) -> Vec<(String, String, String, String)> {
        let conn = rusqlite::Connection::open(db).expect("open");
        let mut stmt = conn
            .prepare(
                "SELECT id, triggered_by, event_payload, status FROM job_history
                 ORDER BY started_at",
            )
            .expect("prepare");
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("query");
        rows.map(|row| row.expect("a row")).collect()
    }

    fn count(db: &Path, sql: &str) -> i64 {
        rusqlite::Connection::open(db)
            .expect("open")
            .query_row(sql, [], |row| row.get(0))
            .expect("count")
    }

    /// `(type, target, status)` of a run's deliveries once `n` have settled.
    /// Delivery is spawned, never awaited by the run, so this is a poll.
    async fn settled(db: &Path, job_id: &str, n: usize) -> Vec<(String, String, String)> {
        use crate::native::schedule::delivery::tests::rows;
        for _ in 0..1000 {
            let found = rows(db, job_id);
            if found.len() == n && found.iter().all(|row| row.3 != "pending") {
                return found.into_iter().map(|r| (r.1, r.2, r.3)).collect();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("deliveries never settled: {:?}", rows(db, job_id));
    }

    async fn dispatch(db: &Path, scheduler: Option<Arc<Scheduler>>, update: TelegramUpdate) {
        process(one_slot(), scheduler, db, "tg", "123:T", update).await;
    }

    #[test]
    fn the_task_link_comes_off_the_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = linked(dir.path(), "active", "[]", "t1");
        let rules = load_rules(&db, "tg").expect("load");
        assert_eq!(rules[0].task_id, "t1");

        add_rule(
            &db,
            "plain",
            true,
            "",
            "[]",
            r#"["42"]"#,
            "2026-02-01 00:00:00 +0000 UTC",
        );
        let rules = load_rules(&db, "tg").expect("load");
        assert_eq!(rules[1].task_id, "", "an unlinked rule reads as empty");
    }

    /// The run is the task's, recorded as Telegram's with the stripped text as
    /// its payload; the answer is the `reply` delivery — here the failure
    /// sentence, since the run failed — and it is the only message sent.
    #[tokio::test]
    async fn a_linked_rule_whose_run_fails_answers_through_the_reply_delivery() {
        let _base = api_base_lock().await;
        let telegram = fake_telegram().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = linked(dir.path(), "active", REPLY_ALWAYS, "t1");
        let scheduler = runtime::detached(&db);

        dispatch(
            &db,
            Some(scheduler),
            update(7, "/ask is the deploy green?", 42),
        )
        .await;
        let runs = jobs(&db);
        assert_eq!(runs.len(), 1, "exactly one job_history row");
        let deliveries = settled(&db, &runs[0].0, 1).await;
        set_api_base(None);

        let (_, triggered_by, payload, status) = &runs[0];
        assert_eq!(triggered_by, "telegram");
        assert_eq!(payload, "is the deploy green?", "the prefix is stripped");
        assert_eq!(status, "failed");
        assert_eq!(
            deliveries,
            vec![("reply".to_string(), "42".to_string(), "sent".to_string())]
        );
        assert_eq!(
            telegram.methods(),
            vec!["sendChatAction".to_string(), "sendMessage".to_string()],
            "typing, then one answer: the delivery's, with no inline reply"
        );
        let sent = &telegram.sent()[0];
        assert_eq!(sent["text"], ERROR_REPLY);
        assert_eq!(sent["chat_id"], 42);
        assert_eq!(sent["reply_to_message_id"], 1007, "quoting the trigger");
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM chat_sessions WHERE title LIKE '[Telegram]%'"
            ),
            0,
            "no inline trigger chat"
        );
    }

    #[tokio::test]
    async fn a_linked_task_with_no_reply_destination_runs_and_sends_nothing() {
        let _base = api_base_lock().await;
        let telegram = fake_telegram().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = linked(dir.path(), "active", "[]", "t1");

        dispatch(&db, Some(runtime::detached(&db)), update(7, "/ask hi", 42)).await;
        // Nothing to wait for by name, so give a stray delivery time to show.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        set_api_base(None);

        let runs = jobs(&db);
        assert_eq!(runs.len(), 1, "it ran, and the run is recorded");
        assert_eq!(runs[0].1, "telegram");
        assert!(telegram.sent().is_empty(), "nothing sent to the chat");
        assert_eq!(count(&db, "SELECT COUNT(*) FROM job_deliveries"), 0);
    }

    /// Every refusal writes no row and never falls back to the inline run.
    /// Pause is silent; a task that is gone, or a process with no scheduler,
    /// answers the failure sentence inline.
    #[tokio::test]
    async fn a_paused_linked_task_is_silent_and_a_dangling_link_answers_the_error_reply() {
        let _base = api_base_lock().await;
        let telegram = fake_telegram().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = linked(dir.path(), "paused", REPLY_ALWAYS, "t1");

        dispatch(&db, Some(runtime::detached(&db)), update(1, "/ask hi", 42)).await;
        assert!(telegram.sent().is_empty(), "a paused task is silent");

        rusqlite::Connection::open(&db)
            .expect("open")
            .execute("UPDATE trigger_rules SET task_id = 'gone'", [])
            .expect("dangle the link");
        dispatch(&db, Some(runtime::detached(&db)), update(2, "/ask hi", 42)).await;
        assert_eq!(telegram.sent().len(), 1, "a missing task answers");

        rusqlite::Connection::open(&db)
            .expect("open")
            .execute("UPDATE trigger_rules SET task_id = 't1'", [])
            .expect("relink");
        rusqlite::Connection::open(&db)
            .expect("open")
            .execute("UPDATE scheduled_tasks SET status = 'active'", [])
            .expect("resume the task");
        dispatch(&db, None, update(3, "/ask hi", 42)).await;
        set_api_base(None);

        let sent = telegram.sent();
        assert_eq!(sent.len(), 2, "and so does a process with no scheduler");
        for body in &sent {
            assert_eq!(body["text"], ERROR_REPLY);
        }
        assert!(jobs(&db).is_empty(), "no refusal writes a job_history row");
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM chat_sessions"),
            0,
            "and none ran inline"
        );
    }

    /// `task_id = ""` is the path every rule took before #684: typing, then the
    /// inline answer — here the failure sentence, since agent `a` does not
    /// exist — and no `job_history` row.
    #[tokio::test]
    async fn an_unlinked_rule_takes_the_inline_path_and_writes_no_job_row() {
        let _base = api_base_lock().await;
        let telegram = fake_telegram().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = linked(dir.path(), "active", REPLY_ALWAYS, "");

        dispatch(&db, Some(runtime::detached(&db)), update(7, "/ask hi", 42)).await;
        set_api_base(None);

        assert_eq!(
            telegram.methods(),
            vec!["sendChatAction".to_string(), "sendMessage".to_string()]
        );
        assert_eq!(telegram.sent()[0]["text"], ERROR_REPLY);
        assert!(jobs(&db).is_empty());
    }

    /// The dispatcher permit is held for the whole of a linked run: with a
    /// one-slot bound and the run waiting on the scheduler's permits, an
    /// unlinked rule's message is not answered until the linked run ends.
    #[tokio::test]
    async fn a_linked_run_holds_its_dispatcher_permit_until_the_run_ends() {
        let _base = api_base_lock().await;
        let telegram = fake_telegram().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = linked(dir.path(), "active", "[]", "t1");
        add_rule(
            &db,
            "plain",
            true,
            "",
            "[]",
            r#"["7"]"#,
            "2026-02-01 00:00:00 +0000 UTC",
        );
        let scheduler = runtime::detached(&db);
        let held = scheduler
            .semaphore()
            .acquire_many_owned(3)
            .await
            .expect("the scheduler's three permits");
        let sem = one_slot();

        let first = spawn_under(sem, &scheduler, &db, update(1, "/ask a", 42));
        for _ in 0..1000 {
            if sem.available_permits() == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(
            sem.available_permits(),
            0,
            "the admitted run took the permit"
        );

        let second = spawn_under(sem, &scheduler, &db, update(2, "inline", 7));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(
            telegram.methods(),
            vec!["sendChatAction".to_string()],
            "only the linked run's typing: the unlinked one waits for the permit"
        );

        drop(held);
        join(first).await;
        join(second).await;
        set_api_base(None);
        assert_eq!(jobs(&db).len(), 1, "the linked run is recorded");
        assert_eq!(
            telegram.sent().len(),
            1,
            "and then the unlinked rule answers inline"
        );
    }

    /// #691's order: the task's limiter first, the dispatcher's bound after.
    /// A message waiting in its task's queue holds no dispatcher permit, so an
    /// unlinked rule is answered while it waits.
    #[tokio::test]
    async fn a_linked_message_waiting_in_its_tasks_queue_holds_no_dispatcher_permit() {
        use crate::native::schedule::limiter::Limits;
        let _base = api_base_lock().await;
        let telegram = fake_telegram().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = linked(dir.path(), "active", "[]", "t1");
        add_rule(
            &db,
            "plain",
            true,
            "",
            "[]",
            r#"["7"]"#,
            "2026-02-01 00:00:00 +0000 UTC",
        );
        let scheduler = runtime::detached(&db);
        let running = scheduler
            .limiter()
            .admit(
                "t1",
                Limits {
                    concurrent: 1,
                    queued: 5,
                    per_hour: 10,
                },
                chrono::Utc::now(),
            )
            .await
            .expect("the task's one slot");
        let sem = one_slot();

        let queued = spawn_under(sem, &scheduler, &db, update(1, "/ask a", 42));
        for _ in 0..1000 {
            if scheduler.limiter().load("t1") == (1, 1) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(scheduler.limiter().load("t1"), (1, 1), "the message queued");
        assert_eq!(sem.available_permits(), 1, "holding no dispatcher permit");

        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            process(
                sem,
                Some(Arc::clone(&scheduler)),
                &db,
                "tg",
                "123:T",
                update(2, "inline", 7),
            ),
        )
        .await
        .expect("the unlinked rule is not held up by the queued event");
        assert_eq!(telegram.sent().len(), 1, "it answered inline");

        drop(running);
        join(queued).await;
        set_api_base(None);
        assert_eq!(jobs(&db).len(), 1, "and the queued message then ran");
    }

    fn spawn_under(
        sem: &'static Semaphore,
        scheduler: &Arc<Scheduler>,
        db: &Path,
        update: TelegramUpdate,
    ) -> tokio::task::JoinHandle<()> {
        let (db, scheduler) = (db.to_path_buf(), Arc::clone(scheduler));
        tokio::spawn(async move {
            process(sem, Some(scheduler), &db, "tg", "123:T", update).await;
        })
    }

    async fn join(handle: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(std::time::Duration::from_secs(10), handle)
            .await
            .expect("the update is processed")
            .expect("join");
    }
}
