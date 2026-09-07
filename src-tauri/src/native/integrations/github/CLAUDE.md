# The GitHub integration (#312)

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

The first of the six, and the largest: twenty tools over five service groups,
token auth only. It is the file to read beside `native/tools/` before porting
#313 — that one settles *how to host a tool*, this one settles *how to port
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

**That empty-union rule is kept, and #501 settled it rather than changing it.**
It is ported, identical in all six integrations and asserted by each of their
own suites, so an explicitly empty `tools` list still means *host everything*.
What #501 closed is the two ways that read as the opposite of what the user
asked for:

- **`filter_config_tools` dropped a service that named no tools**, so an
  integration row storing `{"enabled": true}` — what `POST /api/integrations`
  accepts, and what every row written before the per-service lists carries —
  hosted **zero** tools while `--allowedTools` still named them. A service with
  no list of its own is now given the request **narrowed to its own tools**,
  through the integration's `SERVICE_TOOLS` table.

  **Handing it the whole request instead is unsound, and it is the obvious
  thing to write.** The tempting argument — a name from another group is
  rejected by the service gate anyway — is *nearly* true and fails exactly
  where it matters: `build_allowed_set` unions **every** enabled service, so a
  name injected by a listless service satisfies the `allowed` half for a
  **sibling**, whose own gate passes because it is enabled too. A row of
  `{"gmail":{"enabled":true},"drive":{"enabled":true,"tools":["list_files"]}}`
  would then host `create_file` — a write tool the user's own Drive list
  excludes — on an integration whose every tool result carries attacker-authored
  third-party content. `SERVICE_TOOLS` is the `push` table as data, and each
  integration's `the_service_table_matches_what_is_registered` derives the truth
  from its own registration function (one service enabled at a time) rather than
  transcribing it, so the two cannot drift.
- **The Integrations UI could store `{"enabled": true, "tools": []}`** by
  unchecking a service's last tool, which hosts every tool of that service —
  the exact inverse of "Only the tools you leave on are exposed to agents".
  `IntegrationsView.tsx` now turns the **service** off when its last tool goes,
  so the copy is true of everything the app can store. The API can still store
  that shape and it still hosts everything; refusing it would be a wire change
  on a byte-exact endpoint — so the editor also has to *render* such a row
  honestly, and that rule reads backwards twice over. A listless enabled
  service shows every tool ticked **only when no sibling names one**: "host
  everything" is a property of `build_allowed_set`'s union over the *whole
  integration*, not of one group being empty, so a listless `gmail` beside a
  `drive: ["list_files"]` hosts **nothing** and must show nothing. Getting that
  half wrong is worse than a misdisplay — the union is what makes unchecking
  one box in the ticked-by-default group *grant* the others. Pinned from the
  backend side by `an_enabled_service_with_an_empty_list_still_hosts_everything`,
  which carries the mixed row for this reason.

**And a turn now says what it hosted.** `chat/runner.rs::report_hosted_tools`
logs one `info` line per started MCP server naming the tools it registered, plus
a `warn` per requested tool it did not — read off the handle
(`InProcessMcpServer::tool_names`), not off what was asked for, because the two
disagreeing *is* the defect. It stays a warning: `start_local_tools`' rule is
that a missing tool is the kinder failure than a refused run. Before it, `claude:
mcp "google-7": serving on http://…` was the whole record and a server hosting
nothing printed it byte for byte like a healthy one.

`src-tauri/tests/mcp_e2e_probe.rs` is the `#[ignore]`d suite that would have
caught it: it drives the real CLI through **`runner::build_options`' own
output** — not a hand-built command line, which is what makes it able to see an
option-assembly defect at all — and asserts the tool was both *listed* (it
appears in the CLI's `init` event) and *called*. `claude_mcp_live.rs` remains
its sibling and stops at the handshake on purpose.

**Four surfaces are pinned, not one.** `parity/github_vectors.json` is
taken from the real Go server over its real MCP transport, against a fake GitHub
that **records the request each tool built**: the hosted tool set, each
description and input schema, the request (method, encoded target, headers,
body) and the result text of every success and every failure path. The request
half is what pins the things no response reveals — `url.PathEscape` per segment,
`url.Values.Encode`'s sorted keys and `+`-for-space, the per-page clamp, and
`json.Marshal`'s sorted keys and HTML escaping in every request body. Regenerate
with
`go test ./desktop/parity/ -run TestGitHubVectors -update-github-vectors`.

Five things it brought that #313 will want:

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
- **`reqwest` sends no `User-Agent` where `net/http` sends one, and GitHub
  answers 403 without it** (#514). Go's transport sets `Go-http-client/1.1`
  unasked; `reqwest` sets nothing unless asked, and the port never asked — a
  byte lost silently, because no response, stored value or parity vector records
  it (`google_vectors.json` says outright that `User-Agent` is deliberately not
  recorded). GitHub *requires* it, so **all twenty tools and `validate_pat`
  403'd**, and the message names the credential rather than the header: the user
  saw `validation error for "credentials.personal_access_token"` and regenerated
  a PAT that was never the problem. **Every client comes from
  `native::http::client_builder()`**, which pre-sets
  `Agento/<CARGO_PKG_VERSION> (+https://myagento.app)` and touches nothing else
  — the timeouts still differ per integration and GitHub keeps its second
  no-redirect client. The version is `env!("CARGO_PKG_VERSION")` and **not**
  `native::version::VERSION`, which answers `dev` in every unstamped build.
  `no_client_is_built_outside_this_module` reads the crate's own sources so the
  eighth client cannot omit it the way the first seven did; it found one the
  issue's own table had missed, the OAuth token exchange in
  `integrations/oauth/flow.rs`.
- **The test seam is `#[cfg(test)]`, and should stay that way in #313.**
  `githubAPIBase` had to be *exported* on the Go side (`parity.go`) because
  `parity` is a different package; both Rust callers are in-crate, so
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
