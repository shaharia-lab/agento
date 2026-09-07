# The insight worker and the search index

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

## The insight worker (#408)

The other writer over the corpus, and the one that did not exist for a long
time: the nine processors were ported as pure functions, the summary endpoint
read `session_insights`, and **nothing wrote a row**. Insights therefore
answered 200 with zeros on every fresh install, which is indistinguishable from
a corpus with nothing in it — the failure that only a count over the real
corpus can see, which is what `tests/insights_live.rs` is (`--ignored`; run it
by hand like `scan_live`).

Three rules, each silent when wrong:

- **Every statement keys on `(session_id, project_path)`.** The Go store keys
  on the id alone in all three — the upsert's `ON CONFLICT`, the join, and the
  `SELECT` — and `insight_worker.go` dedups in-flight work on it too. That is
  the #362 family in its third and fourth form. The upsert is the loud one (the
  conflict target does not exist since migration 29); the *join* is the quiet
  one, because a current row for one project satisfies the other project's
  cache row and that session is reported done forever.
- **The reconcile keys on "no cache row remains", never on a path.** A claim
  shift moves a transcript and is an update, not a deletion (#245), and
  `session_insights` has no `file_path` to get this wrong with. Running it after
  the scan's own delete pass is what inherits the unreadable-config-dir
  protection: those cache rows survive, so their insights are not orphans.
  **The ordering is asserted in `scan.rs`'s own tests since #447**, over a
  two-config-dir fixture corpus under a swapped `HOME`: a removed session loses
  its index row, and a session under a `chmod 0o000` config dir keeps its cache,
  insight *and* index rows. Both `delete_orphans` functions had unit tests
  against a hand-built database, and what those cannot say is *where they are
  called from* — moving `search::delete_orphans` above `apply_changes` finds
  every row still present, deletes nothing, and changes no other observable.
- **A full queue asks for a sweep; it does not drop and wait.** The scan
  announces changed sessions on a 100-slot channel, and a first scan announces
  ten times that. Go warns per dropped item and waits for the next five-minute
  rescan, which reproduces the empty-Insights symptom this issue is named after;
  `SWEEP_REQUESTED` turns the whole overflow into one sweep instead.

The worker owns a thread rather than a tokio task — every step is blocking, the
way `scan::ensure_scan` already is — so `db::blocking` does not apply. Note the
scan's staleness markers deliberately do **not** cover
`CURRENT_PROCESSOR_VERSION`: a processor-only bump must not force a full
re-read of `claude_session_cache`, so the five-minute sweep is the only thing
that notices one.

**`run` is the boot sweep plus `loop { run_once(..) }`** (#447), and where the
two tests of it live is the decision worth knowing:

- **`run_once` is private, and it exists for the *unit* tests.** It returns a
  `Pass` so one deterministic pass is assertable — the `BATCH_SIZE` cutoff, the
  remainder waiting for the next pass, the `SWEEP_REQUESTED` follow-up, and
  `enqueue`'s own path end to end. Before the split, every test called
  `process_batch` directly and none of that was reachable. It takes the timeout
  as a parameter purely so a test does not wait five minutes; `RESCAN_INTERVAL`
  is production's only value for it.
- **`run_once` does *not* make "the worker's database work is off the runtime"
  testable**, and that is the trap the split invites. A test calling it from a
  `tokio::spawn` is still the test choosing where the work runs — the same
  non-falsifiability under a new name. The thing under test is **`start`'s
  `std::thread::spawn`**, so it is driven from `src-tauri/tests/insights_worker.rs`
  through the real `start`/`enqueue`, in #366's harness. That binary may hold
  **exactly one** test calling `start`, because `QUEUE` is a process-wide
  `OnceLock` — a second gets `already started` and silently drives the first
  test's worker against the first test's database.

It is **not** `#[ignore]`d: it needs no corpus, only a tempdir.

**Both version markers live on the row, and that is what makes the sweep short**
(#446, migration 36). `session_insights` carries `processor_version` *and*
`search_index_version`, `store::upsert` writes both in the **same transaction**
as the `session_search` row, and `store::needs_processing` selects on either
being behind. Three consequences, and the first is the whole reason the column
moved:

- **A session the sweep skips stays behind and is retried.** An unreadable
  transcript whose cache row survives — an unmounted drive, a permissions
  change, exactly what the unreadable-config-dir protection preserves — never
  reaches the upsert, so its version does not advance and every later sweep
  picks it up again. Under #435's single `claude_cache_metadata` stamp it was
  invisible to every recovery path at once (its insight row was current, its
  `file_mtime` unchanged, the stamp recorded) and stayed unindexed until its
  transcript next changed.
- **An interrupted rebuild resumes.** The stamp was written only when every
  batch had committed, so a process killed mid-rebuild re-read the whole corpus.
- **A version bump no longer blanks the index.** Nothing calls `delete_all` on
  the rebuild path any more; each row is replaced in place, so the un-rebuilt
  part of the corpus keeps answering.

**The upgrade itself indexes nothing**, and that is migration 36's doing rather
than a property of the column: `DEFAULT 0` alone would make every existing row
read as behind and rebuild the whole corpus once, so the migration carries
`claude_cache_metadata.search_index_version` forward onto every row that has an
index row (found through the freshly backfilled `session_search_key`, so it is a
B-tree join and not 1,178 FTS scans). A row with **no** index row is left at 0 —
inheriting the stamp there would mark a never-indexed session done for ever,
which is the very hole this issue closes.

**What a genuine `SEARCH_INDEX_VERSION` bump now costs is 5.4× what #444's did**
— 35.8 s against 6.66 s over the same 1,178 sessions — because per-session
`replace` is not the same work as `delete_all` plus insert-into-empty, and the
side table removed only the *scan* half of the delete (44.57 ms → 7.79 ms; the
rest is FTS5 rewriting one document's term lists, which nothing can remove). It
buys a bump that is resumable, retries what it skipped, and leaves search
answering throughout. Weigh that trade before bumping.

`search::{stored_version,record_version}`, `store::Scope` and `worker::Write`
are gone with it. `claude_cache_metadata.search_index_version` remains as a
**dead column** — the migrations are append-only — so a `SELECT` on it still
works and means nothing.

What paid for it is `session_search_key(session_id, project_path, rowid_ref)`,
also migration 36: `search::delete` resolves the pair there and deletes on
`rowid`, the one non-`MATCH` constraint FTS5's `xBestIndex` accepts, instead of
scanning the `%_content` table. Three rules around it:

- **Every `search` writer maintains it inside the caller's transaction**, and a
  key row pointing at the wrong docid deletes somebody else's session with
  nothing to report it. `the_key_table_tracks_the_index_through_every_writer`
  and `search_live`'s corpus-scale copy of the same check are the guard.
- **A missing key row is a fallback, not an error** — a database indexed before
  migration 36 can hold one (the backfill's `INSERT OR IGNORE` skips a duplicate
  pair), so `delete` falls back to the old scan-shaped predicate.
- **`delete_orphans` is still keyed on the cache, not on the side table.**
  Driving it off the key table would make an index row with no key entry
  unreachable for ever, and it runs once per scan rather than once per session.

## The search index, measured (`src-tauri/tests/search_live.rs`, #439)

The third `#[ignore]`d live suite over the corpus, and the only one whose output
is as much of the point as its assertions. Run it by hand, like its siblings —
and under `--release` when you care about the numbers, because the `bundled`
SQLite is a C dependency compiled at the profile's optimization level, so a
debug run measures a SQLite nobody ships:

```bash
cargo test --test search_live -- --ignored --nocapture            # correctness
cargo test --release --test search_live -- --ignored --nocapture  # the numbers
```

It copies `~/.agento/agento.db`, **migrates the copy** (the installed database
lags the repository — on the reference machine it had no `session_search` table
at all), forces every row's `session_insights.search_index_version` to 0, drives
the rebuild through `worker::start`, and then measures.

**The terminal condition was the global version stamp and since #446 it is
convergence**, which is worth knowing before touching it. There is no stamp any
more, and "nothing is pending" is *not* the replacement: a real corpus holds
transcripts that cannot be read, those sessions deliberately stay behind for
ever, and a suite waiting for zero would time out on a healthy build. So the
loop watches the pending count fall and stops when it holds still — the failure
the old note warned about (a count sits flat for a whole batch's *read* phase,
so a naive "stopped changing" declares success mid-sweep) is answered twice
over: `SETTLE` is 20 s against a sub-second batch, and the reported build time is
taken at the **last observed change**, so the settle window is not charged to it.

**The reference numbers, release profile, 1,178 sessions** — a baseline for
anything that claims to make this faster or smaller, not a promise:

| | |
|---|---|
| cold full rebuild | **6.7 s** (5.7 ms/session) — **#446 made this 35.8 s**, see above |
| incremental `search::delete` | **45 ms** — this was the scan; **#446 made it 7.8 ms** |
| incremental `search::insert` | 11 ms |
| one session through the worker | 207 ms, transcript read included |
| `delete_orphans`, whole index | 35 ms |
| index on disk | **188.7 MB** (164 KB/session) |
| page query, common word | **33.6 s p50** · facets for the same query, 101 ms |
| page query, rare word (16 hits) | 23 ms |
| the suite itself | ~6 min release, ~25 min debug |

Three things those numbers settled:

- **`search::delete`'s scan was real and it was the incremental path's whole
  cost.** 45 ms against a 188 MB index, against the 9.5 ms/17 MB and 63 ms/176 MB
  already recorded — it tracked index size, as a scan does. **#446 removed it**
  with the `session_search_key` rowid side table, so the row above is the
  pre-#446 baseline; re-measure rather than quoting it. That table is also what
  made per-row rebuilds affordable, since a rebuild is per-session `replace`
  again.
- **`tool_text` is 86.5 % of the stored text** (122.2 MB of 141.3 MB), against
  `assistant_text`'s 10.5 % and `user_text`'s 3.0 % — and it carries the *lowest*
  bm25 weight (0.5). #434's caps were **left alone** anyway: they are per tool
  result and already a quarter of `MESSAGE_CAP`, so what is large is the number
  of tool calls in a Claude Code transcript rather than any one of them, and
  `SESSION_CAP` already binds (the widest session indexes 523,339 of 524,288
  bytes). Lowering the cap would trade recall for size against a budget nobody
  has set, and would need a `SEARCH_INDEX_VERSION` bump to take effect.
- **The expensive half of a search is the ranked join and the snippet, not the
  index.** The same query answers its *facets* — which use the membership `IN`,
  with no `bm25()` carried through and no `snippet()` — in 101 ms while the page
  takes 33.6 s, and a term with 16 hits takes 23 ms either way, so the cost
  tracks the number of hits rather than the corpus. Read that before optimizing
  the index for it.
