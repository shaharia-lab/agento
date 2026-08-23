---
name: app-demo-ui-screenshot
description: Produce the README / marketing screenshots of the Agento desktop app against a fully synthetic, anonymised dataset — without touching the real ~/.agento or ~/.claude. Use when asked to "take the README screenshots", "regenerate the app screenshots", "screenshot the app with demo data", or after a UI change that makes docs/screenshots stale.
---

# Demo-data screenshots of the desktop app

Produces `docs/screenshots/light/*.png` — the desktop counterparts of the
images `main`'s README used — from the **real Tauri webview**, over a corpus
that is invented end to end. The map of which file shows what, and what a
designer may still want to touch, is `docs/screenshots/README.md`; this skill
is *how* the set is made. It builds on `ui-verify` (driving the webview) and
adds the data side.

## The idea in one paragraph

A debug build derives everything from `HOME`: the data dir is
`$HOME/.agento-desktop-dev` and the scanner walks `$HOME/.claude`. So the app is
launched with a **fake `HOME`** (`~/.agento-demo` by convention) holding a
generated Claude Code corpus, and a fresh database is created there. Nothing
reads or writes the real install. The only thing that has to be real is the
set of project *directories*: the scanner decodes `-home-you-Projects-auth`
into `/home/you/Projects/auth` only if that directory exists, and the UI only
collapses `/home/<x>/…` to `~/…` — so ten empty dirs are created under
`~/Projects` for the duration of the shoot and removed afterwards.

## Run it

```bash
K=.claude/skills/app-demo-ui-screenshot
DEMO=$HOME/.agento-demo

# 1. corpus + the dirs it decodes against
python3 $K/gen_corpus.py "$DEMO"          # ~300 sessions, 10 projects, sub-agents
$K/make_dirs.sh                           # empty ~/Projects/<name> dirs (records what it created)

# 2. the app, isolated, under XWayland so the window can be sized
.claude/skills/ui-verify/app.sh --stop
setsid nohup $K/launch.sh "$DEMO" > /tmp/agento-app.log 2>&1 < /dev/null &
#    wait for :9224, then size the window to the README frame:
W=$(xwininfo -root -tree | grep '("agento" "Agento")' | grep -v 10x10 | awk '{print $1}')
wmctrl -i -r $W -b remove,maximized_vert,maximized_horz; xdotool windowsize $W 1440 900

# 3. agents / integrations / tasks through the app's own API, chats + job
#    history straight into the scratch DB (no create API for those)
python3 $K/seed_api.py "$DEMO"
.claude/skills/ui-verify/app.sh --stop      # one writer at a time
python3 $K/seed_db.py "$DEMO/.agento-desktop-dev/agento.db"
#    + mark the integrations authenticated and fill service tool lists — see
#      "Hand edits" below
setsid nohup $K/launch.sh "$DEMO" > /tmp/agento-app.log 2>&1 < /dev/null &   # relaunch, resize again
#    session_insights fills itself within a minute of the relaunch — see below

# 4. shoot (light theme — set it in the status bar / ⌘K first)
$K/shoot.sh docs/screenshots/light

# 5. clean up
.claude/skills/ui-verify/app.sh --stop
$K/make_dirs.sh --clean
```

`gen_corpus.py` is seeded and deterministic in everything except the session
UUIDs, so a regenerated corpus is a *new* corpus to the scanner: rescan
(`POST /api/claude-sessions/refresh`) and the app reconciles the orphaned
`session_insights` rows and recomputes the new ones itself.

## The parts that are not obvious

- **`session_insights` fills itself now, and this step used to be manual.**
  Until #408 the app had the nine insight processors as pure functions and the
  summary endpoint that reads the table, but nothing that *wrote* one — so
  Insights was empty on every fresh install, and this skill could only
  photograph it by building a Go writer out of git history (`backfill.sh`,
  pinned at `3b54e41`) and running it against the demo data dir with Agento
  stopped. `backfill.sh` is deleted with the bug.

  What replaces it is `native/insights/worker.rs`, which sweeps at boot and
  every five minutes. Practically: **relaunch and give it a minute**, then check
  before shooting, because an Insights shot taken too early is a page of zeros
  that looks like a rendering bug rather than a timing one:

  ```bash
  sqlite3 -readonly "$DEMO/.agento-desktop-dev/agento.db" \
    "select (select count(*) from session_insights),
            (select count(*) from claude_session_cache)"
  ```
- **Hand edits after `seed_api.py`**, in the scratch DB (`sqlite3`):
  - `UPDATE integrations SET auth='{"validated":true,"username":"acme-platform-bot"}' WHERE type='github'`
    (and `team_name` / `display_name` / `bot_username` for slack / jira /
    telegram) — the real `auth/validate` dials the provider and 400s on a fake
    token, so the rows would otherwise read "Not connected".
  - fill every `services.<svc>.tools` list with the provider's tool names
    (they are in `src/views/integrations/catalog.ts`) — `/available-tools`
    iterates the explicit lists, so an empty list means an empty agent builder.
    Agent `capabilities.mcp` must use the *real* tool names (`list_pulls`,
    `get_pull`, not `list_pull_requests`).
- **Project names must not share a prefix with a real `~/Projects` entry.**
  `decode_project_path` resolves greedily: `docs-site` beside a real `docs/`
  decodes to `…/docs/site`, fails the existence check and falls back to the raw
  `-home-you-Projects-docs-site`. `make_dirs.sh` refuses to clobber an
  existing dir for the same reason.
- **Settle before every shot.** `shoot.sh` waits until a predicate is true on
  four consecutive polls before each `shot`, because `Page.snapshotRect` can
  race React's re-render — the first pass painted the previous task's toggle
  and schedule type under the new task's name. Wait on the *new* view's
  content, never on the navigation, and beware that `text=Sessions` in the
  sidebar does not pop an open session detail (`shoot.sh` clicks the
  toolbar's back button first).
- **The `do` grammar splits on `|`**, so no `||` inside an `eval`/`wait` line;
  write `cond ? a : b` or a nested ternary instead.
- **Timestamps are UTC in the generator** (`NOW` there); keep the newest
  synthetic session a couple of hours behind the real clock or the list shows
  "in 8 m".
- **Windows sizing needs X11.** `launch.sh` exports `GDK_BACKEND=x11`; on
  Wayland nothing outside the app can resize it, and window-state may restore
  it maximised (hence the `wmctrl` line).

## What it leaves behind

`~/.agento-demo` (corpus + scratch DB, ~30 MB) — keep it to re-shoot, delete
it whenever. `make_dirs.sh --clean` removes the `~/Projects` stubs it created.
Nothing under the real `~/.agento` or `~/.claude` is touched; verify with an
mtime check if in doubt.
