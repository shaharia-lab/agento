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
  native/        ported endpoints (phase 2+)
    mod.rs       endpoint registry, mode switch, response shaping
    gojson.rs    Go-compatible JSON encoder — read this before porting anything
    gotime.rs    Go's time.Time on the wire
    db.rs        read-only SQLite handle on the file the Go server owns
    settings.rs  user preferences + Claude config dirs a read is scoped to
    pricing.rs   GET /api/pricing/catalog, plus the rate Resolver
    agents.rs    GET /api/agents and /api/agents/{slug}
    sessions/    GET /api/claude-sessions and /facets; corpus.rs loads the lot
    analytics/   GET /api/claude-analytics
      buckets.rs Go's time.Date/AddDate and the bucket walks, in the request's tz
      params.rs  from/to/project/tz, and the granularity the window picks
      report.rs  every aggregate in the payload
      cards.rs   the Insights cards
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
     (`desktop/parity/`, `go test ./desktop/parity/ -update-golden`). Build the
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

### Phase order (easiest → hardest)

| Phase | Subsystem | Go source | Notes |
|---|---|---|---|
| 1 ✅ | Sidecar + proxy | — | done |
| 2 ← | Pricing + analytics | `internal/pricing`, `internal/claudesessions` | Pure computation over JSONL + SQLite. No external deps. **In progress**: `/api/pricing/catalog`, `/api/claude-sessions`, `/api/claude-sessions/facets`, `/api/claude-analytics` and the agent reads are native and diff clean. Next: `/api/claude-sessions/insights/summary`. |
| 3 | Storage + tasks | `internal/storage`, `internal/scheduler` | 27 SQLite migrations; reuse the same DB file and schema. Scheduler is cron/interval. |
| 4 | Integrations | `internal/integrations`, `internal/trigger` | OAuth2 + MCP servers. WhatsApp (`whatsmeow`) is the blocker — port it last or keep it in Go. |
| 5 | Agent execution | `internal/agent`, `internal/service` | Spawns the `claude` CLI over stream-json stdin/stdout. Hardest, highest risk. |

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
- **`GET /chats/{id}` returns `{session, messages}`**, not a flattened session.
- **The project filter differs between endpoints.** `/claude-analytics` matches
  `decoded_path`; `/claude-sessions` matches `project_path` literally, which is
  the dash-encoded name for some sessions and a real path for others. Sending
  the wrong one returns an empty result with no error — a silent wrong answer.
- **Go `omitempty` drops zero values** the JSON otherwise implies are always
  present (`InsightCard.percent/count/model`, `ProjectBreakdown.folded_projects`,
  `SessionFacets.config_dirs`). Default with `?? 0`; do not trust the type.
- **Empty arrays are inconsistent**: `/claude-analytics` sends `[]`, but
  `/claude-sessions/insights/summary` sends `null` for every `top_*` list.
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

Nothing else is ported. Every write path, the insights summary, the per-session
reads and every other read still forward to Go.

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
