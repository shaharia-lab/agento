# The Credentials Checker

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first. The design is the
> VibeXP artifact `design-credentials-checker` (epic #597); this file carries
> the rules that are enforced in code.

Everything lives in `src-tauri/src/native/security_scan/`: `rules.rs` (the
vendored rule table and `CURRENT_RULESET_VERSION`), `scan.rs` (pure
`text -> Vec<Finding>`), `store.rs` (the whitelist-aware persistence) and
`worker.rs` (the background loop). Each module's `//!` header is the full
statement; the rules below are the ones a change is most likely to break.

## The worker (#603)

- **It runs only while `credentials_checker_enabled` is on.** `lib.rs` calls
  `worker::start_if_enabled` at boot, and the settings `PUT` calls
  `worker::apply_setting` on a flip — and only on a flip
  (`only_a_flip_of_the_credentials_checker_reaches_its_worker`). Off means no
  thread and nothing read.
- **It is stoppable, unlike `insights::worker`.** The running worker is a
  `Mutex<Option<..>>` rather than a `OnceLock`; `stop` sets the worker's
  `stopped` flag and drops its sender, and the loop checks the flag before
  every pass, after every `recv` and between sweep chunks. A stop is not a
  join, so a quick stop-then-start can overlap two writers for one batch —
  safe because `store::record_scan` is one idempotent transaction per session.
- **Incremental and periodic, like insights.** `scan.rs` announces changed
  sessions to both workers from the same `outcome.notifications`; the
  five-minute sweep over `store::needs_scanning` is what picks up a ruleset
  bump and any overflowed announcement.
- **The scanned text is uncapped and nothing is dropped as injected** — unlike
  the search index, a leak detector cannot afford to miss text. Readers hand
  results to the single writer over a channel the size of the reader pool, so
  memory is bounded by the pool, not the batch.
- **One start-driving test per binary**: `tests/security_scan_worker.rs` runs
  the whole lifecycle as one test, because the worker is process-wide.
