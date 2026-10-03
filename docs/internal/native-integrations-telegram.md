# The Telegram integration (#314)

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

Eleven tools in one service group (`messaging`), over a bot token — the largest
tool set after GitHub's twenty. The inbound half is the long-poll worker in
`telegram/polling.rs` (#676), described under *Inbound: long polling* below.
The webhook (`POST /webhooks/telegram/{id}`, #319) is still mounted at the
root, deliberately outside `guards.rs`, but nothing in the UI registers one any
more.

**It reaches two of the three reflector divergences `claude/schema_vectors.rs`
left standing**, both of which it recorded as unreachable because "nothing in the
six integrations" used the shape:

- **`create_poll` takes a `[]string`** — the first slice parameter anywhere in
  the six. `jsonschema-go` renders every slice as `["null","array"]`, because a
  Go nil slice marshals as `null` and must be accepted back; `schemars` renders a
  bare `array`. The map's guidance was "a port that needs one must add the null
  itself", and `messaging::go_string_slice` is that, with
  `null_is_zero_value` on the field so a `null` decodes to an empty `Vec` the way
  it decodes to a nil slice. That second half is not cosmetic: `len(nil)` is 0, so
  a null reaches Go's own "2-10 options" refusal rather than a decode error, and
  the vectors pin `got 0`.
- **`send_location` takes `float64`** — the first floats. The schema costs
  nothing (`normalize_go_schema` drops the `format` `schemars` adds) but the
  *encoding* would have: `encoding/json` spells a float its own way and
  `gojson::go_float` already reproduces it. `1e+21` and `1e-7` are in the request
  vectors because that is the only place the spelling is observable.

**The bot token is in the URL path.** `apiURL` is
`fmt.Sprintf("%s/bot%s/%s", …)`, so the credential is in the request line rather
than a header. Two consequences: an error string must not interpolate a transport
cause (a `reqwest::Error`'s `Display` carries the URL), which Go's wording already
avoids by naming only the method; and `Client::endpoint` needs the same
compare-what-`url`-produced guard the other ports have, because a token could
contain a separator. The accepted half of that guard is the interesting one — a
token cannot *begin* a dot segment, since `apiURL` glues it to the literal `bot`,
so `../evil` becomes the segment `bot..` and is sent by both.

**The envelope decides and the status never does** — not even a 429, which
Slack's client does check. A 500 carrying `{"ok":true}` is a success. All three
`encoding/json` decode rules are applied up front here rather than after review
(`null_is_zero_value`, `GoStruct`, `Option<GoStruct<_>>`), plus a fourth that is
Telegram's own: `result` is a `json.RawMessage`, so an **absent** one renders as
the empty string and an explicit `null` as the four bytes `null` — and both reach
the model in a result sentence, so `Option<Box<RawValue>>` cannot be left to
serde, which folds a JSON null into `None`.

Cap 10 MiB and timeout 60 seconds, the largest of the six on both counts.

## Inbound: long polling (#676)

One `getUpdates` loop per Telegram row with `enabled`, `inbound_enabled` and a
non-empty `bot_token`. It replaced the webhook as the receive path because a
desktop app has no public URL. It is `slack/socket.rs`'s twin and shares its
status writer, so read that module's header first; `telegram/polling.rs`'s own
header states each rule below with its reason.

- **`deleteWebhook` runs before the first poll**, with
  `drop_pending_updates: false`, and the row's `webhook_secret`,
  `webhook_status` and `webhook_error` are cleared to `''`, `'inactive'`, `''`.
  Telegram answers `getUpdates` with a 409 while a webhook is set. A failure of
  either half is a failed attempt and is retried.
- **The payload is `{"allowed_updates":["message"],"offset":N,"timeout":T}`**,
  keys sorted, through `gojson::to_vec_marshal`. `T` is 30, and 0 for the first
  poll after a start or a failure, so the status is known in one round trip.
- **The offset is in memory**, `last update_id + 1`. A restart gets a
  redelivery, and `trigger::receiver::claim_update` (inside the dispatcher's
  spawn) drops what already ran. An element that does not decode is skipped and
  the offset still passes it; a batch that cannot move the offset is a failure.
- **Status is `inbound_status` / `inbound_error`**, on Slack's four words.
  `reconnecting` after a failed attempt, `error` from the fifth consecutive
  one, `connected` again on the next answered poll. The wait is 1 s doubling to
  60 s on the wall clock. A request has its own deadline, the poll timeout plus
  20 s, below the client's 60 s.
- **A `409 Conflict` is rewritten** by `describe_failure` into a sentence
  naming the cause (a second poller, or a webhook set elsewhere), with
  Telegram's text after it. Every other failure is `client::Client::call`'s own
  sentence, which never holds the token.
- **The worker does not need `authenticated`.** The webhook read `enabled`
  alone, and migration 50 turns `inbound_enabled` on for every Telegram row
  whose `webhook_status` was `active`. So `registry::start_worker_only` hosts a
  poll worker on a row that has no tool server, through
  `Registry::put_worker_if_current` — a row that is not authenticated, and
  equally one whose tool server failed to start. Slack never takes that path.
- **The registry owns the worker** as `InboundWorker::Telegram`, in the same
  map, epoch and generation bookkeeping as Slack's socket. Dropping the handle
  cancels the poll in flight. The boot clear covers `type IN ('slack',
  'telegram')`.
- **`POST …/webhook/register` answers 409 while `inbound_enabled` is on**,
  before it calls Telegram. The three webhook routes, the root route and the
  `public_url` setting are otherwise unchanged and unreachable from the UI;
  removing them is a follow-up.
- **Two processes polling one token take turns getting 409.** An installed
  Agento and `npm run app` with the same bot do this.

Pinned by `telegram/polling_tests.rs` — library tests, because the Telegram API
base is a `cfg(test)` seam a `tests/` binary cannot reach — and by
`registry.rs`'s `a_telegram_poll_worker_follows_the_switch_and_the_token_but_not_the_auth`
and `a_row_with_no_server_keeps_a_telegram_poll_worker_and_clears_the_rest`,
`integrations.rs`'s `the_inbound_switch_follows_a_telegram_rows_bot_token` and
`migrate.rs`'s `migration_50_turns_inbound_on_for_an_active_telegram_webhook`.

## Task delivery (#639)

A scheduled task with a `telegram` destination sends its output here after every
run; `schedule/delivery.rs` calls `telegram/delivery.rs::deliver_chat` once per
chat id. The rules, each stated in that module's `//!` header:

- **The send path is the dispatcher's reply**, `trigger/telegram_api.rs::send_reply(token, chat, 0, text)`
  — not the `send_message` MCP tool, which takes a model-supplied payload and
  does not split. So the 4096-**byte** split and the sorted payload bytes are
  that module's, unchanged. A header message (`<task> — Completed in 3m 12s`)
  goes first, then the output; plain text, no `parse_mode`.
- **The token comes from `trigger/receiver.rs::telegram_delivery_token`**, which
  shares its row read with the webhook's `enabled_bot_token` but names each
  refusal (deleted, not Telegram, disabled, no bot token) so the delivery row
  records `skipped` with a reason. It checks `enabled` only, as the webhook does
  — not `authenticated`; the form's warning follows the same rule.
- **Errors are mapped from the envelope's description**, never the status:
  `Bad Request: chat not found`, `Forbidden…` and `Unauthorized` become advice
  naming the chat id, with Telegram's words after it; anything else, including
  `Too Many Requests`, passes through and is not retried in v1.
- **Numeric chat ids only.** `send_reply` takes an `i64`; a channel's
  `@username` is refused at write time (a `-100…` id works).

Pinned by `telegram/delivery.rs`'s tests (against a fake through
`client::set_api_base`), `schedule/delivery.rs`'s two Telegram tests, and
`tasks.rs`'s `every_telegram_destination_rule_is_a_422`.
