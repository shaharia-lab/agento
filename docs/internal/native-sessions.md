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
- **Cache invalidation is multi-dimensional**: TTL (1h), `scanner_version`,
  pricing revision fingerprint, and idle-threshold drift each force a re-read.

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
