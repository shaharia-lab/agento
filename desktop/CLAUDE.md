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
    mcp.rs       loopback HTTP host for in-process MCP servers
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
    db.rs        read-only SQLite handle on the file the Go server owns
    settings.rs  GET /api/settings; also the preferences + config dirs a read is scoped to
    monitoring.rs GET /api/monitoring — monitoring.json and the OTEL_* locks, no exporters
    version.rs   GET /api/version and /version/update-check (dev builds only)
    notifications.rs GET /api/notifications/settings (password masked) and /log
    fs.rs        GET /api/fs — the working-dir picker's listing (Unix; forwards on Windows)
    gopath.rs    Go's filepath.Clean/Dir/Join, pinned to vectors generated from Go
    pricing.rs   GET /api/pricing/catalog, plus the rate Resolver
    agents.rs    GET /api/agents and /api/agents/{slug}
    chats.rs     GET /api/chats and /api/chats/{id}; compact() is Go's, byte for byte
    tasks.rs     GET /api/tasks, /api/job-history and the three reads between them
    sessions/    GET /api/claude-sessions and /facets; corpus.rs loads the lot
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
for months.

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
     `--test` flag to run them all.
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
| 3 | Storage + tasks | `internal/storage`, `internal/scheduler` | 27 SQLite migrations; reuse the same DB file and schema. Scheduler is cron/interval. |
| 4 | Integrations | `internal/integrations`, `internal/trigger` | OAuth2 + MCP servers. WhatsApp (`whatsmeow`) is the blocker — port it last or keep it in Go. |
| 5 | Agent execution | `internal/agent`, `internal/service` | Spawns the `claude` CLI over stream-json stdin/stdout. **The SDK underneath it is ported** (`src/claude/`, below); what remains is the runner, the chat service and the SSE handler on top of it. |

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

One deliberate scope call: `start_in_process_mcp_server` takes an `McpService`
trait rather than an `rmcp` server. Go hands its MCP SDK's `*mcp.Server` to its
own streamable-HTTP handler; the equivalent here would pin `rmcp`'s API to
satisfy no caller, since nothing has an MCP server to host until phase 4. The
seam sits exactly where Go's does — the SDK owns the listener, the caller owns
the tools — and an `rmcp` adapter is one impl away.

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
  field is rejected.
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

### Do not port

Telemetry/OTel, Prometheus metrics, and the self-updater are server concerns.
The desktop app should use Tauri's own updater instead.

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
OAuth and WhatsApp pairing, and agent CRUD — the reference instance has none of
those configured. Use the isolated dev instance for write testing.

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
