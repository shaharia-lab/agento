# Sessions, analytics and the corpus reads — things that will bite

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

- **Cost is stored, not derived.** `claude_session_cache` holds per-session cost
  computed at scan time against the rate in effect at that message's timestamp.
  Analytics sums stored values and never re-prices. Recomputing at read time
  would make the list and the dashboard disagree.
- **Cache-hit rate** is `cacheRead / (input + cacheRead + cacheCreation)` — the
  read share of *every* input-side token, so a non-caching model scores 0
  rather than being excluded from its own denominator.
- **Unpriced models are disclosed, not zeroed.** Unknown models accumulate into
  `unknown_pricing_tokens` / `unpriced_models`; the total is a floor. Never
  silently price them at $0.
- **Session list totals include sub-agents** (`usage + subagent_usage`,
  `cost + subagent_cost`). The facets bar and the rows must agree.
- **The journey's `active_duration_ms` and the sessions list's are the same
  formula over different stamp sets, and neither dominates the other** (#479).
  Both are `Σ min(gap, idle_cap)` over consecutive stamps. The scanner caps each
  transcript on its own and the list adds the results, so `active +
  subagent_active` counts a wall-clock minute once per transcript running in it
  — and delegation is concurrent, so that total can exceed the session's own
  span. The journey takes one sum over every stamp, so it counts that minute
  once. **It is not a union of intervals**, which is the inference to resist:
  absorbing a stamp inside a gap already longer than the cap replaces one capped
  gap with two, so the merged figure can come out *above* the sum — a sidecar
  with a single logged event is enough. So the ordering is observed, never
  asserted (`tests/journey_live.rs` bounds only the sound side). Both figures
  are right about different questions; the view labels which it shows. Do not
  "reconcile" them by changing which stamps reach a tracker.
- **A journey's `active_duration_ms` can exceed its `total_duration_ms`**
  (#479). The time range is widened only by the events the parent walk sees,
  while the active tracker also absorbs every sub-agent's stamps — Go's split,
  and moving `start_time`/`end_time` would change the wire. A delegated
  transcript stamped past the parent's last event therefore lands outside the
  span it is reported beside.
- **A journey can render one more turn than Insights counts, and only ever
  one** (#479). `is_user_turn_content` decides both, so they cannot disagree
  about any single event — but the journey's `ensure_turn` opens a leading turn
  for the events *before* the first genuine prompt, because those steps have to
  live somewhere, while the pipeline counts no turn for them. Every session a
  slash command opened is that shape: the expansion and the skill preamble are
  both injected wrappers and the model answers before the person types. The
  extra turn always has no `user_input` step, which is what
  `tests/journey_live.rs` asserts instead of a tolerance.
- **Time bucketing happens in the request's timezone** (`tz` param), while
  storage stays UTC. Always send `tz`; omitting it falls the dashboard back to
  UTC silently.
- **Session pagination is keyset, not offset.** The cursor encodes the sort, so
  changing `sort` invalidates it — reset the cursor or get a 400. Its tiebreak
  is the whole row key, `(session_id, project_path)`: `session_id` alone is not
  unique, and on the id alone two rows sharing an id *and* a sort value are one
  position, so the second is skipped by every page while `facets` still counts
  it (#364). A cursor minted before the `p` field decodes with it empty and
  pages exactly as it used to, which is Go's missing-field behaviour and why the
  Rust field carries `#[serde(default)]`.
- **`sort=relevance` is the default when `q` is set, and bm25 reaches it
  negated** (#437). `q` with no explicit `sort` resolves to `relevance`; an
  explicit one always wins; `relevance` with no `q` is the unknown-sort fallback
  to `recent`, because there is no `MATCH` to rank. The sort key is
  `COALESCE(-fts.rank, -1.0)` and both halves are load-bearing: SQLite's `bm25()`
  is *negative* — smaller is better — so negating it is what lets "best first"
  stay `DESC` like every other sort, and the `COALESCE` is what keeps the LIKE
  half pageable. A search is an `OR`, so a row can match on its id, its path or a
  title with no index hit at all; left as SQL NULL those rows sort last correctly
  on the **first** page and then vanish, because `NULL < ?` is NULL and the
  keyset predicate drops them. The sentinel is safely below every real value
  because `-bm25` is never negative. The cursor needed no new field — the rank
  goes in `v`, which already carries a decimal for every non-time sort.
- **`match_snippet` is present only on a search response, and only where the
  index matched.** `skip_serializing_if` on an empty string is what kept every
  frozen golden byte-identical; a metadata-only match honestly carries nothing.
  Its markers are **U+0001/U+0002**, unambiguous because `search::normalize`
  strips every control character from the indexed text — never HTML, and never a
  printable sentinel a transcript could contain.
- **Expiry is two trailing, omitted fields and one tri-state filter** (#708).
  A row the scanner stamped `transcript_expired_at` (#705) carries
  `transcript_expired: true` and `transcript_expired_at` as the **last** keys of
  its `SessionSummary`; a live row carries neither, which is what kept every
  frozen golden byte-identical, and `parity/claude_sessions_expired_golden.json`
  pins the expired spelling. `transcript` on the list and the facets is `""`
  (any), `available` or `expired`, and anything else is a 400, exactly like
  `links`. The counts follow the same rule: `SessionFacets.expired_sessions`,
  `AnalyticsSummary.expired_sessions` and `SessionRanking.transcript_expired`
  are last and omitted at zero or false. Analytics totals **include** expired
  rows, because `corpus.rs` reads every cache row. The column is index 54 of
  `SUMMARY_COLUMNS`, so the relevance key `page.rs` appends is index **55**; a
  stale index fails only on `sort=relevance`.
- **A read that needs the file answers 410 for an expired transcript, 404
  otherwise** (#709). `GET /api/claude-sessions/{id}`, its `/journey` and
  `POST …/continue` answer `410 Gone` when **no config dir holds the
  transcript and the cache row is stamped**; an id with no row, and an unstamped
  row whose file is missing, stay `404 session not found`, because the scanner
  is the only thing that decides expiry. A file on disk wins over a stamp. The
  body is `{"error":"transcript expired","transcript_expired":true,"expired_at":…}`
  in that order — `error` first so every existing error reader still finds a
  message — pinned by `parity/session_expired_golden.json`. `detail::expiry` is
  keyed on the file being absent, not on the reader's `None`, since the journey
  also answers `None` for a file with no timestamped event. `continue` answers
  it before opening the database for writing. Enforced by
  `sessions/tests_gone.rs`. `api.ts` retries a 401 only, so a 410 is one
  request; `isTranscriptExpired` narrows the `ApiError`.
- **Only an expired session can be deleted, and a delete removes everything
  stored about it** (#711). `DELETE /api/claude-sessions/{id}` answers `204`,
  `404 session not found` when no cache row has the id, and `409` when a
  transcript for it is on disk or any row with that id is not stamped — decided
  by the same `detail::expiry` the 410 reads use, plus the row's stored
  `file_path`, and all-or-nothing over every pair carrying the id.
  `DELETE /api/claude-sessions` with `{"before":"<RFC 3339>"}` answers
  `200 {"deleted":N}` (`parity/session_delete_golden.json`) for every expired
  pair with `last_activity` **strictly** before the bound whose file is not on
  disk; a missing, `null`, empty or unparseable `before` is a 422, and there is
  no delete-everything form. The bulk candidates come from the list's own
  `build_filter` with `transcript=expired` and the new `ended_before`
  parameter (`c.last_activity < ?`, which also applies under a drill-down), so
  `GET …/facets?transcript=expired&ended_before=T` counts exactly what the bulk
  delete removes — and a hidden project's or an un-indexed config dir's
  sessions are in neither. `delete::delete_pairs` is the one cascade, on the
  caller's transaction: the pair's findings, scan state, insights, search rows
  and cache row, then the id-keyed sub-agent and PR rows **only once no cache
  row carries the id**. It deletes a cache row only while it is still stamped,
  so a pair a scan un-expired in between is skipped whole. **The cascade has a
  third caller that is not a route** (#712): `delete::prune`, which the scan
  runs for the stored `session_history_retention_days`, in `Sweep` mode. It
  selects on the stamp and `last_activity` alone, so unlike the bulk delete it
  is **not** filtered by hidden projects or indexed config dirs; see
  `docs/internal/native-scanner.md` for its place in the scan. The transcript is
  never touched. Both routes are desktop-only and recorded in
  `parity/desktop_routes.json` through `sessions::ROUTES`. Enforced by
  `sessions/tests_delete.rs`.
- **Cache invalidation is multi-dimensional**: TTL (1h), `scanner_version`,
  pricing revision fingerprint, and idle-threshold drift each force a re-read.
- **Session export is a Tauri command, not an `/api` route, and it reads the
  transcript rather than the detail** (#591). `export_session` writes Markdown,
  JSONL or text straight to the path the native Save-As dialog returned, the
  `export_logs` shape. `GET /api/claude-sessions/{id}` is lossy on purpose —
  no `system` events, no image blocks, tool results capped at 2000 characters —
  so `native/sessions/export.rs` walks the JSONL itself and uses the detail read
  only for the metadata header. An unfiltered JSONL line is written byte for
  byte and a filtered one is re-assembled in key order; binary content is never
  inlined in Markdown or text, it is decoded into `<stem>-attachments/`. What
  each toggle governs is that module's `//!` doc. The command rejects with a
  typed `ExportError` (#710) whose `kind` is `transcript_expired` (the file is
  gone and the row is stamped, decided by the same `detail::expiry` the 410
  routes use, with `expired_at`), `not_found`, or `failed` for everything else;
  `lib/sessionExport.ts` rethrows it as a `SessionExportError`.

## Three read-path rules that make a wrong implementation look right

- **`limit=0` means fifty.** The query parser rejects only negative and
  unparsable values; the service *then* maps `limit <= 0` to the default. So a
  literal zero asks for nothing and receives a full page. The 500 cap clamps
  `offset` too, since both share the parser.
- **An unknown task's job history is `200 []`, not a 404.** Nothing checks the
  task exists before listing its runs. Only `/api/tasks/{id}` and
  `/api/job-history/{id}` 404.
- **`GET /api/claude-analytics` is deliberately not memoized.** A rebuild is a
  full corpus load and a dozen walks over it, fired two or three times per
  dashboard open — but measured, that is ~50 ms. A cache would be a second
  thing to invalidate correctly, and nothing about the response depends on one.
