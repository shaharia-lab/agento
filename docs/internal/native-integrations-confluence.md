# The Confluence integration (#317)

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

The second of the six and the smallest: six tools in one service group
(`content`), over an Atlassian site URL, account email and API token. Read it
after `native/integrations/github/` — that one settles how to port an
integration, this one is what an integration adds when its API is not GitHub's.

**Five surfaces are pinned, not four.** `parity/confluence_vectors.json`
carries the four #312 established — hosted tool set, schema, request, result
text — plus `ValidateSiteURL` per input, because that is the one piece of
`Start` that is a *decision* rather than plumbing: it is what stops an `http://`
site URL carrying the user's API token in a `Basic` header over plaintext.
Regenerate with
`go test ./desktop/parity/ -run TestConfluenceVectors -update-confluence-vectors`.

Four things that will recur in #313–#315:

- **The test seam is a parameter, not a static.** GitHub has one API root, so
  `githubAPIBase` is a package variable and the Rust side gates a `RwLock` behind
  `#[cfg(test)]`. A Confluence base comes out of the row, so it is a field on
  `Client` and a parity run simply constructs one against loopback — nothing
  test-only ships at all, which is the narrower answer. Go still needs a seam,
  because `Start` refuses a plaintext URL before it builds anything:
  `internal/integrations/confluence/parity.go`'s `StartAtSiteURL` is `Start` with
  that one line removed, and it is exported for the same reason `SetAPIBase` is.
- **The dot-segment guard compares only the half a tool built.** `client::absolute`
  is `github::client::absolute`'s reasoning verbatim — `url::Url::parse` applies
  WHATWG dot-segment removal and Go's `net/http` does not, so
  `get_page(page_id: "..")` would reach `/wiki/api/v2/` (the space listing) on a
  request carrying the token. What differs is what the expected target is
  compared *against*. GitHub's base is a fixed, already-encoded string, so there
  the whole target is the `path` argument. A site URL is per row and
  **user-typed**, so it need not be encoded at all:
  `https://intranet.example.com/my atlassian` is one Go accepts and sends as
  `/my%20atlassian/…` through `EscapedPath`, and `url` encodes it identically —
  so comparing against the raw concatenation would refuse every call against a
  site URL that works. The base is therefore parsed on its own and its *rendered*
  path is the expected prefix; only the tool's suffix is compared against the
  bytes it built, which is sound because that half is fully `gourl`-encoded.
- **The base needs its own validation, and the authority half must be an
  allowlist rather than a comparison.** This is where getting it wrong sends the
  user's credentials to somebody else, so it is worth stating the shape of the
  argument. Comparing the host `url` resolved against the host `net/url` would
  have resolved catches only a disagreement about where the authority *ends*;
  where the two read the same substring and *interpret* it differently, the
  comparison is a tautology — it is the same parser on the same bytes. There are
  at least three interpretation gaps, each of which grafts the site onto an
  attacker's domain from a string that reads as the legitimate one:
  `evil.com\@acme.atlassian.net` (Go: `invalid userinfo`; `url`: host
  `evil.com`), `acme.atlassian.net%2Eevil.com` (Go: `invalid URL escape`; `url`:
  `acme.atlassian.net.evil.com`), and a NO-BREAK SPACE between two labels (Go
  keeps it literally; `url` IDNA-maps it and joins them). `parseHost` is itself
  an allowlist — `integration_credentials::split_url` says so, having enumerated
  every ASCII byte through it — so `validate_site_url` uses one too, and a
  narrower one, because that module may forward what it is unsure of and this
  one may not: ASCII letters, digits, `.`, `-`, `_`, optional numeric port,
  applied to the **whole** authority so `@` and `\` are out. Nothing in that set
  is transformed by either parser, so agreement is by construction. It refuses
  four things Go serves — userinfo, an IPv6 literal, a non-ASCII host (which
  Go's own IDNA-blind resolver cannot dial either) and a percent escape — each a
  logged non-start.
- **The path half compares against `EscapedPath()`, not against
  `escape(Path, encodePath)`.** The second is only the first's *fallback*: Go
  prefers the raw text whenever it is `validEncoded`, whose allowlist admits
  `! $ & ' ( ) * + , ; = : @ [ ] %` regardless of `shouldEscape`. So `/a!b` and
  `/a%2Fb` are sent verbatim and `url` renders them identically — comparing
  against `escape` alone refuses a base that works. `gourl::valid_encoded_path`
  is that rule; #316 needs it for Jira. The true refusals it keeps are `\` (Go
  `%5C`, `url` `/`), `^`, `|` and the dot segments. Two more shapes have no
  sound comparison at all: a base `url` cannot parse, and one carrying its own
  `?` or `#`. Every refusal Go would have served is pinned as a `rust_error`
  divergence. **Any integration whose API base comes out of the row inherits all
  of this** — #316 is the first, through `native/integrations/base_url.rs`; #313–#315
  have constant bases and need only the tool-suffix guard.
- **`SetBasicAuth` is `reqwest`'s `basic_auth`.** Both are
  `Basic base64(user + ":" + pass)` with standard (not URL-safe) alphabet. The
  vectors pin the encoded header rather than trusting that, because nothing in a
  response reveals it.
- **`net/url`'s parse failures are classified, not quoted.** `ValidateSiteURL`
  asks `url.Parse` two questions (scheme, host) and Go answers a *third* case —
  the parse itself failing — with `net/url`'s vocabulary, `%q`-quoted over the
  caller's input. That wording is not reproducible past printable ASCII, and it
  is a **log line**: `Start`'s error is logged by the registry and never reaches
  a response or the model. So the port reproduces the two refusals exactly,
  reproduces the *classification* of the two parse failures a stored site URL can
  reach (a control character; a scheme-less URL whose first path segment holds a
  colon) under its own wording, and `confluence_vectors.json` carries both as
  `rust_error` divergences. `go_scheme` and `go_host` are `net/url`'s own
  `getScheme` and authority split, written out rather than delegated to
  `url::Url::parse` — which refuses strings `url.Parse` accepts and would answer
  "invalid" where Go answers "not HTTPS".

Two smaller notes. The nested request bodies (`create_page`, `update_page`) go
through `gojson::to_vec_marshal` over `serde_json::json!`, which sorts at *every*
level and HTML-escapes — and unlike #312's, this fires on every real call, since
a page body is XHTML. And the client timeout is **30 seconds**, not GitHub's 15;
it is per API and it is what bounds a graceful shutdown.
