# The Claude settings surface, where the state is files (#304)

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

All nine routes — `GET`/`PUT /api/claude-settings` and the seven profile ones
(list, create, get, update, delete, duplicate, set-default) — moved at once. **The reads could not be left behind, because one of them is a
write**: `GET /api/claude-settings/profiles` runs `ensureDefaultProfileExists`,
which seeds `settings_default.json` from the current `settings.json` and writes
the index. `POST` and `PUT .../{id}/default` do the same; the per-profile
`GET`/`PUT`/`DELETE` and `duplicate` deliberately do not, which is why a `GET` on
an unknown id is a 404 rather than a list that has just been created.

**The cache the issue warned about does not exist.** `ClaudeSettingsProfileService`
holds one field — a logger — and every method calls
`config.LoadProfilesMetadata`, an `os.ReadFile` per call; `appendSettingsOpts`
re-reads it at the moment a run starts. Rather than argue that from the source,
`a_native_write_is_visible_to_the_go_server_immediately` writes the index
underneath a *running* Go server with this port's own encoder and then asks that
server — so the claim is reproduced, not reasoned about. (#305 reached the
opposite conclusion for `PUT /api/settings`, which really does re-apply
process-wide snapshots and trigger a rescan. Different question, different
answer — "does Go cache this?" has to be asked per surface.)

Three things here are silent when wrong:

- **The dir is the run default**, `config.ResolveAgentClaudeDir(nil)` —
  `CLAUDE_CONFIG_DIR`, else the stored global setting, else `~/.claude` —
  resolved from the settings row the way `native/settings.rs` resolves
  everything else, and applied once at the seam so each handler takes a dir.
  `PUT /api/claude-settings` writes the `settings.json` that `--settings`
  resolves against on every run (#242); the wrong dir is not an error, it is a
  run that quietly gets no settings.
- **A named profile keeps the absolute path recorded in the index.** Only the
  unnamed fallback follows the dir. Rebuilding `settings_<id>.json` from the id
  would read a different file — possibly one that exists, with different
  contents — so `detail` uses `file_path` verbatim.
- **Three Go encodings meet, and they are not the same one.** On the wire a
  `json.RawMessage` goes through `compact`: whitespace stripped, `<>&` escaped,
  **key order and number spelling preserved**. On disk everything goes through
  `json.MarshalIndent` over Go's `any`: keys *sorted*, every number a float64,
  two-space indent, `": "` after each key, no trailing newline
  (`gojson::indent_compact`, which is `Indent` — `MarshalIndent` is literally `Marshal`
  then `Indent`, so it decomposes rather than needing a second `Formatter`). And
  a profile created by `POST .../profiles` is neither: it is a **verbatim** byte
  copy of the current default's file.

**`writes::decode_body` is wrong for this area, twice.** It shape-checks through
a `serde_json::Value`, whose parser rejects a number outside float64's range —
which would turn `{"settings":{"n":1e999}}` from Go's **422** into a 400, losing
the one reachable `ValidationError` here — and it requires end of input, while a
`json.Decoder` reads a *stream* and ignores whatever follows the first value.
`claude_settings::decode_request` checks the first token instead: `{` decodes,
`null` is Go's documented no-op zero value, anything else is the type error the
handler turns into its 400. Duplicate keys still forward, for the reason
`decode_body` documents.

Statuses to get right, all verified against a live Go server rather than read off
the service: the **create** handler folds every decode failure and an empty name
into one `400 name is required` (its own check runs before the service, so the
service's 422 is unreachable, and `["x"]` is *not* `invalid JSON body`); the
**update** handler does use `invalid JSON body`; deleting the default profile is
a `409` whose message says *"already exists"*, because it raises a
`ConflictError`; and a rename onto another profile's slug is a 409 while
**create** with the same name silently deduplicates to `-2`.

What forwards rather than being guessed at: a **non-ASCII profile name** (Go
slugifies by Unicode category and then rejects the id it built, unless every
character happened to be dropped — two answers from tables Rust's
`char::is_alphabetic` does not match); a **relative** recorded path
(`filepath.Abs` resolves it against the Go server's working directory, not ours);
a document deeper than serde's 128-level recursion limit but inside Go's 10000
(`json.Valid` is fine — `IgnoredAny` skips iteratively, and the 10000 cap is
checked by hand — but a `Value` decode is not); **bytes that are not UTF-8** (see
below); and everything Go answers with a 500.

**Why a forward is safe here is not "it happens before any mutation" — several
do not.** `create` runs `ensureDefaultProfileExists`, which writes the index,
*before* `slugify` reaches the non-ASCII forward; `put_settings` runs `MkdirAll`
before its undecidable-value forward; `update`, `delete`, `duplicate` and
`set_default` can forward after a profile file has already moved. What makes all
of them safe is that **every step this surface takes before a forward is
idempotent**: seeding no-ops on a non-empty index, `MkdirAll` no-ops on an
existing dir, `deduplicateID` re-derives the same id from the same index,
`moveProfileFile`'s "no file to move" branch tolerates Rust having already moved
it, and every write is a whole-file replace (a temp file renamed over the
target, #668) rather than an append or an increment. Go re-runs the whole
handler and lands on the same state.

That argument is load-bearing, and it is narrower than it looks: **a
non-idempotent step added to this surface breaks the forward, not just itself.**
An append to the index, a counter, a "create only if absent" check with an
error, or a rename that does not tolerate its own output would each make a
forward that follows it a double-apply. The one place the ordering was actually
wrong is `update`, which reached `validatePathWithinDir` only in the closing
`buildProfileDetail` — after the rename had moved the file and the index had been
saved under the new id, so Go looked up the URL's old id and answered 404 where
Go alone would have answered 500. That check is now hoisted to just after the
lookup; the others validate up front already.

**Go's JSON layer is not UTF-8-strict and serde's is.** For `{"a":"x\xffy"}`,
`json.Valid` is true, `Unmarshal` into `any` succeeds with U+FFFD substituted,
`MarshalIndent` writes the replacement character, and the encoder passes a
`json.RawMessage` through byte for byte — all verified against the toolchain.
serde_json splits: `ignore_str` does not validate (so `go_json_valid` agrees)
but `parse_str` does, so every parse that materializes the string fails. Left
unguarded that produced five *wrong answers* rather than five forwards — a 400
where Go writes the file and answers 200, a `settings` key silently dropped from
a 200, and a seeded `settings_default.json` that every later `create` byte-copies.
`claude_settings::is_utf8` is the guard, applied at `decode_stream_head`,
`go_any`, `decode_request` and the two file reads, and all of them forward.
Reproducing Go's answer would mean reproducing where `encoding/json` puts the
replacement character, which is a guess.

**`json.Decoder.Decode` enforces the scanner's `maxNestingDepth` too**, not just
`json.Valid`. A 10001-deep body errors `exceeded max depth` — including when the
depth sits inside a field the struct ignores, which is where it bites: serde
routes an unknown field to `IgnoredAny`, whose skip is iterative and counts
nothing, so `{"name":"x","junk":[×10001]}` decoded here with `name == "x"` and
answered **201** for a request Go refuses with `400 name is required`. The cap is
checked on the body in `decode_request` and `decode_stream_head`.

**A `json.Decoder` and `json.Unmarshal` are different readers, and this surface
uses both.** `PUT /api/claude-settings` decodes a stream and then unmarshals
*what the decode captured*, so `{"a":1} 1e999` is a 200 — `decode_stream_head`
returns the first value's bytes for exactly that reason, and a port that
re-scanned the whole body answered `400 invalid JSON settings`.
`ensureDefaultProfileExists` is the other way round: it calls `json.Unmarshal` on
a whole file, which rejects trailing content, so `{"a":1} junk` seeds `{}` there.
Same bytes, two answers, and the seeding one propagates into every profile
created afterwards.

**The gap #276 left is closed with it.** `native/chat/runner.rs`'s
`settings_file_in` used to return `None` for every non-empty `profile_id`,
because resolving a named profile meant reading `settings_profiles.json` and
that belonged with the profile CRUD. It has landed — `profiles::load` **is**
`config.LoadProfilesMetadata` — so the runner now implements
`LoadProfileFilePathIn` properly: a named id resolves to the path the index
records, an unknown id falls back to the default profile's path, and only then
to `<config dir>/settings.json`. Until then a chat or task pinned to a named
settings profile ran with **no `--settings` at all** in the desktop app while
the Go server passed the recorded path — the same class of silent wrong-account
run #242 existed for. One asymmetry is deliberate and reproduced: Go reads the
index with `LoadProfilesMetadata()`, which resolves the **run default** dir and
not the `dir` argument, so an agent with its own `claude_config_dir` resolves its
named profile against the global index while its *fallback* follows its own dir.

**`GET .../profiles`'s shadow-mode diff proves nothing about seeding.** The proxy
runs Rust first, so Go reads the index Rust just wrote: the two answers agree
because the second call had nothing left to do, not because both would have
seeded the same thing from an empty dir. A wrong `settings_default.json` diffs
clean. It is deliberately *not* in `native::diff_exempt` — that list is for
routes that cannot agree by construction, and these do agree; the agreement is
merely uninformative. The unit tests and the file comparison in the parity suite
are what actually pin seeding. Note too that shadow mode writes into whatever
Claude config dir the developer is running with.

`tests/parity_claude_settings.rs` **refuses to run without `CLAUDE_CONFIG_DIR`**
pointing somewhere other than `~/.claude`. `parity-instance.sh` copies the
database and does nothing for the Claude config dir, and this suite overwrites
`settings.json`. Exporting it before `start` is also what puts both
implementations in one directory — a diff across two would mean nothing.

## Claude Code's own retention, read per config dir (#718)

`GET /api/settings/claude-retention` (`claude_settings/retention.rs`) answers
one entry per **indexed** config dir, default first, from the same resolution
`GET /api/settings/claude-config-dirs` reports as `indexed`
(`settings::indexed_claude_config_dirs`):

```json
{"dirs":[{"config_dir":"/home/u/.claude","cleanup_period_days":30,"source":"default"}]}
```

- `source: "settings"`: `cleanupPeriodDays` is a top-level key of that dir's
  `settings.json` and is a whole, non-negative number. `90.0` and `1e2` count,
  because Claude Code reads the file as JavaScript; `0` is reported as `0`.
- `source: "default"`: the file or the key is absent. `cleanup_period_days` is
  30, the default Claude Code's settings schema states (checked on 2.1.285).
- `source: "unknown"`: the file cannot be read, is not UTF-8, is not valid
  JSON, is not an object, names the key twice, or holds a value that is not a
  whole, non-negative number (a string, a negative, a fraction, `null`).
  `cleanup_period_days` is omitted and `reason` is a phrase the Data pane puts
  after "Could not read Claude Code's retention from `<dir>/settings.json`:".

**It does not reuse `get_settings`, and must not.** That handler reads the run
dir alone and answers 500 for invalid JSON, which is right for the editor. This
route never fails over a file's content, so a broken `settings.json` cannot
turn Settings → Data into an error; the only 500 is an unreadable database.

**The `GET` is a read and only a read.**
`the_route_answers_every_indexed_dir_and_writes_nothing` compares every file's
bytes and modification time before and after, including a default dir that has
no `settings.json` and must not gain one. Writing the key is the `PUT` on the
same path (#720, below), through `claude_settings::patch` (#719).

Two limits, stated rather than solved. Only the user-level file is read, while
Claude Code also takes the key from managed, project-level and
`settings.local.json` files, so the value it uses can differ. And a stored `0`
is not "delete at once": Claude Code 2.1.285 rejects it (`cleanupPeriodDays
must be at least 1`) and skips cleanup until it is fixed, so the Data pane
words `0` as its own sentence instead of "after 0 days".

Neither route has a Go counterpart, so both are recorded in
`parity/desktop_routes.json` through `claude_settings::retention::ROUTES`.

## Raising Claude Code's retention, and never lowering it (#720)

`PUT /api/settings/claude-retention`, `write` scope, body
`{"config_dir": "<absolute path>", "cleanup_period_days": <whole number>}`.
It answers 200 with the `GET`'s document, read again after the write.

**The value may only be raised.** Lowering `cleanupPeriodDays` makes Claude
Code permanently delete every transcript older than the new value, so the
handler refuses anything that would shorten retention, with a 422 and nothing
written. The request is checked first:

- `cleanup_period_days` missing or `null` is `is required`; a fraction, a
  negative, `0`, a string, or anything above 36500 is `must be a whole number
  from 1 to 36500`. Neither is ever decoded as a zero value. `90.0` counts as
  90, as it does on the read.
- `config_dir` must be one of `settings::indexed_claude_config_dirs`, compared
  as stored.

Then `retention::guard`, a pure function over what `read_retention` reports for
that dir:

| on disk | requested | answer |
|---|---|---|
| `source: "unknown"` | any | 422. Agento does not write a file it could not read |
| `0` | any | 422. Claude Code rejects `0` and skips cleanup, so any valid number would *start* deletion |
| `n`, from the file or the default 30 | lower than `n` | 422, `<requested> is lower than the current <n>` |
| `n`, from the file | `n` | 200, nothing written |
| `n` | higher than `n`, or `n` when it is the default | 200, the key written |

An absent file or key therefore counts as 30, and anything under 30 is refused
there. `the_guard_only_ever_lets_the_value_rise` is the table as a test, and
the handler tests assert every file's bytes and modification time after each
refusal.

**The guard is decided on the bytes the write replaces.** Claude Code edits
this file too, so a check made on one read and a write built from another
would leave a window in which a value Claude Code had just raised, or set to
`0`, is overwritten with a lower one. `put` therefore runs the guard twice:
once on `read_retention`, so a file that cannot be read at all is a 422 with
its reason, and again as the `allow` closure of
`patch::set_top_level_key_if`, which is handed the exact bytes the splice is
built from. `patch` then refuses the write (`ChangedUnderneath`) if the file
differs from those bytes just before it is replaced. What remains is `patch`'s
own window between that second read and the rename, stated in its doc and not
solved. `a_value_raised_or_zeroed_before_the_write_is_not_lowered` pins it.

`patch`'s own refusals map as: `ChangedUnderneath` is a 409 with its message;
a dir that is not indexed and the four unreadable-file cases are 422; an I/O
failure is the default 500.

**This is the only route that calls `set_top_level_key_if`**, and outside
tests `patch` has no entry point without an `allow`. Do not add a caller whose
`allow` is not the guard.

The prompt that calls it, and the `claude_retention_prompt_answered` flag on
`user_settings` (migration 49) that records it as answered, are in
`docs/internal/frontend.md`. `PUT /api/settings` keeps that flag true once it
is true (`settings::apply_update`), because the Settings form posts the whole
row and an omitted key decodes to `false`.

## Writing one key of a config dir's `settings.json` (#719)

`claude_settings::patch::set_top_level_key_if(indexed, dir, key, value, allow)`
sets one top-level key and leaves every other byte of the file as it was.
`value` is already-encoded JSON, and `allow` is asked first, with the bytes the
splice is built from (`None` for no file); a `false` writes nothing and answers
`Ok(false)`. `set_top_level_key`, without `allow`, exists for tests only. It
has **no route of its own**: the retention `PUT`
(#720, above) is its only caller and reaches it only through the raise-only
guard, because a general "set one key" route would be a way to lower
`cleanupPeriodDays`, which deletes transcripts.

**It splices bytes; it never decodes the document.** `serde_json` is built
without `preserve_order`, so a `Value` round trip sorts keys, and any decode
respells `1e2` as `100`. `patch::splice` scans the top-level object for byte
spans and builds `prefix + value + suffix`:

- key present: only the value's bytes change;
- key absent: appended after the last member, with the separator (line ending
  and indentation) and the spacing around `:` copied from the last member, so a
  one-line file stays one line and a CRLF file gets a CRLF line;
- key absent from `{}`: the object becomes `{\n  "key": value\n}`;
- no file: created holding only that key, `0600`, in a dir created `0700`.

Key names are decoded to compare them, so `"cleanup\u0050eriodDays"` is the key.
The value must be exactly one JSON value, so it cannot inject members.

**Refusals write nothing**, each a distinct `PatchError`: a `dir` not in
`settings::indexed_claude_config_dirs` (compared as stored), a file that is not
UTF-8, not valid JSON, not an object, or that names the key twice (the cases
`retention` reports as `unknown`). Agento does not repair the user's file.

**Claude Code writes this file too.** The file is read again just before the
write and the write is refused (`ChangedUnderneath`) if the bytes changed. A
small window remains between that read and the rename; it is not solved. The
write is `write_file`, #668's atomic replace, so a failed write leaves the
previous file intact and a symlinked `settings.json` is written through.
`claude_settings/tests_patch.rs` pins all of it over temp dirs.
