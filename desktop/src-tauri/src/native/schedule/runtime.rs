//! The scheduler *runtime*: what `gocron.Scheduler` does around the schedule
//! computation in this module's parent. Mirrors `internal/scheduler/scheduler.go`.
//!
//! # This is an ownership flip, not a route
//!
//! **Only one process may schedule.** Two schedulers over one `scheduled_tasks`
//! table means every task fires twice, so this could never be a claimed route
//! that forwards on doubt — the sidecar was started with `AGENTO_SCHEDULER=off`
//! (until #278 removed it entirely), and nothing else will fire a task. That is
//! the #289 model, and it is why the
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

/// How often the timers are checked against the stored rows. See
/// [`Scheduler::reconcile`] for why a sweep exists at all; the interval is a
/// compromise between "a fallen-back write should start firing soon" and "this
/// is a `SELECT` on a laptop".
const RECONCILE_INTERVAL: StdDuration = StdDuration::from_secs(60);

/// `Config.MaxConcurrency`'s default. `initTaskScheduler` never sets one, so
/// three is not a default in the "unless configured" sense — it is the number.
const MAX_CONCURRENCY: usize = 3;

/// The one running scheduler, or `None` when this process does not schedule.
///
/// `None` is not an error state: it is what every task write checks before
/// calling [`Scheduler::schedule_task`], mirroring Go's `if s.scheduler != nil`
/// in `taskService`. A build with no database path never starts one.
static RUNNING: OnceLock<Arc<Scheduler>> = OnceLock::new();

/// One installed timer, and enough to tell whether it is still the right one.
struct Job {
    handle: AbortHandle,
    /// What the timer was built from. **Only the fields that decide when a task
    /// fires** — not `updated_at`, which every run bumps via
    /// `updateTaskAfterRun`: fingerprinting on that would make the reconcile
    /// replace a live timer after each run, and replacing a `DurationJob`'s
    /// timer restarts its interval from now.
    fingerprint: String,
}

/// What a task's timer is built from, for [`Scheduler::reconcile`].
fn fingerprint(task: &ScheduledTask) -> String {
    let config = crate::native::tasks::marshal_schedule_config(&task.schedule_config)
        .unwrap_or_else(|e| format!("<unencodable: {e}>"));
    format!("{}|{}", task.schedule_type, config)
}

/// `scheduler.Scheduler`.
pub struct Scheduler {
    db_path: PathBuf,
    /// `gocron.WithLocation`'s default, `time.Local`. Every job type but the
    /// one-off renders in it, and a daily job holds its wall clock across a DST
    /// transition *in it* — so this is part of the schedule rather than a
    /// formatting detail.
    loc: Tz,
    /// `s.jobs`: task id → the timer driving it. Aborting the handle is
    /// `cron.RemoveJob`.
    jobs: Mutex<HashMap<String, Job>>,
    /// What [`Scheduler::reconcile`] has already acted on: task id → the
    /// fingerprint it acted on.
    ///
    /// Separate from `jobs` because it must **outlive the timer**. A sweep that
    /// looked only at `jobs` would retry a task it cannot schedule on every
    /// pass — an active `one_off` whose `run_at` is in the past because the
    /// machine was off at the appointed time fails forever, at ~1440 warning
    /// lines a day into a 5 MiB log the user is asked to attach to bug reports,
    /// where Go logged once at startup.
    ///
    /// It is **cleared by `unschedule_task`**, so a task that genuinely returns
    /// to service is picked up again — which is why it is not what bounds the
    /// failed-auto-pause loop. That is bounded at the source instead:
    /// `update_task_after_run` drops the timer only when the write that paused
    /// the row actually succeeded.
    swept: Mutex<HashMap<String, String>>,
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
            previous.handle.abort();
        }
        let handle = self.spawn_job(task.id.clone(), sched, now);
        jobs.insert(
            task.id.clone(),
            Job {
                handle,
                fingerprint: fingerprint(task),
            },
        );
        drop(jobs);

        self.swept
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(task.id.clone(), fingerprint(task));

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
        // Forgotten here as well as dropped: a task that is unscheduled and
        // later becomes active again must be reachable by the sweep.
        self.swept
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(task_id);
        if let Some(job) = self.lock_jobs().remove(task_id) {
            job.handle.abort();
            log::info!("task unscheduled task_id={task_id:?}");
        }
    }

    /// Whether the sweep already knows about this task at its current
    /// schedule — i.e. whether [`Self::reconcile`] would leave it alone.
    pub fn knows_task(&self, task_id: &str) -> bool {
        self.swept
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(task_id)
    }

    /// Bring the installed timers back in line with the stored rows.
    ///
    /// **This exists because a native task write can fall back.** Every one of
    /// them returns `WriteError::Fallback` on an unopenable database, a schema
    /// newer than this build, a `SQLITE_BUSY` past the five-second timeout, or a
    /// failed begin/commit — and the seam then forwards to a sidecar started
    /// `AGENTO_SCHEDULER=off`, whose `taskService` applies the row change and
    /// skips the registration. A created task would never fire, a resumed one
    /// would have no timer, an edited one would keep its old schedule, and all
    /// three would stay that way until the app restarted. That is the exact
    /// split this port set out to close, on the error path.
    ///
    /// It is a sweep rather than a hook on the forward, so it also covers a row
    /// changed by anything else — and so the seam needs no knowledge of tasks.
    ///
    /// **Idempotent by fingerprint**, which is what makes running it on a timer
    /// safe: a task whose schedule has not changed keeps the timer it has.
    /// Rescheduling unconditionally would restart every `DurationJob`'s interval
    /// on every sweep, so a five-minute task swept every minute would never
    /// fire at all.
    pub fn reconcile(self: &Arc<Self>) {
        let tasks = match crate::native::tasks::list_tasks(self.db_path()) {
            Ok(tasks) => tasks,
            Err(e) => {
                log::warn!("task scheduler: reconcile could not read tasks: {e}");
                return;
            }
        };

        let mut active: HashMap<&str, &ScheduledTask> = HashMap::new();
        for task in &tasks {
            if task.status == "active" {
                active.insert(task.id.as_str(), task);
            }
        }

        // Timers whose task is gone, paused, or now on a different schedule.
        let stale: Vec<String> = {
            let jobs = self.lock_jobs();
            jobs.iter()
                .filter(|(id, job)| {
                    active
                        .get(id.as_str())
                        .is_none_or(|task| fingerprint(task) != job.fingerprint)
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in &stale {
            self.unschedule_task(id);
            log::info!("task scheduler: reconcile dropped a stale timer task_id={id:?}");
        }

        // Anything this sweep has not already acted on at this exact schedule —
        // which is what stops an unschedulable task being retried every minute.
        // `swept` is written whether the attempt succeeds or fails, and cleared
        // by `unschedule_task`, so a task genuinely returning to service is
        // still picked up.
        for (id, task) in &active {
            let print = fingerprint(task);
            let already = self
                .swept
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(*id)
                .is_some_and(|seen| *seen == print);
            if already {
                continue;
            }
            // Re-read before installing. The task list was read at the top of
            // the pass, and a `pause_task` or `delete_task` committing since
            // then has already called `unschedule_if_running` — which clears
            // `swept`, so the stale `active` snapshot would look like a task
            // needing a timer and the sweep would undo a write that already
            // landed. `execute_task` re-reads too and would return early, so
            // this only ever cost an orphan timer for a minute; but a sweep
            // that can undo a commit is the wrong shape to leave in place.
            match crate::native::tasks::get_task(self.db_path(), id) {
                Ok(Some(fresh)) if fresh.status == "active" => {}
                Ok(_) => continue,
                Err(e) => {
                    log::warn!("task scheduler: reconcile could not re-read task_id={id:?}: {e}");
                    continue;
                }
            }

            self.swept
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((*id).to_string(), print);

            match self.schedule_task(task) {
                Ok(()) => {
                    log::info!("task scheduler: reconcile installed a missing timer task_id={id:?}")
                }
                Err(e) => log::warn!(
                    "task scheduler: reconcile could not schedule task_id={id:?} error={e} \
                     (not retried until its schedule changes)"
                ),
            }
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
    fn lock_jobs(&self) -> std::sync::MutexGuard<'_, HashMap<String, Job>> {
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
/// While the sidecar existed this decided ownership in **both** directions:
/// whether `sidecar.rs` set `AGENTO_SCHEDULER=off` *and* whether [`start`]
/// installed any timers. #278 removed the sidecar and the seam modes with it,
/// so only the second direction remains.
pub fn shell_owns_scheduler() -> bool {
    // The database path is the whole answer since #278: with no database this
    // process cannot schedule at all. (The seam-mode half of this check died
    // with the sidecar — there is no other process to leave the scheduler to.)
    crate::paths::database_path().is_some()
}

/// `initTaskScheduler` + `Scheduler.Start`: load the active tasks, schedule
/// each, and run.
///
/// Called once, from the app's setup. Idempotent by way of the `OnceLock`: a
/// second call is a no-op rather than a second scheduler.
pub fn start(db_path: PathBuf) {
    if !shell_owns_scheduler() {
        log::warn!("task scheduler not started: there is no database to read tasks from");
        return;
    }

    let scheduler = Arc::new(Scheduler {
        db_path,
        loc: local_tz(),
        jobs: Mutex::new(HashMap::new()),
        swept: Mutex::new(HashMap::new()),
        semaphore: Arc::new(Semaphore::new(MAX_CONCURRENCY)),
    });
    if RUNNING.set(Arc::clone(&scheduler)).is_err() {
        log::warn!("task scheduler already started; ignoring a second start");
        return;
    }

    // **Armed before the first read, not after it.** A transient `SQLITE_BUSY`
    // at boot used to return from here with `RUNNING` already set and the
    // sidecar already told to stand down — a process with no timers, no sweep
    // and no way back until restart, behind one `log::warn!`. The mechanism
    // written to recover from missing timers has to survive the failure it is
    // most likely to be needed for.
    let sweeper = Arc::clone(&scheduler);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(RECONCILE_INTERVAL).await;
            // `reconcile` is synchronous SQLite, and `db::open_read_write` sets
            // a five-second `busy_timeout` — so on the runtime's own worker it
            // could park one for that long every minute while the scan or a Go
            // write holds the lock, stalling an SSE chat stream that shares the
            // runtime. `proxy.rs` puts every native handler on the blocking
            // pool for exactly this reason.
            let pass = Arc::clone(&sweeper);
            if tokio::task::spawn_blocking(move || pass.reconcile())
                .await
                .is_err()
            {
                log::warn!("task scheduler: a reconcile pass panicked");
            }
        }
    });

    let tasks = match crate::native::tasks::list_tasks(scheduler.db_path()) {
        Ok(tasks) => tasks,
        Err(e) => {
            // Go returns this error from `Start` and `initTaskScheduler` logs
            // it as a warning, leaving the scheduler running with no jobs. The
            // sweep above then heals it on its next pass, which Go had no
            // equivalent of.
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

/// A scheduler over `db_path` that is **not** installed as the process-wide one.
///
/// For tests that drive `Scheduler`'s own methods — the run write-back, the
/// reconcile, a whole scheduled run against a scripted CLI — without the
/// `OnceLock` that makes [`start`] single-shot. Not `#[cfg(test)]`, because
/// `tests/scheduled_run.rs` is a separate crate and could not see it.
pub fn detached(db_path: impl Into<PathBuf>) -> Arc<Scheduler> {
    Arc::new(Scheduler {
        db_path: db_path.into(),
        loc: local_tz(),
        jobs: Mutex::new(HashMap::new()),
        swept: Mutex::new(HashMap::new()),
        semaphore: Arc::new(Semaphore::new(MAX_CONCURRENCY)),
    })
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

    /// A task the fingerprint has not seen keeps its timer; one whose schedule
    /// changed does not. This is what makes a 60-second sweep safe — a naive
    /// reconcile would replace every `DurationJob`'s timer each pass and a
    /// five-minute task would never fire.
    #[test]
    fn the_fingerprint_tracks_the_schedule_and_ignores_the_run_counters() {
        let mut task = ScheduledTask {
            id: "t1".to_string(),
            name: "n".to_string(),
            description: String::new(),
            prompt: "p".to_string(),
            agent_slug: String::new(),
            working_directory: String::new(),
            model: String::new(),
            settings_profile_id: String::new(),
            timeout_minutes: 30,
            schedule_type: "interval".to_string(),
            schedule_config: crate::native::tasks::ScheduleConfig {
                every_minutes: 5,
                ..Default::default()
            },
            stop_after_count: 0,
            stop_after_time: None,
            save_output: false,
            status: "active".to_string(),
            run_count: 0,
            last_run_at: None,
            last_run_status: String::new(),
            next_run_at: None,
            created_at: Default::default(),
            updated_at: Default::default(),
        };
        let before = fingerprint(&task);

        // `updateTaskAfterRun` bumps all three of these after every single run.
        task.run_count = 9;
        task.last_run_status = "success".to_string();
        task.last_run_at = Some(crate::native::gotime::GoTime::from_utc(Utc::now()));
        task.updated_at = crate::native::gotime::GoTime::from_utc(Utc::now());
        assert_eq!(
            fingerprint(&task),
            before,
            "a run must not look like a schedule change"
        );

        // A real edit does.
        task.schedule_config.every_minutes = 7;
        assert_ne!(fingerprint(&task), before);

        task.schedule_config.every_minutes = 5;
        assert_eq!(fingerprint(&task), before);
        task.schedule_type = "cron".to_string();
        assert_ne!(fingerprint(&task), before, "the type is part of it too");
    }

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
