# The Slack integration (#315)

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

Seven tools in one service group (`messaging`), over a workspace token. The
fourth of the six, and the first that is not shaped like the three before it —
two of its differences are things no earlier port had to deal with at all.

**The token can come from the `auth` column, and that widened a projection that
deliberately never selected it.** `resolveToken` (`slack/server.go`) switches on
`credentials.auth_mode`: `bot_token` reads the credentials blob, and `oauth`
reads `cfg.ParseOAuthToken()` — the **`auth` column**, decoded as an
`oauth2.Token`. Until #315, `registry.rs`'s `HOSTING_COLUMNS` collapsed `auth` to
a boolean in SQL precisely so a stored token could not exist in this process to
be echoed, and `native/integrations.rs` still never selects it at all. `HostingRow`
now carries it. What did **not** change is where it may go: that struct still
derives neither `Serialize` nor `Debug`, it is private to the module, and only a
`&str` leaves it. There is a third arm too, and it is the one a port drops: an
**unrecognized** `auth_mode` falls back to `bot_token` when that is non-empty, so
a row whose mode was never set still works. The `tokens` block in
`slack_vectors.json` pins all three arms, plus the empty-`access_token` case that
sends a bare `Bearer` header — and both languages observe the resolved token the
same way, by reading the `Authorization` header the fake received rather than
recording the token, which would put the thing under test into the fixture.

**Nothing model-supplied reaches the URL.** The base is a constant and every
method is a literal (`conversations.list`, `chat.postMessage`), so there is no
dot-segment guard and no `base_url::Base` here — the class of problem #312 and
#317 spent their reviews on does not arise. Model input goes in a form body or a
JSON body instead. The seam is back to GitHub's shape, though: `slackAPIBase` is
a package variable, so `slack/parity.go` exports `SetAPIBase` and the Rust side
gates a `RwLock` behind `#[cfg(test)]`. Confluence and Jira needed neither,
because an Atlassian site URL is per row.

Four more Slack-shaped surprises, all pinned:

- **`ok` decides, not the HTTP status.** `readSlackResponse` checks 429 and then
  ignores the status, so a **500 carrying `{"ok":true}` is a success** and a 200
  carrying `{"ok":false}` is a failure. Every sibling gates on the 2xx range, so
  this is the one a port gets backwards.
- **Two encodings.** Five tools send `url.Values.Encode()` as
  `application/x-www-form-urlencoded`; two send `json.Marshal` as
  `application/json; charset=utf-8` — with the charset, which nothing else in the
  tree sends.
- **Every clamp differs**: 1000/100 for the two listers, 100/20 for
  `read_messages` and for `search_messages`' count, and a floor of 1 with **no
  ceiling** for `page`. Read one by one rather than generalised from the first.
- **Rate limiting is its own sentence**, interpolating `Retry-After` verbatim —
  including when the header is absent, which lands mid-sentence as an empty
  string (`retry after  seconds`).

Five of the seven tools return Slack's body **unlabelled**; only the two senders
prefix it. Timeout 60s and cap 5 MiB, the largest of the six.

**An OAuth completion must reload the hosted server, and #318 is what makes
that an event rather than an inference.** The app binds the callback server,
writes the token to the `auth` column and calls `reload_after_auth` directly.
Without it, a Slack or Google integration authenticated by OAuth would first be
served at the next boot.

An earlier version could not do this: the callback server was elsewhere, so the
only part of the flow this process saw was the UI polling `GET
/api/integrations/{id}/auth/status`, and the reload was driven by noticing that
the stored credential had changed underneath it. `Trigger::AuthStatusPolled`,
`registry::reload_if_secrets_changed` and the fingerprint map it read are all
gone. `reload_after_auth` fires on the write itself.

It is **best-effort**: a user who closes the dialog before the flow completes is
still served only at the next boot. #318 owns the OAuth flow itself and is where
that stops being true.

## Socket Mode: the inbound worker (#567)

`slack/socket.rs` is one tokio task per Slack row that has migration 39's
`inbound_enabled` set **and** an `xapp-` token in its credentials blob. A
desktop app has no public URL, so an Events API webhook is not available to it
and this is the only way a Slack event arrives. What an `app_mention` *does* is
#568's and reaches the worker as `SocketOptions::handler`; until then the
default logs at `debug`.

- **Two handles per row, one generation.** `integrations::registry`'s `State`
  gained a `sockets` map beside `servers`, and `stop` / `put_if_current` move
  both inside one critical section. Two calls would let a `stop` landing between
  them keep one half, which for the socket half is a live connection holding the
  app token of a row the user has just changed. The worker is **not** a seventh
  entry in `start_for_type`'s starter table: it is a second handle on one of the
  six, and `start_socket_worker` is where the three conditions are read.
- **The stored-token check is not redundant with the 422 on `PUT
  /api/integrations/{id}/inbound`.** That refusal guards the moment the switch
  goes on; a later `PUT /api/integrations/{id}` can replace the blob with one
  that has no `app_token`, which is why `clears_inbound` exists — and a row
  stored before that clearing did still has to start cleanly rather than open a
  socket with an empty bearer.
- **The ack comes before everything.** Slack redelivers anything it does not see
  acknowledged within seconds, so `{"envelope_id": …}` is written to the socket
  before the dedup claim, before the handler and before any database touch. The
  handler then runs on `tokio::spawn` under **the trigger dispatcher's**
  semaphore, which is why `dispatcher::semaphore` is `pub(crate)` rather than
  this module opening a second bound of ten on the same `claude` subprocesses.
- **Acknowledged is not processed**, so dedup is separate: `claim_event` is
  `trigger::receiver::claim_update`'s shape exactly, `INSERT OR IGNORE` into
  `slack_processed_events` inside an immediate transaction with the row count
  deciding who won, plus the same best-effort 48-hour sweep. A failed claim is
  `false` — never run the agent against a database that could not record it.
- **A stream that ends without a close frame is a failure, not a polite
  reconnect.** Only an explicit `disconnect` envelope or a `Close` frame is
  `Requested`. Calling a dropped TCP connection polite would reset the
  consecutive-failure counter, so a flapping gateway would be retried every base
  wait forever, with `inbound_status` never reaching `error` and nothing telling
  the user anything is wrong.
- **A session that stayed up resets the counter even though it failed.** The
  counter is about a gateway that will not have us, not about a laptop lid:
  without this a socket that runs for hours and dies on a suspend adds one to a
  number that never comes back down, so an integration that has worked all week
  reads `error` on its fifth lifetime drop and reconnects only once a minute
  after. The bar is `max_backoff`, which is the one value that cannot produce a
  hot loop — an attempt that outlived the longest wait the schedule would ever
  impose costs at most one base wait to retry, whatever it does next.
- **A read with no deadline is how an outage becomes invisible.** A half-open
  connection — a suspended laptop, a rebinding NAT — delivers no FIN and no RST,
  so `next()` never completes and the worker parks with the row still reading
  `connected`. `idle_timeout` is what turns that into a reconnect. Slack's own
  pings are traffic, so a healthy connection never approaches it.
- **No database write is ever awaited on the task that reads the socket.**
  `db::open_read_write` carries a five-second `busy_timeout`, so one write meeting
  the session scanner's batch writer would leave the *next* envelope unread and
  unacknowledged for that whole window — past the seconds Slack waits before
  redelivering. The dedup claim lives inside `dispatch`'s `tokio::spawn`, and
  every status transition is *posted* to `status_writer`, a single consumer so
  the writes stay ordered: a `connected` overtaking the `reconnecting` that
  followed it would leave the row lying about a socket that is down.
- **Clearing the status is the registry's, and writing it is the worker's.** A
  worker cannot tell a stop from a replacement — a `reload` is a stop followed
  immediately by a start — and a compare-and-swap cannot break that tie, because
  both workers write the identical `connected`. So `start_one` and `reload` clear
  the row when they decline to start a worker, ordered before the start rather
  than racing it, and `start_all` clears every Slack row once at boot, before any
  worker exists, which is what corrects a `connected` a crash left behind. The
  worker's own half is two checks, and it needs both: `stopped` — set in
  `SocketWorker::drop` before the shutdown oneshot — is the cheap early exit, and
  an **epoch re-read inside `status_lock`** is what holds when the drop lands
  between the check and the write. A `db::blocking` write can sit for the whole
  five-second `busy_timeout`, which is ample time for a replacement to write
  `connected` underneath it, and the row would then be stuck on the old worker's
  last value because a socket that connects and stays connected has no next
  transition. `status_lock` is global and held across the write, so the two
  writes are ordered whichever way they arrive and the stale one is a no-op.
- **The epoch is granted by the registry inside `put_if_current`, never taken by
  the worker**, and that placement is the whole of its correctness. A worker is
  *built* before the decision — `start_for_type` is `async` and can be slow — and
  may then be refused on the generation. A worker that claimed its own epoch at
  spawn could therefore hold a **later** one than the worker that went on to be
  accepted; `status_writer` would discard every status the accepted worker posted
  for the rest of the process, freezing the row on whatever it last held.
  Granting it in the one critical section that decides acceptance makes epoch
  order and acceptance order the same order by construction. `NOT_ACCEPTED`
  matches nothing, so a refused worker is silent. `Registry::stop` and the
  no-worker arm of `put_if_current` retire the epoch under the same lock and hand
  it to `clear_status`, which refuses to write if something has been accepted
  since. Only the boot clear skips all of it, because no worker exists yet.
  `a_refused_socket_never_takes_the_epoch_from_the_accepted_one` is the guard.
- **`tests/slack_socket.rs` drives workers with no registry, so it grants the
  epoch itself** (`accepted_worker`), and **every test uses its own integration
  id** — the epoch map is keyed by id, which is unique in a shipped build and
  emphatically not across the tests in one binary.
- **The backoff is re-anchored on the wall clock**, `schedule::runtime`'s rule
  for gocron's reason: `tokio::time::sleep` measures process time, and on a
  suspended machine that is not elapsed wall-clock time, so one long sleep holds
  the socket down for the length of a shut lid *after* the lid opens.
  `sleep_until` sleeps in one-second slices and re-reads `Utc::now()`. The
  jitter is additive and bounded at a quarter of the step, deliberately: a
  jitter wide enough to reorder two adjacent steps would make "the second
  attempt waits longer than the first" true only most of the time.
- **A fresh `apps.connections.open` on every attempt** — the `wss://` URL it
  returns is single-use. `ok` decides there too, not the HTTP status, the same
  way it does for the seven tools.
- **`error` is a report, not a stop.** After `failure_threshold` consecutive
  failures the status reads `error` and the worker keeps retrying, so an expired
  token the user then fixes reconnects without a restart. A refusal from
  `apps.connections.open` **never** clears the stored credential: that is
  `token_validate::clear_auth`'s decision, on a route a person asked for.
- **The status write touches two columns and no more.** `inbound_status` and
  `inbound_error`, never `updated_at` and never the switch — #566 left that
  contract, and a worker reconnecting hourly would otherwise keep moving the
  record's timestamp for something the user did not do.
- **`SocketOptions` is a struct rather than a `#[cfg(test)]` override**, because
  `tests/slack_socket.rs` is a separate binary and a `cfg(test)` seam in the
  library is invisible to it. `SocketOptions::default` reads
  `client::api_base()`, so an in-crate `registry` test still drives the whole
  start path through the existing seam.
