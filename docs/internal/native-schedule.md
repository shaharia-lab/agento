# The scheduler (#275) and the executor

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

**Only one process may schedule.** Two schedulers over one `scheduled_tasks`
table fire every task twice and re-register the Telegram webhook under whichever
registered last. `runtime::shell_owns_scheduler` is now simply "is there a
database".

**Three pieces, and they had to move together:**

- `native/schedule/` already held the *computation* — `buildJobDefinition` and
  the `next()` of each `gocron/v2` job type, pinned to
  `parity/scheduler_vectors.json` (68 cases from a real
  `gocron.Scheduler` on a `clockwork` fake clock). Untouched by this change; see
  the semantics below, which are still the part most likely to be subtly wrong.
- `native/schedule/runtime.rs` is `scheduler.go`: the `task id → timer` registry,
  `ScheduleTask`/`UnscheduleTask`, the three-slot semaphore, and `Start`.
- `native/schedule/executor.rs` is `executor.go`: one run, end to end.
- The five task writes (`POST /api/tasks`, `PUT`/`DELETE /api/tasks/{id}`,
  `pause`, `resume`) moved with them, because each also registers or unregisters
  a cron entry — a task stored by one process and scheduled by the other is a
  task that never fires. `write_routes.json` records them as `native` now.

**The rule the executor is written around.** With no Go scheduler listening,
*a fire this process declines is a fire nothing serves* — the seam's "return
`Err` and let Go answer" is not available, because there is no request. So every
path ends in a `job_history` row. That includes the one case Go has no
equivalent for: `chat/runner.rs::build_options` can still refuse an agent whose
tools this build cannot supply (a `capabilities.mcp` name that neither
`mcps.yaml` nor a hostable integration resolves — a `whatsapp` row reaches that
by construction — or one whose `mcps.yaml` could not be read), and
where a chat forwards, a scheduled run records a **failed** row reading
`agent tools unavailable in this build: …` and publishes the failed event.
Silence is the one outcome that is not allowed, because a job history with no row
is indistinguishable from a task that was not due.

**One consequence of that rule since #556: a `success` row may carry a non-empty
`error_message`.** A run whose `system`/`init` frame shows the CLI never handed
the model a hosted server's tools completed and answered — that is a notice, not
a failure — but nobody reads the app log at 03:00 and that column is the row's
only free text, so `finish` puts the sentence there and leaves the status alone.
**Do not re-derive "non-empty `error_message` means failed"**: the failure signal
is `Recorded::failure`, which `publish` keys on, and `JobsView` titles the block
off the status for the same reason.

**A chat picks its own permission mode (migration 30).** Everything below about
`appendPermissionOpts` describes the state before it, and the two-branch rule it
describes is still exactly what runs — but only when the chat has expressed no
choice. `chat_sessions.permission_mode` is the conversation-level override, it
wins outright over both branches, and it exists because the interactive branch
was unconditional: a chat *always* has a permission handler, so a `bypass` agent
still stopped to ask and a `plan` agent silently behaved as `default`. Empty is
**not** a fifth mode — it means no choice was recorded, so every row written
before the migration keeps the behaviour it had, and `omitempty` keeps the field
off the wire for them too. All four of Claude Code's modes are accepted here,
where the *agent* validator takes only `""`/`bypass`/`default`: an agent's mode
governs unattended runs where nothing can answer a prompt, a chat's does not.
One trap the port reproduces rather than tidies — the bypass flag is not a
function of the mode. Go calls `WithPermissionMode` alone for `plan` and
`dontAsk`, so the SDK's own default (bypass on) survives for both; only
`default` clears it and only `bypass` sets it deliberately.

**What `build_options` had to grow, and why it is not a special case.** The chat
turn always has an interactive permission handler; a scheduled run never does.
Go models this as `if opts.PermissionHandler != nil` in *two* places, so the
parameter is now `Option<PermissionHandler>` and both branches are reproduced:
with a handler, `WithDefaultPermissions` overrides whatever the agent configured
(which is why a `plan` agent still prompts in the UI); without one, the agent's
own `permission_mode` applies — and an empty one means **bypass**, which is what
an unattended run needs since nothing is there to answer a prompt. `chat_id`
likewise became `custom_session_id`: a chat pins a new CLI session to its own id,
while `buildRunOptions` sets neither session field, so the CLI generates one and
`saveSessionResults` stores it back.

`resolveAgentConfig`'s no-agent branch returns a **synthesized `Agent`**, not
`None`, and that is load-bearing rather than tidy: Go builds a non-nil
`config.AgentConfig` there, and `resolveToolsAndMCP` gives a non-nil config with
empty capabilities **all twelve built-in tools** while a nil config gets none.
`None` would run a no-agent task with no `--allowedTools` argument at all.

**One deliberate divergence in the timer, and one rule that is not a divergence
at all.** gocron parks for the whole interval; `sleep_until` wakes at most every
60 seconds and re-reads the **wall clock**, because `tokio::time::sleep` is
measured on `Instant`, which does not advance while the machine is suspended — on
a laptop a single long park fires late by however long the lid was shut. The
chunking rule is `next_chunk`, a pure function of the two clocks, because a test
of the loop itself would wait the real duration: a paused tokio clock does not
move `Utc::now()`.

Waking up is only half of it. **After each fire the loop re-anchors against
`Utc::now()` via `advance_past_now`, rather than adding one interval**, which is
what gocron's `selectExecJobsOutForRescheduling` does under its own "the machine
went to sleep, and woke up some time later" comment. Stepping one interval at a
time and firing whenever the result is past *replays every missed window*: a
seven-minute task and a lid shut for eight hours is ~68 back-to-back Claude runs,
68 chat sessions, 68 `job_history` rows and a `stop_after_count` budget gone in
seconds. An NTP jump forward does the same. Note the walk is `while next < now`,
so a fire landing *exactly* on the wake instant is due rather than skipped —
gocron's `for next.Before(s.now())` has the same strictness, and it is only
observable when the interval divides the gap.

**Nothing blocking may run on the run's critical path.** `notifications::handle`
reaches lettre's *blocking* `SmtpTransport`, so `executor::publish` hands it to
`spawn_blocking` and **does not await it** — the
caller is still holding a scheduler semaphore permit, and an unreachable mail
host would otherwise throttle the scheduler to three runs per SMTP timeout while
starving the proxy and every in-flight SSE stream.

**Nor may the rusqlite, and that needs saying separately** (#366). A database
call reads as arithmetic and is not: `db::open_read_write` sets a five-second
`busy_timeout`, so a call meeting a lock held by the session scanner's batch
writer parks its thread for up to that long. `proxy.rs` puts its
native handlers on the blocking pool for this reason — but **only the buffered
ones**: `serve_stream` awaits on the worker, and `STREAM_ENDPOINTS` is the chat
turn, so `persist::commit` was a per-turn `open_read_write` plus three writes on
a worker. Beyond that, nothing reached from a *timer* or a *webhook* is covered
at all: the scheduler's executor runs three at once and the trigger dispatcher
ten, against a tokio default of one worker per core — three of four leaves the
SPA and every SSE stream one. `db::blocking(what, f)` is the single hand-off all
three use, and it is greppable on purpose; the label is per call site so the log
says which section panicked. The executor's shape follows from it: `prepare`
and `finish` are whole synchronous sections either side of the one long `await`,
rather than eight individually wrapped calls, because a run's database work is
contiguous. `a_contended_write_lock_does_not_stall_the_runtime` — one in
`tests/scheduled_run.rs`, one in `tests/chat_turn.rs` — is the regression — a
one-worker runtime, an outside thread holding the write lock, and a ticker whose
longest gap goes from ~11 ms to the full 1,500 ms hold if the hand-off is
removed. Its `last` is seeded **before** the spawn deliberately: a starved task
is never polled, so seeding on the first poll starts the clock after the stall
and the test passes against the defect. What is still on the worker is
option-building — `TurnSettings::stored` and `registry::can_host`, shared by all
three callers, plus `runner::load`, which is the chat turn's alone. Every one is
`open_read_only`, and a WAL reader does not wait on a writer, which is why they
are left: the test's contention is a write lock, so it says nothing about them
either way. A *write* added there would be a different matter. **So is a
subprocess**, and #533 nearly added one: `runner::claude_executable` can now
re-walk the CLI order, which spawns a login shell bounded at 3 s plus a
`--version` bounded at 2 s through a `std::thread::sleep` poll loop — twice the
hold this test was written to catch. It is `spawn_blocking`ed at that one call
site for exactly this rule; the list above is "non-blocking reads", not
"whatever option-building happens to do". A panic *inside* a handed-off
section is the one thing `db::blocking` cannot make safe: the executor's `finish`
may have written the session results and not the job row, leaving a `running`
row nothing will finish, which is why the rule is "every path ends in a job
history row" rather than "every path ends correctly".

**A scheduled run uses `claude::client::query`, not `Session`.** That is Go's
own choice — `RunAgent` calls `claude.Query` — and it is not interchangeable
here: a `Session` sets `session_mode`, and `process.rs`'s reader then
deliberately neither closes stdin nor stops at the `result` event, "so the
subprocess survives for the next send". A scheduled run has no next send, so the
event channel never closes, the drain blocks past the answer, and **every run
sits until its timeout** — 30 minutes by default, up to 240 — then records
`failed`, persists no chat, and holds one of the three semaphore permits
throughout. The one-shot reader closes stdin and breaks after emitting the
result, which is also what makes `collectRunResult`'s deliberate drain-past-the-
result terminate instead of hang.

`tests/scheduled_run.rs` is what catches this, and **its fake CLI must not exit
after the result**. A fake that exits closes stdout, ends the stream for free and
passes against a drain that never terminates on its own — the first version of
that test did exactly that and went green against the hang. A real CLI in session
mode stays alive; the fake has to as well.

**A run can also be started by a request, and the difference is one function
call** (#541). `POST /api/tasks/{id}/run` goes through
`executor::run_manual`, which is `execute_task` minus two things — and both
omissions are the feature rather than shortcuts:

- **`due_task` is not consulted.** It refuses a `paused` task and auto-pauses
  one past its `stop_after_count`/`stop_after_time`, in both cases returning
  *silently*. Right for a timer nobody asked to fire; wrong here, where running
  a paused task on demand is most of the point and a task at its limit is
  exactly the one somebody wants to try again.
- **`update_task_after_run` is skipped**, via `RunKind::Manual`. `run_count`,
  `last_run_at`, `last_run_status` and the two auto-pause rules are what the
  *schedule* has done; a manual run is a test of the configuration, and
  consuming a `stop_after_count` budget or pausing the task because the test was
  its tenth run would make the feature hostile. `next_run_at` needs no mention:
  nothing on the run path writes it, so skipping the write-back leaves it alone
  by construction. `RunKind` is a type rather than a `bool` because it is read at
  **four** call sites — `prepare`'s agent-resolution arm, both of `finish`'s
  arms, and `record_failed_run` — each with its own `Manual`/`Scheduled` pair
  over one fixture, because a guard dropped from any one of them ships with
  every other test green.

Everything else is identical, including the module's own rule that **every path
ends in a `job_history` row**. That row's id is **minted by the route**, not by
`create_initial_job_history`, so the `202` can name the row the run is about to
write; a scheduled run passes a fresh v4 uuid, which is what that line generated
before. **The row does not exist when the `202` is sent**, and not merely for a
moment: `run_manual` waits for one of the three permits first, so with three
240-minute runs in flight the id 404s until one ends. A consumer of it — #542 —
has to treat "not there yet" as a state rather than an error.

Two more properties are load-bearing. The **three-slot semaphore** is acquired
inside `run_manual` rather than by the route, because waiting for a permit is
precisely what must not happen inside a request — `Endpoint::serve` is a sync
`fn` on `spawn_blocking` and a run reaches 240 minutes, so the route spawns and
answers. And the **`409`** comes from `Scheduler::in_flight`, a *refcounted*
task-id map added for it: `jobs` holds timers, not runs, and the durable
alternative — a `job_history` row still marked `running` — cannot be used,
because a panic in `finish` leaves exactly such a row with nothing to finish it,
so a crash would 409 that task for ever. **Both run paths mark and only the
manual one refuses**, so a timer's behaviour is untouched while the route can
still see a scheduled run; the check and the mark are one operation under one
lock, or two simultaneous requests both pass and both start. A count rather than
a set, because a scheduled run that outlives its own interval overlaps the next
one and the first to finish would clear an entry the second still owns.

**A task write can fail after storing a row, so the timers are swept.**
`Scheduler::reconcile` runs every 60 seconds and brings the installed timers
back in line with the stored rows. It is a sweep rather than a hook on any one
write, so it also catches a row changed by anything else, and nothing at the
seam needs to know about tasks.

It is idempotent **by fingerprint**, and the fingerprint is deliberately narrow:
`schedule_type` plus the encoded `schedule_config`, and explicitly *not*
`updated_at`, which `updateTaskAfterRun` bumps after every single run.
Fingerprinting on `updated_at` would make each sweep replace a live timer, and
replacing a `DurationJob`'s timer restarts its interval from now — a five-minute
task swept every minute would never fire at all.

**The timer is dropped only when the write that paused the row succeeded.**
`update_task_after_run` used to unschedule unconditionally, which for a
`run_immediately` task whose write had just failed left it `active`, timer-less
*and* forgotten by the sweep — so `reconcile` reinstalled the timer, the task
fired two seconds later, the write failed again, and a full agent run happened
every minute indefinitely. A read-only data dir is enough to reach it. Go leaves
the timer alone on a failed `UpdateTask` too. Note `swept` is *not* what bounds
this: it is cleared by `unschedule_task` precisely so a task returning to service
is picked up, so the bound has to be at the source.

**The run's write-back re-reads the row.** `updateTaskAfterRun` writes the whole
`ScheduledTask` snapshot the timer loaded, so an edit made while the run was in
flight is clobbered — a task paused mid-run (timeouts reach 240 minutes) comes
back `active`. Go got away with that because nothing re-registered the cron
entry; with a reconcile sweep the timer returns within a minute and the "paused"
task goes on firing. So the port re-reads inside the write's own transaction and
applies only the fields the run owns. It never assigns `active`, so a concurrent
pause simply survives.

**The sweep is armed before the first read can fail.** A transient
`SQLITE_BUSY` at boot used to return from `start` with `RUNNING` already set —
no timers, no sweep, no way back until restart. The mechanism written to recover
from missing timers has to survive the failure it is most likely to be needed
for.

**It also remembers what it acted on** (`swept`, cleared by `unschedule_task`),
which is why an unschedulable task is not retried every minute: an active
`one_off` whose `run_at` passed while the machine was off fails forever, and
~1440 warning lines a day into a 5 MiB log is worse than the one line Go wrote at
startup.

**`agent.Interpolate` has two callers with two policies**, in
`native/template.rs`. The scheduler uses the strict form, because Go's
`resolveSystemPrompt` propagates `MissingVariableError` and a scheduled run must
end in a recorded `job_history` row rather than shipping a raw `{{name}}` to the
model — the executor checks the agent's system prompt itself, before
`build_options`, since that function is shared with chat. The chat path uses the
**lenient** form, which substitutes the built-ins and leaves an unknown
placeholder in the text. That is not sloppiness inherited by accident: it is what
that path has always done, and an agent whose prompt contains a literal `{{…}}`
for some other reason (a JSON example, another tool's syntax) would otherwise
lose date substitution entirely. One loop, two policies.

**The run timeout is one deadline across three stages.** Go wraps the whole
`agent.RunAgent` call; timing out only the event drain would let a subprocess
that hangs before its first event leave the `job_history` row `running` forever
*and* hold its permit for the life of the process. It is a shared
`tokio::time::Instant` rather than one future wrapping all three, because
`Session` has **no `Drop`**: cancelling a future that owns one abandons the
subprocess instead of stopping it, so `close()` has to stay reachable.

**Go's `scheduleTaskOnStartup` has no analogue.** It exists to `recover()` from
`robfig/cron` panicking on an expression of exactly `CRON_TZ=UTC` (#330);
`schedule::cron::parse` returns a `Result` for that input, so there is nothing to
catch and the row is skipped with a warning either way.

**A cron expression is validated at save time, through the scheduler's own
`setup`** (#330). `validate_task`'s `"cron"` arm calls
`schedule::validate_cron`, so `POST`/`PUT /api/tasks` answer **422** for an
expression that cannot parse (`CRON_TZ=UTC`, `TZ=`, a four-field spec) or that
parses and never fires (`0 0 30 2 *`), and no row is written. Two things about
it are load-bearing. It is a **wrapper over `setup`, never a second parser or a
regex** — a validator with its own dialect refuses expressions the scheduler
would have run, and `cron.rs`'s dialect is not the obvious one (descriptors are
accepted, six-field seconds specs are not, `?` sets the same bit `*` does). And
the location it validates against is `runtime::local_tz()`, the resolver whose
result **is** `Scheduler::loc`, rather than the running scheduler's field: a
write path has to work before `start` and in tests, and reading it off the
scheduler would make validation depend on boot order.

`schedule_task`'s log-and-skip is unchanged and is now about **rows stored
before that check existed**. Do not remove it along with the gap — it is what
keeps one bad legacy row from being worse than a warning line.

**The notification subscriber is wired at last.** `internal/notification`'s
handler was ported in #307 with its header noting the subscriber "cannot exist
yet" — the publisher was the Go scheduler, in another process. It is now a direct
call: `notifications::handle(db_path, event, payload)`, one publisher and one
subscriber, with an in-process event bus between two functions serving no
purpose. Note `notification_log.created_at` is `time.Now()` **local**, not UTC —
the one write in the codebase that is — and `gotime::now_go_text_local` exists to
reproduce it, because `ListNotifications` orders on that column *as text*.

The gocron semantics the vectors pin, three of them silent when reproduced wrong:

- **`run_immediately` is a one-time job at `now + 2s`.** gocron discards
  one-time start times that are not strictly in the future and then refuses the
  job outright, so "now" would never run. The vector records the *offset*
  rather than an instant, because Go reads `time.Now()` inside the builder.
- **`every_days` + `at_time` is a `DailyJob`, not a 24-hour `DurationJob`.** A
  daily job holds the wall clock across a DST transition; a duration job adds 24
  absolute hours and walks off it. Both are in the vectors from the same
  Europe/Berlin start: midnight stays midnight, 12:00 becomes 13:00.
- **A malformed `at_time` falls back to that duration job with no error.**
  `buildIntervalJob` discards `buildDailyAtTimeJob`'s error, so `9am`,
  `09:00:00`, `25:00` and `09:60` all schedule *something else* and say nothing.
  Note `7:5` is **not** a fallback — `Atoi` needs no zero padding.

Two more the vectors pin, both invisible in UTC:

- **`robfig/cron`'s dialect** (`cron.rs`), because `gocron.CronJob` delegates to
  `ParseStandard` with `CRON_TZ=<location>` prepended. Descriptors *are*
  accepted (`@daily`, `@every 1h30m` — Go's `ParseDuration`, floored at 1s);
  six-field seconds specs are *not*; `N/step` means `N-max/step`; and `?` sets
  the same star bit `*` does, which decides whether day-of-month and day-of-week
  are ANDed or ORed. Its `Next` steps **absolute** hours, so a daily `0 2 * * *`
  in Europe/Berlin skips 2026-03-29 entirely rather than shifting — while
  gocron's own duplicate-wall-clock guard stops it running twice on the October
  Sunday when 02:00 happens twice. `*/30 * * * *` does repeat through that hour,
  because the guard only catches an identical wall clock.
- **A one-off keeps `run_at`'s own offset**; every other job type renders in the
  scheduler's location, which is `time.Local` resolved through
  `iana_time_zone::get_timezone`. That is why `Fire` carries an offset beside the
  instant.

`cron.rs` reuses `analytics/buckets.rs`'s `go_date`/`add_date` rather than
`chrono`'s calendar arithmetic — robfig resets the lower fields with
`time.Date`, which *normalizes* a wall clock a DST gap removed instead of
failing, and that normalization is the answer on the spring-forward day.
