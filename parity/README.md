# parity/ — frozen goldens

Every file here was **generated from the Go server** and asserted by the Rust
port. As of #388 the Go tree is deleted, so the generators are gone and these
files are frozen: they are a record of what the Go implementation did, not a
live check against it.

They are still worth asserting. Each one pins behaviour the port reproduces on
purpose and would otherwise drift silently — Go's JSON encoder, Go's `filepath`
and `net/url`, gocron's schedule arithmetic, the 30 migrations, the reflected
tool schemas, the request each integration tool builds. A change to any of that
should be a deliberate edit to the golden with a reason, not a regeneration.

## What "frozen" costs

Three of these were **audits** — they answered a question by re-deriving it
from the Go source, and now they answer it from a snapshot:

| file | the question it answered | what freezing costs |
| --- | --- | --- |
| `read_routes.json` | which GET routes does chi have, and which does the port claim? | a route Go had and the port never claimed can no longer be discovered — the audit ends at the last recorded run |
| `write_routes.json` | same for the 51 non-GET routes, with a `native`/`deferred`/`dropped` disposition each | same; `deferred` entries now name work with no generator to re-check it |
| `migrations_vectors.json` | are the 30 embedded migrations the ones `internal/storage` applies? | migration 31 is now written here by hand. This is the file the schema *is*; it was already the plan of record (see `native/migrate.rs`), so the change is that nothing cross-checks it |

The Rust-side assertions all still run — `native/mod.rs` still fails if a route's
`claims()` disagrees with its recorded disposition, and `migrate.rs` still
applies and verifies. What is gone is the other half telling us the file is
complete.

## One file here is not a Go golden

`desktop_routes.json` (#405) records routes that exist **only** in the desktop
build — `/api/security/*` and `/.well-known/jwks.json`, which no Go router ever
had. It is the exception to the first sentence of this README, and it is
deliberate rather than an oversight.

The alternative was to add those routes to `read_routes.json` and
`write_routes.json`, which would have made both files stop being what they are:
a record of Go's surface, traceable to a `chi.Walk`. The other alternative was
to record them nowhere, which #405's own scoping flagged — those files exist so
the route surface cannot drift silently, and a whole family recorded in neither
would quietly weaken exactly that.

It also carries the property the two frozen ones lost. Their assertion runs in
**one direction**: `native/mod.rs` iterates their rows, so a route that is
claimed and never recorded passes. `desktop_routes.json` is compared for **set
equality** against `native::security::ROUTES`, a single enumerable const that
`claims` also matches against — so a route there cannot be added, removed or
renamed without this file moving. That is what a router walk used to buy,
recovered the only way available without a router.

Anything added here later must do the same: expose its routes as one const, and
assert equality both ways. A hand-maintained list asserted in one direction is a
list that goes stale.

## Regeneration commands, for the record

These no longer work. They are kept so that anyone reading a golden can see how
it was produced, and so a future port that needs a fresh one knows what to build.
All ran against `main`'s Go tree.

| file | generator |
| --- | --- |
| `claude_analytics_golden.json` | `go test ./desktop/parity/ -update-golden` |
| `claude_dirs_vectors.json` | `go test ./desktop/parity/ -run TestClaudeDirsVectors -update-golden` |
| `confluence_vectors.json` | `go test ./desktop/parity/ -run TestConfluenceVectors -update-confluence-vectors` |
| `github_vectors.json` | `go test ./desktop/parity/ -run TestGitHubVectors -update-github-vectors` |
| `gojson_vectors.json` | `gojson_parity_test.go`, shared table |
| `google_vectors.json` | `go test ./desktop/parity/ -run TestGoogleVectors -update-google-vectors` |
| `gopath_vectors.json` | `go test ./desktop/parity/ -run TestGoPathVectors -update-gopath-vectors` |
| `goquote_vectors.json` | `go test ./desktop/parity/ -run TestGoQuoteVectors -update-goquote-vectors` |
| `gourl_vectors.json` | `go test ./desktop/parity/ -run TestGoURLVectors -update-gourl-vectors` |
| `jira_vectors.json` | `go test ./desktop/parity/ -run TestJiraVectors -update-jira-vectors` |
| `jsonschema_reflect_vectors.json` | `go test ./desktop/parity/ -run TestJSONSchemaReflectVectors -update-jsonschema-reflect-vectors` |
| `local_tools_vectors.json` | `go test ./desktop/parity/ -run TestLocalToolsVectors -update-local-tools-vectors` |
| `migrations_vectors.json` | `go test ./internal/storage/ -update-migration-vectors` |
| `notification_template_golden.json` | `go test ./desktop/parity/ -run TestNotificationTemplateGolden -update-notification-template-golden` |
| `oauth_vectors.json` | `go test ./desktop/parity/ -run TestOAuthVectors -update-oauth-vectors` |
| `pricing_catalog_golden.json` | `go test ./desktop/parity/ -update-golden` |
| `pricing_seed_vectors.json` | `go test ./desktop/parity/ -run TestPricingSeed -update-pricing-seed` |
| `desktop_routes.json` | **no generator** — desktop-only routes, hand-written beside the code and asserted in both directions by `native/mod.rs` |
| `read_routes.json` | `go test ./desktop/parity/ -run TestReadRoutes -update-read-routes` |
| `scheduler_vectors.json` | `go test ./internal/scheduler/ -run TestScheduleVectors -update-scheduler-vectors` |
| `session_metric_vectors.json` | hand-written under `internal/claudesessions/testdata/`, asserted by Go and TypeScript |
| `slack_vectors.json` | `go test ./desktop/parity/ -run TestSlackVectors -update-slack-vectors` |
| `telegram_vectors.json` | `go test ./desktop/parity/ -run TestTelegramVectors -update-telegram-vectors` |
| `trigger_match_vectors.json` | `go test ./desktop/parity/ -run TestTriggerMatchVectors -update-trigger-match-vectors` |
| `write_routes.json` | `go test ./desktop/parity/ -run TestWriteRoutes -update-write-routes` |

To recover a generator, read it out of git history on `main`:
`git show main:desktop/parity/github_parity_test.go`.

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
