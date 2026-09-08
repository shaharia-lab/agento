# The LLM gateway — engine, control plane, usage

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

## Layout

```
  gateway/       the embedded LLM gateway (#421) — beside native/, not inside it:
                 it is a second listener speaking OpenAI's and Anthropic's wire
                 formats, so no /api seam and no parity machinery applies
    config.rs    the settings model, its three tables and the mapping onto
                 ferrox-providers' own config types (#422). Two projections:
                 the public one never selects api_key
    registry.rs  the listener's lifecycle (#424) — integrations/registry.rs's
                 shape, and the *stored* Status a bind failure leaves behind
    server.rs    the five routes, the Host allowlist, the llm-scope auth layer
                 (both header spellings), and the per-surface error dialect
    dispatch.rs  alias → ordered targets, retry, and the fallback walk;
                 is_retryable/should_failover copied from ferrox/src/retry.rs
    stream.rs    the SSE bytes of both surfaces, and the anthropic-beta merge
    usage.rs     one row per served request (#425) — the accumulator a stream
                 reports through, and the cost resolved at write time; plus the
                 retention prune (#428) and the once-a-day gate that rides the
                 write rather than owning a timer
  native/gateway_api.rs  the control plane (/api/gateway/*) — under native/ because it IS the /api seam
```

## The LLM gateway's engine (#424)

The second listener. It is **not** `proxy.rs` and **not** part of the `/api`
seam: its own user-configured port, two third-party wire formats, no parity
machinery, and — since every request it accepts spends the user's provider
credits — a different threat model from anything else in the shell.

`ferrox-providers` supplies every translation and every adapter. What is here is
the listener, the auth, the routing table and the framing:
`registry.rs` (lifecycle), `server.rs` (five routes, two layers),
`dispatch.rs` (alias → targets, retry, failover), `stream.rs` (SSE bytes).
It is **disabled by default** and costs one `SELECT` at boot when off.

**Three rules that keep a permissive listener from being one:**

- **`Scope::Llm`, verified with `token::verify_against`** — the pure
  four-argument function, **not** `security::verify_request`, which derives a
  required scope from an `/api` method and path. A `read` or `write` token is a
  **403** here and an `llm` token is a 403 on `/api`: that disjointness (#423) is
  the whole reason a third scope exists, and the *false* cells are what the tests
  assert.
- **No CORS layer, ever.** ferrox's own server mounts `CorsLayer::permissive()`,
  which is right behind a network boundary and catastrophic on loopback — it
  would let any page the user has open spend their provider credits *and read
  the response*. The **`Host` allowlist** is the other half: preflight already
  shuts a browser out of a `POST` carrying `Authorization`, but a DNS-rebinding
  page reaches a loopback port with a *simple* request. `guards.rs::host_allowed`
  is `pub(crate)` and shared rather than copied — an allowlist that exists twice
  is one that gets widened once. `/healthz` is outside the auth layer and
  **inside** this one.
- **Errors in the client's dialect, never Agento's.** `error::openai_error_body`
  on `/v1/*`, `error::anthropic_error_body` on `/anthropic/*`, picked by path
  prefix so a route added under `/anthropic/` cannot forget. SDKs *branch* on
  `error.type` — `authentication_error` versus `permission_error` drives real
  retry behaviour — so this is behaviour, not wording.

**Both header spellings.** OpenAI SDKs send `Authorization: Bearer`; the
Anthropic SDK and Claude Code send `x-api-key`. Bearer wins a tie, and a useless
`Authorization` (`Basic …`, an empty `Bearer `) must fall *through* to
`x-api-key` rather than shadow it with `""`.

**A bind failure is a stored value, not a log line.** `registry::Status` has a
`BindFailed { port, error }` variant and is stored rather than derived from
whether a handle exists, because that is what #426's status route reads. The
collision is routine rather than exotic — a `~/.agento-desktop-dev` instance and
an installed one read different databases but share the machine's ports — and
without a stored status the second reports "not running" and offers a Start
button that silently does nothing forever. It logs once at `warn` and does
**not** retry another port: a gateway on a port the user did not configure is one
every tool they set up is pointed away from.

**Graceful shutdown comes from `claude/mcp.rs`, not from `proxy.rs`.** `proxy.rs`
never stops — it is spawned once and process exit is its shutdown, so it has no
`with_graceful_shutdown` to copy. This listener is torn down on every settings
write, so a request in flight across a reload is ordinary. Same shape as #311's:
a oneshot fired by `Drop`, awaited as `axum::serve`'s shutdown future. The
generation counter is `integrations/registry.rs`' verbatim, for the same race — a
`stop` landing mid-start must **drop** the handle, and dropping it is what closes
a socket that would otherwise hold provider credentials for the life of the
process.

**`is_retryable` and `should_failover` differ by exactly one status, and they
live in ferrox's *binary* crate** (`ferrox/src/retry.rs`), so they are copied
rather than imported. An upstream **403** fails over *without* retrying: some
providers report quota exhaustion with 403 rather than 429, and the same
provider will keep answering 403 — while this gateway's own `ProxyError::Forbidden`
must **not** fail over, or a client's bad token burns the next provider's quota.
Only the *handshake* of a stream is retried; once chunks are flowing the head is
committed and failing over would replay tokens the client has seen.

**Every unbounded wait races the client's departure — `turn.rs`'s rule, one level
down.** A failed `send` catches a client that left while a frame was being
written, which is what happens when tokens are flowing; it is *not* what happens
when the client leaves while the model is thinking, which is most of a request.
There the loop is parked on `stream.next()`, nothing is being sent, and the
disconnect is invisible — the task parks forever holding the upstream connection,
one leak per abandoned request. `stream::next_or_disconnect` is the
`tokio::select!` against `Sender::closed`, and
`a_disconnect_mid_stream_tears_down_the_upstream_request` is what fails on the
revert. It asserts on the **upstream** side, because that is the only place the
leak is observable.

**`Next::Ended` and `Next::Disconnected` are separate variants deliberately.**
Collapsing them into one `None` loses `data: [DONE]` on a clean stream — and,
worse in the other direction, would *send* one on an abandoned stream, where
`[DONE]` means "the completion finished" and a client would record a truncated
answer as a whole one. Both halves were caught by the suite, in both directions.

**The frames are bytes, not `axum::response::sse::Event`.** The plan was
`impl From<SseFrame> for Event`; both types are foreign, so the orphan rule
refuses it. Bytes are the better answer anyway — the acceptance criteria are byte
properties, and `axum` writes `event:name` with no space where every Anthropic
fixture and ferrox's own API reference show `event: name`. Both parse; this emits
the documented spelling.

**Ordering: the listener starts strictly after `keys::install` and
`tokens::load_revoked`.** Bound before the first, every client gets a 401 until
the key lands — visible, and merely broken. Bound before the second, a token the
user **revoked is honoured** for the length of that window, and nothing reports
it. `a_gateway_start_requires_an_installed_keypair` asserts the order of the
three calls in `lib.rs` against its source, deliberately: the consequence is a
*window* rather than a state, so a test that asked the running app would have to
win that race to see anything.

**The control plane is `/api/gateway/*` and lives under `native/`, not under
`gateway/`** (#426, `native/gateway_api.rs`). The split is the seam, not the
feature: `gateway/` speaks somebody else's wire formats on its own port, this
speaks Agento's, behind Agento's guard with ordinary `read`/`write` scoping —
and `Scope::Llm` opens none of its fourteen routes, which is the disjointness
#423 built seen from the other side (a credential issued to *spend* through the
gateway must not be able to reconfigure which provider it spends with).

Three things about it are decisions:

- **The route table is `parity/desktop_routes.json`, and it now has three
  owners.** #405 created that file for `/api/security/*` — routes with no Go
  ancestor, which could go in neither frozen Go table without destroying what
  those are. Its guarantee is *stronger* than theirs: they assert in one
  direction, so a route claimed and never recorded passes, while this is **set
  equality** against the modules' `ROUTES` consts. The claimed set is now the
  **union** of `security::ROUTES`, `gateway_api::ROUTES` and — since #541 —
  `tasks::ROUTES`, whose single row is `POST /api/tasks/{id}/run`; a fourth
  owner appends there, and forgetting to would quietly weaken the assertion back
  to one direction. Note the third owner is a module that is *mostly* Go's:
  a route with no Go ancestor belongs here whatever else its module claims, and
  adding it to the frozen `write_routes.json` instead would be a silent contract
  break that no test catches. The issue's "resolve at implementation time" question — grow
  the Go tables, or fall back to Tauri commands — is answered by this file
  existing. A command would have been wrong anyway: `logs.rs` is a command
  because the app log belongs to the *process*; gateway config is API surface.
- **An omitted `api_key` preserves the stored one, and that is the whole point
  of `config::update_provider`.** `PUT /api/integrations/{id}` used to wipe
  credentials the caller omits while `GET` scrubs them, so a read-then-write
  round trip — exactly what an edit form does — destroyed the secret. That was
  reproduced there deliberately because it was Go's behaviour, and this surface
  refused to inherit it; **#515 has since fixed the integrations side by
  copying this one**, so the two now share a contract rather than disagreeing
  about it. `api_key` is `Option<String>` with three meanings: absent
  leaves the column out of the `SET` list entirely, `Some("")` is a deliberate
  clear, `Some(k)` replaces. `a_scrubbed_read_written_straight_back_preserves_the_stored_key`
  drives the **real** `GET` body back through the `PUT`, and fails with `""` on
  the revert. No response carries the key either — asserted over the *bytes*,
  because a struct-level check only proves the field the test knows about is
  absent.
- **Referential integrity is checked in code, from both sides.** Routing names
  providers by **name**, inside a JSON column, so nothing in SQL can enforce it:
  deleting a referenced provider is a 409 naming the aliases, and an alias whose
  target names no configured provider is refused. Without both, an alias
  resolves to nothing and fails at *request* time, far from the action that
  caused it.

Every write ends with a spawned `registry::reload` — the write and its effect in
one place — and a `#335`-convention log line with a test. `reload` is not
awaited: it is stop-then-start over a socket bind, and a save that blocked on it
would feel like a hang. `a_provider_save_reloads_without_cutting_an_in_flight_stream`
pins both halves at once, which is where they pull against each other.

**Two of the fourteen routes call an upstream, and they share one dialect
table** — `GET /api/gateway/providers/{id}/models` (#470) and `POST
/api/gateway/providers/validate` (#472), both in `gateway_api::ASYNC_ROUTES` and
both answered by `catalog::fetch`. Five things about that pair:

- **`ASYNC_ROUTES` is a const rather than two literals**, because the property
  that matters is per route and easy to forget on the second one: a route
  claimed by *both* registries leaves the buffered arm permanently unreachable,
  since `proxy.rs` asks `claims_stream` first. `claims` excludes the whole
  const and the guard iterates it, so a third async route inherits the check.
  Note `StreamEndpoint` is the **async** registry, not the long-lived one —
  neither of these streams; `Endpoint::serve` is a sync `fn` on
  `spawn_blocking`, which is right for reading SQLite and wrong for an outbound
  HTTPS call.
- **One dialect table, and #472's spec asked for a second.** That spec predates
  #470 and describes a per-type table in a new `gateway/validate.rs`, sending
  OpenAI to `{base}/v1/models` and putting the Gemini key in `?key=`. All three
  are superseded: the stored OpenAI base is already the versioned root, a
  credential in a URL is what an error string, a redirect and a proxy log all
  see (hence `x-goog-api-key`), and there are **four** provider types, not
  three. `catalog::fetch` therefore takes `(ProviderType, base_url, key)` rather
  than a `ProviderRow`, because #472's caller validates a draft that has no row.
- **A verdict is a `200`, whatever it says.** The outcome of the check is what
  was asked for, so a refused key is a successful answer to "is this key
  refused?" — the same doctrine `ModelsView` already states for the catalog: *a
  failure is a value, not a throw*. The 4xx statuses are reserved for the
  *request* being wrong (undecodable body, a type this build cannot serve, an
  `id` naming no row, nothing to check at all). A route that 4xx'd its verdicts
  would make the form unpack `err.body` to render its own result.
- **The stored-key fallback compares the row's type against the request's**, and
  it is not defensive padding: the form sends the Type dropdown's current value
  with no `api_key`, because an untouched key field is the default for any
  configured provider. Changing Type and pressing Check would otherwise put an
  Anthropic key in an `Authorization: Bearer` header addressed to
  `api.openai.com` — no attacker in it, and the credential lands in a third
  party's request log. An empty `base_url` makes it worse rather than better,
  since it resolves to the *requested* type's production default. A key supplied
  in the body needs no such check.
- **`CatalogError` has three variants because the verdict needs them.** The
  catalog route maps all three onto 400/502 and could not care;
  `unauthorized` / `unreachable` / `unexpected` *is* the question #472 asks, so
  `Upstream` carries the status and a transport failure is its own
  `Unreachable`. `401`/`403` are one bucket (some providers report an exhausted
  quota as `403`) and `404` is grouped with the transport failures, because it
  means the base URL rather than the key — the pair a user otherwise mixes up.

**The form's key field is not a gate, and it was one until #472.** `canSave`
required a freshly typed key on *every* save, described in `ProvidersView.tsx`
as belt-and-braces over the server's `Option<String>`. It was not: the server
has always preserved an omitted key, so what the rule actually did was block
every edit to a timeout on a provider configured months ago — against a secret
no provider dashboard shows twice. It is now `hasKey || (!creating &&
provider.has_api_key)`, a stored key renders as `••• stored` behind an explicit
*Replace key*, and an untouched field still sends **no** `api_key` rather than
`""`, which is the whole distance between preserving a secret and clearing one.
**The validation gate that replaced it is soft on purpose**: a base serving
completions but no model list can never go green, so "Save anyway" is always one
click away. A validation gate with no override is a lockout — do not remove it
to simplify.

**The enable gate is UI-only, and the port probe is advisory** (#474). Two
guardrails over the same first-run path, and what makes each of them safe is the
same thing: neither can refuse a save.

- **`config::GatewaySettings::validate` was deliberately left alone.** A
  server-side refusal of `enabled: true` with no aliases reads as the honest
  one, and it would **422 an existing row whose aliases were later deleted** —
  on a body the form posts *on every save*, so an unrelated retention edit would
  start failing. A deleted provider is already a 409 when an alias references
  it; nothing stops deleting every alias, and that has to stay a save-able
  state. So the gate lives in `SettingsView.tsx` and gates the **switches**,
  never Save.
- **It gates turning-on, not the switch**, and that asymmetry is the whole of
  it: `disabled={!canTurnOn && !enabled}`. A flat `disabled` would lock an
  install that *is* enabled out of switching its own listener off, which is
  exactly what someone whose last alias went away wants to do. Nothing rewrites
  a stored value either — a forced `false` would disable a working gateway on
  the next unrelated save, which is what "existing installs are unaffected by
  the upgrade" means.
- **A failed readiness read is a third state**, not "empty". `Readiness` is
  `loading | unknown | ready | unroutable`, because a request that errored knows
  nothing about stored rows — the rule `ModelsView`'s list column already
  states. It shuts the switches the same way and says something different.
- **"Configured" counts alias *rows*, not enabled ones.** Gating on
  `alias.enabled` would make the gateway switch unusable the moment somebody
  toggles their last alias off, which is a legitimate temporary state.
- **`GET /api/gateway/port-availability?port=N` is the one thing a webview
  cannot do** — bind a socket. Buffered, not async: it is a `bind`, so
  `spawn_blocking` is right and the #470/#472 reasoning does not apply. Three
  rules on it. The **running listener's own port reads as available**
  (`own_port_of`), or the probe finds the port held by the very process being
  configured and offers to move it on every visit — note `BindFailed` carries a
  port too and that one is precisely *not* held, so only `Running` counts. The
  walk is **capped** at `PORT_SCAN_SPAN` above the request and the body says how
  far it looked (`scanned_to`), so "no free port" is a bounded claim rather than
  65535 `bind` syscalls behind a form somebody is typing into. And the answer is
  **never applied for the user**: the port is what gets pasted into
  `OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL`, so a silent rewrite points every
  already-configured tool at nothing.
- **The probe is a TOCTOU check by construction** — the listener is dropped
  immediately, so nothing is reserved. `Status::BindFailed` stays the authority
  and the status strip stays where the real outcome shows up; what this buys is
  moving the *routine* collision (a `~/.agento-desktop-dev` instance and an
  installed one share the machine's ports) to before the save, which a `200`
  from `PUT /gateway/settings` can never do.

**Usage recording is one row per served request, written off the request path**
(#425, `gateway/usage.rs`, migration 34). Four things about it are decisions:

- **A usage row may never fail a request.** The tokens are already spent by the
  time there is anything to record, so `Accounting::finish` spawns and is never
  awaited, the insert goes through `db::blocking` (#366 — this listener is not
  on `proxy.rs`'s blocking pool at all, so an inline insert parks a *runtime
  worker* for up to the five-second `busy_timeout`), and every failure below it
  is a `warn`. `a_contended_write_lock_does_not_stall_the_runtime` is the third
  copy of that regression test; without the hand-off the runtime stalls 1541 ms
  against a 1500 ms hold.
- **Exactly one row, enforced rather than hoped for.** Both surfaces have three
  terminal arms and the Anthropic one reaches them through a translation layer
  with its own state machine. A missed arm writes none; a doubled arm writes
  two, and a log that sometimes double-counts is worse than one that sometimes
  misses because nothing in the numbers says which. `finish` is a
  compare-exchange on a `done` flag, and the status defaults to `Interrupted`
  so the arm most easily missed is the default rather than the special case.
- **Provider usage is a running total, so the counters are replaced, not
  summed.** Accumulating would multiply a 200-chunk stream's prompt tokens by
  200. A chunk carrying no usage leaves them alone, which is what lets an
  **interrupted** stream record the tokens it saw rather than zeros — the arm
  that matters most, since an abandoned stream still spent them. On the
  Anthropic surface the metering happens **before** the translation
  (`usage::meter`), because the emitter consumes the provider stream and a
  frame carries no `usage`.
- **Cost is stored, not derived** — the rule the scanner already enforces. A
  rate correction must not retroactively rewrite past spend, and joining the
  catalog at read time reproduces the list-versus-dashboard disagreement the
  Claude side refuses. An unpriced model stores `NULL` with `unpriced = 1`,
  **never `0.0`**; that is the common case here, not an edge one, since the
  catalog is seeded for Claude models and OpenAI/Gemini/GLM aliases miss.

Two rows are deliberately *not* written: a body that will not decode (no alias,
no provider, nothing spent — four empty columns diluting every average) and a
request refused by **auth** (no attributable token; logging unauthenticated
attempts is a different feature). A refused *dispatch* does get one, because it
names an alias the user configured — and it carries the **last target
attempted**, so an empty `provider` means specifically "resolution failed"
rather than "unknown".

Two smaller notes. `tokens::touch` already throttles to one write a minute and
spawns its own `db::blocking`, so calling it from the middleware is not a
database call on the request path and must **not** be wrapped a second time. And
the `anthropic-beta` header and the body's `betas` array are merged into one
comma-separated header value, header first, with `raw_anthropic_body` forwarding
the client's document verbatim — both copied from
`ferrox/src/handlers/anthropic_messages.rs`, and Claude Code compatibility
depends on them.

**Retention is a horizon, a prune, and a disclosure — and the third is what
makes the first two honest** (#428, migration 35). `gateway_usage_log` was
append-only: at ~100 bytes a row, 5k requests a day reaches ~180 MB in a year.
`gateway_settings.usage_retention_days` bounds it, default 90, ceiling 3650.

Four things about it are decisions rather than details:

- **`0` means keep everything, and it is therefore the *longest* horizon rather
  than the shortest.** Every comparison over this value has to read that way: the
  prune returns before opening a connection, the UI's "you are shortening this"
  warning fires on moving *off* zero and never onto it. Read as a horizon of zero
  days it would mean "delete immediately", which is the exact inverse.
- **The field carries `#[serde(default)]` where every sibling request struct
  deliberately carries none.** `GatewaySettings` *is* the
  `PUT /api/gateway/settings` body, so a required field would 400 every client
  written against #426's three-key shape. The default is the column's — an
  omitted key must not silently mean `0`, which is *keep forever*. The Settings
  view still sends it on every save, because with a default present, omitting it
  is the difference between preserving the stored horizon and resetting it to 90.
- **The prune rides the write; it owns no timer.** It runs inside
  `Accounting::finish`'s spawned `db::blocking` section — already off the request
  path — behind a once-a-day gate shaped like `tokens::due`, plus a launch sweep
  in `lib.rs` because a desktop app open ten minutes a week would never reach the
  daily interval on traffic alone. Both claim the same slot. `<` not `<=`, so the
  boundary row is kept. A failed prune is a `warn` and is dropped, as a failed
  insert is.

  **`prune_since` exists so that last sentence is testable.** With `Utc::now()`
  read inside the function there is no way to write a row landing *exactly* on
  the cut, and the first version of the boundary test put its row five seconds
  late — on the keep side of both `<` and `<=`, so it passed against the very
  flip it was named for. A review caught it. The general form is worth keeping:
  **a test named for a boundary has to construct the boundary**, which usually
  means the value being compared against cannot be read from the clock inside
  the function under test.
- **A pruned window says so.** An under-reported total that looks complete is the
  failure a prune introduces, and it is silent without this — so the Usage view
  labels the window a **floor** whenever it reaches past the horizon, in the same
  wording as the unpriced-cost note, so the two read as one idea rather than two
  warnings.

**The Usage view is #426's endpoint plus the three aggregates it did not ship.**
`by_surface`, `by_token` and a `latency` block (nearest-rank p50/p95/max) were in
#428's acceptance criteria and not in #426's `UsageBody`; they are computed over
columns migration 34 already stores, so the addition is additive and no existing
field moved. `by_token` groups on the token's `sub` — an `api_tokens` row id,
never a secret — and an unattributable row is grouped under `""` rather than
dropped, so the breakdown's total cannot disagree with the window's.

**That id is not a label, and `UsageGroup.label` is why.** The row id appears
nowhere else in the product: the Security tab lists tokens by *name*, so a panel
printing `3f2a…` cannot answer the one question it exists for. `read_usage`
resolves the names itself rather than leaving the view to fetch them, because
`/api/security/*` needs a **`write`** scope whatever the method — a read-only
dashboard has no business holding one. Revoked tokens keep their names, since a
revoked credential's spending history is most of what made the revocation
informed. `label` is `skip_serializing_if = "Option::is_none"` and only
`by_token` ever sets it. **It widens what a `read` token can see, and that is
argued at the site rather than left implicit:** `required_scope` forces `write`
on `/api/security/*` so a `read` token cannot enumerate every credential, and
what this returns is not an enumeration — only tokens with traffic in the
window, only `(id, name)`, with the scope, `jti`, expiry and revocation state
left where they were.

**The launch sweep asks a reader before it asks for a write lock.** It runs on
every boot, and an install that never switched the gateway on has an empty
`gateway_usage_log` forever — so `prune_since` short-circuits on
`SELECT EXISTS(...)` through a WAL reader, which never waits on a writer. That is
what keeps "an install that never configures one pays a single `SELECT` at boot"
true. An *unreadable* database is not an empty one: it falls through, so the
write path reports the real error rather than a silent no-op.

**`validate` returns the field it refused, and must keep doing so.** It is
`Result<(), SettingsError>` with `field` and `message`, not a bare `String`: the
first version had the route recover the field by `starts_with`-ing the message,
a prose coupling that would mislabel the input a form highlights the moment
either string was reworded — and the test written for it asserted
`contains("usage_retention_days")`, which the *message* also satisfies, so it
passed with the field hardcoded. Assert the whole
`validation error for \"<field>\"` prefix, which is the only part of that body
the field actually decides.

**The chart primitives are shared; nothing else is.** `src/components/charts.tsx`
is `views/analytics/charts.tsx` moved, with its stylesheet
(`styles/charts.css`). The locked decision that gateway traffic is never mixed
into Claude analytics is about **data and sections**, not React components —
`charts.tsx` is presentation over `{label, value, hint}[]` and holds no Claude
type. What stayed unshared is everything typed: `analytics/shared.tsx`'s
`entryLabel`/`projectLabel`/`RankList`, `stats.ts`, the wire types, and the
ranked-list CSS (the gateway has its own `.gw-rank`). Prove a move like this
rather than asserting it: building both sides and comparing the emitted CSS as a
sorted set of rules gave 578 identical rules.

## The gateway is documented, and where (#429)

The epic closes with the docs, so the user-facing account of this feature lives
outside this file and must not be duplicated back into it:

| where | what it carries |
|---|---|
| `docs/user-guide.md` → **LLM Gateway** | the whole flow — enable, provider, alias, mint, point a tool, watch usage, and the retention horizon |
| `docs/troubleshooting.md` → **LLM Gateway** | bind failure, 401 vs 403 in both directions, the model-name mismatch, empty Usage, the log lines |
| `docs/development.md` → **The LLM gateway** | the `/api`-versus-listener split, the `ferrox-providers` feature policy, and the two curl commands |
| `README.md` → *Route your other tools through Agento* | one bullet, flagged off by default |

**Three claims a doc must keep making, because each is the inverse of what a
reader assumes.** `0` retention days is *keep everything*, the **longest**
horizon and not the shortest. The Anthropic base URL has **no** `/v1` while the
OpenAI one does, because the Anthropic SDK appends its own — the measured failure
is a 404 on `/anthropic/v1/v1/messages`. And the three scopes are **disjoint, not
ranked**: `write` is not a superset of `llm`, so "use a bigger token" is never
the fix for a gateway 403.

**A cold-start test is the acceptance bar for this feature's docs, and it found
things reading the code did not.** The one worth keeping: Claude Code pointed at
the gateway with only `ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN` asks for
*its own* default model, which is not a configured alias, and stops with "There's
an issue with the selected model". The alias name is the whole routing key, so
either `ANTHROPIC_MODEL` names an alias or an alias is named after what the
client sends. The env snippets in `views/gateway/snippets.ts` show two variables
and the user guide shows three, deliberately.

**The `ferrox-providers` pin is a tag, and the crates.io question is open.**
`Cargo.toml`'s comment defers "tag vs crates.io publish" to before the first
shipping release; #429 records it as a release-gate item rather than deciding it,
because a docs change must not settle a supply-chain choice. See
[#453](https://github.com/shaharia-lab/agento/issues/453).
