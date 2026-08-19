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
| **Real Tauri webview** | WebKit remote inspector | the only place webview-specific bugs exist |

A bug reported from the desktop app is not verified fixed until the fix is
observed **in the real webview** (or the root cause is proven to live in a hop
that Chrome shares).

## Scratch environments — never touch the real instances

- The user's live Agento is on `:8990`. **GET only, never write to it.**
- Scratch Go backend (serves the same API the frontend needs):
  ```bash
  PORT=8991 AGENTO_DATA_DIR="$HOME/.cache/agento-scratch" go run -tags dev . web
  ```
  Vite (`cd desktop && npm run dev`, port 1420) proxies `/api` to `:8991`, so
  Chrome at `http://localhost:1420` is a full web-stack sandbox. The scanner
  will index the real `~/.claude` corpus into the scratch DB — reads only,
  and it gives every sessions/analytics view real data to verify against.
- Desktop Rust backend: run `desktop/src-tauri/target/debug/agento` directly
  (dev builds use `~/.agento-desktop-dev` and bind `127.0.0.1:8991`; a window
  opens on the user's display). Build with the shared target dir:
  `CARGO_TARGET_DIR=<main checkout>/desktop/src-tauri/target cargo build`.
- Chats created for testing: delete them afterwards
  (`DELETE /api/chats/{id}`). A turn abandoned mid-park leaves an orphaned
  `claude` subprocess holding the session id — the next send fails
  `Session ID … is already in use`; use a fresh chat.

## Capturing the chat SSE stream

Every state-changing request needs `Content-Type: application/json`.

```bash
curl -s -X POST http://127.0.0.1:8991/api/chats -H "Content-Type: application/json" \
  -d '{"agent_slug":"","working_directory":"/tmp/x","model":"","settings_profile_id":""}'
curl -sN -X POST http://127.0.0.1:8991/api/chats/<id>/messages \
  -H "Content-Type: application/json" -d '{"content":"..."}' -o /tmp/sse.txt
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

Launch with the inspector and attach from outside — no clicking needed:

```bash
WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:9224 desktop/src-tauri/target/debug/agento
```

`http://127.0.0.1:9224/` lists targets, but its inspector UI only works in a
WebKit browser. Drive the protocol directly instead: WebSocket to
`ws://127.0.0.1:9224/socket/1/1/WebPage`, wait for `Target.targetCreated`,
then wrap every command in
`Target.sendMessageToTarget {targetId, message: JSON.stringify({id, method, params})}`
and unwrap `Target.dispatchMessageFromTarget`. `Runtime.evaluate` (with
`returnByValue: true`) runs JS in the page; `Console.enable` streams console
messages. A `ws` client is available without installing anything:

```js
import { createRequire } from "module";
const require = createRequire("<repo>/e2e/node_modules/playwright-core/package.json");
const { ws } = require("playwright-core/lib/utilsBundle");
```

Driving the React UI from injected JS: set inputs through the **native value
setter** then dispatch an `input` event (React ignores plain `.value =`):

```js
const set = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value").set;
set.call(el, "text"); el.dispatchEvent(new Event("input", { bubbles: true }));
el.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true }));
```

For async observations, write timeline entries into a `window.__log` array and
poll it with a second `Runtime.evaluate` — a long-running evaluate blocks.

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

Mirror CI, from `desktop/`: `npm run build` (tsc + vite). For
`src-tauri/` changes: `cargo fmt --check`, `cargo clippy --all-targets -- -D
warnings`, `cargo test` (use the shared `CARGO_TARGET_DIR`). `cargo test`
stops at the first failing test *binary* — add `--no-fail-fast` for sweeps.
Then re-run the original reproduction one last time on the final build.
