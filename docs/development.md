# Development

Setting up, running and testing Agento Desktop locally.

Read [Architecture](architecture.md) first if you have not. The full working
notes, with the reasoning behind individual decisions, are in
[`CLAUDE.md`](../CLAUDE.md).

- [Prerequisites](#prerequisites)
- [Setup](#setup)
- [Running it](#running-it)
- [Project layout](#project-layout)
- [Tests](#tests)
- [The wire format is exact](#the-wire-format-is-exact)
- [Conventions](#conventions)
- [The LLM gateway](#the-llm-gateway)
- [Debugging](#debugging)
- [Branches and pull requests](#branches-and-pull-requests)

---

## Prerequisites

| Tool | Version |
| --- | --- |
| Rust | stable, 1.88 or newer |
| Node.js | 22 |
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
npm install
```

---

## Running it

All commands run from the repository root.

| Command | What it does |
| --- | --- |
| `npm run app` | The real desktop window, with hot reload |
| `npm run app:alongside` | The same, but without taking over an installed Agento's window |
| `npm run dev` | Vite only, in a browser tab. Fastest loop for pure layout work |
| `npm run build` | Typecheck and build the frontend alone |
| `npm run app:build` | Native installers for the current platform |
| `cd src-tauri && cargo build` | Backend only |

`npm run app` runs against `~/.agento-desktop-dev`, **not** your real `~/.agento`.
That is deliberate: two Agento processes on one data directory share a scheduler,
so every scheduled task would fire twice. Release builds use the real directory.

To seed the dev instance with data, copy your real database into it, or create
rows through the UI.

### Running beside an installed Agento

`npm run app` **cannot** run while an installed Agento is open. It will exit
immediately with no output and focus the installed window instead — which looks
like the command silently failing.

Nothing is wrong and nothing is shared: `tauri-plugin-single-instance` derives
its identity from the app identifier, and dev and release have the same one, so
the dev launch finds the installed app's claim and hands off to it.

```bash
npm run app:alongside
```

merges a one-key config override (`src-tauri/tauri.alongside.conf.json`) that
changes the identifier to `com.shaharialab.agento.dev`. That is the only lever
that works on every platform: the plugin accepts an explicit `dbus_id` on Linux
alone, while Windows uses a named mutex and macOS a socket path, both derived
from the identifier. On Linux you can watch both claims coexist:

```
$ busctl --user list | grep shaharialab
com.shaharialab.agento.SingleInstance       …  agento
com.shaharialab.agento.dev.SingleInstance   …  agento
```

The two process names are identical — both binaries are `agento` since `v1.0.0`
— so it is the bus name that tells them apart, which is the whole point of the
override.

**It does not help you test anything origin-dependent.** A dev build loads the
configured `devUrl`, which Tauri's ACL treats as a *local* origin, so the whole
class of permission bug that only affects release builds is invisible in dev by
construction. That needs `npm run app:build`.

### Pointing at a different Claude CLI

```bash
AGENTO_CLAUDE_EXECUTABLE=/path/to/claude npm run app
```

This is also how the turn tests point the runner at a scripted fake CLI.

---

## Project layout

```
src/                 the React frontend
src-tauri/
  src/lib.rs         startup: database, migrations, pricing seed, window, menu
  src/proxy.rs       the axum server; routes every request into native/
  src/guards.rs      the Host and Content-Type guards, applied before routing
  src/menu.rs        the native macOS menu
  src/paths.rs       data directory and database path
  src/claude/        the Claude Agent SDK
  src/native/        the backend, one module per API area
  src/gateway/       the embedded LLM gateway's own listener — not part of /api
  tests/             integration tests
parity/              the frozen wire-format spec; see parity/README.md
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

From the repository root:

```bash
npm run build                 # tsc --noEmit plus the Vite build
```

CI runs exactly those four, on every push and pull request, unfiltered — with
one tree there is no change that cannot affect the app.

### Tests that need something

Several suites are `#[ignore]`d because they need a corpus or a database that CI
does not have.

```bash
# The scan, against a copy of your real corpus.
cargo test --test scan_live -- --ignored --nocapture

# The MCP server, dialled by the real Claude Code CLI.
cargo test --test claude_mcp_live -- --ignored --nocapture

# The search index, against a copy of your real corpus: correctness, plus
# printed build / index-size / query-latency numbers. Run it under --release
# when you care about the numbers — the bundled SQLite is a C dependency
# compiled at the profile's optimization level, so a debug run measures a
# SQLite nobody ships.
cargo test --test search_live -- --ignored --nocapture
cargo test --release --test search_live -- --ignored --nocapture
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

## The wire format is exact

Agento's JSON is specified down to the byte: field names, key order, escaping,
float spelling. `parity/` is that specification, and CI asserts it on every run.

**Read [`parity/README.md`](../parity/README.md) before touching one of those
files.** It records which of them are *audits* rather than fixtures and what
that costs, and how each is read — `include_str!` with a relative path, or
anchored to `CARGO_MANIFEST_DIR`.

A golden changes by deliberate edit with a reason in the commit message, never
by "refresh until green". Nothing regenerates them, and that is deliberate: a
golden re-recorded to match new behaviour does not prove the new behaviour is
right, it only removes the thing that would have told you it changed.

One shape you will meet and should not tidy: `parity/claude_analytics_golden.json`'s
fixture is built with **no ties on any sort key**. A tie makes the expected
ordering ambiguous, and an ambiguous golden is a flaky test.

---

## Conventions

### Adding an endpoint

1. **Read `native/gojson.rs` first.** Rust's natural JSON is not Agento's.
   Encode through `gojson::to_vec`, keep struct fields in the order they should
   appear on the wire, and use `skip_serializing_if` to omit empty values.
2. Ordering and grouping are part of the answer, including anything hashed — a
   fingerprint over rows in a different order is a different fingerprint for
   identical data.
3. Add it to the endpoint registry in `native/mod.rs`, record it in
   `parity/read_routes.json` or `write_routes.json`, and cover it with a test
   beside the module.

### JSON decoding

`null` has to decode as a zero value rather than a type error, and serde rejects
it by default. Three helper types exist for that, and they are **types rather
than `deserialize_with` functions** for a reason recorded in `gojson.rs`: a
function makes the field required, which forces `#[serde(default)]` on every
call site, which in turn widens the struct into accepting short positional
arrays that must be refused.

| Helper | Covers |
| --- | --- |
| `null_is_zero_value` | A `null` scalar field |
| `GoList<T>` / `GoMap<V>` | A `null` element or map value |
| `GoStruct<T>` | A struct built positionally from a JSON array |

The direction matters. An over-**reject** is visible and loud. An
over-**accept** writes a row that should have been refused, with nothing to
report it.

### Wire types

`src/lib/types.ts` mirrors the API's field names verbatim, snake_case included.
Every array field is `T[] | null`, because an empty slice serialises as `null`.

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

## The LLM gateway

`src-tauri/src/gateway/` is a **second HTTP listener**, and the thing to get
straight before changing anything in it is that it is not part of `/api`.

| | `/api`, in `native/` | the gateway, in `gateway/` |
| --- | --- | --- |
| Port | the app's own | the user's, 8880 by default |
| Wire format | Agento's | OpenAI's and Anthropic's |
| Credential | `read` / `write` | `llm`, and only `llm` |
| Guards | `guards.rs`, before routing | its own auth layer, and `guards::host_allowed` **shared** rather than copied |
| Route tables | `parity/read_routes.json`, `write_routes.json` | **none — they say nothing about it** |

So the parity machinery does not apply to the five routes it serves
(`/v1/chat/completions`, `/v1/models`, `/anthropic/v1/messages`,
`/anthropic/v1/models`, `/healthz`): there is no Go ancestor to be byte-identical
to, and the wire format that matters is somebody else's.

Its **control plane is the opposite** and this is where the split gets confused.
`/api/gateway/*` — fourteen routes in `native/gateway_api.rs` — is ordinary `/api`
surface, behind the ordinary guard with ordinary `read`/`write` scoping, and it
is recorded in `parity/desktop_routes.json`. That file holds the routes with no
Go ancestor, and its assertion is **set equality** against the union of every
owning module's `ROUTES` const. Add a third owner and you must add it to that
union, or the test silently weakens to a one-directional check and still passes.

An `llm` token opens none of those fourteen routes. That is the same disjointness
seen from the other side: a credential issued to *spend* through the gateway must
not be able to reconfigure which provider it spends with.

### The `ferrox-providers` dependency

Every translation and every provider adapter comes from `ferrox-providers`, the
crate extracted from [ferrox](https://github.com/shaharia-lab/ferrox) with this
app as its intended desktop consumer. What lives here is only the listener, the
auth, the routing table and the framing.

```toml
ferrox-providers = { git = "https://github.com/shaharia-lab/ferrox",
                     tag = "providers-v0.1.0", default-features = false,
                     features = ["anthropic", "openai", "gemini"] }
```

Three parts of that line are load-bearing:

- **`default-features = false`**, because the default feature set includes
  `axum`, which pins **axum 0.7** while this crate is on **0.8**. Enabling it
  compiles two axums or fails outright. The casualty is the crate's own Anthropic
  SSE emitter, which is why `gateway/stream.rs` writes raw SSE bytes over the
  framework-free `SseFrame` the crate emits instead.
- **no `bedrock`**, because its AWS SDK pins Rust **1.94.1** against this crate's
  declared **1.88** floor. That is also why `gateway_providers.type` accepts four
  provider types and not five.
- **a tag, not a commit SHA.** The design document predates the tag existing and
  says to pin a SHA. A tag can be moved where a SHA cannot, which is accepted
  here only because ferrox is our own repository and its README documents this
  tag as the reference point.

**Settled in [#453](https://github.com/shaharia-lab/agento/issues/453): the tag
stays and crates.io is declined for now.** `Cargo.lock` records the resolved
commit, so a moved tag changes what a *fresh* resolve picks and nothing that is
already checked out or already released. Publishing would buy an immutable
registry artifact at the cost of a release process in a repository we own and
are the only consumer of. Revisit if `ferrox-providers` gains a consumer outside
this app.

Two more things are copied rather than imported, and both are deliberate:
`is_retryable` / `should_failover` (`gateway/dispatch.rs`) live in ferrox's
*binary* crate, not in `ferrox-providers`; and there is **no `CorsLayer`**, which
ferrox's own server mounts permissively. That is right behind a network boundary
and catastrophic on loopback — it would let any page you have open spend your
provider credits and read the answer.

### Curling it in dev

The gateway needs a credential of its own, and the debug build's `api-token` file
is **not** it — that holds a `write` token, which the gateway refuses with a 403.
Mint an `llm` one first:

```bash
API=$(cat ~/.agento-desktop-dev/api-token)
LLM=$(curl -s -X POST -H "Authorization: Bearer $API" -H "Content-Type: application/json" \
  -d '{"name":"dev","scope":"llm","expires_in_days":1}' \
  http://127.0.0.1:8991/api/security/tokens | jq -r .token)

curl -s http://127.0.0.1:8880/v1/models -H "Authorization: Bearer $LLM" | jq
```

That last line is the cheapest live check that the listener is up and the
credential works; it answers with the configured **aliases**. `/healthz` on the
same port needs no credential at all and is the check for "is anything bound".

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

**Curling the API** works in dev, since the server is on a fixed port — but
`/api` requires a bearer token, so the header comes first:

```bash
curl -s -H "Authorization: Bearer $(cat ~/.agento-desktop-dev/api-token)" \
  http://127.0.0.1:8991/api/agents | jq
```

The token is a **JWT signed by the install's Ed25519 key** (#405). A debug build
writes a freshly minted one to `~/.agento-desktop-dev/api-token` (0600) on every
launch so `curl` can reach the API; a release build writes it nowhere — its API
is reachable from the app window and nothing else. Without the header the guard
answers **401** before the handler runs.

Add `-H "Content-Type: application/json"` for anything that is not a `GET`, or
the guard answers 415 instead.

Chrome on `:1420` needs no token of its own: Vite's proxy reads the same file and
adds the header when it forwards.

**A token for something that is not the app** — a script, a CI job, another
local service — comes from **Settings → Security**, where you choose `read` or
`write` and can revoke it again. It is shown once and stored nowhere, so copy it
then. Note what `write` means before issuing one: `POST /api/agents` can create
an agent with bypassed permissions and `POST /api/chats/{id}/messages` can run
it, so a `write` token is arbitrary command execution on the machine.

Anything holding the public key can **verify** an Agento token without asking
Agento — that is what `GET /.well-known/jwks.json` is for, and it needs no
credential:

```bash
curl -s http://127.0.0.1:8991/.well-known/jwks.json | jq
scripts/verify-jwks.py --token "$(cat ~/.agento-desktop-dev/api-token)"
```

The second line is an independent check (PyJWT over OpenSSL rather than the
`ring` the app signs with). Run it after changing anything about the token
format, the JWKS document or the signing key.

**Two 4xx to tell apart.** A **401** is "you have not proved who you are" —
missing, expired, revoked, or signed by a key this install no longer uses. A
**403** with `this token's scope does not permit this request` is "you have, and
this credential does not reach that"; retrying will not help. There are two ways
to earn it, and they have different fixes:

- a **`read`** token against a state-changing method, or against
  `/api/security/*` (which needs `write` whatever the method) — the fix is a
  `write` token or a different request;
- an **`llm`** token against *anything* under `/api`. That scope is the LLM
  gateway's data plane and is disjoint from `read`/`write` by design, so no
  `/api` route accepts it and a bigger `/api` token is not the fix — you want a
  token of the right kind for the surface you are calling. The reverse holds
  too: `read` and `write` tokens are refused by the gateway.

That last one is worth knowing before it bites in dev: the debug build's
`api-token` file holds a **`write`** token, so it will not work against the
gateway. [The LLM gateway](#the-llm-gateway) has the two commands that mint an
`llm` one and check the listener with it.

**Regenerating the key in Settings → Security invalidates every token at once**,
the dev file's included, which is exactly what it is for. The app window recovers
on its own; a shell holding an old token needs to re-read the file, and any tool
configured against the gateway needs a freshly minted one pasted in.

**A scratch instance, for anything you would not want to do to your own data**,
is a scratch `HOME` — **not** `AGENTO_DATA_DIR`, which a debug build ignores
(`paths.rs::data_dir` is `cfg`-split and the debug arm always answers
`~/.agento-desktop-dev`, so that an exported variable cannot make `npm run app`
share a database and a scheduler with an installed Agento):

```bash
HOME=/tmp/scratch-home ./target/debug/agento
```

That gets its own database, its own signing keypair and an empty `~/.claude` —
a genuinely cold install, which is the only way to test a first-run path.

There is also a project skill, `local-verify`, describing how to reproduce a bug
before fixing it and how to verify each hop separately: backend wire, browser
engine, real Tauri webview, then the UI flow. Use it for anything that "works in
Chrome but not in the app".

---

## Branches and pull requests

- Work happens on a feature branch. PRs target **`main`**.
- Releases are tagged `v*` from `main`. The update manifest keeps its own fixed
  tag, `desktop-latest`, which does **not** move — see [Releasing](releasing.md).

Before opening a PR, from the repository root:

```bash
npm run build
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

Releasing is documented separately in [Releasing](releasing.md).
