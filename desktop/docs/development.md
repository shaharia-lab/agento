# Development

Setting up, running and testing Agento Desktop locally.

Read [Architecture](architecture.md) first if you have not. The full working
notes, with the reasoning behind individual decisions, are in
[`desktop/CLAUDE.md`](../CLAUDE.md).

- [Prerequisites](#prerequisites)
- [Setup](#setup)
- [Running it](#running-it)
- [Project layout](#project-layout)
- [Tests](#tests)
- [Parity testing](#parity-testing)
- [Conventions](#conventions)
- [Debugging](#debugging)
- [Branches and pull requests](#branches-and-pull-requests)

---

## Prerequisites

| Tool | Version |
| --- | --- |
| Rust | stable, 1.88 or newer |
| Node.js | 22 |
| Go | 1.25 or newer, only for the parity tests |
| Claude Code CLI | any recent version, for running agents |

**Linux system packages**, once:

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Fedora and openSUSE have the same libraries under their own names. macOS needs
Xcode command line tools; Windows needs the MSVC build tools.

The crate declares Rust 1.88 as its minimum. That floor is what makes cargo
resolve `rmcp` 3.x rather than silently falling back to a superseded major, and
it turns on clippy's version-gated lints.

---

## Setup

```bash
git clone https://github.com/shaharia-lab/agento.git
cd agento
git checkout desktop
cd desktop
npm install
```

Note the branch. Desktop work happens on `desktop`, not `main`.

---

## Running it

All commands run from `desktop/`.

| Command | What it does |
| --- | --- |
| `npm run app` | The real desktop window, with hot reload |
| `npm run dev` | Vite only, in a browser tab. Fastest loop for pure layout work |
| `npm run build` | Typecheck and build the frontend alone |
| `npm run app:build` | Native installers for the current platform |
| `cd src-tauri && cargo build` | Backend only |

`npm run app` runs against `~/.agento-desktop-dev`, **not** your real `~/.agento`.
That is deliberate: two Agento processes on one data directory share a scheduler,
so every scheduled task would fire twice. Release builds use the real directory.

To seed the dev instance with data, copy your real database into it, or create
rows through the UI.

### Pointing at a different Claude CLI

```bash
AGENTO_CLAUDE_EXECUTABLE=/path/to/claude npm run app
```

This is also how the turn tests point the runner at a scripted fake CLI.

---

## Project layout

```
desktop/
  src/                 the React frontend
  src-tauri/
    src/lib.rs         startup: database, migrations, pricing seed, window, menu
    src/proxy.rs       the axum server; routes every request into native/
    src/guards.rs      the Host and Content-Type guards, applied before routing
    src/menu.rs        the native macOS menu
    src/paths.rs       data directory and database path
    src/claude/        the Claude Agent SDK, ported from Go
    src/native/        the ported backend, one module per API area
    tests/             integration tests, including the parity suites
  parity/              cross-language fixtures, asserted by both Go and Rust
  scripts/
    parity-instance.sh a Go server built from this checkout, on a copy of the DB
  docs/                this documentation
  CLAUDE.md            the full working notes
```

---

## Tests

```bash
cd src-tauri

cargo test                    # unit tests and the non-ignored integration tests
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

From `desktop/`:

```bash
npm run build                 # tsc --noEmit plus the Vite build
```

CI runs exactly those four. It only runs on changes under `desktop/`,
`.github/workflows/desktop-*`, and the Go sources the parity fixtures come from.

### Tests that need something

Several suites are `#[ignore]`d because they need a corpus, a database or a
running Go server that CI does not have.

```bash
# The scan, against a copy of your real corpus.
cargo test --test scan_live -- --ignored --nocapture

# The MCP server, dialled by the real Claude Code CLI.
cargo test --test claude_mcp_live -- --ignored --nocapture
```

Verify scanner changes against real data, not a fixture. The failure that matters
is a scan that runs, reports success and writes nothing, and a three-file fixture
cannot tell that apart from a healthy one.

### The fake CLI

`tests/claude_sdk.rs`, `tests/chat_turn.rs` and `tests/scheduled_run.rs` drive a
scripted fake `claude` CLI: a Python program that logs every line it receives on
stdin and replies to order. It is the only way to test things that are properties
of a **sequence** rather than of a function, such as the handshake order or a
disconnect while a permission prompt is pending.

Two traps in writing those tests:

- **The fake must not exit after the result.** A real CLI in session mode stays
  alive. A fake that exits closes stdout, ends the stream for free, and passes
  against a drain that would never terminate on its own.
- **Assert the revert fails.** A disconnect test that passes with and without the
  fix is testing nothing.

---

## Parity testing

The Rust backend is a port of the Go server, and correctness means matching it
byte for byte. See [Architecture](architecture.md#parity-with-the-go-server) for
why.

### Fixtures

`parity/` holds files generated from Go and asserted by both languages.
Regenerate from the repository root:

```bash
go test ./desktop/parity/ -run TestGitHubVectors    -update-github-vectors
go test ./desktop/parity/ -run TestWriteRoutes      -update-write-routes
go test ./internal/scheduler/ -run TestScheduleVectors -update-scheduler-vectors
go test ./internal/storage/ -update-migration-vectors
```

Each vector file names its own generator in the test beside it. Regenerate
**after** deciding whether the change was intended: these files are the record of
what Go does, not a snapshot to be refreshed on failure.

### Live diffs

```bash
cd desktop
eval "$(./scripts/parity-instance.sh start)"
(cd src-tauri && cargo test --test parity_analytics -- --ignored --nocapture)
./scripts/parity-instance.sh stop
```

To run every suite, drop `--test` and add `--no-fail-fast`. Without it, cargo
stops at the first failing test **binary**, so one red suite hides every suite
after it.

Notes:

- `parity-instance.sh` builds the Go server from this checkout and runs it
  against a **copy** of `~/.agento`. Never diff against the Agento you have
  installed: it drifts behind the repository, and a stale baseline that happens
  to agree hides a real divergence.
- It is safe to run concurrently from separate checkouts. Two agents sharing one
  checkout need `AGENTO_PARITY_WORKER=<id>` or `AGENTO_PARITY_DIR=<path>`.
- `parity_writes` **mutates**, unlike every other suite, so it refuses to run
  unless `AGENTO_LIVE_URL` is set rather than defaulting to `:8990`.
- `parity_claude_settings` refuses to run unless `CLAUDE_CONFIG_DIR` points
  somewhere other than `~/.claude`, because it overwrites `settings.json`.

### Go is not always byte-stable

Several Go analytics builders collect into a map, which iterates randomly, and
then sort unstably, so two rows tying on the sort key come out in either order.
The Rust port sorts stably, so it matches only one of the orderings Go produces.

Before assuming a diff is your bug, **ask Go the same question twice.**

---

## Conventions

### Porting a route

1. **Read `native/gojson.rs` first.** Rust's natural JSON is not Go's. Encode
   through `gojson::to_vec`, keep struct fields in the Go struct's declaration
   order, and use `skip_serializing_if` for `omitempty`.
2. Mirror the Go source's ordering and grouping exactly, including anything
   hashed. A fingerprint over rows in a different order is a different
   fingerprint for identical data.
3. Prove it two ways: a fixture both languages build against a Go-written golden,
   and the live diff against real data.
4. Only then leave it claimed.

### JSON decoding

Go's `json.Unmarshal` treats `null` as a no-op for every type here, and serde
rejects it. Three helper types exist for that, and they are **types rather than
`deserialize_with` functions** for a reason recorded in `gojson.rs`: a function
makes the field required, which forces `#[serde(default)]` on every call site,
which in turn widens the struct into accepting short positional arrays that Go
refuses.

| Helper | Covers |
| --- | --- |
| `null_is_zero_value` | A `null` scalar field |
| `GoList<T>` / `GoMap<V>` | A `null` element or map value |
| `GoStruct<T>` | A struct built positionally from a JSON array |

The direction matters. An over-**reject** is visible. An over-**accept** writes a
row Go would refuse, with nothing to report it.

### Wire types

`src/lib/types.ts` uses the Go `json:` tags verbatim. Every array field is
`T[] | null`, because Go marshals a nil slice as `null`.

Every state-changing `/api` request needs `Content-Type: application/json`,
including the ones with no body. `api.ts` does this for you.

### UI

- Reuse the existing CSS classes. New CSS goes in a per-view file imported by
  that view.
- **No `window.confirm`, `alert` or `prompt`.** They block the WebView and can
  wedge the app. Render inline confirmation UI.
- External links must go through `openExternal` in `lib/tauri.ts`.
  `window.open` and `target="_blank"` do not leave a Tauri webview.
- Directory fields use the native picker via `pickDirectory`. The in-app browser
  is the plain-browser fallback only.
- Charts are inline SVG using the theme tokens. No chart libraries.
- `.card` sets `overflow: hidden`, so as a flex child of a scrolling column its
  min-content height collapses to zero. Dashboard containers need
  `> * { flex: 0 0 auto; }`.

### Database access off the request path

Anything reached from a timer, a webhook or a stream must hand its database work
to `db::blocking(label, f)`. The label is per call site so the log says which
section panicked. See
[Architecture](architecture.md#threading).

### Logging

`message key=value`, every string value with `{:?}`, at `info`, and **after** the
effect. Failures at `warn`, writes at `info`, successful reads at `debug`.

Never log a body, a header or a query string. Bodies here are chat prompts,
system prompts and integration credentials; query strings carry search terms and
project paths.

Every write log line has a test asserting it is emitted. A line with no test is a
line that quietly stops being emitted.

---

## Debugging

**The webview inspector** is available in dev builds: right-click, Inspect
Element, or the usual devtools shortcut.

**The log file** is where backend problems land:

| Platform | Path |
| --- | --- |
| Linux | `~/.local/share/com.shaharialab.agento/logs/Agento.log` |
| macOS | `~/Library/Logs/com.shaharialab.agento/Agento.log` |
| Windows | `%LOCALAPPDATA%\com.shaharialab.agento\logs\Agento.log` |

In `npm run app` the same lines go to the terminal.

**Curling the API** works in dev, since the server is on a fixed port:

```bash
curl -s http://127.0.0.1:8991/api/agents | jq
```

Add `-H "Content-Type: application/json"` for anything that is not a `GET`, or
the guard answers 415 before the handler runs.

There is also a project skill, `local-verify`, describing how to reproduce a bug
before fixing it and how to verify each hop separately: backend wire, browser
engine, real Tauri webview, then the UI flow. Use it for anything that "works in
Chrome but not in the app".

---

## Branches and pull requests

- Desktop work happens on the **`desktop`** branch. PRs target `desktop`, not
  `main`.
- `main` carries the Go server and is left alone until the two converge.
- Desktop releases are tagged `desktop-v*` from `desktop`. The Go server keeps
  its own `v*` tags. The two patterns do not overlap, so both ship independently
  from one repository.

Before opening a PR:

```bash
cd desktop && npm run build
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

Releasing is documented separately in [Releasing](releasing.md).
