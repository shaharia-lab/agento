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
it, and every write is a whole-file truncate rather than an append or an
increment. Go re-runs the whole handler and lands on the same state.

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
