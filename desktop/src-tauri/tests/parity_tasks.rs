//! Live parity for the scheduled-task and job-history reads: diff all five
//! ported endpoints against a *running* Go server, byte for byte.
//!
//! The unit tests prove the port matches Go over a fixture. This proves it
//! matches over the instance's real rows, which is the check the porting plan
//! calls for before a route is trusted.
//!
//! Ignored by default: it needs a real Agento instance and its database, and CI
//! has neither.
//!
//! ```sh
//! cd desktop && eval "$(./scripts/parity-instance.sh start)"
//! (cd src-tauri && cargo test --test parity_tasks -- --ignored --nocapture)
//! ./scripts/parity-instance.sh stop
//! ```
//!
//! **Never point this at the instance on :8990.** That is whatever binary the
//! developer installed, which drifts behind the repo — a stale baseline makes a
//! wrong port look verified, and a right one look broken.
//!
//! **Both tables are empty on a machine that has never scheduled anything**, and
//! two empty lists diff clean while proving nothing. Seed the scratch instance —
//! it is a copy — before trusting a pass:
//!
//! - tasks come from `POST /api/tasks`; create one per `schedule_type` so every
//!   `ScheduleConfig` field is exercised, and one left at its defaults so the
//!   `{}` config and the three absent timestamps are;
//! - **job history has no POST endpoint at all.** Rows are written only by the
//!   scheduler on a real agent run, so insert them straight into the scratch
//!   database — one still `running` (absent `finished_at`), one `success` with
//!   tokens and a `response_text`, one `failed` with an `error_message`.
//!
//! **Read-only.** These issue GETs and nothing else.

mod parity_common;

use parity_common::*;

use agento_lib::native::{gojson, tasks};

/// Every task, every job history entry, and the per-task and per-id reads for
/// each — driven from the lists rather than from hardcoded ids, so the cases
/// only cover what the instance actually has.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn task_and_job_history_reads_match_the_live_go_responses() {
    let db_path = live_db();

    // ── The two collections ───────────────────────────────────────────────
    let go = fetch("/api/tasks").await;
    let native =
        gojson::to_vec(&tasks::list_tasks(&db_path).expect("native tasks")).expect("encode");
    assert_identical("tasks", &go, &native);

    let task_ids = ids_of(&go);
    assert!(
        !task_ids.is_empty(),
        "no tasks on the parity instance — an empty list diffs clean and proves \
         nothing. Seed it through POST /api/tasks first (see this file's header)."
    );

    let go_history = fetch("/api/job-history").await;
    let native_history =
        gojson::to_vec(&tasks::list_all_job_history(&db_path, 50, 0).expect("native history"))
            .expect("encode");
    assert_identical("job-history", &go_history, &native_history);

    let job_ids = ids_of(&go_history);
    assert!(
        !job_ids.is_empty(),
        "no job history on the parity instance — insert rows directly, there is \
         no POST endpoint for them (see this file's header)."
    );

    // ── Per-id reads ──────────────────────────────────────────────────────
    for id in &task_ids {
        let go = fetch(&format!("/api/tasks/{id}")).await;
        let task = tasks::get_task(&db_path, id)
            .expect("native get_task")
            .expect("task");
        assert_identical(
            &format!("task {id}"),
            &go,
            &gojson::to_vec(&task).expect("encode"),
        );
    }

    for id in &job_ids {
        let go = fetch(&format!("/api/job-history/{id}")).await;
        let job = tasks::get_job_history(&db_path, id)
            .expect("native get_job_history")
            .expect("job history");
        assert_identical(
            &format!("job history {id}"),
            &go,
            &gojson::to_vec(&job).expect("encode"),
        );
    }

    // ── The nested list, plus the paging and clamping rules ───────────────
    //
    // The query cases are the part a fixture cannot check: `limit=0` and
    // `limit=abc` both mean fifty, and every value is clamped at 500 — including
    // `offset`, which shares the same parser.
    for id in &task_ids {
        for query in ["", "?limit=1", "?limit=0", "?limit=abc", "?limit=9999"] {
            let go = fetch(&format!("/api/tasks/{id}/job-history{query}")).await;
            let native = gojson::to_vec(
                &tasks::list_task_job_history(&db_path, id, expected_limit(query))
                    .expect("native task history"),
            )
            .expect("encode");
            assert_identical(&format!("task {id} job-history{query}"), &go, &native);
        }
    }

    for (query, limit, offset) in [
        ("", 50, 0),
        ("?limit=1", 1, 0),
        ("?limit=1&offset=1", 1, 1),
        ("?limit=2&offset=99", 2, 99),
        ("?limit=0", 50, 0),
        ("?limit=-3", 50, 0),
        ("?offset=-1", 50, 0),
        // `offset` shares `parseQueryInt`, so it is clamped at 500 too — but a
        // corpus this size cannot *prove* that, since 500 and 9999 both land
        // past the last row. The unit tests pin the rule from the Go source;
        // this case only shows the two agree on the result.
        ("?limit=9999&offset=9999", 500, 500),
    ] {
        let go = fetch(&format!("/api/job-history{query}")).await;
        let native = gojson::to_vec(
            &tasks::list_all_job_history(&db_path, limit, offset).expect("native history page"),
        )
        .expect("encode");
        assert_identical(&format!("job-history{query}"), &go, &native);
    }

    // An unknown task is an empty list with a 200, not the 404 `/api/tasks/{id}`
    // gives — Go never checks the task exists before listing its runs.
    let go = fetch("/api/tasks/does-not-exist/job-history").await;
    let native = gojson::to_vec(
        &tasks::list_task_job_history(&db_path, "does-not-exist", 50).expect("native"),
    )
    .expect("encode");
    assert_identical("unknown task job-history", &go, &native);

    println!(
        "{} tasks, {} job history entries",
        task_ids.len(),
        job_ids.len()
    );
}

fn ids_of(body: &[u8]) -> Vec<String> {
    let listed: serde_json::Value = serde_json::from_slice(body).expect("json");
    listed
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The limit Go lands on for each probed query string, spelled out rather than
/// computed, so this asserts the intended rule instead of re-running the port's
/// own arithmetic against itself.
fn expected_limit(query: &str) -> i64 {
    match query {
        "?limit=1" => 1,
        "?limit=9999" => 500,
        // "", "?limit=0" and "?limit=abc" all mean the default page.
        _ => 50,
    }
}
