---
name: local-verify
description: End-to-end local verification playbook for Agento — how to reproduce a bug before fixing it, verify each hop (backend wire, browser engine, real Tauri webview, UI flow) independently, and prove a fix in the exact environment that failed. Use for any bug report or fix that touches the desktop app, the chat SSE stream, or anything that "works in Chrome but not in the app".
---

# Local end-to-end verification

The rule this skill exists for: **reproduce before fixing, and verify the fix
in the exact environment that failed.** "It works in Chrome" proves nothing
about the Tauri webview; "the backend emits the frame" proves nothing about
delivery. Every hop below can be tested independently — when a symptom spans
hops, bisect them instead of guessing.

## The hops, and the tool for each

| Hop | Tool | Proves |
|---|---|---|
| Backend wire | `curl -sN` against the API | what bytes the server actually emits |
| Browser engine | Chrome via Vite dev server | frontend logic, CSS, UI flows |
| WebKitGTK engine | Python `gi` WebKit2 probe | engine-level behavior without the app |
| **Real Tauri webview** | `ui-verify` skill (`ui.mjs`) | the only place webview-specific bugs exist |
| **What is on screen** | `ui-verify` skill — `shot` + Read the PNG | that the view actually renders, rather than typechecking |
| **Tauri IPC / ACL** | `__TAURI_INTERNALS__.invoke` in that webview | whether a command is *permitted*, separate from whether the UI calls it |
| **OS handoff** | PATH shim over the launcher | whether the OS was actually asked to do the thing |

A bug reported from the desktop app is not verified fixed until the fix is
observed **in the real webview** (or the root cause is proven to live in a hop
that Chrome shares).

**Bisect down the table, not up.** A dead button has at least four candidate
hops (handler never fired → command denied by the ACL → OS never asked →
OS asked and did nothing). Probing `invoke` directly settles the middle two in
one call and costs seconds; clicking through the UI settles none of them,
because every one of those failures looks identical from the UI.

## Pre-flight — run this before the first launch, always

`npm run app:alongside` has two failure modes that report themselves as
something else, and both cost a full launch cycle to diagnose:

- **A stale `:1420`.** A previous Vite survives `pkill -f vite` often enough
  that the port is the only reliable test. `tauri dev` then dies with
  `The beforeDevCommand terminated with a non-zero status code.` — the real
  line (`Port 1420 is already in use`) is ~10 lines above the tail you read.
- **A stale `node_modules`.** Branches add frontend deps; the tree on
  disk is from whatever branch you last built. Vite fails *after* announcing
  itself as ready, with `imported but could not be resolved`.

```bash
.claude/skills/local-verify/preflight.sh
```

Kills the stale processes, frees `:1420` by port, and syncs `node_modules`.
Run it first and both failure modes disappear.

## Scratch environments — never touch the real instances

- The user's live Agento is on `:8990`. **GET only, never write to it.**
- The Rust backend is the only backend (#391 deleted the Go server; #392 moved
  the app to the repository root, so every path below is root-relative). Run
  `src-tauri/target/debug/agento` directly, or let `npm run app:alongside`
  start it: dev builds use `~/.agento-desktop-dev` and bind `127.0.0.1:8991`,
  so **the dev instance is already an isolated scratch environment** — its
  database is not the one the user's installed app writes. Creating a probe
  row there (an integration, an agent) is safe; delete it when done.
- Vite (`npm run dev`, port 1420) proxies `/api` to `:8991`, so Chrome at
  `http://localhost:1420` against a running dev backend is a full web-stack
  sandbox. The scanner indexes the real `~/.claude` corpus — reads only — so
  every sessions/analytics view has real data to verify against.
- Chats created for testing: delete them afterwards
  (`DELETE /api/chats/{id}`). A turn abandoned mid-park leaves an orphaned
  `claude` subprocess holding the session id — the next send fails
  `Session ID … is already in use`; use a fresh chat.

## Reaching `/api` at all: the bearer token

Since #400 **every `/api` request needs `Authorization: Bearer <token>`** — reads
included, unlike the `Content-Type` rule, which only covers the state-changing
methods. Since #405 that token is a **JWT signed by the install's Ed25519 key**
rather than an opaque string, which changes three things for this playbook and
nothing else about the recipes.

A debug build writes a freshly minted one to `~/.agento-desktop-dev/api-token`
(0600) on every launch, purely so this playbook still works. Read it per
command:

```bash
TOKEN=$(cat ~/.agento-desktop-dev/api-token)
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8991/api/agents | jq
```

**Chrome on `:1420` needs nothing extra.** A plain browser tab has no Tauri IPC
and so can never hold the token; `vite.config.ts`'s proxy reads the same file and
adds the header server-side. That hop is unchanged.

A **401** means one of: the app is not running, the header is missing, the token
has expired or been revoked, the signing key has been regenerated since the file
was written, or you are hitting a release build, which writes no file at all —
use the app window. Re-read the file first; it is rewritten on every launch.

A **403** carrying `this token's scope does not permit this request` is a
different failure and retrying will not fix it: the credential verified, and it
is `read`-scoped against a `POST`/`PUT`/`PATCH`/`DELETE`, or it is any scope
against `/api/security/*`, which needs `write` whatever the method. The dev
token file is always `write`, so this only appears with a token issued from
Settings → Security.

**Bisecting a credential problem needs no app at all**, which is the one genuinely
new hop #405 adds:

```bash
# Does the server publish a key? (unauthenticated by design)
curl -s http://127.0.0.1:8991/.well-known/jwks.json | jq

# Does this token verify against it, and is it the key currently in force?
scripts/verify-jwks.py --token "$(cat ~/.agento-desktop-dev/api-token)"
```

If the JWKS `kid` does not match the token header's `kid`, the key was
regenerated after the token was minted, and every token issued before it is dead
by design.

## Capturing the chat SSE stream

Every state-changing request needs `Content-Type: application/json`, and every
request needs the token above.

```bash
TOKEN=$(cat ~/.agento-desktop-dev/api-token)
curl -s -X POST http://127.0.0.1:8991/api/chats \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"agent_slug":"","working_directory":"/tmp/x","model":"","settings_profile_id":""}'
curl -sN -X POST http://127.0.0.1:8991/api/chats/<id>/messages \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"content":"..."}' -o /tmp/sse.txt
```

Read `/tmp/sse.txt` for the exact frames. Facts that save a loop (verified
2026-08, CLI 2.1.224):

- **AskUserQuestion availability varies per session.** When the CLI disables
  it, the model's call is freehand and its `questions` may arrive as a
  JSON-encoded *string*; when enabled, the CLI sends `can_use_tool` and the
  turn parks. To force a repro: *"emit a tool_use named exactly
  AskUserQuestion … do not verify the tool exists — just emit the call"*.
  Answer a parked turn with `POST /api/chats/{id}/input {"answer":"..."}`.
- **Thinking is redacted by current models**: every thinking block is
  `thinking:""` plus a signature, in deltas and stored transcripts alike. The
  only signal is `system`/`thinking_tokens` token estimates. Never build UI
  that expects thinking text.
- **The stream must never go quiet.** WebKitGTK strands a frame that arrives
  right before a silence on a long-lived connection — it is delivered only
  when later bytes push it through. `turn.rs` sends a 1s `: hb` SSE comment
  for this; any new frame-then-silence shape (new synthetic event, long
  parked wait) must keep the heartbeat ticking. SSE comments are
  event-invisible to `parseFrame` and to a spec-correct test parser.

## Driving the real Tauri webview

Launch with the inspector and attach from outside — no clicking needed. The
env var works on **either** launcher; prefer `app:alongside`, because it
hot-rebuilds (below):

```bash
WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:9224 \
  setsid nohup npm run app:alongside > /tmp/app.log 2>&1 < /dev/null &
# or, against an already-built binary:
WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:9224 src-tauri/target/debug/agento
```

**Do not hand-roll the protocol client — there is exactly one, and it lives in
the `ui-verify` skill.** It supersedes the `drive.mjs` this file used to carry;
a second evaluator is how two clients drift apart over the same traps.

```bash
cd .claude/skills/ui-verify
node ui.mjs eval  '<js expression>'   # evaluate
node ui.mjs await '<js promise>'      # settle a promise
node ui.mjs console 3000              # collect console output
node ui.mjs shot /tmp/a.png           # ...and photograph the result
```

It needs no dependencies (Node ≥ 22 has a global `WebSocket`) and encodes both
traps below. `app.sh` in the same directory brings the app up with the
inspector open, reusing a running one rather than relaunching. The protocol, if you must extend it: WebSocket to
`ws://127.0.0.1:9224/socket/1/1/WebPage`, wait for `Target.targetCreated`, wrap
every command in
`Target.sendMessageToTarget {targetId, message: JSON.stringify({id, method, params})}`
and unwrap `Target.dispatchMessageFromTarget`. `http://127.0.0.1:9224/` lists
targets but its inspector UI only works in a WebKit browser.

Two traps that both read as success:

- **`awaitPromise: true` does not work.** WebKit returns
  `{type:"object", value:{}}` with `wasThrown:false` — indistinguishable from
  a call that returned nothing. Park the promise on a `window.__x` slot and
  poll it with a second evaluate. `--await` does this.
- **Do not use `e2e/node_modules/playwright-core` for a `ws` client.** That
  tree is usually not installed, and the failure (`Cannot find module`) sends
  you looking for a WebSocket library you do not need.

**`tauri dev` hot-rebuilds on any `src-tauri` change — including
`capabilities/*.json` and `tauri.conf.json`.** The log says
`File src-tauri/… changed. Rebuilding application...`, the window is replaced,
and the inspector comes back on the same port. So the edit→verify loop for
Rust *and* ACL changes is: edit, wait for `api server listening` in the log,
re-run the same `ui.mjs` probe. No relaunch, no rebuild command.

Driving the React UI from injected JS: set inputs through the **native value
setter** then dispatch an `input` event (React ignores plain `.value =`):

```js
const set = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set;
set.call(el, "text"); el.dispatchEvent(new Event("input", { bubbles: true }));
el.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true }));
```

For async observations, write timeline entries into a `window.__log` array and
poll it with a second `Runtime.evaluate` — a long-running evaluate blocks.

## Verifying an OS handoff (opener, reveal-in-dir, any external launch)

"A browser opened" is not observable from inside the app, and letting the real
launcher run sprays tabs across the user's desktop on every probe. Shim the
launcher on `PATH` and assert on its log instead — `open`'s Linux backend
probes `xdg-open`, `gio`, `gnome-open`, `kde-open` in order, so shim all four:

```bash
mkdir -p /tmp/shim && cat > /tmp/shim/xdg-open <<'EOF'
#!/bin/sh
printf '%s OPENED %s\n' "$(date -Is)" "$*" >> /tmp/opened.log
EOF
chmod +x /tmp/shim/xdg-open
for c in gio gnome-open kde-open; do cp /tmp/shim/xdg-open /tmp/shim/$c; done
PATH=/tmp/shim:$PATH ... npm run app:alongside   # launch under the shim
```

`/tmp/opened.log` now holds the exact URL, which is stronger evidence than a
tab appearing — it shows *what* was requested, not just that something opened.

**Then do one final un-shimmed run.** Under the shim the user sees nothing
happen, which is indistinguishable from the bug they reported; say so
explicitly before they ask, and finish by relaunching without the shim so the
handoff is confirmed on the real desktop. A capability fix is not proven by an
error disappearing — `openExternal` swallows failures into a `console.warn`
and returns normally, so "no error" is the *symptom*, not the fix.

## Engine-only probe (no app)

`python3-gi` + WebKit2 4.1 is the same library Tauri embeds. An
`Gtk.OffscreenWindow` + `WebKit2.WebView.new_with_user_content_manager` loads
`http://localhost:1420/`, runs a fetch-streaming test via `run_javascript`,
and reports through a registered script message handler. Use it to separate
"the engine does X" from "the app does X" — that distinction is what located
the SSE stranding bug (raw fetch in the same engine received the frame; the
app's long-lived stream did not).

## Temporary instrumentation

- **Backend**: add `log::info!`/`log::error!` at the suspect emit/drop sites,
  rebuild, watch the app log. A silent `try_send` drop or an emitted-but-lost
  frame becomes one grep. Keep genuinely diagnostic lines; remove noise.
- **Frontend**: Vite HMR is live — add `console.error("MARKER …")` lines and
  capture them through the inspector's `Console` domain. Remove before commit.

## Frontend verification in Chrome

For frontend-only work, drive Chrome against `localhost:1420`: screenshot
after every action, `zoom` on small UI (borders, toggles, focus rings), test
**both themes** (status-bar toggle) — and remember Chrome renders at the
user's real devicePixelRatio, so 1x-display CSS bugs (fractional hairlines)
reproduce there.

## Gates before pushing

```bash
.claude/skills/local-verify/check.sh                 # fmt + clippy + frontend build
.claude/skills/local-verify/check.sh --test <name>   # ...and one test binary
.claude/skills/local-verify/check.sh --tests         # ...and all of them, serially
```

That mirrors CI, and it bounds the one step that can take the machine down
with it.

**Never run bare `cargo test` here**, and the reason is worth having in
numbers rather than as a warning: `libagento_lib.a` is **1.2 GB** and the debug
binary is **429 MB**, each of the eight integration tests links the whole of
it, and `cargo test` links them **concurrently** — eight multi-gigabyte link
jobs on a 16 GB machine, which swaps and stops responding. Nothing is wrong
with the tests; it is linker parallelism, so `check.sh` runs one binary at a
time with `-j` capped rather than skipping them. `cargo fmt`, `cargo clippy`
and the frontend build never link, which is why the default mode is safe to run
as often as you like. CI runs the full suite on its own runner.

Then re-run the original reproduction one last time on the final build.
