# The notification sender (#307)

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

`PUT /api/notifications/settings` and `POST /api/notifications/test` are
native, and with them `internal/notification/{template,smtp}.go` — the only
code in this shell that talks to a server we do not run. Four things about it
are load-bearing:

- **`encryption` does not mean what it says.** `tlsPolicyFromEncryption` hands
  go-mail a *TLSPolicy*, and every policy there is about **STARTTLS**. go-mail's
  implicit-TLS switch is `WithSSL()`, which Agento never calls — so `ssl_tls`
  means *mandatory STARTTLS*, not SMTPS on 465. Reproducing that is the parity
  bar; "fixing" it moves a working configuration to a port nothing answers on.
- **The parity bar is the rendered mail, not JSON.** Nothing downstream parses
  it, so a divergence has nothing to report it — the first sign would be a user
  saying the email looks different. `parity/notification_template_golden.json`
  is rendered by Go and asserted by both languages. It earned its keep
  immediately: `html/template`'s text escaper is **seven** entries (the usual
  five plus `+` and NUL), and this port had also escaped `=`, which lives in the
  *nospace* table and applies to unquoted attribute values rather than text.
  The template skeleton is Go's **output**, not its source, because
  `html/template` elides HTML comments and `emailTmpl` has six.
- **A failed send forwards; only success is answered.** Go's 400 carries
  go-mail's and the Go runtime's wording, none of it reproducible. Forwarding
  costs a second dial and is safe for one reason only: `send` reports success
  after the server has accepted the message, so an error means nothing was
  delivered. Nothing fallible may run after that point — the response bytes are
  encoded before the dial for exactly that reason.
- **The settings write touches one column**, deliberately. The inherited
  implementation saved all fourteen from an in-memory snapshot, which is how one
  notification save could revert the hidden-project list and the idle threshold
  written by an unrelated request. Writing one column is what makes that
  impossible.

**The subscriber is wired** (#275): `notifications::handle(db_path, event,
payload)` is called directly by the scheduler's executor when a task finishes.
One publisher, one subscriber, no event bus between two functions.
