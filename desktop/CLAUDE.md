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
      runner.rs  an agent's config as SDK options; refuses what it cannot supply
      turn.rs    spawn, stream, and the AskUserQuestion continuation
      persist.rs what a finished turn writes, and what an interrupted one does not
      sse.rs     the frame bytes: raw pass-through vs the two synthetic events
    settings.rs  GET /api/settings and /settings/claude-config-dirs (a filesystem
                 probe; Unix, forwards on Windows); the preferences + config dirs
                 a read is scoped to; and `update`, the PUT — written and tested,
                 deliberately unclaimed
    monitoring.rs GET /api/monitoring — monitoring.json and the OTEL_* locks, no exporters
    version.rs   GET /api/version and /version/update-check (dev builds only)
    notifications/ the settings read (password masked), /log, the settings
                 write and the test send (#307)
      template.rs  html/template's escaper, and why the skeleton is Go's output
      smtp.rs      go-mail's TLS policy — `ssl_tls` is *STARTTLS*, not SMTPS
    integrations.rs GET /api/integrations, /{id}, /available-tools, /{id}/triggers —
                 credentials are never selected and auth is a bool made in SQL;
                 plus POST /api/integrations and the trigger-rule writes (#277).
                 PUT/DELETE /{id} stay with Go: they reload/stop the live MCP server
    integration_credentials.rs the seven per-type validators, and the two failures
                 whose Go error text is not reproducible (both forward)
    scan.rs      GET /api/claude-sessions/status, POST /refresh — and the scan
                 itself: the shell owns it now, the sidecar runs with AGENTO_SCANNER=off
    fs.rs        GET /api/fs — the working-dir picker's listing (Unix; forwards on Windows)
    uploads.rs   POST /api/uploads — the one multipart body, and the extension
                 allowlist that is the route's whole security boundary
    gopath.rs    Go's filepath.Clean/Dir/Join, pinned to vectors generated from Go
    query.rs     one query parameter, read the way r.URL.Query().Get reads it
    pricing.rs   GET /api/pricing/catalog, plus the rate Resolver — and the
                 three rate writes (#306): add and correct are not one upsert
    agents.rs    GET /api/agents and /api/agents/{slug}
    chats.rs     GET /api/chats and /api/chats/{id}; compact() is Go's, byte for byte
    tasks.rs     GET /api/tasks, /api/job-history and the three reads between them
    sessions/    GET /api/claude-sessions, /facets, /projects and /{id}, plus
                 POST /{id}/continue (#308)
      continue_chat.rs the two writes that resume a Claude session as a chat
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

Every `/api` request needs `Content-Type: application/json` — the server's
guard runs before the handler, even for GETs. `api.ts` does this for you.

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
| 3 ← | Storage + tasks | `internal/storage`, `internal/scheduler` | **In progress (#274).** `db.rs` is read-write, the 27 migrations are ported, and the writes whose every effect Rust owns are native — see below. Scheduler is #275. |
| 4 | Integrations | `internal/integrations`, `internal/trigger` | OAuth2 + MCP servers. Six of them: google, github, slack, jira, confluence, telegram, plus `internal/tools`. **How to host one is settled (#282)** — `claude::ToolServer`, see "Hosting a tool" below; do not invent a second way. WhatsApp is **not** among them — see below. |
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
  `native/chats.rs::compact` is the byte pass that avoids it.
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
run natively: an agent whose tools come from an integration or the local MCP
server needs #277/#282, and `runner::build_options` refuses those before any
subprocess exists. That would strand `/stop` for a chat still running on Go, so
the three steering routes answer natively **only when Rust holds a live session
for that chat** and forward otherwise. Go then answers — correctly, because it
is the side that has the session.

Five rules that are silent when broken, all pinned by `tests/chat_turn.rs` —
**except** the deny-with-the-user's-text half of `AskUserQuestion`, which is
reached only through a `can_use_tool` control request the fake CLI does not yet
issue. That gap is #298; do not read the list below as fully covered.

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

**The `tool_use` input must never round-trip through a `serde_json::Value`.**
The first version of `append_assistant_blocks` did, and turned `{"z":1.50,"a":1}`
into `{"a":1,"z":1.5}` — sorted and respelled, with nothing to signal it.
`tests/chat_turn.rs` caught it, which is also why that test's fake CLI emits
literal bytes rather than `json.dumps`: Python normalises `1.50` to `1.5` and
adds spaces, so a byte-exactness test cannot go through it.

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
  typo cannot silently disable the scan.
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
flip worked because `AGENTO_SCANNER=off` switched the Go half off; there is no
equivalent here, and forward-after-write is just the forward. It turns on with
the cut-over that deletes the sidecar.

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
  cannot reach them — the same wall #305 hit on `PUT /api/settings`, with no
  `AGENTO_SCANNER=off` equivalent to switch the Go half off.
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
