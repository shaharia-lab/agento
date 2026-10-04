//! Per-task limits on **event** runs (#691): how many run at once, how many
//! wait, and how many start in an hour.
//!
//! An event is somebody outside the app starting a run — a Slack mention, a
//! Telegram message, a webhook call — so nothing about its rate is the
//! owner's choice. Without a bound per task, ten mentions are ten runs, and
//! they can hold every one of the scheduler's three global slots while they
//! are at it. Scheduled and manual runs do not pass through here and are not
//! counted: the schedule is the owner's own rate, and **Run now** is a person.
//!
//! # The decision, made once, at arrival
//!
//! [`decide`] is the whole rule, as a pure function of three counts:
//!
//! 1. **Rate.** `reserved` is at `per_hour` → [`Decision::RateLimit`].
//! 2. **Concurrency.** `running` is below `concurrent` → [`Decision::Admit`].
//! 3. **Queue.** `queued` is below `queued` → [`Decision::Queue`], first in,
//!    first out.
//! 4. Otherwise → [`Decision::Drop`].
//!
//! `reserved` is every start this task has had in the last sixty minutes
//! **plus every admitted or queued event that has not started yet**. Counting
//! the ones still to start is what lets the rate be checked at arrival and
//! never again: an event that is let in has its place in the hour already, so
//! a queued event can start whenever its turn comes without a second check
//! refusing it after it waited.
//!
//! That is also why the cap holds. Take any sixty minutes and the event that
//! arrived last among those starting in it: when it arrived, every other start
//! in that hour had either happened inside the previous sixty minutes or was
//! still to come, and both are in `reserved`.
//!
//! # No clock and no database in here
//!
//! Every method that needs the time is handed it, and the hour's history
//! before this process started is handed in through [`TaskLimiter::seed`].
//! The executor owns both, which keeps SQLite off this module entirely — the
//! one mutex here is held for arithmetic and never across an `await`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Duration, Utc};
use tokio::sync::oneshot;

/// A task's three limits, as counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Event runs of the task in flight at once.
    pub concurrent: usize,
    /// Events that may wait for one of those slots.
    pub queued: usize,
    /// Event runs that may start in any sixty minutes.
    pub per_hour: usize,
}

/// What a task stores when a write names no limit, and what migration 54 gave
/// every existing row.
pub const DEFAULT_MAX_CONCURRENT_RUNS: i64 = 1;
pub const DEFAULT_MAX_QUEUED_EVENTS: i64 = 5;
pub const DEFAULT_MAX_RUNS_PER_HOUR: i64 = 10;

/// The most a write may store. Concurrency stops at the scheduler's own three
/// slots: a task allowed more could never use them.
pub const MAX_CONCURRENT_RUNS: i64 = 3;
pub const MAX_QUEUED_EVENTS: i64 = 100;
pub const MAX_RUNS_PER_HOUR: i64 = 1000;

/// The window the rate cap is counted over.
fn window() -> Duration {
    Duration::minutes(60)
}

impl Limits {
    /// The limits of a stored row.
    ///
    /// The write path stores only values inside the ranges, so this is for a
    /// row something else wrote: below 1 reads as the default, as the route
    /// reads a 0, and above the range reads as the top of it.
    pub fn from_stored(concurrent: i64, queued: i64, per_hour: i64) -> Self {
        let bounded = |value: i64, default: i64, max: i64| {
            let value = if value < 1 { default } else { value.min(max) };
            usize::try_from(value).unwrap_or(1)
        };
        Self {
            concurrent: bounded(concurrent, DEFAULT_MAX_CONCURRENT_RUNS, MAX_CONCURRENT_RUNS),
            queued: bounded(queued, DEFAULT_MAX_QUEUED_EVENTS, MAX_QUEUED_EVENTS),
            per_hour: bounded(per_hour, DEFAULT_MAX_RUNS_PER_HOUR, MAX_RUNS_PER_HOUR),
        }
    }
}

/// What one task is doing when an event arrives for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Load {
    /// Event runs holding a slot.
    pub running: usize,
    /// Events waiting for one.
    pub queued: usize,
    /// Starts in the last sixty minutes, plus every admitted or queued event
    /// that has not started yet.
    pub reserved: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Admit,
    Queue,
    Drop,
    RateLimit,
}

/// The admission rule. See the module header for the order and why the rate
/// comes first.
pub fn decide(load: Load, limits: Limits) -> Decision {
    if load.reserved >= limits.per_hour {
        Decision::RateLimit
    } else if load.running < limits.concurrent {
        Decision::Admit
    } else if load.queued < limits.queued {
        Decision::Queue
    } else {
        Decision::Drop
    }
}

/// Why an event got no slot. Either way no run starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Every slot was taken and the queue was full.
    Dropped,
    /// The task has had its hour's worth of starts.
    RateLimited,
}

#[derive(Default)]
struct TaskState {
    /// Whether [`TaskLimiter::seed`] has run for this task.
    seeded: bool,
    /// The limits the latest arrival carried — what a freed slot is refilled
    /// under, so an edit to the task applies to the events already waiting.
    concurrent: usize,
    running: usize,
    /// Waiting events, oldest first, by the id of the [`RunSlot`] each holds.
    waiting: VecDeque<(u64, oneshot::Sender<()>)>,
    /// Admitted or queued events whose run has not started.
    unstarted: usize,
    /// When each event run of the last sixty minutes started.
    starts: Vec<DateTime<Utc>>,
}

impl TaskState {
    /// Hand free slots to the events that have waited longest.
    ///
    /// A send that fails is a waiter that has gone away without having run
    /// its `Drop` yet. The slot is counted as its own all the same: that
    /// `Drop` finds itself no longer waiting, gives the slot back and calls
    /// this again.
    fn promote(&mut self) {
        while self.running < self.concurrent {
            let Some((_, wake)) = self.waiting.pop_front() else {
                return;
            };
            self.running += 1;
            let _ = wake.send(());
        }
    }
}

type Tasks = Arc<Mutex<HashMap<String, TaskState>>>;

/// Poisoning cannot happen — nothing panics while this lock is held — but a
/// limiter that refused every event because of one would be silent.
fn lock(tasks: &Tasks) -> MutexGuard<'_, HashMap<String, TaskState>> {
    tasks.lock().unwrap_or_else(|e| e.into_inner())
}

/// The limiter the [`super::runtime::Scheduler`] owns: one small state per
/// task that has had an event since startup.
#[derive(Default)]
pub struct TaskLimiter {
    tasks: Tasks,
    next_id: AtomicU64,
}

/// One event's place: in the queue until it is woken, then a running slot
/// until it is dropped.
///
/// A guard rather than a matching release call, for [`super::runtime::RunGuard`]'s
/// reason: a run has many exits, and so does a wait — the client that sent the
/// event can go away, and the app can shut down. Dropping this wherever it is
/// frees exactly what it held and wakes the next waiter.
pub struct RunSlot {
    tasks: Tasks,
    task_id: String,
    id: u64,
    started: bool,
}

impl RunSlot {
    /// Record that the run began, at `at`. From here the event counts against
    /// the hour as a start rather than as a reservation, and keeps counting
    /// for sixty minutes after `at` whether or not the run has finished.
    pub fn started(&mut self, at: DateTime<Utc>) {
        if self.started {
            return;
        }
        self.started = true;
        let mut tasks = lock(&self.tasks);
        if let Some(state) = tasks.get_mut(&self.task_id) {
            state.unstarted = state.unstarted.saturating_sub(1);
            state.starts.push(at);
        }
    }
}

impl Drop for RunSlot {
    fn drop(&mut self) {
        let mut tasks = lock(&self.tasks);
        let Some(state) = tasks.get_mut(&self.task_id) else {
            return;
        };
        if let Some(at) = state.waiting.iter().position(|(id, _)| *id == self.id) {
            // Still waiting: it leaves the queue and its reservation goes.
            state.waiting.remove(at);
            state.unstarted = state.unstarted.saturating_sub(1);
            return;
        }
        state.running = state.running.saturating_sub(1);
        if !self.started {
            // Admitted and then refused further on — a paused task, a closed
            // semaphore. No run started, so the hour is not charged for one.
            state.unstarted = state.unstarted.saturating_sub(1);
        }
        state.promote();
    }
}

impl TaskLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this task's hour has been seeded since startup.
    pub fn is_seeded(&self, task_id: &str) -> bool {
        lock(&self.tasks)
            .get(task_id)
            .is_some_and(|state| state.seeded)
    }

    /// Give a task the starts it had before this process knew it, so a restart
    /// does not hand out a fresh hour. **The first seed wins**: once a task is
    /// seeded its own slots keep the history, and a second read of
    /// `job_history` would count those runs twice.
    pub fn seed(&self, task_id: &str, starts: Vec<DateTime<Utc>>) {
        let mut tasks = lock(&self.tasks);
        let state = tasks.entry(task_id.to_string()).or_default();
        if state.seeded {
            return;
        }
        state.seeded = true;
        state.starts.extend(starts);
    }

    /// Take a slot for one event that arrived at `now`, waiting in the task's
    /// queue when every slot is busy.
    ///
    /// The decision is made before the first `await` and is final: a refusal
    /// returns at once, and an event that is queued is not refused later.
    /// Dropping the future while it waits gives the queue place back.
    pub async fn admit(
        &self,
        task_id: &str,
        limits: Limits,
        now: DateTime<Utc>,
    ) -> Result<RunSlot, Refusal> {
        let (slot, wake) = {
            let mut tasks = lock(&self.tasks);
            let state = tasks.entry(task_id.to_string()).or_default();
            state.concurrent = limits.concurrent;
            // A raised limit reaches the events already waiting before this
            // one is considered, so it cannot overtake them.
            state.promote();
            // A start stamped in the future — the clock was set back — is
            // kept: counting it is the choice that cannot exceed the cap.
            let cutoff = now - window();
            state.starts.retain(|start| *start > cutoff);

            let load = Load {
                running: state.running,
                queued: state.waiting.len(),
                reserved: state.starts.len() + state.unstarted,
            };
            let slot = |id| RunSlot {
                tasks: Arc::clone(&self.tasks),
                task_id: task_id.to_string(),
                id,
                started: false,
            };
            match decide(load, limits) {
                Decision::RateLimit => return Err(Refusal::RateLimited),
                Decision::Drop => return Err(Refusal::Dropped),
                Decision::Admit => {
                    state.running += 1;
                    state.unstarted += 1;
                    return Ok(slot(self.next_id.fetch_add(1, Ordering::Relaxed)));
                }
                Decision::Queue => {
                    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                    let (wake, woken) = oneshot::channel();
                    state.waiting.push_back((id, wake));
                    state.unstarted += 1;
                    (slot(id), woken)
                }
            }
        };
        // The sender lives in the task's state, which is never removed, so
        // this resolves only by being woken. Were it ever dropped instead, the
        // slot's own `Drop` gives the queue place back.
        match wake.await {
            Ok(()) => Ok(slot),
            Err(_) => Err(Refusal::Dropped),
        }
    }

    /// `(running, queued)` for one task, for tests of the callers.
    #[cfg(test)]
    pub fn load(&self, task_id: &str) -> (usize, usize) {
        lock(&self.tasks)
            .get(task_id)
            .map_or((0, 0), |state| (state.running, state.waiting.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULTS: Limits = Limits {
        concurrent: 1,
        queued: 5,
        per_hour: 10,
    };

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-04T10:00:00Z")
            .expect("a timestamp")
            .with_timezone(&Utc)
            + Duration::minutes(minute)
    }

    fn load(running: usize, queued: usize, reserved: usize) -> Load {
        Load {
            running,
            queued,
            reserved,
        }
    }

    /// Let every spawned waiter reach its `await`.
    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    #[test]
    fn the_decision_at_each_boundary() {
        let cases = [
            (load(0, 0, 0), Decision::Admit),
            // The last free slot, and the first event with none.
            (load(0, 0, 9), Decision::Admit),
            (load(1, 0, 1), Decision::Queue),
            // The last queue place, and the first event past it.
            (load(1, 4, 5), Decision::Queue),
            (load(1, 5, 6), Decision::Drop),
            // The rate comes first, whatever else is free.
            (load(0, 0, 10), Decision::RateLimit),
            (load(1, 5, 10), Decision::RateLimit),
            (load(1, 2, 11), Decision::RateLimit),
        ];
        for (given, want) in cases {
            assert_eq!(decide(given, DEFAULTS), want, "{given:?}");
        }
        let wide = Limits {
            concurrent: 3,
            queued: 1,
            per_hour: 1000,
        };
        assert_eq!(decide(load(2, 0, 2), wide), Decision::Admit);
        assert_eq!(decide(load(3, 0, 3), wide), Decision::Queue);
        assert_eq!(decide(load(3, 1, 4), wide), Decision::Drop);
    }

    #[test]
    fn a_stored_row_outside_the_ranges_reads_as_a_usable_limit() {
        assert_eq!(Limits::from_stored(1, 5, 10), DEFAULTS);
        assert_eq!(Limits::from_stored(0, 0, 0), DEFAULTS, "zero is unset");
        assert_eq!(Limits::from_stored(-4, -1, -9), DEFAULTS);
        assert_eq!(
            Limits::from_stored(99, 5000, 1_000_000),
            Limits {
                concurrent: 3,
                queued: 100,
                per_hour: 1000
            }
        );
    }

    /// Gate (h): 20 events in a minute at the default limits are one running,
    /// five queued and fourteen refused.
    #[tokio::test]
    async fn twenty_events_in_a_minute_are_one_running_five_queued_and_the_rest_dropped() {
        let limiter = Arc::new(TaskLimiter::new());
        let running = limiter
            .admit("t1", DEFAULTS, at(0))
            .await
            .expect("the first runs");

        let mut waiters = Vec::new();
        for _ in 0..5 {
            let limiter = Arc::clone(&limiter);
            waiters.push(tokio::spawn(async move {
                limiter.admit("t1", DEFAULTS, at(0)).await
            }));
        }
        settle().await;
        assert_eq!(limiter.load("t1"), (1, 5));

        for n in 0..14 {
            assert_eq!(
                limiter.admit("t1", DEFAULTS, at(0)).await.err(),
                Some(Refusal::Dropped),
                "event {n} past the queue"
            );
        }
        assert_eq!(limiter.load("t1"), (1, 5), "a refusal holds nothing");
        assert!(waiters.iter().all(|w| !w.is_finished()));
        drop(running);
    }

    /// The queue is first in, first out, and a slow run loses none of it.
    #[tokio::test]
    async fn queued_events_start_in_arrival_order_as_running_ones_finish() {
        let limiter = Arc::new(TaskLimiter::new());
        let mut first = limiter.admit("t1", DEFAULTS, at(0)).await.expect("runs");
        first.started(at(0));

        let order = Arc::new(Mutex::new(Vec::new()));
        // One permit ends one run, so the test decides when each finishes.
        let finish = Arc::new(tokio::sync::Semaphore::new(0));
        let mut waiters = Vec::new();
        for n in 0..3 {
            let (limiter, order, finish) = (
                Arc::clone(&limiter),
                Arc::clone(&order),
                Arc::clone(&finish),
            );
            waiters.push(tokio::spawn(async move {
                let mut slot = limiter.admit("t1", DEFAULTS, at(0)).await.expect("queued");
                slot.started(at(1));
                order.lock().expect("order").push(n);
                // Hold the slot until the test ends this run, so the order
                // recorded is the order the waiters were woken in.
                finish.acquire().await.expect("open").forget();
            }));
            // Arrival order is the order the waiters reach the queue in.
            settle().await;
        }
        assert_eq!(limiter.load("t1"), (1, 3));
        assert!(order.lock().expect("order").is_empty(), "a slow run");

        drop(first);
        settle().await;
        assert_eq!(*order.lock().expect("order"), vec![0], "one at a time");
        assert_eq!(limiter.load("t1"), (1, 2));

        for expected in [vec![0, 1], vec![0, 1, 2]] {
            finish.add_permits(1);
            settle().await;
            assert_eq!(*order.lock().expect("order"), expected);
        }
        finish.add_permits(1);
        for waiter in waiters {
            waiter.await.expect("ran");
        }
        assert_eq!(limiter.load("t1"), (0, 0));
    }

    #[tokio::test]
    async fn one_tasks_queue_does_not_hold_up_another_task() {
        let limiter = Arc::new(TaskLimiter::new());
        let _busy = limiter.admit("t1", DEFAULTS, at(0)).await.expect("runs");
        let waiting = {
            let limiter = Arc::clone(&limiter);
            tokio::spawn(async move { limiter.admit("t1", DEFAULTS, at(0)).await })
        };
        settle().await;
        assert_eq!(limiter.load("t1"), (1, 1));

        let other = limiter.admit("t2", DEFAULTS, at(0)).await;
        assert!(other.is_ok(), "t2 has its own slots");
        assert!(!waiting.is_finished());
        waiting.abort();
    }

    /// The risk the issue names: a waiter that is cancelled must not keep its
    /// queue place, or its reservation in the hour.
    #[tokio::test]
    async fn a_cancelled_waiter_gives_its_queue_place_and_its_reservation_back() {
        let limiter = Arc::new(TaskLimiter::new());
        let tight = Limits {
            concurrent: 1,
            queued: 1,
            per_hour: 2,
        };
        let running = limiter.admit("t1", tight, at(0)).await.expect("runs");
        let waiting = {
            let limiter = Arc::clone(&limiter);
            tokio::spawn(async move { limiter.admit("t1", tight, at(0)).await })
        };
        settle().await;
        assert_eq!(
            limiter.admit("t1", tight, at(0)).await.err(),
            Some(Refusal::RateLimited),
            "one running and one queued are the hour's two"
        );

        waiting.abort();
        let _ = waiting.await;
        assert_eq!(limiter.load("t1"), (1, 0), "the queue place is free");

        let again = {
            let limiter = Arc::clone(&limiter);
            tokio::spawn(async move { limiter.admit("t1", tight, at(0)).await })
        };
        settle().await;
        assert_eq!(limiter.load("t1"), (1, 1), "and so is the reservation");
        drop(running);
        assert!(again.await.expect("joined").is_ok());
    }

    /// A waiter cancelled in the instant it is woken owns a slot it will never
    /// use. Its drop must pass the slot on rather than leak it.
    #[tokio::test]
    async fn a_waiter_cancelled_as_it_is_woken_passes_the_slot_on() {
        let limiter = Arc::new(TaskLimiter::new());
        let running = limiter.admit("t1", DEFAULTS, at(0)).await.expect("runs");
        let mut doomed = Box::pin(limiter.admit("t1", DEFAULTS, at(0)));
        // Polled once, which is what puts it in the queue.
        tokio::select! {
            biased;
            _ = doomed.as_mut() => panic!("the only slot is taken"),
            () = std::future::ready(()) => {}
        }
        let next = {
            let limiter = Arc::clone(&limiter);
            tokio::spawn(async move { limiter.admit("t1", DEFAULTS, at(0)).await })
        };
        settle().await;
        assert_eq!(limiter.load("t1"), (1, 2));

        // Woken, and dropped before it is polled again.
        drop(running);
        drop(doomed);
        let slot = next.await.expect("joined").expect("the slot reached it");
        assert_eq!(limiter.load("t1"), (1, 0));
        drop(slot);
        assert_eq!(limiter.load("t1"), (0, 0));
    }

    /// The cap: with N an hour, the N+1th start has to wait for the first to
    /// leave the window, and no sixty minutes ever holds more than N.
    #[tokio::test]
    async fn no_sixty_minutes_holds_more_starts_than_the_cap() {
        let limiter = TaskLimiter::new();
        let cap = Limits {
            concurrent: 1,
            queued: 5,
            per_hour: 3,
        };
        let mut starts = Vec::new();
        // An event every five minutes for three hours, each run over at once.
        for minute in (0..180).step_by(5) {
            match limiter.admit("t1", cap, at(minute)).await {
                Ok(mut slot) => {
                    slot.started(at(minute));
                    starts.push(minute);
                }
                Err(refused) => assert_eq!(refused, Refusal::RateLimited, "minute {minute}"),
            }
        }
        assert_eq!(starts, vec![0, 5, 10, 60, 65, 70, 120, 125, 130]);
        for from in 0..180 {
            let inside = starts
                .iter()
                .filter(|s| **s > from - 60 && **s <= from)
                .count();
            assert!(inside <= 3, "{inside} starts in the hour ending at {from}");
        }
    }

    /// A run that outlives the hour still counted against it only for the
    /// hour after it started, and a finished one keeps counting until then.
    #[tokio::test]
    async fn a_start_counts_for_sixty_minutes_whether_or_not_the_run_is_over() {
        let limiter = TaskLimiter::new();
        let one = Limits {
            concurrent: 3,
            queued: 5,
            per_hour: 1,
        };
        let mut slot = limiter.admit("t1", one, at(0)).await.expect("runs");
        slot.started(at(0));
        drop(slot);
        assert_eq!(
            limiter.admit("t1", one, at(59)).await.err(),
            Some(Refusal::RateLimited),
            "finished, and still inside its hour"
        );
        let mut long = limiter.admit("t1", one, at(60)).await.expect("a new hour");
        long.started(at(60));
        assert_eq!(
            limiter.admit("t1", one, at(119)).await.err(),
            Some(Refusal::RateLimited)
        );
        assert!(
            limiter.admit("t1", one, at(120)).await.is_ok(),
            "still running, and its hour is over"
        );
    }

    /// An event admitted and then refused further on started no run, so it
    /// leaves the hour as it found it.
    #[tokio::test]
    async fn a_slot_given_back_unstarted_is_not_charged_to_the_hour() {
        let limiter = TaskLimiter::new();
        let one = Limits {
            concurrent: 1,
            queued: 1,
            per_hour: 1,
        };
        drop(limiter.admit("t1", one, at(0)).await.expect("admitted"));
        assert!(limiter.admit("t1", one, at(1)).await.is_ok());
    }

    /// A restart: the hour is seeded from what the last session started, the
    /// seed is taken once, and it ages out like any other start.
    #[tokio::test]
    async fn a_seeded_hour_is_counted_and_seeded_only_once() {
        let limiter = TaskLimiter::new();
        assert!(!limiter.is_seeded("t1"));
        limiter.seed("t1", (0..10).map(|n| at(n - 30)).collect());
        assert!(limiter.is_seeded("t1"));
        // A second read of the same history must not double it.
        limiter.seed("t1", (0..10).map(|n| at(n - 30)).collect());

        assert_eq!(
            limiter.admit("t1", DEFAULTS, at(0)).await.err(),
            Some(Refusal::RateLimited),
            "the eleventh in the hour, across the restart"
        );
        assert!(
            limiter.admit("t1", DEFAULTS, at(30)).await.is_ok(),
            "the oldest seeded start is exactly an hour old and has left"
        );
        assert!(!limiter.is_seeded("t2"), "seeding is per task");
    }

    /// Raising a task's concurrency reaches the events already waiting, and
    /// the arrival that carried the new limit does not overtake them.
    #[tokio::test]
    async fn a_raised_concurrency_limit_wakes_the_waiters_first() {
        let limiter = Arc::new(TaskLimiter::new());
        let _running = limiter.admit("t1", DEFAULTS, at(0)).await.expect("runs");
        let waiting = {
            let limiter = Arc::clone(&limiter);
            tokio::spawn(async move { limiter.admit("t1", DEFAULTS, at(0)).await })
        };
        settle().await;

        let two = Limits {
            concurrent: 2,
            ..DEFAULTS
        };
        let late = {
            let limiter = Arc::clone(&limiter);
            tokio::spawn(async move { limiter.admit("t1", two, at(0)).await })
        };
        let _woken = waiting.await.expect("joined").expect("the waiter runs");
        settle().await;
        assert_eq!(limiter.load("t1"), (2, 1), "and the late one queues");
        late.abort();
    }
}
