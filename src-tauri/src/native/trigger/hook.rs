//! `POST /api/hooks/{task_id}` (#696): a task's local webhook. A call starts
//! one fresh event run of that task, `triggered_by = webhook`, with the body
//! as the event's payload.
//!
//! Rules, each enforced here or named where it is:
//!
//! - **The guard authenticates, not this handler.** Only a `hook` token whose
//!   `task_id` claim is the path's id is served here, and no other token is
//!   (`security::required_scope`, `security::verify_with`, #695). `Host` and
//!   `Content-Type` are the guard's too. So by the time [`serve`] runs, the
//!   caller may start this task and no other.
//! - **The body must be JSON, and is stored as sent.** It is parsed only to
//!   refuse a body that is not JSON (400); the payload is the raw text, which
//!   the executor masks and caps at `MAX_EVENT_PAYLOAD_BYTES` before anything
//!   keeps it. Nothing in it chooses the task, the mode or a destination.
//! - **It answers on the decision, not on the run.** A run lasts up to 240
//!   minutes and a queued event can wait hours for its slot, so the handler
//!   asks the task's limiter for its decision ([`executor::place_event`]),
//!   answers, and leaves the waiting and the run to a spawned task — as
//!   `POST /api/tasks/{id}/run` does. `202` carries the `job_history` id the
//!   run will write.
//! - **Every refusal has its own status**, and none starts a run or writes a
//!   row: no task `404`, not active `409` (an event never overrides pause),
//!   rate-limited `429`, queue full `503` (those two are counted on the task
//!   by the executor), anything else `500`.
//! - **The inbound transports' shared bound applies**: the run takes
//!   [`super::dispatcher::semaphore`] after its slot, as Slack's and
//!   Telegram's linked runs do, so a burst of hooks cannot hold more than its
//!   share of the scheduler.

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use serde::Serialize;

use crate::native::schedule::executor::{self, EventInput, EventRefused};
use crate::native::schedule::runtime::{self, Scheduler};
use crate::native::tasks::TriggeredBy;
use crate::native::writes::WriteError;
use crate::native::{self, gojson, Answer, BoxFuture, StreamEndpoint, StreamRequest};

/// The one route, recorded in `parity/desktop_routes.json`.
pub const ROUTES: &[(&str, &str)] = &[("POST", "/api/hooks/{task_id}")];

/// This route's entry in `native::STREAM_ENDPOINTS`. The async registry,
/// because the decision is an `await` and the run is spawned from it.
pub const STREAM_ENDPOINT: StreamEndpoint = StreamEndpoint {
    name: "task-hook",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    method == Method::POST && task_id(path).is_some()
}

/// The `{task_id}` of `/api/hooks/{task_id}`: one non-empty segment, as chi
/// routes it — a trailing slash or a nested path is not this route.
fn task_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/hooks/")?;
    (!rest.is_empty() && !rest.contains('/')).then_some(rest)
}

fn serve(
    req: StreamRequest,
) -> BoxFuture<'static, Result<axum::http::Response<axum::body::Body>, String>> {
    Box::pin(async move {
        let Some(id) = task_id(&req.path).map(str::to_string) else {
            return Err(format!(
                "{} {} is claimed but has no task id",
                req.method, req.path
            ));
        };
        Ok(native::response(
            answer(runtime::running(), &id, &req.body).await?,
        ))
    })
}

/// The `202` body: the run's job id and its task, as **Run now** answers.
#[derive(Serialize)]
struct HookRunStarted<'a> {
    job_id: &'a str,
    task_id: &'a str,
}

/// The whole route below the path match, against a scheduler the caller
/// supplies, so the tests need neither an HTTP server nor the process-wide one.
async fn answer(
    scheduler: Option<Arc<Scheduler>>,
    task_id: &str,
    body: &[u8],
) -> Result<Answer, String> {
    if serde_json::from_slice::<serde_json::Value>(body).is_err() {
        return Answer::error(StatusCode::BAD_REQUEST, &WriteError::InvalidBody.message());
    }
    let Some(scheduler) = scheduler else {
        // No scheduler means no database, which a shipped build never reaches.
        return Answer::error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the task scheduler is not running",
        );
    };

    let placed = match executor::place_event(&scheduler, task_id).await {
        Ok(placed) => placed,
        Err(refused) => return refusal(task_id, refused),
    };

    let job_id = uuid::Uuid::new_v4().to_string();
    // Built before anything is spawned, so nothing fallible sits after the
    // effect. A failure here drops `placed`, which gives its place back.
    let started = gojson::to_vec(&HookRunStarted {
        job_id: &job_id,
        task_id,
    })
    .map_err(|e| format!("encoding hook run: {e}"))?;
    let event = EventInput::new(
        TriggeredBy::Webhook,
        String::from_utf8_lossy(body).into_owned(),
        None,
        job_id.clone(),
    )
    .ok_or("a webhook is not an event source")?;

    log::info!(
        "task hook accepted task_id={task_id:?} job_id={job_id:?} queued={}",
        placed.is_queued()
    );
    let id = task_id.to_string();
    tokio::spawn(async move {
        let admitted = match placed.admitted(&scheduler).await {
            Ok(admitted) => admitted,
            Err(refused) => {
                log::warn!("a task hook's run did not start task_id={id:?} reason={refused:?}");
                return;
            }
        };
        let Ok(_permit) = super::dispatcher::semaphore().acquire().await else {
            log::warn!("dispatcher stopped, dropping a task hook's run task_id={id:?}");
            return;
        };
        if let Err(refused) = executor::run_admitted(scheduler, admitted, event).await {
            log::warn!("a task hook's run did not start task_id={id:?} reason={refused:?}");
        }
    });
    Ok(Answer::json_status(StatusCode::ACCEPTED, started))
}

/// The status and sentence of each refusal. None of them started a run; the
/// two the limiter answers were counted on the task by the executor.
fn refusal(task_id: &str, refused: EventRefused) -> Result<Answer, String> {
    let (status, message) = refusal_status(task_id, refused);
    Answer::error(status, &message)
}

fn refusal_status(task_id: &str, refused: EventRefused) -> (StatusCode, String) {
    match refused {
        EventRefused::NoSuchTask => (
            StatusCode::NOT_FOUND,
            WriteError::NotFound {
                resource: "task".to_string(),
                id: task_id.to_string(),
            }
            .message(),
        ),
        EventRefused::Paused => (
            StatusCode::CONFLICT,
            "the task is not active; resume it to accept hook calls".to_string(),
        ),
        EventRefused::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            "the task has started its hourly limit of event runs".to_string(),
        ),
        EventRefused::Dropped => (
            StatusCode::SERVICE_UNAVAILABLE,
            "the task's event queue is full".to_string(),
        ),
        // `NoSuchRun` is a continuation's refusal, which a hook never asks for.
        EventRefused::Unavailable | EventRefused::NoSuchRun => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "the task could not be started".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::native::schedule::delivery::fake_received;
    use crate::native::schedule::executor::tests::{settled_deliveries, with_active_task};
    use crate::native::tasks;

    #[test]
    fn only_a_post_to_one_task_segment_is_claimed() {
        assert!(claims(&Method::POST, "/api/hooks/t1"));
        assert_eq!(task_id("/api/hooks/t1"), Some("t1"));
        for method in [Method::GET, Method::PUT, Method::DELETE, Method::PATCH] {
            assert!(!claims(&method, "/api/hooks/t1"), "{method}");
        }
        for path in [
            "/api/hooks/",
            "/api/hooks",
            "/api/hooks/t1/",
            "/api/hooks/t1/run",
            "/hooks/t1",
            "/api/hooksx/t1",
        ] {
            assert!(!claims(&Method::POST, path), "{path}");
        }
    }

    /// One status per refusal, and the two the limiter answers are distinct
    /// from each other and from every other.
    #[test]
    fn every_refusal_has_its_own_status() {
        let cases = [
            (EventRefused::NoSuchTask, StatusCode::NOT_FOUND),
            (EventRefused::Paused, StatusCode::CONFLICT),
            (EventRefused::RateLimited, StatusCode::TOO_MANY_REQUESTS),
            (EventRefused::Dropped, StatusCode::SERVICE_UNAVAILABLE),
            (EventRefused::Unavailable, StatusCode::INTERNAL_SERVER_ERROR),
            (EventRefused::NoSuchRun, StatusCode::INTERNAL_SERVER_ERROR),
        ];
        for (refused, want) in cases {
            assert_eq!(refusal_status("t1", refused).0, want, "{refused:?}");
        }
        assert_eq!(
            refusal_status("t1", EventRefused::NoSuchTask).1,
            "task \"t1\" not found"
        );
    }

    fn scheduler_for(path: &std::path::Path) -> Arc<Scheduler> {
        runtime::detached(path)
    }

    fn set(path: &std::path::Path, sql: &str) {
        rusqlite::Connection::open(path)
            .expect("open")
            .execute(sql, [])
            .expect("update");
    }

    fn job_id_of(answer: &Answer) -> String {
        let doc: serde_json::Value =
            serde_json::from_slice(answer.body.as_deref().expect("a body")).expect("json body");
        doc["job_id"].as_str().expect("job_id").to_string()
    }

    fn rows(path: &std::path::Path) -> Vec<tasks::JobHistory> {
        tasks::list_task_job_history(path, "t1", 100).expect("list")
    }

    fn counters(path: &std::path::Path) -> (i64, i64) {
        let task = tasks::get_task(path, "t1").expect("read").expect("row");
        (task.dropped_event_count, task.rate_limited_event_count)
    }

    async fn until_rows(path: &std::path::Path, n: usize) {
        for _ in 0..400 {
            if rows(path).len() == n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("job_history never reached {n} rows: {}", rows(path).len());
    }

    /// Gate (a): a valid call answers `202` with the job id at once, and that
    /// run writes one `webhook` row carrying the masked payload and is
    /// delivered to the task's destination. The task's prompt cannot be
    /// interpolated, so the run ends in `prepare` without a subprocess — a
    /// failed run, which is still a run, a row and a delivery.
    #[tokio::test]
    async fn a_valid_call_starts_one_webhook_run_and_delivers_it() {
        let file = with_active_task("report for {{quarter}}");
        let scheduler = scheduler_for(file.path());
        let key = ["AK", "IA", "Q7ZX3MPLR2VN6TWB"].concat();
        let body = format!("{{\"note\":\"my key is {key}\"}}");

        let answered = answer(Some(Arc::clone(&scheduler)), "t1", body.as_bytes())
            .await
            .expect("answered");
        assert_eq!(answered.status, StatusCode::ACCEPTED);
        let job_id = job_id_of(&answered);
        assert_eq!(
            answered.body,
            Some(format!("{{\"job_id\":\"{job_id}\",\"task_id\":\"t1\"}}\n").into_bytes())
        );

        settled_deliveries(file.path(), &job_id, 1).await;
        let rows = rows(file.path());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, job_id);
        assert_eq!(rows[0].triggered_by, "webhook");
        assert!(rows[0].event_payload.starts_with("{\"note\":\"my key is "));
        assert!(
            !rows[0].event_payload.contains(&key),
            "the payload is masked"
        );
        assert_eq!(fake_received(&job_id).len(), 1, "delivered");
    }

    /// None of these starts a run, writes a row or moves a counter.
    #[tokio::test]
    async fn a_refused_call_answers_its_status_and_starts_nothing() {
        let file = with_active_task("p");
        let scheduler = scheduler_for(file.path());
        let call = |id: &'static str, body: &'static [u8]| {
            let scheduler = Arc::clone(&scheduler);
            async move {
                answer(Some(scheduler), id, body)
                    .await
                    .expect("answered")
                    .status
            }
        };

        assert_eq!(call("t1", b"").await, StatusCode::BAD_REQUEST);
        assert_eq!(call("t1", b"not json").await, StatusCode::BAD_REQUEST);
        assert_eq!(call("nope", b"{}").await, StatusCode::NOT_FOUND);
        set(file.path(), "UPDATE scheduled_tasks SET status = 'paused'");
        assert_eq!(call("t1", b"{}").await, StatusCode::CONFLICT);
        assert_eq!(
            answer(None, "t1", b"{}").await.expect("answered").status,
            StatusCode::INTERNAL_SERVER_ERROR
        );

        assert!(rows(file.path()).is_empty());
        assert_eq!(counters(file.path()), (0, 0));
        assert_eq!(scheduler.limiter().load("t1"), (0, 0));
    }

    /// Past the hour's cap a call answers `429`, is counted, and writes no row.
    #[tokio::test]
    async fn a_rate_limited_call_answers_429_and_is_counted() {
        let file = with_active_task("report for {{quarter}}");
        set(
            file.path(),
            "UPDATE scheduled_tasks SET max_runs_per_hour = 1",
        );
        let scheduler = scheduler_for(file.path());

        let first = answer(Some(Arc::clone(&scheduler)), "t1", b"{}")
            .await
            .expect("answered");
        assert_eq!(first.status, StatusCode::ACCEPTED);
        until_rows(file.path(), 1).await;

        let second = answer(Some(Arc::clone(&scheduler)), "t1", b"{}")
            .await
            .expect("answered");
        assert_eq!(second.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(counters(file.path()), (0, 1));
        assert_eq!(rows(file.path()).len(), 1, "the refusal wrote no row");
    }

    /// With the slot busy, a call is queued and still answered `202` at once;
    /// with the queue full too, the next answers `503` and is counted. The
    /// queued one runs when the slot frees.
    #[tokio::test]
    async fn a_queued_call_answers_at_once_and_a_dropped_one_answers_503() {
        let file = with_active_task("report for {{quarter}}");
        set(
            file.path(),
            "UPDATE scheduled_tasks SET max_queued_events = 1",
        );
        let scheduler = scheduler_for(file.path());
        let busy = executor::admit_event(&scheduler, "t1")
            .await
            .expect("the slot is held");

        let queued = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            answer(Some(Arc::clone(&scheduler)), "t1", b"{}"),
        )
        .await
        .expect("answered without waiting for the slot")
        .expect("answered");
        assert_eq!(queued.status, StatusCode::ACCEPTED);
        assert_eq!(scheduler.limiter().load("t1"), (1, 1));

        let dropped = answer(Some(Arc::clone(&scheduler)), "t1", b"{}")
            .await
            .expect("answered");
        assert_eq!(dropped.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(counters(file.path()), (1, 0));
        assert!(rows(file.path()).is_empty(), "nothing has run yet");

        drop(busy);
        until_rows(file.path(), 1).await;
        assert_eq!(rows(file.path())[0].id, job_id_of(&queued));
    }
}
