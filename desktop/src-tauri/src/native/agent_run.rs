//! Running an agent to completion with **no interactive permission handler and
//! no SSE** — `agent.RunAgent` plus `collectRunResult`.
//!
//! Shared by the two callers that have no user watching: the scheduler (#275)
//! and the Telegram trigger dispatcher (#319). Go shares it too — both call
//! `agent.RunAgent` — and the reason to share it here is sharper than tidiness.
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

use crate::native::chat::runner::{self, RunSpec};

/// What one run produced. `agent.AgentResult`, narrowed to the fields its two
/// callers store — the thinking, cost and per-model breakdowns are collected
/// by Go and then dropped by this caller.
#[derive(Debug, Default)]
pub struct RunResult {
    pub session_id: String,
    pub answer: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
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
pub async fn run_headless(
    spec: &RunSpec,
    prompt: &str,
    timeout: std::time::Duration,
) -> Result<RunResult, String> {
    let deadline = tokio::time::Instant::now() + timeout;

    // The refusal this port has that Go does not — an agent whose tools cannot
    // be hosted here. The caller decides what to do with it; both callers
    // record it rather than dropping it.
    let (options, tool_servers) =
        tokio::time::timeout_at(deadline, runner::build_options(spec, None))
            .await
            .map_err(|_| DEADLINE_EXCEEDED.to_string())?
            .map_err(|e| format!("agent setup: {e}"))?;

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

    let collected = tokio::time::timeout_at(deadline, collect_run_result(&mut stream))
        .await
        .unwrap_or_else(|_| Err(DEADLINE_EXCEEDED.to_string()));

    stream.close();
    // The in-process tool listeners outlive the subprocess and stop when
    // dropped, so this is after `close` rather than before it.
    drop(tool_servers);
    collected
}

/// A `RunSpec` for a headless run of `agent`, with no session pinned.
///
/// `buildRunOptions` sets neither session field, so the CLI generates its own id
/// and the caller stores it afterwards.
pub fn headless_spec(
    db_path: &std::path::Path,
    agent: crate::native::agents::Agent,
    working_dir: String,
    settings_profile_id: String,
) -> RunSpec {
    RunSpec {
        agent: Some(agent),
        // Never called: both callers synthesize an agent rather than passing
        // `None`, because Go's own resolvers return a non-nil config — and a
        // non-nil config with empty capabilities gets all twelve built-in
        // tools where a nil one gets none.
        no_agent_model: Box::new(String::new),
        settings: Arc::new(runner::TurnSettings::from_db(db_path)),
        working_dir,
        settings_profile_id,
        resume_session_id: None,
        custom_session_id: String::new(),
    }
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
) -> Result<RunResult, String> {
    let mut result: Option<RunResult> = None;
    let mut result_err: Option<String> = None;

    while let Some(event) = stream.next_event().await {
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
            });
        }
    }

    if let Some(err) = result_err {
        return Err(err);
    }
    result.ok_or_else(|| "agent finished without returning a result".to_string())
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
}
