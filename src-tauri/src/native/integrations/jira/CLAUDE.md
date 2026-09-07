# The Jira integration (#316)

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

Nine tools in one service group (`project_management`), over the **same**
`config.AtlassianCredentials` Confluence uses. Read it beside
`native/integrations/confluence/`: the two are twins, and every difference
between them is #277's, deliberately preserved.

| | Confluence | Jira |
|---|---|---|
| create-time validator | HTTPS only, keeps the raw value | http **or** https, trims trailing `/`, **re-marshals** |
| inside `Start` | `ValidateSiteURL` again | **nothing at all** |
| client timeout | 30s | 15s |
| failure sentence | names nothing | names the method and the path |
| `parity` seam | `StartAtSiteURL` needed | **none needed** |

Two of those rows change the shape of the port:

- **`jira.Start` validates nothing, so a bad base is answered per call rather
  than by refusing to host.** Go hosts the server and advertises all nine tools
  whatever the stored site URL says. Refusing to host would change the
  *advertised tool set*, which is what every agent's stored `capabilities.mcp`
  allowlist depends on — so `jira::client::Client` holds `Option<Base>` and
  answers Go's own transport sentence when it is `None`. Confluence refuses at
  `Start` because **Go refuses there too**. Same helper, opposite answers,
  because the two Go packages differ. `jira_vectors.json`'s `site_urls` block
  pins it from both ends: the tool set is unchanged and the call fails.
- **No Go-side seam.** `Start` reads the site URL out of the credentials, so the
  generator puts the `httptest.Server`'s URL there and calls the shipped
  `jira.Start`. There is no `internal/integrations/jira/parity.go`, and that
  absence is a consequence rather than an oversight.

`native/integrations/base_url.rs` is #317's site-URL work extracted for the
second caller — `Base::new` (the four base checks) and `Base::resolve` (the
per-call dot-segment guard). Read its header before porting anything else whose
API base comes out of the row; #313–#315 do not need it, because theirs are
constants.

**The base's path prefix comes from whether it contributed any path *text*, not
from what `url` rendered.** `url::Url::parse` gives both `https://x` and
`https://x/` a `path()` of `/`, while Go concatenates raw text — so
`https://x/` + `/rest/api/3/project` goes on the wire as `//rest/api/3/project`,
empty first segment intact. Deriving the prefix from the rendered path made such
a base refuse every call. It is reachable on Jira and not on Confluence, and the
asymmetry is the same one twice: `validate_site_url` trims trailing slashes
before `Base::new` sees them, while `jira.Start` trims nothing — and `Update`
validates nothing on either, so a user retyping the URL in the edit form can
store one.

Four things in `tools.go` that look like mistakes, are Go's behaviour, and are
pinned:

- **`list_projects` binds `*struct{}`** — the only tool in the six integrations
  with no fields, so its schema is `{"type":"object","additionalProperties":false}`
  with no `properties` key at all.
- **`create_issue` runs `url.PathEscape` over the project key *inside the JSON
  body***, and over nothing else, so a key holding a space is sent as `MY%20PROJ`.
- **`update_issue` and `transition_issue` discard the response** and build their
  result text from the arguments, so a 200 with a surprising body still reads as
  success.
- **`/rest/api/3/issue/` carries its trailing slash in the constant** while
  `/rest/api/3/project` does not — same wire, two spellings.

One divergence is pinned rather than reconciled, in the `site_urls` block: for a
site URL `url.Parse` rejects, Go fails inside `http.NewRequestWithContext` and
answers `creating request: parse "…": …` with `net/url`'s vocabulary and the
stored site URL interpolated. This port refuses before building anything and
answers the transport sentence — which is also the narrower of the two, since
Go's puts the site URL into text the model reads and a `tool_result` stores.
