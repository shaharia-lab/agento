# The integration registry, credentials, and the OAuth flow

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

The six integration ports each carry their own notes one directory down
(`github/`, `confluence/`, `jira/`, `slack/`, `telegram/`, `google/`). Read
`src-tauri/src/claude/CLAUDE.md` (*Hosting a tool*) before any of them, and
`github/` before porting or changing anything: it settles *how to port an
integration*, and each later one records only what it adds.

## The integration registry (#311)

`native/integrations/registry.rs` is the MCP-server lifecycle: `start_all` at
boot, `reload(id)` on every `PUT /api/integrations/{id}`, `stop(id)` on every
`DELETE`, over a process-wide `OnceLock` keyed by integration id — the shape
`native::scan::state` and `native::chat::live` use. The handle *is* the cancel:
dropping an `InProcessMcpServer` stops its listener, so there is no second map
of cancel functions.

**A stale listener is a security problem, not a staleness one**, and it is why
the write and the reload had to land together. A hosted server closes over the
credential it was started with, so one that never hears `Reload`/`Stop` goes on
answering `tools/call` with a token the user just revoked, for the rest of the
process's life. A write that persisted the row without reloading would do
exactly that.

**`HOSTED_TYPES` is the single list of what this process hosts.** `hosts_type`
reads it and the starter dispatch must cover it
(`a_hosted_type_always_has_a_starter`). It covers all six — github, confluence,
jira, slack, telegram, google — and it will never cover `whatsapp`, which is
dropped rather than deferred (#273).

**A WhatsApp row is data and survives; its integration does not run.** Its
starter opened a live whatsmeow WebSocket and registered the client in a
global, which is not a thing this build has. `GET /{id}/whatsapp/status`,
`POST /{id}/whatsapp/reconnect` and QR pairing have no implementation here. The
row lists and reads normally, and `unavailableCopy` in
`views/integrations/catalog.ts` is what explains it to the user.

**`reload` is unconditional.** Stop then start, no diff: there is a window with
no server and the port changes on every save. That window is not exotic — a
model mid-`create_issue` when the user hits Save is the ordinary case, which is
why shutdown is graceful (*Hosting a tool*, `src-tauri/src/claude/CLAUDE.md`) and why
`a_tool_call_in_flight_survives_the_handles_drop` exists.

**No lock is held across the stop, the async row read and the start**, so a
`DELETE` landing in that window would otherwise leave a *bound port holding a
credential* for a row that no longer exists. `Registry::stop` therefore bumps a
per-id generation, and `put_if_current` refuses a handle whose generation moved
since the caller read the row; the refused handle is dropped, which fires its
shutdown oneshot. `start_all` snapshots generations *before* it lists the table,
because it is spawned at boot while the proxy is already answering. Two
concurrent `reload`s were already safe — `HashMap::insert` drops the displaced
handle.

**One `Reload` trigger is not a write.** `POST /api/integrations/{id}/auth/validate`
writes a credential and must reload the hosted server, so
`registry::reload_after_auth` fires on a 2xx, spawned rather than awaited.
`integrations::reload_after_forward` is the route predicate and the place to
read why the list is one entry long.

**A *rejected* credential check is a state change; an *unreachable* one is
not** (#521). That route now has a second write, on the failure path:
`native/integrations/check.rs`'s `CheckKind` — `Rejected` when the provider
answered and refused the credential (a 401 or 403, or a refusal carried in a
200 envelope: Slack's `ok:false`, Telegram's), `Unreachable` for everything
else. On `Rejected`, `token_validate::clear_auth` empties the `auth` column and
reloads, so `authenticated` goes false, `reload` hits its `!is_startable()`
early return with the server already stopped, and `available-tools` drops the
row's tools. On `Unreachable`, nothing moves at all.

Four things about it are load-bearing:

- **Both halves are the fix, and each is the other's inverse.** Without the
  clear, replacing a working token with a rejected one left the badge reading
  `Connected` about the *previous* authorisation while the hosted server went on
  answering `tools/call` with the credential just refused — three correct
  behaviours composing into a lie (`PUT` preserves a non-empty `auth` in SQL so
  the token is never read into the process; `authenticated` is computed from
  that column alone; the `PUT` reloads before any check can run). With a **flat**
  clear, a provider that is briefly unreachable disconnects a working
  integration — the same dishonesty pointing the other way. **A test that only
  exercises the 401 passes against a flat clear**; the unreachable case needs its
  own, over a row that *was* authorised, because a clear against a NULL `auth`
  is unobservable.
- **`Unreachable` is the default for anything unrecognised.** Misclassifying a
  refusal leaves the old bug standing for one shape; misclassifying a transport
  failure invents a new one.
- **Each validator decides its own kind**, because only it can see the status or
  the envelope. `CheckFailure::from_status` is the one place 401 and 403 are
  grouped — `gateway_api/catalog.rs`'s reasoning verbatim, since some providers
  report an exhausted quota as 403.
- **A refusal only clears when it refused the credential `auth` attests, and
  there is exactly one row where it does not.** A Slack integration in `oauth`
  mode keeps its OAuth2 *token* in `auth` while `credentials` holds the client
  pair — and `validateSlackTokenAuth` runs anyway, against
  `credentials.bot_token`, which is **empty** in that mode, so Slack answers
  `not_authed` about a credential that was never the authorisation. Clearing
  there would destroy a working grant and force the whole flow again, on a check
  that tested nothing. `token_validate::clears_on_refusal` reads `auth_mode` off
  the **stored** blob through `integrations::auth_mode_of`'s three-literal
  allowlist, so an absent or unrecognised mode still clears — matching
  `resolveToken`'s own fallback to `bot_token`. `IntegrationsView` states the
  same invariant beside its two auth buttons ("neither writes anything, so this
  is a confusing answer rather than data loss"); keep both true together.
- **Nothing else moves.** The 400 body is byte-identical on both classes — three
  keys, `error` · `valid` · `validated`, and `REPORTS_VALIDATED` is untouched —
  and `credentials` keeps the bytes the `PUT` stored, so re-running the check
  after fixing the token restores the authorisation. A failed `clear_auth` is
  logged and still answers the 400, and a row with nothing to clear is not
  written at all. The UI tells the two classes apart by **re-reading the row**,
  never by matching on the error string.

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

**Reading a credential is this module's job and nobody else's.** The rule in
`native/integrations.rs` — a stored secret never exists in this process to be
echoed, `auth` collapses to a boolean in SQL — still holds there. What has
changed is that the `credentials` column is no longer simply unmentioned: two
things are now derived from it, **both entirely inside SQLite**, and neither is
a widening.

`has_credentials` (#515) is a boolean — whether the column holds anything at
all — which is what lets the edit form say "leaving this alone keeps the stored
one" instead of demanding a re-typed token on every save. `auth_mode` (#513) is
the discriminator the Integrations editor reopens on, rather than guessing it
from the provider's first mode; `auth_mode_sql` compares the extracted value
against a fixed list of three literals (`AUTH_MODES`), so a byte the module did
not already know cannot cross the boundary whatever a `PUT` stored under that
key — and `update` validates nothing, so that is not hypothetical. **Copy that
shape, not a bare `json_extract`, if a third derivation is ever needed**: the
rule these keep is that what leaves SQLite is a value from a set this side
already enumerated.

The real exception — the one place a credential is *read* — is `registry.rs`:
its own `HOSTING_COLUMNS` projection into a `HostingRow` that derives
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

**Two things about the `PUT` that read as bugs and are Go's behaviour**, both
pinned by `parity_writes::the_integration_id_write_answers_match_go` against a
live server: it runs **no** credential validation (`validateIntegrationCredentials`
is `Create`'s alone, so an empty name, an empty type and a `{}` blob are all
200s); and an omitted `services` is stored and returned as `null`, not `{}`,
because `Update` skips the `make(...)` that `Create` does.

**There was a third, and #515 fixed it rather than reproducing it.** A request
omitting `credentials` used to **wipe** them, because the store's upsert
overwrote the column wholesale — and `GET` scrubs the column, so the
read-then-write an edit form performs destroyed the secret. It is now
three-valued, `gateway::config::update_provider`'s contract: **absent leaves
the column out of the `SET` list** so the stored blob survives byte for byte,
and **any present value replaces it**, so `{}` and `null` are an explicit
clear. Two `UPDATE` statements, not one with a `CASE` — the difference is
*which columns are assigned*, and a `CASE` would still bind the blob's
parameter slot on the path that must not have one. The absent arm never reads
the credential into this process, so the module-header rule is intact.

Two consequences worth carrying: the read gained **`has_credentials`**, a
SQL-computed boolean (`''`, `null` and `{}` all count as absent, trimmed of
ASCII whitespace) sorting between `enabled` and `id` on a response whose key
order is the contract; and the reload stays **outside** both arms, because it
is the *replace* path that must reload — a revoked token otherwise keeps
answering `tools/call` for the life of the process.

**Do not open the live database with a bare `rusqlite::Connection::open`.** It
opens `READWRITE|CREATE`, and against a WAL database another process holds, that
was observed to reset the log — a row created two API calls earlier vanished from
the other process's view immediately afterwards. Always go through
`native::db::open_read_only` or `open_read_write`, which set the pragmas and the
busy timeout.


## The OAuth flow (#318)

`POST /api/integrations/{id}/auth/start` and `GET …/auth/status` share
`oauthFlows`, an in-memory map — so the process that starts a flow must be the
one that finishes it. `native/integrations/oauth/flow.rs` binds the loopback
callback server, exchanges the code, writes the token and reloads the hosted
server, all in one place.

**The reload is an event, not an inference.** `reload_after_auth` fires on the
token write itself. An earlier design could not see the token land and instead
watched the UI *poll* `auth/status`, reloading when the stored credential
stopped matching what was running; `Trigger::AuthStatusPolled`,
`registry::reload_if_secrets_changed` and the fingerprint map it read are gone.

**The URL is pinned by a vector.** The redirect port comes from a fresh
`FreePort()`, so the URL is not reproducible request to request;
`parity/oauth_vectors.json` records the expected shape for a fixed port.
Recording rather than transcribing was not ceremony — `oauth2.ApprovalForce`
emits **`prompt=consent`**, not the `approval_prompt=force` its name suggests.
The vectors also pin that the Google
scope union is *ordered* (calendar → gmail → drive) rather than sorted, that
`scope` is **absent** rather than empty when no service is enabled, and that the
encoding is `url.Values.Encode` where a space is `+`.

**The exchange has neither a vector nor a diff**, being a call to the provider,
so its request shape is pinned against a fake token endpoint that records what
it was sent. That is what caught the difference that matters: Google declares
`AuthStyleInParams` and puts its credentials in the body, while Slack declares
no style at all, so `oauth2` tries **HTTP Basic first** and only retries in the
body. "Params first" works against a server that accepts both and fails against
one that does not.

The stored token's encoding is fixed, because the MCP servers read it back.
Note **`expiry` is always present**: a struct is not omitted when empty, so a
token that never expires stores `0001-01-01T00:00:00Z` rather than dropping the
key — and `Token.Valid()` reads that zero instant as *never expiring*, the exact
inverse of how an ordinary timestamp would read. `google_oauth_token` maps it to
`None` for that reason.

`WriteError::Internal` was added for this route and is not a house style: it is
**500 with a body produced here**. A failed OAuth flow must not answer from the
stored token, because `authenticated: false` would be a plausible lie about a
flow that actually errored.


## Secrets at rest

- **Secrets are stored in plaintext** in `integrations.credentials` / `.auth`.
  Protection is perimeter-only (loopback bind + directory perms, plus the
  `/api` bearer token since #400 — which stops another *process* reading them
  back out through the API, but does nothing for the bytes at rest). #405 adds a
  second plaintext secret of its own, `api-signing-key.pk8`, on the same terms
  and deliberately: it is `0600`, it has no reader outside `native::security`,
  and regenerating it is one click. It is a *durable* secret where #400's died
  per launch, which is the accepted price of offline verification. Do not
  introduce a UI that echoes them back; the API scrubs them and the UI must not
  reintroduce them.
