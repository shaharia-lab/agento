//! The scheduler *runtime*: what `gocron.Scheduler` does around the schedule
//! computation in this module's parent. Mirrors `internal/scheduler/scheduler.go`.
//!
//! # This is an ownership flip, not a route
//!
//! **Only one process may schedule.** Two schedulers over one `scheduled_tasks`
//! table means every task fires twice, so this cannot be a claimed route that
//! forwards on doubt — the sidecar is started with `AGENTO_SCHEDULER=off` and
//! nothing else will fire a task. That is the #289 model, and it is why the
//! failure mode to design against is *silence*: a task Rust declines to run does
//! not fall back to Go, it simply never runs, and a job history with no row is
//! indistinguishable from a task that was not due.
//!
//! Everything here therefore fails **loudly** — see [`super::executor`], where
//! an agent this build cannot run records a failed `job_history` row and
//! publishes the failed event rather than returning early.
//!
//! # One deliberate divergence: the sleep is chunked
//!
//! gocron parks a timer for the whole interval. This wakes at most every
//! [`WAKE_INTERVAL`] and re-reads the **wall clock**, because the process runs
//! on a laptop: `tokio::time::sleep` is measured on `Instant`, which does not
//! advance while the machine is suspended, so a single long park would fire late
//! by however long the lid was shut. Re-checking against `Utc::now()` also
//! absorbs a manual clock change. A task due while the machine was asleep fires
//! on the first wake after it, which is what a person expects and what a
//! monotonic park does not do.
//!
//! # What has no analogue here
//!
//! Go's `scheduleTaskOnStartup` exists to `recover()` from a panic in
//! `robfig/cron`'s parser: an expression of exactly `CRON_TZ=UTC` slices out of
//! range and, on the startup goroutine, takes `agento web` down on every boot
//! (issue #330). [`super::cron::parse`] returns a `Result` for that input rather
//! than panicking, so the recover has nothing to catch and is not reproduced —
//! the row is skipped with a warning either way. #330 is still a real bug in the
//! Go server, and validating at save time is still its fix.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Offset, Utc};
use chrono_tz::Tz;
use tokio::sync::Semaphore;
use tokio::task::AbortHandle;

use super::{advance_past_now, build_job_definition, next_runs, setup, JobSchedule};
use crate::native::tasks::ScheduledTask;

/// How long a job may park before it re-reads the wall clock. See the module
/// header — this is the whole of the suspend/clock-change divergence.
const WAKE_INTERVAL: StdDuration = StdDuration::from_secs(60);

/// `Config.MaxConcurrency`'s default. `initTaskScheduler` never sets one, so
/// three is not a default in the "unless configured" sense — it is the number.
const MAX_CONCURRENCY: usize = 3;

/// The one running scheduler, or `None` when this process does not schedule.
///
/// `None` is not an error state: it is what every task write checks before
/// calling [`Scheduler::schedule_task`], mirroring Go's `if s.scheduler != nil`
/// in `taskService`. A build with no database path never starts one.
static RUNNING: OnceLock<Arc<Scheduler>> = OnceLock::new();

/// `scheduler.Scheduler`.
pub struct Scheduler {
    db_path: PathBuf,
    /// `gocron.WithLocation`'s default, `time.Local`. Every job type but the
    /// one-off renders in it, and a daily job holds its wall clock across a DST
    /// transition *in it* — so this is part of the schedule rather than a
    /// formatting detail.
    loc: Tz,
    /// `s.jobs`: task id → the timer task driving it. Aborting the handle is
    /// `cron.RemoveJob`.
    jobs: Mutex<HashMap<String, AbortHandle>>,
    /// `s.semaphore`, acquired around the *execution* rather than around the
    /// timer, exactly as Go's `executeTask` does — a job whose turn is waiting
    /// still advances its own schedule.
    semaphore: Arc<Semaphore>,
}

impl Scheduler {
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// `ScheduleTask`: add or replace a task's schedule.
    ///
    /// The two failures are Go's two, and both leave the task **unscheduled**
    /// rather than half-scheduled: the definition would not build, or gocron
    /// would refuse it. Callers log and carry on, as `taskService` does.
    pub fn schedule_task(self: &Arc<Self>, task: &ScheduledTask) -> Result<(), String> {
        let now = Utc::now();
        // `buildJobDefinition` reads `time.Now()` in the scheduler's location;
        // only `run_immediately` uses it, and it uses the offset.
        let now_local = now.with_timezone(&self.loc);
        let def = build_job_definition(
            &task.schedule_type,
            &task.schedule_config,
            now_local.with_timezone(&now_local.offset().fix()),
        )
        .map_err(|e| format!("building job definition for {:?}: {}", task.id, e.class()))?;
        let sched = setup(&def, self.loc, now)
            .map_err(|e| format!("scheduling task {:?}: {}", task.id, e.class()))?;

        // Replace under the same lock the insert takes, so a concurrent
        // reschedule cannot leave two timers on one task.
        let mut jobs = self.lock_jobs();
        if let Some(previous) = jobs.remove(&task.id) {
            previous.abort();
        }
        let handle = self.spawn_job(task.id.clone(), sched, now);
        jobs.insert(task.id.clone(), handle);
        drop(jobs);

        log::info!(
            "task scheduled task_id={:?} task_name={:?} schedule_type={:?}",
            task.id,
            task.name,
            task.schedule_type
        );
        Ok(())
    }

    /// `UnscheduleTask`. Silent when the task has no timer, exactly as Go's is.
    pub fn unschedule_task(&self, task_id: &str) {
        if let Some(handle) = self.lock_jobs().remove(task_id) {
            handle.abort();
            log::info!("task unscheduled task_id={task_id:?}");
        }
    }

    /// The timer driving one task, and the only place a run is started.
    fn spawn_job(
        self: &Arc<Self>,
        task_id: String,
        sched: JobSchedule,
        now: DateTime<Utc>,
    ) -> AbortHandle {
        let me = Arc::clone(self);
        let handle = tokio::spawn(async move {
            // `selectNewJob`'s initial `nextRun`, advanced past now — the same
            // value `Job.NextRuns(1)` reports, so what the timer waits for and
            // what a caller could ask for cannot disagree.
            let Some(first) = next_runs(&sched, me.loc, now, 1)
                .into_iter()
                .flatten()
                .next()
            else {
                // A schedule with no future run at all. gocron installs the job
                // and it never fires; so does this.
                return;
            };

            let mut next = first;
            loop {
                sleep_until(next.instant).await;

                // Spawned rather than awaited: gocron hands the job to its
                // worker pool and the schedule advances immediately, so a run
                // that outlives its own interval does not shift the next fire.
                // The semaphore is what bounds the overlap.
                let runner = Arc::clone(&me);
                let id = task_id.clone();
                tokio::spawn(async move {
                    let permit = Arc::clone(&runner.semaphore).acquire_owned().await;
                    if permit.is_err() {
                        return;
                    }
                    super::executor::execute_task(&runner, &id).await;
                });

                // **Re-anchor against the wall clock**, do not just add one
                // interval. gocron's `selectExecJobsOutForRescheduling` computes
                // `next` and then, `if next.Before(s.now())`, calls
                // `advancePastNow` — its own comment says "the machine went to
                // sleep, and woke up some time later".
                //
                // Advancing by one interval and firing whenever the result is
                // past would *replay every missed fire*: a five-minute task and
                // a shut lid for eight hours is ~96 back-to-back Claude runs,
                // 96 chat sessions, 96 job_history rows and a
                // `stop_after_count` budget gone in seconds. An NTP jump
                // forward does the same. One fire on the first wake is both
                // what gocron does and what a person expects.
                //
                // `None` is the schedule exhausted or no longer making
                // progress — gocron removes the job, and so does returning.
                let Some(advanced) = advance_past_now(&sched, sched.next(next), Utc::now()) else {
                    return;
                };
                next = advanced;
            }
        });
        handle.abort_handle()
    }

    /// Poisoning cannot happen — nothing panics while this lock is held — but a
    /// scheduler that stopped scheduling because of one would be silent, which
    /// is the failure mode this module exists to avoid.
    fn lock_jobs(&self) -> std::sync::MutexGuard<'_, HashMap<String, AbortHandle>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// How long to park next, or `None` when `target` has arrived.
///
/// The whole of the chunking rule, as a pure function of the two clocks — which
/// is what makes it testable. [`sleep_until`] cannot be: it reads `Utc::now()`,
/// and a paused tokio clock does not move that, so a test of the loop would
/// wait the real duration rather than a simulated one.
fn next_chunk(target: DateTime<Utc>, now: DateTime<Utc>) -> Option<StdDuration> {
    // `Err` is a negative span: already due.
    let remaining = (target - now).to_std().ok()?;
    if remaining.is_zero() {
        return None;
    }
    Some(remaining.min(WAKE_INTERVAL))
}

/// Park until `target`, re-reading the wall clock at most [`WAKE_INTERVAL`]
/// apart. A target already in the past returns immediately, which is how a job
/// whose run overran its interval fires again straight away.
async fn sleep_until(target: DateTime<Utc>) {
    while let Some(chunk) = next_chunk(target, Utc::now()) {
        tokio::time::sleep(chunk).await;
    }
}

/// `time.Local` as a `chrono_tz::Tz`.
///
/// UTC when the machine's zone cannot be read or is not an IANA name the
/// database knows — the same fallback `analytics::params` and the `current_time`
/// tool take, and the only one available: a fixed offset cannot answer "what is
/// 09:00 tomorrow" across a DST transition, which is the whole reason a daily
/// job is not a 24-hour duration job.
fn local_tz() -> Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|name| name.parse::<Tz>().ok())
        .unwrap_or(Tz::UTC)
}

/// Whether **this build** owns the scheduler, which is true only when the seam
/// is fully on.
///
/// The scheduler cannot be owned independently of the task writes, because each
/// write also registers or unregisters a timer and the registration has to
/// happen in the process that stored the row. `may_serve` forwards every
/// non-`GET` in both [`Mode::Off`] and [`Mode::Diff`], so in those modes Go is
/// the only writer — and a Go that had been told `AGENTO_SCHEDULER=off` would
/// store a task neither process ever schedules until the app restarts. That is
/// exactly the split this port set out to close, reappearing through the
/// escape hatch.
///
/// So ownership follows the seam, in **both** directions: this decides whether
/// `sidecar.rs` sets `AGENTO_SCHEDULER=off` *and* whether [`start`] installs any
/// timers. `AGENTO_DESKTOP_NATIVE=off` therefore means what it is documented to
/// mean — the app behaves exactly as it did before the port, with Go scheduling
/// and Go writing.
pub fn shell_owns_scheduler() -> bool {
    crate::native::mode() == crate::native::Mode::On
}

/// `initTaskScheduler` + `Scheduler.Start`: load the active tasks, schedule
/// each, and run.
///
/// Called once, from the app's setup. Idempotent by way of the `OnceLock`: a
/// second call is a no-op rather than a second scheduler.
pub fn start(db_path: PathBuf) {
    if !shell_owns_scheduler() {
        // The sidecar was not told to stop scheduling either, so Go owns it and
        // installing timers here would be the two-scheduler hazard.
        log::info!("task scheduler left with the sidecar: the native seam is not fully on");
        return;
    }

    let scheduler = Arc::new(Scheduler {
        db_path,
        loc: local_tz(),
        jobs: Mutex::new(HashMap::new()),
        semaphore: Arc::new(Semaphore::new(MAX_CONCURRENCY)),
    });
    if RUNNING.set(Arc::clone(&scheduler)).is_err() {
        log::warn!("task scheduler already started; ignoring a second start");
        return;
    }

    let tasks = match crate::native::tasks::list_tasks(scheduler.db_path()) {
        Ok(tasks) => tasks,
        Err(e) => {
            // Go returns this error from `Start` and `initTaskScheduler` logs
            // it as a warning, leaving the scheduler running with no jobs.
            log::warn!("failed to start task scheduler: loading tasks: {e}");
            return;
        }
    };

    let mut scheduled = 0usize;
    for task in &tasks {
        if task.status != "active" {
            continue;
        }
        match scheduler.schedule_task(task) {
            Ok(()) => scheduled += 1,
            Err(e) => log::warn!(
                "failed to schedule task on startup task_id={:?} task_name={:?} error={e}",
                task.id,
                task.name
            ),
        }
    }
    log::info!("task scheduler started active_tasks={scheduled}");
}

/// The running scheduler, or `None` when this process does not schedule.
pub fn running() -> Option<Arc<Scheduler>> {
    RUNNING.get().cloned()
}

/// `if s.scheduler != nil { s.scheduler.ScheduleTask(task) }` — the shape every
/// task write uses, including the "log the failure and carry on" part.
///
/// `reason` names the caller, so the log line says which write left a task
/// stored but not firing.
pub fn schedule_if_running(task: &ScheduledTask, reason: &str) {
    let Some(scheduler) = running() else {
        return;
    };
    if let Err(e) = scheduler.schedule_task(task) {
        log::warn!(
            "failed to schedule {reason} task task_id={:?} error={e}",
            task.id
        );
    }
}

/// `if s.scheduler != nil { s.scheduler.UnscheduleTask(id) }`.
pub fn unschedule_if_running(task_id: &str) {
    if let Some(scheduler) = running() {
        scheduler.unschedule_task(task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::super::Fire;
    use super::*;

    #[test]
    fn the_local_zone_resolves_or_falls_back_to_utc() {
        // Not an assertion about *which* zone — that is the machine's — only
        // that the resolution never panics and always answers something the
        // calendar arithmetic can use.
        let tz = local_tz();
        let _ = Utc::now().with_timezone(&tz);
    }

    #[test]
    fn a_target_that_has_arrived_asks_for_no_park() {
        let now = Utc::now();
        assert_eq!(next_chunk(now - chrono::Duration::seconds(30), now), None);
        assert_eq!(next_chunk(now, now), None, "exactly due is due");
    }

    #[test]
    fn a_long_wait_is_capped_at_the_wake_interval_and_a_short_one_is_not() {
        let now = Utc::now();
        assert_eq!(
            next_chunk(now + chrono::Duration::hours(9), now),
            Some(WAKE_INTERVAL),
            "a nine-hour wait parks in wake-interval chunks, so a suspend is \
             noticed within one of them"
        );
        assert_eq!(
            next_chunk(now + chrono::Duration::seconds(5), now),
            Some(StdDuration::from_secs(5)),
            "a wait shorter than the interval is not rounded up to it — that \
             would fire every task up to a minute late"
        );
        assert_eq!(
            next_chunk(now + chrono::Duration::seconds(60), now),
            Some(WAKE_INTERVAL),
            "exactly the interval is not over it"
        );
    }

    /// The whole of finding #1 on PR #365, as a property of the advance rule.
    ///
    /// Models the timer loop exactly: a fire whose target is not in the future
    /// costs no sleep, so the question is **how many times the loop comes round
    /// before it genuinely parks**. With the advance re-anchored that is once;
    /// stepping one interval at a time it is once per window that elapsed —
    /// ~68 Claude runs here, and a `stop_after_count` budget gone in seconds.
    ///
    /// Seven minutes rather than five, deliberately: eight hours is not a whole
    /// number of them, so the catch-up lands strictly *inside* a window instead
    /// of on its edge. The edge is its own case below.
    #[test]
    fn a_missed_window_yields_one_fire_rather_than_one_per_missed_interval() {
        let sched = JobSchedule::Duration {
            every: chrono::Duration::minutes(7),
            loc: Tz::UTC,
        };
        let scheduled_at = Utc::now();
        let woke = scheduled_at + chrono::Duration::hours(8);

        let mut next = Fire {
            instant: scheduled_at + chrono::Duration::minutes(7),
            offset: chrono::FixedOffset::east_opt(0).expect("UTC"),
        };
        let mut fires = 0;
        while next.instant <= woke {
            // `sleep_until` returns without awaiting, so this is a real fire.
            assert_eq!(
                next_chunk(next.instant, woke),
                None,
                "would not have parked"
            );
            fires += 1;
            assert!(fires <= 5, "the loop is replaying missed fires");
            next = advance_past_now(&sched, sched.next(next), woke).expect("a future fire");
        }

        assert_eq!(fires, 1, "one catch-up fire, not ~68");
        assert!(
            next.instant > woke && next.instant <= woke + chrono::Duration::minutes(7),
            "and the schedule is re-anchored to the next window after the wake"
        );
    }

    /// A fire due at exactly the current instant is **due**, not skipped.
    ///
    /// `advance_past_now` walks `while next < now`, so it stops *on* `now` — and
    /// gocron's own loop is `for next.Before(s.now())`, the same strictness. The
    /// distinction only shows when the sleep is a whole number of intervals, and
    /// getting it backwards would silently drop one run of every task whose
    /// interval happens to divide the gap.
    #[test]
    fn a_fire_landing_exactly_on_now_is_due_rather_than_skipped() {
        let sched = JobSchedule::Duration {
            every: chrono::Duration::minutes(5),
            loc: Tz::UTC,
        };
        let scheduled_at = Utc::now();
        // 8h is exactly 96 five-minute windows, so the advance lands on it.
        let woke = scheduled_at + chrono::Duration::hours(8);
        let stale = Fire {
            instant: scheduled_at + chrono::Duration::minutes(5),
            offset: chrono::FixedOffset::east_opt(0).expect("UTC"),
        };

        let caught_up = advance_past_now(&sched, sched.next(stale), woke).expect("a fire");
        assert_eq!(caught_up.instant, woke, "stopped on now, not past it");
    }

    #[test]
    fn an_exhausted_schedule_removes_the_job_rather_than_spinning() {
        // A one-time job that has already run: `next` is the zero time, so the
        // advance answers `None` and the loop returns.
        let past = Utc::now() - chrono::Duration::hours(1);
        let sched = JobSchedule::OneTime(vec![past.fixed_offset()]);
        let fired = Fire {
            instant: past,
            offset: chrono::FixedOffset::east_opt(0).expect("UTC"),
        };
        assert!(advance_past_now(&sched, sched.next(fired), Utc::now()).is_none());
    }

    #[test]
    fn the_chunks_converge_rather_than_looping_forever() {
        // The property the loop depends on: repeatedly subtracting the chunk it
        // asks for reaches the target in finitely many steps, with the *last*
        // step landing exactly on it rather than past it.
        let now = Utc::now();
        let target = now + chrono::Duration::seconds(150);
        let mut clock = now;
        let mut steps = 0;
        while let Some(chunk) = next_chunk(target, clock) {
            clock += chrono::Duration::from_std(chunk).expect("chunk is small");
            steps += 1;
            assert!(steps < 10, "did not converge");
        }
        assert_eq!(steps, 3, "60 + 60 + 30");
        assert_eq!(clock, target);
    }
}
