# The Credentials Checker

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first. The design is the
> VibeXP artifact `design-credentials-checker` (epic #597); this file carries
> the rules that are enforced in code.

Everything lives in `src-tauri/src/native/security_scan/`: `rules.rs` (the
vendored rule table and `CURRENT_RULESET_VERSION`), `scan.rs` (pure
`text -> Vec<Finding>`, and `mask_text`), `store.rs` (the whitelist-aware
persistence), `worker.rs` (the background loop) and `api.rs` (the
`/api/security-scan/*` routes). Each module's `//!` header is the full
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
- **An expired session is never pending** (#707): `needs_scanning` excludes a
  cache row with `transcript_expired_at` set on both branches, so neither a
  sweep nor a ruleset bump retries a transcript that is gone. Its findings and
  `credential_scan_state` row are kept and still counted
  (`needs_scanning_skips_an_expired_row`) until the user deletes the session
  (#711) or the retention prune removes it (#712), either of which removes both
  through `store::delete_session` in the delete's own transaction. `credential_whitelist` is not session-linked and survives.
- **A changed session is marked before it is announced.** `needs_scanning`
  looks at versions, never at whether a file changed, so `scan.rs` calls
  `store::mark_changed` (resets `ruleset_version` to 0) on every changed
  session first — whether or not the worker runs. Without it, an already-scanned session whose announcement was
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

## Masking a text (#680)

- **`scan::mask_text(&str) -> String` is `scan::scan`'s findings, applied.**
  Every finding's byte range is replaced by `store::mask_for` of it — the
  display form `credential_findings.masked_snippet` already carries — and
  every other byte is copied verbatim; a text with no findings comes back
  byte-identical. Overlapping spans are merged into one replacement first.
- **A rule may carry its own mask, and `store::mask_for(rule_id, matched)` is
  the one entry point for both stored forms** (#741). `rules::Rule::mask` is
  `None` for the default `store::mask` (first four, a fixed run, last four);
  `database-url-credentials` sets it, because its match ends at the end of the
  host and a host under four characters would put the end of the password in
  the default tail. Its tail is the last four characters of the host alone,
  fewer when the host is shorter. A span merged from several findings shows
  only the run when any of them has its own mask. Enforced by
  `a_database_urls_mask_never_shows_a_password_character` and
  `the_stored_snippet_is_the_form_mask_text_gives_the_same_match`.
- **Changing a stored form needs two rewrites, because a rescan does not reach
  every row** (#741). Bumping `CURRENT_RULESET_VERSION` (2 is this one)
  rewrites the snippets of every session the worker can still scan. It never
  reaches a session whose transcript has expired or cannot be read, nor any
  session while the checker is off, and those findings are still listed — so
  migration 55 rewrites the old database URL snippets in place, from the
  snippet alone. A payload already stored in `job_history` was masked once and
  is not rewritten. Enforced by
  `migration_55_rewrites_a_database_url_snippet_that_kept_the_password_tail`
  and `a_snippet_stored_under_the_old_ruleset_is_rewritten_by_one_rescan`.
- **It follows `scan`, not the bare rule table.** A paired rule is masked only
  where `scan` credits it (`mask_text_follows_scan_on_a_paired_rule`), because
  masking the AWS secret shape alone would blank every 40-character base64 run.
- **The whitelist plays no part.** It suppresses *reporting* a finding; a
  payload is masked regardless.
- **Its output is safe to store and its input is not.** An event payload is
  stored only after it (#683 is the first caller). A secret shape no rule knows
  is stored raw: the rule table is the single source, and
  `every_rule_has_a_masking_vector` keeps a new rule from landing without a
  masking vector.
