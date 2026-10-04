//! Running an agent to completion with **no interactive permission handler and
//! no SSE** — `agent.RunAgent` plus `collectRunResult`.
//!
//! Shared by every caller that has no user watching: the scheduler (#275), the
//! Telegram trigger dispatcher (#319), and [`Runner::resume`] (#564), which is
//! the one that continues an *existing* chat rather than minting a fresh one. Go
//! shares it too — both of its callers call `agent.RunAgent` — and the reason to
//! share it here is sharper than tidiness.
//!
//! **Callers reach the CLI only through [`Runner`]** (#677), taken from
//! [`runner`]. The functions that spawn are private to this module, so a rule
//! about what may be spawned is written once, and a second harness is a second
//! implementation rather than a fourth call site. `docs/internal/native-runner.md`
//! has the seam and the per-harness table of fail-open flag combinations.
//!
//! **The trap this file exists to hold in one place:** the run must go through
//! [`crate::claude::client::query`], the *one-shot*, and never through
//! `Session`. A `Session` sets `session_mode`, and `process.rs`'s reader then
//! deliberately neither closes stdin nor stops at the `result` event, "so the
//! subprocess survives for the next send". A headless run has no next send, so
//! the event channel never closes, the drain blocks past the answer, and the
//! run sits until its timeout before being recorded as a failure. That shipped
//! once already, in the scheduler, and was invisible to CI, to byte-identical
//! live parity and to fifty unit tests, because none of them ran a task. One
//! implementation means the second caller cannot reintroduce it.

use std::sync::Arc;

use crate::native::chat::live;
use crate::native::chat::runner::{self, RunSpec};
use crate::native::db;

/// One harness that can run an agent with nobody watching (#677).
///
/// [`RunSpec`], [`RunResult`] and [`ExecutionSettings`] are the neutral types:
/// a caller builds a spec and reads a result without naming the CLI behind
/// them. What is the harness's own is how a spec becomes flags, and which of
/// those flags leave the run unchecked — [`Self::run`] and [`Self::fail_open`].
///
/// Generic rather than `dyn`: the methods return futures, so a trait object
/// would need them boxed, and every caller is on the scheduler's or an inbound
/// worker's `Send` path. A second harness makes [`runner`] answer an enum of
/// runners; it does not change a call site.
pub trait Runner: Send + Sync + Sized {
    /// The harness's stable id, lower-case — `"claude"`.
    fn harness(&self) -> &'static str;

    /// Why a headless run of `spec` would be **fail-open** on this harness —
    /// started with a flag combination under which a tool call nobody approved
    /// can run — or `None` when it would not be.
    ///
    /// A declaration, not an enforcement: nothing here refuses a run. It is
    /// decided from the spec alone, before anything is spawned or bound, so a
    /// caller can refuse on it without knowing the harness. A spec the harness
    /// would refuse to build at all answers `None`: that run fails closed.
    fn fail_open(&self, spec: &RunSpec) -> Option<String>;

    /// Run `prompt` to completion on a fresh session and answer what it
    /// produced. `timeout` covers every stage that can await.
    ///
    /// `on_spawn` is told the subprocess's pid before a byte of its output is
    /// read — the scheduler records it on the job row (#594). The other callers
    /// pass `None`.
    fn run(
        &self,
        spec: &RunSpec,
        prompt: &str,
        timeout: std::time::Duration,
        on_spawn: Option<crate::claude::SpawnHook>,
    ) -> impl std::future::Future<Output = Result<RunResult, String>> + Send;

    /// Run **one turn on an existing chat**, resuming its session, and write
    /// what it produced back onto the chat.
    ///
    /// Provided, because none of it is the harness's: the busy lock, the chat
    /// lookup and the write-back are Agento's own, and the one harness-specific
    /// step in the middle is [`Self::run`]. See [`run_resumed`] for the rules.
    fn resume(
        &self,
        db_path: &std::path::Path,
        chat_id: &str,
        prompt: &str,
        settings: &ExecutionSettings,
        timeout: std::time::Duration,
    ) -> impl std::future::Future<Output = Result<RunResult, String>> + Send {
        run_resumed(self, db_path, chat_id, prompt, settings, timeout)
    }
}

/// The runner every headless caller uses. One constructor, so the answer to
/// "which harness runs this" has one place to change.
pub fn runner() -> ClaudeRunner {
    ClaudeRunner
}

/// The Claude Code CLI, through [`crate::claude::client::query`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeRunner;

impl ClaudeRunner {
    /// [`Runner::harness`]'s answer.
    pub const HARNESS: &'static str = "claude";

    /// The fail-open rule over the options a run is spawned with, which is
    /// where the two flags live (`claude/options.rs`'s argument builder).
    ///
    /// Two combinations, and the second is the wider one:
    ///
    /// - `--permission-mode bypassPermissions`: every permission check is
    ///   skipped.
    /// - `--allow-dangerously-skip-permissions`: the CLI's own help calls it
    ///   "bypassing all permission checks as an option, without it being
    ///   enabled by default". It skips nothing by itself, but a run that sends
    ///   it can be switched into bypass while it runs, so it is not provably
    ///   closed. `Options::new()` sets it, and only `with_default_permissions`
    ///   clears it — so an explicit `plan` or `dontAsk` is reported here too.
    fn fail_open_options(options: &crate::claude::options::Options) -> Option<String> {
        use crate::claude::options::permission_mode::BYPASS_PERMISSIONS;
        if options.permission_mode == BYPASS_PERMISSIONS {
            Some(format!(
                "--permission-mode {BYPASS_PERMISSIONS} skips every permission check"
            ))
        } else if options.allow_dangerously_skip_permissions {
            Some(format!(
                "--allow-dangerously-skip-permissions is sent with --permission-mode {}, \
                 so the run can be switched to {BYPASS_PERMISSIONS}",
                options.permission_mode
            ))
        } else {
            None
        }
    }
}

impl Runner for ClaudeRunner {
    fn harness(&self) -> &'static str {
        Self::HARNESS
    }

    fn fail_open(&self, spec: &RunSpec) -> Option<String> {
        // The same resolution and the same builder calls `build_options` makes
        // for a run with no permission handler, so the declaration cannot
        // drift from what is spawned. An unknown mode is an `Err` there, and
        // the run fails before a subprocess exists.
        let options = runner::headless_permission_options(spec).ok()?;
        Self::fail_open_options(&options)
    }

    async fn run(
        &self,
        spec: &RunSpec,
        prompt: &str,
        timeout: std::time::Duration,
        on_spawn: Option<crate::claude::SpawnHook>,
    ) -> Result<RunResult, String> {
        run_headless(spec, prompt, timeout, on_spawn).await
    }
}

/// What one run produced. `agent.AgentResult`, narrowed to the fields its two
/// callers store — the thinking, cost and per-model breakdowns are collected
/// by Go and then dropped by this caller.
#[derive(Debug, Default, Clone)]
pub struct RunResult {
    pub session_id: String,
    pub answer: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    /// One sentence per MCP server whose hosted tools the CLI never handed the
    /// model (#556). A scheduled run has nobody reading the app log, so this
    /// travels out to `job_history` — see
    /// [`crate::native::schedule::executor`]. Empty is the normal case, and a
    /// non-empty one never fails the run.
    pub tools_not_offered: Vec<String>,
}

/// `agent.RunAgent`: build the options, spawn the CLI, drain it, answer the
/// result.
///
/// `deadline` covers **every stage that can await**, as Go's
/// `context.WithTimeout` around the whole call does — `build_options` included,
/// since it is not arithmetic but starts an in-process MCP server per
/// integration the agent names. It is a shared `Instant` rather than one future
/// wrapping everything, because `Stream` has no `Drop`: cancelling a future that
/// owns one abandons the subprocess instead of stopping it, so `close()` has to
/// stay reachable.
///
/// `on_spawn` is told the CLI's pid before a byte of its output is read — the
/// scheduler records it on the job row (#594). The other callers pass `None`.
///
/// Private: callers reach it through [`Runner::run`].
async fn run_headless(
    spec: &RunSpec,
    prompt: &str,
    timeout: std::time::Duration,
    on_spawn: Option<crate::claude::SpawnHook>,
) -> Result<RunResult, String> {
    // `resolveSystemPrompt`, which Go calls inside `RunAgent` — so it applies to
    // **every** caller, and returning its error unwrapped is what makes an agent
    // with an unresolvable `{{name}}` a recorded failure rather than a run that
    // ships the raw placeholder to the model.
    //
    // It lives here rather than in each caller for the reason the one-shot does:
    // the scheduler had it and the dispatcher did not, which is exactly the
    // divergence a shared function exists to prevent. `build_options` cannot do
    // it — the chat path deliberately interpolates leniently, so the strictness
    // is the headless caller's.
    if let Some(agent) = spec.agent.as_ref() {
        crate::native::template::interpolate(&agent.system_prompt).map_err(|e| e.to_string())?;
    }

    let deadline = tokio::time::Instant::now() + timeout;

    // The refusal this port has that Go does not — an agent whose tools cannot
    // be hosted here. The caller decides what to do with it; both callers
    // record it rather than dropping it.
    let (mut options, tool_servers, hosted_tools) =
        tokio::time::timeout_at(deadline, runner::build_options(spec, None))
            .await
            .map_err(|_| DEADLINE_EXCEEDED.to_string())?
            .map_err(|e| format!("agent setup: {e}"))?;
    options.on_spawn = on_spawn;

    let mut stream = match tokio::time::timeout_at(
        deadline,
        crate::claude::client::query(prompt, options),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => {
            drop(tool_servers);
            return Err(format!("starting agent: {e}"));
        }
        Err(_) => {
            drop(tool_servers);
            return Err(DEADLINE_EXCEEDED.to_string());
        }
    };

    let collected =
        tokio::time::timeout_at(deadline, collect_run_result(&mut stream, &hosted_tools))
            .await
            .unwrap_or_else(|_| Err(DEADLINE_EXCEEDED.to_string()));

    stream.close();
    // The in-process tool listeners outlive the subprocess and stop when
    // dropped, so this is after `close` rather than before it.
    drop(tool_servers);
    collected
}

/// A `RunSpec` for a headless run of `agent`, with no session pinned and
/// `settings` imposed over the agent's own choices.
///
/// `buildRunOptions` sets neither session field, so the CLI generates its own id
/// and the caller stores it afterwards.
///
/// **Empty is "no choice was recorded"** for every field of `settings`, so the
/// zero value is the agent's model, the agent's permission mode — prompts
/// denied when it has none (#675) — and no working directory or settings
/// profile. The scheduler passes an empty model, and names `bypass` for a
/// scheduled or manual run of an agent with no mode of its own.
pub fn headless_spec(
    db_path: &std::path::Path,
    agent: crate::native::agents::Agent,
    settings: &ExecutionSettings,
) -> RunSpec {
    // The caller's model wins over the agent's own, and it is written into
    // `agent.model` rather than into the closure below: `build_options` reads
    // the agent for any spec that has one, and this spec always has one, so the
    // closure is the arm that never runs. `resume_spec` has to write both
    // because a chat may have no agent.
    let mut agent = agent;
    if !settings.model.is_empty() {
        agent.model = settings.model.clone();
    }
    RunSpec {
        agent: Some(agent),
        // Never called: both callers synthesize an agent rather than passing
        // `None`, because Go's own resolvers return a non-nil config — and a
        // non-nil config with empty capabilities gets all twelve built-in
        // tools where a nil one gets none.
        no_agent_model: Box::new(String::new),
        settings: Arc::new(runner::TurnSettings::from_db(db_path)),
        working_dir: settings.working_directory.clone(),
        settings_profile_id: settings.settings_profile_id.clone(),
        // A trigger rule may impose one (#565), and the scheduler names its own
        // (#675). Empty is no conversation-level choice: `build_options`
        // applies the agent's own mode, and denies prompts when it has none.
        permission_mode: settings.permission_mode.clone(),
        resume_session_id: None,
        custom_session_id: String::new(),
    }
}

/// The per-run execution choices a headless caller may impose on a chat.
///
/// The four `trigger_rules` columns #563 added that reach a [`RunSpec`] — the
/// fifth, `timeout_minutes`, is a [`std::time::Duration`] argument to
/// [`Runner::resume`] rather than a spec field, because nothing about a `RunSpec`
/// is time-bounded.
///
/// **Empty means "no choice was recorded"** for every field, exactly as it does
/// on `chat_sessions.permission_mode` and on a trigger rule — so a rule that
/// sets nothing runs the chat on the chat's own settings, and a rule that sets
/// one field overrides only that one.
#[derive(Debug, Default, Clone)]
pub struct ExecutionSettings {
    /// Overrides the agent's model (or, for a chat with no agent, the chat's).
    pub model: String,
    pub working_directory: String,
    pub settings_profile_id: String,
    /// One of [`crate::native::chats::CHAT_PERMISSION_MODES`]. `build_options`
    /// refuses any other value, which fails the run, so a caller that would
    /// rather run restricted than not at all maps it before it gets here.
    pub permission_mode: String,
}

/// A [`RunSpec`] that **continues** `row`'s CLI session, with `settings`
/// applied over the chat's own.
///
/// The counterpart to [`headless_spec`], and the differences are the whole
/// point:
///
/// - `resume_session_id` is the chat's `sdk_session_id`, which is what makes the
///   turn a continuation rather than a fresh conversation (`runner.rs` prefers
///   it over `custom_session_id`).
/// - `custom_session_id` stays **empty**, including for a chat that has never
///   run. Pinning the chat's own id there — which the interactive turn does —
///   would file the transcript under an id the write-back then has no reason to
///   store; instead the CLI mints one and [`run_resumed`] records it, the way
///   both existing headless callers do.
///
/// It takes `db_path` and `agent` that the [`RunSpec`] needs and a `ChatRow`
/// does not carry: the settings row is read lazily through
/// [`runner::TurnSettings`], and the agent is the other half of what
/// [`runner::load`] returns.
pub fn resume_spec(
    db_path: &std::path::Path,
    row: &crate::native::chat::runner::ChatRow,
    agent: Option<crate::native::agents::Agent>,
    settings: &ExecutionSettings,
) -> RunSpec {
    let turn_settings = Arc::new(runner::TurnSettings::from_db(db_path));

    // The caller's model wins, then the chat's own, then — inside the closure —
    // the user's default. `build_options` reads the agent's `model` field for an
    // agent chat and this closure only for a chat with none, so an override has
    // to be written into both arms or it applies to half the chats.
    let mut agent = agent;
    if let Some(agent) = agent.as_mut() {
        if !settings.model.is_empty() {
            agent.model = settings.model.clone();
        }
    }
    let no_agent_model = match pick(&settings.model, &row.model) {
        model if model.is_empty() => {
            let settings = Arc::clone(&turn_settings);
            Box::new(move || settings.default_model()) as Box<dyn Fn() -> String + Send + Sync>
        }
        model => Box::new(move || model.clone()),
    };

    RunSpec {
        agent,
        no_agent_model,
        settings: turn_settings,
        working_dir: pick(&settings.working_directory, &row.working_dir),
        settings_profile_id: pick(&settings.settings_profile_id, &row.settings_profile_id),
        permission_mode: pick(&settings.permission_mode, &row.permission_mode),
        // `None` for a chat that has never run, which falls through to the
        // empty `custom_session_id` below and leaves the CLI to mint an id.
        resume_session_id: Some(row.sdk_session_id.clone()).filter(|s| !s.is_empty()),
        custom_session_id: String::new(),
    }
}

/// `override` when it is set, otherwise `fallback`. Empty is "no choice".
fn pick(overridden: &str, fallback: &str) -> String {
    if overridden.is_empty() {
        fallback.to_string()
    } else {
        overridden.to_string()
    }
}

/// Run **one turn on an existing chat**, resuming its CLI session, and write
/// what it produced back onto the chat.
///
/// # It takes the chat's busy lock, and it is the first headless run that does
///
/// `live::registry().try_lock` had exactly one caller, the interactive turn.
/// That was safe only because the scheduler and the trigger dispatcher mint a
/// **fresh** chat per run, so no headless run could ever collide with a UI one.
/// Resuming an existing chat removes that guarantee: a `POST
/// /api/chats/{id}/messages` and an inbound turn would otherwise spawn two CLI
/// processes against one row, each reading the other's stale `sdk_session_id`.
/// So the lock is taken **before** anything is spawned and a held one is
/// answered with [`live::CHAT_BUSY`] — the same string the chat route's 409
/// carries, so an inbound worker can queue on it rather than fail.
///
/// The release is a guard's `Drop` rather than paired calls, because a leaked
/// lock wedges the chat for the life of the process and the error, timeout and
/// panic paths are exactly the ones a paired release is dropped from.
///
/// **It fences the two runs, not the two writes, and the residual window is
/// deliberate.** The interactive turn releases the lock when its *stream* ends,
/// before `chat/persist::commit` lands — Go's own ordering, reproduced on
/// purpose and argued in [`live`]'s module header. So a `run_resumed` starting
/// in that window loads an `sdk_session_id` the UI turn is still about to write.
/// Two CLI processes on one chat, which is the outcome this lock exists to
/// prevent, remains impossible; a resume from one turn earlier does not.
/// Narrowing it means changing when the interactive turn releases, which is a
/// divergence from that ordering and belongs to whoever decides to take it.
///
/// Private: callers reach it through [`Runner::resume`], whose provided body
/// this is. The spawn in the middle is `runner`'s; everything around it is not.
///
/// # Token totals are **incremented**, unlike every other headless write-back
///
/// `schedule/executor.rs` and `trigger/dispatcher.rs` both *replace* the four
/// totals, which is correct for them: they own a chat nothing else ever wrote
/// to. A resumed chat is shared — a user can answer in the UI between two
/// inbound turns — so replacing would silently erase the UI turns' usage. This
/// increments, the way `chat/persist.rs` does for the interactive turn. **Do not
/// unify this statement with the executor's.**
async fn run_resumed<R: Runner>(
    runner: &R,
    db_path: &std::path::Path,
    chat_id: &str,
    prompt: &str,
    settings: &ExecutionSettings,
    timeout: std::time::Duration,
) -> Result<RunResult, String> {
    if !live::registry().try_lock(chat_id) {
        return Err(live::CHAT_BUSY.to_string());
    }
    let _guard = BusyGuard {
        id: chat_id.to_string(),
    };

    let loaded = {
        let (db, id) = (db_path.to_path_buf(), chat_id.to_string());
        db::blocking("headless resume chat lookup", move || {
            runner::load(&db, &id)
        })
        .await
        .unwrap_or_else(|| Err("the chat lookup task failed".to_string()))?
    };
    let Some((row, agent)) = loaded else {
        return Err(format!("chat {chat_id:?} not found"));
    };

    let spec = resume_spec(db_path, &row, agent, settings);
    let result = runner.run(&spec, prompt, timeout, None).await;

    let result = match result {
        Ok(result) => result,
        Err(e) => {
            // The user turn is stored with no answer, `saveSessionMessages`'
            // shape — so a failed inbound turn is visible in the chat rather
            // than vanishing. Nothing else is touched: the totals and the
            // `sdk_session_id` stay as they were, which is what lets the next
            // attempt resume the same CLI session.
            //
            // A `None` here is a panic inside the section, and it costs the
            // stored question and nothing else — the run has already failed and
            // the caller is answered with its error either way. Best-effort in
            // Go too, so it is ignored, as the dispatcher's two `save_messages`
            // calls are.
            let (db, id, prompt) = (db_path.to_path_buf(), row.id.clone(), prompt.to_string());
            db::blocking("headless resume failed turn", move || {
                let conn = match db::open_read_write(&db) {
                    Ok(conn) => conn,
                    Err(e) => {
                        log::warn!("failed to store user message: {e}");
                        return;
                    }
                };
                save_messages(&conn, &id, &prompt, "");
            })
            .await;
            return Err(e);
        }
    };

    // A `None` here is a panic between [`save_resumed_results`]' three
    // untransacted writes — `db::blocking` says outright that what a half-done
    // section left behind is the caller's problem. What it can leave is the
    // `UPDATE` applied and neither message stored: `sdk_session_id` and the
    // totals advanced for a turn `chat_messages` has no record of, so the chat
    // reads as empty while the next `--resume` continues a session that did
    // happen. It is a panic rather than a rusqlite failure — every statement in
    // there logs and carries on — and the run itself succeeded, so the caller is
    // still answered `Ok` and posts the reply it got. `db::blocking` logs it
    // under the label; there is nothing better to do here than say so.
    {
        let (db, id, prior, run, prompt) = (
            db_path.to_path_buf(),
            row.id.clone(),
            row.sdk_session_id.clone(),
            result.clone(),
            prompt.to_string(),
        );
        db::blocking("headless resume write-back", move || {
            save_resumed_results(&db, &id, &prior, &run, &prompt)
        })
        .await;
    }

    Ok(result)
}

/// Releases the chat's busy lock on **every** exit path — the error and timeout
/// ones included, which is why it is a `Drop` and not a pair of calls.
///
/// Unlike the interactive turn's guard, releasing here cannot be early: nothing
/// is put in the `sessions` map (a headless run has no `/stop`, `/input` or
/// `/permission` to serve), and the write-back has already run.
struct BusyGuard {
    id: String,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        live::registry().release(&self.id);
    }
}

/// The write-back: the totals onto the chat row, then the two messages.
///
/// `write_session_results`' three-independent-statements shape (see
/// `schedule/executor.rs` for why one transaction would be a wider blast radius
/// than the code being ported has), with the two divergences [`run_resumed`]
/// documents: the totals are **incremented**, and `sdk_session_id` is written
/// only when the run reported one — `prior_sdk_session_id` being what the chat
/// carried before the run, and what it keeps when the run reported none.
///
/// **`title` is not written here.** `chat/persist.rs` derives one from the first
/// user message because a UI chat is created before anyone has typed; a chat
/// this function runs against was created by its caller, which knows the origin
/// and titles the row then (`[Task] <name>` for the scheduler, `[Telegram] …`
/// for the dispatcher). Deriving one here would rename a chat the caller had
/// already named.
fn save_resumed_results(
    db_path: &std::path::Path,
    chat_id: &str,
    prior_sdk_session_id: &str,
    result: &RunResult,
    prompt: &str,
) {
    let conn = match db::open_read_write(db_path) {
        Ok(conn) => conn,
        Err(e) => {
            log::warn!("failed to update chat session after a resumed run: {e}");
            return;
        }
    };

    // Only overwrite the CLI session id when the turn returned one — the
    // reasoning at `chat/persist.rs`: blanking it would make the next turn start
    // a new CLI session instead of resuming this one.
    let sdk_session_id = if result.session_id.is_empty() {
        prior_sdk_session_id
    } else {
        result.session_id.as_str()
    };
    if let Err(e) = conn.execute(
        "UPDATE chat_sessions SET
            sdk_session_id = ?1,
            total_input_tokens = total_input_tokens + ?2,
            total_output_tokens = total_output_tokens + ?3,
            total_cache_creation_tokens = total_cache_creation_tokens + ?4,
            total_cache_read_tokens = total_cache_read_tokens + ?5,
            updated_at = ?6
         WHERE id = ?7",
        rusqlite::params![
            sdk_session_id,
            result.input_tokens,
            result.output_tokens,
            result.cache_creation_tokens,
            result.cache_read_tokens,
            crate::native::gotime::now_go_text(),
            chat_id,
        ],
    ) {
        log::warn!("failed to update chat session after a resumed run: {e}");
    }

    save_messages(&conn, chat_id, prompt, &result.answer);
}

/// `saveSessionMessages`: the user turn, then the assistant turn when there is
/// one.
///
/// **The user turn is stored even when there is no answer** — the trigger
/// dispatcher's shape, which #564 asks for by name, and *not*
/// `chat/persist.rs`'s. That file guards both inserts on a non-empty answer
/// because "an interrupted stream must not leave an orphaned user message …
/// the two would diverge on the next resume", and this is the first headless
/// path where the next resume is real. The dispatcher's shape still wins: an
/// inbound message the user can see was asked, with no reply, is the outcome
/// that surface needs, and the CLI reconciles its own transcript on the next
/// `--resume` rather than being driven from `chat_messages`.
///
/// Each insert is logged on its own, as `saveSessionMessages` does: a failed
/// user insert must not skip the assistant one, because the answer has already
/// gone out to whoever asked.
fn save_messages(conn: &rusqlite::Connection, chat_id: &str, prompt: &str, answer: &str) {
    if let Err(e) = append_message(conn, chat_id, "user", prompt) {
        log::warn!("failed to store user message: {e}");
    }
    if !answer.is_empty() {
        if let Err(e) = append_message(conn, chat_id, "assistant", answer) {
            log::warn!("failed to store assistant message: {e}");
        }
    }
}

/// `AppendMessage`, the dispatcher's spelling of it.
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

/// What `context.DeadlineExceeded` reaches Go's caller as, and therefore what
/// lands in `job_history.error_message` for a run that ran out of time.
pub const DEADLINE_EXCEEDED: &str = "context deadline exceeded";

/// `collectRunResult`: drain every event, keep the last result.
///
/// The drain does **not** stop at the result event, and that is Go's comment
/// rather than an accident: the subprocess still has its transcript to write,
/// and returning early would race the scanner against a half-written file.
async fn collect_run_result(
    stream: &mut crate::claude::client::Stream,
    hosted_tools: &[crate::native::chat::runner::HostedTools],
) -> Result<RunResult, String> {
    let mut result: Option<RunResult> = None;
    let mut result_err: Option<String> = None;
    let mut tools_not_offered: Vec<String> = Vec::new();

    while let Some(event) = stream.next_event().await {
        // The `init` frame says what the model was actually given, which is the
        // only place a dropped tool list is visible (#556). A scheduled run has
        // no stream to put a synthetic frame on, so the finding travels out on
        // the result and reaches the `job_history` row instead.
        if let Some(system) = event.system.as_ref() {
            if system.subtype == crate::claude::messages::system_subtype::INIT {
                for dropped in
                    crate::native::chat::runner::report_tools_offered(hosted_tools, system)
                {
                    if dropped.whole_server {
                        tools_not_offered.push(dropped.message());
                    }
                }
            }
        }
        let Some(r) = event.result.as_ref() else {
            continue;
        };
        if r.is_error {
            result_err = Some(build_result_error(r));
        } else {
            result = Some(RunResult {
                session_id: r.session_id.clone(),
                answer: r.result.clone(),
                input_tokens: r.usage.input_tokens,
                output_tokens: r.usage.output_tokens,
                cache_creation_tokens: r.usage.cache_creation_input_tokens,
                cache_read_tokens: r.usage.cache_read_input_tokens,
                tools_not_offered: Vec::new(),
            });
        }
    }

    if let Some(err) = result_err {
        return Err(err);
    }
    // Attached after the loop, so it is carried by whichever `result` frame
    // turned out to be the last one — `init` always precedes them all.
    let mut result =
        result.ok_or_else(|| "agent finished without returning a result".to_string())?;
    result.tools_not_offered = tools_not_offered;
    Ok(result)
}

/// `buildResultError`, message for message.
fn build_result_error(r: &crate::claude::messages::Result) -> String {
    let mut msg = r.result.clone();
    if msg.is_empty() && !r.errors.is_empty() {
        msg = r.errors.join("; ");
    }
    if msg.is_empty() {
        msg = format!("subtype={}", r.subtype);
    }
    format!("agent error: {msg}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_error_prefers_the_message_then_the_errors_then_the_subtype() {
        let mut r = crate::claude::messages::Result {
            subtype: "error_max_turns".to_string(),
            ..Default::default()
        };
        assert_eq!(
            build_result_error(&r),
            "agent error: subtype=error_max_turns"
        );

        r.errors = vec!["one".to_string(), "two".to_string()];
        assert_eq!(build_result_error(&r), "agent error: one; two");

        r.result = "the message".to_string();
        assert_eq!(build_result_error(&r), "agent error: the message");
    }

    /// A path that does not exist, so any eager settings read would show up as
    /// an empty answer rather than as the row's own value — the trick
    /// `turn.rs`'s own model test uses.
    const NO_DB: &str = "/nonexistent/agento/definitely-not-a-db";

    fn chat_row() -> crate::native::chat::runner::ChatRow {
        crate::native::chat::runner::ChatRow {
            id: "chat-1".to_string(),
            title: "New Chat".to_string(),
            agent_slug: String::new(),
            sdk_session_id: String::new(),
            working_dir: "/from/the/chat".to_string(),
            model: "chat-model".to_string(),
            settings_profile_id: "chat-profile".to_string(),
            permission_mode: "plan".to_string(),
        }
    }

    /// The one field that makes this a *resume*: it comes from the chat row, and
    /// an empty one is `None` rather than `Some("")` — which is what sends a
    /// never-run chat down `build_options`' third branch, where it supplies no
    /// session flag at all.
    #[test]
    fn a_never_run_chat_pins_no_session_and_a_run_one_resumes_its_own() {
        let mut row = chat_row();
        let spec = resume_spec(
            std::path::Path::new(NO_DB),
            &row,
            None,
            &ExecutionSettings::default(),
        );
        assert_eq!(spec.resume_session_id, None);
        assert_eq!(
            spec.custom_session_id, "",
            "the CLI mints the id and the write-back stores it; pinning the chat's own \
             would file the transcript under an id nothing then records"
        );

        row.sdk_session_id = "sdk-7".to_string();
        let spec = resume_spec(
            std::path::Path::new(NO_DB),
            &row,
            None,
            &ExecutionSettings::default(),
        );
        assert_eq!(spec.resume_session_id.as_deref(), Some("sdk-7"));
        assert_eq!(spec.custom_session_id, "");
    }

    /// Empty means "no choice was recorded", so an unset caller runs the chat on
    /// the chat's own settings.
    #[test]
    fn empty_execution_settings_leave_the_chats_own_choices_alone() {
        let spec = resume_spec(
            std::path::Path::new(NO_DB),
            &chat_row(),
            None,
            &ExecutionSettings::default(),
        );
        assert_eq!(spec.working_dir, "/from/the/chat");
        assert_eq!(spec.settings_profile_id, "chat-profile");
        assert_eq!(spec.permission_mode, "plan");
        assert_eq!(
            (spec.no_agent_model)(),
            "chat-model",
            "and the chat's model, without opening the settings"
        );
    }

    /// Each of the four overrides independently, which is what lets a rule set
    /// one field and inherit the rest.
    #[test]
    fn each_execution_setting_overrides_the_chats_own() {
        let settings = ExecutionSettings {
            model: "rule-model".to_string(),
            working_directory: "/from/the/rule".to_string(),
            settings_profile_id: "rule-profile".to_string(),
            permission_mode: "bypass".to_string(),
        };
        let spec = resume_spec(std::path::Path::new(NO_DB), &chat_row(), None, &settings);
        assert_eq!(spec.working_dir, "/from/the/rule");
        assert_eq!(spec.settings_profile_id, "rule-profile");
        assert_eq!(spec.permission_mode, "bypass");
        assert_eq!((spec.no_agent_model)(), "rule-model");
    }

    /// The four settings reach a fresh headless run too, not only a resumed
    /// one — which is what a trigger rule needs (#565). The model goes onto the
    /// agent, because that is the arm `build_options` reads for a spec that has
    /// one, and `headless_spec` always has one.
    #[test]
    fn headless_execution_settings_reach_the_spec() {
        let agent = crate::native::agents::Agent {
            name: "A".to_string(),
            slug: "a".to_string(),
            description: String::new(),
            model: "agent-model".to_string(),
            thinking: String::new(),
            permission_mode: String::new(),
            system_prompt: String::new(),
            capabilities: Default::default(),
            claude_config_dir: String::new(),
        };

        let spec = headless_spec(
            std::path::Path::new(NO_DB),
            agent.clone(),
            &ExecutionSettings::default(),
        );
        assert_eq!(spec.working_dir, "");
        assert_eq!(spec.settings_profile_id, "");
        assert_eq!(
            spec.permission_mode, "",
            "no choice recorded, so `build_options` applies the agent's own, or denies prompts"
        );
        assert_eq!(
            spec.agent.as_ref().map(|a| a.model.as_str()),
            Some("agent-model"),
            "the zero value is exactly what a headless run did before #565"
        );

        let spec = headless_spec(
            std::path::Path::new(NO_DB),
            agent,
            &ExecutionSettings {
                model: "rule-model".to_string(),
                working_directory: "/from/the/rule".to_string(),
                settings_profile_id: "rule-profile".to_string(),
                permission_mode: "plan".to_string(),
            },
        );
        assert_eq!(spec.working_dir, "/from/the/rule");
        assert_eq!(spec.settings_profile_id, "rule-profile");
        assert_eq!(spec.permission_mode, "plan");
        assert_eq!(
            spec.agent.as_ref().map(|a| a.model.as_str()),
            Some("rule-model")
        );
    }

    /// The model override has to be written into **both** arms `build_options`
    /// reads: an agent chat takes `agent.model` and never consults the closure,
    /// so setting only the closure would apply the rule's model to exactly the
    /// chats that have no agent.
    #[test]
    fn the_model_override_reaches_an_agent_chat_too() {
        let agent = crate::native::agents::Agent {
            name: "A".to_string(),
            slug: "a".to_string(),
            description: String::new(),
            model: "agent-model".to_string(),
            thinking: String::new(),
            permission_mode: String::new(),
            system_prompt: String::new(),
            capabilities: Default::default(),
            claude_config_dir: String::new(),
        };

        let spec = resume_spec(
            std::path::Path::new(NO_DB),
            &chat_row(),
            Some(agent.clone()),
            &ExecutionSettings::default(),
        );
        assert_eq!(
            spec.agent.as_ref().map(|a| a.model.as_str()),
            Some("agent-model"),
            "no override leaves the agent's own model, empty ones included"
        );

        let settings = ExecutionSettings {
            model: "rule-model".to_string(),
            ..Default::default()
        };
        let spec = resume_spec(
            std::path::Path::new(NO_DB),
            &chat_row(),
            Some(agent),
            &settings,
        );
        assert_eq!(
            spec.agent.as_ref().map(|a| a.model.as_str()),
            Some("rule-model")
        );
    }

    fn agent_with_mode(mode: &str) -> crate::native::agents::Agent {
        crate::native::agents::Agent {
            name: "A".to_string(),
            slug: "a".to_string(),
            description: String::new(),
            model: String::new(),
            thinking: String::new(),
            permission_mode: mode.to_string(),
            system_prompt: String::new(),
            capabilities: Default::default(),
            claude_config_dir: String::new(),
        }
    }

    /// Every stored spelling, on the run and on the agent, against what
    /// `build_options` spawns for it. `bypass` is fail-open by its mode;
    /// `plan` and `dontAsk` are fail-open by the flag `Options::new()` sets
    /// and they never clear; `default` and the unchosen run send neither.
    ///
    /// The list is [`crate::native::chats::CHAT_PERMISSION_MODES`] itself, so
    /// a mode added there has no answer here until someone gives it one.
    #[test]
    fn claude_declares_bypass_and_the_allow_flag_fail_open_for_every_mode() {
        let runner = runner();
        assert_eq!(runner.harness(), "claude");

        let fail_open = |run: &str, agent: &str| {
            let spec = headless_spec(
                std::path::Path::new(NO_DB),
                agent_with_mode(agent),
                &ExecutionSettings {
                    permission_mode: run.to_string(),
                    ..Default::default()
                },
            );
            runner.fail_open(&spec)
        };

        for mode in crate::native::chats::CHAT_PERMISSION_MODES {
            let want = match mode {
                "" | "default" => None,
                "bypass" => Some("--permission-mode bypassPermissions skips"),
                "plan" => {
                    Some("--allow-dangerously-skip-permissions is sent with --permission-mode plan")
                }
                "dontAsk" => Some(
                    "--allow-dangerously-skip-permissions is sent with --permission-mode dontAsk",
                ),
                other => panic!("permission mode {other:?} has no fail-open answer"),
            };
            // The run's own mode, and the same mode stored on the agent with
            // no choice on the run: the two ways a headless run gets one.
            for (run, agent) in [(mode, ""), ("", mode)] {
                let got = fail_open(run, agent);
                match want {
                    None => assert_eq!(got, None, "run={run:?} agent={agent:?}"),
                    Some(prefix) => assert!(
                        got.as_deref().is_some_and(|r| r.starts_with(prefix)),
                        "run={run:?} agent={agent:?} got={got:?}"
                    ),
                }
            }
        }

        // A run's mode beats its agent's, in both directions.
        assert_eq!(fail_open("default", "bypass"), None);
        assert!(fail_open("bypass", "default").is_some());

        // A mode `build_options` refuses is a run that never starts.
        assert_eq!(fail_open("yolo", ""), None);
        assert_eq!(fail_open("", "yolo"), None);
    }

    /// The declaration reads the two fields the argument builder turns into
    /// flags, so it is pinned against them directly as well as through a spec.
    #[test]
    fn the_fail_open_rule_reads_the_mode_and_the_allow_flag() {
        use crate::claude::options::{permission_mode, Options};

        // `Options::new()` is bypass with the flag: the SDK's own default.
        assert!(ClaudeRunner::fail_open_options(&Options::new()).is_some());
        assert_eq!(
            ClaudeRunner::fail_open_options(&Options::new().with_default_permissions()),
            None
        );
        assert_eq!(
            ClaudeRunner::fail_open_options(
                &Options::new()
                    .with_default_permissions()
                    .with_permission_mode(permission_mode::DONT_ASK)
            ),
            None,
            "the unchosen run: dontAsk with the flag cleared"
        );
        let flag_only = ClaudeRunner::fail_open_options(
            &Options::new().with_permission_mode(permission_mode::PLAN),
        );
        assert!(
            flag_only.is_some_and(|r| r.contains("--allow-dangerously-skip-permissions")),
            "plan alone does not clear the flag"
        );
    }
}
