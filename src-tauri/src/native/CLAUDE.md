# The backend — the /api registry, the JSON contract, the write path

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

## Layout

```
  native/        ported endpoints (phase 2+)
    active_time.rs the capped-gap rule, shared by the scanner and the pipeline
    scanner/     the Claude session scanner (issue #270) — computes, never writes
      summary_file.rs one transcript → one cache row
      walk.rs      config dirs, project dirs, claim_session, walked vs protected
      diff.rs      insert/update/delete, and why a moved path is not a discovery
      staleness.rs the three markers that force a full re-read
      store.rs     the cache tables' reads and writes
      apply.rs     parallel read, batched write
      cost.rs      per-message pricing
    mod.rs       endpoint registry, mode switch, response shaping
    gojson.rs    Go-compatible JSON encoder — read this before porting anything
    gotime.rs    Go's time.Time on the wire
    db.rs        the SQLite handles: read-only for reads, read-write for writes
    migrate.rs   39 migrations, embedded from parity/ — applied at startup
                 since #278; verify() still guards every write
    pricing_seed.rs the built-in pricing catalog seed, run at startup (#278) —
                 embeds internal/pricing/catalog.json, pinned to
                 parity/pricing_seed_vectors.json
    health.rs    GET /health — one constant, Go's literal bytes (#278)
    writes.rs    what a write may answer, and what it hands back to Go
    chat/        the SSE turn and the three routes that steer it (#276)
      live.rs    the process-local live-session registry — why the four move together
      runner.rs  an agent's config as SDK options; starts the local tools
                 server (#310) and one per github integration (#311), passes
                 an mcps.yaml server through (#375), and refuses only a name
                 that resolves to neither
      mcps_yaml.rs `<data dir>/mcps.yaml` — LoadMCPRegistry, the three
                 transports and the `${ENV:…}` substitution (#375). The one
                 YAML this app reads and the only caller of `serde_norway`
      turn.rs    spawn, stream, and the AskUserQuestion continuation
      persist.rs what a finished turn writes, and what an interrupted one does not
      sse.rs     the frame bytes: raw pass-through vs the two synthetic events
    tools/       the local in-process tools MCP server (#310) — `internal/tools`
      mod.rs     the server name, the qualified `mcp__local-tools__…` names
      current_time.rs  time.LoadLocation and RFC1123/RFC3339, pinned to vectors
    integrations/github/  the GitHub integration's MCP server (#312) — 20 tools
      mod.rs     the server name, the allowed-set union and the service gates
      client.rs  githubAPIBase, the two clients, the three read caps, the four
                 failure sentences — and what a cancelled call answers
      body.rs    json.Marshal over a Go map, and encoding/json's unmarshal errors
      repos.rs / issues.rs / pulls.rs / actions.rs / releases.rs  one per service
    settings.rs  GET+PUT /api/settings and /settings/claude-config-dirs (a
                 filesystem probe; answered on both platforms since #374); the
                 preferences + config dirs a read is scoped to
    claude_settings/ Claude Code's own settings.json and the profiles beside it (#304) —
      mod.rs     the run config dir, Go's `any`/`MarshalIndent`/`Indent`, GET+PUT
                 /api/claude-settings, and the request decoder that is NOT writes::decode_body
      profiles.rs the seven profile routes and the settings_profiles.json index
    monitoring.rs GET /api/monitoring — monitoring.json and the OTEL_* locks, no exporters
    version.rs   GET /api/version and /version/update-check (dev builds only)
    notifications/ the settings read (password masked), /log, the settings
                 write and the test send (#307)
      template.rs  html/template's escaper, and why the skeleton is Go's output
      smtp.rs      go-mail's TLS policy — `ssl_tls` is *STARTTLS*, not SMTPS
    integrations.rs GET /api/integrations, /{id}, /available-tools, /{id}/triggers —
                 auth is a bool made in SQL, and the credentials column is
                 reduced in SQL too — to `has_credentials` (#515) and, through
                 an allowlist, `auth_mode` (#513); plus POST
                 /api/integrations, the trigger-rule writes (#277),
                 PUT/DELETE /{id} (#311) and PUT /{id}/inbound (#566)
    integrations/registry.rs
                 Start/Reload/Stop for the MCP servers of HOSTED_TYPES. The one
                 place a credential is read, behind its own projection
    integration_credentials.rs the seven per-type validators, and the two failures
                 whose Go error text is not reproducible (both forward)
    scan.rs      GET /api/claude-sessions/status, POST /refresh — and the scan
                 itself: the shell owns it (#289)
    fs.rs        GET /api/fs and POST /api/fs/mkdir — the working-dir picker's
                 listing and its one create (both platforms since #374; note
                 mkdir's guard is filepath.IsAbs, where rooted != absolute)
    uploads.rs   POST /api/uploads — the one multipart body, and the extension
                 allowlist that is the route's whole security boundary
    gopath.rs    Go's filepath.Clean/Dir/Join/Base/IsAbs — BOTH rule sets, Unix
                 and Windows (#374), selected by cfg(windows) and both compiled
                 and vector-tested on every host
    gourl.rs     the path chi routes on — Go decodes only canonical escaping
    query.rs     one query parameter, read the way r.URL.Query().Get reads it
    pricing.rs   GET /api/pricing/catalog, plus the rate Resolver — and the
                 three rate writes (#306): add and correct are not one upsert
    agents.rs    GET /api/agents and /api/agents/{slug}
    chats.rs     GET /api/chats and /api/chats/{id}; compact() is Go's, byte for byte
    tasks.rs     GET /api/tasks, /api/job-history and the three reads between
                 them, the five task writes, and POST /api/tasks/{id}/run
                 (#541) — the desktop-only route that fires a task on demand,
                 and this module's whole `ROUTES` const
    gateway_api.rs the LLM gateway's control plane (#426) — fifteen
                 /api/gateway/* routes; under native/ because it IS the /api
                 seam, where gateway/'s listener is not. Two of them are
                 answered by the ASYNC registry, because they call an upstream:
                 the model catalog (#470) and the credential check (#472), which
                 share one dialect table in gateway_api/catalog.rs. The
                 fifteenth is the port probe (#474) — buffered, because a
                 `bind` is not an HTTPS call
    security/    what `/api` accepts as proof of identity (#405)
      keys.rs    the per-install Ed25519 keypair: create-if-absent, 0600, and
                 the one path that replaces it
      token.rs   the JWT format — mint, verify, the claims, and the two scopes
      tokens.rs  the api_tokens rows, and the revoked-jti set the guard reads
      mod.rs     verify_request (guards.rs's one call here), the five /api/security
                 routes, and the unauthenticated JWKS document
    schedule/    when a task fires — pinned to parity/scheduler_vectors.json (#275)
      mod.rs     buildJobDefinition and gocron's four job types; claims no route
      cron.rs    robfig/cron's dialect, which is the one a cron task is written in
    sessions/    GET /api/claude-sessions, /facets, /projects, /{id} and
                 /{id}/journey (#479), plus POST /{id}/continue (#308) and
                 PATCH /{id} (#296)
      continue_chat.rs the two writes that resume a Claude session as a chat,
                 the lookup that makes continue idempotent, and the three
                 `continued_from_*` columns migration 37 adds (#490)
      update.rs  the rename and the favourite — the only two columns here the
                 user typed, and the only ones the scanner never writes
      detail.rs  one session re-read from its transcript, patched from the cache
      journey.rs the turn-segmented timeline (#479) — the one turn predicate,
                 the `tool_use_id` join that nests a sub-agent's steps under the
                 `Task` that spawned it, and the three places it deliberately
                 does more than the deleted Go did. Read its header before
                 touching either; two of them are the shape of a bug elsewhere
      projects.rs the project picker's list, derived from the same walk a scan is
      corpus.rs  loads the lot
      query.rs   the filter, the sort and the cursor — including `add_search`
                 (#436) and the `relevance` arm over negated bm25 (#437)
      page.rs    the page and the facets; the ranked FTS join and the per-page
                 `match_snippet` read hang off `list_page` alone
    analytics/   GET /api/claude-analytics
      buckets.rs Go's time.Date/AddDate and the bucket walks, in the request's tz
      params.rs  from/to/project/tz, and the granularity the window picks
      report.rs  every aggregate in the payload
      cards.rs   the Insights cards
    insights/    GET /api/claude-sessions/insights/summary
      transcript.rs the session JSONL, decoded — the scanner port builds on this
      processors.rs the nine passes that produce a session_insights row
      store.rs     the rows — and the three statements that key on the *pair* (#408)
      worker.rs    the loop that fills the table: boot sweep, 5-minute rescan,
                   and the queue the scan announces changed sessions on
      summary.rs    the aggregate the endpoint answers with

```

## How it behaves today

- **`lib.rs` performs every startup effect** before the window shows: create the
  data dir and database (`db::ensure_database`), apply the migrations
  (`migrate::apply`), seed the pricing catalog (`native/pricing_seed.rs`, pinned
  to `parity/pricing_seed_vectors.json`).
- **A handler's `Err` answers; it does not forward.** A read handler's `Err` and
  a write handler's `WriteError::Fallback` both render as
  `500 {"error":"internal server error"}`, with the reason in the log. Every
  deliberate 4xx answers its own status and body. `Fallback`'s name is a
  leftover from the port; it means "a machinery failure whose original wording
  this build cannot reproduce", not "somebody else will answer".
- **An unclaimed route answers `404 page not found`**, text/plain, nosniff. The
  routes that deliberately answer it are recorded in `parity/read_routes.json`
  and asserted by `native/mod.rs`. (A wrong *method* on an existing path is also
  that 404, rather than a 405.)
- **`/health` is served** (`native/health.rs`) and **`/metrics` is declined** —
  claimed by `monitoring.rs` so it answers the same deliberate 501 as the
  monitoring writes (#309), never a version-mismatch-shaped 404.
- **`GET /api/version/update-check` short-circuits for every build** — offering
  an update there would duplicate the Tauri updater.
- **The three `filepath` surfaces are answered on Windows too** (#374):
  `native/gopath.rs` carries both rule sets, selected by `cfg(windows)`, with
  both compiled and vector-tested on every host. See the root file's *Known
  gaps* for what the Windows CI job does and does not run.

## The endpoint registry

`proxy.rs` decides per request whether the buffered or the streaming half of the
registry answers. Behind it is a **registry**: each module declares its own
`claims` and `serve` as a `native::Endpoint`, and `ENDPOINTS` in
`native/mod.rs` lists them.

Two properties, both load-bearing:

- **Claiming a route and implementing it are one edit.** The pair lives in the
  module it belongs to, so a route cannot end up claimed by a handler that does
  not exist.
- **Adding an endpoint is one appended line** in `ENDPOINTS` plus its own file.
  Nothing in `mod.rs` knows what a module does, and no module knows about
  another.

`no_two_endpoints_claim_the_same_request` guards the one thing a registry can
get wrong that a match statement could not: two modules claiming one path, where
the first listed silently wins and the other's tests keep passing.

**Every `claims` function matches on the path *chi* routes on, not on the raw
request target** (#294). They are different strings, and which one Go uses is a
property of `url.setPath` rather than of the request: `net/http` decodes the
target into a `url.URL` before any handler runs, and `chi`'s `Mux.routeHTTP`
then routes on `RawPath` when it is set and on the decoded `Path` when it is
not — so **Go decodes exactly when the escaping is canonical**.

| request target | chi's segment |
|---|---|
| `/api/agents/a%2Db` | `a%2Db` — `-` needs no escaping, so the encoding does not round-trip |
| `/api/agents/a%20b` | `a b` — a space *must* be escaped, so it does |
| `/api/agents/caf%C3%A9` | `café` |
| `/api/agents/a%2Fb` | `a%2Fb` — which is what keeps a one-segment route one segment |

`native/gourl.rs` is that rule, applied once in `proxy.rs` where `path` is
derived, so no module's `slug_of`/`id_of` has to know about it and none of the
five can drift apart. Both a blanket decode and a blanket raw match are wrong,
in opposite directions, on rows of that table — and canonicality is a property
of the **whole** path, so one non-canonical escape anywhere leaves every segment
raw. A target whose escaping is malformed, or whose escaping is canonical *and*
whose decoded path is not UTF-8, has **no** route path: the first is a 400
`net/http` answers before any handler and the second is a string Rust cannot
carry, so neither has a route path and both are refused. The order of those two
checks is load-bearing — `/api/agents/%ff` decodes to the same unrepresentable
byte as `%FF` but is not canonical, so the raw target is what routes, which is
plain ASCII. `parity/gourl_vectors.json` records what a live router actually
did, not what the rule says it should.

Getting this wrong is not cosmetic on a write: `agents::update` *answers* 404
and `chats::patch` answers `chat not found` for a request that names a real row.

**A handler failure is a clean 500** — the default error body, reason in the
log.

## Adding an endpoint

1. **Read `native/gojson.rs` first.** Rust's natural JSON is *not* Agento's, and
   the differences (`3` vs `3.0`, the escaping of `<`, the encoder's trailing
   newline) are on nearly every response. Encode through `gojson::to_vec`, keep
   struct fields in the order they should appear on the wire, and use
   `skip_serializing_if` to omit empty values.
2. **Ordering and grouping are part of the answer** — including anything
   hashed, since a fingerprint over rows in a different order is a different
   fingerprint for identical data.
3. **Prove it with a golden**: a fixture the code builds, compared against a
   recorded answer in `parity/`. A shared *primitive* rather than a response
   takes the vector form instead — `gopath_vectors.json` pins what
   `filepath.Clean`/`Dir`/`Join` must answer, which is how #268 caught a doubled
   separator. Build the fixture with **no ties on any sort key**: a tie makes
   the expected ordering ambiguous, and an ambiguous golden is a flaky test.
4. **Register it** in `ENDPOINTS`, and record it in `parity/read_routes.json` or
   `write_routes.json`.

Two rules that outlived the harness they came from, and still apply to any
comparison you build:

- **An "identical" over an empty result set is not evidence.** The agents list
  was empty on the reference machine, so the first clean comparison meant
  nothing until two agents existed.
- **Never validate against an installed Agento.** The binary on `:8990` is
  whatever was last installed and drifts behind the repository in both
  directions: a stale baseline fails correct code, and a stale baseline that
  happens to agree hides a real defect.

## Ties, and why the fixtures avoid them

Worth knowing because it explains a shape in the goldens. The original
implementation collected several analytics aggregates into a hash map and then
sorted unstably, so two rows tying on the sort key came out in either order —
its own responses differed run to run. This code collects into a `BTreeMap` and
sorts stably, so a tie breaks on the model or project name and the response is
reproducible.

That is strictly better, and it is why `parity/claude_analytics_golden.json`'s
fixture is built with **no ties on any sort key**: a golden can only record one
ordering, so the fixture must only be able to produce one.

The same reasoning gave `GET /api/integrations/available-tools` its unusual
test: it is compared as a **multiset of byte-exact elements**, each captured as
a `RawValue`, so a reordered key or a respelled number *inside* an element still
fails and only the order *between* elements is exempt. Prefer that shape over
loosening a comparison wholesale — a test that flakes is worse than no test.

## Known encoder divergence

`serde_json`'s float **parser** is not bit-exact by default — `0.36238800000000004`
in a stored JSON column decodes to a different double and re-encodes as
`0.362388`. `Cargo.toml` enables its `float_roundtrip` feature to fix that; do
not remove it. (Rust's own `str::parse` was always correct; only serde_json's
fast path was not.)

`serde_json` turns NaN and infinity into `null`; Go fails the encode outright
(after `writeJSON` has already committed a 200, so the client gets a truncated
body). Nothing read from SQLite can be either — SQLite stores NaN as NULL — but
a *computed* average or ratio can be, so guard the division at the source
rather than expecting the encoder to notice.

## A JSON `null` is a zero value, not a type error

Go's `json.Unmarshal` treats `null` as a no-op for every type in this codebase,
so `{"parentUuid":null}` leaves `""` and returns **no error**. `serde` rejects
it, and the consequences are wildly out of proportion to the cause: a rejected
field fails its struct, a failed struct drops its whole event, and a dropped
event is simply absent from a transcript with nothing to signal it.

`gojson::null_is_zero_value` is the one answer, and every `#[serde(default)]`
scalar in `native/insights/transcript.rs` and `native/notifications.rs` goes
through it. This is not defensive padding — #271 added `uuid`/`parentUuid` to
the transcript decoder and silently lost the **first user message of every
conversation**, because `parentUuid` is `null` on exactly the event that starts
one. The live diff caught it; no unit test would have, since the fixtures were
all written by hand with the field present.

**`serde` only consults the rule where it is attached, so a container needs its
own** (#295). `null_is_zero_value` covers the *field*: `{"ids":null}` was
already `None` while `{"ids":[null]}` stayed a type error, and Go answers `[""]`
to the second with no error at all. `gojson::GoList<T>` is that one level down
and `gojson::GoMap<V>` is the same for a `null` object *value*
(`{"mcp":{"s":null}}` is the zero struct to Go); both keep the outer `Option`, so
the nil-versus-empty distinction is untouched, and a newtype serializes as its
inner value, so no response byte moves. They are on `BulkDeleteRequest.ids`
(`chats.rs`, `tasks.rs`), on all three of `Capabilities`' lists plus its MCP map,
and on `ServiceConfig.tools`, `CreateIntegrationRequest.services` and the trigger
rule's two filter lists. On a *read* this class of bug degrades to a fallback; on
the writes #274 claimed it is a **400 for a request Go applies**.

**They are types rather than `deserialize_with` functions, and that is the whole
lesson of #295.** Functions were the first version. `serde`'s derive makes a
field carrying `deserialize_with` **required** — the `missing_field` path that
lets a bare `Option` default to `None` is not generated — so every call site had
to add `#[serde(default)]`. That attribute also feeds the derive's `visit_seq`
arm, which rejects a short array only for fields with **no** default: adding it
turned `{"capabilities":[]}` and `{"capabilities":{"mcp":{"s":[]}}}` from the 400
Go answers into a created agent. A fix for a `null` would have shipped a widened
**over-accept** — the one direction this port must not move in, because `Err`
means forward and nothing errors when Rust accepts what Go refuses. A type needs
no attribute at all, so the struct stays exactly as strict about its own shape.
Pinned by `a_container_default_would_have_widened_the_struct_from_array_over_accept`.

**`gojson::GoStruct<T>` is the third type, and it closes what those two left**
(#337). serde builds a struct from a **full-length** JSON array, positionally, so
`{"capabilities":[[...],null,null]}` was accepted and Go answers 400.
`writes::decode_body` guards that shape at the *body* level (#274) and nothing
checked it for a value *inside* the body — the one over-**accept** in the port,
where every other decode divergence has been an over-reject. An over-reject is
visible and `Err`-means-forward turns it into Go's own answer; an over-accept
writes a row Go refuses, with nothing to report it.

The accepted set was not uniform, which is what made it hard to see: the derive's
`visit_seq` errors only when the array runs out of elements for a field with
**no** default, so a struct accepted exactly "as many elements as it has fields
without a default" — three for `Capabilities`, one for `McpCapability`, two for
`ServiceConfig`, and **zero** for `SmtpConfig` and `ScheduledTasksPreferences`,
whose every field carries `#[serde(default)]` because `deserialize_with` makes a
field required. `{"provider":[]}` was a saved SMTP configuration.

`deserialize_map` is the whole mechanism — `serde_json` answers it with
`invalid type: sequence` for anything that is not `{`, and the visitor hands the
`MapAccess` to `T`'s own derived impl, so the inner struct's strictness is
untouched. It is a newtype, so it serializes as its inner value and **no response
byte moves**; `null` and "missing" are still decided one level out by `Option`,
and `GoMap<GoStruct<T>>` still maps a `null` value to the zero struct.

**A field cannot protect itself, so the wrapper goes on the holder**:
`AgentRequest.capabilities`, `Capabilities.mcp`'s values,
`{Create,Update}IntegrationRequest.services`' values,
`NotificationSettings.{provider,preferences}` and
`NotificationPreferences.scheduled_tasks`. The stored `capabilities` column is
read through it too, so a row neither implementation can write is refused rather
than read as a real allowlist. `writes::decode_body`'s doc carries the **whole
enumeration of write bodies** and which of them hold a nested struct, because a
partial check reads as coverage it does not have. `a_full_length_positional_array`
is inverted rather than deleted, on both the read side (`agents.rs`) and in
`gojson.rs`.

Genuinely unparseable input still fails, which is also what Go does — so the
null case and the malformed case need separate tests.

## The write path

**A write does more than write a row, and the extra effect is the hard part.**
Task writes register or unregister a cron entry (#275); chat turns spawn a
subprocess and hold in-memory channels (#276); integration writes reload MCP
servers and call Telegram (#277); `/refresh` drives the scanner (#289). Storing
a task without registering it is worse than not storing it at all, so a write
and its effect belong in one place.

Four rules the write path is built on:

- **`db.rs::open_read_write` sets its pragmas per connection.** WAL is
  persistent in the file, but `busy_timeout`, `foreign_keys` and `synchronous`
  are not. Missing `foreign_keys=ON` is the quiet one — `ON DELETE CASCADE`
  stops firing and a deleted chat leaves its messages behind.
- **A write must fail before it mutates.** Validate, check the schema, and do
  the whole mutation in one transaction.
- **`serde` deserializes a struct from a JSON array**, positionally. Without the
  object check in `writes::decode_body`, `POST /api/agents` with a body of
  `["My Agent"]` would create an agent. A `null` body, conversely, is a zero
  value with no error, so it reaches the handler and fails validation with a
  422.
- **Deleting a missing agent or chat answers 500, not 404** — the store returns
  a plain error and the not-found arm never fires. Job history's delete *is* a
  real 404, because its service checks first. Both are inherited behaviour; see
  the known-bugs list under Status.

## The write surface is enumerated, not described (#296)

Accounting for writes **by category** — scheduler, chat execution,
integrations, scan — reads well and cannot be audited: nothing says whether the
categories cover every route, and two once escaped all of them (`POST
/api/fs/mkdir` and `PATCH /api/claude-sessions/{id}`). A prose table has the
same problem one release later, so there is a file instead.

`parity/write_routes.json` records every non-GET route: method, route, `status`
(`native` | `deferred` | `dropped`), the owning issue and a one-line reason.
`native::tests::every_write_route_matches_its_recorded_disposition` asserts each
route's real `claims()` matches its `status`, so a route cannot be claimed or
unclaimed without the file moving. Read that file for the per-route detail.

Two things about it worth knowing. **It covers the root mount too**, not just
`/api`: `POST /webhooks/telegram/{id}` is a write with a large effect — it
matches a trigger rule and dispatches an agent run — and a table scoped to
`/api` would have left it unclassified. And **`dropped` is not `deferred`**: the
WhatsApp routes are waiting for nothing.

Its one weakness is recorded in `parity/README.md`: the assertion runs in one
direction, so a route that is claimed and never recorded still passes.

**The pricing rate writes have an effect outside their own table (#306).** A
rate edit is only rows — but per-session costs are *stored*, so they keep the
pre-edit figure until every transcript is re-read.
`native::scan::after_pricing_change` is that other half. It runs after the
commit and is best-effort by construction.

Three things about that surface that collapse by accident, each recorded at its
site in `native/pricing.rs`: add and correct are **two endpoints, not an
upsert** (and the 409 carries the colliding row, so it is not the bare
`{"error": …}` every other conflict is); `effective_from` is truncated to
seconds or the read-back after the save finds nothing; and `UpsertRate`
**clears the rate's bands**, because `Rate::price` picks a band before applying
any price and a correction that left them would save and then change nothing.

The migrations are **not transcribed by hand**:
`parity/migrations_vectors.json` is embedded by `native/migrate.rs` with
`include_str!` and **is** the schema. Migrations 1–30 were generated; 31 onward
are authored there directly.

## Wire-format traps (found the hard way — do not re-discover)

- **Envelopes, not bare records.** `GET /settings`, `/monitoring` and
  `/version/update-check` answer `{settings, locked, …}`. `locked` maps a field
  name to the *environment variable* that pinned it; a PUT changing a locked
  field is rejected — with a **400**, not the 409 the monitoring path answers,
  because `locked` is not `EnvLockedError`.
- **`GET /chats/{id}` returns an envelope, not a flattened session — and the
  wire order is `{messages, session}`.** The handler writes a `map[string]any`,
  and `encoding/json` sorts map keys, so the order it is spelled in is not the
  order it ships in. This file said `{session, messages}` until the port
  measured it (#264).
- **A `json.RawMessage` re-encodes through Go's `compact`**, which strips
  whitespace outside strings and HTML-escapes, but **preserves the stored key
  order and number spelling**. A tool_use `input` of `{"z":1.50,"a":1}` ships
  exactly that; decoding it into a `serde_json::Value` and re-encoding would
  ship `{"a":1,"z":1.5}` — reordered and respelled, with nothing to signal it.
  `native/gojson.rs::compact` is the byte pass that avoids it.
- **The project filter differs between endpoints.** `/claude-analytics` matches
  `decoded_path`; `/claude-sessions` matches `project_path` literally, which is
  the dash-encoded name for some sessions and a real path for others. Sending
  the wrong one returns an empty result with no error — a silent wrong answer.
- **Go `omitempty` drops zero values** the JSON otherwise implies are always
  present (`InsightCard.percent/count/model`, `ProjectBreakdown.folded_projects`,
  `SessionFacets.config_dirs`). Default with `?? 0`; do not trust the type.
- **`null` vs `[]` is a real distinction, but not the one this file used to
  claim.** The insights summary sends `[]` for every empty `top_*` list, on both
  paths that reach the zero case — `sortedToolCounts` builds with
  `make([]toolCount, 0, len)` and the zero branch returns explicit empty slices,
  so no code path yields a nil one. What *does* send `null` is the **analytics**
  report's zero-valued summary, whose `unknown_pricing_models` is a genuine nil
  slice. Verified against a Go server built from the checkout; a port written to
  the old claim would have been wrong.
- **`summary.total_tokens` is conversation-only** (input + output). Cache read
  is a separate, much larger number — do not add them for a "total".
- Every ranked insights entry keys its label **`tool`**, whatever the list is of.
- **Agent `permission_mode` cannot be persisted**: `AgentRequest` in
  `internal/api/types.go` has no such field, so the REST API silently drops it
  even though `AgentConfig` and the validator both know about it. The service
  also only accepts `""`, `bypass` or `default`, rejecting `plan`/`dontAsk`.
- **`PUT /integrations/{id}` no longer destroys credentials on a scrubbed
  round-trip, and this entry used to say the opposite** (#515). Go's
  `integrationService.Update` preserved `Auth` ("unless the caller provides a
  new one") and did **not** do the same for `Credentials`, which it replaced
  wholesale while `GET` scrubbed them — reproduced live: a working Telegram
  token went from `invalid bot token: … Unauthorized` (reaching Telegram) to
  `credentials are empty` after one such PUT. **An omitted `credentials` key
  now preserves the stored blob**, so a rename or a tool toggle is an ordinary
  save. The UI's old workaround — refusing to save until the credentials were
  re-entered, behind a warning in its own bottom section — is **gone with it**,
  and must not be reintroduced: it existed to defend against this and now only
  demands a token the user cannot read back. A stored secret renders as
  `••• stored` behind an explicit *Replace*, beside **Name** on both the
  connect and the edit screen, and an untouched field sends no `credentials`
  key at all.
- **That preserve rule is `credentials`', and stops there.** `PUT
  /integrations/{id}/triggers/{rule}` is **replace**: an omitted key resets its
  column, including migration 39's five execution settings. See
  `TriggerRuleRequest`'s doc for why (#563).
- **Validation errors are 422**, conflicts 409 — not 400.
- An invalid `sort` on the sessions list is silently accepted (falls back to
  `recent`); only a cursor/sort mismatch 400s.

## The settings write (#305)

`GET /api/settings/claude-config-dirs` and `PUT /api/settings` are both served
here.

The write was implemented in full — validation, the locked-field 400s, the row
write, the rescan rules — and then deliberately *withheld* for a while, because
a second process held a boot-time snapshot of these preferences and would have
rewritten the row from its stale copy on the next unrelated save. That process
is gone, so the route is claimed. No snapshot exists here at all:
`settings::load` reads the row per request, which is why `apply_data_settings`
is three lines.

Three things the write pins:

- **The `PUT` response is the stored row, not a resolution of it.** `Update`
  assigns the incoming struct to `m.settings` wholesale and the handler answers
  `Get()`, so nothing is re-defaulted: a body sending `"default_model":""` is
  answered `""` where the very next `GET` answers `"sonnet"`. `claude_config_dirs`
  comes back `null` for a request that sent `[]`, because
  `normalizeClaudeConfigDirs` collapses an empty list to a nil slice while `Save`
  still writes `[]` — so the column reads back non-nil.
- **A locked field is a 400, not the 409 the monitoring path uses.**
  `SettingsManager` returns plain errors and the handler flattens every one of
  them, so the validation failures are 400s too — not the service layer's 422.
  Field order is Go's slice order (`default_model`, `default_working_dir`,
  `public_url`, `claude_config_dir`), so a body conflicting on two reports the
  first of *those*, not the first in the JSON. A **blank** incoming value is
  never a conflict — the form posts every tab back — it is pinned instead.
- **The scan trigger is `force_scan`, not `ensure_scan`.** Go calls
  `Cache.EnsureScan`, which admits a scan outright; `ensure_scan` is `ensureFresh`
  and asks the staleness markers first. The threshold branch would pass that gate,
  but the config-dir branch would not — no marker records which dirs were walked —
  and a newly added account would sit unindexed until the TTL.

`claude-config-dirs`'s own trap is that `candidates` distinguishes nil from
empty: a home directory that cannot be listed is `null`, one with nothing to
suggest is `[]`. Its rules are pinned against a crafted home rather than the
developer's, because the four exclusions that matter — a symlink to a good
candidate (`os.ReadDir`'s `IsDir` does not follow), a `.claude*` dir with no
`projects`, one whose `projects` is a file, and a plain file — do not exist on a
real machine. `.claude.bak` and `.clauded` *are* candidates when they have a
`projects` dir: the prefix is literal and the `projects` check is the only filter.

## Uploads, and the body cap (#308)

`POST /api/uploads` is the **only multipart route in the API**, and claiming it
cost the seam two small extensions, both in `proxy.rs`:

- `native::Request` carries the `Content-Type`. A multipart body is unparseable
  without the boundary, and the boundary is only in the header. Nothing else
  reads it.
- The body cap is per route. `MAX_NATIVE_BODY` is 8 MiB and stays there — over
  it a request is answered 400, because `to_bytes` has already consumed the
  body — so uploads needed their own, larger limit. It is a second constant
  rather than a raised one because the cost is real: the whole body is buffered
  in memory rather than spilled to temp files.

The multipart reader is hand-written, because every crate in the ecosystem is
built around an async byte *stream* and this handler runs on `spawn_blocking`
with the body already in memory.

Two rules that a casual reading of the filename handling would get wrong:

- **`sanitizeExtension` is an allowlist, and `filepath.Ext(filepath.Base(f))`
  is not `rfind('.')`.** `Ext` stops at a separator, so `evil.png/../x` has no
  extension at all; `Base("")` is `"."` and `Ext(".")` is `"."`, so an empty
  filename yields `"."` rather than `""`. Only `/` is a separator, because this
  is the Unix `filepath` — a Windows-shaped name is one element, which is safe
  only because the alphanumeric check rejects what the backslashes carry.
- **A part named `file` with no filename is not a file.** It is a form *value*,
  so the request is a 400. Matching on the part name alone would accept it.

`POST /api/claude-sessions/{id}/continue` landed with it. It creates the chat
and records the Claude session id on it **in one transaction**, rather than as a
create followed by an update — a failure between the two would otherwise leave
an orphan chat behind.

## Monitoring is not implemented, on purpose

OpenTelemetry, Prometheus metrics and a self-updater of Agento's own are all
deliberately absent. Tauri's updater replaces the last; the first two are
infrastructure concerns and this is a local desktop app.

OpenTelemetry, Prometheus metrics and a self-updater of Agento's own are all
deliberately absent. Tauri's updater replaces the last; the first two are
infrastructure concerns and this is a local desktop app.

**That is an answer, not an omission (#309).** `PUT /api/monitoring` and
`POST /api/monitoring/test` are claimed and answer **501**, and the Monitoring
section is **read-only**. The two alternatives are worse:

- **Persisting the config without exporters is a save that changes nothing.**
  Writing `monitoring.json` is half the job; rebuilding the providers is the
  other half, and there are no providers. A 200 would tell the user telemetry
  is on while nothing is emitted.
- **Implementing the exporters** is a large piece of work that reverses the
  decision above.

`501` rather than `404` because the route exists and this build declines it; a
404 reads as a version mismatch and sends someone hunting an upgrade that will
never ship — the same reasoning `unavailableCopy` encodes for WhatsApp.

`GET /api/monitoring` stays and the section still renders it, because what it
reports is still true: the file is read at startup, and `locked` names the
`OTEL_*` variables pinning a field — which is what someone debugging a missing
trace comes here to read.

## WhatsApp is dropped, not deferred (#273)

`whatsmeow` has no Rust equivalent and will not be reimplemented. This is a
decision, not a backlog item. A session that finds itself scoping WhatsApp work
has misread this section.

What that means in the code, and the part that is easy to get wrong:

- The UI offers no WhatsApp entry, pairing flow or QR screen. The picker is
  `PROVIDERS` in `src/views/integrations/catalog.ts` — a hardcoded list, so
  **that list is what decides it**, not anything the API returns.
- **An existing row is data and must survive.** Someone who paired under an
  older version has an `integrations` row of that type. `type` is a free-form
  `String` everywhere — no enum, no match — so it lists, opens and reads
  normally; `providerFor` returning `undefined` is a supported answer, and
  `unavailableCopy` beside it turns that into an honest "not available" rather
  than the "newer version of Agento" line, which would send that user hunting
  for an upgrade that will never ship. Both live in `catalog.ts`: what types
  this app knows, and what to say about the ones it does not, are one question.
  The row cannot be removed or edited — those controls only render for a known
  provider — so do not describe it as deletable.
- **Do not filter it out of `available-tools`.** That handler never looks at
  `type`, and suppressing the row would be a wire divergence on an endpoint
  whose bar is byte-identical JSON. An agent whose allowlist names WhatsApp
  tools keeps those entries; they simply do not resolve.
- `GET /api/integrations/{id}/whatsapp/*` is unclaimed and answers the
  unrouted 404. Nothing calls it.
