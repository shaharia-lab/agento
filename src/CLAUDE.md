# The frontend — layout and UI conventions

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

## Layout

```
src/
  lib/
    api.ts       fetch wrapper + qs() + streamChatMessage() (POST-based SSE)
    types.ts     TypeScript mirrors of the Go JSON — snake_case, verbatim
    hooks.ts     useResource / useDebounced / usePoll / describeError
    format.ts    compactNumber, usd, duration, relativeTime, toneFor, …
    stats.ts     cross-view counters for sidebar + status bar
    nav.ts       sidebar sections, view ids, titles — and the cross-view
                 hand-off (#485): `NavTarget`, `NavProvider` and `useNavigate`.
                 **Two mechanisms, and which one to use is decided by where the
                 caller is rendered.** A view `App.tsx` renders directly takes
                 `onNavigate={navigate}` as a prop (the three gateway views
                 and `TasksView`, #542); the context is for a caller several
                 levels down, where the prop would have to be threaded through
                 every parent — the Sessions inspector and `SessionDetail`.
                 `NavTarget` is a view id plus
                 *one optional row id*, and it must stay that: it is a hand-off,
                 not a router, and query state, filters and scroll positions do
                 not belong in it. `App` clears the target on any navigation
                 that carries none, or a consumed chat id would be re-applied on
                 every later visit to that section. **There are three
                 destinations now, and the rule is unchanged**: `chatId` (#485),
                 `sessionId` (#536) and `jobId` (#542), one optional id each. A
                 further destination adds one field here and nothing else — the
                 *number* of fields is not the rule, one-id-per-destination is.
                 **Consuming one is keyed on the nonce, never on the loaded
                 list**, and where the destination's list is a *page* its
                 "always keep something selected" effect has to exempt the
                 handed-off id, which is legitimately absent from it (Sessions
                 and Jobs; `GET /api/chats` has no limit, so Chats guards with a
                 one-shot ref instead)
    icons.tsx    16px / 1.5-stroke icon set
    tauri.ts     window + menu bridge; degrades to a plain browser tab
    clipboard.ts copyText, with the execCommand fallback WebKitGTK needs
    newChatPrefs.ts  what the New Chat bar was last set to (localStorage)
    inspectorPrefs.ts  which inspector groups are open (#538) — keyed by a
                 stable slug, never the rendered title (two of them carry a
                 count), with the ids and the shipped defaults pinned through
                 typeAssert.ts
    logs.ts      the log commands, and the line parser (target before level)
    snippet.ts   the U+0001/U+0002 highlight sentinels, mirrored once from
                 native/search/mod.rs, and snippetParts() (#438). They are
                 **markers, not markup**: a snippet carries the transcript's own
                 bytes, so every run is handed to JSX as a text child and
                 nothing here builds an HTML string
    typeAssert.ts  Eq / Expect — the compile-time value pin, lifted out of
                 views/gateway/snippets.ts by #438 for its second consumer.
                 **This is the repo's only regression guard for a value**, since
                 there is no TypeScript test harness: give the value a literal
                 type and assert it exactly, and respelling it fails `tsc`.
                 `Eq`, never `extends` (`"a" extends string` pins nothing), and
                 export the alias or `noUnusedLocals` deletes the guard
  components/    TitleBar, Sidebar, StatusBar, CommandPalette, ui.tsx,
                 DirField (native picker + the /api/fs fallback), CopyButton,
                 TokenReveal (the show-once minted token, #427 — shared by
                 SecurityPane and the gateway Overview)
    charts.tsx   the inline-SVG chart primitives — AreaChart, BarChart,
                 RateChart, Heatmap, CardEmpty — lifted out of views/analytics/
                 by #428 when the gateway's Usage view needed the same ones.
                 Pure presentation over `{label, value, hint}[]`: it imports
                 nothing from views/ and no Claude type, and it carries its own
                 stylesheet (styles/charts.css) so a consumer outside the
                 analytics section does not have to import that section's sheet.
                 The `.a-` class prefix is history, not scope
    SaveBar.tsx  the one action strip at the foot of a form (#519) — the six
                 savebar views' shared submit/partner pair, its verbs taken
                 from lib/formVerbs.ts and not overridable. Lifted out of
                 IntegrationsView when AgentsView turned out to have forked it
                 into `.agents-savebar`; carries styles/savebar.css itself, the
                 charts.tsx shape. See *Conventions* for what it does and does
                 not take
  views/         one file per section
    sessions/SessionInspector.tsx  the inspector's metadata groups (#538) —
                 `{ session }` and no handlers, the five collapsible groups and
                 the localStorage write-through; `sessions/sessionMetrics.ts`
                 beside it is the totals and the mode tables the table shares
    sessions/SessionLink.tsx   a session rendered as a control, wherever one is
                 *named* (#536) — left-click hands off to the Sessions section
                 through `NavTarget.sessionId`, right-click opens the row menu.
                 `sessionMenuItems` is the **single** definition of those five
                 entries: `SessionsView`'s own rows build their menu from it
                 too, so a second hand-written array cannot drift. What an entry
                 *does* stays with the caller — the list patches its loaded page
                 and reloads its facets, which a link elsewhere has neither of.
                 The favourite is the one item whose label is a function of the
                 row, so an id-only caller hydrates it lazily on right-click
                 through the **list** (`?q=<id>`, `add_search`'s LIKE half
                 covers `session_id`) and never through
                 `GET /claude-sessions/{id}`, which reads the whole transcript
                 back to learn one boolean; while it is unknown the item is
                 disabled and labelled neutrally, because "unknown" is not
                 "not a favourite". `findSessionById` is that read, and the
                 hand-off in `SessionsView` resolves through it too — the by-id
                 route is only the fallback for a session with no list row,
                 because `SessionDetail` fetches the transcript itself on mount
                 and taking it first reads every message twice. A caller must
                 **not** pass `decoded_path` as `projectPath`: analytics ranks
                 on it and the sessions list keys on `project_path` literally,
                 so "Copy project path" would copy a string nothing filters
                 on. Carries `styles/sessionlink.css` itself, the
                 `components/charts.tsx` shape, since its consumers are in
                 sections that do not import `styles/sessions.css`.
                 **A caller that only has an id must resolve the row before
                 offering the control** (#542, the Jobs inspector): a
                 `job_history` row stores a session id whether or not that
                 session still exists, so handing the id straight over offers a
                 button that lands on Sessions with nothing to open. Resolve
                 through `findSessionById` **and fall back to `GET
                 /claude-sessions/{id}` on a miss** — `SessionsView`'s own
                 hand-off order, and the *list* is not the authority: it reads
                 `claude_session_cache`, which a scan only refreshes once it is
                 an hour old (`scan.rs`'s `CACHE_TTL`), while the by-id route
                 re-reads the transcript off disk. Concluding "absent" from the
                 list alone withholds the control for exactly the sessions a run
                 has *just* produced. Only that route's **404** is an absence; a
                 failed lookup is its own state. And **`job_history.chat_session_id`
                 is a `chat_sessions.id`, not a transcript id** — the executor
                 mints a chat row per run and passes no `custom_session_id`, so
                 the CLI's own id lands on `chat_sessions.sdk_session_id` and
                 nowhere else. A run's Claude session is reached *through* the
                 chat; naming the job's column directly resolves to nothing, for
                 every run
    settings/LogsPane.tsx      Settings → Logs: tail, follow, filter, save a copy
    settings/SecurityPane.tsx  Settings → Security: the public key, and issuing
                 and revoking scoped API tokens (#405)
    gateway/     the LLM Gateway section (#427) — its own sidebar section,
                 sharing nothing with Claude analytics or stats.ts
      OverviewView.tsx  status, the mint-once `llm` token, and the env snippets
      snippets.ts       the two base URLs and the type-level pin on them
                        (through lib/typeAssert.ts): OpenAI is `/v1`, Anthropic
                        is `/anthropic` with no `/v1`
      UsageView.tsx     the gateway's own dashboard (#428) over
                        GET /api/gateway/usage — and the two places a total is
                        labelled a *floor* rather than reported as a fact
      ProvidersView.tsx / ModelsView.tsx / SettingsView.tsx (which owns the
                        `usage_retention_days` control, #428)
  styles/        tokens → base → shell → controls → views (+ per-view files)

```

## Conventions

**Desktop, not web.** The UI deliberately diverges from the Agento web app:
three resizable panes per section, 14px type, 28px rows, hairline borders,
status bar, focus-aware selection (accent when the window is focused, grey
when not), ⌘K palette, no browser affordances. Reuse the existing CSS classes;
new CSS goes in a per-view file imported by that view.

**Text selection is a denylist over chrome, not an allowlist over content**
(#469). `base.css` declares `user-select: text` on `body` and `none` on one
named list — the titlebar and every drag region, the sidebar, the toolbar, the
status bar, source-list and table rows, table headers, `button` and `select` —
so a view added later is selectable without anyone remembering a class. It was
the other way round until #469, and the allowlist drifted exactly as one does:
the whole `views/gateway/` section, `views/analytics/` and the session detail
shipped with no opt-in at all, so a user could not copy a session id or an
error string. **Do not re-introduce a `.selectable` class or a per-view
`user-select: text`** — the denylist is the whole rule and it lives in
`base.css`; a second spelling elsewhere is how it comes apart. Rows stay in the
denylist deliberately: a table row's double-click *opens* the session, and a
drag across it means "select this row", so selectable cells would leave a word
highlighted behind the view they opened. `::selection` is focus-aware over the
existing `--bg-select*` tokens, matching `.window--focused`.

**A form's actions are a fixed grammar, and the `+` is not part of it** (#516).
Four rules, and each of them was broken by exactly one view before it was
written down here:

- **The primary verb follows existence**: `Create` while the record does not
  exist yet, `Save` once it does — with the in-flight label following it
  (`Creating…` / `Saving…`). Not `Create agent`, not `Save` for something that
  was never stored.
- **Its partner follows the same split**: `Discard` throws away a record that
  was never stored, `Revert` restores one that was. Two words because they undo
  two different things — a single `Cancel` claims to undo a save, which is why
  the gateway's spelling of it went rather than spreading.
- **A `+` icon marks a list-level *New X* / *Add X*** — an action that opens a
  blank thing (`New task`, `New rule`, `New profile`, `Add fallback`). **Never a
  submit.** `+ Create` on the Integrations connect screen is what this issue was
  reported for: it reads as "add another one" rather than "save this form".
- **Placement is one strip per view, used by both states.** Integrations was the
  only view whose primary action *moved* — toolbar while creating, savebar while
  editing. Which strip a view uses is its own choice (Tasks is the toolbar,
  everything else is the `.savebar` at the foot of the form); moving between
  them inside one view is not.

**`src/components/SaveBar.tsx` is the grammar as code, and since #519 there is
exactly one of it.** The rules above used to be enforced by this paragraph
alone: `IntegrationsView.tsx` owned a local `SaveBar`, described as the
component to copy from, and nothing forced a view to import it — so `AgentsView`
had grown `.agents-savebar`, a second strip with its own class family, its own
message element, its own error variant and `btn--lg` buttons. **The class was
shared and the JSX was copied, and the JSX is what drifted**, which is why the
*component* is the shared thing now and all six savebar views import it.

Three properties of it worth keeping:

- **The button size is `btn`**, decided once. `btn--lg` was Agents-only and
  appeared in no other savebar; it survives everywhere else (empty states,
  About), just not on a save strip.
- **The message slot takes an optional icon and an error tone**, which was the
  only capability the fork had and the shared one lacked. Both are opt-in, so a
  savebar passing neither emits exactly the markup it did before — that is what
  made the merge provably a no-op for the five pre-existing strips (the emitted
  CSS diff is three `.agents-savebar*` rules removed, two `.savebar__text--*`
  added, and `.savebar`/`.savebar__text` byte-identical).
- **`extra` is one slot for one caller** — the gateway provider form's "Save
  anyway", which exists because a base serving no model list can never pass the
  credential check. Every other savebar is exactly two buttons; do not reach for
  `extra` to avoid thinking about a third.

`.savebar*` now lives in `styles/savebar.css`, which `SaveBar.tsx` imports
itself — the `components/charts.tsx` shape. It used to be declared in
`styles/integrations.css` **and** `styles/settings.css` and reached the gateway
views only because `App.tsx` imports those two statically, so a section that
stopped being statically imported would have had unstyled savebars with nothing
to say so.

**`InspGroup` collapsing is opt-in, and it has to stay opt-in** (#538). Eight
views render `InspGroup` — Sessions, Analytics, Chats, Integrations, Tasks,
Agents, Jobs and the gateway Overview — so a `collapsible` that defaulted to on
would silently change every inspector in the app, which is `SaveBar`'s icon and
error tone one level down. A caller passing nothing takes an early return and
emits the `<div className="insp-group__title">` it always did; only a caller
passing `collapsible` gets the `<button …  aria-expanded>` and the chevron.
Proved the way the `charts.tsx` and `SaveBar.tsx` moves were: build both sides
and diff the emitted CSS, as a sorted set **and** as an ordered selector list —
the change is exactly three added rules (`.insp-group__title--toggle`, its
`:hover`, and `.insp-group__title--closed`), declared nowhere else, with every
pre-existing rule byte-identical and in place.

Three properties of it worth keeping:

- **It is controlled, not self-stateful.** The caller owns `open` and persists
  it. A group that kept its own state would need a storage key threaded in
  anyway, and two groups would then disagree about where the state is kept.
- **The header is a real `button`**, so Tab reaches it and Enter/Space toggle it
  with no keydown handler here, and `base.css` already resets a button's font,
  colour, background, border and outline and puts it in the selection denylist
  (#469) — which is why the CSS is a five-line box reset and **not** a
  `cursor: pointer` or a `:focus-visible` rule. This shell draws an arrow over
  its own controls, and the global focus ring already covers a button.
- **The persisted key is a stable slug, never the rendered title.**
  `lib/inspectorPrefs.ts` keys on `activity`/`tokens`/`subagents`/`cost`/`prs`
  and pins both those ids and the shipped defaults through `lib/typeAssert.ts`,
  because two of the session groups interpolate a count into their heading
  (`Sub-agents · 12`) — a title-keyed blob would store a new key per session and
  remember nothing, and a respelled id silently resets everyone's saved state.

**The session inspector is `views/sessions/SessionInspector.tsx`** (#538), not a
private function in `SessionsView.tsx`, for the reason `SaveBar` is a component:
a shape only one file can reach is a shape the next caller copies. It takes
`{ session }` and no handlers — the action strip is `.sess-strip`, which
`SessionsView` renders *above* `.inspector__scroll` and which #486 deliberately
put there. Note the detail page needs no second mount: `SessionDetail` fills
`.pane-detail`, and the `inspectorOpen` `aside.pane-inspector` is its **sibling**
inside the same `.panes`, so a session's Transcript and Journey tabs already
show this pane from this code. `views/sessions/sessionMetrics.ts` holds the
totals and the permission-mode tables that came out with it, because the table's
Cost column and its row badges call them too and exporting them from the view
would have made the view and the inspector import each other.
It carries `styles/sessions.css` itself — `SessionLink.tsx`'s shape, one
directory over — because the `.sess-heading` / `.sess-preview` / `.sess-pr*`
rules it renders are declared only there and reached it until #538 only because
`SessionsView` happened to import that sheet. A caller outside this section
would have rendered the Session heading and the whole Pull requests list
unstyled, with nothing to say so; that is the savebar failure above, and a
component sold as portable has to own its own styling or it is not.

**Destroying a stored record is `Delete`, everywhere, and `Remove` means
something else** (#518). One gesture had three words — Integrations said
`Remove`, Tasks and Settings profiles said `Delete`, Agents said `Delete
agent`. `formVerbs.ts`'s `DESTROY` / `DESTROYING` is the spelling and the
compile-time pin; `Delete` won because it was already the majority and because
`Remove` reads as *detach without destroying*, which is not what
`DELETE /api/integrations/{id}` does — the row and its stored credentials go.

- **The verb is the same in all three places**: the danger button, the
  icon-button `title`, and the confirmation. Not `Delete agent` — the record's
  kind is already on screen, and a per-view suffix is how a fourth word starts.
- **A confirmation is `Delete <name>? <what goes with it>.`** — the verb, the
  record's own name, and one clause naming the collateral (`Its run history
  goes with it.`). Not `Delete this task and its history?`, which names no row,
  and not a bare `Delete rule?`.
- **`Remove` survives for detaching, and only that**: taking a row out of a
  list nothing has stored yet (a gateway alias's fallback target), or
  unregistering something from a third party while the record it belongs to
  stays (the Telegram webhook). If the record is gone afterwards, it is
  `Delete`.

**One connection state gets one word, and it is `Connected`** (#518).
`Integration.authenticated` renders in four places visible at once — the
sidebar row's preview, the detail toolbar badge, the Authorisation section's
Status row and the inspector's State row — and each was written at its call
site, so one row read `GitHub · Not connected` in the sidebar and `Not
authenticated` in the inspector. Two words for one boolean reads as two states.

`connectionState()` in `views/integrations/catalog.ts` is the only place either
word is spelled, pinned the same way, and `IntegrationsView.tsx`'s
`ConnectionBadge` carries the label **and** its `badge--green`/`badge--amber`
tone together — a site that took the label and picked its own tone would be the
same defect in a different column. `Connected` rather than `Authenticated`
because it is the word the surrounding screen already speaks (the sidebar's
`Connected` group, `Nothing connected yet.`, `Not connected yet — …`) and
because it is the *user's* word rather than the wire's: `authenticated` is a
column whose meaning is under review (#513), and copy spelled after a column
has to be re-read every time the column moves. The Authorisation section keeps
its heading and its `Authorise` / `Validate credentials` buttons, which name
the **action**; only the state had two names.

**No `window.confirm` / `alert` / `prompt`** — they block the WebView and can
wedge the app. Render inline confirmation UI.

**Theming.** Tokens are defined on bare `:root` for light, then re-declared
under both `@media (prefers-color-scheme: dark)` and `:root[data-theme="dark"]`.
Never give a colour its only definition inside a media block. Charts are inline
SVG using `var(--accent)` etc. — no chart libraries.

---


### CSS trap

`.card` sets `overflow: hidden`, so as a flex child of a scrolling column its
min-content height collapses to zero and everything below the fold becomes a
sliver. Dashboard containers need `> * { flex: 0 0 auto; }`.
