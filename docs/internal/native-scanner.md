# The session scanner, and the scan that drives it

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

## The session scanner

The walk, the diff, the staleness rules, the transcript→row reader and the
parallel-read/batched-write apply. The scan itself is owned by the app (#289);
see *The scan* below.

**Correctness here is the stored rows, not a response.** Every
`claude_session_cache` and `claude_subagent_cache` row is recomputed from its
own transcript and compared field by field. The row records the mtime it was
read at, so "has this file grown since" is exact; rows that moved on are
skipped, because every figure would read as "computed is larger", which is also
what an over-counting bug looks like.

Four rules that are silent when wrong:

- **"No file on disk" and "we could not look" are different answers.** A
  config dir that failed to list is left out of `walked` and its rows are
  excluded from the delete pass; an unmounted drive would otherwise wipe an
  account's corpus, `custom_title` and `is_favorite` included. A dir that
  exists but has no `projects/` is the case that looks like a failure and
  is not. Protection is per project, not per config dir.
- **A moved path is an update, not a discovery** (#245). Rows key on
  `(session_id, project_path)` while `file_path` is a non-unique index, so
  a claim shift legitimately brings the same row under a new path. The diff
  indexes the cache twice — by path and by row key — to tell them apart.
- **`custom_title` and `is_favorite` are in neither write list.** They are
  the only columns here the user typed.
- **Three markers force a full re-read with nothing changed on disk**:
  scanner version, pricing revision, idle threshold. The last cannot be a
  version constant but makes the same rows stale. Invalidation zeroes
  mtimes rather than dropping rows, so a re-read is an update.

Two encodings to get wrong: `cost_by_model` is JSON but empty stores as
`""`, and `unpriced_models` is newline-joined rather than JSON, because a
model id may contain a slash but never a newline.

`transcript.rs` and `native/active_time.rs` are shared with the insight
pipeline deliberately — `is_user_turn_content` decides `message_count`,
`turn_count` and the journey's turns at once, and the same session's active
duration is stored in two tables under a user-configurable threshold.

## The scan (#289)

The app owns the session scan outright. `lib.rs` starts the boot scan, and
`native/scan.rs` owns admission (one scan at a time), progress, the staleness
markers and the two endpoints — `GET /api/claude-sessions/status` and
`POST /api/claude-sessions/refresh`.

**Reading is what keeps the corpus fresh**, and that is the rule to preserve.
The scan is not only a background job: every corpus read calls
`scan::ensure_scan` on its way past, which starts a rescan when the TTL expires,
the pricing catalog moves, or the idle threshold changes. A read path that
skipped it would silently stop transcripts being re-read, and a rate edit would
never reach stored costs.

**Verify a change here against the real corpus, not a fixture.** The failure that
matters is a scan that runs, reports success and writes nothing, and a
three-file fixture cannot tell that from a healthy one.
`tests/scan_live.rs` copies the real database, forces a full re-read and asserts
the row counts do not shrink and the markers are recorded. It is `#[ignore]`d
(CI has no corpus), so run it by hand:

```bash
cargo test --test scan_live -- --ignored --nocapture
```
