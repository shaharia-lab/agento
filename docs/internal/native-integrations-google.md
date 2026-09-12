# The Google integration (#313)

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

`internal/integrations/google/` — eight tools across **three** service groups
(`calendar`, `gmail`, `drive`) over an OAuth2 token that refreshes itself. Last
of the six, and the only one that is not a port of hand-rolled HTTP.

**Read this before changing anything here.** The other five build their requests
with `http.NewRequest` and `json.Marshal`, so the port reproduces bytes visible in
`tools.go`. Google calls the **generated** client libraries (`calendar/v3`,
`gmail/v1`, `drive/v3`) over an `oauth2` transport, and what those put on the wire
is in neither this repository nor the port. Every URL, query parameter, body field
and sentence was therefore *recorded* off the real libraries against a fake
endpoint — `parity/google_vectors.json` is not a confirmation of the port,
it is the **only written specification** of what the third party's code generator
emits. Three things the port had wrong were found by running those vectors for the
first time, and none was visible from either side's source:

1. **Path parameters use `googleapi.Expand`, not `url.PathEscape`.** The generated
   clients expand RFC 6570 templates, which percent-encode everything outside the
   unreserved set. `PathEscape` leaves the sub-delims alone, so a Gmail message id
   containing `&` came out as `…%3Fd&e` — an `&` that starts a query parameter.
   `client::expand_path_segment`; this is `googleapi`'s rule, not `net/url`'s,
   which is why it is not in `gourl`.
2. **Every JSON body carries a trailing newline**, because `googleapi` writes it
   with `json.NewEncoder(…).Encode`. Both the plain `application/json` POSTs and
   the metadata part of the `multipart/related` upload. One byte, on the wire,
   applied once in `google::marshal`.
3. **`oauth2.RetrieveError` has two forms** and picks between them on whether the
   body named an error code, *not* on the status. A Google refresh refusal takes
   the one the port did not have:
   `oauth2: "invalid_grant" "Token has been expired or revoked."`.

Review then found more of the same shape, each confirmed by adding a vector and
reading what real Go answered — which is the workflow to repeat here:

4. **`googleapi.Error.Error()` is not a four-case table.** The one-error form is
   conditional on the sub-error's message *equalling* the top-level one (the
   normal validation-error shape has them differ, and takes `More details`);
   `error.details` — the modern `google.rpc.ErrorInfo` envelope a real quota error
   carries — was dropped entirely; the raw-form test is "no errors **and** no
   message", not "no code and no message"; and a body beginning `[` is unwrapped
   by `errorReplyFromBody`. It is now a transcription of the Go function rather
   than a table of its outputs, because the table *was* the bug: every case it did
   not list, it got wrong silently.
5. **`omitempty` reaches past `description`.** `calendar.Event.Summary`,
   `EventDateTime.DateTime` and `drive.File.Name` all carry it, so an empty
   `start` sends an object holding nothing but its time zone.
6. **`tokenRefresher` refuses before opening a socket** when the stored token has
   no refresh token — the state a re-consent without `prompt=consent` leaves a row
   in. Reproducing it is not only about the sentence: without it the port POSTs
   the user's `client_secret` on a request Go never sends.
7. **Gmail's `parts` is the one array where a `null` element is graceful** —
   `extractBody` nil-checks it — so it is `Vec<Option<GoStruct<_>>>` where its
   siblings are not, and `googleapi.Error.Errors` needs the same because it is a
   Go **value** slice.
8. **An empty 2xx body is a zero value, not a decode failure**
   (`DecodeResponse` returns untouched on a 204).

A fourth was found by trying to *vector* it: **a response body of exactly `null`
panics every one of the eight Go tools.** The generated clients decode into a
`**T`, `json.Unmarshal` of `null` into a pointer-to-pointer nils the pointer, and
the handler then reads a field off it. That is a 200 with a two-word body, which a
proxy or a misconfigured gateway produces. It joins the two nil-dereferences the
handlers own — `msg.Payload.Headers` in `read_email`/`search_email`,
`ev.Start.DateTime` in `view_events`. The port returns the zero value in all
three; a panic cannot be recorded in a vector and is not a behavior worth
reproducing, so the `null` case is deliberately **absent** from the vectors.

Google-specific behaviour the other five never reached:

- **`gourl::Values` stopped being single-valued for this.** Gmail's
  `MetadataHeaders("Subject","From","Date")` encodes as three `metadataHeaders=`
  pairs in insertion order under one sorted key, so `Values` is now a multimap
  with `set` and `add`.
- **`search_email` makes N+1 requests from one tool call**, and a failed fetch is
  **skipped** rather than surfaced — while the count in the sentence is
  `len(list.Messages)`, the *listed* total. A partial failure therefore produces a
  sentence whose number does not match its body. Go's, and pinned.
- **`create_file`'s media type is sniffed from the content**, not taken from the
  tool's `mime_type` argument, which reaches the metadata JSON alone.
  `drive::detect_content_type` is `net/http.DetectContentType` ported **whole**,
  including the 512-byte bound. It was first written as "the subset a JSON string
  can reach", on the reasoning that a PNG's `0x89` would be UTF-8 encoded before
  it arrived — sound for `0x89`, wrong for most of the table, since `BM`, `%PDF-`,
  `%!PS-Adobe-`, `GIF87a`/`GIF89a`, `ID3`, `OTTO`, `ttcf`, `wOFF`, `wOF2`,
  `RIFF…WAVE` and `FORM…AIFF` are pure ASCII. `BM` is the one that bites: content
  beginning "BMI calculator results…" uploads as `image/bmp`. **The lesson
  generalizes past this function** — an argument that some subset of a third
  party's table is unreachable has to be right about every entry, and the cheaper
  move is to transcribe the table.
- **Drive's upload path is an absolute reference**, so it replaces the base's
  whole path: `/upload/drive/v3/files`, not something under `/drive/v3/`.
- **Every clamp differs.** `view_events` and `list_files` cap at 100, `search_email`
  at 50, and all three fall back to 10 rather than to the cap.
- **`q` is conditional for Drive and unconditional for Gmail** — an empty Drive
  query sends no key, an empty Gmail search still sends `q=`.
- **`download_file` is the only call that is not `alt=json`**, and the only result
  that is the response body verbatim rather than a formatted summary.
- **The three bases are not one host.** Gmail moved to `gmail.googleapis.com` and
  carries `gmail/v1/` in its *relative* paths, where Calendar and Drive carry the
  version in the base.

**The token source is #318's, built here first.** #318 says of it: "Token refresh
is shared with the Google MCP server. One implementation, not two."
`client::TokenSource` is that implementation — 10-second `expiryDelta`, a zero
expiry that never expires, credentials in the **body** (`AuthStyleInParams`, not a
`Basic` header), an absent `refresh_token` keeping the old one, and nothing
persisted. Adopt it in #318; do not write a second.

Three things are pinned as **divergence** rather than matched, all recorded in the
vectors so they cannot drift silently: `X-Goog-Api-Client` and `User-Agent` (they
embed the Go toolchain and library versions — no Rust build can emit `gl-go/…`),
the random `multipart/related` boundary (the *parts* are compared instead), and
Go's `*url.Error` wrapper around a transport or refresh failure (it embeds the
resolver's own message).

`internal/integrations/google/parity.go` exports `SetEndpoints`, a **wider** seam
than the other integrations' `SetAPIBase` because both the API base and the OAuth2
token endpoint have to be redirectable. It hands credentials to whatever host it
names — the token URL receives the `client_secret` and refresh token, the durable
ones — so it is test-only, and the Rust equivalent is behind `#[cfg(test)]`.

**The flip landed as its own change**, after the port. That split was worth it
for the reason it was made: the flip is where the risk in this series has lived —
#315's hosting of Slack silently broke `completeOAuth`'s reload, and Google is the
*other* provider `startProviderCallback` supports. With both hosted, the
poll-driven net covered every OAuth integration in the product. **#318 then made
the net unnecessary** by moving the flow itself, so the reload is an event again
rather than an inference.

`start_google` is the only starter needing **both** secret columns for different
things: `credentials` carries the OAuth2 client pair, `auth` carries the token.
Slack reads `auth` too but only for an access token; here the whole `oauth2.Token`
matters, because `expiry` decides whether the first tool call refreshes. That is
also why `google_oauth_token` **refuses a row whose `expiry` does not parse**
rather than skipping the field as Slack's does — a corrupt expiry means not
hosted, not hosted-with-a-token-that-never-refreshes.

The accept/reject boundary is measured, not read: the `starting` section of
`google_vectors.json` records what `google.Start` does with thirty-one
credential and auth shapes, and the Rust replay calls `google_start_inputs` —
the function the starter itself calls — so the **order** of the three checks is
pinned too, not just their sentences.

**Two findings there are worth carrying forward.** The first is a trap Go's own
writer sets: `omitempty` does not suppress a struct, so `SetOAuthToken` emits
`"expiry":"0001-01-01T00:00:00Z"` for a token with no expiry rather than omitting
the key — and `Token.Valid()` treats that zero `time.Time` as **never expiring**.
Read as an ordinary instant it is permanently expired, the exact inverse, so
`google_oauth_token` maps it to `None`. The vector that was meant to cover this
originally tested the *absent* key, a spelling Go's writer can never produce.

The second is that **`gotime::parse_rfc3339` is now the one Go-RFC3339 parser**,
shared with `native/schedule`'s `run_at` (#275). `chrono::parse_from_rfc3339`
disagrees with `time.Parse` in **five** ways, in both directions: it accepts a
lowercase `t`/`z` and a leap second that Go refuses, and refuses a comma decimal
separator, a one-digit hour and an offset hour past 23 that Go accepts. The last
three are Go being *laxer*, which a stricter port turns into refusing input Go
takes — a schedule that will not build, or an integration that will not host.
#275 found three of the five and wrote `parse_from_rfc3339` plus three guards;
#313 was about to add a second copy with two. Guarding a convenient library has
been wrong twice for one reason — **the guard list has to be right about every
disagreement** — so the shared version parses the grammar and delegates nothing
but the calendar arithmetic. `GoTime::parse` is deliberately left on chrono: it
reads `effective_from`, which only this application writes, normalized.

`whatsapp` is now the stand-in in all three "unported type" tests, and it stays
there: it is dropped rather than deferred.
