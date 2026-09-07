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

**An app started any other way cannot be driven.** `npm run app:alongside` on
its own opens no inspector, and `app.sh --status` then answers `inspector DOWN`
with the launch line to use. Stop that instance (`app.sh --stop`) and launch
through `app.sh`; the binary is already built, so it is back in ~15 s. Do this
before the first screenshot, not after the first failure.

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

Fields split on `|`, so a `||` inside a `wait` predicate is cut into three
fields and fails with `Unexpected end of script`. Write a literal pipe as
`\|` (`wait|a \|\| b|5000`), or use `??` where it means the same thing.

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

## Flows that are known to work, with the selectors they need

Each of these was driven end to end on 2026-09-07. `text=` is a prefix match
on trimmed text, and this UI's checkboxes are `<button role="checkbox">`, so
`input[type=checkbox]` finds nothing anywhere.

**Create an agent** (Agents → the `+` above the list):

```bash
node ui.mjs click 'text=Agents'
node ui.mjs click 'button[title="New Agent"]'        # capital A; "New agent" finds nothing
node ui.mjs type 'input[placeholder="Release Notes Writer"]' 'My Agent'   # Name
# the two textareas carry no placeholder: [0] is Description, [1] is System prompt
node ui.mjs eval 'document.querySelectorAll("textarea")[1].id="sysprompt"; 1'
node ui.mjs type '#sysprompt' 'You are …'
# tick capabilities by label INSIDE one integration group: tool names repeat
# across integrations (Slack and Telegram both have send_message and
# read_messages), so a page-wide label match ticks both and the agent gets a
# second integration nobody asked for. Each group is .agents-group, its name
# is .agents-group__name, each row is .agents-cap with .agents-cap__label.
node ui.mjs eval '(()=>{const g=[...document.querySelectorAll(".agents-group")].find(x=>x.querySelector(".agents-group__name")?.textContent.trim()==="GitHub");[...g.querySelectorAll(".agents-cap")].filter(r=>["list_issues","get_issue"].includes(r.querySelector(".agents-cap__label").textContent.trim())).forEach(r=>r.querySelector("button[role=checkbox]").click());return 1})()'
node ui.mjs click 'text=Create'
node ui.mjs wait '/State\s*Saved/.test(document.querySelector(".pane-inspector").textContent)' 8000
```

`eval` runs in the page's global scope and the page lives on between calls, so
a top-level `const t = …` in one `eval` makes the next one throw *"Can't create
duplicate variable"*. Wrap anything that declares in an IIFE, `(()=>{…})()`.

After Create, read the stored capabilities back (`curl …/api/agents/<slug> |
jq .capabilities`) before starting a chat on the agent: a wrongly ticked write
tool is a message sent somewhere real.

The slug is derived from the name (`My Agent` → `my-agent`); confirm the store
with `curl …/api/agents/<slug>` rather than trusting the inspector alone.

**Start a chat on an agent and send a message** (the "New conversation" pane):

```bash
node ui.mjs click 'text=New Chat'                       # the sidebar button
node ui.mjs wait 'document.querySelector("button.select")!==null' 3000
node ui.mjs click 'button.select'                       # the agent picker
node ui.mjs click 'text=My Agent'                       # a .dropdown__item; "No agent — direct chat" is the first
node ui.mjs type 'input[placeholder="Working directory (required)"]' '/tmp/agento/work'   # must exist
node ui.mjs type 'textarea' 'your message'
node ui.mjs key 'textarea' Enter --ctrl                 # Ctrl+Enter sends; the chat is created on send
```

Run those as one uninterrupted sequence: the draft pane is a piece of view
state, and anything that selects a chat (a click in the list, `text=Chats`, a
hand-off) replaces it — then `textarea` matches the *selected chat's*
composer and the message lands there instead. Check
`node ui.mjs text '.pane-inspector'` names the agent you meant before sending.

**Wait for a turn to finish.** The inspector's Status row is the signal, and
its text has no space between label and value:

```bash
node ui.mjs wait '/Status\s*Running/i.test(document.querySelector(".pane-inspector").textContent)' 10000
node ui.mjs wait '/Status\s*Idle/i.test(document.querySelector(".pane-inspector").textContent)' 180000
node ui.mjs shot /tmp/turn.png
```

A turn that calls tools takes 20–90 s. The status bar's `Idle` is *not* the
same signal — it reads Idle before the first frame arrives, so waiting on it
alone photographs an empty reply. Tool calls render as rows under the
assistant name with a ✓ or ⚠ at the right; a denied built-in (the agent's
allowlist working) is the ⚠.

**Open an existing chat**: `.listrow` rows in the middle pane, most recent
first — `click '.listrow'` is the newest, `'.listrow:nth-child(2)'` the next.

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
