# The Telegram integration (#314)

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

Eleven tools in one service group (`messaging`), over a bot token — the largest
tool set after GitHub's twenty, and the **outbound half only**. The inbound
webhook (`POST /webhooks/telegram/{id}`) and `internal/trigger/`'s dispatcher are
#319 and are untouched; they matter here only because the webhook route is
mounted at the root and deliberately outside `guards.rs`, which nothing in this
module is on.

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
