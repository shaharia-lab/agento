# parity/ — the frozen wire-format specification

These files **are** Agento's wire format. Every one of them pins behaviour that
would otherwise drift silently: the JSON encoder's key order and float spelling,
`filepath` and URL escaping, schedule arithmetic, the migrations, the reflected
tool schemas, and the exact request each integration tool builds.

CI asserts all of them on every run, and nothing regenerates them.

**A change to one of these files is a change to the contract.** Make it as a
deliberate edit with a reason you can state in the commit message — never by
re-recording until the tests go green. A golden refreshed to match new behaviour
does not prove the new behaviour is right; it only stops anything from telling
you it changed.

## What "frozen" costs

Three of these were once **audits**: they answered a question by re-deriving it
from a second implementation, and now they answer it from a snapshot. The
assertions still run; what is gone is the half that confirmed the file was
*complete*.

| file | the question it answers | what freezing costs |
| --- | --- | --- |
| `read_routes.json` | which GET routes exist, and which does the backend claim? | a route that exists and is never claimed can no longer be discovered — the audit ends at the last recorded run |
| `write_routes.json` | the same for the non-GET routes, each with a `native`/`deferred`/`dropped` disposition | the same; `deferred` entries name work with nothing left to re-check it |
| `migrations_vectors.json` | are the embedded migrations the ones actually applied? | this file **is** the schema now, and migrations are appended to it by hand |

Both route files are asserted in **one direction**: `native/mod.rs` iterates
their rows, so a route that is claimed and never recorded still passes.

## `desktop_routes.json` does not have that weakness

It records the routes added by the security surface (#405) — `/api/security/*`
and `/.well-known/jwks.json` — and it is compared for **set equality** against
`native::security::ROUTES`, a single enumerable const that `claims` also matches
against. So a route there cannot be added, removed or renamed without this file
moving.

That is the property to copy. Anything added here later must do the same: expose
its routes as one const, and assert equality both ways. A hand-maintained list
asserted in one direction is a list that goes stale.

## `gopath_windows_vectors.json` has a generator, and it is the only one that does

Everything else here was recorded from the Go server. Go's Windows
`path/filepath` is not that server — it is the *standard library*, and it still
exists — so `gopath_windows_vectors.json` (#374) is regenerable, and it is the
one file in this directory you may re-record without changing the contract:

```bash
go run parity/generators/gopath_windows_vectors.go > parity/gopath_windows_vectors.json
```

`parity/generators/gopath_windows_vectors.go` needs no `go.mod` and no network.
Two things about it are deliberate:

- **It vendors Go's Windows source; it does not re-implement it.** The
  `lazybuf`, `Clean`, `Base`, `Dir`, `VolumeName`, `IsAbs`, `volumeNameLen`,
  `postClean` and `Join` bodies are copied verbatim from
  `internal/filepathlite/path.go`, `internal/filepathlite/path_windows.go` and
  `path/filepath/path_windows.go`, with only the build constraints and two
  `internal/` helpers replaced. That is what makes it a *second opinion* about
  the Rust port rather than a restatement of it — the lesson #268 learned when
  `filepath.Clean` transcribed by eye produced `/a//c` for `/a/b/../c/`. A
  generator that shares the port's belief agrees with a wrong port.
- **The inputs are Go's own test tables**, not invented ones: `cleantests` +
  `wincleantests`, `basetests` + `winbasetests`, `dirtests` + `windirtests`,
  `jointests` + `winjointests`, `isabstests` + `winisabstests` from
  `path/filepath/path_test.go`, plus the paths Agento's own Windows surfaces
  build. The vendored code reproduces every expectation in those tables exactly,
  which is the check that says the vendoring is faithful.

Regenerating it against a newer Go is a **contract change** like any other here:
state the Go version and what moved. The header records the version it was
produced with.

It also carries the one array that has no home elsewhere: `unix_base` pins
`filepath.Base` under the **Unix** rules, produced by the generator host's real
`path/filepath`. `Base` is new to the port (`sessions/projects.rs` needs it) and
`gopath_vectors.json` is frozen, so it lives here rather than growing that file.

## Recovering a generator

The other generators were deleted in #391 (`07b6212`) along with the
implementation they ran against. If you ever need to see how one of those files
was produced, they are in history:

```bash
git show 07b6212^:desktop/parity/
git show 07b6212^:desktop/parity/github_parity_test.go
```

Five files never had a generator and are hand-written beside the code:
`desktop_routes.json`, `session_metric_vectors.json`,
`claude_sessions_search_golden.json`, `session_detail_blocks_golden.json` and
`journey_golden.json`.

The last three were all authored *after* the Go tree was deleted, because none
has a Go ancestor left to record from. `claude_sessions_search_golden.json`
pins one search response — `match_snippet` and the `relevance` sort (#437) are
Agento's own — recording where the field sits, that it is omitted where there is
no index hit, and the ranked order. Two things it deliberately does **not** pin:
SQLite's bm25 *values*, which is why the fixture's page is exhausted and mints
no cursor, and which column `snippet()` picks out of a tie.

`session_detail_blocks_golden.json` pins the rendered `messages` array of one
fixture transcript through `sessions::detail::read_detail` (#482). A
`tool_result` block reached no client before it, so the arm that would have
emitted it never existed on either side. It records the block's position in the
wire object — `is_error` **last**, after `input` — that a *successful* result
carries no `is_error` key at all, that a `tool_use` input still ships with its
own key order and number spelling, and that a tool-result carrier is a user
message with `blocks` and no `content`.

`journey_golden.json` pins the whole `GET /api/claude-sessions/{id}/journey`
response of one fixture transcript (#479). The route's Go implementation was
deleted at the cut-over, so this records what the *port* answers rather than
what Go did — and the port deliberately differs from it in three places, each
argued in `sessions/journey.rs`'s header. What it pins: the key order of every
object on the wire, that a `tool_call`'s `input` reaches it with the
transcript's own key order and number spelling (`{"z":1.50,"cmd":"make"}` —
neither sorted nor respelled), Go's `\u003c`/`\u0026` escaping *inside* a step
payload, that a zero `duration_ms` is absent from a step and present on a turn,
that a successful `tool_result` still carries `is_error`, and that a nested
sub-agent's steps sit under the call that spawned it rather than beside it. Its
fixture has no ties on any sort key, so only one ordering can be produced.

## Who reads them

Two mechanisms, and the difference matters when files move:

- **`include_str!` with a relative path** — `gojson.rs`, `gotime.rs`,
  `goquote.rs`, `migrate.rs`, `pricing.rs`, `pricing_seed.rs`,
  `trigger/match_rule.rs`, `schedule/tests_vectors.rs`,
  `integrations/oauth/mod.rs`, `notifications/template.rs`,
  `analytics/tests_golden.rs`, `sessions/tests_db.rs`,
  `sessions/tests_search.rs`. These count directory levels and break if either
  tree moves.
- **`concat!(env!("CARGO_MANIFEST_DIR"), "/../parity/…")`** — `settings.rs`,
  `gopath.rs`, `gourl.rs`, `mod.rs`, and each integration's `tests_vectors.rs`.
  Anchored to the crate, so they survive a move.
