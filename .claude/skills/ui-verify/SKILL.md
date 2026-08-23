---
name: ui-verify
description: See and drive the running Agento desktop app from an agent — screenshot the real Tauri webview, click, type, wait on state, read the console. Use to verify any UI change or bug fix visually, to check what is actually on screen before claiming a view works, or whenever a task says "check the app", "take a screenshot", "click through it", or "does it look right".
---

# Verifying the desktop UI by looking at it

An agent can see this app. `ui.mjs` attaches to the **real Tauri webview**
through WebKit's remote inspector and returns a PNG of exactly what the user
would see — no Chrome, no second app instance, no WebDriver, no extra process.
Screenshots come back as files the Read tool renders, so "does the Insights
page render" stops being a question you answer by reading JSX.

**A UI change is not verified until it has been photographed.** The frontend
typechecking, the backend returning the right JSON, and the component looking
correct in Chrome are three things that are all true of a view that renders as
a blank pane in the app.

## Start the app

```bash
.claude/skills/ui-verify/app.sh          # idempotent — reuses a running app
.claude/skills/ui-verify/app.sh --status # what is up
```

A cold start links a ~430 MB debug binary and takes minutes; a warm one is
instant because the script reuses whatever is already listening. **Leave the
app running between verifications** — relaunching per check is the single
most expensive thing you can do here, and it is never necessary: `tauri dev`
hot-rebuilds on any `src-tauri/**` or frontend change and the inspector comes
back on the same port.

## Drive it

```bash
cd .claude/skills/ui-verify

node ui.mjs probe                          # url, title, view, viewport, theme, visible errors
node ui.mjs shot /tmp/a.png                # the whole viewport
node ui.mjs shot /tmp/bar.png '.statusbar' # one element — 5 KB instead of 156 KB
node ui.mjs click 'text=Agents'            # by visible text
node ui.mjs click '.agentrow:nth-child(2)' # or by CSS
node ui.mjs type 'textarea' 'hello there'  # React-safe
node ui.mjs key 'textarea' Enter --ctrl
node ui.mjs text '.statusbar'
node ui.mjs eval 'location.href'
node ui.mjs await 'fetch("/api/agents").then(r => r.status)'
node ui.mjs wait 'document.querySelectorAll(".agentrow").length > 0' 5000
node ui.mjs console 3000
```

Then `Read` the PNG. That is the verification.

### Batch every flow through `do`

One connection, one process, for the whole sequence:

```bash
node ui.mjs do <<'EOF'
click|text=Token Usage
wait|!/Loading/.test(document.body.textContent)|15000
shot|/tmp/tokens.png
EOF
```

Measured on this app: **six steps — two navigations, two waits, two
screenshots — in 0.63 s wall and 50 ms of CPU.** A single `shot` is ~140 ms.
That budget is why there is no excuse for skipping the visual check, and why
`do` is the default rather than a dozen separate invocations.

## The three rules that decide whether the screenshot means anything

- **Wait on the thing you are about to photograph, never on the navigation.**
  Clicking "Token Usage" and shooting immediately captures
  `Loading analytics…` — a screenshot that proves the router works and says
  nothing about the view. Every `wait` predicate should name content that only
  exists once the data has arrived.
- **Photograph the element, not the page, when you know what changed.** A
  status-bar strip is 5 KB against 156 KB for the viewport, and the reading
  agent pays for those pixels in context. Full-viewport shots are for layout,
  first looks, and "something is wrong somewhere".
- **`text=` matches the start of the trimmed text, and this UI appends badge
  counts.** The sidebar's Agents row reads `Agents0`, Chats reads `Chats8`.
  `text=Agents` works; an equality match against `Agents` finds nothing.

## Traps that read as success

- **`awaitPromise: true` does not work over this protocol.** WebKit answers
  `{type:"object", value:{}}` with `wasThrown:false` — identical to a call that
  returned nothing. `ui.mjs await` parks the promise on `window` and polls it;
  do not "simplify" that back.
- **React ignores `el.value = "x"`.** `ui.mjs type` goes through the prototype's
  native value setter and then dispatches `input`, which is the only sequence
  React's synthetic event layer observes.
- **A long-running `evaluate` blocks the page.** Poll from outside (`wait`)
  instead of looping inside the webview.
- **`pkill -f "tauri dev"` kills the shell running the command**, because the
  pattern appears in that shell's own argv. `app.sh` writes it as `tauri[ ]dev`.

## When you need *real* OS input, not a DOM click

`el.click()` proves the handler and the state change. It does not go through
hit-testing, `data-tauri-drag-region`, the native titlebar, or the OS's own
input path — so it cannot verify that a control is reachable, unobscured, or
that the window drags.

Under Wayland the app's window is invisible to X11 tooling. Relaunch it on
XWayland and both native input and native capture work:

```bash
.claude/skills/ui-verify/app.sh --x11
xwininfo -root -tree | grep -i agento     # 0xe00003 = webview, 0x800027 = frame
xdotool windowactivate 0xe00003
xdotool mousemove --window 0xe00003 66 296 click 1
import -window 0x800027 /tmp/native.png   # includes the OS titlebar and buttons
```

Both were verified on this machine: the click navigated the app, and the
capture came back 1308x886 with window decorations. Use this tier only for the
things that need it — reachability, drag regions, window chrome. It costs a
relaunch, and `import` is X11-only.

## What this does *not* prove

The webview is one hop. A frame that never left the backend, a command the ACL
denied, or an OS handoff that was never requested all look like a UI that did
nothing. When a symptom could live in more than one hop, bisect with the
**`local-verify`** skill — it owns the backend-wire, engine, IPC/ACL and
OS-handoff probes, and the resource-safe pre-push gates.

## Cost, and why this shape was chosen over the alternatives

| Approach | Extra processes | Measured cost | Notes |
|---|---|---|---|
| **This skill** (inspector) | none — reuses the running app | 46 ms eval, 140 ms shot | real webview, real pixels |
| `xdotool` + `import` | none | ~130 ms | X11 only; adds native chrome and true input |
| WebdriverIO + `@wdio/tauri-service` | driver + a second app instance + Node | seconds per test, own build | Tauri's official E2E route; cross-platform and CI-shaped |
| Playwright | downloads and runs Chromium | heaviest | not the engine this app ships on Linux |

WebdriverIO is the right answer for a **CI** suite later — it is what Tauri
officially recommends, it runs on all three platforms, and it drives real W3C
input. It is the wrong answer for the inner loop an agent works in, because it
wants its own build and its own app instance for every run. Nothing here
forecloses adding it.
