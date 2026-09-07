# The Claude Agent SDK port, and hosting a tool

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

## Layout

```
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
```

## The Claude Agent SDK

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

## Hosting a tool — there is exactly one way (#282)

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
- **A hand-written `list` handler owns `ttlMs` and `cacheScope`** (#555).
  Protocol revision `2026-07-28` (SEP-2549) makes both **required** on the
  result of `tools/list`, `prompts/list`, `resources/list`, `resources/read` and
  `resources/templates/list`; the Claude Code CLI validates against that schema
  and **discards the whole tool list** when either is missing, while still
  reporting the server `"status":"connected"` — so every integration goes dark
  and nothing anywhere says why. `rmcp` advertises the revision in
  `ProtocolVersion::KNOWN_VERSIONS`, so we have promised the contract, but keeps
  both fields `Option` for older peers and `with_all_items` sets neither; the
  macro handlers it fixed for this (rust-sdk #1120) are unreachable because the
  `macros` feature is off. `ToolServer::list_tools` therefore sets `ttlMs: 0`
  (the spec's "immediately stale" — the tool set is per turn and per integration
  row) and `cacheScope: private` (nothing is shareable across authorization
  contexts; every listener has its own bearer token), pinned on the wire by
  `mcp::tests::a_tool_list_carries_the_cache_hints_the_cli_requires`. Agento
  serves no prompts or resources; the first one it serves needs the same two
  fields. The revision's other requirements are `rmcp`'s and already met —
  `server/discover`, `resultType` on every result (stripped for pre-2026 peers),
  the `Mcp-Method`/`Mcp-Name` request headers, `subscriptions/listen` in place of
  the `GET` stream, and no `Mcp-Session-Id` (we are stateless already).
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
  bound is real. A handler added by #313 with no client timeout would fail
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
  `parity/jsonschema_reflect_vectors.json` reflects one reference struct
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
  already written to `chat_messages`. `parity/local_tools_vectors.json`
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
