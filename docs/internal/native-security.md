# What /api accepts as proof of identity — the token, the scopes, the guards

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

## The bearer token

**And every `/api` request — reads included — needs
`Authorization: Bearer <token>`** (#400, #405). `api.ts` attaches it at its two
header sites, so nothing in a view changes. Note the scope difference from the
rule above: the content-type guard is an allowlist over the state-changing four,
while the credential covers **every method**, because a `GET` is what returns
chat transcripts and agent system prompts.

**Since #405 that token is an EdDSA (Ed25519) JWT signed by a per-install
keypair**, not the opaque per-launch string #400 minted. The keypair is created
on first run beside the database (`api-signing-key.pk8`, `0600`) and reused on
every launch; the app's own session is a self-signed JWT with `sub:
desktop-webview` and no database row, minted fresh by `host_info` on every
invocation and delivered over Tauri IPC. See `native/security/` for the whole
thing and the reasoning behind it.

Four consequences worth knowing before debugging anything:

- **`curl` against `:8991` needs the header.** A debug build writes a freshly
  minted token to `<data dir>/api-token` (0600) precisely so it can:
  `curl -H "Authorization: Bearer $(cat ~/.agento-desktop-dev/api-token)" …`.
  A release build writes it nowhere.
- **Opening the release URL in an ordinary browser no longer works.** There is no
  IPC there, so no token; `api.ts` turns the resulting 401 into one honest
  message rather than a wall of per-view errors. Chrome on `:1420` is unaffected
  — Vite's proxy adds the header server-side.
- **A 403 is not a 401.** 401 means the credential did not verify — absent,
  malformed, expired, revoked, signed by a superseded key. 403 with `this
  token's scope does not permit this request` means it did, and the token is
  `read`-scoped against a state-changing method (or against `/api/security/*`,
  which needs `write` whatever the method), **or it is `llm`-scoped and on
  `/api` at all**. Retrying will not help.
- **There are three scopes, and the third is not on the ladder** (#423).
  `read` < `write` is a hierarchy; **`llm` is disjoint from both**. It is the LLM
  gateway's data-plane credential: `write` does **not** cover it, `llm` covers
  nothing on `/api`, and `required_scope` never returns it — those two halves
  together are what make it disjoint, so neither can be relaxed alone. The
  reasoning is that a gateway token is pasted into tool configs in plaintext
  (`OPENAI_API_KEY`, `ANTHROPIC_AUTH_TOKEN`), where `write` would be arbitrary
  command execution and `read` would be every chat transcript; and conversely a
  credential for spending provider credits has no business reading chat history.
  `Scope::covers` is deliberately **enumerated rather than wildcarded** — it was
  `(Write, _) | (Read, Read)`, and adding a variant under that wildcard would
  have made `write` a gateway credential silently, with every test still green.
  Do not reintroduce a wildcard there.
  One dev consequence: the debug build's `api-token` file holds a **`write`**
  token, so it will *not* work against the gateway — mint an `llm` one via
  `POST /api/security/tokens` first.
- **`api.ts` retries a 401 exactly once**, re-invoking `host_info` for a fresh
  token first. That is what makes a keypair regenerate recoverable without a
  restart, and the bound is structural rather than a counter — see `withAuth`,
  and do not turn it into a loop.


## The guards

`src-tauri/src/guards.rs` runs in `dispatch`, **before** routing is decided, so
a request is refused identically whichever endpoint would have answered it. It
applies three checks (#329, #400, #405).

The first and third are *browser* defences and neither inconveniences a local
process at all: `curl` sends a loopback `Host` and sets its own `Content-Type`.
The middle one is why a bearer token exists — without it the whole API, which
can create a `bypass`-permission agent and run it, was open to anything on the
machine. That also settled a standing asymmetry: every in-process MCP server has
required a token since #282, while the far more powerful API server did not.
**Do not read "Agento ships without authentication on purpose" anywhere as
current**; #246 recorded that and #400 revised it.

**#405 replaced the credential and left the check where it was.** The token is
now an EdDSA JWT signed by a per-install keypair rather than an opaque
per-launch string, so `token_rejection` is a signature-and-claims verification
plus a scope comparison instead of a constant-time compare. That is the shape
#400 deliberately left room for — its own note said the check should be "does
this request carry an accepted credential — one `verify(&str) -> bool` — not a
hardcoded compare against a single string". What it adds:

- **A scope, and one definition of it.** `security::required_scope` maps
  `is_state_changing` onto `read`/`write`, so the guard has a single
  read-versus-write rule rather than a second table. Its one exception is
  `/api/security/*`, which needs `write` whatever the method, because those
  reads *are* the credential system — a `read` token must not enumerate every
  credential on the machine. A per-route permission model over ~90 endpoints is
  explicitly deferred.
- **Insufficient scope is a 403**, sharing a status with the `Host` rejection
  and not its body.
- **A third and fourth process-wide static, both in `native::security`** — the
  keypair (a `RwLock`, because regenerate replaces it while the listener
  serves) and the revoked-`jti` set (in memory, and authoritative rather than a
  cache, because this process is the only writer of `api_tokens`). `guards.rs`
  itself now holds no credential at all.
- **A new unguarded route.** `GET /.well-known/jwks.json` publishes the public
  key with no credential — requiring one to fetch the thing credentials are
  verified against is a bootstrap problem with no answer. It is outside `/api`,
  so the scoping rule below already exempts it, and `proxy::is_api_path` names
  it so it reaches the registry instead of the embedded frontend assets.

Four things about it are load-bearing:

- **It is scoped to `/api`.** `POST /webhooks/telegram/{id}` is mounted at the
  root, arrives from Telegram's servers with a foreign `Host` and is
  authenticated by its own secret token; a global guard would break inbound
  triggers. `/health`, `/metrics` and the SPA are likewise untouched.
- **It runs before `gourl::route_path`.** A guard that needs no route in order
  to say no is the simpler property to keep true.
- **A body-less request is not exempt.** Several state-changing endpoints take
  no body, and a cross-origin `POST` with neither body nor `Content-Type` is
  *itself* a simple request. `api.ts` sends the header on every request, so
  requiring it always costs nothing.
- **The allowed `Host` set is `localhost` and the loopback literals**, and
  nothing else. The proxy binds `127.0.0.1` unconditionally and has no public
  URL, so widening it would admit names the app cannot be reached at.

A rejection logs as `Served::Rejected` (`rejected`), because the check runs
before routing is decided.

**Byte-identical JSON is the bar.** Field names, key order, escaping and float
spelling are all part of the contract, and only a byte comparison catches all
four. The goldens in `parity/` are what enforces it.
