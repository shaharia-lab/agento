# The Credentials Checker

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first. The design is the
> VibeXP artifact `design-credentials-checker` (epic #597); this file carries
> the rules that are enforced in code.

Everything lives in `src-tauri/src/native/security_scan/`: `rules.rs` (the
vendored rule table and `CURRENT_RULESET_VERSION`), `scan.rs` (pure
`text -> Vec<Finding>`), `store.rs` (the whitelist-aware persistence),
`worker.rs` (the background loop) and `api.rs` (the `/api/security-scan/*`
routes). Each module's `//!` header is the full
statement; the rules below are the ones a change is most likely to break.

## The worker (#603)

- **It runs only while `credentials_checker_enabled` is on.** `lib.rs` calls
  `worker::sync` at boot, and the settings `PUT` calls it after every save
  (`every_save_syncs_the_credentials_checker_worker`). `sync` reads the
  *stored* flag under the worker lock and is a no-op when already in step, so
  racing saves cannot leave a worker running under a stored "off", and an
  unrelated save restarts nothing. Off means no thread and nothing read.
- **It is stoppable, unlike `insights::worker`.** The running worker is a
  `Mutex<Option<..>>` rather than a `OnceLock`; stopping sets the worker's
  `stopped` flag and drops its sender, and the loop checks the flag before
  every pass, after every `recv` and between sweep chunks. A stop is not a
  join, so a quick stop-then-start can overlap two writers for one batch —
  safe because `store::record_scan` is one idempotent transaction per session.
- **Incremental and periodic, like insights.** `scan.rs` announces changed
  sessions to both workers from the same `outcome.notifications`; the
  five-minute sweep over `store::needs_scanning` picks up a ruleset bump.
- **A changed session is marked before it is announced.** `needs_scanning`
  compares versions only, so `scan.rs` calls `store::mark_changed` (resets
  `ruleset_version` to 0) on every changed session first — whether or not the
  worker runs. Without it, an already-scanned session whose announcement was
  dropped (full queue, checker off) is never rescanned
  (`a_changed_session_whose_announcement_was_dropped_is_swept`).
- **The scanned text is uncapped and nothing is dropped as injected** — unlike
  the search index, a leak detector cannot afford to miss text. Readers hand
  results to the single writer over a channel the size of the reader pool, so
  memory is bounded by the pool, not the batch.
- **One start-driving test per binary**: `tests/security_scan_worker.rs` runs
  the whole lifecycle as one test, because the worker is process-wide.

## The `/api` surface (#604)

- **Seven routes, desktop-only.** `api::ROUTES` is the fifth owner of
  `parity/desktop_routes.json` (set equality), not a row in the frozen Go
  route files. `/api/security-scan/` is not under `/api/security/`, so the
  `GET`s need `read` and the writes `write`
  (`the_reads_need_read_and_the_writes_need_write`).
- **No `match_hash` on the wire, anywhere.** It is a plain SHA-256 of a leaked
  value. A finding is whitelisted by value through
  `POST …/findings/{id}/whitelist`, which reads the hash server-side; a value
  entry is listed by `kind: "value"`, reason and date.
- **One whitelist entry per target.** `store::ensure_whitelist_entry` looks up
  and inserts in one `IMMEDIATE` transaction, so a repeat answers the existing
  entry with `200` rather than adding a second row that would make a delete
  appear not to re-open anything. A rule-level entry must name a rule in
  `rules::compiled()` (else `422`).
- **`false_positive` is permanent** through this surface: no rescan and no
  whitelist write moves a row out of it, and no route re-opens one.
- **`status` reports `enabled` and `running` separately** — the stored
  setting and `worker::is_running()`. They differ when the worker failed to
  spawn.
