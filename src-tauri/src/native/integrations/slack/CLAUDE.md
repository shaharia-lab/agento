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
