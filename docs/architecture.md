# Architecture

How Agento is put together, and why.

For the exhaustive working notes, including the reasoning behind individual
decisions, read [`CLAUDE.md`](../CLAUDE.md). This document is the map;
that one is the territory.

- [The short version](#the-short-version)
- [Process model](#process-model)
- [Why there is a local HTTP server](#why-there-is-a-local-http-server)
- [The native backend](#the-native-backend)
- [The Claude Agent SDK](#the-claude-agent-sdk)
- [Data](#data)
- [The frontend](#the-frontend)
- [Design principles](#design-principles)
- [The frozen goldens in `parity/`](#the-frozen-goldens-in-parity)
- [What Agento deliberately does not do](#what-agento-deliberately-does-not-do)

---

## The short version

| Layer | Technology |
| --- | --- |
| Shell and windowing | Tauri 2 |
| Backend | Rust, `axum`, `rusqlite` |
| Frontend | React 18, TypeScript, Vite |
| Storage | SQLite at `~/.agento/agento.db` |
| Agent runtime | The Claude Code CLI, spawned as a subprocess |

One process. Nothing bundled beside it, nothing fetched at runtime. The only
external dependency is the Claude Code CLI, which is not redistributed.

The wire format the backend and the frontend agree on is specified by the frozen
goldens in `parity/`, which CI asserts on every run — see
[The frozen goldens in `parity/`](#the-frozen-goldens-in-parity).

---

## Process model

```
┌─ Agento ────────────────────────────────────────────────┐
│ Tauri (Rust)                                            │
│   lib.rs    create data dir, migrate, seed pricing      │
│   proxy.rs  axum on 127.0.0.1:<port>                    │
│      /api/*  ──> native/ endpoint registry              │
│      /*      ──> the embedded frontend (release only)   │
│   menu.rs   native menu (macOS), emits menu://action    │
│                                                         │
│   WebView ──> React UI ──> fetch("/api/...")            │
└─────────────────────────────────────────────────────────┘
                     │
                     └── spawns `claude` per agent run
```

Startup order in `lib.rs`:

1. Resolve the data directory (`paths.rs`).
2. Create the SQLite database if it does not exist, apply migrations, seed the
   pricing catalog.
3. Bind the axum server on loopback.
4. Start the background scan of the Claude Code corpus.
5. Start the task scheduler.
6. Show the window.

| | dev | release |
| --- | --- | --- |
| UI served by | Vite on `:1420` | the Rust server, from embedded assets |
| `/api` reaches Rust via | Vite proxy to `:8991` | same origin |
| Server port | fixed `8991` | assigned by the OS |
| Data directory | `~/.agento-desktop-dev` | `~/.agento` |
| `/api` credential | a JWT signed by the install's key, **also** written to `<data dir>/api-token` (0600) so `curl` and Vite's proxy can reach the API | a JWT, delivered over IPC only |
| Signing key | `~/.agento-desktop-dev/api-signing-key.pk8` — its own, so a dev launch can never mint a token the release install honours | `<data dir>/api-signing-key.pk8` |

Dev uses its own data directory on purpose. Two Agento processes sharing
`~/.agento` share one SQLite file and one scheduler, so scheduled tasks fire
twice.

---

## Why there is a local HTTP server

A Tauri app could call Rust through `invoke` commands instead. This one serves
HTTP on loopback because:

- **One origin in front of both halves sidesteps CORS entirely**, and keeps
  server-sent events intact. Chat streaming is a POST whose response is an SSE
  stream, which an `invoke` shim cannot carry.
- **The frontend talks plain HTTP**, so `fetch("/api/...")` works unchanged in a
  browser tab during development.

Do not remove it to "simplify".

Because the server is reachable by anything on the machine, `guards.rs` applies
three protections before routing:

- **`validateHost`** rejects a `Host` header the server is not served under. DNS
  rebinding otherwise makes an attacker's page same-origin, at which point CORS
  stops applying.
- **A signed bearer token** rejects a request that does not carry a valid
  `Authorization: Bearer <jwt>`. The two guards either side of it are
  *browser*-shaped and neither stops a local process: `curl` sends a loopback
  `Host` and sets its own content type, and this API can create a
  `bypass`-permission agent and run it. The app's own credential is delivered to
  the webview over Tauri IPC — the one channel another local process cannot
  reach.

  Since #405 the credential is an **EdDSA (Ed25519) JWT signed by a per-install
  keypair** rather than #400's opaque per-launch string, which is what lets the
  user hand out access deliberately instead of copying the app's own token out
  of a debug file. Three properties follow, and they are why the design is
  asymmetric rather than a table of hashed opaque keys:

  - **Scopes.** `read` serves `GET`/`HEAD`/`OPTIONS`; `write` serves everything,
    and *is* arbitrary command execution, so the creation UI says so. The split
    is `is_state_changing`, reused, so there is one definition of it in the tree.
  - **Offline verification.** `GET /.well-known/jwks.json` publishes the public
    key, unauthenticated, so another local service can verify a token with a
    stock JWT library and no Agento code — which opaque keys cannot do at all.
  - **Regenerate invalidates everything**, with no denylist and no per-token
    bookkeeping: every previously issued signature simply stops verifying. Single
    tokens are revoked individually by `jti`.

  A **401** means the caller has not proved who it is (absent, malformed, signed
  by another key, expired, wrong `aud`, revoked); a **403** means it has, and
  the token's scope does not cover this request.
- **`requireJSONContentType`** rejects a state-changing request that does not
  declare `application/json`. Without it a cross-origin `POST` carrying
  `text/plain` is a CORS simple request, sent with no preflight, and the side
  effect lands even though the response is unreadable.

A body-less request is not exempt: several state-changing endpoints take no body,
and a `POST` with neither is itself a simple request.

The order is load-bearing. A request failing both the `Host` check and the token
reads **403**, not 401; an unauthenticated one is refused **401** before it is
told the content-type rule.

All three are scoped to `/api`, so the SPA document, `/health` and the Telegram
webhook — which arrives with a foreign `Host` and authenticates with its own
secret — are untouched. The token is defence in depth, not a new boundary: a
process running as this user can read `agento.db` directly. Where it is a real
boundary is a multi-user machine, since loopback is not user-scoped.

---

## The native backend

`src-tauri/src/native/` is one module per API area. Each declares its own
`claims` (which requests it answers) and `serve` (how), and `ENDPOINTS` in
`native/mod.rs` lists them.

Two properties are load-bearing:

- **Claiming a route and implementing it are one edit**, in the module they
  belong to. A route cannot end up claimed by a handler that does not exist.
- **Adding an endpoint is one appended line** plus a file. A single `match` over
  every route meant two ports in flight always collided in the same hunk.

A test asserts no two endpoints claim the same request, because the registry's
one failure mode is a silent first-wins overlap.

### Two registries

| Registry | Shape | Used by |
| --- | --- | --- |
| `Endpoint` | Sync `fn`, returns a buffered `Vec<u8>` | Everything that hands back a finished document |
| `StreamEndpoint` | Async, returns a `Response<Body>` | The chat turn, which lasts as long as the model talks |

### Threading

`rusqlite` is blocking, and the connection sets a five second busy timeout, so a
call that meets a lock parks its thread. `proxy.rs` runs buffered handlers on the
blocking pool for that reason.

Anything reached from a **timer** or a **webhook** rather than a request is not
covered by that, so the scheduler, the trigger dispatcher and the chat turn's
persistence all hand database work to `db::blocking(label, f)`. It is greppable
on purpose. A regression test drives a one-worker runtime against a held write
lock and fails if the hand-off is removed.

### Areas

Roughly, one directory or file per area:

| Module | Covers |
| --- | --- |
| `scanner/` | Reading Claude Code transcripts into the cache |
| `insights/` | Nine passes producing a per-session insight row |
| `analytics/` | The dashboards, bucketed in the request's timezone |
| `sessions/` | The paged session list, facets, detail, continue-as-chat |
| `chat/` | The SSE turn and the three routes that steer it |
| `schedule/` | When a task fires, and running it |
| `integrations/` | Six in-process MCP servers, and their lifecycle |
| `tools/` | Agento's own local in-process tools |
| `pricing.rs` | The rate catalog and resolver |
| `agents.rs`, `chats.rs`, `tasks.rs` | Ordinary CRUD |
| `gojson.rs`, `gotime.rs`, `gourl.rs`, `gopath.rs` | The wire format's own encoding rules |

Those last four are the ones to read before touching a response. Agento's wire
format is exact — key order, float spelling, HTML escaping, timestamp format,
path and URL escaping are all part of the contract, and none of them is what a
Rust crate does by default. `3` and not `3.0`, `<` and not `<`, declared
field order and not sorted. The `go` prefix is historical: these are transcribed
from the format's original implementation, and `parity/` is what pins them.

---

## The Claude Agent SDK

`src-tauri/src/claude/` is a Rust port of
[`claude-agent-sdk-go`](https://github.com/shaharia-lab/claude-agent-sdk-go), the
library every agent run goes through.

It is **not an API client.** It spawns the `claude` CLI and speaks stream-json
over stdio plus a control protocol. There is no inference to reimplement and no
API key: the CLI's own sign-in is the credential.

Four protocol facts that are expensive to rediscover:

- **The handshake order matters.** Reader task live, register the request id,
  write `initialize`, block on the acknowledgement, and only then send the first
  user message. Getting it wrong races rather than fails.
- **`sdkMcpServers` is never sent.** The CLI accepts only strings there, and a
  rejection fails the entire initialize, silently taking hooks, agents, the
  system prompt and the output format with it. MCP servers travel as
  `--mcp-config`.
- **`control_response` routes on the nested `response.request_id`**, and the
  caller gets the innermost payload.
- **Every inbound `control_request` must be answered.** A missing reply hangs the
  CLI with no error on either side. `can_use_tool` with no handler is answered
  with an error, never an allow.

### Tools and MCP

Every integration is an in-process MCP server, hosted over `rmcp` (the official
Rust MCP SDK) on a loopback listener. `claude::ToolServer` is the typed-tool
layer: schemas are derived from the input struct, and tools are registered at
runtime from the integration's allowlist.

Two rules:

- **A tool's error is text the model reads**, never a protocol error. A protocol
  error renders as "tool result missing due to internal error", which tells the
  model nothing to retry against.
- **Every server requires a bearer token.** From the moment those servers answer
  with the user's live Slack, GitHub and Google credentials, loopback is not a
  boundary: it separates hosts, not processes.

---

## Data

One SQLite file, `~/.agento/agento.db`, holding agents, chats, messages,
scheduled tasks, job history, integrations, settings, the pricing catalog, and
the cache of the Claude Code corpus.

Migrations are embedded from `parity/migrations_vectors.json` and applied at
startup. That file **is** the schema: add a migration by appending to it, never
by writing DDL somewhere else.

Two rules that surprise people:

- **Cost is stored, not derived.** Each session's cost is computed at scan time,
  per assistant message, against the rate in effect at that message's timestamp.
  Analytics sums stored values and never re-prices. Editing a rate therefore
  forces a re-read of the corpus, which is what the pricing revision fingerprint
  in `claude_cache_metadata` is for.
- **Duration means active duration.** Sessions are resumable, so the span from
  first to last event counts idle days. Gaps beyond a user-configurable
  threshold are excluded, and the resulting figure is stored, so changing the
  threshold invalidates the same rows a rate edit does.

The scan is parallel-read and batched-write: a small pool decodes transcripts,
one writer commits them in batches, because SQLite serializes writers and a
transaction per file was thousands of fsyncs on a full re-read.

---

## The frontend

```
src/
  lib/
    api.ts       fetch wrapper, and POST-based SSE for chat streaming
    types.ts     TypeScript mirrors of the API's JSON, field for field
    hooks.ts     useResource / useDebounced / usePoll / describeError
    format.ts    numbers, money, durations, relative time
    stats.ts     the sidebar and status bar counters
    nav.ts       sections and view ids
    icons.tsx    16px, 1.5 stroke icon set
    tauri.ts     window and menu bridge; degrades to a plain browser tab
    updater.ts   the in-app update flow
  components/    TitleBar, Sidebar, StatusBar, CommandPalette, ui.tsx
  views/         one file per section
  styles/        tokens, base, shell, controls, views, plus per-view files
```

**Wire types are not translated.** `types.ts` uses the API's field names exactly
as they travel, snake_case included. Renaming at the boundary would only hide
drift. An empty slice serialises as `null`, so every array field is
`T[] | null` — handle it.

**`lib/tauri.ts` degrades.** Every Tauri call is behind it, so `npm run dev` in a
browser tab still renders the UI.

---

## Design principles

Agento is a desktop application and is built like one. The obvious way to lay
out this feature set is as a page — a nav rail, a large heading, a primary
button top right, a wide column of cards. That is correct for a browser and
wrong for a window, so none of it is here.

- **Three panes, not one page.** Every section is list, detail, inspector. You
  navigate within a window instead of replacing its contents, which is why there
  is no page-level heading anywhere.
- **Density.** 14px base type, 28px rows, hairline borders. The web defaults of
  roughly 16px and 48px read, at desktop distances, as a website in a frame.
- **A status bar.** Persistent and always truthful. Web apps rarely have one.
- **Selection behaves natively.** Rows fill with the accent colour when the
  window is focused and drop to neutral grey when it is not. It is the single
  strongest "this is a real app" signal, and almost nothing browser-shaped
  does it.
- **Keyboard first.** A command palette, plus a native macOS menu that emits
  actions the webview handles, so menu and shortcut run identical code.
- **No browser affordances.** Text is not selectable except where you would read
  or copy it, the context menu is suppressed, scrollbars are overlay style, and
  focus rings appear for keyboard navigation only.
- **Window chrome belongs to the OS.** `decorations: true` everywhere. macOS gets
  its native traffic lights over the app's own titlebar strip; Linux and Windows
  get an ordinary decorated titlebar.

Theming: tokens are defined on bare `:root` for light, then re-declared under
both `@media (prefers-color-scheme: dark)` and `:root[data-theme="dark"]`, so an
explicit choice wins in either direction. Never give a colour its only definition
inside a media block.

---

## The frozen goldens in `parity/`

`parity/` is Agento's **wire-format specification**, and CI asserts it on every
run. The bar it encodes is **byte-identical JSON**: field names, key order,
escaping and float spelling are all part of the contract, and only a byte
comparison catches all four.

It holds two shapes of file:

1. **Response goldens** — a fixture the code builds, compared against a recorded
   answer. `claude_analytics_golden.json` and `pricing_catalog_golden.json` are
   the large ones.
2. **Primitive vectors** — inputs paired with their exact expected outputs, for
   the encoding rules everything else sits on. `gopath_vectors.json`,
   `gourl_vectors.json` and `gojson_vectors.json` are the ones a response bug
   usually bottoms out in.

Alongside them, `write_routes.json` and `read_routes.json` record every route
and its disposition, and the Rust tests assert each route's real `claims()`
matches. A route cannot be claimed or unclaimed without that file moving.

**Nothing regenerates these files.** They are a decision, not a snapshot, and
that is the whole point of the directory: read
[`parity/README.md`](../parity/README.md) before touching one. A golden changed
until the tests go green is a contract silently rewritten — if a change to one
is correct, it is correct for a reason you can state in the commit message.

---

## What Agento deliberately does not do

| Not here | Why |
| --- | --- |
| **WhatsApp** | `whatsmeow` has no Rust equivalent and will not be reimplemented. Existing rows survive and are read-only |
| **OpenTelemetry and Prometheus** | Infrastructure concerns, and this is a local app. The two config routes answer 501 rather than 404, which would read as a version mismatch |
| **A self-updater of its own** | Tauri's updater does it. `GET /api/version/update-check` short-circuits so the two cannot race |

Instead of telemetry, the app writes one access line per API request to a
rotating log file, plus a line per write saying what it did. No bodies, no
headers, no query strings.
