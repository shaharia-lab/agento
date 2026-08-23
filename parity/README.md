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

## Recovering a generator

The generators were deleted in #391 (`07b6212`) along with the implementation
they ran against. If you ever need to see how one of these files was produced,
they are in history:

```bash
git show 07b6212^:desktop/parity/
git show 07b6212^:desktop/parity/github_parity_test.go
```

Two files never had a generator and are hand-written beside the code:
`desktop_routes.json` and `session_metric_vectors.json`.

## Who reads them

Two mechanisms, and the difference matters when files move:

- **`include_str!` with a relative path** — `gojson.rs`, `gotime.rs`,
  `goquote.rs`, `migrate.rs`, `pricing.rs`, `pricing_seed.rs`,
  `trigger/match_rule.rs`, `schedule/tests_vectors.rs`,
  `integrations/oauth/mod.rs`, `notifications/template.rs`,
  `analytics/tests_golden.rs`, `sessions/tests_db.rs`. These count directory
  levels and break if either tree moves.
- **`concat!(env!("CARGO_MANIFEST_DIR"), "/../parity/…")`** — `settings.rs`,
  `gopath.rs`, `gourl.rs`, `mod.rs`, and each integration's `tests_vectors.rs`.
  Anchored to the crate, so they survive a move.
