# Agento Desktop — working notes

Native desktop client for Agento, living at `desktop/` inside the Agento repo.
**Tauri 2 + Rust** shell, **React + TypeScript** UI, and — for now — the
repo's own **Go** server running inside the app as a bundled sidecar.

The long-term goal is to port the Go backend to Rust. Read
[Porting Go → Rust](#porting-go--rust) before touching `src-tauri/`.

## Branch and release model

This work lives on the **`desktop`** branch. `main` carries the existing Go
server and is left alone until the two converge.

- Desktop PRs target **`desktop`**, not `main`.
- Desktop releases are tagged **`desktop-v*`** from `desktop`
  (`.github/workflows/desktop-release.yml`).
- The Go server keeps its own release on `v*` tags. The two tag patterns do not
  overlap, so both ship independently from the same repo.

---

## Run it

All commands run from `desktop/`.

```bash
npm install
npm run app          # desktop window, hot reload (Vite + cargo watch)
npm run dev          # browser-only UI; no backend, most views will error
npm run app:build    # native installers for the current platform
npm run build        # typecheck + build the frontend alone
cd src-tauri && cargo build
```

First run needs the Go sidecar built. The script maps the Rust target triple to
`GOOS`/`GOARCH` and names the output the way Tauri's `externalBin` expects:

```bash
./scripts/build-sidecar.sh                        # host
./scripts/build-sidecar.sh aarch64-apple-darwin   # cross
```

The Go server is pure Go (`modernc.org/sqlite`, no CGO), so `CGO_ENABLED=0`
cross-compiles every target from any host. Tauri's *bundles* cannot cross-
compile, which is why the release workflow uses one runner per OS.

Linux system deps (once): `libwebkit2gtk-4.1-dev build-essential curl wget file
libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`.

---

## Architecture

```
┌─ Agento.app ────────────────────────────────────────────┐
│ Tauri (Rust)                                            │
│   sidecar.rs ── spawns ──> agento web --port <random>   │
│                            (Go binary, bundled)         │
│   proxy.rs   ── axum on 127.0.0.1:<port>                │
│        /api/*  ──> Go sidecar        (streams, incl SSE)│
│        /*      ──> frontend assets   (release only)     │
│   menu.rs    ── native menu, emits menu://action        │
│                                                         │
│   WebView ──> React UI ──> fetch("/api/...")            │
└─────────────────────────────────────────────────────────┘
```

**Why the proxy exists.** The Go server answers `/api` for same-origin requests
only — its CORS middleware is a deliberate no-op in production builds. Putting
one origin in front of both the UI and the API sidesteps CORS entirely and
keeps SSE intact, which a Rust-side `fetch` shim would not. It is also the
migration seam (see below). **Do not remove it** to "simplify".

**Dev vs release**
| | dev | release |
|---|---|---|
| UI served by | Vite :1420 | Rust proxy (embedded assets) |
| `/api` reaches Go via | Vite proxy → Rust proxy :8991 | Rust proxy (same origin) |
| Proxy port | fixed 8991 | OS-assigned |
| Sidecar data dir | `~/.agento-desktop-dev` | `~/.agento` (the real one) |

Dev uses a separate data directory on purpose: two Agento processes sharing
`~/.agento` share one SQLite file *and* one scheduler, so a scheduled task
fires twice and the Telegram webhook gets re-registered underneath whichever
instance registered it last.

---

## Layout

```
src/
  lib/
    api.ts       fetch wrapper + qs() + streamChatMessage() (POST-based SSE)
    types.ts     TypeScript mirrors of the Go JSON — snake_case, verbatim
    hooks.ts     useResource / useDebounced / usePoll / describeError
    format.ts    compactNumber, usd, duration, relativeTime, toneFor, …
    stats.ts     cross-view counters for sidebar + status bar
    nav.ts       sidebar sections, view ids, titles
    icons.tsx    16px / 1.5-stroke icon set
    tauri.ts     window + menu bridge; degrades to a plain browser tab
  components/    TitleBar, Sidebar, StatusBar, CommandPalette, ui.tsx
  views/         one file per section
  styles/        tokens → base → shell → controls → views (+ per-view files)

src-tauri/src/
  lib.rs         setup: ports, sidecar, proxy, window, menu
  paths.rs       data dir + database path; shared by the sidecar and native/
  sidecar.rs     spawn + health-wait + shutdown of the Go server
  proxy.rs       axum reverse proxy; route_is_native() is the porting switch
  menu.rs        native menu → menu://action events
  claude/        the Claude Agent SDK, ported from Go (phase 5's foundation)
    process.rs   spawn, the control protocol, and the initialize handshake
    client.rs    Stream (events) + StreamControl (interrupt, set_model, …)
    session.rs   persistent multi-turn conversations on one subprocess
    options.rs   every option, and which of the two channels it travels on
    messages.rs  the wire types and parse_line
    permissions.rs / hooks.rs   the two callback round trips
    mcp.rs       loopback HTTP host for in-process MCP servers (rmcp, stateless)
    tool.rs      a tool is a function: derived schemas, runtime registration
    schema_vectors.rs  tests only: which Go shapes schemars reproduces, and what
                 a port must write for the ones it does not (#312)
    lenient.rs   Go's partial-decode semantics, which serde does not have
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
    migrate.rs   the 27 migrations, embedded from parity/ — verified, not applied
    writes.rs    what a write may answer, and what it hands back to Go
    chat/        the SSE turn and the three routes that steer it (#276)
      live.rs    the process-local live-session registry — why the four move together
      runner.rs  an agent's config as SDK options; starts the local tools
                 server (#310) and one per github integration (#311), and
                 refuses only what it still cannot supply
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
    settings.rs  GET /api/settings and /settings/claude-config-dirs (a filesystem
                 probe; Unix, forwards on Windows); the preferences + config dirs
                 a read is scoped to; and `update`, the PUT — written and tested,
                 deliberately unclaimed
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
                 credentials are never selected and auth is a bool made in SQL;
                 plus POST /api/integrations, the trigger-rule writes (#277) and
                 PUT/DELETE /{id} (#311)
    integrations/registry.rs
                 Start/Reload/Stop for the MCP servers of HOSTED_TYPES — the one
                 list both processes are configured from (the sidecar is started
                 with AGENTO_INTEGRATIONS=off:<it>). The one place a credential
                 is read, behind its own projection
    integration_credentials.rs the seven per-type validators, and the two failures
                 whose Go error text is not reproducible (both forward)
    scan.rs      GET /api/claude-sessions/status, POST /refresh — and the scan
                 itself: the shell owns it now, the sidecar runs with AGENTO_SCANNER=off
    fs.rs        GET /api/fs and POST /api/fs/mkdir — the working-dir picker's
                 listing and its one create (Unix; forwards on Windows)
    uploads.rs   POST /api/uploads — the one multipart body, and the extension
                 allowlist that is the route's whole security boundary
    gopath.rs    Go's filepath.Clean/Dir/Join, pinned to vectors generated from Go
    gourl.rs     the path chi routes on — Go decodes only canonical escaping
    query.rs     one query parameter, read the way r.URL.Query().Get reads it
    pricing.rs   GET /api/pricing/catalog, plus the rate Resolver — and the
                 three rate writes (#306): add and correct are not one upsert
    agents.rs    GET /api/agents and /api/agents/{slug}
    chats.rs     GET /api/chats and /api/chats/{id}; compact() is Go's, byte for byte
    tasks.rs     GET /api/tasks, /api/job-history and the three reads between them
    schedule/    when a task fires — pinned to parity/scheduler_vectors.json (#275)
      mod.rs     buildJobDefinition and gocron's four job types; claims no route
      cron.rs    robfig/cron's dialect, which is the one a cron task is written in
    sessions/    GET /api/claude-sessions, /facets, /projects and /{id}, plus
                 POST /{id}/continue (#308) and PATCH /{id} (#296)
      continue_chat.rs the two writes that resume a Claude session as a chat
      update.rs  the rename and the favourite — the only two columns here the
                 user typed, and the only ones the scanner never writes
      detail.rs  one session re-read from its transcript, patched from the cache
      projects.rs the project picker's list, derived from the same walk a scan is
      corpus.rs  loads the lot
    analytics/   GET /api/claude-analytics
      buckets.rs Go's time.Date/AddDate and the bucket walks, in the request's tz
      params.rs  from/to/project/tz, and the granularity the window picks
      report.rs  every aggregate in the payload
      cards.rs   the Insights cards
    insights/    GET /api/claude-sessions/insights/summary
      transcript.rs the session JSONL, decoded — the scanner port builds on this
      processors.rs the nine passes that produce a session_insights row
      summary.rs    the aggregate the endpoint answers with
    diff.rs      byte comparison + reporting for shadow mode

scripts/
  parity-instance.sh   Go server built from THIS checkout, on a copy of the DB

src-tauri/tests/ one live-diff suite per area, plus parity_common/ shared by all
parity/          cross-language fixtures, asserted by both Go and Rust tests
```

---

## Conventions

**Wire types are not translated.** `src/lib/types.ts` uses the Go `json:` tags
as-is. Renaming at the boundary would only hide drift. Go marshals empty slices
as `null`, so every array field is `T[] | null` — handle it.

**Verify against the live instance.** A real Agento runs on
`http://127.0.0.1:8990` with the user's real history and credentials.
`curl -H "Content-Type: application/json" http://127.0.0.1:8990/api/...` is the
ground truth for wire formats — better than reading Go structs. **GET only.**
Never write to it. For write-path testing, start your own instance with
`AGENTO_DATA_DIR` pointed at a scratch directory.

Every state-changing `/api` request needs `Content-Type: application/json` —
`POST`, `PUT`, `PATCH`, `DELETE`; the server's guard (`isStateChanging` in
`internal/server/guards.go`) runs before the handler and 415s without it,
including on the payload-free endpoints. `GET`/`HEAD`/`OPTIONS` are untouched.
`api.ts` does this for you.

**Desktop, not web.** The UI deliberately diverges from the Agento web app:
three resizable panes per section, 13px type, 26px rows, hairline borders,
custom titlebar, status bar, focus-aware selection (accent when the window is
focused, grey when not), ⌘K palette, no browser affordances. Reuse the existing
CSS classes; new CSS goes in a per-view file imported by that view.

**No `window.confirm` / `alert` / `prompt`** — they block the WebView and can
wedge the app. Render inline confirmation UI.

**Theming.** Tokens are defined on bare `:root` for light, then re-declared
under both `@media (prefers-color-scheme: dark)` and `:root[data-theme="dark"]`.
Never give a colour its only definition inside a media block. Charts are inline
SVG using `var(--accent)` etc. — no chart libraries.

---

## Porting Go → Rust

The Go backend is ~80k lines across ~90 REST endpoints. The agreed strategy is
**sidecar first, then port subsystem by subsystem**, because two Go
dependencies have no mature Rust equivalent — the Claude Agent SDK for Go and
`whatsmeow` for WhatsApp — and a big-bang rewrite would leave nothing runnable
for months. The SDK was ported (`src/claude/`); **WhatsApp was dropped instead**
(see below).

### The seam

`proxy.rs::route_is_native(method, path)` decides per request whether Rust
answers or the Go sidecar does. Behind it is a **registry**: each ported module
declares its own `claims` and `serve` as a `native::Endpoint`, and `ENDPOINTS`
in `native/mod.rs` lists them.

Two properties, both load-bearing:

- **Claiming a route and implementing it are one edit.** The pair lives in the
  module it belongs to, so a route cannot end up claimed by a handler that does
  not exist — which fails as a *silent* fallback to Go, not as a compile error.
- **Adding an endpoint is one appended line** in `ENDPOINTS` plus its own file.
  `native/mod.rs` used to hold a single `match` over every route, so two ports
  in flight always collided in the same hunk. Nothing in `mod.rs` knows what a
  module does, and no module knows about another.

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
carry, so both forward and Go answers. The order of those two checks is
load-bearing — `/api/agents/%ff` decodes to the same unrepresentable byte as
`%FF` but is not canonical, so chi routes on the raw target, which is plain
ASCII. `desktop/parity/gourl_vectors.json` records what a live chi router
actually did, not what the rule says it should.

While every claimed route was a read this was invisible — a miss produced `Err`
and forwarded, and Go answered correctly. It stopped being invisible when #274
and #276 claimed writes: `agents::update` *answers* 404 and `chats::patch`
answers `chat not found` for a request Go would have applied to a real row.

`AGENTO_DESKTOP_NATIVE` steers the whole seam:

| value | behaviour |
|---|---|
| unset / `on` | claimed routes are answered by Rust |
| `off` | nothing is claimed; everything forwards to Go |
| `diff` | Go answers, Rust computes alongside, bytes are compared and mismatches logged |

**A native failure is never surfaced.** Handlers return `Result`, and an `Err`
is logged and forwarded to Go rather than turned into a 500 — a ported route
can only ever be as broken as an unported one. That is what makes flipping a
route safe: the worst case is the behaviour the app had before.

**The guards used to be the one place that property did not hold, and now they
are not** (#329). A natively-served request never reached
`internal/server/guards.go`, so neither `validateHost` nor
`requireJSONContentType` applied to any ported route and the guards' coverage
shrank with every endpoint the port claimed — true since #274, and increasingly
pointed as the claimed surfaces came to include `~/.claude/settings.json` with
its `hooks` key and `POST /api/fs/mkdir`.

`src-tauri/src/guards.rs` is `guards.go` at the proxy, applied in `dispatch`
**before** the seam decides who answers, so a claimed route and a forwarded one
are refused identically. Four things about it are load-bearing:

- **It is scoped to `/api`, exactly as Go's is.** `POST /webhooks/telegram/{id}`
  is mounted at the root, arrives from Telegram's servers with a foreign `Host`
  and is authenticated by its own secret token; a global guard would break
  inbound triggers. `/health`, `/metrics` and the SPA are likewise untouched.
- **For the content type the sidecar's copy is a second line. For the `Host` it
  is not.** `forward` rewrites `Host` to the upstream authority — that is what
  makes a proxied request indistinguishable from a same-origin one, and the
  sidecar would 403 everything without it — so Go's `validateHost` has never
  seen the browser's `Host`. The proxy is the only place it can be checked,
  which is also why the check runs before `gourl::route_path`: the two "no route
  path" cases forward, and forwarding is where the `Host` is lost.
- **A body-less request is not exempt**, whatever the root `CLAUDE.md` said
  before this change and whatever #329's own text repeated. `guards_test.go`
  pins "a body-less DELETE is refused without the header", and the reason is in
  `guards.go`'s comment: several state-changing endpoints take no body, and a
  cross-origin `POST` with neither body nor `Content-Type` is *itself* a simple
  request. `api.ts` sends the header on every request, so requiring it always
  costs nothing.
- **The two branches of `hostAllowed` that depend on deployment are not
  reproduced.** Go admits the configured `PublicURL`'s host and, for a
  non-loopback `AGENTO_BIND`, any bare IP literal. The proxy binds `127.0.0.1`
  unconditionally and has no public URL, so `localhost` and loopback literals
  are the whole set; adding either branch would widen the guard past anything
  the app can be reached at.

A rejection logs as `Served::Rejected` (`rejected`) rather than under either
half, because the check runs before routing is decided.

Because both implementations run at once, a ported route is verifiable:
replay the same request against Rust and against Go and diff the JSON.
**Byte-identical JSON is the bar** — the frontend is shared, so any
field-name, key-order, escaping or float-spelling drift is a regression, and
only a byte comparison catches all four.

### How to port a route

1. Read `native/gojson.rs` first. Rust's natural JSON is *not* Go's, and the
   differences (`3` vs `3.0`, `<` vs `<`, the encoder's trailing newline)
   are on nearly every response. Encode through `gojson::to_vec`, keep struct
   fields in the Go struct's declaration order, and use
   `skip_serializing_if` for `omitempty`.
2. Implement it, mirroring the Go source's ordering and grouping exactly —
   including anything hashed, since a fingerprint over rows in a different
   order is a different fingerprint for identical data.
3. Seed the scratch instance if the endpoint has no data on this machine. It is
   a *copy*, so writing to it is safe and it is the only way to diff a shape the
   developer's own install does not contain — the agents list was empty here, so
   the first "identical" meant nothing until two agents were created through it.
4. Prove it three ways:
   - a fixture both languages build, compared against a golden file Go wrote
     (`desktop/parity/`, `go test ./desktop/parity/ -update-golden`). A shared
     *primitive* rather than a response takes the vector form instead —
     `gopath_vectors.json` records what Go's `filepath.Clean`/`Dir`/`Join`
     answer and both languages assert against it, which is how #268 found a
     doubled separator the Rust `Clean` produced. Build the
     fixture with **no ties on any sort key** — see below; a tie makes the
     golden flaky in Go before Rust ever sees it, which is why
     `TestAnalyticsFixtureHasNoTiesOnAnySortKey` asserts the property;
   - the live diff, against real data **and a Go server built from this
     checkout**:
     ```bash
     eval "$(./scripts/parity-instance.sh start)"
     (cd src-tauri && cargo test --test parity_analytics -- --ignored --nocapture)
     ./scripts/parity-instance.sh stop
     ```
     One suite per area (`tests/parity_<area>.rs`, sharing `tests/parity_common/`),
     so a port runs its own diff and two ports do not edit one file. Drop the
     `--test` flag to run them all — but note that `parity_writes` **mutates**,
     unlike every other suite. It creates, renames and deletes rows, so it
     refuses to run unless `AGENTO_LIVE_URL` is set rather than falling back to
     the `:8990` default the read suites use. Start the scratch instance first.
     It also compares differently: a write cannot be asked of both
     implementations at once, so it pins Go's answers — status *and* bytes — as
     literals, and the unit tests assert the same literals against Rust;
   - optionally `AGENTO_DESKTOP_NATIVE=diff npm run app`, which compares every
     real request the UI makes.
5. Only then leave it claimed.

### Never diff against the installed server

`parity-instance.sh` exists because the Agento on `:8990` is whatever binary the
developer installed, which drifts behind the repo. The first sessions-list diff
"failed" purely because that instance predated `config_dir` joining the summary
— the port was right and the baseline was stale. The reverse is worse: an old
server that happens to agree hides a real divergence. The script builds the
server from the checkout and runs it against a **copy** of `~/.agento` (the
current source may carry migrations the installed one has never applied, and
applying them to the real file would upgrade it under a running instance).

**It is safe to run concurrently.** The work dir defaults to a name derived from
the checkout's own path, and `stop` kills only the PID recorded in that dir — so
two agents in separate worktrees need no coordination, and neither can clobber
the other's scratch database or replace the binary underneath a running server.
Two agents sharing one checkout still collide; set `AGENTO_PARITY_WORKER=<id>`
(a suffix) or `AGENTO_PARITY_DIR=<path>` to separate them. `start` exports
`AGENTO_PARITY_DIR` alongside the URL, and `parity-instance.sh url` re-prints
the exports for a shell that lost them, without restarting anything.

### Go itself is not always byte-stable

**Where a Go map feeds an unstable sort, Go's own response differs run to run**,
and no port can be byte-identical to all of it. Several analytics builders
collect into a map — random iteration order — and then call `sort.Slice`, which
is pdqsort and unstable, so two rows tying on the sort key come out in either
order. It is observable on the reference corpus: `sessions_per_model` has two
models with one session each, and repeated *uncached* requests swap them. (The
memo in `analytics_cache.go` hides this: the same query string returns the same
bytes until the entry is evicted, which takes 21 distinct windows.)

The Rust port collects into a `BTreeMap` and sorts stably, so a tie breaks on the
model or project name and the response is reproducible — strictly better, but it
matches only *one* of the orderings Go produces. `fetch_analytics_until` in the
live parity test therefore re-asks Go, evicting the memo between attempts, and
reports which attempt matched. A real divergence still fails, with the byte
offset and surrounding context.

Before assuming a diff is your bug, **ask Go the same question twice**.

**Retrying only works when the orderings come up evenly.** `GET /api/integrations/available-tools`
ranges `cfg.Services`, a Go map, and measured over 25 requests against a
two-service integration it emitted 22 of one order and 3 of the other — so
twelve attempts miss about one run in five. `tests/parity_integrations.rs`
compares that one endpoint as a **multiset of byte-exact elements** instead:
each element is captured as a `RawValue` so a reordered key or a respelled
number *inside* one still fails, and only the order *between* elements is
exempt. Prefer this shape over raising the retry count when a diff is unstable
for a reason Go cannot promise away — a test that flakes is worse than no test,
and a byte diff of the whole body is not a property either side can have.

### Known encoder divergence

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

### A JSON `null` is a zero value, not a type error

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

### The build stamp, and the half of `/version` that is not ported

`internal/build`'s three variables are set by `-ldflags`, and **only the
Makefile does that**. `scripts/build-sidecar.sh` builds with `-ldflags "-s -w"`
and `scripts/parity-instance.sh` with none at all, so the sidecar the app ships
and the server every parity test diffs against both serve the package defaults —
`dev` / `unknown` / `unknown`. `native/version.rs` declares the same defaults,
behind `option_env!("AGENTO_BUILD_VERSION")` and friends, so stamping the desktop
bundle later is a build-script change rather than a code change. Do not stamp
Rust while the Go sidecar is still unstamped: that is a parity failure by
construction.

`/api/version/update-check` is deliberately **half** ported. Go's answer for a
build that names no published release needs no network — it short-circuits to
`update_available: false`, and that is every build the desktop app ships. Its
other branch asks GitHub for the latest release and compares, which *is* the
self-updater, the one subsystem the handover excludes because Tauri's updater
replaces it. So the short-circuit is native and anything else returns `Err` and
forwards. That is the seam working as designed, not an omission — but it is one
of the `Err` arms the cut-over has to turn into a real response.

### Phase order (easiest → hardest)

| Phase | Subsystem | Go source | Notes |
|---|---|---|---|
| 1 ✅ | Sidecar + proxy | — | done |
| 2 ← | Pricing + analytics | `internal/pricing`, `internal/claudesessions` | Pure computation over JSONL + SQLite. No external deps. **In progress**: `/api/pricing/catalog`, `/api/claude-sessions`, `/api/claude-sessions/facets`, `/api/claude-analytics`, `/api/claude-sessions/insights/summary` and the agent reads are native and diff clean. |
| 3 ← | Storage + tasks | `internal/storage`, `internal/scheduler` | **In progress (#274, #275).** `db.rs` is read-write, the 27 migrations are ported, and the writes whose every effect Rust owns are native — see below. The scheduler's *schedule computation* is ported and pinned (#275); its ownership is not, and is blocked — see below. |
| 4 ← | Integrations | `internal/integrations`, `internal/trigger` | OAuth2 + MCP servers. Six of them: google, github, slack, jira, confluence, telegram, plus `internal/tools`. **How to host one is settled (#282)** — `claude::ToolServer`, see "Hosting a tool" below; do not invent a second way. **`internal/tools` is done (#310)** — `native/tools/`, and it is the worked example the six should be read against. **GitHub is done (#312)** — `native/integrations/github/`, the worked example for an *integration*. **The registry is done (#311)** — `native/integrations/registry.rs` plus `PUT`/`DELETE /api/integrations/{id}`, and the sidecar now runs with `AGENTO_INTEGRATIONS=off:<the types the shell hosts>`, so every type has exactly one owner. **Confluence is done (#317)** — `native/integrations/confluence/`, the smallest of the six and the one that shows what an integration adds over #312: a per-row API base, basic auth, and a `Start` check that is a decision. **Jira is done (#316)** — `native/integrations/jira/`, Confluence's twin, and the one that shows that a shared credential type does not mean shared behaviour: `jira.Start` validates nothing, so a bad base is answered per call instead of by refusing to host. **Slack is done (#315)** — `native/integrations/slack/`, the first that is not Atlassian-shaped: no model input in the URL at all, and the only integration whose token can come from the `auth` column. #313 and #314 are each "add a starter **and its name to `HOSTED_TYPES`**", which is the one list both processes are configured from. WhatsApp is **not** among them — but Go still hosts its rows, because its starter opens a live connection rather than just a port; see below. |
| 5 ← | Agent execution | `internal/agent`, `internal/service` | **In progress (#276).** The chat SSE turn is native: `/messages`, `/input`, `/permission` and `/stop`, on top of the ported SDK. The scheduler's executor (#275) is the other caller and still Go's. |

### The session scanner (`src-tauri/src/native/scanner/`)

`scanner.go` + `scan_apply.go` in Rust: the walk, the diff, the staleness
rules, the transcript→row reader and the parallel-read/batched-write apply.

**It computes; it does not write.** `native/db.rs` opens the database
read-only on purpose, and the Go sidecar runs its own scanner on every read
path — two processes writing one SQLite file is what that read-only flag
exists to prevent. So this follows the precedent #263 set for the insight
processors: port the logic, verify it against the rows Go already wrote.
Wiring it in, and with it retiring `freshness_probe`, belongs to phase 3.

**Parity is the stored rows, not a response.** Every
`claude_session_cache` and `claude_subagent_cache` row is recomputed from
its own transcript and compared field by field — 926 and 920 rows on the
reference corpus (`tests/parity_scanner.rs`). The row records the mtime it
was read at, so "has this file grown since" is exact; rows that moved on
are skipped, because every figure would read as "computed is larger",
which is also what an over-counting bug looks like.

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

### The Claude Agent SDK (`src-tauri/src/claude/`)

`github.com/shaharia-lab/claude-agent-sdk-go` reimplemented in Rust — the
library every agent run goes through, and the thing phases 4 and 5 both sit on
(every Agento integration is an in-process MCP server, which is this SDK's
`StartInProcessMCPServer`). Read `~/Projects/claude-agent-sdk-go` as the spec;
it is our own OSS project and carries the protocol decisions in its comments.

It is **not an API client**: it spawns the `claude` CLI and speaks stream-json
over stdio plus a control protocol, so there is no inference to reimplement and
no API key — the CLI's own sign-in is the credential. Nothing calls it yet.

**The parity bar is different here.** There is no JSON response to diff, so
`parity-instance.sh` has nothing to say about it. What must hold is the **SSE
stream**: raw CLI JSON lines passed through verbatim plus Agento's two synthetic
events (`user_input_required`, `permission_request`). `Event::raw` is what makes
that possible and is why every message keeps its bytes.

The tests are a **scripted fake CLI** (`tests/claude_sdk.rs`) — a Python program
that logs every stdin line and replies to order. That is the only way to test
the things that are properties of a *sequence* rather than of a function, and
all four failure modes below are silent without it.

Four protocol facts that cost real time, all of them re-discoverable only the
hard way:

- **The handshake order is load-bearing.** Reader task live → register the
  request id → write `initialize` → *block* on the acknowledgement → only then
  the first user message. A `control_response` cannot be routed before something
  reads stdout, and MCP servers, agents and hooks are configured during the
  acknowledgement. Getting it wrong races rather than fails.
- **`sdkMcpServers` is never sent.** The CLI accepts only an array of strings
  there, and a rejection fails the *entire* initialize — silently taking hooks,
  agents, the system prompt and the output format with it. Naming a server there
  also marks it SDK-hosted, so the CLI drops its transport and routes tool calls
  back over `mcp_message`, which this SDK does not implement. Every MCP server
  travels as `--mcp-config` instead.
- **`control_response` routes on the *nested* `response.request_id`**, and the
  caller gets the *innermost* payload. There is no top-level fallback; inventing
  one is what broke this before. An absent inner payload is a success with no
  data, not an error.
- **Every inbound `control_request` must be answered.** A missing reply hangs
  the CLI with no error on either side — including for the requests we only
  acknowledge. `can_use_tool` with no handler is answered with an *error*, never
  an allow: fail closed.

Four places where a mechanical port would have been wrong, each documented at
its site: Go's partial-decode semantics (`lenient.rs`), the `Stream` /
`StreamControl` split (reading a channel needs `&mut self`), async callbacks
(Go blocks a goroutine inline; blocking a runtime worker is a bug), and handle
lifetimes standing in for `context.Context`.

### Hosting a tool — there is exactly one way (#282)

**Decision: `rmcp`, the official Rust MCP SDK, and the hand-rolled `McpService`
trait is gone.** `start_in_process_mcp_server(name, service)` takes an
`rmcp::ServerHandler`; `claude::new_tool` / `claude::ToolServer` /
`Options::with_tools` (`claude/tool.rs`) are the typed-tool layer over it, ported
from `claude/tool.go`. Every phase-4 integration and the local-tools server build
a `ToolServer` and hand it to `start_in_process_mcp_server`. **Do not add a
second path** — that is the whole point of settling this before #310–#317 start.

#281 deferred the choice on purpose, because nothing had a server to host and
binding to `rmcp`'s API would have satisfied no caller. Phase 4 is what forces
it, and the evidence came down one way:

- **The 62 tools all want a derived schema.** Every Go integration is
  `mcp.AddTool(server, &mcp.Tool{…}, handler)` over a params struct with
  `jsonschema:` tags — the schema is reflected off the type. Keeping the trait
  meant hand-writing 62 JSON Schemas beside 62 Rust structs and keeping them in
  step by hand, plus hand-rolling initialize, capability and protocol-version
  negotiation, `tools/list`, `tools/call`, the error codes and the content
  encoding. `schemars` (which `rmcp` already pulls) is that whole job.
- **It costs almost nothing.** `rmcp` 3.1.2 with `server` +
  `transport-streamable-http-server` adds **nine packages** — 569 → 578 in
  `Cargo.lock` — and no measurable build time against a Tauri/GTK tree. Its
  async and HTTP halves are already here: `tokio`, `http`, `http-body(-util)`,
  `tokio-stream`, `tokio-util`, `chrono`, `uuid`, `serde_json`, `thiserror`,
  `base64`, `tracing`, and `schemars` 1.2.2 was **already in the lock** (three
  schemars majors arrive via Tauri). The nine are `rmcp`, `sse-stream`,
  `futures`, `pastey`, `rand`, `rand_core`, `chacha20`, and — because enabling
  `schemars`'s derive is what `#[derive(JsonSchema)]` needs —
  `schemars_derive` 1.2.2 and `serde_derive_internals` 0.30 alongside the 0.8 /
  0.29 copies Tauri already brought.
- **It mirrors Go rather than diverging from it.** Go delegates the protocol to
  `modelcontextprotocol/go-sdk` and keeps only the listener. This does the same
  with the same project's Rust SDK, so the seam stays where Go's is.

Consequences, each load-bearing:

- **The crate's MSRV moved 1.77 → 1.88**, because `rmcp` 3.x declares 1.88 and
  cargo otherwise silently resolves `rmcp` 2.x, a superseded major. Both CI
  workflows install `stable`, so nothing was holding the floor at 1.77 — it was
  the Tauri template's value. It is not free: clippy's MSRV-gated lints wake up,
  which is why three `map_or(true, …)` sites became `is_none_or` in the same
  change. Expect that whenever the floor moves. Note the floor is **declared,
  not enforced**: with `stable` in both workflows and no `rust-toolchain.toml`,
  nothing ever compiles at 1.88, so a 1.89-only API would land green. What
  `rust-version` buys is the `rmcp` 3.x resolution and clippy's MSRV lints.
- **The `macros` feature is off.** `#[tool_router]` / `#[tool]` fix a tool set at
  compile time, and all seven of Agento's in-process servers choose their tools
  at **runtime** from the integration's `services[].tools` allowlist, over
  credentials read from the database. `ToolServer::add_tool` is that loop.
  Leaving the macros enabled would give the tree two ways to declare a tool for
  no caller's benefit.
- **A tool's error is text the model reads, never a protocol error.** Go's
  `mcp.AddTool` uses `ToolHandlerFor`, which packs a returned `error` into
  `CallToolResult.Content` with `IsError` set — which is why every one of the 62
  handlers reads `return nil, nil, fmt.Errorf("github: …: %w", err)` and why the
  model retries on that text. So `new_tool`'s handler returns
  `Result<CallToolResult, String>` and the wrapper builds
  `CallToolResult::error` from an `Err`. There is deliberately **no way to raise
  a JSON-RPC error from a handler**, exactly as there is none in Go: `rmcp`
  renders one as "Tool result missing due to internal error", which tells the
  model nothing. The practical cost is that `?` needs a `String`, so every
  fallible call carries `.map_err(|e| format!("…: {e}"))` — the same context
  `fmt.Errorf` supplies, and the same message the model gets.
- **A handler takes the call's `CancellationToken`.** Go threads `ctx` into
  every `http.NewRequestWithContext`, so a cancelled turn aborts the outbound
  call. Rust does not inherit that: `rmcp` spawns a handler detached and
  cancelling a request only cancels its token. A handler that cannot see the
  token runs to completion, so a dropped `InProcessMcpServer` would leave
  in-flight Slack/GitHub/Google calls alive in orphaned tasks. It is in the
  signature from the start because widening it after #310–#317 is a 62-site
  edit.
- **The transport is stateless** (`json_response: true`,
  `legacy_session_mode: false`, and `NeverSessionManager` so the map is gone
  rather than merely unreachable), against `rmcp`'s session-based default and
  against Go's stateful handler. An in-process tool server has no
  server-to-client traffic — no sampling, no elicitation, no progress — so a
  session buys nothing and costs per-session state in seven servers. It also
  keeps the module's contract literally what #281 wrote down: one POST, one JSON
  reply, `202` for a notification. The two things a client can notice — no
  `Mcp-Session-Id`, and `405` for the stream `GET` — are asserted in `mcp.rs`'s
  own tests, because a POST behaves the same in either mode and nothing else
  would catch a regression. **It is also verified against the real client** —
  `tests/claude_mcp_live.rs` (`--ignored`) has the Claude Code CLI dial one and
  report `✔ Connected`. Re-run it if `server_config()` ever changes.
- **Every server requires a bearer token, which Go's does not.** This is the one
  deliberate divergence. Go binds an unauthenticated loopback port; from phase 4
  that port answers `tools/call` with the user's live Slack, GitHub and Google
  credentials, and loopback separates hosts, not processes — any other program
  running as the user could call it. The browser is already shut out (the
  transport needs non-safelisted headers, so a page's `fetch` is preflighted and
  gets a bare `405`; `allowed_hosts` blocks DNS rebinding), but nothing stopped
  a local process. So `start_in_process_mcp_server` mints a random token per
  listener and requires `Authorization: Bearer …`, carried in `McpHttpServer`'s
  `headers` — a field that existed and was always empty. The CLI sends
  configured headers on every request; verified against 2.1.224 and covered by
  the live test. Know its limit: `--mcp-config` is inline JSON in the
  subprocess's argv, so the token is readable from `/proc/<pid>/cmdline`. What
  it closes is the caller that can only speak HTTP to a port it found; code
  already running as this user was never in scope, since it can read the
  integration credentials straight out of `agento.db`.
- **Every tool handler's HTTP client must set a timeout; it is what bounds
  shutdown.** Since #311 a dropped `InProcessMcpServer` shuts down
  **gracefully** — `axum::serve` waits for in-flight requests before the
  cancellation token fires — so a `tools/call` crossing a reload gets its answer
  instead of a 500. That is Go's behaviour (`Shutdown(context.Background())`,
  equally unbounded), and it means the only ceiling on the drain is the slowest
  handler. `github::client` sets 15s and nothing holds a long-lived stream
  (`legacy_session_mode: false` makes the stream `GET` a `405`), so today the
  bound is real. A handler added by #313 or #314 with no client timeout would fail
  no test while leaving a revoked credential answering `tools/call` for as long
  as its socket stays open. Set the timeout on the client, not on the drain.
- **Go's `ServeStdioMCP` / `SelfAsStdioMCPServer` are not ported**, and
  `transport-io` is off with them. Nothing in Agento calls either — a second,
  untested hosting path in the PR whose thesis is that there is exactly one
  would undo the thesis. `McpStdioServer` the *config type* stays: it describes
  an external server from `mcps.yaml`, which is somebody else's subprocess.
- **`rmcp`'s own `tracing` events now reach the log.** Nothing installs a
  `tracing` subscriber and the app logs through `tauri-plugin-log` over `log`,
  so every protocol-layer event was discarded — including "rejected request with
  disallowed Host header". `tracing` is a direct dependency purely so its `log`
  feature is on, which forwards to `log` while no subscriber exists;
  `tests/claude_mcp_tracing.rs` asserts that rather than assuming it, and would
  fail if a future dependency installed a subscriber.

`NewTool[In, Out]`'s `Out` has no counterpart: it is Go's *structured* result,
no Agento tool passes anything but `nil`, and `rmcp` puts the same thing in
`CallToolResult::structured_content`. `WithTools` returns the server handle
alongside the options, because a listener's life is a handle's here — an
`Options` that owned it would tie the port to a `Clone` value.

**The shape every ported tool takes**, and the one that does not compile: a
handler is `Fn`, so a credential must be *captured and cloned per call*, not
moved into the async block — `move |input, ct| { let token = token.clone();
async move { … } }`. Moving it in makes the closure `FnOnce`. There is no
`&self` to read from either: `ToolServer` is one shared type holding nothing
integration-specific, so capture is the only channel. `claude/tool.rs`'s module
docs carry a full worked example.

**The first real caller is `native/tools/` (#310)**, the local in-process server
— one tool, no credentials — and it is the file to read before porting an
integration. Four things are settled before the six integration ports —
three by #310 and one by #312 — and all six inherit every one:

- **`new_tool` normalizes the schema towards Go's** (`normalize_go_schema`).
  Three keys are dropped, and all three are keys `jsonschema.For` can never
  emit, so removing one can only move a Rust schema towards Go's: `$schema`
  (the dialect key `schemars` stamps on everything), `format` (`"int64"` for an
  `i64`; Go's `Schema.Format` is only ever filled by hand) and `default`
  (`#[serde(default)]` is the shape that reproduces `omitempty`, and `schemars`
  advertises the default value it implies). They are dropped by replacing the
  `Arc`, not through it: `rmcp` memoizes one schema per input type and hands
  every route a clone, so an in-place edit would reach into a process-wide
  cache. The walk is structure-aware rather than a key sweep — all three are
  legal property *names* too — and it follows every position `schemars` can put
  a subschema in, including the ones nothing has reached yet
  (`unevaluatedProperties`/`unevaluatedItems`, which 1.x emits for
  `#[serde(flatten)]` under `deny_unknown_fields`, plus `if`/`then`/`else`,
  `contains` and `dependentSchemas`): an unwalked keyword leaves a nested
  `$schema`/`format`/`default` in a schema the model reads and nothing says so.
  `#[serde(deny_unknown_fields)]` is what produces `additionalProperties:
  false`, which Go reflects onto every struct and its server *validates*
  against.
- **The reflector divergence map is generated, and it is the file to read
  before porting an integration** (#312).
  `desktop/parity/jsonschema_reflect_vectors.json` reflects one reference struct
  covering every shape class through `jsonschema.For`, and
  `src-tauri/src/claude/schema_vectors.rs` declares the corresponding Rust
  shapes and pins, per shape, whether they match and what to write when they do
  not. Two findings drive every port:
  - **`jsonschema:"required,…"` is not a directive.** `jsonschema-go` reads the
    whole tag as the property's *description*, `required,` prefix included, and
    marks a field optional only on `omitempty`/`omitzero`. No params struct in
    the six integrations writes either, so **every field of all 62 tools is
    required** and every description the model reads begins with `required,`.
    Copy the tag verbatim into the doc comment; "fixing" it changes the wire.
  - **An optional Go field is `#[serde(default)] String`, never
    `Option<String>`.** `omitempty` moves a field out of `required` and leaves
    its type alone; an `Option` renders `["string","null"]`, which is a
    different type in front of the model.

  Three divergences are left standing, each unreachable from #312 and each
  documented at its site with what a port must do instead: a Go slice renders
  `["null","array"]` (nothing in the integrations takes a list — every
  multi-value input is a comma-separated `string` through `splitCSV`), a nested
  struct is inlined by Go and lifted to `$defs`/`$ref` by `schemars` (every
  params struct is flat; flatten yours), and a sized or unsigned integer
  reflects as bounds in Go and as a format in Rust (use `i64`, which is what a
  Go `int` and `int64` both are).
- **Malformed arguments are the same *kind* of failure, with different
  wording.** Both servers answer a missing field, an extra field, a wrong type
  or an absent `arguments` with a `CallToolResult` carrying `IsError`, never a
  JSON-RPC error — `rmcp`'s `into_tool_argument_error` converts its own
  extractor's `INVALID_PARAMS` for exactly this. Only the text the model reads
  differs: Go's `validating "arguments": …` against `rmcp`'s `failed to
  deserialize parameters: …`. It is a property of `new_tool`, so every ported
  tool has it; there is no missing conversion to add.

  Nothing in this port implements it, though, and that is the part worth
  knowing: `into_tool_argument_error` **prefix-matches a hardcoded string**
  (`"failed to deserialize parameters:"`) against its own extractor's message,
  so an `rmcp` upgrade rewording either half would flip all 62 ported tools to
  protocol errors at once — which the CLI renders as "Tool result missing due
  to internal error", nothing for the model to retry against.
  `malformed_arguments_are_a_tool_error_rather_than_a_protocol_error`
  (`native/integrations/github/tests_vectors.rs`) sends a missing field and an
  unknown field and pins the **kind**, deliberately not the text. Every ported
  integration inherits the property from `new_tool`, so one test covers all of
  them; add another only if a port stops going through `new_tool`.
- **A tool's name is not renameable.** `mcp__local-tools__current_time` is in
  agents' stored `capabilities.local` allowlists and in every `tool_use` block
  already written to `chat_messages`. `desktop/parity/local_tools_vectors.json`
  pins the server name, the tool names, the descriptions and the schemas, taken
  from a **live `tools/list`** against the Go server rather than from its source;
  it also pins `current_time`'s answer text across seventeen zones and two
  instants, because that text lands in a stored `tool_result` and depends on the
  tz database's abbreviations (`+0545`, `-05`) agreeing between Go's tzdata and
  `chrono-tz`'s. Regenerate with
  `go test ./desktop/parity/ -run TestLocalToolsVectors -update-local-tools-vectors`.
- **The listener is per turn, not per process.** Go starts its one server in
  `cmd/web.go` and shares it; here the handle *is* the lifetime, so
  `build_options` starts one and hands it back, and `turn.rs` drops it **after**
  `session.close()`, since dropping the listener cancels every handler's token.
  `close` does not wait for the subprocess — it flips a flag and fires a
  oneshot — so the ordering is best-effort rather than a barrier; the stream has
  already ended by then, so no `tools/call` should be outstanding either way.

One Go rule that only becomes visible once local tools exist:
`appendDisallowedTools` keys on the agent's **explicit built-in list**, not on
the allowlist it produced. An agent naming `local: [current_time]` and no
built-ins has a non-empty `--allowedTools` and still gets no `--disallowedTools`;
subtracting the allowlist from the twelve built-ins instead would deny all of
them.

### The GitHub integration (`native/integrations/github/`, #312)

The first of the six, and the largest: twenty tools over five service groups,
token auth only. It is the file to read beside `native/tools/` before porting
#313 and #314 — that one settles *how to host a tool*, this one settles *how to port
an integration*.

**Where it lives, and why there.** `native/integrations.rs` stays a file and
gains one `pub mod github;` line; the port is `native/integrations/github/`.
Rust admits a `foo.rs` beside a `foo/` directory, so this is the layout that
moves nothing — the alternative was renaming a 1,400-line module to
`integrations/mod.rs` for the same result. `ServiceConfig` is reused from it
rather than redeclared.

**It is the server, not the registry.** `Start(ctx, cfg)` refuses an
unauthenticated integration, parses `config.GitHubCredentials` and then hosts;
only the hosting is here. The first two read the `auth` and `credentials`
columns, which `native/integrations.rs` deliberately never selects — so they
live in `native/integrations/registry.rs` (#311), which owns
`Start`/`Stop`/`Reload`, `PUT`/`DELETE /api/integrations/{id}`, and the one
projection in the port that reads a credential. `start_github_mcp_server` takes
the token from it. `auth.go`'s `ValidatePAT` is still unported — its route
(`POST /api/integrations/{id}/auth/validate`) dials GitHub and stays with Go, so
it would be dead code, which trips clippy.

**The gating rule reads backwards, and it is the thing a port gets wrong.** A
service registers when its row says `enabled`; within it, a tool registers when
the union of *every enabled service's* `tools` list names it — **or when that
union is empty**. So all-enabled-with-no-lists hosts all twenty, and one name
anywhere narrows every service at once. Both halves are in the vectors.

**Four surfaces are pinned, not one.** `desktop/parity/github_vectors.json` is
taken from the real Go server over its real MCP transport, against a fake GitHub
that **records the request each tool built**: the hosted tool set, each
description and input schema, the request (method, encoded target, headers,
body) and the result text of every success and every failure path. The request
half is what pins the things no response reveals — `url.PathEscape` per segment,
`url.Values.Encode`'s sorted keys and `+`-for-space, the per-page clamp, and
`json.Marshal`'s sorted keys and HTML escaping in every request body. Regenerate
with
`go test ./desktop/parity/ -run TestGitHubVectors -update-github-vectors`.

Five things it brought that #313 and #314 will want:

- **`gourl.rs` now has all three of `net/url`'s escaping modes.**
  `url.PathEscape` is `encodePathSegment` and escapes `/ ; , ?`;
  `url.QueryEscape` is `encodeQueryComponent` and escapes everything reserved
  **plus a space as `+`**. `form_urlencoded` — already in the tree — matches
  neither: it escapes `~` where Go does not and keeps `*` where Go does not.
  `gourl::Values` is `url.Values` restricted to `Set`, which is all any
  integration uses. All pinned in `gourl_vectors.json`.
- **A request body is a `BTreeMap` through `gojson::to_vec_marshal`**
  (`github/body.rs`), which is what reproduces `json.Marshal` over a Go map:
  sorted keys and `\u003c`/`\u003e`/`\u0026`. Watch the conditions — Go writes
  a key only when it is non-empty or true, so `draft: false` sends **no key**,
  an all-empty update sends `{}` (and therefore still sends `Content-Type`), and
  a `labels` string of only separators sends `null` rather than `[]`, because
  `splitCSV` returns a nil slice.
- **The response bytes never round-trip through a JSON value.** Every success
  sentence interpolates GitHub's own body verbatim; decoding and re-encoding
  would reorder its keys and respell its numbers.
- **A cancelled call answers Go's own sentence.** In Go a cancelled `ctx` is how
  `client.Do` fails, so `calling GitHub %s %s: request failed` is what the model
  reads there too — no divergence to invent. Every outbound call
  `tokio::select!`s on the token, because `rmcp` spawns handlers detached.
- **`reqwest` gained a TLS backend, and it reads the *platform* trust store.**
  Every hop this shell made was loopback until now, so none was configured and
  `https://api.github.com` would have failed at runtime. rustls rather than
  `native-tls` for the reasoning already written on `lettre` (five release
  triples, no C toolchain) — but `rustls-tls-native-roots`, **not** the
  `rustls-tls` alias, which is a Mozilla snapshot compiled in. Go's `net/http`
  reads the platform store, and the case bundled roots break is exactly the one
  they are usually chosen for: a TLS-inspecting corporate proxy intercepts
  `api.github.com` like anything else and its CA exists only in the system
  store, so the web UI's integration would work and the desktop app's would
  answer `request failed` with nothing to point at the cause. It costs no new
  crate (`rustls-native-certs` was already in the tree through
  `tauri-plugin-updater`'s `reqwest 0.13`) and still resolves to `ring`.
  `lettre`'s webpki roots are #307's decision and are deliberately left alone.
  One consequence to keep: `reqwest` loads the roots inside `build()` and
  reports an unusable store as a **builder** error, where Go's `&http.Client{…}`
  is a struct literal that cannot fail — so `client.rs` holds
  `OnceLock<Option<Client>>` and answers `calling GitHub …: request failed`
  rather than panicking inside a handler `rmcp` spawned detached.
- **`reqwest` gained `gzip`.** Go's transport adds `Accept-Encoding: gzip` and
  decompresses transparently, so every Go-side integration call is compressed
  and every uncompressed one here was a silent divergence — most visibly
  `get_pull_diff`, whose 10 MB cap is 10 MB of highly compressible text. The
  fakes never compress, so no vector can see it. The cap semantics do not move:
  reqwest decompresses in its service stack, so `bytes_stream()` yields
  decompressed bytes and `read_capped` caps what Go's `io.LimitReader` over a
  gunzipped `resp.Body` caps.
- **The test seam is `#[cfg(test)]`, and should stay that way in #313 and #314.**
  `githubAPIBase` had to be *exported* on the Go side (`parity.go`) because
  `desktop/parity` is a different package; both Rust callers are in-crate, so
  `API_BASE`/`set_api_base` compile out of a shipped binary entirely. What they
  are is a primitive for pointing every GitHub request — each bearing the
  user's PAT — at an arbitrary host.

One fact to have before the next port, because it is easy to assume the other
way: **`tools/list` does not carry registration order.** Both SDKs sort by
name — `rmcp`'s `ToolRouter::list_all` ends in
`tools.sort_by(|a, b| a.name.cmp(&b.name))`, and `modelcontextprotocol/go-sdk`
holds tools in a `featureSet` that lists by sorted key — which is why
`github_vectors.json`'s `tools` array starts at `create_issue` rather than at
`list_repos`. Registration order is still worth keeping in `SERVICES` order so
the two `buildMCPServer`s read alike, and it is pinned by
`an_empty_allowed_set_hosts_every_tool` against `GITHUB_TOOL_NAMES` — but a
`tools/list` comparison is set equality, not an order assertion.

Four divergences are pinned rather than reconciled, all in the vectors:

- **`encoding/json`'s syntax-error vocabulary.** `trigger_workflow` parses a
  caller-supplied document, and Go's `invalid character 'o' in literal null
  (expecting 'u')` has no `serde_json` equivalent. Every *well-formed* document
  matches exactly — including `null` parsing to a nil map with no error, and a
  null value decoding to `""` — and a *truncated* one matches too
  (`unexpected end of JSON input`). The vector's `rust_text` field carries the
  one that does not.
- **The Go MCP SDK rounds an integer argument above 2^53.** `mcp/tool.go`
  unmarshals `arguments` into a `map[string]any`, applies schema defaults and
  re-marshals before the typed decode, so an `int64` reaches a Go handler
  rounded; `rmcp` deserializes straight into the input struct and does not.
  Carried by the vector's `rust_target` field. Unreachable with a real GitHub
  run id, and deliberately not reproduced — degrading Rust to match would be
  worse than the divergence.
- **A zero-fraction float is an integer to Go and not to serde.** Same
  mechanism, and far more reachable: that same `map[string]any` round trip
  *validates* against the reflected schema, where JSON Schema counts `30.0` as
  an `integer`, and re-marshals `float64(30)` back as `30`, so the typed decode
  succeeds. `serde_json::from_value::<i64>(Number(30.0))` fails outright, and
  `{"per_page": 30.0}` is something models emit — six of the twenty tools take
  an integer. Accepting it would mean a newtype on 21 fields whose `JsonSchema`
  has to be hand-written to stay inlined rather than lifted into `$defs`, which
  is a schema risk taken for a wording difference, so it is pinned instead:
  `rust_text` plus `rust_no_request`.
- **A `.` or `..` path segment reaches a different endpoint.** `url::Url::parse`
  — which `reqwest` builds every request through — applies WHATWG dot-segment
  removal for http(s); Go's `net/http` does not normalize and `url.PathEscape`
  leaves both alone, so `list_issues(owner: "..", repo: "..")` asks Go's GitHub
  for `/repos/../../issues` (a 404) and would ask this one for `/issues` — *the
  authenticated user's issues across every repository*, on a request carrying
  the PAT. Escaping does not help; `%2E%2E` is collapsed too. `owner`, `repo`
  and `workflow_id` are model-supplied and every tool result carries
  attacker-authored GitHub content, so it is reachable under prompt injection,
  and it applies to the write tools too. `reqwest` offers no unnormalized
  target, so `client::absolute` compares the parsed path and query against the
  ones the tool built and **refuses** rather than calling somewhere else —
  answering the site's own `calling GitHub …: request failed`. Five vectors,
  covering all three URL builders and both the read and the write path, carry
  `rust_text` and `rust_no_request`. The comparison is exact rather than a `..`
  scan, so anything else `url` normalizes is caught by construction; nothing
  legitimate trips it, because `gourl`'s escaping already covers every byte in
  `url`'s path and query encode sets. **A port needs this guard wherever model
  input reaches the path** — #316 and #317 do, and share
  `native/integrations/base_url.rs` for what a *per-row* base costs on top of it;
  #315 does not, because Slack's methods are literals.

### The Confluence integration (`native/integrations/confluence/`, #317)

The second of the six and the smallest: six tools in one service group
(`content`), over an Atlassian site URL, account email and API token. Read it
after `native/integrations/github/` — that one settles how to port an
integration, this one is what an integration adds when its API is not GitHub's.

**Five surfaces are pinned, not four.** `desktop/parity/confluence_vectors.json`
carries the four #312 established — hosted tool set, schema, request, result
text — plus `ValidateSiteURL` per input, because that is the one piece of
`Start` that is a *decision* rather than plumbing: it is what stops an `http://`
site URL carrying the user's API token in a `Basic` header over plaintext.
Regenerate with
`go test ./desktop/parity/ -run TestConfluenceVectors -update-confluence-vectors`.

Four things that will recur in #313–#315:

- **The test seam is a parameter, not a static.** GitHub has one API root, so
  `githubAPIBase` is a package variable and the Rust side gates a `RwLock` behind
  `#[cfg(test)]`. A Confluence base comes out of the row, so it is a field on
  `Client` and a parity run simply constructs one against loopback — nothing
  test-only ships at all, which is the narrower answer. Go still needs a seam,
  because `Start` refuses a plaintext URL before it builds anything:
  `internal/integrations/confluence/parity.go`'s `StartAtSiteURL` is `Start` with
  that one line removed, and it is exported for the same reason `SetAPIBase` is.
- **The dot-segment guard compares only the half a tool built.** `client::absolute`
  is `github::client::absolute`'s reasoning verbatim — `url::Url::parse` applies
  WHATWG dot-segment removal and Go's `net/http` does not, so
  `get_page(page_id: "..")` would reach `/wiki/api/v2/` (the space listing) on a
  request carrying the token. What differs is what the expected target is
  compared *against*. GitHub's base is a fixed, already-encoded string, so there
  the whole target is the `path` argument. A site URL is per row and
  **user-typed**, so it need not be encoded at all:
  `https://intranet.example.com/my atlassian` is one Go accepts and sends as
  `/my%20atlassian/…` through `EscapedPath`, and `url` encodes it identically —
  so comparing against the raw concatenation would refuse every call against a
  site URL that works. The base is therefore parsed on its own and its *rendered*
  path is the expected prefix; only the tool's suffix is compared against the
  bytes it built, which is sound because that half is fully `gourl`-encoded.
- **The base needs its own validation, and the authority half must be an
  allowlist rather than a comparison.** This is where getting it wrong sends the
  user's credentials to somebody else, so it is worth stating the shape of the
  argument. Comparing the host `url` resolved against the host `net/url` would
  have resolved catches only a disagreement about where the authority *ends*;
  where the two read the same substring and *interpret* it differently, the
  comparison is a tautology — it is the same parser on the same bytes. There are
  at least three interpretation gaps, each of which grafts the site onto an
  attacker's domain from a string that reads as the legitimate one:
  `evil.com\@acme.atlassian.net` (Go: `invalid userinfo`; `url`: host
  `evil.com`), `acme.atlassian.net%2Eevil.com` (Go: `invalid URL escape`; `url`:
  `acme.atlassian.net.evil.com`), and a NO-BREAK SPACE between two labels (Go
  keeps it literally; `url` IDNA-maps it and joins them). `parseHost` is itself
  an allowlist — `integration_credentials::split_url` says so, having enumerated
  every ASCII byte through it — so `validate_site_url` uses one too, and a
  narrower one, because that module may forward what it is unsure of and this
  one may not: ASCII letters, digits, `.`, `-`, `_`, optional numeric port,
  applied to the **whole** authority so `@` and `\` are out. Nothing in that set
  is transformed by either parser, so agreement is by construction. It refuses
  four things Go serves — userinfo, an IPv6 literal, a non-ASCII host (which
  Go's own IDNA-blind resolver cannot dial either) and a percent escape — each a
  logged non-start.
- **The path half compares against `EscapedPath()`, not against
  `escape(Path, encodePath)`.** The second is only the first's *fallback*: Go
  prefers the raw text whenever it is `validEncoded`, whose allowlist admits
  `! $ & ' ( ) * + , ; = : @ [ ] %` regardless of `shouldEscape`. So `/a!b` and
  `/a%2Fb` are sent verbatim and `url` renders them identically — comparing
  against `escape` alone refuses a base that works. `gourl::valid_encoded_path`
  is that rule; #316 needs it for Jira. The true refusals it keeps are `\` (Go
  `%5C`, `url` `/`), `^`, `|` and the dot segments. Two more shapes have no
  sound comparison at all: a base `url` cannot parse, and one carrying its own
  `?` or `#`. Every refusal Go would have served is pinned as a `rust_error`
  divergence. **Any integration whose API base comes out of the row inherits all
  of this** — #316 is the first, through `native/integrations/base_url.rs`; #313–#315
  have constant bases and need only the tool-suffix guard.
- **`SetBasicAuth` is `reqwest`'s `basic_auth`.** Both are
  `Basic base64(user + ":" + pass)` with standard (not URL-safe) alphabet. The
  vectors pin the encoded header rather than trusting that, because nothing in a
  response reveals it.
- **`net/url`'s parse failures are classified, not quoted.** `ValidateSiteURL`
  asks `url.Parse` two questions (scheme, host) and Go answers a *third* case —
  the parse itself failing — with `net/url`'s vocabulary, `%q`-quoted over the
  caller's input. That wording is not reproducible past printable ASCII, and it
  is a **log line**: `Start`'s error is logged by the registry and never reaches
  a response or the model. So the port reproduces the two refusals exactly,
  reproduces the *classification* of the two parse failures a stored site URL can
  reach (a control character; a scheme-less URL whose first path segment holds a
  colon) under its own wording, and `confluence_vectors.json` carries both as
  `rust_error` divergences. `go_scheme` and `go_host` are `net/url`'s own
  `getScheme` and authority split, written out rather than delegated to
  `url::Url::parse` — which refuses strings `url.Parse` accepts and would answer
  "invalid" where Go answers "not HTTPS".

Two smaller notes. The nested request bodies (`create_page`, `update_page`) go
through `gojson::to_vec_marshal` over `serde_json::json!`, which sorts at *every*
level and HTML-escapes — and unlike #312's, this fires on every real call, since
a page body is XHTML. And the client timeout is **30 seconds**, not GitHub's 15;
it is per API and it is what bounds a graceful shutdown.

### The Jira integration (`native/integrations/jira/`, #316)

Nine tools in one service group (`project_management`), over the **same**
`config.AtlassianCredentials` Confluence uses. Read it beside
`native/integrations/confluence/`: the two are twins, and every difference
between them is #277's, deliberately preserved.

| | Confluence | Jira |
|---|---|---|
| create-time validator | HTTPS only, keeps the raw value | http **or** https, trims trailing `/`, **re-marshals** |
| inside `Start` | `ValidateSiteURL` again | **nothing at all** |
| client timeout | 30s | 15s |
| failure sentence | names nothing | names the method and the path |
| `desktop/parity` seam | `StartAtSiteURL` needed | **none needed** |

Two of those rows change the shape of the port:

- **`jira.Start` validates nothing, so a bad base is answered per call rather
  than by refusing to host.** Go hosts the server and advertises all nine tools
  whatever the stored site URL says. Refusing to host would change the
  *advertised tool set*, which is what every agent's stored `capabilities.mcp`
  allowlist depends on — so `jira::client::Client` holds `Option<Base>` and
  answers Go's own transport sentence when it is `None`. Confluence refuses at
  `Start` because **Go refuses there too**. Same helper, opposite answers,
  because the two Go packages differ. `jira_vectors.json`'s `site_urls` block
  pins it from both ends: the tool set is unchanged and the call fails.
- **No Go-side seam.** `Start` reads the site URL out of the credentials, so the
  generator puts the `httptest.Server`'s URL there and calls the shipped
  `jira.Start`. There is no `internal/integrations/jira/parity.go`, and that
  absence is a consequence rather than an oversight.

`native/integrations/base_url.rs` is #317's site-URL work extracted for the
second caller — `Base::new` (the four base checks) and `Base::resolve` (the
per-call dot-segment guard). Read its header before porting anything else whose
API base comes out of the row; #313–#315 do not need it, because theirs are
constants.

**The base's path prefix comes from whether it contributed any path *text*, not
from what `url` rendered.** `url::Url::parse` gives both `https://x` and
`https://x/` a `path()` of `/`, while Go concatenates raw text — so
`https://x/` + `/rest/api/3/project` goes on the wire as `//rest/api/3/project`,
empty first segment intact. Deriving the prefix from the rendered path made such
a base refuse every call. It is reachable on Jira and not on Confluence, and the
asymmetry is the same one twice: `validate_site_url` trims trailing slashes
before `Base::new` sees them, while `jira.Start` trims nothing — and `Update`
validates nothing on either, so a user retyping the URL in the edit form can
store one.

Four things in `tools.go` that look like mistakes, are Go's behaviour, and are
pinned:

- **`list_projects` binds `*struct{}`** — the only tool in the six integrations
  with no fields, so its schema is `{"type":"object","additionalProperties":false}`
  with no `properties` key at all.
- **`create_issue` runs `url.PathEscape` over the project key *inside the JSON
  body***, and over nothing else, so a key holding a space is sent as `MY%20PROJ`.
- **`update_issue` and `transition_issue` discard the response** and build their
  result text from the arguments, so a 200 with a surprising body still reads as
  success.
- **`/rest/api/3/issue/` carries its trailing slash in the constant** while
  `/rest/api/3/project` does not — same wire, two spellings.

One divergence is pinned rather than reconciled, in the `site_urls` block: for a
site URL `url.Parse` rejects, Go fails inside `http.NewRequestWithContext` and
answers `creating request: parse "…": …` with `net/url`'s vocabulary and the
stored site URL interpolated. This port refuses before building anything and
answers the transport sentence — which is also the narrower of the two, since
Go's puts the site URL into text the model reads and a `tool_result` stores.

### The Slack integration (`native/integrations/slack/`, #315)

Seven tools in one service group (`messaging`), over a workspace token. The
fourth of the six, and the first that is not shaped like the three before it —
two of its differences are things no earlier port had to deal with at all.

**The token can come from the `auth` column, and that widened a projection that
deliberately never selected it.** `resolveToken` (`slack/server.go`) switches on
`credentials.auth_mode`: `bot_token` reads the credentials blob, and `oauth`
reads `cfg.ParseOAuthToken()` — the **`auth` column**, decoded as an
`oauth2.Token`. Until #315, `registry.rs`'s `HOSTING_COLUMNS` collapsed `auth` to
a boolean in SQL precisely so a stored token could not exist in this process to
be echoed, and `native/integrations.rs` still never selects it at all. `HostingRow`
now carries it. What did **not** change is where it may go: that struct still
derives neither `Serialize` nor `Debug`, it is private to the module, and only a
`&str` leaves it. There is a third arm too, and it is the one a port drops: an
**unrecognized** `auth_mode` falls back to `bot_token` when that is non-empty, so
a row whose mode was never set still works. The `tokens` block in
`slack_vectors.json` pins all three arms, plus the empty-`access_token` case that
sends a bare `Bearer` header — and both languages observe the resolved token the
same way, by reading the `Authorization` header the fake received rather than
recording the token, which would put the thing under test into the fixture.

**Nothing model-supplied reaches the URL.** The base is a constant and every
method is a literal (`conversations.list`, `chat.postMessage`), so there is no
dot-segment guard and no `base_url::Base` here — the class of problem #312 and
#317 spent their reviews on does not arise. Model input goes in a form body or a
JSON body instead. The seam is back to GitHub's shape, though: `slackAPIBase` is
a package variable, so `slack/parity.go` exports `SetAPIBase` and the Rust side
gates a `RwLock` behind `#[cfg(test)]`. Confluence and Jira needed neither,
because an Atlassian site URL is per row.

Four more Slack-shaped surprises, all pinned:

- **`ok` decides, not the HTTP status.** `readSlackResponse` checks 429 and then
  ignores the status, so a **500 carrying `{"ok":true}` is a success** and a 200
  carrying `{"ok":false}` is a failure. Every sibling gates on the 2xx range, so
  this is the one a port gets backwards.
- **Two encodings.** Five tools send `url.Values.Encode()` as
  `application/x-www-form-urlencoded`; two send `json.Marshal` as
  `application/json; charset=utf-8` — with the charset, which nothing else in the
  tree sends.
- **Every clamp differs**: 1000/100 for the two listers, 100/20 for
  `read_messages` and for `search_messages`' count, and a floor of 1 with **no
  ceiling** for `page`. Read one by one rather than generalised from the first.
- **Rate limiting is its own sentence**, interpolating `Retry-After` verbatim —
  including when the header is absent, which lands mid-sentence as an empty
  string (`retry after  seconds`).

Five of the seven tools return Slack's body **unlabelled**; only the two senders
prefix it. Timeout 60s and cap 5 MiB, the largest of the six.

**Hosting Slack opened a hole in #311's reload hook, and closing it needed a
second trigger.** Go's `handleOAuthToken` writes the token to the `auth` column
and then calls `registry.Reload`, and `startProviderCallback` supports exactly
`google` and `slack`. `registry.rs`'s header used to say that was harmless
because neither type was hosted here — true until #315. Now that `Reload` reaches
nothing, so a Slack integration authenticated by OAuth would first be served at
the next boot.

`reload_after_auth` cannot cover it: that hook hangs off a **forwarded request**,
and an OAuth token does not arrive on one — the browser delivers it to a callback
server the *sidecar* opens on its own port, which this process never sees. The
one part of the flow that does come through the proxy is the UI polling
`GET /api/integrations/{id}/auth/status` while it waits, so `reload_after_forward`
now recognises two routes and returns a `Trigger` saying which:

- `POST …/auth/validate` is an **event** — reload unconditionally, as Go does.
- `GET …/auth/status` is a **poll** — `registry::reload_if_secrets_changed`
  compares the row's current `credentials`+`auth` fingerprint against the one the
  running server was started with, and does nothing when they match. That matters
  because `reload` is unconditional by design: firing it per poll would rebind
  the port and drop in-flight `tools/call`s once a second while the dialog is
  open. The registry keeps the fingerprint (a hash, not the values) beside each
  handle for exactly this question.

It is **best-effort**: a user who closes the dialog before the flow completes is
still served only at the next boot. #318 owns the OAuth flow itself and is where
that stops being true.

### The integration registry, and the port's second ownership flip (#311)

`native/integrations/registry.rs` is `internal/integrations/registry.go`:
`start_all` at boot, `reload(id)` on every `PUT /api/integrations/{id}`,
`stop(id)` on every `DELETE`, over a process-wide `OnceLock` keyed by
integration id — the shape `native::scan::state` and `native::chat::live` use.
The handle *is* the cancel: dropping an `InProcessMcpServer` stops its listener,
so there is no second map for the `context.CancelFunc`s Go keeps.

**It is the only implementation *for the types it hosts*, and that is what let
the two writes move.** #277 left `PUT`/`DELETE /api/integrations/{id}` with Go
because Rust had no server to reload, and a native write would have persisted
the row while the sidecar kept serving the old config. What makes that sharper
than ordinary staleness is the listener: Go's
`claude.StartInProcessMCPServer` binds an **unauthenticated** loopback port and
the server closes over the credential it was started with, so a sidecar that
never hears `Reload`/`Stop` answers `tools/call` with a token the user just
revoked for the rest of its life. A security regression rather than a staleness
one.

So `AGENTO_INTEGRATIONS` is `AGENTO_SCANNER`'s sibling, added by this issue and
set by `sidecar.rs` alongside it — with one difference that is the whole design:
**it carries a list**. `off:github` means "github is somebody else's; everything
else is still yours". A bare `off` means every type, an empty list (`off:`) and
anything unrecognized mean none, so both typos fail toward hosting. On the Go
side it is a `hostedTypes` set snapshotted into `IntegrationRegistry` at
construction and consulted by `Start` (per row) and `Reload` (per stored row's
type). `Stop` is **ungated**: stopping only ever removes a server that process
started, so for a type it does not host the maps are already empty — while a
guard there would be the one thing able to make a server unstoppable.

**Why a list and not a bool: a starter is not always a pure MCP-server
constructor.** It reads as one, and for five of the six it is — so switching Go
off costs a bound port and the port is what the flip exists to remove. `whatsapp`
is the counterexample: `internal/integrations/whatsapp/server.go` opens a real
whatsmeow WebSocket, registers the live client in a package global and only then
returns a server config, and `whatsapp/status.go` reads that global. So a
process-wide off switch does not cost WhatsApp a port, it costs WhatsApp:
`GET /{id}/whatsapp/status` pinned to `{"connected":false}`, `POST
/{id}/whatsapp/reconnect` a 200 that does nothing, and a freshly QR-paired
client that never connects. Note this is *not* in tension with "WhatsApp is
dropped, not deferred" (#273): the **port** drops it, but the rows are live data
the sidecar still serves, and the sidecar is still shipped.

**The list is built in Rust and travels in the environment.** `registry.rs`'s
`HOSTED_TYPES` is the single list; `hosts_type` reads it, the starter dispatch
must cover it (`a_hosted_type_always_has_a_starter`), and `hosting_env_value`
renders it into what `sidecar.rs` sets. That shape was chosen over a constant
hardcoded on both sides because of which failure it makes impossible: the shell
is the process that *knows* what it hosts, so #313 and #314 each add one string
in one place. The failure being designed against is a Rust slack starter landing
while Go is never told to stop hosting slack — two processes on one integration —
and its mirror is the WhatsApp bug above. A hardcoded Go list would have to be
edited in lockstep with a Rust table, in another language, in another PR.

**The native writes decline what Go still hosts.** `update` and `delete` read
the row's `type` and, if it is not `hosts_type`, return `WriteError::Fallback`
so Go serves the whole request and fires its own `Reload`/`Stop`. The check is
**pre-write** — straight after the existence read, before any mutation — because
a `Fallback` forwards the request and Go re-applies it; see the invariant in
`writes.rs`. Without it the stranded-listener bug would be relocated rather than
removed. `a_write_for_a_type_the_sidecar_hosts_forwards_without_touching_the_row`
asserts both halves, the fallback and the untouched row.

**It deliberately does not gate `StartFilteredServer`.** That is what an agent
run uses: `runner.go` reads the integration row afresh per run, builds a
throwaway server and records nothing. Gating it would have broken every
integration-using chat, scheduled task and Telegram trigger the sidecar still
serves — which is now the minority, since only two of the six types are #313's
and #314's.
The Go tests assert both halves, because the switch is only safe while that
asymmetry holds.

Two consequences worth knowing before touching this:

- **`github` (#312), `confluence` (#317), `jira` (#316) and `slack` (#315) have
  starters here, and Go still hosts the rest.** A type not
  in `HOSTED_TYPES` reaches this module's own unregistered-type path — `no
  starter registered for integration type "slack"`, logged and never surfaced —
  but in practice it does not reach it at all, because the writes decline it
  first. Nothing about a Slack or WhatsApp integration changed with this issue.
- **`reload` is unconditional.** Stop then start, no diff: there is a window
  with no server and the port changes on every save. Reproduced rather than
  improved on, because "nothing changed, skip it" is a different set of live
  ports after the same sequence of requests. What is **not** reproduced is Go's
  orphan. No lock is held across the stop, the async row read and the start, so
  a `DELETE` landing in that window would have Go leave a map entry nobody reads
  and Rust leave a *bound port holding a credential* for a row that no longer
  exists — the class of thing this issue exists to prevent. `Registry::stop`
  therefore bumps a per-id generation, and `put_if_current` refuses a handle
  whose generation moved since the caller read the row; the refused handle is
  dropped, which fires its shutdown oneshot. `start_all` snapshots generations
  *before* it lists the table, because it is spawned at boot while the proxy is
  already answering. Two concurrent `reload`s were already safe —
  `HashMap::insert` drops the displaced handle.

**Not every `Reload` caller is covered by the two ported writes.** Go's `Reload`
has seven callers; `Update` and `Delete` are the ported two. Four of the
remaining five — the confluence, telegram, jira and slack validators — and
`completeOAuth` are fine *because the gate is per type*: every type they can
reach is still Go's, and `startProviderCallback` supports exactly `google` and
`slack`, so an OAuth completion never concerns a hosted type. That leaves
`validateGitHubPATAuth`, behind `POST /api/integrations/{id}/auth/validate`,
which writes a credential for a type the shell hosts and cannot tell it. Without
a hook a GitHub integration would only be hosted at the *next boot's*
`start_all`. So the seam runs in the other direction for that one route:
`native::after_forward` (called by the proxy with Go's status in hand) fires
`registry::reload_after_auth` on a 2xx, spawned rather than awaited — which is
what Go's own `reloadIntegration` does with it. `integrations::reload_after_forward`
is the route predicate and the place to read why the list is one entry long.

**Shutdown is graceful, and it took one line's placement.** Go stops a server
with `httpServer.Shutdown(context.Background())` and hands each tool handler the
*HTTP request's* context, so a `tools/call` in flight when the server is torn
down runs to completion and its response is delivered. `claude/mcp.rs` used to
fire the transport's `CancellationToken` **as** the graceful-shutdown signal —
and that token is the parent of every tool call's, so the in-flight outbound
request was aborted and the client got a 500 instead of a result. It now fires
after `axum::serve` returns, which keeps the teardown (a detached handler cannot
outlive its listener) without the abort.

That window is not exotic: an unconditional reload on every integration save
means a model mid-`create_issue` when the user hits Save is the ordinary case.
`a_tool_call_in_flight_survives_the_handles_drop` pins it, and it fails with the
two lines swapped.

**The runner refuses less, not nothing.** `chat/runner.rs::build_options` now
serves an agent whose `capabilities.mcp` names only github integrations,
starting one filtered server per name and handing the handles to the turn beside
the local-tools one. Three things still forward: a name whose row is not
`github`, a name with **no** integration row (Go would still resolve it from
`mcps.yaml`), and — broader than it strictly needs to be — *any* `mcp`
capability at all when `<data dir>/mcps.yaml` exists, because
`resolveServerConfig` consults that file **before** the integrations and this
build reads none. The server is registered under the **bare integration id**,
not `github::server_name`'s `github-<id>`: the latter is `mcp.NewServer`'s
implementation name and never appears on a tool, while the id is the
`mcpServers` key and so the prefix on every qualified name already in an agent's
allowlist.

**Reading a credential is this module's job and nobody else's.** The rule in
`native/integrations.rs` — `credentials` is never selected, `auth` collapses to
a boolean in SQL — still holds there, and `registry.rs` is where the exception
lives: its own `HOSTING_COLUMNS` projection into a `HostingRow` that derives
neither `Serialize` **nor `Debug`** (a `{row:?}` in a log line is the same leak
with a longer fuse), private to the module, with only a `&str` ever leaving it.
A credential that fails to decode reports line and column and never the serde
message, which quotes the offending value — the forwarding
`native/integration_credentials.rs` already established. Note the native `PUT`
does not read a credential at all: it rewrites `auth` **from itself in SQL**
(`auth = CASE WHEN auth IS NOT NULL AND auth != '' AND auth != 'null' THEN auth
ELSE NULL END`), which both preserves the token without holding it and
reproduces Go's one real effect there — a column holding `''` or the literal
four bytes `null` becomes SQL `NULL`, because `Save` writes `authJSON` only when
`IsAuthenticated()`.

**Three things about the `PUT` that read as bugs and are Go's behaviour**, all
pinned by `parity_writes::the_integration_id_write_answers_match_go` against a
live server: it runs **no** credential validation (`validateIntegrationCredentials`
is `Create`'s alone, so an empty name, an empty type and a `{}` blob are all
200s); a request omitting `credentials` **wipes** them, because the store's
upsert overwrites the column wholesale; and an omitted `services` is stored and
returned as `null`, not `{}`, because `Update` skips the `make(...)` that
`Create` does.

**Do not open the live database with a bare `rusqlite::Connection::open`.** It
opens `READWRITE|CREATE`, and against a WAL database the Go sidecar currently
holds it was observed to reset the log — a row created two API calls earlier was
gone from *Go's own* view immediately afterwards. `native::db::open_read_only`
and `open_read_write` (the helpers the app's own handlers use, pragmas and busy
timeout included) are fine; the parity suites use those.

### Things that will bite

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
- **Time bucketing happens in the request's timezone** (`tz` param), while
  storage stays UTC. Always send `tz`; omitting it falls the dashboard back to
  UTC silently.
- **Session pagination is keyset, not offset.** The cursor encodes the sort, so
  changing `sort` invalidates it — reset the cursor or get a 400.
- **Cache invalidation is multi-dimensional**: TTL (1h), `scanner_version`,
  pricing revision fingerprint, and idle-threshold drift each force a re-read.
- **Chat SSE is a POST response**, so `EventSource` cannot be used. Events are
  the raw Claude CLI JSON lines passed through verbatim, plus two synthetic
  ones Agento adds (`user_input_required`, `permission_request`).
- **`AskUserQuestion` is answered by *denying* the tool** with the user's text
  in `Message` — that is how the answer reaches the model. Not a bug.
- **There is no terminal SSE event**, and `result` can arrive more than once in
  one request (an `AskUserQuestion` keeps the stream open past it). End the
  turn on stream close, never on `result`.
- **Mid-stream failures arrive as `result` with `is_error: true`**, not as an
  `error` event — HTTP 200 is already committed by then.
- **A turn that produced no final text persists nothing** — not even the user
  message. Keep locally-produced turns in memory until a fresh server
  transcript for that chat loads.
- **Secrets are stored in plaintext** in `integrations.credentials` / `.auth`.
  Protection is perimeter-only (loopback bind + directory perms). Do not
  introduce a UI that echoes them back; the API scrubs them and the UI must not
  reintroduce them.

### Wire-format traps (found the hard way — do not re-discover)

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
- **`PUT /integrations/{id}` destroys credentials on a scrubbed round-trip.**
  `integrationService.Update` explicitly preserves `Auth` ("unless the caller
  provides a new one") but does **not** do the same for `Credentials`, which it
  replaces wholesale — while `GET` scrubs them. So read-then-write wipes the
  stored secrets. Reproduced live: a working Telegram token went from
  `invalid bot token: … Unauthorized` (reaching Telegram) to
  `credentials are empty` after one such PUT. The reference web frontend has
  this flaw. **This UI refuses to save an integration with pending changes
  until credentials are re-entered**, rather than silently wiping them —
  do not "simplify" that away.
- **Validation errors are 422**, conflicts 409 — not 400.
- An invalid `sort` on the sessions list is silently accepted (falls back to
  `recent`); only a cursor/sort mismatch 400s.

### CSS trap

`.card` sets `overflow: hidden`, so as a flex child of a scrolling column its
min-content height collapses to zero and everything below the fold becomes a
sliver. Dashboard containers need `> * { flex: 0 0 auto; }`.

### Phase 3: the write path (#274)

**A route moves only when Rust can reproduce *every* effect it has.** That is
the whole rule, and it is what decides the split — not how hard the SQL is.
Most of the ~50 write routes do something besides writing a row, and that
something belongs to another issue: task writes register cron entries (#275),
chat turns spawn a subprocess and hold in-memory channels (#276), integration
writes reload MCP servers and call Telegram (#277), `/refresh` drives the
scanner (#289). Porting `POST /api/tasks` without the scheduler would store a
task that never fires — worse than not porting it.

So the writes that moved are the ones that are only rows: the agent CRUD, the
chat CRUD (**not** `/messages`, `/input`, `/permission`, `/stop`), and the two
job-history deletes. Everything else still forwards, and the `claims` tests in
each module say which and why.

### The write surface is enumerated, not described (#296)

#293 accounted for the deferred writes **by category** — scheduler, chat
execution, integrations, scan. That reads well and it cannot be audited: nothing
said whether the categories covered every route, and two escaped all of them
(`POST /api/fs/mkdir` and `PATCH /api/claude-sessions/{id}`, both since ported).
A prose table has the same problem one release later, so the table is
**generated and cross-checked** instead:

- `desktop/parity/write_routes_parity_test.go` runs `chi.Walk` over the router
  **`server.New` actually builds**, reached through `Server.Routes()`, and
  classifies every non-GET route against a `dispositions` map. Asking the router
  rather than rebuilding its mounts is deliberate: an earlier version
  reconstructed them by hand and missed the webhook, and the next root-level
  mount would have escaped the same way. Zero-value dependencies are enough,
  because nothing is dereferenced at construction and `Mount` registers every
  route unconditionally. A route the
  router has and the map does not classify **fails the Go suite**; a
  classification naming a route the router no longer has fails it too.
- `desktop/parity/write_routes.json` is what that produces: method, route,
  `status`, the owning issue, and the one-line reason. For a deferral the reason
  is the *effect Rust cannot reproduce*, because that is the only thing #274's
  rule turns on. `status` is `native` | `deferred` | `dropped` rather than a
  boolean, because **`deferred` and `dropped` are not the same answer** — the
  WhatsApp routes are waiting for nothing — and a file meant to be queried has
  to say so in a field rather than inside a sentence.
- `native::tests::every_write_route_matches_its_recorded_disposition` reads the
  same file and asserts each route's real `claims()` matches its `status`, so a
  route cannot be claimed or unclaimed without the file moving.

Regenerate with `go test ./desktop/parity/ -run TestWriteRoutes
-update-write-routes`, *after* deciding about whatever changed. Read the file
for the per-route detail; the standing summary is:

**Native (37 of 51).** Agent and chat CRUD and the job-history deletes (#274); the
chat turn and its three steering routes (#276); `POST /api/integrations` and the
three trigger-rule writes (#277); `/claude-sessions/refresh` (#289);
`PUT`/`DELETE /integrations/{id}` (#311); the three
pricing rate writes (#306); the two notification writes (#307); `POST /uploads`
and `/claude-sessions/{id}/continue` (#308); `PUT /monitoring` and
`/monitoring/test`, which answer **501** (#309); the Claude settings file and
the five profile writes (#327); and `POST /fs/mkdir` plus
`PATCH /claude-sessions/{id}` (#296).

**Deferred (12), each by the effect Rust does not own.** The five task writes
register or unregister a cron entry (#275). `PUT /settings` resolves
through a snapshot the sidecar still holds, with no off switch for the Go half
(#305). Five more talk to somebody else's
server — the two `auth/*` routes and the three Telegram `webhook/*` ones. The
twelfth is `POST /webhooks/telegram/{id}`, the one write outside `/api`:
inbound from Telegram, authenticated by its own secret token rather than by the
two guards, and its effect is an agent run through the dispatcher — which is
#275's executor by another name.

**Dropped (2), which is not the same as deferred.** The two WhatsApp routes are
`dropped` in the table — a status of their own, because the reason will never be
resolved: `whatsmeow` is not being ported, and they die with the sidecar rather
than moving (#273).

The walk covers **both** mounts, not just `/api`: `internal/server/server.go`
also mounts the Telegram webhook handler at the root, and
`POST /webhooks/telegram/{id}` is a write with a large effect — it matches a
trigger rule and dispatches an agent run. Walking `/api` alone would have left it
unclassified, which is the same shape of miss this section exists to end.

GET routes are deliberately out of the table. A read that misses is a fallback,
not a double-apply, so it carries none of the hazard the rule exists for.

**The pricing rate writes are the exception that proves the rule (#306).** They
are only rows too — but a rate edit has an effect outside the table: #188's
per-session costs are *stored*, so they keep the pre-edit figure until every
transcript is re-read. Go's `afterRateChange` invalidates its cache and lets the
next read's freshness gate do it; since #289 that scan is the shell's, so
`native::scan::after_pricing_change` is the other half of the port rather than a
nicety. It runs after the commit and is best-effort by construction: an error
there would forward the request to Go and write the rate a second time.

Three things about that surface a port collapses by accident, each recorded at
its site in `native/pricing.rs`: add and correct are **two endpoints, not an
upsert** (and the 409 carries the colliding row, so it is not the bare
`{"error": …}` every other conflict is); `effective_from` is truncated to
seconds or the read-back after the save finds nothing; and `UpsertRate` **clears
the rate's bands**, because `Rate::price` picks a band before applying any price
and a correction that left them would save and then change nothing.

Four things the write path establishes:

- **`db.rs::open_read_write` sets its pragmas per connection.** WAL is
  persistent in the file, but `busy_timeout`, `foreign_keys` and `synchronous`
  are not. Go gets away with setting them once because `SetMaxOpenConns(1)`
  means it has exactly one connection; a second process does not inherit them.
  Missing `foreign_keys=ON` is the quiet one — `ON DELETE CASCADE` stops firing
  and a deleted chat leaves its messages behind.
- **Rust verifies the schema version; it does not apply migrations.** Go's
  `applyMigrations` reads the current version *outside* the transaction that
  applies the next one, so two processes migrating together both try the same
  version and the loser's DDL fails, taking its startup with it. While the
  sidecar is bundled, exactly one process may migrate and it is Go.
  `migrate::apply` is written and tested and turns on with #278.
- **`Mode::Diff` never runs a write.** Shadow mode runs both implementations and
  compares them; for a mutation that applies it twice. `native::may_serve` makes
  it a blanket rule over the method rather than a per-endpoint flag someone
  forgets. Writes are verified by unit tests and a scratch instance instead.
- **A write must fail before it mutates.** `Err` still means "forward to Go", so
  a handler that half-applied something and then errored would have it applied
  twice. Every handler validates, checks the schema, and does the whole mutation
  in one transaction.

Two Go behaviours the port reproduces rather than improves:

- **Deleting a missing agent or chat is a 500, not a 404** — the store returns a
  plain error, so `httpErr`'s `NotFoundError` arm never fires. This port does
  not reproduce 500s: it detects the case, writes nothing, and forwards.
  Job-history's delete *is* a real 404, because its service checks first.
- **`serde` deserializes a struct from a JSON array**, positionally. Go does
  not. Without the object check in `writes::decode_body`, `POST /api/agents`
  with a body of `["My Agent"]` would create an agent on a request Go answers
  with a 400. A `null` body, conversely, is a *no-op* to Go — zero value, no
  error — so it reaches the handler and fails validation with a 422.

The migrations are **not transcribed**: `desktop/parity/migrations_vectors.json`
is generated from Go (`go test ./internal/storage/ -update-migration-vectors`),
asserted against the slice by `internal/storage/migrations_vector_test.go`, and
embedded by `native/migrate.rs` with `include_str!`. Adding migration 28 without
regenerating fails Go's own suite — and the file is what records the schema once
the Go server is deleted.

### Phase 5: the chat turn (#276)

**The seam grew a second registry.** `Answer` is a buffered `Vec<u8>` and
`Endpoint::serve` is a sync `fn` on `spawn_blocking` — right for thirteen areas
that hand back a finished document, wrong for a turn that lasts as long as the
model talks. `StreamEndpoint` is async and returns a `Response<Body>` built from
`Body::from_stream`. `native::claims` is the union of both, because the proxy
asks one question; `route_is_native` excludes the streaming ones so the buffered
path cannot try to answer a chat turn with a `Vec<u8>`.

**The four routes share a process-local registry, so they moved together** —
`/messages` puts a session in, the others look one up. But not every chat *can*
run natively: an agent whose tools come from an integration this build cannot
host still needs #313 or #314, and `runner::build_options` refuses those before
any subprocess exists. (Parts of that refusal are gone — the **local** server
(#310) and any agent naming only integrations in `HOSTED_TYPES`: **github** since
#311, **confluence** since #317, **jira** since #316, **slack** since #315. `build_options`
starts each of them, and is `async` for that reason, returning the listener
handles alongside the options because dropping one stops its server.) That would strand `/stop` for a chat still running on Go, so
the three steering routes answer natively **only when Rust holds a live session
for that chat** and forward otherwise. Go then answers — correctly, because it
is the side that has the session.

Five rules that are silent when broken, all pinned by `tests/chat_turn.rs` —
including, since #298, the deny-with-the-user's-text half of `AskUserQuestion`.
That one was the gap the claim used to paper over: the suite drove
`AskUserQuestion` as an assistant `tool_use` block, which reaches
`extract_ask_user_question` and the post-result continuation and **not the
permission handler at all**. The fake CLI issues a real `can_use_tool` control
request now, and the assertion is on what the SDK wrote *back* — the whole
observable effect of that round trip is a `control_response`, so the fake CLI
logs every stdin line and the tests read it. Reverting the deny to an allow
leaves every frame unchanged and fails only there.

Covered with it: `wrap_permission_handler`'s allowlist (a tool the agent does not
name is denied **without a prompt** — the absence is the assertion), the
`AskUserQuestion` bypass of that allowlist, and the `permission_request` frame
with its allow and deny answers.

- **`result` is not terminal.** With an `AskUserQuestion` pending the same
  subprocess carries on, so one HTTP request spans several turns and several
  `result` frames. The turn ends on stream close, on an error result, or on a
  final result with nothing pending.
- **A mid-stream failure is a `result` with `is_error: true`**, never an `error`
  event — the 200 was committed before the first frame.
- **An event with no raw line emits nothing.** The SDK synthesizes process
  failures that way, so a crashed subprocess tells the client nothing and the
  stream just ends. Reproduced, not "fixed".
- **`AskUserQuestion` is answered by *denying* the tool** with the user's text as
  the message. That is how the answer reaches the model without the tool running.
- **A turn with no final text persists no messages** — not even the user's — but
  the session row is still written: `updated_at`, the token totals, and on a
  first message a title derived from a message that was never stored.

**The model a turn runs is the agent's or the no-agent branch's — never both**
(#299). `resolveAgentConfig` branches on whether the chat **names an agent**, not
on whether that agent has a model: it returns the agent's config outright, and
`runner.go` then sets a model only when `agentCfg.Model != ""`. So an agent with
an empty model gets **no model option from Agento at all** — the SDK's own
default (`claude-sonnet-4-6`, the same constant in both SDKs) is what reaches the
CLI — and the session's model and the user's default are read only in the
no-agent branch. `RunSpec.no_agent_model` is a closure for that reason: resolving
it eagerly loaded the settings row on every turn of every agent chat to throw the
answer away, *and* treated an agent's empty model as a request for a default Go
would never have given it.

It is named for the branch and not for the fallback because
`Options::fallback_model` already means something else in the same function — the
CLI's `--fallback-model`, for when the *primary* model is unavailable.

**Read the user's default through `settings::resolve`, never `load_stored`.** Go
reads `settingsMgr.Get()`, and `SettingsManager.load` fills `"sonnet"` when
nothing is stored *before* `applyEnvOverrides` applies `AGENTO_DEFAULT_MODEL` /
`ANTHROPIC_DEFAULT_SONNET_MODEL`. The raw `SELECT` has neither: a user who had
never saved settings ran on the SDK default rather than `sonnet` — two different
strings — and one who exported `AGENTO_DEFAULT_MODEL` had it silently ignored.
`resolve` is the documented mirror of `Get()`, and every other caller in the port
already goes through it.

**One `user_settings` read per turn, and the two resolutions on top of it are not
the same** (#340). Go needs no equivalent: `settingsMgr.Get()` is an in-memory
snapshot, so the config dir and the default model are free reads of one value.
This port has no manager, so each consumer opened its own read-only connection
and decoded the same row — twice for an ordinary turn, three times for one pinned
to a named settings profile, on the latency path of every message. #299 removed
the eager *model* read; it did not remove the config-dir one, and the PR should
not be read as having done so.

`runner::TurnSettings` is the shared load: a `OnceLock` over the **stored** row,
carried on `RunSpec` and shared with the `no_agent_model` closure via an `Arc`.
The value is not that it saves a connection — it is that **two consumers of one
row with different fields is the shape that drifts**, which is what #339's review
found when one of them read `load_stored` where Go reads the resolved settings
and the other already went through `resolve`. Putting the two accessors side by
side makes the asymmetry legible, and it is a real asymmetry rather than an
oversight:

- `default_model()` is `settingsMgr.Get().DefaultModel`, so it goes through
  `settings::resolve`.
- `run_config_dir()` is `config.ClaudeRunConfigDir`, which reads
  `claudeDirs.runOverride` — the value `ApplyClaudeDirs` **stored** — and applies
  `CLAUDE_CONFIG_DIR` itself, ahead of it. Handing it the *resolved* row instead
  would diverge for a `CLAUDE_CONFIG_DIR` that is set but not absolute: `resolve`
  overwrites the field with it, `absolute_dir` then rejects it, and a stored
  absolute dir Go would have used is skipped for the default.

So the shared thing is the stored row, and `resolve` is applied where Go applies
it and nowhere else. Two other properties are pinned by counting loads rather
than argued: a turn reads the row **at most once** however many fields it wants,
and an agent carrying an **absolute** `claude_config_dir` reads it **zero** times
— `ResolveAgentClaudeDir` returns before `ClaudeRunConfigDir` for the same
reason. An unreadable database is still `None` rather than a zero row, so the
model stays `""` (i.e. "set no model option") rather than becoming `resolve`'s
`"sonnet"`: a database this process cannot open is not a user who never saved
settings.

**An embedded raw value is compacted and HTML-escaped on the way out, and Go
does it on the way *in*** (#298). `encoding/json` runs
`compact(…, escapeHTML=true)` over a `Marshaler`'s output, so a nested
`json.RawMessage` is whitespace-stripped and has `<`, `>`, `&` and U+2028/9
escaped — while keeping its key order and number spelling. `serde_json` writes a
`RawValue`'s bytes as-is through `write_raw_fragment`, which `GoFormatter` never
sees. Two places that mattered:

- the **synthetic SSE frames**: Go ships `{"question":"a \u0026 b"}` where Rust
  shipped `{"question":"a & b"}`;
- the **stored `blocks` column**: Go compacts **on store**, not on emit — this
  file and `persist.rs` both used to say the opposite — so writing the SDK's bytes
  verbatim left the two implementations' databases different for the same input.
  It was masked on read, because `chats::decode_blocks` compacts what it loads,
  which is exactly why nothing noticed.

**The rule lives on the field, not at the construction site**, at all four of
them: the two SSE structs in `chat/turn.rs`, plus `chats::MessageBlock::input`
and `sessions::detail::NormalizedBlock::input`. `MessageBlock` is why — it has
*two* sinks, the column via `persist::append_message` and the wire via
`GET /api/chats/{id}`, and it had two independent compaction points, one of which
was simply missing. A third construction path would have been silently wrong the
same way. The call-site `compact_raw`s are left as belt-and-braces; compaction is
idempotent, so nothing moved when the field-level rule went in.

**The `tool_use` input must never round-trip through a `serde_json::Value`.**
The first version of `append_assistant_blocks` did, and turned `{"z":1.50,"a":1}`
into `{"a":1,"z":1.5}` — sorted and respelled, with nothing to signal it.
`tests/chat_turn.rs` caught it, which is also why that test's fake CLI emits
literal bytes rather than `json.dumps`: Python normalises `1.50` to `1.5` and
adds spaces, so a byte-exactness test cannot go through it.

**The same rule holds in the other direction, on the input the tool actually
runs with** (#342). `can_use_tool`'s allow arm echoes the CLI's own tool input
back as `updatedInput`, and that echo used to go through a `serde_json::Value` —
so the CLI ran a re-sorted, re-spelled payload, on the *allow* path. Go's
`process.go` is `resp["updatedInput"] = envelope.Request.Input`, a
`json.RawMessage`, echoed verbatim. `PermissionResult::Allow::updated_input` is
therefore `Option<Box<RawValue>>` rather than a `Value` — bytes for both the
echo and a handler's rewrite — which cost no call site, since every handler in
the tree returns `PermissionResult::allow()`. Two consequences worth knowing:
`PermissionResult`'s `PartialEq` is hand-written, because `RawValue` has none and
byte equality is the only comparison the type can honestly offer; and
`write_control_success_raw` builds the two control-response envelopes as structs
whose **field order is the wire order**, spelled to match what `encoding/json`
does to Go's `map[string]any` (`response` before `type`; `request_id` before
`response` before `subtype`).

Both halves are pinned by asserting the **whole** `"updatedInput":…` substring of
the raw logged line — a per-key assertion passes against a reordered, respelled
object, which is why `tests/claude_sdk.rs` grew a `logged_line` beside
`logged_message`, and why the fake CLI now logs the stdin bytes it received
rather than a `json.dumps` of the decoded object.

**A disconnect has to be raced explicitly, not inferred.** The permission
handler is awaited *inline on the SDK's reader task*, so while it is parked no
events arrive and the stream loop has nothing to send — a closed tab is
invisible to every code path that would otherwise notice. All four unbounded
waits (the loop, the post-result continuation, and both permission arms) race
the body channel's closure, which is what Go gets from `r.Context().Done()`.
Without it, closing a tab on an open prompt held the busy lock and leaked a
`claude` subprocess for the life of the process.

The obvious way to write that is a **bug**, and the fix for it is the reason
`Answers.disconnect` is an `mpsc::WeakSender`. A plain `Sender` clone works for
detecting the disconnect, but this struct is reachable from the permission
handler, which the SDK's reader task owns until *stdout* hits EOF — so a strong
clone keeps the body's sender set non-empty past the end of the turn and the SSE
response stays open. That is ~5s when the CLI ignores `SIGTERM` and **unbounded**
when it leaves a grandchild holding stdout, which any backgrounding `Bash` call
produces. `useChatStream.ts` clears its streaming state only in `onDone`, so the
symptom is a chat stuck mid-stream with the composer blocked long after the
commit ran — and Go's handler returns as soon as `consumeAgentEvents` does, so
it is a parity break too. **Nothing that outlives the turn may hold a strong body
sender.**

Each of these is pinned by a test that fails when the fix is reverted —
`a_disconnect_while_a_prompt_is_pending_releases_the_chat` (reads frames until
the prompt arrives *before* disconnecting, or it would only exercise the
pre-existing failed-send path), its `..._silent_cli_...` sibling for the loop
arm, and `the_body_ends_with_the_turn_even_when_a_grandchild_holds_stdout_open`
for the sender strength. Assert the revert fails; a disconnect test that passes
either way is the easy mistake here.

**`AGENTO_CLAUDE_EXECUTABLE`** overrides which binary is spawned, falling back to
`find_claude_cli()` and then the bare name. The fallback matters — a GUI process
inherits a minimal `PATH`, which is why that helper already existed for the
startup banner — and the override is how the turn tests point at a fake CLI.

### The scan is the shell's, and that is the port's first ownership flip (#289)

Every route before this one could **forward on doubt**: `Err` meant "let the
sidecar answer", so a ported route could only ever be as broken as an unported
one. That property does not hold for the scan. Once the sidecar stops scanning
there is no second implementation behind it — forwarding `/status` would ask Go
about a scan Go is not running, and Go would answer `false`/`0`/`0` with
complete confidence.

So the flip is all of a piece, and has to stay that way:

- `sidecar.rs` starts the child with **`AGENTO_SCANNER=off`**. On the Go side
  that is checked in `Cache.EnsureScan`, which is the single place a scan
  starts — it covers both the boot-time `StartBackgroundScan` and every read
  path's `ensureFresh`, so no caller has to know. Unset means **on**, because a
  plain `agento web` must keep scanning; an unrecognized value is also on, so a
  typo cannot silently disable the scan. It has a sibling since #311 —
  `AGENTO_INTEGRATIONS`, same four off-values and the same unset-means-on
  rule — so this is now a shape rather than a one-off. Note where the sibling
  *departs*: it carries a list of integration types (`off:github`) rather than
  switching a subsystem off wholesale, because one of the six starters opens a
  live connection the sidecar's own endpoints read. See "The integration
  registry" above before copying this shape a third time.
- `lib.rs` starts the boot scan, replacing `sessionCache.StartBackgroundScan()`.
- `native/scan.rs` owns admission (one scan at a time), progress, the staleness
  markers and the two endpoints.

**The freshness probe is gone.** Answering a corpus read natively used to remove
the very thing that kept the corpus fresh, because Go's handler called
`ensureFresh` on the way past; `Answer::with_probe` put it back by firing a
cheap request at the sidecar. Those call sites now call `scan::ensure_scan`
directly — the same thing `Cache.List` does, one process earlier. `PROBE_PATH`,
`Answer.probe` and `spawn_freshness_probe` are deleted rather than left dormant,
because a probe that still fired would ask a sidecar that no longer scans to
start a scan.

**Verify a change here against the real corpus, not a fixture.** The failure that
matters is a scan that runs, reports success and writes nothing, and a
three-file fixture cannot tell that from a healthy one.
`tests/scan_live.rs` copies the real database, forces a full re-read and asserts
the row counts do not shrink and the markers are recorded. It is `#[ignore]`d
(CI has no corpus), so run it by hand:

```bash
cargo test --test scan_live -- --ignored --nocapture
```

### The notification sender (#307)

`PUT /api/notifications/settings` and `POST /api/notifications/test` are
native, and with them `internal/notification/{template,smtp}.go` — the only
code in this shell that talks to a server we do not run. Four things about it
are load-bearing:

- **`encryption` does not mean what it says.** `tlsPolicyFromEncryption` hands
  go-mail a *TLSPolicy*, and every policy there is about **STARTTLS**. go-mail's
  implicit-TLS switch is `WithSSL()`, which Agento never calls — so `ssl_tls`
  means *mandatory STARTTLS*, not SMTPS on 465. Reproducing that is the parity
  bar; "fixing" it moves a working configuration to a port nothing answers on.
- **The parity bar is the rendered mail, not JSON.** Nothing downstream parses
  it, so a divergence has nothing to report it — the first sign would be a user
  saying the email looks different. `desktop/parity/notification_template_golden.json`
  is rendered by Go and asserted by both languages. It earned its keep
  immediately: `html/template`'s text escaper is **seven** entries (the usual
  five plus `+` and NUL), and this port had also escaped `=`, which lives in the
  *nospace* table and applies to unquoted attribute values rather than text.
  The template skeleton is Go's **output**, not its source, because
  `html/template` elides HTML comments and `emailTmpl` has six.
- **A failed send forwards; only success is answered.** Go's 400 carries
  go-mail's and the Go runtime's wording, none of it reproducible. Forwarding
  costs a second dial and is safe for one reason only: `send` reports success
  after the server has accepted the message, so an error means nothing was
  delivered. Nothing fallible may run after that point — the response bytes are
  encoded before the dial for exactly that reason.
- **The settings write touches one column**, where Go's `UpdateSettings` saves
  all fourteen from the sidecar's boot-time snapshot. That is a deliberate
  divergence in *mechanism*: it is the bug #305 reproduced (one notification
  save reverting a natively-written hidden-project list and idle threshold), and
  the only way to be observably identical would be to reproduce it.

**The subscriber is not wired**, and cannot be: the event bus fires when a
scheduled task finishes and the executor is #275's, still in the sidecar. What
is ported is everything downstream of that call — settings → message → send.

`cmd/web.go` changed with this, and had to. The sidecar's notification
`SettingsLoader` read `settingsMgr.Get()`, an in-memory snapshot taken at boot;
with a second writer that meant a native save left every scheduled-task email on
the previous SMTP credentials until restart, silently. It reads the row now,
which is what its own doc comment already promised.

### Uploads, and the seam's second body cap (#308)

`POST /api/uploads` is the **only multipart route in the API**, and claiming it
cost the seam two small extensions, both in `proxy.rs`:

- `native::Request` carries the `Content-Type`. A multipart body is unparseable
  without the boundary, and the boundary is only in the header. Nothing else
  reads it.
- The body cap is per route. `MAX_NATIVE_BODY` is 8 MiB and stays there —
  **over it a request is answered 400 rather than forwarded**, because
  `to_bytes` has already consumed the body — so a 100 MiB upload needed its own
  limit or the shell would have refused a file the server accepts. It is a
  second constant rather than a raised one because the cost is real: the shell
  buffers the whole body, where Go's `ParseMultipartForm` keeps 10 MiB and
  spills the rest to temp files.

The multipart reader is hand-written, because every crate in the ecosystem is
built around an async byte *stream* and this handler runs on `spawn_blocking`
with the body already in memory.

Two rules the live parity run confirmed, both of which a reading of the Go
would get wrong:

- **`sanitizeExtension` is an allowlist, and `filepath.Ext(filepath.Base(f))`
  is not `rfind('.')`.** `Ext` stops at a separator, so `evil.png/../x` has no
  extension at all; `Base("")` is `"."` and `Ext(".")` is `"."`, so an empty
  filename yields `"."` rather than `""`. Only `/` is a separator, because this
  is the Unix `filepath` — a Windows-shaped name is one element, which is safe
  only because the alphanumeric check rejects what the backslashes carry.
- **A part named `file` with no filename is not a file.** `multipart.readForm`
  puts it in `Form.Value`, so `FormFile` answers `ErrMissingFile` and the
  handler 400s. Matching on the part name alone accepts a request Go rejects.

`POST /api/claude-sessions/{id}/continue` moved with it. Go creates the chat and
then updates it to carry the Claude session id; this does both in one
transaction, which is a deliberate divergence with only one alternative: `Err`
forwards, so a Rust failure *between* the two writes would leave Go's orphan
chat **and** have Go create a second one.

### The settings write is written, tested and not claimed (#305)

`GET /api/settings/claude-config-dirs` is native. `PUT /api/settings` is
implemented in full — validation, the locked-field 400s, the row write, the
rescan rules — and left out of `claims`, the way `migrate::apply` was in #274.

The reason is **not** the one this file used to give. Rust holds no snapshot of
these preferences at all: `settings::load` reads the row per request, which is
why `apply_data_settings` is three lines where Go's is thirty. The obstacle is
the **sidecar**, which does hold one and is still serving routes that read it:

- `notificationServiceImpl.UpdateSettings` is a read-modify-write over
  `settingsMgr.Get()` that persists the **whole** `user_settings` row, so a
  native settings save followed by one SMTP save through Go rewrites
  `hidden_projects`, `idle_gap_threshold_minutes` and both config-dir columns
  from the sidecar's boot-time copy. Reproduced against a Go server built from
  this checkout: a row edited to `["…/native-wrote-this"]` / 42 read back
  `["…/hidden-one"]` / 25 after one unrelated notification save, with no error
  anywhere — silent, total reversion of the Data & Analytics tab. #307 has
  since taken `PUT /api/notifications/settings` native, and its write touches
  one column precisely so it cannot do this; but the Go method is unchanged and
  `Err` still forwards, so the path is narrowed, not closed.
- `config.ResolveAgentClaudeDir` resolves each run's Claude account from
  `claudeDirs.runOverride`, so scheduled tasks (#275) and Telegram triggers —
  both still Go's — would keep authenticating as the previous account.
- `config.profiles` resolves `/api/claude-settings*` against the same snapshot.

#274's rule decides it: *a route moves only when Rust can reproduce every effect
it has*, and "the sidecar now agrees" is one of this route's effects. #289's
flip worked because `AGENTO_SCANNER=off` switched the Go half off, and #311's
because `AGENTO_INTEGRATIONS=off:<types>` does the same for the MCP servers of
the types the shell hosts —
so the mechanism is established and the question here is only whether it
applies. It does not: those two switch off a *subsystem* the sidecar owns,
while `SettingsManager` is a snapshot the sidecar **reads** on paths it is
still serving, and there is no switch that makes those paths read the row
instead. Forward-after-write is just the forward. It turns on with the
cut-over that deletes the sidecar.

Three things the write itself pins, each captured from a live Go server:

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

### The Claude settings surface, where the state is files (#304)

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

### The scheduler: the computation moved, the ownership did not (#275)

**Only one process may schedule**, and today that process is Go: `cmd/web.go`'s
`initTaskScheduler` constructs and starts one on every boot. Two schedulers over
one `scheduled_tasks` table means every task fires twice and the Telegram
webhook is re-registered under whichever instance registered last, so taking the
scheduler is a *flip* on the #289 model — sidecar off, shell on, one commit —
not a route that can forward on doubt. **That flip is blocked; see below.**

What is here is the half that is verifiable before the flip, and the half most
likely to be subtly wrong: **when a task fires**. `native/schedule/` ports
`buildJobDefinition` and the `next()` of each `gocron/v2` job type it can
produce, pinned to `desktop/parity/scheduler_vectors.json` — 68 cases generated
from a *real* `gocron.Scheduler` driven by a `clockwork` fake clock
(`go test ./internal/scheduler/ -update-scheduler-vectors`), asserted against Go
by `internal/scheduler/schedule_vectors_test.go` and against Rust by
`include_str!`, exactly the shape `gopath_vectors.json` uses. A change to
`buildJobDefinition`, to gocron or to `robfig/cron` fails **Go's** suite first.

The semantics are gocron's, not the cron string's, and three of them are silent
when reproduced wrong:

- **`run_immediately` is a one-time job at `now + 2s`.** gocron discards
  one-time start times that are not strictly in the future and then refuses the
  job outright, so "now" would never run. The vector records the *offset*
  rather than an instant, because Go reads `time.Now()` inside the builder.
- **`every_days` + `at_time` is a `DailyJob`, not a 24-hour `DurationJob`.** A
  daily job holds the wall clock across a DST transition; a duration job adds 24
  absolute hours and walks off it. Both are in the vectors from the same
  Europe/Berlin start: midnight stays midnight, 12:00 becomes 13:00.
- **A malformed `at_time` falls back to that duration job with no error.**
  `buildIntervalJob` discards `buildDailyAtTimeJob`'s error, so `9am`,
  `09:00:00`, `25:00` and `09:60` all schedule *something else* and say nothing.
  Note `7:5` is **not** a fallback — `Atoi` needs no zero padding.

Two more the vectors pin, both invisible in UTC:

- **`robfig/cron`'s dialect** (`cron.rs`), because `gocron.CronJob` delegates to
  `ParseStandard` with `CRON_TZ=<location>` prepended. Descriptors *are*
  accepted (`@daily`, `@every 1h30m` — Go's `ParseDuration`, floored at 1s);
  six-field seconds specs are *not*; `N/step` means `N-max/step`; and `?` sets
  the same star bit `*` does, which decides whether day-of-month and day-of-week
  are ANDed or ORed. Its `Next` steps **absolute** hours, so a daily `0 2 * * *`
  in Europe/Berlin skips 2026-03-29 entirely rather than shifting — while
  gocron's own duplicate-wall-clock guard stops it running twice on the October
  Sunday when 02:00 happens twice. `*/30 * * * *` does repeat through that hour,
  because the guard only catches an identical wall clock.
- **A one-off keeps `run_at`'s own offset**; every other job type renders in the
  scheduler's location. That is why `Fire` carries an offset beside the instant.

`cron.rs` reuses `analytics/buckets.rs`'s `go_date`/`add_date` rather than
`chrono`'s calendar arithmetic — robfig resets the lower fields with
`time.Date`, which *normalizes* a wall clock a DST gap removed instead of
failing, and that normalization is the answer on the spring-forward day.

**The executor is deliberately not ported.** It is fifteen functions needing the
agent runner, job-history writes, the event bus, chat-session creation and OTel
spans, and — unlike `migrate::apply`, which is small with a crisp contract — an
unwired executor has no testable contract at all. It would be several hundred
lines nothing could check until the flip. It follows the flip; it does not
precede it.

**The blocker, verified.** The flip needs Rust to *run* a task, and
`chat/runner.rs::build_options` still refuses an agent whose capabilities name
an MCP server this build cannot host — two of the six providers (#313 and #314). For a chat that is safe — the three steering routes forward
and Go answers, because Go holds the session. **For a scheduled task there is no
second implementation behind it**: with the sidecar not scheduling, a task Rust
cannot run does not fail, it *silently never runs*, and the job history has no
row to show for it. That is the one property the whole port has relied on,
absent.

How much it strands: an agent is runnable natively only if its allowlist is
built-ins plus local tools. Against 12 built-ins and `internal/tools`' one there
are 61 integration tools across the six supported providers, plus anything in
`mcps.yaml` — none of which Rust can supply, and the scheduled task that fetches
a calendar or triages issues is the archetype rather than the edge. #310 moved
one of the two agents Agento *ships* (`agents/hello-world.yaml`, which names
`local: current_time`) off that list; the other side of the count is unchanged.
(Neither `~/.agento` nor `~/.agento-desktop-dev` has an agent, task or
integration on this machine, so that is the structural count, not a measured
one.)

There is also no escape hatch that keeps Go executing: the API has no
"run this task now" route, so a Rust-owned timer has nothing to call. Adding one
would put the scheduling decision in two processes again, which is the hazard
the flip exists to remove.

So `cmd/web.go` is **untouched** — no `AGENTO_SCHEDULER` gate was added, because
a gate whose only correct setting is "on" is a switch for turning tasks off. The
flip is one commit when `build_options` can supply every tool source: gate
`initTaskScheduler` on `AGENTO_SCHEDULER` the way `Cache.EnsureScan` is gated on
`AGENTO_SCANNER` and `IntegrationRegistry` on `AGENTO_INTEGRATIONS` (unset = on,
unrecognized = on, so a plain `agento web` is
unchanged and a typo cannot disable it — and note the integrations switch also
carries a *list*, which a scheduler gate would not need), set it from
`sidecar.rs`, port the
executor, and move `POST/PUT/DELETE /api/tasks` and the pause/resume actions —
which #274 left with Go precisely because each also calls
`ScheduleTask`/`UnscheduleTask`, and a row written without a registration is a
task that never fires.

### Do not port

Telemetry/OTel, Prometheus metrics, and the self-updater are server concerns.
The desktop app should use Tauri's own updater instead.

**#309 turned that from an omission into an answer.** "Out of scope" was fine
while the two config routes forwarded to a sidecar that *did* export; after the
cut-over it would have meant the settings page 404s. So `PUT /api/monitoring`
and `POST /api/monitoring/test` are claimed and answer **501** with a message
naming the alternative, and the Monitoring section is **read-only**.

The two options not taken, and why:

- **Persisting the config without exporters is a save that changes nothing.**
  `Manager.Update` writes `monitoring.json` *and* rebuilds the providers; this
  build has no providers. A 200 there tells the user telemetry is on while
  nothing is emitted. It would also be stale a second way before the cut-over,
  since the sidecar builds its providers once at `Update` and a native write
  cannot reach them — the same wall #305 hit on `PUT /api/settings`. #289 and
  #311 both got past it by switching the Go half off; there is no such switch
  for a provider set the sidecar has already built.
- **Porting the exporters** is the largest option in the plan and reverses the
  decision above.

`501` rather than `404` because the route exists and this build declines it; a
404 reads as a version mismatch and sends someone hunting an upgrade that will
never ship — the same reasoning `unavailableCopy` encodes for WhatsApp. The
routes are **claimed rather than forwarded** on purpose: a forward would reach
the sidecar, which would save the config and reload its own providers, which is
the outcome the decision exists to stop.

`GET /api/monitoring` stays native and the section still renders it, because
what it reports is still true: `monitoring.json` is shared with an `agento web`
on the same data dir, and `locked` names the `OTEL_*` variables pinning a field
— which is what someone debugging a missing trace comes here to read.

One thing worth recording before it disappears with the sidecar: Go's
`POST /api/monitoring/test` is close to a no-op. `grpc.Dial` is lazy and almost
never errors, `WaitForStateChange(ctx, Idle)` returns as soon as the state
leaves `Idle` — immediately, at `Connecting` — and `Connecting` counts as
success. It answers `ok: true` for an endpoint nothing is listening on, unless
the failure lands inside the microseconds before the state is read.

**#301 answered the other half: what this build *emits*.** #309 settled the
config surface; the emission side was still undecided, and drifting. **No native
handler starts an OTel span, and none will.** Go instruments every handler and
service method (`otel.Tracer("agento").Start(ctx, "integration.create")`), but a
span with no provider behind it is a no-op — the only thing emitting them from
Rust could ever lead to is porting the exporters, which is the option declined
directly above as the largest in the plan. Adding spans first and deciding later
gets that order backwards.

**Do not read the sidecar's remaining spans as coverage.** While it is bundled
it still exports whatever it still serves, so a trace view shows a *shrinking,
arbitrary* subset: exactly the routes that happen not to be ported yet. That is
the port's progress rendered as a graph, not a statement about the app — anyone
auditing "did the integration write stop happening?" from a trace is reading the
wrong instrument, since a ported write leaves no trace precisely because it
worked. It is a transitional artifact and it goes away with #278, which is why
#301 was answered by writing this down rather than by filling in the other half
with spans that would then be deleted.

**The request log is the half that is reproduced, because it is not
telemetry.** "Do not port" covers the exporters; Go's `internal/logger` is not
on that list, and its logging never depended on OTel being configured — the
`otelslog` bridge is an optional add-on. The desktop equivalent is **one access
line per `/api` request, emitted at the seam** in `proxy.rs::handle`: method,
path, status, elapsed ms, and which implementation answered (`native`,
`native-stream`, `forwarded`, `native-failed-forwarded`, `diff`).

**Be precise about which Go log that is.** What is reproduced there is Go's
`requestLogger` (`internal/server/server.go`) — method, path, status, duration —
which Go writes at **debug for every method**, and which the seam promotes to
info for the requests worth keeping. Go's *service-layer* `Info` lines are a
different animal and **could not be reproduced at the seam**: `POST
/api/integrations 201 12ms native` carries neither the entity nor the outcome,
and no access line can say how many chats a bulk delete took. The seam sees a
*request*, not an operation.

**#335 added them at the call sites, for the subsystems that are native today.**
Sixteen lines, mirroring their Go counterpart's message and keys: the agent CRUD
(#274), the chat CRUD and the turn's five (#276), the two job-history deletes
(#274), `POST /api/integrations` plus the `{id}` writes and the three
trigger-rule writes (#277, #347), and `POST /api/uploads` (#308).
`writes::service_log_convention` is the rule — `message key=value`, every string
value `{:?}`, `info` and **after** the effect — and it is a doc rather than a
helper because the alternative is the rule restated sixteen times. Each line has
a test: a `log` sink the write tests assert against
(`writes::testlog`, and a second copy in `tests/chat_turn.rs`, which is a
different crate), because a line with no test is a line that quietly stops being
emitted — which for this half is the whole failure mode.

Two of them carry user-authored text the access line does not, and both are
deliberate on the same terms the agent slug in the path already is:
`integration created … name=`, because a line that cannot say which integration
was created is most of what it is for, and `file uploaded path=`, which is the
*generated* destination under the uploads dir and which the response body
returns anyway. Nothing logs a credential, a prompt or a message body.

**What is still missing is missing by construction, and the list is the point.**
The background `Info` lines have no request behind them at all —
`scheduler.go`'s `"task scheduled"`, `executor.go`'s
`"task execution completed"`, `trigger/dispatcher.go`'s `"trigger rule matched"`,
and the notification handler's — and they belong to subsystems the sidecar still
owns, so a line here would log an event this process did not cause. The five
`… validated` lines in `integration_service.go` belong to `ValidateTokenAuth`,
i.e. the deferred `auth/*` routes; `PUT /api/settings`'s two are deferred with it
(#305). They come from the sidecar today; **#278 is when this stops being
optional**, because until then the sidecar still emits its own lines for what it
still serves. That is why these land per subsystem as it is ported rather than as
one pass. Go's line also carries a `request_id`, which is the thing
that would let a `forwarded` line be matched against the sidecar's own record of
the same request. Correlation is deliberately not attempted: it would mean
minting an id here and threading a header through the forward, for a pairing
only useful while both halves are running.

**The access line is at the seam and not in the handlers, for the reason the
whole issue exists.** `handle` is the one point every `/api` request passes
through whether Rust, Go or a fallback answers it, so the record covers claimed
and unclaimed routes alike and **cannot go selectively sparse as the port
advances** — a line per handler would be fifteen edits that drift, and the
sixteenth port would forget one. It is the same shape Go got from wrapping the
router in `otelhttp` rather than instrumenting each handler. `dispatch` exists
only so that the eight-odd return paths do not each need their own log call.

The #335 lines are the ones that *have* to be per handler, and they accept that
cost knowingly: only the handler knows the entity and the outcome, so there is no
seam to put them at. Their protection against going sparse is a test per line
rather than a single call site.

Two rules there are deliberate and must survive anyone "improving" it:

- **Failures at `warn`, writes at `info`, successful reads at `debug`.**
  `tauri_plugin_log` is built at `LevelFilter::Info`, so this three-way split is
  what the file holds by default. Reads are at debug for one reason — volume:
  the sessions list polls `GET /api/claude-sessions/status` on a timer for the
  whole length of a scan, and an info line per poll buries everything else,
  which is how a log stops being read. A read that answered 4xx or 5xx is not
  that volume; it is the one read anybody wants in the file, so the status
  outranks the method. Without that arm the first native `GET` handler to answer
  404 as `Ok(Answer)` — the natural shape once a read stops wanting the Go
  fallback — would be invisible, and so would a Go 5xx forwarded through on a
  `GET`. `HEAD` and `OPTIONS` sit with the reads: the router is `any(handle)` so
  both reach the seam, and neither changes anything. The split is reads against
  writes, not `GET` against everything else.
- **No bodies, no headers, no query string.** The bodies here are chat prompts,
  agent system prompts and integration credentials, and the query string carries
  search terms and project paths. The log is a plain file on disk. `log_path`
  drops the query in one place so there is one place to check that it does.
  **The path itself is logged, and two route families put user-authored text in
  it** — the agent slug (`routeAgentBySlug`, `internal/api/server.go`, derived
  from the name the user typed) and the settings-profile id (`routeProfileByID`,
  the same shape). So `PUT /api/agents/acme-corp-earnings-reviewer 200 6ms
  native` writes a user's own wording to an unencrypted file. That is accepted
  rather than overlooked: a name is not a body, and dropping the segment would
  leave the line unable to say which agent was written, which is most of what it
  is for. Nothing else in the route table carries user data in a path segment —
  no filesystem path, no secret — and a route that wanted to would need to be
  argued here first.

It lands where a user can retrieve it, which is the only thing that makes this
an answer rather than a gesture: `tauri_plugin_log`'s default targets are stdout
**and** `LogDir`, so the line is written to Tauri's app-log directory —
`~/.local/share/com.shaharialab.agento/logs/Agento.log` on Linux,
`~/Library/Logs/com.shaharialab.agento/Agento.log` on macOS,
`%LOCALAPPDATA%\com.shaharialab.agento\logs\Agento.log` on Windows, which ships
too. (`Path::app_log_dir` special-cases macOS to `~/Library/Logs/<identifier>`
and everywhere else is `data_local_dir()/<identifier>/logs`; the file is named
for the product, `Agento.log`.) Nothing in `lib.rs` needs to ask for the target;
it is what `Builder::new()` already does. A packaged `.app`/`.AppImage`/`.exe`
has no console, so a stdout-only logger would have made the whole exercise
unobservable.

**Retention is deliberate, and it is a #301 decision rather than a default.**
`tauri_plugin_log`'s own defaults are `DEFAULT_MAX_FILE_SIZE = 40_000` bytes and
`DEFAULT_ROTATION_STRATEGY = KeepOne` — and `KeepOne` does not mean "keep one
archive", it means `rotate()` is `fs::remove_file(&self.path)`, so there is no
archive. At roughly 90 bytes an access line that is ~440 requests of history and
then a wipe, reached well inside one ordinary session and fastest in `diff`
mode, where every compared request also logs `identical`. Harmless while the
file held a handful of startup lines; fatal once it is the access log, because
the situation the log exists for — a user who hits a bug an hour in — is exactly
the one where the evidence has already been deleted. `lib.rs` therefore sets
**5 MiB with `KeepSome(3)`**: three dated archives beside the live file, ~20 MiB
and days of history. Both settings are load-bearing in the same way the default
targets are, and neither should be dropped back to the default.

The elapsed figure is time-to-headers whenever the body is still arriving, which
is more lines than the `native-stream` label suggests. `forward` also streams —
it builds its body with `Body::from_stream` — so any `forwarded` or
`native-failed-forwarded` line for an SSE route reports the same thing. Under
`AGENTO_DESKTOP_NATIVE=off`, a documented and supported mode, a three-minute
chat turn logs `POST /api/chats/{id}/messages 200 9ms forwarded`. Only `native`
and `diff` buffer the whole response before the line is written, and only they
report a duration the request actually took.

None of this is a property of the *data*. An `agento web` pointed at the same
`~/.agento` exports OTel exactly as it always did; it is this build that
declines to.

### WhatsApp is dropped, not deferred (#273)

`whatsmeow` has no Rust equivalent and will not be reimplemented. This is a
decision, not a backlog item: do not port `internal/integrations/whatsapp`, and
do not keep a Go sidecar, subprocess or shim for it — that would defeat the
point of the port, which is to stop maintaining two implementations. A session
that finds itself scoping WhatsApp work has misread this section. `main` keeps
WhatsApp indefinitely; the desktop app is what loses it, and that is the
accepted trade.

What that means in the code, and the part that is easy to get wrong:

- The UI offers no WhatsApp entry, pairing flow or QR screen. The picker is
  `PROVIDERS` in `src/views/integrations/catalog.ts` — a hardcoded list, so
  **that list is what decides it**, not anything the API returns.
- **An existing row is data and must survive.** Someone who paired under the Go
  server has an `integrations` row of that type. `type` is a free-form `String`
  everywhere — no enum, no match — so it lists, opens and reads normally;
  `providerFor` returning `undefined` is a supported answer, and
  `unavailableCopy` beside it turns that into an honest "not available here"
  rather than the "newer version of Agento" line, which would send that user
  hunting for an upgrade that will never ship. Both live in `catalog.ts`: what
  types this app knows, and what to say about the ones it does not, are one
  question. The row cannot be removed or edited from the desktop app — those
  controls only render for a known provider — so do not describe it as
  deletable.
- **Do not filter it out of `available-tools`.** Go's handler never looks at
  `type`, so suppressing the row is a byte-level divergence on an endpoint
  whose bar is byte-identical JSON. Agents whose allowlists name WhatsApp tools
  keep those entries, and — while the sidecar is still bundled — those tools
  still **resolve and work**: agent execution is phase 5, and `cmd/web.go`
  registers the `whatsapp` starter in the binary the app ships. They stop
  resolving when the sidecar is deleted at the cut-over. That is the accepted
  trade, and it is a removal that happens *then*, not one this issue skipped.
- `GET /api/integrations/{id}/whatsapp/*` stays unclaimed and forwards. Nothing
  calls it any more, so it dies with the sidecar rather than needing removal.
- **The sidecar must keep *hosting* a whatsapp row, not merely answering about
  it** (#311). This is where "dropped, not deferred" is easiest to over-apply.
  `whatsapp/server.go`'s starter is not a plain MCP-server constructor: it opens
  a whatsmeow WebSocket and registers the live client in a package global that
  `ConnectionStatus`, `POST /{id}/whatsapp/reconnect` and QR pairing all read.
  So `AGENTO_INTEGRATIONS` names the types the *shell* hosts rather than
  switching Go's hosting off wholesale — a process-wide switch would leave
  `whatsapp/status` permanently `{"connected":false}`, reconnect a 200 that does
  nothing and a freshly paired client that never connects. The row's data
  surviving is one requirement; its connection surviving until the sidecar goes
  is another.

---

## Packaging

`npm run app:build` produces the native format for the host platform
(`bundle.targets` is `"all"`): `.deb` + `.rpm` + `.AppImage` on Linux, `.dmg` on
macOS, NSIS `.exe` on Windows. The release workflow builds five target triples
on their own runners — Linux x86_64 and aarch64, macOS aarch64 and x86_64,
Windows x86_64 — and attaches the installers to a draft release.

### Updates

Signed with **our own minisign key**, generated by `tauri signer generate`.
It has nothing to do with Apple or Microsoft code signing; it only proves an
update came from us. That is why in-app updates work fine on unsigned macOS
builds — Gatekeeper gates the first launch of a *downloaded* app, not a bundle
the updater swapped in.

- Private key: 1Password → GCP Secret Manager
  (`github-repo-agento-tauri-updater-private-key`) → the repo's
  `TAURI_SIGNING_PRIVATE_KEY` Actions secret, all via `terraform/`. **Losing it
  means no existing install can ever be updated again.**
- Public key: `plugins.updater.pubkey` in `tauri.conf.json`.
- Manifest: `.github/scripts/build-update-manifest.py` assembles `latest.json`
  and publishes it to the fixed `desktop-latest` tag, so the updater has a
  stable URL. `releases/latest` could not be used — it would point at the Go
  server's `v*` releases. The script *fails the release* if any platform is
  missing a signature, because a manifest that silently omits a platform
  strands exactly the users already running it, and they would never be told.

`.deb`/`.rpm` installs are **notify-only**: dpkg and rpm own their files, so
`install_kind()` in `lib.rs` detects them at runtime (the AppImage runtime is
the only one that identifies itself, via `$APPIMAGE`) and the UI offers a link
instead of an install button. This has to be a runtime check — one Linux
`tauri build` emits deb, rpm and AppImage from the *same* executable, so no
compile-time flag could tell them apart.

### What ships inside, and the one thing that does not

Bundled: the Go server (statically linked, stripped, ~43 MB), the frontend, and
the Rust shell. Nothing is fetched at runtime.

Platform runtime: the webview. macOS uses the system WKWebView. Windows bundles
the WebView2 offline installer (`webviewInstallMode`), so the installer is
self-contained. On Linux the `.deb`/`.rpm` **declare** GTK/WebKitGTK as package
dependencies — apt/dnf resolve them, which is the correct native behaviour;
statically bundling them is not a thing Debian packaging does. The `.AppImage`
bundles them for distro-independent use.

### Binary naming, and renaming it back later

The Debian/RPM package is named **`agento`** (from `productName`), but the GUI
binary inside it is **`agento-desktop`** (`mainBinaryName`). That split exists
only because the Go CLI currently installs an `agento` onto `PATH`; a package
dropping `/usr/bin/agento` would shadow it depending on PATH order.

When the CLI is retired and the desktop app becomes *the* Agento, renaming is
one line: set `mainBinaryName` back to `agento` (or delete the field). Nothing
else has to change, and specifically:

- **dpkg/rpm handle it themselves.** The package name is unchanged, so an
  upgrade is an ordinary file change within the same package — the old
  `/usr/bin/agento-desktop` is removed and `/usr/bin/agento` added. No
  `Conflicts:`/`Replaces:` needed, because no *package* ever owned that path
  (the CLI is installed by hand into `~/.local/bin`).
- **The `.desktop` launcher is regenerated** by Tauri with the new `Exec=`.
- **The updater is unaffected.** Every format replaces the whole artifact — the
  AppImage file, the `.app` bundle, the NSIS install — so a binary rename
  crosses an update boundary cleanly.
- **macOS and Windows never cared**: the binary name is internal to the bundle
  and the installer.

The one leftover is users who still have `~/.local/bin/agento` from the CLI;
that is the CLI's uninstall to handle, not the package's.

### The one external dependency

**The Claude Code CLI is not bundled.**
The Go server runs every agent by spawning `claude` as a subprocess
(`ClaudeExecutable` defaults to the bare name, resolved on `PATH`). It is a
~280 MB separate install we do not redistribute, and it needs its own sign-in.
The app detects it at startup (`find_claude_cli` in `lib.rs`, which also checks
per-user install dirs because a GUI process often inherits a minimal `PATH`)
and shows one clear banner instead of letting every chat fail with
`exec: not found`. Removing this dependency means reimplementing the agent
runtime — that is phase 5 of the port, not a packaging change.

---

## Status

**Phase 1 complete.** Sidecar spawns on a random port and is health-gated; the
axum proxy streams `/api` through with the `Host` rewrite the Go guard needs.
All nine views are wired to the real API, typecheck clean, and were verified
against live data — including a real chat turn streamed end to end over SSE.

**Phase 2 in progress.** `GET /api/pricing/catalog` is answered by Rust
(`native/pricing.rs`), reading the same SQLite file the sidecar writes. It is
byte-identical to Go on the reference instance — 46,681 bytes both sides, 36
models, matching FNV revision — and on the shared fixture in `parity/`. The
infrastructure it brought is what the rest of the phase rides on: `gojson.rs`
(Go's encoder in Rust), `gotime.rs`, `paths.rs`, the read-only DB handle, the
three-mode seam and the two parity harnesses.

The sessions list and its facets followed (`native/sessions/`), diffed across 21
filter and sort combinations plus four pages of cursor interoperability per
sort — each page continuing from the cursor **Go** minted, so the two have to
agree on the cursor's bytes as well as the page's.

The agent reads followed (`native/agents.rs`), which is where the **nil-versus-
empty** rule bites: Go marshals a nil slice as `null` and an empty one as `[]`,
and `capabilities.built_in` is stored as whichever the writer produced. Every
list field there is an `Option` for that reason — a `Vec` defaulting to empty
would change the wire for every agent.

`GET /api/claude-analytics` followed (`native/analytics/`) — the largest single
endpoint in the plan, byte-identical across fifteen live cases: every
granularity band, four timezones, a window spanning the EU spring-forward, an
empty window, RFC 3339 and bare-date bounds, a project filter and the default
window. Three things it brought:

- **`buckets.rs` is Go's `time` package, not `chrono`'s.** `go_date` reproduces
  `time.Date`'s two-lookup zone resolution, so a wall clock the DST gap removed
  still resolves to a real instant instead of `LocalResult::None`; steps advance
  the calendar unit, because a local day is 23 or 25 hours across a transition.
  `chrono-tz` supplies the offsets `time.LoadLocation` would.
- **An unknown timezone is an error, not a fallback.** Go answers in UTC for a
  zone it cannot load, but *its* tzdata may know a zone this build's does not,
  and answering in UTC where Go answers in Asia/Kathmandu is a wrong answer
  rather than a missing one. Returning `Err` forwards to Go, which gives
  whichever answer Go would have given.
- **Accumulation order is part of the answer.** Go's summary sums the four cost
  categories separately and totals them; the cache-savings card sums `total_usd`
  per session. On the reference corpus that is $30775.990068829993 against
  $30775.990068829982 for the same money, and both spellings ship. Float
  addition is not associative — reproduce the loop, not the arithmetic.

It is deliberately **not memoized**. Go's LRU (`analytics_cache.go`) exists
because a rebuild is a full corpus load and a dozen walks over it, fired two or
three times per dashboard open; measured, that rebuild is ~50 ms, and this port
does the same work. A cache is a second thing to invalidate correctly and
nothing about the response depends on one.

`GET /api/claude-sessions/insights/summary` followed, together with the nine
insight processors behind the rows it reads (`native/insights/`). Two things
about that port are worth knowing:

**The processors write nothing, deliberately.** On the Go side
`insight_worker.go` is a background writer, and porting the *upsert* now would
put two processes on one SQLite file — the sidecar's worker and ours — racing
over the same rows. So they are ported as what they are: a function from a
transcript to a `SessionInsight`. That is the whole of the logic, and it is
verifiable without writing anything. The worker is a loop around it when the
storage layer moves.

**Their parity bar is the stored rows, not a response.** Every
`session_insights` row at the current processor version is recomputed from its
own transcript and compared field by field — ~900 sessions and ~1 GB of real
JSONL on the reference corpus, which is a far stronger check than any fixture.
It found nothing wrong with the port and two things about the *method*: a
transcript that has grown since its row was written makes every figure read
"computed is larger" (exactly what an over-counting bug looks like), so rows are
anchored on `session_insights.scanned_at`; and `claude_working_time_ms` is not
reproducible at all where two events share a timestamp and differ on whether
they are assistant events, because the gap leading into the tied pair is
credited by an unstable sort's order. Three sessions are in that state.

`native/insights/transcript.rs` is deliberately about the *file format* rather
than about insights: the scanner port (issue #270) needs the same decoder and
the same `isUserTurnContent` predicate, and two readers of one format is exactly
how `message_count` and `turn_count` would drift apart.

**The Claude Agent SDK is ported** (`src/claude/`, see above) — the whole of
`claude-agent-sdk-go`'s surface Agento uses: the session lifecycle, all the
options, the control protocol including permission and hook round trips and
interrupt, and in-process MCP servers. It is a library with no caller yet: the
agent runner, the chat service and the SSE handler that would sit on it are
still Go, and nothing in `native/` routes to it. It is here first because phases
4 and 5 both need it — every integration is one of its MCP servers — and because
its correctness is testable in isolation, against a scripted CLI, in a way it
would not be once a route depends on it.

The **chat reads** followed (`native/chats.rs`, #264) and then the **scheduled-task
and job-history reads** (`native/tasks.rs`, #265) — the first two areas whose data
is ordinary SQLite rather than the Claude corpus, and the first whose parity had to
be *seeded* rather than found: both areas were completely empty on the reference
machine, so every list diffed `[]` against `[]` until rows existed. Each suite now
fails rather than passes in that state.

Three rules they added to the ones above, each of which makes a wrong port look
right:

- **A handler that writes a Go map ships alphabetical keys.** `handleGetChat`
  spells `{session, messages}` and sends `{messages, session}`. Check
  struct-versus-map before modelling any envelope.
- **`limit=0` means fifty.** `parseQueryInt` only rejects negative and unparsable
  values, and the service *then* maps `limit <= 0` to the default — so a literal
  zero asks for nothing and receives a full page. `maxQueryLimit` (500) clamps
  `offset` too, since both share the parser.
- **An unknown task's job history is `200 []`, not a 404.** Go never checks the
  task exists before listing its runs, so this must be *answered* rather than
  forwarded; only `/api/tasks/{id}` and `/api/job-history/{id}` 404.

**Phase 4 has started.** `internal/tools` (#310) and the **GitHub integration**
(#312) are ported: `native/tools/` and `native/integrations/github/`, both built
on the typed-tool layer #282 settled and both pinned by parity vectors taken
from the running Go server rather than from its source. **#311 hosts them**:
`native/integrations/registry.rs` is the Start/Reload/Stop lifecycle and the one
place a credential is read, `PUT`/`DELETE /api/integrations/{id}` are native, and
the sidecar runs with `AGENTO_INTEGRATIONS=off:<the types the shell hosts>` so
every type has a single owner — the second ownership flip after the scan's, and
the one that had to be *per type*, since `whatsapp`'s starter opens a live
connection the sidecar's own endpoints read. `chat/runner.rs` therefore refuses
only an agent naming an integration Rust cannot host, which is the two types
#313 and #314 cover. #312 also produced the reflector divergence map
(`parity/jsonschema_reflect_vectors.json` + `claude/schema_vectors.rs`), which
is the file to read before starting #313 and #314. **#317, #316 and #315 followed** — the
Confluence and Jira integrations, six and nine tools, pinned by
`parity/confluence_vectors.json` and `parity/jira_vectors.json`. Read Confluence
first (it is the smaller worked example) and then Jira, which is where a shared
credential type turns out not to mean shared behaviour.

Nothing else is ported. Every endpoint not listed above — every write path, the
per-session reads and every other read — still forwards to Go.

**Reading is what keeps the corpus fresh.** `Cache.ensureFresh` runs on every Go
read path and starts a background rescan when the TTL expires, the pricing
catalog moves, or the idle threshold changes. A ported read removes that trigger
and nothing would say so — transcripts would stop being re-read and a rate edit
would never reach stored costs. `native/sessions/freshness_probe` puts it back by
firing one cheap request at the sidecar, so the *rules* stay in the code that
owns them rather than being reimplemented and left to drift.

Verified but not exercised against real data: task/job writes, integration
OAuth and agent CRUD — the reference instance has none of those configured.
Use the isolated dev instance for write testing.

**Known gaps**
- Cross-view navigation: "Continue in chat" on a session can only report the
  new `chat_id`; it cannot switch to the Chats view. Needs a nav context in
  `App.tsx`.
- `useAppStats` counters refresh on a 30s poll and on window focus, not on
  mutation, so a create in one view lags in the sidebar briefly.
- Session table is not virtualised; 900+ rows render eagerly after "Load more".
- No multi-window, tray icon, native context menus, drag-and-drop, or Tauri
  auto-update.

**Upstream (Go) bugs worth fixing there, not here**
- `PUT /integrations/{id}` wipes stored credentials on a scrubbed round-trip
  (see above). This is data loss and affects the shipped web UI too — the
  highest-value fix on this list.
- `AgentRequest` is missing `permission_mode`, so agents can never persist it.
- The project filter means different things on `/claude-analytics`
  (`decoded_path`) and `/claude-sessions` (`project_path`).
- `/claude-sessions/projects` returns `decoded_path` identical to
  `encoded_name`, so the decode never actually happens for the picker.
- The Claude settings surface writes with `os.WriteFile`, which truncates: a
  crash between the truncate and the write leaves an empty
  `settings_profiles.json`, orphaning every profile. A temp file plus `rename`
  is byte-identical in final content and unobservable through the API. Not done
  in the port alone, because then the desktop app and `agento web` would survive
  a power cut differently.
- `writeProfileSettings` and `moveProfileFile` are the only two places that touch
  a recorded `file_path` without `validatePathWithinDir`, while `buildProfileDetail`,
  `DeleteProfile`, `DuplicateProfile` and `SetDefaultProfile` all check. A
  hand-edited index can therefore have an update write outside the settings dir.
  The port reproduces this deliberately (adding the check would refuse a write Go
  performs) and says so at both sites.
- **A cron task can be saved that stops the server from starting.**
  Filed as [#330](https://github.com/shaharia-lab/agento/issues/330), because
  this list is not somewhere a server engineer looks. `Scheduler.Start` now
  recovers per task so one bad row cannot brick a boot (#324); the validation
  half is still open.
  `validateScheduleConfig` checks only that `expression` is non-empty, and
  `robfig/cron`'s `Parse` slices between `=` and the first space — so an
  expression of exactly `CRON_TZ=UTC` panics with `slice bounds out of range
  [:-1]`. `CreateTask` writes the row *before* calling `ScheduleTask`, and
  `middleware.Recoverer` turns the request into a 500, so the row survives.
  On the next boot `Scheduler.Start` reached it with nothing recovering, and
  `agento web` died. Reproduced against gocron v2.22.0. The fix is to validate
  the crontab at save time — but note the obvious implementation does not work:
  `gocron.NewDefaultCron(false).IsValid` panics on this input too, since it is
  the same `ParseStandard` underneath, so validation needs its own `recover()`
  or a prefix pre-check. `native/schedule/cron.rs` answers `Err`
  there instead of panicking, deliberately, and has no vector for it: pinning a
  panic is pinning a bug.
